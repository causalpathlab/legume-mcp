//! The benchmark tools of the `legume` server, so an agent writes a spec
//! from the person's words, checks it and follows it without the command
//! line:
//!
//! - `benchmark_plan` takes the spec (as JSON, or the path of its file),
//!   keeps it in `specs-in/<name>.json`, and plans it as `legume benchmark
//!   plan` does; with `on`, it checks it only, as a run on that cluster
//!   plans it again there.
//! - `benchmark_start` starts `legume benchmark run SPEC --on REMOTE` as a
//!   background job of this server ([`crate::Call::spawn_job`]) and returns
//!   at once.
//! - `benchmark_status` reads where a benchmark stands from its folder:
//!   `events.tsv`, `plan.json` and `summary.tsv`.
//!
//! A refusal comes back as the tool's error, naming the spec's field. Each
//! reply stays under [`REPLY_MAX`] bytes.

use super::{bench_rel, key, plan, remote, spec, tools};
use crate::command::{refuse, Exit};
use crate::{Call, ToolSpec};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The most a reply says, in bytes.
pub(crate) const REPLY_MAX: usize = 4000;

/// Where a spec given as JSON is kept, in the project: the folder a run
/// on a cluster keeps it in there too.
const SPECS_IN: &str = "specs-in";

/// Failures listed in a status, the latest.
const FAILURES_SHOWN: usize = 5;

/// Lines of `summary.tsv` a status shows.
const SUMMARY_LINES: usize = 12;

/// The names of the benchmark tools.
pub(crate) const TOOLS: [&str; 3] = ["benchmark_plan", "benchmark_start", "benchmark_status"];

/// The benchmark tools, as `tools/list` shows them.
pub(crate) fn tools() -> Vec<ToolSpec> {
    let spec = json!({
        "type": ["object", "string"],
        "description": "The benchmark's spec as a JSON object (kept in specs-in/<name>.json), or the path of a spec file from the project. Guide section `benchmark` describes its fields",
    });
    let on = json!({
        "type": "string",
        "description": "A remote (a cluster) named in the person's remotes.toml, e.g. REMOTE1",
    });
    vec![
        ToolSpec::new(
            "benchmark_plan",
            "Check a benchmark spec and plan it: the tasks (grid points x replicates x stages and methods), what each task books (threads, memory, timeout), the tools, and what is done already. Without `on`, it plans here as `legume benchmark plan` does (tools found on PATH, inputs keyed, results/benchmarks/<name>/ written). With `on`, the spec is checked here and planned on that cluster when benchmark_start runs it, where its tools and inputs are. A refusal names the field to fix, such as stages[1].methods.M1.threads. Read guide section `benchmark` first; show the person the plan before starting it.",
            json!({
                "type": "object",
                "properties": { "spec": spec, "on": on },
                "required": ["spec"],
            }),
        )
        .writes()
        .idempotent(),
        ToolSpec::new(
            "benchmark_start",
            "Start a benchmark on a cluster: `legume benchmark run SPEC --on REMOTE` as a background job of this server, returning at once. It plans on a compute node, then runs each task as a Slurm array task; tasks done in an earlier run are reused, so starting again resumes. Follow it with benchmark_status. It runs while this server does. A remote not in remotes.toml is refused with how to add it: ask the person.",
            json!({
                "type": "object",
                "properties": { "spec": spec, "on": on },
                "required": ["spec", "on"],
            }),
        )
        .writes()
        .open_world(),
        ToolSpec::new(
            "benchmark_status",
            "Where a benchmark stands, from results/benchmarks/<name>/: its tasks by state (done, failed, skipped, running, pending), the latest failures, the run this server started, and the head of summary.tsv once gathered. Without `name`, the benchmarks of the project.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "The benchmark's name, the spec's `name`" } },
            }),
        )
        .read_only(),
    ]
}

/// The runs this server started, by benchmark name: their job ids.
#[derive(Default)]
pub(crate) struct Runs {
    jobs: HashMap<String, String>,
}

