//! `legume benchmark plan SPEC --edit`: a spec shaped in the terminal
//! rather than written as JSON by hand. This is the editor without its
//! screen ([`super::edit_view`] draws it, feature `tui`): the spec as
//! JSON, the rows a cursor moves over, the edits, what the spec plans and
//! what it refuses, and the diff a save shows first.
//!
//! - **Rows.** The name, the cores and memory it books, the seeds (n and
//!   base), each axis and its values, the excluded points, and each stage
//!   and method with its threads, mem_gb and timeout, as the file writes
//!   them. A value toggled off is an excluded point of that value alone,
//!   so the spec says it.
//! - **Counts.** From the spec alone, after every edit: its points, reps
//!   and tasks for each stage and method, the cores and memory it books,
//!   and its first refusal ([`spec::parse`], [`plan::check`], and the
//!   threads and memory against what it books), with the field it names.
//!   A spec is often edited here and run on a cluster, so nothing is
//!   looked up on PATH, no tool is snapshot and no plan is written.
//! - **Save.** Only the spec's file, as pretty JSON with its keys sorted,
//!   after a diff of the old and new canonical JSON. The `run` and `makes`
//!   lists stay as the file has them: they are edited in the file.

use super::{key, machine, plan, spec, tools};
use crate::command::refuse;
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// What an edit said: what it did, or why it did nothing.
pub type Said = std::result::Result<String, String>;

/// A row of the editor: one thing the cursor can stand on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Name,
    Cores,
    MemGb,
    SeedsN,
    SeedsBase,
    /// The axes, as a heading: an axis is added here.
    Axes,
    Axis(String),
    /// An axis's value, by its place in the axis.
    Value {
        axis: String,
        index: usize,
    },
    /// The excluded points, as a heading: a point is added here.
    Exclude,
    /// An `exclude` entry, by its place.
    Excluded(usize),
    /// The stages, as a heading.
    Stages,
    Stage(usize),
    Method {
        stage: usize,
        name: String,
    },
}

/// What a stage or a method needs, as the editor sets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Threads,
    MemGb,
    Timeout,
}

impl Need {
    /// Its field in the spec.
    pub fn field(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::MemGb => "mem_gb",
            Self::Timeout => "timeout",
        }
    }
}

/// A field the person sets by typing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Cores,
    MemGb,
    SeedsN,
    SeedsBase,
    /// What stage `stage` needs, or its method `method`.
    Need {
        stage: usize,
        method: Option<String>,
        need: Need,
    },
}

/// What a typed answer is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Set(Field),
    /// Values to add to this axis.
    AddValues(String),
    /// A new axis: its name and values.
    AddAxis,
    /// A new excluded point: axis=value pairs.
    AddExcluded,
}

/// What the person asks of the row the cursor is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// The row's own field.
    Edit,
    /// A stage's or method's need.
    Set(Need),
    /// Something new where the cursor is: a value, an axis, a point.
    Add,
}

/// A row as the screen shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct RowLine {
    /// How far in, in steps.
    pub depth: u16,
    pub text: String,
    /// An axis value toggled off.
    pub off: bool,
    /// A heading.
    pub head: bool,
}

/// What the spec plans, counted from the spec alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Counts {
    /// The grid's points before `exclude`.
    pub grid: usize,
    pub points: usize,
    pub reps: usize,
    /// Each stage's methods (a stage without methods, as one).
    pub methods: Vec<MethodCount>,
    pub tasks: usize,
    /// The cores it books; the machine's when not given.
    pub cores: Option<u32>,
    /// The memory it books, GB; MemAvailable where it plans when not given.
    pub mem_gb: Option<f64>,
    /// Every task's threads times its timeout, in hours: what it books at
    /// most.
    pub core_hours: f64,
    /// The spec's key, of its canonical JSON.
    pub key: String,
}

/// One stage's method: its tasks and what each needs.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodCount {
    /// `<stage>-<method>`, or the stage's name.
    pub name: String,
    pub tasks: usize,
    pub threads: u32,
    pub mem_gb: f64,
    pub timeout_secs: u64,
    pub gpus: u32,
    /// The most of its tasks that run at once, when it says.
    pub max_running: Option<u32>,
    /// The word naming its tool ([`spec::Method::tool_word`]).
    pub tool: String,
}

/// What the spec, as it stands, plans and refuses.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Report {
    /// None when the spec cannot be read as one.
    pub counts: Option<Counts>,
    /// The first refusal, which names its field (`stages[0].threads: ...`).
    pub refusal: Option<String>,
}

impl Report {
    /// The field the refusal names: what comes before its first `: `.
    pub fn refused_at(&self) -> Option<&str> {
        let r = self.refusal.as_deref()?;
        r.split_once(": ").map(|(at, _)| at)
    }
}

/// A line of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Same,
    Gone,
    New,
}

/// A line of a diff, marked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub mark: Mark,
    pub text: String,
}

/// The editor: the spec being shaped, and what it was when opened or
/// last saved.
pub struct Editor {
    path: PathBuf,
    /// The spec as the file has it; None for a file not there yet.
    saved: Option<Value>,
    /// The file's text is laid out otherwise than a save writes it.
    relaid: bool,
    value: Value,
    rows: Vec<Row>,
    cursor: usize,
    report: Report,
    /// The counts of the last spec that could be read, kept while a typo
    /// makes it unreadable.
    last: Option<Counts>,
}

