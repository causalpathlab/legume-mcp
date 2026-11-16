#![cfg(target_os = "linux")]
//! `legume benchmark run SPEC` without `--on`: the spec planned here, each
//! task run on this machine under the real wrapper, within the cores the
//! spec books, the scores gathered here; a run again reuses what is done,
//! Ctrl-C stops the tasks, and `--detach` runs it in the background. Every
//! task here lasts a second or two at most, but the one stopped.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Two points and two seeds, a sim and two scoring methods compared and
/// plotted: twelve tasks, on two cores, two tasks at a time.
const SPEC: &str = r#"{"name": "N1", "seeds": {"n": 2, "base": 1}, "cores": 2,
  "axes": {"A": ["1", "2"]},
  "stages": [
    {"name": "sim", "run": ["TOOL1", "--seed", "{rep_seed}", "-o", "{out}/sim"], "makes": ["sim.tsv"]},
    {"name": "score", "methods": {
      "M0": {"run": ["TOOL2", "{sim}/sim.tsv", "0", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]},
      "M1": {"run": ["TOOL2", "{sim}/sim.tsv", "0.{A}", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]}}}],
  "compare": [["M1/t", "M0/t"]],
  "plot": {"x": "A"}}"#;

/// A sim that makes 0.5, after `delay`.
fn sim(delay: &str) -> String {
    format!(
        r#"sleep {delay}; while [ $# -gt 0 ]; do [ "$1" = -o ] && o=$2; shift; done; printf 0.5 > "$o.tsv""#
    )
}

/// A score: the sim's value plus the bias, as a score file.
const SCORE: &str = r#"v=$(cat "$1"); s=$(echo "$v + $2" | bc -l)
printf 'score\tmetric\tvalue\tpart\nt\tauprc\t%s\tall\n' "$s" > "$3""#;

/// A stand-in tool at `p`, a sh script running `body`.
fn write_tool(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A project with its tools first on PATH.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new(sim_delay: &str) -> Self {
        let w = World {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(w.bin()).unwrap();
        write_tool(&w.bin().join("TOOL1"), &sim(sim_delay));
        write_tool(&w.bin().join("TOOL2"), SCORE);
        std::fs::write(w.root().join("spec.json"), SPEC).unwrap();
        w
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn bin(&self) -> PathBuf {
        self.root().join("bin")
    }

    fn bench(&self) -> PathBuf {
        self.root().join("results/benchmarks/N1")
    }

    fn legume(&self, args: &[&str]) -> Command {
        let path = std::env::join_paths(std::iter::once(self.bin()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let mut c = Command::new(env!("CARGO_BIN_EXE_legume"));
        c.args(args)
            .current_dir(self.root())
            .env("PATH", path)
            .env("LEGUME_MCP_PROJECT", self.root())
            .env("LEGUME_BENCHMARK_POLL_MS", "100")
            .stdin(Stdio::null());
        c
    }

    fn run(&self) -> Output {
        self.legume(&["benchmark", "run", "spec.json"])
            .output()
            .unwrap()
    }

    /// How many rows of events.tsv are `event`.
    fn count(&self, event: &str) -> usize {
        std::fs::read_to_string(self.bench().join("events.tsv"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.split('\t').nth(3) == Some(event))
            .count()
    }
}

fn said(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Until `ok` holds, for at most `within`.
fn wait_for(within: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < within {
        if ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ok()
}

#[test]
fn a_spec_runs_here_is_gathered_and_a_run_again_reuses_it() {
    let w = World::new("0");
    let o = w.run();
    let s = said(&o);
    assert!(o.status.success(), "{s}");
    assert!(s.contains("12 tasks: 12 done"), "{s}");
    assert!(s.contains("gathered 8 of 8 scored tasks"), "{s}");
    assert_eq!((w.count("start"), w.count("done")), (12, 12));
    let summary = std::fs::read_to_string(w.bench().join("summary.tsv")).unwrap();
    let m1: Vec<&str> = summary
        .lines()
        .filter(|l| l.starts_with("2\tM1\t"))
        .collect();
    assert_eq!(m1.len(), 1, "{summary}");
    // 0.5 and a bias of 0.2: the mean of two replicates.
    assert!(m1[0].contains("\t2\t2\t0\t0.7\t"), "{summary}");
    assert!(w.bench().join("paired.tsv").exists());
    assert!(w.bench().join("summary.svg").exists());

    // Again: every task is reused, none starts.
    let o = w.run();
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("12 done before"), "{}", said(&o));
    assert_eq!((w.count("start"), w.count("reused")), (12, 12));
}

#[test]
fn ctrl_c_stops_the_tasks_here() {
    let w = World::new("30");
    let child = w
        .legume(&["benchmark", "run", "spec.json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Two sims at once on two cores: both started.
    assert!(wait_for(Duration::from_secs(20), || w.count("start") == 2));
    // SAFETY: a signal to the child this test started.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let started = Instant::now();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(130), "{}", said(&o));
    assert!(started.elapsed() < Duration::from_secs(25));
    assert_eq!(w.count("stopped"), 2, "{}", said(&o));
}

#[test]
fn a_detached_run_here_goes_on_and_stop_cancels_it() {
    let w = World::new("30");
    let o = w
        .legume(&["benchmark", "run", "spec.json", "--detach"])
        .output()
        .unwrap();
    let s = said(&o);
    assert!(o.status.success(), "{s}");
    assert!(s.contains("started benchmark N1 on this machine"), "{s}");
    let runner = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(w.bench().join("runner.json")).unwrap()).unwrap()
    };
    assert!(runner()["remote"].is_null(), "{}", runner());
    assert!(wait_for(Duration::from_secs(20), || w.count("start") == 2));
    let o = w.legume(&["benchmark", "stop", "N1"]).output().unwrap();
    assert!(o.status.success(), "{}", said(&o));
    assert!(wait_for(Duration::from_secs(25), || runner()["exit"]
        .as_i64()
        .is_some()));
    assert_eq!(runner()["exit"], 130);
    assert_eq!(w.count("stopped"), 2);
}
