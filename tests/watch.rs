//! Following a server while it serves: its feed, its view, and a person
//! stopping a call and leaving the agent a note.

use anyhow::Result;
use legume_mcp::watch::{self, Body, From};
use legume_mcp::{serve_with, Call, Content, Handler, Options, ServerInfo, ToolSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::Command;
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
fn a_person_follows_stops_and_leaves_a_note() {
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

    let found = until("the server's folder", || watch::live_in(&base).pop());
    assert!(found.alive);
    assert_eq!(
        (found.meta.server.as_str(), found.meta.version.as_str()),
        ("t", "0")
    );
    let dir = found.dir;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "owner-only");
    }

    // A long call, followed, then stopped by the person, with a note.
    send(json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/call",
                 "params": { "name": "wait", "arguments": {} } }));
    let mut feed = watch::follow(&dir, From::Start).unwrap();
    until("the child's first line", || {
        std::iter::from_fn(|| feed.try_next())
            .find(|e| matches!(&e.body, Body::Progress { id, message } if id == "a" && message == "started"))
    });
    let control = watch::control(&dir);
    control.note("look at cluster 3").unwrap();
    control.stop("a").unwrap();
    let stopped: Value = serde_json::from_str(&replies.next().unwrap().unwrap()).unwrap();
    assert_eq!(stopped["id"], "a");
    assert_eq!(stopped["result"]["isError"], true);
    let texts: Vec<&str> = stopped["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "stopped by the user",
            "Note from the user: look at cluster 3"
        ]
    );

    // A call that publishes its view.
    let offset = feed.offset();
    send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                 "params": { "name": "show", "arguments": { "focus": [3] } } }));
    let shown: Value = serde_json::from_str(&replies.next().unwrap().unwrap()).unwrap();
    assert_eq!(
        shown["result"]["content"].as_array().unwrap().len(),
        1,
        "the note went once"
    );
    assert_eq!(watch::state(&dir), Some(json!({ "focus": [3] })));
    drop(send);
    server.join().unwrap();

    let events: Vec<Body> = std::iter::from_fn({
        let mut f = watch::follow(&dir, From::Start).unwrap();
        move || f.try_next()
    })
    .map(|e| e.body)
    .collect();
    assert!(matches!(&events[0], Body::Server(m) if m.server == "t"));
    let has = |want: &dyn Fn(&Body) -> bool| events.iter().any(want);
    assert!(has(
        &|b| matches!(b, Body::Call { id, tool, .. } if id == "a" && tool == "wait")
    ));
    assert!(has(
        &|b| matches!(b, Body::Note { text } if text == "look at cluster 3")
    ));
    assert!(has(
        &|b| matches!(b, Body::Cancelled { id, by } if id == "a" && by == "user")
    ));
    assert!(has(
        &|b| matches!(b, Body::Result { id, ok: false, text, .. } if id == "a" && text.contains("stopped by the user"))
    ));
    assert!(has(
        &|b| matches!(b, Body::Notebook { entry, text } if entry == "user" && text == "look at cluster 3")
    ));
    assert!(has(
        &|b| matches!(b, Body::State { id, state } if id == "2" && state["focus"] == json!([3]))
    ));
    assert!(has(
        &|b| matches!(b, Body::Record { line } if line.contains("did not succeed"))
    ));
    // From an offset: only what came after.
    let later: Vec<Body> = std::iter::from_fn({
        let mut f = watch::follow(&dir, From::Offset(offset)).unwrap();
        move || f.try_next()
    })
    .map(|e| e.body)
    .collect();
    assert!(
        matches!(&later[0], Body::Note { .. }),
        "the first event after the progress line followed: {later:?}"
    );
    assert!(!later
        .iter()
        .any(|b| matches!(b, Body::Call { id, .. } if id == "a")));
    let notebook = std::fs::read_dir(project.join("results"))
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .map(|e| std::fs::read_to_string(e.path().join("notebook.md")).unwrap())
        .unwrap();
    assert!(
        notebook.contains("· t · user\n\nlook at cluster 3"),
        "{notebook}"
    );
}
