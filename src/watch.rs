//! Watching and steering a server while an agent uses it. Each server
//! writes, to a folder of its own under the runtime directory, what it is
//! doing as it does it; a person follows that from a terminal and can stop
//! a call or leave the agent a note.
//!
//! ```text
//! $XDG_RUNTIME_DIR/legume-mcp/<server>-<pid>/     (owner-only)
//!   meta.json      the server: name, version, pid, project, started
//!   feed.jsonl     its events, one per line, each written whole
//!   state.json     the tool's current view (Call::state), replaced whole
//!   control.sock   {"stop": id} or {"note": text}, one JSON per line
//! ```
//!
//! The feed is for watching, not for redoing: the project's records
//! (`runall.sh`, the notebook) stay the durable account. A folder outlives
//! its server so a follower can read to the end; folders of servers gone for
//! a day are pruned when another starts. `LEGUME_MCP_WATCH=off` writes none;
//! a path there puts the folders under it instead.
//!
//! The reader ([`live`], [`follow`], [`state`], [`control`]) has no UI in
//! it, so every tool's viewer can follow its own server.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `off`, or the folder to keep the servers' folders in.
pub const ENV: &str = "LEGUME_MCP_WATCH";
/// A result's text is cut to this many bytes in the feed.
const TEXT_CAP: usize = 4096;
/// Folders of servers gone this long are pruned.
const KEEP_ENDED: Duration = Duration::from_secs(24 * 3600);
/// How often a follower at the end of the feed looks for more.
const POLL: Duration = Duration::from_millis(200);
/// How long the control socket waits on one client's command or reply.
const CONTROL_WAIT: Duration = Duration::from_secs(2);

/// One line of the feed: when, and what.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Local time, RFC 3339.
    pub t: String,
    #[serde(flatten)]
    pub body: Body,
}

/// What happened. `id` is the client's request id, as text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// The feed's first line.
    Server(Meta),
    Call {
        id: String,
        tool: String,
        args: Value,
    },
    Progress {
        id: String,
        message: String,
    },
    /// `text`: the result's text and JSON, cut to 4 KB; an image as
    /// `[image/png, N bytes]`.
    Result {
        id: String,
        ok: bool,
        ms: u64,
        text: String,
    },
    /// `by`: `client` or `user`.
    Cancelled {
        id: String,
        by: String,
    },
    /// The tool's view after a call changed it (as `state.json` holds it).
    State {
        id: String,
        state: Value,
    },
    /// A block added to the day's `runall.sh`; for a step that ran,
    /// `command` is its bare command line (no guard, no `mkdir`).
    Record {
        line: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    /// A notebook entry: `entry` is its kind (plan, observation, …, user).
    Notebook {
        entry: String,
        text: String,
    },
    /// A note the person left for the agent.
    Note {
        text: String,
    },
    /// The server stopped serving: its input closed (`ok`), or it failed.
    /// The feed's last line; a crashed server writes none, and its pid is
    /// gone instead.
    Ended {
        ok: bool,
    },
}

/// A server, as `meta.json` describes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub server: String,
    pub version: String,
    pub pid: u32,
    pub project: Option<PathBuf>,
    pub started: String,
}

/// A server's folder, and whether it is still running.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub meta: Meta,
    pub dir: PathBuf,
    pub alive: bool,
}

/// Where the servers' folders go: none when [`ENV`] is `off`.
pub fn base() -> Option<PathBuf> {
    match std::env::var(ENV) {
        Ok(v) if v.eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => Some(match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) => PathBuf::from(d).join("legume-mcp"),
            None => dirs_cache()?.join("legume-mcp").join("run"),
        }),
    }
}

fn dirs_cache() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache")),
    )
}

/// The servers with a folder under [`base`], newest first.
pub fn live() -> Vec<Live> {
    base().map(|b| live_in(&b)).unwrap_or_default()
}

/// [`live`] under `base`.
pub fn live_in(base: &Path) -> Vec<Live> {
    let mut out: Vec<Live> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let meta: Meta =
                serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()?;
            let alive = alive(meta.pid);
            Some(Live { meta, dir, alive })
        })
        .collect();
    out.sort_by(|a, b| b.meta.started.cmp(&a.meta.started));
    out
}

/// The tool's current view in `dir`, when it published one.
pub fn state(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("state.json")).ok()?).ok()
}

/// Where [`follow`] starts reading.
#[derive(Clone, Copy, Debug)]
pub enum From {
    Start,
    /// What comes next; with [`state`], a late follower's way in.
    End,
    /// A byte offset an earlier [`Follow::offset`] gave.
    Offset(u64),
}

