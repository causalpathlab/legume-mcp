//! A spec expanded to its tasks: the grid (plain axes times tuple axes,
//! less `exclude`), times the replicates, times the stages and their
//! methods. Each task gets its id, its seeds, its argv with the
//! placeholders filled, and its task key.
//!
//! - **Task id.** `<8 hex of the canonical point>.r<rep>.<stage>[-<method>]`.
//!   The point is its axes' labels, so an id stays when values are added.
//! - **Seeds.** `rep_seed = splitmix64(base ^ splitmix64(rep)) & 0x7fffffff`
//!   (or the listed seed), shared by every point of a replicate, and
//!   `seed = splitmix64(rep_seed ^ fnv64("<stage>/<method>")) & 0x7fffffff`,
//!   the method empty in a stage without methods, so no two stages share
//!   a stream.
//! - **Methods.** In a later stage, a method sees `{<stage>}` as the
//!   folder of that stage's method of the same name. A method the
//!   upstream stage does not have may not use its placeholder, nor may a
//!   stage without methods use one with.
//! - **Key.** FNV-1a 128 over the canonical JSON of the stage, the method,
//!   the argv template, the thread variables as templates, the tool's
//!   content key (and legume's, when another tool's argv embeds
//!   `{legume}`), the declared inputs it reads (by content, named by
//!   `{inputs.NAME}` or by a path under one) and the keys of the tasks it
//!   reads. The template keeps every path placeholder
//!   (`{out}`, `{<stage>}`, `{glob:...}`, `{inputs.NAME}`, `{legume}`) and
//!   `{threads}` and `{gpu}` as written: what they name is keyed by
//!   content, so a key survives moving the project.
//! - **Braces.** `{{` and `}}` write one brace (an awk program, a JSON
//!   value); the key's template keeps them doubled, apart from a
//!   placeholder.
//! - **Globs.** `{glob:<stage>/<pattern>}` names files in the upstream
//!   task's folder; the plan's argv holds it with the folder made
//!   absolute. [`glob`] is what expands it when the task starts, once
//!   tasks run; nothing calls it yet.

use super::key;
use super::spec::{
    bad, is_plain_relative, AxisValue, Method, Seeds, Spec, Stage, RESERVED, THREAD_VARS,
};
use super::tools::Resolved;
use crate::command::refuse;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The tool word of `{legume}`, this program.
pub const LEGUME: &str = "{legume}";

/// What expanding a spec needs from outside it.
pub struct Context<'a> {
    /// The benchmark's folder, absolute.
    pub dir: PathBuf,
    /// Each tool word's (and [`LEGUME`]'s) tool.
    pub tools: &'a [Resolved],
    /// Each declared input: its path, resolved, and content key.
    pub inputs: &'a HashMap<String, (PathBuf, String)>,
}

impl Context<'_> {
    /// The tool of `word`.
    fn tool(&self, word: &str) -> Option<&Resolved> {
        self.tools.iter().find(|t| t.word == word)
    }
}

/// One task, as `plan.json` lists it, and as the wrapper reads it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub point: BTreeMap<String, String>,
    pub rep: u32,
    pub rep_seed: u64,
    pub seed: u64,
    pub stage: String,
    pub method: Option<String>,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub threads: u32,
    pub mem_gb: f64,
    pub gpus: u32,
    pub timeout_secs: u64,
    pub max_running: Option<u32>,
    pub dir: PathBuf,
    pub makes: Vec<String>,
    pub upstream: Vec<String>,
    pub key: String,
}

/// A run of an argv word: text, or a placeholder's name.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg<'a> {
    Text(&'a str),
    Name(&'a str),
}

/// `token` split at its placeholders, `{{` and `}}` each one brace of
/// text; why not, when a brace is left open or a placeholder is empty.
fn segments(token: &str) -> std::result::Result<Vec<Seg<'_>>, String> {
    let mut out = Vec::new();
    let mut rest = token;
    while let Some(at) = rest.find(['{', '}']) {
        if at > 0 {
            out.push(Seg::Text(&rest[..at]));
        }
        let (brace, after) = (&rest[at..=at], &rest[at + 1..]);
        if let Some(after) = after.strip_prefix(brace) {
            out.push(Seg::Text(brace));
            rest = after;
            continue;
        }
        if brace == "}" {
            return Err(format!("{token:?} has a }} with no {{ (}}}} writes a }})"));
        }
        let Some(close) = after.find('}') else {
            return Err(format!("{token:?} leaves a {{ open ({{{{ writes a {{)"));
        };
        let name = &after[..close];
        if name.is_empty() || name.contains('{') {
            return Err(format!("{token:?} has an empty or nested placeholder"));
        }
        out.push(Seg::Name(name));
        rest = &after[close + 1..];
    }
    if !rest.is_empty() {
        out.push(Seg::Text(rest));
    }
    Ok(out)
}

/// Another tool's argv names `{legume}`: `run[1..]`, since `run[0]` is
/// replaced by the tool's own path.
fn embeds_legume(method: &Method) -> bool {
    method.tool_word() != LEGUME
        && method.run[1..].iter().any(|t| {
            segments(t)
                .unwrap_or_default()
                .contains(&Seg::Name("legume"))
        })
}

/// What a placeholder names.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ph<'a> {
    /// An axis, a binding, `rep`, `rep_seed` or `seed`: a value of the task.
    Value(&'a str),
    Method,
    Threads,
    Gpu,
    Out,
    Legume,
    Stage(&'a str),
    Input(&'a str),
    Glob(&'a str, &'a str),
}

/// What `name` names in `spec`, if anything.
fn classify<'a>(name: &'a str, spec: &Spec) -> Option<Ph<'a>> {
    if let Some(rest) = name.strip_prefix("glob:") {
        let (stage, pattern) = rest.split_once('/').unwrap_or((rest, ""));
        return Some(Ph::Glob(stage, pattern));
    }
    if let Some(input) = name.strip_prefix("inputs.") {
        return Some(Ph::Input(input));
    }
    match name {
        "rep" | "rep_seed" | "seed" => return Some(Ph::Value(name)),
        "method" => return Some(Ph::Method),
        "threads" => return Some(Ph::Threads),
        "gpu" => return Some(Ph::Gpu),
        "out" => return Some(Ph::Out),
        "legume" => return Some(Ph::Legume),
        _ => {}
    }
    let bound = spec.axes.iter().any(|a| {
        a.name == name
            || a.values
                .first()
                .is_some_and(|v| v.bindings.iter().any(|(b, _)| b == name))
    });
    if bound {
        return Some(Ph::Value(name));
    }
    spec.stages
        .iter()
        .any(|s| s.name == name)
        .then_some(Ph::Stage(name))
}

/// Each word of `method`'s argv, with where it is, as its parts; refused
/// when one does not parse or a placeholder names nothing.
fn words<'a>(spec: &Spec, method: &'a Method) -> Result<Vec<(String, Vec<Part<'a>>)>> {
    let mut out = Vec::new();
    for (i, token) in method.run.iter().enumerate() {
        let at = format!("{}.run[{i}]", method.at);
        let mut parts = Vec::new();
        for seg in segments(token).map_err(|e| bad(&at, e))? {
            let name = match seg {
                Seg::Text(t) => {
                    parts.push(Part::Text(t));
                    continue;
                }
                Seg::Name(name) => name,
            };
            let ph = classify(name, spec).ok_or_else(|| {
                let own: Vec<&str> = RESERVED
                    .into_iter()
                    .filter(|r| !matches!(*r, "inputs" | "glob"))
                    .collect();
                bad(
                    &at,
                    format!(
                        "{{{name}}} is no placeholder: not an axis, a binding, a stage, \
                         inputs.NAME, glob:STAGE/PATTERN, or one of {}",
                        own.join(", ")
                    ),
                )
            })?;
            parts.push(Part::Ph(name, ph));
        }
        out.push((at, parts));
    }
    Ok(out)
}