impl Editor {
    /// The spec at `path`, or a new one from a small template when there is
    /// no file. Refused: a file that is not a JSON object, and a spec
    /// under a benchmark's `specs/`, which is named by its content.
    pub fn open(path: &Path) -> Result<Self> {
        let in_specs = path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|d| d == "specs");
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        if in_specs && stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(refuse(format!(
                "{}: a spec under specs/ is named by its content and not edited in place; \
                 edit a copy",
                path.display()
            )));
        }
        let (value, saved, relaid) = match std::fs::read_to_string(path) {
            Ok(text) => {
                let v: Value = serde_json::from_str(&text)
                    .map_err(|e| refuse(format!("{}: is not JSON ({e})", path.display())))?;
                if !v.is_object() {
                    return Err(refuse(format!(
                        "{}: is not a JSON object, as a spec is",
                        path.display()
                    )));
                }
                let relaid = crate::pretty(&v)? != text;
                (v.clone(), Some(v), relaid)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (template(path), None, false),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut ed = Self {
            path: path.to_path_buf(),
            saved,
            relaid,
            value,
            rows: Vec::new(),
            cursor: 0,
            report: Report::default(),
            last: None,
        };
        ed.refresh();
        Ok(ed)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file is not there yet.
    pub fn is_new(&self) -> bool {
        self.saved.is_none()
    }

    /// The spec as it stands.
    pub fn value(&self) -> &Value {
        &self.value
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The row the cursor is on.
    pub fn row(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    /// The cursor moved by `delta` rows, kept on the rows.
    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// The cursor on row `i`, or the last.
    pub fn move_to(&mut self, i: usize) {
        self.cursor = i.min(self.rows.len().saturating_sub(1));
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// The counts of the spec now, or, while it cannot be read, of the
    /// last one that could; whether they are of now.
    pub fn counts(&self) -> Option<(&Counts, bool)> {
        match &self.report.counts {
            Some(c) => Some((c, true)),
            None => self.last.as_ref().map(|c| (c, false)),
        }
    }

    /// The spec differs from the file's (a new file always does).
    pub fn changed(&self) -> bool {
        self.saved
            .as_ref()
            .is_none_or(|s| key::canonical(s) != key::canonical(&self.value))
    }

    /// A save writes the file in another layout than it has.
    pub fn relaid(&self) -> bool {
        self.relaid
    }

    /// The row the refusal is about: the one whose field is the longest
    /// that the refusal's field is, or is inside.
    pub fn refused_row(&self) -> Option<usize> {
        let at = self.report.refused_at()?;
        let inside = |p: &str| {
            at.strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '[']))
        };
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| row_path(r).filter(|p| inside(p)).map(|p| (p.len(), i)))
            .max()
            .map(|(_, i)| i)
    }

    /// Row `row` as the screen shows it.
    pub fn line(&self, row: &Row) -> RowLine {
        let v = &self.value;
        let shown = |x: Option<&Value>| x.map_or("-".to_string(), text);
        let line = |depth, text: String| RowLine {
            depth,
            text,
            off: false,
            head: false,
        };
        let head = |text: &str| RowLine {
            depth: 0,
            text: text.to_string(),
            off: false,
            head: true,
        };
        match row {
            Row::Name => line(0, format!("name        {}", shown(v.get("name")))),
            Row::Cores => line(
                0,
                match v.get("cores") {
                    Some(c) => format!("cores       {}", text(c)),
                    None => "cores       - (the machine's logical CPUs)".into(),
                },
            ),
            Row::MemGb => line(
                0,
                match v.get("mem_gb") {
                    Some(m) => format!("mem_gb      {}", text(m)),
                    None => "mem_gb      - (MemAvailable where it plans)".into(),
                },
            ),
            Row::SeedsN => {
                let seeds = v.get("seeds");
                match seeds.and_then(|s| s.get("list")) {
                    Some(Value::Array(l)) => {
                        let l: Vec<String> = l.iter().map(text).collect();
                        line(0, format!("seeds       list {}", l.join(", ")))
                    }
                    _ => line(
                        0,
                        format!("seeds n     {}", shown(seeds.and_then(|s| s.get("n")))),
                    ),
                }
            }
            Row::SeedsBase => line(
                0,
                format!(
                    "seeds base  {}",
                    shown(v.get("seeds").and_then(|s| s.get("base")))
                ),
            ),
            Row::Axes => head("axes"),
            Row::Axis(a) => {
                let values = self.axis(a);
                let tuple = values.first().is_some_and(Value::is_object);
                let n = values.len();
                let kind = if tuple { "tuple, " } else { "" };
                line(1, format!("{a}  ({kind}{n} value{})", plural(n)))
            }
            Row::Value { axis, index } => {
                let x = self.axis(axis).get(*index).cloned().unwrap_or(Value::Null);
                let label = value_label(&x);
                let off = self.excluded_alone(axis, &label).is_some();
                let mut t = format!("[{}] {label}", if off { ' ' } else { 'x' });
                if let Value::Object(m) = &x {
                    for (k, b) in m.iter().filter(|(k, _)| *k != "label") {
                        t += &format!("  {k}={}", text(b));
                    }
                }
                RowLine {
                    depth: 2,
                    text: t,
                    off,
                    head: false,
                }
            }
            Row::Exclude => {
                let n = self.excludes().len();
                head(&format!("exclude  ({n} point{})", plural(n)))
            }
            Row::Excluded(i) => {
                let entry = self.excludes().get(*i).cloned().unwrap_or(Value::Null);
                let t = match &entry {
                    Value::Object(m) => {
                        let pairs: Vec<String> =
                            m.iter().map(|(k, x)| format!("{k}={}", text(x))).collect();
                        pairs.join(" ")
                    }
                    other => other.to_string(),
                };
                line(1, t)
            }
            Row::Stages => head("stages  (run and makes: in the file)"),
            Row::Stage(i) => {
                let s = self.stage(*i);
                let name = s.and_then(|s| s.get("name")).map_or("?".into(), text);
                line(1, format!("{name:<12}{}", needs_text(s)))
            }
            Row::Method { stage, name } => {
                let m = self.method(*stage, name);
                line(2, format!("{name:<10}{}", needs_text(m)))
            }
        }
    }

    /// The text field `field` has now, to start an answer from.
    pub fn current(&self, field: &Field) -> String {
        let (holder, k) = match field {
            Field::Name => (Some(&self.value), "name"),
            Field::Cores => (Some(&self.value), "cores"),
            Field::MemGb => (Some(&self.value), "mem_gb"),
            Field::SeedsN => (self.value.get("seeds"), "n"),
            Field::SeedsBase => (self.value.get("seeds"), "base"),
            Field::Need {
                stage,
                method,
                need,
            } => {
                let holder = match method {
                    Some(m) => self.method(*stage, m),
                    None => self.stage(*stage),
                };
                (holder, need.field())
            }
        };
        holder.and_then(|h| h.get(k)).map_or(String::new(), text)
    }

    /// What `want` asks on the cursor's row, and the answer to start from;
    /// None when the row has no such thing.
    pub fn ask(&self, want: Want) -> Option<(Ask, String)> {
        let row = self.row()?;
        let field = match (want, row) {
            (Want::Edit, Row::Name) => Field::Name,
            (Want::Edit, Row::Cores) => Field::Cores,
            (Want::Edit, Row::MemGb) => Field::MemGb,
            (Want::Edit, Row::SeedsN) => Field::SeedsN,
            (Want::Edit, Row::SeedsBase) => Field::SeedsBase,
            (Want::Edit, Row::Stage(_) | Row::Method { .. }) => {
                return self.ask(Want::Set(Need::Threads))
            }
            (Want::Set(need), Row::Stage(i)) => Field::Need {
                stage: *i,
                method: None,
                need,
            },
            (Want::Set(need), Row::Method { stage, name }) => Field::Need {
                stage: *stage,
                method: Some(name.clone()),
                need,
            },
            (Want::Add, Row::Axes) => return Some((Ask::AddAxis, String::new())),
            (Want::Add, Row::Axis(a) | Row::Value { axis: a, .. }) => {
                return Some((Ask::AddValues(a.clone()), String::new()))
            }
            (Want::Add, Row::Exclude | Row::Excluded(_)) => {
                return Some((Ask::AddExcluded, String::new()))
            }
            _ => return None,
        };
        let now = self.current(&field);
        Some((Ask::Set(field), now))
    }

    /// The prompt for `ask`.
    pub fn prompt(&self, ask: &Ask) -> String {
        match ask {
            Ask::Set(Field::Name) => "name".into(),
            Ask::Set(Field::Cores) => "cores (empty: the machine's)".into(),
            Ask::Set(Field::MemGb) => "mem_gb (empty: MemAvailable)".into(),
            Ask::Set(Field::SeedsN) => "seeds n".into(),
            Ask::Set(Field::SeedsBase) => "seeds base".into(),
            Ask::Set(Field::Need {
                stage,
                method,
                need,
            }) => {
                let whose = match method {
                    Some(m) => format!("method {m}"),
                    None => format!("stages[{stage}]"),
                };
                let hint = match need {
                    Need::Timeout => format!(
                        " (such as 30m or 1h30m; empty: the stage's, or {})",
                        duration_text(spec::DEFAULT_TIMEOUT_SECS)
                    ),
                    _ => " (empty: the stage's, or the default)".to_string(),
                };
                format!("{} of {whose}{hint}", need.field())
            }
            Ask::AddValues(a) => format!("values to add to axis {a}, space between"),
            Ask::AddAxis => "a new axis: NAME VALUE ...".into(),
            Ask::AddExcluded => "a point to exclude: AXIS=VALUE ...".into(),
        }
    }

    /// The answer `typed` to `ask` taken in.
    pub fn answer(&mut self, ask: &Ask, typed: &str) -> Said {
        let typed = typed.trim();
        let said = match ask {
            Ask::Set(field) => self.set(field, typed),
            Ask::AddValues(a) => self.add_values(a, typed),
            Ask::AddAxis => self.add_axis(typed),
            Ask::AddExcluded => self.add_excluded(typed),
        };
        self.refresh();
        said
    }

    /// The value under the cursor toggled: off is an excluded point of it
    /// alone.
    pub fn toggle(&mut self) -> Said {
        let Some(Row::Value { axis, index }) = self.row().cloned() else {
            return Err("Space toggles an axis value".into());
        };
        let label = value_label(&self.axis(&axis)[index]);
        let said = match self.excluded_alone(&axis, &label) {
            Some(i) => {
                self.excludes_mut().remove(i);
                format!("{axis}={label} on")
            }
            None => {
                self.excludes_mut()
                    .push(json!({ axis.clone(): label.clone() }));
                format!("{axis}={label} off")
            }
        };
        self.tidy();
        self.refresh();
        Ok(said)
    }

    /// The value, axis or excluded point under the cursor removed. A
    /// removed value or axis takes the excluded points that name it.
    pub fn remove(&mut self) -> Said {
        let said = match self.row().cloned() {
            Some(Row::Value { axis, index }) => {
                if self.axis(&axis).len() == 1 {
                    return Err(format!(
                        "an axis has a value; d on axis {axis} removes the axis"
                    ));
                }
                let gone = self.axes_mut()[&axis]
                    .as_array_mut()
                    .map(|l| l.remove(index))
                    .unwrap_or(Value::Null);
                let label = value_label(&gone);
                let dropped =
                    self.drop_excluded(|e| e.get(&axis).is_some_and(|x| text(x) == label));
                format!("removed {axis}={label}{}", dropped_text(dropped))
            }
            Some(Row::Axis(a)) => {
                self.axes_mut().remove(&a);
                let dropped = self.drop_excluded(|e| e.get(&a).is_some());
                format!("removed axis {a}{}", dropped_text(dropped))
            }
            Some(Row::Excluded(i)) => {
                self.excludes_mut().remove(i);
                "removed the excluded point".to_string()
            }
            _ => return Err("d removes a value, an axis or an excluded point".into()),
        };
        self.tidy();
        self.refresh();
        Ok(said)
    }

    /// The diff a save shows: the file's spec, then this one, as canonical
    /// pretty JSON.
    pub fn diff(&self) -> Vec<DiffLine> {
        let old = self.saved.as_ref().map_or(String::new(), pretty);
        diff(&old, &pretty(&self.value))
    }

    /// The spec key of the file's spec, if there is one, and of this one.
    pub fn keys(&self) -> (Option<String>, String) {
        let k = key::spec_key;
        (self.saved.as_ref().map(k), k(&self.value))
    }

    /// The spec written to its file, whole, as pretty JSON.
    pub fn save(&mut self) -> Said {
        let dir = match self.path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        crate::job::replace(&dir, &name, pretty(&self.value).as_bytes())
            .map_err(|e| format!("not saved: writing {}: {e}", self.path.display()))?;
        self.saved = Some(self.value.clone());
        self.relaid = false;
        let (_, k) = self.keys();
        Ok(format!(
            "saved {}, spec {}",
            self.path.display(),
            tools::short(&k)
        ))
    }

    /// The rows and the report made again after an edit.
    fn refresh(&mut self) {
        self.rows = rows(&self.value);
        self.move_to(self.cursor);
        self.report = report(&self.value);
        if let Some(c) = &self.report.counts {
            self.last = Some(c.clone());
        }
    }

    /// The root object, which a spec always is here.
    fn root_mut(&mut self) -> &mut Map<String, Value> {
        if !self.value.is_object() {
            self.value = json!({});
        }
        self.value.as_object_mut().expect("an object")
    }

    /// Axis `a`'s values, as the spec lists them.
    fn axis(&self, a: &str) -> &[Value] {
        self.value
            .get("axes")
            .and_then(|x| x.get(a))
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// The axes, made an object when they are not one.
    fn axes_mut(&mut self) -> &mut Map<String, Value> {
        object_at(self.root_mut(), "axes")
    }

    fn excludes(&self) -> &[Value] {
        self.value
            .get("exclude")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// The `exclude` list, made a list when it is not one.
    fn excludes_mut(&mut self) -> &mut Vec<Value> {
        let root = self.root_mut();
        if !root.get("exclude").is_some_and(Value::is_array) {
            root.insert("exclude".into(), json!([]));
        }
        root["exclude"].as_array_mut().expect("a list")
    }

    /// The place of the excluded point of `axis`=`label` alone.
    fn excluded_alone(&self, axis: &str, label: &str) -> Option<usize> {
        self.excludes().iter().position(|e| {
            e.as_object()
                .is_some_and(|m| m.len() == 1 && m.get(axis).is_some_and(|x| text(x) == label))
        })
    }

    /// The excluded points `gone` picks removed; how many.
    fn drop_excluded(&mut self, gone: impl Fn(&Value) -> bool) -> usize {
        let before = self.excludes().len();
        if before == 0 {
            return 0;
        }
        self.excludes_mut().retain(|e| !gone(e));
        before - self.excludes().len()
    }

    /// No empty `exclude` or `axes` left behind: a spec without them is
    /// the same spec, and keys the same.
    fn tidy(&mut self) {
        let root = self.root_mut();
        for k in ["exclude", "axes"] {
            let empty = match root.get(k) {
                Some(Value::Array(l)) => l.is_empty(),
                Some(Value::Object(m)) => m.is_empty(),
                _ => false,
            };
            if empty {
                root.remove(k);
            }
        }
    }

    /// Stage `i`, as the spec has it.
    fn stage(&self, i: usize) -> Option<&Value> {
        self.value.get("stages").and_then(|s| s.get(i))
    }

    /// Method `m` of stage `i`, as the spec has it.
    fn method(&self, i: usize, m: &str) -> Option<&Value> {
        self.stage(i)
            .and_then(|s| s.get("methods"))
            .and_then(|ms| ms.get(m))
    }

    /// `field` set to `typed`; empty takes it out.
    fn set(&mut self, field: &Field, typed: &str) -> Said {
        let as_text = matches!(
            field,
            Field::Name
                | Field::Need {
                    need: Need::Timeout,
                    ..
                }
        );
        let new = (!typed.is_empty()).then(|| typed_value(typed, as_text));
        let (holder, k): (&mut Map<String, Value>, &str) = match field {
            Field::Name => (self.root_mut(), "name"),
            Field::Cores => (self.root_mut(), "cores"),
            Field::MemGb => (self.root_mut(), "mem_gb"),
            Field::SeedsN | Field::SeedsBase => {
                let seeds = object_at(self.root_mut(), "seeds");
                // A list of seeds is replaced only by a value given for n
                // or base, and said so; nothing typed keeps it.
                if let Some(Value::Array(l)) = seeds.get("list") {
                    let Some(x) = new else {
                        return Err(
                            "this spec lists its seeds; the list stays (edit it in the file)"
                                .into(),
                        );
                    };
                    let n = l.len();
                    seeds.remove("list");
                    let said = if *field == Field::SeedsN {
                        seeds.insert("n".into(), x.clone());
                        format!("n {}", text(&x))
                    } else {
                        seeds.insert("n".into(), json!(n));
                        seeds.insert("base".into(), x.clone());
                        format!("n {n}, base {}", text(&x))
                    };
                    return Ok(format!(
                        "the listed seeds are replaced by derived ones: {said}"
                    ));
                }
                let k = if *field == Field::SeedsN { "n" } else { "base" };
                (seeds, k)
            }
            Field::Need {
                stage,
                method,
                need,
            } => {
                let Some(s) = self
                    .root_mut()
                    .get_mut("stages")
                    .and_then(|s| s.get_mut(*stage))
                    .and_then(Value::as_object_mut)
                else {
                    return Err(format!("stages[{stage}] is not an object"));
                };
                let holder = match method {
                    None => s,
                    Some(m) => match s
                        .get_mut("methods")
                        .and_then(|ms| ms.get_mut(m))
                        .and_then(Value::as_object_mut)
                    {
                        Some(o) => o,
                        None => return Err(format!("method {m} is not an object")),
                    },
                };
                (holder, need.field())
            }
        };
        match new {
            Some(x) => {
                let said = format!("{k} {}", text(&x));
                holder.insert(k.to_string(), x);
                Ok(said)
            }
            None => {
                holder.remove(k);
                Ok(format!("{k} left out"))
            }
        }
    }

    /// `typed`'s values added to axis `a`: plain words, or on a tuple axis
    /// `LABEL NAME=VALUE ...`.
    fn add_values(&mut self, a: &str, typed: &str) -> Said {
        if typed.is_empty() {
            return Err("no value given".into());
        }
        let tuple = self.axis(a).first().is_some_and(Value::is_object);
        let new: Vec<Value> = if tuple {
            let mut words = typed.split_whitespace();
            let label = words.next().unwrap_or_default();
            let mut m = Map::new();
            m.insert("label".into(), json!(label));
            for w in words {
                let (k, x) = w
                    .split_once('=')
                    .ok_or_else(|| format!("{w:?}: a tuple value is LABEL NAME=VALUE ..."))?;
                m.insert(k.into(), json!(x));
            }
            vec![Value::Object(m)]
        } else {
            typed.split_whitespace().map(|w| json!(w)).collect()
        };
        let labels: Vec<String> = self.axis(a).iter().map(value_label).collect();
        let fresh: Vec<Value> = new
            .into_iter()
            .filter(|v| !labels.contains(&value_label(v)))
            .collect();
        if fresh.is_empty() {
            return Err(format!("axis {a} has those values already"));
        }
        let added: Vec<String> = fresh.iter().map(value_label).collect();
        let axes = self.axes_mut();
        if !axes.get(a).is_some_and(Value::is_array) {
            axes.insert(a.into(), json!([]));
        }
        axes[a].as_array_mut().expect("a list").extend(fresh);
        Ok(format!("added {a}={}", added.join(", ")))
    }

    /// The axis `NAME VALUE ...` added.
    fn add_axis(&mut self, typed: &str) -> Said {
        let mut words = typed.split_whitespace();
        let Some(name) = words.next() else {
            return Err("no axis given: NAME VALUE ...".into());
        };
        let values: Vec<Value> = words.map(|w| json!(w)).collect();
        if values.is_empty() {
            return Err(format!("give axis {name} a value: {name} VALUE ..."));
        }
        if self.value.get("axes").and_then(|a| a.get(name)).is_some() {
            return Err(format!("axis {name} is there already; a adds values to it"));
        }
        self.axes_mut().insert(name.into(), Value::Array(values));
        Ok(format!("added axis {name}"))
    }

    /// The excluded point `AXIS=VALUE ...` added.
    fn add_excluded(&mut self, typed: &str) -> Said {
        let mut m = Map::new();
        for w in typed.split_whitespace() {
            let (k, x) = w
                .split_once('=')
                .ok_or_else(|| format!("{w:?}: a point is AXIS=VALUE ..."))?;
            m.insert(k.into(), json!(x));
        }
        if m.is_empty() {
            return Err("no point given: AXIS=VALUE ...".into());
        }
        self.excludes_mut().push(Value::Object(m));
        Ok("added an excluded point".into())
    }
}

/// `legume benchmark plan SPEC --edit`: the editor on `path`, on this
/// terminal, then what became of the spec said on `out`.
#[cfg(feature = "tui")]
pub fn main(path: &Path, out: &mut dyn Write) -> Result<()> {
    let ed = Editor::open(path)?;
    let said = super::edit_view::run(ed)?;
    Ok(writeln!(out, "{said}")?)
}

/// `--edit` in a build without `tui`: refused, saying how to get it.
#[cfg(not(feature = "tui"))]
pub fn main(_: &Path, _: &mut dyn Write) -> Result<()> {
    Err(refuse(
        "--edit needs a legume built with tui: cargo install legume-mcp --features tui,benchmark",
    ))
}

/// The spec a new file starts from: one axis, three seeds, a stage that
/// makes data and one method that scores it, named after the file.
fn template(path: &Path) -> Value {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(40)
        .collect();
    let name = if name.is_empty() || name.starts_with('.') {
        "bench".to_string()
    } else {
        name
    };
    json!({
        "name": name,
        "seeds": {"n": 3, "base": 0},
        "axes": {"size": ["100", "1000"]},
        "stages": [
            {"name": "sim",
             "run": ["TOOL", "--size", "{size}", "--seed", "{seed}", "--out", "{out}/data"],
             "makes": ["data"]},
            {"name": "fit",
             "methods": {
                "M1": {"run": ["TOOL", "--in", "{sim}/data", "--out", "{out}/m.score.tsv"],
                       "makes": ["m.score.tsv"]}}}
        ]
    })
}

/// `v` as pretty JSON, its keys sorted, ending in a newline, as
/// [`crate::pretty`] writes a file; a JSON value always writes.
fn pretty(v: &Value) -> String {
    crate::pretty(v).unwrap_or_default()
}

/// `v` as a row shows it: a string bare, anything else as JSON.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// An axis value's label: the value on a plain axis, its `label` on a
/// tuple axis.
fn value_label(v: &Value) -> String {
    match v {
        Value::Object(m) => m.get("label").map_or(String::new(), text),
        other => text(other),
    }
}

/// A plural's `s`, unless `n` is 1.
pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `, and N excluded points` when a removal took some.
fn dropped_text(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => ", and the excluded point naming it".into(),
        n => format!(", and the {n} excluded points naming it"),
    }
}

/// A typed answer as JSON: a number when it reads as one (unless
/// `as_text`), else the text, which the spec checks then refuse where a
/// number belongs.
fn typed_value(typed: &str, as_text: bool) -> Value {
    if !as_text {
        if let Ok(n @ Value::Number(_)) = serde_json::from_str::<Value>(typed) {
            return n;
        }
    }
    Value::String(typed.to_string())
}

/// Field `k` of `map` as an object, made one when it is not.
fn object_at<'a>(map: &'a mut Map<String, Value>, k: &str) -> &'a mut Map<String, Value> {
    if !map.get(k).is_some_and(Value::is_object) {
        map.insert(k.into(), json!({}));
    }
    map[k].as_object_mut().expect("an object")
}

