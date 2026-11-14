//! A benchmark's spec, parsed from JSON and checked field by field: the
//! grid (axes, tuple axes, exclude), the seeds, the declared inputs, the
//! stages and their lanes (methods), and what each cell needs (threads,
//! memory, GPUs, timeout, tool). A refusal names the field it is about,
//! as a path such as `stages[2].methods.M1.threads`.
//!
//! What the placeholders in `run` may name, and the lane rules between
//! stages, are checked when the spec is planned ([`super::plan::check`]).

use crate::command::refuse;
use anyhow::Result;
use serde_json::{Map, Value};

/// The child environment's thread variables, each set to the cell's
/// threads.
pub const THREAD_VARS: [&str; 6] = [
    "RAYON_NUM_THREADS",
    "OMP_NUM_THREADS",
    "OPENBLAS_NUM_THREADS",
    "MKL_NUM_THREADS",
    "VECLIB_MAXIMUM_THREADS",
    "POLARS_MAX_THREADS",
];

/// A cell's timeout when none is given: 4 h.
pub const DEFAULT_TIMEOUT_SECS: u64 = 4 * 3600;

/// The placeholders every cell has; no axis, binding or stage may take
/// these names.
pub const RESERVED: [&str; 10] = [
    "rep", "rep_seed", "seed", "method", "threads", "gpu", "out", "legume", "inputs", "glob",
];

/// A parsed spec.
#[derive(Clone, Debug)]
pub struct Spec {
    pub name: String,
    /// Cores to book; the machine's logical CPUs when not given.
    pub cores: Option<u32>,
    /// Memory to book, in GB; MemAvailable at plan time when not given.
    pub mem_gb: Option<f64>,
    pub gpus: Vec<u32>,
    pub gpu_slots: u32,
    pub grace_secs: u64,
    pub max_hours: Option<f64>,
    pub min_free_gb: f64,
    /// By name.
    pub axes: Vec<Axis>,
    /// Partial points to drop: (axis, label) pairs, all of which match.
    pub exclude: Vec<Vec<(String, String)>>,
    pub seeds: Seeds,
    /// By name.
    pub inputs: Vec<Input>,
    pub stages: Vec<Stage>,
    pub compare: Vec<[String; 2]>,
    pub plot: Option<Plot>,
}

/// An axis of the grid.
#[derive(Clone, Debug)]
pub struct Axis {
    pub name: String,
    pub values: Vec<AxisValue>,
    /// Its values are objects: a label and bindings.
    pub tuple: bool,
}

/// An axis value: its label (the value itself on a plain axis), and on a
/// tuple axis the bindings it sets, by name.
#[derive(Clone, Debug)]
pub struct AxisValue {
    pub label: String,
    pub bindings: Vec<(String, String)>,
}

/// The replicates' seeds.
#[derive(Clone, Debug)]
pub enum Seeds {
    /// Replicates 1 to `n`, each seed derived from `base`.
    Derived { n: u32, base: u64 },
    /// One replicate per seed, the seed as given.
    List(Vec<u64>),
}

/// A declared input: the only files whose content enters a key.
#[derive(Clone, Debug)]
pub struct Input {
    pub name: String,
    /// As written: relative to the project.
    pub path: String,
    /// A folder, keyed by its listing.
    pub dir: bool,
}

/// A stage: one lane, or one per method.
#[derive(Clone, Debug)]
pub struct Stage {
    pub name: String,
    pub methods: bool,
    pub lanes: Vec<Lane>,
}

/// What one cell of a stage, or of a method's lane in it, runs and needs,
/// the stage's values filled in where the lane gives none.
#[derive(Clone, Debug)]
pub struct Lane {
    /// The method; `None` in a stage without methods.
    pub method: Option<String>,
    /// Where it is in the spec, for refusals.
    pub at: String,
    pub run: Vec<String>,
    pub makes: Vec<String>,
    pub threads: u32,
    pub mem_gb: f64,
    pub gpus: u32,
    pub timeout_secs: u64,
    /// The binary to run, resolved through PATH: `run[0]` when not given.
    pub tool: Option<String>,
    pub snapshot: bool,
    pub max_running: Option<u32>,
}

impl Lane {
    /// The word naming its tool: `tool`, else `run[0]`.
    pub fn tool_word(&self) -> &str {
        self.tool.as_deref().unwrap_or(&self.run[0])
    }
}

/// What gather plots.
#[derive(Clone, Debug)]
pub struct Plot {
    pub x: String,
    pub facet: Option<String>,
    pub metrics: Vec<String>,
}

/// `text` as a spec, and as JSON (for its canonical form).
pub fn parse(text: &str) -> Result<(Spec, Value)> {
    let v: Value =
        serde_json::from_str(text).map_err(|e| refuse(format!("the spec is not JSON: {e}")))?;
    let spec = from_value(&v)?;
    Ok((spec, v))
}

