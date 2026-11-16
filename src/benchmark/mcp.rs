//! The benchmark tools of the `legume` server, so an agent writes a spec
//! from the person's words, checks it and follows it without the command
//! line:
//!
//! - `benchmark_plan` takes the spec (as JSON, or the path of its file),
//!   keeps it in `specs-in/<name>.json`, and plans it as `legume benchmark
//!   plan` does; with `on`, it checks it only, as a run on that cluster
//!   plans it again there.
//! - `benchmark_start` starts `legume benchmark run SPEC --on REMOTE
//!   --detach` ([`super::runner`]): the run goes on in the background, past
//!   this server's end, and the tool returns at once.
//! - `benchmark_status` reads where a benchmark stands from its folder:
//!   `runner.json` and `run.log`, `events.tsv`, `plan.json` and
//!   `summary.tsv`.
//! - `benchmark_stop` sends its runner TERM, as `legume benchmark stop`.
//!
//! A refusal comes back as the tool's error, naming the spec's field. Each
//! reply stays under [`REPLY_MAX`] bytes.

use super::{bench_rel, edit, key, plan, remote, runner, spec, tools};
use crate::command::{refuse, Exit};
use crate::{Call, ToolSpec};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The most a reply says, in bytes.
pub(crate) const REPLY_MAX: usize = 4000;

/// Where a spec given as JSON is kept, in the project: the folder a run
/// on a cluster keeps it in there too.
const SPECS_IN: &str = "specs-in";

/// Failures listed in a status, the latest.
const FAILURES_SHOWN: usize = 5;

/// Lines of `summary.tsv` a status shows.
const SUMMARY_LINES: usize = 10;

/// Lines of `run.log` a status shows, the last.
const LOG_LINES: usize = 8;

/// The most of `run.log` a status reads, from its end, in bytes.
const LOG_READ: u64 = 64 * 1024;

/// The most a status shows of one line, in bytes.
const LINE_MAX: usize = 160;

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
    let name =
        json!({ "type": "string", "description": "The benchmark's name, the spec's `name`" });
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
            "Start a benchmark, in the background, returning at once: with `on`, on that cluster (`legume benchmark run SPEC --on REMOTE --detach`: it plans on a compute node, then runs each task as a Slurm array task); without `on`, on this machine (planned here, its tasks within the cores and memory the spec books; Linux only). The run goes on after this server ends. Tasks done in an earlier run are reused, so starting again resumes. Follow it with benchmark_status, stop it with benchmark_stop. A start while its runner is alive is refused. A remote not in remotes.toml is refused with how to add it: ask the person.",
            json!({
                "type": "object",
                "properties": { "spec": spec, "on": on },
                "required": ["spec"],
            }),
        )
        .writes()
        .open_world(),
        ToolSpec::new(
            "benchmark_status",
            "Where a benchmark stands, from results/benchmarks/<name>/: whether its runner is alive (or how it ended), its tasks by state (done, failed, skipped, running, pending), the latest failures, the head of summary.tsv once gathered, and the end of run.log. Without `name`, the benchmarks of the project.",
            json!({
                "type": "object",
                "properties": { "name": name.clone() },
            }),
        )
        .read_only(),
        ToolSpec::new(
            "benchmark_stop",
            "Stop a benchmark's runner: it is sent TERM, so it stops its tasks (its Slurm array tasks, on a cluster), waits for them to stop and ends. Refused when no runner is alive. Ask the person first; benchmark_start again resumes it.",
            json!({
                "type": "object",
                "properties": { "name": name },
                "required": ["name"],
            }),
        )
        .destructive(),
    ]
}

/// Benchmark tool `name` called with `args`: its reply.
pub(crate) fn call(name: &str, args: &Value, cx: &Call) -> Result<String> {
    let root = || project_root(cx);
    let said = match name {
        "benchmark_plan" => plan_tool(&root()?, args, cx),
        "benchmark_start" => start(&root()?, args, cx),
        "benchmark_status" => match args["name"].as_str() {
            Some(n) => named(&root()?, n).map(|(rel, dir)| status_text(&rel, &dir)),
            None => listed(&root()?),
        },
        "benchmark_stop" => {
            let n = args["name"]
                .as_str()
                .context("`name` is required: the benchmark's name")?;
            named(&root()?, n).and_then(|(_, dir)| runner::stop_runner(&dir, n))
        }
        _ => anyhow::bail!("no tool `{name}`"),
    }?;
    Ok(clip(&said, REPLY_MAX))
}

