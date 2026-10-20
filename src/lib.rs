//! A small Model Context Protocol server over stdio for the legume tools.
//!
//! A tool binary implements [`Handler`] (its tool list and how each is
//! called) and hands it to [`serve`]. The server speaks JSON-RPC 2.0, one
//! message per line, and answers `initialize`, `ping`, `tools/list` and
//! `tools/call`; it offers tools only (no resources, prompts or sampling).
//!
//! - stdout carries the protocol, so a tool must never print there: logs go
//!   to stderr, and work that prints runs as a child through [`Call::run`].
//! - A tool's failure is a result with `isError` and the error's chain as
//!   text, for the agent to read; JSON-RPC errors are kept for protocol
//!   faults (unknown method or tool, unreadable message).
//! - Calls run one at a time, in order. `ping` is answered and
//!   `notifications/cancelled` is taken while a call runs: the call sees it
//!   through [`Call::is_cancelled`], [`Call::run`] kills its child, and a
//!   cancelled call gets no response, as the protocol asks.
//! - [`Call::progress`] sends `notifications/progress` when the client
//!   asked for them (a `progressToken`), and does nothing otherwise.
//! - [`serve`] keeps the project's records (see [`project`], after Noble's
//!   guide to organizing computational biology projects): a dated results
//!   folder with a driver script that repeats every step that wrote
//!   something, a lab notebook the agent writes through the built-in
//!   `notebook` tool, an index of outputs, and the inputs' provenance. Tools
//!   that write run a child through [`Call::run`] or [`Call::run_making`],
//!   make their inputs through [`Call::write_file`], and put default
//!   outputs under [`Call::results_dir`], so a user can find and redo an
//!   agent's work without the agent.
//! - [`serve`] also writes a feed a person can follow live, and takes their
//!   stop and note commands (see [`watch`]); tools publish their view with
//!   [`Call::state`].

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod jobs;
pub mod project;
pub mod watch;
use jobs::{Jobs, State as JobState};
use project::Project;
use watch::{Body, Feed, Steer};

/// Protocol revisions this server speaks, newest first. A client asking for
/// one of them gets it; any other gets the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Lines of a failed child's stderr quoted in its error.
const STDERR_TAIL: usize = 20;
/// Lines of a child's stderr kept to find its error in.
const STDERR_KEPT: usize = 400;
/// How often a waiting child run looks for a cancellation.
const POLL: Duration = Duration::from_millis(100);

/// What `initialize` reports: the server's name and version, and the
/// instructions an agent reads before its first call (what the tools are
/// for, and in which order to call them).
#[derive(Clone, Debug, Default)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub instructions: Option<String>,
}

