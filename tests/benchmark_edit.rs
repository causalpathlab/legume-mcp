//! `legume benchmark plan SPEC --edit` as a person sees it, on a test
//! terminal at 120x30 and at 80x24: the rows, the counts that follow every
//! key, a refusal in red at its field, the diff a save shows, and the
//! question before quitting with changes.

use legume_mcp::benchmark::edit::Editor;
use legume_mcp::benchmark::edit_view::{EditView, Handled};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::Color;
use ratatui::Terminal;
use std::path::PathBuf;

/// The view drawn on a `w` by `h` terminal.
fn draw(view: &mut EditView, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| view.render(f.area(), f.buffer_mut())).unwrap();
    t
}

/// The screen's text at 120x30.
fn screen(view: &mut EditView) -> String {
    draw(view, 120, 30).backend().to_string()
}

fn press(view: &mut EditView, code: KeyCode) -> Handled {
    view.key(KeyEvent::from(code))
}

fn chars(view: &mut EditView, s: &str) {
    for c in s.chars() {
        press(view, KeyCode::Char(c));
    }
}

/// The cursor moved down to the first row whose screen line has `text`.
fn down_to(view: &mut EditView, text: &str) {
    for _ in 0..40 {
        let ed = view.editor();
        let row = ed.row().unwrap();
        if ed.line(row).text.contains(text) {
            return;
        }
        press(view, KeyCode::Down);
    }
    panic!("no row has {text:?}");
}

/// A view on the file `B1.json`, not there yet, in a folder of its own.
fn new_view() -> (tempfile::TempDir, PathBuf, EditView) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("B1.json");
    let view = EditView::new(Editor::open(&p).unwrap());
    (d, p, view)
}

#[test]
fn a_new_spec_shows_its_rows_counts_and_keys() {
    let (_d, _p, mut view) = new_view();
    let s = screen(&mut view);
    for want in [
        "benchmark spec",
        "new file",
        "name        B1",
        "seeds n     3",
        "size  (2 values)",
        "[x] 100",
        "fit         threads -",
        "M1        threads -",
        "tasks 12",
        "points 2 (0 excluded) x reps 3",
        "sim        6  1t 0G 4h",
        "fit-M1     6  1t 0G 4h",
        "books cores the machine's, memory MemAvailable",
        "at most 48 core-hours",
        "plans; its tools are found where it runs",
        "↑↓ move · Enter sets",
        "run and makes lists stay as the file has them",
    ] {
        assert!(s.contains(want), "{want:?} in\n{s}");
    }
    // Narrow, the same counts and the check still show.
    let small = draw(&mut view, 80, 24).backend().to_string();
    for want in [
        "tasks 12",
        "points 2 (0 excluded)",
        "fit-M1",
        "plans; its",
        "s save · q quit",
    ] {
        assert!(small.contains(want), "{want:?} in\n{small}");
    }
}

// Each key changes the counts at once; a refusal shows in red, with its
// field, and its row is marked.
#[test]
fn counts_and_refusals_follow_every_key() {
    let (_d, _p, mut view) = new_view();
    down_to(&mut view, "[x] 100");
    press(&mut view, KeyCode::Char(' '));
    let s = screen(&mut view);
    assert!(s.contains("[ ] 100") && s.contains("size=100 off"), "{s}");
    assert!(
        s.contains("tasks 6") && s.contains("points 1 (1 excluded)"),
        "{s}"
    );
    // A new value, typed.
    press(&mut view, KeyCode::Char('a'));
    chars(&mut view, "5000");
    assert!(screen(&mut view).contains("values to add to axis size, space between: 5000"));
    press(&mut view, KeyCode::Enter);
    assert!(screen(&mut view).contains("tasks 12"));
    // A stage's threads at 0 is refused, at its field.
    down_to(&mut view, "sim");
    press(&mut view, KeyCode::Char('t'));
    chars(&mut view, "0");
    press(&mut view, KeyCode::Enter);
    let t = draw(&mut view, 120, 30);
    let s = t.backend().to_string();
    assert!(s.contains("refused: stages[0].threads: is 0;"), "{s}");
    // The counts stay, as last read, while the spec cannot be read.
    assert!(
        s.contains("plans, as last read") && s.contains("tasks 12"),
        "{s}"
    );
    assert!(s.contains("! "), "{s}");
    let buf = t.backend().buffer();
    let (x, y) = (0..buf.area.height)
        .find_map(|y| {
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            line.find("refused:").map(|i| {
                let x = line[..i].chars().count() as u16;
                (x, y)
            })
        })
        .unwrap();
    assert_eq!(buf[(x, y)].fg, Color::Red);
    // At 80x24 the refusal still shows, wrapped.
    let small = draw(&mut view, 80, 24).backend().to_string();
    assert!(small.contains("refused: stages[0].threads"), "{small}");
}

// A save shows the diff first and writes on y; any other key goes back.
#[test]
fn a_save_shows_its_diff_and_waits_for_y() {
    let (_d, p, mut view) = new_view();
    press(&mut view, KeyCode::Char('s'));
    let s = screen(&mut view);
    assert!(s.contains("spec none (a new file) -> "), "{s}");
    assert!(s.contains("+   \"name\": \"B1\","), "{s}");
    press(&mut view, KeyCode::Char('n'));
    assert!(screen(&mut view).contains("not saved"));
    assert!(!p.exists());
    press(&mut view, KeyCode::Char('s'));
    press(&mut view, KeyCode::Char('y'));
    let s = screen(&mut view);
    assert!(s.contains(&format!("saved {}", p.display())), "{s}");
    assert!(s.contains("· saved"), "{s}");
    assert!(p.exists());
    // Only the changed lines and their context, after a change.
    down_to(&mut view, "seeds base");
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Backspace);
    chars(&mut view, "9");
    press(&mut view, KeyCode::Enter);
    press(&mut view, KeyCode::Char('s'));
    let s = screen(&mut view);
    assert!(
        s.contains("-     \"base\": 0,") && s.contains("+     \"base\": 9,"),
        "{s}"
    );
    assert!(s.contains("…"), "{s}");
    press(&mut view, KeyCode::Char('y'));
    let back = std::fs::read_to_string(&p).unwrap();
    assert!(back.contains("\"base\": 9"));
    assert!(view.farewell().starts_with("saved "));
}

#[test]
fn quitting_with_changes_asks_first() {
    let (_d, p, mut view) = new_view();
    assert_eq!(press(&mut view, KeyCode::Char('q')), Handled::Consumed);
    assert!(screen(&mut view).contains("unsaved changes: y quits without saving"));
    assert_eq!(press(&mut view, KeyCode::Esc), Handled::Consumed);
    assert!(!screen(&mut view).contains("unsaved changes"));
    press(&mut view, KeyCode::Char('q'));
    assert_eq!(press(&mut view, KeyCode::Char('y')), Handled::Quit);
    assert_eq!(view.farewell(), format!("{}: not saved", p.display()));
    // With nothing changed, q quits at once.
    std::fs::write(&p, "{\"name\": \"B1\"}").unwrap();
    let mut view = EditView::new(Editor::open(&p).unwrap());
    let s = screen(&mut view);
    assert!(s.contains("refused: seeds: is required"), "{s}");
    assert_eq!(press(&mut view, KeyCode::Char('q')), Handled::Quit);
    assert_eq!(view.farewell(), format!("{}: unchanged", p.display()));
}
