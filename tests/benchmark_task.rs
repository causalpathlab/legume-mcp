#![cfg(target_os = "linux")]
//! `legume benchmark task`: one task of a plan run under the wrapper, as
//! on a compute node, with stand-in tools (shell scripts this test writes).
//! Every task here lasts a second or two at most.

#[path = "common/bench.rs"]
mod bench;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A plan of stand-in tasks in a folder of its own.
struct Bench {
    dir: tempfile::TempDir,
    tasks: Vec<Value>,
}

impl Bench {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            tasks: Vec::new(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Task `id` running `body` in sh, making `makes`, `timeout` seconds.
    fn task(&mut self, id: &str, body: &str, makes: &[&str], timeout: u64) -> PathBuf {
        let tool = self.root().join(format!("{id}.sh"));
        bench::write_tool(&tool, body);
        let dir = self.root().join("tasks").join(id);
        let argv = json!([tool, "ARG1"]);
        self.tasks
            .push(bench::plan_task(id, argv, &dir, (2, 1.0, timeout), makes));
        dir
    }

    fn plan(&self, grace: u64) -> PathBuf {
        let p = self.root().join("plan.json");
        let plan = json!({"name": "B1", "grace_secs": grace, "tasks": self.tasks});
        std::fs::write(&p, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
        p
    }
}

/// `legume benchmark task --plan PLAN`, outside any array job.
fn wrapper(plan: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_legume"));
    c.args(["benchmark", "task", "--plan"])
        .arg(plan)
        .env_remove("SLURM_ARRAY_TASK_ID");
    c
}

fn task(plan: &Path, id: &str) -> Output {
    wrapper(plan).args(["--task", id]).output().unwrap()
}

/// The task on line `index` of `list`, or the array index's line.
fn listed(plan: &Path, list: &Path, index: Option<&str>) -> Command {
    let mut c = wrapper(plan);
    c.arg("--list").arg(list);
    if let Some(i) = index {
        c.args(["--index", i]);
    }
    c
}

/// Task `id` started, and its tool's pid, once the tool writes it to
/// `pidfile`.
fn start(plan: &Path, id: &str, pidfile: &Path) -> (Child, i32) {
    let child = wrapper(plan)
        .args(["--task", id])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !pidfile.exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    // The pid is written whole once its line is.
    std::thread::sleep(Duration::from_millis(100));
    (child, read_pid(pidfile))
}

fn read_pid(p: &Path) -> i32 {
    std::fs::read_to_string(p).unwrap().trim().parse().unwrap()
}

fn log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("log")).unwrap()
}

fn read(p: &Path) -> Value {
    let t = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&t).unwrap()
}

fn said(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Process `pid` is gone (or a zombie no one will reap: not running).
fn gone(pid: i32) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
    match stat {
        Err(_) => true,
        Ok(s) => s.rsplit(')').next().unwrap().trim_start().starts_with('Z'),
    }
}