/// The events of the server in `dir` as they come, from `from`. It waits at
/// the end for more, and ends once the server is gone and the feed read.
pub fn follow(dir: &Path, from: From) -> Result<Follow> {
    let path = dir.join("feed.jsonl");
    let mut file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let offset = match from {
        From::Start => 0,
        From::End => file.seek(SeekFrom::End(0))?,
        From::Offset(o) => file.seek(SeekFrom::Start(o))?,
    };
    let pid = serde_json::from_str::<Meta>(&std::fs::read_to_string(dir.join("meta.json"))?)?.pid;
    Ok(Follow {
        reader: BufReader::new(file),
        pid,
        offset,
        partial: String::new(),
        ended: false,
    })
}

/// See [`follow`].
pub struct Follow {
    reader: BufReader<File>,
    pid: u32,
    offset: u64,
    partial: String,
    /// The server's [`Body::Ended`] was read.
    ended: bool,
}

impl Follow {
    /// The byte offset after the last event returned.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Whether the server has ended: its [`Body::Ended`] was read, or its
    /// process is gone (what it wrote last is still there for
    /// [`Self::try_next`]). It never waits, for a viewer's tick.
    pub fn ended(&self) -> bool {
        self.ended || !alive(self.pid)
    }

    /// The next event if one is there now, without waiting.
    pub fn try_next(&mut self) -> Option<Event> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            self.partial.push_str(&line);
            if !self.partial.ends_with('\n') {
                continue; // the rest of the line is still being written
            }
            let whole = std::mem::take(&mut self.partial);
            self.offset += whole.len() as u64;
            if let Ok(e) = serde_json::from_str::<Event>(&whole) {
                self.ended |= matches!(e.body, Body::Ended { .. });
                return Some(e);
            }
        }
    }
}

impl Iterator for Follow {
    type Item = Event;

