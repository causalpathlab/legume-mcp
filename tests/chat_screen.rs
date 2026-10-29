//! `legume chat` on its screen, in a pseudo-terminal: the person's line
//! reaches the model, its calls and answer show beside the records, and
//! the chat goes on while the person is away (detached) until they come
//! back (`legume attach`) and leave.

#![cfg(unix)]

mod stand;

use legume_tui::testing::Private;
use legume_tui::Size;
use serde_json::json;
use stand::{calls, says, stand};

const SIZE: Size = Size {
    rows: 30,
    cols: 120,
    width: 1200,
    height: 600,
};

#[test]
fn the_chat_runs_on_its_screen_and_waits_for_the_person_to_come_back() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    let s = stand(vec![
        calls(
            "plan",
            json!({ "text": "Why topic 7, rather than topic 2?" }),
        ),
        says("Planned. What data?"),
    ]);
    let dir = Private::new(env!("CARGO_BIN_EXE_legume"));
    let args = [
        "chat",
        "--url",
        &s.url,
        "--model",
        "stand-in",
        "--project",
        project.to_str().unwrap(),
        "--tools",
        "",
    ];
    let mut t = dir.term_sized(&args, &[], SIZE);
    t.wait_screen("legume chat · stand-in · 8192 tokens · small chat");
    t.wait_screen("No steps yet");
    t.type_(b"start\r");
    t.wait_screen("you  start");
    t.wait_screen("Planned. What data?");
    t.wait_screen("· plan {\"text\":\"Why topic 7");
    t.wait_screen("  → Done.");
    // The plan is in today's notebook, beside the chat.
    t.type_(b"\t");
    t.wait_screen(" legume plan");
    t.wait_screen("   Why topic 7, rather than topic 2?");

    // Away: the chat goes on in its session.
    t.type_(b"\x1b\x04");
    t.answer(1, b"\r");
    assert_eq!(t.exit_code(), 0);
    dir.wait_listed("idle");

    // Back: as it was; Ctrl-D leaves, and the tools' servers end with it.
    let mut back = dir.term_sized(&["attach"], &[], SIZE);
    back.wait_screen("Planned. What data?");
    back.type_(b"\x04");
    assert_eq!(back.exit_code(), 0);
    let record = std::fs::read_dir(project.join("results"))
        .unwrap()
        .flatten()
        .find(|e| e.path().is_dir())
        .map(|d| std::fs::read_to_string(d.path().join("chat.md")).unwrap())
        .unwrap();
    assert!(record.contains("> start\n"), "{record}");
    assert!(record.contains("> Planned. What data?\n"), "{record}");
}
