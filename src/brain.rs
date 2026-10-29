//! The `legume` server: what the tools share. Each tool's server runs its
//! own steps and notes what it did; this one keeps the reasoning across
//! them: the readings an agent proposes, the user's answers, the
//! hypotheses and the evidence each source gives (`evidence.rs`), the
//! offers and choices, and the guide to working this way
//! (`docs/collaboration.md`), served a section at a time so a small model's
//! context holds it.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::project::Project;
use crate::{
    evidence, notebook, project, Call, Content, Handler, Options, Prompt, Role, ServerInfo,
    ToolSpec,
};

/// The server's name, as clients register it.
pub const NAME: &str = "legume";

/// The guide: how people, agents and the legume tools work together.
pub const GUIDE: &str = include_str!("../docs/collaboration.md");

/// Serve the `legume` server on stdin and stdout from inside the project
/// ([`project::enter`]), as `legume mcp` does.
pub fn serve() -> Result<()> {
    let options = Options {
        project: project::enter()?,
        watch: crate::watch::base(),
    };
    crate::serve_stdio(info(), options, Role::Brain, Brain)
}

/// [`serve`] on any input and output, keeping the records of the project
/// at `project` when given.
pub fn serve_in<R, W>(input: R, output: W, project: Option<PathBuf>) -> Result<()>
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
{
    let options = Options {
        project,
        watch: None,
    };
    crate::serve_as(input, output, info(), options, Role::Brain, Brain)
}

fn info() -> ServerInfo {
    let card = section("quick").map_or("", |s| s.body);
    ServerInfo {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        instructions: Some(format!(
            "The legume server keeps the reasoning the legume tools share: what results \
             might mean, what the user decided, and how each hypothesis stands against \
             independent sources of evidence. The tools' own servers run the steps. \
             `guide` lists the sections of the guide to this work and gives any one of \
             them; read the quick card below first. `status` says where the project \
             stands: its data, recent outputs, today's notebook, a question waiting for the \
             user, and the hypotheses. `hypotheses` gives one hypothesis's triangulation \
             table.\n\nQuick card for agents:{card}"
        )),
    }
}

/// A section of the guide: its id (`8.3`, `quick`), title and text.
pub(crate) struct Section {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) depth: usize,
    pub(crate) body: &'static str,
}

/// The guide's sections in order: its opening (`intro`), each `##` heading,
/// and each `###` within one. Headings inside code blocks are text.
pub(crate) fn sections() -> Vec<Section> {
    let mut heads: Vec<(usize, usize, &str)> = Vec::new(); // offset, depth, title
    let mut fenced = false;
    let mut at = 0;
    for line in GUIDE.split_inclusive('\n') {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            if let Some(t) = line.strip_prefix("## ") {
                heads.push((at, 2, t.trim()));
            } else if let Some(t) = line.strip_prefix("### ") {
                heads.push((at, 3, t.trim()));
            }
        }
        at += line.len();
    }
    let first = heads.first().map_or(GUIDE.len(), |h| h.0);
    let title = GUIDE
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("# ");
    let mut out = vec![Section {
        id: "intro".into(),
        title: title.into(),
        depth: 1,
        body: &GUIDE[..first],
    }];
    for (i, &(start, depth, title)) in heads.iter().enumerate() {
        let end = heads[i + 1..]
            .iter()
            .find(|h| h.1 <= depth)
            .map_or(GUIDE.len(), |h| h.0);
        let (id, title) = match title.split_once(' ') {
            Some((n, rest)) if n.starts_with(|c: char| c.is_ascii_digit()) => {
                (n.trim_end_matches('.').to_string(), rest.trim())
            }
            _ => (
                title
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_lowercase(),
                title,
            ),
        };
        let body = &GUIDE[start..end];
        let body = body.split_once('\n').map_or("", |(_, b)| b);
        out.push(Section {
            id,
            title: title.into(),
            depth,
            body: body.trim_end_matches(['\n', '-']).trim_end(),
        });
    }
    out
}

