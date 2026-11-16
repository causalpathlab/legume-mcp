#![cfg(target_os = "linux")]
//! The benchmark tools of the `legume` server, `legume mcp` run as the
//! binary in a project of its own: `benchmark_plan` checks and plans a spec,
//! naming the field a refusal is about; `benchmark_start` refuses a remote
//! it does not know and starts a run against stand-in ssh and Slurm (no
//! real ssh); `benchmark_status` counts tasks from `events.tsv`. Every
//! reply stays under 4 KB.

#[path = "common/fake_slurm.rs"]
mod fake_slurm;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// The most a reply says.
const REPLY_MAX: usize = 4000;

/// Two seeds, a sim and a score: four tasks, the scores reading the sims.
const SPEC: &str = r#"{"name": "N1", "seeds": {"n": 2, "base": 1},
  "stages": [
    {"name": "sim", "run": ["TOOL1", "--seed", "{rep_seed}", "-o", "{out}/sim"], "makes": ["sim.tsv"]},
    {"name": "score", "threads": 2, "timeout": "10m",
     "run": ["TOOL2", "{sim}/sim.tsv", "-o", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]}]}"#;

/// A sim that writes a value from its seed's last digit.
const SIM: &str = r#"seed=$2; while [ $# -gt 0 ]; do [ "$1" = -o ] && o=$2; shift; done
printf '0.%s' "${seed#"${seed%?}"}" > "$o.tsv""#;

/// A score that passes the sim's value on as an auprc.
const SCORE: &str = r#"s=$(cat "$1"); while [ $# -gt 0 ]; do [ "$1" = -o ] && o=$2; shift; done
printf 'score\tmetric\tvalue\tpart\nt\tauprc\t%s\tall\n' "$s" > "$o""#;

/// A project, a cluster folder beside it, legume's tools there, remote
/// REMOTE1 in remotes.toml, and the stand-in ssh and Slurm.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let w = World {
            dir: tempfile::tempdir().unwrap(),
        };
        fake_slurm::install(&w.root().join("bin"), &w.root().join("state"));
        std::fs::create_dir_all(w.tools()).unwrap();
        std::fs::create_dir_all(w.project()).unwrap();
        std::fs::create_dir_all(w.root().join("cluster")).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_legume"), w.tools().join("legume")).unwrap();
        w.tool("TOOL1", SIM);
        w.tool("TOOL2", SCORE);
        std::fs::create_dir_all(w.remotes().parent().unwrap()).unwrap();
        let toml = format!(
            "[REMOTE1]\nssh = \"HOST1\"\nfolder = \"{}\"\nbin = \"{}\"\npartition = \"PART1\"\n",
            w.root().join("cluster").display(),
            w.tools().display()
        );
        std::fs::write(w.remotes(), toml).unwrap();
        w
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// legume's tools on the cluster, an install's bin.
    fn tools(&self) -> PathBuf {
        self.root().join("pfx/bin")
    }

    fn project(&self) -> PathBuf {
        self.root().join("P1")
    }

    fn remotes(&self) -> PathBuf {
        self.root().join("config/remotes.toml")
    }

    fn bench(&self) -> PathBuf {
        self.project().join("results/benchmarks/N1")
    }

    fn tool(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.tools().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `legume mcp` in the project, the stand-ins first on PATH, and with
    /// `here` on PATH after them when given (tools found here).
    fn serve(&self, here: Option<&Path>) -> Server {
        let mut path = vec![self.root().join("bin")];
        path.extend(here.map(Path::to_path_buf));
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut child = Command::new(env!("CARGO_BIN_EXE_legume"))
            .arg("mcp")
            .current_dir(self.project())
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("FAKE_SLURM", self.root().join("state"))
            .env("LEGUME_REMOTES", self.remotes())
            .env("LEGUME_MCP_PROJECT", self.project())
            .env("LEGUME_MCP_WATCH", "off")
            .env("LEGUME_BENCHMARK_POLL_MS", "100")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Server {
            stdin: Some(child.stdin.take().unwrap()),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 0,
        }
    }
}

/// A `legume mcp` the test talks to, a call at a time.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Server {
    /// Tool `name` called with `args`: its text, and whether it is an
    /// error.
    fn call(&mut self, name: &str, args: Value) -> (String, bool) {
        self.next += 1;
        let id = self.next;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                          "params": { "name": name, "arguments": args } });
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server ended"
            );
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] != json!(id) {
                continue;
            }
            let r = &v["result"];
            let text = r["content"][0]["text"].as_str().unwrap_or_default();
            let text = text.to_string();
            assert!(text.len() < REPLY_MAX, "{} bytes:\n{text}", text.len());
            return (text, r["isError"] == json!(true));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

