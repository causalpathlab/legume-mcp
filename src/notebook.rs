//! The lab notebook: the built-in `notebook` tool of every server with
//! records, for the plan, the observations and notes, and the full one of
//! the `legume` server, which also keeps the reasoning across tools: the
//! readings an agent proposes, the user's answers, the evidence for each
//! hypothesis ([`crate::evidence`]), and the offers and choices.

use anyhow::Result;
use serde_json::{json, Value};

use crate::project::{self, Project};
use crate::{evidence, ToolSpec};

/// What every server tells the agent of the records.
pub(crate) const RECORDS_NOTE: &str = "\
Records (after Noble, PLoS Comput Biol 2009, doi:10.1371/journal.pcbi.1000424), \
under the project {root}: outputs you do not name go to results/<today>/; every \
step that writes is added to results/<today>/runall.sh, which repeats the work \
without you; results/INDEX.tsv lists each output with a summary. Use the \
`notebook` tool: before starting, a `plan` (the question, the data, and how the \
result will be checked); then what you did and saw, as `observation`s. ";

/// What a tool's server adds: the reasoning is the `legume` server's.
pub(crate) const TOOL_NOTE: &str = "\
What the results mean is the user's to say: propose readings, and record the \
user's answers and the evidence for each hypothesis, in the notebook of the \
`legume` server (its `guide` says how; if it is not connected, ask the user to \
add it: `legume mcp`). Tell the user where the records are when the work is done.";

/// What the `legume` server adds: how the reasoning is kept.
pub(crate) const REASONING_NOTE: &str = "\
What the results mean is the user's to say: propose two or more `readings` in an \
`interpretation` (each a claim, its evidence, the alternatives and why not, a \
rationale, and the check that would tell it from the others; they are numbered \
R1, R2, ...), then ask the user which hold, so a number or a short line is an \
answer (as a multiple-choice question when your client offers one), and record \
their reply as an `answer`: `chose` with the numbers of the readings they chose \
(none when they chose none) and `words` with their reply exactly as they gave it. \
A reply that does not say which readings hold (\"ok\", \"continue\") is not an \
answer: ask again. The readings they choose become hypotheses H1, H2, ... for the \
whole project. Test each against sources of evidence that share little with the \
data it came from (other donors, another study, genetics, a perturbation; several \
tools on the same cells are one source), and record what each source says of it as \
`evidence`: the result is its triangulation table, the verb it allows, and the \
sources not yet used. Other choices you put to the user (which sources next, \
whether to look further) go in an `offer` of numbered `options`, and their reply in \
a `choice`, the same way. Tell the user where the records are when the work is done.";

/// The kinds a tool's own notebook takes; the others are the `legume`
/// server's.
const TOOL_KINDS: [&str; 3] = ["plan", "observation", "note"];

/// The kinds the `legume` server's notebook takes (and `conclusion`, the
/// old name of `interpretation`).
const KINDS: [&str; 8] = [
    "plan",
    "observation",
    "interpretation",
    "answer",
    "evidence",
    "offer",
    "choice",
    "note",
];

