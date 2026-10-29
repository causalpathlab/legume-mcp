//! `legume chat` on a screen of its own: the conversation beside the
//! project's records, in a session of `legume`'s that the person can leave
//! running (a long step, a question waiting for them) and come back to
//! with `legume attach`.
//!
//! ```text
//!  legume chat · model · 8192 tokens · small chat · project      ⚑ 1  H 2  working 12s
//! ┌ chat ───────────────────────────────┐┌ steps · notebook · hypotheses ┐
//! │ you  why is cluster 4 apart?        ││ 10:02 data-beans qc → a.zarr  │
//! │ · plan {"question": …}              ││   ← data/a.h5                 │
//! │   → Recorded the plan.              ││   cells 9812 of 11204         │
//! │ …                                   ││   ⚑ mito 0.3 (flagged: high)  │
//! └─────────────────────────────────────┘└───────────────────────────────┘
//!  › what the person types▏
//!  Enter sends · Tab records · PgUp/PgDn scroll · Ctrl-C stops · Ctrl-D leaves
//! ```
//!
//! The chat itself runs on a thread of its own, as it does on lines: the
//! screen hands it the person's lines and their answers to its questions,
//! and draws what it says. The records are read from the files every few
//! seconds and after each turn, so a step another client ran shows too.

use super::show::{Said, Show};
use super::{model::Mode, Chat, Options, STOP, USAGE_SHORT};
use crate::records::{Entry, Records};
use anyhow::Result;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// `legume`'s sessions: `legume-chat-1`, ...
pub const TOOL: legume_tui::Tool = legume_tui::Tool::new("legume");

/// How often the records are read again while nothing else happens.
const REREAD: Duration = Duration::from_secs(3);
/// A terminal at least this wide shows the records beside the chat; a
/// narrower one, under it.
const SIDE_BY_SIDE: u16 = 110;

/// What the chat's thread tells the screen.
enum Event {
    Said(Said),
    /// A question for the person, and the replies it takes.
    Ask(String, String),
    /// The person's line is done with.
    Idle,
    /// The chat has ended: its servers are stopped.
    Ended,
}

/// The chat's [`Show`] on the screen: what it says sent over, a question
/// answered by the person's key.
struct Remote {
    tx: Sender<Event>,
    answers: Receiver<String>,
}

impl Show for Remote {
    fn say(&mut self, said: Said) -> std::io::Result<()> {
        let _ = self.tx.send(Event::Said(said));
        Ok(())
    }

    fn ask(&mut self, question: &str, choices: &str) -> Option<String> {
        self.tx
            .send(Event::Ask(question.into(), choices.into()))
            .ok()?;
        self.answers.recv().ok()
    }
}

