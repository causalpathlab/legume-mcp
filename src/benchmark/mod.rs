//! `legume benchmark`: a sweep of a grid of parameters times seeds times
//! stages and their methods, planned from a spec. `legume benchmark plan
//! SPEC` expands the spec to its cells, resolves and snapshots its tools,
//! and writes the benchmark's configuration to `results/benchmarks/<name>/`
//! in the project, apart from any output, so it can be run again:
//!
//! ```text
//! specs/<key>.json   the spec as canonical JSON, named by its content key
//! plan.json          every cell: argv, env, threads, GPUs, timeout, seeds, key
//! machine.json       the machine it was first planned on
//! versions.tsv       each tool's source, content key, snapshot and --version
//! tools/<key>/       the snapshots that run
//! ```
//!
//! Nothing runs yet.

pub mod key;
pub mod machine;
pub mod plan;
pub mod spec;
pub mod tools;

use crate::command::{refuse, usage};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `legume benchmark`'s usage.
pub const USAGE: &str = "\
legume benchmark plan SPEC [--retool]

A benchmark's plan: the spec SPEC (JSON) expanded to its cells, a grid
of parameters times seeds times stages and their methods, each cell with
its argv, seeds and run key. Every tool is resolved through PATH (a
path with a / from the project) and copied, by its content, into the
benchmark's folder, from where it runs.
It writes results/benchmarks/<name>/ in the project:

  specs/<key>.json   the spec as canonical JSON, named by its key
  plan.json          every cell: argv, env, threads, GPUs, timeout, seeds, key
  machine.json       the machine it was first planned on
  versions.tsv       each tool's source, content key, snapshot and --version
  tools/<key>/       the snapshots that run

and says what it planned. A later plan lists what changed: the machine,
and each tool installed since, whose snapshot it keeps. --retool plans
with the tools installed now, which rekeys their cells. Nothing runs yet.";

/// `legume benchmark` with `args` (those after `benchmark`), saying what
/// it planned on `out` and its warnings on `err`. A refusal is a
/// [`crate::command::Exit`] with its exit code.
pub fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => Ok(writeln!(out, "{USAGE}")?),
        Some("plan") => {
            let mut spec = None;
            let mut retool = false;
            for a in &args[1..] {
                match a.as_str() {
                    "-h" | "--help" => return Ok(writeln!(out, "{USAGE}")?),
                    "--retool" => retool = true,
                    s if s.starts_with('-') => return Err(usage(format!("unknown argument {s}"))),
                    s if spec.is_none() => spec = Some(PathBuf::from(s)),
                    s => return Err(usage(format!("one SPEC, not also {s}"))),
                }
            }
            let spec = spec.ok_or_else(|| usage("plan needs SPEC, the spec's JSON file"))?;
            plan_spec(&spec, retool, out, err)
        }
        Some(other) => Err(usage(format!(
            "unknown command {other}: benchmark has plan"
        ))),
    }
}

/// A spec under a benchmark's `specs/` is named by its key: refused when
/// its content has another.
fn check_name(path: &Path, spec_key: &str) -> Result<()> {
    let in_specs = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == "specs");
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let is_key = stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit());
    if in_specs && is_key && stem != spec_key {
        return Err(refuse(format!(
            "{}: its content's key is {spec_key}, not its name; a spec under specs/ is not \
             edited in place",
            path.display()
        )));
    }
    Ok(())
}

