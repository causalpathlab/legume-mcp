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

pub mod project;
use project::Project;

/// Protocol revisions this server speaks, newest first. A client asking for
/// one of them gets it; any other gets the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Lines of a failed child's stderr quoted in its error.
const STDERR_TAIL: usize = 20;
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
}

impl Call {
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

    /// Whether the client has cancelled this call.
    pub fn is_cancelled(&self) -> bool {
        let set = self.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        set.contains(&self.id)
    }

    /// Tell the client what the call is doing, when it asked to hear.
    pub fn progress(&self, message: &str) {
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
        let line = self.records.as_ref().map(|p| p.command_line(&cmd, outputs));
        let result = self.run_child(cmd);
        if let (Some(p), Some(line)) = (&self.records, line) {
            match &result {
                Ok(_) => {
                    p.script(&format!("{line}\n"));
                    p.index(&self.tool, outputs, summary);
                }
                Err(e) => {
                    let why = p.rel(&format!("{e:#}"));
                    let why = why.lines().next().unwrap_or_default();
                    let commented: String = line.lines().map(|l| format!("#   {l}\n")).collect();
                    p.script(&format!("# did not succeed ({why}):\n{commented}"));
                }
            }
        }
        result
    }

    fn run_child(&self, mut cmd: Command) -> Result<String> {
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
        let mut tail = VecDeque::with_capacity(STDERR_TAIL);
        let status = loop {
            if self.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("cancelled");
            }
            match rx.recv_timeout(POLL) {
                Ok(line) => {
                    self.progress(&line);
                    if tail.len() == STDERR_TAIL {
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
            let tail: Vec<String> = tail.into();
            anyhow::bail!("{shown} failed ({status}):\n{}", tail.join("\n"));
        }
        Ok(out)
    }
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
/// records of the project [`project::ROOT_ENV`] and [`project::OFF_ENV`] say.
pub fn serve(info: ServerInfo, handler: impl Handler) -> Result<()> {
    serve_in(
        BufReader::new(std::io::stdin()),
        std::io::stdout(),
        info,
        Project::root_from_env(),
        handler,
    )
}

/// [`serve`] on any input and output (tests, sockets), keeping no records.
pub fn serve_on<R, W>(input: R, output: W, info: ServerInfo, handler: impl Handler) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    serve_in(input, output, info, None, handler)
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
    mut info: ServerInfo,
    project: Option<PathBuf>,
    mut handler: impl Handler,
) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    let records: Records = project.map(|root| {
        let p = Project::new(root, &info.name, &info.version);
        let note = RECORDS_NOTE.replace("{root}", &p.root().display().to_string());
        info.instructions = Some(match info.instructions.take() {
            Some(text) => format!("{text}\n\n{note}"),
            None => note,
        });
        Arc::new(p)
    });
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
                if let Some(p) = &records {
                    p.call(name, &args);
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
                    respond(&out, id, call_result(result));
                    continue;
                }
                let cx = Call {
                    tool: name.to_string(),
                    records: records.clone(),
                    id: id.to_string(),
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
                let was_cancelled = cx.is_cancelled();
                cancelled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&cx.id);
                if !was_cancelled {
                    respond(&out, id, call_result(result));
                }
            }
            _ => fail(&out, id, -32601, &format!("method not found: {method}")),
        }
    }
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("the stdin reader panicked"))
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
