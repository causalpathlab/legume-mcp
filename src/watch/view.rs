//! The person's side of [`super`]: a pane that follows a server's feed, for
//! a tool's own TUI to embed ([`AgentPane`]), and a full screen of it in a
//! session of the tool's own ([`run`], for `<tool> mcp watch`).
//!
//! What the pane does is the same in every tool:
//!
//! 1. It follows when it opens: the calls as they come, the latest one
//!    selected, the tool's view taken from `state.json` and then from each
//!    `state` event.
//! 2. Moving the selection pauses following; the pane says so ("paused — F
//!    follows") and keeps listing the calls as they come.
//! 3. F follows again: the latest call is selected.
//! 4. Steering (s stops the selected call, n leaves the agent a note) is
//!    there only when the tool offers it; its keys are not shown otherwise,
//!    and nothing goes to the server.
//! 5. When the server is gone (its last line says it ended, or its process
//!    is gone) the pane says "ended" and keeps what it had; a call it was
//!    still running is marked lost.
//! 6. A result's image shows as its `[image/png, N bytes]` line; o hands
//!    the selected call to the tool ([`Handled::Open`]), which may open a
//!    view of its own on what it wrote.
//!
//! Only key presses act; a key's release or repeat is taken and dropped.

use super::{control, follow, Body, Control, Event, Follow, From, Meta};
use anyhow::Result;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, StatefulWidget, Widget, Wrap,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Lines of a call's log kept for the pane.
const PROGRESS_KEPT: usize = 40;
/// Notebook entries kept for the pane.
const NOTES_KEPT: usize = 50;

/// What became of a key the pane was given.
pub enum Handled {
    /// The pane used it (or it was not a press).
    Consumed,
    /// Not the pane's: the host's to handle (q, its own keys).
    Ignored,
    /// The person opened this call (o): the tool decides what that shows.
    Open(CallView),
}

/// How a call went.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Running,
    Done {
        ms: u64,
    },
    Failed {
        ms: u64,
    },
    /// `by`: the client or the user.
    Stopped {
        by: String,
    },
    /// The server went while it ran.
    Lost,
}

/// One call, as the feed told it.
#[derive(Clone, Debug)]
pub struct CallView {
    pub id: String,
    pub tool: String,
    pub args: Value,
    /// When it came, `12:34:56`.
    pub at: String,
    pub status: Status,
    /// What came of it: the result's text and JSON, cut to 4 KB.
    pub text: String,
    /// Its log, the last lines; dropped once a result says what came of it.
    progress: VecDeque<String>,
}

fn seconds(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

impl Status {
    fn shown(&self) -> (String, Style) {
        match self {
            Status::Running => (
                "running".into(),
                Style::new().cyan().add_modifier(Modifier::BOLD),
            ),
            Status::Done { ms } => (format!("done {}", seconds(*ms)), Style::new().green()),
            Status::Failed { ms } => (format!("failed {}", seconds(*ms)), Style::new().red()),
            Status::Stopped { by } => (format!("stopped by the {by}"), Style::new().yellow()),
            Status::Lost => ("lost: the server went".into(), Style::new().red()),
        }
    }
}

/// The clock time of an RFC 3339 stamp: `12:34:56`.
fn clock(t: &str) -> &str {
    t.get(11..19).unwrap_or(t)
}

/// A line as a terminal would leave it: what came after its last carriage
/// return (a redrawn bar's last state), tabs as spaces, no control left.
fn plain(line: &str) -> String {
    let line = line.rsplit('\r').next().unwrap_or(line);
    line.replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// A log line without its terminal colours or "[time LEVEL module]" prefix.
#[cfg(unix)]
fn log(line: &str) -> String {
    plain(&legume_tui::child::log_line(line))
}

#[cfg(not(unix))]
fn log(line: &str) -> String {
    plain(line)
}

/// A server in a line: `name version · pid N · project`.
fn who(m: &Meta) -> String {
    let mut s = format!("{} {} · pid {}", m.server, m.version, m.pid);
    if let Some(p) = &m.project {
        s += &format!(" · {}", p.display());
    }
    s
}

/// A bordered, titled list whose selected line is shown reversed.
fn list<'a>(items: Vec<ListItem<'a>>, title: String) -> List<'a> {
    List::new(items)
        .block(Block::new().borders(Borders::ALL).title(title))
        .highlight_style(Style::new().reversed())
}