/// The `notebook` tool: a tool server's (`full` false), for the plan,
/// observations and notes; the `legume` server's, for the reasoning too.
pub(crate) fn notebook_spec(full: bool) -> ToolSpec {
    if !full {
        return ToolSpec::new(
            "notebook",
            "Add an entry to today's lab notebook (results/<today>/notebook.md, shared by every legume server): your account of the work, kept beside the facts the tools record. Kinds: `plan` (first: the question, the data, how the result will be checked), `observation` (what you did and saw, with numbers), `note` (anything else; `by` user for the user's own words). The entry is `text`. Readings, the user's answers, evidence, offers and choices go to the `legume` server's notebook.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": TOOL_KINDS },
                    "text": { "type": "string", "description": "The entry, in Markdown" },
                    "by": {
                        "type": "string",
                        "enum": ["agent", "user"],
                        "description": "agent (when left out) for your own; user for the user's own words, as they gave them.",
                    },
                },
                "required": ["kind", "text"],
            }),
        );
    }
    ToolSpec::new(
        "notebook",
        "Add an entry to today's lab notebook (results/<today>/notebook.md, shared by every legume server): your account of the work, kept beside the facts the tools record. Kinds, in the order of a piece of work: `plan` (first), `observation` (what you did and saw), `interpretation` (two or more `readings` you propose), `answer` (the user's reply to them: `chose` and `words`; the readings chosen become hypotheses H1, H2, ... for the whole project), `evidence` (what one source of evidence says of one hypothesis: `hypothesis`, `source`, `class`, `verdict`, and its numbers in `text`; returns the hypothesis's triangulation table, the verb it allows and the sources not yet used). Other choices you put to the user (which sources next, whether to look further) are an `offer` of numbered `options`, and their reply a `choice` (`chose` and `words`). `note` for anything else. A plan, observation or note is `text`.",
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["plan", "observation", "interpretation", "answer", "evidence", "offer", "choice", "note"] },
                "hypothesis": { "type": "string", "description": "Evidence: the hypothesis it bears on, as H1, H2, ..." },
                "source": { "type": "string", "description": "Evidence: a name for the data it comes from, the same each time, e.g. \"our cells, donors 1-4\", \"held-out donors 5-6\", a study's accession. Several tools on the same data are one source." },
                "class": {
                    "type": "string",
                    "enum": evidence::CLASSES.iter().map(|c| c.0).collect::<Vec<_>>(),
                    "description": "Evidence: the kind of source, from most to least shared with the data the hypothesis came from: our_cells (that data, any tool, a term or gene-set test on it too), same_cells (another measurement of those cells: ATAC, alleles, protein), same_donors (another sample of those donors: a spatial section, another tissue), other_donors (donors held out from the same study), other_study (published or another lab's data), genetics (genotypes, eQTL, GWAS), perturbation (knockouts, screens, drugs), curated (what earlier studies established: known gene sets, the Cell Ontology, papers).",
                },
                "verdict": { "type": "string", "enum": evidence::VERDICTS, "description": "Evidence: what the source says of the hypothesis: agrees, disagrees, or mixed (it agrees in part: some donors, some of its analyses)." },
                "text": { "type": "string", "description": "The entry, in Markdown; for evidence, what the source shows, with numbers and thresholds" },
                "readings": {
                    "type": "array",
                    "minItems": 2,
                    "description": "An agent's interpretation: the readings it proposes, numbered R1, R2, ... in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "claim": { "type": "string" },
                            "evidence": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": { "kind": { "type": "string" }, "term": { "type": "string" } },
                                    "required": ["kind", "term"],
                                },
                                "description": "Typed items, as {kind: marker|term|figure|statistic|..., term}",
                            },
                            "alternatives": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": { "label": { "type": "string" }, "why_not": { "type": "string" } },
                                    "required": ["label", "why_not"],
                                },
                            },
                            "rationale": { "type": "string" },
                            "check": { "type": "string", "description": "What would tell this reading from the others" },
                        },
                        "required": ["claim", "rationale", "check"],
                    },
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "items": { "type": "string" },
                    "description": "An offer: the choices put to the user, numbered 1, 2, ... in order; stopping can be one.",
                },
                "chose": {
                    "type": "array",
                    "items": { "type": "integer", "minimum": 1 },
                    "description": "An answer or a choice: the numbers the user chose (readings for an answer, options for a choice); leave it out when they chose none.",
                },
                "words": {
                    "type": "string",
                    "description": "An answer or a choice: the user's reply exactly as they gave it.",
                },
                "by": {
                    "type": "string",
                    "enum": ["agent", "user"],
                    "description": "A note or observation: agent (when left out) for your own; user for the user's own words, as they gave them. An answer or a choice needs none.",
                },
            },
            "required": ["kind"],
        }),
    )
    .writes()
}

