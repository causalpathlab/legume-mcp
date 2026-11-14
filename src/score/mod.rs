//! `legume score`: a method's scores against a truth, over a universe of
//! items, as AUROC and average precision with the counts beside them
//! ([`crate::metric`]).
//!
//! The universe is every item the method could have scored. It is always
//! given: a method that scores few items would otherwise be judged on
//! those alone, and a pruned output looks perfect. An item in the universe
//! the method left unscored ranks at the bottom; a truth item outside the
//! universe is refused (or dropped and counted); a scored item outside it
//! is dropped and counted.

mod cli;

pub use cli::{main, USAGE};

use crate::command::{usage, Exit};
use crate::metric;
use crate::table::{self, Value};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The output's columns, in order: the header, and the cells of each
/// [`ScoreRow`].
pub const COLUMNS: [&str; 14] = [
    "score",
    "metric",
    "value",
    "part",
    "n_pos",
    "n_neg",
    "n_universe",
    "pos_rate",
    "tied_share",
    "unscored",
    "unscored_pos",
    "outside_pos",
    "outside_scored",
    "n_inf",
];

/// One line of the output: a score's metric over a part of the universe,
/// and the counts beside it.
struct ScoreRow<'a> {
    score: &'a str,
    metric: &'a str,
    value: f64,
    part: &'a str,
    counts: metric::Counts,
    outside_pos: usize,
    outside_scored: usize,
}

impl ScoreRow<'_> {
    /// The header line, [`COLUMNS`].
    fn header(out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(out, "{}", COLUMNS.join("\t"))
    }

    /// The row as one line, a cell for each of [`COLUMNS`] in its order.
    fn write(&self, out: &mut dyn Write) -> std::io::Result<()> {
        let c = &self.counts;
        let cells: [&dyn std::fmt::Display; COLUMNS.len()] = [
            &self.score,
            &self.metric,
            &self.value,
            &self.part,
            &c.n_pos,
            &c.n_neg,
            &c.n_universe,
            &c.pos_rate,
            &c.tied_share,
            &c.unscored,
            &c.unscored_pos,
            &self.outside_pos,
            &self.outside_scored,
            &c.n_inf,
        ];
        for (i, cell) in cells.iter().enumerate() {
            let tab = if i == 0 { "" } else { "\t" };
            write!(out, "{tab}{cell}")?;
        }
        writeln!(out)
    }
}

/// How a column becomes a score, higher meaning more likely true.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Transform {
    Plain,
    Neg,
    Abs,
    /// `-log10 p`, for a p value in `[0, 1]`; `p == 0` is `+inf`.
    NegLog10,
}

impl Transform {
    fn apply(self, x: f64) -> Result<f64> {
        Ok(match self {
            Transform::Plain => x,
            Transform::Neg => -x,
            Transform::Abs => x.abs(),
            Transform::NegLog10 => {
                if !(0.0..=1.0).contains(&x) && !x.is_nan() {
                    bail!("neglog10 of {x}: a p value is in [0, 1]");
                }
                -x.log10()
            }
        })
    }
}

/// One `--score NAME=[neg:|abs:|neglog10:]COL`.
#[derive(Debug, PartialEq)]
struct Score {
    name: String,
    transform: Transform,
    column: String,
}

/// What to do with items outside the universe.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outside {
    Fail,
    Drop,
}

/// What to do with a key the scores list twice.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Dup {
    Fail,
    Max,
}

#[derive(Debug, PartialEq)]
enum Universe {
    /// Every row of the scores file, which also holds the truth.
    Same,
    File {
        path: PathBuf,
        key: Vec<String>,
    },
}

/// A `legume score` command line, checked.
#[derive(Debug)]
struct Spec {
    scores: PathBuf,
    key: Vec<String>,
    score: Vec<Score>,
    truth: PathBuf,
    truth_key: Vec<String>,
    /// A truth row is positive when this column is above the value; without
    /// it, every truth row is.
    label: Option<(String, f64)>,
    universe: Universe,
    truth_outside: Outside,
    scored_outside: Outside,
    dup: Dup,
    out: PathBuf,
}

/// The universe's items, each once, in the order first seen.
struct Items {
    index: HashMap<String, usize>,
    rows: usize,
}

impl Items {
    fn new(keys: Vec<String>) -> Self {
        let rows = keys.len();
        let mut index = HashMap::with_capacity(rows);
        for k in keys {
            let next = index.len();
            index.entry(k).or_insert(next);
        }
        Items { index, rows }
    }

    fn len(&self) -> usize {
        self.index.len()
    }
}

