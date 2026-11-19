//! The Slurm executor: a benchmark's tasks run as Slurm array tasks on a
//! cluster, each under the wrapper (`legume benchmark task`), driven
//! through a [`Shell`] from wherever the runner is.
//!
//! - **Launch.** Tasks of one shape (threads, memory, timeout) go in as
//!   one array job, at most `max_array` tasks each. A list of their ids
//!   is written beside the benchmark, named by the run and its content
//!   (`slurm/<run>-<key>.tasks`, so a list an earlier array job still
//!   reads is never rewritten with other lines), and array task `i` runs
//!   the task on line `i`. An earlier attempt in a task's folder is moved
//!   aside whole, to `.attempts/<id>.<n>` beside it, never written over
//!   (they are kept, outputs and all, until removed by hand); each array
//!   task's Slurm log is named by its job id and index. Every array job
//!   is named for the run, so the runner finds its own and no one else's. Once every array job is in,
//!   each task's folder gets `slurm.json`: its array job and index. One
//!   shell call submits a whole launch.
//! - **Poll.** One shell call per round: `squeue` for the run's array
//!   jobs (a pending range is one line, however many tasks it holds),
//!   and the `exit.json` of each task the runner has that is not
//!   pending. A task ends when its `exit.json` is there, whatever the
//!   queue says: its `outcome` is the wrapper's. One gone from the queue
//!   without it is lost after [`Slurm::lost_after`].
//! - **Cancel.** `scancel` of the array tasks themselves, never by name.
//!   Slurm sends TERM; the wrapper passes it on and says it stopped.
//!
//! Slurm 17.02 and later: no `--gpus`, and nothing run on the login node
//! but bash, coreutils and Slurm's own commands.

use super::executor::{Ended, Executor, Resumed, TaskExit};
use super::plan::{Placed, Task, ATTEMPTS, PLACED};
use super::pool::{Launch, Live, Outcome};
use super::remote::Shell;
use crate::project::quote;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// Where and how the run's array jobs go in.
#[derive(Clone, Debug)]
pub struct Site {
    /// `legume` on the cluster's nodes.
    pub legume: String,
    /// The plan's `plan.json` on the cluster.
    pub plan: String,
    /// The folder the run's lists and Slurm logs go in.
    pub slurm_dir: String,
    /// The run's id, every array job's name.
    pub run: String,
    pub partition: Option<String>,
    pub account: Option<String>,
    /// Tasks in one array job at most.
    pub max_array: usize,
}

/// An array task: its array job and index.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Job {
    array_job: String,
    index: usize,
}

impl Job {
    fn id(&self) -> String {
        format!("{}_{}", self.array_job, self.index)
    }
}

/// A task the runner has running.
struct Running {
    job: Job,
    /// When it was first seen gone from the queue without an `exit.json`.
    gone_since: Option<Instant>,
}

/// The Slurm executor over `shell`.
pub struct Slurm {
    shell: Shell,
    site: Site,
    /// The plan's tasks, by index.
    tasks: Vec<Task>,
    /// The plan's grace, between TERM and KILL.
    grace_secs: u64,
    running: BTreeMap<usize, Running>,
    /// The array tasks the last poll saw pending, whose files are not
    /// looked at.
    pending: HashSet<Job>,
    /// Tasks a person stopped: one lost is stopped, not lost.
    stopping: HashSet<usize>,
    /// Array tasks resume found queued for a task under another key.
    stale: Vec<Job>,
    /// How long a task gone from the queue without an `exit.json` is
    /// waited for (a shared filesystem may show a new file late).
    pub lost_after: Duration,
}

impl Slurm {
    /// The executor of the plan's `tasks`, with its `grace_secs`, at
    /// `site`, through `shell`.
    pub fn new(shell: Shell, site: Site, tasks: Vec<Task>, grace_secs: u64) -> Self {
        Slurm {
            shell,
            site,
            tasks,
            grace_secs,
            running: BTreeMap::new(),
            pending: HashSet::new(),
            stopping: HashSet::new(),
            stale: Vec::new(),
            lost_after: Duration::from_secs(60),
        }
    }