// The tools whose quirks the plan knows, by their argv: what `check`
// refuses and `warnings` warns of for them.

/// A pinto `lra-*` run, which sizes itself to the memory it sees free.
fn is_pinto_lra(method: &Method) -> bool {
    base(method.tool_word()).starts_with("pinto")
        && method.run.get(1).is_some_and(|s| s.starts_with("lra-"))
}

/// A data-beans run, whose per-cell streams are `rseed + offset + j`.
fn is_data_beans(method: &Method) -> bool {
    base(method.tool_word()).starts_with("data-beans")
}

/// A data-beans multiome run with `--reference-atac`, whose pair truth no
/// universe holds.
fn is_reference_multiome(method: &Method) -> bool {
    is_data_beans(method)
        && method.run.iter().any(|t| t == "multiome")
        && method
            .run
            .iter()
            .any(|t| t == "--reference-atac" || t.starts_with("--reference-atac="))
}

/// The basename of a tool word.
pub(super) fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The spec's rules between its parts: every placeholder names something,
/// each in a place it may be used (the method rules), the last stage of each
/// method makes one score, pinto `lra-*` books its memory, and no score reads
/// the pair truth of a reference-ATAC multiome.
pub fn check(spec: &Spec) -> Result<()> {
    let methods = spec.stages.iter().enumerate();
    let methods = methods.flat_map(|(si, s)| s.methods.iter().map(move |m| (si, m)));
    for (si, method) in methods {
        let words = words(spec, method)?;
        let placeholders = words.iter().flat_map(|(at, parts)| {
            parts.iter().filter_map(move |part| match *part {
                Part::Ph(name, ph) => Some((at, name, ph)),
                Part::Text(_) => None,
            })
        });
        for (at, name, ph) in placeholders {
            match ph {
                Ph::Method if method.name.is_none() => {
                    return Err(bad(at, "{method} is for a stage with methods"))
                }
                Ph::Gpu if method.gpus == 0 => {
                    return Err(bad(at, "{gpu} names a GPU, and this task asks for none"))
                }
                Ph::Input(n) if !spec.inputs.iter().any(|i| i.name == n) => {
                    return Err(bad(at, format!("{{{name}}}: {n:?} is no declared input")))
                }
                Ph::Stage(t) => upstream(spec, si, method, t, at, name)?,
                Ph::Glob(t, pattern) => {
                    if !is_plain_relative(pattern) {
                        return Err(bad(
                            at,
                            format!("{{{name}}}: a glob is {{glob:STAGE/PATTERN}}, the pattern inside the stage's folder"),
                        ));
                    }
                    upstream(spec, si, method, t, at, name)?;
                }
                _ => {}
            }
        }
        if is_pinto_lra(method) && method.mem_gb <= 0.0 {
            return Err(bad(
                &format!("{}.mem_gb", method.at),
                "is required for pinto lra-*: it sizes itself to the memory it sees free, \
                 so each task books what it takes",
            ));
        }
    }
    scores_made(spec)?;
    reference_truth(spec)?;
    grid(spec)
}

/// The grid has a point, and each point an id of its own (two of one id
/// would share their tasks' folders).
fn grid(spec: &Spec) -> Result<()> {
    let points = points(spec);
    if points.is_empty() {
        return Err(refuse("exclude: drops every point of the grid"));
    }
    let shown = |l: &BTreeMap<String, String>| {
        let pairs: Vec<String> = l.iter().map(|(a, v)| format!("{a}={v}")).collect();
        pairs.join(",")
    };
    let mut ids: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    for point in &points {
        let l = labels(point);
        let id = point_id(&l);
        if let Some(first) = ids.get(&id) {
            return Err(refuse(format!(
                "axes: the points {} and {} share the id {id}; relabel one",
                shown(first),
                shown(&l)
            )));
        }
        ids.insert(id, l);
    }
    Ok(())
}

/// `{<t>}` (or a glob in `t`) used at `at`, by `method` of stage `si`: `t` is
/// an earlier stage, and a method of it this method may read.
fn upstream(spec: &Spec, si: usize, method: &Method, t: &str, at: &str, name: &str) -> Result<()> {
    let Some(ti) = spec.stages.iter().position(|s| s.name == t) else {
        return Err(bad(at, format!("{{{name}}}: {t:?} is no stage")));
    };
    if ti == si {
        return Err(bad(
            at,
            format!("{{{name}}} is this stage's own folder; that is {{out}}"),
        ));
    }
    if ti > si {
        return Err(bad(
            at,
            format!("{{{name}}}: stage {t} runs after this one"),
        ));
    }
    let up = &spec.stages[ti];
    if !up.has_methods() {
        return Ok(());
    }
    let names = || method_names(up).join(", ");
    match &method.name {
        None => Err(bad(
            at,
            format!(
                "{{{name}}}: stage {t} has methods ({}), and this stage has none, \
                 so it names no one of them",
                names()
            ),
        )),
        Some(m) if !up.methods.iter().any(|o| o.name.as_ref() == Some(m)) => Err(bad(
            at,
            format!(
                "{{{name}}}: stage {t} has no method {m} (its methods: {}); \
                 a method that skips {t} may not use {{{t}}}",
                names()
            ),
        )),
        Some(_) => Ok(()),
    }
}

/// The names of `stage`'s methods.
fn method_names(stage: &Stage) -> Vec<&str> {
    stage
        .methods
        .iter()
        .filter_map(|m| m.name.as_deref())
        .collect()
}