/// A refusal of what is at `at`.
fn bad(at: &str, what: impl std::fmt::Display) -> anyhow::Error {
    refuse(format!("{at}: {what}"))
}

/// A name: one to 40 of `[A-Za-z0-9._-]`, as an axis value or a label.
fn is_value(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A placeholder name: a letter or `_`, then letters, digits and `_`.
fn is_ident(s: &str) -> bool {
    let mut b = s.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// A method's name: one to 40 of `[A-Za-z0-9_-]`, not starting with `-`.
fn is_method(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// An object's fields, each taken once; [`Self::done`] refuses the rest.
struct Obj<'a> {
    at: String,
    map: &'a Map<String, Value>,
    taken: Vec<&'static str>,
}

impl<'a> Obj<'a> {
    fn new(at: impl Into<String>, v: &'a Value) -> Result<Self> {
        let at = at.into();
        match v {
            Value::Object(map) => Ok(Self {
                at,
                map,
                taken: Vec::new(),
            }),
            _ => Err(bad(&at, format!("is {}, not an object", kind(v)))),
        }
    }

    fn at(&self, k: &str) -> String {
        if self.at.is_empty() {
            k.to_string()
        } else {
            format!("{}.{k}", self.at)
        }
    }

    fn get(&mut self, k: &'static str) -> Option<&'a Value> {
        self.taken.push(k);
        self.map.get(k).filter(|v| !v.is_null())
    }

    fn string(&mut self, k: &'static str) -> Result<Option<String>> {
        match self.get(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(v) => Err(bad(&self.at(k), format!("is {}, not a string", kind(v)))),
        }
    }

    fn uint(&mut self, k: &'static str) -> Result<Option<u64>> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| {
                bad(
                    &self.at(k),
                    format!("is {v}, not a whole number of 0 or more"),
                )
            }),
        }
    }

    fn u32(&mut self, k: &'static str) -> Result<Option<u32>> {
        match self.uint(k)? {
            None => Ok(None),
            Some(n) => u32::try_from(n)
                .map(Some)
                .map_err(|_| bad(&self.at(k), format!("{n} is too large"))),
        }
    }

    fn number(&mut self, k: &'static str) -> Result<Option<f64>> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => match v.as_f64() {
                Some(x) if x >= 0.0 => Ok(Some(x)),
                _ => Err(bad(
                    &self.at(k),
                    format!("is {v}, not a number of 0 or more"),
                )),
            },
        }
    }

    fn bool(&mut self, k: &'static str) -> Result<Option<bool>> {
        match self.get(k) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(v) => Err(bad(
                &self.at(k),
                format!("is {}, not true or false", kind(v)),
            )),
        }
    }

    fn strings(&mut self, k: &'static str) -> Result<Option<Vec<String>>> {
        let Some(v) = self.get(k) else {
            return Ok(None);
        };
        let at = self.at(k);
        let Value::Array(items) = v else {
            return Err(bad(&at, format!("is {}, not a list of strings", kind(v))));
        };
        items
            .iter()
            .enumerate()
            .map(|(i, x)| match x {
                Value::String(s) => Ok(s.clone()),
                _ => Err(bad(
                    &format!("{at}[{i}]"),
                    format!("is {}, not a string", kind(x)),
                )),
            })
            .collect::<Result<_>>()
            .map(Some)
    }

    /// Refuses a field no `get` took.
    fn done(self) -> Result<()> {
        if let Some(k) = self.map.keys().find(|k| !self.taken.contains(&k.as_str())) {
            let mut known = self.taken.clone();
            known.sort_unstable();
            known.dedup();
            return Err(bad(
                &self.at(k),
                format!("is no field here (it takes {})", known.join(", ")),
            ));
        }
        Ok(())
    }
}

/// What kind of JSON `v` is.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// A duration: whole numbers each with a unit, s, m, h or d (`30m`,
/// `1h30m`), in seconds.
pub fn duration(s: &str) -> Option<u64> {
    let mut secs = 0u64;
    let mut n: Option<u64> = None;
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            n = Some(n.unwrap_or(0).checked_mul(10)?.checked_add(u64::from(d))?);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        secs = secs.checked_add(n.take()?.checked_mul(unit)?)?;
    }
    (n.is_none() && !s.is_empty() && secs > 0).then_some(secs)
}

/// What a stage gives its lanes, and a lane gives itself.
#[derive(Clone, Default)]
struct Needs {
    threads: Option<u32>,
    mem_gb: Option<f64>,
    gpus: Option<u32>,
    timeout_secs: Option<u64>,
    tool: Option<String>,
    snapshot: Option<bool>,
    max_running: Option<u32>,
}