impl Runs {
    /// Benchmark tool `name` called with `args`: its reply.
    pub(crate) fn call(&mut self, name: &str, args: &Value, cx: &Call) -> Result<String> {
        let root = project_root(cx)?;
        let said = match name {
            "benchmark_plan" => plan_tool(&root, args, cx),
            "benchmark_start" => self.start(&root, args, cx),
            "benchmark_status" => match args["name"].as_str() {
                Some(n) => self.status(&root, n, cx),
                None => listed(&root),
            },
            _ => anyhow::bail!("no tool `{name}`"),
        }?;
        Ok(clip(&said, REPLY_MAX))
    }

    /// `benchmark_start`.
    fn start(&mut self, root: &Path, args: &Value, cx: &Call) -> Result<String> {
        let on = args["on"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("`on` is required: the remote to run on, as remotes.toml names it")?;
        let given = given_spec(root, &args["spec"], cx)?;
        plan::check(&given.spec).map_err(fix_hint)?;
        known_remote(on)?;
        let name = &given.spec.name;
        if let Some(id) = self.jobs.get(name) {
            if cx.jobs.status(Some(id))?["state"] == "running" {
                return Err(refuse(format!(
                    "benchmark {name} is running already, as {id}: follow it with \
                     benchmark_status; starting it again once it ends resumes it"
                )));
            }
        }
        let exe = crate::own_exe().context("finding this server's binary")?;
        let cmd = run_command(Command::new(exe), root, &given.rel, on);
        let summary_tsv = root.join(bench_rel(name)).join("summary.tsv");
        let id = cx.spawn_job(
            cmd,
            &[summary_tsv],
            &format!("benchmark {name} run on {on}"),
        )?;
        self.jobs.insert(name.clone(), id.clone());
        Ok(format!(
            "started benchmark {name} on {on}, as {id}: legume benchmark run {} --on {on}\n\
             It first plans on a compute node there (that waits in Slurm's queue), then runs \
             each task as a Slurm array task; tasks done before are reused. Follow it with \
             benchmark_status {{\"name\": \"{name}\"}}; its events go to {}/events.tsv as \
             tasks start and end, and summary.tsv comes once it ends. It runs while this \
             server does: when the server ends, the run is stopped (its Slurm tasks \
             cancelled, or left for the next start to adopt), and benchmark_start again \
             resumes it.\n",
            given.rel.display(),
            bench_rel(name).display(),
        ))
    }

    /// `benchmark_status` of benchmark `name`.
    fn status(&self, root: &Path, name: &str, cx: &Call) -> Result<String> {
        spec::benchmark_name(name)?;
        let rel = bench_rel(name);
        let dir = root.join(&rel);
        let job = match self.jobs.get(name) {
            Some(id) => Some((id.as_str(), cx.jobs.status(Some(id))?)),
            None => None,
        };
        if !dir.is_dir() && job.is_none() {
            return Err(refuse(format!(
                "no benchmark {name}: {} is not there; benchmark_status without `name` lists \
                 the benchmarks",
                rel.display()
            )));
        }
        Ok(status_text(&rel, &dir, job))
    }
}

/// The project's folder: the server's records', else the one `legume`
/// finds from here.
fn project_root(cx: &Call) -> Result<PathBuf> {
    match &cx.records {
        Some(p) => Ok(p.root().to_path_buf()),
        None => crate::project::root().context("finding the project's folder"),
    }
}

/// A spec a call gives: its file, from the project and absolute, the
/// spec read, and its key.
struct Given {
    rel: PathBuf,
    file: PathBuf,
    spec: spec::Spec,
    key: String,
}

/// The spec `v` a call gives, in the project at `root`: a JSON object,
/// kept in `specs-in/<name>.json` (and in the driver script), or the path
/// of a spec file, from the project.
fn given_spec(root: &Path, v: &Value, cx: &Call) -> Result<Given> {
    let read = |text: &str| spec::parse(text).map_err(fix_hint);
    let (rel, (spec, value)) = match v {
        Value::Object(_) => {
            let text = format!("{}\n", serde_json::to_string_pretty(v)?);
            let parsed = read(&text)?;
            let rel = Path::new(SPECS_IN).join(format!("{}.json", parsed.0.name));
            let folder = root.join(SPECS_IN);
            std::fs::create_dir_all(&folder)
                .with_context(|| format!("making {}", folder.display()))?;
            cx.write_file(&root.join(&rel), &text)?;
            (rel, parsed)
        }
        Value::String(p) if !p.is_empty() => {
            let file = root.join(p);
            let text =
                std::fs::read_to_string(&file).with_context(|| format!("reading the spec {p}"))?;
            let rel = file
                .strip_prefix(root)
                .map_or(file.clone(), Path::to_path_buf);
            (rel, read(&text)?)
        }
        _ => {
            return Err(refuse(
                "`spec` is required: the spec as a JSON object, or the path of its file",
            ))
        }
    };
    Ok(Given {
        file: root.join(&rel),
        rel,
        spec,
        key: key::key(key::spec_file(&value).as_bytes()),
    })
}

/// A refusal of the spec, with what to do: fix the field it names.
fn fix_hint(e: anyhow::Error) -> anyhow::Error {
    hinted(e, "Fix that field and call benchmark_plan again (guide section `benchmark` describes the fields).")
}

/// A refusal `e`, with `hint` after it; any other error as it is.
fn hinted(e: anyhow::Error, hint: &str) -> anyhow::Error {
    match e.downcast_ref::<Exit>() {
        Some(x) => refuse(format!("the spec is refused: {}\n{hint}", x.message)),
        None => e,
    }
}

/// `benchmark_plan`.
fn plan_tool(root: &Path, args: &Value, cx: &Call) -> Result<String> {
    let given = given_spec(root, &args["spec"], cx)?;
    match args["on"].as_str().filter(|s| !s.is_empty()) {
        Some(on) => checked(&given, on),
        None => planned_here(root, &given),
    }
}

/// The spec planned here, as `legume benchmark plan` plans it, with what
/// each of its methods books.
fn planned_here(root: &Path, given: &Given) -> Result<String> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    super::plan_spec(root, &given.file, false, &mut out, &mut err).map_err(|e| {
        let said = e.to_string();
        if said.starts_with("not found on PATH") || said.starts_with("inputs.") {
            hinted(
                e,
                "A spec for a cluster names its tools and inputs there: call benchmark_plan \
                 with `on`, the remote, to check it here; it is planned there when it starts.",
            )
        } else {
            fix_hint(e)
        }
    })?;
    let mut said = String::from_utf8_lossy(&out).into_owned();
    said.push_str(&needs(&given.spec));
    said.push_str(&String::from_utf8_lossy(&err));
    let _ = writeln!(
        said,
        "spec: {}; benchmark_start {{\"spec\": \"{}\", \"on\": REMOTE}} runs it on a cluster",
        given.rel.display(),
        given.rel.display()
    );
    Ok(said)
}

