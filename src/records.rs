//! The project's records as a reader takes them in: the data on disk and
//! what the person said of it, the steps with their inputs and numbers, a
//! day's notebook, the hypotheses with their sources, the figures kept, and
//! what is open for the person. Read from the files, as the `legume` server
//! reads them, for its status and the report; nothing here writes.

use crate::evidence::{self, Ledger};
use crate::notebook::{DATA, FIGURES};
use crate::project::{read_record, Project};
use crate::step::{read_facts, Fact};
use std::path::Path;

/// What the records say now.
#[derive(Default)]
pub(crate) struct Records {
    /// The files under `data/`, with what the person said of each.
    pub data: Vec<Data>,
    /// The outputs the records list ([`steps`]), oldest first.
    pub steps: Vec<Step>,
    /// The day's notebook entries, oldest first.
    pub notebook: Vec<Entry>,
    pub hypotheses: Vec<Hypothesis>,
    /// The figures and tables the person kept, by number.
    pub figures: Vec<Figure>,
}

/// A file under `data/`: where it came from and the person's verdict, as
/// they said them (empty when not said yet).
pub(crate) struct Data {
    pub path: String,
    pub bytes: u64,
    pub from: String,
    pub verdict: String,
}

/// A row of `INDEX.tsv`.
pub(crate) struct Step {
    pub date: String,
    pub time: String,
    pub server: String,
    pub tool: String,
    pub output: String,
    pub summary: String,
    pub inputs: Vec<String>,
    pub facts: Vec<Fact>,
}

/// A notebook entry: its heading (`10:00:00 · server · kind`) and its
/// text.
pub(crate) struct Entry {
    pub time: String,
    pub server: String,
    pub kind: String,
    pub text: String,
}

/// A hypothesis, as the person put it, and what its sources say of it.
pub(crate) struct Hypothesis {
    pub id: usize,
    pub claim: String,
    pub rivals: String,
    pub nullify: String,
    pub ledger: Ledger,
}

/// A figure or table the person kept for the paper.
pub(crate) struct Figure {
    pub number: String,
    pub output: String,
    pub hypothesis: String,
    pub take_home: String,
}

/// Every output the project's records list, oldest first: the rows set
/// aside before 0.5.0 (six columns: no inputs or numbers), then
/// `INDEX.tsv`'s.
pub(crate) fn steps(root: &Path) -> Vec<Step> {
    let mut steps: Vec<Step> = crate::project::index_files(root)
        .iter()
        .flat_map(|f| {
            read_record(f)
                .lines()
                .filter(|l| !l.starts_with("date\t") && !l.trim().is_empty())
                .map(row)
                .collect::<Vec<_>>()
        })
        .collect();
    // In time order, a step's rows kept together.
    steps.sort_by(|a, b| (&a.date, in_day(&a.time)).cmp(&(&b.date, in_day(&b.time))));
    steps
}

/// `time` (HH:MM:SS) in the order of the working day, which turns over at
/// 5 a.m. (`project::DAY_TURNS`): the small hours after the evening.
pub(crate) fn in_day(time: &str) -> (bool, &str) {
    let small = time
        .get(..2)
        .and_then(|h| h.parse::<u32>().ok())
        .is_some_and(|h| h < 5);
    (small, time)
}

/// `path` as the person may have named it (`./data/x/`) as the records
/// name it (`data/x`).
fn clean(path: &str) -> &str {
    let path = path.trim();
    path.strip_prefix("./")
        .unwrap_or(path)
        .trim_end_matches('/')
}

/// Whether what was said of `said` (a file, or a folder) is said of
/// `path`.
fn covers(said: &str, path: &str) -> bool {
    let said = clean(said);
    path == said || path.strip_prefix(said).is_some_and(|r| r.starts_with('/'))
}

/// A row of `INDEX.tsv` as a step.
fn row(line: &str) -> Step {
    let r: Vec<&str> = line.split('\t').collect();
    let cell = |i: usize| r.get(i).copied().unwrap_or_default().to_string();
    Step {
        date: cell(0),
        time: cell(1),
        server: cell(2),
        tool: cell(3),
        output: cell(4),
        summary: cell(5),
        inputs: cell(6)
            .split(';')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        facts: read_facts(&cell(7)),
    }
}

/// The rows of a ledger under `results/`, each split into its cells after
/// the date, time and server.
fn ledger_rows(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .skip(1)
        .map(|l| l.split('\t').skip(3).map(str::to_string).collect())
        .collect()
}

/// The files under `data/` (a `.zarr` folder is one), by path, with their
/// sizes; hidden ones left out.
fn data_files(root: &Path) -> Vec<(String, u64)> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, u64)>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut items: Vec<_> = read.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "README.md" {
                continue;
            }
            let path = e.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let Ok(m) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if m.is_dir() && !name.ends_with(".zarr") {
                if depth < 3 {
                    walk(root, &path, depth + 1, out);
                }
            } else {
                out.push((rel, if m.is_file() { m.len() } else { 0 }));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &root.join("data"), 0, &mut out);
    out
}