/// Who decided a notebook entry: lupin's `decided_by` values, shared by
/// every legume tool.
const DECIDED_BY: [&str; 4] = [
    "agent",
    "agent_proposed_user_accepted",
    "user_override",
    "user",
];

/// A notebook entry: its heading label, its markdown, and what the call
/// tells the agent, if anything.
type Entry = (String, String, Option<String>);

/// A notebook call's entry, with what it adds to the project's ledger
/// ([`evidence`]); a tool server's notebook (`full` false) takes only its
/// own kinds. An agent's interpretation is its proposed readings, two or
/// more; the others say whose they are. `conclusion` is the old name of
/// `interpretation` and, as before, takes text alone.
pub(crate) fn notebook_entry(args: &Value, p: &Project, full: bool) -> Result<Entry> {
    let Some(kind) = args["kind"].as_str() else {
        anyhow::bail!("`kind` is required");
    };
    if !full && !TOOL_KINDS.contains(&kind) && kind != "conclusion" {
        anyhow::bail!(
            "kind `{kind}` goes to the `legume` server's notebook, which keeps the readings, \
             the user's answers and the evidence across tools; this one takes {}. If the \
             `legume` server is not connected, ask the user to add it (`legume mcp`)",
            TOOL_KINDS.join(", ")
        );
    }
    // The kind names the entry's heading: only these, so none claims to be
    // the user's or starts another entry.
    if full && !KINDS.contains(&kind) && kind != "conclusion" {
        anyhow::bail!("`kind` is one of {}, not `{kind}`", KINDS.join(", "));
    }
    match kind {
        "answer" => return reply_entry(Asked::Readings, args, p),
        "choice" => return reply_entry(Asked::Options, args, p),
        "offer" => return offer_entry(args).map(|(l, t)| (l, t, None)),
        "evidence" => return evidence_entry(args, p),
        _ => {}
    }
    if !args["options"].is_null() {
        anyhow::bail!("`options` go with kind `offer`");
    }
    if !args["hypothesis"].is_null() || !args["verdict"].is_null() {
        anyhow::bail!("`hypothesis` and `verdict` go with kind `evidence`");
    }
    let legacy = kind == "conclusion";
    let kind = if legacy { "interpretation" } else { kind };
    let by = args["by"].as_str().unwrap_or("agent");
    // A tool's notebook takes the agent's own entries and the user's words;
    // who accepted a reading is the `legume` server's to record.
    let decided: &[&str] = if full {
        &DECIDED_BY
    } else {
        &["agent", "user"]
    };
    if !decided.contains(&by) {
        anyhow::bail!("`by` is one of {}, not `{by}`", decided.join(", "));
    }
    let readings = match &args["readings"] {
        Value::Null => vec![],
        Value::Array(r) => r.iter().map(reading).collect::<Result<Vec<_>>>()?,
        _ => anyhow::bail!("`readings` is a list"),
    };
    let proposing = kind == "interpretation" && by == "agent";
    if proposing && !legacy && readings.len() < 2 {
        anyhow::bail!(
            "an agent's interpretation proposes two or more `readings`, each {{claim, evidence?, \
             alternatives?, rationale, check}}; the user says which hold"
        );
    }
    let mut text = args["text"]
        .as_str()
        .unwrap_or_default()
        .trim_end()
        .to_string();
    for (i, r) in readings.iter().enumerate() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let (claim, lines) = r;
        text.push_str(&format!("**R{}. {claim}**\n{lines}", i + 1));
    }
    if text.is_empty() {
        anyhow::bail!("`text` or `readings` is required");
    }
    let label = match by {
        "agent" if proposing => "interpretation (proposed)".to_string(),
        "agent" => kind.to_string(),
        "agent_proposed_user_accepted" => format!("{kind} (accepted by the user)"),
        "user_override" => format!("{kind} (user override)"),
        _ => format!("{kind} (user)"),
    };
    Ok((label, text, None))
}

