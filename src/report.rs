//! The `legume` server's `report`: a day's work as slides, built from the
//! records alone, in `results/<day>/report/`: `report.pdf` (typeset by this
//! program with Typst, nothing installed), `report.md` (the same slides as
//! text, for any editor), and the trail `graph.svg`: the person's entries,
//! with what ran under each. The folder is the report's own: no tool's
//! output is replaced.
//!
//! The slides, in order: the title; the person's summary of the day, in
//! their words; their question; the data, with where each file came from
//! and their verdict; the steps (the graph, then each output with what it
//! was made from and its numbers, the flagged ones marked); the figures
//! they kept, then the other images, each with the line of `runall.sh`
//! that made it; what was seen; each hypothesis, as they put it, with its
//! rivals, what would nullify it, its sources and the verb they allow; what
//! is open for the person; and how to replay the work. The agent writes
//! none of it.
//!
//! Every word from the records goes into the Typst source as a string and
//! into the Markdown escaped, so no record, however written, is read as
//! markup or code.

use crate::evidence;
use crate::graph::Graph;
use crate::project::read_record;
use crate::records::{Entry, Records, Step};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The room for the graph on its slide, in cm: width, height.
const GRAPH_ROOM: (f64, f64) = (26.0, 10.0);

/// What a report made.
pub(crate) struct Built {
    /// The PDF, or why there is none.
    pub pdf: std::result::Result<PathBuf, String>,
    pub md: PathBuf,
    pub graph: Option<PathBuf>,
    /// Whether the person summarised the day.
    pub summary: bool,
}

/// The report of `day` (today when none) for the project at `root`.
pub(crate) fn build(root: &Path, day: Option<&str>) -> Result<Built> {
    let day = day_of(day)?;
    let dir = root.join("results").join(&day);
    anyhow::ensure!(
        dir.is_dir(),
        "no records for {day}: {} is not there",
        dir.display()
    );
    let records = Records::read(root, &day);
    // Its own folder, so it replaces no tool's output; and none it lists.
    let out = dir.join("report");
    match std::fs::symlink_metadata(&out) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => anyhow::bail!("{} is not a folder: no report written", out.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&out).with_context(|| format!("making {}", out.display()))?
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", out.display())),
    }
    let unlisted = |path: &Path| -> Result<()> {
        match records.steps.iter().find(|s| root.join(&s.output) == path) {
            Some(s) => anyhow::bail!(
                "{} is an output of {} {}: not replaced",
                path.display(),
                s.server,
                s.tool
            ),
            None => Ok(()),
        }
    };
    let fresh = |path: &Path, bytes: &[u8]| -> Result<()> {
        unlisted(path)?;
        crate::project::write_fresh(path, bytes)
    };
    let steps: Vec<&Step> = records.steps.iter().filter(|s| s.date == day).collect();
    // The graph, and how it fits a slide: as wide as the slide when it is
    // wider than the room for it, else as tall.
    let graph = if steps.is_empty() && records.notebook.is_empty() {
        None
    } else {
        let path = out.join("graph.svg");
        let (svg, w, h) = Graph::of(&steps, &records.notebook).drawn(GRAPH_ROOM.0 / GRAPH_ROOM.1);
        fresh(&path, svg.as_bytes())?;
        Some((path, w / h > GRAPH_ROOM.0 / GRAPH_ROOM.1))
    };
    let name = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    let deck = slides(root, &day, &records, &steps, graph.as_ref().map(|g| g.1));
    let md = out.join("report.md");
    fresh(&md, markdown(&deck, &name, &day).as_bytes())?;
    let pdf_path = out.join("report.pdf");
    unlisted(&pdf_path)?;
    let pdf = pdf(root, &typst(&deck, &name, &day)).and_then(|bytes| {
        crate::project::write_fresh(&pdf_path, &bytes)
            .map(|()| pdf_path.clone())
            .map_err(|e| format!("{e:#}"))
    });
    if pdf.is_err() {
        // An earlier build's slides would disagree with report.md.
        let _ = std::fs::remove_file(&pdf_path);
    }
    Ok(Built {
        pdf,
        md,
        graph: graph.map(|g| g.0),
        summary: records.notebook.iter().any(|e| e.kind == "summary"),
    })
}

/// What was made, in words, paths from the project.
pub(crate) fn said(root: &Path, b: &Built) -> String {
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let mut out = match &b.pdf {
        Ok(p) => format!("Slides: {}", rel(p)),
        Err(why) => format!("No PDF ({why})."),
    };
    out += &format!("\nThe same, as text: {}", rel(&b.md));
    if let Some(g) = &b.graph {
        out += &format!("\nThe trail of the day's steps: {}", rel(g));
    }
    if !b.summary {
        out += "\nThe person has not summarised the day: the slides open with their \
                question. Ask them for the day's summary (kind `summary`, their words) and \
                build the report again.";
    }
    out
}

/// `day`, checked (`YYYY-MM-DD`: it names a folder), or today.
fn day_of(day: Option<&str>) -> Result<String> {
    let Some(d) = day else {
        return Ok(crate::project::now().0);
    };
    let ok = d.len() == 10
        && d.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    anyhow::ensure!(ok, "a day is YYYY-MM-DD, not {d}");
    Ok(d.to_string())
}

/// A slide: its title and what it shows, in the order shown.
struct Slide {
    title: String,
    blocks: Vec<Block>,
}

/// A table's cell: its text, a dimmer second line, and whether it is
/// flagged.
#[derive(Default)]
struct Cell {
    text: String,
    sub: String,
    warn: bool,
}