/// The chat on its screen, in a session (`no_detach`: in this terminal
/// only). It is set up as lines first, so a question it asks (may it
/// start the model server?) is asked as on lines.
pub fn run(options: Options, no_detach: bool) -> Result<()> {
    // As on lines: the model server and the tools' servers end with the
    // chat, whatever ends it. On the screen Ctrl-C is a key.
    super::signals();
    let session = TOOL.session(
        "chat",
        &legume_tui::Options {
            no_detach,
            ..Default::default()
        },
    )?;
    let chat = {
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();
        super::setup(options, &mut lines, &mut std::io::stdout())?
    };
    let mut screen = Screen::new(headline(&chat), chat.project.clone());
    let mut terminal = session.terminal()?;
    let (to_chat, heard) = channel::<String>();
    let (answer, answers) = channel::<String>();
    let (tx, events) = channel::<Event>();
    let worker = std::thread::spawn(move || {
        let mut chat = chat;
        let mut show = Remote { tx, answers };
        for line in heard {
            let _ = chat.hear(&line, &mut show);
            let _ = show.tx.send(Event::Idle);
        }
        // The servers stopped, as the chat ends.
        drop(chat);
        let _ = show.tx.send(Event::Ended);
    });
    let mut to_chat = Some(to_chat);
    let mut answer = Some(answer);
    let mut dirty = true;
    loop {
        while let Ok(e) = events.try_recv() {
            dirty = true;
            screen.take(e);
        }
        if screen.ended || worker.is_finished() {
            break;
        }
        if screen.read_at.elapsed() >= REREAD {
            screen.reread();
            dirty = true;
        }
        session.status(&screen.status());
        if dirty || screen.busy {
            session.draw(&mut terminal, |f| screen.render(f.area(), f.buffer_mut()))?;
        }
        let wait = if screen.busy {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(500)
        };
        let round = session.poll(&mut terminal, wait)?;
        dirty = !matches!(round, legume_tui::Round::Timeout);
        let act = match round {
            legume_tui::Round::Stopped => Act::Leave,
            legume_tui::Round::Event(TermEvent::Key(k)) => screen.key(k),
            legume_tui::Round::Event(TermEvent::Paste(text)) => {
                screen.paste(&text);
                Act::Nothing
            }
            _ => Act::Nothing,
        };
        match act {
            Act::Nothing => {}
            Act::Send(line) => match &to_chat {
                Some(c) if c.send(line).is_ok() => {}
                _ => break,
            },
            Act::Answer(a) => {
                if let Some(s) = &answer {
                    let _ = s.send(a);
                }
            }
            Act::Stop => STOP.store(true, Ordering::Relaxed),
            Act::Leave => {
                // The turn in progress stops, a question goes unanswered,
                // and the chat's thread ends with its servers.
                STOP.store(true, Ordering::Relaxed);
                to_chat = None;
                answer = None;
                screen.leaving = true;
            }
        }
    }
    let ended = screen.ended;
    drop(to_chat);
    drop(answer);
    let _ = worker.join();
    if !ended {
        session.leave("the chat ended unexpectedly; its record is in results/<today>/chat.md");
    }
    Ok(())
}

/// The model, as the status line names it.
fn headline(chat: &Chat) -> String {
    format!(
        "{} · {} tokens · {}{}",
        chat.model.name,
        chat.model.context,
        if chat.small {
            "small chat"
        } else {
            "full chat"
        },
        if chat.model.mode == Mode::Json {
            " · JSON actions"
        } else {
            ""
        }
    )
}

/// What a key asks the screen's owner to do.
#[derive(Debug, PartialEq)]
enum Act {
    Nothing,
    /// The person's line, for the chat.
    Send(String),
    /// The person's reply to the chat's question.
    Answer(String),
    /// Stop the work in progress.
    Stop,
    Leave,
}

/// The records' tabs.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Steps,
    Notebook,
    Hypotheses,
}

const TABS: [(Tab, &str); 3] = [
    (Tab::Steps, "steps"),
    (Tab::Notebook, "notebook"),
    (Tab::Hypotheses, "hypotheses"),
];

struct Screen {
    head: String,
    project: PathBuf,
    /// The conversation, as drawn.
    lines: Vec<Line<'static>>,
    /// Lines scrolled back from the end; 0 follows.
    back: usize,
    input: String,
    /// A question waiting for the person, and the replies it takes.
    asking: Option<(String, String)>,
    /// The chat is working on the person's line, since when.
    busy: bool,
    since: Instant,
    /// The running call's latest progress.
    progress: Option<String>,
    leaving: bool,
    ended: bool,
    records: Records,
    read_at: Instant,
    tab: Tab,
    /// A word for the person in the footer, until the next key.
    said: Option<String>,
}

impl Screen {
    fn new(head: String, project: PathBuf) -> Self {
        let mut s = Self {
            head,
            records: Records::today(&project),
            project,
            lines: Vec::new(),
            back: 0,
            input: String::new(),
            asking: None,
            busy: false,
            since: Instant::now(),
            progress: None,
            leaving: false,
            ended: false,
            read_at: Instant::now(),
            tab: Tab::Steps,
            said: None,
        };
        s.push(Line::from(USAGE_SHORT.to_string()).dim().italic());
        s
    }

    fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    /// `text`, one line a line, each line's spans made by `first` and the
    /// rest by `rest`.
    fn push_text(&mut self, text: &str, style: Style) {
        for l in text.lines() {
            self.push(Line::styled(l.to_string(), style));
        }
    }

