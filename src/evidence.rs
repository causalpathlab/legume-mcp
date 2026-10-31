//! The project's triangulation ledger. The person's hypotheses are numbered
//! H1, H2, ... for the whole project (`results/HYPOTHESES.tsv`), each with
//! the rivals they named and what would nullify it; what each source of
//! evidence says of one, and why the person judged it so, is a row of
//! `results/EVIDENCE.tsv`. From them the notebook builds a hypothesis's
//! table, the verb its agreeing sources allow, and the kinds of source not
//! yet used, so neither has to be worked out by the agent.

use anyhow::Result;
use serde_json::Value;

use crate::project::one_line;

/// The ledger's files under `results/`, and their columns after the date,
/// time and server.
pub(crate) const HYPOTHESES: (&str, &str) = ("HYPOTHESES.tsv", "id\tfrom\tclaim\trivals\tnullify");
pub(crate) const EVIDENCE: (&str, &str) = (
    "EVIDENCE.tsv",
    "hypothesis\tsource\tclass\tverdict\tsummary\twhy",
);

/// The kinds of source, from most to least shared with the data a
/// hypothesis came from: name, what it is, what it shares with our cells.
pub(crate) const CLASSES: [(&str, &str, &str); 8] = [
    ("our_cells", "our cells", "everything"),
    (
        "same_cells",
        "the same cells, another measurement",
        "cells, donors",
    ),
    ("same_donors", "the same donors, another sample", "donors"),
    ("other_donors", "other donors, same study", "lab, platform"),
    ("other_study", "another study", "tissue, species"),
    ("genetics", "genetics", "donors, or nothing"),
    ("perturbation", "perturbation", "nothing"),
    ("curated", "curated knowledge", "nothing directly"),
];

pub(crate) const VERDICTS: [&str; 3] = ["agrees", "disagrees", "mixed"];

/// What `class` is, and what it shares with our cells, in words.
pub(crate) fn what(class: &str) -> (&'static str, &'static str) {
    CLASSES
        .iter()
        .find(|c| c.0 == class)
        .map_or(("", ""), |c| (c.1, c.2))
}

/// The classes of our own cells and donors: agreement among them is one
/// group of cells, never a replication.
fn ours(class: &str) -> bool {
    matches!(class, "our_cells" | "same_cells" | "same_donors")
}

/// A hypothesis, as the person put it: its number, claim, the rivals they
/// named and what would nullify it (empty when not said yet).
#[derive(Clone)]
pub(crate) struct Hypothesis {
    pub(crate) id: usize,
    pub(crate) claim: String,
    pub(crate) rivals: String,
    pub(crate) nullify: String,
}

/// One row of evidence.
#[derive(Clone)]
pub(crate) struct Evidence {
    pub(crate) hypothesis: usize,
    pub(crate) source: String,
    pub(crate) class: String,
    pub(crate) verdict: String,
    pub(crate) summary: String,
    /// Why the person judged it so, in their words.
    pub(crate) why: String,
}

impl Evidence {
    /// Its cells for [`EVIDENCE`].
    pub(crate) fn cells(&self) -> Vec<String> {
        vec![
            format!("H{}", self.hypothesis),
            self.source.clone(),
            self.class.clone(),
            self.verdict.clone(),
            self.summary.clone(),
            self.why.clone(),
        ]
    }
}

/// The rows of a ledger, each split into the cells after the date, time
/// and server.
fn rows(tsv: &str) -> impl Iterator<Item = Vec<&str>> {
    tsv.lines()
        .skip(1)
        .map(|l| l.split('\t').skip(3).collect::<Vec<_>>())
}

/// `H3`, `h3`, `3` or 3 as 3.
fn number(v: &Value) -> Option<usize> {
    match v {
        Value::Number(n) => n.as_u64().map(|n| n as usize),
        Value::String(s) => s.trim().trim_start_matches(['H', 'h']).parse().ok(),
        _ => None,
    }
}

/// The hypotheses in `HYPOTHESES.tsv`, in order of their numbers; a
/// hypothesis revised is its latest row.
pub(crate) fn hypotheses(tsv: &str) -> Vec<Hypothesis> {
    let mut out: Vec<Hypothesis> = Vec::new();
    for r in rows(tsv) {
        let Some(id) = r.first().and_then(|c| number(&Value::String((*c).into()))) else {
            continue;
        };
        let cell = |i: usize| r.get(i).copied().unwrap_or_default().to_string();
        let h = Hypothesis {
            id,
            claim: cell(2),
            rivals: cell(3),
            nullify: cell(4),
        };
        match out.iter_mut().find(|o| o.id == id) {
            Some(o) => *o = h,
            None => out.push(h),
        }
    }
    out
}

