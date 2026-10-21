//! Jobs: long steps a tool starts in the background ([`crate::Call::spawn_job`])
//! so its call returns at once with an id, for runs longer than a client
//! waits on one call (a model fit, a large pass). The agent follows them with
//! the built-in `job_status`, `job_wait` and `job_cancel`; a person following
//! the server sees each as a call of its own (`job-N`) and can stop it. A
//! job that succeeds is recorded like any step (`runall.sh`, `INDEX.tsv`)
//! when it ends. A plain job ends with its server; one a tool starts in a
//! session of its own TUI ([`crate::Call::start_session_job`]) runs on, and
//! the next server takes it up.

use crate::absolute;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Lines of a job's log kept for `job_status`.
const TAIL: usize = 20;
/// The longest `job_wait` blocks the server.
pub(crate) const WAIT_MAX: u64 = 120;
/// What a job's output is cut to in a status.
const OUT_CAP: usize = 4096;

/// How a job stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }
}

/// A job that runs in a session of the tool's TUI: what a person types to
/// watch it, and how it is stopped.
pub(crate) struct Session {
    /// The session's id.
    pub id: String,
    /// The command that attaches to it (`tool attach ID`).
    pub attach: String,
    /// Ask the session to stop; it returns at once.
    pub stop: Box<dyn Fn() + Send + Sync>,
}

struct Progress {
    state: State,
    tail: VecDeque<String>,
    /// The child's stdout once it is done; the error once it failed.
    out: String,
    ms: Option<u64>,
}

/// One job.
pub(crate) struct Job {
    pub(crate) id: String,
    tool: String,
    summary: String,
    /// What it writes, absolute as given, each with its [`hold_key`]:
    /// reserved while it runs.
    outputs: Vec<(PathBuf, PathBuf)>,
    started_at: String,
    started: Instant,
    progress: Mutex<Progress>,
    ended: Condvar,
    cancel: AtomicBool,
    session: Option<Session>,
}

impl Job {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Ask it to stop: a session job's session is asked, a child is killed
    /// by its thread.
    pub(crate) fn ask_to_stop(&self) {
        if !self.cancel.swap(true, Ordering::Relaxed) {
            if let Some(s) = &self.session {
                (s.stop)();
            }
        }
    }

    pub(crate) fn line(&self, line: &str) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if p.tail.len() == TAIL {
            p.tail.pop_front();
        }
        p.tail.push_back(line.into());
    }

    /// The job is over, as `state` and `text` say (its output, or why it
    /// did not succeed): told to whoever follows the server in `feed`,
    /// with who stopped it (`by_user`, else the client) when it was, then
    /// ended.
    pub(crate) fn conclude(
        &self,
        feed: Option<&crate::watch::Feed>,
        state: State,
        text: String,
        by_user: bool,
    ) {
        use crate::watch::Body;
        if let Some(f) = feed {
            if state == State::Cancelled {
                let by = if by_user { "user" } else { "client" };
                f.emit(Body::Cancelled {
                    id: self.id.clone(),
                    by: by.into(),
                });
            }
            let ok = state == State::Done;
            let result = json!({ "content": [{ "type": "text", "text": text }], "isError": !ok });
            f.result(&self.id, &result, self.ms());
        }
        self.end(state, text);
    }

    /// The job is over: `state`, with `out` (stdout, or the error).
    pub(crate) fn end(&self, state: State, out: String) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.state = state;
        p.out = out;
        p.ms = Some(self.started.elapsed().as_millis() as u64);
        self.ended.notify_all();
    }

    /// How long it ran, or has run so far, in milliseconds.
    pub(crate) fn ms(&self) -> u64 {
        let p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.ms.unwrap_or_else(|| self.started.elapsed().as_millis() as u64)
    }

    fn running(&self) -> bool {
        let p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.state == State::Running
    }

    /// The job as `job_status` reports it.
    pub(crate) fn status(&self) -> Value {
        let p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = p.out.clone();
        if out.len() > OUT_CAP {
            let mut cut = OUT_CAP;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push('…');
        }
        let mut v = json!({
            "id": self.id,
            "tool": self.tool,
            "summary": self.summary,
            "state": p.state.name(),
            "started": self.started_at,
            "seconds": p.ms.unwrap_or_else(|| self.started.elapsed().as_millis() as u64) / 1000,
            "last_lines": p.tail.iter().rev().take(5).rev().collect::<Vec<_>>(),
        });
        if let Some(s) = &self.session {
            v["session"] = json!(s.id);
            v["attach"] = json!(s.attach);
        }
        match p.state {
            // Asked to stop, and stopping.
            State::Running if self.cancelled() => v["stopping"] = json!(true),
            State::Running => {}
            State::Done => v["output"] = json!(out),
            State::Failed | State::Cancelled => v["error"] = json!(out),
        }
        v
    }
}

