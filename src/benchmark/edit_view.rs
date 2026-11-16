//! [`super::edit`] on a terminal: the spec's rows on the left, what it
//! plans and its first refusal on the right, the keys at the foot, and a
//! diff to confirm before a save. Only key presses act.
//!
//! Keys: ↑↓ (j k), PgUp PgDn, Home End move; Enter edits the row's field
//! (a stage's or method's threads); t, m and o set a stage's or method's
//! threads, mem_gb and timeout; Space toggles a value on and off; a adds a
//! value, an axis or an excluded point, where the cursor is; d removes
//! one; s saves, after the diff (y saves); q quits, asking first when
//! there are changes.

use super::edit::{duration_text, hunks, Ask, DiffLine, Editor, Mark, Need, Row, Said, Want};
use super::tools;
use crate::watch::view::list;
use anyhow::Result;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, ListItem, ListState, Paragraph, StatefulWidget, Widget, Wrap,
};
use std::time::Duration;

/// The unchanged lines a diff shows around each change.
const CONTEXT: usize = 3;

/// What the screen is doing.
enum Mode {
    /// Moving over the rows.
    Browse,
    /// An answer being typed.
    Typing { ask: Ask, text: String },
    /// The diff of a save, waiting for y.
    Diff(Saving),
    /// Quit asked with changes unsaved.
    Leaving,
}

/// A save's diff as the screen shows it, made once when it is asked for.
struct Saving {
    /// How far it is scrolled, in lines.
    scroll: u16,
    lines: Vec<DiffLine>,
    /// The spec key of the file's spec, if there is one, and of this one.
    keys: (Option<String>, String),
}

/// What became of a key.
#[derive(Debug, PartialEq, Eq)]
pub enum Handled {
    /// The view used it, or it was not a press.
    Consumed,
    /// The person is done.
    Quit,
}

/// The editor on a screen.
pub struct EditView {
    ed: Editor,
    mode: Mode,
    list: ListState,
    /// What the last key did, said at the foot until the next key; red
    /// when it did nothing.
    said: Option<Said>,
    /// The last save's words, said when the editor closes.
    saved: Option<String>,
}

impl EditView {
    pub fn new(ed: Editor) -> Self {
        Self {
            ed,
            mode: Mode::Browse,
            list: ListState::default(),
            said: None,
            saved: None,
        }
    }

    pub fn editor(&self) -> &Editor {
        &self.ed
    }

    /// What to say once the editor is closed.
    pub fn farewell(&self) -> String {
        match (&self.saved, self.ed.changed()) {
            (_, true) => format!("{}: not saved", self.ed.path().display()),
            (Some(s), false) => s.clone(),
            (None, false) => format!("{}: unchanged", self.ed.path().display()),
        }
    }

    /// The view's answer to a key.
    pub fn key(&mut self, key: KeyEvent) -> Handled {
        if key.kind != KeyEventKind::Press {
            return Handled::Consumed;
        }
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        self.said = None;
        match std::mem::replace(&mut self.mode, Mode::Browse) {
            Mode::Typing { ask, mut text } => match key.code {
                KeyCode::Enter => self.said = Some(self.ed.answer(&ask, &text)),
                KeyCode::Esc => {}
                _ if ctrl_c => {}
                KeyCode::Backspace => {
                    text.pop();
                    self.mode = Mode::Typing { ask, text };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text.push(c);
                    self.mode = Mode::Typing { ask, text };
                }
                _ => self.mode = Mode::Typing { ask, text },
            },
            Mode::Diff(mut d) => {
                let step = match key.code {
                    KeyCode::Up | KeyCode::Char('k') => -1,
                    KeyCode::Down | KeyCode::Char('j') => 1,
                    KeyCode::PageUp => -10,
                    KeyCode::PageDown => 10,
                    KeyCode::Char('y') => {
                        let said = self.ed.save();
                        if let Ok(s) = &said {
                            self.saved = Some(s.clone());
                        }
                        self.said = Some(said);
                        return Handled::Consumed;
                    }
                    _ => {
                        self.said = Some(Err("not saved".into()));
                        return Handled::Consumed;
                    }
                };
                d.scroll = d.scroll.saturating_add_signed(step);
                self.mode = Mode::Diff(d);
            }
            Mode::Leaving => match key.code {
                KeyCode::Char('y') => return Handled::Quit,
                KeyCode::Char('s') => self.mode = self.diff(),
                _ => {}
            },
            Mode::Browse => return self.browse(key, ctrl_c),
        }
        Handled::Consumed
    }