impl Needs {
    fn take(o: &mut Obj) -> Result<Self> {
        let timeout_secs = match o.string("timeout")? {
            None => None,
            Some(t) => Some(duration(&t).ok_or_else(|| {
                bad(
                    &o.at("timeout"),
                    format!("{t:?} is no duration (such as 45s, 30m, 3h or 1h30m)"),
                )
            })?),
        };
        let threads = o.u32("threads")?;
        if threads == Some(0) {
            return Err(bad(&o.at("threads"), "is 0; a cell needs a thread"));
        }
        let max_running = o.u32("max_running")?;
        if max_running == Some(0) {
            return Err(bad(&o.at("max_running"), "is 0, so nothing would run"));
        }
        let tool = o.string("tool")?;
        if tool
            .as_deref()
            .is_some_and(|t| t.is_empty() || t.contains('{'))
        {
            return Err(bad(&o.at("tool"), "names no binary"));
        }
        Ok(Self {
            threads,
            mem_gb: o.number("mem_gb")?,
            gpus: o.u32("gpus")?,
            timeout_secs,
            tool,
            snapshot: o.bool("snapshot")?,
            max_running,
        })
    }

    /// `self`, with `stage`'s values where it gives none.
    fn over(self, stage: &Needs) -> Self {
        Self {
            threads: self.threads.or(stage.threads),
            mem_gb: self.mem_gb.or(stage.mem_gb),
            gpus: self.gpus.or(stage.gpus),
            timeout_secs: self.timeout_secs.or(stage.timeout_secs),
            tool: self.tool.or_else(|| stage.tool.clone()),
            snapshot: self.snapshot.or(stage.snapshot),
            max_running: self.max_running.or(stage.max_running),
        }
    }
}

/// A lane's `run` and `makes`, with what it needs.
fn lane(o: &mut Obj, method: Option<String>, needs: Needs) -> Result<Lane> {
    let run = o
        .strings("run")?
        .ok_or_else(|| bad(&o.at("run"), "is required: the command, as a list of words"))?;
    if run.is_empty() || run[0].is_empty() {
        return Err(bad(&o.at("run"), "names no command"));
    }
    if run[0] != "{legume}" && run[0].contains('{') {
        return Err(bad(
            &o.at("run[0]"),
            format!("{:?}: a command is a tool's name or {{legume}}", run[0]),
        ));
    }
    let makes = o
        .strings("makes")?
        .ok_or_else(|| bad(&o.at("makes"), "is required: the files the cell makes"))?;
    if makes.is_empty() {
        return Err(bad(
            &o.at("makes"),
            "is empty; a cell is done when what it makes is there",
        ));
    }
    for (i, m) in makes.iter().enumerate() {
        let p = std::path::Path::new(m);
        if m.is_empty()
            || p.is_absolute()
            || m.contains('{')
            || p.components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(bad(
                &o.at(&format!("makes[{i}]")),
                format!("{m:?} is not a plain path inside the cell's folder"),
            ));
        }
    }
    Ok(Lane {
        method,
        at: o.at.clone(),
        run,
        makes,
        threads: needs.threads.unwrap_or(1),
        mem_gb: needs.mem_gb.unwrap_or(0.0),
        gpus: needs.gpus.unwrap_or(0),
        timeout_secs: needs.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        tool: needs.tool,
        snapshot: needs.snapshot.unwrap_or(true),
        max_running: needs.max_running,
    })
}

fn stage(at: String, v: &Value) -> Result<Stage> {
    let mut o = Obj::new(at, v)?;
    let name = o
        .string("name")?
        .ok_or_else(|| bad(&o.at("name"), "is required"))?;
    if !is_ident(&name) || name.len() > 40 {
        return Err(bad(
            &o.at("name"),
            format!("{name:?} is no stage name (a letter, then letters, digits and _)"),
        ));
    }
    let needs = Needs::take(&mut o)?;
    let methods = o.get("methods");
    let has_run = o.map.contains_key("run") || o.map.contains_key("makes");
    let lanes = match methods {
        None => vec![lane(&mut o, None, needs)?],
        Some(_) if has_run => {
            return Err(bad(
                &o.at,
                "has both run and methods; each method gives its own run and makes",
            ))
        }
        Some(m) => {
            let mo = Obj::new(o.at("methods"), m)?;
            if mo.map.is_empty() {
                return Err(bad(&mo.at, "names no method"));
            }
            let mut lanes = Vec::new();
            for (method, lv) in mo.map {
                let mut lo = Obj::new(mo.at(method), lv)?;
                if !is_method(method) {
                    return Err(bad(
                        &lo.at,
                        format!("{method:?} is no method name (letters, digits, _ and -)"),
                    ));
                }
                let own = Needs::take(&mut lo)?;
                let l = lane(&mut lo, Some(method.clone()), own.over(&needs))?;
                lo.done()?;
                lanes.push(l);
            }
            lanes
        }
    };
    o.done()?;
    Ok(Stage {
        name,
        methods: methods.is_some(),
        lanes,
    })
}