/// `benchmark_start`: the run detached ([`runner::detach`], which checks
/// for a live runner and the remote), the spec kept only once it goes
/// ahead, and the run recorded.
fn start(root: &Path, args: &Value, cx: &Call) -> Result<String> {
    let on = args["on"].as_str().filter(|s| !s.is_empty());
    let given = given_spec(root, &args["spec"])?;
    reported(&given)?;
    let d = runner::Detach {
        root,
        spec_file: &given.file,
        spec: &given.spec,
        spec_key: &given.key,
        on,
    };
    let started = runner::detach(&d, None, &mut || keep(&given, cx)).map_err(reworded)?;
    // The driver script repeats the run in the foreground, from the
    // project, skipped once it has gathered.
    if let Some(p) = &cx.records {
        let mut cmd = runner::run_command(&given.rel, on)?;
        if std::env::current_dir().ok().as_deref() != Some(root) {
            cmd.current_dir(root);
        }
        let bare = p.command(&cmd);
        let summary = root.join(bench_rel(&started.name)).join("summary.tsv");
        let line = p.guarded(&bare, &[summary]);
        p.script_step(&format!("{line}\n"), Some(&bare));
    }
    let how = match on {
        Some(_) => {
            "It first plans on a compute node there (that waits in Slurm's queue), then runs \
             each task as a Slurm array task"
        }
        None => "It plans here, then runs each task on this machine within what the spec books",
    };
    Ok(format!(
        "{}{how}; tasks done before are reused. It goes on after this server ends. Follow it \
         with benchmark_status {{\"name\": \"{}\"}}: events.tsv gets a row as each task starts \
         and ends, and summary.tsv comes once it ends. benchmark_stop stops it; \
         benchmark_start again resumes it.\n",
        started.said(),
        started.name
    ))
}

/// A refusal of [`runner::detach`], in an agent's words; any other error
/// as it is.
fn reworded(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<runner::Refused>() {
        Some(runner::Refused::Running { name, runner: r }) => refuse(format!(
            "benchmark {name} is running already, as pid {} (since {}, on {}): follow it \
             with benchmark_status, or stop it with benchmark_stop",
            r.pid,
            r.started,
            runner::place(r.remote.as_deref())
        )),
        Some(runner::Refused::UnknownRemote { on, file, known }) => {
            let known = if known.is_empty() {
                "it names none".to_string()
            } else {
                format!("it names {}", known.join(", "))
            };
            refuse(format!(
                "no remote {on} in {} ({known}). A remote is the person's own, and is not \
                 asked for over MCP. Ask them to run `legume benchmark run SPEC --on {on}` \
                 once in a terminal, which asks for it and keeps it, or to add it to that \
                 file:\n\n\
                 [{on}]\n\
                 ssh = \"HOST1\"           # a host or an alias from ~/.ssh/config\n\
                 folder = \"/abs/folder\"  # on the cluster, for benchmarks\n\
                 bin = \"/abs/prefix/bin\" # legume's tools there; leave out when on PATH\n\
                 partition = \"PART1\"     # optional\n\
                 account = \"ACCT1\"       # optional\n\n\
                 then call again.",
                file.display()
            ))
        }
        None => e,
    }
}

/// Benchmark `name`'s folder, from the project and under `root`; refused
/// when the name is none or the folder is not there.
fn named(root: &Path, name: &str) -> Result<(PathBuf, PathBuf)> {
    spec::benchmark_name(name)?;
    let rel = bench_rel(name);
    let dir = root.join(&rel);
    if !dir.is_dir() {
        return Err(refuse(format!(
            "no benchmark {name}: {} is not there; benchmark_status without `name` lists \
             the benchmarks",
            rel.display()
        )));
    }
    Ok((rel, dir))
}

/// The project's folder: the server's records', else the one `legume`
/// finds from here.
fn project_root(cx: &Call) -> Result<PathBuf> {
    match &cx.records {
        Some(p) => Ok(p.root().to_path_buf()),
        None => crate::project::root().context("finding the project's folder"),
    }
}

/// A spec a call gives: its file, from the project and absolute, its
/// text, the spec read and as JSON ([`spec::parse`]), its key, and whether
/// it was given as JSON (and is to be kept in its file, [`keep`]).
struct Given {
    rel: PathBuf,
    file: PathBuf,
    text: String,
    spec: spec::Spec,
    value: Value,
    key: String,
    given_as_json: bool,
}