fn spec() -> Value {
    serde_json::from_str(SPEC).unwrap()
}

#[test]
fn a_refused_spec_names_the_field_to_fix() {
    let w = World::new();
    let mut s = w.serve(None);
    let mut bad = spec();
    bad["stages"][1]["threads"] = json!(0);
    let (text, error) = s.call("benchmark_plan", json!({ "spec": bad, "on": "REMOTE1" }));
    assert!(error, "{text}");
    assert!(
        text.contains("the spec is refused: stages[1].threads: is 0"),
        "{text}"
    );
    assert!(text.contains("Fix that field"), "{text}");
    // A placeholder that names nothing, refused by the plan's checks.
    let mut bad = spec();
    bad["stages"][1]["run"][1] = json!("{nothing}/sim.tsv");
    let (text, error) = s.call("benchmark_plan", json!({ "spec": bad }));
    assert!(error, "{text}");
    assert!(
        text.contains("stages[1].run[1]: {nothing} is no placeholder"),
        "{text}"
    );
    // Not a spec at all.
    let (text, error) = s.call("benchmark_plan", json!({ "spec": 7 }));
    assert!(error && text.contains("`spec` is required"), "{text}");
}

#[test]
fn the_guides_example_checks_for_a_cluster() {
    let guide = include_str!("../docs/collaboration.md");
    let at = guide.find("## Benchmark specs").unwrap();
    let block = &guide[at..];
    let start = block.find("```json\n").unwrap() + "```json\n".len();
    let end = start + block[start..].find("```").unwrap();
    let example: Value = serde_json::from_str(&block[start..end]).unwrap();
    let w = World::new();
    let mut s = w.serve(None);
    let (text, error) = s.call(
        "benchmark_plan",
        json!({ "spec": example, "on": "REMOTE1" }),
    );
    assert!(!error, "{text}");
    assert!(
        text.contains("tasks 27: 3 points x 3 reps x 3 stage methods"),
        "{text}"
    );
    assert!(
        text.contains("fit-M1: 9 tasks, 4 threads, 8 GB, timeout 1h"),
        "{text}"
    );
    assert!(
        text.contains("fit-M2: 9 tasks, 1 thread, 0 GB, timeout 1h, at most 20 at once"),
        "{text}"
    );
    assert!(text.contains("tools, found on REMOTE1"), "{text}");
    assert!(text.contains("TOOL1, TOOL2, TOOL3"), "{text}");
    assert!(text.contains("inputs, from the project's folder on REMOTE1: ref = data/ref.tsv"));
    // Kept in the project, as a run reads it.
    let kept = std::fs::read_to_string(w.project().join("specs-in/demo-1.json")).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&kept).unwrap(), example);
    // Nothing planned here.
    assert!(!w.project().join("results/benchmarks/demo-1").exists());
}

