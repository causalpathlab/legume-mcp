//! `watch::Agent` on a live server, with no UI: what a sidebar or a tool's
//! own map would follow.

use anyhow::Result;
use legume_mcp::watch::{self, Action, Agent, Outcome, Status};
use legume_mcp::{serve_with, Call, Content, Handler, Options, ServerInfo, ToolSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

struct Tools;

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        ["wait", "make", "show"]
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
            "make" => {
                let out = PathBuf::from(args["out"].as_str().unwrap());
                let mut cmd = Command::new("touch");
                cmd.arg(&out);
                cx.run_making(cmd, std::slice::from_ref(&out), "a file")?;
                Ok(vec![Content::Text("made".into())])
            }
            _ => {
                cx.state(&args);
                Ok(vec![Content::Text("shown".into())])
            }
        }
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

#[test]
fn an_agent_follows_a_server_without_a_screen() {
    let tmp = tempfile::tempdir().unwrap();
    let (base, project) = (tmp.path().join("w"), tmp.path().join("p"));
    let (reader, mut writer) = std::io::pipe().unwrap();
    let (out_r, out_w) = std::io::pipe().unwrap();
    let options = Options {
        project: Some(project.clone()),
        watch: Some(base.clone()),
    };
    let info = ServerInfo {
        name: "t".into(),
        version: "0".into(),
        instructions: None,
    };
    let server = std::thread::spawn(move || {
        serve_with(BufReader::new(reader), out_w, info, options, Tools).unwrap();
    });
    let mut replies = BufReader::new(out_r).lines();
    let mut send = move |m: Value| writeln!(writer, "{m}").unwrap();
    let live = until("the server", || watch::find_in(&base, "t", None).pop());
    let mut agent = Agent::new(&live.dir, true).unwrap();
    assert_eq!(agent.take_new_state(), None, "no view published yet");

    // A step that writes: its command comes through.
    let made = project.join("made.txt");
    send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                 "params": { "name": "make", "arguments": { "out": made } } }));
    replies.next().unwrap().unwrap();
    // A view, taken once.
    send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                 "params": { "name": "show", "arguments": { "focus": [3] } } }));
    replies.next().unwrap().unwrap();
    until("both calls", || {
        agent.poll();
        (agent.calls().len() == 2).then_some(())
    });
    assert!(agent
        .records()
        .any(|c| c.starts_with("touch ") && c.ends_with("made.txt")));
    assert_eq!(agent.take_new_state(), Some(json!({ "focus": [3] })));
    assert_eq!(agent.take_new_state(), None, "a view is taken once");
    // Opened after the view was published: it comes once, not again when
    // the first poll replays the feed.
    let mut late = Agent::new(&live.dir, false).unwrap();
    assert_eq!(late.take_new_state(), Some(json!({ "focus": [3] })));
    late.poll();
    assert_eq!(late.take_new_state(), None, "the replay is not a new view");
    assert_eq!(late.calls().len(), 2, "the feed so far is read when opened");

    // A sidebar's lines.
    let lines = agent.lines(3, 30);
    assert_eq!(lines[0], "t · following");
    assert!(
        lines[1].contains("make done") && lines[2].starts_with('>'),
        "{lines:?}"
    );
    assert!(lines.iter().all(|l| l.chars().count() <= 30));

    // Moving pauses; Follow resumes.
    assert!(matches!(agent.act(Action::Previous), Outcome::Done));
    assert!(!agent.following());
    assert!(
        agent.lines(3, 40)[1].starts_with('>'),
        "the selected call marked"
    );
    agent.act(Action::Follow);
    assert!(agent.following());

    // Steering: a note, then a stop of a running call.
    send(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                 "params": { "name": "wait", "arguments": {} } }));
    until("the long call running", || {
        agent.poll();
        agent
            .calls()
            .last()
            .filter(|c| c.tool == "wait" && !c.progress.is_empty())
            .map(|_| ())
    });
    assert!(matches!(agent.act(Action::Note), Outcome::WriteNote));
    assert!(agent.note("look at cluster 3").starts_with("note left"));
    assert!(
        matches!(agent.act(Action::Stop), Outcome::Say(s) if s.starts_with("asked to stop wait"))
    );
    let stopped: Value = serde_json::from_str(&replies.next().unwrap().unwrap()).unwrap();
    assert_eq!(stopped["result"]["isError"], true);
    until("the stop", || {
        agent.poll();
        matches!(
            agent.calls().last().map(|c| &c.status),
            Some(Status::Stopped { .. })
        )
        .then_some(())
    });
    assert!(agent
        .notes()
        .any(|(who, t)| who == "you" && t == "look at cluster 3"));

    // The server ends: steering goes, and what it had stays.
    drop(send);
    server.join().unwrap();
    until("the end", || {
        agent.poll();
        agent.ended().then_some(())
    });
    assert!(!agent.can_steer());
    assert!(matches!(agent.act(Action::Stop), Outcome::Unavailable));
    assert_eq!(agent.lines(1, 40), ["t · ended"]);
    assert_eq!(agent.calls().len(), 3);
}