fn axis(at: String, name: &str, v: &Value) -> Result<Axis> {
    if !is_ident(name) || RESERVED.contains(&name) {
        return Err(bad(&at, format!("{name:?} cannot name an axis")));
    }
    let Value::Array(items) = v else {
        return Err(bad(&at, format!("is {}, not a list of values", kind(v))));
    };
    if items.is_empty() {
        return Err(bad(&at, "has no values"));
    }
    let tuple = items[0].is_object();
    let mut values: Vec<AxisValue> = Vec::new();
    for (i, x) in items.iter().enumerate() {
        let first_at = format!("{at}[0]");
        let at = format!("{at}[{i}]");
        let value = match x {
            Value::String(s) if !tuple => {
                if !is_value(s) {
                    return Err(bad(
                        &at,
                        format!("{s:?} is not 1 to 40 of A-Z a-z 0-9 . _ -"),
                    ));
                }
                AxisValue {
                    label: s.clone(),
                    bindings: Vec::new(),
                }
            }
            Value::Object(map) if tuple => tuple_value(&at, map)?,
            Value::Number(n) if !tuple => {
                return Err(bad(
                    &at,
                    format!(
                        "{n} is a number; write it as the string \"{n}\", as argv will read it"
                    ),
                ))
            }
            _ if tuple => return Err(bad(&at, "the first value is an object, so each one is")),
            _ => return Err(bad(&at, format!("is {}, not a string", kind(x)))),
        };
        if values.iter().any(|w| w.label == value.label) {
            return Err(bad(&at, format!("{:?} is listed twice", value.label)));
        }
        if let Some(first) = values.first() {
            let names = |v: &AxisValue| v.bindings.iter().map(|b| b.0.clone()).collect::<Vec<_>>();
            if names(first) != names(&value) {
                return Err(bad(
                    &at,
                    format!(
                        "binds {}, but {first_at} binds {}; each value of an axis binds \
                         the same names",
                        names(&value).join(", "),
                        names(first).join(", "),
                    ),
                ));
            }
        }
        values.push(value);
    }
    Ok(Axis {
        name: name.to_string(),
        values,
        tuple,
    })
}

fn tuple_value(at: &str, map: &Map<String, Value>) -> Result<AxisValue> {
    let label = match map.get("label") {
        Some(Value::String(s)) if is_value(s) => s.clone(),
        Some(Value::String(s)) => {
            return Err(bad(
                &format!("{at}.label"),
                format!("{s:?} is not 1 to 40 of A-Z a-z 0-9 . _ -"),
            ))
        }
        _ => return Err(bad(at, "needs a label, a string")),
    };
    let mut bindings = Vec::new();
    for (k, v) in map.iter().filter(|(k, _)| *k != "label") {
        let at = format!("{at}.{k}");
        if !is_ident(k) || RESERVED.contains(&k.as_str()) {
            return Err(bad(&at, format!("{k:?} cannot name a binding")));
        }
        match v {
            Value::String(s) if is_value(s) => bindings.push((k.clone(), s.clone())),
            Value::String(s) => {
                return Err(bad(
                    &at,
                    format!("{s:?} is not 1 to 40 of A-Z a-z 0-9 . _ -"),
                ))
            }
            _ => return Err(bad(&at, format!("is {}, not a string", kind(v)))),
        }
    }
    if bindings.is_empty() {
        return Err(bad(at, "binds nothing; a plain value would do"));
    }
    Ok(AxisValue { label, bindings })
}

fn seeds(v: Option<&Value>) -> Result<Seeds> {
    let Some(v) = v else {
        return Err(bad(
            "seeds",
            "is required: {\"n\": N, \"base\": B} or {\"list\": [...]}",
        ));
    };
    let mut o = Obj::new("seeds", v)?;
    let list = o.get("list");
    let n = o.u32("n")?;
    let base = o.uint("base")?;
    o.done()?;
    match (list, n) {
        (Some(_), Some(_)) => Err(bad("seeds", "has both n and list; give one")),
        (None, None) => Err(bad("seeds", "needs n (and base) or list")),
        (None, Some(0)) => Err(bad("seeds.n", "is 0, so nothing would run")),
        (None, Some(n)) => Ok(Seeds::Derived {
            n,
            base: base.unwrap_or(0),
        }),
        (Some(_), None) if base.is_some() => Err(bad(
            "seeds.base",
            "is for n; a listed seed is used as it is",
        )),
        (Some(l), None) => {
            let Value::Array(items) = l else {
                return Err(bad("seeds.list", "is not a list"));
            };
            if items.is_empty() {
                return Err(bad("seeds.list", "is empty, so nothing would run"));
            }
            let mut out: Vec<u64> = Vec::new();
            for (i, x) in items.iter().enumerate() {
                let at = format!("seeds.list[{i}]");
                let s = x
                    .as_u64()
                    .filter(|&s| s <= 0x7fff_ffff)
                    .ok_or_else(|| bad(&at, format!("{x} is not a seed from 0 to 2^31 - 1")))?;
                if out.contains(&s) {
                    return Err(bad(&at, format!("{s} is listed twice")));
                }
                out.push(s);
            }
            Ok(Seeds::List(out))
        }
    }
}

