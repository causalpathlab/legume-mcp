//! `legume report`: a day's work as slides, built from the records alone,
//! and `legume graph`: its provenance graph. The slides are Typst source
//! (`results/<day>/report/report.typ`, beside `graph.svg` and the story),
//! made into a PDF when `typst` is on the PATH; nothing is installed. The
//! report's folder is its own: no tool's output is replaced, and a story
//! replaced is kept (`story.1.md`, ...).
//!
//! The slides, in order: the title; the story, the one part an agent
//! writes (labelled as its own); the question (the day's plans); the data
//! (`data/README.md`); the steps (the graph, then each output with what it
//! was made from and its numbers, the flagged ones marked); the figures,
//! each with the line of `runall.sh` that made it; the observations; the
//! interpretation, the person's (or "not yet interpreted"); each
//! hypothesis's triangulation table, the verb it allows and the sources not
//! yet used; and how to replay the work.
//!
//! Every word from the records goes into the source as a Typst string, so
//! no record, however written, is read as markup or code.

use crate::evidence;
use crate::graph::Graph;
use crate::project::read_record;
use crate::records::{Entry, Records, Step};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The room for the graph on its slide, in cm: width, height.
const GRAPH_ROOM: (f64, f64) = (26.5, 10.5);

/// `legume report`'s usage.
pub const REPORT_USAGE: &str = "\
legume report [--day YYYY-MM-DD] [--story FILE] [--project DIR] [--no-pdf]

The day's work as slides, from the records, in results/<day>/report/:
report.typ (Typst) and graph.svg, and report.pdf when typst is on the PATH.
The story, the one part an agent or a person writes, comes from --story or
from the folder's story.md (the `legume` server's `report` tool writes it;
a story replaced is kept as story.1.md, ...); it is labelled as theirs.
Everything else is read from the records.";

/// `legume graph`'s usage.
pub const GRAPH_USAGE: &str = "\
legume graph [--day YYYY-MM-DD | --all] [--format svg|dot|text] [--project DIR]

The provenance graph from results/INDEX.tsv: which files each step read and
made. The day's steps (today's unless --day; every day's with --all), on
stdout: SVG (the default), Graphviz DOT, or text.";

/// What a report made.
pub struct Built {
    pub typ: PathBuf,
    pub graph: Option<PathBuf>,
    /// The PDF, or why there is none.
    pub pdf: std::result::Result<PathBuf, String>,
    /// Whether the slides have a story.
    pub story: bool,
}

/// The report of `day` (today when none) for the project at `root`, with
/// `story` (else the report's `story.md`, kept from before); `pdf`: make the
/// PDF when `typst` is on the PATH.
pub fn build(root: &Path, day: Option<&str>, story: Option<&str>, pdf: bool) -> Result<Built> {
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
        write_fresh(path, bytes)
    };
    let story_file = out.join("story.md");
    if let Some(s) = story {
        let before = read_record(&story_file);
        if !before.trim().is_empty() && before.trim_end() != s.trim_end() {
            keep_aside(&story_file, before.as_bytes())?;
        }
        fresh(&story_file, s.trim_end().as_bytes())?;
    }
    let story = read_record(&story_file);
    let steps: Vec<&Step> = records.steps.iter().filter(|s| s.date == day).collect();
    // The graph, and how it fits a slide: as wide as the slide when it is
    // wider than the room for it, else as tall.
    let graph = if steps.is_empty() {
        None
    } else {
        let path = out.join("graph.svg");
        let (svg, w, h) = Graph::of(&steps).drawn();
        fresh(&path, svg.as_bytes())?;
        Some((path, w / h > GRAPH_ROOM.0 / GRAPH_ROOM.1))
    };
    let typ = out.join("report.typ");
    let source = slides(
        root,
        &day,
        &records,
        &steps,
        story.trim(),
        graph.as_ref().map(|g| g.1),
    );
    fresh(&typ, source.as_bytes())?;
    let pdf = if pdf {
        unlisted(&typ.with_extension("pdf"))?;
        compile(root, &typ)
    } else {
        Err("not asked for (--no-pdf)".into())
    };
    Ok(Built {
        typ,
        graph: graph.map(|g| g.0),
        pdf,
        story: !story.trim().is_empty(),
    })
}

