//! `legume score`'s command line.

use super::{run, Dup, Outside, Score, Spec, Transform, Universe};
use crate::command::{flag_pairs, one_of, required, usage};
use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;

/// `legume score`'s usage.
pub const USAGE: &str = "\
legume score --scores F --key C1[,C2] --score NAME=[neg:|abs:|neglog10:]COL ...
             --truth F --truth-key C1[,C2] [--label COL --above X]
             (--universe F --universe-key C1[,C2] | --universe same)
             [--truth-outside fail|drop] [--scored-outside drop|fail] [--dup fail|max]
             -o OUT.score.tsv

A method's scores against a truth over a universe: AUROC and average
precision for each --score, with the counts beside them. Tables are TSV
with a header (gzipped or not) or parquet.

  --scores F         the method's output; --key names its key columns
  --score NAME=COL   a score: higher is more likely true. neg:COL, abs:COL and
                     neglog10:COL (a p value in [0, 1]; p = 0 is +inf) turn a
                     column into one. Repeat for more scores
  --truth F          the true items, keyed by --truth-key. Every row is true,
                     or, with --label COL --above X, the rows whose COL is
                     strictly above X
  --universe F       every item the method could have scored, keyed by
                     --universe-key; `same` when the scores file is also the
                     truth, keyed by --key as --truth-key, and lists every
                     item. Required: an item the
                     method left unscored ranks last, it is never dropped
  --truth-outside    a true item outside the universe: fail (exit 3, the
                     default) or drop (counted as outside_pos)
  --scored-outside   a scored row outside the universe: drop (the default,
                     counted as outside_scored) or fail (exit 3)
  --dup              a key scored twice: fail (the default) or max (its
                     highest score)
  -o OUT             the table: score metric value part n_pos n_neg n_universe
                     pos_rate tied_share unscored unscored_pos outside_pos
                     outside_scored n_inf, a row for each score's auroc and
                     auprc";

/// `legume score` with `args` (those after `score`), saying what it counted
/// on `out` and its warnings on `err`. A refusal is a
/// [`crate::command::Exit`] with its exit code.
pub fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    match parse(args)? {
        Some(spec) => run(&spec, out, err),
        None => Ok(writeln!(out, "{USAGE}")?),
    }
}

/// What `--truth-outside` and `--scored-outside` take.
const OUTSIDE: &[(&str, Outside)] = &[("fail", Outside::Fail), ("drop", Outside::Drop)];

/// A comma-separated list of column names.
fn columns(flag: &str, s: &str) -> Result<Vec<String>> {
    let cols: Vec<String> = s.split(',').map(str::to_string).collect();
    if cols.iter().any(String::is_empty) {
        return Err(usage(format!("{flag} {s:?} names an empty column")));
    }
    Ok(cols)
}

/// `NAME=[neg:|abs:|neglog10:]COL`.
fn score(s: &str) -> Result<Score> {
    let Some((name, column)) = s.split_once('=') else {
        return Err(usage(format!("--score {s:?} is not NAME=COL")));
    };
    let (transform, column) = match column.split_once(':') {
        Some(("neg", c)) => (Transform::Neg, c),
        Some(("abs", c)) => (Transform::Abs, c),
        Some(("neglog10", c)) => (Transform::NegLog10, c),
        _ => (Transform::Plain, column),
    };
    if name.is_empty() || column.is_empty() || name.contains(char::is_whitespace) {
        return Err(usage(format!(
            "--score {s:?} needs a name, without spaces, and a column"
        )));
    }
    Ok(Score {
        name: name.into(),
        transform,
        column: column.into(),
    })
}