/// The spec checked here for a run on remote `on`, which plans it there:
/// its tasks, what each method books, and the tools and inputs it names.
fn checked(given: &Given, on: &str) -> Result<String> {
    let s = &given.spec;
    plan::check(s).map_err(fix_hint)?;
    let words = plan::tool_words(s).map_err(fix_hint)?;
    known_remote(on)?;
    let mut said = format!(
        "benchmark {}  spec {}  checked here; planned on a compute node of {on} when it starts\n",
        s.name,
        tools::short(&given.key)
    );
    said.push_str(&counts(s));
    said.push_str(&needs(s));
    let names: Vec<&str> = words
        .iter()
        .map(|(w, _, _)| if w == plan::LEGUME { "legume" } else { w })
        .collect();
    let _ = writeln!(
        said,
        "tools, found on {on} (its bin, then PATH) when it plans: {}",
        names.join(", ")
    );
    if !s.inputs.is_empty() {
        let inputs: Vec<String> = s
            .inputs
            .iter()
            .map(|i| format!("{} = {}", i.name, i.path))
            .collect();
        let _ = writeln!(
            said,
            "inputs, from the project's folder on {on}: {}",
            inputs.join(", ")
        );
    }
    let _ = writeln!(
        said,
        "spec: {}; benchmark_start {{\"spec\": \"{}\", \"on\": \"{on}\"}} runs it",
        given.rel.display(),
        given.rel.display()
    );
    Ok(said)
}