fn section(id: &str) -> Option<Section> {
    sections().into_iter().find(|s| s.id == id)
}

/// The guide's sections, or the one named `id`.
pub fn guide(id: Option<&str>) -> Result<String> {
    let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) else {
        let mut out = String::from(
            "The guide to working with the legume tools, by section. Ask for one by its \
             id, e.g. {\"section\": \"8.3\"}; a section takes in its subsections.\n\n",
        );
        for s in sections() {
            let indent = if s.depth == 3 { "  " } else { "" };
            out.push_str(&format!("{indent}{:<10} {}\n", s.id, s.title));
        }
        return Ok(out);
    };
    let id = id
        .trim_start_matches("section")
        .trim()
        .trim_end_matches('.');
    match section(&id.to_lowercase()) {
        Some(s) => Ok(format!("{} {}\n{}\n", s.id, s.title, s.body.trim_end())),
        None => {
            let ids: Vec<String> = sections().into_iter().map(|s| s.id).collect();
            anyhow::bail!(
                "the guide has no section `{id}`; its sections: {}",
                ids.join(", ")
            )
        }
    }
}

/// The recipes for whole pieces of work, as MCP prompts: what a person
/// starts with (`/mcp__legume__explore` in Claude Code), or a local model's
/// chat starts a stage with.
pub(crate) fn prompts() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "explore",
            "Explore a data set",
            "Work through a data set with the person, stage by stage, from a survey to hypotheses tested against independent sources (guide sections 4 and 13).",
            EXPLORE,
        )
        .arg("question", "What the person wants to know, if anything yet", false)
        .arg("data", "The data to start from: a file or folder under the project", false),
        Prompt::new(
            "triangulate",
            "Triangulate a hypothesis",
            "Test a hypothesis the person chose against sources of evidence that share little with each other, and say the verb they allow (guide section 8).",
            TRIANGULATE,
        )
        .arg("hypothesis", "The hypothesis, as H1, H2, ... (`hypotheses` lists them)", false),
    ]
}

const EXPLORE: &str = "\
Let us explore a data set together, the way the legume guide says (its sections 4 and 13).

Question: {question}
Data: {data}

1. Call `status` (this server) to see the project: its data, recent outputs, any \
question waiting for me, and the hypotheses so far. If the project folder looks \
wrong (source code, a home folder), tell me first.
2. If there is no question, the question is \"what varies in this data, and which \
differences are worth testing?\". Put it as a contrast: a fact rather than a foil.
3. Record a `plan` in the notebook: the question, the data, two or more sources of \
evidence that share as little as possible (guide 8.3), and how the result will be \
checked (held-out donors, say).
4. Work in stages, and stop after each one to show me what you found:
   survey (cells, genes, donors, quality), structure (axes, clusters, cell \
types), contrast (where conditions differ), shortlist (three to five \
hypotheses, each a contrast), triangulate (one new source at a time), report.
   Read-only tools first; record an `observation` with the numbers after each \
step; follow a job with `job_wait` until it is done.
5. When a result needs interpreting, propose two or more readings \
(`interpretation`) and ask me which hold. Never decide what a result means \
alone. Record my reply as an `answer` with my exact words.
6. The result of exploring is hypotheses, not findings: say so at the end, with \
where the records are and, for each hypothesis, the source still missing. Build \
the day's slides with `report`, your story in `story`, and tell me where they are.

Read a guide section with `guide` when a step is unclear.";

const TRIANGULATE: &str = "\
Let us test a hypothesis against independent sources of evidence (the legume guide, section 8).

Hypothesis: {hypothesis}

1. Call `hypotheses` (this server): without an id it lists them with their \
standing. If no hypothesis was named, show me the list and ask which one.
2. Call `hypotheses` with its id: its table, the verb its sources allow now, and \
the kinds of source not yet used.
3. For each source already looked at but not in the table, record what it says \
as notebook kind `evidence` (hypothesis, source, class, verdict, the numbers in \
`text`). Several tools on the same data are one source. A gene-set or term test \
on our cells is our cells, not curated knowledge.
4. Offer the next sources as notebook kind `offer` with numbered options, those \
that share least with the sources that agree first (guide 8.6); stopping can be \
one. Ask me, and record my reply as kind `choice` with my exact words.
5. Run what I chose, record its `evidence`, and go back to 2 until I stop.
6. Use only the verb the table allows. Name what disagrees. \"Drives\" needs \
genetic, perturbation or applied-treatment evidence that agrees.";