/// Push to the back, dropping from the front past `kept`.
fn push_kept<T>(q: &mut VecDeque<T>, x: T, kept: usize) {
    if q.len() == kept {
        q.pop_front();
    }
    q.push_back(x);
}

/// A pane following one server.
pub struct AgentPane {
    dir: PathBuf,
    feed: Follow,
    meta: Option<Meta>,
    calls: Vec<CallView>,
    /// The notebook and the person's notes: (who or what kind, text).
    notes: VecDeque<(String, String)>,
    state: Option<Value>,
    ended: bool,
    /// The call the person moved to; none while following.
    selected: Option<usize>,
    /// Where the call list is scrolled to, kept between frames.
    list: ListState,
    /// The server's steering, when the tool offers it.
    steer: Option<Control>,
    /// A note being typed.
    typing: Option<String>,
    /// What the last key did, said in the footer until the next key.
    said: Option<String>,
}

impl AgentPane {
    /// Follow the server in `dir` (one of [`super::live`]) from the start of
    /// its feed; `steer` offers stop and note.
    pub fn new(dir: &Path, steer: bool) -> Result<Self> {
        Ok(Self {
            dir: dir.to_path_buf(),
            feed: follow(dir, From::Start)?,
            meta: None,
            calls: vec![],
            notes: VecDeque::new(),
            state: super::state(dir),
            ended: false,
            selected: None,
            list: ListState::default(),
            steer: steer.then(|| control(dir)),
            typing: None,
            said: None,
        })
    }

