//! Background jobs: a tool starts a long step and returns at once; the
//! agent follows it with job_status / job_wait / job_cancel; a person can
//! stop it too; a job that succeeds is recorded when it ends.

use anyhow::Result;
use legume_mcp::watch::{self, Body, From};
use legume_mcp::{serve_with, Call, Content, Handler, Options, ServerInfo, Step, ToolSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Lines, PipeReader, Write};
use std::path::PathBuf;
use std::process::Command;

struct Tools;

impl Handler for Tools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec::new("fit", "fit", json!({ "type": "object" })),
            ToolSpec::new("free", "is out free", json!({ "type": "object" })),
        ]
    }

    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>> {
        let out = PathBuf::from(args["out"].as_str().unwrap());
        if name == "free" {
            cx.ensure_free(&[out])?;
            return Ok(vec![Content::Json(json!({ "free": true }))]);
        }
        let mut cmd = Command::new("sh");
        // `write: false` exits well without writing its output.
        let touch = if args["write"] == false {
            String::new()
        } else {
            format!("printf new > '{}';", out.display())
        };
        let script = format!(
            "echo fitting >&2; sleep {}; {touch} printf fitted",
            args["seconds"]
        );
        cmd.args(["-c", &script]);
        let mut step = Step::new(std::slice::from_ref(&out), "a model fit");
        // `replace: true` rewrites an output that may exist.
        if args["replace"] == true {
            step = step.replacing();
        }
        let id = cx.spawn_step(cmd, step)?;
        Ok(vec![Content::Json(json!({ "job": id }))])
    }
}

struct Client {
    send: Box<dyn FnMut(Value)>,
    replies: Lines<BufReader<PipeReader>>,
    id: u64,
}

impl Client {
    fn call(&mut self, name: &str, args: Value) -> Value {
        self.id += 1;
        (self.send)(
            json!({ "jsonrpc": "2.0", "id": self.id, "method": "tools/call",
                            "params": { "name": name, "arguments": args } }),
        );
        let reply: Value = serde_json::from_str(&self.replies.next().unwrap().unwrap()).unwrap();
        reply["result"].clone()
    }
}

fn structured(r: &Value) -> &Value {
    &r["structuredContent"]
}

