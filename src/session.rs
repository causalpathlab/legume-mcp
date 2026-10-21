//! Session jobs, the server's side: a background step started in a
//! detached session of the tool's own TUI ([`Call::start_session_job`]),
//! followed through its folder ([`crate::job`]) by whichever server holds
//! the folder's lock: the one that started it, or, once that one is gone,
//! the next one for the same project.

use crate::job::{self, End, Spec};
use crate::jobs::{Job, Jobs, Session, State};
use crate::watch::{Body, Feed, Steer};
use crate::{absolute, lock, Call, ServerInfo};
use anyhow::{Context, Result};
use serde_json::json;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a follower reads its job's log and end.
const POLL: Duration = Duration::from_millis(200);
/// How often a follower asks its session's host whether it is still there.
const ASK_EVERY: Duration = Duration::from_secs(2);
/// How long a cancel waits for the session to wrap up before it kills it.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Folders of jobs a server saw end, kept this long.
const KEEP_SEEN: Duration = Duration::from_secs(24 * 3600);

/// A step to run in a session of the tool's own TUI.
pub struct SessionJob {
    /// The tool whose session it is (`senna`, for `senna attach`).
    pub tool: legume_tui::Tool,
    /// The session's screen, the middle of its id (`senna-fit-2`).
    pub screen: String,
    /// The session's label, the end of its id, when no live session has
    /// it (else the next number is), made one a session can have: other
    /// characters than letters, digits, `-`, `_` and `.` become `-`.
    pub label: Option<String>,
    /// The TUI that runs the step: this tool's own executable, its
    /// arguments, folder and environment. It reports through
    /// [`crate::job::Reporter`].
    pub watch: Command,
    /// The step as the driver script repeats it, without the TUI.
    pub record: Command,
    /// What it writes: held while it runs, as a job's outputs are.
    pub outputs: Vec<PathBuf>,
    /// One line: what it makes, from what.
    pub summary: String,
}

/// A session job started: its id for `job_status`, and its session's.
#[derive(Clone, Debug)]
pub struct Started {
    pub job: String,
    pub session: String,
}

/// The server, as its session jobs know it.
pub(crate) struct Server {
    name: String,
    version: String,
    /// Where its session jobs keep their folders.
    base: Option<PathBuf>,
    feed: Option<Arc<Feed>>,
    steer: Arc<Mutex<Steer>>,
    /// Set as it ends: its followers stop, leaving their jobs to the next
    /// server.
    ended: Arc<AtomicBool>,
}

/// A session job's folder, held: what a follower is given.
struct Held {
    dir: PathBuf,
    /// The folder's lock ([`try_lock`]), let go as it is dropped.
    _lock: File,
    spec: Spec,
    tool: legume_tui::Tool,
}

impl Server {
    /// The server `info`, its watch folders in `watch` when given.
    pub(crate) fn new(
        info: &ServerInfo,
        watch: Option<&Path>,
        feed: Option<Arc<Feed>>,
        steer: Arc<Mutex<Steer>>,
    ) -> Self {
        let base = watch
            .map(Path::to_path_buf)
            .or_else(crate::watch::default_base)
            .map(|w| w.join("jobs"));
        Self {
            name: info.name.clone(),
            version: info.version.clone(),
            base,
            feed,
            steer,
            ended: Arc::default(),
        }
    }

    /// The server ends: its followers stop.
    pub(crate) fn end(&self) {
        self.ended.store(true, Ordering::Relaxed);
    }