    /// Take in what the feed has now, without waiting; whether anything
    /// changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(e) = self.feed.try_next() {
            self.apply(e);
            changed = true;
        }
        if !self.ended && self.feed.ended() {
            // What it wrote last, then nothing more.
            while let Some(e) = self.feed.try_next() {
                self.apply(e);
            }
            self.ended = true;
            for c in self
                .calls
                .iter_mut()
                .filter(|c| c.status == Status::Running)
            {
                c.status = Status::Lost;
            }
            changed = true;
        }
        changed
    }

    fn call(&mut self, id: &str) -> Option<&mut CallView> {
        self.calls.iter_mut().rev().find(|c| c.id == id)
    }

    fn apply(&mut self, e: Event) {
        match e.body {
            Body::Server(m) => self.meta = Some(m),
            Body::Call { id, tool, args } => self.calls.push(CallView {
                id,
                tool,
                args,
                at: clock(&e.t).to_string(),
                status: Status::Running,
                text: String::new(),
                progress: VecDeque::new(),
            }),
            Body::Progress { id, message } => {
                if let Some(c) = self.call(&id) {
                    push_kept(&mut c.progress, log(&message), PROGRESS_KEPT);
                }
            }
            Body::Result { id, ok, ms, text } => {
                if let Some(c) = self.call(&id) {
                    // A stop comes before its failed result, and stays.
                    if c.status == Status::Running {
                        c.status = if ok {
                            Status::Done { ms }
                        } else {
                            Status::Failed { ms }
                        };
                    }
                    c.text = text;
                    c.progress = VecDeque::new();
                }
            }
            Body::Cancelled { id, by } => {
                if let Some(c) = self.call(&id) {
                    c.status = Status::Stopped { by };
                }
            }
            Body::State { state, .. } => self.state = Some(state),
            // The person's notes show as they leave them; the notebook's
            // copy of one (as `user`) is not shown twice.
            Body::Note { text } => push_kept(&mut self.notes, ("you".into(), text), NOTES_KEPT),
            Body::Notebook { entry, .. } if entry == "user" => {}
            Body::Notebook { entry, text } => push_kept(&mut self.notes, (entry, text), NOTES_KEPT),
            // The end is the feed's to tell (`Follow::ended`).
            Body::Record { .. } | Body::Ended { .. } => {}
        }
    }

    /// The tool's view as the agent last left it.
    pub fn state(&self) -> Option<&Value> {
        self.state.as_ref()
    }

    pub fn following(&self) -> bool {
        self.selected.is_none()
    }

    /// The selected call's place: the latest while following.
    fn selected(&self) -> usize {
        self.selected.unwrap_or(self.calls.len().saturating_sub(1))
    }

    /// Stop following: the host's own navigation moved the view.
    pub fn pause(&mut self) {
        self.selected = Some(self.selected());
    }

    /// Follow again, from the latest call.
    pub fn resume(&mut self) {
        self.selected = None;
    }

    /// Whether the server is gone.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Whether a note is being typed: the host should give the pane every
    /// key, its own (q) included.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// The steering, while there is a server to steer.
    fn steering(&self) -> Option<&Control> {
        self.steer.as_ref().filter(|_| !self.ended)
    }

    /// What a stop or a note that did not go says.
    fn not_sent(what: &str, e: &anyhow::Error) -> String {
        format!("the {what} may not have reached the server ({e:#}); it may still take it")
    }

    /// The pane's answer to a key.
    pub fn key(&mut self, key: KeyEvent) -> Handled {
        if key.kind != KeyEventKind::Press {
            return Handled::Consumed;
        }
        self.said = None;
        if let Some(text) = &mut self.typing {
            match key.code {
                KeyCode::Esc => self.typing = None,
                KeyCode::Enter => {
                    let text = self.typing.take().unwrap_or_default();
                    let text = text.trim();
                    if !text.is_empty() {
                        self.said = Some(match self.steering() {
                            None => "the server has ended: the note did not go".into(),
                            Some(c) => match c.note(text) {
                                Ok(()) => {
                                    "note left: the agent gets it with its next result".into()
                                }
                                Err(e) => Self::not_sent("note", &e),
                            },
                        });
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
                _ => {}
            }
            return Handled::Consumed;
        }
        let last = self.calls.len().saturating_sub(1);
        let at = self.selected();
        let to = match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(at.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => Some(at + 1),
            KeyCode::PageUp => Some(at.saturating_sub(10)),
            KeyCode::PageDown => Some(at + 10),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(last),
            _ => None,
        };
        if let Some(to) = to {
            self.selected = Some(to.min(last));
            return Handled::Consumed;
        }
        match key.code {
            KeyCode::Char('f' | 'F') => {
                self.resume();
                Handled::Consumed
            }
            KeyCode::Char('o') => match self.calls.get(at) {
                Some(c) if c.status != Status::Running => Handled::Open(c.clone()),
                _ => {
                    self.said = Some("nothing to open on this call yet".into());
                    Handled::Consumed
                }
            },
            KeyCode::Char('s') if self.steering().is_some() => {
                self.said = Some(match (self.calls.get(at), self.steering()) {
                    (Some(c), Some(steer)) if c.status == Status::Running => {
                        match steer.stop(&c.id) {
                            Ok(()) => format!("asked to stop {} (call {})", c.tool, c.id),
                            Err(e) => Self::not_sent("stop", &e),
                        }
                    }
                    _ => "the selected call is not running".into(),
                });
                Handled::Consumed
            }
            KeyCode::Char('n') if self.steering().is_some() => {
                self.typing = Some(String::new());
                Handled::Consumed
            }
            _ => Handled::Ignored,
        }
    }

    /// Say `text` in the footer until the next key: what the host made of
    /// an open, say.
    pub fn say(&mut self, text: impl Into<String>) {
        self.said = Some(text.into());
    }

    /// Draw the pane in `area`.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let [head, body, notes, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .areas(area);
        self.render_head(head, buf);
        let [calls, detail] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .areas(body);
        self.render_calls(calls, buf);
        self.render_detail(detail, buf);
        self.render_notes(notes, buf);
        self.render_foot(foot, buf);
    }

    fn render_head(&self, area: Rect, buf: &mut Buffer) {
        let who = self
            .meta
            .as_ref()
            .map_or_else(|| self.dir.display().to_string(), who);
        let (live, style, follow) = if self.ended {
            ("ended", Style::new().red(), "")
        } else if self.following() {
            ("live", Style::new().green(), " · following")
        } else {
            ("live", Style::new().green(), " · paused — F follows")
        };
        Line::from(vec![
            Span::from(who).bold(),
            Span::from(" · "),
            Span::styled(live, style),
            Span::from(follow),
        ])
        .render(area, buf);
    }

    fn render_calls(&mut self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .calls
            .iter()
            .map(|c| {
                let (status, style) = c.status.shown();
                ListItem::new(Line::from(vec![
                    Span::from(c.at.as_str()).dim(),
                    Span::from(" "),
                    Span::from(c.tool.as_str()).bold(),
                    Span::from(" "),
                    Span::styled(status, style),
                ]))
            })
            .collect();
        let selected = (!self.calls.is_empty()).then(|| self.selected());
        self.list.select(selected);
        let title = format!(" calls ({}) ", self.calls.len());
        StatefulWidget::render(list(items, title), area, buf, &mut self.list);
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::new().borders(Borders::ALL);
        let Some(c) = self.calls.get(self.selected()) else {
            Paragraph::new("no calls yet")
                .block(block)
                .render(area, buf);
            return;
        };
        let (status, style) = c.status.shown();
        let block = block.title(Line::from(vec![
            Span::from(" "),
            Span::styled(status, style),
            Span::from(" "),
        ]));
        let inner = block.inner(area);
        block.render(area, buf);
        let [head, body] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(inner);
        // The call: its tool and its arguments, each on a line of its own.
        Paragraph::new(vec![
            Line::from(vec![
                Span::from(c.tool.as_str()).bold(),
                Span::from(format!("  call {}", c.id)).dim(),
            ]),
            Line::from(c.args.to_string()).dim(),
        ])
        .render(head, buf);
        if c.text.is_empty() {
            // Its log, the end of it: one screen line a log line.
            let skip = c.progress.len().saturating_sub(body.height as usize);
            let lines: Vec<Line> = c
                .progress
                .iter()
                .skip(skip)
                .map(|l| Line::from(l.as_str()).dim())
                .collect();
            Paragraph::new(lines).render(body, buf);
        } else {
            let lines: Vec<Line> = c.text.lines().map(|l| Line::from(plain(l))).collect();
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .render(body, buf);
        }
    }

    fn render_notes(&self, area: Rect, buf: &mut Buffer) {
        let shown = area.height.saturating_sub(2) as usize;
        let skip = self.notes.len().saturating_sub(shown);
        let lines: Vec<Line> = self
            .notes
            .iter()
            .skip(skip)
            .map(|(kind, text)| {
                let first = text.lines().next().unwrap_or_default();
                Line::from(vec![
                    Span::from(format!("{kind}: ")).bold(),
                    Span::from(plain(first)),
                ])
            })
            .collect();
        Paragraph::new(lines)
            .block(Block::new().borders(Borders::ALL).title(" notebook "))
            .render(area, buf);
    }

    fn render_foot(&self, area: Rect, buf: &mut Buffer) {
        let line = match (&self.typing, &self.said) {
            (Some(text), _) => Line::from(vec![
                Span::from("note for the agent: ").bold(),
                Span::from(format!("{text}▏")),
                Span::from("   Enter leaves it · Esc drops it").dim(),
            ]),
            (None, Some(said)) => Line::from(said.as_str()),
            (None, None) => {
                let mut keys: Vec<String> =
                    ["↑↓ select", "F follow", "o open"].map(String::from).into();
                if self.steering().is_some() {
                    keys.extend(["s stop".into(), "n note".into()]);
                }
                keys.push("q quit".into());
                // A session's own keys: how to leave it running, and to go to another.
                keys.extend(
                    legume_tui::help_keys()
                        .into_iter()
                        .map(|k| format!("{} {}", k.key, k.short)),
                );
                Line::from(keys.join(" · ")).dim()
            }
        };
        line.render(area, buf);
    }
}