/// One tool as `tools/list` shows it; `input_schema` is a JSON Schema
/// object for its arguments.
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolSpec {
    pub fn new(name: &str, description: &str, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// A piece of a tool's result.
#[derive(Clone, Debug)]
pub enum Content {
    Text(String),
    /// Sent as pretty-printed text and, for the first object among the
    /// result's pieces, as `structuredContent` too.
    Json(Value),
    /// A PNG image, sent base64-encoded.
    Png(Vec<u8>),
}

/// A tool binary's side of the server: its tools, and a call to one of them
/// by name with the arguments the client sent (`{}` when it sent none).
/// State kept between calls lives in the handler.
pub trait Handler {
    fn tools(&self) -> Vec<ToolSpec>;
    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>>;
}

type Out = Arc<Mutex<Box<dyn Write + Send>>>;
/// Ids (as JSON text) of requests the client has cancelled.
type Cancelled = Arc<Mutex<HashSet<String>>>;
type Records = Option<Arc<Project>>;

fn send(out: &Out, msg: &Value) {
    let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
    // A closed stdout ends the session; the reader sees it too.
    let _ = writeln!(w, "{msg}").and_then(|()| w.flush());
}

fn respond(out: &Out, id: &Value, result: Value) {
    send(
        out,
        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    );
}

fn fail(out: &Out, id: &Value, code: i64, message: &str) {
    send(
        out,
        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

/// The call in progress, as a tool sees it.
pub struct Call {
    id: String,
    token: Option<Value>,
    step: AtomicU64,
    out: Out,
    cancelled: Cancelled,
    tool: String,
    records: Records,
    /// The request id as the feed and the control socket name it.
    key: String,
    feed: Option<Arc<Feed>>,
    steer: Arc<Mutex<Steer>>,
    jobs: Arc<Jobs>,
}

impl Call {
    /// Publish the tool's view (what a person following the server should
    /// see now: a map's settings, the round in hand): `state.json` in the
    /// server's watch folder, and an event in its feed.
    pub fn state(&self, state: &Value) {
        if let Some(f) = &self.feed {
            f.state(&self.key, state);
        }
    }

    /// Write `contents` to `path`: an input the tool makes for a child (a
    /// decisions file, say), kept in the driver script as a heredoc.
    pub fn write_file(&self, path: &Path, contents: &str) -> Result<()> {
        std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
        if let Some(p) = &self.records {
            p.file(path, contents);
        }
        Ok(())
    }

    /// Today's results folder (`results/YYYY-MM-DD` under the project),
    /// where a tool puts the outputs the agent did not name; `None` when the
    /// server keeps no records.
    pub fn results_dir(&self) -> Option<PathBuf> {
        let p = self.records.as_ref()?;
        p.day_dir()
            .inspect_err(|e| log::warn!("results folder under {}: {e}", p.root().display()))
            .ok()
    }

    /// Note in `data/README.md` where input `path` came from (a URL, a
    /// repository and version, a file it was converted from).
    pub fn provenance(&self, path: &Path, source: &str) {
        if let Some(p) = &self.records {
            p.provenance(&self.tool, path, source);
        }
    }

    /// Whether the client cancelled this call, or the person following the
    /// server stopped it.
    pub fn is_cancelled(&self) -> bool {
        let set = self.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        set.contains(&self.id) || self.is_stopped()
    }

    fn is_stopped(&self) -> bool {
        let steer = self.steer.lock().unwrap_or_else(|e| e.into_inner());
        steer.stopped.contains(&self.key)
    }

    /// Tell the client what the call is doing, when it asked to hear, and
    /// whoever follows the server.
    pub fn progress(&self, message: &str) {
        if let Some(f) = &self.feed {
            f.emit(Body::Progress {
                id: self.key.clone(),
                message: message.into(),
            });
        }
        let Some(token) = &self.token else { return };
        let n = self.step.fetch_add(1, Ordering::Relaxed) + 1;
        send(
            &self.out,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": { "progressToken": token, "progress": n, "message": message },
            }),
        );
    }

    /// Run `cmd` to its end and return its stdout. Each stderr line is
    /// passed on as progress; a cancellation kills the child; a failure
    /// quotes the last lines of its stderr. The driver script gets its
    /// command line, or a comment when it did not succeed.
    pub fn run(&self, cmd: Command) -> Result<String> {
        self.run_making(cmd, &[], "")
    }

    /// [`Self::run`] for a child that writes nothing (a preview, a report
    /// on stdout): kept out of the driver script, whose call comment is
    /// record enough, so a replay never runs it or misses its inputs.
    pub fn run_reading(&self, cmd: Command) -> Result<String> {
        self.run_child(cmd)
    }

    /// [`Self::run`] for a step that writes `outputs`: the driver script
    /// skips it when they all exist, and `results/INDEX.tsv` lists each
    /// with `summary` (one line: what was made, from what).
    pub fn run_making(&self, cmd: Command, outputs: &[PathBuf], summary: &str) -> Result<String> {
        self.ensure_free(outputs)?;
        let bare = self.records.as_ref().map(|p| p.command(&cmd));
        let result = self
            .run_child(cmd)
            .and_then(|out| made(outputs).map(|()| out));
        record_step(&self.records, &self.tool, bare, outputs, summary, &result);
        result
    }

    /// Refuse `outputs` a running job is writing (the same file, or one
    /// inside the other, however spelled), naming the job. [`Self::run_making`]
    /// and [`Self::spawn_job`] check this themselves; a tool calls it first
    /// when it writes beside the outputs before the step (a step's input
    /// file), or to skip a held name when it chooses one.
    pub fn ensure_free(&self, outputs: &[PathBuf]) -> Result<()> {
        match self.jobs.holder(outputs) {
            None => Ok(()),
            Some((job, path)) => {
                let shown = path.display().to_string();
                let shown = self
                    .records
                    .as_ref()
                    .map_or(shown.clone(), |p| p.rel(&shown));
                anyhow::bail!(
                    "{shown} is being written by {job}: job_wait for it, or choose another out"
                )
            }
        }
    }

    /// [`Self::run_making`] in the background, for a step longer than a
    /// client waits on one call: it returns the job's id (`job-N`) at once,
    /// for the tool to give the agent, who follows it with `job_status`,
    /// `job_wait` and `job_cancel`. The job's log goes to the feed under its
    /// id (a person can stop it there too); it is recorded when it ends, as
    /// a step that ran.
    pub fn spawn_job(&self, cmd: Command, outputs: &[PathBuf], summary: &str) -> Result<String> {
        self.ensure_free(outputs)?;
        let job = self.jobs.add(&self.tool, summary, outputs);
        let id = job.id.clone();
        if let Some(f) = &self.feed {
            f.emit(Body::Call {
                id: id.clone(),
                tool: format!("{} (job)", self.tool),
                args: json!({ "summary": summary, "from": self.key }),
            });
        }
        let bare = self.records.as_ref().map(|p| p.command(&cmd));
        let (records, feed, steer) = (self.records.clone(), self.feed.clone(), self.steer.clone());
        let (tool, outputs, summary) = (self.tool.clone(), outputs.to_vec(), summary.to_string());
        let started = std::time::Instant::now();
        std::thread::Builder::new()
            .name(id.clone())
            .spawn(move || {
                let stopped = || {
                    let s = steer.lock().unwrap_or_else(|e| e.into_inner());
                    s.stopped.contains(&job.id)
                };
                let progress = |line: &str| {
                    job.line(line);
                    if let Some(f) = &feed {
                        f.emit(Body::Progress {
                            id: job.id.clone(),
                            message: line.into(),
                        });
                    }
                };
                let result = run_child(cmd, &|| job.cancelled() || stopped(), &progress)
                    .and_then(|out| made(&outputs).map(|()| out));
                record_step(&records, &tool, bare, &outputs, &summary, &result);
                let (state, text) = match &result {
                    Ok(out) => (JobState::Done, out.clone()),
                    Err(_) if job.cancelled() || stopped() => {
                        if let Some(f) = &feed {
                            let by = if stopped() { "user" } else { "client" };
                            f.emit(Body::Cancelled {
                                id: job.id.clone(),
                                by: by.into(),
                            });
                        }
                        (JobState::Cancelled, "cancelled".to_string())
                    }
                    Err(e) => (JobState::Failed, format!("{e:#}")),
                };
                if let Some(f) = &feed {
                    let ok = state == JobState::Done;
                    let result =
                        json!({ "content": [{ "type": "text", "text": text }], "isError": !ok });
                    f.result(&job.id, &result, started.elapsed().as_millis() as u64);
                }
                job.end(state, text);
            })
            .context("starting a job")?;
        Ok(id)
    }

    fn run_child(&self, cmd: Command) -> Result<String> {
        run_child(cmd, &|| self.is_cancelled(), &|line| self.progress(line))
    }
}

/// Record a step that ran `bare` (when records are kept): its guarded line
/// in the driver script and its outputs in the index when it succeeded, a
/// commented line saying why when it did not.
fn record_step(
    records: &Records,
    tool: &str,
    bare: Option<String>,
    outputs: &[PathBuf],
    summary: &str,
    result: &Result<String>,
) {
    let (Some(p), Some(bare)) = (records, bare) else {
        return;
    };
    let line = p.guarded(&bare, outputs);
    match result {
        Ok(_) => {
            p.script_step(&format!("{line}\n"), Some(&bare));
            p.index(tool, outputs, summary);
        }
        Err(e) => {
            let why = p.rel(&format!("{e:#}"));
            let why = why.lines().next().unwrap_or_default();
            let commented: String = line.lines().map(|l| format!("#   {l}\n")).collect();
            p.script(&format!("# did not succeed ({why}):\n{commented}"));
        }
    }
}

/// Fail a step that exited well but left one of its `outputs` unwritten.
fn made(outputs: &[PathBuf]) -> Result<()> {
    let missing: Vec<String> = outputs
        .iter()
        .filter(|o| !o.exists())
        .map(|o| o.display().to_string())
        .collect();
    anyhow::ensure!(
        missing.is_empty(),
        "finished without writing {}",
        missing.join(", ")
    );
    Ok(())
}

/// What a failed child's stderr is quoted as, [`STDERR_TAIL`] lines at
/// most: its last lines; or, when its last `Error:` (or panic) line is
/// followed closely by a backtrace (a Rust program failing with backtraces
/// on), from that line to where the backtrace starts. An error line with
/// other output after it (a retry that went on to fail otherwise) is not the
/// end, so the tail is.
fn quote(lines: &VecDeque<String>) -> String {
    // A panic ends a Rust program the way an error does.
    let is_error = |l: &str| {
        let l = l.trim_start();
        l.starts_with("Error:") || l.starts_with("error:") || l.contains("' panicked at ")
    };
    let is_backtrace = |l: &str| {
        let l = l.trim_start().to_ascii_lowercase();
        l.starts_with("stack backtrace:")
    };
    let tail = lines.len().saturating_sub(STDERR_TAIL);
    let (from, to) = match lines.iter().rposition(|l| is_error(l)) {
        Some(e) => match lines.iter().skip(e).position(|l| is_backtrace(l)) {
            // The error, up to its backtrace.
            Some(b) if b <= STDERR_TAIL => (e, e + b),
            // The error near the end: it is how the child ended.
            _ if e >= tail => (e, lines.len()),
            _ => (tail, lines.len()),
        },
        None => (tail, lines.len()),
    };
    let picked: Vec<&str> = lines
        .iter()
        .skip(from)
        .take(to.saturating_sub(from).min(STDERR_TAIL))
        .map(String::as_str)
        .collect();
    picked.join("\n")
}

/// Run `cmd` to its end and return its stdout; each stderr line goes to
/// `progress`; `cancelled` is asked as it runs, and kills the child when
/// it says so; a failure quotes its error ([`quote`]). A Rust child prints
/// no backtrace unless the tool asks for one: the agent reads the error.
fn run_child(
    mut cmd: Command,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(&str),
) -> Result<String> {
    // Left as the tool has it when it set the variable, or removed it
    // (get_envs lists a removal too).
    for var in ["RUST_BACKTRACE", "RUST_LIB_BACKTRACE"] {
        if !cmd.get_envs().any(|(k, _)| k == var) {
            cmd.env(var, "0");
        }
    }
    let shown = format!("{cmd:?}");
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {shown}"))?;
    let mut stdout = child.stdout.take().context("child stdout")?;
    let stderr = child.stderr.take().context("child stderr")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        stdout.read_to_string(&mut s).map(|_| s)
    });
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut tail = VecDeque::with_capacity(STDERR_KEPT);
    let status = loop {
        if cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("cancelled");
        }
        match rx.recv_timeout(POLL) {
            Ok(line) => {
                progress(&line);
                if tail.len() == STDERR_KEPT {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // stderr is closed: the child is ending, or ended.
            Err(RecvTimeoutError::Disconnected) => match child.try_wait()? {
                Some(status) => break status,
                None => std::thread::sleep(POLL),
            },
        }
    };
    let out = reader
        .join()
        .map_err(|_| anyhow::anyhow!("reading the child's stdout"))??;
    if !status.success() {
        anyhow::bail!("{shown} failed ({status}):\n{}", quote(&tail));
    }
    Ok(out)
}

/// This server's own binary: where it was started from, or, once that file
/// is replaced (a reinstall while serving), the running image itself.
pub(crate) fn own_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let running = Path::new("/proc/self/exe");
    Some(if !exe.exists() && running.exists() {
        running.to_path_buf()
    } else {
        exe
    })
}