/// What one source of evidence says of one hypothesis, added to the
/// project's ledger; the agent is given the hypothesis's table, the verb it
/// allows, and the sources it has not used.
fn evidence_entry(args: &Value, p: &Project) -> Result<Entry> {
    let hyps = evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES));
    let mut all = evidence::evidence(&p.read_ledger(evidence::EVIDENCE));
    let (h, e) = evidence::check(args, &hyps, &all)?;
    p.ledger(evidence::EVIDENCE, &[e.cells()]);
    let head = format!(
        "H{} from {} ({}): {}.\n\n{}",
        h.id,
        e.source,
        e.class.replace('_', " "),
        e.verdict,
        e.summary
    );
    all.push(e);
    let table = evidence::table(h, &all);
    let text = format!("{head}\n\n{table}");
    let reply = format!("{table}\n\n{}", evidence::next(h, &all));
    Ok(("evidence".into(), text, Some(reply)))
}

/// What the user was last asked today: readings to choose among (an
/// agent's `interpretation`), or other options (an `offer`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Asked {
    Readings,
    Options,
}

impl Asked {
    /// The reply kind, what it was asked in, and what its numbers are.
    fn names(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Asked::Readings => ("answer", "proposal", "reading"),
            Asked::Options => ("choice", "offer", "option"),
        }
    }
}

/// The user's reply to the question it answers, today's latest: the
/// numbers they chose (`chose`) and their reply as they gave it (`words`).
/// An `answer` replies to a proposal: readings chosen, it was accepted;
/// none, the user set it aside for their own. A `choice` replies to an
/// offer.
fn reply_entry(asked: Asked, args: &Value, p: &Project) -> Result<Entry> {
    let (reply, question, number) = asked.names();
    let today = p.notebook_today();
    let Some((last, time, server, items)) = latest_question(&today) else {
        anyhow::bail!(
            "there is no {question} today to {reply}; record the user's own words as a \
             `note` with `by` `user`"
        );
    };
    if waiting(&today).is_none() {
        let (_, theirs, _) = last.names();
        anyhow::bail!(
            "the {theirs} of {time} ({server}) has its reply already, and no question waits \
             now: record the user's words as a `note` with `by` `user`, or put a new \
             question to them"
        );
    }
    if last != asked {
        let (right, theirs, _) = last.names();
        anyhow::bail!(
            "the latest question today is the {theirs} of {time} ({server}): record the \
             reply to it as `{right}` (a reply to an earlier question is a `note` with \
             `by` `user`)"
        );
    }
    let words = args["words"].as_str().map(str::trim).unwrap_or_default();
    anyhow::ensure!(
        !words.is_empty(),
        "`words` is required: the user's reply exactly as they gave it"
    );
    let count = items.len();
    let given: &[Value] = match &args["chose"] {
        Value::Null => &[],
        Value::Array(a) => a,
        // One number, as a small model may send it.
        one @ Value::Number(_) => std::slice::from_ref(one),
        other => anyhow::bail!(
            "`chose` is a list of {number} numbers of the {question} of {time} (1 to \
             {count}), not {other}; leave it out when the user chose none"
        ),
    };
    let mut chose: Vec<usize> = Vec::new();
    for n in given {
        match n.as_u64() {
            Some(n) if (1..=count as u64).contains(&n) => {
                if !chose.contains(&(n as usize)) {
                    chose.push(n as usize);
                }
            }
            _ => anyhow::bail!(
                "`chose` takes {number} numbers of the {question} of {time} (1 to {count}), \
                 not {n}"
            ),
        }
    }
    let head = format!("Answers the {question} of {time} ({server})");
    let mut told = None;
    let (label, what) = match (asked, chose.is_empty()) {
        (Asked::Readings, true) => (
            "interpretation (user override)",
            format!("{head}: set aside."),
        ),
        (Asked::Readings, false) => {
            let named = hypotheses_from(p, &chose, &items, &time, &server);
            let names: Vec<String> = chose.iter().map(|n| format!("R{n}")).collect();
            told = Some(format!(
                "{}. Record what each source of evidence says of a hypothesis as `evidence`.",
                named.join(", ")
            ));
            (
                "interpretation (accepted by the user)",
                format!(
                    "{head}: chose {}. Hypotheses: {}.",
                    names.join(", "),
                    named.join(", ")
                ),
            )
        }
        (Asked::Options, true) => ("choice (user)", format!("{head}: chose none.")),
        (Asked::Options, false) => {
            let names: Vec<String> = chose.iter().map(ToString::to_string).collect();
            (
                "choice (user)",
                format!("{head}: chose {}.", names.join(", ")),
            )
        }
    };
    let quoted: Vec<String> = words.lines().map(|l| format!("> {l}")).collect();
    Ok((
        label.into(),
        format!("{what}\n\n{}", quoted.join("\n")),
        told,
    ))
}