/// A stage's or method's threads, mem_gb and timeout, as written; `-`
/// where it gives none.
fn needs_text(holder: Option<&Value>) -> String {
    let get = |k| holder.and_then(|h| h.get(k)).map_or("-".to_string(), text);
    format!(
        "threads {:<3} mem_gb {:<4} timeout {}",
        get("threads"),
        get("mem_gb"),
        get("timeout")
    )
}

/// The field of the spec row `r` stands for, as a refusal names it.
fn row_path(r: &Row) -> Option<String> {
    Some(match r {
        Row::Name => "name".into(),
        Row::Cores => "cores".into(),
        Row::MemGb => "mem_gb".into(),
        Row::SeedsN => "seeds".into(),
        Row::SeedsBase => "seeds.base".into(),
        Row::Axes => "axes".into(),
        Row::Axis(a) => format!("axes.{a}"),
        Row::Value { axis, index } => format!("axes.{axis}[{index}]"),
        Row::Exclude => "exclude".into(),
        Row::Excluded(i) => format!("exclude[{i}]"),
        Row::Stages => "stages".into(),
        Row::Stage(i) => format!("stages[{i}]"),
        Row::Method { stage, name } => format!("stages[{stage}].methods.{name}"),
    })
}

/// The rows of spec `v`: its fields, then its axes and their values, its
/// excluded points, and its stages and their methods.
fn rows(v: &Value) -> Vec<Row> {
    let mut rows = vec![
        Row::Name,
        Row::Cores,
        Row::MemGb,
        Row::SeedsN,
        Row::SeedsBase,
        Row::Axes,
    ];
    if let Some(Value::Object(axes)) = v.get("axes") {
        for (a, values) in axes {
            rows.push(Row::Axis(a.clone()));
            let n = values.as_array().map_or(0, Vec::len);
            rows.extend((0..n).map(|index| Row::Value {
                axis: a.clone(),
                index,
            }));
        }
    }
    rows.push(Row::Exclude);
    let n = v
        .get("exclude")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    rows.extend((0..n).map(Row::Excluded));
    rows.push(Row::Stages);
    if let Some(Value::Array(stages)) = v.get("stages") {
        for (i, s) in stages.iter().enumerate() {
            rows.push(Row::Stage(i));
            if let Some(Value::Object(ms)) = s.get("methods") {
                rows.extend(ms.keys().map(|name| Row::Method {
                    stage: i,
                    name: name.clone(),
                }));
            }
        }
    }
    rows
}

