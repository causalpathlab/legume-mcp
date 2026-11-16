//! `legume benchmark task`: the wrapper that runs one task of a plan, as a
//! compute node runs it. The tool runs in a process group of its own, in
//! the task's folder, with its output in `log`. The wrapper is its
//! subreaper, so a descendant that leaves the group (or the session) is
//! still the wrapper's to end. It enforces the task's timeout (TERM, then
//! KILL after the plan's grace) and passes TERM on to the tool when it is
//! stopped itself, as Slurm's scancel does.
//!
//! What it leaves in the task's folder, each file written whole:
//! `done.json` when the tool exited 0, before its timeout or a stop, and
//! made every `makes` file, then
//! `exit.json` always, so a reader that sees `exit.json` sees the end. A
//! rerun clears both first and keeps the last log as `log.<attempt>`.

use super::plan::Task;
use crate::command::{canonical_flags, failed, flag_pairs, required, usage, Flag};
use crate::procgroup::Group;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
/// `legume benchmark task`'s usage.
pub const USAGE: &str = "\
legume benchmark task --plan PLAN (--task ID | --list FILE [--index N])

Runs one task of the plan PLAN (its plan.json) under the wrapper, as a
compute node runs it: the task ID, or line N (from 0) of FILE, a task id
per line, N being $SLURM_ARRAY_TASK_ID when --index is not given. The
tool runs in the task's folder with its output in log; it is stopped
at its timeout (TERM, then KILL after the plan's grace) and when the
wrapper is. The wrapper writes done.json when the tool exited 0, before
its timeout or a stop, and made every makes file, then exit.json, and
exits as the tool did.";

const FLAGS: &[Flag] = &[
    Flag::once(&["--plan"]),
    Flag::once(&["--task"]),
    Flag::once(&["--list"]),
    Flag::once(&["--index"]),
];

/// The longest grace a stopped task gets: inside Slurm's default
/// KillWait, 30 s, after which Slurm kills the wrapper too.
const STOP_GRACE: Duration = Duration::from_secs(20);

/// How long what is left of the tool's tree gets after TERM, once the
/// tool itself has ended.
const LINGER: Duration = Duration::from_secs(1);

/// How often the wrapper looks at the tool, its clock and its own stop.
const TICK: Duration = Duration::from_millis(50);

/// What the wrapper reads of `plan.json`.
#[derive(Deserialize)]
struct Plan {
    grace_secs: u64,
    tasks: Vec<Task>,
}

/// `legume benchmark task` with `args` (those after `task`).
pub fn main(args: &[String], out: &mut dyn Write, _err: &mut dyn Write) -> Result<()> {
    let Some(pairs) = flag_pairs(args)? else {
        return Ok(writeln!(out, "{USAGE}")?);
    };
    let (mut plan, mut id, mut list, mut index) = (None, None, None, None);
    for (flag, v) in canonical_flags(FLAGS, pairs)? {
        match flag {
            "--plan" => plan = Some(PathBuf::from(v)),
            "--task" => id = Some(v.to_string()),
            "--list" => list = Some(PathBuf::from(v)),
            _ => index = Some(v.to_string()),
        }
    }
    let plan_file = required(plan, "--plan")?;
    let id = match (id, list) {
        (Some(id), None) if index.is_none() => id,
        (None, Some(list)) => listed(&list, index)?,
        _ => return Err(usage("one of --task ID, or --list FILE with its --index")),
    };
    let text = std::fs::read_to_string(&plan_file)
        .with_context(|| format!("reading {}", plan_file.display()))?;
    let plan: Plan =
        serde_json::from_str(&text).with_context(|| format!("reading {}", plan_file.display()))?;
    let Some(task) = plan.tasks.iter().find(|t| t.id == id) else {
        return Err(usage(format!("{}: no task {id}", plan_file.display())));
    };
    match run(task, Duration::from_secs(plan.grace_secs))? {
        None => Ok(()),
        // The wrapper exits as the tool did, said as no usage is.
        Some((code, said)) => Err(failed(code, format!("task {}: {said}", task.id))),
    }
}

/// The task id on line `index` of `list`, `index` being
/// `$SLURM_ARRAY_TASK_ID` when not given.
fn listed(list: &Path, index: Option<String>) -> Result<String> {
    let index = index
        .or_else(|| std::env::var("SLURM_ARRAY_TASK_ID").ok())
        .ok_or_else(|| usage("--list needs --index, or $SLURM_ARRAY_TASK_ID"))?;
    let n: usize = index
        .parse()
        .map_err(|_| usage(format!("--index is a line number from 0, not {index:?}")))?;
    let text =
        std::fs::read_to_string(list).with_context(|| format!("reading {}", list.display()))?;
    match text.lines().nth(n).map(str::trim) {
        Some("") => Err(usage(format!("{}: line {n} is empty", list.display()))),
        Some(id) => Ok(id.to_string()),
        None => Err(usage(format!(
            "{}: no line {n}, it has {}",
            list.display(),
            text.lines().count()
        ))),
    }
}

/// The signal that stopped the wrapper, 0 until one does.
static STOP: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_stop(signal: libc::c_int) {
    STOP.store(signal, Ordering::SeqCst);
}

/// TERM, INT and HUP to the wrapper are noted in [`STOP`], for the wrapper
/// to pass on to the tool; the tool's own handlers are reset by its exec.
fn catch_stops() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: a handler that only stores to an atomic, installed with
        // an empty mask.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_stop as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// How the tool ended.
#[derive(Clone, Copy)]
enum Status {
    Exit(i32),
    Signal(i32),
}

impl Status {
    fn of(status: std::process::ExitStatus) -> Status {
        match (status.code(), status.signal()) {
            (Some(c), _) => Status::Exit(c),
            (None, s) => Status::Signal(s.unwrap_or(0)),
        }
    }

    /// The wrapper's exit code: the tool's, or 128 plus its signal.
    fn code(self) -> i32 {
        match self {
            Status::Exit(c) => c,
            Status::Signal(s) => 128 + s,
        }
    }
}

/// How the tool ended, as the wrapper saw it.
struct End {
    status: Status,
    timed_out: bool,
    stopped: bool,
}

/// Task `t` run once under the wrapper, with `grace` between TERM and
/// KILL: `None` when it is done, or the wrapper's exit code and why not.
fn run(t: &Task, grace: Duration) -> Result<Option<(i32, String)>> {
    let dir = &t.dir;
    let (first, rest) = t.argv.split_first().context("a task's argv is empty")?;
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    for stale in ["done.json", "exit.json"] {
        let stale = dir.join(stale);
        match std::fs::remove_file(&stale) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("clearing {}", stale.display()));
            }
            _ => {}
        }
    }
    let attempt = keep_last_log(dir)?;
    let log_path = dir.join("log");
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("making {}", log_path.display()))?;
    // SAFETY: prctl with an integer argument, on this process only.
    unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    catch_stops();

    let start = Instant::now();
    let mut cmd = std::process::Command::new(first);
    cmd.args(rest)
        .envs(&t.env)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // The task's time is the tool's, not that of ending what it left.
    let (end, secs) = match Group::spawn(&mut cmd) {
        Ok(mut group) => {
            let (timed_out, stopped) =
                watch(&mut group, Duration::from_secs(t.timeout_secs), grace)?;
            let secs = start.elapsed().as_secs_f64();
            let status = Status::of(group.stop(LINGER)?);
            end_adopted();
            let end = End {
                status,
                timed_out,
                stopped,
            };
            (end, secs)
        }
        Err(e) => {
            // As a shell says a command it cannot run: 127.
            let mut log = std::fs::OpenOptions::new().append(true).open(&log_path)?;
            writeln!(log, "legume benchmark task: running {first}: {e}")?;
            let end = End {
                status: Status::Exit(127),
                timed_out: false,
                stopped: false,
            };
            (end, start.elapsed().as_secs_f64())
        }
    };
    let (cpu_secs, max_rss_mb) = children_usage();

    let not_done = if end.stopped {
        Some("stopped".to_string())
    } else if end.timed_out {
        Some(format!("timed out after {} s", t.timeout_secs))
    } else {
        match end.status {
            Status::Exit(0) => write_done(t, secs, cpu_secs).err(),
            Status::Exit(c) => Some(format!("exit {c}")),
            Status::Signal(s) => Some(format!("signal {s}")),
        }
    };
    let (exit, signal) = match end.status {
        Status::Exit(c) => (Some(c), None),
        Status::Signal(s) => (None, Some(s)),
    };
    let exit = json!({
        "task": t.id,
        "key": t.key,
        "attempt": attempt,
        "exit": exit,
        "signal": signal,
        "timed_out": end.timed_out,
        "stopped": end.stopped,
        "oom": oom_said(&log_path),
        "secs": secs,
        "cpu_secs": cpu_secs,
        "max_rss_mb": max_rss_mb,
        "node": super::machine::host().unwrap_or_default(),
    });
    crate::job::replace(dir, "exit.json", crate::pretty(&exit)?.as_bytes())?;
    // A tool that exits 0 and is not done (its timeout, a stop, a makes
    // file not made) ends the wrapper with 1.
    Ok(not_done.map(|why| (end.status.code().max(1), why)))
}

