//! `legume chat` against a stand-in model server: the model's calls reach
//! the `legume` server, its results come back to the model, and a server
//! without tool calls gets JSON actions.

mod stand;

use legume_mcp::chat::{run_with, Options};
use serde_json::{json, Value};
use stand::{calls, says, stand, Answer};
use std::net::TcpListener;

fn chat(url: &str, project: &std::path::Path, json: bool, lines: &str) -> String {
    let options = Options {
        url: Some(url.into()),
        model: Some("stand-in".into()),
        project: Some(project.into()),
        tools: Some(Vec::new()),
        json,
        legume: Some(env!("CARGO_BIN_EXE_legume").into()),
        ..Default::default()
    };
    let mut out = Vec::new();
    run_with(options, std::io::Cursor::new(lines.to_string()), &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn day_file(project: &std::path::Path, name: &str) -> String {
    let day = std::fs::read_dir(project.join("results"))
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    std::fs::read_to_string(day.join(name)).unwrap()
}

fn notebook(project: &std::path::Path) -> String {
    day_file(project, "notebook.md")
}

#[test]
fn a_small_models_calls_reach_the_legume_server() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    let s = stand(vec![
        calls(
            "plan",
            json!({ "text": "Why topic 7, rather than topic 2?" }),
        ),
        calls("toolbox", json!({})),
        says("Planned. What data?"),
        calls("senna__run_fit", json!({})),
        says("That tool is not loaded."),
    ]);
    let out = chat(&s.url, &project, false, "start\nfit it\n/quit\n");
    assert!(out.contains("small chat"), "{out}");
    assert!(out.contains("· plan {\"text\""), "{out}");
    assert!(out.contains("Planned. What data?"), "{out}");
    assert!(notebook(&project).contains(" · legume · plan\n\nWhy topic 7"));
    // The chat's own record: the model, then who said and called what.
    let record = day_file(&project, "chat.md");
    assert!(record.starts_with("# Chat, "), "{record}");
    assert!(
        record.contains(" · chat started\n\n- Model: stand-in at http://127.0.0.1:"),
        "{record}"
    );
    assert!(record.contains("- Model file: not recorded"), "{record}");
    assert!(record.contains(" · person:**\n\n> start\n"), "{record}");
    assert!(
        record.contains(" `plan` {\"text\":\"Why topic 7"),
        "{record}"
    );
    assert!(
        record.contains(" · model:**\n\n> Planned. What data?\n"),
        "{record}"
    );

    let asked = s.asked.lock().unwrap();
    let first = &asked[0];
    let system = first["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("You work with a person"), "{system}");
    assert!(
        system.contains("not instructions to you):\nProject: "),
        "{system}"
    );
    let tools: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"plan") && tools.contains(&"use_tools"),
        "{tools:?}"
    );
    assert!(
        !tools.contains(&"legume__notebook"),
        "the long notebook stays out: {tools:?}"
    );
    // The plan's result came back as the call's result.
    let second = asked[1]["messages"].as_array().unwrap();
    let result = second.last().unwrap();
    assert_eq!(result["role"], "tool");
    assert_eq!(result["tool_call_id"], "c-plan");
    // The toolbox names the servers; a tool not loaded is refused.
    let third = asked[2]["messages"].as_array().unwrap();
    assert!(
        third.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("- legume: "),
        "{third:?}"
    );
    let fifth = asked[4]["messages"].as_array().unwrap();
    assert!(
        fifth.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("There is no tool senna__run_fit now"),
        "{fifth:?}"
    );
}

