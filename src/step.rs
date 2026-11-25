//! A step as the records keep it: what it wrote, what it read, and the key
//! numbers it made. `results/INDEX.tsv` lists each output with the step's
//! summary, its inputs (so each output can be traced back to the data it
//! came from) and its facts (so an agent or a reader sees the numbers that
//! matter, and the ones a tool flagged, without opening the outputs). A
//! step run as it was by a choice carries [`Why`]: the driver script says it
//! above the step's line and the notebook keeps it as a `choice` entry.

use crate::project::{one_line, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A key number a step made: what it counts, its value, out of how many,
/// by which rule, and why it looks wrong when it does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fact {
    name: String,
    /// A number; null when it was not one (NaN, infinite).
    value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    of: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flag: Option<String>,
}

impl Fact {
    /// `name` counted: `Fact::count("cells", 9812)`.
    pub fn count(name: &str, n: u64) -> Self {
        Self::of_value(name, n.into())
    }

    /// `name` measured: `Fact::value("loss", 1.83)`.
    pub fn value(name: &str, x: f64) -> Self {
        Self::of_value(name, x.into())
    }

    fn of_value(name: &str, value: Value) -> Self {
        Self {
            name: name.into(),
            value,
            of: None,
            rule: None,
            flag: None,
        }
    }

    /// Out of `total`: 9812 cells of 11204.
    pub fn of(mut self, total: u64) -> Self {
        self.of = Some(total.into());
        self
    }

    /// The rule the number was counted by: "FDR < 0.1", "3 MADs".
    pub fn rule(mut self, rule: &str) -> Self {
        self.rule = Some(rule.into());
        self
    }

    /// Why the number looks wrong ("fewer than half the cells kept"), for
    /// the agent and the user to look at before going on.
    pub fn flag(mut self, why: &str) -> Self {
        self.flag = Some(why.into());
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn number(&self) -> Option<f64> {
        self.value.as_f64()
    }

    pub fn flagged(&self) -> Option<&str> {
        self.flag.as_deref()
    }
}

impl std::fmt::Display for Fact {
    /// `cells 9812 of 11204 (3 MADs; flagged: fewer than half kept)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.name, self.value)?;
        if let Some(of) = &self.of {
            write!(f, " of {of}")?;
        }
        let notes: Vec<String> = self
            .rule
            .iter()
            .cloned()
            .chain(self.flag.iter().map(|w| format!("flagged: {w}")))
            .collect();
        if !notes.is_empty() {
            write!(f, " ({})", notes.join("; "))?;
        }
        Ok(())
    }
}

/// The facts as `INDEX.tsv` keeps them: a JSON array, empty when none.
pub(crate) fn facts_cell(facts: &[Fact]) -> String {
    if facts.is_empty() {
        return String::new();
    }
    serde_json::to_string(facts).unwrap_or_default()
}

/// The facts of an `INDEX.tsv` cell; none when it is empty or not facts.
pub(crate) fn read_facts(cell: &str) -> Vec<Fact> {
    serde_json::from_str(cell).unwrap_or_default()
}

/// Who made a choice that steers a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum By {
    /// The person chose it.
    User,
    /// The agent chose it on its own.
    Agent,
    /// The agent proposed it and the person accepted it.
    AgentAccepted,
}

impl std::fmt::Display for By {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            By::User => "user",
            By::Agent => "agent",
            By::AgentAccepted => "agent, user accepted",
        })
    }
}

/// Why a step was run as it was (its root cells, its number of topics): the
/// reason, who chose it, and the evidence it cites.
///
/// ```
/// # use legume_mcp::{By, Why};
/// let why = Why::new("the root is the Ngn3-high cells, the least differentiated", By::User)
///     .citing(&["lupin round 2, annotate_why".into()]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Why {
    text: String,
    by: By,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cites: Vec<String>,
}

