//! The runner in the background: `legume benchmark run SPEC --on REMOTE
//! --detach` starts the run in a session of its own, which outlives the
//! terminal or server that started it, and `legume benchmark stop NAME`
//! asks it to stop. The benchmark's folder keeps:
//!
//! - `run.log`: the runner's stdout and stderr, appended run after run;
//! - `runner.json`: the runner's pid and the time its process started,
//!   when and on which remote it started, the spec's key, and, once it
//!   ended, its exit code;
//! - `runner.lock`: held while a start checks for a live runner, starts
//!   one and writes `runner.json`, and while a runner notes its end, so
//!   two starts never both go ahead and an end is never noted before the
//!   start that it ends.
//!
//! A runner is alive while its pid is there, started when `runner.json`
//! says (on Linux, by `/proc/<pid>/stat`), and runs `benchmark run`.

use super::bench_rel;
use super::remote::{self, Remote};
use crate::command::{refuse, usage};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `legume benchmark stop`'s usage.
pub const STOP_USAGE: &str = "\
legume benchmark stop NAME

Stops the runner of benchmark NAME that legume benchmark run --detach
started: it is sent TERM, so it cancels its Slurm array tasks and ends,
as Ctrl-C ends a run in a terminal. A run again resumes.";

/// Set in a detached runner's environment, to its `runner.json`: it notes
/// there how it ended.
pub(super) const RUNNER_ENV: &str = "LEGUME_BENCHMARK_RUNNER";

/// The runner's file, in the benchmark's folder.
pub(crate) const RUNNER_FILE: &str = "runner.json";

/// The lock a start and an end hold, in the benchmark's folder.
const LOCK_FILE: &str = "runner.lock";

/// The runner's log, in the benchmark's folder.
pub(crate) const LOG_FILE: &str = "run.log";

/// `runner.json`: a detached runner.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunnerFile {
    pub(crate) pid: u32,
    /// When its process started, in clock ticks since boot (Linux's
    /// `/proc/<pid>/stat`): a pid used again later has another.
    #[serde(default)]
    pub(crate) start_ticks: Option<u64>,
    /// When it started (RFC 3339).
    pub(crate) started: String,
    pub(crate) spec_key: String,
    pub(crate) remote: String,
    /// Its exit code, once it ended.
    #[serde(default)]
    pub(crate) exit: Option<i32>,
    /// When it ended.
    #[serde(default)]
    pub(crate) ended: Option<String>,
}

/// What a detached start refuses, for each front door to say in its own
/// words.
#[derive(Debug)]
pub(crate) enum Refused {
    /// The benchmark's runner is alive.
    Running { name: String, runner: RunnerFile },
    /// The remote is not in `remotes.toml` (at `file`, which names
    /// `known`), and is not to be asked for.
    UnknownRemote {
        on: String,
        file: PathBuf,
        known: Vec<String>,
    },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Running { name, runner: r } => write!(
                f,
                "benchmark {name} is running already, as pid {} (since {}, on {}): \
                 legume benchmark stop {name} stops it",
                r.pid, r.started, r.remote
            ),
            Refused::UnknownRemote { on, file, .. } => {
                write!(f, "no remote {on} in {}", file.display())
            }
        }
    }
}

impl std::error::Error for Refused {}

