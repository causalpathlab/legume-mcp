//! `legume benchmark gather SPEC`: a benchmark's answer, from the
//! `*.score.tsv` each scored task made (the last stage of each method makes
//! one, [`super::plan::check`]), into its folder:
//!
//! - `results.tsv`: every row of every done task's scores, long;
//! - `summary.tsv`: per point, method/score, part and metric, the
//!   replicates' values summarized, never pooled;
//! - `paired.tsv`, when the spec has `compare`: each pair on the
//!   replicates both sides have, point by point, and a sign count across
//!   the points;
//! - `summary.svg`, when the spec has `plot`: a panel per metric (and
//!   facet), the mean per x value, a line per method/score, q1 to q3 bars.
//!
//! A task counts once its `done.json` has its key, so a benchmark stopped
//! part way gathers what it has, and `summary.tsv` says how many of the
//! planned replicates each row has.

use super::plan::{PlanDoc, Task};
use super::spec::{Axis, Plot, Spec};
use crate::command::{refuse, usage};
use crate::graph::xml;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `legume benchmark gather`'s usage.
pub const USAGE: &str = "\
legume benchmark gather SPEC

Gathers the benchmark of the spec SPEC from its folder here: the
*.score.tsv of each scored task done so far, into results.tsv (every
row), summary.tsv (per point, method/score, part and metric: n of the
planned reps, n_nan, mean, sd, median, q1, q3), paired.tsv (each compare
pair on the reps both have: mean_diff, sd_diff, wins, ties, losses) and
summary.svg (with plot). A run on a cluster gathers at its end.";

/// `legume benchmark gather` with `args` (those after `gather`).
pub fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let spec_file = match args {
        [] => return Ok(writeln!(out, "{USAGE}")?),
        [a] if matches!(a.as_str(), "-h" | "--help") => return Ok(writeln!(out, "{USAGE}")?),
        [a] if !a.starts_with('-') => PathBuf::from(a),
        _ => return Err(usage("gather takes one SPEC, the spec's JSON file")),
    };
    let text = std::fs::read_to_string(&spec_file)
        .with_context(|| format!("reading {}", spec_file.display()))?;
    let (spec, value) = super::spec::parse(&text)?;
    let spec_key = super::key::spec_key(&value);
    let root = crate::project::root().context("finding the project's folder")?;
    let dir = root.join(super::bench_rel(&spec.name));
    let file = dir.join("plan.json");
    let bytes = std::fs::read(&file).with_context(|| {
        format!(
            "reading {}: plan or run the benchmark first",
            file.display()
        )
    })?;
    let plan: PlanDoc =
        serde_json::from_slice(&bytes).with_context(|| format!("reading {}", file.display()))?;
    if plan.spec_key.as_deref().is_some_and(|k| k != spec_key) {
        return Err(refuse(format!(
            "{} is of another spec than {}: plan or run this one first",
            file.display(),
            spec_file.display()
        )));
    }
    gather(&dir, &spec, &spec_key, &plan)?.say(&crate::rel_path(&root, &dir), out, err)
}

/// What a gather made.
#[derive(Debug)]
pub struct Gathered {
    /// Scored tasks done, of those planned.
    pub done: usize,
    pub planned: usize,
    /// The files written, by name.
    pub files: Vec<&'static str>,
    pub warnings: Vec<String>,
}

impl Gathered {
    /// Said on `out`, its warnings on `err`; `dir` is where, as said.
    pub fn say(&self, dir: &str, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
        for w in &self.warnings {
            writeln!(err, "warning: {w}")?;
        }
        writeln!(
            out,
            "gathered {} of {} scored tasks: {} in {dir}",
            self.done,
            self.planned,
            self.files.join(", ")
        )?;
        Ok(())
    }
}

/// The tasks of `plan` that make a score, in its order, with the file.
pub fn scored(plan: &PlanDoc) -> impl Iterator<Item = (&Task, &str)> {
    plan.tasks.iter().filter_map(|t| {
        t.makes
            .iter()
            .find(|m| m.ends_with(".score.tsv"))
            .map(|m| (t, m.as_str()))
    })
}