    /// On resume, one shell call: the tasks done in an earlier run (their
    /// `done.json` has the task's key), and the tasks of an earlier run
    /// still in the queue (`slurm.json` names an array task the queue
    /// has, and no `exit.json` says it ended), which are adopted. A task
    /// done, or still queued, for another key is rekeyed; one still
    /// queued is kept for [`Executor::cancel_rekeyed`]. The script is the
    /// same size for any number of tasks: one `find` at the tasks' depth
    /// under their common folder, and one `awk` that prints each file
    /// found.
    fn resumed(&mut self) -> Result<Resumed> {
        let root = common_folder(self.tasks.iter().map(|t| t.dir.as_path()))
            .context("a plan with no tasks")?;
        let depths = self.tasks.iter().map(|t| {
            t.dir
                .strip_prefix(&root)
                .map_or(0, |d| d.components().count())
        });
        let (min, max) = depths.fold((usize::MAX, 0), |(a, b), d| (a.min(d), b.max(d)));
        let script = format!(
            "{}find {} -mindepth {} -maxdepth {} -path '*/{ATTEMPTS}/*' -prune -o \
             \\( -name done.json -o -name slurm.json -o -name exit.json \\) -type f -print0 2>/dev/null | xargs -0 -r awk 'FNR == 1 \
             {{ if (NR > 1) print \"\"; printf \"f\\t%s\\t\", FILENAME }} {{ printf \"%s\", $0 }} \
             END {{ if (NR > 0) print \"\" }}'\nexit 0\n",
            squeue_line(&self.site.run),
            quote(&root.display().to_string()),
            min + 1,
            max + 1
        );
        let ran = self.shell.run(&script)?;
        let poll = parse_poll(&ran.stdout);
        if !ran.ok() || !poll.queue_ok {
            bail!(
                "reading the benchmark's tasks on the cluster: {}",
                ran.stderr.trim()
            );
        }
        let files = task_files(&ran.stdout, &self.tasks);
        let mut resumed = Resumed::default();
        let mut watched = Vec::new();
        for (i, t) in self.tasks.iter().enumerate() {
            let f = files.get(&i);
            let done = f
                .and_then(|f| f.done.as_deref())
                .and_then(|j| super::plan::done_key(j.as_bytes()));
            if done.as_deref() == Some(t.key.as_str()) {
                resumed.done.push(i);
                continue;
            }
            let placed = f
                .filter(|f| !f.exit)
                .and_then(|f| f.slurm.as_deref())
                .and_then(|j| serde_json::from_str::<Placed>(j).ok())
                .map(|p| {
                    let job = Job {
                        array_job: p.array_job_id,
                        index: p.index,
                    };
                    (job, p.key == t.key)
                })
                .filter(|(j, _)| poll.queued.contains(j) || poll.pending.contains(j));
            if done.is_some() || matches!(placed, Some((_, false))) {
                resumed.rekeyed.push(i);
            }
            match placed {
                Some((job, true)) => watched.push((i, job)),
                Some((job, false)) => self.stale.push(job),
                None => {}
            }
        }
        for (i, job) in watched {
            self.watch(i, job);
            resumed.adopted.push(i);
        }
        self.pending = poll.pending;
        Ok(resumed)
    }

    /// Task `i` watched as array task `job`.
    fn watch(&mut self, i: usize, job: Job) {
        self.running.insert(
            i,
            Running {
                job,
                gone_since: None,
            },
        );
    }

    fn task(&self, i: usize) -> Result<&Task> {
        self.tasks
            .get(i)
            .with_context(|| format!("no task {i} in the plan"))
    }

    /// `sbatch` of an array job of `n` tasks of `t`'s shape whose list is
    /// `list`, its logs `name`.
    fn sbatch(&self, t: &Task, n: usize, list: &str, name: &str) -> String {
        let s = &self.site;
        // The wrapper's timeout and grace go first; Slurm's --time is
        // only a backstop, a few minutes after.
        let minutes = (t.timeout_secs + self.grace_secs).div_ceil(60) + 5;
        let wrap = format!(
            "exec {} benchmark task --plan {} --list {}",
            quote(&s.legume),
            quote(&s.plan),
            quote(list)
        );
        let mut args = format!(
            "sbatch --parsable --job-name={} --array=0-{} --cpus-per-task={}{} \
             --time={minutes} --open-mode=append --output={} --wrap={}",
            quote(&s.run),
            n - 1,
            t.threads,
            mem_flag(t.mem_gb),
            quote(&format!("{}/{name}_%A_%a.log", s.slurm_dir)),
            quote(&wrap)
        );
        args.push_str(&place_flags(s.partition.as_deref(), s.account.as_deref()));
        args
    }
}

