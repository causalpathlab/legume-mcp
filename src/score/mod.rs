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
use crate::table::{self, Keyed, Numbers};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

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

impl Spec {
    /// The column `--label` names.
    fn label_column(&self) -> Option<&str> {
        self.label.as_ref().map(|(col, _)| col.as_str())
    }
}

/// The universe's items, each once, in the order first seen.
struct Items<'a> {
    index: HashMap<&'a str, usize>,
    rows: usize,
}

impl<'a> Items<'a> {
    /// The items of the rows `keys`; with `rows`, each row's item pushed
    /// onto it as it is found.
    fn new(keys: &'a [String], mut rows: Option<&mut Vec<Option<usize>>>) -> Self {
        let mut index = HashMap::with_capacity(keys.len());
        for k in keys {
            let next = index.len();
            let at = *index.entry(k.as_str()).or_insert(next);
            if let Some(rows) = rows.as_deref_mut() {
                rows.push(Some(at));
            }
        }
        Items {
            index,
            rows: keys.len(),
        }
    }

    /// Each of `keys`' item; `None` outside the universe.
    fn find(&self, keys: &[String]) -> Vec<Option<usize>> {
        keys.iter()
            .map(|k| self.index.get(k.as_str()).copied())
            .collect()
    }

    fn len(&self) -> usize {
        self.index.len()
    }
}

/// The truth over the universe: which items are positive, and how many
/// positives fall outside.
struct Truth {
    labels: Vec<bool>,
    rows: usize,
    keys: usize,
    outside_pos: usize,
}

/// The truth of the rows `keys`, each at its item `at` among `items`
/// items: each row positive or, with `--label`, positive when its cell of
/// `label` is above the value.
fn truth_of(
    spec: &Spec,
    keys: &[String],
    at: &[Option<usize>],
    label: Option<&Numbers>,
    items: usize,
) -> Result<Truth> {
    let label = match (&spec.label, label) {
        (Some((col, above)), Some(values)) => {
            if let Some((row, cell)) = values.first_bad(|_| true) {
                bail!(
                    "row {} of {col} in {}: {cell:?} is not a number",
                    row + 1,
                    spec.truth.display()
                );
            }
            Some((&values.values, *above))
        }
        _ => None,
    };
    let (mut labels, mut seen) = (vec![false; items], vec![false; items]);
    // Each key outside the universe, and whether it is positive.
    let mut outside: HashMap<&str, bool> = HashMap::new();
    for (i, (key, &at)) in keys.iter().zip(at).enumerate() {
        let positive = label.is_none_or(|(values, above)| values[i] > above);
        match at {
            Some(at) => {
                labels[at] |= positive;
                seen[at] = true;
            }
            None => *outside.entry(key.as_str()).or_default() |= positive,
        }
    }
    Ok(Truth {
        labels,
        rows: keys.len(),
        keys: seen.iter().filter(|&&s| s).count() + outside.len(),
        outside_pos: outside.values().filter(|&&p| p).count(),
    })
}

/// The score rows over the universe: each row's item, `None` outside it.
struct Scored {
    at: Vec<Option<usize>>,
    outside: usize,
    duplicates: usize,
}