impl Why {
    /// The reason, in one line or a few, and who chose it.
    pub fn new(text: &str, by: By) -> Self {
        Self {
            text: text.into(),
            by,
            cites: Vec::new(),
        }
    }

    /// The evidence it rests on: a tool's result, a figure, a paper.
    pub fn citing(mut self, cites: &[String]) -> Self {
        self.cites.extend_from_slice(cites);
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn by(&self) -> By {
        self.by
    }

    pub fn cites(&self) -> &[String] {
        &self.cites
    }

    /// The cites in one line, project paths relative.
    fn cited(&self, p: &Project) -> String {
        let cites: Vec<String> = self.cites.iter().map(|c| plain(p, c)).collect();
        cites.join("; ")
    }

    /// The comment the driver script gets above the step's line: each part
    /// on one line, so nothing in it ends the comment.
    fn comment(&self, p: &Project) -> String {
        let mut out = format!("# why ({}): {}\n", self.by, plain(p, &self.text));
        if !self.cites.is_empty() {
            out.push_str(&format!("#   cites: {}\n", self.cited(p)));
        }
        out
    }

    /// The notebook's `choice` entry for the step of `summary`, begun at
    /// `started` (HH:MM) when it is known, which did not succeed when
    /// `failed` says why. Plain lines: the agent records it, so nothing in
    /// it reads as the person's quoted words.
    fn entry(
        &self,
        p: &Project,
        summary: &str,
        started: Option<&str>,
        failed: Option<&str>,
    ) -> String {
        let started = started.map_or(String::new(), |t| format!(" (started {})", plain(p, t)));
        // A summary starting `>` would read as the person's words.
        let summary = plain(p, summary);
        let summary = match summary.strip_prefix('>') {
            Some(rest) => format!("\\>{rest}"),
            None => summary,
        };
        let mut text = format!(
            "{summary}{started}\n\n- Why: {}\n- By: {}",
            plain(p, &self.text),
            self.by
        );
        if !self.cites.is_empty() {
            text.push_str(&format!("\n- Cites: {}", self.cited(p)));
        }
        if let Some(failed) = failed {
            text.push_str(&format!("\n- Did not succeed: {}", plain(p, failed)));
        }
        text
    }
}

/// `s` on one line, without control characters, project paths relative.
fn plain(p: &Project, s: &str) -> String {
    one_line(&p.rel(s))
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// The notebook kind of a step's [`Why`].
pub(crate) const CHOICE: &str = "choice";

/// The `choice` entry of `step`, which did not succeed when `failed` says
/// why, written to the notebook, and the comment the driver script gets
/// above its line; none when it has no [`Why`].
pub(crate) fn chosen(p: &Project, step: &Step, failed: Option<&str>) -> String {
    let Some(why) = &step.why else {
        return String::new();
    };
    let started = step.started.as_deref();
    p.notebook(CHOICE, &why.entry(p, &step.summary, started, failed));
    why.comment(p)
}

/// What a facts callback is given once the step is done: the child's
/// stdout and the outputs.
type After = Box<dyn FnOnce(&str, &[PathBuf]) -> Vec<Fact> + Send>;

/// A step that writes files, for [`crate::Call::run_step`] and
/// [`crate::Call::spawn_step`]:
///
/// ```no_run
/// # use legume_mcp::{By, Call, Fact, Step, Why};
/// # fn f(cx: &Call, cmd: std::process::Command, outputs: Vec<std::path::PathBuf>,
/// #      counts: std::path::PathBuf) -> anyhow::Result<()> {
/// let step = Step::new(&outputs, "senna fit, 12 topics")
///     .inputs(&[counts])
///     .fact(Fact::count("cells", 9812).of(11204))
///     .why(Why::new("12 topics: the elbow of the fit's loss", By::Agent))
///     .after(|_stdout, _outputs| vec![Fact::value("loss", 1.83)]);
/// cx.run_step(cmd, step)?;
/// # Ok(()) }
/// ```
pub struct Step {
    pub(crate) outputs: Vec<PathBuf>,
    pub(crate) summary: String,
    inputs: Option<Vec<PathBuf>>,
    pub(crate) facts: Vec<Fact>,
    pub(crate) why: Option<Why>,
    /// When it began (HH:MM), for a step recorded long after: a session
    /// job's.
    pub(crate) started: Option<String>,
    after: Option<After>,
}

impl Step {
    /// A step writing `outputs`; `summary` says in one line what it makes,
    /// from what.
    pub fn new(outputs: &[PathBuf], summary: &str) -> Self {
        Self {
            outputs: outputs.to_vec(),
            summary: summary.into(),
            inputs: None,
            facts: Vec::new(),
            why: None,
            started: None,
            after: None,
        }
    }

    /// The files it reads. Without this they are the existing files its
    /// command line names that are not its outputs ([`inputs_of`]).
    pub fn inputs(mut self, inputs: &[PathBuf]) -> Self {
        self.inputs = Some(inputs.to_vec());
        self
    }

    /// A number known before it runs (the cells it is given, say).
    pub fn fact(mut self, fact: Fact) -> Self {
        self.facts.push(fact);
        self
    }

    /// Why it is run as it is: a choice that steers it. The driver script
    /// says it above the step's line (`# why (user): ...`) and the day's
    /// notebook keeps it as a `choice` entry, whether the step succeeds or
    /// not; `INDEX.tsv` does not.
    pub fn why(mut self, why: Why) -> Self {
        self.why = Some(why);
        self
    }

    /// The numbers known once it is done, from its stdout and outputs.
    /// Called only when it succeeded.
    pub fn after(mut self, f: impl FnOnce(&str, &[PathBuf]) -> Vec<Fact> + Send + 'static) -> Self {
        self.after = Some(Box::new(f));
        self
    }

    /// Its inputs, as given or as `cmd` names them.
    pub(crate) fn resolve_inputs(&mut self, cmd: &Command) {
        if self.inputs.is_none() {
            self.inputs = Some(inputs_of(cmd, &self.outputs));
        }
    }

    pub(crate) fn input_paths(&self) -> &[PathBuf] {
        self.inputs.as_deref().unwrap_or_default()
    }

    /// The step succeeded with `stdout`: its facts, with those found now.
    /// A callback that panics adds nothing: the step is still recorded.
    pub(crate) fn conclude(&mut self, stdout: &str) {
        if let Some(after) = self.after.take() {
            let outputs = &self.outputs;
            let found =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| after(stdout, outputs)));
            match found {
                Ok(found) => self.facts.extend(found),
                Err(_) => log::warn!("a step's facts callback panicked; recorded without them"),
            }
        }
    }
}