/// `legume benchmark plan`.
fn plan_spec(path: &Path, retool: bool, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (spec, value) = spec::parse(&text)?;
    let canonical = key::spec_file(&value);
    let spec_key = key::key(canonical.as_bytes());
    check_name(path, &spec_key)?;
    let here = machine::here();
    let (cores, mem_gb, mut warnings) = machine::fit(&spec, &here)?;
    plan::check(&spec)?;
    let words = plan::tool_words(&spec)?;
    let legume = std::env::current_exe().context("finding this legume")?;
    let path_env = std::env::var_os("PATH");
    let root = crate::project::root().context("finding the project's folder")?;
    let (found, missing) = tools::find(&words, path_env.as_deref(), &legume, &root);
    if !missing.is_empty() {
        let names: Vec<String> = missing
            .iter()
            .map(|(w, at)| format!("{w} ({at})"))
            .collect();
        return Err(refuse(format!("not found on PATH: {}", names.join(", "))));
    }
    let dir = root.join("results").join("benchmarks").join(&spec.name);
    // The benchmark's folder resolved, as each input is; it need not be
    // there yet.
    let real_dir = dir.canonicalize().unwrap_or_else(|_| {
        let real_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        real_root
            .join("results")
            .join("benchmarks")
            .join(&spec.name)
    });
    let mut inputs: HashMap<String, (PathBuf, String)> = HashMap::new();
    for i in &spec.inputs {
        let p = crate::absolute(&root.join(&i.path));
        let there = if i.dir { p.is_dir() } else { p.is_file() };
        if !there {
            let what = if i.dir { "folder" } else { "file" };
            return Err(refuse(format!(
                "inputs.{}.path: no {what} at {}",
                i.name,
                p.display()
            )));
        }
        // An input is the file it names, its `..` and links resolved, so an
        // argv path to it is found however it is written.
        let p = p
            .canonicalize()
            .with_context(|| format!("resolving {}", p.display()))?;
        if i.dir && (real_dir.starts_with(&p) || p.starts_with(&real_dir)) {
            return Err(refuse(format!(
                "inputs.{}.path: {} overlaps the benchmark's folder, whose files change with \
                 every plan and run; name a folder apart from it",
                i.name,
                p.display()
            )));
        }
        let k = if i.dir {
            key::dir_key(&p)?
        } else {
            key::file_key(&p)?
        };
        inputs.insert(i.name.clone(), (p, k));
    }
    // The last refusal, before any tool is snapshot: what a refusal stops
    // writes nothing but this spec, named by its content.
    let spec_file = dir.join("specs").join(format!("{spec_key}.json"));
    write_spec(&spec_file, &canonical)?;
    let (resolved, mut drift) = tools::settle(&dir, &found, retool, tools::VERSION_TIMEOUT)?;
    let ctx = plan::Context {
        dir: dir.clone(),
        tools: &resolved,
        inputs: &inputs,
    };
    let cells = plan::cells(&spec, &ctx)?;
    warnings.extend(plan::warnings(&spec, &cells, &ctx));

    let machine_file = dir.join("machine.json");
    match std::fs::read(&machine_file) {
        Ok(bytes) => match serde_json::from_slice::<machine::Machine>(&bytes) {
            Ok(old) => drift.extend(machine::drift(&old, &here)),
            Err(e) => warnings.push(format!(
                "{}: unreadable ({e}); kept",
                machine_file.display()
            )),
        },
        Err(_) => {
            crate::job::replace(&dir, "machine.json", crate::pretty(&here)?.as_bytes())?;
        }
    }
    crate::job::replace(
        &dir,
        "versions.tsv",
        tools::versions_tsv(&resolved).as_bytes(),
    )?;
    let plan_json = PlanFile {
        name: &spec.name,
        spec_key: &spec_key,
        dir: &dir,
        cores,
        mem_gb,
        gpus: &spec.gpus,
        gpu_slots: spec.gpu_slots,
        grace_secs: spec.grace_secs,
        max_hours: spec.max_hours,
        min_free_gb: spec.min_free_gb,
        tools: &resolved,
        cells: &cells,
    };
    crate::job::replace(&dir, "plan.json", crate::pretty(&plan_json)?.as_bytes())?;

    let shown = |p: &Path| {
        p.strip_prefix(&root)
            .map_or_else(|_| p.display().to_string(), |r| r.display().to_string())
    };
    let said = Summary {
        spec: &spec,
        cells: &cells,
        tools: &resolved,
        drift: &drift,
        dir: shown(&dir),
        spec_key: &spec_key,
        cores,
        mem_gb,
        mem_total_gb: here.mem_total_gb,
    };
    said.write(out)?;
    for w in &warnings {
        writeln!(err, "warning: {w}")?;
    }
    Ok(())
}

/// `plan.json`: what the benchmark books and the tools it runs, then
/// every cell.
#[derive(Serialize)]
struct PlanFile<'a> {
    name: &'a str,
    spec_key: &'a str,
    dir: &'a Path,
    cores: u32,
    mem_gb: f64,
    gpus: &'a [u32],
    gpu_slots: u32,
    grace_secs: u64,
    max_hours: Option<f64>,
    min_free_gb: f64,
    tools: &'a [tools::Resolved],
    cells: &'a [plan::Cell],
}

/// The canonical spec at `file`, made only when it is not there yet; one
/// there with other bytes is refused.
fn write_spec(file: &Path, canonical: &str) -> Result<()> {
    let folder = file.parent().context("a spec file's folder")?;
    std::fs::create_dir_all(folder).with_context(|| format!("making {}", folder.display()))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
    {
        Ok(mut f) => f
            .write_all(canonical.as_bytes())
            .with_context(|| format!("writing {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(file)? != canonical.as_bytes() {
                return Err(refuse(format!(
                    "{}: is there with other bytes than this spec's canonical JSON; a spec \
                     under specs/ is not edited in place",
                    file.display()
                )));
            }
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("writing {}", file.display())),
    }
}

