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
//! succeeded (with the facts the TUI gives [`Reporter::finish_with`]), a
//! commented line saying why when it did not, and its [`crate::Why`] when
//! it was started with one. The TUI does it ([`Reporter::finish`]), so it
//! is recorded whether or not a server is still there to see it end.

use crate::project::Project;
use crate::watch::Feed;
use crate::{Fact, Step};
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
    /// What it reads, absolute (none from a server before 0.5.0).
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
    /// The step as the driver script repeats it.
    pub record: Option<String>,
    /// Why it runs as it does (none from a server before 0.6.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<crate::Why>,
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

/// The file a session job's stages are in, replaced whole each time.
const PROGRESS: &str = "progress.json";

/// How long ago `path` was last written; None when it is not there.
#[cfg(all(feature = "sessions", unix))]
pub(crate) fn age(path: &Path) -> Option<Duration> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

/// One counted stage of a job (a progress bar), as it last reported it: a
/// session job's TUI with [`Reporter::progress`], a child run off a
/// terminal in a [`PREFIX`] line. Shown to the agent by `job_status` and
/// `job_wait`, where the drawn bars never reach.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    /// What it counts (a phase's name, `"blocks"`); may be empty.
    pub label: String,
    pub pos: u64,
    /// `None` for a stage with no known end.
    pub len: Option<u64>,
    pub elapsed_s: u64,
    /// The estimated time left; `None` when there is no end to estimate.
    pub eta_s: Option<u64>,
}

impl Stage {
    /// `label pos/len (eta 6m52s)`, as a person reads it.
    pub fn say(&self) -> String {
        let count = match self.len {
            Some(len) => format!("{}/{len}", self.pos),
            None => self.pos.to_string(),
        };
        let eta = self
            .eta_s
            .map(|s| format!(" (eta {}m{:02}s)", s / 60, s % 60))
            .unwrap_or_default();
        format!("{} {count}{eta}", self.label).trim().to_string()
    }
}

/// What starts a child's progress line on its stderr, as legume-numeric's
/// bars say themselves off a terminal; one JSON object follows:
///
/// ```text
/// legume-progress {"id":3,"label":"blocks","pos":12,"len":40,"elapsed_s":31,"eta_s":72,"done":false}
/// ```
///
/// `id` tells a child's bars apart, `len` and `eta_s` may be `null`, and
/// `done` is true on a bar's last line. Any tool may print them.
pub const PREFIX: &str = "legume-progress ";

/// A child's progress line as [`PREFIX`] says it.
#[derive(Deserialize)]
struct Line {
    id: u64,
    #[serde(default)]
    label: String,
    pos: u64,
    len: Option<u64>,
    #[serde(default)]
    elapsed_s: u64,
    eta_s: Option<u64>,
    #[serde(default)]
    done: bool,
}

/// A child's stages, as its progress lines (see [`PREFIX`]) said them.
#[derive(Default)]
pub(crate) struct Stages(std::collections::BTreeMap<u64, Stage>);

impl Stages {
    /// `line` read, when it is a progress line: its stage as it now stands
    /// (gone from the stages when the line was its last).
    pub(crate) fn read(&mut self, line: &str) -> Option<Stage> {
        let l: Line = serde_json::from_str(line.strip_prefix(PREFIX)?).ok()?;
        let stage = Stage {
            label: l.label,
            pos: l.pos,
            len: l.len,
            elapsed_s: l.elapsed_s,
            eta_s: l.eta_s,
        };
        if l.done {
            self.0.remove(&l.id);
        } else {
            self.0.insert(l.id, stage.clone());
        }
        Some(stage)
    }

    /// The stages running, in the order they began.
    pub(crate) fn running(&self) -> Vec<Stage> {
        self.0.values().cloned().collect()
    }
}

/// The stages job `dir` last reported; none when it has not reported any,
/// or not lately: a job that stopped saying where it stands (it hangs, or
/// its TUI does) would have an old count read as current.
#[cfg(all(feature = "sessions", unix))]
pub(crate) fn read_progress(dir: &Path) -> Vec<Stage> {
    const STALE: Duration = Duration::from_secs(30);
    let path = dir.join(PROGRESS);
    if !age(&path).is_some_and(|a| a < STALE) {
        return Vec::new();
    }
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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
    // Numbered too, so two threads of this process never share one.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}", std::process::id()));
    let replaced = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, dir.join(name)));
    if replaced.is_err() {
        // Not left behind half written, or written and not put in place.
        let _ = std::fs::remove_file(&tmp);
    }
    replaced
}