/// The benchmark in `dir`, of `spec` (key `spec_key`) as `plan` planned it,
/// gathered into `dir`.
pub fn gather(dir: &Path, spec: &Spec, spec_key: &str, plan: &PlanDoc) -> Result<Gathered> {
    let axes: Vec<&str> = spec.axes.iter().map(|a| a.name.as_str()).collect();
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let (mut done, mut planned) = (0, 0);
    let mut reps: BTreeMap<(Vec<String>, String), u32> = BTreeMap::new();
    for (t, file) in scored(plan) {
        planned += 1;
        let point: Vec<String> = axes
            .iter()
            .map(|a| t.point.get(*a).cloned().unwrap_or_default())
            .collect();
        let method = t.method.clone().unwrap_or_else(|| t.stage.clone());
        *reps.entry((point.clone(), method.clone())).or_default() += 1;
        let Some(at) = plan.task_dir(dir, t) else {
            warnings.push(format!("{}: its folder is not in the plan's", t.id));
            continue;
        };
        if !t.done_in(&at) {
            continue;
        }
        match read_scores(&at.join(file)) {
            Ok((scores, bad)) => {
                done += 1;
                if let Some(bad) = bad {
                    warnings.push(format!("{}: {file}: {bad}", t.id));
                }
                rows.extend(scores.into_iter().map(|(score, part, metric, value)| Row {
                    point: point.clone(),
                    rep: t.rep,
                    method: method.clone(),
                    score,
                    part,
                    metric,
                    value,
                }));
            }
            Err(e) => warnings.push(format!("{}: {e:#}", t.id)),
        }
    }
    let mut files = vec!["results.tsv", "summary.tsv"];
    write(dir, "results.tsv", results(&axes, spec_key, &rows))?;
    write(dir, "summary.tsv", summary(&axes, &rows, &reps))?;
    if !spec.compare.is_empty() {
        write(dir, "paired.tsv", paired(&axes, &rows, &spec.compare))?;
        files.push("paired.tsv");
    }
    if let Some(plot) = &spec.plot {
        write(dir, "summary.svg", svg(&spec.axes, &rows, plot))?;
        files.push("summary.svg");
    }
    Ok(Gathered {
        done,
        planned,
        files,
        warnings,
    })
}

/// `bytes` as `name` in `dir`, replaced whole.
pub(super) fn write(dir: &Path, name: &str, bytes: impl AsRef<[u8]>) -> Result<()> {
    crate::job::replace(dir, name, bytes.as_ref())
        .with_context(|| format!("writing {}", dir.join(name).display()))
}

/// One value of one replicate.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    /// Its label on each axis, in the spec's order.
    point: Vec<String>,
    rep: u32,
    /// The method, or the stage of a stage without methods.
    method: String,
    score: String,
    part: String,
    metric: String,
    value: f64,
}

/// A score, part and metric, and its value.
type Scored = (String, String, String, f64);

/// The rows of the `*.score.tsv` at `path`, by its columns `score`, `part`,
/// `metric` and `value`, a value that is no number NaN; and the first such
/// cell that is not one of the ways to write none (NA), to warn of.
fn read_scores(path: &Path) -> Result<(Vec<Scored>, Option<String>)> {
    let key: Vec<String> = ["score", "part", "metric"].map(String::from).into();
    let table = crate::table::read_keyed(path, &key, &["value"])?;
    let values = &table.numbers[0];
    let bad = values
        .first_bad(|_| true)
        .map(|(row, cell)| format!("row {}: value {cell:?} is no number", row + 1));
    let rows = table
        .keys
        .iter()
        .zip(&values.values)
        .map(|(k, v)| {
            let mut k = k.splitn(3, '\t').map(String::from);
            let mut next = || k.next().unwrap_or_default();
            (next(), next(), next(), *v)
        })
        .collect();
    Ok((rows, bad))
}