/// The command line as a [`Spec`]; `None` for `--help`.
fn parse(args: &[String]) -> Result<Option<Spec>> {
    let (mut scores, mut key, mut truth, mut truth_key) = (None, None, None, None);
    let (mut label, mut above, mut universe, mut universe_key) = (None, None, None, None);
    let (mut truth_outside, mut scored_outside, mut dup) =
        (Outside::Fail, Outside::Drop, Dup::Fail);
    let (mut list, mut out) = (Vec::new(), None);
    let Some(pairs) = flag_pairs(args)? else {
        return Ok(None);
    };
    for (flag, v) in pairs {
        match flag {
            "--scores" => scores = Some(PathBuf::from(v)),
            "--key" => key = Some(columns(flag, v)?),
            "--score" => list.push(score(v)?),
            "--truth" => truth = Some(PathBuf::from(v)),
            "--truth-key" => truth_key = Some(columns(flag, v)?),
            "--label" => label = Some(v.to_string()),
            "--above" => match v.parse::<f64>() {
                Ok(x) if !x.is_nan() => above = Some(x),
                _ => return Err(usage(format!("--above {v:?} is not a number"))),
            },
            "--universe" => universe = Some(v.to_string()),
            "--universe-key" => universe_key = Some(columns(flag, v)?),
            "--truth-outside" => truth_outside = one_of(flag, v, OUTSIDE)?,
            "--scored-outside" => scored_outside = one_of(flag, v, OUTSIDE)?,
            "--dup" => dup = one_of(flag, v, &[("fail", Dup::Fail), ("max", Dup::Max)])?,
            "-o" | "--out" => out = Some(PathBuf::from(v)),
            _ => return Err(usage(format!("unknown argument {flag}"))),
        }
    }
    let universe = match (required(universe, "--universe")?.as_str(), universe_key) {
        ("same", None) => Universe::Same,
        ("same", Some(_)) => {
            return Err(usage(
                "--universe same takes no --universe-key: it is the scores' own keys",
            ))
        }
        (path, Some(key)) => Universe::File {
            path: path.into(),
            key,
        },
        (_, None) => return Err(usage("--universe F needs --universe-key")),
    };
    let label = match (label, above) {
        (Some(col), Some(x)) => Some((col, x)),
        (None, None) => None,
        _ => return Err(usage("--label and --above go together")),
    };
    if list.is_empty() {
        return Err(usage("--score is required, once for each score"));
    }
    for (i, s) in list.iter().enumerate() {
        if list[..i].iter().any(|t| t.name == s.name) {
            return Err(usage(format!("--score {} is named twice", s.name)));
        }
    }
    let spec = Spec {
        scores: required(scores, "--scores")?,
        key: required(key, "--key")?,
        score: list,
        truth: required(truth, "--truth")?,
        truth_key: required(truth_key, "--truth-key")?,
        label,
        universe,
        truth_outside,
        scored_outside,
        dup,
        out: required(out, "-o")?,
    };
    if spec.universe == Universe::Same && spec.truth_key != spec.key {
        return Err(usage(
            "--universe same takes --truth-key as --key: the one file is read by its keys once",
        ));
    }
    let arity = spec.key.len();
    let other = match &spec.universe {
        Universe::File { key, .. } => key.len(),
        Universe::Same => arity,
    };
    if spec.truth_key.len() != arity || other != arity {
        return Err(usage(
            "--key, --truth-key and --universe-key name as many columns each",
        ));
    }
    Ok(Some(spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn code(e: anyhow::Error) -> i32 {
        crate::command::code(&e)
    }

    const BASE: &str = "--scores s.tsv --key K --truth t.tsv --truth-key K -o o.tsv";

    #[test]
    fn a_score_takes_its_transform_before_the_column() {
        let s = score("M1=neglog10:p").unwrap();
        assert_eq!(
            (s.name.as_str(), s.transform, s.column.as_str()),
            ("M1", Transform::NegLog10, "p")
        );
        assert_eq!(score("M2=neg:distance").unwrap().transform, Transform::Neg);
        assert_eq!(score("M3=abs:t").unwrap().transform, Transform::Abs);
        assert_eq!(score("M4=t").unwrap().transform, Transform::Plain);
        for bad in ["M1", "=t", "M1=", "M1=neg:", "M 1=t"] {
            assert_eq!(code(score(bad).unwrap_err()), 2, "{bad}");
        }
    }

    #[test]
    fn the_command_line_is_checked_before_anything_is_read() {
        let ok = format!("{BASE} --score M1=v --universe u.tsv --universe-key K");
        assert!(parse(&args(&ok)).unwrap().is_some());
        assert!(parse(&args("--help")).unwrap().is_none());
        for bad in [
            format!("{BASE} --score M1=v"),
            format!("{BASE} --score M1=v --universe u.tsv"),
            format!("{BASE} --score M1=v --universe same --universe-key K"),
            format!("{BASE} --universe same"),
            format!("{BASE} --score M1=v --score M1=w --universe same"),
            format!("{BASE} --score M1=v --universe same --label w"),
            format!("{BASE} --score M1=v --universe same --above 1"),
            format!("{BASE} --score M1=v --universe same --dup min"),
            format!("{BASE} --score M1=v --universe same --truth-outside keep"),
            format!("{BASE} --score M1=v --universe u.tsv --universe-key K,L"),
            format!("{BASE} --score M1=v --universe same --bogus 1"),
            format!("{BASE} --score M1=v --universe same --key"),
            // The one file is read by --key alone, so --truth-key must be it.
            format!("{BASE} --score M1=v --universe same --truth-key L"),
            format!("{BASE} --key K,L --truth-key L,K --score M1=v --universe same"),
        ] {
            assert_eq!(code(parse(&args(&bad)).unwrap_err()), 2, "{bad}");
        }
    }
}
