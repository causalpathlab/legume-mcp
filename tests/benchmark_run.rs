#![cfg(target_os = "linux")]
//! `legume benchmark run SPEC --on R1`, end to end against stand-in ssh,
//! srun, sbatch, squeue and scancel, with a "cluster" folder beside the
//! project: the spec planned there, each task run by the real wrapper,
//! the events kept here, and a run again resuming. Every task here lasts
//! a second or two at most.

#[path = "common/fake_slurm.rs"]
mod fake_slurm;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Two seeds, a sim and a score: four tasks, the scores reading the sims.
const SPEC: &str = r#"{"name": "N1", "seeds": {"n": 2, "base": 1},
  "stages": [
    {"name": "sim", "run": ["TOOL1", "--seed", "{rep_seed}", "-o", "{out}/sim"], "makes": ["sim.tsv"]},
    {"name": "score", "run": ["TOOL2", "{sim}/sim.tsv", "-o", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]}]}"#;

/// A project here, a cluster folder, legume's tools there, and the
/// stand-in commands.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new(tool1: &str) -> Self {
        let w = World {
            dir: tempfile::tempdir().unwrap(),
        };
        fake_slurm::install(&w.root().join("bin"), &w.state());
        std::fs::create_dir_all(w.tools()).unwrap();
        std::fs::create_dir_all(w.project()).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_legume"), w.tools().join("legume")).unwrap();
        w.tool("TOOL1", tool1);
        w.tool(
            "TOOL2",
            r#"s=$(cat "$1"); while [ $# -gt 0 ]; do [ "$1" = -o ] && o=$2; shift; done
printf 'score\tmetric\tvalue\tpart\nt\tauprc\t%s\tall\n' "$s" > "$o""#,
        );
        std::fs::write(w.project().join("spec.json"), SPEC).unwrap();
        w
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    /// The cluster's legume tools, an install's bin folder.
    fn tools(&self) -> PathBuf {
        self.root().join("pfx/bin")
    }

    fn project(&self) -> PathBuf {
        self.root().join("P1")
    }

    fn cluster(&self) -> PathBuf {
        self.root().join("cluster")
    }

    fn remotes(&self) -> PathBuf {
        self.root().join("config/remotes.toml")
    }

    fn tool(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.tools().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Remote R1 kept in remotes.toml.
    fn with_remote(self) -> Self {
        std::fs::create_dir_all(self.remotes().parent().unwrap()).unwrap();
        let toml = format!(
            "[R1]\nssh = \"HOST1\"\nfolder = \"{}\"\nbin = \"{}\"\npartition = \"PART1\"\n",
            self.cluster().display(),
            self.tools().display()
        );
        std::fs::write(self.remotes(), toml).unwrap();
        self
    }

    fn command(&self) -> Command {
        self.legume(&["benchmark", "run", "spec.json", "--on", "R1"])
    }

    /// legume with `args`, in the project, against the stand-ins.
    fn legume(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.root().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new(env!("CARGO_BIN_EXE_legume"));
        c.args(args)
            .current_dir(self.project())
            .env("PATH", path)
            .env("FAKE_SLURM", self.state())
            .env("LEGUME_REMOTES", self.remotes())
            .env("LEGUME_MCP_PROJECT", self.project())
            .env("LEGUME_BENCHMARK_POLL_MS", "100");
        c
    }

    fn run(&self) -> Output {
        self.command().stdin(Stdio::null()).output().unwrap()
    }

    fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.state().join(name)).unwrap_or_default()
    }

    /// The rows of events.tsv here, split on tabs, without its header.
    fn events(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.project().join("results/benchmarks/N1/events.tsv"))
            .unwrap();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("time\ttask\tattempt\tevent\twall_secs\tcpu_secs\tqueue_secs\tnode")
        );
        lines
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect()
    }

    /// How many events are `event`.
    fn count(&self, event: &str) -> usize {
        self.events().iter().filter(|r| r[3] == event).count()
    }
}

