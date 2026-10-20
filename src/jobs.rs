//! Jobs: long steps a tool starts in the background ([`crate::Call::spawn_job`])
//! so its call returns at once with an id, for runs longer than a client
//! waits on one call (a model fit, a large pass). The agent follows them with
//! the built-in `job_status`, `job_wait` and `job_cancel`; a person following
//! the server sees each as a call of its own (`job-N`) and can stop it. A
//! job that succeeds is recorded like any step (`runall.sh`, `INDEX.tsv`)
//! when it ends. Jobs end with their server: a step longer than a client
//! session is the tool's own command line's to run.

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
    /// What it writes, absolute: reserved while it runs.
    outputs: Vec<PathBuf>,
    started_at: String,
    started: Instant,
    progress: Mutex<Progress>,
    ended: Condvar,
    cancel: AtomicBool,
}

impl Job {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub(crate) fn line(&self, line: &str) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if p.tail.len() == TAIL {
            p.tail.pop_front();
        }
        p.tail.push_back(line.into());
    }

    /// The job is over: `state`, with `out` (stdout, or the error).
    pub(crate) fn end(&self, state: State, out: String) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.state = state;
        p.out = out;
        p.ms = Some(self.started.elapsed().as_millis() as u64);
        self.ended.notify_all();
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
        match p.state {
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
    /// it makes.
    pub(crate) fn add(&self, tool: &str, summary: &str, outputs: &[PathBuf]) -> Arc<Job> {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let job = Arc::new(Job {
            id: format!("job-{n}"),
            tool: tool.into(),
            summary: summary.into(),
            outputs: outputs.iter().map(|o| absolute(o)).collect(),
            started_at: crate::project::rfc3339(),
            started: Instant::now(),
            progress: Mutex::new(Progress {
                state: State::Running,
                tail: VecDeque::new(),
                out: String::new(),
                ms: None,
            }),
            ended: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        let mut all = self.all.lock().unwrap_or_else(|e| e.into_inner());
        all.insert(n, job.clone());
        job
    }

    /// A running job writing one of `outputs` (the same path, or one inside
    /// the other): its id and the path. A job's outputs are free again once
    /// it ends, however it ends.
    pub(crate) fn holder(&self, outputs: &[PathBuf]) -> Option<(String, PathBuf)> {
        let wanted: Vec<PathBuf> = outputs.iter().map(|o| absolute(o)).collect();
        let all = self.all.lock().unwrap_or_else(|e| e.into_inner());
        all.values().filter(|j| j.running()).find_map(|j| {
            let hit = j.outputs.iter().find(|held| {
                wanted
                    .iter()
                    .any(|w| w.starts_with(held) || held.starts_with(w))
            })?;
            Some((j.id.clone(), hit.clone()))
        })
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

    /// `job_cancel`: ask job `id` to stop; its child is killed shortly.
    pub(crate) fn cancel(&self, id: &str) -> anyhow::Result<Value> {
        let job = self.get(id).ok_or_else(|| Self::unknown(id))?;
        job.cancel.store(true, Ordering::Relaxed);
        Ok(job.status())
    }

    /// The server is ending: stop the jobs still running, and give them a
    /// moment to.
    pub(crate) fn stop_all(&self) {
        let running: Vec<Arc<Job>> = {
            let all = self.all.lock().unwrap_or_else(|e| e.into_inner());
            all.values().filter(|j| j.running()).cloned().collect()
        };
        for j in &running {
            j.cancel.store(true, Ordering::Relaxed);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while running.iter().any(|j| j.running()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `p` absolute, against the working directory, without touching the disk.
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}
