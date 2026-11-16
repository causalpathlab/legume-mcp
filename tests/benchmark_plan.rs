//! `legume benchmark plan`, run as the binary in a project of its own,
//! with stand-in tools (shell scripts this test writes) first on PATH:
//! what it writes, what it keys, what it says, and what it refuses.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A project with a folder of stand-in tools.
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let p = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(p.bin()).unwrap();
        p
    }

    /// A project whose TOOL1 and TOOL2 do nothing.
    fn with_tools() -> Self {
        let p = Self::new();
        p.tool("TOOL1", "true");
        p.tool("TOOL2", "true");
        p
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn bin(&self) -> PathBuf {
        self.root().join("bin")
    }

    /// The stand-in tool `name`, a shell script running `body`.
    fn tool(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let p = self.bin().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `text` as the spec file `name`.
    fn spec(&self, name: &str, text: &str) -> PathBuf {
        let p = self.root().join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    /// `legume benchmark` with `args`, in the project, the tools first on
    /// PATH.
    fn legume(&self, args: &[&str]) -> Output {
        let path = std::env::join_paths(std::iter::once(self.bin()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_legume"))
            .arg("benchmark")
            .args(args)
            .current_dir(self.root())
            .env("PATH", path)
            .env("LEGUME_MCP_PROJECT", self.root())
            .output()
            .unwrap()
    }

    /// `legume benchmark plan SPEC` with `more` after it.
    fn plan_spec(&self, spec: &Path, more: &[&str]) -> Output {
        let mut args = vec!["plan", spec.to_str().unwrap()];
        args.extend_from_slice(more);
        self.legume(&args)
    }

    fn bench(&self, name: &str) -> PathBuf {
        self.root().join("results/benchmarks").join(name)
    }

    fn plan(&self, name: &str) -> Value {
        let text = std::fs::read(self.bench(name).join("plan.json")).unwrap();
        serde_json::from_slice(&text).unwrap()
    }

    /// The rows of benchmark `name`'s versions.tsv, split on tabs.
    fn versions(&self, name: &str) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.bench(name).join("versions.tsv"))
            .unwrap()
            .lines()
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) {
    assert!(o.status.success(), "{}\n{}", stdout(o), stderr(o));
}

/// The stderr of `o`, which exited 3, refused.
fn refused(o: &Output) -> String {
    let e = stderr(o);
    assert_eq!(o.status.code(), Some(3), "{e}");
    e
}

/// `TWO` with the top-level `fields` before its seeds.
fn two_with(fields: &str) -> String {
    TWO.replace("\"seeds\"", &format!("{fields}, \"seeds\""))
}

/// Each task's id and key.
fn keys(plan: &Value) -> Vec<(String, String)> {
    plan["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let s = |k: &str| c[k].as_str().unwrap().to_string();
            (s("id"), s("key"))
        })
        .collect()
}

/// A two-stage spec running the stand-in TOOL1, then TOOL2.
const TWO: &str = r#"{"name": "N1", "seeds": {"n": 2, "base": 1},
  "axes": {"A": ["1", "2"]},
  "stages": [
    {"name": "sim", "run": ["TOOL1", "--a", "{A}", "--seed", "{rep_seed}", "-o", "{out}/sim"], "makes": ["sim.tsv"]},
    {"name": "score", "run": ["TOOL2", "{sim}/sim.tsv", "-o", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]}]}"#;

// The first real benchmark's shape (B1c): pinto lra-simulate --one per
// point and replicate, then legume score over the same parquet. Every
// file plan writes is there; 100 tasks; each sim argv filled and pointed
// at its snapshot; each score task reads its own sim.
#[test]
fn the_lra_one_spec_plans_its_tasks_tools_and_keys() {
    let p = Project::new();
    p.tool("pinto", "[ \"$1\" = --version ] && echo 'pinto 0.17.0'");
    std::fs::create_dir_all(p.root().join("RUN1")).unwrap();
    std::fs::write(p.root().join("RUN1/manifest.json"), "{}\n").unwrap();
    let spec = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lra_one.spec.json");
    let o = p.plan_spec(&spec, &[]);
    ok(&o);
    let bench = p.bench("lra-one");
    let plan = p.plan("lra-one");
    let key = plan["spec_key"].as_str().unwrap();
    let canonical = std::fs::read(bench.join(format!("specs/{key}.json"))).unwrap();
    assert!(canonical.ends_with(b"}\n"));
    assert!(bench.join("machine.json").is_file());
    let tasks = plan["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 100);
    // What the benchmark books comes first, the tasks last.
    let text = std::fs::read_to_string(bench.join("plan.json")).unwrap();
    assert!(
        text.starts_with("{\n  \"name\": \"lra-one\",\n  \"spec_key\": "),
        "{}",
        &text[..200]
    );
    assert!(text.ends_with("]\n}\n"));

    let rows = p.versions("lra-one");
    assert_eq!(
        rows[0],
        ["tool", "source", "content_key", "exec", "version"]
    );
    let pinto = rows.iter().find(|r| r[0] == "pinto").unwrap();
    assert_eq!(pinto[1], p.bin().join("pinto").display().to_string());
    let snap = bench.join("tools").join(&pinto[2]).join("pinto");
    assert_eq!(pinto[3], snap.display().to_string());
    assert_eq!(pinto[4], "pinto 0.17.0");
    let legume = rows.iter().find(|r| r[0] == "{legume}").unwrap();
    assert!(legume[3].ends_with("/legume"), "{legume:?}");
    assert!(legume[4].starts_with("legume-mcp "), "{legume:?}");

    let sim = tasks
        .iter()
        .find(|c| {
            c["stage"] == "sim"
                && c["rep"] == 2
                && c["point"]["conf"] == "CT1"
                && c["point"]["effect_pve"] == "0.1"
        })
        .unwrap();
    let id = sim["id"].as_str().unwrap();
    assert!(id.ends_with(".r2.sim"), "{id}");
    let out = bench.join("tasks").join(id);
    let argv: Vec<&str> = sim["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    // Inputs are resolved through links (on macOS the temp folder is one).
    let run1 = std::fs::canonicalize(p.root().join("RUN1/manifest.json")).unwrap();
    let rep_seed = sim["rep_seed"].as_u64().unwrap().to_string();
    let sim_out = format!("{}/sim", out.display());
    assert_eq!(
        argv,
        [
            snap.to_str().unwrap(),
            "lra-simulate",
            "--one",
            "--from",
            run1.to_str().unwrap(),
            "--set",
            "effect_pve=0.1",
            "--set",
            "confounder=celltype",
            "--set",
            "confound_scale=1",
            "--set",
            "n_planted=10",
            "--set",
            "n_null=10",
            "--seed",
            rep_seed.as_str(),
            "-o",
            sim_out.as_str(),
        ]
    );
    assert_eq!(sim["threads"], 2);
    assert_eq!(sim["mem_gb"], 4.0);
    assert_eq!(sim["timeout_secs"], 1800);
    assert_eq!(sim["env"]["RAYON_NUM_THREADS"], "2");
    let score = tasks
        .iter()
        .find(|c| c["stage"] == "score" && c["upstream"][0] == id)
        .unwrap();
    assert_eq!(score["argv"][0], legume[3]);
    assert_eq!(
        score["argv"][3],
        format!("{}/sim.lra-simulate.parquet", out.display())
    );
    assert_eq!(score["timeout_secs"], 600);

    let said = stdout(&o);
    assert!(
        said.starts_with(&format!(
            "benchmark lra-one  spec {}  results/benchmarks/lra-one\n",
            &key[..16]
        )),
        "{said}"
    );
    assert!(
        said.contains("tasks 100: 10 points x 5 reps; sim 50, score 50\n"),
        "{said}"
    );
    assert!(said.contains("done 0 of 100 (by key)\n"), "{said}");
    assert!(said.contains("\nbooks "), "{said}");
    assert!(said.contains(" GB of "), "{said}");
    assert!(said.contains(" GB, no GPUs\n"), "{said}");
    assert!(stderr(&o).is_empty(), "{}", stderr(&o));

    // Planned again unchanged: the same keys, nothing changed.
    let again = p.plan_spec(&spec, &[]);
    ok(&again);
    assert_eq!(keys(&p.plan("lra-one")), keys(&plan));
    assert!(
        !stdout(&again).contains("changed since the last plan"),
        "{}",
        stdout(&again)
    );

    // The run input's content enters its tasks' keys, and their
    // downstream's.
    std::fs::write(p.root().join("RUN1/manifest.json"), "{\"v\": 2}\n").unwrap();
    ok(&p.plan_spec(&spec, &[]));
    let edited = keys(&p.plan("lra-one"));
    for ((id, k), (id2, k2)) in keys(&plan).iter().zip(&edited) {
        assert_eq!(id, id2);
        assert_ne!(k, k2, "{id}");
    }
}

// same_bytes_same_key: two projects whose tools have the same bytes, at
// other paths, plan the same keys; a tool of other bytes does not.
#[test]
fn same_bytes_same_key() {
    let plan = |body: &str| {
        let p = Project::new();
        p.tool("TOOL1", body);
        p.tool("TOOL2", "true");
        ok(&p.plan_spec(&p.spec("S1.json", TWO), &[]));
        let plan = p.plan("N1");
        (keys(&plan), plan["spec_key"].clone())
    };
    let (a, ka) = plan("true");
    let (b, kb) = plan("true");
    assert_eq!(ka, kb);
    assert_eq!(a, b);
    let (c, _) = plan("echo other");
    assert_eq!(a.len(), c.len());
    for ((id, k), (id2, k2)) in a.iter().zip(&c) {
        assert_eq!(id, id2);
        assert_ne!(k, k2, "{id}: every task reads TOOL1's bytes");
    }
}

// A task is done when its folder's done.json has its key: one of a stale
// key, or unreadable, is not.
#[test]
fn done_counts_only_tasks_of_their_key() {
    let p = Project::with_tools();
    let spec = p.spec("S1.json", TWO);
    ok(&p.plan_spec(&spec, &[]));
    let plan = p.plan("N1");
    let tasks = plan["tasks"].as_array().unwrap();
    for (c, done) in tasks.iter().zip([
        format!("{{\"key\": {}}}", tasks[0]["key"]),
        "{\"key\": \"0\"}".to_string(),
        "{\"key\"".to_string(),
    ]) {
        let dir = Path::new(c["dir"].as_str().unwrap());
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("done.json"), done).unwrap();
    }
    let o = p.plan_spec(&spec, &[]);
    ok(&o);
    assert!(
        stdout(&o).contains("done 1 of 8 (by key)\n"),
        "{}",
        stdout(&o)
    );
}

// The spec's key is its canonical content: another key order, spacing
// and file name plan the same spec file and keys.
#[test]
fn a_spec_written_another_way_is_the_same_spec() {
    let p = Project::with_tools();
    ok(&p.plan_spec(&p.spec("S1.json", TWO), &[]));
    let first = p.plan("N1");
    let v: Value = serde_json::from_str(TWO).unwrap();
    let pretty = serde_json::to_string_pretty(&v)
        .unwrap()
        .replace("\"seeds\"", " \"seeds\"");
    ok(&p.plan_spec(&p.spec("S2.json", &pretty), &[]));
    assert_eq!(p.plan("N1")["spec_key"], first["spec_key"]);
    assert_eq!(keys(&p.plan("N1")), keys(&first));
    let specs = std::fs::read_dir(p.bench("N1").join("specs"))
        .unwrap()
        .count();
    assert_eq!(specs, 1);
    // The canonical copy plans as itself; edited in place, it is refused.
    let copy = p.bench("N1").join(format!(
        "specs/{}.json",
        first["spec_key"].as_str().unwrap()
    ));
    ok(&p.plan_spec(&copy, &[]));
    std::fs::write(&copy, TWO.replace("\"n\": 2", "\"n\": 3")).unwrap();
    let o = p.plan_spec(&copy, &[]);
    let e = refused(&o);
    assert!(e.contains("not its name; a spec under specs/ is not edited in place"));
}

// no_version_still_keys: a tool with no --version (it exits 2) has NA
// for its version, and its tasks are keyed by its bytes all the same.
#[test]
fn no_version_still_keys() {
    let p = Project::new();
    p.tool("TOOL1", "[ \"$1\" = --version ] && exit 2; true");
    p.tool("TOOL2", "echo 'TOOL2 1.0'");
    ok(&p.plan_spec(&p.spec("S1.json", TWO), &[]));
    let rows = p.versions("N1");
    let row = |t: &str| rows.iter().find(|r| r[0] == t).unwrap();
    assert_eq!(row("TOOL1")[4], "NA");
    assert_eq!(row("TOOL2")[4], "TOOL2 1.0");
    let k1 = &row("TOOL1")[2];
    assert_eq!(k1.len(), 32);
    let plan = p.plan("N1");
    let keys = keys(&plan);
    assert_eq!(keys.len(), 8);
    assert!(keys.iter().all(|(_, k)| k.len() == 32));
    assert_eq!(plan["tools"][0]["content_key"], k1.as_str());
    // plan.json lists each tool's word, source, key and what runs; its
    // version is versions.tsv's.
    let mut fields: Vec<&String> = plan["tools"][0].as_object().unwrap().keys().collect();
    fields.sort_unstable();
    assert_eq!(fields, ["content_key", "exec", "source", "tool"]);
    assert_eq!(plan["tools"][0]["tool"], "TOOL1");
}

// rebuild_mid_benchmark_keeps_snapshot: a tool rebuilt after the plan leaves
// its snapshot's bytes alone; planned again, the snapshot runs and the
// keys stay, and the change is said; --retool plans the rebuilt one,
// with new keys, and the old snapshot is kept.
#[test]
fn rebuild_mid_benchmark_keeps_snapshot() {
    let p = Project::new();
    p.tool("TOOL1", "echo v1");
    p.tool("TOOL2", "true");
    let spec = p.spec("S1.json", TWO);
    ok(&p.plan_spec(&spec, &[]));
    let first = p.plan("N1");
    let exec = first["tasks"][0]["argv"][0].as_str().unwrap().to_string();
    assert!(
        exec.starts_with(p.bench("N1").join("tools").to_str().unwrap()),
        "{exec}"
    );

    p.tool("TOOL1", "echo v2");
    let o = p.plan_spec(&spec, &[]);
    ok(&o);
    let kept = p.plan("N1");
    assert_eq!(keys(&kept), keys(&first));
    assert_eq!(kept["tasks"][0]["argv"][0], exec.as_str());
    let ran = Command::new(&exec).output().unwrap();
    assert_eq!(stdout(&ran), "v1\n");
    let said = stdout(&o);
    assert!(
        said.contains("changed since the last plan\n  TOOL1: changed since the last plan ("),
        "{said}"
    );
    assert!(
        said.contains("planned with its snapshot, --retool plans with the one installed\n"),
        "{said}"
    );

    ok(&p.plan_spec(&spec, &["--retool"]));
    let retooled = p.plan("N1");
    assert_ne!(retooled["tasks"][0]["argv"][0], exec.as_str());
    for ((id, k), (_, k2)) in keys(&first).iter().zip(&keys(&retooled)) {
        assert_ne!(k, k2, "{id}");
    }
    assert_eq!(stdout(&Command::new(&exec).output().unwrap()), "v1\n");
}

// mem_over_total_refused: memory above the machine's is refused, exit 3,
// with nothing written.
#[test]
fn mem_over_total_refused() {
    let p = Project::with_tools();
    let spec = two_with("\"mem_gb\": 1000000");
    let o = p.plan_spec(&p.spec("S1.json", &spec), &[]);
    let e = refused(&o);
    assert!(
        e.starts_with(
            "legume benchmark: mem_gb: 1000000 GB is more than this machine's MemTotal, "
        ),
        "{e}"
    );
    assert!(!p.root().join("results").exists());
}

// reference_atac_pair_truth_refused, at the command line: exit 3 before
// any tool is looked for.
#[test]
fn reference_atac_pair_truth_refused() {
    let p = Project::new();
    let spec = r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
      {"name": "sim", "run": ["data-beans-sim", "multiome", "--reference-atac", "R.zarr", "-o", "{out}/sim"],
       "makes": ["sim.ground_truth.tsv.gz"]},
      {"name": "score", "run": ["{legume}", "score", "--truth", "{sim}/sim.ground_truth.tsv.gz"],
       "makes": ["m.score.tsv"]}]}"#;
    let o = p.plan_spec(&p.spec("S1.json", spec), &[]);
    let e = refused(&o);
    assert!(
        e.contains("stages[1].run[3]: reads the pair truth of stage sim"),
        "{e}"
    );
    assert!(!p.root().join("results").exists());
}

