//! The lab notebook: the `legume` server's `notebook` tool, the one notebook
//! of a project. The agent records what was done and seen; everything that
//! interprets is the person's, in their own words: their plan, where data
//! came from and whether it is good, their hypotheses with the rivals they
//! named and what would nullify each, their judgement of each source of
//! evidence, the figures they keep, and the summary of their day.

use anyhow::Result;
use serde_json::{json, Value};

use crate::project::{self, Project};
use crate::{evidence, ToolSpec};

/// What every server tells the agent of the records.
pub(crate) const RECORDS_NOTE: &str = "\
Records (after Noble, PLoS Comput Biol 2009, doi:10.1371/journal.pcbi.1000424), \
under the project {root}: outputs you do not name go to results/<today>/; every \
step that writes is added to results/<today>/runall.sh, which repeats the work \
without you; results/INDEX.tsv lists each output with a summary, its inputs and \
its key numbers. ";

/// What a tool's server adds: the notebook is the `legume` server's.
pub(crate) const TOOL_NOTE: &str = "\
The notebook is the `legume` server's (`legume init` connects it): record there \
the person's plan, what you did and saw, and everything the person decides, in \
their words. Speak to the person plainly: what is running and about how long, \
not job ids; when they can watch a step, the command that shows it.";

/// What the `legume` server adds: who writes what.
pub(crate) const REASONING_NOTE: &str = "\
The person thinks; you run the tools, show the facts, ask, and check. Never write \
what a result means, a hypothesis, a verdict, a figure's take-home or a summary for \
them: ask, and record their words as they said them (`words`). Explain what a \
number measures and what is typical; what it means here is theirs to say. Ask them \
to predict before you show a result. When they state a hypothesis, ask what else \
could explain it and what would nullify it. Tell the person where the records are \
when the work is done.";

/// The kinds of entry.
const KINDS: [&str; 8] = [
    "plan",
    "data",
    "observation",
    "hypothesis",
    "evidence",
    "figure",
    "summary",
    "note",
];

/// The data the person described, under `results/`: each path, their
/// verdict and why.
pub(crate) const DATA: (&str, &str) = ("DATA.tsv", "path\tverdict\twhy");
/// The figures and tables the person kept, under `results/`.
pub(crate) const FIGURES: (&str, &str) = ("FIGURES.tsv", "number\toutput\thypothesis\ttake_home");

/// A data verdict.
const DATA_VERDICTS: [&str; 3] = ["good", "caveats", "bad"];

/// The `legume` server's `notebook` tool.
pub(crate) fn notebook_spec() -> ToolSpec {
    ToolSpec::new(
        "notebook",
        "Add an entry to today's lab notebook (results/<today>/notebook.md). You write `observation`s (what you did and saw, with numbers) and `note`s. Every other kind is the person's, in their own words (`words`), never yours: `plan` (their question, and what would change their mind), `data` (where a file came from, and their verdict on it), `hypothesis` (their claim, the rivals they name, what would nullify it; numbered H1, H2, ...; give `hypothesis` to revise one), `evidence` (what one source says of a hypothesis, and why they judge it so), `figure` (an output they keep for the paper, its number and take-home), `summary` (their day: what changed, what argues against it, what is next).",
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": KINDS },
                "words": { "type": "string", "description": "The person's own words, as they said them: their plan, where data came from, their claim, why they judge evidence so, a figure's take-home, their summary." },
                "text": { "type": "string", "description": "Yours: an observation or note; for evidence, what the source shows, with numbers and thresholds; for a plan, the data and checks, as facts." },
                "path": { "type": "string", "description": "Data: the file or folder in the project" },
                "verdict": { "type": "string", "description": "Data: good, caveats or bad. Evidence: agrees, disagrees or mixed. The person's, never yours." },
                "hypothesis": { "type": "string", "description": "Evidence, or a figure's claim, or a hypothesis to revise: H1, H2, ..." },
                "rivals": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Hypothesis: what else could explain it, as the person named it",
                },
                "nullify": { "type": "string", "description": "Hypothesis: the result that would make the person drop it, in their words" },
                "source": { "type": "string", "description": "Evidence: a name for the data it comes from, the same each time, e.g. \"held-out donors 5-6\". Several tools on the same data are one source." },
                "class": {
                    "type": "string",
                    "enum": evidence::CLASSES.iter().map(|c| c.0).collect::<Vec<_>>(),
                    "description": "Evidence: the kind of source, from most to least shared with the data the hypothesis came from: our_cells, same_cells (another measurement of those cells), same_donors (another sample of those donors), other_donors (held out from the same study), other_study, genetics, perturbation, curated (what earlier studies established). Say it to the person in plain words.",
                },
                "output": { "type": "string", "description": "Figure: the image or table kept, as INDEX.tsv lists it" },
                "number": { "type": "string", "description": "Figure: its number in the paper, as the person gives it, e.g. 2B or S1" },
                "by": { "type": "string", "enum": ["agent", "user"], "description": "Note: agent (when left out), or user for the person's own words" },
            },
            "required": ["kind"],
        }),
    )
    .writes()
}