/// The chosen readings as the project's hypotheses, `R1 = H4`: a claim
/// already among them keeps its number, a new one is added to the ledger.
fn hypotheses_from(
    p: &Project,
    chose: &[usize],
    claims: &[String],
    time: &str,
    server: &str,
) -> Vec<String> {
    // The claims numbered so far, with those this answer adds.
    let mut known: Vec<(usize, String)> =
        evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES))
            .into_iter()
            .map(|h| (h.id, h.claim))
            .collect();
    let mut next = known.iter().map(|h| h.0).max().unwrap_or(0);
    let mut rows = Vec::new();
    let named = chose
        .iter()
        .map(|&n| {
            let claim = &claims[n - 1];
            let id = match known.iter().find(|h| &h.1 == claim) {
                Some(h) => h.0,
                None => {
                    next += 1;
                    rows.push(vec![
                        format!("H{next}"),
                        format!("R{n} of the proposal of {time} ({server})"),
                        claim.clone(),
                    ]);
                    known.push((next, claim.clone()));
                    next
                }
            };
            format!("R{n} = H{id}")
        })
        .collect();
    if !rows.is_empty() {
        p.ledger(evidence::HYPOTHESES, &rows);
    }
    named
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

/// The last question in `notebook` a reply can answer: a proposal with
/// numbered readings (an old `conclusion` has none) or an offer, with its
/// time, its server, and its readings' claims or its options, in order.
fn latest_question(notebook: &str) -> Option<(Asked, String, String, Vec<String>)> {
    entries(notebook).into_iter().rev().find_map(question)
}

/// The question `entry` asks, if it is one.
fn question(entry: &str) -> Option<(Asked, String, String, Vec<String>)> {
    let (head, body) = entry.split_once('\n')?;
    let mut head = head.splitn(3, " · ");
    let (time, server, label) = (head.next()?, head.next()?, head.next()?);
    let (asked, prefix) = match label {
        "interpretation (proposed)" => (Asked::Readings, "**R"),
        "offer" => (Asked::Options, "- **"),
        _ => return None,
    };
    let items: Vec<String> = body
        .lines()
        .filter_map(|l| {
            let r = l.strip_prefix(prefix)?;
            let digits = r.len() - r.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let item = r[digits..].strip_prefix('.').filter(|_| digits > 0)?;
            // `**R1. claim**`, `- **1.** option`
            let item = item.strip_prefix("**").unwrap_or(item).trim();
            Some(item.strip_suffix("**").unwrap_or(item).trim().to_string())
        })
        .collect();
    (!items.is_empty()).then(|| (asked, time.to_string(), server.to_string(), items))
}