/// The spec `v` a call gives, in the project at `root`: a JSON object, for
/// `specs-in/<name>.json`, or the path of a spec file inside the project.
fn given_spec(root: &Path, v: &Value) -> Result<Given> {
    let (rel, text) = match v {
        Value::Object(_) => (None, format!("{}\n", serde_json::to_string_pretty(v)?)),
        Value::String(p) if !p.is_empty() => {
            let rel = inside(root, p)?;
            let text = std::fs::read_to_string(root.join(&rel))
                .with_context(|| format!("reading the spec {p}"))?;
            (Some(rel), text)
        }
        _ => {
            return Err(refuse(
                "`spec` is required: the spec as a JSON object, or the path of its file",
            ))
        }
    };
    let (spec, value) = spec::parse(&text).map_err(fix_hint)?;
    let given_as_json = rel.is_none();
    let rel = rel.unwrap_or_else(|| Path::new(SPECS_IN).join(format!("{}.json", spec.name)));
    Ok(Given {
        file: root.join(&rel),
        rel,
        text,
        key: key::spec_key(&value),
        spec,
        value,
        given_as_json,
    })
}

/// The spec file `p` from the project at `root`, resolved: refused when it
/// is not there or not inside the project.
fn inside(root: &Path, p: &str) -> Result<PathBuf> {
    let real_root = root
        .canonicalize()
        .with_context(|| format!("resolving {}", root.display()))?;
    let real = root
        .join(p)
        .canonicalize()
        .map_err(|e| refuse(format!("spec: {p}: {e}")))?;
    match real.strip_prefix(&real_root) {
        Ok(rel) => Ok(rel.to_path_buf()),
        Err(_) => Err(refuse(format!(
            "spec: {p} is not inside the project; give a spec file in it, or the spec as JSON"
        ))),
    }
}

/// A spec given as JSON, kept in its file (and in the driver script).
fn keep(given: &Given, cx: &Call) -> Result<()> {
    if !given.given_as_json {
        return Ok(());
    }
    let folder = given.file.parent().context("a spec file's folder")?;
    std::fs::create_dir_all(folder).with_context(|| format!("making {}", folder.display()))?;
    cx.write_file(&given.file, &given.text)
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
    let given = given_spec(root, &args["spec"])?;
    keep(&given, cx)?;
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
                 with `on`, the remote, to check it here; it is planned there when it starts. \
                 To run it on this machine instead, install its tools here first.",
            )
        } else {
            fix_hint(e)
        }
    })?;
    let mut said = String::from_utf8_lossy(&out).into_owned();
    said.push_str(&needs(&edit::counts(&given.spec, &given.value)));
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
    let c = reported(given)?;
    let words = plan::tool_words(s).map_err(fix_hint)?;
    known_remote(on)?;
    let mut said = format!(
        "benchmark {}  spec {}  checked here; planned on a compute node of {on} when it starts\n",
        s.name,
        tools::short(&given.key)
    );
    let _ = writeln!(
        said,
        "tasks {}: {} points x {} reps x {} stage methods",
        c.tasks,
        c.points,
        c.reps,
        c.methods.len()
    );
    said.push_str(&needs(&c));
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

/// What the spec plans, counted, as the editor checks it
/// ([`edit::report`]): refused as it refuses, a method's threads past
/// the cores the spec books among it.
fn reported(given: &Given) -> Result<edit::Counts> {
    let r = edit::report(&given.value);
    if let Some(why) = r.refusal {
        return Err(fix_hint(refuse(why)));
    }
    r.counts.context("a spec read and not counted")
}

/// What each stage's methods book, a line each.
fn needs(c: &edit::Counts) -> String {
    let mut out = String::from("each task books\n");
    for m in &c.methods {
        let _ = write!(
            out,
            "  {}: {} tasks, {} thread{}, {} GB, timeout {}",
            m.name,
            m.tasks,
            m.threads,
            edit::plural(m.threads as usize),
            m.mem_gb,
            edit::duration_text(m.timeout_secs)
        );
        if m.gpus > 0 {
            let _ = write!(out, ", {} GPUs", m.gpus);
        }
        if let Some(n) = m.max_running {
            let _ = write!(out, ", at most {n} at once");
        }
        let _ = writeln!(out, ", runs {}", m.tool);
    }
    out
}