/// What spec `v` plans and its first refusal, from the spec alone: no
/// tool is looked for, nothing is written. For anything that shows a spec
/// before it is planned (the editor, the MCP tools).
pub(crate) fn report(v: &Value) -> Report {
    let (spec, canonical) = match spec::parse(&v.to_string()) {
        Ok(x) => x,
        Err(e) => {
            return Report {
                counts: None,
                refusal: Some(e.to_string()),
            }
        }
    };
    let refusal = plan::check(&spec)
        .and_then(|()| machine::fit(&spec, &booked(&spec)).map(drop))
        .err()
        .map(|e| e.to_string());
    Report {
        counts: Some(counts(&spec, &canonical)),
        refusal,
    }
}

/// The spec's tasks counted from its structure, each stage's method on
/// its own: every method runs at every point for every rep. `canonical`
/// is the spec as [`spec::parse`] gives it, for its key.
pub(crate) fn counts(spec: &spec::Spec, canonical: &Value) -> Counts {
    let grid = spec
        .axes
        .iter()
        .fold(1usize, |n, a| n.saturating_mul(a.values.len()));
    let points = plan::points(spec).len();
    let reps = plan::reps(&spec.seeds).len();
    let each = points.saturating_mul(reps);
    let methods: Vec<MethodCount> = spec
        .stages
        .iter()
        .flat_map(|s| {
            s.methods.iter().map(move |m| MethodCount {
                name: match &m.name {
                    Some(n) => format!("{}-{n}", s.name),
                    None => s.name.clone(),
                },
                tasks: each,
                threads: m.threads,
                mem_gb: m.mem_gb,
                timeout_secs: m.timeout_secs,
                gpus: m.gpus,
                max_running: m.max_running,
                tool: m.tool_word().to_string(),
            })
        })
        .collect();
    let core_hours = methods
        .iter()
        .map(|m| m.tasks as f64 * f64::from(m.threads) * m.timeout_secs as f64 / 3600.0)
        .sum();
    Counts {
        grid,
        points,
        reps,
        tasks: methods.iter().map(|m| m.tasks).sum(),
        methods,
        cores: spec.cores,
        mem_gb: spec.mem_gb,
        core_hours,
        key: key::spec_key(canonical),
    }
}