/// A notebook entry: its heading label, its markdown, and what the call
/// tells the agent, if anything.
type Entry = (String, String, Option<String>);

/// A notebook call's entry, with what it adds to the project's ledgers.
pub(crate) fn notebook_entry(args: &Value, p: &Project) -> Result<Entry> {
    let kind = args["kind"].as_str().unwrap_or_default();
    anyhow::ensure!(
        KINDS.contains(&kind),
        "`kind` is one of {}, not `{kind}`",
        KINDS.join(", ")
    );
    match kind {
        "plan" => {
            let words = words(
                args,
                "the person's question as they put it, and what would change their mind",
            )?;
            let facts = optional(args, "text");
            Ok(("plan".into(), joined(&quote(&words), &facts), None))
        }
        "observation" => Ok((
            "observation".into(),
            required(args, "text", "what you did and saw, with numbers")?,
            None,
        )),
        "note" => {
            let text = required(args, "text", "the note")?;
            match args["by"].as_str().unwrap_or("agent") {
                "agent" => Ok(("note".into(), text, None)),
                "user" => Ok(("note (user)".into(), quote(&text), None)),
                by => anyhow::bail!("`by` is agent or user, not `{by}`"),
            }
        }
        "summary" => {
            let words = words(
                args,
                "the person's summary of the day: what changed, what argues against it, what is next",
            )?;
            Ok(("summary".into(), quote(&words), None))
        }
        "data" => data_entry(args, p),
        "hypothesis" => hypothesis_entry(args, p),
        "evidence" => evidence_entry(args, p),
        _ => figure_entry(args, p),
    }
}

/// Field `name`, required: one or more lines.
fn required(args: &Value, name: &str, what: &str) -> Result<String> {
    match args[name].as_str().map(str::trim) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => anyhow::bail!("`{name}` is required: {what}"),
    }
}

/// Field `name`, if given.
fn optional(args: &Value, name: &str) -> String {
    args[name]
        .as_str()
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// The person's words, required: never the agent's account of them.
fn words(args: &Value, what: &str) -> Result<String> {
    required(
        args,
        "words",
        &format!("{what}, in their own words; ask them, and record what they say"),
    )
}

/// `text` quoted, as the person's.
pub(crate) fn quote(text: &str) -> String {
    text.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn joined(a: &str, b: &str) -> String {
    if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}\n\n{b}")
    }
}

/// `path` as the project names it: inside it, and there.
fn in_project(p: &Project, path: &str) -> Result<String> {
    let rel = p.rel(path.trim());
    anyhow::ensure!(
        !rel.starts_with('/') && !rel.split('/').any(|c| c == ".."),
        "`{path}` is not inside the project"
    );
    anyhow::ensure!(p.root().join(&rel).exists(), "`{rel}` is not there");
    Ok(rel)
}