fn wait_gone(pid: i32, within: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < within {
        if gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    gone(pid)
}

#[test]
fn a_task_that_makes_its_files_is_done() {
    let mut b = Bench::new();
    let dir = b.task(
        "C1",
        r#"echo "$1 $RAYON_NUM_THREADS $(pwd)"; printf x > out.tsv"#,
        &["out.tsv"],
        30,
    );
    let o = task(&b.plan(5), "C1");
    assert!(o.status.success(), "{}", said(&o));
    let exit = read(&dir.join("exit.json"));
    assert_eq!(exit["exit"], 0);
    assert_eq!(exit["outcome"], "done");
    assert_eq!(exit["signal"], Value::Null);
    assert_eq!(exit["timed_out"], false);
    assert_eq!(exit["key"], "key-C1");
    assert!(exit["secs"].as_f64().unwrap() >= 0.0);
    assert!(exit["max_rss_mb"].as_f64().unwrap() > 0.0, "{exit}");
    assert!(exit["node"].as_str().is_some_and(|n| !n.is_empty()));
    let done = read(&dir.join("done.json"));
    assert_eq!(done["key"], "key-C1");
    assert_eq!(done["makes"][0]["path"], "out.tsv");
    assert_eq!(done["makes"][0]["bytes"], 1);
    // Its argv, env and folder, with output to the log.
    let log = log(&dir);
    let real = dir.canonicalize().unwrap();
    assert_eq!(log.trim(), format!("ARG1 2 {}", real.display()));
}

#[test]
fn done_needs_every_makes_file() {
    let mut b = Bench::new();
    let dir = b.task("C1", "printf x > a; : > b", &["a", "b", "c"], 30);
    let o = task(&b.plan(5), "C1");
    assert!(!o.status.success());
    assert_eq!(read(&dir.join("exit.json"))["exit"], 0);
    assert!(!dir.join("done.json").exists());
    let s = said(&o);
    // Both the empty and the missing file are named.
    assert!(s.contains('b') && s.contains('c'), "{s}");
}

#[test]
fn a_failing_tool_gives_its_exit() {
    let mut b = Bench::new();
    let dir = b.task("C1", "echo bad >&2; exit 7", &[], 30);
    let o = task(&b.plan(5), "C1");
    assert_eq!(o.status.code(), Some(7));
    let exit = read(&dir.join("exit.json"));
    assert_eq!(
        (exit["exit"].as_i64(), exit["outcome"].as_str()),
        (Some(7), Some("failed"))
    );
    assert!(!dir.join("done.json").exists());
    assert!(log(&dir).contains("bad"));
}

#[test]
fn timeout_term_then_kill() {
    // The tool ignores TERM: it is killed after the grace.
    let mut b = Bench::new();
    let dir = b.task("C1", "trap '' TERM; sleep 30", &[], 1);
    let start = Instant::now();
    let o = task(&b.plan(1), "C1");
    let took = start.elapsed();
    assert!(!o.status.success());
    let exit = read(&dir.join("exit.json"));
    assert_eq!(exit["timed_out"], true);
    assert_eq!(exit["outcome"], "timeout");
    assert_eq!(exit["signal"], 9, "{exit}");
    assert!(took < Duration::from_secs(8), "{took:?}");
    assert!(took >= Duration::from_secs(2), "{took:?}");
}

#[test]
fn a_tool_that_exits_0_on_its_timeout_is_not_done() {
    let mut b = Bench::new();
    let dir = b.task(
        "C1",
        "printf x > out.tsv; trap 'exit 0' TERM; sleep 30 & wait",
        &["out.tsv"],
        1,
    );
    let o = task(&b.plan(5), "C1");
    assert!(!o.status.success(), "{}", said(&o));
    let exit = read(&dir.join("exit.json"));
    assert_eq!(exit["timed_out"], true, "{exit}");
    assert_eq!(exit["exit"], 0, "{exit}");
    assert!(!dir.join("done.json").exists());
    assert!(said(&o).contains("timed out"), "{}", said(&o));
}

#[test]
fn a_tool_that_ends_on_term_is_stopped_within_the_grace() {
    let mut b = Bench::new();
    let dir = b.task("C1", "sleep 30", &[], 1);
    let start = Instant::now();
    task(&b.plan(20), "C1");
    let exit = read(&dir.join("exit.json"));
    assert_eq!(exit["timed_out"], true, "{exit}");
    assert_eq!(exit["signal"], 15, "{exit}");
    assert!(start.elapsed() < Duration::from_secs(6));
}

#[test]
fn term_to_the_wrapper_stops_the_tool() {
    // As Slurm's scancel does: TERM to the wrapper, whose tool is in a
    // group of its own.
    let mut b = Bench::new();
    let pidfile = b.root().join("tool.pid");
    let dir = b.task(
        "C1",
        &format!("echo $$ > {}; sleep 30", pidfile.display()),
        &[],
        60,
    );
    let (mut child, tool) = start(&b.plan(20), "C1", &pidfile);
    // SAFETY: a plain kill of the child this test started.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = child.wait().unwrap();
    assert!(!status.success());
    assert!(
        wait_gone(tool, Duration::from_secs(5)),
        "tool {tool} left running"
    );
    let exit = read(&dir.join("exit.json"));
    assert_eq!(exit["stopped"], true, "{exit}");
    assert_eq!(exit["outcome"], "stopped");
    assert_eq!(exit["timed_out"], false);
}

#[test]
fn orphan_that_closes_fds_is_killed() {
    // The tool starts a grandchild in a session of its own, which closes
    // its descriptors, and exits: the grandchild is killed all the same.
    let mut b = Bench::new();
    let pidfile = b.root().join("orphan.pid");
    let dir = b.task(
        "C1",
        &format!(
            "setsid sh -c 'exec 0<&- 1>&- 2>&-; echo $$ > {}; exec sleep 30' & sleep 0.5; printf x > out",
            pidfile.display()
        ),
        &["out"],
        30,
    );
    let o = task(&b.plan(5), "C1");
    assert!(o.status.success(), "{}", said(&o));
    assert!(dir.join("done.json").exists());
    let orphan = read_pid(&pidfile);
    assert!(
        wait_gone(orphan, Duration::from_secs(2)),
        "orphan {orphan} left running"
    );
}

#[test]
fn a_rerun_moves_the_last_attempt_aside_whole() {
    let mut b = Bench::new();
    let flag = b.root().join("fail");
    let dir = b.task(
        "C1",
        &format!("[ -e {} ] && exit 4; printf x > out", flag.display()),
        &["out"],
        30,
    );
    let plan = b.plan(5);
    assert!(task(&plan, "C1").status.success());
    assert!(dir.join("done.json").exists());
    std::fs::write(&flag, "").unwrap();
    assert_eq!(task(&plan, "C1").status.code(), Some(4));
    assert!(!dir.join("done.json").exists(), "a stale done.json kept");
    assert!(!dir.join("out").exists(), "an earlier output kept in place");
    assert_eq!(read(&dir.join("exit.json"))["exit"], 4);
    // The finished attempt is kept as it was, beside the task folders.
    let first = b.root().join("tasks/.attempts/C1.1");
    for f in ["done.json", "exit.json", "log", "out"] {
        assert!(first.join(f).exists(), "{f}");
    }
    assert_eq!(read(&first.join("exit.json"))["exit"], 0);
}

#[test]
fn a_new_attempt_keeps_its_placement() {
    let mut b = Bench::new();
    let dir = b.task("C1", "true", &[], 30);
    let plan = b.plan(5);
    std::fs::create_dir_all(&dir).unwrap();
    // As a launch leaves it: only slurm.json, not moved.
    std::fs::write(dir.join("slurm.json"), "{}").unwrap();
    assert!(task(&plan, "C1").status.success());
    assert_eq!(read(&dir.join("exit.json"))["attempt"], 1);
    assert!(!b.root().join("tasks/.attempts").exists());
    // A requeue: the ended attempt moved, its placement copied back.
    assert!(task(&plan, "C1").status.success());
    assert_eq!(read(&dir.join("exit.json"))["attempt"], 2);
    assert!(dir.join("slurm.json").exists());
    assert!(b.root().join("tasks/.attempts/C1.1/slurm.json").exists());
}

#[test]
fn the_task_comes_from_a_list_and_the_array_index() {
    let mut b = Bench::new();
    b.task("C1", "printf 1 > out", &["out"], 30);
    let d2 = b.task("C2", "printf 2 > out", &["out"], 30);
    let plan = b.plan(5);
    let list = b.root().join("array.tasks");
    std::fs::write(&list, "C1\nC2\n").unwrap();
    let o = listed(&plan, &list, None)
        .env("SLURM_ARRAY_TASK_ID", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert_eq!(std::fs::read_to_string(d2.join("out")).unwrap(), "2");
    assert!(!b.root().join("tasks/C1/exit.json").exists());
}

#[test]
fn unknown_tasks_and_indices_are_refused() {
    let mut b = Bench::new();
    b.task("C1", "true", &[], 30);
    let plan = b.plan(5);
    let o = task(&plan, "C9");
    assert_eq!(o.status.code(), Some(2), "{}", said(&o));
    assert!(said(&o).contains("C9"));
    let list = b.root().join("array.tasks");
    std::fs::write(&list, "C1\n").unwrap();
    let o = listed(&plan, &list, Some("3")).output().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", said(&o));
}

#[test]
fn each_attempt_is_numbered_and_kept() {
    let mut b = Bench::new();
    let dir = b.task("C1", "echo run", &[], 30);
    let plan = b.plan(5);
    for _ in 0..3 {
        task(&plan, "C1");
    }
    assert_eq!(read(&dir.join("exit.json"))["attempt"], 3);
    let attempts = b.root().join("tasks/.attempts");
    for (n, d) in [
        (1, attempts.join("C1.1")),
        (2, attempts.join("C1.2")),
        (3, dir),
    ] {
        assert_eq!(read(&d.join("exit.json"))["attempt"], n);
        assert_eq!(std::fs::read_to_string(d.join("log")).unwrap(), "run\n");
    }
}

#[test]
fn a_tool_that_cannot_run_exits_127() {
    let mut b = Bench::new();
    let dir = b.task("C1", "true", &[], 30);
    std::fs::remove_file(b.root().join("C1.sh")).unwrap();
    let o = task(&b.plan(5), "C1");
    assert_eq!(o.status.code(), Some(127), "{}", said(&o));
    assert_eq!(read(&dir.join("exit.json"))["exit"], 127);
    assert!(log(&dir).contains("C1.sh"));
}

#[test]
fn a_tool_exit_of_2_is_not_a_usage_error() {
    let mut b = Bench::new();
    b.task("C1", "exit 2", &[], 30);
    let o = task(&b.plan(5), "C1");
    assert_eq!(o.status.code(), Some(2));
    let s = said(&o);
    assert!(
        s.contains("exit 2") && !s.contains("Usage") && !s.contains("--plan"),
        "{s}"
    );
}

#[test]
fn rusts_out_of_memory_message_is_evidence() {
    let mut b = Bench::new();
    let d1 = b.task(
        "C1",
        "echo 'memory allocation of 1024 bytes failed' >&2; kill -ABRT $$",
        &[],
        30,
    );
    let d2 = b.task("C2", "echo 'out of memory, maybe'; exit 1", &[], 30);
    let plan = b.plan(5);
    task(&plan, "C1");
    task(&plan, "C2");
    let exit = read(&d1.join("exit.json"));
    assert_eq!(exit["oom"], true, "{exit}");
    assert_eq!(exit["signal"], 6, "{exit}");
    assert_eq!(exit["outcome"], "oom");
    assert_eq!(read(&d2.join("exit.json"))["oom"], false);
}

/// Task `id` of `plan` run with its stdout appended to `out`, as Slurm
/// gives a job's output.
fn task_into(plan: &Path, id: &str, out: &Path) {
    let file = std::fs::OpenOptions::new().append(true).open(out).unwrap();
    wrapper(plan)
        .args(["--task", id])
        .env("JOB_OUT", out)
        .stdout(file)
        .status()
        .unwrap();
}

#[test]
fn slurms_memory_kill_is_out_of_memory_not_stopped() {
    let mut b = Bench::new();
    // As slurmstepd does: its word in the job's output, then TERM to the
    // job (the wrapper, the tool's parent).
    let d1 = b.task(
        "C1",
        "echo 'slurmstepd: error: Job 1 exceeded memory limit (2340 > 1024), being killed' >> \"$JOB_OUT\"; kill -TERM $PPID; sleep 5",
        &[],
        30,
    );
    let d2 = b.task("C2", "exit 1", &[], 30);
    let plan = b.plan(5);
    let out = b.root().join("job.log");
    std::fs::write(&out, "").unwrap();
    task_into(&plan, "C1", &out);
    let exit = read(&d1.join("exit.json"));
    assert_eq!(exit["outcome"], "oom", "{exit}");
    assert_eq!(exit["oom"], true);
    // The word an earlier attempt left in the output is not this one's.
    task_into(&plan, "C2", &out);
    assert_eq!(read(&d2.join("exit.json"))["outcome"], "failed");
}

#[test]
fn a_stopped_task_gets_at_most_twenty_seconds_of_grace() {
    // The plan's grace is longer than Slurm's KillWait leaves: the tool,
    // which ignores TERM, is killed at 20 s, before Slurm would kill the
    // wrapper too.
    let mut b = Bench::new();
    let pidfile = b.root().join("tool.pid");
    let dir = b.task(
        "C1",
        &format!(
            "trap '' TERM; echo $$ > {}; while :; do sleep 1; done",
            pidfile.display()
        ),
        &[],
        600,
    );
    let (mut child, _) = start(&b.plan(300), "C1", &pidfile);
    // SAFETY: a plain kill of the child this test started.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let stopped = Instant::now();
    child.wait().unwrap();
    let took = stopped.elapsed();
    assert!(
        took >= Duration::from_secs(19) && took < Duration::from_secs(25),
        "{took:?}"
    );
    let exit = read(&dir.join("exit.json"));
    assert_eq!(exit["stopped"], true, "{exit}");
    assert_eq!(exit["signal"], 9, "{exit}");
}

#[test]
fn what_the_tool_leaves_running_is_killed_when_it_ignores_term() {
    let mut b = Bench::new();
    let pidfile = b.root().join("left.pid");
    let dir = b.task(
        "C1",
        &format!(
            "trap '' TERM; sleep 30 & echo $! > {}; printf x > out",
            pidfile.display()
        ),
        &["out"],
        30,
    );
    let o = task(&b.plan(5), "C1");
    assert!(o.status.success(), "{}", said(&o));
    let left = read_pid(&pidfile);
    assert!(
        wait_gone(left, Duration::from_secs(2)),
        "{left} left running"
    );
    // The task's time is the tool's, not the second it took to end that.
    assert!(read(&dir.join("exit.json"))["secs"].as_f64().unwrap() < 0.9);
}

#[test]
fn the_index_counts_every_line_of_the_list() {
    let mut b = Bench::new();
    b.task("C1", "true", &[], 30);
    let plan = b.plan(5);
    let list = b.root().join("array.tasks");
    std::fs::write(&list, "\nC1\n").unwrap();
    let run = |i| listed(&plan, &list, Some(i)).output().unwrap();
    assert!(run("1").status.success());
    let o = run("0");
    assert_eq!(o.status.code(), Some(2));
    assert!(said(&o).contains("line 0 is empty"), "{}", said(&o));
}

#[test]
fn a_folder_whose_attempt_cannot_be_set_aside_is_not_written_to() {
    let mut b = Bench::new();
    let dir = b.task("T1", "printf new > log", &[], 30);
    // The earlier attempt cannot be moved aside: a file is where it goes.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("log"), "earlier\n").unwrap();
    std::fs::write(b.root().join("tasks/.attempts"), "").unwrap();
    let plan = b.plan(1);
    let o = task(&plan, "T1");
    assert!(!o.status.success());
    assert!(said(&o).contains(".attempts"), "{}", said(&o));
    assert_eq!(
        std::fs::read_to_string(dir.join("log")).unwrap(),
        "earlier\n"
    );
    assert!(!dir.join("exit.json").exists());
}

#[test]
fn a_job_placed_for_another_key_does_not_run() {
    let mut b = Bench::new();
    let dir = b.task("T1", "printf ran > out", &["out"], 30);
    let plan = b.plan(1);
    std::fs::create_dir_all(&dir).unwrap();
    let placed = r#"{"array_job_id": "77", "index": 0, "key": "old"}"#;
    std::fs::write(dir.join("slurm.json"), placed).unwrap();
    // Job 77 was placed for the old key: refused, nothing written.
    let o = wrapper(&plan)
        .args(["--task", "T1"])
        .env("SLURM_ARRAY_JOB_ID", "77")
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(said(&o).contains("placed for key old"), "{}", said(&o));
    assert!(!dir.join("out").exists() && !dir.join("exit.json").exists());
    // Another job, placed since: it runs.
    let o = wrapper(&plan)
        .args(["--task", "T1"])
        .env("SLURM_ARRAY_JOB_ID", "78")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert!(dir.join("out").exists());
}