struct Brain;

impl Handler for Brain {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec::new(
                "guide",
                "The guide to working with the legume tools and the user: without `section`, its sections; with one (e.g. \"8.3\", \"quick\"), that section's text.",
                json!({
                    "type": "object",
                    "properties": { "section": { "type": "string", "description": "A section's id, as the list gives it" } },
                }),
            )
            .read_only(),
            ToolSpec::new(
                "status",
                "Where the project stands: its data, recent outputs, today's notebook, a question waiting for the user's reply, and the hypotheses with how far each got.",
                json!({ "type": "object", "properties": {} }),
            )
            .read_only(),
            ToolSpec::new(
                "hypotheses",
                "The project's hypotheses (the readings the user chose), each with how far it got; with `id`, its triangulation table, the verb it allows and the sources not yet used.",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string", "description": "A hypothesis, as H1, H2, ..." } },
                }),
            )
            .read_only(),
            ToolSpec::new(
                "report",
                "The day's work as slides, built from the records, in results/<day>/report/ (report.typ, its provenance graph graph.svg, and report.pdf when Typst is installed; a story replaced is kept): the question, the data, the steps and their numbers, the figures with the commands that made them, what was seen, the person's interpretation (or \"not yet interpreted\"), each hypothesis's triangulation table, and how to replay it. The `story` is the one part you write: a few short paragraphs on what was asked, done and found, as hypotheses, with the verbs the tables allow; the slides label it as yours. Build it when the person asks for a report or the day's work ends, and show them where it is.",
                json!({
                    "type": "object",
                    "properties": {
                        "story": { "type": "string", "description": "What was asked, done and found, in a few short paragraphs, from the records; it replaces the day's story. Left out, the story written before is kept." },
                        "day": { "type": "string", "description": "YYYY-MM-DD; today when left out" },
                    },
                }),
            )
            .writes()
            .idempotent(),
        ]
    }

    fn prompts(&self) -> Vec<Prompt> {
        prompts()
    }

    fn call(&mut self, name: &str, args: Value, cx: &Call) -> Result<Vec<Content>> {
        let text = match name {
            "guide" => guide(args["section"].as_str())?,
            "status" => status(records(cx)?),
            "hypotheses" => hypotheses(records(cx)?, &args["id"])?,
            "report" => {
                let p = records(cx)?;
                let built = crate::report::build(
                    p.root(),
                    args["day"].as_str(),
                    args["story"].as_str(),
                    true,
                )?;
                crate::report::said(p.root(), &built)
            }
            _ => anyhow::bail!("no tool `{name}`"),
        };
        Ok(vec![Content::Text(text)])
    }
}

fn records(cx: &Call) -> Result<&Project> {
    cx.records.as_deref().ok_or_else(|| {
        anyhow::anyhow!("this server keeps no records ({} is off)", project::OFF_ENV)
    })
}

/// The ledger, read.
fn ledger(p: &Project) -> (Vec<evidence::Hypothesis>, Vec<evidence::Evidence>) {
    (
        evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES)),
        evidence::evidence(&p.read_ledger(evidence::EVIDENCE)),
    )
}

fn hypotheses(p: &Project, id: &Value) -> Result<String> {
    let (hyps, all) = ledger(p);
    if id.is_null() {
        if hyps.is_empty() {
            return Ok(
                "No hypotheses yet: a reading becomes one when the user chooses it in an `answer`."
                    .into(),
            );
        }
        let lines: Vec<String> = hyps.iter().map(|h| evidence::standing(h, &all)).collect();
        return Ok(lines.join("\n"));
    }
    let args = json!({ "hypothesis": id });
    let h = evidence::find(&args, &hyps)?;
    Ok(format!(
        "{}\n\n{}",
        evidence::table(h, &all),
        evidence::next(h, &all)
    ))
}