/// The most data files [`Records::open`] asks about by name.
const ASKED: usize = 5;

impl Records {
    /// The records of the project at `root`, with the notebook of `day`
    /// (`2026-10-28`).
    pub(crate) fn read(root: &Path, day: &str) -> Self {
        let p = Project::new(
            root.to_path_buf(),
            crate::brain::NAME,
            env!("CARGO_PKG_VERSION"),
        );
        let steps = steps(root);
        let readme = read_record(&root.join("data/README.md"));
        let said = ledger_rows(&p.read_ledger(DATA));
        // The latest of what the person said of a file, or of a folder that
        // holds it, that says this (its verdict, or where it came from): a
        // row that leaves one out does not take it back.
        let latest = |path: &str, i: usize| -> String {
            said.iter()
                .rev()
                .filter(|r| r.first().is_some_and(|s| covers(s, path)))
                .find_map(|r| r.get(i).filter(|c| !c.trim().is_empty()).cloned())
                .unwrap_or_default()
        };
        let mut files = data_files(root);
        // Data the person described outside data/, while it is there.
        for r in &said {
            let Some(s) = r.first().map(|s| clean(s)) else {
                continue;
            };
            if s.is_empty() || files.iter().any(|(f, _)| covers(s, f) || covers(f, s)) {
                continue;
            }
            if let Ok(m) = std::fs::symlink_metadata(root.join(s)) {
                files.push((s.to_string(), if m.is_file() { m.len() } else { 0 }));
            }
        }
        let data = files
            .into_iter()
            .map(|(path, bytes)| {
                // A tool's import says where its output came from too.
                let from = match latest(&path, 2) {
                    w if !w.is_empty() => w,
                    _ => readme
                        .lines()
                        .find(|l| l.contains(&format!("`{path}`")))
                        .and_then(|l| l.split_once("`: "))
                        .map(|(_, w)| w.to_string())
                        .unwrap_or_default(),
                };
                Data {
                    verdict: latest(&path, 1),
                    from,
                    path,
                    bytes,
                }
            })
            .collect();
        let day_notebook = read_record(&root.join("results").join(day).join("notebook.md"));
        let notebook = crate::notebook::entries(&day_notebook)
            .into_iter()
            .map(|e| {
                let (head, text) = e.split_once('\n').unwrap_or((e, ""));
                let mut head = head.splitn(3, " · ").map(str::to_string);
                let text = text.trim().to_string();
                Entry {
                    time: head.next().unwrap_or_default(),
                    server: head.next().unwrap_or_default(),
                    kind: head.next().unwrap_or_default(),
                    text,
                }
            })
            .collect();
        let all = evidence::evidence(&p.read_ledger(evidence::EVIDENCE));
        let hypotheses = evidence::hypotheses(&p.read_ledger(evidence::HYPOTHESES))
            .into_iter()
            .map(|h| Hypothesis {
                ledger: evidence::ledger(&h, &all),
                id: h.id,
                claim: h.claim,
                rivals: h.rivals,
                nullify: h.nullify,
            })
            .collect();
        let mut figures: Vec<Figure> = Vec::new();
        for r in ledger_rows(&p.read_ledger(FIGURES)) {
            let cell = |i: usize| r.get(i).cloned().unwrap_or_default();
            let f = Figure {
                number: cell(0),
                output: cell(1),
                hypothesis: cell(2),
                take_home: cell(3),
            };
            match figures.iter_mut().find(|o| o.number == f.number) {
                Some(o) => *o = f,
                None => figures.push(f),
            }
        }
        Self {
            data,
            steps,
            notebook,
            hypotheses,
            figures,
        }
    }