/// The inputs a command line names: each argument (or the value of a
/// `--flag=value`) that is an existing file or folder, other than an output,
/// a folder an output is in, or a file within an output. Relative paths are
/// read from the command's folder.
pub fn inputs_of(cmd: &Command, outputs: &[PathBuf]) -> Vec<PathBuf> {
    let here = cmd
        .get_current_dir()
        .map(crate::absolute)
        .unwrap_or_else(|| crate::absolute(Path::new(".")));
    let outputs: Vec<PathBuf> = outputs.iter().map(|o| crate::absolute(o)).collect();
    let mut found: Vec<PathBuf> = Vec::new();
    for arg in cmd.get_args() {
        let Some(arg) = arg.to_str() else { continue };
        let word = match arg.strip_prefix('-') {
            Some(flag) => match flag.split_once('=') {
                Some((_, value)) => value,
                None => continue,
            },
            None => arg,
        };
        if word.is_empty() {
            continue;
        }
        let path = crate::absolute(&here.join(word));
        let related = |o: &PathBuf| o.starts_with(&path) || path.starts_with(o);
        if path.exists() && !outputs.iter().any(related) && !found.contains(&path) {
            found.push(path);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inputs_are_the_files_named_that_it_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(d.join("counts.zarr"), "").unwrap();
        std::fs::write(d.join("batch.tsv"), "").unwrap();
        std::fs::create_dir(d.join("out")).unwrap();
        let mut cmd = Command::new("senna");
        cmd.current_dir(d).args([
            "fit",
            "counts.zarr",
            "--batch=batch.tsv",
            "--out",
            "out/run",
            "-k",
            "12",
            ".",
            "missing.tsv",
            "counts.zarr",
        ]);
        let found = inputs_of(&cmd, &[d.join("out/run.json")]);
        let d = crate::absolute(d);
        assert_eq!(found, vec![d.join("counts.zarr"), d.join("batch.tsv")]);
    }

    #[test]
    fn a_steps_why_goes_above_its_line_and_into_the_notebook() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let p = std::sync::Arc::new(Project::new(root.to_path_buf(), "senna", "0"));
        let records = Some(p.clone());
        let out = root.join("ode.out");
        std::fs::write(&out, "").unwrap();
        let why = Why::new(
            "the root is the Ngn3-high cells,\nthe least differentiated",
            By::User,
        )
        .citing(&[format!("{}/lupin/annotate_why.json", root.display())]);
        let step = Step::new(std::slice::from_ref(&out), "senna ode from the root cells").why(why);
        let cmd = Command::new("senna");
        let bare = Some(p.command(&cmd));
        crate::record_step(&records, "ode", bare.clone(), &step, &Ok(String::new()));
        let failed =
            Step::new(&[], "> senna ode again").why(Why::new("try fewer steps", By::AgentAccepted));
        let err = Err(anyhow::anyhow!("bad\nmore"));
        crate::record_step(&records, "ode", bare, &failed, &err);

        let day = root.join("results").join(crate::project::now().0);
        let script = std::fs::read_to_string(day.join("runall.sh")).unwrap();
        assert!(
            script.contains(
                "# why (user): the root is the Ngn3-high cells, the least differentiated\n\
                 #   cites: lupin/annotate_why.json\nif [ ! -e ode.out ]"
            ),
            "{script}"
        );
        assert!(
            script.contains(
                "# why (agent, user accepted): try fewer steps\n# did not succeed (bad):"
            ),
            "{script}"
        );
        let notebook = p.notebook_today();
        assert!(
            notebook.contains(
                " · senna · choice\n\nsenna ode from the root cells\n\n\
                 - Why: the root is the Ngn3-high cells, the least differentiated\n\
                 - By: user\n- Cites: lupin/annotate_why.json\n"
            ),
            "{notebook}"
        );
        assert!(
            notebook.contains(
                "\\> senna ode again\n\n- Why: try fewer steps\n\
                 - By: agent, user accepted\n- Did not succeed: bad\n"
            ),
            "{notebook}"
        );
        let index = std::fs::read_to_string(root.join("results/INDEX.tsv")).unwrap();
        assert!(!index.contains("Ngn3"), "{index}");
    }

    #[test]
    fn facts_read_back_as_written_and_show_their_flags() {
        let facts = vec![
            Fact::count("cells", 9812).of(11204).rule("3 MADs"),
            Fact::value("loss", 1.83).flag("did not converge"),
            Fact::value("ratio", f64::NAN),
        ];
        let cell = facts_cell(&facts);
        assert!(
            cell.starts_with(r#"[{"name":"cells","value":9812,"of":11204"#),
            "{cell}"
        );
        let back = read_facts(&cell);
        assert_eq!(back, facts);
        assert_eq!(back[0].to_string(), "cells 9812 of 11204 (3 MADs)");
        assert_eq!(back[1].to_string(), "loss 1.83 (flagged: did not converge)");
        assert_eq!(back[1].flagged(), Some("did not converge"));
        assert_eq!(back[2].number(), None);
        assert!(read_facts("").is_empty() && read_facts("not json").is_empty());
        assert_eq!(facts_cell(&[]), "");
    }
}