/// `done.json` for task `t`, when the tool made every `makes` file (there
/// and not empty); why not otherwise.
fn write_done(t: &Task, secs: f64, cpu_secs: f64) -> std::result::Result<(), String> {
    let sizes: Vec<(&String, Option<u64>)> = t
        .makes
        .iter()
        .map(|m| {
            let bytes = std::fs::metadata(t.dir.join(m))
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            (m, bytes)
        })
        .collect();
    let unmade: Vec<String> = sizes
        .iter()
        .filter_map(|(m, bytes)| match bytes {
            None => Some(format!("{m} is missing")),
            Some(0) => Some(format!("{m} is empty")),
            Some(_) => None,
        })
        .collect();
    if !unmade.is_empty() {
        return Err(format!("exit 0, but {}", unmade.join(", ")));
    }
    let makes: Vec<_> = sizes
        .iter()
        .map(|(m, bytes)| json!({"path": m, "bytes": bytes}))
        .collect();
    let done = json!({"key": t.key, "makes": makes, "secs": secs, "cpu_secs": cpu_secs});
    // Not done when done.json cannot be written; exit.json says the end
    // all the same.
    let done = crate::pretty(&done).map_err(|e| e.to_string())?;
    crate::job::replace(&t.dir, "done.json", done.as_bytes())
        .map_err(|e| format!("exit 0, but writing done.json: {e}"))
}