/// Each row's key: its key columns' cells, joined by tabs.
fn keys(columns: &[Vec<Value>], names: &[String]) -> Result<Vec<String>> {
    let rows = columns.first().map_or(0, Vec::len);
    (0..rows)
        .map(|i| {
            let mut key = String::new();
            for (j, col) in columns.iter().enumerate() {
                if j > 0 {
                    key.push('\t');
                }
                col[i]
                    .push_key(&mut key)
                    .with_context(|| format!("row {} of {}", i + 1, names[j]))?;
            }
            Ok(key)
        })
        .collect()
}

/// The keys of the table at `path`.
fn read_keys(path: &Path, key: &[String]) -> Result<Vec<String>> {
    let names: Vec<&str> = key.iter().map(String::as_str).collect();
    keys(&table::read(path, &names)?, key).with_context(|| format!("keys of {}", path.display()))
}

/// The truth over the universe: which items are positive, and how many
/// positives fall outside.
struct Truth {
    labels: Vec<bool>,
    rows: usize,
    keys: usize,
    outside_pos: usize,
}

fn read_truth(spec: &Spec, items: &Items) -> Result<Truth> {
    let mut want: Vec<&str> = spec.truth_key.iter().map(String::as_str).collect();
    if let Some((col, _)) = &spec.label {
        want.push(col);
    }
    let mut columns = table::read(&spec.truth, &want)?;
    let label = spec.label.as_ref().map(|(col, above)| {
        let values = columns.pop().expect("the label column was read");
        (col, *above, values)
    });
    let keys = keys(&columns, &spec.truth_key)
        .with_context(|| format!("keys of {}", spec.truth.display()))?;
    let mut labels = vec![false; items.len()];
    let mut outside = HashSet::new();
    for (i, key) in keys.iter().enumerate() {
        let positive = match &label {
            None => true,
            Some((col, above, values)) => values[i]
                .number()
                .with_context(|| format!("row {} of {col} in {}", i + 1, spec.truth.display()))?
                .is_some_and(|v| v > *above),
        };
        match items.index.get(key) {
            Some(&at) => labels[at] |= positive,
            None if positive => {
                outside.insert(key.as_str());
            }
            None => {}
        }
    }
    Ok(Truth {
        labels,
        rows: keys.len(),
        keys: keys.iter().collect::<HashSet<_>>().len(),
        outside_pos: outside.len(),
    })
}

/// The scores over the universe, one vector per `--score`.
struct Scored {
    values: Vec<Vec<Option<f64>>>,
    rows: usize,
    outside: usize,
    duplicates: usize,
}

