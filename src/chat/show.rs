//! What the chat says to the person and how it asks them, apart from where:
//! lines on a terminal ([`Plain`]), or the chat's screen (feature `screen`).

use std::io::{Result, Write};

/// One thing the chat says, its text already cleaned for a terminal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Said {
    /// The model's answer, which ends its turn.
    Answer(String),
    /// What the model says as it goes on to call tools.
    Aside(String),
    /// A call that runs: its tool, and its arguments as JSON.
    Call(String, String),
    /// A line of a running call's progress.
    Progress(String),
    /// The first line of a call's result.
    Result(String),
    /// The chat's own word: a command's reply, a stop, an error.
    Chat(String),
}

/// Where the chat speaks, and hears the person's reply to a question.
pub(crate) trait Show {
    fn say(&mut self, said: Said) -> Result<()>;
    /// The person's reply to `question` (one of `choices`, as `[y]es, [n]o`
    /// says them); none when they have gone.
    fn ask(&mut self, question: &str, choices: &str) -> Option<String>;
}

/// The chat as lines: its words to `out`, the person's replies from `input`.
pub(crate) struct Plain<'a> {
    pub out: &'a mut dyn Write,
    pub input: &'a mut dyn Iterator<Item = Result<String>>,
}

impl Show for Plain<'_> {
    fn say(&mut self, said: Said) -> Result<()> {
        match said {
            Said::Answer(t) => writeln!(self.out, "\n{t}\n"),
            Said::Aside(t) | Said::Chat(t) => writeln!(self.out, "{t}"),
            Said::Call(name, args) => writeln!(self.out, "· {name} {args}"),
            Said::Progress(m) => writeln!(self.out, "  … {m}"),
            Said::Result(first) => writeln!(self.out, "  {first}"),
        }
    }

    fn ask(&mut self, question: &str, choices: &str) -> Option<String> {
        write!(self.out, "{question} {choices}: ").ok()?;
        self.out.flush().ok()?;
        self.input.next()?.ok()
    }
}