    /// What is open for the person: data whose origin or verdict they have
    /// not given, hypotheses without rivals or what would nullify them,
    /// figures that support no hypothesis.
    pub(crate) fn open(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut asked = 0;
        let mut left = 0;
        for d in &self.data {
            let ask = if d.from.is_empty() {
                format!("Where did {} come from?", d.path)
            } else if d.verdict.is_empty() {
                format!("Is {} good enough? (after the quality checks)", d.path)
            } else {
                continue;
            };
            if asked < ASKED {
                out.push(ask);
                asked += 1;
            } else {
                left += 1;
            }
        }
        if left > 0 {
            out.push(format!(
                "{left} more files under data/ are not described yet: what is said of a \
                 folder is said of every file in it."
            ));
        }
        for h in &self.hypotheses {
            if h.rivals.is_empty() {
                out.push(format!("H{}: what else could explain it?", h.id));
            }
            if h.nullify.is_empty() {
                out.push(format!("H{}: what result would make you drop it?", h.id));
            }
        }
        for f in self.figures.iter().filter(|f| f.hypothesis.is_empty()) {
            out.push(format!("Figure {}: which claim does it support?", f.number));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_records_read_as_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let day = crate::project::now().0;
        std::fs::create_dir_all(root.join(format!("results/{day}"))).unwrap();
        std::fs::create_dir_all(root.join("data/sub/b.zarr")).unwrap();
        std::fs::write(root.join("data/a.h5"), "12345").unwrap();
        std::fs::write(root.join("data/.hidden"), "x").unwrap();
        std::fs::write(
            root.join("results/INDEX.tsv"),
            format!(
                "{}\n{day}\t10:00:00\tdata-beans\tqc\tresults/{day}/a.zarr.zip\tkept cells\tdata/a.h5;data/b.tsv\t{}\n",
                crate::project::INDEX_HEAD,
                crate::step::facts_cell(&[
                    Fact::count("cells", 9812).of(11204),
                    Fact::value("mito", 0.3).flag("high"),
                ])
            ),
        )
        .unwrap();
        std::fs::write(
            root.join(format!("results/{day}/notebook.md")),
            "# Notebook\n\n## 10:01:00 · legume · plan\n\n> Why does cluster 4 differ?\n\n\
             ## 10:02:00 · legume · observation\n\nQC kept 9,812 cells.\n",
        )
        .unwrap();
        let p = Project::new(root.to_path_buf(), "legume", "0");
        p.ledger(
            DATA,
            &[vec![
                "data/a.h5".into(),
                String::new(),
                "from the core".into(),
            ]],
        );
        let r = Records::read(root, &day);
        let paths: Vec<&str> = r.data.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["data/a.h5", "data/sub/b.zarr"]);
        assert_eq!(
            (r.data[0].bytes, r.data[0].from.as_str()),
            (5, "from the core")
        );
        assert_eq!(r.steps.len(), 1);
        let s = &r.steps[0];
        assert_eq!((s.tool.as_str(), s.inputs.len()), ("qc", 2));
        assert_eq!(s.facts.iter().filter(|f| f.flagged().is_some()).count(), 1);
        assert_eq!(r.notebook.len(), 2);
        assert_eq!(r.notebook[0].kind, "plan");
        assert_eq!(r.notebook[0].text, "> Why does cluster 4 differ?");
        assert!(r.hypotheses.is_empty());
        assert_eq!(
            r.open(),
            [
                "Is data/a.h5 good enough? (after the quality checks)",
                "Where did data/sub/b.zarr come from?"
            ]
        );
    }

    #[test]
    fn what_the_person_said_holds_for_a_folder_and_until_they_change_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let day = crate::project::now().0;
        std::fs::create_dir_all(root.join("data/run1/filtered")).unwrap();
        std::fs::create_dir_all(root.join("raw")).unwrap();
        for f in [
            "data/run1/filtered/matrix.mtx.gz",
            "data/run1/filtered/barcodes.tsv.gz",
        ] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        for i in 0..7 {
            std::fs::write(root.join(format!("data/s{i}.h5")), "x").unwrap();
        }
        std::fs::write(root.join("raw/pbmc.h5"), "12").unwrap();
        let p = Project::new(root.to_path_buf(), "legume", "0");
        let row =
            |path: &str, verdict: &str, why: &str| vec![path.into(), verdict.into(), why.into()];
        p.ledger(
            DATA,
            &[
                row("./data/run1/", "good", "10x run 1, core facility"),
                row("data/run1", "", "10x run 1, donors 1-6 only"),
                row("raw/pbmc.h5", "caveats", "a public set"),
                row("raw/gone.h5", "bad", "deleted since"),
            ],
        );
        let r = Records::read(root, &day);
        let of = |path: &str| r.data.iter().find(|d| d.path == path).unwrap();
        let m = of("data/run1/filtered/matrix.mtx.gz");
        assert_eq!(
            (m.from.as_str(), m.verdict.as_str()),
            ("10x run 1, donors 1-6 only", "good")
        );
        assert_eq!(
            of("raw/pbmc.h5").verdict,
            "caveats",
            "outside data/, while it is there"
        );
        assert!(r.data.iter().all(|d| d.path != "raw/gone.h5"));
        // Seven files not described: five asked by name, the rest counted.
        let open = r.open();
        assert_eq!(open.len(), 6, "{open:?}");
        assert!(open[5].starts_with("2 more files under data/"), "{open:?}");
    }

    #[test]
    fn the_small_hours_are_the_end_of_the_working_day() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let line =
            |time: &str, tool: &str| format!("2026-10-28\t{time}\ts\t{tool}\tr/{tool}\t\t\t[]\n");
        std::fs::create_dir_all(root.join("results")).unwrap();
        std::fs::write(
            root.join("results/INDEX.tsv"),
            format!(
                "{}\n{}{}{}",
                crate::project::INDEX_HEAD,
                line("01:30:00", "late"),
                line("10:00:00", "morning"),
                line("22:00:00", "evening")
            ),
        )
        .unwrap();
        let tools: Vec<String> = steps(root).into_iter().map(|s| s.tool).collect();
        assert_eq!(tools, ["morning", "evening", "late"]);
    }
}
