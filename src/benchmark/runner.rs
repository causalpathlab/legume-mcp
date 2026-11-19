//! The runner in the background: `legume benchmark run SPEC --on REMOTE
//! --detach` starts the run in a session of its own, which outlives the
//! terminal or server that started it, and `legume benchmark stop NAME`
//! asks it to stop. The benchmark's folder keeps:
//!
//! - `run.log`: the runner's stdout and stderr, appended run after run;
//! - `runner.json`: the runner's pid, when and on which remote it
//!   started, the spec's key, and, once it ended, its exit code;
//! - `runner.alive`: locked by the runner for its whole life, so the
//!   system lets it go as the runner ends, however it ends. The start
//!   takes it, under `runner.lock`, and hands it down to the runner;
//! - `runner.lock`: held while a start checks for a live runner, starts
//!   one and writes `runner.json`, and while a runner notes its end, so
//!   two starts never both go ahead and an end is never noted before the
//!   start that it ends.
//!
//! A runner is alive while `runner.alive` is locked.

use super::bench_rel;
use super::remote::{self, Remote};
use super::spec::Spec;
use crate::command::{refuse, usage};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `legume benchmark stop`'s usage.
pub const STOP_USAGE: &str = "\
legume benchmark stop NAME

Stops the runner of benchmark NAME that legume benchmark run --detach
started: it is sent TERM, so it stops its tasks (its Slurm array tasks,
on a cluster) and ends, as Ctrl-C ends a run in a terminal. A run again
resumes.";

/// Set in a detached runner's environment, to its `runner.json`: it notes
/// there how it ended.
pub(super) const RUNNER_ENV: &str = "LEGUME_BENCHMARK_RUNNER";

/// Set in a detached runner's environment, to the descriptor it holds
/// `runner.alive` locked by.
const ALIVE_FD_ENV: &str = "LEGUME_BENCHMARK_ALIVE_FD";

/// The runner's file, in the benchmark's folder.
pub(crate) const RUNNER_FILE: &str = "runner.json";

/// The lock a runner holds for its whole life, in the benchmark's folder.
const ALIVE_FILE: &str = "runner.alive";

/// The lock a start and an end hold, in the benchmark's folder.
const LOCK_FILE: &str = "runner.lock";

/// The runner's log, in the benchmark's folder.
pub(crate) const LOG_FILE: &str = "run.log";

/// `runner.json`: a detached runner.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunnerFile {
    pub(crate) pid: u32,
    /// When it started (RFC 3339).
    pub(crate) started: String,
    pub(crate) spec_key: String,
    /// The remote it runs on; none for this machine.
    #[serde(default)]
    pub(crate) remote: Option<String>,
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
                r.pid,
                r.started,
                place(r.remote.as_deref())
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

/// Whether the runner of the benchmark in `dir` is alive: another holds
/// its `runner.alive`.
pub(crate) fn alive(dir: &Path) -> bool {
    let path = dir.join(ALIVE_FILE);
    path.exists() && matches!(crate::lock::try_lock(&path), Ok(None))
}

/// The live runner of the benchmark in `dir`, if one is.
pub(crate) fn live(dir: &Path) -> Option<RunnerFile> {
    alive(dir).then(|| read(dir)).flatten()
}

/// `runner.lock` in `dir`, held until the file returned is dropped.
fn lock(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK_FILE);
    crate::lock::lock(&path).with_context(|| format!("locking {}", path.display()))
}