/// A server's jobs.
#[derive(Default)]
pub(crate) struct Jobs {
    next: AtomicU64,
    all: Mutex<BTreeMap<u64, Arc<Job>>>,
}

impl Jobs {
    /// A new running job of `tool`'s, writing `outputs`: `summary` says what
    /// it makes; one running in `session` when given, started `ago` (at
    /// the time given, that long ago; now when None).
    pub(crate) fn add(
        &self,
        tool: &str,
        summary: &str,
        outputs: &[PathBuf],
        session: Option<Session>,
        ago: Option<(String, Duration)>,
    ) -> Arc<Job> {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (started_at, started) = match ago {
            Some((at, ago)) => (
                at,
                Instant::now().checked_sub(ago).unwrap_or_else(Instant::now),
            ),
            None => (crate::project::rfc3339(), Instant::now()),
        };
        let job = Arc::new(Job {
            id: format!("job-{n}"),
            tool: tool.into(),
            summary: summary.into(),
            outputs: outputs.iter().map(|o| (absolute(o), hold_key(o))).collect(),
            started_at,
            started,
            progress: Mutex::new(Progress {
                state: State::Running,
                tail: VecDeque::new(),
                out: String::new(),
                ms: None,
            }),
            ended: Condvar::new(),
            cancel: AtomicBool::new(false),
            session,
        });
        let mut all = self.all.lock().unwrap_or_else(|e| e.into_inner());
        all.insert(n, job.clone());
        job
    }

    /// A running job writing one of `outputs` (the same file, or one inside
    /// the other, however either is spelled): its id and the path as that
    /// job gave it, made absolute. A job's outputs are free again once it
    /// ends, however it ends.
    pub(crate) fn holder(&self, outputs: &[PathBuf]) -> Option<(String, PathBuf)> {
        let running: Vec<Arc<Job>> = {
            let all = self.all.lock().unwrap_or_else(|e| e.into_inner());
            all.values().filter(|j| j.running()).cloned().collect()
        };
        if running.is_empty() {
            return None;
        }
        // Resolved off the lock: it reads the file system.
        let wanted: Vec<PathBuf> = outputs.iter().map(|o| hold_key(o)).collect();
        running.iter().find_map(|j| {
            let (given, _) = j.outputs.iter().find(|(_, held)| {
                wanted
                    .iter()
                    .any(|w| w.starts_with(held) || held.starts_with(w))
            })?;
            Some((j.id.clone(), given.clone()))
        })
    }

    /// Whether this server has job `id`.
    pub(crate) fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    fn get(&self, id: &str) -> Option<Arc<Job>> {
        let n: u64 = id.strip_prefix("job-")?.parse().ok()?;
        let all = self.all.lock().unwrap_or_else(|e| e.into_inner());
        all.get(&n).cloned()
    }

    fn unknown(id: &str) -> anyhow::Error {
        anyhow::anyhow!("no job {id}: job_status lists them")
    }

    /// `job_status`: one job, or all of them, newest last.
    pub(crate) fn status(&self, id: Option<&str>) -> anyhow::Result<Value> {
        match id {
            Some(id) => Ok(self.get(id).ok_or_else(|| Self::unknown(id))?.status()),
            None => {
                let all = self.all.lock().unwrap_or_else(|e| e.into_inner());
                Ok(json!({ "jobs": all.values().map(|j| j.status()).collect::<Vec<_>>() }))
            }
        }
    }

