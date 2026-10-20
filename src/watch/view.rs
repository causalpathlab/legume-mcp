//! The person's side of [`super`] on a terminal: a pane drawing an
//! [`Agent`], for a tool's own TUI to embed ([`AgentPane`]), and a full
//! screen of it in a session of the tool's own ([`run`], for
//! `<tool> mcp watch`). A host that draws the agent its own way (a sidebar,
//! its own map) uses [`Agent`] alone.
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
//! The keys are a table ([`Keys`]) the host can rebind, so F can follow
//! elsewhere. Only key presses act; a key's release or repeat is taken and
//! dropped.

use super::agent::clock;
pub use super::agent::{Action, Agent, CallView, Outcome, Status};
use super::Meta;
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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// What became of a key the pane was given.
pub enum Handled {
    /// The pane used it (or it was not a press).
    Consumed,
    /// Not the pane's: the host's to handle (q, its own keys).
    Ignored,
    /// The person opened this call (o): the tool decides what that shows.
    Open(CallView),
}

/// The pane's keys: which key asks for which [`Action`].
#[derive(Clone, Debug)]
pub struct Keys(Vec<(KeyCode, Action)>);

impl Default for Keys {
    fn default() -> Self {
        use Action::*;
        Self(vec![
            (KeyCode::Up, Previous),
            (KeyCode::Char('k'), Previous),
            (KeyCode::Down, Next),
            (KeyCode::Char('j'), Next),
            (KeyCode::PageUp, PageUp),
            (KeyCode::PageDown, PageDown),
            (KeyCode::Home, First),
            (KeyCode::End, Last),
            (KeyCode::Char('F'), Follow),
            (KeyCode::Char('f'), Follow),
            (KeyCode::Char('o'), Open),
            (KeyCode::Char('s'), Stop),
            (KeyCode::Char('n'), Note),
        ])
    }
}

impl Keys {
    /// `key` asks for `action`, in place of whatever it asked for before.
    #[must_use]
    pub fn bind(mut self, key: KeyCode, action: Action) -> Self {
        self.0.retain(|(k, _)| *k != key);
        self.0.push((key, action));
        self
    }

    /// `key` asks for nothing: the host keeps it.
    #[must_use]
    pub fn unbind(mut self, key: KeyCode) -> Self {
        self.0.retain(|(k, _)| *k != key);
        self
    }

    fn action(&self, key: KeyCode) -> Option<Action> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, a)| *a)
    }

    /// The first key bound to `action`, as the footer names it.
    fn name(&self, action: Action) -> Option<String> {
        let (k, _) = self.0.iter().find(|(_, a)| *a == action)?;
        Some(match k {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Tab => "Tab".into(),
            other => format!("{other:?}"),
        })
    }
}

/// The colour of a status.
fn style(status: &Status) -> Style {
    match status {
        Status::Running => Style::new().cyan().add_modifier(Modifier::BOLD),
        Status::Done { .. } => Style::new().green(),
        Status::Failed { .. } | Status::Lost => Style::new().red(),
        Status::Stopped { .. } => Style::new().yellow(),
    }
}

