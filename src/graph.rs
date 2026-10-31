//! The trail of a day, drawn like a hidden Markov model: along the top, the
//! person's own entries in the notebook (their question, data verdicts,
//! hypotheses, evidence, figures, summary), one leading to the next; under
//! each, what ran and was seen after it, until their next entry. A faint
//! line joins a step to one that read a file it made (`INDEX.tsv`'s
//! `inputs` and `output` columns). Drawn as SVG with no program but this
//! one, for the report.

use crate::records::{in_day, Entry, Step};
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// One of the person's entries; `said` false for the stand-in over the
    /// steps that came before any.
    Thought { said: bool },
    /// A step: the rows one tool wrote at one time; whether a number it
    /// made was flagged.
    Step { flagged: bool },
    /// An entry of the agent's: what was seen, or a note.
    Seen,
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub kind: Kind,
    /// What the drawing shows: the entry's kind (H1, Figure 2B), a step's
    /// tool.
    pub label: String,
    /// The second line: the person's words, a step's server and time.
    pub detail: String,
    /// A step's outputs, by name.
    pub files: Vec<String>,
    /// Everything, for a tooltip.
    pub title: String,
    /// The person's entry it falls under, by its place along the top.
    pub column: usize,
}

pub(crate) struct Graph {
    pub nodes: Vec<Node>,
    /// From, to: a step to a later one that read a file it made.
    pub files: Vec<(usize, usize)>,
}

/// The longest a label is drawn.
const LABEL: usize = 30;
/// The most a column shows under its entry; the rest are in the steps'
/// table.
const SHOWN: usize = 6;
/// The notebook kinds that are the person's words, when the legume server
/// wrote them.
const THEIRS: [&str; 6] = [
    "plan",
    "data",
    "hypothesis",
    "evidence",
    "figure",
    "summary",
];

impl Graph {
    /// The trail of `steps` (rows of `INDEX.tsv`, in order) and the day's
    /// `notebook`, in the order they happened.
    pub(crate) fn of(steps: &[&Step], notebook: &[Entry]) -> Self {
        // A step is the rows one tool wrote at one time, in a row.
        let mut groups: Vec<Vec<&Step>> = Vec::new();
        for &s in steps {
            match groups.last_mut() {
                Some(g)
                    if (&g[0].date, &g[0].time, &g[0].server, &g[0].tool)
                        == (&s.date, &s.time, &s.server, &s.tool) =>
                {
                    g.push(s)
                }
                _ => groups.push(vec![s]),
            }
        }
        // Entries before steps of the same second: the question comes
        // before what it asked for.
        enum Event<'a> {
            Entry(&'a Entry),
            Step(&'a [&'a Step]),
        }
        let mut events: Vec<((bool, &str), u8, Event)> = notebook
            .iter()
            .map(|e| (in_day(&e.time), 0, Event::Entry(e)))
            .chain(
                groups
                    .iter()
                    .map(|g| (in_day(&g[0].time), 1, Event::Step(g.as_slice()))),
            )
            .collect();
        events.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