    fn reread(&mut self) {
        self.records = Records::today(&self.project);
        self.read_at = Instant::now();
    }

    /// What the chat's thread said.
    fn take(&mut self, e: Event) {
        match e {
            Event::Said(s) => self.said_by_chat(s),
            Event::Ask(question, choices) => {
                self.push(Line::from(vec![
                    Span::from("? ").yellow().bold(),
                    Span::from(question.clone()).yellow(),
                ]));
                self.asking = Some((question, choices));
                self.back = 0;
            }
            Event::Idle => {
                self.busy = false;
                self.progress = None;
                self.reread();
            }
            Event::Ended => self.ended = true,
        }
    }

    fn said_by_chat(&mut self, s: Said) {
        match s {
            Said::Answer(t) => {
                self.push(Line::default());
                self.push_text(&t, Style::new());
                self.push(Line::default());
            }
            Said::Aside(t) => self.push_text(&t, Style::new().italic()),
            Said::Call(name, args) => {
                self.progress = None;
                self.push(Line::from(vec![
                    Span::from("· "),
                    Span::from(name).yellow().bold(),
                    Span::from(" "),
                    Span::from(super::servers::cut(&args, 300)).dim(),
                ]));
            }
            Said::Progress(m) => self.progress = Some(m),
            Said::Result(first) => {
                self.push(Line::from(vec![Span::from("  → ").green(), Span::from(first)]).dim());
                // A step may have written a record.
                self.reread();
            }
            Said::Chat(t) => self.push_text(&t, Style::new().dim().italic()),
        }
    }

    /// The session's line, for `legume sessions` and the picker.
    fn status(&self) -> String {
        if let Some((q, _)) = &self.asking {
            return format!("waiting for you: {q}");
        }
        if self.busy {
            return "working".into();
        }
        if self.records.waiting.is_some() {
            return "a question in the notebook waits for you".into();
        }
        "idle".into()
    }

    fn paste(&mut self, text: &str) {
        if self.asking.is_none() {
            // One line: the chat takes a line at a time.
            self.input
                .extend(text.chars().map(|c| if c.is_control() { ' ' } else { c }));
        }
    }