    /// Take up the session jobs of this server's tool on `project` that no
    /// server follows (running, or ended unseen), as jobs in `jobs`. With
    /// `sweep`, folders seen to end a day ago go too.
    pub(crate) fn adopt(&self, jobs: &Jobs, project: Option<&Path>, sweep: bool) {
        let Some(entries) = self.base.as_ref().and_then(|b| std::fs::read_dir(b).ok()) else {
            return;
        };
        let prefix = format!("{}-", self.name);
        let mut found: Vec<Held> = Vec::new();
        for dir in entries.flatten().map(|e| e.path()) {
            let ours = dir
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&prefix));
            if !ours {
                continue;
            }
            if let Some(seen) = age(&dir.join("seen")) {
                if sweep && seen > KEEP_SEEN {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                continue;
            }
            let Some(spec) = job::read_spec(&dir) else {
                continue;
            };
            let mine = spec.server == self.name && spec.project.as_deref() == project;
            if !mine || spec.session.is_empty() {
                continue;
            }
            // Followed by no server: ours from here.
            if let Ok(Some(lock)) = try_lock(&dir) {
                let tool = legume_tui::Tool::new(interned(&spec.tui));
                found.push(Held {
                    dir,
                    _lock: lock,
                    spec,
                    tool,
                });
            }
        }
        found.sort_by_key(|h| h.spec.epoch);
        for held in found {
            let ago = Duration::from_secs(epoch().saturating_sub(held.spec.epoch));
            let dir = held.dir.clone();
            if let Err(e) = self.take_up(jobs, held, Some(ago), None) {
                log::warn!("taking up {}: {e:#}", dir.display());
            }
        }
    }

    /// A job in `jobs` for the session job `held`, followed to its end by
    /// a thread of its own; `ago` is how long ago it started, for one a
    /// server before this one started; `from` the call that started it,
    /// here.
    fn take_up(
        &self,
        jobs: &Jobs,
        held: Held,
        ago: Option<Duration>,
        from: Option<&str>,
    ) -> Result<Arc<Job>> {
        let (spec, tool) = (&held.spec, held.tool);
        let (id, stop) = (spec.session.clone(), held.dir.join("stop"));
        let session = Session {
            id: id.clone(),
            attach: format!("{} attach {id}", tool.name()),
            stop: Box::new(move || {
                // Said in the folder, for whichever server sees it end.
                let _ = std::fs::write(&stop, "");
                let id = id.clone();
                // Off the call: a session may take a while to wrap up.
                std::thread::spawn(move || {
                    if !tool.stop(&id, false, STOP_GRACE).unwrap_or(true) {
                        let _ = tool.stop(&id, true, STOP_GRACE);
                    }
                });
            }),
        };
        let ago = ago.map(|a| (spec.started.clone(), a));
        let job = jobs.add(&spec.tool, &spec.summary, &spec.outputs, Some(session), ago);
        if let Some(f) = &self.feed {
            f.emit(Body::Call {
                id: job.id.clone(),
                tool: format!("{} (job)", spec.tool),
                args: json!({ "summary": spec.summary, "from": from, "session": spec.session }),
            });
        }
        let following = Following {
            job: job.clone(),
            held,
            feed: self.feed.clone(),
            steer: self.steer.clone(),
            ended: self.ended.clone(),
        };
        // Ended already (while no server followed it, or before this one
        // took it up): said here, so the call that took it up reports it
        // ended.
        if job::read_end(&following.held.dir).is_some() {
            following.run();
            return Ok(job);
        }
        std::thread::Builder::new()
            .name(job.id.clone())
            .spawn(move || following.run())
            .context("following a session job")?;
        Ok(job)
    }
}