fn said(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A sim that makes a value from its seed's last digit, 0.d, which the
/// score passes on.
const MAKES_SIM: &str = r#"seed=$2; while [ $# -gt 0 ]; do [ "$1" = -o ] && o=$2; shift; done
printf '0.%s' "${seed#"${seed%?}"}" > "$o.tsv""#;

#[test]
fn a_spec_is_planned_on_the_cluster_run_there_and_resumed() {
    let w = World::new(MAKES_SIM).with_remote();
    let o = w.run();
    assert!(o.status.success(), "{}", said(&o));
    let s = said(&o);
    assert!(s.contains("4 tasks: 4 done"), "{s}");
    // Planned on a node, in the project's folder on the cluster.
    // Named for the project and a key of where it is here.
    let projects: Vec<_> = std::fs::read_dir(w.cluster())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        projects.len() == 1 && projects[0].starts_with("P1-") && projects[0].len() == 11,
        "{projects:?}"
    );
    let bench = w.cluster().join(&projects[0]).join("results/benchmarks/N1");
    assert!(bench.join("plan.json").exists());
    assert!(w.log("srun.log").contains("--partition=PART1"));
    // The sims went in as one array job, then the scores as another.
    let sbatch = w.log("sbatch.log");
    assert_eq!(sbatch.lines().count(), 2, "{sbatch}");
    assert!(sbatch.lines().all(|l| l.contains("--array=0-1")));
    assert_eq!((w.count("start"), w.count("done")), (4, 4));
    let done = w.events().into_iter().find(|r| r[3] == "done").unwrap();
    assert_eq!(done[2], "1", "{done:?}");
    assert!(done[6].parse::<f64>().is_ok(), "queue_secs: {done:?}");
    assert!(!done[7].is_empty(), "the node: {done:?}");
    // The scores came here and were gathered.
    assert!(
        s.contains("gathered 2 of 2 scored tasks: results.tsv, summary.tsv"),
        "{s}"
    );
    let local = w.project().join("results/benchmarks/N1");
    let summary = std::fs::read_to_string(local.join("summary.tsv")).unwrap();
    let rows: Vec<&str> = summary.lines().collect();
    assert_eq!(
        rows[0],
        "method\tscore\tpart\tmetric\treps\tn\tn_nan\tmean\tsd\tmedian\tq1\tq3"
    );
    assert!(
        rows[1].starts_with("score\tt\tall\tauprc\t2\t2\t0\t"),
        "{summary}"
    );
    let results = std::fs::read_to_string(local.join("results.tsv")).unwrap();
    assert_eq!(results.lines().count(), 3, "{results}");
    // And gathered again here, by hand, the same.
    let o = w
        .legume(&["benchmark", "gather", "spec.json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("gathered 2 of 2"), "{}", said(&o));
    assert_eq!(
        std::fs::read_to_string(local.join("summary.tsv")).unwrap(),
        summary
    );

    // Again: every task is reused, nothing goes in.
    let o = w.run();
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("4 done before"), "{}", said(&o));
    assert_eq!(w.log("sbatch.log").lines().count(), 2);
    assert_eq!(w.count("reused"), 4);
}

#[test]
fn a_failed_sim_fails_the_run_and_its_score_is_skipped() {
    let w = World::new("echo no >&2; exit 1").with_remote();
    let o = w.run();
    assert_eq!(o.status.code(), Some(1), "{}", said(&o));
    let s = said(&o);
    assert!(s.contains("2 failed") && s.contains("2 skipped"), "{s}");
    assert_eq!((w.count("failed"), w.count("skipped")), (2, 2));
    assert!(s.contains("gathered 0 of 2 scored tasks"), "{s}");
    assert_eq!(w.log("sbatch.log").lines().count(), 1);
}

#[test]
fn a_dropped_connection_is_tried_again() {
    let w = World::new(MAKES_SIM).with_remote();
    // Calls: the plan, plan.json, the resume, then the first launch.
    let o = w.command().env("FAKE_SSH_FAIL_AT", "4").output().unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("trying again"), "{}", said(&o));
    assert_eq!(w.count("done"), 4);
}

