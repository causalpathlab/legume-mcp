//! The provenance graph: which files each step was made from and made, as
//! `results/INDEX.tsv` records them (its `inputs` and `output` columns). A
//! step is the rows one tool wrote at one time. Drawn as SVG with no
//! program but this one (columns from the data on the left to the last
//! outputs on the right), or given as Graphviz DOT or text.

use crate::records::Step;
use std::collections::HashMap;
use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// A file; `made` when a step in the graph made it, else data (or the
    /// output of a step outside it).
    File { made: bool },
    /// A step: its tool, and whether a number it made was flagged.
    Step { flagged: bool },
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub kind: Kind,
    /// What the drawing shows: a file's name, a step's tool.
    pub label: String,
    /// The second line: a step's server and time.
    pub detail: String,
    /// Everything, for a tooltip: a file's path, a step's summary.
    pub title: String,
    layer: usize,
    order: f64,
}

pub(crate) struct Graph {
    pub nodes: Vec<Node>,
    /// From, to: a file to the step that read it, a step to a file it made.
    pub edges: Vec<(usize, usize)>,
}

/// The longest a label is drawn.
const LABEL: usize = 30;

impl Graph {
    /// The graph of `steps` (rows of `INDEX.tsv`, in order).
    pub(crate) fn of(steps: &[&Step]) -> Self {
        let mut g = Graph {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        let mut files: HashMap<String, usize> = HashMap::new();
        let mut file = |g: &mut Graph, path: &str| -> usize {
            *files.entry(path.to_string()).or_insert_with(|| {
                let name = path.rsplit('/').next().unwrap_or(path);
                g.nodes.push(Node {
                    kind: Kind::File { made: false },
                    label: short(name),
                    detail: String::new(),
                    title: path.to_string(),
                    layer: 0,
                    order: 0.0,
                });
                g.nodes.len() - 1
            })
        };
        // A step is the rows one tool wrote at one time, in a row.
        let mut i = 0;
        while i < steps.len() {
            let s = steps[i];
            let same = |t: &Step| {
                (&t.date, &t.time, &t.server, &t.tool) == (&s.date, &s.time, &s.server, &s.tool)
            };
            let rows: Vec<&Step> = steps[i..].iter().copied().take_while(|t| same(t)).collect();
            i += rows.len();
            let flagged = rows
                .iter()
                .any(|r| r.facts.iter().any(|f| f.flagged().is_some()));
            g.nodes.push(Node {
                kind: Kind::Step { flagged },
                label: short(&s.tool),
                detail: format!("{} · {} {}", s.server, s.date, clock(&s.time)),
                title: format!("{} {}: {}", s.server, s.tool, s.summary),
                layer: 0,
                order: 0.0,
            });
            let step = g.nodes.len() - 1;
            let mut inputs: Vec<&String> = rows.iter().flat_map(|r| &r.inputs).collect();
            inputs.dedup();
            for input in inputs {
                let f = file(&mut g, input);
                if !g.edges.contains(&(f, step)) {
                    g.edges.push((f, step));
                }
            }
            for r in &rows {
                let f = file(&mut g, &r.output);
                g.nodes[f].kind = Kind::File { made: true };
                if f != step && !g.edges.contains(&(step, f)) {
                    g.edges.push((step, f));
                }
            }
        }
        g.layout();
        g
    }

    /// Columns by the longest path from the data; within one, an order
    /// that keeps the lines between columns from crossing much.
    fn layout(&mut self) {
        let n = self.nodes.len();
        let mut preds = vec![Vec::new(); n];
        let mut succs = vec![Vec::new(); n];
        for &(a, b) in &self.edges {
            preds[b].push(a);
            succs[a].push(b);
        }
        // Longest path from a node with no inputs; a cycle (a file a step
        // both read and made) is cut where it closes.
        let mut layer: Vec<Option<usize>> = vec![None; n];
        fn deepest(
            v: usize,
            preds: &[Vec<usize>],
            layer: &mut [Option<usize>],
            on: &mut [bool],
        ) -> usize {
            if let Some(l) = layer[v] {
                return l;
            }
            on[v] = true;
            let mut l = 0;
            for &p in &preds[v] {
                if !on[p] {
                    l = l.max(deepest(p, preds, layer, on) + 1);
                }
            }
            on[v] = false;
            layer[v] = Some(l);
            l
        }
        let mut on = vec![false; n];
        for v in 0..n {
            deepest(v, &preds, &mut layer, &mut on);
        }
        for (v, l) in layer.iter().enumerate() {
            self.nodes[v].layer = l.unwrap_or(0);
            self.nodes[v].order = v as f64;
        }
        // Data stands just before the first step that reads it, not all at
        // the far left.
        for v in 0..n {
            if preds[v].is_empty() {
                if let Some(first) = succs[v].iter().map(|&u| self.nodes[u].layer).min() {
                    self.nodes[v].layer = first.saturating_sub(1);
                }
            }
        }
        let layers = self
            .nodes
            .iter()
            .map(|x| x.layer)
            .max()
            .map_or(0, |m| m + 1);
        // Each node at the mean place of its neighbours in the column
        // before (then after), a few times over.
        for round in 0..4 {
            let down = round % 2 == 0;
            let order: Vec<usize> = if down {
                (1..layers).collect()
            } else {
                (0..layers.saturating_sub(1)).rev().collect()
            };
            for l in order {
                let place = self.places();
                let mut column: Vec<usize> = (0..n).filter(|&v| self.nodes[v].layer == l).collect();
                let near = |v: usize| if down { &preds[v] } else { &succs[v] };
                let key: Vec<f64> = column
                    .iter()
                    .map(|&v| {
                        let ns = near(v);
                        if ns.is_empty() {
                            place[v]
                        } else {
                            ns.iter().map(|&u| place[u]).sum::<f64>() / ns.len() as f64
                        }
                    })
                    .collect();
                let mut keyed: Vec<(f64, usize)> = key.into_iter().zip(column.drain(..)).collect();
                keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
                for (i, (_, v)) in keyed.into_iter().enumerate() {
                    self.nodes[v].order = i as f64;
                }
            }
        }
    }

    /// Each node's place in its column, 0 first.
    fn places(&self) -> Vec<f64> {
        let mut place = vec![0.0; self.nodes.len()];
        let layers = self
            .nodes
            .iter()
            .map(|x| x.layer)
            .max()
            .map_or(0, |m| m + 1);
        for l in 0..layers {
            let mut column: Vec<usize> = (0..self.nodes.len())
                .filter(|&v| self.nodes[v].layer == l)
                .collect();
            column.sort_by(|&a, &b| self.nodes[a].order.total_cmp(&self.nodes[b].order));
            for (i, v) in column.into_iter().enumerate() {
                place[v] = i as f64;
            }
        }
        place
    }

    /// The graph drawn: files as plain boxes (data grey, outputs dark),
    /// steps as rounded ones (a flagged step's in red), lines left to
    /// right.
    pub(crate) fn svg(&self) -> String {
        self.drawn().0
    }

    /// The SVG, and its width and height.
    pub(crate) fn drawn(&self) -> (String, f64, f64) {
        const CHAR: f64 = 7.2;
        const PAD: f64 = 10.0;
        const GAP_X: f64 = 56.0;
        const GAP_Y: f64 = 14.0;
        const FILE_H: f64 = 26.0;
        const STEP_H: f64 = 38.0;
        let width = |v: &Node| {
            let chars = v
                .label
                .chars()
                .count()
                .max(v.detail.chars().count() * 5 / 6);
            chars as f64 * CHAR + 2.0 * PAD
        };
        let height = |v: &Node| match v.kind {
            Kind::Step { .. } => STEP_H,
            Kind::File { .. } => FILE_H,
        };
        let place = self.places();
        let layers = self
            .nodes
            .iter()
            .map(|x| x.layer)
            .max()
            .map_or(0, |m| m + 1);
        // Each column as wide as its widest node; as tall as its nodes.
        let mut col_w = vec![0.0f64; layers];
        let mut col_h = vec![0.0f64; layers];
        for v in &self.nodes {
            col_w[v.layer] = col_w[v.layer].max(width(v));
            col_h[v.layer] += height(v) + GAP_Y;
        }
        let total_h = col_h.iter().cloned().fold(0.0, f64::max).max(FILE_H) + GAP_Y;
        let mut col_x = vec![GAP_Y; layers];
        for l in 1..layers {
            col_x[l] = col_x[l - 1] + col_w[l - 1] + GAP_X;
        }
        let total_w = col_x.last().zip(col_w.last()).map_or(0.0, |(x, w)| x + w) + GAP_Y;
        // Top left of each node: its column's nodes centred, in order.
        let mut at = vec![(0.0, 0.0); self.nodes.len()];
        for l in 0..layers {
            let mut column: Vec<usize> = (0..self.nodes.len())
                .filter(|&v| self.nodes[v].layer == l)
                .collect();
            column.sort_by(|&a, &b| place[a].total_cmp(&place[b]));
            let mut y = (total_h - col_h[l]) / 2.0 + GAP_Y / 2.0;
            for v in column {
                let node = &self.nodes[v];
                let x = col_x[l] + (col_w[l] - width(node)) / 2.0;
                at[v] = (x, y);
                y += height(node) + GAP_Y;
            }
        }
        let mut out = String::new();
        let _ = writeln!(
            out,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{total_w:.0}" height="{total_h:.0}" viewBox="0 0 {total_w:.0} {total_h:.0}" font-family="Helvetica, Arial, sans-serif" font-size="12">"##
        );
        let _ = writeln!(
            out,
            r##"<defs><marker id="a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#888"/></marker></defs>"##
        );
        let _ = writeln!(out, r##"<rect width="100%" height="100%" fill="white"/>"##);
        for &(a, b) in &self.edges {
            let (ax, ay) = at[a];
            let (bx, by) = at[b];
            let (x1, y1) = (
                ax + width(&self.nodes[a]),
                ay + height(&self.nodes[a]) / 2.0,
            );
            let (x2, y2) = (bx, by + height(&self.nodes[b]) / 2.0);
            let mid = (x1 + x2) / 2.0;
            let _ = writeln!(
                out,
                r##"<path d="M{x1:.1},{y1:.1} C{mid:.1},{y1:.1} {mid:.1},{y2:.1} {x2:.1},{y2:.1}" fill="none" stroke="#888" stroke-width="1.2" marker-end="url(#a)"/>"##
            );
        }
        for (v, node) in self.nodes.iter().enumerate() {
            let (x, y) = at[v];
            let (w, h) = (width(node), height(node));
            let title = format!("<title>{}</title>", xml(&node.title));
            match node.kind {
                Kind::File { made } => {
                    let (fill, stroke, ink) = if made {
                        ("#f4f6fb", "#4a5a7a", "#1d2433")
                    } else {
                        ("#f2f2f2", "#9a9a9a", "#555555")
                    };
                    let _ = writeln!(
                        out,
                        r##"<g>{title}<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" fill="{fill}" stroke="{stroke}"/><text x="{:.1}" y="{:.1}" text-anchor="middle" fill="{ink}">{}</text></g>"##,
                        x + w / 2.0,
                        y + h / 2.0 + 4.0,
                        xml(&node.label)
                    );
                }
                Kind::Step { flagged } => {
                    let stroke = if flagged { "#b03a2e" } else { "#2f6f4f" };
                    let _ = writeln!(
                        out,
                        r##"<g>{title}<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="9" fill="#eef7f1" stroke="{stroke}" stroke-width="{}"/><text x="{:.1}" y="{:.1}" text-anchor="middle" font-weight="bold" fill="#14301f">{}</text><text x="{:.1}" y="{:.1}" text-anchor="middle" font-size="10" fill="#4d6b58">{}</text></g>"##,
                        if flagged { 2.2 } else { 1.2 },
                        x + w / 2.0,
                        y + 16.0,
                        xml(&node.label),
                        x + w / 2.0,
                        y + 30.0,
                        xml(&node.detail)
                    );
                }
            }
        }
        out.push_str("</svg>\n");
        (out, total_w, total_h)
    }

    /// The graph for Graphviz (`dot -Tpdf`).
    pub(crate) fn dot(&self) -> String {
        let mut out = String::from(
            "digraph provenance {\n  rankdir=LR;\n  node [fontname=\"Helvetica\", fontsize=11];\n",
        );
        for (v, node) in self.nodes.iter().enumerate() {
            let (shape, color) = match node.kind {
                Kind::File { made: true } => ("box", "#4a5a7a"),
                Kind::File { made: false } => ("box", "#9a9a9a"),
                Kind::Step { flagged: true } => ("box, style=rounded", "#b03a2e"),
                Kind::Step { flagged: false } => ("box, style=rounded", "#2f6f4f"),
            };
            let label = if node.detail.is_empty() {
                dot_text(&node.title)
            } else {
                format!("{}\\n{}", dot_text(&node.label), dot_text(&node.detail))
            };
            let _ = writeln!(
                out,
                "  n{v} [label=\"{label}\", shape={shape}, color=\"{color}\", tooltip=\"{}\"];",
                dot_text(&node.title)
            );
        }
        for &(a, b) in &self.edges {
            let _ = writeln!(out, "  n{a} -> n{b};");
        }
        out.push_str("}\n");
        out
    }

    /// The graph as lines: each step, what it read and what it made.
    pub(crate) fn text(&self) -> String {
        let mut out = String::new();
        for (v, node) in self.nodes.iter().enumerate() {
            let Kind::Step { flagged } = node.kind else {
                continue;
            };
            let paths = |ends: Vec<usize>| -> String {
                let p: Vec<&str> = ends.iter().map(|&f| self.nodes[f].title.as_str()).collect();
                if p.is_empty() {
                    "nothing recorded".into()
                } else {
                    p.join(", ")
                }
            };
            let read = paths(
                self.edges
                    .iter()
                    .filter(|e| e.1 == v)
                    .map(|e| e.0)
                    .collect(),
            );
            let made = paths(
                self.edges
                    .iter()
                    .filter(|e| e.0 == v)
                    .map(|e| e.1)
                    .collect(),
            );
            let _ = writeln!(
                out,
                "{} ({}){}\n  from {read}\n  made {made}",
                node.label,
                node.detail,
                if flagged { ", a number flagged" } else { "" }
            );
        }
        out
    }
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

/// `text` safe in a DOT string.
fn dot_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            c => vec![c],
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

    #[test]
    fn steps_join_through_the_files_they_share() {
        let rows = [
            step("10:00:00", "qc", &["data/a.h5"], "r/a.qc.zip", true),
            step("10:05:00", "fit", &["r/a.qc.zip"], "r/fit.json", false),
            step("10:05:00", "fit", &["r/a.qc.zip"], "r/fit.<b>.png", false),
            step(
                "10:09:00",
                "annotate",
                &["r/fit.json", "panel.tsv"],
                "r/round1",
                false,
            ),
        ];
        let refs: Vec<&Step> = rows.iter().collect();
        let g = Graph::of(&refs);
        // 3 steps; files: a.h5, a.qc.zip, fit.json, fit.png, panel.tsv, round1.
        assert_eq!(g.nodes.len(), 9);
        let layer = |label: &str| g.nodes.iter().find(|n| n.label == label).unwrap().layer;
        assert_eq!(layer("a.h5"), 0);
        assert_eq!(layer("qc"), 1);
        assert_eq!(layer("fit"), 3);
        assert_eq!(layer("round1"), 6);
        assert_eq!(layer("panel.tsv"), 4, "beside the step that reads it");
        // Data that no step made, and outputs, are told apart.
        let kind = |label: &str| {
            g.nodes
                .iter()
                .find(|n| n.label == label)
                .unwrap()
                .kind
                .clone()
        };
        assert_eq!(kind("panel.tsv"), Kind::File { made: false });
        assert_eq!(kind("fit.json"), Kind::File { made: true });
        assert_eq!(kind("qc"), Kind::Step { flagged: true });

        let svg = g.svg();
        assert!(svg.starts_with("<svg "), "{svg}");
        assert!(
            svg.contains("fit.&lt;b&gt;.png"),
            "names are escaped: {svg}"
        );
        assert_eq!(svg.matches("marker-end").count(), g.edges.len());
        let dot = g.dot();
        assert!(dot.contains("rankdir=LR") && dot.contains(" -> "), "{dot}");
        let text = g.text();
        assert!(
            text.contains(
                "annotate (srv · 2026-10-28 10:09)\n  from r/fit.json, panel.tsv\n  made r/round1"
            ),
            "{text}"
        );
        assert!(
            text.contains("qc (srv · 2026-10-28 10:00), a number flagged"),
            "{text}"
        );
    }

    #[test]
    fn a_step_that_reads_what_it_writes_does_not_loop() {
        let rows = [step("10:00:00", "fix", &["r/x"], "r/x", false)];
        let refs: Vec<&Step> = rows.iter().collect();
        let g = Graph::of(&refs);
        assert_eq!(g.nodes.len(), 2);
        assert!(g.svg().ends_with("</svg>\n"));
    }
}
