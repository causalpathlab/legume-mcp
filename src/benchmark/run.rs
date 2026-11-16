//! `legume benchmark run SPEC --on REMOTE`: a benchmark run on a cluster
//! from this machine, in one command. The remote is asked for once and
//! kept in this person's `remotes.toml` ([`super::remote`]). Then, over
//! one SSH connection:
//!
//! 1. the spec goes to the project's folder on the cluster, and is planned
//!    there on a compute node (`srun legume benchmark plan`), so tools
//!    are found and snapshot, and inputs keyed, where they run;
//! 2. tasks done in an earlier run (their `done.json` has their key) are
//!    reused, and tasks of an earlier run still in Slurm's queue adopted;
//! 3. the pool here decides what starts, and the Slurm executor
//!    ([`super::slurm`]) runs it as array jobs, each task under the
//!    wrapper, one SSH call per round;
//! 4. each start and end is a row of `events.tsv` in the benchmark's
//!    folder here;
//! 5. at the end, the scores of the tasks done are copied here, with
//!    plan.json, and gathered ([`super::gather`]).
//!
//! Ctrl-C cancels the run's array tasks, waits for them to say they
//! stopped, and exits 130; a run again resumes. The run exits 0 when
//! every task is done, 1 when any is not.

use super::executor::{round, unix_secs, Ended, Executor, Round, TaskExit};
use super::plan::{PlanDoc, Task};
use super::pool::{Budget, Pool, State, Want};
use super::remote::{self, Remote, Shell};
use super::slurm::{srun, Site, Slurm};
use crate::command::{canonical_flags, failed, flag_pairs, refuse, required, usage, Flag};
use crate::project::quote;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `legume benchmark run`'s usage.
pub const USAGE: &str = "\
legume benchmark run SPEC --on REMOTE

Runs the benchmark of the spec SPEC on the cluster REMOTE, from here:
the spec is planned on a compute node there, and each task runs as a
Slurm array task under the wrapper, as many at once as Slurm and each
method's max_running allow. A remote not in remotes.toml (in
~/.config/legume, or $LEGUME_REMOTES) is asked for and kept there: its
SSH target, the folder on the cluster for benchmarks, the folder of
legume's tools there, and a Slurm partition and account. Tasks done in
an earlier run are reused, and tasks still in Slurm's queue adopted.
Each start and end is a row of results/benchmarks/<name>/events.tsv
here. At the end, the scores of the tasks done are copied here and
gathered into summary.tsv and the rest (legume benchmark gather --help).
Ctrl-C cancels the run's tasks; run again to resume.";

const FLAGS: &[Flag] = &[Flag::once(&["--on"])];

/// Cores the pool may book at once on a cluster: Slurm's scheduler and
/// each method's max_running are the real limits.
const CLUSTER_CORES: u32 = 100_000;

/// How long a round waits when something changed, growing by
/// [`WAIT_STEP`] each quiet round up to [`WAIT_MAX`].
const WAIT_FIRST: Duration = Duration::from_secs(40);
const WAIT_STEP: Duration = Duration::from_secs(10);
const WAIT_MAX: Duration = Duration::from_secs(180);
/// How long a round waits once the run is stopping.
const WAIT_STOPPING: Duration = Duration::from_secs(10);

/// Rounds in a row the cluster may leave unanswered before the run ends.
const MAX_FAILURES: u32 = 5;

/// A fixed wait between rounds, in ms, for tests.
const POLL_ENV: &str = "LEGUME_BENCHMARK_POLL_MS";

/// `legume benchmark run` with `args` (those after `run`).
pub fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let (spec, rest) = match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => return Ok(writeln!(out, "{USAGE}")?),
        Some(s) if s.starts_with('-') => (None, args),
        Some(s) => (Some(PathBuf::from(s)), &args[1..]),
    };
    let Some(pairs) = flag_pairs(rest)? else {
        return Ok(writeln!(out, "{USAGE}")?);
    };
    let mut on = None;
    for (_, v) in canonical_flags(FLAGS, pairs)? {
        on = Some(v.to_string());
    }
    let spec = spec.ok_or_else(|| usage("run needs SPEC, the spec's JSON file"))?;
    let on = required(on, "--on")?;
    let code = run(&spec, &on, out, err)?;
    if code == 0 {
        Ok(())
    } else {
        Err(failed(code, "not every task is done"))
    }
}