/// How many of `cells` are done already: a `done.json` with their key.
fn done(cells: &[plan::Cell]) -> usize {
    cells
        .iter()
        .filter(|c| {
            std::fs::read(c.dir.join("done.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some_and(|v| v.get("key").and_then(Value::as_str) == Some(c.key.as_str()))
        })
        .count()
}

/// What the plan says on stdout.
struct Summary<'a> {
    spec: &'a spec::Spec,
    cells: &'a [plan::Cell],
    tools: &'a [tools::Resolved],
    drift: &'a [String],
    /// The benchmark's folder, from the project.
    dir: String,
    spec_key: &'a str,
    cores: u32,
    mem_gb: f64,
    mem_total_gb: f64,
}

impl Summary<'_> {
    fn write(&self, out: &mut dyn Write) -> Result<()> {
        let Self {
            spec,
            cells,
            tools,
            drift,
            ref dir,
            spec_key,
            cores,
            mem_gb,
            mem_total_gb,
        } = *self;
        let points = plan::points(spec).len();
        let reps = plan::reps(&spec.seeds).len();
        writeln!(
            out,
            "benchmark {}  spec {}  {dir}",
            spec.name,
            tools::short(spec_key)
        )?;
        let mut lanes: Vec<(String, usize)> = Vec::new();
        for c in cells {
            let name = match &c.method {
                Some(m) => format!("{}-{m}", c.stage),
                None => c.stage.clone(),
            };
            match lanes.iter_mut().find(|(n, _)| *n == name) {
                Some((_, k)) => *k += 1,
                None => lanes.push((name, 1)),
            }
        }
        let lanes: Vec<String> = lanes.iter().map(|(n, k)| format!("{n} {k}")).collect();
        writeln!(
            out,
            "cells {}: {points} points x {reps} reps; {}",
            cells.len(),
            lanes.join(", ")
        )?;
        let gpus = if spec.gpus.is_empty() {
            "no GPUs".to_string()
        } else {
            let ids: Vec<String> = spec.gpus.iter().map(u32::to_string).collect();
            format!("GPUs {} x {} slots", ids.join(","), spec.gpu_slots)
        };
        writeln!(
            out,
            "books {cores} cores, {mem_gb} GB of {mem_total_gb} GB, {gpus}"
        )?;
        writeln!(out, "tools")?;
        for t in tools {
            writeln!(
                out,
                "  {}  {}  {}  {}",
                t.word,
                t.source.display(),
                tools::short(&t.content_key),
                t.version.as_deref().unwrap_or("NA")
            )?;
        }
        writeln!(out, "done {} of {} (by key)", done(cells), cells.len())?;
        if !drift.is_empty() {
            writeln!(out, "changed since the last plan")?;
            for d in drift {
                writeln!(out, "  {d}")?;
            }
        }
        writeln!(
            out,
            "wrote specs/{spec_key}.json, plan.json, machine.json, versions.tsv, tools/"
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::code;

    // heredoc_roundtrip_same_key: the canonical spec ends in a newline, so
    // the heredoc runall.sh writes it with gives back the same bytes, and
    // the same key.
    #[test]
    fn heredoc_roundtrip_same_key() {
        let text = r#"{"stages": [{"run": ["T", "a\tb"], "name": "s", "makes": ["m.score.tsv"]}],
                       "seeds": {"n": 1}, "name": "N1"}"#;
        let (_, v) = spec::parse(text).unwrap();
        let canonical = key::spec_file(&v);
        let k = key::key(canonical.as_bytes());
        let d = tempfile::tempdir().unwrap();
        let project = crate::project::Project::new(d.path().to_path_buf(), "legume", "0");
        let file = d
            .path()
            .join(format!("results/benchmarks/N1/specs/{k}.json"));
        project.file(&file, &canonical);
        let runall = project.day_dir().unwrap().join("runall.sh");
        let ran = std::process::Command::new("bash")
            .arg(&runall)
            .current_dir(d.path())
            .output()
            .unwrap();
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        let back = std::fs::read(&file).unwrap();
        assert_eq!(back, canonical.as_bytes());
        assert_eq!(key::key(&back), k);
        let (_, again) = spec::parse(std::str::from_utf8(&back).unwrap()).unwrap();
        assert_eq!(key::spec_file(&again), canonical);
        check_name(&file, &k).unwrap();
    }

    #[test]
    fn a_spec_under_specs_is_named_by_its_key() {
        let k = key::key(b"{}\n");
        let other = "0".repeat(32);
        check_name(Path::new(&format!("B/specs/{k}.json")), &k).unwrap();
        let e = check_name(Path::new(&format!("B/specs/{other}.json")), &k).unwrap_err();
        assert_eq!(code(&e), 3);
        assert!(e
            .to_string()
            .contains(&format!("its content's key is {k}, not its name")));
        // Anywhere else, or by another name, a spec is the person's own.
        check_name(Path::new(&format!("B/{other}.json")), &k).unwrap();
        check_name(Path::new("B/specs/S1.json"), &k).unwrap();
    }
}