/// A machine that is just what the spec books, so its tasks' threads and
/// memory are checked against that and nothing of this one: no cores or
/// memory limit where it books none.
fn booked(spec: &spec::Spec) -> machine::Machine {
    machine::Machine {
        host: None,
        os: None,
        kernel: None,
        cores_logical: spec.cores.unwrap_or(u32::MAX),
        cores_physical: None,
        mem_total_gb: spec.mem_gb.unwrap_or(f64::INFINITY),
        mem_available_gb: None,
        gpus: vec![String::new(); spec.gpus.len()],
        gpu_driver: None,
        legume: String::new(),
        features: Vec::new(),
    }
}

/// A duration as a spec writes it (`4h`, `1h30m`, `45s`), as a task's
/// timeout is shown wherever one is.
pub(crate) fn duration_text(secs: u64) -> String {
    let mut out = String::new();
    let mut rest = secs;
    for (unit, n) in [("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)] {
        if rest >= n {
            out += &format!("{}{unit}", rest / n);
            rest %= n;
        }
    }
    if out.is_empty() {
        "0s".into()
    } else {
        out
    }
}

/// The lines of `old` and `new` marked: the longest run they share kept,
/// what only `old` has gone, what only `new` has new.
pub fn diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // lcs[i][j]: the longest common run of a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let line = |mark, s: &str| DiffLine {
        mark,
        text: s.to_string(),
    };
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(line(Mark::Same, a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(line(Mark::Gone, a[i]));
            i += 1;
        } else {
            out.push(line(Mark::New, b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|s| line(Mark::Gone, s)));
    out.extend(b[j..].iter().map(|s| line(Mark::New, s)));
    out
}

/// The changed lines of `lines` with `context` unchanged lines around
/// each; None where unchanged lines are left out.
pub fn hunks(lines: &[DiffLine], context: usize) -> Vec<Option<&DiffLine>> {
    let near = |i: usize| {
        let lo = i.saturating_sub(context);
        let hi = (i + context + 1).min(lines.len());
        lines[lo..hi].iter().any(|l| l.mark != Mark::Same)
    };
    let mut out = Vec::new();
    let mut gap = false;
    for (i, l) in lines.iter().enumerate() {
        if near(i) {
            out.push(Some(l));
            gap = false;
        } else if !gap {
            out.push(None);
            gap = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An editor on `text` as the file `spec.json` in a folder of its own.
    fn editor(text: &str) -> (tempfile::TempDir, Editor) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("spec.json");
        std::fs::write(&p, text).unwrap();
        let ed = Editor::open(&p).unwrap();
        (d, ed)
    }

    /// An editor on a file not there yet, `B1.json`: the template.
    fn new_editor() -> (tempfile::TempDir, Editor) {
        let d = tempfile::tempdir().unwrap();
        let ed = Editor::open(&d.path().join("B1.json")).unwrap();
        (d, ed)
    }

    fn counts(ed: &Editor) -> &Counts {
        ed.report().counts.as_ref().unwrap()
    }

    fn go(ed: &mut Editor, row: &Row) {
        let i = ed.rows().iter().position(|r| r == row).unwrap();
        ed.move_to(i);
    }

    const TWO_AXES: &str = r#"{"name": "N1", "seeds": {"n": 2, "base": 5},
        "axes": {"A": ["1", "2", "3"], "B": ["x", "y"]},
        "stages": [{"name": "sim", "run": ["TOOL1", "{A}", "{B}", "{out}"], "makes": ["d"]},
                   {"name": "fit", "threads": 2, "mem_gb": 4, "methods": {
                     "M1": {"run": ["TOOL2", "{sim}"], "makes": ["m.score.tsv"]},
                     "M2": {"threads": 8, "run": ["TOOL3", "{sim}"], "makes": ["m.score.tsv"]}}}]}"#;

    // A new file starts from a template that plans: its tool is a word no
    // PATH has, and nothing looks for it.
    #[test]
    fn a_new_file_starts_from_a_template_that_plans() {
        let (_d, ed) = new_editor();
        assert!(ed.is_new() && ed.changed());
        assert_eq!(ed.report().refusal, None);
        assert_eq!(ed.value()["name"], "B1");
        let c = counts(&ed);
        assert_eq!((c.grid, c.points, c.reps, c.tasks), (2, 2, 3, 12));
        let names: Vec<&str> = c.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["sim", "fit-M1"]);
        assert_eq!(c.core_hours, 12.0 * 4.0);
    }

    #[test]
    fn counts_come_from_the_spec_alone() {
        let (_d, ed) = editor(TWO_AXES);
        assert!(!ed.changed());
        let c = counts(&ed);
        assert_eq!((c.grid, c.points, c.reps, c.tasks), (6, 6, 2, 36));
        let fit: Vec<(u32, f64)> = c.methods[1..]
            .iter()
            .map(|m| (m.threads, m.mem_gb))
            .collect();
        assert_eq!(fit, [(2, 4.0), (8, 4.0)]);
        assert_eq!((c.cores, c.mem_gb), (None, None));
    }

    // Toggling a value off excludes its points, as an exclude entry of
    // it alone; on again takes the entry out, and the spec is as it was.
    #[test]
    fn a_value_toggled_off_is_an_excluded_point() {
        let (_d, mut ed) = editor(TWO_AXES);
        go(
            &mut ed,
            &Row::Value {
                axis: "A".into(),
                index: 1,
            },
        );
        assert_eq!(ed.toggle(), Ok("A=2 off".into()));
        assert_eq!(ed.value()["exclude"], json!([{"A": "2"}]));
        assert_eq!(counts(&ed).points, 4);
        let row = ed.row().unwrap().clone();
        assert!(ed.line(&row).off);
        assert_eq!(ed.line(&row).text, "[ ] 2");
        assert_eq!(ed.toggle(), Ok("A=2 on".into()));
        assert!(ed.value().get("exclude").is_none());
        assert!(!ed.changed());
    }

    #[test]
    fn values_and_axes_are_added_and_removed() {
        let (_d, mut ed) = editor(TWO_AXES);
        go(&mut ed, &Row::Axis("B".into()));
        let (ask, start) = ed.ask(Want::Add).unwrap();
        assert_eq!((&ask, start.as_str()), (&Ask::AddValues("B".into()), ""));
        assert_eq!(ed.answer(&ask, "y z w"), Ok("added B=z, w".into()));
        assert_eq!(counts(&ed).points, 12);
        assert_eq!(
            ed.answer(&ask, "x"),
            Err("axis B has those values already".into())
        );
        // A removed value takes the excluded points that name it.
        ed.answer(&Ask::AddExcluded, "A=1 B=z").unwrap();
        go(
            &mut ed,
            &Row::Value {
                axis: "B".into(),
                index: 2,
            },
        );
        assert_eq!(
            ed.remove(),
            Ok("removed B=z, and the excluded point naming it".into())
        );
        assert!(ed.value().get("exclude").is_none());
        go(&mut ed, &Row::Axes);
        let (ask, _) = ed.ask(Want::Add).unwrap();
        assert_eq!(
            ed.answer(&ask, "C"),
            Err("give axis C a value: C VALUE ...".into())
        );
        assert_eq!(ed.answer(&ask, "C 1 2"), Ok("added axis C".into()));
        // The axis is in the grid, though no argv names it yet.
        assert_eq!(counts(&ed).grid, 18);
        go(&mut ed, &Row::Axis("C".into()));
        assert_eq!(ed.remove(), Ok("removed axis C".into()));
        assert_eq!(counts(&ed).grid, 9);
        // The last value goes with its axis, not alone.
        ed.answer(&ask, "D 1").unwrap();
        go(
            &mut ed,
            &Row::Value {
                axis: "D".into(),
                index: 0,
            },
        );
        assert!(ed.remove().unwrap_err().contains("removes the axis"));
    }

    #[test]
    fn excluded_points_are_added_and_removed() {
        let (_d, mut ed) = editor(TWO_AXES);
        go(&mut ed, &Row::Exclude);
        let (ask, _) = ed.ask(Want::Add).unwrap();
        assert_eq!(
            ed.answer(&ask, "A"),
            Err("\"A\": a point is AXIS=VALUE ...".into())
        );
        ed.answer(&ask, "A=1 B=x").unwrap();
        assert_eq!(counts(&ed).points, 5);
        assert_eq!(ed.line(&Row::Excluded(0)).text, "A=1 B=x");
        // One the spec refuses is still the person's to fix.
        ed.answer(&ask, "A=9").unwrap();
        assert_eq!(
            ed.report().refusal.as_deref(),
            Some("exclude[1].A: \"9\" is no value of axis A")
        );
        assert_eq!(ed.rows()[ed.refused_row().unwrap()], Row::Excluded(1));
        go(&mut ed, &Row::Excluded(1));
        ed.remove().unwrap();
        assert_eq!(ed.report().refusal, None);
    }

    // Threads, memory and timeouts are set on a stage and on a method;
    // empty leaves the field out, and the method takes the stage's again.
    #[test]
    fn needs_are_set_on_stages_and_methods() {
        let (_d, mut ed) = editor(TWO_AXES);
        let m2 = Row::Method {
            stage: 1,
            name: "M2".into(),
        };
        go(&mut ed, &m2);
        let (ask, start) = ed.ask(Want::Set(Need::Threads)).unwrap();
        assert_eq!(start, "8");
        assert_eq!(ed.answer(&ask, ""), Ok("threads left out".into()));
        assert_eq!(counts(&ed).methods[2].threads, 2);
        let (ask, _) = ed.ask(Want::Set(Need::Timeout)).unwrap();
        assert_eq!(
            ed.prompt(&ask),
            "timeout of method M2 (such as 30m or 1h30m; empty: the stage's, or 4h)"
        );
        ed.answer(&ask, "1h30m").unwrap();
        assert_eq!(counts(&ed).methods[2].timeout_secs, 5400);
        assert_eq!(
            ed.line(&m2).text,
            "M2        threads -   mem_gb -    timeout 1h30m"
        );
        go(&mut ed, &Row::Stage(1));
        let (ask, _) = ed.ask(Want::Set(Need::MemGb)).unwrap();
        ed.answer(&ask, "16").unwrap();
        assert_eq!(counts(&ed).methods[1].mem_gb, 16.0);
        // What is no number is refused by the spec, at its field.
        let (ask, _) = ed.ask(Want::Set(Need::Threads)).unwrap();
        ed.answer(&ask, "many").unwrap();
        assert_eq!(
            ed.report().refusal.as_deref(),
            Some("stages[1].threads: is \"many\", not a whole number of 0 or more")
        );
        assert_eq!(ed.rows()[ed.refused_row().unwrap()], Row::Stage(1));
        assert!(ed.report().counts.is_none());
        let (last, now) = ed.counts().unwrap();
        assert!(!now);
        assert_eq!(last.methods[1].mem_gb, 16.0);
    }

    // Threads above the cores it books are refused, as the plan refuses
    // them; with no cores booked, nothing of this machine bounds them.
    #[test]
    fn threads_and_memory_are_checked_against_what_it_books() {
        let (_d, mut ed) = editor(TWO_AXES);
        assert_eq!(ed.report().refusal, None);
        ed.answer(&Ask::Set(Field::Cores), "4").unwrap();
        assert_eq!(
            ed.report().refusal.as_deref(),
            Some("stages[1].methods.M2.threads: 8 is more than the benchmark's cores, 4")
        );
        assert_eq!(
            ed.rows()[ed.refused_row().unwrap()],
            Row::Method {
                stage: 1,
                name: "M2".into()
            }
        );
        ed.answer(&Ask::Set(Field::Cores), "16").unwrap();
        ed.answer(&Ask::Set(Field::MemGb), "2").unwrap();
        assert!(ed
            .report()
            .refusal
            .as_deref()
            .unwrap()
            .starts_with("stages[1].methods.M1.mem_gb: 4 GB is more than"));
        assert_eq!(
            (counts(&ed).cores, counts(&ed).mem_gb),
            (Some(16), Some(2.0))
        );
    }

    #[test]
    fn seeds_take_n_and_base_in_place_of_a_list() {
        let (_d, mut ed) = editor(&TWO_AXES.replace(r#""n": 2, "base": 5"#, r#""list": [7, 3]"#));
        assert_eq!(ed.line(&Row::SeedsN).text, "seeds       list 7, 3");
        assert_eq!(counts(&ed).reps, 2);
        // Nothing typed keeps the list, for n and for base.
        let keeps = Err("this spec lists its seeds; the list stays (edit it in the file)".into());
        assert_eq!(ed.answer(&Ask::Set(Field::SeedsN), ""), keeps);
        assert_eq!(ed.answer(&Ask::Set(Field::SeedsBase), " "), keeps);
        assert_eq!(ed.value()["seeds"], json!({"list": [7, 3]}));
        assert!(!ed.changed());
        // A base given replaces it, and says so.
        assert_eq!(
            ed.answer(&Ask::Set(Field::SeedsBase), "11"),
            Ok("the listed seeds are replaced by derived ones: n 2, base 11".into())
        );
        assert_eq!(ed.value()["seeds"], json!({"n": 2, "base": 11}));
        ed.answer(&Ask::Set(Field::SeedsN), "5").unwrap();
        assert_eq!(counts(&ed).reps, 5);
        ed.answer(&Ask::Set(Field::SeedsN), "0").unwrap();
        assert_eq!(
            ed.report().refusal.as_deref(),
            Some("seeds.n: is 0, so nothing would run")
        );
        assert_eq!(ed.rows()[ed.refused_row().unwrap()], Row::SeedsN);
    }

    #[test]
    fn an_n_given_replaces_a_seed_list_and_says_so() {
        let (_d, mut ed) = editor(&TWO_AXES.replace(r#""n": 2, "base": 5"#, r#""list": [7, 3]"#));
        assert_eq!(
            ed.answer(&Ask::Set(Field::SeedsN), "4"),
            Ok("the listed seeds are replaced by derived ones: n 4".into())
        );
        assert_eq!(ed.value()["seeds"], json!({"n": 4}));
        assert_eq!(counts(&ed).reps, 4);
    }

    // Saving shows the diff first: old and new canonical JSON. It writes
    // only the spec, and then nothing differs.
    #[test]
    fn a_save_writes_only_the_spec_after_its_diff() {
        let (d, mut ed) = editor(TWO_AXES);
        assert!(ed.relaid());
        ed.answer(&Ask::Set(Field::SeedsN), "4").unwrap();
        let lines = ed.diff();
        let changed: Vec<(Mark, &str)> = lines
            .iter()
            .filter(|l| l.mark != Mark::Same)
            .map(|l| (l.mark, l.text.trim()))
            .collect();
        assert_eq!(changed, [(Mark::Gone, "\"n\": 2"), (Mark::New, "\"n\": 4")]);
        let shown = hunks(&lines, 1);
        assert_eq!(shown.first(), Some(&None));
        assert_eq!(shown.iter().filter(|l| l.is_some()).count(), 4);
        let (old, new) = ed.keys();
        assert_ne!(old.unwrap(), new);
        let said = ed.save().unwrap();
        assert!(
            said.starts_with(&format!("saved {}", ed.path().display())),
            "{said}"
        );
        assert!(!ed.changed() && !ed.relaid());
        let files: Vec<_> = std::fs::read_dir(d.path()).unwrap().collect();
        assert_eq!(files.len(), 1);
        let back: Value =
            serde_json::from_str(&std::fs::read_to_string(ed.path()).unwrap()).unwrap();
        assert_eq!(back, *ed.value());
        assert!(ed.diff().iter().all(|l| l.mark == Mark::Same));
    }

    #[test]
    fn what_it_cannot_edit_is_refused_on_open() {
        let d = tempfile::tempdir().unwrap();
        let k = "0".repeat(32);
        let p = d.path().join("specs").join(format!("{k}.json"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{}").unwrap();
        let e = Editor::open(&p).err().unwrap();
        assert!(e.to_string().contains("not edited in place"), "{e}");
        let p = d.path().join("list.json");
        std::fs::write(&p, "[1]").unwrap();
        let e = Editor::open(&p).err().unwrap();
        assert!(e
            .to_string()
            .ends_with("is not a JSON object, as a spec is"));
        std::fs::write(&p, "{").unwrap();
        assert!(Editor::open(&p).is_err());
    }

    #[test]
    fn durations_read_as_a_spec_writes_them() {
        assert_eq!(duration_text(4 * 3600), "4h");
        assert_eq!(duration_text(5400), "1h30m");
        assert_eq!(duration_text(45), "45s");
        assert_eq!(duration_text(86_400 + 60), "1d1m");
    }

    #[test]
    fn a_diff_keeps_the_longest_shared_run() {
        let marks: Vec<(Mark, String)> = diff("a\nb\nc\n", "a\nc\nd\n")
            .into_iter()
            .map(|l| (l.mark, l.text))
            .collect();
        use Mark::*;
        assert_eq!(
            marks,
            [
                (Same, "a".into()),
                (Gone, "b".into()),
                (Same, "c".into()),
                (New, "d".into())
            ]
        );
    }
}