/// Where a file came from, and the person's verdict on it.
fn data_entry(args: &Value, p: &Project) -> Result<Entry> {
    let path = in_project(p, &required(args, "path", "the data's file or folder")?)?;
    let words = words(args, "where the data came from, and what they think of it")?;
    let verdict = optional(args, "verdict");
    anyhow::ensure!(
        verdict.is_empty() || DATA_VERDICTS.contains(&verdict.as_str()),
        "a data `verdict` is one of {}, not `{verdict}`",
        DATA_VERDICTS.join(", ")
    );
    p.provenance("notebook", &p.root().join(&path), &words);
    p.ledger(DATA, &[vec![path.clone(), verdict.clone(), words.clone()]]);
    let head = match verdict.as_str() {
        "" => format!("`{path}`"),
        v => format!("`{path}`, verdict: {v}"),
    };
    let told = verdict.is_empty().then(|| {
        format!(
            "Recorded where {path} came from. Its verdict is not given: show the person \
             the quality checks, say what argues against the data, and ask whether it is \
             good, has caveats, or is bad."
        )
    });
    Ok(("data".into(), format!("{head}\n\n{}", quote(&words)), told))
}

/// The person's hypothesis, new or revised, numbered for the project.
fn hypothesis_entry(args: &Value, p: &Project) -> Result<Entry> {
    let hyps = evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES));
    let rivals: Vec<String> = match &args["rivals"] {
        Value::Null => Vec::new(),
        Value::String(s) => vec![s.trim().to_string()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => anyhow::bail!("`rivals` is a list of what else could explain it"),
    };
    let rivals = project::one_line(&rivals.join("; "));
    let nullify = project::one_line(&optional(args, "nullify"));
    let claim = args["words"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (h, label) = if args["hypothesis"].is_null() {
        let Some(claim) = claim else {
            anyhow::bail!(
                "`words` is required: the person's claim, as they put it (this rather than \
                 that); ask them, and record what they say"
            );
        };
        let id = hyps.iter().map(|h| h.id).max().unwrap_or(0) + 1;
        let h = evidence::Hypothesis {
            id,
            claim: project::one_line(claim),
            rivals,
            nullify,
        };
        (h, format!("hypothesis H{id}"))
    } else {
        let old = evidence::find(args, &hyps)?.clone();
        let h = evidence::Hypothesis {
            id: old.id,
            claim: claim.map_or(old.claim, project::one_line),
            rivals: if rivals.is_empty() {
                old.rivals
            } else {
                rivals
            },
            nullify: if nullify.is_empty() {
                old.nullify
            } else {
                nullify
            },
        };
        let label = format!("hypothesis H{} (revised)", h.id);
        (h, label)
    };
    p.ledger(
        evidence::HYPOTHESES,
        &[vec![
            format!("H{}", h.id),
            format!("the person, notebook {}", project::now().1),
            h.claim.clone(),
            h.rivals.clone(),
            h.nullify.clone(),
        ]],
    );
    let mut text = quote(&h.claim);
    if !h.rivals.is_empty() {
        text.push_str(&format!("\n\nRivals they named: {}", h.rivals));
    }
    if !h.nullify.is_empty() {
        text.push_str(&format!("\n\nWould nullify it: {}", h.nullify));
    }
    let mut told = format!("Recorded as H{}.", h.id);
    if h.rivals.is_empty() {
        told.push_str(
            " Ask the person what else could explain it, and record their answer as \
             `rivals` (revise with `hypothesis`).",
        );
    }
    if h.nullify.is_empty() {
        told.push_str(
            " Ask what result would make them drop it, before looking further; record it \
             as `nullify`.",
        );
    }
    Ok((label, text, Some(told)))
}

/// What one source says of one hypothesis, and why the person judges it so;
/// the agent is given the hypothesis's table, the verb it allows, and the
/// sources not yet used.
fn evidence_entry(args: &Value, p: &Project) -> Result<Entry> {
    let hyps = evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES));
    let mut all = evidence::evidence(&p.read_ledger(evidence::EVIDENCE));
    let (h, e) = evidence::check(args, &hyps, &all)?;
    p.ledger(evidence::EVIDENCE, &[e.cells()]);
    let head = format!(
        "H{} from {} ({}): {}.\n\n{}\n\n{}",
        h.id,
        e.source,
        evidence::what(&e.class).0,
        e.verdict,
        e.summary,
        quote(&e.why)
    );
    let label = format!("evidence H{}", h.id);
    all.push(e);
    let table = evidence::table(h, &all);
    let text = format!("{head}\n\n{table}");
    let reply = format!("{table}\n\n{}", evidence::next(h, &all));
    Ok((label, text, Some(reply)))
}

