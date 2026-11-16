//! The Slurm executor: a benchmark's tasks run as Slurm array tasks on a
//! cluster, each under the wrapper (`legume benchmark task`), driven
//! through a [`Shell`] from wherever the runner is.
//!
//! - **Launch.** Tasks of one shape (threads, memory, timeout) go in as
//!   one array job, at most `max_array` tasks each. A list of their ids
//!   is written beside the benchmark, named by the run and its content
//!   (`slurm/<run>-<key>.tasks`, so a list an earlier array job still
//!   reads is never rewritten with other lines), and array task `i` runs
//!   the task on line `i`. Every array job is named for the run, so the
//!   runner finds its own and no one else's. Once every array job is in,
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

use super::executor::{Executor, TaskExit};
use super::plan::Task;
use super::pool::{Budget, Launch, Live, Outcome};
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
    budget: Budget,
    running: BTreeMap<usize, Running>,
    /// The array tasks the last poll saw pending, whose files are not
    /// looked at.
    pending: HashSet<Job>,
    /// Tasks a person stopped: one lost is stopped, not lost.
    stopping: HashSet<usize>,
    /// How long a task gone from the queue without an `exit.json` is
    /// waited for (a shared filesystem may show a new file late).
    pub lost_after: Duration,
}

impl Slurm {
    /// The executor of the plan's `tasks`, with its `grace_secs`, at
    /// `site`, through `shell`, booking at most `budget` at once.
    pub fn new(
        shell: Shell,
        site: Site,
        tasks: Vec<Task>,
        grace_secs: u64,
        budget: Budget,
    ) -> Result<Self> {
        if site.max_array == 0 {
            bail!("max_array is at least 1");
        }
        Ok(Slurm {
            shell,
            site,
            tasks,
            grace_secs,
            budget,
            running: BTreeMap::new(),
            pending: HashSet::new(),
            stopping: HashSet::new(),
            lost_after: Duration::from_secs(60),
        })
    }

    /// On resume: task `task` is array task `job` (`<array job>_<index>`)
    /// of an earlier run, still queued or running, watched as if launched.
    pub fn adopt(&mut self, task: usize, job: &str) -> Result<()> {
        let job = parse_job(job).with_context(|| format!("adopting {job}"))?;
        self.task(task)?;
        self.running.insert(
            task,
            Running {
                job,
                gone_since: None,
            },
        );
        Ok(())
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
        let mem_mb = (t.mem_gb * 1024.0).ceil().max(1.0) as u64;
        let wrap = format!(
            "exec {} benchmark task --plan {} --list {}",
            quote(&s.legume),
            quote(&s.plan),
            quote(list)
        );
        let mut args = format!(
            "sbatch --parsable --job-name={} --array=0-{} --cpus-per-task={} --mem={mem_mb}M \
             --time={minutes} --output={} --wrap={}",
            quote(&s.run),
            n - 1,
            t.threads,
            quote(&format!("{}/{name}_%a.log", s.slurm_dir)),
            quote(&wrap)
        );
        for (flag, v) in [("--partition", &s.partition), ("--account", &s.account)] {
            if let Some(v) = v {
                let _ = write!(args, " {flag}={}", quote(v));
            }
        }
        args
    }
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

/// Tasks of one shape go in as one array job.
fn shape(t: &Task) -> (u32, u64, u64) {
    (t.threads, t.mem_gb.to_bits(), t.timeout_secs)
}

/// What a launch says of each array job it submitted: `@@group <g> <id>`.
const GROUP: &str = "@@group ";

impl Executor for Slurm {
    fn budget(&self) -> Budget {
        self.budget.clone()
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
        let mut script = format!("set -e\nmkdir -p {}\n", quote(&s.slurm_dir));
        let mut placed = String::new();
        for (g, members) in groups.iter().enumerate() {
            let group: Vec<&Task> = members.iter().map(|&i| &self.tasks[i]).collect();
            let ids: String = group.iter().map(|t| format!("{}\n", t.id)).collect();
            let dirs: String = group
                .iter()
                .map(|t| format!("{}\n", t.dir.display()))
                .collect();
            let key = super::key::key(format!("{ids}{dirs}").as_bytes());
            let name = format!("{}-{}", s.run, &key[..12]);
            let list = format!("{}/{name}.tasks", s.slurm_dir);
            // The folders made and a last attempt's ends cleared: a few
            // programs for the group, not some for each task.
            let _ = writeln!(
                script,
                "dirs_{g}=$(cat <<'@@dirs'\n{dirs}@@dirs\n)\n\
                 printf '%s\\n' \"$dirs_{g}\" | xargs -d '\\n' mkdir -p\n\
                 printf '%s\\n' \"$dirs_{g}\" | \
                 sed 's|$|/exit.json|;p;s|exit.json$|done.json|;p;s|done.json$|slurm.json|' | \
                 xargs -d '\\n' rm -f\n\
                 printf '%s' {} > {}\n\
                 jid_{g}=$({})\njid_{g}=${{jid_{g}%%;*}}\necho \"{GROUP}{g} $jid_{g}\"",
                quote(&ids),
                quote(&list),
                self.sbatch(group[0], group.len(), &list, &name)
            );
            // Written once every array job is in, so a launch that fails
            // leaves none naming a job it cancels.
            let _ = writeln!(
                placed,
                "i=0\nwhile IFS= read -r d; do\n  \
                 printf '{{\"array_job_id\": \"%s\", \"index\": %d}}\\n' \"$jid_{g}\" \"$i\" \
                 > \"$d/slurm.json\"\n  i=$((i + 1))\ndone <<< \"$dirs_{g}\""
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
                self.running.insert(
                    i,
                    Running {
                        job,
                        gone_since: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn poll(&mut self, wait: Duration) -> Result<Vec<(usize, Outcome)>> {
        if self.running.is_empty() {
            return Ok(Vec::new());
        }
        // One look after `wait`: the runner sets how often.
        std::thread::sleep(wait);
        // What is queued, then the exit.json of each task that may have
        // one: every line one record.
        let mut script = format!(
            "if q=$(squeue -h --name={} -o '%i %t'); then printf '%s\\n' \"$q\" | sed 's/^/q /'; \
             else echo qfail; fi\n",
            quote(&self.site.run)
        );
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
            if let Some(exit) = poll
                .exits
                .get(&i)
                .and_then(|text| serde_json::from_str::<TaskExit>(text).ok())
            {
                ended.push((i, exit.outcome));
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
                ended.push((i, outcome));
            }
        }
        for (i, _) in &ended {
            self.running.remove(i);
            self.stopping.remove(i);
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
        self.shell
            .check(&format!("scancel {}", jobs.join(" ")), "scancel")?;
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
/// lines, each a task's `exit.json`.
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