/// A command that runs this server's own binary with `args`: the way a tool
/// runs its CLI as a child. The session's records name it by the server's
/// name.
pub fn own_command<I, S>(args: I) -> Result<Command>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let exe = own_exe().context("finding this server's binary")?;
    let mut cmd = Command::new(exe);
    cmd.args(args);
    Ok(cmd)
}

/// Serve `handler` on stdin and stdout until stdin closes, keeping the
/// records of the project [`project::ROOT_ENV`] and [`project::OFF_ENV`] say,
/// and a feed to follow where [`watch::ENV`] says.
pub fn serve(info: ServerInfo, handler: impl Handler) -> Result<()> {
    let options = Options {
        project: Project::root_from_env(),
        watch: watch::base(),
    };
    serve_with(
        BufReader::new(std::io::stdin()),
        std::io::stdout(),
        info,
        options,
        handler,
    )
}

/// Where a server keeps what it keeps: the project for its records, and
/// the folder its feed's folder goes in ([`watch`]).
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub project: Option<PathBuf>,
    pub watch: Option<PathBuf>,
}

/// [`serve`] on any input and output (tests, sockets), keeping no records.
pub fn serve_on<R, W>(input: R, output: W, info: ServerInfo, handler: impl Handler) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    serve_with(input, output, info, Options::default(), handler)
}