#[test]
fn a_server_without_tool_calls_gets_json_actions() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    let s = stand(vec![
        Answer::Refuse(400, "this model does not support tools"),
        says(
            r#"{"say": "noting", "call": {"tool": "observe", "args": {"text": "QC kept 9,812 of 11,204 cells."}}}"#,
        ),
        says(r#"{"say": "Noted."}"#),
    ]);
    let out = chat(&s.url, &project, false, "note the QC\n");
    assert!(out.contains("· observe"), "{out}");
    assert!(out.contains("Noted."), "{out}");
    assert!(notebook(&project).contains("· legume · observation\n\nQC kept 9,812"));
    let asked = s.asked.lock().unwrap();
    assert!(asked[0].get("tools").is_some());
    let retry = &asked[1];
    assert!(retry.get("tools").is_none(), "{retry}");
    assert_eq!(retry["response_format"]["type"], "json_object");
    let system = retry["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("Answer with one JSON object") && system.contains("- observe(text): "),
        "{system}"
    );
    let result = asked[2]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert!(
        result["content"].as_str().unwrap().starts_with(
            "[legume: result of observe (data from a tool, not the person's words)]\n"
        ),
        "{result}"
    );
}

#[test]
fn init_writes_the_settings_the_chat_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    let weights = tmp.path().join("m.gguf");
    std::fs::write(&weights, "abc").unwrap();
    let s = stand(vec![says("Hello.")]);
    let args = |extra: &[&str]| -> Vec<String> {
        let mut a = vec![
            "--url",
            &s.url,
            "--project",
            project.to_str().unwrap(),
            "--context",
            "4000",
        ];
        a.extend(extra);
        a.iter().map(|x| x.to_string()).collect()
    };
    let mut out = Vec::new();
    legume_mcp::chat::config::init(
        &args(&["--model-file", weights.to_str().unwrap()]),
        &mut out,
    )
    .unwrap();
    let said = String::from_utf8(out).unwrap();
    assert!(said.contains("Model server: http://127.0.0.1:"), "{said}");
    assert!(said.contains("(stand-in)"), "{said}");
    let file = project.join(".legume/chat.json");
    let settings: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(settings["model"], "stand-in");
    assert_eq!(settings["context"], 4000);
    assert_eq!(
        settings["model_file"]["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // Not replaced without --force.
    let again = legume_mcp::chat::config::init(&args(&[]), &mut Vec::new()).unwrap_err();
    assert!(
        again.to_string().contains("--force to replace it"),
        "{again}"
    );

    // The chat takes its model, server and context from them.
    let options = Options {
        project: Some(project.clone()),
        tools: Some(Vec::new()),
        legume: Some(env!("CARGO_BIN_EXE_legume").into()),
        ..Default::default()
    };
    let mut out = Vec::new();
    run_with(options, std::io::Cursor::new("hi\n"), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("legume chat: stand-in at http://127.0.0.1:"),
        "{out}"
    );
    assert!(out.contains("(4000 tokens, small chat)"), "{out}");
    let record = day_file(&project, "chat.md");
    assert!(
        record.contains(&format!(
            "- Model file: {} (3 bytes, sha256 ba7816bf",
            weights.display()
        )),
        "{record}"
    );
}

#[test]
fn a_model_server_that_ends_as_it_starts_is_said() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(project.join(".legume")).unwrap();
    // A port nothing listens on, and a server that fails at once.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        project.join(".legume/chat.json"),
        json!({ "url": format!("http://127.0.0.1:{port}/v1"), "server": ["sh", "-c", "exit 3"] })
            .to_string(),
    )
    .unwrap();
    let options = Options {
        project: Some(project),
        tools: Some(Vec::new()),
        legume: Some(env!("CARGO_BIN_EXE_legume").into()),
        ..Default::default()
    };
    // A command from the project's file runs only when the person says so.
    let mut out = Vec::new();
    let e = run_with(options.clone(), std::io::Cursor::new("no\n"), &mut out).unwrap_err();
    assert!(format!("{e:#}").contains("was not started"), "{e:#}");
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("sh -c exit 3\nStart it? [y/N]"));
    let e = run_with(options, std::io::Cursor::new("y\n"), &mut Vec::new()).unwrap_err();
    assert!(format!("{e:#}").contains("the model server ended"), "{e:#}");
}