        let mut g = Graph {
            nodes: Vec::new(),
            files: Vec::new(),
        };
        let mut column: Option<usize> = None;
        let mut made: HashMap<&str, usize> = HashMap::new();
        for (_, _, event) in events {
            let node = match event {
                Event::Entry(e) if is_theirs(e) => {
                    let c = column.map_or(0, |c| c + 1);
                    column = Some(c);
                    g.nodes.push(thought(e, c));
                    continue;
                }
                Event::Entry(e) => Node {
                    kind: Kind::Seen,
                    label: short(&e.kind),
                    detail: short(first_line(&e.text)),
                    files: Vec::new(),
                    title: format!("{} {}: {}", clock(&e.time), e.kind, e.text),
                    column: 0,
                },
                Event::Step(rows) => {
                    let s = rows[0];
                    let mut files: Vec<String> = rows.iter().map(|r| name(&r.output)).collect();
                    files.dedup();
                    Node {
                        kind: Kind::Step {
                            flagged: rows
                                .iter()
                                .any(|r| r.facts.iter().any(|f| f.flagged().is_some())),
                        },
                        label: short(&s.tool),
                        detail: format!("{} · {}", s.server, clock(&s.time)),
                        files,
                        title: format!("{} {}: {}", s.server, s.tool, s.summary),
                        column: 0,
                    }
                }
            };
            let c = *column.get_or_insert_with(|| {
                g.nodes.push(Node {
                    kind: Kind::Thought { said: false },
                    label: "No question yet".into(),
                    detail: "nothing in the person's words".into(),
                    files: Vec::new(),
                    title: "Steps run before the person's first entry of the day".into(),
                    column: 0,
                });
                0
            });
            g.nodes.push(Node { column: c, ..node });
            let v = g.nodes.len() - 1;
            if let Event::Step(rows) = event {
                let mut inputs: Vec<&str> = rows
                    .iter()
                    .flat_map(|r| r.inputs.iter().map(String::as_str))
                    .collect();
                inputs.dedup();
                for input in inputs {
                    if let Some(&from) = made.get(input) {
                        if from != v && !g.files.contains(&(from, v)) {
                            g.files.push((from, v));
                        }
                    }
                }
                for r in rows {
                    made.insert(&r.output, v);
                }
            }
        }
        g
    }

    /// The trail drawn as SVG, shaped to fit about `aspect` (width over
    /// height), and its width and height: the person's entries joined by
    /// arrows, in rows that wrap, each over a panel of what ran (rounded,
    /// a flagged step's in red) and was seen (dashed) after it.
    pub(crate) fn drawn(&self, aspect: f64) -> (String, f64, f64) {
        const CHAR: f64 = 7.2;
        const PAD: f64 = 10.0;
        const GUTTER: f64 = 100.0;
        const GAP_X: f64 = 44.0;
        const GAP_Y: f64 = 10.0;
        const DROP: f64 = 34.0;
        const BAND_GAP: f64 = 36.0;
        const TURN: f64 = 22.0;
        const THOUGHT_H: f64 = 54.0;
        const LINE: f64 = 13.0;
        let height = |v: &Node| match v.kind {
            Kind::Thought { .. } => THOUGHT_H,
            Kind::Step { .. } => 38.0 + LINE * v.files.len().min(2) as f64,
            Kind::Seen => 38.0,
        };
        let width = |v: &Node| {
            let mut chars = v.label.chars().count();
            for l in v.detail.lines() {
                chars = chars.max(l.chars().count() * 5 / 6);
            }
            for f in v.files.iter().take(2) {
                chars = chars.max(f.chars().count() * 5 / 6 + 2);
            }
            chars as f64 * CHAR + 2.0 * PAD
        };
        let columns = self.nodes.iter().map(|v| v.column + 1).max().unwrap_or(0);
        // Each column's entry and what it shows under it, in order.
        let mut top = vec![None; columns];
        let mut under: Vec<Vec<usize>> = vec![Vec::new(); columns];
        for (v, node) in self.nodes.iter().enumerate() {
            match node.kind {
                Kind::Thought { .. } => top[node.column] = Some(v),
                _ => under[node.column].push(v),
            }
        }
        let hidden: Vec<usize> = under
            .iter()
            .map(|u| u.len().saturating_sub(SHOWN))
            .collect();
        let shown = |c: usize| &under[c][..under[c].len().min(SHOWN)];
        let mut col_w = vec![0.0f64; columns];
        let mut col_h = vec![0.0f64; columns];
        for c in 0..columns {
            for &v in top[c].iter().chain(shown(c)) {
                col_w[c] = col_w[c].max(width(&self.nodes[v]));
            }
            col_h[c] = shown(c)
                .iter()
                .map(|&v| height(&self.nodes[v]) + GAP_Y)
                .sum::<f64>()
                + if hidden[c] > 0 { LINE + GAP_Y } else { 0.0 };
            col_w[c] += 2.0 * PAD;
        }
        // Rows of `per` entries, as many to a row as fit the shape best.
        let all: Vec<usize> = (0..columns).collect();
        let band_h = |band: &[usize]| {
            THOUGHT_H + DROP + band.iter().map(|&c| col_h[c]).fold(0.0, f64::max) + GAP_Y
        };
        let size = |per: usize| {
            let (mut w, mut h) = (0.0f64, 0.0);
            for band in all.chunks(per) {
                w = w.max(band.iter().map(|&c| col_w[c] + GAP_X).sum::<f64>() - GAP_X);
                h += band_h(band) + BAND_GAP;
            }
            (GUTTER + w + TURN + PAD, h - BAND_GAP + 2.0 * PAD)
        };
        let per = (1..=columns.max(1))
            .max_by(|&a, &b| {
                let fit = |n| {
                    let (w, h) = size(n);
                    (aspect / w).min(1.0 / h)
                };
                fit(a).total_cmp(&fit(b))
            })
            .unwrap_or(1);
        let (total_w, total_h) = size(per);
        let mut col_x = vec![0.0; columns];
        let mut col_y = vec![0.0; columns];
        let mut y = PAD;
        for band in all.chunks(per) {
            let mut x = GUTTER;
            for &c in band {
                (col_x[c], col_y[c]) = (x, y);
                x += col_w[c] + GAP_X;
            }
            y += band_h(band) + BAND_GAP;
        }
        let panel_y = |c: usize| col_y[c] + THOUGHT_H + DROP;
        // Each node drawn: its top left, width and height; what is under
        // an entry as wide as its panel.
        let mut at: Vec<Option<(f64, f64, f64, f64)>> = vec![None; self.nodes.len()];
        for c in 0..columns {
            if let Some(v) = top[c] {
                let w = width(&self.nodes[v]);
                at[v] = Some((col_x[c] + (col_w[c] - w) / 2.0, col_y[c], w, THOUGHT_H));
            }
            let mut y = panel_y(c) + GAP_Y;
            for &v in shown(c) {
                let h = height(&self.nodes[v]);
                at[v] = Some((col_x[c] + PAD, y, col_w[c] - 2.0 * PAD, h));
                y += h + GAP_Y;
            }
        }

        let mut out = String::new();
        let _ = writeln!(
            out,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total_w:.0}" height="{total_h:.0}" viewBox="0 0 {total_w:.0} {total_h:.0}" font-family="Helvetica, Arial, sans-serif" font-size="12">"##
        );
        let _ = writeln!(
            out,
            r##"<defs>{}{}{}</defs>"##,
            marker("a", "#999999"),
            marker("t", "#1d3557"),
            marker("f", "#b8b8b8")
        );
        let _ = writeln!(out, r##"<rect width="100%" height="100%" fill="white"/>"##);
        for c in 0..columns {
            let x = col_x[c];
            // The rows' names, in the gutter of each band.
            if c % per == 0 {
                for (y, name) in [
                    (col_y[c] + THOUGHT_H / 2.0, "The person"),
                    (panel_y(c) + GAP_Y + 19.0, "What ran"),
                ] {
                    let _ = writeln!(
                        out,
                        r##"<text x="{PAD}" y="{:.1}" font-size="11" font-weight="bold" fill="#777">{name}</text>"##,
                        y + 4.0
                    );
                }
            }
            // The panel, and the arrow down to it.
            if !shown(c).is_empty() {
                let _ = writeln!(
                    out,
                    r##"<rect x="{x:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="6" fill="#f7f7f5" stroke="#dddddd"/>"##,
                    panel_y(c),
                    col_w[c],
                    col_h[c] + GAP_Y
                );
                let mid = x + col_w[c] / 2.0;
                let _ = writeln!(
                    out,
                    r##"<path d="M{mid:.1},{:.1} L{mid:.1},{:.1}" stroke="#999" stroke-width="1.2" marker-end="url(#a)"/>"##,
                    col_y[c] + THOUGHT_H,
                    panel_y(c) - 1.0
                );
            }
            if hidden[c] > 0 {
                let _ = writeln!(
                    out,
                    r##"<text x="{:.1}" y="{:.1}" text-anchor="middle" font-size="10" fill="#777">… {} more, in the steps' table</text>"##,
                    x + col_w[c] / 2.0,
                    panel_y(c) + col_h[c] - 2.0,
                    hidden[c]
                );
            }
            // The arrow from the entry before: along the row, or round
            // to the start of the next.
            if c == 0 {
                continue;
            }
            let (Some(Some((ax, ay, aw, _))), Some(Some((bx, by, _, _)))) =
                (top[c - 1].map(|a| at[a]), top[c].map(|b| at[b]))
            else {
                continue;
            };
            let (y1, y2) = (ay + THOUGHT_H / 2.0, by + THOUGHT_H / 2.0);
            let path = if col_y[c] == col_y[c - 1] {
                format!("M{:.1},{y1:.1} L{:.1},{y2:.1}", ax + aw, bx - 1.0)
            } else {
                let right = col_x[c - 1] + col_w[c - 1] + TURN;
                let left = col_x[c] - TURN;
                let between = col_y[c] - BAND_GAP / 2.0;
                format!(
                    "M{:.1},{y1:.1} L{right:.1},{y1:.1} L{right:.1},{between:.1} L{left:.1},{between:.1} L{left:.1},{y2:.1} L{:.1},{y2:.1}",
                    ax + aw,
                    bx - 1.0
                )
            };
            let _ = writeln!(
                out,
                r##"<path d="{path}" fill="none" stroke="#1d3557" stroke-width="1.6" marker-end="url(#t)"/>"##
            );
        }
        // A step to one that read its file: faint, under the boxes.
        for &(a, b) in &self.files {
            let (Some((ax, ay, aw, ah)), Some((bx, by, _, bh))) = (at[a], at[b]) else {
                continue;
            };
            let (y1, y2) = (ay + ah / 2.0, by + bh / 2.0);
            let path = if self.nodes[a].column == self.nodes[b].column {
                // One above the other: round the right.
                let x = ax + aw;
                let bend = x + 18.0;
                format!("M{x:.1},{y1:.1} C{bend:.1},{y1:.1} {bend:.1},{y2:.1} {x:.1},{y2:.1}")
            } else {
                let (x1, x2) = (ax + aw, bx);
                let mid = (x1 + x2) / 2.0;
                format!("M{x1:.1},{y1:.1} C{mid:.1},{y1:.1} {mid:.1},{y2:.1} {x2:.1},{y2:.1}")
            };
            let _ = writeln!(
                out,
                r##"<path d="{path}" fill="none" stroke="#b8b8b8" stroke-width="1" stroke-dasharray="4 3" marker-end="url(#f)"/>"##
            );
        }
        for (v, node) in self.nodes.iter().enumerate() {
            let Some((x, y, w, h)) = at[v] else {
                continue;
            };
            let cx = x + w / 2.0;
            let title = format!("<title>{}</title>", xml(&node.title));
            let (rect, ink, sub) = match node.kind {
                Kind::Thought { said: true } => (
                    r##"rx="4" fill="#e8eef6" stroke="#1d3557" stroke-width="1.4""##,
                    "#1d3557",
                    r##"font-style="italic" fill="#333333""##,
                ),
                Kind::Thought { said: false } => (
                    r##"rx="4" fill="white" stroke="#999999" stroke-dasharray="4 3""##,
                    "#777777",
                    r##"fill="#999999""##,
                ),
                Kind::Step { flagged: true } => (
                    r##"rx="9" fill="#eef7f1" stroke="#b03a2e" stroke-width="2.2""##,
                    "#14301f",
                    r##"fill="#4d6b58""##,
                ),
                Kind::Step { flagged: false } => (
                    r##"rx="9" fill="#eef7f1" stroke="#2f6f4f" stroke-width="1.2""##,
                    "#14301f",
                    r##"fill="#4d6b58""##,
                ),
                Kind::Seen => (
                    r##"rx="2" fill="white" stroke="#aaaaaa" stroke-dasharray="3 2""##,
                    "#555555",
                    r##"font-style="italic" fill="#555555""##,
                ),
            };
            let _ = write!(
                out,
                r##"<g>{title}<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" {rect}/><text x="{cx:.1}" y="{:.1}" text-anchor="middle" font-weight="bold" fill="{ink}">{}</text>"##,
                y + 16.0,
                xml(&node.label),
            );
            for (i, l) in node.detail.lines().enumerate() {
                let _ = write!(
                    out,
                    r##"<text x="{cx:.1}" y="{:.1}" text-anchor="middle" font-size="10" {sub}>{}</text>"##,
                    y + 30.0 + LINE * i as f64,
                    xml(l)
                );
            }
            let more = node.files.len().saturating_sub(2);
            for (i, f) in node.files.iter().take(2).enumerate() {
                let f = if i == 1 && more > 0 {
                    format!("{f} +{more}")
                } else {
                    f.clone()
                };
                let _ = write!(
                    out,
                    r##"<text x="{cx:.1}" y="{:.1}" text-anchor="middle" font-size="10" fill="#1d2433">{}</text>"##,
                    y + 44.0 + LINE * i as f64,
                    xml(&f)
                );
            }
            out.push_str("</g>\n");
        }
        out.push_str("</svg>\n");
        (out, total_w, total_h)
    }
}