    fn key(&mut self, key: KeyEvent) -> Act {
        if key.kind != KeyEventKind::Press {
            return Act::Nothing;
        }
        self.said = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.asking.is_some() {
                    return self.answer("n");
                }
                if self.busy {
                    self.said = Some("stopping the work…".into());
                    return Act::Stop;
                }
                if self.input.is_empty() {
                    self.said = Some("nothing is running; Ctrl-D leaves".into());
                }
                self.input.clear();
                return Act::Nothing;
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => return Act::Leave,
            KeyCode::Tab => {
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                self.tab = TABS[(i + 1) % TABS.len()].0;
                return Act::Nothing;
            }
            KeyCode::BackTab => {
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                self.tab = TABS[(i + TABS.len() - 1) % TABS.len()].0;
                return Act::Nothing;
            }
            KeyCode::PageUp => {
                self.back += 10;
                return Act::Nothing;
            }
            KeyCode::PageDown => {
                self.back = self.back.saturating_sub(10);
                return Act::Nothing;
            }
            KeyCode::Up => {
                self.back += 1;
                return Act::Nothing;
            }
            KeyCode::Down => {
                self.back = self.back.saturating_sub(1);
                return Act::Nothing;
            }
            KeyCode::End => {
                self.back = 0;
                return Act::Nothing;
            }
            _ => {}
        }
        if self.asking.is_some() {
            return match key.code {
                KeyCode::Char(c @ ('y' | 'n' | 'Y' | 'N')) if !ctrl => {
                    self.answer(&c.to_ascii_lowercase().to_string())
                }
                // Only where the question offers it.
                KeyCode::Char('a' | 'A')
                    if !ctrl && self.asking.as_ref().is_some_and(|(_, c)| c.contains("[a]")) =>
                {
                    self.answer("a")
                }
                KeyCode::Esc => self.answer("n"),
                _ => Act::Nothing,
            };
        }
        match key.code {
            KeyCode::Enter => {
                let line = self.input.trim().to_string();
                if line.is_empty() {
                    return Act::Nothing;
                }
                if self.busy {
                    self.said = Some("the model is working: wait, or Ctrl-C stops it".into());
                    return Act::Nothing;
                }
                self.input.clear();
                if line == "/quit" || line == "/exit" {
                    return Act::Leave;
                }
                self.push(Line::from(vec![
                    Span::from("you  ").cyan().bold(),
                    Span::from(line.clone()).bold(),
                ]));
                self.busy = true;
                self.since = Instant::now();
                self.back = 0;
                Act::Send(line)
            }
            KeyCode::Backspace => {
                self.input.pop();
                Act::Nothing
            }
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                Act::Nothing
            }
            KeyCode::Char('w') if ctrl => {
                let kept = self.input.trim_end().rfind(' ').map_or(0, |i| i + 1);
                self.input.truncate(kept);
                Act::Nothing
            }
            KeyCode::Char(c) if !ctrl => {
                self.input.push(c);
                Act::Nothing
            }
            _ => Act::Nothing,
        }
    }

    /// The person's reply to the question, said in the conversation.
    fn answer(&mut self, a: &str) -> Act {
        self.asking = None;
        let word = match a {
            "y" => "yes",
            "a" => "always, for this tool",
            _ => "no",
        };
        self.push(Line::from(vec![
            Span::from("you  ").cyan().bold(),
            Span::from(word).bold(),
        ]));
        Act::Answer(a.to_string())
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let [top, body, input, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.render_top(top, buf);
        let [chat, records] = if body.width >= SIDE_BY_SIDE {
            Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(body)
        } else {
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body)
        };
        self.render_chat(chat, buf);
        self.render_records(records, buf);
        self.render_input(input, buf);
        self.render_foot(foot, buf);
    }

    fn render_top(&self, area: Rect, buf: &mut Buffer) {
        let project = self.project.file_name().map_or_else(
            || self.project.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        let mut right = vec![];
        let flagged = self.records.flagged().count();
        if flagged > 0 {
            right.push(Span::from(format!(" ⚑ {flagged} ")).red().bold());
        }
        if !self.records.hypotheses.is_empty() {
            right.push(Span::from(format!(" H {} ", self.records.hypotheses.len())).bold());
        }
        if self.records.waiting.is_some() {
            right.push(Span::from(" ? waits for you ").yellow().bold());
        }
        if self.leaving {
            right.push(Span::from(" ending: stopping the tools' servers… ").yellow());
        } else if self.busy {
            let s = self.since.elapsed().as_secs();
            right.push(Span::from(format!(" working {s}s ")).cyan().bold());
        }
        let width: usize = right.iter().map(Span::width).sum();
        let [left, rest] =
            Layout::horizontal([Constraint::Min(10), Constraint::Length(width as u16)]).areas(area);
        Line::from(vec![
            Span::from(" legume chat").bold(),
            Span::from(format!(" · {} · {project}", self.head)).dim(),
        ])
        .render(left, buf);
        Line::from(right).render(rest, buf);
    }

    fn render_chat(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::new().borders(Borders::ALL).title(" chat ");
        let inner = block.inner(area);
        block.render(area, buf);
        let mut lines = self.lines.clone();
        if self.busy {
            if let Some(p) = &self.progress {
                lines.push(Line::from(format!("  … {p}")).dim());
            }
        }
        let text = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = text.line_count(inner.width);
        let height = inner.height as usize;
        let most = total.saturating_sub(height);
        self.back = self.back.min(most);
        let top = most - self.back;
        text.scroll((top.min(u16::MAX as usize) as u16, 0))
            .render(inner, buf);
        if self.back > 0 {
            let note = format!(" {} more below · End follows ", self.back);
            let w = note.chars().count() as u16;
            if area.width > w + 2 {
                Line::from(note).reversed().render(
                    Rect::new(area.x + area.width - w - 1, area.y + area.height - 1, w, 1),
                    buf,
                );
            }
        }
    }

    fn render_records(&self, area: Rect, buf: &mut Buffer) {
        let mut title = vec![Span::from(" ")];
        for (i, (t, name)) in TABS.iter().enumerate() {
            if i > 0 {
                title.push(Span::from(" · ").dim());
            }
            let s = Span::from(*name);
            title.push(if *t == self.tab {
                s.bold().reversed()
            } else {
                s.dim()
            });
        }
        title.push(Span::from(" "));
        let block = Block::new().borders(Borders::ALL).title(Line::from(title));
        let inner = block.inner(area);
        block.render(area, buf);
        let (lines, empty) = match self.tab {
            Tab::Steps => (
                self.steps(),
                "No steps yet: the tools record each output here.",
            ),
            Tab::Notebook => (
                self.notebook(),
                "Nothing in today's notebook yet: the work starts with a plan.",
            ),
            Tab::Hypotheses => (
                self.hypotheses(),
                "No hypotheses yet: a reading becomes one when you choose it.",
            ),
        };
        if lines.is_empty() {
            Paragraph::new(empty)
                .dim()
                .wrap(Wrap { trim: true })
                .render(inner, buf);
            return;
        }
        // The newest at the bottom, as in the chat.
        let text = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total = text.line_count(inner.width);
        let top = total.saturating_sub(inner.height as usize);
        text.scroll((top.min(u16::MAX as usize) as u16, 0))
            .render(inner, buf);
    }

    /// The steps: each output, what it was made from and the numbers it
    /// came with, a flagged number marked.
    fn steps(&self) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for s in &self.records.steps {
            out.push(Line::from(vec![
                Span::from(clock(&s.time)).dim(),
                Span::from(format!(" {} {} ", s.server, s.tool)),
                Span::from(format!("→ {}", s.output)).bold(),
            ]));
            if !s.inputs.is_empty() {
                out.push(Line::from(format!("   ← {}", s.inputs.join(", "))).dim());
            }
            if !s.summary.is_empty() {
                out.push(Line::from(format!("   {}", s.summary)).dim());
            }
            for f in &s.facts {
                out.push(match f.flagged() {
                    Some(_) => Line::from(format!("   ⚑ {f}")).red(),
                    None => Line::from(format!("   {f}")),
                });
            }
        }
        out
    }

    /// Today's notebook, a line an entry, the question waiting first.
    fn notebook(&self) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for e in &self.records.notebook {
            out.push(Line::from(vec![
                Span::from(clock(&e.time)).dim(),
                Span::from(format!(" {} ", e.server)).dim(),
                Span::styled(e.kind.clone(), kind_style(e)),
            ]));
            if !e.first.is_empty() {
                out.push(Line::from(format!("   {}", e.first)));
            }
        }
        if let Some(q) = &self.records.waiting {
            out.push(Line::default());
            out.push(
                Line::from(format!("? Waiting for you: {q}"))
                    .yellow()
                    .bold(),
            );
        }
        out
    }

    /// The hypotheses: each source's word, how far they take the claim,
    /// and the kinds of source not yet used.
    fn hypotheses(&self) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for h in &self.records.hypotheses {
            let l = &h.ledger;
            out.push(Line::from(format!("H{}. {}", h.id, h.claim)).bold());
            let level = match l.level {
                0 => Style::new().dim(),
                1 => Style::new().yellow(),
                2 => Style::new().cyan(),
                _ => Style::new().green().bold(),
            };
            out.push(Line::from(vec![
                Span::styled(format!("   level {}/3", l.level), level),
                Span::from(format!(": {}", l.standing)).dim(),
            ]));
            for e in &l.sources {
                let (mark, style) = match e.verdict.as_str() {
                    "agrees" => ("✓", Style::new().green()),
                    "disagrees" => ("✗", Style::new().red()),
                    _ => ("~", Style::new().yellow()),
                };
                out.push(Line::from(vec![
                    Span::styled(format!("   {mark} "), style),
                    Span::from(e.source.clone()),
                    Span::from(format!(" ({})", e.class)).dim(),
                    Span::from(if e.summary.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", e.summary)
                    })
                    .dim(),
                ]));
            }
            if !l.unused.is_empty() {
                out.push(Line::from(format!("   not yet: {}", l.unused.join(", "))).dim());
            }
        }
        out
    }

    fn render_input(&self, area: Rect, buf: &mut Buffer) {
        let line = match &self.asking {
            Some((_, choices)) => Line::from(vec![
                Span::from(" ? ").yellow().bold(),
                Span::from(format!("{choices}: press y, n or a")).yellow(),
            ]),
            None => {
                // The end of a long line, where the typing is.
                let room = area.width.saturating_sub(4) as usize;
                let n = self.input.chars().count();
                let shown: String = self.input.chars().skip(n.saturating_sub(room)).collect();
                let prompt = if self.busy {
                    Span::from(" › ").dim()
                } else {
                    Span::from(" › ").cyan().bold()
                };
                Line::from(vec![prompt, Span::from(shown), Span::from("▏")])
            }
        };
        line.render(area, buf);
    }

    fn render_foot(&self, area: Rect, buf: &mut Buffer) {
        if let Some(s) = &self.said {
            Line::from(format!(" {s}")).render(area, buf);
            return;
        }
        let mut keys: Vec<String> = if self.asking.is_some() {
            vec![
                "y runs it".into(),
                "n does not".into(),
                "a runs this tool from now on".into(),
            ]
        } else if self.busy {
            vec!["Ctrl-C stops the work".into()]
        } else {
            vec!["Enter sends".into()]
        };
        keys.extend([
            "Tab records".into(),
            "PgUp/PgDn scroll".into(),
            "Ctrl-D leaves".into(),
        ]);
        keys.extend(
            legume_tui::help_keys()
                .into_iter()
                .map(|k| format!("{} {}", k.key, k.short)),
        );
        Line::from(format!(" {}", keys.join(" · ")))
            .dim()
            .render(area, buf);
    }
}