/// The `--mem` flag for a task of `mem_gb`, with a space before it; none
/// for 0, a task whose memory is not counted, which then gets the
/// partition's default.
fn mem_flag(mem_gb: f64) -> String {
    if mem_gb > 0.0 {
        format!(" --mem={}M", (mem_gb * 1024.0).ceil() as u64)
    } else {
        String::new()
    }
}

/// The `--partition` and `--account` flags of `srun` or `sbatch`, each
/// with a space before it; nothing for one not given.
fn place_flags(partition: Option<&str>, account: Option<&str>) -> String {
    let mut f = String::new();
    for (flag, v) in [("--partition", partition), ("--account", account)] {
        if let Some(v) = v {
            let _ = write!(f, " {flag}={}", quote(v));
        }
    }
    f
}

/// `<array job>_<index>`.
fn parse_job(s: &str) -> Result<Job> {
    let (array_job, index) = s
        .split_once('_')
        .with_context(|| format!("{s:?} is not <array job>_<index>"))?;
    let index = index
        .parse()
        .with_context(|| format!("{s:?}: the index is a number"))?;
    if array_job.is_empty() || !array_job.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{s:?}: the array job id is a number");
    }
    Ok(Job {
        array_job: array_job.to_string(),
        index,
    })
}

/// The array tasks of a pending range, `<array job>_[1-3,5%2]`.
fn parse_range(s: &str) -> Option<Vec<Job>> {
    let (array_job, rest) = s.split_once("_[")?;
    // `%N` is the array job's throttle, not an index.
    let spec = rest.strip_suffix(']')?.split('%').next()?;
    let mut jobs = Vec::new();
    for part in spec.split(',') {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        for index in a.parse::<usize>().ok()?..=b.parse::<usize>().ok()? {
            jobs.push(Job {
                array_job: array_job.to_string(),
                index,
            });
        }
    }
    Some(jobs)
}

/// The files of one task's folder that resume reads.
#[derive(Default)]
struct TaskFiles {
    done: Option<String>,
    slurm: Option<String>,
    exit: bool,
}

/// The folder every path in `dirs` is under.
fn common_folder<'a>(
    mut dirs: impl Iterator<Item = &'a std::path::Path>,
) -> Option<std::path::PathBuf> {
    let mut common = dirs.next()?.to_path_buf();
    for d in dirs {
        while !d.starts_with(&common) {
            if !common.pop() {
                break;
            }
        }
    }
    Some(common)
}

/// The `f <path> <text>` lines of a resume, by task: each task's
/// `done.json` and `slurm.json` text and whether its `exit.json` is
/// there. A file in no task's own folder is not the task's.
fn task_files(out: &str, tasks: &[Task]) -> HashMap<usize, TaskFiles> {
    let by_dir: HashMap<&std::path::Path, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.dir.as_path(), i))
        .collect();
    let mut files: HashMap<usize, TaskFiles> = HashMap::new();
    for line in out.lines() {
        let Some((path, text)) = line.strip_prefix("f\t").and_then(|r| r.split_once('\t')) else {
            continue;
        };
        let path = std::path::Path::new(path);
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            continue;
        };
        let Some(&i) = by_dir.get(dir) else {
            continue;
        };
        let f = files.entry(i).or_default();
        match name.to_str() {
            Some("done.json") => f.done = Some(text.to_string()),
            Some("slurm.json") => f.slurm = Some(text.to_string()),
            Some("exit.json") => f.exit = true,
            _ => {}
        }
    }
    files
}

