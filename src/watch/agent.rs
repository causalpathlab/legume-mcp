//! What a person following a server knows of it, with no UI in it: the
//! calls as the feed tells them, the notebook, the tool's view, the steps
//! recorded, and whether the person is following or has looked away. A
//! tool's viewer draws it however suits its screen (the [`super::view`] pane,
//! a sidebar from [`Agent::lines`], its own map following
//! [`Agent::take_new_state`]); the steering goes through [`Agent::act`] and
//! [`Agent::note`].

use super::{control, follow, plain, Body, Control, Event, Follow, From, Meta};
use anyhow::Result;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Lines of a call's log kept.
const PROGRESS_KEPT: usize = 40;
/// Notebook entries kept.
const NOTES_KEPT: usize = 50;
/// Recorded commands kept.
const RECORDS_KEPT: usize = 50;
/// How far a page moves the selection.
const PAGE: usize = 10;

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

impl Status {
    /// The status in words: `running`, `done 1.2 s`, `stopped by the user`.
    pub fn label(&self) -> String {
        match self {
            Status::Running => "running".into(),
            Status::Done { ms } => format!("done {}", seconds(*ms)),
            Status::Failed { ms } => format!("failed {}", seconds(*ms)),
            Status::Stopped { by } => format!("stopped by the {by}"),
            Status::Lost => "lost: the server went".into(),
        }
    }
}

fn seconds(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
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
    /// Its log, the last lines (plain); dropped once a result says what came
    /// of it.
    pub progress: VecDeque<String>,
}

/// What a person can ask of the agent's view; a host binds its own keys to
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Previous,
    Next,
    PageUp,
    PageDown,
    First,
    Last,
    /// Follow again, from the latest call.
    Follow,
    /// Hand the selected call to the tool.
    Open,
    /// Stop the selected call (steering).
    Stop,
    /// Start a note for the agent (steering): the host takes the text, then
    /// [`Agent::note`].
    Note,
}

/// What came of an [`Action`].
#[derive(Debug)]
pub enum Outcome {
    Done,
    /// The person opened this call: the tool decides what that shows.
    Open(CallView),
    /// Something to tell the person (a stop asked for, or why not).
    Say(String),
    /// The host should take a note's text, then give it to [`Agent::note`].
    WriteNote,
    /// Not available here (steering on a watch-only view, or once ended).
    Unavailable,
}

/// A server, followed.
pub struct Agent {
    dir: PathBuf,
    feed: Follow,
    meta: Option<Meta>,
    calls: Vec<CallView>,
    /// The notebook and the person's notes: (who or what kind, text).
    notes: VecDeque<(String, String)>,
    /// The bare commands of the steps recorded, oldest first.
    records: VecDeque<String>,
    state: Option<Value>,
    /// A state not yet taken by [`Agent::take_new_state`].
    state_new: bool,
    ended: bool,
    /// The call the person moved to; none while following.
    selected: Option<usize>,
    /// The server's steering, when the view offers it.
    steer: Option<Control>,
}