/// The last `n` lines of `text` that `keep`.
fn tail(text: &str, n: usize, keep: impl Fn(&str) -> bool) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().filter(|l| keep(l)).collect();
    lines.drain(..lines.len().saturating_sub(n));
    lines
}

pub(crate) fn status(p: &Project) -> String {
    let root = p.root();
    let read = |rel: &str| project::read_record(&root.join(rel));
    let mut out = format!("Project: {}\n", root.display());
    let data = read("data/README.md");
    let data = tail(&data, 10, |l| l.starts_with("- "));
    out.push_str("\nData (data/README.md):\n");
    if data.is_empty() {
        out.push_str("  none recorded\n");
    }
    for l in data {
        out.push_str(&format!("  {l}\n"));
    }
    let steps = crate::records::steps(root);
    out.push_str("\nRecent outputs (results/INDEX.tsv), with the numbers each step made:\n");
    if steps.is_empty() {
        out.push_str("  none yet\n");
    }
    for s in &steps[steps.len().saturating_sub(8)..] {
        out.push_str(&format!(
            "  {} {} {} {}: {} ({})\n",
            s.date, s.time, s.server, s.tool, s.output, s.summary
        ));
        if !s.facts.is_empty() {
            let facts: Vec<String> = s.facts.iter().map(ToString::to_string).collect();
            out.push_str(&format!("      {}\n", facts.join("; ")));
        }
    }
    let flagged: Vec<String> = steps
        .iter()
        .flat_map(|s| {
            s.facts
                .iter()
                .filter(|f| f.flagged().is_some())
                .map(move |f| format!("  {} ({} {}): {f}\n", s.output, s.server, s.tool))
        })
        .collect();
    if !flagged.is_empty() {
        out.push_str(
            "\nFlagged by the tools (look at these with the user before building on them):\n",
        );
        for f in &flagged[flagged.len().saturating_sub(8)..] {
            out.push_str(f);
        }
    }
    let today = p.notebook_today();
    let heads: Vec<&str> = notebook::entries(&today)
        .into_iter()
        .filter_map(|e| e.lines().next())
        .collect();
    out.push_str("\nToday's notebook (results/<today>/notebook.md), latest entries:\n");
    if heads.is_empty() {
        out.push_str("  none yet: start with a `plan`\n");
    }
    for h in &heads[heads.len().saturating_sub(8)..] {
        out.push_str(&format!("  {h}\n"));
    }
    if let Some(q) = notebook::open_question(&today) {
        out.push_str(&format!("\nWaiting for the user: {q}\n"));
    }
    let (hyps, all) = ledger(p);
    out.push_str("\nHypotheses (results/HYPOTHESES.tsv, EVIDENCE.tsv):\n");
    if hyps.is_empty() {
        out.push_str("  none yet\n");
    }
    for h in &hyps {
        out.push_str(&format!("  {}\n", evidence::standing(h, &all)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_splits_at_its_headings_not_inside_code() {
        let all = sections();
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        for id in ["intro", "quick", "1", "8", "8.3", "8.5", "16", "references"] {
            assert!(ids.contains(&id), "{id} in {ids:?}");
        }
        assert!(
            !all.iter().any(|s| s.title.contains("·")),
            "the notebook examples are not sections"
        );
        let eight = section("8").unwrap();
        assert!(eight.body.contains("### 8.3 Sources of evidence"));
        let three = section("8.3").unwrap();
        assert!(!three.body.contains("### 8.4"));
        assert!(guide(Some("8.3"))
            .unwrap()
            .starts_with("8.3 Sources of evidence\n"));
        assert!(guide(Some("9.9")).unwrap_err().to_string().contains("8.3"));
        assert!(info()
            .instructions
            .unwrap()
            .contains("First call: notebook kind \"plan\""));
    }
}