/// A result's line as a terminal would leave it: what came after its last
/// carriage return, tabs as spaces, no control left. (Progress lines also
/// lose a log's head, through [`super::plain`]; a result's text keeps its
/// brackets.)
fn text_line(line: &str) -> String {
    let line = line.rsplit('\r').next().unwrap_or(line);
    line.replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
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

/// A pane following one server: an [`Agent`] drawn, with its keys.
pub struct AgentPane {
    agent: Agent,
    keys: Keys,
    /// Where the call list is scrolled to, kept between frames.
    list: ListState,
    /// A note being typed.
    typing: Option<String>,
    /// What the last key did, said in the footer until the next key.
    said: Option<String>,
}

impl AgentPane {
    /// Follow the server in `dir` (one of [`super::live`]) from the start of
    /// its feed; `steer` offers stop and note.
    pub fn new(dir: &Path, steer: bool) -> Result<Self> {
        Self::with_keys(dir, steer, Keys::default())
    }

    /// [`Self::new`] with the host's own key table.
    pub fn with_keys(dir: &Path, steer: bool, keys: Keys) -> Result<Self> {
        Ok(Self {
            agent: Agent::new(dir, steer)?,
            keys,
            list: ListState::default(),
            typing: None,
            said: None,
        })
    }

    /// The followed server, as the pane knows it.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn agent_mut(&mut self) -> &mut Agent {
        &mut self.agent
    }

    /// Take in what the feed has now, without waiting; whether anything
    /// changed.
    pub fn poll(&mut self) -> bool {
        self.agent.poll()
    }

    /// The tool's view as the agent last left it.
    pub fn state(&self) -> Option<&Value> {
        self.agent.state()
    }

    pub fn following(&self) -> bool {
        self.agent.following()
    }

    /// Stop following: the host's own navigation moved the view.
    pub fn pause(&mut self) {
        self.agent.pause();
    }

    /// Follow again, from the latest call.
    pub fn resume(&mut self) {
        self.agent.resume();
    }

    /// Whether the server is gone.
    pub fn ended(&self) -> bool {
        self.agent.ended()
    }

    /// Whether a note is being typed: the host should give the pane every
    /// key, its own (q) included.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
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
                    if !text.trim().is_empty() {
                        self.said = Some(self.agent.note(&text));
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
        let Some(action) = self.keys.action(key.code) else {
            return Handled::Ignored;
        };
        match self.agent.act(action) {
            Outcome::Done => Handled::Consumed,
            Outcome::Open(c) => Handled::Open(c),
            Outcome::Say(s) => {
                self.said = Some(s);
                Handled::Consumed
            }
            Outcome::WriteNote => {
                self.typing = Some(String::new());
                Handled::Consumed
            }
            // Steering the view does not offer: the key is the host's.
            Outcome::Unavailable => Handled::Ignored,
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
        let a = &self.agent;
        let who = a.meta().map_or_else(|| a.dir().display().to_string(), who);
        let (live, style, follow) = if a.ended() {
            ("ended", Style::new().red(), String::new())
        } else if a.following() {
            ("live", Style::new().green(), " · following".to_string())
        } else {
            // Which key follows again, when one does.
            let paused = match self.keys.name(Action::Follow) {
                Some(k) => format!(" · paused — {k} follows"),
                None => " · paused".to_string(),
            };
            ("live", Style::new().green(), paused)
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
        let calls = self.agent.calls();
        let items: Vec<ListItem> = calls
            .iter()
            .map(|c| {
                ListItem::new(Line::from(vec![
                    Span::from(c.at.as_str()).dim(),
                    Span::from(" "),
                    Span::from(c.tool.as_str()).bold(),
                    Span::from(" "),
                    Span::styled(c.status.label(), style(&c.status)),
                ]))
            })
            .collect();
        let title = format!(" calls ({}) ", calls.len());
        self.list.select(self.agent.selected());
        StatefulWidget::render(list(items, title), area, buf, &mut self.list);
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::new().borders(Borders::ALL);
        let Some(c) = self
            .agent
            .selected()
            .and_then(|i| self.agent.calls().get(i))
        else {
            Paragraph::new("no calls yet")
                .block(block)
                .render(area, buf);
            return;
        };
        let block = block.title(Line::from(vec![
            Span::from(" "),
            Span::styled(c.status.label(), style(&c.status)),
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
            let lines: Vec<Line> = c.text.lines().map(|l| Line::from(text_line(l))).collect();
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .render(body, buf);
        }
    }

    fn render_notes(&self, area: Rect, buf: &mut Buffer) {
        let shown = area.height.saturating_sub(2) as usize;
        let notes: Vec<_> = self.agent.notes().collect();
        let skip = notes.len().saturating_sub(shown);
        let lines: Vec<Line> = notes
            .into_iter()
            .skip(skip)
            .map(|(kind, text)| {
                let first = text.lines().next().unwrap_or_default();
                Line::from(vec![
                    Span::from(format!("{kind}: ")).bold(),
                    Span::from(text_line(first)),
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
                let mut keys: Vec<String> = vec![];
                if let (Some(p), Some(n)) = (
                    self.keys.name(Action::Previous),
                    self.keys.name(Action::Next),
                ) {
                    keys.push(format!("{p}{n} select"));
                }
                let mut named = vec![(Action::Follow, "follow"), (Action::Open, "open")];
                if self.agent.can_steer() {
                    named.extend([(Action::Stop, "stop"), (Action::Note, "note")]);
                }
                for (action, what) in named {
                    if let Some(k) = self.keys.name(action) {
                        keys.push(format!("{k} {what}"));
                    }
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
        None => super::find(tool.name(), None),
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
    use super::*;

    #[test]
    fn a_line_reads_as_a_terminal_leaves_it() {
        // Columns stay apart; a redrawn bar shows its last state.
        assert_eq!(text_line("gene\tscore"), "gene    score");
        assert_eq!(text_line("10%\r20%\r30%"), "30%");
        assert_eq!(text_line("a\u{7}b"), "ab");
        // A result's brackets stay (a progress line's log head would not).
        assert_eq!(text_line("[a, b] c"), "[a, b] c");
    }

    #[test]
    fn keys_rebind_and_name_themselves() {
        let keys = Keys::default()
            .unbind(KeyCode::Char('F'))
            .unbind(KeyCode::Char('f'))
            .bind(KeyCode::Char('g'), Action::Follow);
        assert_eq!(keys.action(KeyCode::Char('F')), None);
        assert_eq!(keys.action(KeyCode::Char('g')), Some(Action::Follow));
        assert_eq!(keys.name(Action::Follow).as_deref(), Some("g"));
        assert_eq!(keys.name(Action::Previous).as_deref(), Some("↑"));
    }
}
