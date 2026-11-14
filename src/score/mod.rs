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
pub(crate) mod table;

pub use cli::{main, USAGE};

use crate::metric;
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use table::Value;

/// A refusal and the exit code it ends `legume` with: 2 for a command line
/// that cannot run, 3 for a truth or a score outside the universe.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

/// A command line that cannot run, with the usage `usage` it breaks.
pub(crate) fn usage(message: impl std::fmt::Display, usage: &str) -> anyhow::Error {
    Exit {
        code: 2,
        message: format!("{message}\n\n{usage}"),
    }
    .into()
}

/// `what`, or the refusal that `flag` is required.
pub(crate) fn required<T>(what: Option<T>, flag: &str, usage_text: &str) -> Result<T> {
    what.ok_or_else(|| usage(format!("{flag} is required"), usage_text))
}

/// The output's columns, in order.
pub const COLUMNS: &str = "score\tmetric\tvalue\tpart\tn_pos\tn_neg\tn_universe\tpos_rate\t\
                           tied_share\tunscored\tunscored_pos\toutside_pos\toutside_scored\tn_inf";

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
/// `out`.
fn run(spec: &Spec, out: &mut dyn Write) -> Result<()> {
    if spec.universe == Universe::Same && !same_file(&spec.scores, &spec.truth) {
        return Err(usage(
            "--universe same needs --truth and --scores to be one file; give the universe as a \
             file otherwise",
            USAGE,
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

    let mut table = format!("{COLUMNS}\n");
    for (s, values) in spec.score.iter().zip(&scored.values) {
        let metric::Metrics {
            auroc,
            average_precision,
            counts: c,
        } = metric::rank(values, &truth.labels);
        let scored_items = c.n_universe - c.unscored;
        if c.n_inf * 100 > scored_items {
            eprintln!(
                "warning: {} scores {} of its {scored_items} scored items +inf, one tie at the top \
                 (a p value of 0 is +inf under neglog10)",
                s.name, c.n_inf
            );
        }
        for (metric, value) in [("auroc", auroc), ("auprc", average_precision)] {
            table.push_str(&format!(
                "{}\t{metric}\t{value}\tall\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                s.name,
                c.n_pos,
                c.n_neg,
                c.n_universe,
                c.pos_rate,
                c.tied_share,
                c.unscored,
                c.unscored_pos,
                truth.outside_pos,
                scored.outside,
                c.n_inf
            ));
        }
    }
    let mut file = table::TextOut::create(&spec.out)?;
    file.write_all(table.as_bytes())?;
    file.finish()?;
    writeln!(out, "wrote {}", spec.out.display())?;
    Ok(())
}