/// The run of spec `spec_file` on remote `on`: its exit code.
fn run(spec_file: &Path, on: &str, out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let text = std::fs::read_to_string(spec_file)
        .with_context(|| format!("reading {}", spec_file.display()))?;
    let (spec, value) = super::spec::parse(&text)?;
    let spec_key = super::key::key(super::key::spec_file(&value).as_bytes());
    let root = crate::project::root().context("finding the project's folder")?;
    // Named for the project and its place here, so two projects of one
    // name keep apart there.
    let project = format!(
        "{}-{}",
        root.file_name()
            .map_or("project".into(), |n| n.to_string_lossy()),
        &super::key::key(root.display().to_string().as_bytes())[..8]
    );
    let remote = remote::named(on, out)?;
    let shell = Shell::ssh(&remote.ssh);
    let c = Cluster {
        dir: format!("{}/{project}", remote.folder.trim_end_matches('/')),
        legume: remote.bin.as_ref().map_or("legume".into(), |b| {
            format!("{}/legume", b.trim_end_matches('/'))
        }),
        remote: &remote,
        shell: &shell,
    };
    let local = root.join(super::bench_rel(&spec.name));
    let result = c.run(&spec, &spec_key, &text, &local, out, err);
    shell.close();
    result
}

/// What starts the planning call's output when the cluster's install
/// has another legume, before its version.
const LEGUME_MARK: &str = "@@legume ";
/// The line before plan.json in the planning call's output.
const PLAN_MARK: &str = "@@plan.json";

/// What a run needs of the cluster, as shell words.
struct Cluster<'a> {
    remote: &'a Remote,
    shell: &'a Shell,
    /// The project's folder there.
    dir: String,
    /// `legume` there.
    legume: String,
}

