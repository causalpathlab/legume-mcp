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
/// Calls kept: a viewer left on a long session drops the oldest.
const CALLS_KEPT: usize = 500;
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

impl CallView {
    /// What the call is doing or came to, in a line: its last log line while
    /// it runs, the first line of its result once it has one.
    pub fn doing(&self) -> Option<&str> {
        if self.text.is_empty() {
            self.progress.back().map(String::as_str)
        } else {
            self.text.lines().next()
        }
    }
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
    /// The call the person picked; none means the latest.
    selected: Option<usize>,
    /// Not following: the person picked a call, or the host's own view
    /// moved away ([`Agent::pause`]).
    paused: bool,
    /// The server's steering, when the view offers it.
    steer: Option<Control>,
}

impl Agent {
    /// Follow the server in `dir` (one of [`super::live`] or
    /// [`super::find`]) from the start of its feed; `steer` offers stop and
    /// note.
    pub fn new(dir: &Path, steer: bool) -> Result<Self> {
        let mut agent = Self {
            dir: dir.to_path_buf(),
            feed: follow(dir, From::Start)?,
            meta: None,
            calls: vec![],
            notes: VecDeque::new(),
            records: VecDeque::new(),
            state: None,
            state_new: false,
            ended: false,
            selected: None,
            paused: false,
            steer: steer.then(|| control(dir)),
        };
        // The feed so far, read now: its views are one view, new once, and a
        // later poll marks only the views after it. `state.json` stands in
        // when the feed's own are gone.
        agent.poll();
        if agent.state.is_none() {
            agent.state = super::state(dir);
            agent.state_new = agent.state.is_some();
        }
        Ok(agent)
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
            Body::Call { id, tool, args } => {
                if self.calls.len() == CALLS_KEPT {
                    self.calls.remove(0);
                    // The call the person picked stays picked, or the
                    // oldest left when it was the one dropped.
                    if let Some(s) = &mut self.selected {
                        *s = s.saturating_sub(1);
                    }
                }
                self.calls.push(CallView {
                    id,
                    tool,
                    args,
                    at: clock(&e.t).to_string(),
                    status: Status::Running,
                    text: String::new(),
                    progress: VecDeque::new(),
                });
            }
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

    /// The tool's latest view if it changed since the last take (the first
    /// take gives the view the server had when it was opened; several views
    /// between takes give the last): for a host that applies a view once.
    pub fn take_new_state(&mut self) -> Option<Value> {
        std::mem::take(&mut self.state_new)
            .then(|| self.state.clone())
            .flatten()
    }

    /// Whether the view follows the agent.
    pub fn following(&self) -> bool {
        !self.paused
    }

    /// The selected call's place: the one the person picked, else the
    /// latest; `None` before the first call.
    pub fn selected(&self) -> Option<usize> {
        (!self.calls.is_empty()).then(|| self.at())
    }

    fn at(&self) -> usize {
        self.selected.unwrap_or(self.calls.len().saturating_sub(1))
    }

    /// Stop following because the host's own view moved (the person panned
    /// the map): the calls keep coming in as they are, the latest selected;
    /// only picking a call ([`Action::Previous`] and the like) holds one.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Follow again, from the latest call; the agent's latest view is new
    /// again ([`Self::take_new_state`]), for the host to go back to it.
    pub fn resume(&mut self) {
        self.selected = None;
        self.paused = false;
        self.state_new = self.state.is_some();
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
            // Picking a call holds it, and stops following.
            self.selected = Some(to.min(last));
            self.paused = true;
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
    /// the server and its pid and whether it is followed, then the latest
    /// calls with their status, the selected one marked `>`.
    pub fn lines(&self, n: usize, width: usize) -> Vec<String> {
        if n == 0 {
            return vec![];
        }
        let who = self.meta.as_ref().map_or_else(
            || self.dir.display().to_string(),
            |m| format!("{} {}", m.server, m.pid),
        );
        let how = if self.ended {
            "ended"
        } else if self.following() {
            "following"
        } else {
            "paused"
        };
        let mut out = vec![fit(&format!("{who} · {how}"), width)];
        out.extend(self.call_lines(n - 1, width));
        out
    }

    /// [`Self::lines`] without its head: the latest calls with their status
    /// (at most `n`), the selected one marked `>`, each cut to `width`; for a
    /// host that writes its own title.
    pub fn call_lines(&self, n: usize, width: usize) -> Vec<String> {
        let sel = self.selected();
        // The selected call in sight: the window ends at the call the person
        // picked, else at the latest (a host's pause does not hold one).
        let end = sel.map_or(0, |s| s + 1).max(n.min(self.calls.len()));
        let start = end.saturating_sub(n);
        self.calls
            .iter()
            .enumerate()
            .take(end)
            .skip(start)
            .map(|(i, c)| {
                let mark = if Some(i) == sel { '>' } else { ' ' };
                fit(
                    &format!("{mark}{} {} {}", c.at, c.tool, c.status.label()),
                    width,
                )
            })
            .collect()
    }
}

/// `s` cut to `width` characters (not display columns: wide characters
/// take two), an ellipsis marking the cut.
pub fn fit(s: &str, width: usize) -> String {
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
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_are_cut_to_fit() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(fit("abc", 0), "");
    }

    /// A server's folder whose feed holds `calls` calls, each done.
    fn folder(calls: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let meta = json!({ "server": "t", "version": "0", "pid": std::process::id(),
                           "project": null, "started": "x" });
        std::fs::write(dir.path().join("meta.json"), meta.to_string()).unwrap();
        let t = "2026-10-20T12:00:00-07:00";
        let mut server = meta.clone();
        server["t"] = json!(t);
        server["kind"] = json!("server");
        let mut feed = format!("{server}\n");
        for i in 0..calls {
            let id = i.to_string();
            feed += &format!(
                "{}\n{}\n",
                json!({ "t": t, "kind": "call", "id": id, "tool": "x", "args": {} }),
                json!({ "t": t, "kind": "result", "id": id, "ok": true, "ms": 1, "text": "" })
            );
        }
        std::fs::write(dir.path().join("feed.jsonl"), feed).unwrap();
        dir
    }

    #[test]
    fn a_long_session_keeps_its_latest_calls() {
        let dir = folder(CALLS_KEPT + 7);
        let agent = Agent::new(dir.path(), false).unwrap();
        assert_eq!(agent.calls().len(), CALLS_KEPT);
        assert_eq!(agent.calls()[0].id, "7", "the oldest dropped");
        assert_eq!(
            agent.calls().last().unwrap().id,
            (CALLS_KEPT + 6).to_string()
        );
    }

    #[test]
    fn a_hosts_pause_holds_no_call() {
        let dir = folder(5);
        let mut agent = Agent::new(dir.path(), false).unwrap();
        agent.pause();
        assert!(!agent.following());
        assert_eq!(agent.selected(), Some(4), "the latest, not held");
        assert!(
            agent.lines(3, 40)[2].starts_with(">"),
            "the window at the latest"
        );
        // Picking a call holds it.
        agent.act(Action::First);
        assert_eq!(agent.selected(), Some(0));
        assert!(agent.lines(3, 40)[1].starts_with(">"));
        agent.act(Action::Follow);
        assert!(agent.following());
        assert_eq!(agent.selected(), Some(4));
    }

    #[test]
    fn rows_without_a_head_and_a_call_in_a_line() {
        let dir = folder(3);
        let agent = Agent::new(dir.path(), false).unwrap();
        let rows = agent.call_lines(2, 40);
        assert_eq!(rows.len(), 2);
        assert!(
            rows[1].starts_with('>') && rows[1].contains("x done"),
            "{rows:?}"
        );
        assert_eq!(&agent.lines(3, 40)[1..], &rows[..]);
        let mut c = agent.calls()[0].clone();
        assert_eq!(c.doing(), None);
        c.progress.push_back("50%".into());
        c.text.clear();
        assert_eq!(c.doing(), Some("50%"));
        c.text = "made it\nmore".into();
        assert_eq!(c.doing(), Some("made it"));
    }

    #[test]
    fn following_again_hands_the_view_over_again() {
        let dir = folder(1);
        std::fs::write(dir.path().join("state.json"), r#"{"focus":[1]}"#).unwrap();
        let mut agent = Agent::new(dir.path(), false).unwrap();
        assert!(agent.take_new_state().is_some());
        agent.pause();
        assert_eq!(agent.take_new_state(), None);
        agent.resume();
        assert_eq!(
            agent.take_new_state(),
            Some(serde_json::json!({ "focus": [1] }))
        );
    }
}