/// The evidence in `EVIDENCE.tsv`.
pub(crate) fn evidence(tsv: &str) -> Vec<Evidence> {
    rows(tsv)
        .filter_map(|r| {
            Some(Evidence {
                hypothesis: number(&Value::String((*r.first()?).into()))?,
                source: (*r.get(1)?).into(),
                class: (*r.get(2)?).into(),
                verdict: (*r.get(3)?).into(),
                summary: r.get(4).copied().unwrap_or_default().into(),
                why: r.get(5).copied().unwrap_or_default().into(),
            })
        })
        .collect()
}

/// The hypothesis `args` names (`hypothesis`), among `hyps`.
pub(crate) fn find<'h>(args: &Value, hyps: &'h [Hypothesis]) -> Result<&'h Hypothesis> {
    let listed = || match hyps.last() {
        Some(h) => format!("H1 to H{}", h.id),
        None => "none yet".into(),
    };
    match number(&args["hypothesis"]) {
        Some(n) => hyps.iter().find(|h| h.id == n).ok_or_else(|| {
            anyhow::anyhow!(
                "there is no hypothesis H{n} (the hypotheses are {}, in \
                 results/HYPOTHESES.tsv); the person's hypothesis is recorded as kind \
                 `hypothesis`",
                listed()
            )
        }),
        None => anyhow::bail!(
            "`hypothesis` is required: the hypothesis, as H1, H2, ... ({})",
            listed()
        ),
    }
}

/// The evidence a notebook call gives, checked against the ledger.
pub(crate) fn check<'h>(
    args: &Value,
    hyps: &'h [Hypothesis],
    known: &[Evidence],
) -> Result<(&'h Hypothesis, Evidence)> {
    let h = find(args, hyps)?;
    let text = |name: &str, what: &str| -> Result<String> {
        match args[name].as_str().map(str::trim) {
            Some(s) if !s.is_empty() => Ok(one_line(s)),
            _ => anyhow::bail!("`{name}` is required: {what}"),
        }
    };
    let source = text(
        "source",
        "a name for the data this comes from, e.g. \"held-out donors 5-6\"",
    )?;
    let class = text("class", "the kind of source")?;
    let names: Vec<&str> = CLASSES.iter().map(|c| c.0).collect();
    anyhow::ensure!(
        names.contains(&class.as_str()),
        "`class` is one of {}, not `{class}`",
        names.join(", ")
    );
    if let Some(e) = known
        .iter()
        .find(|e| e.source == source && e.class != class)
    {
        anyhow::bail!(
            "source \"{source}\" was recorded as class `{}`; one source keeps one class (name \
             another source differently)",
            e.class
        );
    }
    let verdict = text("verdict", "agrees, disagrees or mixed")?;
    anyhow::ensure!(
        VERDICTS.contains(&verdict.as_str()),
        "`verdict` is one of {}, not `{verdict}`",
        VERDICTS.join(", ")
    );
    let summary = text(
        "text",
        "what this source shows, with its numbers and thresholds",
    )?;
    let why = text(
        "words",
        "why the person judges that it agrees, disagrees or is mixed, in their words; \
         show them the numbers and ask",
    )?;
    let e = Evidence {
        hypothesis: h.id,
        source,
        class,
        verdict,
        summary,
        why,
    };
    Ok((h, e))
}

/// Each source's latest word on hypothesis `h`, in the order the sources
/// were first used: one source counts once.
fn by_source(h: usize, all: &[Evidence]) -> Vec<Evidence> {
    let mut out: Vec<Evidence> = Vec::new();
    for e in all.iter().filter(|e| e.hypothesis == h) {
        match out.iter_mut().find(|o| o.source == e.source) {
            Some(o) => *o = e.clone(),
            None => out.push(e.clone()),
        }
    }
    out
}

/// How far the agreeing sources take a claim, as section 8.3 of the
/// collaboration paper sets it out: 0 nothing, 1 one group of cells,
/// 2 replicated in cells that share none, 3 and genetics or a perturbation.
fn level(sources: &[Evidence]) -> u8 {
    let agree: Vec<&Evidence> = sources
        .iter()
        .filter(|e| e.verdict == "agrees" && e.class != "curated")
        .collect();
    let groups = agree.iter().any(|e| ours(&e.class)) as usize
        + agree
            .iter()
            .filter(|e| matches!(e.class.as_str(), "other_donors" | "other_study"))
            .count();
    let causal = agree
        .iter()
        .any(|e| matches!(e.class.as_str(), "genetics" | "perturbation"));
    match (groups, causal) {
        (2.., true) => 3,
        (2.., false) => 2,
        _ if !agree.is_empty() => 1,
        _ => 0,
    }
}