fn from_value(v: &Value) -> Result<Spec> {
    let mut o = Obj::new("", v).map_err(|_| refuse("the spec is not a JSON object"))?;
    let name = o
        .string("name")?
        .ok_or_else(|| bad("name", "is required: the benchmark's folder name"))?;
    if !is_value(&name) || name.starts_with('.') {
        return Err(bad(
            "name",
            format!("{name:?} is not 1 to 40 of A-Z a-z 0-9 . _ -"),
        ));
    }
    let cores = o.u32("cores")?;
    if cores == Some(0) {
        return Err(bad("cores", "is 0"));
    }
    let mem_gb = o.number("mem_gb")?;
    let gpus = match o.get("gpus") {
        None => Vec::new(),
        Some(Value::Array(ids)) => {
            let mut out: Vec<u32> = Vec::new();
            for (i, x) in ids.iter().enumerate() {
                let id = x
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| bad(&format!("gpus[{i}]"), format!("{x} is no GPU id")))?;
                if out.contains(&id) {
                    return Err(bad(
                        &format!("gpus[{i}]"),
                        format!("GPU {id} is listed twice"),
                    ));
                }
                out.push(id);
            }
            out
        }
        Some(x) => {
            return Err(bad(
                "gpus",
                format!(
                    "is {}, not a list of GPU ids (a stage's gpus is a count)",
                    kind(x)
                ),
            ))
        }
    };
    let gpu_slots = o.u32("gpu_slots")?.unwrap_or(1);
    if gpu_slots == 0 {
        return Err(bad("gpu_slots", "is 0, so no GPU could be given"));
    }
    let grace_secs = o.uint("grace_secs")?.unwrap_or(10);
    let max_hours = o.number("max_hours")?;
    let min_free_gb = match o.get("disk") {
        None => 10.0,
        Some(d) => {
            let mut d = Obj::new("disk", d)?;
            let f = d.number("min_free_gb")?.unwrap_or(10.0);
            d.done()?;
            f
        }
    };
    if let Some(k) = o.string("keep")? {
        if k != "all" {
            return Err(bad(
                "keep",
                format!("{k:?}: only \"all\" is built (pruning is deferred)"),
            ));
        }
    }
    let mut axes = Vec::new();
    if let Some(a) = o.get("axes") {
        let ao = Obj::new("axes", a)?;
        for (name, v) in ao.map {
            axes.push(axis(ao.at(name), name, v)?);
        }
    }
    // Bindings are placeholders beside the axes: no name twice.
    let mut names: Vec<(&str, String)> = Vec::new();
    for a in &axes {
        names.push((&a.name, format!("axes.{}", a.name)));
        for (b, _) in a.values.first().map_or(&[][..], |v| &v.bindings) {
            names.push((b, format!("axes.{}[0].{b}", a.name)));
        }
    }
    for (i, (n, at)) in names.iter().enumerate() {
        if let Some((_, first)) = names[..i].iter().find(|(m, _)| m == n) {
            return Err(bad(
                at,
                format!("{n:?} is already a placeholder, at {first}"),
            ));
        }
    }
    let exclude = match o.get("exclude") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, x)| excluded(&format!("exclude[{i}]"), x, &axes))
            .collect::<Result<_>>()?,
        Some(x) => {
            return Err(bad(
                "exclude",
                format!("is {}, not a list of points", kind(x)),
            ))
        }
    };
    let seeds = seeds(o.get("seeds"))?;
    let mut inputs = Vec::new();
    if let Some(i) = o.get("inputs") {
        let io = Obj::new("inputs", i)?;
        for (name, v) in io.map {
            let mut x = Obj::new(io.at(name), v)?;
            if !is_ident(name) {
                return Err(bad(&x.at, format!("{name:?} cannot name an input")));
            }
            let path = x
                .string("path")?
                .filter(|p| !p.is_empty())
                .ok_or_else(|| bad(&x.at("path"), "is required"))?;
            let dir = x.bool("dir")?.unwrap_or(false);
            x.done()?;
            inputs.push(Input {
                name: name.clone(),
                path,
                dir,
            });
        }
    }
    let stages = match o.get("stages") {
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .enumerate()
            .map(|(i, s)| stage(format!("stages[{i}]"), s))
            .collect::<Result<Vec<_>>>()?,
        Some(Value::Array(_)) => return Err(bad("stages", "is empty")),
        Some(x) => return Err(bad("stages", format!("is {}, not a list", kind(x)))),
        None => return Err(bad("stages", "is required")),
    };
    for (i, s) in stages.iter().enumerate() {
        let at = format!("stages[{i}].name");
        if RESERVED.contains(&s.name.as_str()) {
            return Err(bad(
                &at,
                format!("{:?} is a placeholder every cell has", s.name),
            ));
        }
        if let Some((_, first)) = names.iter().find(|(n, _)| *n == s.name) {
            return Err(bad(
                &at,
                format!("{:?} is already a placeholder, at {first}", s.name),
            ));
        }
        if let Some(j) = stages[..i].iter().position(|t| t.name == s.name) {
            return Err(bad(&at, format!("{:?} is also stages[{j}]", s.name)));
        }
    }
    let lanes: Vec<&str> = stages
        .iter()
        .flat_map(|s| s.lanes.iter().filter_map(|l| l.method.as_deref()))
        .collect();
    let compare = match o.get("compare") {
        None => Vec::new(),
        Some(Value::Array(pairs)) => pairs
            .iter()
            .enumerate()
            .map(|(i, p)| compared(&format!("compare[{i}]"), p, &lanes))
            .collect::<Result<_>>()?,
        Some(x) => {
            return Err(bad(
                "compare",
                format!("is {}, not a list of pairs", kind(x)),
            ))
        }
    };
    let plot = match o.get("plot") {
        None => None,
        Some(p) => {
            let mut po = Obj::new("plot", p)?;
            let x = po
                .string("x")?
                .ok_or_else(|| bad("plot.x", "is required"))?;
            let facet = po.string("facet")?;
            let metrics = po.strings("metrics")?.unwrap_or_default();
            po.done()?;
            for (k, a) in [("x", Some(&x)), ("facet", facet.as_ref())] {
                if let Some(a) = a.filter(|a| !axes.iter().any(|ax| &ax.name == *a)) {
                    return Err(bad(&format!("plot.{k}"), format!("{a:?} is no axis")));
                }
            }
            Some(Plot { x, facet, metrics })
        }
    };
    o.done()?;
    Ok(Spec {
        name,
        cores,
        mem_gb,
        gpus,
        gpu_slots,
        grace_secs,
        max_hours,
        min_free_gb,
        axes,
        exclude,
        seeds,
        inputs,
        stages,
        compare,
        plot,
    })
}