/// What the agent is told about the records, after the server's own
/// instructions.
const RECORDS_NOTE: &str = "\
Records (after Noble, PLoS Comput Biol 2009, doi:10.1371/journal.pcbi.1000424), \
under the project {root}: outputs you do not name go to results/<today>/; every \
step that writes is added to results/<today>/runall.sh, which repeats the work \
without you; results/INDEX.tsv lists each output with a summary. Use the \
`notebook` tool: before starting, a `plan` (the question, the data, and how the \
result will be checked); then what you did and saw, and your conclusions. Tell \
the user where these are when the work is done.";

/// The built-in tools that follow jobs ([`Call::spawn_job`]).
const JOB_TOOLS: [&str; 3] = ["job_status", "job_wait", "job_cancel"];

fn job_specs() -> Vec<ToolSpec> {
    let id = json!({ "type": "string", "description": "The job's id (job-N), as the tool that started it gave it" });
    vec![
        ToolSpec::new(
            "job_status",
            "A background job's state (running, done, failed, cancelled), its last log lines and, once done, its output; without `id`, every job of this server.",
            json!({ "type": "object", "properties": { "id": id } }),
        ),
        ToolSpec::new(
            "job_wait",
            "Wait for a background job to end, up to `seconds` (30; at most 120), then its status as job_status gives it. Call again while it is still running.",
            json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "seconds": { "type": "integer", "minimum": 1, "maximum": jobs::WAIT_MAX },
                },
                "required": ["id"],
            }),
        ),
        ToolSpec::new(
            "job_cancel",
            "Stop a background job: its child is killed, and nothing it would have written is recorded.",
            json!({ "type": "object", "properties": { "id": id }, "required": ["id"] }),
        ),
    ]
}