/// `10:02:03` as `10:02`.
fn clock(time: &str) -> String {
    time.get(..5).unwrap_or(time).to_string()
}

/// A notebook entry's colour, by its kind: what waits for the person
/// stands out, what they decided too.
fn kind_style(e: &Entry) -> Style {
    let k = e.kind.as_str();
    if k.contains("proposed") || k == "offer" {
        Style::new().yellow().bold()
    } else if k.contains("accepted") || k.contains("user") || k == "answer" || k == "choice" {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if k == "evidence" {
        Style::new().green()
    } else {
        Style::new().bold()
    }
}

#[cfg(test)]
mod tests {
    use super::super::LEAVE;
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn shown(screen: &mut Screen, width: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(width, 24)).unwrap();
        t.draw(|f| screen.render(f.area(), f.buffer_mut())).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn screen() -> (tempfile::TempDir, Screen) {
        let dir = tempfile::tempdir().unwrap();
        let s = Screen::new(
            "stand-in · 8192 tokens · small chat".into(),
            dir.path().into(),
        );
        (dir, s)
    }

    #[test]
    fn a_line_goes_to_the_chat_and_its_words_come_back() {
        let (_dir, mut s) = screen();
        for c in "hello".chars() {
            assert_eq!(s.key(key(KeyCode::Char(c))), Act::Nothing);
        }
        assert_eq!(s.key(key(KeyCode::Enter)), Act::Send("hello".into()));
        assert!(s.busy);
        // While it works, a second line waits; Ctrl-C stops the work.
        s.input = "more".into();
        assert_eq!(s.key(key(KeyCode::Enter)), Act::Nothing);
        assert_eq!(s.key(ctrl('c')), Act::Stop);
        s.take(Event::Said(Said::Call("plan".into(), "{}".into())));
        s.take(Event::Said(Said::Progress("reading".into())));
        let text = shown(&mut s, 120);
        assert!(text.contains("you  hello"), "{text}");
        assert!(text.contains("· plan {}"), "{text}");
        assert!(text.contains("… reading"), "{text}");
        assert!(text.contains("working"), "{text}");
        s.take(Event::Said(Said::Answer("Planned.".into())));
        s.take(Event::Idle);
        assert!(!s.busy);
        let text = shown(&mut s, 80);
        assert!(text.contains("Planned."), "{text}");
        assert!(!text.contains("… reading"), "{text}");
    }

    #[test]
    fn a_question_takes_only_its_replies() {
        let (_dir, mut s) = screen();
        s.busy = true;
        s.take(Event::Ask("Run senna__run_fit {}?".into(), LEAVE.into()));
        assert_eq!(s.status(), "waiting for you: Run senna__run_fit {}?");
        assert_eq!(s.key(key(KeyCode::Char('x'))), Act::Nothing);
        assert!(s.input.is_empty(), "typing does not go to the line");
        let text = shown(&mut s, 120);
        assert!(text.contains("press y, n or a"), "{text}");
        assert_eq!(s.key(key(KeyCode::Char('a'))), Act::Answer("a".into()));
        assert!(s.asking.is_none());
        let text = shown(&mut s, 120);
        assert!(text.contains("you  always, for this tool"), "{text}");
        // Ctrl-C at a question says no.
        s.take(Event::Ask("Run x?".into(), LEAVE.into()));
        assert_eq!(s.key(ctrl('c')), Act::Answer("n".into()));
    }

    #[test]
    fn the_records_show_steps_numbers_and_hypotheses() {
        let (dir, mut s) = screen();
        let root = dir.path();
        std::fs::create_dir_all(root.join("results")).unwrap();
        std::fs::write(
            root.join("results/INDEX.tsv"),
            format!(
                "{}\n2026-10-28\t10:02:03\tdata-beans\tqc\tresults/a.zarr.zip\tkept\tdata/a.h5\t{}\n",
                crate::project::INDEX_HEAD,
                crate::step::facts_cell(&[crate::Fact::value("mito", 0.3).flag("high")])
            ),
        )
        .unwrap();
        let ledger = |file: &str, head: &str, row: &str| {
            std::fs::write(
                root.join("results").join(file),
                format!("date\ttime\tserver\t{head}\n2026-10-28\t10:05:00\tlegume\t{row}\n"),
            )
            .unwrap()
        };
        ledger(
            "HYPOTHESES.tsv",
            crate::evidence::HYPOTHESES.1,
            "H1\t10:04 legume\tcluster 4 is stressed",
        );
        ledger(
            "EVIDENCE.tsv",
            crate::evidence::EVIDENCE.1,
            "H1\tsenna markers\tour_cells\tagrees\tHSPs up",
        );
        s.reread();
        let text = shown(&mut s, 140);
        assert!(
            text.contains("10:02 data-beans qc → results/a.zarr.zip"),
            "{text}"
        );
        assert!(text.contains("← data/a.h5"), "{text}");
        assert!(text.contains("⚑ mito"), "{text}");
        assert!(text.contains("⚑ 1"), "the top line counts it: {text}");
        s.key(key(KeyCode::Tab));
        s.key(key(KeyCode::Tab));
        let text = shown(&mut s, 140);
        assert!(text.contains("H1. cluster 4 is stressed"), "{text}");
        assert!(text.contains("level 1/3"), "{text}");
        assert!(text.contains("✓ senna markers (our_cells)"), "{text}");
        assert!(text.contains("not yet: other_donors"), "{text}");
    }

    #[test]
    fn leaving_and_scrolling() {
        let (_dir, mut s) = screen();
        for i in 0..100 {
            s.take(Event::Said(Said::Chat(format!("line {i}"))));
        }
        s.key(key(KeyCode::PageUp));
        let text = shown(&mut s, 120);
        assert!(text.contains("more below"), "{text}");
        assert!(!text.contains("line 99"), "{text}");
        s.key(key(KeyCode::End));
        let text = shown(&mut s, 120);
        assert!(text.contains("line 99"), "{text}");
        assert_eq!(s.key(ctrl('d')), Act::Leave);
        s.input = "/quit".into();
        assert_eq!(s.key(key(KeyCode::Enter)), Act::Leave);
    }
}
