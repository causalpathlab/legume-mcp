//! A session job's own side: the TUI a tool runs in a detached session for
//! a background step (`Call::start_session_job`, feature `sessions`) tells
//! legume-mcp what it does through a [`Reporter`], which it finds in its
//! environment.
//!
//! Each session job has a folder in `jobs` beside the servers' watch
//! folders (there even when watching is off), kept apart from any server's
//! so it outlives the server that started it:
//!
//! ```text
//! <base>/<server>-<time>-<n>/
//!   job.json   what the job is: its server, project, session, step and outputs
//!   log        its log, a line at a time, as the TUI says it
//!   ending     taken by whoever says how it ended first: that one records it
//!   end.json   how it ended: by the TUI, or by a server that found its
//!              session gone without a word
//!   stop       a server asked it to stop
//!   following  locked by the server that follows it
//!   seen       a server saw it end
//! ```
//!
//! The step is recorded as the job ends, as `Call::spawn_job` records one:
//! its line in `runall.sh` and its outputs in `INDEX.tsv` when it
//! succeeded, a commented line saying why when it did not. The TUI does it
//! ([`Reporter::finish`]), so it is recorded whether or not a server is
//! still there to see it end.

use crate::project::Project;
use crate::watch::Feed;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Set in a session job's environment, to its folder.
pub const ENV: &str = "LEGUME_MCP_JOB";

/// How long one that did not say first waits for the word of the one that
/// did.
const WORD: Duration = Duration::from_secs(5);

/// What a session job is, as its server wrote it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Spec {
    /// The server that started it: its name and version.
    pub server: String,
    pub version: String,
    /// The project whose records it writes.
    pub project: Option<PathBuf>,
    /// The MCP tool whose step it is.
    pub tool: String,
    /// The legume-tui tool whose session it runs in, and the session's id
    /// once it is up.
    pub tui: String,
    pub session: String,
    pub summary: String,
    /// What it writes, absolute.
    pub outputs: Vec<PathBuf>,
    /// The step as the driver script repeats it.
    pub record: Option<String>,
    /// When it started: RFC 3339, and seconds since the epoch.
    pub started: String,
    pub epoch: u64,
}

impl Spec {
    /// The project's records the job writes, telling `feed` of them.
    pub(crate) fn records(&self, feed: Option<Arc<Feed>>) -> Option<Arc<Project>> {
        let root = self.project.clone()?;
        let p = Project::new(root, &self.server, &self.version).watched(feed);
        Some(Arc::new(p))
    }
}

/// How a session job ended, as `end.json` has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "text", rename_all = "snake_case")]
pub enum End {
    /// It succeeded, with what it says it made.
    Done(String),
    /// It failed, and why.
    Failed(String),
    /// It was stopped before it was done.
    Cancelled,
}

pub(crate) fn log_file(dir: &Path) -> PathBuf {
    dir.join("log")
}

fn spec_file(dir: &Path) -> PathBuf {
    dir.join("job.json")
}

fn end_file(dir: &Path) -> PathBuf {
    dir.join("end.json")
}

pub(crate) fn read_spec(dir: &Path) -> Option<Spec> {
    serde_json::from_slice(&std::fs::read(spec_file(dir)).ok()?).ok()
}

/// How job `dir` ended, once it has said.
pub(crate) fn read_end(dir: &Path) -> Option<End> {
    serde_json::from_slice(&std::fs::read(end_file(dir)).ok()?).ok()
}

/// `bytes` as `name` in `dir`, replaced whole: a reader never sees half.
pub(crate) fn replace(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, dir.join(name))
}

/// The job in `dir` ended as `end`: unless another said first, recorded as
/// a step (one that ran only when it is done and every output is there; a
/// job done without one failed), with what it records told to `feed`,
/// then said in `end.json`. How it ended, as said, by whoever said first.
pub(crate) fn conclude(dir: &Path, end: End, feed: Option<Arc<Feed>>) -> Option<End> {
    let first = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("ending"));
    match first {
        Ok(_) => {}
        // Another said first: its word, once it is written.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let start = std::time::Instant::now();
            while start.elapsed() < WORD {
                if let Some(end) = read_end(dir) {
                    return Some(end);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            return read_end(dir);
        }
        Err(e) => {
            log::warn!("session job in {}: {e}", dir.display());
            return None;
        }
    }
    let spec = read_spec(dir);
    let end = match (end, &spec) {
        (End::Done(out), Some(spec)) => match crate::made(&spec.outputs) {
            Ok(()) => End::Done(out),
            Err(e) => End::Failed(format!("{e:#}")),
        },
        (end, _) => end,
    };
    // Recorded before it is said: whoever hears the end finds the record.
    if let Some(spec) = &spec {
        let result = match &end {
            End::Done(out) => Ok(out.clone()),
            End::Failed(why) => Err(anyhow::anyhow!("{why}")),
            End::Cancelled => Err(anyhow::anyhow!("cancelled")),
        };
        crate::record_step(
            &spec.records(feed),
            &spec.tool,
            spec.record.clone(),
            &spec.outputs,
            &spec.summary,
            &result,
        );
    }
    let said = serde_json::to_vec(&end).map_err(std::io::Error::from);
    if let Err(e) = said.and_then(|b| replace(dir, "end.json", &b)) {
        log::warn!("session job in {}: writing end.json: {e}", dir.display());
    }
    Some(end)
}

/// The session job this process is, as its TUI tells legume-mcp of it.
pub struct Reporter {
    dir: PathBuf,
    log: Mutex<std::fs::File>,
    /// It has said how the job ended.
    finished: std::sync::atomic::AtomicBool,
}

impl Reporter {
    /// The session job this process runs, when it is one ([`ENV`] names
    /// its folder); None otherwise, as when the TUI is run by hand.
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os(ENV)?);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file(&dir))
            .inspect_err(|e| log::warn!("session job log in {}: {e}", dir.display()))
            .ok()?;
        Some(Self {
            dir,
            log: Mutex::new(log),
            finished: Default::default(),
        })
    }

    /// A line of the job's log, for `job_status` and whoever follows the
    /// server.
    pub fn line(&self, line: &str) {
        let mut log = crate::lock(&self.log);
        // One write a line, so a reader never sees half of one.
        let _ = log.write_all(format!("{}\n", line.trim_end_matches('\n')).as_bytes());
    }

    /// The job is over, as `end` says: recorded as a step (only as one that
    /// ran when it is done and every output is there), then told to the
    /// server. A job done without one of its outputs failed. Said once:
    /// a later word changes nothing, and lines after it go nowhere.
    pub fn finish(&self, end: End) {
        if !self
            .finished
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            conclude(&self.dir, end, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_word_changes_nothing_and_does_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter {
            dir: dir.path().to_path_buf(),
            log: Mutex::new(std::fs::File::create(log_file(dir.path())).unwrap()),
            finished: Default::default(),
        };
        reporter.finish(End::Failed("first".into()));
        // As if end.json could not be written: the second word must not
        // wait for it.
        std::fs::remove_file(end_file(dir.path())).unwrap();
        let start = std::time::Instant::now();
        reporter.finish(End::Done("second".into()));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(read_end(dir.path()), None);
    }
}