/// The kinds of source `sources` have not used, those that would raise
/// the verb first.
fn unused(sources: &[Evidence]) -> Vec<&'static str> {
    let order: [&'static str; 8] = match level(sources) {
        0 | 1 => [
            "our_cells",
            "other_donors",
            "other_study",
            "same_cells",
            "same_donors",
            "genetics",
            "perturbation",
            "curated",
        ],
        _ => [
            "our_cells",
            "genetics",
            "perturbation",
            "other_study",
            "other_donors",
            "same_cells",
            "same_donors",
            "curated",
        ],
    };
    order
        .into_iter()
        .filter(|c| !sources.iter().any(|e| e.class == *c))
        .collect()
}

/// A hypothesis as a screen draws it: each source's latest word, the
/// level (0 to 3) and standing they give it, and the kinds of source not
/// yet used, those that would raise it first.
pub(crate) struct Ledger {
    pub(crate) sources: Vec<Evidence>,
    pub(crate) level: u8,
    /// The verb the agreeing sources allow, and where that leaves the claim.
    pub(crate) verb: String,
    pub(crate) standing: &'static str,
    pub(crate) unused: Vec<&'static str>,
}

pub(crate) fn ledger(h: &Hypothesis, all: &[Evidence]) -> Ledger {
    let sources = by_source(h.id, all);
    let (verb, standing) = verb(&sources);
    Ledger {
        level: level(&sources),
        verb,
        standing,
        unused: unused(&sources),
        sources,
    }
}

/// The verb the agreeing sources allow, and where that leaves the claim.
fn verb(sources: &[Evidence]) -> (String, &'static str) {
    let only_ours = sources
        .iter()
        .filter(|e| e.verdict == "agrees" && e.class != "curated")
        .all(|e| ours(&e.class));
    match level(sources) {
        3 => (
            "\"affects\", \"drives\"".into(),
            "replicated, with genetics or a perturbation",
        ),
        2 => (
            "\"is associated with\"".into(),
            "replicated: sources that share no cells agree",
        ),
        1 if only_ours => (
            "\"marks\" (when only clusters and markers agree) or \"is associated with, in \
             these donors\" (a contrast across donors or conditions)"
                .into(),
            "one group of cells, not replicated",
        ),
        1 => (
            "\"is associated with\", in that source only".into(),
            "one source, not replicated",
        ),
        _ if sources.iter().any(|e| e.verdict == "agrees") => (
            "\"is consistent with earlier studies\"".into(),
            "curated knowledge only, which does not raise the verb",
        ),
        _ => (
            "none: say what was seen, not what it means".into(),
            "no source agrees",
        ),
    }
}

/// The hypothesis's triangulation table and the verb it allows.
pub(crate) fn table(h: &Hypothesis, all: &[Evidence]) -> String {
    let sources = by_source(h.id, all);
    let mut out = format!(
        "**H{}. {}**\n\n| Source | Kind | Shares with our cells (by kind) | Says | Evidence | Why, in the person's words |\n|---|---|---|---|---|---|\n",
        h.id, h.claim
    );
    for e in &sources {
        let (kind, shares) = what(&e.class);
        out.push_str(&format!(
            "| {} | {kind} | {shares} | {} | {} | {} |\n",
            cell(&e.source),
            e.verdict,
            cell(&e.summary),
            cell(&e.why)
        ));
    }
    let (verb, standing) = verb(&sources);
    out.push_str(&format!("\nVerb: {verb}. Standing: {standing}."));
    let against: Vec<String> = sources
        .iter()
        .filter(|e| e.verdict != "agrees")
        .map(|e| format!("{} ({})", e.source, e.verdict))
        .collect();
    if !against.is_empty() {
        out.push_str(&format!(
            " Name with the verb what does not agree: {}.",
            against.join(", ")
        ));
    }
    if level(&sources) < 2
        && sources.iter().any(|e| {
            e.verdict == "agrees" && matches!(e.class.as_str(), "genetics" | "perturbation")
        })
    {
        out.push_str(" Genetics or a perturbation agrees, but the association is not replicated yet: \"affects\" needs both.");
    }
    out
}

