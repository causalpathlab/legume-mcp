//! The legume tools' servers, each run as a child (`<tool> mcp`) and
//! spoken to as an MCP client: its tools listed once, each call sent and
//! waited on, its progress passed on, a stop sent as a cancellation.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How long a server has to start and list its tools.
const START: Duration = Duration::from_secs(20);
/// How long a cancelled call has to wrap up before it is given up on.
const WRAP_UP: Duration = Duration::from_secs(3);

/// A tool's server, running.
pub(crate) struct Server {
    pub name: String,
    pub instructions: String,
    /// Its tools as `tools/list` gave them.
    pub tools: Vec<Value>,
    child: Child,
    /// Closed first as the chat ends: the server then ends its jobs.
    stdin: Option<ChildStdin>,
    /// Where its stderr goes.
    log: std::path::PathBuf,
    rx: Receiver<Value>,
    next: u64,
}

impl Server {
    /// Start `program args` for the project at `project` and list its tools.
    pub(crate) fn start(name: &str, mut cmd: Command, project: &Path) -> Result<Self> {
        let (log, stderr, _) = super::guard::log(project, name);
        // Its own process group: Ctrl-C in the chat's terminal stops the
        // call (a cancellation), not the server and its steps.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd
            .env(crate::project::ROOT_ENV, project)
            .current_dir(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .with_context(|| format!("starting {name}"))?;
        super::guard::keep(child.id());
        let stdin = child.stdin.take().context("its stdin")?;
        let stdout = child.stdout.take().context("its stdout")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let mut s = Self {
            name: name.into(),
            instructions: String::new(),
            tools: Vec::new(),
            child,
            stdin: Some(stdin),
            log,
            rx,
            next: 0,
        };
        let never = AtomicBool::new(false);
        let init = json!({
            "protocolVersion": crate::PROTOCOL_VERSIONS[0],
            "capabilities": {},
            "clientInfo": { "name": "legume chat", "version": env!("CARGO_PKG_VERSION") },
        });
        let r = s.request("initialize", init, &never, START, &mut |_| {})?;
        s.instructions = r["instructions"].as_str().unwrap_or_default().into();
        s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let r = s.request("tools/list", json!({}), &never, START, &mut |_| {})?;
        s.tools = r["tools"].as_array().cloned().unwrap_or_default();
        Ok(s)
    }

    fn send(&mut self, msg: &Value) {
        if let Some(stdin) = &mut self.stdin {
            let _ = writeln!(stdin, "{msg}").and_then(|()| stdin.flush());
        }
    }

    /// Send `method` and wait for its answer; progress goes to `progress`.
    /// `stop` set sends a cancellation, and the call is given up on.
    fn request(
        &mut self,
        method: &str,
        mut params: Value,
        stop: &AtomicBool,
        within: Duration,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        params["_meta"] = json!({ "progressToken": id });
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let start = Instant::now();
        let mut cancelled: Option<Instant> = None;
        loop {
            if cancelled.is_none() && stop.load(Ordering::Relaxed) {
                self.send(&json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": { "requestId": id, "reason": "stopped by the user" },
                }));
                cancelled = Some(Instant::now());
            }
            if cancelled.is_some_and(|c| c.elapsed() > WRAP_UP) {
                bail!("stopped by the user");
            }
            if start.elapsed() > within {
                bail!(
                    "{} did not answer {method} in {} s",
                    self.name,
                    within.as_secs()
                );
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) if msg["id"] == id => {
                    if cancelled.is_some() {
                        bail!("stopped by the user");
                    }
                    if let Some(e) = msg.get("error") {
                        bail!(
                            "{}: {}",
                            self.name,
                            e["message"].as_str().unwrap_or("error")
                        );
                    }
                    return Ok(msg["result"].clone());
                }
                Ok(msg) if msg["method"] == "notifications/progress" => {
                    if let Some(m) = msg["params"]["message"].as_str() {
                        progress(m);
                    }
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("it ended (has it an MCP server? its log: {})", self.log())
                }
            }
        }
    }

    fn log(&self) -> String {
        self.log.display().to_string()
    }

    /// Call `tool` with `args`: its result as `tools/call` gives it.
    pub(crate) fn call(
        &mut self,
        tool: &str,
        args: Value,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Value> {
        // A day at most: jobs are followed with job_wait, a call at a time.
        let params = json!({ "name": tool, "arguments": args });
        self.request(
            "tools/call",
            params,
            stop,
            Duration::from_secs(86_400),
            progress,
        )
    }
}

/// How long a server has to end its jobs and record them once its stdin
/// is closed.
const END: Duration = Duration::from_secs(5);

impl Server {
    /// Wait until it has ended, or `deadline`.
    fn wait_until(&mut self, deadline: Instant) {
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// End `servers` together: each told at once (its stdin closed), so they
/// end their jobs side by side, in [`END`] at most for them all; one that
/// does not end is made to as it is dropped.
pub(crate) fn end_all(servers: &mut [Server]) {
    for s in servers.iter_mut() {
        drop(s.stdin.take());
    }
    let deadline = Instant::now() + END;
    for s in servers.iter_mut() {
        s.wait_until(deadline);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Its stdin closed ends it, as any MCP client's end does: it stops
        // its jobs and records them. Only one that does not end is made to.
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
            self.wait_until(Instant::now() + END);
        }
        super::guard::stop_group(&mut self.child, Duration::from_millis(500));
    }
}

/// The text a model reads for a tool's result: its text pieces, a note
/// for each image, and `isError` said; at most `limit` characters.
pub(crate) fn result_text(result: &Value, limit: usize) -> String {
    let mut out = String::new();
    if result["isError"] == true {
        out.push_str("Error: ");
    }
    for piece in result["content"].as_array().into_iter().flatten() {
        match piece["type"].as_str() {
            Some("text") => out.push_str(piece["text"].as_str().unwrap_or_default()),
            Some("image") => out.push_str("[an image, not shown in the chat]"),
            _ => {}
        }
        out.push('\n');
    }
    if out.trim().is_empty() {
        // Said, so a small model does not take silence for a failure.
        return "Done.".into();
    }
    cut(out.trim_end(), limit)
}

/// `text`, at most `limit` characters, saying how much was left out.
pub(crate) fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.into();
    }
    let kept: String = text.chars().take(limit).collect();
    let left = text.chars().count() - limit;
    format!("{kept}\n[… {left} more characters not shown; ask for less, e.g. fewer rows]")
}