/// Remote `on`, as remotes.toml has it; else the refusal that says how a
/// person adds it, which a call over MCP cannot ask them for.
fn known_remote(on: &str) -> Result<remote::Remote> {
    runner::kept_remote(on, None).map_err(reworded)
}

/// What a status reads of `plan.json`: its tasks' ids.
#[derive(Deserialize)]
struct PlannedIds {
    tasks: Vec<IdOnly>,
}

/// A task of `plan.json`, by its id alone.
#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

/// A task as `events.tsv` has it: its latest state, and its latest
/// failure, if any.
struct Seen<'a> {
    state: &'static str,
    failed: Option<Failure<'a>>,
}

/// A failure's row of `events.tsv`: where it is among the rows, and its
/// time, event, attempt and node.
struct Failure<'a> {
    row: usize,
    time: &'a str,
    event: &'a str,
    attempt: &'a str,
    node: &'a str,
}

/// The tasks of `events` (the text of `events.tsv`), by id: each one's
/// latest state ([`super::run::event_state`]) and latest failure. A row
/// whose event says no state, or that names no task, is passed over.
fn tasks_seen(events: &str) -> HashMap<&str, Seen<'_>> {
    let mut lines = events.lines();
    let head = lines.next().unwrap_or_default();
    let col = |name: &str| head.split('\t').position(|h| h == name);
    let cols = [
        col("time"),
        col("task"),
        col("attempt"),
        col("event"),
        col("node"),
    ];
    let mut seen: HashMap<&str, Seen> = HashMap::new();
    for (row, line) in lines.enumerate() {
        let mut f = [""; 5];
        for (i, field) in line.split('\t').enumerate() {
            if let Some(k) = cols.iter().position(|c| *c == Some(i)) {
                f[k] = field;
            }
        }
        let [time, id, attempt, event, node] = f;
        let Some(state) = super::run::event_state(event) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let task = seen.entry(id).or_insert(Seen {
            state,
            failed: None,
        });
        task.state = state;
        if state == "failed" {
            task.failed = Some(Failure {
                row,
                time,
                event,
                attempt,
                node,
            });
        }
    }
    seen
}

/// The first [`SUMMARY_LINES`] lines of the file at `path`, and how many
/// it has, the rest counted and not kept.
fn head_lines(path: &Path) -> std::io::Result<(Vec<String>, usize)> {
    let mut r = BufReader::new(File::open(path)?);
    let mut head = Vec::new();
    let mut line = String::new();
    while head.len() < SUMMARY_LINES && r.read_line(&mut line)? > 0 {
        head.push(line.trim_end_matches(['\n', '\r']).to_string());
        line.clear();
    }
    let (mut n, mut last) = (head.len(), b'\n');
    loop {
        let buf = r.fill_buf()?;
        let Some(&end) = buf.last() else { break };
        n += buf.iter().filter(|&&b| b == b'\n').count();
        last = end;
        let len = buf.len();
        r.consume(len);
    }
    // A last line without its newline.
    if last != b'\n' {
        n += 1;
    }
    Ok((head, n))
}