impl Cluster<'_> {
    /// `PATH` with the remote's tools first, as a shell assignment, or
    /// nothing.
    fn path(&self) -> String {
        self.remote
            .bin
            .as_ref()
            .map_or(String::new(), |b| format!("PATH={}:\"$PATH\" ", quote(b)))
    }

    /// The benchmark's folder there.
    fn bench(&self, name: &str) -> String {
        format!("{}/{}", self.dir, super::bench_rel(name).display())
    }

    /// `spec`, whose text is `text`, sent to the project's folder there and
    /// planned on a compute node as job `run`, in one call: what the plan
    /// said, and its plan.json, as text and read.
    fn plan(
        &self,
        spec: &super::spec::Spec,
        text: &str,
        run: &str,
    ) -> Result<(String, String, PlanDoc)> {
        let d = quote(&self.dir);
        let s = quote(&format!("specs-in/{}.json", spec.name));
        let plan = srun(
            run,
            self.remote.partition.as_deref(),
            self.remote.account.as_deref(),
            (4, 60),
            &format!("{} benchmark plan {s}", quote(&self.legume)),
        );
        // An install's own record of its legume, read before the planning
        // job waits in the queue, which a legume of another version would
        // only be refused after.
        let installed = self.remote.prefix().map_or(String::new(), |p| {
            format!(
                "v=$(grep -o '\"legume-mcp [^ ]*' {}/.crates2.json 2> /dev/null | head -1 | cut -d' ' -f2)\n\
                 if [ -n \"$v\" ] && [ \"$v\" != {here} ]; then echo \"{LEGUME_MARK}$v\"; exit 0; fi\n",
                quote(p),
                here = env!("CARGO_PKG_VERSION"),
            )
        });
        let said = self.shell.check(
            &format!(
                "set -e\n{installed}mkdir -p {d}/specs-in\nprintf '%s\\n' {} > {d}/{s}\n\
                 cd {d}\n{path}LEGUME_MCP_PROJECT={d} {plan}\necho {PLAN_MARK}\ncat {}/plan.json\n",
                quote(text),
                quote(&self.bench(&spec.name)),
                path = self.path(),
            ),
            "planning on the cluster",
        )?;
        if let Some((_, there)) = said.split_once(LEGUME_MARK) {
            return Err(self.other_legume(Some(there.trim())));
        }
        let (said, plan) = said
            .split_once(&format!("{PLAN_MARK}\n"))
            .context("planning on the cluster: no plan.json")?;
        let doc: PlanDoc = serde_json::from_str(plan).context("reading plan.json")?;
        if doc.legume.as_deref() != Some(env!("CARGO_PKG_VERSION")) {
            return Err(self.other_legume(doc.legume.as_deref()));
        }
        Ok((said.to_string(), plan.to_string(), doc))
    }

    /// The refusal of a cluster whose legume is `there`, not this one.
    fn other_legume(&self, there: Option<&str>) -> anyhow::Error {
        refuse(format!(
            "the cluster's legume is {}, and this one {}: legume benchmark install --on {} \
             installs this one there",
            there.unwrap_or("older"),
            env!("CARGO_PKG_VERSION"),
            self.remote.name
        ))
    }

    /// The benchmark of `spec`, of key `spec_key`, whose text is `text`,
    /// run, its events kept and its scores gathered in `local`: its exit
    /// code.
    fn run(
        &self,
        spec: &super::spec::Spec,
        spec_key: &str,
        text: &str,
        local: &Path,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> Result<i32> {
        // The run's name in Slurm, the same for every run of this benchmark
        // here, so a run again finds the last one's array jobs; it says
        // nothing of the person or the place.
        let key = super::key::key(format!("{}\n{}", self.dir, spec.name).as_bytes());
        let run = format!("legume-{}", &key[..12]);
        writeln!(
            out,
            "planning {} on {} (a compute node)",
            spec.name, self.remote.name
        )?;
        let (said, plan_text, plan) = self.plan(spec, text, &run)?;
        write!(out, "{said}")?;
        if let Some(t) = plan.tasks.iter().find(|t| t.gpus > 0) {
            return Err(refuse(format!(
                "{} wants GPUs, which a run on a cluster does not book yet",
                t.id
            )));
        }
        // plan.json here from the start, so a status read while the run
        // goes on knows every task; the end writes it again with the scores.
        std::fs::create_dir_all(local).with_context(|| format!("making {}", local.display()))?;
        super::gather::write(local, "plan.json", &plan_text)?;
        let bench = self.bench(&spec.name);
        let site = Site {
            legume: self.legume.clone(),
            plan: format!("{bench}/plan.json"),
            slurm_dir: format!("{bench}/slurm"),
            run,
            partition: self.remote.partition.clone(),
            account: self.remote.account.clone(),
            max_array: 1000,
        };
        let stages: Vec<&str> = spec.stages.iter().map(|s| s.name.as_str()).collect();
        let wants = Want::of_plan(&plan.tasks, &stages)?;
        let budget = Budget {
            cores: CLUSTER_CORES,
            mem_gb: f64::from(CLUSTER_CORES) * 64.0,
            gpus: Vec::new(),
            gpu_slots: 0,
        };
        let ex = Slurm::new(
            self.shell.clone(),
            site,
            plan.tasks.clone(),
            plan.grace_secs,
        );
        let pool = Pool::new(wants, budget)?;
        let mut r = Runner {
            skipped_said: vec![false; plan.tasks.len()],
            tasks: &plan.tasks,
            pool,
            ex,
            events: Events::open(local)?,
            submitted: HashMap::new(),
            started: Instant::now(),
        };
        r.resume(out)?;
        let code = r.drive(err)?;
        let code = summary(&r.pool, plan.tasks.len(), code, local, out)?;
        // A run stopped by hand ends now; the next run gathers.
        if super::stop::asked() {
            return Ok(code);
        }
        match self.gather(spec, spec_key, &plan_text, &plan, local) {
            Ok(g) => g.say(
                &super::bench_rel(&spec.name).display().to_string(),
                out,
                err,
            )?,
            Err(e) => writeln!(err, "warning: gathering: {e:#}")?,
        }
        Ok(code)
    }

    /// The scores of `plan`'s scored tasks done there and not yet here,
    /// with their `done.json`, copied to their places under `local` in one
    /// call, and `plan_text` as its plan.json: then gathered there.
    fn gather(
        &self,
        spec: &super::spec::Spec,
        spec_key: &str,
        plan_text: &str,
        plan: &PlanDoc,
        local: &Path,
    ) -> Result<super::gather::Gathered> {
        let mut wanted = Vec::new();
        for (t, f) in super::gather::scored(plan) {
            let here = plan
                .task_dir(local, t)
                .with_context(|| format!("{}: its folder is not in the plan's", t.id))?;
            if !t.done_in(&here) {
                wanted.push((t, f, here));
            }
        }
        let there = |t: &Task, name: &str| t.dir.join(name).display().to_string();
        let paths: Vec<String> = wanted
            .iter()
            .flat_map(|(t, f, _)| [there(t, f), there(t, "done.json")])
            .collect();
        let got = self.shell.fetch(&paths, "fetching the scores")?;
        super::gather::write(local, "plan.json", plan_text)?;
        for (t, f, here) in &wanted {
            // The score before done.json, which says it is whole.
            for name in [*f, "done.json"] {
                match got.get(&there(t, name)) {
                    Some(bytes) => {
                        std::fs::create_dir_all(here)
                            .with_context(|| format!("making {}", here.display()))?;
                        super::gather::write(here, name, bytes)?;
                    }
                    None => {
                        let _ = std::fs::remove_file(here.join(name));
                    }
                }
            }
        }
        super::gather::gather(local, spec, spec_key, plan)
    }
}