#[test]
fn a_long_step_runs_as_a_job_and_is_recorded_when_it_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let (base, project) = (tmp.path().join("w"), tmp.path().join("p"));
    std::fs::create_dir_all(&project).unwrap();
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
    let mut c = Client {
        send: Box::new(move |m| writeln!(writer, "{m}").unwrap()),
        replies: BufReader::new(out_r).lines(),
        id: 0,
    };
    (c.send)(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} }));
    let init: Value = serde_json::from_str(&c.replies.next().unwrap().unwrap()).unwrap();
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("job_wait"));

    // It returns at once, running.
    let made = project.join("model.bin");
    let started = std::time::Instant::now();
    let r = c.call("fit", json!({ "out": made, "seconds": 1 }));
    assert!(
        started.elapsed().as_millis() < 900,
        "the call did not wait for the fit"
    );
    let job = structured(&r)["job"].as_str().unwrap().to_string();
    assert_eq!(job, "job-1");
    let s = c.call("job_status", json!({ "id": job }));
    assert_eq!(structured(&s)["state"], "running");
    assert_eq!(s["_meta"]["legume/next"][0]["tool"], "job_wait", "{s}");

    // Waited on, it ends done, with its output; recorded as it ends.
    let w = c.call("job_wait", json!({ "id": job, "seconds": 20 }));
    assert_eq!(structured(&w)["state"], "done", "{w}");
    assert_eq!(structured(&w)["output"], "fitted");
    let next = &w["_meta"]["legume/next"][0];
    assert_eq!(next["args"], json!({ "kind": "observation" }), "{w}");
    assert!(
        w["content"][1]["text"]
            .as_str()
            .unwrap()
            .starts_with("Next, if the person agrees"),
        "{w}"
    );
    assert!(made.exists());
    let day = std::fs::read_dir(project.join("results"))
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    let script = std::fs::read_to_string(day.join("runall.sh")).unwrap();
    assert!(script.contains("if [ ! -e model.bin ]; then"), "{script}");
    let index = std::fs::read_to_string(project.join("results/INDEX.tsv")).unwrap();
    assert!(
        index.contains("\tt\tfit\tmodel.bin\ta model fit"),
        "{index}"
    );

    // A job cancelled by the agent, and one stopped by the person: neither
    // is recorded as a step.
    let long = structured(&c.call("fit", json!({ "out": project.join("a"), "seconds": 30 })))
        ["job"]
        .as_str()
        .unwrap()
        .to_string();
    c.call("job_cancel", json!({ "id": long }));
    let w = c.call("job_wait", json!({ "id": long, "seconds": 10 }));
    assert_eq!(structured(&w)["state"], "cancelled", "{w}");
    let other = structured(&c.call("fit", json!({ "out": project.join("b"), "seconds": 30 })))
        ["job"]
        .as_str()
        .unwrap()
        .to_string();
    let dir = watch::find_in(&base, "t", None).pop().unwrap().dir;
    watch::control(&dir).stop(&other).unwrap();
    let w = c.call("job_wait", json!({ "id": other, "seconds": 10 }));
    assert_eq!(structured(&w)["state"], "cancelled", "{w}");
    let all = c.call("job_status", json!({}));
    assert_eq!(structured(&all)["jobs"].as_array().unwrap().len(), 3);
    assert!(c.call("job_wait", json!({ "id": "job-9" }))["isError"] == true);

    // A running job's output is reserved: a second call for it is refused,
    // naming the job, until the job ends.
    let held = project.join("held");
    let first = structured(&c.call("fit", json!({ "out": held, "seconds": 30 })))["job"]
        .as_str()
        .unwrap()
        .to_string();
    let refused = c.call("fit", json!({ "out": held, "seconds": 1 }));
    assert_eq!(refused["isError"], true, "{refused}");
    let why = refused["content"][0]["text"].as_str().unwrap();
    assert!(
        why.contains(&format!("is being written by {first}")),
        "{why}"
    );
    // A tool asks the same before writing anything of its own.
    let spelled = project.join("sub/../held/part");
    assert_eq!(c.call("free", json!({ "out": spelled }))["isError"], true);
    let free = c.call("free", json!({ "out": project.join("other") }));
    assert_eq!(structured(&free)["free"], true, "{free}");
    c.call("job_cancel", json!({ "id": first }));
    c.call("job_wait", json!({ "id": first, "seconds": 10 }));
    let again = c.call("fit", json!({ "out": held, "seconds": 0 }));
    assert_ne!(
        again["isError"], true,
        "free again once the job ended: {again}"
    );
    let id = structured(&again)["job"].as_str().unwrap().to_string();
    c.call("job_wait", json!({ "id": id, "seconds": 10 }));

    // Exiting well without writing its output is a failure, not recorded.
    let unwritten = project.join("never");
    let quiet = structured(&c.call(
        "fit",
        json!({ "out": unwritten, "seconds": 0, "write": false }),
    ))["job"]
        .as_str()
        .unwrap()
        .to_string();
    let w = c.call("job_wait", json!({ "id": quiet, "seconds": 10 }));
    assert_eq!(structured(&w)["state"], "failed", "{w}");
    assert!(structured(&w)["error"]
        .as_str()
        .unwrap()
        .contains("finished without writing"));
    let index = std::fs::read_to_string(project.join("results/INDEX.tsv")).unwrap();
    assert!(!index.contains("never"), "{index}");
    assert!(!project.join("a").exists() && !project.join("b").exists());

    // A step that replaces its output runs in the background too: it holds
    // the output while it runs, rewrites it, and is recorded unguarded.
    let coords = project.join("coords.tsv");
    std::fs::write(&coords, "old").unwrap();
    let slow = structured(&c.call(
        "fit",
        json!({ "out": coords, "seconds": 30, "replace": true }),
    ))["job"]
        .as_str()
        .unwrap()
        .to_string();
    for replace in [true, false] {
        let refused = c.call(
            "fit",
            json!({ "out": coords, "seconds": 0, "replace": replace }),
        );
        assert_eq!(refused["isError"], true, "{refused}");
        let why = refused["content"][0]["text"].as_str().unwrap();
        assert!(
            why.contains(&format!("is being written by {slow}")),
            "{why}"
        );
    }
    c.call("job_cancel", json!({ "id": slow }));
    c.call("job_wait", json!({ "id": slow, "seconds": 10 }));
    let redo = structured(&c.call(
        "fit",
        json!({ "out": coords, "seconds": 0, "replace": true }),
    ))["job"]
        .as_str()
        .unwrap()
        .to_string();
    let w = c.call("job_wait", json!({ "id": redo, "seconds": 10 }));
    assert_eq!(structured(&w)["state"], "done", "{w}");
    assert_eq!(std::fs::read_to_string(&coords).unwrap(), "new");
    let script = std::fs::read_to_string(day.join("runall.sh")).unwrap();
    assert!(
        script.contains("# replaces coords.tsv\nsh -c ") && !script.contains("[ ! -e coords.tsv ]"),
        "{script}"
    );
    let index = std::fs::read_to_string(project.join("results/INDEX.tsv")).unwrap();
    assert!(
        index.contains("\tt\tfit\tcoords.tsv\ta model fit"),
        "{index}"
    );
    // One that exits well without its output failed, as any step.
    let gone = project.join("gone.tsv");
    let quiet = structured(&c.call(
        "fit",
        json!({ "out": gone, "seconds": 0, "write": false, "replace": true }),
    ))["job"]
        .as_str()
        .unwrap()
        .to_string();
    let w = c.call("job_wait", json!({ "id": quiet, "seconds": 10 }));
    assert_eq!(structured(&w)["state"], "failed", "{w}");
    // So did one that left the output it replaces as it was.
    let stale = structured(&c.call(
        "fit",
        json!({ "out": coords, "seconds": 0, "write": false, "replace": true }),
    ))["job"]
        .as_str()
        .unwrap()
        .to_string();
    let w = c.call("job_wait", json!({ "id": stale, "seconds": 10 }));
    assert_eq!(structured(&w)["state"], "failed", "{w}");
    assert!(w.to_string().contains("without rewriting"), "{w}");

    // A person following sees each job as a call of its own.
    drop(c);
    server.join().unwrap();
    let events: Vec<Body> = watch::follow(&dir, From::Start)
        .unwrap()
        .map(|e| e.body)
        .collect();
    let has = |want: &dyn Fn(&Body) -> bool| events.iter().any(want);
    assert!(has(
        &|b| matches!(b, Body::Call { id, tool, .. } if id == "job-1" && tool == "fit (job)")
    ));
    assert!(has(
        &|b| matches!(b, Body::Progress { id, message } if id == "job-1" && message == "fitting")
    ));
    assert!(has(
        &|b| matches!(b, Body::Result { id, ok: true, .. } if id == "job-1")
    ));
    assert!(has(
        &|b| matches!(b, Body::Cancelled { id, by } if id == &other && by == "user")
    ));
}