/// `items`' keys, each once, in the order they first come, with the items
/// of each.
fn grouped<T, K: Clone + Eq + std::hash::Hash>(
    items: impl IntoIterator<Item = T>,
    key: impl Fn(&T) -> K,
) -> Vec<(K, Vec<T>)> {
    let mut at = std::collections::HashMap::new();
    let mut groups: Vec<(K, Vec<T>)> = Vec::new();
    for item in items {
        let k = key(&item);
        let i = *at.entry(k.clone()).or_insert_with(|| {
            groups.push((k, Vec::new()));
            groups.len() - 1
        });
        groups[i].1.push(item);
    }
    groups
}

/// A number as a cell: NaN, or up to 6 decimals without trailing zeros.
fn num(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    let s = format!("{x:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0" } else { s }.to_string()
}

/// A header line: `first`, the axes, then `rest`.
fn header(first: &[&str], axes: &[&str], rest: &[&str]) -> String {
    let mut h: Vec<&str> = first.to_vec();
    h.extend(axes);
    h.extend(rest);
    h.join("\t") + "\n"
}

/// `labels` as cells, each followed by a tab.
fn cells(labels: &[String]) -> String {
    labels.iter().map(|l| format!("{l}\t")).collect()
}

/// `results.tsv`: every row.
fn results(axes: &[&str], spec_key: &str, rows: &[Row]) -> String {
    let mut out = header(
        &["spec_key"],
        axes,
        &["rep", "method", "score", "part", "metric", "value"],
    );
    for r in rows {
        let _ = writeln!(
            out,
            "{spec_key}\t{}{}\t{}\t{}\t{}\t{}\t{}",
            cells(&r.point),
            r.rep,
            r.method,
            r.score,
            r.part,
            r.metric,
            r.value
        );
    }
    out
}

/// The summary of some values: n, n_nan, mean, sd, median, q1, q3.
#[derive(Debug, PartialEq)]
struct Stats {
    n: usize,
    n_nan: usize,
    mean: f64,
    sd: f64,
    median: f64,
    q1: f64,
    q3: f64,
}

/// `values` summarized, the NaN counted apart; sd with n - 1, quartiles of
/// type 7 (R's default).
fn stats(values: &[f64]) -> Stats {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| !x.is_nan()).collect();
    v.sort_by(f64::total_cmp);
    let k = v.len();
    let mean = if k == 0 {
        f64::NAN
    } else {
        v.iter().sum::<f64>() / k as f64
    };
    let sd = if k < 2 {
        f64::NAN
    } else {
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (k - 1) as f64).sqrt()
    };
    Stats {
        n: values.len(),
        n_nan: values.len() - k,
        mean,
        sd,
        median: quantile(&v, 0.5),
        q1: quantile(&v, 0.25),
        q3: quantile(&v, 0.75),
    }
}

/// The `p` quantile of `sorted`, of type 7: NaN when it is empty.
fn quantile(sorted: &[f64], p: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return f64::NAN;
    };
    let h = last as f64 * p;
    let (lo, hi) = (h.floor() as usize, h.ceil() as usize);
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

/// `summary.tsv`: per point, method/score, part and metric, of the `reps`
/// planned for its point and method.
fn summary(axes: &[&str], rows: &[Row], reps: &BTreeMap<(Vec<String>, String), u32>) -> String {
    let mut out = header(
        &[],
        axes,
        &[
            "method", "score", "part", "metric", "reps", "n", "n_nan", "mean", "sd", "median",
            "q1", "q3",
        ],
    );
    let groups = grouped(rows, |r| {
        (&r.point, &r.method, &r.score, &r.part, &r.metric)
    });
    for ((point, method, score, part, metric), rs) in groups {
        let values: Vec<f64> = rs.iter().map(|r| r.value).collect();
        let s = stats(&values);
        let planned = reps
            .get(&(point.clone(), method.clone()))
            .copied()
            .unwrap_or(0);
        let _ = writeln!(
            out,
            "{}{method}\t{score}\t{part}\t{metric}\t{planned}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            cells(point),
            s.n,
            s.n_nan,
            num(s.mean),
            num(s.sd),
            num(s.median),
            num(s.q1),
            num(s.q3)
        );
    }
    out
}