impl Agent {
    /// Follow the server in `dir` (one of [`super::live`] or
    /// [`super::find`]) from the start of its feed; `steer` offers stop and
    /// note.
    pub fn new(dir: &Path, steer: bool) -> Result<Self> {
        let state = super::state(dir);
        Ok(Self {
            dir: dir.to_path_buf(),
            feed: follow(dir, From::Start)?,
            meta: None,
            calls: vec![],
            notes: VecDeque::new(),
            records: VecDeque::new(),
            state_new: state.is_some(),
            state,
            ended: false,
            selected: None,
            steer: steer.then(|| control(dir)),
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
                    push_kept(&mut c.progress, plain(&message), PROGRESS_KEPT);
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
            Body::State { state, .. } => {
                self.state = Some(state);
                self.state_new = true;
            }
            // The person's notes show as they leave them; the notebook's
            // copy of one (as `user`) is not shown twice.
            Body::Note { text } => push_kept(&mut self.notes, ("you".into(), text), NOTES_KEPT),
            Body::Notebook { entry, .. } if entry == "user" => {}
            Body::Notebook { entry, text } => push_kept(&mut self.notes, (entry, text), NOTES_KEPT),
            Body::Record { command, .. } => {
                if let Some(c) = command {
                    push_kept(&mut self.records, c, RECORDS_KEPT);
                }
            }
            // The end is the feed's to tell (`Follow::ended`).
            Body::Ended { .. } => {}
        }
    }

    /// The server's folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The server, once its first line is read.
    pub fn meta(&self) -> Option<&Meta> {
        self.meta.as_ref()
    }

    /// The calls, oldest first.
    pub fn calls(&self) -> &[CallView] {
        &self.calls
    }

    /// The notebook and the person's notes, oldest first: (who or what
    /// kind, text).
    pub fn notes(&self) -> impl Iterator<Item = &(String, String)> {
        self.notes.iter()
    }

    /// The commands of the steps recorded, oldest first (the last 50).
    pub fn records(&self) -> impl Iterator<Item = &str> {
        self.records.iter().map(String::as_str)
    }

    /// The tool's view as the agent last left it.
    pub fn state(&self) -> Option<&Value> {
        self.state.as_ref()
    }

    /// The tool's view if it changed since the last take (the first take
    /// gives the view the server had when it was opened): for a host that
    /// applies each new view once.
    pub fn take_new_state(&mut self) -> Option<Value> {
        std::mem::take(&mut self.state_new)
            .then(|| self.state.clone())
            .flatten()
    }

    /// Whether the view follows the agent (the latest call selected).
    pub fn following(&self) -> bool {
        self.selected.is_none()
    }

    /// The selected call's place: the latest while following; `None`
    /// before the first call.
    pub fn selected(&self) -> Option<usize> {
        (!self.calls.is_empty()).then(|| self.at())
    }

    fn at(&self) -> usize {
        self.selected.unwrap_or(self.calls.len().saturating_sub(1))
    }

    /// Stop following: the host's own navigation moved the view.
    pub fn pause(&mut self) {
        self.selected = Some(self.at());
    }

    /// Follow again, from the latest call.
    pub fn resume(&mut self) {
        self.selected = None;
    }

    /// Whether the server is gone.
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Whether stop and note are offered: the view steers, and the server
    /// is still there.
    pub fn can_steer(&self) -> bool {
        self.steering().is_some()
    }

    fn steering(&self) -> Option<&Control> {
        self.steer.as_ref().filter(|_| !self.ended)
    }

    /// What a stop or a note that did not go says.
    fn not_sent(what: &str, e: &anyhow::Error) -> String {
        format!("the {what} may not have reached the server ({e:#}); it may still take it")
    }

    /// Do what the person asked.
    pub fn act(&mut self, action: Action) -> Outcome {
        let last = self.calls.len().saturating_sub(1);
        let at = self.at();
        let to = match action {
            Action::Previous => Some(at.saturating_sub(1)),
            Action::Next => Some(at + 1),
            Action::PageUp => Some(at.saturating_sub(PAGE)),
            Action::PageDown => Some(at + PAGE),
            Action::First => Some(0),
            Action::Last => Some(last),
            _ => None,
        };
        if let Some(to) = to {
            self.selected = Some(to.min(last));
            return Outcome::Done;
        }
        match action {
            Action::Follow => {
                self.resume();
                Outcome::Done
            }
            Action::Open => match self.calls.get(at) {
                Some(c) if c.status != Status::Running => Outcome::Open(c.clone()),
                _ => Outcome::Say("nothing to open on this call yet".into()),
            },
            Action::Stop => match (self.steering(), self.calls.get(at)) {
                (None, _) => Outcome::Unavailable,
                (Some(steer), Some(c)) if c.status == Status::Running => {
                    Outcome::Say(match steer.stop(&c.id) {
                        Ok(()) => format!("asked to stop {} (call {})", c.tool, c.id),
                        Err(e) => Self::not_sent("stop", &e),
                    })
                }
                (Some(_), _) => Outcome::Say("the selected call is not running".into()),
            },
            Action::Note => match self.steering() {
                None => Outcome::Unavailable,
                Some(_) => Outcome::WriteNote,
            },
            _ => Outcome::Done,
        }
    }

    /// Leave the agent `text`: it comes with its next tool result. What to
    /// tell the person.
    pub fn note(&self, text: &str) -> String {
        let text = text.trim();
        if text.is_empty() {
            return "an empty note: nothing left".into();
        }
        match self.steering() {
            None => "the server has ended: the note did not go".into(),
            Some(c) => match c.note(text) {
                Ok(()) => "note left: the agent gets it with its next result".into(),
                Err(e) => Self::not_sent("note", &e),
            },
        }
    }

    /// The view in `n` lines of at most `width` characters, for a sidebar:
    /// the server and whether it is followed, then the latest calls with
    /// their status, the selected one marked `>`.
    pub fn lines(&self, n: usize, width: usize) -> Vec<String> {
        if n == 0 {
            return vec![];
        }
        let who = self
            .meta
            .as_ref()
            .map_or_else(|| self.dir.display().to_string(), |m| m.server.clone());
        let how = if self.ended {
            "ended"
        } else if self.following() {
            "following"
        } else {
            "paused"
        };
        let mut out = vec![fit(&format!("{who} · {how}"), width)];
        let shown = n - 1;
        let sel = self.selected();
        // The selected call in sight: the window ends at it, or at the
        // latest while following.
        let end = sel.map_or(0, |s| s + 1).max(shown.min(self.calls.len()));
        let start = end.saturating_sub(shown);
        for (i, c) in self.calls.iter().enumerate().take(end).skip(start) {
            let mark = if Some(i) == sel { '>' } else { ' ' };
            let line = format!("{mark}{} {} {}", c.at, c.tool, c.status.label());
            out.push(fit(&line, width));
        }
        out
    }
}

/// `s` cut to `width` characters, an ellipsis marking the cut.
fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.into();
    }
    let keep = width.saturating_sub(1);
    let mut cut: String = s.chars().take(keep).collect();
    if width > 0 {
        cut.push('…');
    }
    cut
}

/// The clock time of an RFC 3339 stamp: `12:34:56`.
pub(crate) fn clock(t: &str) -> &str {
    t.get(11..19).unwrap_or(t)
}

/// Push to the back, dropping from the front past `kept`.
fn push_kept<T>(q: &mut VecDeque<T>, x: T, kept: usize) {
    if q.len() == kept {
        q.pop_front();
    }
    q.push_back(x);
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn lines_are_cut_to_fit() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(fit("abc", 0), "");
    }
}