/// What the agent is told about jobs, after the server's instructions.
const JOBS_NOTE: &str = "\
A tool may run a long step as a background job and return its id (job-N) at \
once: follow it with job_status or job_wait (wait again while it runs), and \
stop it with job_cancel. Its outputs exist once it is done; do not use them \
before. Jobs end when this server does.";

/// The built-in tool every server with records offers.
fn notebook_spec() -> ToolSpec {
    ToolSpec::new(
        "notebook",
        "Add an entry to today's lab notebook (results/<today>/notebook.md, shared by every legume server): your account of the work, kept beside the facts the tools record. Open with a `plan`.",
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["plan", "observation", "conclusion", "note"] },
                "text": { "type": "string", "description": "Markdown" },
            },
            "required": ["kind", "text"],
        }),
    )
}

/// [`serve_on`], keeping the records of the project at `project` when
/// given; the instructions tell the agent about them.
pub fn serve_in<R, W>(
    input: R,
    output: W,
    info: ServerInfo,
    project: Option<PathBuf>,
    handler: impl Handler,
) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    let options = Options {
        project,
        watch: None,
    };
    serve_with(input, output, info, options, handler)
}

/// [`serve_on`] with the records and the feed `options` ask for.
pub fn serve_with<R, W>(
    input: R,
    output: W,
    mut info: ServerInfo,
    options: Options,
    mut handler: impl Handler,
) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    let feed: Option<Arc<Feed>> = options.watch.as_deref().and_then(|base| {
        let meta = watch::Meta {
            server: info.name.clone(),
            version: info.version.clone(),
            pid: std::process::id(),
            project: options.project.clone(),
            started: project::rfc3339(),
        };
        Feed::start(base, meta)
            .inspect_err(|e| log::warn!("mcp feed under {}: {e}", base.display()))
            .ok()
            .map(Arc::new)
    });
    let records: Records = options.project.map(|root| {
        let p = Project::new(root, &info.name, &info.version).watched(feed.clone());
        let note = RECORDS_NOTE.replace("{root}", &p.root().display().to_string());
        info.instructions = Some(match info.instructions.take() {
            Some(text) => format!("{text}\n\n{note}"),
            None => note,
        });
        Arc::new(p)
    });
    let steer: Arc<Mutex<Steer>> = Arc::default();
    #[cfg(unix)]
    if let Some(f) = &feed {
        let records = records.clone();
        let listening = f.listen(steer.clone(), move |text| {
            if let Some(p) = &records {
                p.notebook("user", text);
            }
        });
        if let Err(e) = listening {
            log::warn!("mcp control socket in {}: {e}", f.dir().display());
        }
    }
    let out: Out = Arc::new(Mutex::new(Box::new(output)));
    let cancelled: Cancelled = Arc::default();
    let (tx, rx) = channel::<Value>();
    // The reader answers what must not wait for a running call: pings,
    // cancellations and unreadable lines. The rest queues for the loop.
    let reader = {
        let (out, cancelled) = (out.clone(), cancelled.clone());
        std::thread::spawn(move || {
            let mut input = input;
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match input.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!("mcp: reading stdin: {e}");
                        break;
                    }
                }
                // A line that is not UTF-8 or not JSON is answered, and the
                // session goes on.
                let Some(msg) = std::str::from_utf8(&buf)
                    .ok()
                    .and_then(|l| serde_json::from_str::<Value>(l).ok())
                else {
                    if !buf.iter().all(u8::is_ascii_whitespace) {
                        fail(&out, &Value::Null, -32700, "parse error");
                    }
                    continue;
                };
                if !msg.is_object() || msg.get("method").is_some_and(|m| !m.is_string()) {
                    fail(
                        &out,
                        &msg["id"],
                        -32600,
                        "invalid request (batches are not taken)",
                    );
                    continue;
                }
                match msg.get("method").and_then(Value::as_str) {
                    Some("ping") => respond(&out, &msg["id"], json!({})),
                    Some("notifications/cancelled") => {
                        let id = msg["params"]["requestId"].to_string();
                        cancelled
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id);
                    }
                    _ => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        })
    };
    let mut specs = handler.tools();
    let builtin_notebook = records.is_some() && !specs.iter().any(|t| t.name == "notebook");
    if builtin_notebook {
        specs.push(notebook_spec());
    }
    let jobs: Arc<Jobs> = Arc::default();
    let builtin_jobs = !specs.iter().any(|t| JOB_TOOLS.contains(&t.name.as_str()));
    if builtin_jobs {
        specs.extend(job_specs());
        info.instructions = Some(match info.instructions.take() {
            Some(text) => format!("{text}\n\n{JOBS_NOTE}"),
            None => JOBS_NOTE.to_string(),
        });
    }
    for msg in rx {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue; // a response to a request we never send
        };
        let Some(id) = msg.get("id") else {
            continue; // notifications/initialized and the like
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => respond(&out, id, initialize(&info, &params)),
            "tools/list" => {
                let tools: Vec<Value> = specs
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                        })
                    })
                    .collect();
                respond(&out, id, json!({ "tools": tools }));
            }
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                if !specs.iter().any(|t| t.name == name) {
                    fail(&out, id, -32602, &format!("unknown tool: {name}"));
                    continue;
                }
                let args = match params.get("arguments") {
                    Some(a) if !a.is_null() => a.clone(),
                    _ => json!({}),
                };
                let key = id.as_str().map_or_else(|| id.to_string(), str::to_string);
                if let Some(p) = &records {
                    p.call(name, &args);
                }
                if let Some(f) = &feed {
                    f.emit(Body::Call {
                        id: key.clone(),
                        tool: name.into(),
                        args: args.clone(),
                    });
                }
                let started = std::time::Instant::now();
                let done = Done {
                    out: &out,
                    feed: feed.as_deref(),
                    steer: &steer,
                    id,
                    key: &key,
                    started,
                };
                if builtin_jobs && JOB_TOOLS.contains(&name) {
                    let id = args["id"].as_str();
                    let result = match (name, id) {
                        ("job_status", id) => jobs.status(id),
                        ("job_wait", Some(id)) => {
                            let seconds = args["seconds"].as_u64().unwrap_or(30);
                            jobs.wait(id, seconds)
                        }
                        ("job_cancel", Some(id)) => jobs.cancel(id),
                        _ => Err(anyhow::anyhow!("`id` is required")),
                    };
                    done.respond(call_result(result.map(|v| vec![Content::Json(v)])));
                    continue;
                }
                if builtin_notebook && name == "notebook" {
                    let p = records.as_ref().expect("a built-in notebook has records");
                    let result = match (args["kind"].as_str(), args["text"].as_str()) {
                        (Some(kind), Some(text)) => {
                            p.notebook(kind, text);
                            Ok(vec![])
                        }
                        _ => Err(anyhow::anyhow!("`kind` and `text` are required")),
                    };
                    done.respond(call_result(result));
                    continue;
                }
                let cx = Call {
                    tool: name.to_string(),
                    records: records.clone(),
                    id: id.to_string(),
                    key: key.clone(),
                    feed: feed.clone(),
                    steer: steer.clone(),
                    jobs: jobs.clone(),
                    token: params["_meta"].get("progressToken").cloned(),
                    step: AtomicU64::new(0),
                    out: out.clone(),
                    cancelled: cancelled.clone(),
                };
                let result = catch_unwind(AssertUnwindSafe(|| handler.call(name, args, &cx)))
                    .unwrap_or_else(|p| {
                        let what = p
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| p.downcast_ref::<&str>().copied())
                            .unwrap_or("panic");
                        Err(anyhow::anyhow!("{name} panicked: {what}"))
                    });
                let stopped = steer
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stopped
                    .remove(&key);
                let by_client = cancelled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&cx.id);
                if stopped {
                    // The person stopped it: unlike a client's cancel, the
                    // client is still owed an answer.
                    done.cancelled("user");
                    done.respond(call_result(Err(anyhow::anyhow!("stopped by the user"))));
                } else if by_client {
                    done.cancelled("client");
                } else {
                    done.respond(call_result(result));
                }
            }
            _ => fail(&out, id, -32601, &format!("method not found: {method}")),
        }
    }
    let result = reader
        .join()
        .map_err(|_| anyhow::anyhow!("the stdin reader panicked"));
    // Jobs end with their server.
    jobs.stop_all();
    if let Some(f) = &feed {
        f.emit(Body::Ended { ok: result.is_ok() });
    }
    result
}

