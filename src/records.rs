//! The project's records as a reader takes them in: the steps with their
//! inputs and numbers, a day's notebook and the question waiting in it, and
//! the hypotheses with their sources. Read from the files, as the `legume`
//! server reads them, for the chat's screen, the provenance graph and the
//! report; nothing here writes.

use crate::evidence::{self, Ledger};
use crate::project::{read_record, Project};
use crate::step::{read_facts, Fact};
use std::path::Path;

/// What the records say now.
#[derive(Default)]
pub(crate) struct Records {
    /// The outputs the records list ([`steps`]), oldest first.
    pub steps: Vec<Step>,
    /// The day's notebook entries, oldest first.
    pub notebook: Vec<Entry>,
    /// The question in the day's notebook waiting for the person.
    pub waiting: Option<String>,
    pub hypotheses: Vec<Hypothesis>,
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

/// A notebook entry: its heading (`10:00:00 · server · kind`), its text,
/// and that text's first line.
pub(crate) struct Entry {
    pub time: String,
    pub server: String,
    pub kind: String,
    pub text: String,
    pub first: String,
}

/// A hypothesis and what its sources say of it.
pub(crate) struct Hypothesis {
    pub id: usize,
    pub claim: String,
    pub ledger: Ledger,
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
    steps.sort_by(|a, b| (&a.date, &a.time).cmp(&(&b.date, &b.time)));
    steps
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

impl Records {
    /// The records of the project at `root`, with today's notebook.
    #[cfg_attr(not(feature = "screen"), allow(dead_code))]
    pub(crate) fn today(root: &Path) -> Self {
        Self::read(root, &crate::project::now().0)
    }

    /// The records of the project at `root`, with the notebook of `day`
    /// (`2026-10-28`).
    pub(crate) fn read(root: &Path, day: &str) -> Self {
        let p = Project::new(
            root.to_path_buf(),
            crate::brain::NAME,
            env!("CARGO_PKG_VERSION"),
        );
        let steps = steps(root);
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
                    first: text.lines().next().unwrap_or_default().trim().to_string(),
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
            })
            .collect();
        Self {
            steps,
            notebook,
            waiting: crate::notebook::open_question(&day_notebook),
            hypotheses,
        }
    }

    /// The numbers the tools flagged, each with the output it came with.
    #[cfg_attr(not(feature = "screen"), allow(dead_code))]
    pub(crate) fn flagged(&self) -> impl Iterator<Item = (&Step, &Fact)> {
        self.steps
            .iter()
            .flat_map(|s| s.facts.iter().map(move |f| (s, f)))
            .filter(|(_, f)| f.flagged().is_some())
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
            "# Notebook\n\n## 10:01:00 · legume · plan\n\nWhy does cluster 4 differ?\n\n\
             ## 10:02:00 · legume · offer\n\n- **1.** split by donor\n- **2.** stop\n",
        )
        .unwrap();
        let r = Records::today(root);
        assert_eq!(r.steps.len(), 1);
        let s = &r.steps[0];
        assert_eq!((s.tool.as_str(), s.inputs.len()), ("qc", 2));
        assert_eq!(r.flagged().count(), 1);
        assert_eq!(r.notebook.len(), 2);
        assert_eq!(r.notebook[0].kind, "plan");
        assert_eq!(r.notebook[0].first, "Why does cluster 4 differ?");
        assert!(r.waiting.is_some(), "the offer waits for the person");
        assert!(r.hypotheses.is_empty());
    }
}