// A grid that exclude empties is refused, exit 3, before any tool is
// snapshot or asked its version: nothing written.
#[test]
fn an_empty_grid_is_refused_with_nothing_written() {
    let p = Project::with_tools();
    let spec = two_with("\"exclude\": [{\"A\": \"1\"}, {\"A\": \"2\"}]");
    let o = p.plan_spec(&p.spec("S1.json", &spec), &[]);
    let e = refused(&o);
    assert_eq!(
        e,
        "legume benchmark: exclude: drops every point of the grid\n"
    );
    assert!(!p.root().join("results").exists());
}

// A file under specs/ with other bytes than the spec's canonical JSON is
// refused before any tool is snapshot.
#[test]
fn a_spec_file_of_other_bytes_is_refused_before_the_tools() {
    let p = Project::with_tools();
    let spec = p.spec("S1.json", TWO);
    ok(&p.plan_spec(&spec, &[]));
    let key = p.plan("N1")["spec_key"].as_str().unwrap().to_string();
    let bench = p.bench("N1");
    std::fs::write(bench.join(format!("specs/{key}.json")), "{}\n").unwrap();
    std::fs::remove_dir_all(bench.join("tools")).unwrap();
    let other = "is there with other bytes than this spec's canonical JSON; a spec under \
                 specs/ is not edited in place\n";
    let o = p.plan_spec(&spec, &[]);
    let e = refused(&o);
    assert!(e.ends_with(other), "{e}");
    assert!(!bench.join("tools").exists());

    // The same spec laid out another way in its place keeps its key, so
    // its name passes; its bytes are refused.
    let file = bench.join(format!("specs/{key}.json"));
    let same: Value = serde_json::from_str(TWO).unwrap();
    std::fs::write(&file, serde_json::to_string_pretty(&same).unwrap()).unwrap();
    let o = p.plan_spec(&file, &[]);
    let e = refused(&o);
    assert!(e.ends_with(other), "{e}");
    assert!(!bench.join("tools").exists());
}