impl Cell {
    fn of(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// What a slide shows. Every string is read from the records, and is shown
/// as written.
enum Block {
    Text(String),
    Dim(String),
    Warn(String),
    /// A notebook entry: where it is from, and its words.
    Quote(String, String),
    List(Vec<String>),
    /// A labelled line: `Verb: ...`.
    Field(String, String),
    Table(Vec<String>, Vec<Vec<Cell>>),
    /// An image under the project, its caption and a note on how it was
    /// made; `wide`: as wide as the slide, else as tall as its room. `view`:
    /// for a PDF figure, the picture beside it that a Markdown viewer can
    /// show.
    Image {
        path: String,
        view: Option<String>,
        caption: String,
        note: String,
        wide: bool,
    },
    Code(String),
}

/// The slides of the day, from the records.
fn slides(
    root: &Path,
    day: &str,
    records: &Records,
    steps: &[&Step],
    graph: Option<bool>,
) -> Vec<Slide> {
    let dir = root.join("results").join(day);
    let notebook = format!("results/{day}/notebook.md");
    let entries = |keep: &dyn Fn(&str) -> bool| -> Vec<&Entry> {
        records.notebook.iter().filter(|e| keep(&e.kind)).collect()
    };
    // An entry: the person's words (its `> ` lines) quoted, what the
    // agent recorded with them plain.
    let quote = |e: &Entry, n: usize| -> Vec<Block> {
        let head = format!("{} · {} · {}", e.time, e.server, e.kind);
        let (theirs, rest) = split_words(&e.text);
        let mut blocks = Vec::new();
        if !theirs.is_empty() {
            blocks.push(Block::Quote(head.clone(), cut(&theirs, n, &notebook)));
        }
        if !rest.is_empty() {
            if theirs.is_empty() {
                blocks.push(Block::Dim(head));
            }
            blocks.push(Block::Text(cut(&rest, n, &notebook)));
        }
        blocks
    };
    let mut deck = Vec::new();

    // The person's summary of the day.
    let summaries = entries(&|k| k == "summary");
    if !summaries.is_empty() {
        let mut blocks = vec![Block::Dim(
            "In the person's words; every slide after this one is read from the records.".into(),
        )];
        blocks.extend(summaries.into_iter().flat_map(|e| quote(e, 2500)));
        deck.push(Slide {
            title: "The day, in the person's words".into(),
            blocks,
        });
    }

    // The question.
    let plans = entries(&|k| k == "plan");
    let mut blocks: Vec<Block> = plans.into_iter().flat_map(|e| quote(e, 1200)).collect();
    if blocks.is_empty() {
        blocks.push(Block::Warn("No question was recorded this day.".into()));
        blocks.push(Block::Dim(
            "A plan is the person's question, as a contrast, and what would change their mind."
                .into(),
        ));
    }
    deck.push(Slide {
        title: "The question".into(),
        blocks,
    });

    // The data.
    let mut blocks = Vec::new();
    if records.data.is_empty() {
        blocks.push(Block::Dim("No files under data/.".into()));
    } else {
        // As many as a slide holds; the rest counted.
        const ROWS: usize = 12;
        let rows = records
            .data
            .iter()
            .take(ROWS)
            .map(|d| {
                let from = match d.from.as_str() {
                    "" => Cell {
                        text: "not recorded".into(),
                        warn: true,
                        ..Default::default()
                    },
                    f => Cell::of(f),
                };
                let verdict = match d.verdict.as_str() {
                    "" => Cell {
                        text: "not given".into(),
                        warn: true,
                        ..Default::default()
                    },
                    v @ ("caveats" | "bad") => Cell {
                        text: v.into(),
                        warn: true,
                        ..Default::default()
                    },
                    v => Cell::of(v),
                };
                vec![Cell::of(&d.path), from, verdict]
            })
            .collect();
        blocks.push(Block::Table(
            vec![
                "File".into(),
                "Where it came from".into(),
                "The person's verdict".into(),
            ],
            rows,
        ));
        let rest = &records.data[records.data.len().min(ROWS)..];
        if !rest.is_empty() {
            let count =
                |f: &dyn Fn(&crate::records::Data) -> bool| rest.iter().filter(|d| f(d)).count();
            blocks.push(Block::Dim(format!(
                "{} more files under data/: {} with where they came from not recorded, {} \
                 without a verdict.",
                rest.len(),
                count(&|d| d.from.is_empty()),
                count(&|d| d.verdict.is_empty())
            )));
        }
    }
    deck.push(Slide {
        title: "The data".into(),
        blocks,
    });

    // The steps.
    if let Some(wide) = graph {
        deck.push(Slide {
            title: "The trail".into(),
            blocks: vec![Block::Image {
                path: format!("results/{day}/report/graph.svg"),
                view: None,
                caption: "The person's entries in order (blue); under each, what ran (rounded; \
                          red flagged a number) and was seen (dashed) after it. A faint line: \
                          a step read a file another made."
                    .into(),
                note: String::new(),
                wide,
            }],
        });
    }
    let mut blocks = Vec::new();
    if steps.is_empty() {
        blocks.push(Block::Dim(
            "No step recorded an output this day (results/INDEX.tsv).".into(),
        ));
    } else {
        let rows = steps
            .iter()
            .map(|st| {
                let numbers: Vec<String> = st.facts.iter().map(ToString::to_string).collect();
                vec![
                    Cell::of(clock(&st.time)),
                    Cell::of(format!("{} {}", st.server, st.tool)),
                    Cell {
                        text: st.output.clone(),
                        sub: if st.inputs.is_empty() {
                            String::new()
                        } else {
                            format!("from {}", st.inputs.join(", "))
                        },
                        warn: false,
                    },
                    Cell {
                        text: numbers.join("; "),
                        warn: st.facts.iter().any(|f| f.flagged().is_some()),
                        ..Default::default()
                    },
                ]
            })
            .collect();
        blocks.push(Block::Table(
            vec![
                "Time".into(),
                "Step".into(),
                "Output, from".into(),
                "Key numbers".into(),
            ],
            rows,
        ));
        let flagged: Vec<String> = steps
            .iter()
            .flat_map(|st| {
                st.facts
                    .iter()
                    .filter(|f| f.flagged().is_some())
                    .map(move |f| format!("{} ({} {}): {f}", st.output, st.server, st.tool))
            })
            .collect();
        if !flagged.is_empty() {
            blocks.push(Block::Warn(
                "Flagged by the tools: look at these before building on them.".into(),
            ));
            blocks.push(Block::List(flagged));
        }
    }
    deck.push(Slide {
        title: "The steps, with their key numbers".into(),
        blocks,
    });

    // The figures the person kept, then the day's other images.
    let runall = read_record(&dir.join("runall.sh"));
    let made = |output: &str| -> String {
        match steps.iter().find(|st| st.output == output) {
            Some(st) => made_by(&runall, st, day),
            None => String::new(),
        }
    };
    // Each file shown once: a PNG and the PDF or SVG beside it are one.
    let mut shown: Vec<String> = Vec::new();
    for f in &records.figures {
        let mut blocks = Vec::new();
        if is_figure(root, &f.output) {
            let path = sharpest(root, &f.output);
            shown.push(path.clone());
            blocks.push(Block::Image {
                view: viewable(root, &path),
                path,
                caption: f.take_home.clone(),
                note: made(&f.output),
                wide: true,
            });
        } else {
            blocks.push(Block::Text(f.take_home.clone()));
            blocks.push(Block::Warn(if root.join(&f.output).exists() {
                format!(
                    "{} is not shown: the slides show only the person's own image files, \
                     not links or files others can change.",
                    f.output
                )
            } else {
                format!("{} is not there.", f.output)
            }));
        }
        if !f.hypothesis.is_empty() {
            blocks.push(Block::Field("For".into(), f.hypothesis.clone()));
        }
        deck.push(Slide {
            title: format!("Figure {}", f.number),
            blocks,
        });
    }
    for st in steps.iter().filter(|st| is_figure(root, &st.output)) {
        let path = sharpest(root, &st.output);
        if shown.contains(&path) {
            continue;
        }
        shown.push(path.clone());
        deck.push(Slide {
            title: format!("Image: {}", file_name(&st.output)),
            blocks: vec![Block::Image {
                view: viewable(root, &path),
                path,
                caption: st.summary.clone(),
                note: made_by(&runall, st, day),
                wide: true,
            }],
        });
    }

    // What was seen.
    let seen = entries(&|k| k.starts_with("observation") || k.starts_with("note") || k == "user");
    if !seen.is_empty() {
        deck.push(Slide {
            title: "What was seen".into(),
            blocks: seen.into_iter().flat_map(|e| quote(e, 900)).collect(),
        });
    }

    // The hypotheses.
    for h in &records.hypotheses {
        let l = &h.ledger;
        let mut blocks = vec![Block::Quote(
            "the person's hypothesis".into(),
            h.claim.clone(),
        )];
        let said = |label: &str, text: &str, missing: &str| -> Block {
            if text.is_empty() {
                Block::Field(label.into(), format!("not said yet: {missing}"))
            } else {
                Block::Field(label.into(), text.into())
            }
        };
        blocks.push(said("Rivals", &h.rivals, "what else could explain it?"));
        blocks.push(said(
            "Would nullify it",
            &h.nullify,
            "what result would make them drop it?",
        ));
        if l.sources.is_empty() {
            blocks.push(Block::Dim("No source has been weighed for it yet.".into()));
        } else {
            let rows = l
                .sources
                .iter()
                .map(|e| {
                    let (kind, shares) = evidence::what(&e.class);
                    vec![
                        Cell::of(&e.source),
                        Cell {
                            text: kind.into(),
                            sub: format!("shares {shares}"),
                            warn: false,
                        },
                        Cell {
                            text: e.verdict.clone(),
                            warn: e.verdict != "agrees",
                            ..Default::default()
                        },
                        Cell {
                            text: e.why.clone(),
                            sub: e.summary.clone(),
                            warn: false,
                        },
                    ]
                })
                .collect();
            blocks.push(Block::Table(
                vec![
                    "Source".into(),
                    "Kind".into(),
                    "Says".into(),
                    "Why, and the numbers".into(),
                ],
                rows,
            ));
        }
        blocks.push(Block::Field("Verb".into(), l.verb.clone()));
        blocks.push(Block::Field(
            "Standing".into(),
            format!("{} (level {} of 3)", l.standing, l.level),
        ));
        if !l.unused.is_empty() {
            let names: Vec<String> = l
                .unused
                .iter()
                .map(|c| evidence::what(c).0.to_string())
                .collect();
            blocks.push(Block::Field(
                "Sources not yet used".into(),
                names.join("; "),
            ));
        }
        deck.push(Slide {
            title: format!("Hypothesis H{}", h.id),
            blocks,
        });
    }

    // What is open for the person.
    let open = records.open();
    if !open.is_empty() {
        deck.push(Slide {
            title: "Open for the person".into(),
            blocks: vec![Block::List(open)],
        });
    }

    // Replay.
    let mut blocks = vec![
        Block::Text("Every step of the day, in order, without the agent:".into()),
        Block::Code(format!("bash results/{day}/runall.sh")),
    ];
    let mut tools: Vec<String> = Vec::new();
    for l in runall.lines().filter_map(|l| l.strip_prefix("# ")) {
        let w: Vec<&str> = l.split_whitespace().collect();
        // `# 10:02:03 server version tool {args}`
        // The notebook's own lines are not a tool's.
        if w.len() >= 4
            && w[0].len() == 8
            && w[0].as_bytes()[2] == b':'
            && w[1] != crate::brain::NAME
        {
            let t = format!("{} {}", w[1], w[2]);
            if !tools.contains(&t) {
                tools.push(t);
            }
        }
    }
    if !tools.is_empty() {
        blocks.push(Block::Field("Tools".into(), tools.join(", ")));
    }
    blocks.push(Block::Dim(format!(
        "The records: results/{day}/notebook.md (the day's account), results/{day}/runall.sh \
         (the steps), results/INDEX.tsv (each output, its inputs and numbers), \
         results/HYPOTHESES.tsv and EVIDENCE.tsv (the triangulation), results/DATA.tsv and \
         FIGURES.tsv (the person's verdicts and kept figures)."
    )));
    deck.push(Slide {
        title: "Replay".into(),
        blocks,
    });
    deck
}

/// A notebook entry's text in two: the person's words (its `> ` lines,
/// without the marks; the slides set them apart themselves), and what the
/// agent recorded with them.
fn split_words(text: &str) -> (String, String) {
    let (mut theirs, mut rest) = (Vec::new(), Vec::new());
    for l in text.lines() {
        match l.strip_prefix("> ").or(l.strip_prefix('>')) {
            Some(w) => theirs.push(w),
            None => rest.push(l),
        }
    }
    let joined = |v: Vec<&str>| v.join("\n").trim().to_string();
    (joined(theirs), joined(rest))
}

/// `10:02:03` as `10:02`.
fn clock(time: &str) -> String {
    time.chars().take(5).collect()
}

/// `text` cut to `n` characters, saying where the rest is.
fn cut(text: &str, n: usize, rest: &str) -> String {
    if text.chars().count() <= n {
        return text.into();
    }
    let kept: String = text.chars().take(n).collect();
    format!("{kept}… (the rest in {rest})")
}

/// The sharpest version of image `output` there is: the SVG, else the PDF,
/// beside a PNG or JPEG of the same name, which a slide scales without
/// blurring.
fn sharpest(root: &Path, output: &str) -> String {
    let Some((stem, ext)) = output.rsplit_once('.') else {
        return output.into();
    };
    if matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg") {
        for vector in ["svg", "pdf"] {
            let path = format!("{stem}.{vector}");
            if is_figure(root, &path) {
                return path;
            }
        }
    }
    output.into()
}

/// For a PDF figure, the picture of the same name beside it (`.svg`, `.png`,
/// `.jpg`) that a Markdown viewer can show, which cannot show a PDF.
fn viewable(root: &Path, path: &str) -> Option<String> {
    let (stem, ext) = path.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("pdf") {
        return None;
    }
    ["svg", "png", "jpg", "jpeg"]
        .iter()
        .map(|e| format!("{stem}.{e}"))
        .find(|p| is_figure(root, p))
}

/// Whether `output` is a figure the slides can show: an image (or a PDF
/// figure) inside the project that is there, and that Typst may read (the
/// person's own file: no link, none others can change).
fn is_figure(root: &Path, output: &str) -> bool {
    let image = ["png", "jpg", "jpeg", "gif", "svg", "pdf"].iter().any(|e| {
        output
            .rsplit_once('.')
            .is_some_and(|(_, x)| x.eq_ignore_ascii_case(e))
    });
    image
        && !output.starts_with('/')
        && !output.split('/').any(|p| p == "..")
        && crate::project::is_trusted(&root.join(output))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Where in `runall.sh` the step that made `st.output` is: its line and
/// command.
fn made_by(runall: &str, st: &Step, day: &str) -> String {
    let lines: Vec<&str> = runall.lines().collect();
    let guard = lines
        .iter()
        .position(|l| l.starts_with("if [ ! -e ") && l.contains(st.output.as_str()));
    if let Some(i) = guard {
        let command = lines[i + 1..]
            .iter()
            .map(|l| l.trim())
            .take_while(|l| *l != "fi")
            .find(|l| !l.starts_with("mkdir -p "));
        if let Some(c) = command {
            return format!(
                "results/{day}/runall.sh, line {}: {}",
                i + 1,
                cut(c, 220, "runall.sh")
            );
        }
    }
    format!(
        "Made by {} {} at {}; its step is in results/{day}/runall.sh.",
        st.server, st.tool, st.time
    )
}

/// `text` as a Typst string literal: shown as written, never read as
/// markup or code.
fn lit(text: &str) -> String {
    let mut s = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\t' => s.push_str("    "),
            c if c.is_control() => {}
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// `text` as Typst markup: each line a string of its own, a blank line
/// between paragraphs, Markdown's bold marks dropped.
fn para(text: &str) -> String {
    let mut out = String::new();
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let next_blank = lines.get(i + 1).is_none_or(|n| n.trim().is_empty());
        out.push_str(&format!("#{}", lit(&l.replace("**", ""))));
        out.push_str(if next_blank { "\n" } else { " \\\n" });
    }
    out
}

/// The page set up, in the manner of a Beamer deck: a title bar on each
/// slide, a footline, Computer Modern.
const PREAMBLE: &str = r##"#let ink = rgb("#1d3557")
#set page(paper: "presentation-16-9", margin: (x: 1.4cm, top: 2.3cm, bottom: 1.3cm),
  footer: context {
    set text(size: 9pt, fill: white)
    move(dx: -1.4cm, block(width: 100% + 2.8cm, inset: (x: 1.4cm, y: 0.25cm), fill: ink, outset: (bottom: 1cm))[
      #footer-text #h(1fr) #counter(page).display() / #counter(page).final().first()
    ])
  }, footer-descent: 0.6cm)
#set text(font: "New Computer Modern", size: 15pt)
#show raw: set text(font: "DejaVu Sans Mono", size: 12pt)
#set par(justify: false, leading: 0.6em)
#let slide(title, body) = {
  pagebreak(weak: true)
  // A long title is set smaller, so it stays on the bar.
  let size = if title.len() > 52 { 16pt } else { 21pt }
  place(top + left, dx: -1.4cm, dy: -2.3cm, block(width: 100% + 2.8cm, height: 1.6cm,
    inset: (x: 1.4cm), fill: ink, align(horizon, text(size: size, fill: white, weight: "bold", title))))
  body
}
#let dim(body) = text(fill: luma(100), body)
#let warn(body) = text(fill: rgb("#b03a2e"), weight: "bold", body)
#let quoted(body) = block(inset: (left: 0.9em, y: 0.3em), stroke: (left: 2pt + ink.lighten(50%)), body)
#let small(body) = { set text(size: 11pt); body }
"##;