/// `srun` of `command` on a compute node, as the run's job `run`, with
/// one core and `mem_gb` of memory for at most `minutes`. Its stdin is
/// none: srun passes its own on to the command, and a script fed to bash
/// on stdin would lose its lines after it.
pub fn srun(
    run: &str,
    partition: Option<&str>,
    account: Option<&str>,
    (mem_gb, minutes): (u32, u32),
    command: &str,
) -> String {
    format!(
        "srun --job-name={} --cpus-per-task=1 --mem={mem_gb}G --time={minutes}{} {command} \
         < /dev/null",
        quote(run),
        place_flags(partition, account)
    )
}

/// `squeue` of the run's array jobs, each row a `q` line, or `qfail`.
fn squeue_line(run: &str) -> String {
    format!(
        "if q=$(squeue -h --name={} -o '%i %t'); then printf '%s\\n' \"$q\" | sed 's/^/q /'; \
         else echo qfail; fi\n",
        quote(run)
    )
}

/// Tasks of one shape go in as one array job.
fn shape(t: &Task) -> (u32, u64, u64) {
    (t.threads, t.mem_gb.to_bits(), t.timeout_secs)
}

/// The launch script's start: `set -e`, and `aside`, which reads `<task
/// folder>\t<key>` lines and moves each folder that holds an earlier
/// attempt (anything there but its placement, which the launch writes
/// over) aside whole, as the wrapper's `set_aside` does: shell builtins
/// for each task, programs only for each one moved.
fn aside_fn() -> String {
    format!(
        "set -e\nshopt -s nullglob dotglob extglob\n\
         aside() {{\n  \
         while IFS=$'\\t' read -r d k; do\n    \
         f=(\"$d\"/!({PLACED}))\n    \
         if [ ${{#f[@]}} -gt 0 ]; then\n      \
         a=\"${{d%/*}}/{ATTEMPTS}\"; b=\"${{d##*/}}\"; n=1\n      \
         while [ -e \"$a/$b.$n\" ]; do n=$((n + 1)); done\n      \
         [ -d \"$a\" ] || mkdir -p \"$a\"\n      \
         mv \"$d\" \"$a/$b.$n\"\n    \
         fi\n  \
         done\n\
         }}\n"
    )
}

/// What a launch says of each array job it submitted: `@@group <g> <id>`.
const GROUP: &str = "@@group ";

impl Executor for Slurm {
    fn resume(&mut self) -> Result<Resumed> {
        self.resumed()
    }

    fn cancel_rekeyed(&mut self) -> Result<()> {
        let stale: Vec<String> = self.stale.drain(..).map(|j| quote(&j.id())).collect();
        if stale.is_empty() {
            return Ok(());
        }
        self.shell.check(
            &format!("scancel {} || true", stale.join(" ")),
            "cancelling attempts of tasks rekeyed since",
        )?;
        Ok(())
    }

    fn live(&mut self) -> Result<Live> {
        // The scheduler places memory; the cluster's disk is not watched.
        Ok(Live::default())
    }