/// Today's last question with no reply after it, if any.
fn waiting(notebook: &str) -> Option<(Asked, String, String, Vec<String>)> {
    for entry in entries(notebook).into_iter().rev() {
        let head = entry.lines().next().unwrap_or_default();
        let label = head.splitn(3, " · ").nth(2).unwrap_or_default();
        if label.starts_with("interpretation (accepted")
            || label.starts_with("interpretation (user override")
            || label == "choice (user)"
        {
            return None;
        }
        // An old `conclusion` asks nothing: an older question may wait.
        if let Some(q) = question(entry) {
            return Some(q);
        }
    }
    None
}

/// The question in `notebook` still waiting for the user's reply, if any:
/// today's last proposal or offer with no reply after it.
pub(crate) fn open_question(notebook: &str) -> Option<String> {
    let (asked, time, server, items) = waiting(notebook)?;
    let (reply, question, _) = asked.names();
    Some(format!(
        "the {question} of {time} ({server}), to record as `{reply}`: {}",
        numbered(asked, &items, &[]).join(" / ")
    ))
}

/// The waiting question's items, numbered as the user sees them (only
/// `chose` when it names any).
fn numbered(asked: Asked, items: &[String], chose: &[usize]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .filter(|(i, _)| chose.is_empty() || chose.contains(&(i + 1)))
        .map(|(i, t)| match asked {
            Asked::Readings => format!("R{}. {t}", i + 1),
            Asked::Options => format!("{}. {t}", i + 1),
        })
        .collect()
}

/// A reply choosing `chose` of the question waiting in `notebook`, in
/// words, for the user to confirm: what they chose, or that they chose
/// none of it.
#[cfg_attr(not(feature = "chat"), allow(dead_code))]
pub(crate) fn reply_in_words(notebook: &str, chose: &[usize]) -> Option<String> {
    let (asked, time, _, items) = waiting(notebook)?;
    let (_, question, number) = asked.names();
    if chose.is_empty() {
        return Some(format!("none of the {number}s of the {question} of {time}"));
    }
    let named = numbered(asked, items.as_slice(), chose);
    (!named.is_empty()).then(|| named.join(" / "))
}

/// An agent's offer: choices put to the user that are not readings (which
/// step next, whether to look further), numbered 1, 2, ... for a `choice`.
fn offer_entry(args: &Value) -> Result<(String, String)> {
    let options: Vec<String> = match &args["options"] {
        Value::Array(o) => o
            .iter()
            .map(|o| match o.as_str().map(str::trim) {
                Some(o) if !o.is_empty() => Ok(project::one_line(o)),
                _ => anyhow::bail!("each of `options` is a line of text"),
            })
            .collect::<Result<_>>()?,
        _ => vec![],
    };
    anyhow::ensure!(
        options.len() >= 2,
        "an offer puts two or more `options` to the user (stopping can be one)"
    );
    let mut text = args["text"]
        .as_str()
        .unwrap_or_default()
        .trim_end()
        .to_string();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    for (i, o) in options.iter().enumerate() {
        text.push_str(&format!("- **{}.** {o}\n", i + 1));
    }
    Ok(("offer".into(), text))
}