/// The runner `runner.json` in `dir` names, if any.
pub(crate) fn read(dir: &Path) -> Option<RunnerFile> {
    let bytes = std::fs::read(dir.join(RUNNER_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// When process `pid` started, in clock ticks since boot: field 22 of
/// `/proc/<pid>/stat`, read after its name (which may hold spaces).
#[cfg(target_os = "linux")]
fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    // After the name, the fields from the third: the 22nd is the 20th.
    after.split_whitespace().nth(19)?.parse().ok()
}

/// No start times off Linux.
#[cfg(not(target_os = "linux"))]
fn start_ticks(_pid: u32) -> Option<u64> {
    None
}

/// Whether runner `r` is alive: its pid there, started when it says, and
/// running `benchmark run`, whatever its program's path (a reinstalled
/// legume's is `/proc/self/exe`).
#[cfg(target_os = "linux")]
pub(crate) fn alive(r: &RunnerFile) -> bool {
    if r.start_ticks.is_some_and(|t| start_ticks(r.pid) != Some(t)) {
        return false;
    }
    // A zombie's, or a gone process's, cmdline is empty.
    let Ok(bytes) = std::fs::read(format!("/proc/{}/cmdline", r.pid)) else {
        return false;
    };
    let args: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    args.windows(2)
        .any(|w| w[0] == b"benchmark" && w[1] == b"run")
}

/// Whether runner `r`'s pid is there: it can be sent a signal.
#[cfg(not(target_os = "linux"))]
pub(crate) fn alive(r: &RunnerFile) -> bool {
    let Ok(pid) = libc::pid_t::try_from(r.pid) else {
        return false;
    };
    // SAFETY: signal 0 checks the pid, and sends nothing.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The live runner of the benchmark in `dir`, if one is.
pub(crate) fn live(dir: &Path) -> Option<RunnerFile> {
    read(dir).filter(|r| r.exit.is_none() && alive(r))
}

/// `runner.lock` in `dir`, held until the file returned is dropped.
fn lock(dir: &Path) -> Result<std::fs::File> {
    let path = dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: flock on a file this function opened, waiting for it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("locking {}", path.display()));
    }
    Ok(file)
}

/// Remote `on` as `remotes.toml` keeps it; one not there is asked for on
/// stdin and kept, said on `ask`, when given, else refused
/// ([`Refused::UnknownRemote`]).
pub(crate) fn kept_remote(on: &str, ask: Option<&mut dyn Write>) -> Result<Remote> {
    if let Some(out) = ask {
        return remote::named(on, out);
    }
    let file = remote::config_file()
        .ok_or_else(|| refuse("no config folder for remotes.toml: set HOME, or LEGUME_REMOTES"))?;
    let all = remote::read(&file)?;
    match all.iter().find(|r| r.name == on) {
        Some(r) => Ok(r.clone()),
        None => Err(Refused::UnknownRemote {
            on: on.to_string(),
            file,
            known: all.into_iter().map(|r| r.name).collect(),
        }
        .into()),
    }
}

/// A detached start: the project, the spec's file (as the runner reads
/// it) and text, and the remote.
pub(crate) struct Detach<'a> {
    pub(crate) root: &'a Path,
    pub(crate) spec_file: &'a Path,
    pub(crate) text: &'a str,
    pub(crate) on: &'a str,
}

/// A runner started.
pub(crate) struct Started {
    pub(crate) name: String,
    pub(crate) on: String,
    pub(crate) pid: u32,
}

impl Started {
    /// What a start says of it.
    pub(crate) fn said(&self) -> String {
        format!(
            "started benchmark {} on {} in the background, as pid {}\nlog: {}\n",
            self.name,
            self.on,
            self.pid,
            bench_rel(&self.name).join(LOG_FILE).display()
        )
    }
}

/// The run of `d` started in a session of its own, from the project, its
/// output appended to `run.log` and its pid in `runner.json`, both in the
/// benchmark's folder. Under `runner.lock`: refused while a runner of the
/// benchmark is alive ([`Refused::Running`]); the remote found, asked for
/// on `ask` when given ([`kept_remote`]); then `before` (what the start
/// writes once it goes ahead), the runner started, and `runner.json`
/// written.
pub(crate) fn detach(
    d: &Detach,
    ask: Option<&mut dyn Write>,
    before: &mut dyn FnMut() -> Result<()>,
) -> Result<Started> {
    let (spec, value) = super::spec::parse(d.text)?;
    let spec_key = super::key::key(super::key::spec_file(&value).as_bytes());
    let dir = d.root.join(bench_rel(&spec.name));
    std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let _held = lock(&dir)?;
    if let Some(runner) = live(&dir) {
        return Err(Refused::Running {
            name: spec.name,
            runner,
        }
        .into());
    }
    kept_remote(d.on, ask)?;
    before()?;
    let log_path = dir.join(LOG_FILE);
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let started = crate::project::rfc3339();
    writeln!(
        log,
        "== {started}: legume benchmark run {} --on {}",
        d.spec_file.display(),
        d.on
    )?;
    let exe = crate::own_exe().context("finding this legume")?;
    let mut cmd = Command::new(exe);
    cmd.arg("benchmark")
        .arg("run")
        .arg(d.spec_file)
        .arg("--on")
        .arg(d.on)
        .current_dir(d.root)
        .env(crate::project::ROOT_ENV, d.root)
        .env(RUNNER_ENV, dir.join(RUNNER_FILE))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe, the one call between fork and
    // exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("starting the runner")?;
    let pid = child.id();
    // Reaped here while this process lasts; once it ends, by whoever
    // adopts the runner.
    std::thread::spawn(move || child.wait());
    let file = RunnerFile {
        pid,
        start_ticks: start_ticks(pid),
        started,
        spec_key,
        remote: d.on.to_string(),
        exit: None,
        ended: None,
    };
    crate::job::replace(&dir, RUNNER_FILE, crate::pretty(&file)?.as_bytes())?;
    Ok(Started {
        name: spec.name,
        on: d.on.to_string(),
        pid,
    })
}