/// How a tool call ends: its answer, with the person's notes, to the client
/// and the feed.
struct Done<'a> {
    out: &'a Out,
    feed: Option<&'a Feed>,
    steer: &'a Mutex<Steer>,
    id: &'a Value,
    key: &'a str,
    started: std::time::Instant,
}

impl Done<'_> {
    /// Answer with `result`, the notes left since the last answer added.
    fn respond(&self, mut result: Value) {
        let notes = std::mem::take(&mut self.steer.lock().unwrap_or_else(|e| e.into_inner()).notes);
        if let Some(content) = result["content"].as_array_mut() {
            for n in notes {
                let text = format!("Note from the user: {n}");
                content.push(json!({ "type": "text", "text": text }));
            }
        }
        if let Some(f) = self.feed {
            f.result(self.key, &result, self.started.elapsed().as_millis() as u64);
        }
        respond(self.out, self.id, result);
    }

    fn cancelled(&self, by: &str) {
        if let Some(f) = self.feed {
            f.emit(Body::Cancelled {
                id: self.key.into(),
                by: by.into(),
            });
        }
    }
}

fn initialize(info: &ServerInfo, params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    let mut v = json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": info.name, "version": info.version },
    });
    if let Some(text) = &info.instructions {
        v["instructions"] = json!(text);
    }
    v
}