/// One line on hypothesis `h`: its claim, how far it got, and its sources.
pub(crate) fn standing(h: &Hypothesis, all: &[Evidence]) -> String {
    let sources = by_source(h.id, all);
    let (_, standing) = verb(&sources);
    let count = |v: &str| sources.iter().filter(|e| e.verdict == v).count();
    format!(
        "H{}. {} ({standing}; sources: {} agree, {} disagree, {} mixed)",
        h.id,
        h.claim,
        count("agrees"),
        count("disagrees"),
        count("mixed")
    )
}

/// The kinds of source hypothesis `h` has not used, those that would raise
/// its verb first, for the agent to offer.
pub(crate) fn next(h: &Hypothesis, all: &[Evidence]) -> String {
    let sources = by_source(h.id, all);
    let unused: Vec<String> = unused(&sources)
        .into_iter()
        .map(|c| format!("{} (`{c}`)", what(c).0))
        .collect();
    let mut out = String::new();
    if !unused.is_empty() {
        out.push_str(&format!(
            "Sources not yet used for H{}, those that would raise the verb first: {}. ",
            h.id,
            unused.join("; ")
        ));
    }
    if level(&sources) < 2 {
        out.push_str(
            "Another study or other donors can replicate it; another source of the same \
             kind counts too, when it shares no cells. ",
        );
    }
    if h.nullify.is_empty() {
        out.push_str(&format!(
            "The person has not said what would nullify H{}: ask before the next source. ",
            h.id
        ));
    }
    out.push_str(
        "Tell the person, in plain words, which sources could corroborate or nullify it and \
         what each shares with our cells; they choose which to pursue, or to stop.",
    );
    out
}

/// `s` fit for a markdown table cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(source: &str, class: &str, verdict: &str) -> Evidence {
        Evidence {
            hypothesis: 1,
            source: source.into(),
            class: class.into(),
            verdict: verdict.into(),
            summary: String::new(),
            why: String::new(),
        }
    }

    #[test]
    fn the_verb_climbs_only_with_sources_that_share_no_cells() {
        let ours = e("donors 1-4", "our_cells", "agrees");
        let atac = e("ATAC", "same_cells", "agrees");
        let held = e("donors 5-6", "other_donors", "agrees");
        let gwas = e("eQTL", "genetics", "agrees");
        let go = e("GO", "curated", "agrees");
        assert_eq!(level(&[]), 0);
        assert_eq!(level(std::slice::from_ref(&go)), 0);
        assert_eq!(level(&[ours.clone(), atac.clone()]), 1);
        assert_eq!(level(&[ours.clone(), gwas.clone()]), 1);
        assert_eq!(level(&[ours.clone(), held.clone()]), 2);
        assert_eq!(level(&[held.clone(), e("GEO", "other_study", "agrees")]), 2);
        assert_eq!(level(&[ours.clone(), held.clone(), gwas]), 3);
        assert_eq!(
            level(&[ours, e("donors 5-6", "other_donors", "disagrees")]),
            1
        );
    }

    #[test]
    fn a_source_counts_once_with_its_latest_word() {
        let all = [
            e("donors 5-6", "other_donors", "agrees"),
            e("donors 1-4", "our_cells", "agrees"),
            e("donors 5-6", "other_donors", "disagrees"),
        ];
        let s = by_source(1, &all);
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].source.as_str(), s[0].verdict.as_str()),
            ("donors 5-6", "disagrees")
        );
        assert_eq!(level(&s), 1);
    }

    #[test]
    fn a_revised_hypothesis_is_its_latest_row() {
        let tsv = format!(
            "date\ttime\tserver\t{}\n\
             d\tt\tlegume\tH1\tyou\tstress\t\t\n\
             d\tt\tlegume\tH2\tyou\ta donor\t\t\n\
             d\tt\tlegume\tH1\tyou\tstress\tdamaged cells\tno heat shock in sample 2\n",
            HYPOTHESES.1
        );
        let h = hypotheses(&tsv);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].id, h[0].rivals.as_str()), (1, "damaged cells"));
        assert_eq!(h[0].nullify, "no heat shock in sample 2");
        assert_eq!(h[1].claim, "a donor");
    }

    #[test]
    fn hypotheses_are_named_however_the_agent_writes_them() {
        for v in [
            json(r#""H3""#),
            json(r#""h3""#),
            json("3"),
            json(r#"" 3 ""#),
        ] {
            assert_eq!(number(&v), Some(3));
        }
        assert_eq!(number(&json(r#""R3x""#)), None);
    }

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }
}