/// An output the person keeps for the paper: its number, its claim and
/// its take-home.
fn figure_entry(args: &Value, p: &Project) -> Result<Entry> {
    let output = in_project(p, &required(args, "output", "the image or table to keep")?)?;
    let number = project::one_line(&required(
        args,
        "number",
        "its number in the paper, as the person gives it (2B, S1)",
    )?);
    let take_home = words(args, "the figure's take-home, what a reader should see")?;
    let hypothesis = match &args["hypothesis"] {
        Value::Null => String::new(),
        _ => {
            let hyps = evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES));
            format!("H{}", evidence::find(args, &hyps)?.id)
        }
    };
    p.ledger(
        FIGURES,
        &[vec![
            number.clone(),
            output.clone(),
            hypothesis.clone(),
            project::one_line(&take_home),
        ]],
    );
    let mut text = format!("`{output}`");
    if !hypothesis.is_empty() {
        text.push_str(&format!(", for {hypothesis}"));
    }
    text.push_str(&format!("\n\n{}", quote(&take_home)));
    let told = hypothesis.is_empty().then(|| {
        format!(
            "Kept as {number}. It supports no hypothesis yet: ask the person which claim it \
             supports, and how strongly."
        )
    });
    Ok((format!("figure {number}"), text, told))
}