impl Call {
    /// Run `job` in the background, as [`Self::spawn_job`] does, in a new
    /// detached session of the tool's TUI, so a person can watch it with
    /// `<tool> attach` and it outlives this server. The agent follows it
    /// with `job_status`, `job_wait` and `job_cancel` as any job (its
    /// status names the session); the next server for the project takes
    /// it up if this one ends first. The job records itself as it ends
    /// ([`crate::job::Reporter::finish`]).
    pub fn start_session_job(&self, job: SessionJob) -> Result<Started> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let server = &self.server;
        let outputs: Vec<PathBuf> = job.outputs.iter().map(|o| absolute(o)).collect();
        self.ensure_free(&outputs)?;
        let base = server.base.as_ref().context("no folder for session jobs")?;
        crate::watch::private_dir(base).with_context(|| format!("making {}", base.display()))?;
        // Its own, never one an earlier server left.
        let n = NEXT.fetch_add(1, Ordering::Relaxed) + 1;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = base.join(format!("{}-{nanos}-{n}", server.name));
        std::fs::create_dir(&dir).with_context(|| format!("making {}", dir.display()))?;
        let mut spec = Spec {
            server: server.name.clone(),
            version: server.version.clone(),
            project: self.records.as_ref().map(|p| p.root().to_path_buf()),
            tool: self.tool.clone(),
            tui: job.tool.name().into(),
            session: String::new(),
            summary: job.summary.clone(),
            outputs,
            record: self.records.as_ref().map(|p| p.command(&job.record)),
            started: crate::project::rfc3339(),
            epoch: epoch(),
        };
        // Held from the start: no other server takes it up meanwhile.
        let started = try_lock(&dir)
            .and_then(|lock| lock.ok_or_else(|| std::io::Error::other("held by another")))
            .and_then(|lock| write_spec(&dir, &spec).map(|()| lock))
            .context("setting up the job's folder")
            .and_then(|lock| start(&job, &dir).map(|id| (lock, id)));
        let (lock, session) = match started {
            Ok(started) => started,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        spec.session.clone_from(&session);
        // Its reporter reads this only as it ends.
        let added = write_spec(&dir, &spec)
            .context("writing job.json")
            .and_then(|()| {
                let held = Held {
                    dir: dir.clone(),
                    _lock: lock,
                    spec,
                    tool: job.tool,
                };
                server.take_up(&self.jobs, held, None, Some(&self.key))
            });
        match added {
            Ok(added) => Ok(Started {
                job: added.id.clone(),
                session,
            }),
            // Not followed, so not left running.
            Err(e) => {
                let _ = job.tool.stop(&session, true, STOP_GRACE);
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }
}

fn write_spec(dir: &Path, spec: &Spec) -> std::io::Result<()> {
    job::replace(dir, "job.json", &serde_json::to_vec_pretty(spec)?)
}

/// Start `job`'s TUI in its session, reporting to folder `dir`; its id.
fn start(job: &SessionJob, dir: &Path) -> Result<String> {
    let run = |label: Option<&str>| {
        let mut watch = clone(&job.watch);
        watch.env(job::ENV, dir);
        job.tool.start_detached(&job.screen, label, watch)
    };
    let label = job
        .label
        .as_deref()
        .map(session_label)
        .filter(|l| !l.is_empty());
    let started = match label.as_deref() {
        // A label a live session holds: the next number instead.
        Some(label) => run(Some(label)).or_else(|e| match e.kind() {
            std::io::ErrorKind::AddrInUse => run(None),
            _ => Err(e),
        }),
        None => run(None),
    };
    started.with_context(|| format!("starting a {} session", job.tool.name()))
}

/// `label` as a session can have it: letters, digits, `-`, `_` and `.`
/// kept, every other character `-`, no `.` first, at most [`LABEL_MAX`].
fn session_label(label: &str) -> String {
    let kept: String = label
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') => c,
            _ => '-',
        })
        .take(LABEL_MAX)
        .collect();
    kept.trim_start_matches('.').to_string()
}

/// The longest label a session job asks for.
const LABEL_MAX: usize = 64;

/// A copy of `cmd`: its program, arguments, environment and folder.
fn clone(cmd: &Command) -> Command {
    let mut c = Command::new(cmd.get_program());
    c.args(cmd.get_args());
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    if let Some(d) = cmd.get_current_dir() {
        c.current_dir(d);
    }
    c
}

fn epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How long ago `path` was last written; None when it is not there.
fn age(path: &Path) -> Option<Duration> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

/// The lock on job folder `dir` that its follower holds while it follows
/// it, when no one holds it; the system lets it go with the process.
fn try_lock(dir: &Path) -> std::io::Result<Option<File>> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("following"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let e = std::io::Error::last_os_error();
    match e.kind() {
        std::io::ErrorKind::WouldBlock => Ok(None),
        _ => Err(e),
    }
}

/// A session job followed by this server; its lock goes with it.
struct Following {
    job: Arc<Job>,
    held: Held,
    feed: Option<Arc<Feed>>,
    steer: Arc<Mutex<Steer>>,
    ended: Arc<AtomicBool>,
}