    /// A key while moving over the rows.
    fn browse(&mut self, key: KeyEvent, ctrl_c: bool) -> Handled {
        let ask = |v: &mut Self, want| match v.ed.ask(want) {
            Some((ask, text)) => v.mode = Mode::Typing { ask, text },
            None => v.said = Some(Err("nothing to set here".into())),
        };
        match key.code {
            _ if ctrl_c => return self.quit(),
            KeyCode::Up | KeyCode::Char('k') => self.ed.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.ed.move_by(1),
            KeyCode::PageUp => self.ed.move_by(-10),
            KeyCode::PageDown => self.ed.move_by(10),
            KeyCode::Home => self.ed.move_to(0),
            KeyCode::End => self.ed.move_to(usize::MAX),
            KeyCode::Enter | KeyCode::Char('e') => ask(self, Want::Edit),
            KeyCode::Char('t') => ask(self, Want::Set(Need::Threads)),
            KeyCode::Char('m') => ask(self, Want::Set(Need::MemGb)),
            KeyCode::Char('o') => ask(self, Want::Set(Need::Timeout)),
            KeyCode::Char('a') => ask(self, Want::Add),
            KeyCode::Char(' ') => self.said = Some(self.ed.toggle()),
            KeyCode::Char('d') | KeyCode::Delete => self.said = Some(self.ed.remove()),
            KeyCode::Char('s') if self.ed.changed() => self.mode = self.diff(),
            KeyCode::Char('s') => self.said = Some(Ok("nothing to save".into())),
            KeyCode::Char('q') | KeyCode::Esc => return self.quit(),
            _ => {}
        }
        Handled::Consumed
    }

    /// The diff of a save, at its top.
    fn diff(&self) -> Mode {
        Mode::Diff(Saving {
            scroll: 0,
            lines: self.ed.diff(),
            keys: self.ed.keys(),
        })
    }

    /// Quit, or ask first when there are changes.
    fn quit(&mut self) -> Handled {
        if self.ed.changed() {
            self.mode = Mode::Leaving;
            Handled::Consumed
        } else {
            Handled::Quit
        }
    }