/// A paired comparison of `diffs`, as cells: n, mean_diff, sd_diff, wins,
/// ties, losses; and the mean.
fn compared(diffs: &[f64]) -> (String, f64) {
    let s = stats(diffs);
    let count = |f: fn(&f64) -> bool| diffs.iter().filter(|d| f(d)).count();
    let cells = format!(
        "{}\t{}\t{}\t{}\t{}\t{}",
        s.n,
        num(s.mean),
        num(s.sd),
        count(|d| *d > 0.0),
        count(|d| *d == 0.0),
        count(|d| *d < 0.0)
    );
    (cells, s.mean)
}

/// `paired.tsv`: each pair `[a, b]` (`method/score`) of `compare`, `a - b`
/// on the replicates where both have a value, per point, part and metric
/// (`scope` point); then a row per part and metric (`scope` all-points)
/// whose counts are of the points' mean differences.
fn paired(axes: &[&str], rows: &[Row], compare: &[[String; 2]]) -> String {
    let mut out = header(
        &["a", "b", "scope"],
        axes,
        &[
            "part",
            "metric",
            "n",
            "mean_diff",
            "sd_diff",
            "wins",
            "ties",
            "losses",
        ],
    );
    let blank = cells(&vec!["NA".to_string(); axes.len()]);
    // Per point, part and metric, in the order the rows first have them:
    // each method/score's values that are not NaN, by replicate.
    let places = grouped(rows.iter().filter(|r| !r.value.is_nan()), |r| {
        (&r.point, &r.part, &r.metric)
    });
    let by_rep = |rs: &[&Row], name: &str| -> BTreeMap<u32, f64> {
        let (m, s) = name.split_once('/').unwrap_or((name, ""));
        rs.iter()
            .filter(|r| r.method == m && r.score == s)
            .map(|r| (r.rep, r.value))
            .collect()
    };
    for [a, b] in compare {
        let mut across: Vec<((&String, &String), f64)> = Vec::new();
        for ((point, part, metric), rs) in &places {
            let right = by_rep(rs, b);
            let d: Vec<f64> = by_rep(rs, a)
                .iter()
                .filter_map(|(rep, x)| right.get(rep).map(|y| x - y))
                .collect();
            if d.is_empty() {
                continue;
            }
            let (said, mean) = compared(&d);
            let _ = writeln!(
                out,
                "{a}\t{b}\tpoint\t{}{part}\t{metric}\t{said}",
                cells(point)
            );
            across.push(((*part, *metric), mean));
        }
        for ((part, metric), means) in grouped(across, |(k, _)| *k) {
            let means: Vec<f64> = means.into_iter().map(|(_, m)| m).collect();
            let _ = writeln!(
                out,
                "{a}\t{b}\tall-points\t{blank}{part}\t{metric}\t{}",
                compared(&means).0
            );
        }
    }
    out
}

/// Line colour `i`, from eight in turn.
fn colour(i: usize) -> &'static str {
    const COLOURS: [&str; 8] = [
        "#2f6f4f", "#b03a2e", "#2e5fa8", "#b07a1e", "#6d3fa0", "#1e8a8a", "#7a7a7a", "#a03a78",
    ];
    COLOURS[i % COLOURS.len()]
}

/// A panel's values, by series and x.
type Panel = BTreeMap<(usize, usize), Vec<f64>>;

/// A panel's size, and its margins about the plot, in px.
const W: f64 = 300.0;
const H: f64 = 190.0;
const LEFT: f64 = 56.0;
const TOP: f64 = 28.0;
const BOTTOM: f64 = 40.0;
const RIGHT: f64 = 14.0;
/// The x values set in from the frame.
const INSET: f64 = 16.0;