/// The last log in `dir`, if any, kept as `log.<n>`, the first `n` free;
/// the attempt the next log is, from 1.
fn keep_last_log(dir: &Path) -> Result<u32> {
    let log = dir.join("log");
    if !log.exists() {
        return Ok(1);
    }
    let mut n = 1;
    while dir.join(format!("log.{n}")).exists() {
        n += 1;
    }
    let kept = dir.join(format!("log.{n}"));
    std::fs::rename(&log, &kept).with_context(|| format!("keeping {}", kept.display()))?;
    Ok(n + 1)
}

/// The tool's `group` watched until its leader exits, left unreaped for
/// [`Group::stop`]: TERM to the group at the task's `timeout` or when the
/// wrapper is stopped, KILL `grace` after (at most [`STOP_GRACE`] when
/// stopped). Descendants the wrapper adopted are reaped as they end.
/// Whether it timed out, and whether it was stopped.
fn watch(group: &mut Group, timeout: Duration, grace: Duration) -> Result<(bool, bool)> {
    let start = Instant::now();
    let (mut timed_out, mut stopped) = (false, false);
    let mut kill_at: Option<Instant> = None;
    while !group.exited()? {
        reap_adopted(group.id());
        let now = Instant::now();
        let term_by = if !stopped && STOP.load(Ordering::SeqCst) != 0 {
            stopped = true;
            Some(now + grace.min(STOP_GRACE))
        } else if !timed_out && !stopped && now - start >= timeout {
            timed_out = true;
            Some(now + grace)
        } else {
            None
        };
        if let Some(by) = term_by {
            if kill_at.is_none() {
                group.signal(libc::SIGTERM);
            }
            kill_at = Some(kill_at.map_or(by, |k| k.min(by)));
        }
        if kill_at.is_some_and(|k| now >= k) {
            group.signal(libc::SIGKILL);
        }
        std::thread::sleep(TICK);
    }
    Ok((timed_out, stopped))
}

