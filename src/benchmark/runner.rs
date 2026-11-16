//! The runner in the background: `legume benchmark run SPEC --on REMOTE
//! --detach` starts the run in a session of its own, which outlives the
//! terminal or server that started it, and `legume benchmark stop NAME`
//! asks it to stop. The benchmark's folder keeps:
//!
//! - `run.log`: the runner's stdout and stderr, appended run after run;
//! - `runner.json`: the runner's pid, when and on which remote it started,
//!   the spec's key, and, once it ended, its exit code.
//!
//! A runner is alive while its pid is a `legume benchmark run` (on Linux,
//! by `/proc/<pid>/cmdline`; elsewhere, while the pid is there at all).

use super::bench_rel;
use crate::command::{refuse, usage};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
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

/// The runner's log, in the benchmark's folder.
pub(crate) const LOG_FILE: &str = "run.log";

/// `runner.json`: a detached runner.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RunnerFile {
    pub(crate) pid: u32,
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

/// The runner `runner.json` in `dir` names, if any.
pub(crate) fn read(dir: &Path) -> Option<RunnerFile> {
    let bytes = std::fs::read(dir.join(RUNNER_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether `pid` is a `legume benchmark run`.
#[cfg(target_os = "linux")]
pub(crate) fn alive(pid: u32) -> bool {
    // A zombie's, or a gone process's, cmdline is empty.
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let args: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    let program = args.first().map_or(&b""[..], |a| a);
    let base = program.rsplit(|&b| b == b'/').next().unwrap_or(program);
    base.starts_with(b"legume")
        && args
            .windows(2)
            .any(|w| w[0] == b"benchmark" && w[1] == b"run")
}

/// Whether `pid` is there: it can be sent a signal.
#[cfg(not(target_os = "linux"))]
pub(crate) fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 checks the pid, and sends nothing.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The live runner of the benchmark in `dir`, if one is.
pub(crate) fn live(dir: &Path) -> Option<RunnerFile> {
    read(dir).filter(|r| r.exit.is_none() && alive(r.pid))
}

/// `legume benchmark run SPEC --on ON --detach`: the run started in a
/// session of its own, from this folder, its output appended to `run.log`
/// and its pid in `runner.json`, both in the benchmark's folder; said on
/// `out`. Refused while a runner of the benchmark is alive. A remote not
/// kept yet is asked for here first.
pub(super) fn detach(spec_file: &Path, on: &str, out: &mut dyn Write) -> Result<()> {
    let text = std::fs::read_to_string(spec_file)
        .with_context(|| format!("reading {}", spec_file.display()))?;
    let (spec, value) = super::spec::parse(&text)?;
    let spec_key = super::key::key(super::key::spec_file(&value).as_bytes());
    let root = crate::project::root().context("finding the project's folder")?;
    let rel = bench_rel(&spec.name);
    let dir = root.join(&rel);
    if let Some(r) = live(&dir) {
        return Err(refuse(format!(
            "benchmark {} is running already, as pid {} (since {}, on {}): legume benchmark \
             stop {} stops it",
            spec.name, r.pid, r.started, r.remote, spec.name
        )));
    }
    super::remote::named(on, out)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let log_path = dir.join(LOG_FILE);
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let started = crate::project::rfc3339();
    writeln!(
        log,
        "== {started}: legume benchmark run {} --on {on}",
        spec_file.display()
    )?;
    let exe = crate::own_exe().context("finding this legume")?;
    let mut cmd = Command::new(exe);
    cmd.arg("benchmark")
        .arg("run")
        .arg(spec_file)
        .arg("--on")
        .arg(on)
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
    let pid = leave(cmd.spawn().context("starting the runner")?);
    let file = RunnerFile {
        pid,
        started,
        spec_key,
        remote: on.to_string(),
        exit: None,
        ended: None,
    };
    crate::job::replace(&dir, RUNNER_FILE, crate::pretty(&file)?.as_bytes())?;
    writeln!(
        out,
        "started benchmark {} on {on} in the background, as pid {pid}\nlog: {}\n\
         legume benchmark stop {} stops it; a run again resumes it",
        spec.name,
        rel.join(LOG_FILE).display(),
        spec.name
    )?;
    Ok(())
}

/// The runner `child`'s pid, the runner left to run: in a session of its
/// own, it is reaped by whoever adopts it once this process ends.
fn leave(child: std::process::Child) -> u32 {
    child.id()
}

/// A detached runner ended with `code`: noted in its `runner.json` (at
/// `file`) and its log.
pub(super) fn ended(file: &Path, code: i32, err: &mut dyn Write) {
    let _ = writeln!(
        err,
        "== {}: the run ended, exit {code}",
        crate::project::rfc3339()
    );
    let Some(dir) = file.parent() else { return };
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