/// `summary.svg`: a panel per metric of `plot` (each metric when it lists
/// none), and per value of its facet; x is its axis, in the axis's order; a
/// line per series (method/score, then any part and other axes that vary),
/// at the replicates' mean, with a bar from q1 to q3. The spec checks that
/// x and the facet are axes.
fn svg(axes: &[Axis], rows: &[Row], plot: &Plot) -> String {
    let at = |name: &str| axes.iter().position(|a| a.name == name);
    let distinct = |f: &dyn Fn(&Row) -> &str| -> Vec<String> {
        grouped(rows, |r| f(r))
            .into_iter()
            .map(|(k, _)| k.to_string())
            .collect()
    };
    // An axis's labels that the rows have, in the axis's order.
    let labels = |i: usize| -> Vec<String> {
        let had: HashSet<&str> = rows.iter().map(|r| r.point[i].as_str()).collect();
        axes[i]
            .values
            .iter()
            .map(|v| v.label.clone())
            .filter(|l| had.contains(l.as_str()))
            .collect()
    };
    let x_at = at(&plot.x).unwrap_or(0);
    let facet = plot.facet.as_deref().and_then(|f| Some((at(f)?, f)));
    let metrics = if plot.metrics.is_empty() {
        distinct(&|r| &r.metric)
    } else {
        plot.metrics.clone()
    };
    let xs = labels(x_at);
    let facets = facet.map_or(vec![String::new()], |(i, _)| labels(i));
    // What else tells two lines apart: the parts, and the other axes, when
    // they vary.
    let others: Vec<usize> = (0..axes.len())
        .filter(|i| *i != x_at && Some(*i) != facet.map(|f| f.0))
        .filter(|i| distinct(&|r| &r.point[*i]).len() > 1)
        .collect();
    let parts = distinct(&|r| &r.part).len() > 1;
    let row_series: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut s = format!("{}/{}", r.method, r.score);
            if parts {
                let _ = write!(s, " {}", r.part);
            }
            for i in &others {
                let _ = write!(s, " {}={}", axes[*i].name, r.point[*i]);
            }
            s
        })
        .collect();
    let series: Vec<&String> = grouped(&row_series, |s| *s)
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    // Each panel's values, by series and x, in one pass.
    let mut values: BTreeMap<(&str, &str), Panel> = BTreeMap::new();
    for (r, s) in rows.iter().zip(&row_series) {
        let si = series.iter().position(|x| *x == s);
        let xi = xs.iter().position(|x| *x == r.point[x_at]);
        if let (Some(si), Some(xi)) = (si, xi) {
            let f = facet.map_or("", |(i, _)| r.point[i].as_str());
            values
                .entry((r.metric.as_str(), f))
                .or_default()
                .entry((si, xi))
                .or_default()
                .push(r.value);
        }
    }
    let (total_w, plots_h) = (W * facets.len() as f64, H * metrics.len() as f64);
    let total_h = plots_h + 18.0 * series.len() as f64 + 12.0;
    let mut out = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total_w:.0}" height="{total_h:.0}" viewBox="0 0 {total_w:.0} {total_h:.0}" font-family="Helvetica, Arial, sans-serif" font-size="11"><rect width="100%" height="100%" fill="white"/>
"##
    );
    let empty = BTreeMap::new();
    for (mi, metric) in metrics.iter().enumerate() {
        for (fi, f) in facets.iter().enumerate() {
            let title = match facet {
                Some((_, name)) => format!("{metric}, {name}={f}"),
                None => metric.clone(),
            };
            let points: Vec<(usize, usize, Stats)> = values
                .get(&(metric.as_str(), f.as_str()))
                .unwrap_or(&empty)
                .iter()
                .map(|((si, xi), v)| (*si, *xi, stats(v)))
                .filter(|(_, _, st)| !st.mean.is_nan())
                .collect();
            let origin = (fi as f64 * W, mi as f64 * H);
            panel(&mut out, origin, &title, &plot.x, &xs, &series, &points);
        }
    }
    for (si, s) in series.iter().enumerate() {
        let y = plots_h + 14.0 + 18.0 * si as f64;
        let _ = writeln!(
            out,
            r##"<line x1="{LEFT:.1}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="{}" stroke-width="2"/><text x="{:.1}" y="{:.1}" fill="#1d2433">{}</text>"##,
            LEFT + 22.0,
            colour(si),
            LEFT + 28.0,
            y + 4.0,
            xml(s)
        );
    }
    out.push_str("</svg>\n");
    out
}