    /// Draw the view in `area`.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let [head, body, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .areas(area);
        self.render_head(head, buf);
        if let Mode::Diff(d) = &self.mode {
            self.render_diff(body, buf, d);
        } else {
            let [rows, side] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(body);
            self.render_rows(rows, buf);
            let [plan, check] =
                Layout::vertical([Constraint::Min(6), Constraint::Length(6)]).areas(side);
            self.render_plan(plan, buf);
            self.render_check(check, buf);
        }
        self.render_foot(foot, buf);
    }

    fn render_head(&self, area: Rect, buf: &mut Buffer) {
        let state = if self.ed.is_new() {
            Span::styled("new file", Style::new().yellow())
        } else if self.ed.changed() {
            Span::styled("changed", Style::new().yellow())
        } else {
            Span::styled("saved", Style::new().green())
        };
        Line::from(vec![
            Span::from("benchmark spec ").bold(),
            Span::from(self.ed.path().display().to_string()),
            Span::from(" · "),
            state,
        ])
        .render(area, buf);
    }

    fn render_rows(&mut self, area: Rect, buf: &mut Buffer) {
        let refused = self.ed.refused_row();
        let items: Vec<ListItem> = self
            .ed
            .rows()
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let l = self.ed.line(r);
                let mark = if refused == Some(i) { "! " } else { "  " };
                let text = format!("{mark}{}{}", "  ".repeat(l.depth.into()), l.text);
                let style = if refused == Some(i) {
                    Style::new().red()
                } else if l.off {
                    Style::new().dim()
                } else if l.head {
                    Style::new().bold()
                } else {
                    Style::new()
                };
                ListItem::new(Line::styled(text, style))
            })
            .collect();
        self.list.select(Some(self.ed.cursor()));
        StatefulWidget::render(list(items, " spec ".into()), area, buf, &mut self.list);
    }

    fn render_plan(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::new().borders(Borders::ALL);
        let Some((c, now)) = self.ed.counts() else {
            Paragraph::new("no counts: the spec cannot be read (see below)")
                .wrap(Wrap { trim: true })
                .block(block.title(" plans "))
                .render(area, buf);
            return;
        };
        let block = if now {
            block.title(" plans ")
        } else {
            block.title(Line::styled(
                " plans, as last read: the spec cannot be read now ",
                Style::new().yellow(),
            ))
        };
        let excluded = c.grid - c.points;
        let mut lines = vec![
            Line::from(vec![
                Span::from(format!("tasks {}", c.tasks)).bold(),
                Span::from(format!("  spec {}", tools::short(&c.key))).dim(),
            ]),
            Line::from(format!(
                "points {} ({excluded} excluded) x reps {}",
                c.points, c.reps
            )),
        ];
        let width = c.methods.iter().map(|m| m.name.len()).max().unwrap_or(0);
        for m in &c.methods {
            lines.push(Line::from(format!(
                " {:<width$} {:>5}  {}t {}G {}",
                m.name,
                m.tasks,
                m.threads,
                m.mem_gb,
                duration_text(m.timeout_secs)
            )));
        }
        let cores = c
            .cores
            .map_or("the machine's".to_string(), |n| n.to_string());
        let mem = c
            .mem_gb
            .map_or("MemAvailable".to_string(), |m| format!("{m} GB"));
        lines.push(Line::from(format!("books cores {cores}, memory {mem}")));
        lines.push(Line::from(format!(
            "at most {:.0} core-hours (each to its timeout)",
            c.core_hours
        )));
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block)
            .render(area, buf);
    }

    fn render_check(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::new().borders(Borders::ALL).title(" check ");
        let line = match &self.ed.report().refusal {
            Some(r) => Line::styled(format!("refused: {r}"), Style::new().red()),
            None => Line::styled(
                "plans; its tools are found where it runs",
                Style::new().green(),
            ),
        };
        Paragraph::new(line)
            .wrap(Wrap { trim: true })
            .block(block)
            .render(area, buf);
    }

    fn render_diff(&self, area: Rect, buf: &mut Buffer, d: &Saving) {
        let block = Block::new().borders(Borders::ALL).title(format!(
            " save {}? y saves · any other key goes back ",
            self.ed.path().display()
        ));
        let (old, new) = &d.keys;
        let old = old.as_deref().map_or("none (a new file)", tools::short);
        let mut lines = vec![Line::from(format!("spec {old} -> {}", tools::short(new))).bold()];
        if self.ed.relaid() {
            lines.push(
                Line::from("the file is written again as pretty JSON, its keys sorted").dim(),
            );
        }
        if let Some(r) = &self.ed.report().refusal {
            lines.push(Line::styled(
                format!("refused as it is: {r}; y saves it still"),
                Style::new().red(),
            ));
        }
        for l in hunks(&d.lines, CONTEXT) {
            lines.push(match l {
                None => Line::from("  …").dim(),
                Some(l) => match l.mark {
                    Mark::Same => Line::from(format!("  {}", l.text)).dim(),
                    Mark::Gone => Line::styled(format!("- {}", l.text), Style::new().red()),
                    Mark::New => Line::styled(format!("+ {}", l.text), Style::new().green()),
                },
            });
        }
        Paragraph::new(lines)
            .scroll((d.scroll, 0))
            .block(block)
            .render(area, buf);
    }

    fn render_foot(&self, area: Rect, buf: &mut Buffer) {
        let [first, second] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        let line = match (&self.mode, &self.said) {
            (Mode::Typing { ask, text }, _) => Line::from(vec![
                Span::from(format!("{}: ", self.ed.prompt(ask))).bold(),
                Span::from(format!("{text}▏")),
                Span::from("  Enter sets · Esc drops").dim(),
            ]),
            (Mode::Leaving, _) => Line::styled(
                "unsaved changes: y quits without saving · s saves · any other key stays",
                Style::new().yellow(),
            ),
            (Mode::Diff(_), _) => Line::from("↑↓ scroll · y saves · any other key goes back").dim(),
            (Mode::Browse, Some(Ok(s))) => Line::from(s.as_str()),
            (Mode::Browse, Some(Err(s))) => Line::styled(s.as_str(), Style::new().red()),
            (Mode::Browse, None) => Line::from(self.row_keys()).dim(),
        };
        line.render(first, buf);
        Line::from(
            "s save · q quit · run and makes lists stay as the file has them: edit them there",
        )
        .dim()
        .render(second, buf);
    }

    /// The keys that act on the cursor's row.
    fn row_keys(&self) -> String {
        let keys = match self.ed.row() {
            Some(Row::Scalar(_)) => "Enter sets",
            Some(Row::Axes) => "a adds an axis",
            Some(Row::Axis(_)) => "a adds values · d removes the axis",
            Some(Row::Value { .. }) => "Space on/off · a adds values · d removes it",
            Some(Row::Exclude) => "a excludes a point",
            Some(Row::Excluded(_)) => "a excludes a point · d removes it",
            Some(Row::Stage(_) | Row::Method { .. }) => "t threads · m mem_gb · o timeout",
            Some(Row::Stages) | None => "",
        };
        format!("↑↓ move · {keys}")
    }
}

/// The editor on this terminal until the person quits; what to say then.
/// It runs plain, in this terminal: an editor's unsaved changes are not
/// left in a detached session.
pub fn run(ed: Editor) -> Result<String> {
    const TOOL: legume_tui::Tool = legume_tui::Tool::new("legume");
    let opts = legume_tui::Options {
        no_detach: true,
        target: true,
        ..Default::default()
    };
    let session = TOOL.session("edit", &opts)?;
    let mut terminal = session.terminal()?;
    let mut view = EditView::new(ed);
    loop {
        session.draw(&mut terminal, |f| view.render(f.area(), f.buffer_mut()))?;
        let round = session.poll(&mut terminal, Duration::from_millis(500))?;
        match round {
            legume_tui::Round::Stopped => break,
            legume_tui::Round::Event(TermEvent::Key(key)) if view.key(key) == Handled::Quit => {
                break
            }
            _ => {}
        }
    }
    drop(terminal);
    session.restore();
    Ok(view.farewell())
}