/// The spec's task count, as points x replicates x methods.
fn counts(s: &spec::Spec) -> String {
    let points = plan::points(s).len();
    let reps = plan::reps(&s.seeds).len();
    let methods: usize = s.stages.iter().map(|st| st.methods.len()).sum();
    format!(
        "tasks {}: {points} points x {reps} reps x {methods} stage methods\n",
        points * reps * methods
    )
}

/// What each stage's methods book, a line each.
fn needs(s: &spec::Spec) -> String {
    let per = plan::points(s).len() * plan::reps(&s.seeds).len();
    let mut out = String::from("each task books\n");
    for st in &s.stages {
        for m in &st.methods {
            let name = match &m.name {
                Some(n) => format!("{}-{n}", st.name),
                None => st.name.clone(),
            };
            let _ = write!(
                out,
                "  {name}: {per} tasks, {} thread{}, {} GB, timeout {}",
                m.threads,
                if m.threads == 1 { "" } else { "s" },
                m.mem_gb,
                duration(m.timeout_secs)
            );
            if m.gpus > 0 {
                let _ = write!(out, ", {} GPUs", m.gpus);
            }
            if let Some(n) = m.max_running {
                let _ = write!(out, ", at most {n} at once");
            }
            let _ = writeln!(out, ", runs {}", m.tool_word());
        }
    }
    out
}