/// The provenance graph of `day` (today when none; every day with `all`)
/// as `format`: `svg`, `dot` or `text`.
pub fn graph(root: &Path, day: Option<&str>, all: bool, format: &str) -> Result<String> {
    let day = day_of(day)?;
    let records = Records::read(root, &day);
    let steps: Vec<&Step> = records
        .steps
        .iter()
        .filter(|s| all || s.date == day)
        .collect();
    let g = Graph::of(&steps);
    Ok(match format {
        "svg" => g.svg(),
        "dot" => g.dot(),
        "text" if steps.is_empty() => format!("No steps recorded for {day}.\n"),
        "text" => g.text(),
        other => anyhow::bail!("--format is svg, dot or text, not {other}"),
    })
}

/// `legume report ...` and `legume graph ...` on the command line.
pub fn main(command: &str, args: &[String], out: &mut dyn std::io::Write) -> Result<()> {
    let mut day = None;
    let mut story = None;
    let mut project = None;
    let mut format = "svg".to_string();
    let mut all = false;
    let mut pdf = true;
    let usage = if command == "graph" {
        GRAPH_USAGE
    } else {
        REPORT_USAGE
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| {
            it.next()
                .cloned()
                .with_context(|| format!("{a} needs {what}"))
        };
        match (command, a.as_str()) {
            (_, "--day") => day = Some(value("a day, YYYY-MM-DD")?),
            (_, "--project") => project = Some(PathBuf::from(value("a folder")?)),
            ("report", "--story") => {
                let path = value("a file")?;
                story = Some(
                    std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?,
                )
            }
            ("report", "--no-pdf") => pdf = false,
            ("graph", "--format") => format = value("svg, dot or text")?,
            ("graph", "--all") => all = true,
            (_, "-h" | "--help") => {
                writeln!(out, "{usage}")?;
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument {a}\n\n{usage}"),
        }
    }
    let root = match project {
        Some(p) => crate::absolute(&p),
        None => crate::project::root_from_env()
            .map_or_else(std::env::current_dir, Ok)
            .context("the project folder")?,
    };
    if command == "graph" {
        write!(out, "{}", graph(&root, day.as_deref(), all, &format)?)?;
        return Ok(());
    }
    let built = build(&root, day.as_deref(), story.as_deref(), pdf)?;
    writeln!(out, "{}", said(&root, &built))?;
    Ok(())
}

/// What was made, in words, paths from the project.
pub(crate) fn said(root: &Path, b: &Built) -> String {
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let mut out = format!("Slides: {}", rel(&b.typ));
    if let Some(g) = &b.graph {
        out += &format!("\nProvenance graph: {}", rel(g));
    }
    match &b.pdf {
        Ok(p) => out += &format!("\nPDF: {}", rel(p)),
        Err(why) => {
            out += &format!(
                "\nNo PDF ({why}). With Typst installed: typst compile --root . {}",
                rel(&b.typ)
            )
        }
    }
    if !b.story {
        out += "\nNo story yet: the slides open with the question. A story (a few \
                paragraphs on what was asked, done and found, as hypotheses) goes in \
                with `report`'s `story`, or `legume report --story FILE`.";
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

/// `bytes` as the file at `path`, in place of what was there: never
/// written through a symlink, which is removed as a link.
fn write_fresh(path: &Path, bytes: &[u8]) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("replacing {}", path.display())),
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o644);
    o.open(path)
        .and_then(|mut f| f.write_all(bytes))
        .with_context(|| format!("writing {}", path.display()))
}

/// An earlier story, kept beside the new one as `story.1.md`, `story.2.md`,
/// ...: a story replaced is never lost.
fn keep_aside(story: &Path, bytes: &[u8]) -> Result<()> {
    for n in 1..=999 {
        let kept = story.with_file_name(format!("story.{n}.md"));
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o644);
        match o.open(&kept) {
            Ok(mut f) => {
                return f
                    .write_all(bytes)
                    .with_context(|| format!("writing {}", kept.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("writing {}", kept.display())),
        }
    }
    anyhow::bail!("{} has 999 earlier stories: none kept", story.display())
}

/// The PDF of `typ`, by `typst` on the PATH, its files read from `root`
/// alone; or why there is none.
fn compile(root: &Path, typ: &Path) -> std::result::Result<PathBuf, String> {
    let Some(typst) = crate::on_path("typst") else {
        return Err("typst is not on the PATH".into());
    };
    let pdf = typ.with_extension("pdf");
    let out = std::process::Command::new(typst)
        .arg("compile")
        .arg("--root")
        .arg(root)
        .arg(typ)
        .arg(&pdf)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("typst did not run: {e}"))?;
    if out.status.success() {
        Ok(pdf)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "typst failed: {}",
            crate::project::one_line(err.trim())
        ))
    }
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