/// `cmd` set to start in a session of its own, holding `alive` (the
/// start's lock on `runner.alive`) from its first moment, by a descriptor
/// kept across exec and named in [`ALIVE_FD_ENV`].
fn hand_down(cmd: &mut Command, alive: &File) {
    let fd = alive.as_raw_fd();
    cmd.env(ALIVE_FD_ENV, fd.to_string());
    // SAFETY: setsid and fcntl are async-signal-safe, the calls between
    // fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 || libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// In a detached runner: the descriptor of its lock on `runner.alive`
/// ([`hand_down`]) closed on exec again, so the commands it runs do not
/// hold the lock past its end.
pub(super) fn keep_alive_lock() {
    let fd = std::env::var(ALIVE_FD_ENV)
        .ok()
        .and_then(|v| v.parse::<RawFd>().ok());
    if let Some(fd) = fd {
        // SAFETY: fcntl on a descriptor number: on any other, it fails.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

/// Remote `on` as `remotes.toml` keeps it; one not there is asked for on
/// stdin and kept, said on `ask`, when given, else refused
/// ([`Refused::UnknownRemote`]).
pub(crate) fn kept_remote(on: &str, ask: Option<&mut dyn Write>) -> Result<Remote> {
    if let Some(out) = ask {
        return remote::named(on, out);
    }
    let (file, all) = remote::kept()?;
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

/// `legume benchmark run SPEC [--on ON] [--rerun-changed]`, run by this
/// legume's own binary: on remote `on`, or on this machine.
pub(crate) fn run_command(
    spec_file: &Path,
    on: Option<&str>,
    rerun_changed: bool,
) -> Result<Command> {
    let mut cmd = crate::own_command(["benchmark", "run"])?;
    cmd.arg(spec_file);
    if let Some(on) = on {
        cmd.arg("--on").arg(on);
    }
    if rerun_changed {
        cmd.arg("--rerun-changed");
    }
    Ok(cmd)
}

/// Where a run goes, as said: remote `on`, or this machine.
pub(crate) fn place(on: Option<&str>) -> &str {
    on.unwrap_or(HERE)
}

/// How a run on this machine is named where a remote would be.
pub(crate) const HERE: &str = "this machine";

/// A detached start: the project, the spec's file (as the runner reads
/// it), the spec read and its key, and the remote (none: this machine).
pub(crate) struct Detach<'a> {
    pub(crate) root: &'a Path,
    pub(crate) spec_file: &'a Path,
    pub(crate) spec: &'a Spec,
    pub(crate) spec_key: &'a str,
    pub(crate) on: Option<&'a str>,
    /// Tasks the spec changed since an earlier run are run again.
    pub(crate) rerun_changed: bool,
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
/// writes once it goes ahead), the runner started holding `runner.alive`,
/// and `runner.json` written.
pub(crate) fn detach(
    d: &Detach,
    ask: Option<&mut dyn Write>,
    before: &mut dyn FnMut() -> Result<()>,
) -> Result<Started> {
    let name = &d.spec.name;
    let dir = d.root.join(bench_rel(name));
    std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let _held = lock(&dir)?;
    if let Some(runner) = live(&dir) {
        return Err(Refused::Running {
            name: name.clone(),
            runner,
        }
        .into());
    }
    let alive_path = dir.join(ALIVE_FILE);
    let alive = crate::lock::try_lock(&alive_path)
        .with_context(|| format!("locking {}", alive_path.display()))?
        .with_context(|| {
            format!(
                "{} is held, though {RUNNER_FILE} names no live runner",
                alive_path.display()
            )
        })?;
    if let Some(on) = d.on {
        kept_remote(on, ask)?;
    }
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
        "== {started}: legume benchmark run {}{}{}",
        d.spec_file.display(),
        d.on.map_or(String::new(), |on| format!(" --on {on}")),
        if d.rerun_changed {
            " --rerun-changed"
        } else {
            ""
        }
    )?;
    let mut cmd = run_command(d.spec_file, d.on, d.rerun_changed)?;
    cmd.current_dir(d.root)
        .env(crate::project::ROOT_ENV, d.root)
        .env(RUNNER_ENV, dir.join(RUNNER_FILE))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    hand_down(&mut cmd, &alive);
    let mut child = cmd.spawn().context("starting the runner")?;
    // The runner holds it from here.
    drop(alive);
    let pid = child.id();
    // Reaped here while this process lasts; once it ends, by whoever
    // adopts the runner.
    std::thread::spawn(move || child.wait());
    let file = RunnerFile {
        pid,
        started,
        spec_key: d.spec_key.to_string(),
        remote: d.on.map(str::to_string),
        exit: None,
        ended: None,
    };
    super::gather::write(&dir, RUNNER_FILE, crate::pretty(&file)?)?;
    Ok(Started {
        name: name.clone(),
        on: place(d.on).to_string(),
        pid,
    })
}

/// `legume benchmark run SPEC --on ON --detach`, said on `out`: a refusal
/// is the command's (exit 3).
pub(super) fn detach_main(
    spec_file: &Path,
    on: Option<&str>,
    rerun_changed: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let text = std::fs::read_to_string(spec_file)
        .with_context(|| format!("reading {}", spec_file.display()))?;
    let (spec, value) = super::spec::parse(&text)?;
    let root = crate::project::root().context("finding the project's folder")?;
    let spec_file = crate::absolute(spec_file);
    let d = Detach {
        root: &root,
        spec_file: &spec_file,
        spec: &spec,
        spec_key: &super::key::spec_key(&value),
        on,
        rerun_changed,
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
        let _ = super::gather::write(dir, RUNNER_FILE, text);
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
    // SAFETY: a plain kill of the pid runner.json names, alive by its lock.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context(format!("stopping pid {pid}"));
    }
    Ok(format!(
        "sent TERM to the runner of benchmark {name}, pid {pid}: it stops its tasks, waits \
         for them to stop, and ends (exit 130); its log is {}\n",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_is_alive_while_its_lock_is_held_handed_down_or_not() {
        let d = tempfile::tempdir().unwrap();
        assert!(!alive(d.path()), "no runner.alive");
        let held = crate::lock::try_lock(&d.path().join(ALIVE_FILE))
            .unwrap()
            .unwrap();
        assert!(alive(d.path()));
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        hand_down(&mut cmd, &held);
        let mut child = cmd.spawn().unwrap();
        drop(held);
        // The child holds it now.
        assert!(alive(d.path()));
        child.kill().unwrap();
        child.wait().unwrap();
        // A child another test forks meanwhile holds it too, until it
        // execs.
        let gone = (0..100).any(|_| {
            let gone = !alive(d.path());
            if !gone {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            gone
        });
        assert!(gone);
    }

    #[test]
    fn a_run_is_legume_benchmark_run_spec_on_remote() {
        let cmd = run_command(Path::new("specs-in/N1.json"), Some("REMOTE1"), false).unwrap();
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(
            args,
            ["benchmark", "run", "specs-in/N1.json", "--on", "REMOTE1"]
        );
        let here = run_command(Path::new("specs-in/N1.json"), None, false).unwrap();
        let args: Vec<&std::ffi::OsStr> = here.get_args().collect();
        assert_eq!(args, ["benchmark", "run", "specs-in/N1.json"]);
    }
}