/// The entries of a day's notebook, each from its heading's text
/// (`10:00:00 · server · kind`) on. A `## ` line within an entry's own text
/// is part of it: a heading is `## ` and a time, then ` · `.
pub(crate) fn entries(notebook: &str) -> Vec<&str> {
    let is_head = project::is_entry_head;
    let mut starts = Vec::new();
    let mut at = 0;
    for line in notebook.split_inclusive('\n') {
        if is_head(line) {
            starts.push(at + 3);
        }
        at += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &a)| {
            let end = starts.get(i + 1).map_or(notebook.len(), |&b| b - 3);
            notebook[a..end].trim_end_matches('\n')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let p = Project::new(dir.path().to_path_buf(), "legume", "0");
        (dir, p)
    }

    /// The entry `args` makes, recorded as the server records it.
    fn record(p: &Project, args: Value) -> Result<(String, Option<String>)> {
        let (label, text, told) = notebook_entry(&args, p)?;
        p.notebook(&label, &text);
        Ok((label, told))
    }

    #[test]
    fn the_persons_kinds_take_only_their_words() {
        let (_dir, p) = project();
        for kind in ["plan", "summary", "hypothesis"] {
            let e = record(&p, json!({ "kind": kind, "text": "the agent's version" })).unwrap_err();
            assert!(e.to_string().contains("`words` is required"), "{kind}: {e}");
        }
        for kind in ["interpretation", "answer", "offer", "choice", "conclusion"] {
            let e = record(&p, json!({ "kind": kind, "text": "t" })).unwrap_err();
            assert!(e.to_string().contains("`kind` is one of"), "{kind}: {e}");
        }
        let e = record(
            &p,
            json!({ "kind": "note", "text": "t", "by": "agent_proposed_user_accepted" }),
        )
        .unwrap_err();
        assert!(e.to_string().contains("`by` is agent or user"), "{e}");
        record(
            &p,
            json!({ "kind": "plan", "words": "Why is cluster 4 apart, rather than with 2?" }),
        )
        .unwrap();
        assert!(p
            .notebook_today()
            .contains(" · legume · plan\n\n> Why is cluster 4 apart"));
    }

    #[test]
    fn a_hypothesis_asks_for_its_rivals_and_what_would_nullify_it() {
        let (_dir, p) = project();
        let (label, told) = record(
            &p,
            json!({ "kind": "hypothesis", "words": "Cluster 4 is stressed monocytes" }),
        )
        .unwrap();
        assert_eq!(label, "hypothesis H1");
        let told = told.unwrap();
        assert!(
            told.contains("what else could explain it") && told.contains("make them drop it"),
            "{told}"
        );
        let (label, told) = record(
            &p,
            json!({ "kind": "hypothesis", "hypothesis": "H1", "rivals": ["damaged cells", "a sample artefact"], "nullify": "no heat-shock genes in sample 2" }),
        )
        .unwrap();
        assert_eq!(label, "hypothesis H1 (revised)");
        assert_eq!(told.unwrap(), "Recorded as H1.");
        let h = evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES));
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].claim, "Cluster 4 is stressed monocytes");
        assert_eq!(h[0].rivals, "damaged cells; a sample artefact");
        let (label, _) = record(
            &p,
            json!({ "kind": "hypothesis", "words": "Donor 2 carries cluster 4" }),
        )
        .unwrap();
        assert_eq!(label, "hypothesis H2");
    }

    #[test]
    fn evidence_is_judged_by_the_person() {
        let (_dir, p) = project();
        record(
            &p,
            json!({ "kind": "hypothesis", "words": "IFN drives topic 7" }),
        )
        .unwrap();
        let mut ev = json!({ "kind": "evidence", "hypothesis": "H1", "source": "donors 5-6", "class": "other_donors", "verdict": "agrees", "text": "5 of 6 higher" });
        let e = record(&p, ev.clone()).unwrap_err();
        assert!(e.to_string().contains("`words` is required"), "{e}");
        ev["words"] = json!("held-out donors show it too");
        let (label, told) = record(&p, ev).unwrap();
        assert_eq!(label, "evidence H1");
        let told = told.unwrap();
        assert!(told.contains("| held-out donors show it too |"), "{told}");
        assert!(
            told.contains("has not said what would nullify H1"),
            "{told}"
        );
    }

    #[test]
    fn data_and_figures_are_the_persons_and_in_the_project() {
        let (dir, p) = project();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(dir.path().join("data/a.h5"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("results")).unwrap();
        std::fs::write(dir.path().join("results/umap.png"), "x").unwrap();
        let e = record(
            &p,
            json!({ "kind": "data", "path": "../etc/passwd", "words": "w" }),
        )
        .unwrap_err();
        assert!(e.to_string().contains("not inside the project"), "{e}");
        let (_, told) = record(
            &p,
            json!({ "kind": "data", "path": "data/a.h5", "words": "10x PBMC from the core" }),
        )
        .unwrap();
        assert!(told.unwrap().contains("ask whether it is good"));
        record(
            &p,
            json!({ "kind": "data", "path": "data/a.h5", "words": "fine, but one donor", "verdict": "caveats" }),
        )
        .unwrap();
        let readme = std::fs::read_to_string(dir.path().join("data/README.md")).unwrap();
        assert!(
            readme.contains("`data/a.h5`: 10x PBMC from the core"),
            "{readme}"
        );
        let (label, told) = record(
            &p,
            json!({ "kind": "figure", "output": "results/umap.png", "number": "2B", "words": "cluster 4 sits apart" }),
        )
        .unwrap();
        assert_eq!(label, "figure 2B");
        assert!(told.unwrap().contains("supports no hypothesis yet"));
        let figures = p.read_ledger(FIGURES);
        assert!(
            figures.contains("\t2B\tresults/umap.png\t\tcluster 4 sits apart"),
            "{figures}"
        );
    }

    #[test]
    fn a_heading_in_an_entrys_text_stays_in_the_entry() {
        let day = "# Notebook\n\n## 10:00:00 · legume · plan\n\n> Why?\n## Background\nmore\n\n\
                   ## 10:01:00 · legume · observation\n\nsaw\n";
        let e = entries(day);
        assert_eq!(e.len(), 2, "{e:?}");
        assert!(e[0].contains("## Background"), "{}", e[0]);
    }
}