/// `<tool> mcp watch`: the pane full screen, in a session of `tool`'s own,
/// on the server in `dir`, or `tool`'s one live server, or one the person
/// picks among them. `steer` offers stop and note; `open` turns an opened
/// call into a command to run in the terminal (a view of the tool's own on
/// what it wrote), or says there is nothing to open.
pub fn run(
    tool: &legume_tui::Tool,
    dir: Option<PathBuf>,
    steer: bool,
    open: impl Fn(&CallView) -> Option<Command>,
) -> Result<()> {
    // The servers to choose among, before any screen goes up.
    let mut servers: Vec<_> = match &dir {
        Some(_) => vec![],
        None => super::live()
            .into_iter()
            .filter(|l| l.alive && l.meta.server == tool.name())
            .collect(),
    };
    let chosen = match (dir, servers.len()) {
        (Some(d), _) => Some(d),
        (None, 0) => anyhow::bail!(
            "no {} MCP server is running: start one, then watch it",
            tool.name()
        ),
        (None, 1) => Some(servers.remove(0).dir),
        (None, _) => None,
    };
    let mut pane = chosen
        .as_deref()
        .map(|d| AgentPane::new(d, steer))
        .transpose()?;
    // What to show is named: no resume picker first. Dropped, the session
    // gives the terminal back.
    let opts = legume_tui::Options {
        target: true,
        ..Default::default()
    };
    let session = tool.session("watch", &opts)?;
    let mut terminal = session.terminal()?;
    let mut pane = match pane.take() {
        Some(p) => p,
        None => match pick(&session, &mut terminal, servers)? {
            Some(d) => AgentPane::new(&d, steer)?,
            None => return Ok(()),
        },
    };
    let mut dirty = true;
    loop {
        dirty |= pane.poll();
        if dirty {
            session.draw(&mut terminal, |f| pane.render(f.area(), f.buffer_mut()))?;
        }
        let round = session.poll(&mut terminal, Duration::from_millis(250))?;
        // Only a quiet round leaves the screen as it was.
        dirty = !matches!(round, legume_tui::Round::Timeout);
        match round {
            legume_tui::Round::Stopped => return Ok(()),
            legume_tui::Round::Event(TermEvent::Key(key)) => match pane.key(key) {
                Handled::Ignored if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) => {
                    return Ok(())
                }
                Handled::Open(call) => match open(&call) {
                    Some(mut cmd) => match session.run_child(&mut cmd) {
                        Ok(status) if status.success() => {}
                        Ok(status) => pane.say(format!("it did not open ({status})")),
                        Err(e) => pane.say(format!("it did not open: {e}")),
                    },
                    None => pane.say(format!("nothing to open on {}", call.tool)),
                },
                _ => {}
            },
            _ => {}
        }
    }
}