/// `text` as markup: each line a string of its own, a blank line between
/// paragraphs, Markdown's bold marks dropped.
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

/// `text` cut to `n` characters, saying where the rest is.
fn cut(text: &str, n: usize, rest: &str) -> String {
    if text.chars().count() <= n {
        return text.into();
    }
    let kept: String = text.chars().take(n).collect();
    format!("{kept}… (the rest in {rest})")
}

/// The page set up, and the helpers the slides use.
const PREAMBLE: &str = r##"#set page(paper: "presentation-16-9", margin: (x: 1.6cm, top: 1.2cm, bottom: 1.5cm), footer: context {
  set text(size: 9pt, fill: luma(120))
  [#footer-text #h(1fr) #counter(page).display()]
})
#set text(size: 14pt)
#set par(justify: false)
#show heading.where(level: 1): it => {
  pagebreak(weak: true)
  set text(size: 22pt)
  block(below: 0.9em, it.body)
}
#let dim(body) = text(fill: luma(105), body)
#let warn(body) = text(fill: rgb("#b03a2e"), weight: "bold", body)
#let quoted(body) = block(inset: (left: 0.9em, y: 0.3em), stroke: (left: 2pt + luma(190)), body)
#let small(body) = { set text(size: 11pt); body }
"##;

/// The Typst source of the slides.
fn slides(
    root: &Path,
    day: &str,
    records: &Records,
    steps: &[&Step],
    story: &str,
    graph: Option<bool>,
) -> String {
    let name = root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    let dir = root.join("results").join(day);
    let notebook = format!("results/{day}/notebook.md");
    let entries = |keep: &dyn Fn(&str) -> bool| -> Vec<&Entry> {
        records.notebook.iter().filter(|e| keep(&e.kind)).collect()
    };
    let mut s = String::new();
    let _ = writeln!(
        s,
        "// legume report {}, built from the records of {name} for {day}.\n\
         // The story is the agent's (or the person's); every other word is read from the records.\n\
         // Rebuild: legume report --day {day}. Compile from the project folder:\n\
         //   typst compile --root . results/{day}/report/report.typ",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(s, "#let footer-text = {}", lit(&format!("{name} · {day}")));
    let _ = writeln!(
        s,
        "#set document(title: {})",
        lit(&format!("{name}, {day}"))
    );
    s.push_str(PREAMBLE);

    // Title.
    let _ = writeln!(
        s,
        "\n#align(horizon)[\n  #text(size: 32pt, weight: \"bold\")[#{}]\n\n  #text(size: 18pt)[#{}]\n\n  \
         #dim[Built by legume report {} from the project's records.] \\\n  \
         #dim[Exploratory work: what it found are hypotheses, not findings.]\n]",
        lit(&name),
        lit(day),
        env!("CARGO_PKG_VERSION")
    );

    // The story.
    if !story.is_empty() {
        let _ = writeln!(
            s,
            "\n= Story\n#dim[Written by the agent (or the person), from the records; every slide after this one is read from the records themselves.]\n\n{}",
            para(&cut(story, 2500, &format!("results/{day}/report/story.md")))
        );
    }

    // The question.
    s.push_str("\n= The question\n");
    let plans = entries(&|k| k.starts_with("plan"));
    if plans.is_empty() {
        s.push_str("#warn[No plan was recorded this day.] #dim[A plan says the question, the data, the sources of evidence and how the result will be checked.]\n");
    }
    for e in plans {
        let _ = writeln!(
            s,
            "#dim[#{}]\n#quoted[\n{}]\n",
            lit(&format!("{} · {} · {}", e.time, e.server, e.kind)),
            para(&cut(&e.text, 1200, &notebook))
        );
    }

    // The data.
    s.push_str("\n= The data\n");
    let data = read_record(&root.join("data/README.md"));
    let data: Vec<&str> = data.lines().filter(|l| l.starts_with("- ")).collect();
    if data.is_empty() {
        s.push_str("#dim[No data recorded in data/README.md.]\n");
    }
    for l in data.iter().rev().take(14).rev() {
        let _ = writeln!(s, "- #{}", lit(&l[2..]));
    }

    // The steps.
    s.push_str("\n= The steps\n");
    if steps.is_empty() {
        s.push_str("#dim[No step recorded an output this day (results/INDEX.tsv).]\n");
    }
    if let Some(wide) = graph {
        let size = if wide {
            "width: 100%".to_string()
        } else {
            format!("height: {}cm", GRAPH_ROOM.1)
        };
        let _ = writeln!(
            s,
            "#v(1fr)\n#align(center, image({}, {size}))\n#v(1fr)\n\
             #dim[Each step (rounded) between the files it read and made; grey files are data no step here made; a red step flagged a number.]",
            lit(&format!("/results/{day}/report/graph.svg"))
        );
        s.push_str("\n= The steps, with their numbers\n");
    }
    if !steps.is_empty() {
        s.push_str(
            "#small[#table(columns: (auto, auto, 1.4fr, 1fr), stroke: 0.5pt + luma(200), inset: 5pt,\n  \
             table.header([*Time*], [*Step*], [*Output, from*], [*Numbers*]),\n",
        );
        for st in steps {
            let mut out = format!("#{}", lit(&st.output));
            if !st.inputs.is_empty() {
                out += &format!(
                    " \\ #dim[#{}]",
                    lit(&format!("from {}", st.inputs.join(", ")))
                );
            }
            let numbers: Vec<String> = st
                .facts
                .iter()
                .map(|f| match f.flagged() {
                    Some(_) => format!("#warn[#{}]", lit(&format!("flagged: {f}"))),
                    None => format!("#{}", lit(&f.to_string())),
                })
                .collect();
            let _ = writeln!(
                s,
                "  [#{}], [#{}], [{out}], [{}],",
                lit(&st.time.chars().take(5).collect::<String>()),
                lit(&format!("{} {}", st.server, st.tool)),
                numbers.join(" \\ ")
            );
        }
        s.push_str(")]\n");
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
            s.push_str("\n#warn[Flagged by the tools: look at these before building on them.]\n");
            for f in flagged {
                let _ = writeln!(s, "- #{}", lit(&f));
            }
        }
    }

    // The figures.
    let runall = read_record(&dir.join("runall.sh"));
    for st in steps.iter().filter(|st| is_figure(root, &st.output)) {
        let _ = writeln!(
            s,
            "\n= {}\n#align(center, image({}, width: 100%, height: 10cm, fit: \"contain\"))\n#dim[#{}] \\\n#small[#dim[#{}]]",
            format_args!("#{}", lit(&format!("Figure: {}", file_name(&st.output)))),
            lit(&format!("/{}", st.output)),
            lit(&st.summary),
            lit(&made_by(&runall, st, day))
        );
    }

    // The observations.
    let seen = entries(&|k| k.starts_with("observation") || k.starts_with("note"));
    if !seen.is_empty() {
        s.push_str("\n= What was seen\n");
        for e in seen {
            let _ = writeln!(
                s,
                "#dim[#{}]\n#quoted[\n{}]\n",
                lit(&format!("{} · {} · {}", e.time, e.server, e.kind)),
                para(&cut(&e.text, 900, &notebook))
            );
        }
    }

    // The interpretation.
    s.push_str("\n= The interpretation\n");
    let decided = entries(&|k| {
        k.starts_with("interpretation (accepted") || k.starts_with("interpretation (user")
    });
    if decided.is_empty() {
        s.push_str("#warn[Not yet interpreted.] #dim[No reading has been accepted or set aside by the person; the readings proposed are below.]\n\n");
        for e in entries(&|k| k.starts_with("interpretation")) {
            let _ = writeln!(
                s,
                "#dim[#{}]\n#quoted[\n{}]\n",
                lit(&format!("{} · {} · {}", e.time, e.server, e.kind)),
                para(&cut(&e.text, 900, &notebook))
            );
        }
    }
    for e in &decided {
        let _ = writeln!(
            s,
            "#dim[#{}]\n#quoted[\n{}]\n",
            lit(&format!("{} · {} · {}", e.time, e.server, e.kind)),
            para(&cut(&e.text, 1200, &notebook))
        );
    }
    if let Some(q) = &records.waiting {
        let _ = writeln!(s, "#warn[Waiting for the person:] #{}", lit(q));
    }
    let chosen = entries(&|k| k.starts_with("choice"));
    if !chosen.is_empty() {
        s.push_str("\n*Next steps the person chose*\n\n");
        for e in chosen {
            let _ = writeln!(s, "- #{}", lit(&cut(&e.first, 300, &notebook)));
        }
    }

    // The hypotheses.
    for h in &records.hypotheses {
        let l = &h.ledger;
        let _ = writeln!(s, "\n= #{}", lit(&format!("H{}. {}", h.id, h.claim)));
        if l.sources.is_empty() {
            s.push_str("#dim[No source has been weighed for it yet.]\n");
        } else {
            s.push_str(
                "#small[#table(columns: (auto, auto, auto, auto, 1fr), stroke: 0.5pt + luma(200), inset: 5pt,\n  \
                 table.header([*Source*], [*Kind*], [*Shares with our cells*], [*Says*], [*Evidence*]),\n",
            );
            for e in &l.sources {
                let (kind, shares) = evidence::what(&e.class);
                let says = match e.verdict.as_str() {
                    "agrees" => format!("#{}", lit("agrees")),
                    v => format!("#warn[#{}]", lit(v)),
                };
                let _ = writeln!(
                    s,
                    "  [#{}], [#{}], [#{}], [{says}], [#{}],",
                    lit(&e.source),
                    lit(kind),
                    lit(shares),
                    lit(&e.summary)
                );
            }
            s.push_str(")]\n");
        }
        let _ = writeln!(
            s,
            "\n*Verb:* #{} \\\n*Standing:* #{} #dim[#{}]",
            lit(&l.verb),
            lit(l.standing),
            lit(&format!("(level {} of 3)", l.level))
        );
        if !l.unused.is_empty() {
            let names: Vec<String> = l
                .unused
                .iter()
                .map(|c| format!("{} ({c})", evidence::what(c).0))
                .collect();
            let _ = writeln!(
                s,
                "\n#dim[Sources not yet used, those that would raise the verb first:] #{}",
                lit(&names.join("; "))
            );
        }
    }

    // Replay.
    s.push_str("\n= Replay\nEvery step of the day, in order, without the agent or the model:\n");
    let _ = writeln!(
        s,
        "#raw({}, block: true, lang: \"bash\")",
        lit(&format!("bash results/{day}/runall.sh"))
    );
    let mut tools: Vec<String> = Vec::new();
    for l in runall.lines().filter_map(|l| l.strip_prefix("# ")) {
        let w: Vec<&str> = l.split_whitespace().collect();
        // `# 10:02:03 server version tool {args}`
        if w.len() >= 4 && w[0].len() == 8 && w[0].as_bytes()[2] == b':' {
            let t = format!("{} {}", w[1], w[2]);
            if !tools.contains(&t) {
                tools.push(t);
            }
        }
    }
    if !tools.is_empty() {
        let _ = writeln!(s, "\n*Tools:* #{}", lit(&tools.join(", ")));
    }
    let chat = read_record(&dir.join("chat.md"));
    let mut models: Vec<&str> = chat
        .lines()
        .filter(|l| l.starts_with("- Model:") || l.starts_with("- Model file:"))
        .collect();
    models.dedup();
    if !models.is_empty() {
        s.push_str("\n*The model in `legume chat`* #dim[(not needed to replay)]\n");
        for m in models {
            let _ = writeln!(s, "- #{}", lit(&m[2..]));
        }
    }
    let _ = writeln!(
        s,
        "\n#dim[The records: #{}]",
        lit(&format!(
            "results/{day}/notebook.md (the reasoning), results/{day}/runall.sh (the steps), \
             results/INDEX.tsv (each output, its inputs and numbers), results/HYPOTHESES.tsv \
             and EVIDENCE.tsv (the triangulation){}",
            if chat.is_empty() {
                String::new()
            } else {
                format!(", results/{day}/chat.md (the chat)")
            }
        ))
    );
    s
}