/// The slides as Typst source.
fn typst(deck: &[Slide], name: &str, day: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "#let footer-text = {}", lit(&format!("{name} · {day}")));
    let _ = writeln!(
        s,
        "#set document(title: {})",
        lit(&format!("{name}, {day}"))
    );
    s.push_str(PREAMBLE);
    let _ = writeln!(
        s,
        "\n#align(horizon)[\n  #text(size: 34pt, weight: \"bold\", fill: ink)[#{}]\n\n  \
         #text(size: 20pt)[#{}]\n\n  #dim[Built by legume {} from the project's records.] \\\n  \
         #dim[Exploratory work: what it found are hypotheses, not findings.]\n]",
        lit(name),
        lit(day),
        env!("CARGO_PKG_VERSION")
    );
    for slide in deck {
        let _ = writeln!(s, "\n#slide({})[", lit(&slide.title));
        for b in &slide.blocks {
            s.push_str(&typst_block(b));
        }
        s.push_str("]\n");
    }
    s
}

fn typst_block(b: &Block) -> String {
    match b {
        Block::Text(t) => format!("{}\n", para(t)),
        Block::Dim(t) => format!("#dim[#{}]\n\n", lit(t)),
        Block::Warn(t) => format!("#warn[#{}]\n\n", lit(t)),
        Block::Quote(from, t) => format!("#small[#dim[#{}]]\n#quoted[\n{}]\n", lit(from), para(t)),
        Block::List(items) => {
            let mut s: String = items.iter().map(|i| format!("- #{}\n", lit(i))).collect();
            s.push('\n');
            s
        }
        Block::Field(label, value) => format!("*#{}:* #{}\n\n", lit(label), lit(value)),
        Block::Table(head, rows) => {
            let mut s = format!(
                "#small[#table(columns: {}, stroke: 0.5pt + luma(200), inset: 5pt,\n  table.header({}),\n",
                head.len(),
                head.iter()
                    .map(|h| format!("[*#{}*]", lit(h)))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for row in rows {
                let cells: Vec<String> = row
                    .iter()
                    .map(|c| {
                        let text = if c.warn {
                            format!("#warn[#{}]", lit(&c.text))
                        } else {
                            format!("#{}", lit(&c.text))
                        };
                        if c.sub.is_empty() {
                            format!("[{text}]")
                        } else {
                            format!("[{text} \\ #dim[#{}]]", lit(&c.sub))
                        }
                    })
                    .collect();
                let _ = writeln!(s, "  {},", cells.join(", "));
            }
            s.push_str(")]\n\n");
            s
        }
        Block::Image {
            path,
            caption,
            note,
            wide,
            ..
        } => {
            let size = if *wide {
                "width: 100%, height: 9cm, fit: \"contain\"".to_string()
            } else {
                format!("height: {}cm", GRAPH_ROOM.1)
            };
            let mut s = format!(
                "#align(center, image({}, {size}))\n#dim[#{}]",
                lit(&format!("/{path}")),
                lit(caption)
            );
            if !note.is_empty() {
                let _ = write!(s, " \\\n#small[#dim[#{}]]", lit(note));
            }
            s.push_str("\n\n");
            s
        }
        Block::Code(c) => format!("#raw({}, block: true, lang: \"bash\")\n\n", lit(c)),
    }
}

/// `text` safe in Markdown: shown as written, never as a heading, a link,
/// HTML or a table's border.
fn md(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '#' | '|' | '!' => {
                out.push('\\');
                out.push(c);
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\t' => out.push_str("    "),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// The slides as Markdown, for any editor; images linked from the report's
/// folder.
fn markdown(deck: &[Slide], name: &str, day: &str) -> String {
    let mut s = format!(
        "# {}, {}\n\nBuilt by legume {} from the project's records. Exploratory work: what it \
         found are hypotheses, not findings.\n",
        md(name),
        md(day),
        env!("CARGO_PKG_VERSION")
    );
    for slide in deck {
        let _ = writeln!(s, "\n## {}\n", md(&slide.title));
        for b in &slide.blocks {
            s.push_str(&md_block(b, day));
        }
    }
    s
}

fn md_block(b: &Block, day: &str) -> String {
    let one = |t: &str| md(&t.replace('\n', " "));
    match b {
        Block::Text(t) => format!("{}\n\n", md(t)),
        Block::Dim(t) => format!("_{}_\n\n", one(t)),
        Block::Warn(t) => format!("**{}**\n\n", one(t)),
        Block::Quote(from, t) => {
            let lines: Vec<String> = t.lines().map(|l| format!("> {}", md(l))).collect();
            format!("_{}_\n\n{}\n\n", one(from), lines.join("\n"))
        }
        Block::List(items) => {
            let mut s: String = items.iter().map(|i| format!("- {}\n", one(i))).collect();
            s.push('\n');
            s
        }
        Block::Field(label, value) => format!("**{}:** {}\n\n", one(label), one(value)),
        Block::Table(head, rows) => {
            let mut s = format!(
                "| {} |\n|{}\n",
                head.iter().map(|h| one(h)).collect::<Vec<_>>().join(" | "),
                "---|".repeat(head.len())
            );
            for row in rows {
                let cells: Vec<String> = row
                    .iter()
                    .map(|c| {
                        let text = if c.warn {
                            format!("**{}**", one(&c.text))
                        } else {
                            one(&c.text)
                        };
                        if c.sub.is_empty() {
                            text
                        } else {
                            format!("{text}<br>_{}_", one(&c.sub))
                        }
                    })
                    .collect();
                let _ = writeln!(s, "| {} |", cells.join(" | "));
            }
            s.push('\n');
            s
        }
        Block::Image {
            path,
            view,
            caption,
            note,
            ..
        } => {
            // The report is in results/<day>/report/: three folders down.
            let own = format!("results/{day}/report/");
            let to_link = |path: &str| -> String {
                let link = match path.strip_prefix(&own) {
                    Some(name) => name.to_string(),
                    None => format!("../../../{path}"),
                };
                // Any character a link could end at, or be read as, escaped.
                link.chars()
                    .map(|c| match c {
                        'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' => {
                            c.to_string()
                        }
                        c => c.to_string().bytes().map(|b| format!("%{b:02X}")).collect(),
                    })
                    .collect()
            };
            let link = to_link(path);
            // Markdown viewers show images, not PDFs: a PDF is shown by the
            // picture beside it, if there is one, and linked.
            let pdf = link.to_ascii_lowercase().ends_with(".pdf");
            let mut s = match view {
                Some(view) if pdf => format!(
                    "![{}]({})\n\n_{}_ ([PDF]({link}))\n\n",
                    one(caption),
                    to_link(view),
                    one(caption)
                ),
                _ => format!(
                    "{}[{}]({link})\n\n_{}_\n\n",
                    if pdf { "" } else { "!" },
                    one(caption),
                    one(caption)
                ),
            };
            if !note.is_empty() {
                let _ = write!(s, "_{}_\n\n", one(note));
            }
            s
        }
        Block::Code(c) => format!("```bash\n{}\n```\n\n", c.replace("```", "")),
    }
}

/// The PDF of Typst `source`, its images read from the project at `root`
/// alone; or why there is none.
#[cfg(feature = "pdf")]
fn pdf(root: &Path, source: &str) -> std::result::Result<Vec<u8>, String> {
    typeset::pdf(root, source)
}

#[cfg(not(feature = "pdf"))]
fn pdf(_: &Path, _: &str) -> std::result::Result<Vec<u8>, String> {
    Err(
        "this legume was built without PDFs: report.md has the slides \
         (`cargo install legume-mcp --features pdf` makes them a PDF too)"
            .into(),
    )
}

/// Typst, in this program: the slides' source, the fonts Typst ships, and
/// the project's files as the only files it can read.
#[cfg(feature = "pdf")]
mod typeset {
    use std::path::{Path, PathBuf};
    use std::sync::LazyLock;
    use typst::diag::{FileError, FileResult};
    use typst::foundations::{Bytes, Datetime, Duration};
    use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
    use typst::text::{Font, FontBook};
    use typst::utils::LazyHash;
    use typst::{Library, LibraryExt, World};
    use typst_layout::PagedDocument;

    static LIBRARY: LazyLock<LazyHash<Library>> =
        LazyLock::new(|| LazyHash::new(Library::default()));
    static FONTS: LazyLock<(LazyHash<FontBook>, Vec<Font>)> = LazyLock::new(|| {
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|d| Font::iter(Bytes::new(d)))
            .collect();
        (LazyHash::new(FontBook::from_fonts(&fonts)), fonts)
    });

    struct Slides {
        root: PathBuf,
        main: Source,
    }

    impl World for Slides {
        fn library(&self) -> &LazyHash<Library> {
            &LIBRARY
        }
        fn book(&self) -> &LazyHash<FontBook> {
            &FONTS.0
        }
        fn main(&self) -> FileId {
            self.main.id()
        }
        fn source(&self, id: FileId) -> FileResult<Source> {
            if id == self.main.id() {
                Ok(self.main.clone())
            } else {
                Err(FileError::NotSource)
            }
        }
        /// A file of the project: the path Typst gives is rooted in it.
        fn file(&self, id: FileId) -> FileResult<Bytes> {
            let rel = id
                .vpath()
                .get_with_slash()
                .trim_start_matches('/')
                .to_string();
            let path = self.root.join(&rel);
            match crate::project::read_trusted(&path) {
                Ok(b) => Ok(Bytes::new(b)),
                Err(e) => Err(FileError::from_io(e, &path)),
            }
        }
        fn font(&self, index: usize) -> Option<Font> {
            FONTS.1.get(index).cloned()
        }
        fn today(&self, _: Option<Duration>) -> Option<Datetime> {
            None
        }
    }

    pub(super) fn pdf(root: &Path, source: &str) -> Result<Vec<u8>, String> {
        let id = VirtualPath::new("/report.typ")
            .map(|v| RootedPath::new(VirtualRoot::Project, v).intern())
            .map_err(|e| format!("{e:?}"))?;
        let world = Slides {
            root: root.to_path_buf(),
            main: Source::new(id, source.to_string()),
        };
        let doc = typst::compile::<PagedDocument>(&world)
            .output
            .map_err(|errs| {
                let msgs: Vec<String> = errs.iter().map(|e| e.message.to_string()).collect();
                format!("the slides did not typeset: {}", msgs.join("; "))
            })?;
        typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).map_err(|errs| {
            let msgs: Vec<String> = errs.iter().map(|e| e.message.to_string()).collect();
            format!("the PDF was not written: {}", msgs.join("; "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::Fact;
    use serde_json::json;

    #[test]
    fn record_text_is_a_string_never_markup_or_code() {
        assert_eq!(lit(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(lit("x\u{1b}[2Jy"), "\"x[2Jy\"");
        assert_eq!(
            para("#read(\"/etc/passwd\")\n**R1. claim**\n\nnext"),
            "#\"#read(\\\"/etc/passwd\\\")\" \\\n#\"R1. claim\"\n\n#\"next\"\n"
        );
        assert_eq!(
            md("# H1 <script>![x](y)</script> | *b*"),
            "\\# H1 &lt;script&gt;\\!\\[x\\](y)&lt;/script&gt; \\| \\*b\\*"
        );
        assert!(day_of(Some("../../x")).is_err());
        assert!(day_of(Some("2026-10-28")).is_ok());
    }

    #[test]
    fn a_slide_shows_the_vector_figure_beside_a_png() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let touch = |f: &str| std::fs::write(root.join(f), "").unwrap();
        touch("qc.png");
        assert_eq!(sharpest(root, "qc.png"), "qc.png");
        touch("qc.pdf");
        assert_eq!(sharpest(root, "qc.png"), "qc.pdf");
        touch("qc.svg");
        assert_eq!(sharpest(root, "qc.png"), "qc.svg");
        let image = |path: &str| {
            md_block(
                &Block::Image {
                    path: path.into(),
                    view: None,
                    caption: "qc".into(),
                    note: String::new(),
                    wide: false,
                },
                "2026-10-28",
            )
        };
        assert!(image("results/2026-10-28/qc.pdf")
            .starts_with("[qc](../../../results/2026-10-28/qc.pdf)"));
        // The report's own folder, and no other day's, is here.
        assert!(image("results/2026-10-28/report/graph.svg").starts_with("![qc](graph.svg)"));
        assert!(image("results/2026-10-27/report/graph.svg")
            .starts_with("![qc](../../../results/2026-10-27/report/graph.svg)"));
        assert!(image("results/a (1).png").starts_with("![qc](../../../results/a%20%281%29.png)"));

        // A PDF figure, in Markdown, by the picture beside it.
        std::fs::remove_file(root.join("qc.svg")).unwrap();
        assert_eq!(viewable(root, "qc.pdf").as_deref(), Some("qc.png"));
        assert_eq!(viewable(root, "qc.png"), None);
        let shown = md_block(
            &Block::Image {
                path: "results/2026-10-28/qc.pdf".into(),
                view: Some("results/2026-10-28/qc.png".into()),
                caption: "qc".into(),
                note: String::new(),
                wide: true,
            },
            "2026-10-28",
        );
        assert!(
            shown.starts_with(
                "![qc](../../../results/2026-10-28/qc.png)\n\n_qc_ ([PDF](../../../results/2026-10-28/qc.pdf))"
            ),
            "{shown}"
        );
    }

    #[test]
    fn the_persons_words_are_quoted_and_the_agents_are_not() {
        let (theirs, rest) = split_words("> Why 4,\n> not 2?\n\nData: data/a.h5, 6 donors.");
        assert_eq!(theirs, "Why 4,\nnot 2?");
        assert_eq!(rest, "Data: data/a.h5, 6 donors.");
    }

    #[test]
    fn each_picture_is_shown_once_and_only_the_persons_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let day = crate::project::now().0;
        let d = root.join("results").join(&day);
        std::fs::create_dir_all(d.join("report")).unwrap();
        for f in ["umap.png", "umap.svg", "shared.png"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o664);
            std::fs::set_permissions(d.join("shared.png"), perms).unwrap();
        }
        let line = |tool: &str, out: &str| {
            format!("{day}\t10:00:00\tsenna\t{tool}\tresults/{day}/{out}\tthe map\t\t[]\n")
        };
        std::fs::write(
            root.join("results/INDEX.tsv"),
            format!(
                "{}\n{}{}{}",
                crate::project::INDEX_HEAD,
                line("view_export", "umap.png"),
                line("view_export", "umap.svg"),
                line("plot", "shared.png")
            ),
        )
        .unwrap();
        let p = Project::new(root.to_path_buf(), "legume", "0");
        p.ledger(
            crate::notebook::FIGURES,
            &[vec![
                "3".into(),
                format!("results/{day}/gone.png"),
                String::new(),
                "x".into(),
            ]],
        );
        // An earlier build's slides, which must not outlive a failed one.
        std::fs::write(d.join("report/report.pdf"), "old").unwrap();
        let built = build(root, Some(&day)).unwrap();
        let md = std::fs::read_to_string(&built.md).unwrap();
        assert_eq!(md.matches("umap.svg)").count(), 1, "{md}");
        #[cfg(unix)]
        assert!(
            !md.contains("## Image: shared.png"),
            "others can change it: {md}"
        );
        assert!(md.contains("gone.png is not there"), "{md}");
        if built.pdf.is_err() {
            assert!(!d.join("report/report.pdf").exists());
        }
    }

    #[test]
    fn the_slides_come_from_the_records_and_the_persons_words() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pbmc");
        let day = crate::project::now().0;
        let d = root.join("results").join(&day);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/pbmc.h5"), "x").unwrap();
        // A real PNG, 1x1, for the PDF to embed.
        std::fs::write(
            d.join("umap.png"),
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("results/INDEX.tsv"),
            format!(
                "{}\n\
                 {day}\t10:00:00\tdata-beans\tqc\tresults/{day}/qc.zarr.zip\tkept cells\tdata/pbmc.h5\t{}\n\
                 {day}\t10:05:00\tsenna\tview_export\tresults/{day}/umap.png\tthe map by cluster\tresults/{day}/qc.zarr.zip\t[]\n\
                 2026-10-27\t09:00:00\tsenna\tfit\tresults/2026-10-27/old.json\told\t\t[]\n",
                crate::project::INDEX_HEAD,
                crate::step::facts_cell(&[
                    Fact::count("cells", 9812).of(11204),
                    Fact::value("mito", 0.31).flag("above 0.2"),
                ]),
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("runall.sh"),
            format!(
                "#!/usr/bin/env bash\n\n# 10:05:00 senna 0.9.0 view_export {{}}\n\
                 if [ ! -e results/{day}/umap.png ]; then\n  mkdir -p results/{day}\n  senna view export --out results/{day}/umap.png\nfi\n"
            ),
        )
        .unwrap();
        let p = Project::new(root.clone(), "legume", "0");
        let note = |args: serde_json::Value| {
            let (label, text, _) = crate::notebook::notebook_entry(&args, &p).unwrap();
            p.notebook(&label, &text);
        };
        note(
            json!({ "kind": "plan", "words": "Why is cluster 4 apart, rather than with 2? #set page(width: 1cm)" }),
        );
        note(
            json!({ "kind": "data", "path": "data/pbmc.h5", "words": "10x PBMC, from 10x Genomics", "verdict": "caveats" }),
        );
        note(
            json!({ "kind": "hypothesis", "words": "Cluster 4 is stressed *monocytes*", "rivals": ["damaged cells"] }),
        );
        note(
            json!({ "kind": "figure", "output": format!("results/{day}/umap.png"), "number": "2B", "hypothesis": "H1", "words": "cluster 4 sits apart" }),
        );
        note(json!({ "kind": "summary", "words": "We asked why cluster 4 stands apart." }));

        let b = build(&root, Some(&day)).unwrap();
        assert!(b.summary && b.graph.is_some());
        let text = std::fs::read_to_string(&b.md).unwrap();
        for want in [
            "## The day, in the person's words",
            "> We asked why cluster 4 stands apart.",
            "> Why is cluster 4 apart, rather than with 2? \\#set page(width: 1cm)",
            "| data/pbmc.h5 | 10x PBMC, from 10x Genomics | **caveats** |",
            "(flagged: above 0.2)**",
            "## Figure 2B",
            "](../../../results/",
            "## Hypothesis H1",
            "> Cluster 4 is stressed \\*monocytes\\*",
            "**Rivals:** damaged cells",
            "**Would nullify it:** not said yet",
            "runall.sh, line 4: senna view export",
            "H1: what result would make you drop it?",
        ] {
            assert!(text.contains(want), "{want} in {text}");
        }
        assert!(!text.contains("old.json"), "another day's steps stay out");
        assert!(
            !text.contains("## Image: umap.png"),
            "a kept figure shows once"
        );
        let source = typst(
            &slides(&root, &day, &Records::read(&root, &day), &[], None),
            "pbmc",
            &day,
        );
        assert!(source
            .contains("#\"Why is cluster 4 apart, rather than with 2? #set page(width: 1cm)\""));
        #[cfg(feature = "pdf")]
        {
            let pdf = std::fs::read(b.pdf.as_ref().unwrap()).unwrap();
            assert!(pdf.starts_with(b"%PDF"), "a PDF");
            assert!(said(&root, &b).starts_with("Slides: results/"));
        }
        assert!(build(&root, Some("2026-01-01")).is_err());
        // A tool's output there is not replaced.
        let mut index = std::fs::read_to_string(root.join("results/INDEX.tsv")).unwrap();
        index.push_str(&format!(
            "{day}\t11:00:00\tsenna\tview_export\tresults/{day}/report/graph.svg\ta map\t\t[]\n"
        ));
        std::fs::write(root.join("results/INDEX.tsv"), index).unwrap();
        let Err(e) = build(&root, Some(&day)) else {
            panic!("a tool's output replaced");
        };
        assert!(
            e.to_string().contains("is an output of senna view_export"),
            "{e}"
        );
    }
}