/// `legume benchmark run SPEC --on ON --detach`, said on `out`: a refusal
/// is the command's (exit 3).
pub(super) fn detach_main(spec_file: &Path, on: &str, out: &mut dyn Write) -> Result<()> {
    let text = std::fs::read_to_string(spec_file)
        .with_context(|| format!("reading {}", spec_file.display()))?;
    let root = crate::project::root().context("finding the project's folder")?;
    let spec_file = crate::absolute(spec_file);
    let d = Detach {
        root: &root,
        spec_file: &spec_file,
        text: &text,
        on,
    };
    let started = detach(&d, Some(&mut *out), &mut || Ok(())).map_err(|e| {
        match e.downcast_ref::<Refused>() {
            Some(r) => refuse(r),
            None => e,
        }
    })?;
    write!(out, "{}", started.said())?;
    writeln!(
        out,
        "legume benchmark stop {} stops it; a run again resumes it",
        started.name
    )?;
    Ok(())
}

/// A detached runner ended with `code`: noted in its `runner.json` (at
/// `file`), under the lock its start held while writing it, and in its log.
pub(super) fn ended(file: &Path, code: i32, err: &mut dyn Write) {
    let _ = writeln!(
        err,
        "== {}: the run ended, exit {code}",
        crate::project::rfc3339()
    );
    let Some(dir) = file.parent() else { return };
    let Ok(_held) = lock(dir) else { return };
    let Some(mut r) = read(dir) else { return };
    if r.pid != std::process::id() {
        return;
    }
    r.exit = Some(code);
    r.ended = Some(crate::project::rfc3339());
    if let Ok(text) = crate::pretty(&r) {
        let _ = crate::job::replace(dir, RUNNER_FILE, text.as_bytes());
    }
}

/// The runner of the benchmark in `dir` (`name`), sent TERM: what was
/// done, or the refusal that no runner is alive.
pub(crate) fn stop_runner(dir: &Path, name: &str) -> Result<String> {
    let Some(r) = live(dir) else {
        let why = match read(dir) {
            None => "no runner.json: it was not started with --detach".to_string(),
            Some(RunnerFile {
                exit: Some(code), ..
            }) => format!("its runner ended, exit {code}"),
            Some(r) => format!("its runner, pid {}, is gone", r.pid),
        };
        return Err(refuse(format!(
            "benchmark {name} has no live runner ({why})"
        )));
    };
    let pid = libc::pid_t::try_from(r.pid).context("a pid out of range")?;
    // SAFETY: a plain kill of the pid runner.json names, checked alive.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context(format!("stopping pid {pid}"));
    }
    Ok(format!(
        "sent TERM to the runner of benchmark {name}, pid {pid}: it cancels its Slurm tasks, \
         waits for them to stop, and ends (exit 130); its log is {}\n",
        bench_rel(name).join(LOG_FILE).display()
    ))
}

/// `legume benchmark stop` with `args` (those after `stop`).
pub fn stop_main(args: &[String], out: &mut dyn Write) -> Result<()> {
    let name = match args {
        [] => return Err(usage("stop needs NAME, the benchmark's name")),
        [a] if a == "-h" || a == "--help" => return Ok(writeln!(out, "{STOP_USAGE}")?),
        [a] => a,
        _ => return Err(usage("stop takes one NAME")),
    };
    super::spec::benchmark_name(name)?;
    let root: PathBuf = crate::project::root().context("finding the project's folder")?;
    let said = stop_runner(&root.join(bench_rel(name)), name)?;
    Ok(write!(out, "{said}")?)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn runner(pid: u32, start_ticks: Option<u64>) -> RunnerFile {
        RunnerFile {
            pid,
            start_ticks,
            started: String::new(),
            spec_key: String::new(),
            remote: String::new(),
            exit: None,
            ended: None,
        }
    }

    #[test]
    fn a_runner_is_alive_by_its_start_and_its_arguments_not_its_name() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30; true", "benchmark", "run"])
            .spawn()
            .unwrap();
        let pid = child.id();
        let ticks = start_ticks(pid).unwrap();
        assert!(alive(&runner(pid, Some(ticks))));
        assert!(alive(&runner(pid, None)));
        // Another process at that pid started at another time.
        assert!(!alive(&runner(pid, Some(ticks + 1))));
        // This test is not a `benchmark run`.
        assert!(!alive(&runner(std::process::id(), None)));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!alive(&runner(pid, Some(ticks))));
    }
}