    /// `job_wait`: until job `id` ends, or `seconds` (at most
    /// [`WAIT_MAX`]) pass; its status either way.
    pub(crate) fn wait(&self, id: &str, seconds: u64) -> anyhow::Result<Value> {
        let job = self.get(id).ok_or_else(|| Self::unknown(id))?;
        let deadline = Instant::now() + Duration::from_secs(seconds.min(WAIT_MAX));
        let mut p = job.progress.lock().unwrap_or_else(|e| e.into_inner());
        while p.state == State::Running {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            p = job
                .ended
                .wait_timeout(p, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        drop(p);
        Ok(job.status())
    }

    /// `job_cancel`: ask job `id` to stop; its child is killed, or its
    /// session stopped, shortly.
    pub(crate) fn cancel(&self, id: &str) -> anyhow::Result<Value> {
        let job = self.get(id).ok_or_else(|| Self::unknown(id))?;
        if job.running() {
            job.ask_to_stop();
        }
        Ok(job.status())
    }

    /// The server is ending: stop the plain jobs still running, and give
    /// them a moment to. Session jobs run on.
    pub(crate) fn stop_all(&self) {
        let running: Vec<Arc<Job>> = {
            let all = self.all.lock().unwrap_or_else(|e| e.into_inner());
            all.values()
                .filter(|j| j.running() && j.session.is_none())
                .cloned()
                .collect()
        };
        for j in &running {
            j.ask_to_stop();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while running.iter().any(|j| j.running()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Links followed before a path is taken as it is spelled (a loop).
const LINKS_MAX: usize = 40;

/// The file `p` names, however it is spelled, for comparing outputs only
/// (what is recorded stays as given): `p` absolute, resolved a component
/// at a time as the system would, so each link is followed (also one to a
/// folder not made yet) before a `..` after it; a part that does not exist
/// yet is taken as spelled. Two spellings of one file (`a/../fit` and
/// `fit`, a linked folder and its target, `/var` and `/private/var` on
/// macOS) get one key, before and after the job makes its folders. Case
/// is not folded: on a case-insensitive file system `Fit` and `fit` are
/// two keys.
pub(crate) fn hold_key(p: &Path) -> PathBuf {
    resolve(&absolute(p), &mut 0)
}

fn resolve(p: &Path, links: &mut usize) -> PathBuf {
    use std::path::Component;
    let mut real = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => real.push(c),
            Component::CurDir => {}
            // `real` holds no links, so its parent is where `..` goes.
            Component::ParentDir => {
                real.pop();
            }
            Component::Normal(name) => {
                let next = real.join(name);
                // Not a link, or not there yet: no target.
                real = match std::fs::read_link(&next).ok() {
                    Some(t) if *links < LINKS_MAX => {
                        *links += 1;
                        resolve(&real.join(t), links)
                    }
                    _ => next,
                };
            }
        }
    }
    real
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn held_by(jobs: &Jobs, out: &Path) -> Option<String> {
        jobs.holder(&[out.to_path_buf()]).map(|(id, _)| id)
    }

    #[test]
    fn one_out_spelled_two_ways_is_held_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("runs")).unwrap();
        let jobs = Jobs::default();
        jobs.add("fit", "a fit", &[root.join("runs/../fit")], None, None);
        assert_eq!(held_by(&jobs, &root.join("fit")).as_deref(), Some("job-1"));
        assert_eq!(
            held_by(&jobs, &root.join("./fit/x")).as_deref(),
            Some("job-1")
        );
        assert_eq!(held_by(&jobs, &root.join("other")), None);
        // The holder is named as it was given.
        let (_, given) = jobs.holder(&[root.join("fit")]).unwrap();
        assert_eq!(given, root.join("runs/../fit"));
    }

    #[test]
    #[cfg(unix)]
    fn a_link_is_followed_before_the_dots_after_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("deep/real")).unwrap();
        symlink(root.join("deep/real"), root.join("link")).unwrap();
        // `link/../x` is `deep/x`, as the system resolves it, not `x`.
        assert_eq!(
            hold_key(&root.join("link/../x")),
            hold_key(&root.join("deep/x"))
        );
        assert_ne!(hold_key(&root.join("link/../x")), hold_key(&root.join("x")));
    }

    #[test]
    #[cfg(unix)]
    fn a_linked_folder_is_its_target_before_and_after_it_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A relative link to a folder not made yet.
        symlink("target", root.join("link")).unwrap();
        let jobs = Jobs::default();
        jobs.add("fit", "a fit", &[root.join("link/fit")], None, None);
        assert_eq!(
            held_by(&jobs, &root.join("target/fit")).as_deref(),
            Some("job-1")
        );
        std::fs::create_dir(root.join("target")).unwrap();
        for out in ["target/fit", "link/fit"] {
            assert_eq!(held_by(&jobs, &root.join(out)).as_deref(), Some("job-1"));
        }
        // The key matches the system's own resolution once it exists.
        std::fs::write(root.join("target/fit"), "").unwrap();
        assert_eq!(
            hold_key(&root.join("link/fit")),
            root.join("link/fit").canonicalize().unwrap()
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_link_loop_ends() {
        let tmp = tempfile::tempdir().unwrap();
        // The real folder, so the loop's links are the only ones (macOS's
        // temp folder sits under the `/var` link).
        let root = tmp.path().canonicalize().unwrap();
        symlink(root.join("b"), root.join("a")).unwrap();
        symlink(root.join("a"), root.join("b")).unwrap();
        // Followed up to the cap, then taken as the link it stopped at.
        let key = hold_key(&root.join("a/fit"));
        assert!(
            key == root.join("a/fit") || key == root.join("b/fit"),
            "{}",
            key.display()
        );
        assert_eq!(key, hold_key(&root.join("a/fit")));
    }
}
