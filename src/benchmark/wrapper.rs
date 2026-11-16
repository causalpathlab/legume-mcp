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
//! `exit.json` always ([`TaskExit`], its `outcome` the end the pool
//! counts), so a reader that sees `exit.json` sees the end. A
//! rerun clears both first and keeps the last log as `log.<attempt>`.

use super::executor::TaskExit;
use super::plan::Task;
use super::pool::Outcome;
use crate::command::{canonical_flags, failed, flag_pairs, required, usage, Flag};
use crate::procgroup::Group;
use anyhow::{Context, Result};
use serde_json::json;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
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
    let plan: super::plan::PlanDoc =
        serde_json::from_str(&text).with_context(|| format!("reading {}", plan_file.display()))?;
    let Some(task) = plan.tasks.iter().find(|t| t.id == id) else {
        return Err(usage(format!("{}: no task {id}", plan_file.display())));
    };
    let ran = run(task, Duration::from_secs(plan.grace_secs));
    if ran.is_err() {
        failed_itself(task);
    }
    match ran? {
        None => Ok(()),
        // The wrapper exits as the tool did, said as no usage is.
        Some((code, said)) => Err(failed(code, format!("task {}: {said}", task.id))),
    }
}

/// `exit.json` for task `t` when the wrapper itself failed (why is in
/// the Slurm log), so the runner sees the end now and not only once the
/// task is lost; nothing when even that cannot be written.
fn failed_itself(t: &Task) {
    let exit = TaskExit {
        task: t.id.clone(),
        key: t.key.clone(),
        attempt: 0,
        outcome: Outcome::Failed,
        exit: None,
        signal: None,
        timed_out: false,
        stopped: false,
        oom: false,
        started: super::executor::unix_secs(),
        secs: 0.0,
        cpu_secs: 0.0,
        max_rss_mb: 0.0,
        node: super::machine::host().unwrap_or_default(),
    };
    if let Ok(text) = crate::pretty(&exit) {
        let _ = crate::job::replace(&t.dir, "exit.json", text.as_bytes());
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
    super::stop::catch();

    // Where this attempt's part of the job's output starts: Slurm appends
    // every attempt's to one file.
    let output_from = job_output().map_or(0, |(_, len)| len);
    let oom_kills_before = oom_kills();
    let start = Instant::now();
    let started = super::executor::unix_secs();
    let mut cmd = std::process::Command::new(first);
    cmd.args(rest)
        .envs(&t.env)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // The task's time is the tool's, not that of ending what it left.
    let (status, timed_out, stopped, secs) = match Group::spawn(&mut cmd) {
        Ok(mut group) => {
            let (timed_out, stopped) =
                watch(&mut group, Duration::from_secs(t.timeout_secs), grace)?;
            let secs = start.elapsed().as_secs_f64();
            let status = group.stop(LINGER)?;
            end_adopted();
            (status, timed_out, stopped, secs)
        }
        Err(e) => {
            // As a shell says a command it cannot run: 127.
            let mut log = std::fs::OpenOptions::new().append(true).open(&log_path)?;
            writeln!(log, "legume benchmark task: running {first}: {e}")?;
            let status = std::process::ExitStatus::from_raw(127 << 8);
            (status, false, false, start.elapsed().as_secs_f64())
        }
    };
    let (cpu_secs, max_rss_mb) = children_usage();
    // Ended for its memory: the kernel killed something in the job's
    // memory group meanwhile, or Slurm said so in the job's own output
    // (this wrapper's stdout); a tool that still exited 0 was not.
    let memory_killed = status.code() != Some(0)
        && (oom_kills()
            .zip(oom_kills_before)
            .is_some_and(|(now, before)| now > before)
            || slurm_said_oom(output_from));
    let oom = memory_killed || oom_said(&log_path);
    // Ended for its memory, then stopped, then timed out, then done, then
    // out of memory by the tool's say, then failed.
    let (outcome, not_done) = if memory_killed {
        let why = "ended for exceeding its memory".to_string();
        (Outcome::Oom, Some(why))
    } else if stopped {
        (Outcome::Stopped, Some("stopped".to_string()))
    } else if timed_out {
        let why = format!("timed out after {} s", t.timeout_secs);
        (Outcome::Timeout, Some(why))
    } else {
        let why = match (status.code(), status.signal()) {
            (Some(0), _) => write_done(t, secs, cpu_secs).err(),
            (Some(c), _) => Some(format!("exit {c}")),
            (None, s) => Some(format!("signal {}", s.unwrap_or(0))),
        };
        let outcome = match &why {
            None => Outcome::Done,
            Some(_) if oom => Outcome::Oom,
            Some(_) => Outcome::Failed,
        };
        (outcome, why)
    };
    let exit = TaskExit {
        task: t.id.clone(),
        key: t.key.clone(),
        attempt,
        outcome,
        exit: status.code(),
        signal: status.signal(),
        timed_out,
        stopped,
        oom,
        started,
        secs,
        cpu_secs,
        max_rss_mb,
        node: super::machine::host().unwrap_or_default(),
    };
    crate::job::replace(dir, "exit.json", crate::pretty(&exit)?.as_bytes())?;
    // A tool that exits 0 and is not done (its timeout, a stop, a makes
    // file not made) ends the wrapper with 1.
    // The wrapper's code: the tool's, or 128 plus its signal.
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    Ok(not_done.map(|why| (code.max(1), why)))
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
        let term_by = if !stopped && super::stop::asked() {
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
    super::tail::tail(log, TAIL)
        .lines()
        .any(|l| l.contains("memory allocation of ") && l.ends_with(" bytes failed"))
}

/// How many times the kernel has killed a process for memory in this
/// process's memory group and its parents up to its Slurm job's (or only
/// its own, outside Slurm), from cgroup v2's `memory.events` or v1's
/// `memory.oom_control`; none when neither can be read.
fn oom_kills() -> Option<u64> {
    let groups = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let (root, path) = groups.lines().find_map(|l| {
        let mut parts = l.splitn(3, ':');
        let (_, controllers, path) = (parts.next()?, parts.next()?, parts.next()?);
        match controllers {
            "" => Some(("/sys/fs/cgroup".to_string(), path)),
            c if c.split(',').any(|c| c == "memory") => {
                Some(("/sys/fs/cgroup/memory".to_string(), path))
            }
            _ => None,
        }
    })?;
    let mut total = None;
    for dir in oom_groups(path) {
        let dir = Path::new(&root).join(dir.trim_start_matches('/'));
        let text = std::fs::read_to_string(dir.join("memory.events"))
            .or_else(|_| std::fs::read_to_string(dir.join("memory.oom_control")));
        if let Some(n) = text.ok().as_deref().and_then(oom_kill_count) {
            total = Some(total.unwrap_or(0) + n);
        }
    }
    total
}

/// The memory groups whose kills count for a process in group `path`:
/// it and its parents up to the one named for its Slurm job (`job_N`),
/// or only it when none is.
fn oom_groups(path: &str) -> Vec<&str> {
    let Some(job) = path.find("/job_") else {
        return vec![path];
    };
    // The job's group ends at the next `/`, or is the whole path.
    let job_end = path[job + 1..].find('/').map_or(path.len(), |i| job + 1 + i);
    let mut groups = vec![path];
    let mut at = path;
    while at.len() > job_end {
        at = &at[..at.rfind('/').unwrap_or(job_end)];
        groups.push(at);
    }
    groups
}

/// The `oom_kill` count in a `memory.events` or `memory.oom_control`.
fn oom_kill_count(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("oom_kill ")?.trim().parse().ok())
}

/// The job's output file (this wrapper's stdout, when it is a file) and
/// its length now.
fn job_output() -> Option<(std::path::PathBuf, u64)> {
    let path = std::fs::read_link("/proc/self/fd/1").ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    meta.is_file().then_some((path, meta.len()))
}

/// Whether Slurm said, in the job's output past byte `from` (this
/// attempt's part), that it ended the job for its memory: slurmstepd
/// writes that there before it stops the job.
fn slurm_said_oom(from: u64) -> bool {
    job_output().is_some_and(|(path, _)| slurm_oom_in(&super::tail::tail_from(&path, from, TAIL)))
}

/// Whether `text` has slurmstepd's word that a job went over its memory.
fn slurm_oom_in(text: &str) -> bool {
    text.lines().any(|l| {
        l.starts_with("slurmstepd")
            && (l.contains("exceeded memory limit")
                || l.contains("Exceeded job memory limit")
                || l.contains("oom-kill"))
    })
}

/// How much of the end of a log is read for what it says.
const TAIL: u64 = 64 << 10;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slurms_word_on_memory_is_read_and_nothing_else() {
        for said in [
            "slurmstepd: error: Job 1 exceeded memory limit (2340 > 1024), being killed",
            "slurmstepd: error: Exceeded job memory limit",
            "slurmstepd: error: Detected 1 oom-kill event(s) in step 1.batch cgroup.",
        ] {
            assert!(slurm_oom_in(&format!("x\n{said}\n")), "{said}");
        }
        assert!(!slurm_oom_in("my tool: exceeded memory limit\n"));
        assert!(!slurm_oom_in(
            "slurmstepd: error: *** JOB 1 CANCELLED ***\n"
        ));
    }

    #[test]
    fn the_kernels_kills_count_up_to_the_slurm_job() {
        assert_eq!(
            oom_groups("/slurm/uid_1/job_2/step_batch/task_0"),
            [
                "/slurm/uid_1/job_2/step_batch/task_0",
                "/slurm/uid_1/job_2/step_batch",
                "/slurm/uid_1/job_2"
            ]
        );
        assert_eq!(oom_groups("/user.slice/a.scope"), ["/user.slice/a.scope"]);
        assert_eq!(
            oom_kill_count("low 0\nhigh 0\nmax 3\noom 1\noom_kill 2\n"),
            Some(2)
        );
        assert_eq!(
            oom_kill_count("oom_kill_disable 0\nunder_oom 0\noom_kill 5\n"),
            Some(5)
        );
        assert_eq!(oom_kill_count("oom_kill_disable 0\n"), None);
    }

    #[test]
    fn a_tail_starts_past_what_an_earlier_attempt_wrote() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("out.log");
        std::fs::write(
            &f,
            "old: exceeded
new
",
        )
        .unwrap();
        assert_eq!(
            super::super::tail::tail_from(&f, 14, TAIL),
            "new
"
        );
    }
}