/// The job in `dir` ended as `end`: unless another said first, recorded as
/// a step (one that ran only when it is done and every output is there; a
/// job done without one failed), with what it records told to `feed`,
/// then said in `end.json`. How it ended, as said, by whoever said first.
pub(crate) fn conclude(
    dir: &Path,
    end: End,
    facts: Vec<Fact>,
    feed: Option<Arc<Feed>>,
) -> Option<End> {
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
        let mut step = Step::new(&spec.outputs, &spec.summary).inputs(&spec.inputs);
        step.facts = facts;
        step.why.clone_from(&spec.why);
        step.started = started(&spec.started, &crate::project::rfc3339());
        crate::record_step(
            &spec.records(feed),
            &spec.tool,
            spec.record.clone(),
            &step,
            &result,
        );
    }
    let said = serde_json::to_vec(&end).map_err(std::io::Error::from);
    if let Err(e) = said.and_then(|b| replace(dir, "end.json", &b)) {
        log::warn!("session job in {}: writing end.json: {e}", dir.display());
    }
    Some(end)
}

/// When a job that began at `started` (RFC 3339) began, as its end at
/// `now` records it: `10:02` the same day, `2026-11-24 23:50` a day before.
fn started(started: &str, now: &str) -> Option<String> {
    let t = started.get(..16)?;
    if now.get(..10) == t.get(..10) {
        t.get(11..).map(str::to_string)
    } else {
        Some(t.replacen('T', " ", 1))
    }
}