/// The last stage of each method (and the last stage) makes exactly one
/// `*.score.tsv`.
fn scores_made(spec: &Spec) -> Result<()> {
    let last = spec.stages.len() - 1;
    for (si, stage) in spec.stages.iter().enumerate() {
        for method in &stage.methods {
            let ends = si == last
                || method.name.as_ref().is_some_and(|m| {
                    !spec.stages[si + 1..]
                        .iter()
                        .any(|s| s.methods.iter().any(|o| o.name.as_ref() == Some(m)))
                });
            if !ends {
                continue;
            }
            let n = scores(method);
            if n != 1 {
                let what = method
                    .name
                    .as_ref()
                    .map_or(format!("stage {}", stage.name), |m| format!("method {m}"));
                return Err(bad(
                    &format!("{}.makes", method.at),
                    format!(
                        "lists {n} *.score.tsv; {what} ends here, and the last stage of each \
                         method makes exactly one"
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// How many `*.score.tsv` `method` makes.
fn scores(method: &Method) -> usize {
    method
        .makes
        .iter()
        .filter(|m| m.ends_with(".score.tsv"))
        .count()
}

/// A data-beans multiome run with `--reference-atac` writes its pair truth
/// in synthetic index space against real peak names: no universe holds
/// those pairs, so no method that makes a score may read it.
fn reference_truth(spec: &Spec) -> Result<()> {
    for (si, stage) in spec.stages.iter().enumerate() {
        if !stage.methods.iter().any(is_reference_multiome) {
            continue;
        }
        for later in &spec.stages[si + 1..] {
            for method in later.methods.iter().filter(|m| scores(m) > 0) {
                for (i, (at, parts)) in words(spec, method)?.into_iter().enumerate() {
                    let reads = parts.iter().any(|p| {
                        matches!(p, Part::Ph(_, Ph::Stage(t) | Ph::Glob(t, _)) if *t == stage.name)
                    });
                    if reads && method.run[i].contains("ground_truth") {
                        return Err(bad(
                            &at,
                            format!(
                                "reads the pair truth of stage {}, a data-beans multiome run \
                                 with --reference-atac: those pairs index synthetic peaks \
                                 against real peak names, so no universe holds them",
                                stage.name
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// The replicates: (rep, rep_seed), from 1.
pub fn reps(seeds: &Seeds) -> Vec<(u32, u64)> {
    match seeds {
        Seeds::Derived { n, base } => (1..=*n).map(|r| (r, rep_seed(*base, r))).collect(),
        Seeds::List(list) => (1..).zip(list.iter().copied()).collect(),
    }
}

/// A replicate's seed: `splitmix64(base ^ splitmix64(rep)) & 0x7fffffff`.
pub fn rep_seed(base: u64, rep: u32) -> u64 {
    key::splitmix64(base ^ key::splitmix64(u64::from(rep))) & 0x7fff_ffff
}

/// A task's `{seed}`: `splitmix64(rep_seed ^ fnv64("<stage>/<method>")) &
/// 0x7fffffff`, `method` empty in a stage without methods.
pub fn stage_seed(rep_seed: u64, stage: &str, method: &str) -> u64 {
    let stream = format!("{stage}/{method}");
    key::splitmix64(rep_seed ^ key::fnv64(stream.as_bytes())) & 0x7fff_ffff
}

/// A point of the grid: each axis and its value there.
pub type Point<'a> = Vec<(&'a str, &'a AxisValue)>;

/// The grid's points, in the axes' order, less those `exclude` drops.
pub fn points(spec: &Spec) -> Vec<Point<'_>> {
    let mut grid: Vec<Point> = vec![Vec::new()];
    for axis in &spec.axes {
        grid = grid
            .into_iter()
            .flat_map(|p| {
                axis.values.iter().map(move |v| {
                    let mut q = p.clone();
                    q.push((axis.name.as_str(), v));
                    q
                })
            })
            .collect();
    }
    grid.retain(|p| {
        !spec.exclude.iter().any(|ex| {
            ex.iter()
                .all(|(a, l)| p.iter().any(|(b, v)| b == a && &v.label == l))
        })
    });
    grid
}

/// A point's labels, by axis.
fn labels(point: &Point) -> BTreeMap<String, String> {
    point
        .iter()
        .map(|(a, v)| (a.to_string(), v.label.clone()))
        .collect()
}

/// The first 8 hex digits of the key of a point's canonical labels.
pub fn point_id(labels: &BTreeMap<String, String>) -> String {
    let v: Value = json!(labels);
    key::key_of(&v)[..8].to_string()
}

/// A task's id.
pub fn task_id(point: &str, rep: u32, stage: &str, method: Option<&str>) -> String {
    match method {
        Some(m) => format!("{point}.r{rep}.{stage}-{m}"),
        None => format!("{point}.r{rep}.{stage}"),
    }
}

/// Each tool word the spec runs, with whether it is snapshot, and the
/// first method that names it; [`LEGUME`] also when an argv word uses
/// `{legume}`, snapshot as the methods that run it are, and when none does,
/// snapshot. A tool snapshot in one method and not in another is refused.
pub fn tool_words(spec: &Spec) -> Result<Vec<(String, bool, String)>> {
    let mut out: Vec<(String, bool, String)> = Vec::new();
    let mut embeds: Option<&str> = None;
    for method in spec.stages.iter().flat_map(|s| &s.methods) {
        let word = method.tool_word();
        if embeds.is_none() && embeds_legume(method) {
            embeds = Some(&method.at);
        }
        match out.iter().find(|(w, _, _)| w == word) {
            Some((_, s, first)) if *s != method.snapshot => {
                return Err(bad(
                    &format!("{}.snapshot", method.at),
                    format!(
                        "is {}, but {first} runs {word} with snapshot {s}",
                        method.snapshot
                    ),
                ))
            }
            Some(_) => {}
            None => out.push((word.to_string(), method.snapshot, method.at.clone())),
        }
    }
    if let Some(at) = embeds.filter(|_| !out.iter().any(|(w, _, _)| w == LEGUME)) {
        out.push((LEGUME.to_string(), true, at.to_string()));
    }
    Ok(out)
}

/// A run of an argv word, its placeholder classified.
#[derive(Clone, Copy)]
enum Part<'a> {
    Text(&'a str),
    Ph(&'a str, Ph<'a>),
}

/// What a method's tasks share: its words parsed, what they read, its tool
/// and its environment.
struct Prepared<'a> {
    stage: &'a Stage,
    method: &'a Method,
    /// `run[1..]`, each word's parts.
    words: Vec<Vec<Part<'a>>>,
    /// Each stage it reads, and whether that stage has methods.
    upstream: Vec<(&'a str, bool)>,
    inputs: Vec<&'a str>,
    /// legume's content key, when another tool's argv embeds `{legume}`.
    legume: Option<&'a str>,
    tool: &'a Resolved,
    env: BTreeMap<String, String>,
}

impl<'a> Prepared<'a> {
    fn new(spec: &'a Spec, stage: &'a Stage, method: &'a Method, ctx: &'a Context) -> Result<Self> {
        let word = method.tool_word();
        let tool = ctx.tool(word).ok_or_else(|| {
            bad(
                &format!("{}.run[0]", method.at),
                format!("{word} is not resolved"),
            )
        })?;
        let words: Vec<Vec<Part>> = words(spec, method)?
            .into_iter()
            .skip(1)
            .map(|w| w.1)
            .collect();
        let (mut upstream, mut inputs) = (Vec::new(), Vec::new());
        // Another tool's argv names `{legume}`, as [`embeds_legume`] says.
        let mut embeds = false;
        for part in words.iter().flatten() {
            match *part {
                Part::Ph(_, Ph::Stage(t) | Ph::Glob(t, _))
                    if !upstream.iter().any(|u: &(&str, bool)| u.0 == t) =>
                {
                    let has_methods = spec.stages.iter().any(|s| s.name == t && s.has_methods());
                    upstream.push((t, has_methods));
                }
                Part::Ph(_, Ph::Input(n)) if !inputs.contains(&n) => inputs.push(n),
                Part::Ph(_, Ph::Legume) if word != LEGUME => embeds = true,
                _ => {}
            }
        }
        let legume = embeds
            .then(|| ctx.tool(LEGUME).map(|t| t.content_key.as_str()))
            .flatten();
        let env = THREAD_VARS
            .iter()
            .map(|v| (v.to_string(), method.threads.to_string()))
            .collect();
        Ok(Self {
            stage,
            method,
            words,
            upstream,
            inputs,
            legume,
            tool,
            env,
        })
    }

    /// The task key of a task of this method that runs `argv`, from
    /// `template`, and reads the tasks keyed `upstream_keys`.
    fn key(
        &self,
        template: &[String],
        argv: &[String],
        upstream_keys: &[&String],
        ctx: &Context,
    ) -> String {
        // A declared input named by its path is read as well.
        let mut inputs = self.inputs.clone();
        for t in &argv[1..] {
            let p = named_path(t);
            if !p.is_absolute() {
                continue;
            }
            for n in inputs_under(p, ctx.inputs) {
                if !inputs.contains(&n) {
                    inputs.push(n);
                }
            }
        }
        inputs.sort_unstable();
        let input_keys: Vec<Value> = inputs
            .iter()
            .map(|n| json!({"name": n, "hash": ctx.inputs.get(*n).map(|i| &i.1)}))
            .collect();
        let mut keyed = json!({
            "stage": self.stage.name,
            "method": self.method.name,
            "argv": template,
            "env": *THREADS_ENV,
            "tool": self.tool.content_key,
            "inputs": input_keys,
            "upstream": upstream_keys,
        });
        if let Some(k) = self.legume {
            keyed["legume"] = json!(k);
        }
        key::key_of(&keyed)
    }
}

/// The environment a task key records: each thread variable as
/// `{threads}`, the same for every task.
static THREADS_ENV: LazyLock<Value> = LazyLock::new(|| {
    let env: BTreeMap<&str, &str> = THREAD_VARS.iter().map(|v| (*v, "{threads}")).collect();
    json!(env)
});

/// The values a task of `point` in replicate `rep` shares with the other
/// tasks there: each axis's label, its bindings, `rep` and `rep_seed`.
fn values<'a>(point: &Point<'a>, rep: u32, rep_seed: u64) -> HashMap<&'a str, String> {
    let mut values: HashMap<&str, String> = HashMap::new();
    for &(axis, v) in point {
        values.insert(axis, v.label.clone());
        for (b, x) in &v.bindings {
            values.insert(b, x.clone());
        }
    }
    values.insert("rep", rep.to_string());
    values.insert("rep_seed", rep_seed.to_string());
    values
}

/// What fills one task's placeholders.
struct Fill<'a> {
    values: &'a HashMap<&'a str, String>,
    method: Option<&'a str>,
    threads: u32,
    out: &'a Path,
    stages: &'a HashMap<&'a str, PathBuf>,
    ctx: &'a Context<'a>,
}

impl Fill<'_> {
    /// `word` filled: with `template`, only the task's values and method,
    /// every path, `{threads}` and `{gpu}` left as written; else all but
    /// `{gpu}` (given at launch), a glob's stage made its folder.
    fn word(&self, word: &[Part], template: bool) -> String {
        let mut s = String::new();
        for &part in word {
            let (name, ph) = match part {
                // The template writes a brace as the spec does, apart from
                // a placeholder.
                Part::Text(t @ ("{" | "}")) if template => {
                    s.push_str(t);
                    s.push_str(t);
                    continue;
                }
                Part::Text(t) => {
                    s.push_str(t);
                    continue;
                }
                Part::Ph(name, ph) => (name, ph),
            };
            let keep = format!("{{{name}}}");
            let filled = match ph {
                Ph::Value(v) => self.values.get(v).cloned().unwrap_or(keep),
                Ph::Method => self.method.map_or(keep, str::to_string),
                _ if template => keep,
                Ph::Threads => self.threads.to_string(),
                Ph::Out => self.out.display().to_string(),
                Ph::Legume => self
                    .ctx
                    .tool(LEGUME)
                    .map_or(keep, |t| t.exec.display().to_string()),
                Ph::Stage(t) => self.stages.get(t).map_or(keep, |d| d.display().to_string()),
                Ph::Input(n) => self
                    .ctx
                    .inputs
                    .get(n)
                    .map_or(keep, |(p, _)| p.display().to_string()),
                Ph::Glob(t, pattern) => self
                    .stages
                    .get(t)
                    .map_or(keep, |d| format!("{{glob:{}}}", d.join(pattern).display())),
                Ph::Gpu => keep,
            };
            s.push_str(&filled);
        }
        s
    }
}

/// Every task of `spec`, a spec [`check`] passed, replicate by replicate,
/// point by point, stage by stage.
pub fn tasks(spec: &Spec, ctx: &Context) -> Result<Vec<Task>> {
    let points = points(spec);
    let prepared: Vec<Prepared> = spec
        .stages
        .iter()
        .flat_map(|stage| stage.methods.iter().map(move |method| (stage, method)))
        .map(|(stage, method)| Prepared::new(spec, stage, method, ctx))
        .collect::<Result<_>>()?;
    let tasks_dir = ctx.dir.join("tasks");
    let mut keys: HashMap<String, String> = HashMap::new();
    let mut tasks = Vec::new();
    for (rep, rep_seed) in reps(&spec.seeds) {
        for point in &points {
            let labels = labels(point);
            let pid = point_id(&labels);
            let shared = values(point, rep, rep_seed);
            for p in &prepared {
                let (stage, method) = (p.stage, p.method);
                let name = method.name.as_deref();
                let id = task_id(&pid, rep, &stage.name, name);
                let seed = stage_seed(rep_seed, &stage.name, name.unwrap_or(""));
                let mut values = shared.clone();
                values.insert("seed", seed.to_string());
                let mut upstream: Vec<String> = Vec::new();
                let mut stages: HashMap<&str, PathBuf> = HashMap::new();
                for &(t, has_methods) in &p.upstream {
                    let uid = task_id(&pid, rep, t, name.filter(|_| has_methods));
                    stages.insert(t, tasks_dir.join(&uid));
                    upstream.push(uid);
                }
                let dir = tasks_dir.join(&id);
                let fill = Fill {
                    values: &values,
                    method: name,
                    threads: method.threads,
                    out: &dir,
                    stages: &stages,
                    ctx,
                };
                let mut template = vec![method.run[0].clone()];
                let mut argv = vec![p.tool.exec.display().to_string()];
                for w in &p.words {
                    template.push(fill.word(w, true));
                    argv.push(fill.word(w, false));
                }
                let upstream_keys: Vec<&String> =
                    upstream.iter().filter_map(|u| keys.get(u)).collect();
                let key = p.key(&template, &argv, &upstream_keys, ctx);
                keys.insert(id.clone(), key.clone());
                tasks.push(Task {
                    id,
                    point: labels.clone(),
                    rep,
                    rep_seed,
                    seed,
                    stage: stage.name.clone(),
                    method: name.map(str::to_string),
                    argv,
                    env: p.env.clone(),
                    threads: method.threads,
                    mem_gb: method.mem_gb,
                    gpus: method.gpus,
                    timeout_secs: method.timeout_secs,
                    max_running: method.max_running,
                    dir,
                    makes: method.makes.clone(),
                    upstream,
                    key,
                });
            }
        }
    }
    Ok(tasks)
}

/// What the plan warns of, though it runs: listed seeds close together
/// given to data-beans (its per-cell streams are `rseed + offset + j`, so
/// nearby seeds share most of them), and an argv path to an existing file
/// outside the benchmark that is no declared input (its content is not
/// keyed), not under a declared one.
pub fn warnings(spec: &Spec, tasks: &[Task], ctx: &Context) -> Vec<String> {
    let dir = ctx.dir.as_path();
    let mut out = Vec::new();
    if let Seeds::List(list) = &spec.seeds {
        let data_beans = spec
            .stages
            .iter()
            .flat_map(|s| &s.methods)
            .any(is_data_beans);
        let mut sorted = list.clone();
        sorted.sort_unstable();
        let close = sorted.windows(2).any(|w| w[1] - w[0] < CLOSE_SEEDS);
        if data_beans && close {
            out.push(format!(
                "seeds.list: seeds less than {CLOSE_SEEDS} apart overlap in data-beans, whose \
                 per-cell streams are rseed + offset + j; seeds.n derives seeds far apart"
            ));
        }
    }
    let mut seen: HashSet<&Path> = HashSet::new();
    for task in tasks {
        for token in &task.argv[1..] {
            let p = named_path(token);
            if !p.is_absolute() || p.starts_with(dir) || !seen.insert(p) {
                continue;
            }
            let declared = inputs_under(p, ctx.inputs).next().is_some();
            if !declared && p.is_file() {
                let path = p.display();
                out.push(format!(
                    "task {}: {path} is a file outside the benchmark; declare it in inputs, \
                     so its content enters the key",
                    task.id
                ));
            }
        }
    }
    out
}

/// The declared inputs at or above the absolute path `p`, as written or
/// once resolved.
fn inputs_under<'a>(
    p: &'a Path,
    inputs: &'a HashMap<String, (PathBuf, String)>,
) -> impl Iterator<Item = &'a str> {
    let real = p.canonicalize().ok();
    inputs
        .iter()
        .filter(move |(_, (i, _))| {
            p.starts_with(i) || real.as_deref().is_some_and(|r| r.starts_with(i))
        })
        .map(|(n, _)| n.as_str())
}

/// The path an argv word names: all of it, or what follows its last `=`
/// (`--from=PATH`).
fn named_path(token: &str) -> &Path {
    Path::new(token.rsplit_once('=').map_or(token, |p| p.1))
}

/// Listed seeds nearer than this overlap in data-beans' streams.
const CLOSE_SEEDS: u64 = 1 << 20;

/// What `{glob:<folder>/<pattern>}` gives when a task starts: every path
/// under `folder` that `pattern` matches (`*` and `?` within a part of
/// the path, a hidden name only by a part that starts with `.`, as in a
/// shell), natural-sorted, joined by commas; refused when none does.
pub fn glob(folder: &Path, pattern: &str) -> Result<String> {
    let mut found = vec![folder.to_path_buf()];
    for part in pattern.split('/').filter(|p| !p.is_empty()) {
        let mut next = Vec::new();
        for dir in &found {
            if !part.contains(['*', '?']) {
                let p = dir.join(part);
                if std::fs::symlink_metadata(&p).is_ok() {
                    next.push(p);
                }
                continue;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                let hidden = name.starts_with('.') && !part.starts_with('.');
                if !hidden && wild(part.as_bytes(), name.as_bytes()) {
                    next.push(e.path());
                }
            }
        }
        found = next;
    }
    if found.is_empty() {
        return Err(refuse(format!(
            "{{glob:{pattern}}} matches nothing in {}",
            folder.display()
        )));
    }
    let mut names: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
    names.sort_by(|a, b| natural(a, b));
    Ok(names.join(","))
}

/// `pattern` (with `*` and `?`) matches all of `s`.
fn wild(pattern: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, i));
            p += 1;
        } else if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == s[i]) {
            p += 1;
            i += 1;
        } else if let Some((sp, si)) = star {
            p = sp + 1;
            i = si + 1;
            star = Some((sp, si + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

/// Natural order: runs of digits compare as numbers.
fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let n = x.iter().take_while(|c| c.is_ascii_digit()).count();
                let m = y.iter().take_while(|c| c.is_ascii_digit()).count();
                let (u, v) = (trim_zeros(&x[..n]), trim_zeros(&y[..m]));
                let o = u.len().cmp(&v.len()).then_with(|| u.cmp(v));
                if o != Ordering::Equal {
                    return o;
                }
                x = &x[n..];
                y = &y[m..];
            }
            (Some(c), Some(d)) if c != d => return c.cmp(d),
            _ => {
                x = &x[1..];
                y = &y[1..];
            }
        }
    }
}

/// A run of digits less its leading zeros, keeping one digit.
fn trim_zeros(d: &[u8]) -> &[u8] {
    let z = d.iter().take_while(|&&c| c == b'0').count();
    &d[z.min(d.len().saturating_sub(1))..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::spec::parse;
    use crate::command::code;

    fn spec(text: &str) -> Spec {
        parse(text).unwrap().0
    }

    fn refused(text: &str) -> String {
        let e = check(&spec(text)).unwrap_err();
        assert_eq!(code(&e), 3, "{e}");
        e.to_string()
    }

    /// Every tool word as itself, keyed by its name.
    fn tools(spec: &Spec) -> Vec<Resolved> {
        tool_words(spec)
            .unwrap()
            .into_iter()
            .map(|(w, _, _)| Resolved {
                source: PathBuf::from(format!("/S/{w}")),
                exec: PathBuf::from(format!("/T/{w}")),
                content_key: format!("K-{w}"),
                version: None,
                word: w,
            })
            .collect()
    }

    /// The tool of `word` in `tools`, to change.
    fn tool<'a>(tools: &'a mut [Resolved], word: &str) -> &'a mut Resolved {
        tools.iter_mut().find(|t| t.word == word).unwrap()
    }

    fn expand(spec: &Spec) -> Vec<Task> {
        check(spec).unwrap();
        let tools = tools(spec);
        let inputs: HashMap<String, (PathBuf, String)> = spec
            .inputs
            .iter()
            .map(|i| {
                (
                    i.name.clone(),
                    (
                        PathBuf::from(format!("/I/{}", i.path)),
                        format!("H-{}", i.name),
                    ),
                )
            })
            .collect();
        plan_at(spec, "/B", &tools, &inputs)
    }

    /// The context of a benchmark at `dir`.
    fn context<'a>(
        dir: &Path,
        tools: &'a [Resolved],
        inputs: &'a HashMap<String, (PathBuf, String)>,
    ) -> Context<'a> {
        Context {
            dir: dir.to_path_buf(),
            tools,
            inputs,
        }
    }

    /// The tasks of `s` at `dir`.
    fn plan_at(
        s: &Spec,
        dir: &str,
        tools: &[Resolved],
        inputs: &HashMap<String, (PathBuf, String)>,
    ) -> Vec<Task> {
        tasks(s, &context(Path::new(dir), tools, inputs)).unwrap()
    }

    /// One declared input, `name` at `path` keyed `hash`.
    fn input(name: &str, path: &str, hash: &str) -> HashMap<String, (PathBuf, String)> {
        [(name.to_string(), (PathBuf::from(path), hash.to_string()))].into()
    }

    fn task<'a>(tasks: &'a [Task], id_end: &str, rep: u32) -> &'a Task {
        tasks
            .iter()
            .find(|c| c.id.ends_with(id_end) && c.rep == rep)
            .unwrap_or_else(|| panic!("no task {id_end} r{rep}"))
    }

    const METHODS: &str = r#"{"name": "N1", "seeds": {"n": 1},
      "stages": [
        {"name": "sim", "run": ["TOOL1", "-o", "{out}/sim"], "makes": ["sim.tsv"]},
        {"name": "fit", "methods": {
          "M1": {"run": ["TOOL2", "{sim}/sim.tsv", "{out}/fit"], "makes": ["fit.tsv"]}}},
        {"name": "score", "methods": {
          "M0": {"run": ["{legume}", "score", "{sim}/sim.tsv", "-o", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]},
          "M1": {"run": ["{legume}", "score", "{fit}/fit.tsv", "-o", "{out}/m.score.tsv"], "makes": ["m.score.tsv"]}}}]}"#;

    // A method that an upstream methods stage lacks (M0 skips fit) may not
    // use that stage's placeholder; one it has may; a stage without
    // methods names no one method of one with.
    #[test]
    fn methods_must_match() {
        let tasks = expand(&spec(METHODS));
        let m1 = task(&tasks, ".score-M1", 1);
        let fit = task(&tasks, ".fit-M1", 1);
        assert_eq!(m1.upstream, [fit.id.as_str()]);
        assert_eq!(m1.argv[2], format!("/B/tasks/{}/fit.tsv", fit.id));
        let m0 = task(&tasks, ".score-M0", 1);
        assert_eq!(m0.upstream, [task(&tasks, ".sim", 1).id.as_str()]);

        let skipped = METHODS.replace("\"{sim}/sim.tsv\", \"-o\"", "\"{fit}/fit.tsv\", \"-o\"");
        assert_eq!(
            refused(&skipped),
            "stages[2].methods.M0.run[2]: {fit}: stage fit has no method M0 (its methods: M1); \
             a method that skips fit may not use {fit}"
        );
        let glob = METHODS.replace("\"{sim}/sim.tsv\", \"-o\"", "\"{glob:fit/*.tsv}\", \"-o\"");
        assert_eq!(
            refused(&glob),
            "stages[2].methods.M0.run[2]: {glob:fit/*.tsv}: stage fit has no method M0 \
             (its methods: M1); a method that skips fit may not use {fit}"
        );
        let plain = r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "fit", "methods": {"M1": {"run": ["T"], "makes": ["a"]}}},
            {"name": "score", "run": ["T", "{fit}"], "makes": ["m.score.tsv"]}]}"#;
        assert_eq!(
            refused(plain),
            "stages[1].run[1]: {fit}: stage fit has methods (M1), and this stage \
             has none, so it names no one of them"
        );
    }

    #[test]
    fn placeholders_name_something_in_its_place() {
        let cases: &[(&str, &str)] = &[
            ("{bogus}", "{bogus} is no placeholder: not an axis, a binding, a stage, inputs.NAME, \
              glob:STAGE/PATTERN, or one of rep, rep_seed, seed, method, threads, gpu, out, legume"),
            ("{method}", "{method} is for a stage with methods"),
            ("{gpu}", "{gpu} names a GPU, and this task asks for none"),
            ("{inputs.RUN2}", "{inputs.RUN2}: \"RUN2\" is no declared input"),
            ("{score}", "{score}: stage score runs after this one"),
            ("{sim}", "{sim} is this stage's own folder; that is {out}"),
            ("{glob:sim}", "{glob:sim}: a glob is {glob:STAGE/PATTERN}, the pattern inside the stage's folder"),
            ("{glob:sim/../x}", "{glob:sim/../x}: a glob is {glob:STAGE/PATTERN}, the pattern inside the stage's folder"),
            ("{out", "\"{out\" leaves a { open ({{ writes a {)"),
            ("{}", "\"{}\" has an empty or nested placeholder"),
            ("a}b", "\"a}b\" has a } with no { (}} writes a })"),
        ];
        for (token, want) in cases {
            let text = format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "inputs": {{"RUN1": {{"path": "R"}}}}, "stages": [
                  {{"name": "sim", "run": ["T", "{token}"], "makes": ["a"]}},
                  {{"name": "score", "run": ["T"], "makes": ["m.score.tsv"]}}]}}"#
            );
            assert_eq!(
                refused(&text),
                format!("stages[0].run[1]: {want}"),
                "{token}"
            );
        }
    }

    // tuple_axis_binds_all_fields: a tuple axis's label fills its own
    // placeholder and the point; each binding fills its own; the grid is
    // the plain axes times the tuple axes, less exclude.
    #[test]
    fn tuple_axis_binds_all_fields() {
        let s = spec(
            r#"{"name": "N1", "seeds": {"n": 1},
            "axes": {"DS": [{"label": "L1", "p1": "0", "p2": "0.5"},
                            {"label": "L2", "p1": "0.01", "p2": "0.02"}],
                     "K": ["k1", "k2", "k3"]},
            "exclude": [{"DS": "L2", "K": "k3"}],
            "stages": [{"name": "sim",
              "run": ["T", "--set", "a={p1}", "--set", "b={p2}", "--ds", "{DS}", "-k", "{K}"],
              "makes": ["m.score.tsv"]}]}"#,
        );
        let tasks = expand(&s);
        assert_eq!(tasks.len(), 5);
        let l2 = tasks
            .iter()
            .find(|c| c.point["DS"] == "L2" && c.point["K"] == "k1")
            .unwrap();
        assert_eq!(
            l2.argv[1..],
            ["--set", "a=0.01", "--set", "b=0.02", "--ds", "L2", "-k", "k1"]
        );
        assert!(!tasks
            .iter()
            .any(|c| c.point["DS"] == "L2" && c.point["K"] == "k3"));
        let ids: Vec<&str> = tasks.iter().map(|c| &c.id[..8]).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 5, "each point its own id");
    }

    // Task ids: 8 hex digits of the canonical point, then rep, stage and
    // method. A point keeps its id when an axis gains a value.
    #[test]
    fn task_ids_name_the_point_rep_stage_and_method() {
        let one = |values: &str| {
            let s = spec(&format!(
                r#"{{"name": "N1", "seeds": {{"n": 2}}, "axes": {{"A": [{values}]}},
                "stages": [{{"name": "fit", "methods": {{"M1": {{"run": ["T", "{{A}}"], "makes": ["m.score.tsv"]}}}}}}]}}"#
            ));
            expand(&s)
        };
        let small = one(r#""1""#);
        let large = one(r#""1", "2""#);
        let labels: BTreeMap<String, String> = [("A".to_string(), "1".to_string())].into();
        let pid = point_id(&labels);
        assert_eq!(pid, key::key_of(&json!({"A": "1"}))[..8]);
        assert_eq!(small[1].id, format!("{pid}.r2.fit-M1"));
        assert!(large
            .iter()
            .any(|c| c.id == small[1].id && c.key == small[1].key));
        assert_eq!(task_id("0a1b2c3d", 1, "sim", None), "0a1b2c3d.r1.sim");
    }

    // seeds_stage_derived_31bit: rep_seed and seed by the stated formulas
    // (the values computed apart from this code), within 31 bits, the
    // same rep_seed at every point of a replicate, and no two methods or
    // stages sharing a seed.
    #[test]
    fn seeds_stage_derived_31bit() {
        assert_eq!(rep_seed(1, 1), 1_448_800_798);
        assert_eq!(rep_seed(1, 2), 848_024_197);
        assert_eq!(stage_seed(1_448_800_798, "sim", ""), 983_310_059);
        assert_eq!(stage_seed(1_448_800_798, "score", ""), 708_204_895);
        let s = spec(
            r#"{"name": "N1", "seeds": {"n": 3, "base": 1}, "axes": {"A": ["1", "2"]},
            "stages": [{"name": "sim", "run": ["T", "{rep_seed}", "{seed}"], "makes": ["s"]},
                       {"name": "fit", "methods": {
                          "M1": {"run": ["T", "{seed}"], "makes": ["m.score.tsv"]},
                          "M2": {"run": ["T", "{seed}"], "makes": ["m.score.tsv"]}}}]}"#,
        );
        let tasks = expand(&s);
        assert_eq!(tasks.len(), 2 * 3 * 3);
        for c in &tasks {
            assert!(c.rep_seed <= 0x7fff_ffff && c.seed <= 0x7fff_ffff, "{c:?}");
            assert_eq!(c.rep_seed, rep_seed(1, c.rep));
            let m = c.method.as_deref().unwrap_or("");
            assert_eq!(c.seed, stage_seed(c.rep_seed, &c.stage, m));
        }
        let sim = task(&tasks, ".sim", 1);
        assert_eq!(sim.argv[1..], ["1448800798", "983310059"]);
        let r1: Vec<&Task> = tasks.iter().filter(|c| c.rep == 1).collect();
        assert!(r1.iter().all(|c| c.rep_seed == 1_448_800_798));
        let mut seeds: Vec<u64> = r1
            .iter()
            .filter(|c| c.point["A"] == "1")
            .map(|c| c.seed)
            .collect();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), 3, "sim, M1 and M2 each a stream");

        let listed = spec(
            r#"{"name": "N1", "seeds": {"list": [7, 3]},
            "stages": [{"name": "sim", "run": ["T", "{rep}", "{rep_seed}"], "makes": ["m.score.tsv"]}]}"#,
        );
        let tasks = expand(&listed);
        assert_eq!(tasks[1].argv[1..], ["2", "3"]);
        assert_eq!(tasks[1].seed, stage_seed(3, "sim", ""));
    }

    // The key: what a task runs, by content. Paths, {threads} and {gpu}
    // stay templates; a value, the tool's bytes or an input's do not.
    #[test]
    fn a_key_follows_what_the_task_runs_and_not_where() {
        let text = r#"{"name": "N1", "seeds": {"n": 1}, "inputs": {"RUN1": {"path": "R"}},
          "stages": [
            {"name": "sim", "threads": 4, "run": ["T1", "{inputs.RUN1}", "-t", "{threads}", "-o", "{out}"], "makes": ["s"]},
            {"name": "score", "run": ["T2", "{sim}/s"], "makes": ["m.score.tsv"]}]}"#;
        let s = spec(text);
        let tools = tools(&s);
        let inputs = input("RUN1", "/I/R", "H1");
        let a = plan_at(&s, "/B", &tools, &inputs);
        assert_eq!(
            a[0].argv[1..],
            ["/I/R", "-t", "4", "-o", &format!("/B/tasks/{}", a[0].id)]
        );
        assert_eq!(a[0].env["RAYON_NUM_THREADS"], "4");
        let moved = plan_at(&s, "/elsewhere", &tools, &inputs);
        assert_eq!(a[0].key, moved[0].key);
        assert_eq!(a[1].key, moved[1].key);

        let threads = spec(&text.replace("\"threads\": 4", "\"threads\": 2"));
        let t = plan_at(&threads, "/B", &tools, &inputs);
        assert_eq!(a[0].key, t[0].key, "threads are keyed as {{threads}}");

        let b = plan_at(&s, "/B", &tools, &input("RUN1", "/I/R", "H2"));
        assert_ne!(a[0].key, b[0].key, "an input's content");
        assert_ne!(a[1].key, b[1].key, "and so its downstream");
        let mut rebuilt = tools.clone();
        tool(&mut rebuilt, "T2").content_key = "K-T2b".into();
        let c = plan_at(&s, "/B", &rebuilt, &inputs);
        assert_eq!(a[0].key, c[0].key);
        assert_ne!(a[1].key, c[1].key, "a tool's bytes");
    }

    /// Each task's key, with `tools` and the input RUN1 at /I/R keyed `h`.
    fn keys_with(s: &Spec, tools: &[Resolved], h: &str) -> Vec<String> {
        plan_at(s, "/B", tools, &input("RUN1", "/I/R", h))
            .into_iter()
            .map(|c| c.key)
            .collect()
    }

    // A declared input named by its path, not by {inputs.NAME}, is read
    // all the same: its content enters the key. A path beside it is not
    // under it.
    #[test]
    fn a_declared_input_named_by_its_path_is_keyed() {
        for (token, keyed) in [
            ("/I/R/m.json", true),
            ("--from=/I/R", true),
            ("/I/R2/m.json", false),
        ] {
            let s = spec(&format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "inputs": {{"RUN1": {{"path": "R"}}}},
                "stages": [{{"name": "sim", "run": ["T1", "{token}"], "makes": ["m.score.tsv"]}}]}}"#
            ));
            let tools = tools(&s);
            let (a, b) = (keys_with(&s, &tools, "H1"), keys_with(&s, &tools, "H2"));
            assert_eq!(a != b, keyed, "{token}");
        }
    }

    // Nested inputs found by one path enter the key in the order of their
    // names, not in the order a map of them gives.
    #[test]
    fn inputs_found_by_path_are_keyed_in_name_order() {
        let s = spec(
            r#"{"name": "N1", "seeds": {"n": 1},
            "inputs": {"R1": {"path": "R"}, "R2": {"path": "R/S"}, "R3": {"path": "R/S/T"}},
            "stages": [{"name": "sim", "run": ["T1", "/I/R/S/T/a"], "makes": ["m.score.tsv"]}]}"#,
        );
        let tools = tools(&s);
        // Each map of the same inputs iterates in an order of its own.
        let key = || {
            let inputs: HashMap<String, (PathBuf, String)> =
                [("R1", "/I/R"), ("R2", "/I/R/S"), ("R3", "/I/R/S/T")]
                    .into_iter()
                    .map(|(n, p)| (n.to_string(), (PathBuf::from(p), format!("H-{n}"))))
                    .collect();
            plan_at(&s, "/B", &tools, &inputs).remove(0).key
        };
        let first = key();
        for _ in 0..32 {
            assert_eq!(key(), first);
        }
    }

    // {legume} inside a word of another tool's argv runs legume: its bytes
    // enter the key, as a run[0] tool's do.
    #[test]
    fn an_embedded_legume_is_keyed() {
        let s = spec(
            r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "sim", "run": ["T1", "-c", "{legume} score --x {out}/x"], "makes": ["m.score.tsv"]}]}"#,
        );
        let tools = tools(&s);
        let mut rebuilt = tools.clone();
        tool(&mut rebuilt, LEGUME).content_key = "K-legume-b".into();
        assert_ne!(keys_with(&s, &tools, "H1"), keys_with(&s, &rebuilt, "H1"));
    }

    // Two points of one id would share a folder: refused, as is a grid
    // exclude empties.
    #[test]
    fn each_point_has_its_own_id() {
        let text = |axis: &str, exclude: &str| {
            format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "axes": {{"A": {axis}}}, "exclude": {exclude},
                "stages": [{{"name": "sim", "run": ["T", "{{A}}"], "makes": ["m.score.tsv"]}}]}}"#
            )
        };
        let one = |l: &str| point_id(&[("A".to_string(), l.to_string())].into());
        assert_eq!(one("82777"), one("105161"));
        assert_eq!(
            refused(&text(r#"["82777", "105161"]"#, "[]")),
            format!(
                "axes: the points A=82777 and A=105161 share the id {}; relabel one",
                one("82777")
            )
        );
        assert_eq!(
            refused(&text(r#"["1"]"#, r#"[{"A": "1"}]"#)),
            "exclude: drops every point of the grid"
        );
    }

    // {{ and }} write a brace: in argv one brace, in the key's template
    // as written, so a literal {out} is keyed apart from the folder.
    #[test]
    fn doubled_braces_write_one() {
        let one = |token: &str| {
            let s = spec(&format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "stages": [
                {{"name": "sim", "run": ["T", "-e", "{token}"], "makes": ["m.score.tsv"]}}]}}"#
            ));
            let words = tool_words(&s).unwrap();
            let c = expand(&s).remove(0);
            (c.argv[2].clone(), c.key, words.len())
        };
        let (argv, _, words) = one("{{print $1}} {{legume}}");
        assert_eq!(argv, "{print $1} {legume}");
        assert_eq!(words, 1, "a literal {{legume}} runs no legume");
        let (argv, literal, _) = one("{{out}}");
        assert_eq!(argv, "{out}");
        let (argv, folder, _) = one("{out}");
        assert!(argv.starts_with("/B/tasks/"), "{argv}");
        assert_ne!(literal, folder);
    }

    #[test]
    fn the_last_stage_of_each_method_makes_one_score() {
        let no_score = METHODS.replace(
            r#""makes": ["m.score.tsv"]}}}]}"#,
            r#""makes": ["m.tsv"]}}}]}"#,
        );
        assert_eq!(
            refused(&no_score),
            "stages[2].methods.M1.makes: lists 0 *.score.tsv; method M1 ends here, and the last \
             stage of each method makes exactly one"
        );
        let ends_early = METHODS.replace(
            r#""M1": {"run": ["TOOL2", "{sim}/sim.tsv", "{out}/fit"], "makes": ["fit.tsv"]}"#,
            r#""M1": {"run": ["TOOL2", "{sim}/sim.tsv", "{out}/fit"], "makes": ["fit.tsv"]},
               "M3": {"run": ["TOOL2"], "makes": ["fit.tsv"]}"#,
        );
        assert_eq!(
            refused(&ends_early),
            "stages[1].methods.M3.makes: lists 0 *.score.tsv; method M3 ends here, and the last \
             stage of each method makes exactly one"
        );
        let two_scores = METHODS.replace(
            r#""makes": ["m.score.tsv"]}}}]}"#,
            r#""makes": ["m.score.tsv", "n.score.tsv"]}}}]}"#,
        );
        assert_eq!(
            refused(&two_scores),
            "stages[2].methods.M1.makes: lists 2 *.score.tsv; method M1 ends here, and the last \
             stage of each method makes exactly one"
        );
    }

    #[test]
    fn pinto_lra_books_its_memory() {
        let text = |mem: &str| {
            format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "stages": [
                {{"name": "sim", {mem} "run": ["pinto", "lra-simulate", "--one"], "makes": ["a"]}},
                {{"name": "score", "run": ["T"], "makes": ["m.score.tsv"]}}]}}"#
            )
        };
        assert_eq!(
            refused(&text("")),
            "stages[0].mem_gb: is required for pinto lra-*: it sizes itself to the memory it \
             sees free, so each task books what it takes"
        );
        check(&spec(&text(r#""mem_gb": 8,"#))).unwrap();
    }

    // reference_atac_pair_truth_refused: a score of a data-beans multiome
    // run's pair truth with --reference-atac is refused; the synthetic one
    // is not.
    #[test]
    fn reference_atac_pair_truth_refused() {
        let text = |extra: &str| {
            format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "stages": [
                {{"name": "sim", "run": ["data-beans-sim", "multiome", {extra} "-o", "{{out}}/sim"],
                  "makes": ["sim.ground_truth.tsv.gz"]}},
                {{"name": "score", "run": ["{{legume}}", "score", "--truth", "{{sim}}/sim.ground_truth.tsv.gz"],
                  "makes": ["m.score.tsv"]}}]}}"#
            )
        };
        check(&spec(&text(""))).unwrap();
        for extra in [
            r#""--reference-atac", "R.zarr","#,
            r#""--reference-atac=R.zarr","#,
        ] {
            assert_eq!(
                refused(&text(extra)),
                "stages[1].run[3]: reads the pair truth of stage sim, a data-beans multiome run \
                 with --reference-atac: those pairs index synthetic peaks against real peak \
                 names, so no universe holds them",
                "{extra}"
            );
        }
        // A stage that makes no score may read it, for a diagnostic.
        let diagnostic = r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "sim", "run": ["data-beans-sim", "multiome", "--reference-atac", "R.zarr"],
             "makes": ["sim.ground_truth.tsv.gz"]},
            {"name": "fit", "run": ["T", "--truth", "{sim}/sim.ground_truth.tsv.gz"], "makes": ["fit.tsv"]},
            {"name": "score", "run": ["T", "{fit}/fit.tsv"], "makes": ["m.score.tsv"]}]}"#;
        check(&spec(diagnostic)).unwrap();
        // A score may read any other file of that stage.
        let other_file = r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "sim", "run": ["data-beans-sim", "multiome", "--reference-atac", "R.zarr"],
             "makes": ["sim.h5ad"]},
            {"name": "score", "run": ["T", "{sim}/sim.h5ad"], "makes": ["m.score.tsv"]}]}"#;
        check(&spec(other_file)).unwrap();
    }

    #[test]
    fn a_tool_is_snapshot_in_every_method_or_none() {
        let s = spec(
            r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "fit", "methods": {
              "M1": {"run": ["T"], "makes": ["m.score.tsv"]},
              "M2": {"snapshot": false, "run": ["T"], "makes": ["m.score.tsv"]}}}]}"#,
        );
        let e = tool_words(&s).unwrap_err();
        assert_eq!(
            e.to_string(),
            "stages[0].methods.M2.snapshot: is false, but stages[0].methods.M1 runs T with snapshot true"
        );
        // {legume} run unsnapshot is one tool, not two at odds.
        let own = spec(
            r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "score", "snapshot": false, "run": ["{legume}", "score"], "makes": ["m.score.tsv"]}]}"#,
        );
        let words = tool_words(&own).unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!((words[0].0.as_str(), words[0].1), (LEGUME, false));
        // {legume} inside another method's argv runs it as the methods that
        // run it do, and snapshot when none does.
        let embedded = |m1: &str| {
            let s = spec(&format!(
                r#"{{"name": "N1", "seeds": {{"n": 1}}, "stages": [
                {{"name": "fit", "methods": {{{m1}
                  "M2": {{"run": ["T", "{{legume}}"], "makes": ["m.score.tsv"]}}}}}}]}}"#
            ));
            let words = tool_words(&s).unwrap();
            words.iter().find(|w| w.0 == LEGUME).map(|w| w.1)
        };
        let m1 =
            r#""M1": {"snapshot": false, "run": ["{legume}", "score"], "makes": ["m.score.tsv"]},"#;
        assert_eq!(embedded(m1), Some(false));
        assert_eq!(embedded(""), Some(true));
        // run[0] is replaced by the tool's own path: under another tool's
        // name, a {legume} there runs no legume.
        let named = spec(
            r#"{"name": "N1", "seeds": {"n": 1}, "stages": [
            {"name": "fit", "tool": "T", "run": ["{legume}", "x"], "makes": ["m.score.tsv"]}]}"#,
        );
        let words = tool_words(&named).unwrap();
        assert!(words.iter().all(|w| w.0 != LEGUME), "{words:?}");
        assert_eq!(expand(&named).remove(0).argv[0], "/T/T");
    }

    // glob_natural_sort_fails_empty: matches are natural-sorted and
    // comma-joined; no match is refused.
    #[test]
    fn glob_natural_sort_fails_empty() {
        let d = tempfile::tempdir().unwrap();
        let sec = d.path().join("sim/sections");
        std::fs::create_dir_all(&sec).unwrap();
        for n in ["S10.zarr", "S2.zarr", "S1.zarr", "S2.coords.parquet"] {
            std::fs::write(sec.join(n), b"").unwrap();
        }
        let at = |n: &str| sec.join(n).display().to_string();
        assert_eq!(
            glob(d.path(), "sim/sections/S*.zarr").unwrap(),
            [at("S1.zarr"), at("S2.zarr"), at("S10.zarr")].join(",")
        );
        assert_eq!(
            glob(d.path(), "sim/sections/S?.coords.*").unwrap(),
            at("S2.coords.parquet")
        );
        let e = glob(d.path(), "sim/sections/T*.zarr").unwrap_err();
        assert_eq!(code(&e), 3);
        assert!(e
            .to_string()
            .starts_with("{glob:sim/sections/T*.zarr} matches nothing in "));
        assert_eq!(natural("S01", "S1"), Ordering::Equal);
        assert_eq!(natural("a9", "a10"), Ordering::Less);
        assert!(wild(b"*", b"") && wild(b"a*b*c", b"aXbYbc") && !wild(b"a?c", b"ac"));
        // A star in a name is matched by the pattern's star too.
        assert!(wild(b"*b", b"*ab"));
        // A wildcard part, as in a shell, leaves out a hidden file (an
        // atomic write's temporary) unless it starts with a dot itself.
        std::fs::write(sec.join(".S3.zarr.123"), b"").unwrap();
        assert_eq!(
            glob(d.path(), "sim/sections/*").unwrap().split(',').count(),
            4
        );
        assert_eq!(
            glob(d.path(), "sim/sections/.S*").unwrap(),
            at(".S3.zarr.123")
        );
    }

    #[test]
    fn warnings_name_close_seeds_and_undeclared_files() {
        let d = tempfile::tempdir().unwrap();
        let (f, declared) = (d.path().join("R1.tsv"), d.path().join("R2.tsv"));
        std::fs::write(&f, b"").unwrap();
        std::fs::write(&declared, b"").unwrap();
        let s = spec(&format!(
            r#"{{"name": "N1", "seeds": {{"list": [1, 2]}}, "inputs": {{"R2": {{"path": "R2.tsv"}}}},
            "stages": [{{"name": "sim", "run": ["data-beans-sim", "--x={}", "{{inputs.R2}}"],
              "makes": ["m.score.tsv"]}}]}}"#,
            f.display()
        ));
        let tools = tools(&s);
        let inputs = input("R2", &declared.display().to_string(), "H2");
        let at = |dir: &Path| {
            let ctx = context(dir, &tools, &inputs);
            let tasks = tasks(&s, &ctx).unwrap();
            let w = warnings(&s, &tasks, &ctx);
            (tasks, w)
        };
        let (tasks, w) = at(Path::new("/B"));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].starts_with("seeds.list: seeds less than 1048576 apart overlap"));
        assert_eq!(
            w[1],
            format!(
                "task {}: {} is a file outside the benchmark; declare it in inputs, so its \
                 content enters the key",
                tasks[0].id,
                f.display()
            )
        );
        assert_eq!(at(d.path()).1.len(), 1, "inside the benchmark");
    }
}