    fn launch(&mut self, tasks: &[Launch]) -> Result<()> {
        // Groups of one shape, in launch order, each cut at max_array.
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut open: HashMap<(u32, u64, u64), usize> = HashMap::new();
        for l in tasks {
            let key = shape(self.task(l.task)?);
            match open.get(&key) {
                Some(&g) if groups[g].len() < self.site.max_array => groups[g].push(l.task),
                _ => {
                    open.insert(key, groups.len());
                    groups.push(vec![l.task]);
                }
            }
        }
        let s = &self.site;
        let mut script = format!("{}mkdir -p {}\n", aside_fn(), quote(&s.slurm_dir));
        let mut placed = String::new();
        for (g, members) in groups.iter().enumerate() {
            let group: Vec<&Task> = members.iter().map(|&i| &self.tasks[i]).collect();
            let ids: String = group.iter().map(|t| format!("{}\n", t.id)).collect();
            // Each task's folder and key, a line each.
            let entries: Vec<String> = group
                .iter()
                .map(|t| format!("{}\t{}", t.dir.display(), t.key))
                .collect();
            let entries = entries.join("\n");
            let key = super::key::key(format!("{ids}{entries}").as_bytes());
            let name = format!("{}-{}", s.run, &key[..12]);
            let list = format!("{}/{name}.tasks", s.slurm_dir);
            // Earlier attempts moved aside, then the folders made.
            let _ = writeln!(
                script,
                "e_{g}={}\n\
                 aside <<< \"$e_{g}\"\n\
                 printf '%s\\n' \"$e_{g}\" | cut -f1 | xargs -d '\\n' mkdir -p\n\
                 printf '%s' {} > {}\n\
                 jid_{g}=$({})\njid_{g}=${{jid_{g}%%;*}}\necho \"{GROUP}{g} $jid_{g}\"",
                quote(&entries),
                quote(&ids),
                quote(&list),
                self.sbatch(group[0], group.len(), &list, &name)
            );
            // Written once every array job is in, so a launch that fails
            // leaves none naming a job it cancels.
            let _ = writeln!(
                placed,
                "i=0\nwhile IFS=$'\\t' read -r d k; do\n  \
                 printf '{{\"array_job_id\": \"%s\", \"index\": %d, \"key\": \"%s\"}}\\n' \
                 \"$jid_{g}\" \"$i\" \"$k\" > \"$d/{PLACED}\"\n  i=$((i + 1))\ndone <<< \"$e_{g}\""
            );
        }
        script.push_str(&placed);
        let ran = self.shell.run(&script)?;
        let mut jids: Vec<Option<String>> = vec![None; groups.len()];
        for line in ran.stdout.lines() {
            let Some((g, jid)) = line.strip_prefix(GROUP).and_then(|r| r.split_once(' ')) else {
                continue;
            };
            if let Some(slot) = g.parse::<usize>().ok().and_then(|g| jids.get_mut(g)) {
                *slot = Some(jid.to_string()).filter(|j| !j.is_empty());
            }
        }
        let Some(jids) = jids
            .iter()
            .cloned()
            .collect::<Option<Vec<String>>>()
            .filter(|_| ran.ok())
        else {
            // All or none: what went in is cancelled.
            let given: Vec<String> = jids.iter().flatten().map(|j| quote(j)).collect();
            if !given.is_empty() {
                let _ = self.shell.run(&format!("scancel {}", given.join(" ")));
            }
            bail!(
                "submitting {} array job(s): {}",
                groups.len(),
                ran.stderr.trim()
            );
        };
        for (members, array_job) in groups.iter().zip(jids) {
            for (index, &i) in members.iter().enumerate() {
                let job = Job {
                    array_job: array_job.clone(),
                    index,
                };
                self.watch(i, job);
            }
        }
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<Ended>> {
        if self.running.is_empty() {
            return Ok(Vec::new());
        }
        // What is queued, then the exit.json of each task that may have
        // one: every line one record.
        let mut script = squeue_line(&self.site.run);
        for (i, r) in &self.running {
            if self.pending.contains(&r.job) {
                continue;
            }
            let _ = writeln!(
                script,
                "f={}/exit.json; [ -f \"$f\" ] && printf 'x {i} %s\\n' \"$(tr -d '\\n' < \"$f\")\"",
                quote(&self.task(*i)?.dir.display().to_string())
            );
        }
        script.push_str("exit 0\n");
        let ran = self.shell.run(&script)?;
        if !ran.ok() {
            bail!("polling Slurm: {}", ran.stderr.trim());
        }
        let poll = parse_poll(&ran.stdout);
        if poll.queue_ok {
            self.pending = poll.pending;
        }
        let now = Instant::now();
        let mut ended = Vec::new();
        for (&i, r) in self.running.iter_mut() {
            // An exit.json still being written on a lagging filesystem is
            // read again next round; one that never reads is waited for
            // as if not there, so the task is lost rather than watched
            // forever.
            // One of another key is an earlier plan's attempt (a task
            // rekeyed since), not this task's end.
            if let Some(exit) = poll
                .exits
                .get(&i)
                .and_then(|text| serde_json::from_str::<TaskExit>(text).ok())
                .filter(|e| self.tasks.get(i).is_some_and(|t| t.key == e.key))
            {
                ended.push(Ended {
                    task: i,
                    outcome: exit.outcome,
                    exit: Some(exit),
                });
                continue;
            }
            if !poll.queue_ok || poll.queued.contains(&r.job) || self.pending.contains(&r.job) {
                r.gone_since = None;
                continue;
            }
            let since = *r.gone_since.get_or_insert(now);
            if now.duration_since(since) >= self.lost_after {
                let outcome = if self.stopping.contains(&i) {
                    Outcome::Stopped
                } else {
                    Outcome::Lost
                };
                ended.push(Ended::bare(i, outcome));
            }
        }
        for e in &ended {
            self.running.remove(&e.task);
            self.stopping.remove(&e.task);
        }
        Ok(ended)
    }

    fn cancel(&mut self, tasks: &[usize]) -> Result<()> {
        // Only tasks running here are stopping: one marked otherwise would
        // end as stopped, not lost, on a later launch.
        let (ids, jobs): (Vec<usize>, Vec<String>) = tasks
            .iter()
            .filter_map(|&i| self.running.get(&i).map(|r| (i, quote(&r.job.id()))))
            .unzip();
        if jobs.is_empty() {
            return Ok(());
        }
        // A job already gone from the queue is no error: it ends by its
        // exit.json.
        self.shell
            .check(&format!("scancel {} || true", jobs.join(" ")), "scancel")?;
        self.stopping.extend(ids);
        Ok(())
    }
}

/// A poll's output.
#[derive(Default)]
struct Poll {
    /// The array tasks in the queue that are not pending.
    queued: HashSet<Job>,
    /// The pending ones.
    pending: HashSet<Job>,
    /// Each task's `exit.json` text, by task index.
    exits: HashMap<usize, String>,
    /// `squeue` answered.
    queue_ok: bool,
}

/// `q <job> <state>` lines from `squeue`, or `qfail`; `x <task> <json>`
/// lines, each a task's `exit.json`; other lines are not the poll's.
fn parse_poll(out: &str) -> Poll {
    let mut p = Poll {
        queue_ok: true,
        ..Poll::default()
    };
    for line in out.lines() {
        if line == "qfail" {
            p.queue_ok = false;
        } else if let Some(row) = line.strip_prefix("q ") {
            let mut words = row.split_whitespace();
            let Some(job) = words.next() else {
                continue;
            };
            if let Some(range) = parse_range(job) {
                p.pending.extend(range);
            } else if let Ok(job) = parse_job(job) {
                if words.next() == Some("PD") {
                    p.pending.insert(job);
                } else {
                    p.queued.insert(job);
                }
            }
        } else if let Some((i, json)) = line.strip_prefix("x ").and_then(|r| r.split_once(' ')) {
            if let Ok(i) = i.parse() {
                p.exits.insert(i, json.to_string());
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_whose_memory_is_not_counted_asks_for_none() {
        // Not the 1 MB that Slurm would kill it at.
        assert_eq!(mem_flag(0.0), "");
        assert_eq!(mem_flag(1.0), " --mem=1024M");
        assert_eq!(mem_flag(2.5), " --mem=2560M");
    }

    fn job(array_job: &str, index: usize) -> Job {
        Job {
            array_job: array_job.into(),
            index,
        }
    }

    #[test]
    fn a_poll_reads_the_queue_and_each_exit() {
        let out = "q 10_0 R\nq 10_1 PD\nq 10_[2-3,5%2] PD\nq 11 R\nx 3 {\"exit\": 0}\nx 4\n";
        let p = parse_poll(out);
        assert!(p.queue_ok);
        assert_eq!(p.queued, HashSet::from([job("10", 0)]));
        assert_eq!(
            p.pending,
            HashSet::from([job("10", 1), job("10", 2), job("10", 3), job("10", 5)])
        );
        assert_eq!(p.exits[&3], "{\"exit\": 0}");
        assert!(!p.exits.contains_key(&4));
        assert!(!parse_poll("qfail\n").queue_ok);
    }

    #[test]
    fn job_ids_are_an_array_job_and_index() {
        assert_eq!(parse_job("39_7").unwrap(), job("39", 7));
        for bad in ["39", "39_[1-3]", "a_1", "_1"] {
            assert!(parse_job(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_range("39_[4]").unwrap(), vec![job("39", 4)]);
        assert!(parse_range("39_4").is_none() && parse_range("39_[a-2]").is_none());
    }
}