/// Whether `output` is a figure the slides can show: an image inside the
/// project that is there.
fn is_figure(root: &Path, output: &str) -> bool {
    let image = ["png", "jpg", "jpeg", "gif", "svg"].iter().any(|e| {
        output
            .rsplit_once('.')
            .is_some_and(|(_, x)| x.eq_ignore_ascii_case(e))
    });
    image
        && !output.starts_with('/')
        && !output.split('/').any(|p| p == "..")
        && root.join(output).is_file()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Fact;

    #[test]
    fn record_text_is_a_string_never_markup_or_code() {
        assert_eq!(lit(r#"a "b" \c"#), r#""a \"b\" \\c""#);
        assert_eq!(lit("x\u{1b}[2Jy"), "\"x[2Jy\"");
        assert_eq!(
            para("#read(\"/etc/passwd\")\n**R1. claim**\n\nnext"),
            "#\"#read(\\\"/etc/passwd\\\")\" \\\n#\"R1. claim\"\n\n#\"next\"\n"
        );
        assert!(day_of(Some("../../x")).is_err());
        assert!(day_of(Some("2026-10-28")).is_ok());
    }

    #[test]
    fn the_slides_come_from_the_records() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pbmc");
        let day = "2026-10-28";
        let d = root.join("results").join(day);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("data/README.md"),
            "# Data\n\n- data/pbmc.h5: 10x PBMC, from 10x Genomics\n",
        )
        .unwrap();
        std::fs::write(d.join("umap.png"), b"\x89PNG").unwrap();
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
        std::fs::write(
            d.join("notebook.md"),
            "# Notebook\n\n## 10:01:00 · legume · plan\n\nWhy is cluster 4 apart, rather than with 2?\n\n\
             ## 10:06:00 · legume · interpretation (proposed)\n\n**R1. stress** #emph[x]\n\n**R2. a donor**\n",
        )
        .unwrap();
        let b = build(
            &root,
            Some(day),
            Some("We asked why *cluster 4* stands apart."),
            false,
        )
        .unwrap();
        let typ = std::fs::read_to_string(&b.typ).unwrap();
        assert!(typ.contains("= Story\n"), "{typ}");
        assert!(
            typ.contains("#\"We asked why *cluster 4* stands apart.\""),
            "{typ}"
        );
        assert!(
            typ.contains("#\"Why is cluster 4 apart, rather than with 2?\""),
            "{typ}"
        );
        assert!(
            typ.contains("#\"data/pbmc.h5: 10x PBMC, from 10x Genomics\""),
            "{typ}"
        );
        assert!(
            typ.contains("image(\"/results/2026-10-28/report/graph.svg\""),
            "{typ}"
        );
        assert!(
            typ.contains("#warn[#\"flagged: mito 0.31 (flagged: above 0.2)\"]"),
            "{typ}"
        );
        assert!(
            !typ.contains("old.json"),
            "another day's steps stay out: {typ}"
        );
        // The figure, with the line that made it.
        assert!(
            typ.contains("image(\"/results/2026-10-28/umap.png\""),
            "{typ}"
        );
        assert!(
            typ.contains("results/2026-10-28/runall.sh, line 4: senna view export --out results/2026-10-28/umap.png"),
            "{typ}"
        );
        // No reading accepted: said, and the proposal shown as text.
        assert!(typ.contains("#warn[Not yet interpreted.]"), "{typ}");
        assert!(typ.contains("#\"R1. stress #emph[x]\""), "{typ}");
        assert!(typ.contains("*Tools:* #\"senna 0.9.0\""), "{typ}");
        assert!(b.graph.is_some() && b.story);
        assert!(std::fs::read_to_string(d.join("report/story.md"))
            .unwrap()
            .starts_with("We asked"));
        // Built again without a story, the day's is kept.
        let again = build(&root, Some(day), None, false).unwrap();
        assert!(again.story);
        let words = said(&root, &again);
        assert!(
            words.contains("Slides: results/2026-10-28/report/report.typ"),
            "{words}"
        );
        assert!(words.contains("No PDF (not asked for"), "{words}");
        // The graph on its own.
        let text = graph(&root, Some(day), false, "text").unwrap();
        assert!(
            text.contains("view_export (senna · 2026-10-28 10:05)"),
            "{text}"
        );
        assert!(graph(&root, Some(day), true, "text")
            .unwrap()
            .contains("old.json"));
        assert!(build(&root, Some("2026-01-01"), None, false).is_err());
        // A new story keeps the one before.
        build(&root, Some(day), Some("Another story."), false).unwrap();
        assert!(std::fs::read_to_string(d.join("report/story.1.md"))
            .unwrap()
            .starts_with("We asked"));
        // A tool's output there is not replaced.
        let mut index = std::fs::read_to_string(root.join("results/INDEX.tsv")).unwrap();
        index.push_str(&format!(
            "{day}\t11:00:00\tsenna\tview_export\tresults/{day}/report/graph.svg\ta map\t\t[]\n"
        ));
        std::fs::write(root.join("results/INDEX.tsv"), index).unwrap();
        let Err(e) = build(&root, Some(day), None, false) else {
            panic!("a tool's output replaced");
        };
        assert!(
            e.to_string().contains("is an output of senna view_export"),
            "{e}"
        );
    }
}