/// An arrowhead `id`, in `fill`.
fn marker(id: &str, fill: &str) -> String {
    format!(
        r##"<marker id="{id}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="{fill}"/></marker>"##
    )
}

/// Whether `e` is the person's words: one of their kinds, written by the
/// legume server, or a note the person wrote (in any tool's watch view:
/// `user` before 0.5.0).
fn is_theirs(e: &Entry) -> bool {
    let kind = e.kind.split(' ').next().unwrap_or("");
    e.server == crate::brain::NAME && THEIRS.contains(&kind)
        || e.kind == "note (user)"
        || e.kind == "user"
}

/// The top-row node of the person's entry `e`: its kind as a name (H1,
/// Figure 2B), their words under it.
fn thought(e: &Entry, column: usize) -> Node {
    let (kind, rest) = e.kind.split_once(' ').unwrap_or((&e.kind, ""));
    let label = match kind {
        "plan" => "Question".into(),
        "data" => match e.text.split_once("verdict: ") {
            Some((_, v)) => format!("Data: {}", v.lines().next().unwrap_or("").trim()),
            None => "Data".into(),
        },
        "hypothesis" => rest.to_string(),
        "evidence" => format!("Evidence, {rest}"),
        "figure" => format!("Figure {rest}"),
        "summary" => "Summary".into(),
        _ => "Note".into(),
    };
    let words = e
        .text
        .lines()
        .find_map(|l| l.strip_prefix("> "))
        .unwrap_or_else(|| first_line(&e.text));
    Node {
        kind: Kind::Thought { said: true },
        label: short(&label),
        detail: wrapped(words, 34, 2),
        files: Vec::new(),
        title: format!("{} {}: {}", clock(&e.time), e.kind, e.text),
        column,
    }
}