/// An `exclude` entry: axis to value (a tuple axis's label), each one the
/// axis has.
fn excluded(at: &str, v: &Value, axes: &[Axis]) -> Result<Vec<(String, String)>> {
    let Value::Object(map) = v else {
        return Err(bad(at, format!("is {}, not a partial point", kind(v))));
    };
    if map.is_empty() {
        return Err(bad(at, "is empty, so it would drop every point"));
    }
    map.iter()
        .map(|(k, x)| {
            let at = format!("{at}.{k}");
            let axis = axes
                .iter()
                .find(|a| &a.name == k)
                .ok_or_else(|| bad(&at, format!("{k:?} is no axis")))?;
            let label = x.as_str().unwrap_or_default();
            if !axis.values.iter().any(|v| v.label == label) {
                return Err(bad(&at, format!("{x} is no value of axis {k}")));
            }
            Ok((k.clone(), label.to_string()))
        })
        .collect()
}

/// A `compare` pair: two `method/score` names, each method a lane.
fn compared(at: &str, v: &Value, lanes: &[&str]) -> Result<[String; 2]> {
    let pair = match v {
        Value::Array(p) if p.len() == 2 => [&p[0], &p[1]],
        _ => return Err(bad(at, "is not a pair [\"M1/score\", \"M0/score\"]")),
    };
    let mut out = [String::new(), String::new()];
    for (i, x) in pair.into_iter().enumerate() {
        let at = format!("{at}[{i}]");
        let s = x.as_str().ok_or_else(|| bad(&at, "is not a string"))?;
        match s.split_once('/') {
            Some((m, sc)) if lanes.contains(&m) && !sc.is_empty() => out[i] = s.to_string(),
            Some((m, _)) if !lanes.contains(&m) => {
                return Err(bad(&at, format!("{m:?} is no method of any stage")))
            }
            _ => return Err(bad(&at, format!("{s:?} is not method/score"))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::code;

    fn refused(text: &str) -> String {
        let e = parse(text).unwrap_err();
        assert_eq!(code(&e), 3, "{e}");
        e.to_string()
    }

    const ONE: &str = r#"{"name": "N1", "seeds": {"n": 2},
        "stages": [{"name": "sim", "run": ["TOOL1"], "makes": ["m.score.tsv"]}]"#;

    fn with(extra: &str) -> String {
        format!("{ONE}, {extra}}}")
    }

    #[test]
    fn a_small_spec_takes_its_defaults() {
        let (s, _) = parse(&format!("{ONE}}}")).unwrap();
        assert_eq!(s.name, "N1");
        assert!(matches!(s.seeds, Seeds::Derived { n: 2, base: 0 }));
        let l = &s.stages[0].lanes[0];
        assert_eq!(
            (l.threads, l.mem_gb, l.gpus, l.timeout_secs, l.snapshot),
            (1, 0.0, 0, DEFAULT_TIMEOUT_SECS, true)
        );
        assert_eq!(l.tool_word(), "TOOL1");
        assert_eq!((s.gpu_slots, s.grace_secs, s.min_free_gb), (1, 10, 10.0));
    }

    #[test]
    fn durations_take_units_and_nothing_else() {
        assert_eq!(duration("30m"), Some(1800));
        assert_eq!(duration("1h30m"), Some(5400));
        assert_eq!(duration("2d"), Some(172_800));
        assert_eq!(duration("45s"), Some(45));
        for bad in ["", "30", "m", "0s", "3 h", "1.5h", "h3"] {
            assert_eq!(duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn lanes_take_the_stage_values_they_do_not_override() {
        let (s, _) = parse(
            r#"{"name": "N1", "seeds": {"list": [7, 3]},
            "stages": [{"name": "fit", "threads": 8, "mem_gb": 12, "timeout": "3h",
              "methods": {
                "M1": {"run": ["TOOL1"], "makes": ["a"]},
                "M2": {"threads": 4, "gpus": 1, "tool": "TOOL2", "snapshot": false,
                       "timeout": "10m", "max_running": 2, "run": ["TOOL2"], "makes": ["b"]}}}]}"#,
        )
        .unwrap();
        assert!(matches!(&s.seeds, Seeds::List(l) if l == &[7, 3]));
        let st = &s.stages[0];
        assert!(st.methods);
        let (m1, m2) = (&st.lanes[0], &st.lanes[1]);
        assert_eq!(m1.method.as_deref(), Some("M1"));
        assert_eq!(
            (m1.threads, m1.mem_gb, m1.gpus, m1.timeout_secs),
            (8, 12.0, 0, 10_800)
        );
        assert_eq!(
            (m2.threads, m2.mem_gb, m2.gpus, m2.timeout_secs),
            (4, 12.0, 1, 600)
        );
        assert_eq!(
            (m2.tool_word(), m2.snapshot, m2.max_running),
            ("TOOL2", false, Some(2))
        );
        assert_eq!(m2.at, "stages[0].methods.M2");
    }

    // tuple_axis_binds_all_fields (parse side): a tuple axis's values each
    // bind the same names, and those names are placeholders of their own.
    #[test]
    fn tuple_axes_bind_the_same_names_in_every_value() {
        let (s, _) = parse(&with(
            r#""axes": {"DS": [{"label": "L1", "p1": "0", "p2": "0.5"},
                               {"label": "L2", "p1": "0.01", "p2": "0.02"}],
                        "K": ["new-state", "new-mix"]}"#,
        ))
        .unwrap();
        let ds = s.axes.iter().find(|a| a.name == "DS").unwrap();
        assert!(ds.tuple);
        assert_eq!(ds.values[1].label, "L2");
        assert_eq!(
            ds.values[1].bindings,
            [("p1".into(), "0.01".into()), ("p2".into(), "0.02".into())]
        );
        let e = refused(&with(
            r#""axes": {"DS": [{"label": "L1", "p1": "0", "p2": "0"}, {"label": "L2", "p1": "0"}]}"#,
        ));
        assert_eq!(
            e,
            "axes.DS[1]: binds p1, but axes.DS[0] binds p1, p2; \
             each value of an axis binds the same names"
        );
        let e = refused(&with(
            r#""axes": {"DS": [{"label": "L1", "K": "0"}], "K": ["1"]}"#,
        ));
        assert_eq!(e, "axes.K: \"K\" is already a placeholder, at axes.DS[0].K");
    }

    #[test]
    fn refusals_say_what_is_wrong_and_where() {
        let cases: &[(&str, &str)] = &[
            (r#""axes": {"pve": [0.1]}"#,
             "axes.pve[0]: 0.1 is a number; write it as the string \"0.1\", as argv will read it"),
            (r#""axes": {"pve": ["a b"]}"#, "axes.pve[0]: \"a b\" is not 1 to 40 of A-Z a-z 0-9 . _ -"),
            (r#""axes": {"seed": ["1"]}"#, "axes.seed: \"seed\" cannot name an axis"),
            (r#""axes": {"A": ["1", "1"]}"#, "axes.A[1]: \"1\" is listed twice"),
            (r#""axes": {"A": ["1"]}, "exclude": [{"A": "2"}]"#, "exclude[0].A: \"2\" is no value of axis A"),
            (r#""axes": {"A": ["1"]}, "exclude": [{"B": "1"}]"#, "exclude[0].B: \"B\" is no axis"),
            (r#""exclude": [{}]"#, "exclude[0]: is empty, so it would drop every point"),
            (r#""gpus": 2"#, "gpus: is a number, not a list of GPU ids (a stage's gpus is a count)"),
            (r#""keep": "until-done""#, "keep: \"until-done\": only \"all\" is built (pruning is deferred)"),
            (r#""bogus": 1"#, "bogus: is no field here (it takes axes, compare, cores, disk, \
              exclude, gpu_slots, gpus, grace_secs, inputs, keep, max_hours, mem_gb, name, plot, seeds, stages)"),
            (r#""inputs": {"RUN1": {"dir": true}}"#, "inputs.RUN1.path: is required"),
            (r#""plot": {"x": "A"}"#, "plot.x: \"A\" is no axis"),
            (r#""compare": [["M1/s", "M0/s"]]"#, "compare[0][0]: \"M1\" is no method of any stage"),
        ];
        for (extra, want) in cases {
            assert_eq!(refused(&with(extra)), *want, "{extra}");
        }
        let stages: &[(&str, &str)] = &[
            (r#"{"name": "a-b", "run": ["T"], "makes": ["m"]}"#,
             "stages[0].name: \"a-b\" is no stage name (a letter, then letters, digits and _)"),
            (r#"{"name": "s", "run": ["T"]}"#, "stages[0].makes: is required: the files the cell makes"),
            (r#"{"name": "s", "run": [], "makes": ["m"]}"#, "stages[0].run: names no command"),
            (r#"{"name": "s", "run": ["{inputs.X}"], "makes": ["m"]}"#,
             "stages[0].run[0]: \"{inputs.X}\": a command is a tool's name or {legume}"),
            (r#"{"name": "s", "run": ["T"], "makes": ["../m"]}"#,
             "stages[0].makes[0]: \"../m\" is not a plain path inside the cell's folder"),
            (r#"{"name": "s", "timeout": "30 min", "run": ["T"], "makes": ["m"]}"#,
             "stages[0].timeout: \"30 min\" is no duration (such as 45s, 30m, 3h or 1h30m)"),
            (r#"{"name": "s", "threads": 0, "run": ["T"], "makes": ["m"]}"#,
             "stages[0].threads: is 0; a cell needs a thread"),
            (r#"{"name": "s", "run": ["T"], "makes": ["m"], "methods": {"M1": {}}}"#,
             "stages[0]: has both run and methods; each method gives its own run and makes"),
            (r#"{"name": "s", "methods": {"M1": {"run": ["T"], "makes": ["m"], "thread": 2}}}"#,
             "stages[0].methods.M1.thread: is no field here (it takes gpus, makes, max_running, \
              mem_gb, run, snapshot, threads, timeout, tool)"),
            (r#"{"name": "s", "methods": {"M.1": {"run": ["T"], "makes": ["m"]}}}"#,
             "stages[0].methods.M.1: \"M.1\" is no method name (letters, digits, _ and -)"),
            (r#"{"name": "out", "run": ["T"], "makes": ["m"]}"#,
             "stages[0].name: \"out\" is a placeholder every cell has"),
        ];
        for (st, want) in stages {
            let text = format!(r#"{{"name": "N1", "seeds": {{"n": 1}}, "stages": [{st}]}}"#);
            assert_eq!(refused(&text), *want, "{st}");
        }
        let seeds: &[(&str, &str)] = &[
            (r#"{"n": 0}"#, "seeds.n: is 0, so nothing would run"),
            (
                r#"{"n": 2, "list": [1]}"#,
                "seeds: has both n and list; give one",
            ),
            (r#"{"list": [1, 1]}"#, "seeds.list[1]: 1 is listed twice"),
            (
                r#"{"list": [2147483648]}"#,
                "seeds.list[0]: 2147483648 is not a seed from 0 to 2^31 - 1",
            ),
            (
                r#"{"list": [1], "base": 3}"#,
                "seeds.base: is for n; a listed seed is used as it is",
            ),
        ];
        for (sd, want) in seeds {
            let text = format!(
                r#"{{"name": "N1", "seeds": {sd}, "stages": [{{"name": "s", "run": ["T"], "makes": ["m"]}}]}}"#
            );
            assert_eq!(refused(&text), *want, "{sd}");
        }
        assert!(refused("{").starts_with("the spec is not JSON: "));
        assert_eq!(
            refused(r#"{"name": "N1", "stages": []}"#),
            "seeds: is required: {\"n\": N, \"base\": B} or {\"list\": [...]}"
        );
    }
}