/// Every adopted descendant that has ended, reaped; the tool, `tool`, is
/// left for its [`Group`].
fn reap_adopted(tool: u32) {
    loop {
        // SAFETY: an all-zero siginfo_t is valid; waitid fills it in, and
        // WNOWAIT reaps nothing.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        // SAFETY: `info` is a valid siginfo_t to write to.
        let r = unsafe { libc::waitid(libc::P_ALL, 0, &mut info, options) };
        // SAFETY: waitid succeeded, so si_pid is set: 0 when none ended.
        let pid = unsafe { info.si_pid() };
        if r != 0 || pid == 0 || pid as u32 == tool {
            return;
        }
        // SAFETY: waitpid of a child that has ended, reaping it.
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
    }
}

/// Every descendant the wrapper adopted (one that left the tool's group)
/// ended once the tool's group has: TERM, then KILL after [`LINGER`], until
/// the wrapper has no child left. One whose parent is killed is adopted
/// and killed in a later turn; one that cannot be killed is left after a
/// while.
fn end_adopted() {
    let signal = |signal| {
        for pid in children() {
            // SAFETY: a signal to a child of this wrapper.
            unsafe { libc::kill(pid, signal) };
        }
    };
    signal(libc::SIGTERM);
    let start = Instant::now();
    while start.elapsed() < LINGER + Duration::from_secs(10) {
        // SAFETY: waitpid that keeps no status.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        let left = children();
        if left.is_empty() {
            return;
        }
        if start.elapsed() >= LINGER {
            signal(libc::SIGKILL);
        }
        std::thread::sleep(TICK);
    }
}

/// The wrapper's children, as Linux lists them.
fn children() -> Vec<i32> {
    let me = std::process::id();
    std::fs::read_to_string(format!("/proc/self/task/{me}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// CPU seconds and the largest resident set, in MB, of the wrapper's
/// reaped descendants.
fn children_usage() -> (f64, f64) {
    // SAFETY: getrusage into a struct the wrapper owns.
    let usage = unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_CHILDREN, &mut u);
        u
    };
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    // ru_maxrss is in KB on Linux.
    (
        secs(usage.ru_utime) + secs(usage.ru_stime),
        usage.ru_maxrss as f64 / 1024.0,
    )
}

/// Whether the end of `log` says memory ran out, as Rust's allocator says
/// it ("memory allocation of N bytes failed"). Evidence only: a kill by
/// Slurm for memory is not said here.
fn oom_said(log: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 << 10;
    let Ok(mut f) = std::fs::File::open(log) else {
        return false;
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let mut tail = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).is_err()
        || f.read_to_end(&mut tail).is_err()
    {
        return false;
    }
    String::from_utf8_lossy(&tail)
        .lines()
        .any(|l| l.contains("memory allocation of ") && l.ends_with(" bytes failed"))
}
