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
    evidence, project, Call, Content, Handler, Options, Prompt, Role, ServerInfo, ToolSpec,
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
            "The legume server keeps the project's notebook, the one the legume tools \
             share: the person's plan, their data verdicts, their hypotheses, and how each \
             stands against independent sources of evidence. The tools' own servers run \
             the steps. \
             `guide` lists the sections of the guide to this work and gives any one of \
             them; read the quick card below first. `status` says where the project \
             stands: its data, recent outputs, today's notebook, the hypotheses and what \
             is open for the person; with a `hypothesis`, its triangulation table. \
             `report` makes the day's slides.\n\nQuick card for agents:{card}"
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
/// starts with (`/mcp__legume__explore` in Claude Code).
pub(crate) fn prompts() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "explore",
            "Explore a data set",
            "Work through a data set with the person, step by step: they ask, predict, judge and write; you run the tools, show the facts and ask (guide sections 4 and 13).",
            EXPLORE,
        )
        .arg("question", "What the person wants to know, if anything yet", false)
        .arg("data", "The data to start from: a file or folder under the project", false),
        Prompt::new(
            "triangulate",
            "Triangulate a hypothesis",
            "Test the person's hypothesis against sources that could corroborate or nullify it; they choose and judge each, and the table sets the verb (guide section 8).",
            TRIANGULATE,
        )
        .arg("hypothesis", "The hypothesis, as H1, H2, ... (`status` lists them)", false),
    ]
}

const EXPLORE: &str = "\
Let us explore a data set together, the way the legume guide says (its sections 4 and 13).

Question: {question}
Data: {data}

1. Call `status` (this server) and show me where the project stands, in plain \
words: the data and what is not recorded about it, what was done, the hypotheses, \
and what is open for me. If the project folder looks wrong (source code, a home \
folder), tell me first.
2. Ask me my question, as a contrast (this rather than that), and what result \
would change my mind. Record my words as the `plan`. If I have no question yet, \
ask what I expect to vary in this data.
3. For data whose origin or quality I have not given: run the read-only checks, \
explain what each number measures and what is typical, say what argues against the \
data, and ask me. Record my words as `data`.
4. Work in steps, and stop after each to show me what came out: ask me first what \
I expect, then show it. Read-only tools first; record an `observation` with the \
numbers after each step. Tell me plainly what is running and how long it takes.
5. Never say what a result means. Ask me; when I state a hypothesis, ask what \
else could explain it and what would nullify it, and record my words as a \
`hypothesis`. When I want a figure or table for the paper, record it as a \
`figure` with my number and take-home.
6. At the end, ask me for the day's summary and record my words as `summary`; then \
build the slides with `report` and tell me where the records and the slides are.

Read a guide section with `guide` when a step is unclear.";

const TRIANGULATE: &str = "\
Let us test a hypothesis against independent sources of evidence (the legume guide, section 8).

Hypothesis: {hypothesis}