/// The pool, the executor and the event log of one run.
struct Runner<'a> {
    tasks: &'a [Task],
    pool: Pool,
    ex: Slurm,
    events: Events,
    /// When each task launched here was submitted, in seconds since 1970.
    submitted: HashMap<usize, f64>,
    /// The tasks whose skip events.tsv has.
    skipped_said: Vec<bool>,
    started: Instant,
}

impl Runner<'_> {
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// What an earlier run left: tasks done reused, tasks still queued
    /// adopted.
    fn resume(&mut self, out: &mut dyn Write) -> Result<()> {
        let resumed = self.ex.resume()?;
        self.pool.resume(0, &resumed.done, &resumed.adopted)?;
        for (list, event) in [(&resumed.done, "reused"), (&resumed.adopted, "adopted")] {
            for &i in list {
                self.events.mark(&self.tasks[i], event)?;
            }
        }
        self.events.flush()?;
        writeln!(
            out,
            "{} tasks: {} done before, {} still in the queue",
            self.tasks.len(),
            resumed.done.len(),
            resumed.adopted.len()
        )?;
        Ok(())
    }

    /// Rounds until nothing runs: `None` when the run ended so, or the
    /// exit code of a run that leaves early.
    fn drive(&mut self, err: &mut dyn Write) -> Result<Option<i32>> {
        super::stop::catch();
        let fixed = std::env::var(POLL_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis);
        let mut wait = Duration::ZERO;
        let mut stopping = false;
        let mut failures = 0;
        loop {
            sleep_unless_stopped(wait, stopping);
            if super::stop::asked_times() > 1 {
                writeln!(
                    err,
                    "stopped again: leaving now; tasks not seen to end may still run on \
                     the cluster, and a run again adopts them"
                )?;
                return Ok(Some(130));
            }
            // Checked after the wait, so nothing launches once a stop is
            // asked; a drained pool launches nothing more.
            if super::stop::asked() && !stopping {
                stopping = true;
                self.stop(err)?;
            }
            let started = self.started;
            let mut clock = || started.elapsed().as_millis() as u64;
            let round = round(&mut self.pool, &mut self.ex, &mut clock);
            self.record(&round)?;
            // A round the cluster did not answer (a dropped connection) is
            // tried again: what it would have launched is pending again.
            if let Some(e) = round.error {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return Err(e.context(format!(
                        "the cluster did not answer {MAX_FAILURES} rounds in a row; its tasks \
                         keep running, and a run again adopts them"
                    )));
                }
                writeln!(err, "warning: {e:#}; trying again")?;
                wait = fixed.unwrap_or(WAIT_FIRST);
                continue;
            }
            failures = 0;
            if self.pool.running().next().is_none() {
                return Ok(stopping.then_some(130));
            }
            let changed = !round.ended.is_empty() || !round.launched.is_empty();
            wait = match fixed {
                Some(f) => f,
                // Stopped tasks end within the wrapper's stop grace.
                None if stopping => WAIT_STOPPING,
                None if changed => WAIT_FIRST,
                None => (wait + WAIT_STEP).clamp(WAIT_FIRST, WAIT_MAX),
            };
        }
    }

    /// A person's stop: nothing more starts, and the running tasks are
    /// cancelled.
    fn stop(&mut self, err: &mut dyn Write) -> Result<()> {
        self.pool.drain(self.now());
        let running: Vec<usize> = self.pool.running().collect();
        writeln!(
            err,
            "stopping: cancelling {} running task(s)",
            running.len()
        )?;
        for &i in &running {
            self.events.mark(&self.tasks[i], "stop-requested")?;
        }
        self.events.flush()?;
        self.ex.cancel(&running)
    }

    /// `round`'s ends and starts in events.tsv, and the skips they caused.
    fn record(&mut self, round: &Round) -> Result<()> {
        for Ended {
            task,
            outcome,
            exit,
        } in &round.ended
        {
            let at = self.submitted.get(task).copied();
            self.events
                .row(&self.tasks[*task], outcome.word(), exit.as_ref(), at)?;
        }
        let at = unix_secs();
        for l in &round.launched {
            self.submitted.insert(l.task, at);
            self.events.mark(&self.tasks[l.task], "start")?;
        }
        // A task is skipped only when an end (its upstream's, or its
        // method's halt) says so.
        if !round.ended.is_empty() {
            for (i, said) in self.skipped_said.iter_mut().enumerate() {
                if !*said && matches!(self.pool.state(i), State::Skipped { .. }) {
                    *said = true;
                    self.events.mark(&self.tasks[i], "skipped")?;
                }
            }
        }
        self.events.flush()
    }
}