/// The higher of two scores, as the ranking orders them (unscored lowest).
fn higher(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (metric::clean(a), metric::clean(b)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The scores file, read once: each row's key, and each `--score`'s column.
fn read_score_table(spec: &Spec) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
    let mut want: Vec<&str> = spec.key.iter().map(String::as_str).collect();
    want.extend(spec.score.iter().map(|s| s.column.as_str()));
    let mut columns = table::read(&spec.scores, &want)?;
    let scores = columns.split_off(spec.key.len());
    let keys =
        keys(&columns, &spec.key).with_context(|| format!("keys of {}", spec.scores.display()))?;
    Ok((keys, scores))
}

fn read_scores(
    spec: &Spec,
    items: &Items,
    keys: &[String],
    columns: &[Vec<Value>],
) -> Result<Scored> {
    let mut values = vec![vec![None; items.len()]; spec.score.len()];
    let mut seen = vec![false; items.len()];
    let (mut outside, mut duplicates, mut first_dup) = (0, 0, None);
    for (i, key) in keys.iter().enumerate() {
        let mut row = Vec::with_capacity(spec.score.len());
        for (s, col) in spec.score.iter().zip(columns) {
            let at = || format!("row {} of {} in {}", i + 1, s.column, spec.scores.display());
            let x = col[i].number().with_context(at)?;
            row.push(
                x.map(|x| s.transform.apply(x))
                    .transpose()
                    .with_context(at)?,
            );
        }
        let Some(&at) = items.index.get(key) else {
            outside += 1;
            continue;
        };
        if seen[at] {
            duplicates += 1;
            first_dup.get_or_insert(key);
            for (v, x) in values.iter_mut().zip(row) {
                v[at] = higher(v[at], x);
            }
        } else {
            seen[at] = true;
            for (v, x) in values.iter_mut().zip(row) {
                v[at] = x;
            }
        }
    }
    if let (Dup::Fail, Some(key)) = (spec.dup, first_dup) {
        bail!(
            "{duplicates} duplicate keys in {}, the first {key:?}; --dup max keeps each key's \
             highest score",
            spec.scores.display()
        );
    }
    Ok(Scored {
        values,
        rows: keys.len(),
        outside,
        duplicates,
    })
}

/// Two paths that name one file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Score as `spec` says, write its table, and say what was counted on
/// `out` and what to doubt on `err`.
fn run(spec: &Spec, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    if spec.universe == Universe::Same && !same_file(&spec.scores, &spec.truth) {
        return Err(usage(
            "--universe same needs --truth and --scores to be one file; give the universe as a \
             file otherwise",
        ));
    }
    let (score_keys, score_columns) = read_score_table(spec)?;
    let items = match &spec.universe {
        Universe::Same => Items::new(score_keys.clone()),
        Universe::File { path, key } => Items::new(read_keys(path, key)?),
    };
    writeln!(
        out,
        "universe: {} items ({} duplicate rows)",
        items.len(),
        items.rows - items.len()
    )?;

    let truth = read_truth(spec, &items)?;
    let positives = truth.labels.iter().filter(|&&p| p).count();
    writeln!(
        out,
        "truth: {} rows, {} keys, {positives} positive in the universe, {} outside it",
        truth.rows, truth.keys, truth.outside_pos
    )?;
    if truth.outside_pos > 0 && spec.truth_outside == Outside::Fail {
        return Err(Exit {
            code: 3,
            message: format!(
                "{} truth positives are outside the universe; --truth-outside drop leaves them \
                 out, counted as outside_pos",
                truth.outside_pos
            ),
        }
        .into());
    }

    let scored = read_scores(spec, &items, &score_keys, &score_columns)?;
    writeln!(
        out,
        "scores: {} rows, {} outside the universe, {} duplicate keys",
        scored.rows, scored.outside, scored.duplicates
    )?;
    if scored.outside > 0 && spec.scored_outside == Outside::Fail {
        return Err(Exit {
            code: 3,
            message: format!(
                "{} scored rows are outside the universe; --scored-outside drop leaves them out, \
                 counted as outside_scored",
                scored.outside
            ),
        }
        .into());
    }

    let mut rows = Vec::with_capacity(2 * spec.score.len());
    for (s, values) in spec.score.iter().zip(&scored.values) {
        let m = metric::rank(values, &truth.labels);
        let c = m.counts;
        let scored_items = c.n_universe - c.unscored;
        if c.n_inf * 100 > scored_items {
            writeln!(
                err,
                "warning: {} scores {} of its {scored_items} scored items +inf, one tie at the top \
                 (a p value of 0 is +inf under neglog10)",
                s.name, c.n_inf
            )?;
        }
        for (metric, value) in [("auroc", m.auroc), ("auprc", m.average_precision)] {
            rows.push(ScoreRow {
                score: &s.name,
                metric,
                value,
                part: "all",
                counts: c,
                outside_pos: truth.outside_pos,
                outside_scored: scored.outside,
            });
        }
    }
    let mut file = table::TextOut::create(&spec.out)?;
    ScoreRow::header(&mut file)?;
    for r in &rows {
        r.write(&mut file)?;
    }
    file.finish()?;
    writeln!(out, "wrote {}", spec.out.display())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every cell distinct, so a cell written under another column's name
    // reads back wrong.
    #[test]
    fn a_row_writes_each_cell_under_its_column() {
        let row = ScoreRow {
            score: "M1",
            metric: "auroc",
            value: 0.25,
            part: "all",
            counts: metric::Counts {
                n_pos: 1,
                n_neg: 2,
                n_universe: 3,
                pos_rate: 0.5,
                tied_share: 0.75,
                unscored: 4,
                unscored_pos: 5,
                n_inf: 8,
            },
            outside_pos: 6,
            outside_scored: 7,
        };
        let mut text = Vec::new();
        ScoreRow::header(&mut text).unwrap();
        row.write(&mut text).unwrap();
        let text = String::from_utf8(text).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let header: Vec<&str> = lines[0].split('\t').collect();
        assert_eq!(header, COLUMNS);
        let cells: HashMap<&str, &str> = header.into_iter().zip(lines[1].split('\t')).collect();
        let want = [
            ("score", "M1"),
            ("metric", "auroc"),
            ("value", "0.25"),
            ("part", "all"),
            ("n_pos", "1"),
            ("n_neg", "2"),
            ("n_universe", "3"),
            ("pos_rate", "0.5"),
            ("tied_share", "0.75"),
            ("unscored", "4"),
            ("unscored_pos", "5"),
            ("outside_pos", "6"),
            ("outside_scored", "7"),
            ("n_inf", "8"),
        ];
        assert_eq!(cells, HashMap::from(want));
        assert_eq!(lines[1].split('\t').count(), COLUMNS.len());
    }
}