/// The score rows `keys` at their items `at` among `items` items, a key
/// listed twice refused unless `--dup max`.
fn scored_rows(
    spec: &Spec,
    keys: &[String],
    at: Vec<Option<usize>>,
    items: usize,
) -> Result<Scored> {
    let mut seen = vec![false; items];
    let (mut duplicates, mut first_dup) = (0, None);
    for (key, &at) in keys.iter().zip(&at) {
        if let Some(at) = at {
            if std::mem::replace(&mut seen[at], true) {
                duplicates += 1;
                first_dup.get_or_insert(key);
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
        outside: at.iter().filter(|a| a.is_none()).count(),
        at,
        duplicates,
    })
}

/// The higher of two scores, as the ranking orders them (unscored lowest).
fn higher(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (metric::clean(a), metric::clean(b)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// The score `s` over the universe, into `values` (one an item), from its
/// column `col`, each row at its item `at`. A row outside the universe is
/// never read, so a cell it holds cannot fail the run.
fn score_of(
    spec: &Spec,
    s: &Score,
    col: &Numbers,
    at: &[Option<usize>],
    values: &mut [Option<f64>],
) -> Result<()> {
    let whence = |row: usize| {
        format!(
            "row {} of {} in {}",
            row + 1,
            s.column,
            spec.scores.display()
        )
    };
    if let Some((row, cell)) = col.first_bad(|row| at[row].is_some()) {
        bail!("{}: {cell:?} is not a number", whence(row));
    }
    values.fill(None);
    for (row, (&at, &x)) in at.iter().zip(&col.values).enumerate() {
        // NaN is no score, so leaves the item as it was.
        let Some(at) = at.filter(|_| !x.is_nan()) else {
            continue;
        };
        let x = s.transform.apply(x).with_context(|| whence(row))?;
        values[at] = higher(values[at], Some(x));
    }
    Ok(())
}

/// The refusal of `count` `what` outside the universe under `policy`
/// fail (`flag` drop leaves them out, counted as `column`): exit 3.
fn outside(count: usize, policy: Outside, what: &str, flag: &str, column: &str) -> Result<()> {
    if count > 0 && policy == Outside::Fail {
        return Err(Exit {
            code: 3,
            message: format!(
                "{count} {what} are outside the universe; {flag} drop leaves them out, counted \
                 as {column}"
            ),
        }
        .into());
    }
    Ok(())
}

/// The universe `spec` names, the truth over it and the score rows' items,
/// from the scores read, saying what it counted on `out`: the score
/// columns, the truth and the rows. The keys are left here, so only those
/// are held while the scores are ranked.
fn over_universe(
    spec: &Spec,
    scores: Keyed,
    out: &mut dyn Write,
) -> Result<(Vec<Numbers>, Truth, Scored)> {
    let Keyed { keys, mut numbers } = scores;
    let same_label = numbers.split_off(spec.score.len());
    let same = spec.universe == Universe::Same;
    let (universe_keys, truth_table);
    let mut own = Vec::new();
    let (items, truth_keys, truth_label) = match &spec.universe {
        Universe::Same => {
            own.reserve(keys.len());
            (Items::new(&keys, Some(&mut own)), &keys, same_label.first())
        }
        Universe::File { path, key } => {
            universe_keys = table::read_keyed(path, key, &[])?.keys;
            truth_table =
                table::read_keyed(&spec.truth, &spec.truth_key, spec.label_column().as_slice())?;
            (
                Items::new(&universe_keys, None),
                &truth_table.keys,
                truth_table.numbers.first(),
            )
        }
    };
    writeln!(
        out,
        "universe: {} items ({} duplicate rows)",
        items.len(),
        items.rows - items.len()
    )?;

    let at = if same { own } else { items.find(&keys) };
    let found;
    let truth_at = if same {
        &at
    } else {
        found = items.find(truth_keys);
        &found
    };
    let truth = truth_of(spec, truth_keys, truth_at, truth_label, items.len())?;
    let positives = truth.labels.iter().filter(|&&p| p).count();
    writeln!(
        out,
        "truth: {} rows, {} keys, {positives} positive in the universe, {} outside it",
        truth.rows, truth.keys, truth.outside_pos
    )?;
    outside(
        truth.outside_pos,
        spec.truth_outside,
        "truth positives",
        "--truth-outside",
        "outside_pos",
    )?;

    let scored = scored_rows(spec, &keys, at, items.len())?;
    writeln!(
        out,
        "scores: {} rows, {} outside the universe, {} duplicate keys",
        keys.len(),
        scored.outside,
        scored.duplicates
    )?;
    outside(
        scored.outside,
        spec.scored_outside,
        "scored rows",
        "--scored-outside",
        "outside_scored",
    )?;
    Ok((numbers, truth, scored))
}

/// Score as `spec` says, write its table, and say what was counted on
/// `out` and what to doubt on `err`. The scores are ranked one at a time,
/// over one vector of the universe.
fn run(spec: &Spec, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let same = spec.universe == Universe::Same;
    if same && spec.scores != spec.truth && !crate::same_file(&spec.scores, &spec.truth) {
        return Err(usage(
            "--universe same needs --truth and --scores to be one file; give the universe as a \
             file otherwise",
        ));
    }
    let mut want: Vec<&str> = spec.score.iter().map(|s| s.column.as_str()).collect();
    if same {
        // The one file is read once, the label with the scores: a pipe
        // cannot be read twice.
        want.extend(spec.label_column());
    }
    let scores = table::read_keyed(&spec.scores, &spec.key, &want)?;
    let (columns, truth, scored) = over_universe(spec, scores, out)?;

    let mut values = vec![None; truth.labels.len()];
    let mut rows = Vec::with_capacity(2 * spec.score.len());
    for (s, col) in spec.score.iter().zip(&columns) {
        score_of(spec, s, col, &scored.at, &mut values)?;
        let m = metric::rank(&values, &truth.labels);
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
    table::write_text(&spec.out, |w| {
        ScoreRow::header(w)?;
        rows.iter().try_for_each(|r| r.write(w))
    })?;
    writeln!(out, "wrote {}", spec.out.display())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_item_outside_the_universe_is_refused_under_fail_only() {
        assert!(outside(0, Outside::Fail, "M1", "--m1", "m1").is_ok());
        assert!(outside(5, Outside::Drop, "M1", "--m1", "m1").is_ok());
        let e = outside(
            1,
            Outside::Fail,
            "scored rows",
            "--scored-outside",
            "outside_scored",
        )
        .unwrap_err();
        assert_eq!(crate::command::code(&e), 3);
        assert_eq!(
            e.to_string(),
            "1 scored rows are outside the universe; --scored-outside drop leaves them out, \
             counted as outside_scored"
        );
    }

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