/// One panel of [`svg`] at `origin`: its frame and ticks, then a line per
/// series through `points` (series, x, stats, by series then x).
fn panel(
    out: &mut String,
    (ox, oy): (f64, f64),
    title: &str,
    x_name: &str,
    xs: &[String],
    series: &[&String],
    points: &[(usize, usize, Stats)],
) {
    let finite = |x: f64| x.is_finite().then_some(x);
    let lo = points
        .iter()
        .filter_map(|(_, _, s)| finite(s.q1.min(s.mean)))
        .fold(f64::INFINITY, f64::min);
    let hi = points
        .iter()
        .filter_map(|(_, _, s)| finite(s.q3.max(s.mean)))
        .fold(f64::NEG_INFINITY, f64::max);
    let (lo, hi) = match (lo.is_finite(), hi > lo) {
        (false, _) => (0.0, 1.0),
        (true, true) => (lo - (hi - lo) * 0.08, hi + (hi - lo) * 0.08),
        (true, false) => (lo - 0.5, lo + 0.5),
    };
    let (pw, ph) = (W - LEFT - RIGHT, H - TOP - BOTTOM);
    let px = |xi: usize| {
        ox + LEFT
            + if xs.len() < 2 {
                pw / 2.0
            } else {
                INSET + (pw - 2.0 * INSET) * xi as f64 / (xs.len() - 1) as f64
            }
    };
    let py = |y: f64| oy + TOP + ph * (1.0 - (y.clamp(lo, hi) - lo) / (hi - lo));
    let _ = write!(
        out,
        r##"<g><text x="{:.1}" y="{:.1}" font-weight="bold" fill="#14301f">{}</text><rect x="{:.1}" y="{:.1}" width="{pw:.1}" height="{ph:.1}" fill="none" stroke="#cccccc"/>"##,
        ox + LEFT,
        oy + 18.0,
        xml(title),
        ox + LEFT,
        oy + TOP,
    );
    let (ticks, decimals) = ticks(lo, hi);
    for y in ticks {
        let _ = write!(
            out,
            r##"<line x1="{:.1}" y1="{at:.1}" x2="{:.1}" y2="{at:.1}" stroke="#eeeeee"/><text x="{:.1}" y="{:.1}" text-anchor="end" font-size="10" fill="#555555">{y:.decimals$}</text>"##,
            ox + LEFT,
            ox + LEFT + pw,
            ox + LEFT - 4.0,
            py(y) + 3.5,
            at = py(y),
        );
    }
    for (xi, x) in xs.iter().enumerate() {
        let _ = write!(
            out,
            r##"<text x="{:.1}" y="{:.1}" text-anchor="middle" font-size="10" fill="#555555">{}</text>"##,
            px(xi),
            oy + TOP + ph + 14.0,
            xml(x)
        );
    }
    let _ = write!(
        out,
        r##"<text x="{:.1}" y="{:.1}" text-anchor="middle" font-size="10" fill="#333333">{}</text>"##,
        ox + LEFT + pw / 2.0,
        oy + H - 8.0,
        xml(x_name)
    );
    for line in points.chunk_by(|a, b| a.0 == b.0) {
        let c = colour(line[0].0);
        if line.len() > 1 {
            let at: Vec<String> = line
                .iter()
                .map(|(_, xi, s)| format!("{:.1},{:.1}", px(*xi), py(s.mean)))
                .collect();
            let _ = write!(
                out,
                r##"<polyline points="{}" fill="none" stroke="{c}" stroke-width="1.6"/>"##,
                at.join(" ")
            );
        }
        for (si, xi, s) in line {
            let x = px(*xi);
            if s.q1.is_finite() && s.q3.is_finite() {
                let _ = write!(
                    out,
                    r##"<line x1="{x:.1}" y1="{:.1}" x2="{x:.1}" y2="{:.1}" stroke="{c}" stroke-width="1"/>"##,
                    py(s.q1),
                    py(s.q3)
                );
            }
            let _ = write!(
                out,
                r##"<circle cx="{x:.1}" cy="{:.1}" r="3" fill="{c}"><title>{} {}: mean {}, q1 {}, q3 {}, n {}</title></circle>"##,
                py(s.mean),
                xml(series[*si]),
                xml(&xs[*xi]),
                num(s.mean),
                num(s.q1),
                num(s.q3),
                s.n - s.n_nan
            );
        }
    }
    out.push_str("</g>\n");
}