/// `secs` as the spec writes a duration: `4h`, `1h30m`, `45s`.
fn duration(secs: u64) -> String {
    let mut out = String::new();
    let mut left = secs;
    for (unit, n) in [("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)] {
        if left >= n {
            let _ = write!(out, "{}{unit}", left / n);
            left %= n;
        }
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

/// Remote `on`, as remotes.toml has it; else the refusal that says how a
/// person adds it, which a call over MCP cannot ask them for.
fn known_remote(on: &str) -> Result<remote::Remote> {
    let file = remote::config_file()
        .ok_or_else(|| refuse("no config folder for remotes.toml: set HOME, or LEGUME_REMOTES"))?;
    let all = remote::read(&file)?;
    if let Some(r) = all.iter().find(|r| r.name == on) {
        return Ok(r.clone());
    }
    let names: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
    let known = if names.is_empty() {
        "it names none".to_string()
    } else {
        format!("it names {}", names.join(", "))
    };
    Err(refuse(format!(
        "no remote {on} in {} ({known}). A remote is the person's own, and is not asked \
         for over MCP. Ask them to run `legume benchmark run SPEC --on {on}` once in a \
         terminal, which asks for it and keeps it, or to add it to that file:\n\n\
         [{on}]\n\
         ssh = \"HOST1\"           # a host or an alias from ~/.ssh/config\n\
         folder = \"/abs/folder\"  # on the cluster, for benchmarks\n\
         bin = \"/abs/prefix/bin\" # legume's tools there; leave out when on PATH\n\
         partition = \"PART1\"     # optional\n\
         account = \"ACCT1\"       # optional\n\n\
         then call again.",
        file.display()
    )))
}

/// The command `benchmark_start` runs: `legume`, as `exe` runs it, with
/// `benchmark run SPEC --on ON`, from the project at `root`, the spec
/// `spec` from there.
fn run_command(mut exe: Command, root: &Path, spec: &Path, on: &str) -> Command {
    exe.arg("benchmark")
        .arg("run")
        .arg(spec)
        .arg("--on")
        .arg(on);
    // From the project, which a server in it already is.
    let here = std::env::current_dir().ok();
    if here.as_deref() != Some(root) {
        exe.current_dir(root);
    }
    exe
}

/// What one task's latest event says it is.
fn state_of(event: &str) -> Option<&'static str> {
    Some(match event {
        "start" | "adopted" => "running",
        "done" | "reused" => "done",
        "failed" | "timeout" | "oom" | "lost" => "failed",
        "stopped" => "stopped",
        "skipped" => "skipped",
        _ => return None,
    })
}

/// A benchmark's status, from its folder `dir` (`rel` from the project),
/// with the run this server started for it, when it did: its id and
/// status.
fn status_text(rel: &Path, dir: &Path, job: Option<(&str, Value)>) -> String {
    let mut out = format!("benchmark at {}\n", rel.display());
    match &job {
        Some((id, v)) => {
            let state = v["state"].as_str().unwrap_or("unknown");
            let _ = writeln!(
                out,
                "run: {id}, {state}, {} s",
                v["seconds"].as_u64().unwrap_or(0)
            );
            let said = match state {
                "running" => lines_of(&v["last_lines"]),
                "done" => tail(v["output"].as_str().unwrap_or(""), 4),
                _ => tail(v["error"].as_str().unwrap_or(""), 8),
            };
            for l in said {
                let _ = writeln!(out, "  {}", clip(l, 240));
            }
        }
        None => out.push_str("run: none started by this server\n"),
    }
    let planned: Option<Vec<String>> = std::fs::read(dir.join("plan.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<plan::PlanDoc>(&b).ok())
        .map(|p| p.tasks.into_iter().map(|t| t.id).collect());
    let events = std::fs::read_to_string(dir.join("events.tsv")).unwrap_or_default();
    let mut latest: BTreeMap<String, &'static str> = BTreeMap::new();
    // Each failure: its task and what to say of it.
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut lines = events.lines();
    let head: Vec<&str> = lines.next().unwrap_or_default().split('\t').collect();
    let col = |name: &str| head.iter().position(|h| *h == name);
    let (time, task, attempt, event, node) = (
        col("time"),
        col("task"),
        col("attempt"),
        col("event"),
        col("node"),
    );
    for line in lines {
        let row: Vec<&str> = line.split('\t').collect();
        let get = |i: Option<usize>| i.and_then(|i| row.get(i)).copied().unwrap_or("");
        let (id, ev) = (get(task), get(event));
        let Some(state) = state_of(ev) else { continue };
        if id.is_empty() {
            continue;
        }
        latest.insert(id.to_string(), state);
        if state == "failed" {
            let mut f = format!("{} {id} {ev}", get(time));
            if !get(attempt).is_empty() {
                let _ = write!(f, ", attempt {}", get(attempt));
            }
            if !get(node).is_empty() {
                let _ = write!(f, ", on {}", get(node));
            }
            failures.push((id.to_string(), f));
        }
    }
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for s in latest.values() {
        *by.entry(s).or_default() += 1;
    }
    let total = match &planned {
        Some(ids) => {
            let pending = ids.iter().filter(|id| !latest.contains_key(*id)).count();
            by.insert("pending", pending);
            ids.len()
        }
        None => latest.len(),
    };
    let mut said: Vec<String> = ["done", "failed", "skipped", "running", "pending"]
        .iter()
        .map(|s| format!("{} {s}", by.get(s).copied().unwrap_or(0)))
        .collect();
    if let Some(n) = by.get("stopped") {
        said.push(format!("{n} stopped"));
    }
    if planned.is_none() {
        if events.is_empty() {
            out.push_str("tasks: no plan.json or events.tsv yet (planning, or not started)\n");
        } else {
            let _ = writeln!(
                out,
                "tasks seen in events.tsv {total}: {} (no plan.json yet, so none pending)",
                said.join(", ")
            );
        }
    } else {
        let _ = writeln!(out, "tasks {total}: {}", said.join(", "));
    }
    // A failure is listed while its task's latest event is a failure.
    let still: Vec<&String> = failures
        .iter()
        .filter(|(id, _)| latest.get(id) == Some(&"failed"))
        .map(|(_, f)| f)
        .collect();
    if !still.is_empty() {
        let _ = writeln!(
            out,
            "failed, latest {} of {}:",
            still.len().min(FAILURES_SHOWN),
            still.len()
        );
        for f in &still[still.len().saturating_sub(FAILURES_SHOWN)..] {
            let _ = writeln!(out, "  {}", clip(f, 240));
        }
    }
    match std::fs::read_to_string(dir.join("summary.tsv")) {
        Ok(text) => {
            let n = text.lines().count();
            let _ = writeln!(
                out,
                "summary.tsv ({n} lines{}):",
                if n > SUMMARY_LINES {
                    format!(", the first {SUMMARY_LINES}")
                } else {
                    String::new()
                }
            );
            for l in text.lines().take(SUMMARY_LINES) {
                let _ = writeln!(out, "  {}", clip(l, 240));
            }
        }
        Err(_) => out.push_str("summary.tsv: not yet (a run gathers it at its end)\n"),
    }
    out
}

/// The strings of the JSON list `v`.
fn lines_of(v: &Value) -> Vec<&str> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The last `n` lines of `text`.
fn tail(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// The benchmarks of the project at `root`: the folders under
/// `results/benchmarks`.
fn listed(root: &Path) -> Result<String> {
    let base = root.join("results").join("benchmarks");
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        return Ok("no benchmarks yet in results/benchmarks/\n".into());
    }
    Ok(format!(
        "benchmarks in results/benchmarks/: {}\nbenchmark_status with `name` says where one stands\n",
        names.join(", ")
    ))
}

/// `text` cut to at most `max` bytes, at a character, saying so.
fn clip(text: &str, max: usize) -> String {
    const CUT: &str = " ... (cut)\n";
    if text.len() <= max {
        return text.to_string();
    }
    let mut at = max.saturating_sub(CUT.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    format!("{}{CUT}", &text[..at])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_legume_benchmark_run_from_the_project() {
        let root = Path::new("/P1");
        let cmd = run_command(
            Command::new("legume"),
            root,
            Path::new("specs-in/N1.json"),
            "REMOTE1",
        );
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(
            args,
            ["benchmark", "run", "specs-in/N1.json", "--on", "REMOTE1"]
        );
        assert_eq!(cmd.get_current_dir(), Some(root));
    }

    #[test]
    fn durations_read_as_the_spec_writes_them() {
        assert_eq!(duration(4 * 3600), "4h");
        assert_eq!(duration(5400), "1h30m");
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(0), "0s");
    }

    #[test]
    fn a_long_reply_is_cut_under_the_limit() {
        let long = "é".repeat(3000);
        let cut = clip(&long, REPLY_MAX);
        assert!(cut.len() <= REPLY_MAX, "{}", cut.len());
        assert!(cut.ends_with("(cut)\n"));
        assert_eq!(clip("short", REPLY_MAX), "short");
    }

    #[test]
    fn a_status_counts_each_task_by_its_latest_event() {
        let d = tempfile::tempdir().unwrap();
        let head = "time\ttask\tattempt\tevent\twall_secs\tcpu_secs\tqueue_secs\tnode";
        let rows = [
            "T1\tA.r1.sim\t\tstart\t\t\t\t",
            "T2\tA.r1.sim\t1\tdone\t1.0\t0.5\t0.1\tNODE1",
            "T3\tA.r1.score\t\tstart\t\t\t\t",
            "T4\tA.r1.score\t1\tfailed\t1.0\t0.5\t0.1\tNODE1",
            "T5\tB.r1.sim\t\tstart\t\t\t\t",
            "T6\tB.r1.sim\t1\toom\t1.0\t0.5\t0.1\tNODE1",
            "T7\tB.r1.sim\t\tstart\t\t\t\t",
            "T8\tB.r1.score\t\tskipped\t\t\t\t",
            "T9\tC.r1.sim\t\treused\t\t\t\t",
            "T10\tC.r1.sim\t\tstop-requested\t\t\t\t",
        ];
        std::fs::write(
            d.path().join("events.tsv"),
            format!("{head}\n{}\n", rows.join("\n")),
        )
        .unwrap();
        let said = status_text(Path::new("results/benchmarks/N1"), d.path(), None);
        assert!(
            said.contains(
                "tasks seen in events.tsv 5: 2 done, 1 failed, 1 skipped, 1 running, 0 pending"
            ),
            "{said}"
        );
        // B's out-of-memory attempt was retried, so only A's score failed.
        assert!(
            said.contains("T4 A.r1.score failed, attempt 1, on NODE1"),
            "{said}"
        );
        assert!(!said.contains("B.r1.sim oom"), "{said}");
        assert!(said.contains("run: none started by this server"), "{said}");
        assert!(said.contains("summary.tsv: not yet"), "{said}");
    }
}