/// The session job this process is, as its TUI tells legume-mcp of it.
pub struct Reporter {
    dir: PathBuf,
    log: Mutex<std::fs::File>,
    /// It has said how the job ended.
    finished: std::sync::atomic::AtomicBool,
    /// Its last word on progress was "no stages": not said again.
    idle: std::sync::atomic::AtomicBool,
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
        // What an earlier process in this folder said is not this one's.
        let _ = std::fs::remove_file(dir.join(PROGRESS));
        Some(Self {
            dir,
            log: Mutex::new(log),
            finished: Default::default(),
            idle: Default::default(),
        })
    }

    /// A line of the job's log, for `job_status` and whoever follows the
    /// server.
    pub fn line(&self, line: &str) {
        let mut log = crate::lock(&self.log);
        // One write a line, so a reader never sees half of one.
        let _ = log.write_all(format!("{}\n", line.trim_end_matches('\n')).as_bytes());
    }

    /// Where the job's counted stages stand now, replacing what it said last:
    /// `job_status` and `job_wait` show them while it runs. Nothing once the
    /// job has said how it ended.
    pub fn progress(&self, stages: &[Stage]) {
        use std::sync::atomic::Ordering::SeqCst;
        let idle = stages.is_empty();
        if self.finished.load(SeqCst) || (idle && self.idle.swap(true, SeqCst)) {
            return;
        }
        self.idle.store(idle, SeqCst);
        let said = serde_json::to_vec(stages).map_err(std::io::Error::from);
        if let Err(e) = said.and_then(|b| replace(&self.dir, PROGRESS, &b)) {
            log::warn!(
                "session job in {}: writing {PROGRESS}: {e}",
                self.dir.display()
            );
        }
    }

    /// The job is over, as `end` says: recorded as a step (only as one that
    /// ran when it is done and every output is there), then told to the
    /// server. A job done without one of its outputs failed. Said once:
    /// a later word changes nothing, and lines after it go nowhere.
    pub fn finish(&self, end: End) {
        self.finish_with(end, Vec::new());
    }

    /// [`Self::finish`] with the key numbers the step made, for its row in
    /// `INDEX.tsv` (kept only when it is done).
    pub fn finish_with(&self, end: End, facts: Vec<Fact>) {
        if !self
            .finished
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            conclude(&self.dir, end, facts, None);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stage as `senna run` reports a fit's, at `pos` of 1000.
    pub(crate) fn stage(pos: u64) -> Stage {
        Stage {
            label: "Phase 1 (hier)".into(),
            pos,
            len: Some(1000),
            elapsed_s: 20,
            eta_s: Some(412),
        }
    }

    /// A session job's why is kept in its job.json and recorded as it
    /// ends, with when it started; a job.json without one still reads.
    #[test]
    fn a_session_jobs_why_is_recorded_as_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (root, job) = (dir.path().join("proj"), dir.path().join("job"));
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let out = root.join("ode.out");
        std::fs::write(&out, "").unwrap();
        let spec = Spec {
            server: "senna".into(),
            version: "0".into(),
            project: Some(root.clone()),
            tool: "ode".into(),
            tui: "senna".into(),
            session: "senna-ode-1".into(),
            summary: "senna ode from the root cells".into(),
            outputs: vec![out],
            inputs: Vec::new(),
            record: Some("senna ode".into()),
            why: Some(crate::Why::new(
                "the root is the Ngn3-high cells",
                crate::By::User,
            )),
            started: "2026-11-25T10:02:03-08:00".into(),
            epoch: 0,
        };
        let mut old = serde_json::to_value(&spec).unwrap();
        assert_eq!(old["why"]["by"], "user");
        old.as_object_mut().unwrap().remove("why");
        let old: Spec = serde_json::from_value(old).unwrap();
        assert!(old.why.is_none());
        replace(&job, "job.json", &serde_json::to_vec(&spec).unwrap()).unwrap();
        conclude(&job, End::Done(String::new()), Vec::new(), None);
        let p = spec.records(None).unwrap();
        let day = root.join("results").join(crate::project::now().0);
        let script = std::fs::read_to_string(day.join("runall.sh")).unwrap();
        assert!(
            script.contains("# why (user): the root is the Ngn3-high cells\nif [ ! -e ode.out ]"),
            "{script}"
        );
        let notebook = p.notebook_today();
        assert!(
            notebook.contains(
                " · senna · choice\n\nsenna ode from the root cells (started 10:02)\n\n\
                 - Why: the root is the Ngn3-high cells\n- By: user\n"
            ),
            "{notebook}"
        );
    }

    #[test]
    fn a_job_begun_the_day_before_says_its_date() {
        let now = "2026-11-25T08:00:00-08:00";
        assert_eq!(started("2026-11-25T07:02:03-08:00", now).unwrap(), "07:02");
        let before = started("2026-11-24T23:50:00-08:00", now).unwrap();
        assert_eq!(before, "2026-11-24 23:50");
        assert!(started("", now).is_none());
    }

    #[cfg(all(feature = "sessions", unix))]
    /// What the TUI reports is what the server reads back, replaced whole
    /// each time; once the job has said how it ended, nothing more is
    /// written.
    #[test]
    fn progress_is_read_back_as_last_reported_until_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter {
            dir: dir.path().to_path_buf(),
            log: Mutex::new(std::fs::File::create(log_file(dir.path())).unwrap()),
            finished: Default::default(),
            idle: Default::default(),
        };
        assert!(read_progress(dir.path()).is_empty());
        reporter.progress(&[stage(10)]);
        reporter.progress(&[stage(215)]);
        assert_eq!(read_progress(dir.path()), vec![stage(215)]);
        // "No stages" is written once, not again until something moves.
        let file = dir.path().join(PROGRESS);
        reporter.progress(&[]);
        assert!(read_progress(dir.path()).is_empty());
        std::fs::remove_file(&file).unwrap();
        reporter.progress(&[]);
        assert!(!file.exists());
        reporter.progress(&[stage(300)]);
        assert_eq!(read_progress(dir.path()), vec![stage(300)]);

        reporter.finish(End::Done(String::new()));
        reporter.progress(&[stage(900)]);
        assert_eq!(read_progress(dir.path()), vec![stage(300)]);
    }

    #[test]
    fn a_stage_says_its_count_and_time_left() {
        assert_eq!(stage(215).say(), "Phase 1 (hier) 215/1000 (eta 6m52s)");
        let spin = Stage {
            label: String::new(),
            pos: 7,
            len: None,
            elapsed_s: 1,
            eta_s: None,
        };
        assert_eq!(spin.say(), "7");
    }

    #[test]
    fn a_second_word_changes_nothing_and_does_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        let reporter = Reporter {
            dir: dir.path().to_path_buf(),
            log: Mutex::new(std::fs::File::create(log_file(dir.path())).unwrap()),
            finished: Default::default(),
            idle: Default::default(),
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

    #[test]
    fn replacing_one_file_from_two_threads_at_once_never_loses_or_corrupts_it() {
        let dir = tempfile::tempdir().unwrap();
        let words: Vec<Vec<u8>> = (0..2u8).map(|t| vec![b'a' + t; 64 * 1024]).collect();
        let start = std::sync::Barrier::new(words.len());
        let failed: usize = std::thread::scope(|s| {
            let threads: Vec<_> = words
                .iter()
                .map(|word| {
                    s.spawn(|| {
                        start.wait();
                        (0..200)
                            .map(|_| replace(dir.path(), "end.json", word))
                            .filter(Result::is_err)
                            .count()
                    })
                })
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).sum()
        });
        assert_eq!(failed, 0, "replacements that failed");
        let left = std::fs::read(dir.path().join("end.json")).unwrap();
        assert!(
            words.contains(&left),
            "the file is not one of the words, whole"
        );
        // No temporary file is left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["end.json"]);
    }

    #[test]
    fn a_replace_that_fails_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        // A folder with something in it is not replaced by a file.
        std::fs::create_dir_all(dir.path().join("end.json").join("in")).unwrap();
        assert!(replace(dir.path(), "end.json", b"{}").is_err());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["end.json"]);
    }
}