#[test]
fn a_new_remote_is_asked_for_and_kept() {
    let w = World::new(MAKES_SIM);
    let mut child = w
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let answers = format!(
        "HOST1\n{}\n{}\n\n\n",
        w.cluster().display(),
        w.tools().display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(answers.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", said(&o));
    let kept = std::fs::read_to_string(w.remotes()).unwrap();
    assert!(
        kept.starts_with("[R1]\nssh = \"HOST1\"\n") && !kept.contains("partition"),
        "{kept}"
    );
    assert!(said(&o).contains("kept remote R1"));
}

#[test]
fn an_unknown_remote_without_answers_is_refused() {
    let w = World::new(MAKES_SIM);
    let o = w.run();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    assert!(!w.remotes().exists());
}

/// `child` waited for, up to `within`.
fn wait(child: &mut Child, within: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < within {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn ctrl_c_cancels_the_runs_tasks() {
    let w = World::new("sleep 30").with_remote();
    let mut child = w
        .command()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Both sims running under their wrappers.
    let start = Instant::now();
    let running = || {
        std::fs::read_dir(w.state().join("q"))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().join("pid").exists())
                    .count()
            })
            .unwrap_or(0)
    };
    while running() < 2 {
        assert!(start.elapsed() < Duration::from_secs(20), "never ran");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: a plain signal to the child this test started.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let status = wait(&mut child, Duration::from_secs(20)).expect("still running");
    assert_eq!(status.code(), Some(130));
    let cancelled = w.log("scancel.log");
    assert_eq!(cancelled.split_whitespace().count(), 2, "{cancelled}");
    assert_eq!((w.count("stop-requested"), w.count("stopped")), (2, 2));
}

#[test]
fn install_submits_one_job_with_this_legume_and_not_while_one_is_queued() {
    let w = World::new(MAKES_SIM).with_remote();
    let install = || {
        w.legume(&["benchmark", "install", "--on", "R1"])
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let o = install();
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("Submitted job 101"), "{}", said(&o));
    let installs = w.root().join("pfx/install");
    assert!(installs.join("slurm-install.sh").exists());
    let runs: Vec<_> = std::fs::read_dir(&installs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let job = std::fs::read_to_string(runs[0].join("install.sbatch")).unwrap();
    let pinned = format!("legume-mcp@{}:benchmark", env!("CARGO_PKG_VERSION"));
    assert!(
        job.contains(&pinned) && job.contains("--partition=PART1"),
        "{job}"
    );

    // While job 101 is queued, another install says so and starts none,
    // even past a later install that never got a job.
    std::fs::create_dir_all(installs.join("29991231-000000")).unwrap();
    let o = install();
    assert!(o.status.success(), "{}", said(&o));
    assert!(
        said(&o).contains("job 101 is still in Slurm's queue, submitted"),
        "{}",
        said(&o)
    );
    assert_eq!(w.log("sbatch.log").lines().count(), 1);

    // Nor while another install is being submitted.
    std::fs::remove_dir_all(w.state().join("q/101")).unwrap();
    let mut held = Command::new("flock")
        .arg(installs.join(".lock"))
        .args(["sleep", "30"])
        .spawn()
        .unwrap();
    let start = Instant::now();
    while Command::new("flock")
        .args(["-n"])
        .arg(installs.join(".lock"))
        .arg("true")
        .status()
        .unwrap()
        .success()
    {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the lock is never held"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let o = install();
    held.kill().unwrap();
    held.wait().unwrap();
    assert!(
        said(&o).contains("being submitted right now"),
        "{}",
        said(&o)
    );
    assert_eq!(w.log("sbatch.log").lines().count(), 1);
}

#[test]
fn install_wants_bin_to_be_an_install_prefixs() {
    let w = World::new(MAKES_SIM);
    std::fs::create_dir_all(w.remotes().parent().unwrap()).unwrap();
    std::fs::write(
        w.remotes(),
        "[R1]\nssh = \"HOST1\"\nfolder = \"/x\"\nbin = \"/x/tools\"\n",
    )
    .unwrap();
    let o = w
        .legume(&["benchmark", "install", "--on", "R1"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    assert!(said(&o).contains("PREFIX/bin"), "{}", said(&o));
    assert_eq!(w.log("sbatch.log"), "");
}

#[test]
fn a_cluster_legume_of_another_version_is_refused() {
    let w = World::new(MAKES_SIM).with_remote();
    // The cluster's legume plans, then says it is 0.0.1.
    std::fs::remove_file(w.tools().join("legume")).unwrap();
    w.tool(
        "legume",
        &format!(
            "{} \"$@\" || exit\n[ \"$2\" = plan ] && sed -i 's/\"legume\": \"[^\"]*\"/\"legume\": \"0.0.1\"/' \
             results/benchmarks/N1/plan.json",
            env!("CARGO_BIN_EXE_legume")
        ),
    );
    let o = w.run();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    let s = said(&o);
    assert!(s.contains("the cluster's legume is 0.0.1"), "{s}");
    assert!(s.contains("legume benchmark install --on R1"), "{s}");
    assert_eq!(w.log("sbatch.log"), "");
}

#[test]
fn an_install_of_another_legume_is_refused_before_planning() {
    let w = World::new(MAKES_SIM).with_remote();
    std::fs::write(
        w.root().join("pfx/.crates2.json"),
        r#"{"installs":{"legume-mcp 0.0.1 (registry+https://example.org/index)":{}}}"#,
    )
    .unwrap();
    let o = w.run();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    assert!(
        said(&o).contains("the cluster's legume is 0.0.1"),
        "{}",
        said(&o)
    );
    assert_eq!(w.log("srun.log"), "", "no planning job");

    // The install's record of this legume plans as before.
    std::fs::write(
        w.root().join("pfx/.crates2.json"),
        format!(
            r#"{{"installs":{{"legume-mcp {} (registry+https://example.org/index)":{{}}}}}}"#,
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    let o = w.run();
    assert!(o.status.success(), "{}", said(&o));
}

#[test]
fn a_task_wanting_gpus_is_refused_on_a_cluster() {
    let w = World::new(MAKES_SIM).with_remote();
    let spec = SPEC.replace(
        r#""makes": ["sim.tsv"]"#,
        r#""makes": ["sim.tsv"], "gpus": 1"#,
    );
    std::fs::write(
        w.project().join("spec.json"),
        spec.replace(r#""seeds""#, r#""gpus": [0], "seeds""#),
    )
    .unwrap();
    let o = w.run();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    assert!(said(&o).contains("wants GPUs"), "{}", said(&o));
    assert_eq!(w.log("sbatch.log"), "");
}

/// `runner.json` in the benchmark's folder here, read.
fn runner_json(w: &World) -> Option<serde_json::Value> {
    let bytes = std::fs::read(w.project().join("results/benchmarks/N1/runner.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[test]
fn a_detached_run_returns_at_once_and_stop_cancels_its_tasks() {
    let w = World::new("sleep 30").with_remote();
    let mut detach = w.command();
    detach.arg("--detach");
    let started = Instant::now();
    let o = detach.stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert!(started.elapsed() < Duration::from_secs(10));
    let s = said(&o);
    assert!(
        s.contains("started benchmark N1 on R1 in the background, as pid "),
        "{s}"
    );
    assert!(s.contains("log: results/benchmarks/N1/run.log"), "{s}");
    let r = runner_json(&w).unwrap();
    assert_eq!(r["remote"], "R1");
    assert_eq!(r["spec_key"].as_str().unwrap().len(), 32);
    assert!(r["exit"].is_null(), "{r}");
    // Another, while it is alive, is refused.
    let o = w
        .command()
        .arg("--detach")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    assert!(
        said(&o).contains("is running already, as pid"),
        "{}",
        said(&o)
    );
    // Both sims running under their wrappers, then stopped.
    let start = Instant::now();
    let running = || {
        std::fs::read_dir(w.state().join("q"))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().join("pid").exists())
                    .count()
            })
            .unwrap_or(0)
    };
    while running() < 2 {
        assert!(start.elapsed() < Duration::from_secs(20), "never ran");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    let o = w.legume(&["benchmark", "stop", "N1"]).output().unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert!(
        said(&o).contains("sent TERM to the runner of benchmark N1"),
        "{}",
        said(&o)
    );
    let start = Instant::now();
    let exit = loop {
        if let Some(code) = runner_json(&w).and_then(|r| r["exit"].as_i64()) {
            break code;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "still running");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(exit, 130);
    let cancelled = w.log("scancel.log");
    assert_eq!(cancelled.split_whitespace().count(), 2, "{cancelled}");
    assert_eq!(w.count("stopped"), 2);
    let log = std::fs::read_to_string(w.project().join("results/benchmarks/N1/run.log")).unwrap();
    assert!(log.contains("planning N1 on R1"), "{log}");
    assert!(log.contains("the run ended, exit 130"), "{log}");
    // Nothing is left to stop.
    let o = w.legume(&["benchmark", "stop", "N1"]).output().unwrap();
    assert_eq!(o.status.code(), Some(3), "{}", said(&o));
    assert!(
        said(&o).contains("its runner ended, exit 130"),
        "{}",
        said(&o)
    );
}
