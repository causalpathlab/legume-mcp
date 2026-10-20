//! The person's pane on a live server: it follows the calls, pauses when
//! moved and follows again on F, stops a running call, leaves the agent a
//! note, takes the tool's view, opens a call through the tool, and says when
//! the server has ended. Watch-only panes offer no steering.

use anyhow::Result;
use legume_mcp::watch;
use legume_mcp::watch::view::{AgentPane, Handled, Status};
use legume_mcp::{serve_with, Call, Content, Handler, Options, ServerInfo, ToolSpec};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Lines, PipeReader, PipeWriter, Write};
use std::path::PathBuf;
use std::process::Command;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Tools;

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        ["wait", "show"]
            .map(|n| ToolSpec::new(n, n, json!({ "type": "object" })))
            .into()
    }

    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>> {
        match name {
            "wait" => {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "echo started >&2; sleep 30"]);
                Ok(vec![Content::Text(cx.run(cmd)?)])
            }
            _ => {
                cx.state(&args);
                Ok(vec![Content::Text("shown".into())])
            }
        }
    }
}

/// A server on pipes, with its feed under the test's folder.
struct Server {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    send: Option<PipeWriter>,
    replies: Lines<BufReader<PipeReader>>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("w");
        let (reader, writer) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let options = Options {
            project: Some(tmp.path().join("p")),
            watch: Some(base.clone()),
        };
        let info = ServerInfo {
            name: "t".into(),
            version: "0".into(),
            instructions: None,
        };
        let thread = std::thread::spawn(move || {
            serve_with(BufReader::new(reader), out_w, info, options, Tools).unwrap();
        });
        let dir = until("the server's folder", || watch::live_in(&base).pop()).dir;
        Self {
            _tmp: tmp,
            dir,
            send: Some(writer),
            replies: BufReader::new(out_r).lines(),
            thread: Some(thread),
        }
    }

    fn call(&mut self, id: Value, tool: &str) {
        let m = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
                        "params": { "name": tool, "arguments": { "focus": [3] } } });
        writeln!(self.send.as_ref().unwrap(), "{m}").unwrap();
    }

    fn reply(&mut self) -> Value {
        serde_json::from_str(&self.replies.next().unwrap().unwrap()).unwrap()
    }

    /// Close its input and wait for it to end.
    fn end(&mut self) {
        self.send.take();
        self.thread.take().unwrap().join().unwrap();
    }
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(x) = f() {
            return x;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "waited too long for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The pane drawn on a 160 x 30 screen, as text.
fn screen(pane: &mut AgentPane) -> String {
    let mut t = Terminal::new(TestBackend::new(160, 30)).unwrap();
    t.draw(|f| pane.render(f.area(), f.buffer_mut())).unwrap();
    t.backend().to_string()
}

/// Poll the pane until its screen shows `want`.
fn shows(pane: &mut AgentPane, want: &str) -> String {
    until(want, || {
        pane.poll();
        let s = screen(pane);
        s.contains(want).then_some(s)
    })
}

fn key(c: char) -> KeyEvent {
    KeyCode::Char(c).into()
}

#[test]
fn a_person_follows_pauses_stops_notes_and_opens() {
    let mut server = Server::start();
    let mut pane = AgentPane::new(&server.dir, true).unwrap();

    // Following: the running call, its log, selected as it comes.
    server.call(json!("a"), "wait");
    let s = shows(&mut pane, "started");
    assert!(
        s.contains("wait") && s.contains("running") && s.contains("following"),
        "{s}"
    );
    assert!(
        s.contains("s stop") && s.contains("n note"),
        "steering offered: {s}"
    );

    // Moving pauses following; F follows again.
    assert!(matches!(pane.key(KeyCode::Up.into()), Handled::Consumed));
    assert!(!pane.following());
    assert!(screen(&mut pane).contains("paused — F follows"));
    pane.key(key('F'));
    assert!(pane.following());

    // A note typed and left, then the running call stopped.
    pane.key(key('n'));
    assert!(pane.typing());
    for c in "look at cluster 3".chars() {
        pane.key(key(c));
    }
    assert!(screen(&mut pane).contains("note for the agent: look at cluster 3"));
    pane.key(KeyCode::Enter.into());
    assert!(!pane.typing());
    pane.key(key('s'));
    let stopped = server.reply();
    assert_eq!(stopped["result"]["isError"], true, "{stopped}");
    assert!(
        stopped
            .to_string()
            .contains("Note from the user: look at cluster 3"),
        "{stopped}"
    );
    let s = shows(&mut pane, "stopped by the user");
    assert!(
        s.contains("you: look at cluster 3"),
        "the notebook shows the note: {s}"
    );

    // A call that publishes the tool's view, then opened through the tool.
    server.call(json!(2), "show");
    server.reply();
    shows(&mut pane, "done");
    assert_eq!(pane.state(), Some(&json!({ "focus": [3] })));
    // Opened, the call goes to the tool whole: its name, arguments, result.
    match pane.key(key('o')) {
        Handled::Open(c) => {
            assert_eq!(
                (c.tool.as_str(), &c.args),
                ("show", &json!({ "focus": [3] }))
            );
            assert!(
                matches!(c.status, Status::Done { .. }) && c.text.contains("shown"),
                "{c:?}"
            );
        }
        _ => panic!("o opens a finished call"),
    }
    // The stopped call opens too, for the tool to say what it can.
    pane.key(key('k'));
    assert!(
        matches!(pane.key(key('o')), Handled::Open(c) if matches!(c.status, Status::Stopped { .. }))
    );

    // The server ends: the pane says so and keeps what it had.
    server.end();
    let s = shows(&mut pane, "ended");
    assert!(s.contains("show") && s.contains("wait"), "{s}");
    assert!(!s.contains("s stop"), "no steering once ended: {s}");
}

#[test]
fn a_watch_only_pane_offers_no_steering() {
    let mut server = Server::start();
    let mut pane = AgentPane::new(&server.dir, false).unwrap();
    server.call(json!("a"), "wait");
    let s = shows(&mut pane, "started");
    assert!(!s.contains("s stop") && !s.contains("n note"), "{s}");
    assert!(matches!(pane.key(key('s')), Handled::Ignored));
    assert!(matches!(pane.key(key('n')), Handled::Ignored));
    assert!(!pane.typing());
    // Nothing went to the server: the call still runs.
    pane.poll();
    assert!(screen(&mut pane).contains("running"));
    watch::control(&server.dir).stop("a").unwrap();
    server.reply();
    server.end();
}