/// Ticks from `lo` to `hi`, at a round step (1, 2 or 5 times a power of
/// ten) that gives three to six, and the decimals the step needs.
fn ticks(lo: f64, hi: f64) -> (Vec<f64>, usize) {
    let raw = (hi - lo) / 4.0;
    let power = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * power)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * power);
    let decimals = (-step.log10().floor()).clamp(0.0, 6.0) as usize;
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    ((first..=last).map(|k| k as f64 * step).collect(), decimals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::spec::AxisValue;

    fn row(point: &[&str], rep: u32, method: &str, part: &str, value: f64) -> Row {
        Row {
            point: point.iter().map(|s| s.to_string()).collect(),
            rep,
            method: method.into(),
            score: "s".into(),
            part: part.into(),
            metric: "auprc".into(),
            value,
        }
    }

    fn lines(text: &str) -> Vec<Vec<String>> {
        text.lines()
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect()
    }

    #[test]
    fn quartiles_are_of_type_7() {
        let s = stats(&[4.0, 1.0, f64::NAN, 3.0, 2.0]);
        assert_eq!((s.n, s.n_nan), (5, 1));
        assert_eq!((s.q1, s.median, s.q3), (1.75, 2.5, 3.25));
        assert_eq!(s.mean, 2.5);
        assert!((s.sd - (5.0f64 / 3.0).sqrt()).abs() < 1e-12);
        let one = stats(&[7.0]);
        assert_eq!((one.median, one.q1, one.q3), (7.0, 7.0, 7.0));
        assert!(one.sd.is_nan() && stats(&[]).mean.is_nan());
    }

    #[test]
    fn per_replicate_never_pooled() {
        // Two parts of one replicate stay two rows, each of one value; the
        // reps are counted against those planned.
        let rows = [
            row(&["1"], 1, "M1", "p1", 0.2),
            row(&["1"], 1, "M1", "p2", 0.8),
            row(&["1"], 2, "M1", "p1", 0.4),
        ];
        let reps = [((vec!["1".to_string()], "M1".to_string()), 3)].into();
        let t = lines(&summary(&["A"], &rows, &reps));
        assert_eq!(
            t[0].join(" "),
            "A method score part metric reps n n_nan mean sd median q1 q3"
        );
        assert_eq!(t[1][..9].join(" "), "1 M1 s p1 auprc 3 2 0 0.3");
        assert_eq!(t[2][..9].join(" "), "1 M1 s p2 auprc 3 1 0 0.8");
        assert_eq!(t[2][8..].join(" "), "0.8 NaN 0.8 0.8 0.8");
    }

    #[test]
    fn pairs_on_the_replicates_both_have() {
        let rows = [
            row(&["1"], 1, "M1", "all", 0.5),
            row(&["1"], 1, "M0", "all", 0.3),
            row(&["1"], 2, "M1", "all", 0.2),
            row(&["1"], 2, "M0", "all", f64::NAN),
            row(&["1"], 3, "M1", "all", 0.4),
            row(&["1"], 3, "M0", "all", 0.4),
            row(&["2"], 1, "M1", "all", 0.1),
            row(&["2"], 1, "M0", "all", 0.6),
        ];
        let t = lines(&paired(&["A"], &rows, &[["M1/s".into(), "M0/s".into()]]));
        assert_eq!(
            t[0].join(" "),
            "a b scope A part metric n mean_diff sd_diff wins ties losses"
        );
        // Rep 2 has no M0, rep 3 ties.
        assert_eq!(t[1][..8].join(" "), "M1/s M0/s point 1 all auprc 2 0.1");
        assert_eq!(t[1][9..].join(" "), "1 1 0");
        assert_eq!(t[2][..8].join(" "), "M1/s M0/s point 2 all auprc 1 -0.5");
        assert_eq!(
            t[3].join(" "),
            "M1/s M0/s all-points NA all auprc 2 -0.2 0.424264 1 0 1"
        );
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn scores_are_read_by_their_header_and_a_bad_value_said() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("m.score.tsv");
        std::fs::write(
            &f,
            "metric\tscore\tvalue\tpart\tn_pos\r\nauprc\tt\t0.25\tall\t3\r\n\r\nauroc\tt\tNA\tall\tNA\r\nap\tt\tx\tp1\t1\r\n",
        )
        .unwrap();
        let (rows, bad) = read_scores(&f).unwrap();
        assert_eq!(rows[0], ("t".into(), "all".into(), "auprc".into(), 0.25));
        assert!(rows[1].3.is_nan() && rows[2].3.is_nan() && rows[2].1 == "p1");
        assert_eq!(bad.as_deref(), Some("row 3: value \"x\" is no number"));
        std::fs::write(&f, "y\n").unwrap();
        assert!(read_scores(&f).is_err());
    }

    #[test]
    fn numbers_drop_trailing_zeros() {
        assert_eq!(num(0.30000000000000004), "0.3");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(2.0), "2");
        assert_eq!(num(f64::NAN), "NaN");
        let said = |lo, hi| {
            let (t, d) = ticks(lo, hi);
            t.iter()
                .map(|y| format!("{y:.d$}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(said(0.213, 0.607), "0.3 0.4 0.5 0.6");
        assert_eq!(said(0.0, 400.0), "0 100 200 300 400");
        assert_eq!(said(0.5, 1.5), "0.5 1.0 1.5");
    }

    #[test]
    fn the_plot_has_a_panel_per_metric_and_facet_and_a_line_per_method() {
        let mut rows = Vec::new();
        for (x, f) in [("1", "a"), ("2", "a"), ("1", "b"), ("2", "b")] {
            for (m, v) in [("M0", 0.2), ("M1", 0.6)] {
                rows.push(row(&[x, f], 1, m, "all", v));
                rows.push(row(&[x, f], 2, m, "all", v + 0.1));
            }
        }
        let axis = |name: &str, labels: [&str; 2]| Axis {
            name: name.into(),
            values: labels
                .iter()
                .map(|l| AxisValue {
                    label: l.to_string(),
                    bindings: vec![],
                })
                .collect(),
            tuple: false,
        };
        let plot = Plot {
            x: "X".into(),
            facet: Some("F".into()),
            metrics: vec![],
        };
        let s = svg(
            &[axis("X", ["2", "1"]), axis("F", ["a", "b"])],
            &rows,
            &plot,
        );
        assert!(s.starts_with("<svg") && s.ends_with("</svg>\n"));
        assert_eq!(s.matches("<polyline").count(), 4, "{s}");
        assert_eq!(s.matches("<circle").count(), 8);
        assert!(s.contains(">auprc, F=a<") && s.contains(">auprc, F=b<"));
        assert!(
            s.contains("M1/s 2: mean 0.65, q1 0.625, q3 0.675, n 2"),
            "{s}"
        );
        // x follows the axis's order: 2 left of 1.
        let x_of = |label: &str| {
            let at = s.find(&format!("M0/s {label}: mean")).unwrap();
            let rest = &s[s[..at].rfind("cx=\"").unwrap() + 4..];
            rest[..rest.find('"').unwrap()].parse::<f64>().unwrap()
        };
        assert!(x_of("2") < x_of("1"));
    }
}