// Every unresolved tool is named, exit 3, nothing written.
#[test]
fn unresolved_tools_are_listed() {
    let p = Project::new();
    let o = p.plan_spec(&p.spec("S1.json", TWO), &[]);
    let e = refused(&o);
    assert_eq!(
        e,
        "legume benchmark: not found on PATH: TOOL1 (stages[0]), TOOL2 (stages[1])\n"
    );
    assert!(!p.root().join("results").exists());
}

// The command line: usage errors exit 2 with the usage; an unreadable
// spec file exits 1; a declared input that is not there exits 3.
#[test]
fn the_command_line_exits_by_what_went_wrong() {
    let p = Project::with_tools();
    let usage = legume_mcp::benchmark::USAGE;
    for (args, said) in [
        (&["plan"][..], "plan needs SPEC, the spec's JSON file"),
        (&["plan", "S1.json", "--bogus"], "unknown argument --bogus"),
        (
            &["plan", "S1.json", "S2.json"],
            "one SPEC, not also S2.json",
        ),
        (
            &["run", "S1.json"],
            "unknown command run: benchmark has plan and task",
        ),
    ] {
        let o = p.legume(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert_eq!(
            stderr(&o),
            format!("legume benchmark: {said}\n\n{usage}\n"),
            "{args:?}"
        );
    }
    for help in [&[][..], &["--help"], &["plan", "--help"]] {
        let o = p.legume(help);
        ok(&o);
        assert_eq!(stdout(&o), format!("{usage}\n"));
    }
    let o = p.legume(&["plan", "missing.json"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).starts_with("legume benchmark: reading missing.json: "));

    let spec = two_with("\"inputs\": {\"RUN1\": {\"path\": \"RUN1\", \"dir\": true}}");
    let o = p.plan_spec(&p.spec("S1.json", &spec), &[]);
    let e = refused(&o);
    assert_eq!(
        e,
        format!(
            "legume benchmark: inputs.RUN1.path: no folder at {}\n",
            p.root().join("RUN1").display()
        )
    );
    // A file is no folder.
    std::fs::write(p.root().join("RUN1"), "").unwrap();
    let o = p.plan_spec(&p.root().join("S1.json"), &[]);
    let e = refused(&o);
    assert!(e.contains("inputs.RUN1.path: no folder at "), "{e}");
    let o = p.plan_spec(&p.spec("S2.json", "{\"name\": 1}"), &[]);
    let e = refused(&o);
    assert_eq!(e, "legume benchmark: name: is a number, not a string\n");
}

// A machine that changed since the first plan is listed; machine.json
// stays the first one's.
#[test]
fn a_changed_machine_is_said_and_the_first_kept() {
    let p = Project::with_tools();
    let spec = p.spec("S1.json", TWO);
    ok(&p.plan_spec(&spec, &[]));
    let file = p.bench("N1").join("machine.json");
    let mut m: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    m["legume"] = "0.0.1".into();
    let first = serde_json::to_string(&m).unwrap();
    std::fs::write(&file, &first).unwrap();
    let o = p.plan_spec(&spec, &[]);
    ok(&o);
    let line = format!("  legume: 0.0.1 -> {}\n", env!("CARGO_PKG_VERSION"));
    assert!(stdout(&o).contains(&line), "{}", stdout(&o));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
}

// A folder input that holds the benchmark's folder, or lies in it, would
// list the plan's own files and every task's output, and so rekey every
// task on every plan: refused, exit 3, nothing written. `..` is resolved
// first.
#[test]
fn a_folder_input_holding_the_benchmark_is_refused() {
    let p = Project::with_tools();
    std::fs::create_dir_all(p.bench("N1").join("tasks")).unwrap();
    for path in [".", "bin/..", "results/benchmarks/N1/tasks"] {
        let spec = two_with(&format!(
            "\"inputs\": {{\"R\": {{\"path\": \"{path}\", \"dir\": true}}}}"
        ));
        let o = p.plan_spec(&p.spec("S1.json", &spec), &[]);
        let e = refused(&o);
        assert!(
            e.starts_with("legume benchmark: inputs.R.path: "),
            "{path}: {e}"
        );
        assert!(
            e.ends_with(
                " overlaps the benchmark's folder, whose files change with every plan and \
                 run; name a folder apart from it\n"
            ),
            "{path}: {e}"
        );
        assert_eq!(
            std::fs::read_dir(p.bench("N1")).unwrap().count(),
            1,
            "{path}: nothing written"
        );
    }
}

// An input whose path has `..` or a link is the file it resolves to: an
// argv path to that file, however written, reads it, so its content
// enters the key, and no warning calls it undeclared.
#[test]
fn an_input_is_matched_once_resolved() {
    for (path, named) in [
        ("d/../d/r.tsv", "d/r.tsv"),
        ("link/r.tsv", "d/r.tsv"),
        ("d/r.tsv", "link/r.tsv"),
    ] {
        let p = Project::with_tools();
        let r = p.root().join("d/r.tsv");
        std::fs::create_dir_all(r.parent().unwrap()).unwrap();
        std::fs::write(&r, "GENE1\n").unwrap();
        std::os::unix::fs::symlink(p.root().join("d"), p.root().join("link")).unwrap();
        let spec = two_with(&format!("\"inputs\": {{\"R\": {{\"path\": \"{path}\"}}}}")).replace(
            "\"{A}\"",
            &format!("\"{{A}}\", \"{}\"", p.root().join(named).display()),
        );
        let spec = p.spec("S1.json", &spec);
        let o = p.plan_spec(&spec, &[]);
        ok(&o);
        assert!(
            !stderr(&o).contains("is a file outside the benchmark"),
            "{path}: {}",
            stderr(&o)
        );
        let before = keys(&p.plan("N1"));
        std::fs::write(&r, "GENE2\n").unwrap();
        ok(&p.plan_spec(&spec, &[]));
        assert_ne!(before[0], keys(&p.plan("N1"))[0], "{path}");
    }
}
