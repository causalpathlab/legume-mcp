#![cfg(target_os = "linux")]
//! The Slurm executor against stand-in `sbatch`, `squeue` and `scancel`
//! (bash scripts this test writes, keeping their queue in a folder), with
//! the real wrapper running each array task, and the SSH shell against a
//! stand-in `ssh`. Every task here lasts a second or two at most.

use legume_mcp::benchmark::executor::Executor;
use legume_mcp::benchmark::plan::Task;
use legume_mcp::benchmark::pool::{Launch, Outcome};
use legume_mcp::benchmark::remote::Shell;
use legume_mcp::benchmark::slurm::{Site, Slurm};
#[path = "common/bench.rs"]
mod bench;
#[path = "common/fake_slurm.rs"]
mod fake_slurm;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A benchmark's folder, its plan of stand-in tasks, and the stand-in
/// Slurm commands.
struct Cluster {
    dir: tempfile::TempDir,
    plan: Vec<Value>,
}

impl Cluster {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fake_slurm::install(&dir.path().join("bin"), &dir.path().join("state"));
        Self {
            dir,
            plan: Vec::new(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    /// Task `id` running `body` in sh with `threads` and `mem_gb`; its
    /// index in the plan.
    fn task(&mut self, id: &str, body: &str, threads: u32, mem_gb: f64, timeout: u64) -> usize {
        let tool = self.root().join(format!("{id}.sh"));
        bench::write_tool(&tool, body);
        let dir = self.root().join("tasks").join(id);
        let shape = (threads, mem_gb, timeout);
        self.plan
            .push(bench::plan_task(id, json!([tool]), &dir, shape, &[]));
        self.plan.len() - 1
    }

    /// `sh` with the stand-in commands first on its PATH.
    fn env(&self, sh: Shell) -> Shell {
        let path = format!(
            "{}:{}",
            self.root().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        sh.with_env("PATH", path)
            .with_env("FAKE_SLURM", self.state())
    }

    fn shell(&self) -> Shell {
        self.env(Shell::local())
    }

    fn slurm_with(&self, shell: Shell, max_array: usize) -> Slurm {
        let plan = self.root().join("plan.json");
        let doc = json!({"grace_secs": 1, "tasks": self.plan});
        std::fs::write(&plan, doc.to_string()).unwrap();
        let site = Site {
            legume: env!("CARGO_BIN_EXE_legume").into(),
            plan: plan.display().to_string(),
            slurm_dir: self.root().join("slurm").display().to_string(),
            run: "RUN1".into(),
            partition: Some("PART1".into()),
            account: None,
            max_array,
        };
        let tasks: Vec<Task> = serde_json::from_value(json!(self.plan)).unwrap();
        let mut s = Slurm::new(shell, site, tasks, 1);
        s.lost_after = Duration::from_millis(300);
        s
    }

    fn slurm(&self, max_array: usize) -> Slurm {
        self.slurm_with(self.shell(), max_array)
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.state().join(name)).unwrap_or_default()
    }

    /// Array task `job` of the run, queued in `state` (`R` or `PD`).
    fn queue(&self, job: &str, state: &str) {
        let q = self.state().join("q").join(job);
        std::fs::create_dir_all(&q).unwrap();
        std::fs::write(q.join("name"), "RUN1\n").unwrap();
        std::fs::write(q.join("state"), format!("{state}\n")).unwrap();
    }
}

fn launch(tasks: &[usize]) -> Vec<Launch> {
    tasks
        .iter()
        .map(|&task| Launch {
            task,
            gpus: Vec::new(),
        })
        .collect()
}

/// Every end, polled until `n` tasks have ended.
fn ends(s: &mut Slurm, n: usize) -> Vec<(usize, Outcome)> {
    let mut all = Vec::new();
    let start = Instant::now();
    while all.len() < n {
        assert!(start.elapsed() < Duration::from_secs(20), "ended: {all:?}");
        std::thread::sleep(Duration::from_millis(100));
        let ended = s.poll().unwrap();
        all.extend(ended.into_iter().map(|e| (e.task, e.outcome)));
    }
    all.sort_by_key(|e| e.0);
    all
}

#[test]
fn tasks_of_one_shape_go_in_as_one_array_job() {
    let mut c = Cluster::new();
    let a = c.task("T1", "true", 2, 1.0, 30);
    let b = c.task("T2", "exit 3", 2, 1.0, 30);
    let d = c.task("T3", "true", 4, 2.5, 30);
    let mut s = c.slurm(100);
    s.launch(&launch(&[a, b, d])).unwrap();
    assert_eq!(
        ends(&mut s, 3),
        vec![(a, Outcome::Done), (b, Outcome::Failed), (d, Outcome::Done)]
    );
    let sbatch = c.log("sbatch.log");
    let calls: Vec<&str> = sbatch.lines().collect();
    assert_eq!(calls.len(), 2, "{sbatch}");
    for want in [
        "--job-name=RUN1",
        "--array=0-1",
        "--cpus-per-task=2",
        "--mem=1024M",
        "--time=6",
        "--partition=PART1",
    ] {
        assert!(calls[0].contains(want), "{want}: {}", calls[0]);
    }
    assert!(!calls[0].contains("--account"), "{}", calls[0]);
    assert!(calls[1].contains("--array=0-0") && calls[1].contains("--mem=2560M"));
    let job: Value = serde_json::from_str(
        &std::fs::read_to_string(c.root().join("tasks/T2/slurm.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        job,
        json!({"array_job_id": "101", "index": 1, "key": "key-T2"})
    );
    // The list says which task each array index runs.
    let mut lists: Vec<String> = std::fs::read_dir(c.root().join("slurm"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "tasks"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with("RUN1-"), "{name}");
            std::fs::read_to_string(&p).unwrap()
        })
        .collect();
    lists.sort();
    assert_eq!(lists, ["T1\nT2\n", "T3\n"]);
}

#[test]
fn a_launch_again_keeps_the_earlier_attempt_and_its_log() {
    let mut c = Cluster::new();
    let a = c.task("T1", "printf x > out", 1, 1.0, 30);
    let b = c.task("T2", "true", 1, 1.0, 30);
    // T2 was placed once and never ran: no attempt to keep.
    std::fs::create_dir_all(c.root().join("tasks/T2")).unwrap();
    std::fs::write(c.root().join("tasks/T2/slurm.json"), "{}").unwrap();
    let mut s = c.slurm(100);
    s.launch(&launch(&[a, b])).unwrap();
    assert_eq!(
        ends(&mut s, 2),
        vec![(a, Outcome::Done), (b, Outcome::Done)]
    );
    assert!(!c.root().join("tasks/.attempts/T2.1").exists());
    s.launch(&launch(&[a])).unwrap();
    assert_eq!(ends(&mut s, 1), vec![(a, Outcome::Done)]);
    // The first attempt, whole, as its job left it.
    let first = c.root().join("tasks/.attempts/T1.1");
    for f in ["done.json", "exit.json", "slurm.json", "log", "out"] {
        assert!(first.join(f).exists(), "{f}");
    }
    let placed = |d: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(d.join("slurm.json")).unwrap()).unwrap()
    };
    assert_eq!(placed(&first)["array_job_id"], "101");
    assert_eq!(placed(&c.root().join("tasks/T1"))["array_job_id"], "102");
    // Each job's own Slurm log, by its id.
    let logs: Vec<String> = std::fs::read_dir(c.root().join("slurm"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".log"))
        .collect();
    assert_eq!(logs.len(), 3, "{logs:?}");
    for want in ["_101_0.log", "_101_1.log", "_102_0.log"] {
        assert!(logs.iter().any(|l| l.ends_with(want)), "{want}: {logs:?}");
    }
}

#[test]
fn an_array_job_holds_at_most_max_array_tasks() {
    let mut c = Cluster::new();
    let ids: Vec<usize> = (1..=3)
        .map(|n| c.task(&format!("T{n}"), "true", 1, 1.0, 30))
        .collect();
    let mut s = c.slurm(2);
    s.launch(&launch(&ids)).unwrap();
    ends(&mut s, 3);
    let sbatch = c.log("sbatch.log");
    let arrays: Vec<&str> = sbatch
        .split_whitespace()
        .filter(|a| a.starts_with("--array="))
        .collect();
    assert_eq!(arrays, ["--array=0-1", "--array=0-0"]);
}

#[test]
fn one_squeue_per_round_for_any_number_of_tasks() {
    let mut c = Cluster::new();
    let ids: Vec<usize> = (1..=6)
        .map(|n| c.task(&format!("T{n}"), "sleep 1", 1, 1.0, 30))
        .collect();
    let mut s = c.slurm(100);
    s.launch(&launch(&ids)).unwrap();
    let mut rounds = 0;
    let mut ended = 0;
    while ended < 6 {
        std::thread::sleep(Duration::from_millis(200));
        ended += s.poll().unwrap().len();
        rounds += 1;
        assert!(rounds < 100);
    }
    assert_eq!(c.log("squeue.log").lines().count(), rounds);
}

#[test]
fn a_timeout_and_a_stop_are_said_by_the_wrapper() {
    let mut c = Cluster::new();
    let slow = c.task("T1", "sleep 30", 1, 1.0, 1);
    let long = c.task("T2", "sleep 30", 1, 1.0, 60);
    let mut s = c.slurm(100);
    s.launch(&launch(&[slow, long])).unwrap();
    // The stop goes to T2's array task once it runs.
    let started = c.root().join("tasks/T2/log");
    let start = Instant::now();
    while !started.exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    s.cancel(&[long]).unwrap();
    assert_eq!(
        ends(&mut s, 2),
        vec![(slow, Outcome::Timeout), (long, Outcome::Stopped)]
    );
    // Only the run's own array task, by its id: T2's own array job, its
    // timeout being another shape than T1's.
    assert_eq!(c.log("scancel.log").trim(), "102_0");
}

#[test]
fn a_task_gone_from_the_queue_without_an_exit_is_lost_after_a_while() {
    let mut c = Cluster::new();
    let t = c.task("T1", "true", 1, 1.0, 30);
    let shell = c.shell().with_env("FAKE_SLURM_HOLD", "1");
    let mut s = c.slurm_with(shell, 100);
    s.launch(&launch(&[t])).unwrap();
    // Queued: not lost, however long it waits.
    std::thread::sleep(Duration::from_millis(400));
    assert!(s.poll().unwrap().is_empty());
    std::fs::remove_dir_all(c.state().join("q/101_0")).unwrap();
    // Gone: lost once it has been gone for lost_after.
    assert!(s.poll().unwrap().is_empty());
    assert_eq!(ends(&mut s, 1), vec![(t, Outcome::Lost)]);
}

#[test]
fn a_stop_after_a_task_left_the_queue_still_stops_the_rest() {
    let mut c = Cluster::new();
    let a = c.task("T1", "true", 1, 1.0, 30);
    let b = c.task("T2", "true", 1, 1.0, 30);
    let shell = c.shell().with_env("FAKE_SLURM_HOLD", "1");
    let mut s = c.slurm_with(shell, 100);
    s.launch(&launch(&[a, b])).unwrap();
    // T1 left the queue since the last poll; scancel says so, and T2 is
    // stopped all the same.
    std::fs::remove_dir_all(c.state().join("q/101_0")).unwrap();
    s.cancel(&[a, b]).unwrap();
    assert!(!c.state().join("q/101_1").exists());
    assert_eq!(
        ends(&mut s, 2),
        vec![(a, Outcome::Stopped), (b, Outcome::Stopped)]
    );
}

#[test]
fn a_pending_task_stopped_is_stopped_not_lost() {
    let mut c = Cluster::new();
    let t = c.task("T1", "true", 1, 1.0, 30);
    let shell = c.shell().with_env("FAKE_SLURM_HOLD", "1");
    let mut s = c.slurm_with(shell, 100);
    s.launch(&launch(&[t])).unwrap();
    s.cancel(&[t]).unwrap();
    assert_eq!(ends(&mut s, 1), vec![(t, Outcome::Stopped)]);
}

#[test]
fn a_failed_submission_cancels_what_went_in() {
    let mut c = Cluster::new();
    let a = c.task("T1", "sleep 5", 1, 1.0, 30);
    let b = c.task("T2", "sleep 5", 2, 1.0, 30);
    let shell = c.shell().with_env("FAKE_SLURM_FAIL_AT", "2");
    let mut s = c.slurm_with(shell, 100);
    let e = s.launch(&launch(&[a, b])).unwrap_err().to_string();
    assert!(e.contains("submission failed"), "{e}");
    assert_eq!(c.log("scancel.log").trim(), "101");
    // No task keeps a slurm.json naming the cancelled job.
    assert!(!c.root().join("tasks/T1/slurm.json").exists());
    // Nothing is watched: no end comes.
    assert!(s.poll().unwrap().is_empty());
}

#[test]
fn the_ssh_shell_shares_one_connection() {
    let c = Cluster::new();
    let sh = c.env(Shell::ssh("HOST1"));
    let ran = sh.run("echo \"hi $((1 + 2))\"").unwrap();
    assert_eq!(ran.stdout, "hi 3\n");
    sh.close();
    let log = c.log("ssh.log");
    let calls: Vec<&str> = log.lines().collect();
    assert!(calls[0].contains("ControlMaster=auto") && calls[0].contains("ControlPersist=10m"));
    assert!(calls[0].ends_with("-- HOST1 bash -s"), "{}", calls[0]);
    assert!(calls[1].contains("-O exit -- HOST1"), "{}", calls[1]);
}

#[test]
fn resume_finds_done_tasks_by_key_and_adopts_queued_ones() {
    let mut c = Cluster::new();
    let ids: Vec<usize> = (1..=5)
        .map(|n| c.task(&format!("T{n}"), "true", 1, 1.0, 30))
        .collect();
    let dir = |n: usize| c.root().join(format!("tasks/T{n}"));
    for n in 1..=5 {
        std::fs::create_dir_all(dir(n)).unwrap();
    }
    // T1 done with its key, T2 done with another; T3 queued; T4 placed
    // but ended, so not adopted; T5 queued for another key, so cancelled
    // when asked to rerun what changed.
    std::fs::write(dir(1).join("done.json"), r#"{"key": "key-T1"}"#).unwrap();
    std::fs::write(dir(2).join("done.json"), r#"{"key": "old"}"#).unwrap();
    for (n, key) in [(3, "key-T3"), (4, "key-T4"), (5, "old")] {
        let placed =
            format!("{{\n  \"array_job_id\": \"77\", \"index\": {n}, \"key\": \"{key}\"\n}}\n");
        std::fs::write(dir(n).join("slurm.json"), placed).unwrap();
        c.queue(&format!("77_{n}"), "R");
    }
    std::fs::write(dir(4).join("exit.json"), "{}").unwrap();
    let mut s = c.slurm(100);
    let r = s.resume().unwrap();
    assert_eq!(r.done, vec![ids[0]]);
    assert_eq!(r.adopted, vec![ids[2]]);
    assert_eq!(r.rekeyed, vec![ids[1], ids[4]]);
    // Resume changes nothing; the runner cancels once it goes ahead.
    assert_eq!(c.log("scancel.log").trim(), "");
    s.cancel_rekeyed().unwrap();
    assert_eq!(c.log("scancel.log").trim(), "77_5");
    // The adopted task is watched, and ends by its exit.json.
    assert!(s.poll().unwrap().is_empty());
    let exit = json!({
        "task": "T3", "key": "key-T3", "attempt": 1, "outcome": "done", "exit": 0,
        "signal": null, "timed_out": false, "stopped": false, "oom": false, "started": 1.0,
        "secs": 1.0, "cpu_secs": 0.5, "max_rss_mb": 3.0, "node": "NODE1",
    });
    std::fs::write(dir(3).join("exit.json"), exit.to_string()).unwrap();
    assert_eq!(ends(&mut s, 1), vec![(ids[2], Outcome::Done)]);
}