impl Following {
    /// Follow the job: its log into its tail and the feed, a person's stop
    /// to its session, and its end, said by its TUI or, when its session
    /// is gone without a word, by this server. Left to the next server if
    /// this one ends first.
    fn run(self) {
        let (job, dir) = (&self.job, &self.held.dir);
        let mut log = Tail::new(job::log_file(dir));
        let stopped = || lock(&self.steer).stopped.contains(&job.id);
        let say = |line: &str| {
            job.line(line);
            if let Some(f) = &self.feed {
                f.emit(Body::Progress {
                    id: job.id.clone(),
                    message: line.into(),
                });
            }
        };
        let mut asked = std::time::Instant::now();
        // Ended already: read at once, not at the first look of the loop.
        let end = match job::read_end(dir) {
            Some(end) => {
                log.each(say);
                end
            }
            None => loop {
                if self.ended.load(Ordering::Relaxed) {
                    return;
                }
                // Its last lines come before its end: looked for when it says
                // something, and now and then.
                let said = log.each(say);
                if said || asked.elapsed() >= ASK_EVERY {
                    if let Some(end) = job::read_end(dir) {
                        log.each(say);
                        break end;
                    }
                }
                if stopped() {
                    job.ask_to_stop();
                }
                if asked.elapsed() >= ASK_EVERY {
                    asked = std::time::Instant::now();
                    // Gone (a busy host is not): what it said as it went is
                    // read, then how it ended.
                    if matches!(self.held.tool.find(&self.held.spec.session), Ok(None)) {
                        log.each(say);
                        break self.ended_silent();
                    }
                }
                std::thread::sleep(POLL);
            },
        };
        let (state, text) = match end {
            End::Done(out) => {
                self.show_record();
                (State::Done, out)
            }
            End::Failed(why) => (State::Failed, why),
            End::Cancelled => (State::Cancelled, "cancelled".to_string()),
        };
        job.conclude(self.feed.as_deref(), state, text, stopped());
        // A server saw it end: the next one need not take it up.
        let _ = std::fs::write(dir.join("seen"), "");
    }

    /// How it ended, its session gone: as it said, else cancelled when a
    /// server asked it to stop (it may have gone before it could say so)
    /// or failed, said and recorded for it.
    fn ended_silent(&self) -> End {
        let dir = &self.held.dir;
        let end = if dir.join("stop").exists() {
            End::Cancelled
        } else {
            End::Failed(format!(
                "its session {} ended without saying how",
                self.held.spec.session
            ))
        };
        job::read_end(dir)
            .or_else(|| job::conclude(dir, end.clone(), self.feed.clone()))
            .unwrap_or(end)
    }

    /// The step the job recorded as it succeeded, shown to whoever follows
    /// the server, as a plain job's record is.
    fn show_record(&self) {
        let spec = &self.held.spec;
        let (Some(f), Some(bare), Some(p)) = (&self.feed, &spec.record, spec.records(None)) else {
            return;
        };
        f.emit(Body::Record {
            line: p.guarded(bare, &spec.outputs),
            command: Some(bare.clone()),
        });
    }
}

/// `name` for the life of the process, once each: a session tool's name
/// read back from a job's folder.
fn interned(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = lock(&NAMES);
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(n);
    n
}

/// A log file read as it grows, a whole line at a time.
struct Tail {
    path: PathBuf,
    file: Option<File>,
    at: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            at: 0,
            partial: Vec::new(),
        }
    }

    /// Each whole line written since last time; whether there were any.
    fn each(&mut self, mut f: impl FnMut(&str)) -> bool {
        if self.file.is_none() {
            self.file = File::open(&self.path).ok();
        }
        let Some(file) = &mut self.file else {
            return false;
        };
        let grown = file.metadata().is_ok_and(|m| m.len() > self.at);
        if !grown || file.seek(SeekFrom::Start(self.at)).is_err() {
            return false;
        }
        let Ok(n) = file.read_to_end(&mut self.partial) else {
            return false;
        };
        self.at += n as u64;
        let mut any = false;
        while let Some(end) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            f(String::from_utf8_lossy(&line[..end]).as_ref());
            any = true;
        }
        any
    }
}