/// One proposed reading, checked: its claim, and the lines that go
/// under it.
fn reading(r: &Value) -> Result<(String, String)> {
    let field = |name: &str| -> Result<String> {
        match r[name].as_str().map(str::trim) {
            Some(s) if !s.is_empty() => Ok(project::one_line(s)),
            _ => anyhow::bail!(
                "each reading needs `claim`, `rationale` and `check`; `{name}` is missing"
            ),
        }
    };
    let claim = field("claim")?;
    let mut lines = String::new();
    let evidence: Vec<String> = r["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .map(
            |e| match (e["kind"].as_str(), e["term"].as_str(), e.as_str()) {
                (Some(k), Some(t), _) => format!("{k} {t}"),
                (_, _, Some(s)) => s.to_string(),
                _ => e.to_string(),
            },
        )
        .collect();
    if !evidence.is_empty() {
        lines.push_str(&format!(
            "- Evidence: {}\n",
            project::one_line(&evidence.join("; "))
        ));
    }
    for a in r["alternatives"].as_array().into_iter().flatten() {
        let label = a["label"].as_str().unwrap_or_default();
        let why = a["why_not"].as_str().unwrap_or_default();
        lines.push_str(&format!(
            "- Not {}: {}\n",
            project::one_line(label),
            project::one_line(why)
        ));
    }
    lines.push_str(&format!("- Rationale: {}\n", field("rationale")?));
    lines.push_str(&format!("- Check: {}", field("check")?));
    Ok((claim, lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: &str = "# Notebook, 2026-10-28\n\nThe agents' account.\n\
\n## 10:00:00 · legume · offer\n\nNext?\n\n- **1.** Held-out donors\n- **2.** Stop\n\
\n## 10:01:00 · legume · interpretation (proposed)\n\nContext:\n## Background\nmore\n\n\
**R1. IFN drives topic 7**\n- Rationale: r\n\n**R2. Donor 2 carries it**\n- Rationale: r\n";

    #[test]
    fn a_heading_in_an_entrys_text_stays_in_the_entry() {
        let e = entries(DAY);
        assert_eq!(e.len(), 2, "{e:?}");
        assert!(
            e[1].contains("## Background") && e[1].contains("**R2."),
            "{}",
            e[1]
        );
        let (asked, time, _, items) = latest_question(DAY).unwrap();
        assert_eq!((asked, time.as_str()), (Asked::Readings, "10:01:00"));
        assert_eq!(items, ["IFN drives topic 7", "Donor 2 carries it"]);
    }

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let p = Project::new(dir.path().to_path_buf(), "legume", "0");
        (dir, p)
    }

    /// The entry `args` makes, recorded as the server records it.
    fn record(p: &Project, args: Value, full: bool) -> Result<String> {
        let (label, text, _) = notebook_entry(&args, p, full)?;
        p.notebook(&label, &text);
        Ok(label)
    }

    #[test]
    fn no_entry_claims_to_be_the_users_by_its_kind_or_a_tools_by() {
        let (_dir, p) = project();
        for kind in [
            "interpretation (accepted by the user)",
            "plan\n## 10:00:00 · x · offer",
        ] {
            let e = record(&p, json!({ "kind": kind, "text": "t" }), true).unwrap_err();
            assert!(e.to_string().contains("`kind` is one of"), "{e}");
        }
        let accepted =
            json!({ "kind": "conclusion", "by": "agent_proposed_user_accepted", "text": "t" });
        let e = record(&p, accepted.clone(), false).unwrap_err();
        assert!(e.to_string().contains("`by` is one of agent, user"), "{e}");
        // The `legume` server takes lupin's values.
        assert_eq!(
            record(&p, accepted, true).unwrap(),
            "interpretation (accepted by the user)"
        );
    }

    #[test]
    fn a_question_is_answered_once() {
        let (_dir, p) = project();
        let reading = |c: &str| json!({ "claim": c, "rationale": "r", "check": "c" });
        let propose = json!({ "kind": "interpretation", "readings": [reading("A"), reading("B")] });
        record(&p, propose, true).unwrap();
        assert_eq!(reply_in_words(&p.notebook_today(), &[2]).unwrap(), "R2. B");
        let answer = json!({ "kind": "answer", "chose": [1], "words": "1" });
        record(&p, answer.clone(), true).unwrap();
        assert!(reply_in_words(&p.notebook_today(), &[2]).is_none());
        let e = record(&p, answer, true).unwrap_err();
        assert!(e.to_string().contains("has its reply already"), "{e}");
    }

    #[test]
    fn an_old_conclusion_does_not_hide_an_open_offer() {
        let day = format!(
            "{}\n## 10:02:00 · senna · interpretation (proposed)\n\nTopic 7 is IFN.\n",
            &DAY[..DAY.find("\n## 10:01").unwrap()]
        );
        let open = open_question(&day).unwrap();
        assert!(open.starts_with("the offer of 10:00:00 (legume)"), "{open}");
    }
}