/// A benchmark's status, from its folder `dir` (`rel` from the project):
/// its runner, its tasks, its summary and the end of its log.
fn status_text(rel: &Path, dir: &Path) -> String {
    let mut out = format!("benchmark at {}\n", rel.display());
    match runner::read(dir) {
        Some(r) => {
            let state = match r.exit {
                Some(code) => format!(
                    "ended {}, exit {code}",
                    r.ended.as_deref().unwrap_or("at a time not noted")
                ),
                None if runner::alive(dir) => "alive".to_string(),
                None => "gone, without noting how it ended (see run.log)".to_string(),
            };
            let _ = writeln!(
                out,
                "runner: pid {}, on {}, started {}: {state}",
                r.pid,
                runner::place(r.remote.as_deref()),
                r.started
            );
        }
        None => out.push_str("runner: none started with --detach (no runner.json)\n"),
    }
    let planned: Option<PlannedIds> = std::fs::read(dir.join("plan.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let events = std::fs::read_to_string(dir.join("events.tsv")).unwrap_or_default();
    let seen = tasks_seen(&events);
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for t in seen.values() {
        *by.entry(t.state).or_default() += 1;
    }
    let total = match &planned {
        Some(p) => {
            let pending = p
                .tasks
                .iter()
                .filter(|t| !seen.contains_key(t.id.as_str()))
                .count();
            by.insert("pending", pending);
            p.tasks.len()
        }
        None => seen.len(),
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
    // A task's failure is listed while its latest event is a failure, in
    // the order they failed.
    let mut still: Vec<(&str, &Failure)> = seen
        .iter()
        .filter(|(_, t)| t.state == "failed")
        .filter_map(|(id, t)| Some((*id, t.failed.as_ref()?)))
        .collect();
    if !still.is_empty() {
        still.sort_by_key(|(_, f)| f.row);
        let _ = writeln!(
            out,
            "failed, latest {} of {}:",
            still.len().min(FAILURES_SHOWN),
            still.len()
        );
        for (id, f) in &still[still.len().saturating_sub(FAILURES_SHOWN)..] {
            let mut line = format!("{} {id} {}", f.time, f.event);
            if !f.attempt.is_empty() {
                let _ = write!(line, ", attempt {}", f.attempt);
            }
            if !f.node.is_empty() {
                let _ = write!(line, ", on {}", f.node);
            }
            let _ = writeln!(out, "  {}", clip(&line, LINE_MAX));
        }
    }
    match head_lines(&dir.join("summary.tsv")) {
        Ok((head, n)) => {
            let _ = writeln!(
                out,
                "summary.tsv ({n} lines{}):",
                if n > SUMMARY_LINES {
                    format!(", the first {SUMMARY_LINES}")
                } else {
                    String::new()
                }
            );
            for l in &head {
                let _ = writeln!(out, "  {}", clip(l, LINE_MAX));
            }
        }
        Err(_) => out.push_str("summary.tsv: not yet (a run gathers it at its end)\n"),
    }
    let log = super::tail::tail(&dir.join(runner::LOG_FILE), LOG_READ);
    if !log.is_empty() {
        let _ = writeln!(out, "{}, the last lines:", runner::LOG_FILE);
        for l in last_lines(&log, LOG_LINES) {
            let _ = writeln!(out, "  {}", clip(l, LINE_MAX));
        }
    }
    out
}

/// The last `n` lines of `text`.
fn last_lines(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// The benchmarks of the project at `root`: the folders under
/// `results/benchmarks`.
fn listed(root: &Path) -> Result<String> {
    let rel = bench_rel("");
    let mut names: Vec<String> = std::fs::read_dir(root.join(&rel))
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        return Ok(format!("no benchmarks yet in {}\n", rel.display()));
    }
    Ok(format!(
        "benchmarks in {}: {}\nbenchmark_status with `name` says where one stands\n",
        rel.display(),
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
    fn a_summary_shows_its_head_and_counts_every_line() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("summary.tsv");
        let lines: Vec<String> = (1..=12).map(|i| format!("L{i}")).collect();
        for text in [lines.join("\n"), format!("{}\n", lines.join("\n"))] {
            std::fs::write(&path, text).unwrap();
            let (head, n) = head_lines(&path).unwrap();
            assert_eq!(head, lines[..SUMMARY_LINES]);
            assert_eq!(n, 12);
        }
        std::fs::write(&path, "L1\nL2").unwrap();
        assert_eq!(
            head_lines(&path).unwrap(),
            (vec!["L1".into(), "L2".into()], 2)
        );
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
            "T4b\tA.r1.score\t\tstart\t\t\t\t",
            "T4c\tA.r1.score\t2\tfailed\t1.0\t0.5\t0.1\tNODE1",
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
        let said = status_text(Path::new("results/benchmarks/N1"), d.path());
        assert!(
            said.contains(
                "tasks seen in events.tsv 5: 2 done, 1 failed, 1 skipped, 1 running, 0 pending"
            ),
            "{said}"
        );
        // B's out-of-memory attempt was retried, so only A's score failed,
        // listed once, by its latest failure.
        assert!(said.contains("failed, latest 1 of 1:\n"), "{said}");
        assert!(
            said.contains("T4c A.r1.score failed, attempt 2, on NODE1"),
            "{said}"
        );
        assert!(!said.contains("attempt 1"), "{said}");
        assert!(!said.contains("B.r1.sim oom"), "{said}");
        assert!(
            said.contains("runner: none started with --detach"),
            "{said}"
        );
        assert!(said.contains("summary.tsv: not yet"), "{said}");
    }
}