fn call_result(result: Result<Vec<Content>>) -> Value {
    let pieces = match result {
        Ok(p) => p,
        Err(e) => {
            return json!({
                "content": [{ "type": "text", "text": format!("{e:#}") }],
                "isError": true,
            })
        }
    };
    let mut structured: Option<Map<String, Value>> = None;
    let content: Vec<Value> = pieces
        .into_iter()
        .map(|c| match c {
            Content::Text(text) => json!({ "type": "text", "text": text }),
            Content::Json(v) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_default();
                if structured.is_none() {
                    if let Value::Object(m) = v {
                        structured = Some(m);
                    }
                }
                json!({ "type": "text", "text": text })
            }
            Content::Png(bytes) => json!({
                "type": "image",
                "mimeType": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            }),
        })
        .collect();
    let mut v = json!({ "content": content, "isError": false });
    if let Some(m) = structured {
        v["structuredContent"] = Value::Object(m);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> VecDeque<String> {
        text.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_rust_error_is_quoted_up_to_its_backtrace() {
        let mut l = vec![
            "loading",
            "Error: no section matches qc",
            "",
            "Stack backtrace:",
        ];
        let frames: Vec<String> = (0..40).map(|i| format!("  {i}: frame")).collect();
        l.extend(frames.iter().map(String::as_str));
        assert_eq!(quote(&lines(&l)), "Error: no section matches qc\n");
    }

    #[test]
    fn an_error_the_child_went_on_from_is_not_its_end() {
        // A retry logged as an error, then much more, then the real end.
        let mut l = vec!["error: fetch failed, retrying"];
        let more: Vec<String> = (0..30).map(|i| format!("step {i}")).collect();
        l.extend(more.iter().map(String::as_str));
        l.push("fatal: out of disk");
        let q = quote(&lines(&l));
        assert!(q.ends_with("fatal: out of disk"), "{q}");
        assert!(!q.contains("retrying"), "{q}");
    }

    #[test]
    fn an_error_near_the_end_is_quoted_from() {
        let l = ["a", "b", "Error: bad input", "caused by: x"];
        assert_eq!(quote(&lines(&l)), "Error: bad input\ncaused by: x");
    }

    #[test]
    fn a_panic_after_an_early_error_is_the_end_quoted() {
        // A retry logged as an error, much more, then a panic with backtrace.
        let mut l = vec!["error: fetch failed, retrying"];
        let more: Vec<String> = (0..30).map(|i| format!("step {i}")).collect();
        l.extend(more.iter().map(String::as_str));
        l.extend([
            "thread 'main' panicked at src/fit.rs:7:5:",
            "index out of bounds",
            "stack backtrace:",
            "  0: frame",
        ]);
        let q = quote(&lines(&l));
        assert_eq!(
            q,
            "thread 'main' panicked at src/fit.rs:7:5:\nindex out of bounds"
        );
    }

    #[test]
    fn an_error_far_from_a_backtrace_is_not_paired_with_it() {
        // An error, then far more than a quote, then a backtrace with no
        // error line of its own: the tail is quoted, not the old error.
        let mut l = vec!["error: fetch failed, retrying"];
        let more: Vec<String> = (0..30).map(|i| format!("step {i}")).collect();
        l.extend(more.iter().map(String::as_str));
        l.extend(["stack backtrace:", "  0: frame"]);
        let q = quote(&lines(&l));
        assert!(!q.contains("retrying"), "{q}");
        assert!(q.ends_with("  0: frame"), "{q}");
    }
}