/// `wait`, cut short by a stop (once stopping, by another).
fn sleep_unless_stopped(wait: Duration, stopping: bool) {
    let until = Instant::now() + wait;
    let stops = u32::from(stopping);
    while Instant::now() < until && super::stop::asked_times() <= stops {
        std::thread::sleep(Duration::from_millis(100).min(until - Instant::now()));
    }
}

/// How the run ended, said on `out`: its exit code, `code` when it left
/// early.
fn summary(
    pool: &Pool,
    n: usize,
    code: Option<i32>,
    local: &Path,
    out: &mut dyn Write,
) -> Result<i32> {
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for i in 0..n {
        let word = match pool.state(i) {
            State::Ended(o) => o.word(),
            State::Skipped { .. } => "skipped",
            State::Pending => "not run",
            State::Running(_) => "running",
        };
        *counts.entry(word).or_default() += 1;
    }
    let said: Vec<String> = counts.iter().map(|(w, n)| format!("{n} {w}")).collect();
    writeln!(out, "{n} tasks: {}", said.join(", "))?;
    writeln!(out, "events: {}", local.join("events.tsv").display())?;
    Ok(code.unwrap_or(if counts.get("done") == Some(&n) { 0 } else { 1 }))
}

/// `events.tsv`: a row per start and end of each task, appended.
struct Events {
    file: std::io::BufWriter<std::fs::File>,
}

const EVENTS_HEADER: &str = "time\ttask\tattempt\tevent\twall_secs\tcpu_secs\tqueue_secs\tnode\n";

impl Events {
    fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
        let path = dir.join("events.tsv");
        // The header with the file, only by whoever makes it.
        let file = match std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(EVENTS_HEADER.as_bytes())?;
                f
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::OpenOptions::new().append(true).open(&path)?
            }
            Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
        };
        Ok(Events {
            file: std::io::BufWriter::new(file),
        })
    }

    /// Task `t`'s `event`, with nothing more to say.
    fn mark(&mut self, t: &Task, event: &str) -> Result<()> {
        self.row(t, event, None, None)
    }

    /// Task `t`'s `event`, its attempt's `exit` when it ended by one, and
    /// its time in the queue when it was `submitted` here (seconds since
    /// 1970).
    fn row(
        &mut self,
        t: &Task,
        event: &str,
        exit: Option<&TaskExit>,
        submitted: Option<f64>,
    ) -> Result<()> {
        let (attempt, wall, cpu, node, queue) = match exit {
            Some(e) => (
                e.attempt.to_string(),
                format!("{:.1}", e.secs),
                format!("{:.1}", e.cpu_secs),
                crate::project::one_line(&e.node),
                submitted.map_or(String::new(), |at| {
                    format!("{:.1}", (e.started - at).max(0.0))
                }),
            ),
            None => Default::default(),
        };
        writeln!(
            self.file,
            "{}\t{}\t{attempt}\t{event}\t{wall}\t{cpu}\t{queue}\t{node}",
            crate::project::rfc3339(),
            crate::project::one_line(&t.id)
        )?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(self.file.flush()?)
    }
}