/// `text` in at most `lines` lines of `width` characters, broken between
/// words, the last cut short (…) when there is more.
fn wrapped(text: &str, width: usize, lines: usize) -> String {
    let mut out: Vec<String> = vec![String::new()];
    for word in text.split_whitespace() {
        let n = out.len();
        let line = &mut out[n - 1];
        if line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        } else if n < lines {
            out.push(word.to_string());
        } else {
            line.push_str(" …");
            break;
        }
    }
    for line in &mut out {
        if line.chars().count() > width {
            *line = format!("{}…", line.chars().take(width - 1).collect::<String>());
        }
    }
    out.join("\n")
}

fn first_line(text: &str) -> &str {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

fn name(path: &str) -> String {
    short(path.rsplit('/').next().unwrap_or(path))
}

/// `text` cut to [`LABEL`] characters.
fn short(text: &str) -> String {
    if text.chars().count() <= LABEL {
        return text.into();
    }
    let kept: String = text.chars().take(LABEL - 1).collect();
    format!("{kept}…")
}

fn clock(time: &str) -> &str {
    time.get(..5).unwrap_or(time)
}

/// `text` safe in XML text and attributes.
fn xml(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Fact;

    fn step(time: &str, tool: &str, inputs: &[&str], output: &str, flag: bool) -> Step {
        Step {
            date: "2026-10-28".into(),
            time: time.into(),
            server: "srv".into(),
            tool: tool.into(),
            output: output.into(),
            summary: format!("{tool} made {output}"),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            facts: if flag {
                vec![Fact::value("mito", 0.3).flag("high")]
            } else {
                vec![]
            },
        }
    }

    fn entry(time: &str, server: &str, kind: &str, text: &str) -> Entry {
        Entry {
            time: time.into(),
            server: server.into(),
            kind: kind.into(),
            text: text.into(),
        }
    }

    fn find<'a>(g: &'a Graph, label: &str) -> &'a Node {
        g.nodes.iter().find(|n| n.label == label).unwrap()
    }

    #[test]
    fn each_step_falls_under_the_persons_entry_before_it() {
        let brain = crate::brain::NAME;
        let rows = [
            step("09:00:00", "info", &["data/a.h5"], "r/a.info.txt", false),
            step("10:00:00", "qc", &["data/a.h5"], "r/a.qc.zip", true),
            step("10:05:00", "fit", &["r/a.qc.zip"], "r/fit.json", false),
            step("10:05:00", "fit", &["r/a.qc.zip"], "r/fit.<b>.png", false),
            step("10:09:00", "annotate", &["r/fit.json"], "r/round1", false),
        ];
        let notebook = [
            entry(
                "10:00:00",
                brain,
                "plan",
                "> Why is <4> apart, rather than with 2?",
            ),
            entry(
                "10:01:00",
                brain,
                "data",
                "`data/a.h5`, verdict: caveats\n\n> one donor",
            ),
            entry(
                "10:02:00",
                "senna",
                "plan",
                "the agent's own plan, from an older tool",
            ),
            entry("10:06:00", brain, "observation", "fit: 12 clusters"),
            entry("10:07:00", brain, "hypothesis H1", "> 4 is a doublet group"),
        ];
        let refs: Vec<&Step> = rows.iter().collect();
        let g = Graph::of(&refs, &notebook);

        // Along the top: the stand-in for the step before any entry, then
        // the person's entries in order; an older tool's plan is not theirs.
        let top: Vec<&str> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, Kind::Thought { .. }))
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(top, ["No question yet", "Question", "Data: caveats", "H1"]);
        assert_eq!(find(&g, "info").column, 0);
        assert_eq!(
            find(&g, "qc").column,
            1,
            "after the plan of the same second"
        );
        assert_eq!(find(&g, "fit").column, 2);
        assert_eq!(find(&g, "fit").files, ["fit.json", "fit.<b>.png"]);
        assert_eq!(find(&g, "observation").column, 2);
        assert_eq!(find(&g, "plan").kind, Kind::Seen);
        assert_eq!(find(&g, "annotate").column, 3);
        assert_eq!(find(&g, "qc").kind, Kind::Step { flagged: true });
        // qc made what fit read, fit what annotate read.
        assert_eq!(g.files.len(), 2);

        let svg = g.drawn(2.6).0;
        assert!(svg.starts_with("<svg "), "{svg}");
        assert!(
            svg.contains("Why is &lt;4&gt; apart, rather") && svg.contains(">2?<"),
            "words escaped: {svg}"
        );
        assert!(svg.contains("fit.&lt;b&gt;.png"), "names escaped: {svg}");
        // 3 arrows along the top, 4 down to the panels, 2 between files.
        assert_eq!(svg.matches("marker-end").count(), 3 + 4 + 2);
    }

    #[test]
    fn the_persons_words_break_between_words() {
        assert_eq!(wrapped("a few words", 34, 2), "a few words");
        assert_eq!(wrapped("aaa bbb ccc", 7, 2), "aaa bbb\nccc");
        assert_eq!(wrapped("aaa bbb ccc dd eee", 7, 2), "aaa bbb\nccc dd…");
        assert_eq!(wrapped("abcdefghij", 7, 2), "abcdef…");
    }

    #[test]
    fn a_long_day_wraps_into_rows() {
        let notebook: Vec<Entry> = (0..8)
            .map(|i| entry(&format!("10:0{i}:00"), crate::brain::NAME, "summary", "> s"))
            .collect();
        let (_, w, h) = Graph::of(&[], &notebook).drawn(2.6);
        assert!(w / h < 6.0, "{w} x {h}");
    }

    #[test]
    fn the_small_hours_come_after_the_evening() {
        let rows = [
            step("01:30:00", "late", &[], "r/late", false),
            step("22:00:00", "evening", &[], "r/evening", false),
        ];
        let notebook = [entry("23:00:00", crate::brain::NAME, "summary", "> done")];
        let refs: Vec<&Step> = rows.iter().collect();
        let g = Graph::of(&refs, &notebook);
        assert_eq!(find(&g, "evening").column, 0);
        assert_eq!(find(&g, "late").column, 1, "under the summary");
    }

    #[test]
    fn a_long_column_shows_its_first_steps_and_counts_the_rest() {
        let rows: Vec<Step> = (0..9)
            .map(|i| {
                step(
                    &format!("10:0{i}:00"),
                    &format!("s{i}"),
                    &[],
                    &format!("r/{i}"),
                    false,
                )
            })
            .collect();
        let refs: Vec<&Step> = rows.iter().collect();
        let svg = Graph::of(&refs, &[]).drawn(2.6).0;
        assert!(svg.contains(">s5<") && !svg.contains(">s6<"), "{svg}");
        assert!(svg.contains("… 3 more, in the steps' table"), "{svg}");
    }
}