/// The person's pick among `servers`; `None` when they leave without one.
fn pick(
    session: &legume_tui::Session,
    terminal: &mut ratatui::DefaultTerminal,
    mut servers: Vec<super::Live>,
) -> Result<Option<PathBuf>> {
    let lines: Vec<String> = servers
        .iter()
        .map(|l| format!("{} · since {}", who(&l.meta), clock(&l.meta.started)))
        .collect();
    let mut state = ListState::default().with_selected(Some(0));
    loop {
        session.draw(terminal, |f| {
            let items = lines.iter().map(|l| ListItem::new(l.as_str())).collect();
            let picker = list(items, " which server? Enter follows · q leaves ".into());
            StatefulWidget::render(picker, f.area(), f.buffer_mut(), &mut state);
        })?;
        let round = session.poll(terminal, Duration::from_millis(500))?;
        if matches!(round, legume_tui::Round::Stopped) {
            return Ok(None);
        }
        let Some(TermEvent::Key(key)) = round.event() else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => state.select_previous(),
            KeyCode::Down | KeyCode::Char('j') => state.select_next(),
            KeyCode::Enter => {
                let i = state.selected().unwrap_or(0).min(servers.len() - 1);
                return Ok(Some(servers.swap_remove(i).dir));
            }
            KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn a_line_reads_as_a_terminal_leaves_it() {
        // Columns stay apart; a redrawn bar shows its last state.
        assert_eq!(plain("gene\tscore"), "gene    score");
        assert_eq!(plain("10%\r20%\r30%"), "30%");
        assert_eq!(plain("a\u{7}b"), "ab");
    }
}