1. Call `status` (this server): it lists the hypotheses with their standing. If \
no hypothesis was named, show me the list and ask which one.
2. Call `status` with its `hypothesis`: its table, the verb its sources allow now, \
and the kinds of source not yet used. If I have not said what would nullify it, ask \
me before anything else.
3. Tell me, in plain words, which sources could corroborate or nullify it and what \
each shares with our cells; I choose which to pursue, or to stop.
4. Run what I chose and show me the numbers. Ask me whether it agrees, disagrees or \
is mixed, and why; record it as `evidence` with the numbers in `text` and my words \
in `words`. Several tools on the same data are one source. A gene-set or term test \
on our cells is our cells, not curated knowledge.
5. Go back to 2 until I stop.
6. Use only the verb the table allows, and name what disagrees. \"Drives\" needs \
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
                "Where the project stands: its data, recent outputs, today's notebook, the hypotheses with how far each got, and what is open for the person. With `hypothesis`, that hypothesis's triangulation table, the verb it allows and the sources not yet used.",
                json!({
                    "type": "object",
                    "properties": { "hypothesis": { "type": "string", "description": "A hypothesis, as H1, H2, ..." } },
                }),
            )
            .read_only(),
            ToolSpec::new(
                "report",
                "The day's work as slides, built from the records, in results/<day>/report/: report.pdf, the same slides as text in report.md, and the trail of the day's steps in graph.svg. The person's summary of the day, their question, the data, the steps and their numbers, the figures with the commands that made them, what was seen, each hypothesis's triangulation table, and how to replay it. Every word is read from the records or is the person's own; you write none of it. Build it when the person asks for a report or the day's work ends, and show them where it is.",
                json!({
                    "type": "object",
                    "properties": {
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
            "status" => match &args["hypothesis"] {
                Value::Null => status(records(cx)?),
                id => hypothesis(records(cx)?, id)?,
            },
            "report" => {
                let p = records(cx)?;
                let built = crate::report::build(p.root(), args["day"].as_str())?;
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

/// A hypothesis's triangulation table, the verb it allows and what to
/// look at next.
fn hypothesis(p: &Project, id: &Value) -> Result<String> {
    let (hyps, all) = ledger(p);
    let args = json!({ "hypothesis": id });
    let h = evidence::find(&args, &hyps)?;
    Ok(format!(
        "{}\n\n{}",
        evidence::table(h, &all),
        evidence::next(h, &all)
    ))
}

pub(crate) fn status(p: &Project) -> String {
    let root = p.root();
    let r = crate::records::Records::read(root, &project::now().0);
    let mut out = format!("Project: {}\n", root.display());
    out.push_str("\nData (data/):\n");
    if r.data.is_empty() {
        out.push_str("  none yet: the person's data goes in data/\n");
    }
    let listed = 20;
    if r.data.len() > listed {
        let undescribed = r.data.iter().filter(|d| d.from.is_empty()).count();
        out.push_str(&format!(
            "  {} files ({undescribed} not described); the first {listed}:\n",
            r.data.len()
        ));
    }
    for d in r.data.iter().take(listed) {
        let size = match d.bytes {
            0 => String::new(),
            b if b >= 1 << 30 => format!(" ({:.1} GB)", b as f64 / (1u64 << 30) as f64),
            b if b >= 1 << 20 => format!(" ({} MB)", b >> 20),
            b => format!(" ({} kB)", (b >> 10).max(1)),
        };
        let from = match d.from.as_str() {
            "" => "where it came from is not recorded".to_string(),
            f => format!("\"{f}\""),
        };
        let verdict = match d.verdict.as_str() {
            "" => String::new(),
            v => format!("; the person's verdict: {v}"),
        };
        out.push_str(&format!("  {}{size}: {from}{verdict}\n", d.path));
    }
    out.push_str("\nRecent outputs (results/INDEX.tsv), with the key numbers each step made:\n");
    if r.steps.is_empty() {
        out.push_str("  none yet\n");
    }
    for s in &r.steps[r.steps.len().saturating_sub(8)..] {
        out.push_str(&format!(
            "  {} {} {} {}: {} ({})\n",
            s.date, s.time, s.server, s.tool, s.output, s.summary
        ));
        if !s.facts.is_empty() {
            let facts: Vec<String> = s.facts.iter().map(ToString::to_string).collect();
            out.push_str(&format!("      {}\n", facts.join("; ")));
        }
    }
    let flagged: Vec<String> = r
        .steps
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
            "\nFlagged by the tools (look at these with the person before building on them):\n",
        );
        for f in &flagged[flagged.len().saturating_sub(8)..] {
            out.push_str(f);
        }
    }
    out.push_str("\nToday's notebook (results/<today>/notebook.md), latest entries:\n");
    if r.notebook.is_empty() {
        out.push_str("  none yet: start with the person's `plan`\n");
    }
    for e in &r.notebook[r.notebook.len().saturating_sub(8)..] {
        out.push_str(&format!("  {} · {} · {}\n", e.time, e.server, e.kind));
    }
    let (hyps, all) = ledger(p);
    out.push_str("\nHypotheses (results/HYPOTHESES.tsv, EVIDENCE.tsv):\n");
    if hyps.is_empty() {
        out.push_str("  none yet\n");
    }
    for h in &hyps {
        out.push_str(&format!("  {}\n", evidence::standing(h, &all)));
    }
    if !r.figures.is_empty() {
        out.push_str("\nKept for the paper (results/FIGURES.tsv):\n");
        for f in &r.figures {
            let claim = match f.hypothesis.as_str() {
                "" => String::new(),
                h => format!(", for {h}"),
            };
            out.push_str(&format!(
                "  {}: {}{claim}: \"{}\"\n",
                f.number, f.output, f.take_home
            ));
        }
    }
    let open = r.open();
    if !open.is_empty() {
        out.push_str("\nOpen for the person (ask, in plain words):\n");
        for o in open {
            out.push_str(&format!("  {o}\n"));
        }
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
            .contains("First, call `status` on the legume server"));
    }
}