#[test]
fn a_plan_here_plans_as_the_command_line_does_and_a_status_counts_its_events() {
    let w = World::new();
    // The tools here are the cluster's.
    let mut s = w.serve(Some(&w.tools()));
    let (text, error) = s.call("benchmark_plan", json!({ "spec": spec() }));
    assert!(!error, "{text}");
    assert!(text.starts_with("benchmark N1  spec "), "{text}");
    assert!(
        text.contains("tasks 4: 1 points x 2 reps; sim 2, score 2"),
        "{text}"
    );
    assert!(text.contains("books "), "{text}");
    assert!(
        text.contains("score: 2 tasks, 2 threads, 0 GB, timeout 10m, runs TOOL2"),
        "{text}"
    );
    assert!(text.contains("done 0 of 4 (by key)"), "{text}");
    let plan: Value =
        serde_json::from_slice(&std::fs::read(w.bench().join("plan.json")).unwrap()).unwrap();
    let ids: Vec<&str> = plan["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 4);

    // Without the tools here, the refusal says to plan for a cluster.
    let mut bare = w.serve(None);
    let (text, error) = bare.call("benchmark_plan", json!({ "spec": spec() }));
    assert!(error, "{text}");
    assert!(text.contains("not found on PATH: TOOL1"), "{text}");
    assert!(text.contains("call benchmark_plan with `on`"), "{text}");

    // events.tsv written by hand: a sim done, a sim failed, a score
    // running, and one task never started.
    let head = "time\ttask\tattempt\tevent\twall_secs\tcpu_secs\tqueue_secs\tnode";
    let rows = [
        format!("T1\t{}\t\tstart\t\t\t\t", ids[0]),
        format!("T2\t{}\t1\tdone\t1.0\t0.5\t0.1\tNODE1", ids[0]),
        format!("T3\t{}\t\tstart\t\t\t\t", ids[2]),
        format!("T4\t{}\t1\tfailed\t1.0\t0.5\t0.1\tNODE1", ids[2]),
        format!("T5\t{}\t\tstart\t\t\t\t", ids[1]),
    ];
    std::fs::write(
        w.bench().join("events.tsv"),
        format!("{head}\n{}\n", rows.join("\n")),
    )
    .unwrap();
    let (text, error) = s.call("benchmark_status", json!({ "name": "N1" }));
    assert!(!error, "{text}");
    assert!(
        text.contains("tasks 4: 1 done, 1 failed, 0 skipped, 1 running, 1 pending"),
        "{text}"
    );
    assert!(
        text.contains(&format!("T4 {} failed, attempt 1, on NODE1", ids[2])),
        "{text}"
    );
    assert!(text.contains("summary.tsv: not yet"), "{text}");
    let (text, _) = s.call("benchmark_status", json!({}));
    assert!(
        text.contains("benchmarks in results/benchmarks/: N1"),
        "{text}"
    );
    let (text, error) = s.call("benchmark_status", json!({ "name": "N2" }));
    assert!(error && text.contains("no benchmark N2"), "{text}");
    let (text, error) = s.call("benchmark_status", json!({ "name": "../x" }));
    assert!(error && text.contains("name:"), "{text}");
}

#[test]
fn a_start_refuses_a_remote_it_does_not_know_with_how_to_add_it() {
    let w = World::new();
    let mut s = w.serve(None);
    let (text, error) = s.call(
        "benchmark_start",
        json!({ "spec": spec(), "on": "REMOTE2" }),
    );
    assert!(error, "{text}");
    assert!(text.contains("no remote REMOTE2 in"), "{text}");
    assert!(text.contains("it names REMOTE1"), "{text}");
    assert!(text.contains("[REMOTE2]\nssh = \"HOST1\""), "{text}");
    assert!(
        text.contains("legume benchmark run SPEC --on REMOTE2"),
        "{text}"
    );
    // Nothing went over ssh.
    assert!(!w.root().join("state/ssh.log").exists());
    let (text, error) = s.call("benchmark_start", json!({ "spec": spec() }));
    assert!(error && text.contains("`on` is required"), "{text}");
}

#[test]
fn a_start_runs_on_the_cluster_and_the_status_follows_it_to_its_summary() {
    let w = World::new();
    let mut s = w.serve(None);
    let (text, error) = s.call(
        "benchmark_start",
        json!({ "spec": spec(), "on": "REMOTE1" }),
    );
    assert!(!error, "{text}");
    assert!(
        text.starts_with(
            "started benchmark N1 on REMOTE1, as job-1: legume benchmark run \
             specs-in/N1.json --on REMOTE1"
        ),
        "{text}"
    );
    // A second start while it runs is refused.
    let (again, error) = s.call(
        "benchmark_start",
        json!({ "spec": "specs-in/N1.json", "on": "REMOTE1" }),
    );
    if error {
        assert!(again.contains("is running already, as job-1"), "{again}");
    }
    let until = Instant::now() + Duration::from_secs(120);
    let text = loop {
        let (text, error) = s.call("benchmark_status", json!({ "name": "N1" }));
        assert!(!error, "{text}");
        if text.contains("run: job-1, done") || text.contains("run: job-1, failed") {
            break text;
        }
        assert!(Instant::now() < until, "still running:\n{text}");
        std::thread::sleep(Duration::from_millis(300));
    };
    assert!(text.contains("run: job-1, done"), "{text}");
    assert!(
        text.contains("tasks 4: 4 done, 0 failed, 0 skipped, 0 running, 0 pending"),
        "{text}"
    );
    assert!(text.contains("summary.tsv (2 lines):"), "{text}");
    assert!(text.contains("  score\tt\tall\tauprc\t2\t2\t0\t"), "{text}");
    // It went over the stand-in ssh, to the remote's target.
    let ssh = std::fs::read_to_string(w.root().join("state/ssh.log")).unwrap();
    assert!(ssh.contains("HOST1"), "{ssh}");
    // The driver script keeps the spec and the run.
    drop(s);
    let day = std::fs::read_dir(w.project().join("results"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("runall.sh").exists())
        .unwrap();
    let runall = std::fs::read_to_string(day.join("runall.sh")).unwrap();
    assert!(runall.contains("specs-in/N1.json"), "{runall}");
    assert!(
        runall.contains("legume benchmark run specs-in/N1.json --on REMOTE1"),
        "{runall}"
    );
}