    /// The next event, waiting for it; none once the server has ended (its
    /// last event read) or is gone.
    fn next(&mut self) -> Option<Event> {
        loop {
            if let Some(e) = self.try_next() {
                return Some(e);
            }
            if self.ended {
                return None;
            }
            if !alive(self.pid) {
                // Gone: whatever it wrote last is read, then the end.
                return self.try_next();
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The steering side of the server in `dir`.
pub fn control(dir: &Path) -> Control {
    Control {
        socket: dir.join("control.sock"),
    }
}

/// See [`control`].
pub struct Control {
    socket: PathBuf,
}

impl Control {
    /// Stop call `id`: its child is killed and the agent is told the user
    /// stopped it.
    pub fn stop(&self, id: &str) -> Result<()> {
        self.send(&serde_json::json!({ "stop": id }))
    }

    /// Leave the agent a note: it comes with the next tool result, and goes
    /// into the notebook.
    pub fn note(&self, text: &str) -> Result<()> {
        self.send(&serde_json::json!({ "note": text }))
    }

    #[cfg(unix)]
    fn send(&self, msg: &Value) -> Result<()> {
        let mut s = std::os::unix::net::UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting to {}", self.socket.display()))?;
        writeln!(s, "{msg}")?;
        let mut reply = String::new();
        BufReader::new(s).read_line(&mut reply)?;
        anyhow::ensure!(reply.trim() == "ok", "{}", reply.trim());
        Ok(())
    }

    #[cfg(not(unix))]
    fn send(&self, _msg: &Value) -> Result<()> {
        anyhow::bail!("steering needs a Unix socket")
    }
}

#[cfg(unix)]
/// Whether process `pid` is running.
pub fn alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only checks the process exists.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
/// Whether process `pid` is running (assumed, where it cannot be asked).
pub fn alive(_pid: u32) -> bool {
    true
}

/// What the person asked for through the control socket, for the serve loop.
#[derive(Default)]
pub(crate) struct Steer {
    /// Calls the person stopped.
    pub(crate) stopped: HashSet<String>,
    /// Notes not yet handed to the agent.
    pub(crate) notes: Vec<String>,
}

/// A server's side of its folder.
pub(crate) struct Feed {
    dir: PathBuf,
    file: Mutex<File>,
}

impl Feed {
    /// Make the folder for this server under `base` (pruning those of
    /// servers gone a day), and write its first event.
    pub(crate) fn start(base: &Path, meta: Meta) -> std::io::Result<Self> {
        prune(base);
        let dir = base.join(format!("{}-{}", meta.server, meta.pid));
        private_dir(base)?;
        private_dir(&dir)?;
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta)?)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(dir.join("feed.jsonl"))?;
        let feed = Self {
            dir,
            file: Mutex::new(file),
        };
        feed.emit(Body::Server(meta));
        Ok(feed)
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one event. A feed that cannot be written is logged, never the
    /// call's failure.
    pub(crate) fn emit(&self, body: Body) {
        let event = Event {
            t: crate::project::rfc3339(),
            body,
        };
        let Ok(mut line) = serde_json::to_string(&event) else {
            return;
        };
        line.push('\n');
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = f.write_all(line.as_bytes()) {
            log::warn!("mcp feed {}: {e}", self.dir.display());
        }
    }

    /// Publish the tool's view: `state.json` replaced whole, and the event.
    pub(crate) fn state(&self, id: &str, state: &Value) {
        let tmp = self.dir.join(".state.json.tmp");
        let written = serde_json::to_vec(state)
            .map_err(std::io::Error::other)
            .and_then(|b| std::fs::write(&tmp, b))
            .and_then(|()| std::fs::rename(&tmp, self.dir.join("state.json")));
        if let Err(e) = written {
            log::warn!("mcp state {}: {e}", self.dir.display());
        }
        self.emit(Body::State {
            id: id.into(),
            state: state.clone(),
        });
    }

    /// A tool result as the feed shows it.
    pub(crate) fn result(&self, id: &str, result: &Value, ms: u64) {
        let ok = result["isError"] != true;
        let mut text = String::new();
        for c in result["content"].as_array().into_iter().flatten() {
            if !text.is_empty() {
                text.push('\n');
            }
            match c["type"].as_str() {
                Some("image") => {
                    let n = c["data"].as_str().map_or(0, |d| d.len() * 3 / 4);
                    let mime = c["mimeType"].as_str().unwrap_or("image");
                    text += &format!("[{mime}, {n} bytes]");
                }
                _ => text += c["text"].as_str().unwrap_or_default(),
            }
        }
        if text.len() > TEXT_CAP {
            let mut cut = TEXT_CAP;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push('…');
        }
        self.emit(Body::Result {
            id: id.into(),
            ok,
            ms,
            text,
        });
    }

    /// Take commands on `control.sock` until the server ends: a stop marks
    /// the call in `steer`, a note queues there, and both go in the feed;
    /// `on_note` also gets each note (the notebook).
    #[cfg(unix)]
    pub(crate) fn listen(
        self: &Arc<Self>,
        steer: Arc<Mutex<Steer>>,
        on_note: impl Fn(&str) + Send + 'static,
    ) -> std::io::Result<()> {
        let path = self.dir.join("control.sock");
        // A socket's path has room for 103 bytes on macOS, 107 on Linux.
        let room = if cfg!(target_os = "macos") { 103 } else { 107 };
        let len = path.as_os_str().len();
        if len > room {
            return Err(std::io::Error::other(format!(
                "{} is {len} bytes, more than a socket path can hold ({room}): \
                 the feed is written but stop and note are off; set {ENV} to a \
                 shorter folder",
                path.display()
            )));
        }
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let feed = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // A client that connects and goes quiet (killed mid-write)
                // must not hold every later stop and note.
                if stream.set_read_timeout(Some(CONTROL_WAIT)).is_err()
                    || stream.set_write_timeout(Some(CONTROL_WAIT)).is_err()
                {
                    continue;
                }
                let mut line = String::new();
                let Ok(mut reader) = stream.try_clone().map(BufReader::new) else {
                    continue;
                };
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let msg: Value = serde_json::from_str(&line).unwrap_or_default();
                let reply = if let Some(id) = msg["stop"].as_str() {
                    steer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .stopped
                        .insert(id.into());
                    "ok".to_string()
                } else if let Some(text) = msg["note"].as_str() {
                    steer
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .notes
                        .push(text.into());
                    feed.emit(Body::Note { text: text.into() });
                    on_note(text);
                    "ok".to_string()
                } else {
                    "error: expected {\"stop\": id} or {\"note\": text}".to_string()
                };
                let mut s = stream;
                let _ = writeln!(s, "{reply}");
            }
        });
        Ok(())
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.dir.join("control.sock"));
    }
}

/// `dir`, made if missing, and owner-only.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Remove the folders under `base` of servers gone for a day.
fn prune(base: &Path) {
    for l in live_in(base) {
        let old = std::fs::metadata(l.dir.join("feed.jsonl"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > KEEP_ENDED);
        if !l.alive && old {
            let _ = std::fs::remove_dir_all(&l.dir);
        }
    }
}
