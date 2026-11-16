//! The pool: which of a benchmark's tasks may start now. It is pure: no
//! process, clock or file. The runner tells it the time (in ms), what the
//! machine has free, and when each task ends; it answers with the tasks
//! to launch and which devices each gets. Where a task runs is the
//! executor's business ([`super::executor`]).
//!
//! - **Resources.** The threads of running tasks never sum past `cores`,
//!   nor their `mem_gb` past `mem_gb`; each GPU device holds at most
//!   `gpu_slots` tasks, and a task gets distinct devices. A method's
//!   `max_running` caps its running tasks; a stage's is each of its
//!   methods' default, as for `threads`.
//! - **Memory floor.** With a reading of the machine's available memory,
//!   a task starts only if what running tasks may still grow by (their
//!   `mem_gb` less their resident size), plus its own `mem_gb`, plus 4 GB,
//!   is available.
//! - **Order.** Deepest stage first, then rep-major, then plan order. A
//!   task that does not fit is passed over (backfill). One held by cores,
//!   memory, a GPU or the disk that has waited [`Pool::starve_ms`] while
//!   the benchmark's tasks run reserves what they free: nothing behind it
//!   starts until it fits, and the cores held for it count as `no_fit`.
//! - **Halts.** A method halts when its first 3 counted attempts all
//!   fail, or once at least 10 have ended and at least half failed. A
//!   stage without methods counts as one method of its own, unnamed. A
//!   person's stop is not counted. A halted method's pending tasks are
//!   skipped. The benchmark halts when every method of a stage has
//!   halted; then nothing starts.
//! - **Downstream** of a task that did not end done is skipped.
//! - **Accounting**, in core-ms between events: `booked` (threads
//!   running) and the free cores, as `slack` when a ready task fits but
//!   waits for the next round, `no_fit` when a ready task lacks only
//!   cores, `capped` when one is held by a method cap, a drain or a halt,
//!   `blocked` when one is held by memory, a GPU or the disk, and `idle`
//!   when no task is ready. The first of these that applies names the
//!   free cores.

use super::plan::Task;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Free memory kept beyond what running tasks may still grow by, in GB.
pub const FLOOR_GB: f64 = 4.0;

/// How long a task may be passed over before nothing behind it starts.
pub const STARVE_MS: u64 = 10 * 60 * 1000;

/// One task as the pool sees it.
#[derive(Clone, Debug)]
pub struct Want {
    pub id: String,
    /// The stage's depth: its index in the spec.
    pub stage: usize,
    pub stage_name: String,
    /// The method, for a stage with methods.
    pub method: Option<String>,
    pub rep: u32,
    pub threads: u32,
    pub mem_gb: f64,
    pub gpus: u32,
    pub max_running: Option<u32>,
    /// The tasks (indices) this one reads.
    pub upstream: Vec<usize>,
}

impl Want {
    /// The pool's view of `tasks`, a plan, whose stages in spec order are
    /// `stages`: why not, when a task names a stage not in `stages` or
    /// reads a task not in the plan.
    pub fn of_plan(tasks: &[Task], stages: &[&str]) -> Result<Vec<Self>> {
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (i, t) in tasks.iter().enumerate() {
            if index.insert(t.id.as_str(), i).is_some() {
                bail!("{}: two tasks have this id", t.id);
            }
        }
        tasks
            .iter()
            .map(|t| {
                let Some(stage) = stages.iter().position(|s| *s == t.stage) else {
                    bail!("{}: stage {} is not in the spec", t.id, t.stage);
                };
                let upstream = t
                    .upstream
                    .iter()
                    .map(|u| match index.get(u.as_str()) {
                        Some(&i) => Ok(i),
                        None => bail!("{}: reads {u}, which is not in the plan", t.id),
                    })
                    .collect::<Result<_>>()?;
                Ok(Self {
                    id: t.id.clone(),
                    stage,
                    stage_name: t.stage.clone(),
                    method: t.method.clone(),
                    rep: t.rep,
                    threads: t.threads,
                    mem_gb: t.mem_gb,
                    gpus: t.gpus,
                    max_running: t.max_running,
                    upstream,
                })
            })
            .collect()
    }
}

/// What the pool may book.
#[derive(Clone, Debug)]
pub struct Budget {
    pub cores: u32,
    pub mem_gb: f64,
    /// GPU device ids.
    pub gpus: Vec<u32>,
    /// Tasks a GPU device holds at once.
    pub gpu_slots: u32,
}

/// The machine now.
#[derive(Clone, Debug, Default)]
pub struct Live {
    /// Available memory, when the executor reads it; `None` leaves memory
    /// to the executor's own scheduler.
    pub mem_available_gb: Option<f64>,
    /// Free disk is under the spec's floor.
    pub disk_low: bool,
}

/// A task to launch, and the GPU devices it gets.
#[derive(Clone, Debug, PartialEq)]
pub struct Launch {
    pub task: usize,
    pub gpus: Vec<u32>,
}

/// How a task ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Done,
    Failed,
    Timeout,
    Oom,
    /// The executor lost it: gone without a word from its wrapper.
    Lost,
    /// A person stopped it; not counted against its method.
    Stopped,
}

impl Outcome {
    /// Its name, as `exit.json` and `events.tsv` write it.
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Failed => "failed",
            Outcome::Timeout => "timeout",
            Outcome::Oom => "oom",
            Outcome::Lost => "lost",
            Outcome::Stopped => "stopped",
        }
    }
}

/// Where a task is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Pending,
    /// On these GPU devices.
    Running(Vec<u32>),
    Ended(Outcome),
    /// Never run: `upstream` is the task whose end or skip caused it, or
    /// `None` when its own method halted.
    Skipped {
        upstream: Option<usize>,
    },
}

impl State {
    fn is_running(&self) -> bool {
        matches!(self, State::Running(_))
    }
}

/// Core-ms by class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Acct {
    pub booked: u64,
    pub slack: u64,
    pub no_fit: u64,
    pub capped: u64,
    pub blocked: u64,
    pub idle: u64,
}

/// Why a ready task does not start, best first: the free cores take the
/// class of the best hold among ready tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Hold {
    Fits,
    /// It lacks only cores, or they are reserved for a starved task: the
    /// free cores are `no_fit`.
    Cores,
    Capped,
    Blocked,
    Nothing,
}

/// A method's cap and tally: a stage's method, or a stage without
/// methods, which counts as one method of its own, unnamed.
#[derive(Clone, Debug)]
struct MethodTally {
    stage: String,
    /// `None` for a stage without methods.
    name: Option<String>,
    max_running: Option<u32>,
    /// Counted attempts: ended, and of them failed.
    ended: u32,
    failed: u32,
    halted: bool,
}

/// The pool over one plan.
#[derive(Debug)]
pub struct Pool {
    wants: Vec<Want>,
    state: Vec<State>,
    budget: Budget,
    /// Each task's method, an index into `methods`.
    method_of: Vec<usize>,
    methods: Vec<MethodTally>,
    /// Tasks reading each task.
    downstream: Vec<Vec<usize>>,
    /// Every task, best first: deepest stage, then rep, then plan order.
    order: Vec<usize>,
    /// Resident GB of running tasks, as last read.
    rss: HashMap<usize, f64>,
    /// When each task was first seen ready and not started; cleared when
    /// it ends or is adopted.
    ready_since: HashMap<usize, u64>,
    draining: bool,
    halted: Option<String>,
    live: Live,
    /// How long a task may be passed over; [`STARVE_MS`] by default.
    pub starve_ms: u64,
    acct: Acct,
    /// When the accounting last advanced, and the class of free cores
    /// since then.
    at: u64,
    hold: Hold,
}

impl Pool {
    /// The pool over `wants` with `budget`: why not, when a task asks more
    /// than the budget, more GPUs than there are devices, reads a task
    /// that is not before it (so the plan has no cycle), or two tasks of
    /// one method give different `max_running`.
    pub fn new(wants: Vec<Want>, budget: Budget) -> Result<Self> {
        let mut downstream = vec![Vec::new(); wants.len()];
        let mut methods: Vec<MethodTally> = Vec::new();
        let mut method_of = Vec::with_capacity(wants.len());
        for (i, w) in wants.iter().enumerate() {
            if w.threads > budget.cores {
                bail!(
                    "{}: {} threads, over {} cores",
                    w.id,
                    w.threads,
                    budget.cores
                );
            }
            if w.mem_gb > budget.mem_gb {
                bail!("{}: {} GB, over {} GB", w.id, w.mem_gb, budget.mem_gb);
            }
            if w.gpus as usize > budget.gpus.len() || (w.gpus > 0 && budget.gpu_slots == 0) {
                bail!(
                    "{}: {} GPUs, over the {} devices",
                    w.id,
                    w.gpus,
                    budget.gpus.len()
                );
            }
            for &u in &w.upstream {
                if u >= i {
                    bail!(
                        "{}: reads task {u}, which is not before it in the plan",
                        w.id
                    );
                }
                downstream[u].push(i);
            }
            let of = |m: &MethodTally| m.stage == w.stage_name && m.name == w.method;
            let k = match methods.iter().position(of) {
                Some(k) if methods[k].max_running != w.max_running => {
                    bail!(
                        "{}: max_running differs from its method's other tasks",
                        w.id
                    )
                }
                Some(k) => k,
                None => {
                    methods.push(MethodTally {
                        stage: w.stage_name.clone(),
                        name: w.method.clone(),
                        max_running: w.max_running,
                        ended: 0,
                        failed: 0,
                        halted: false,
                    });
                    methods.len() - 1
                }
            };
            method_of.push(k);
        }
        let mut order: Vec<usize> = (0..wants.len()).collect();
        order.sort_by_key(|&i| (std::cmp::Reverse(wants[i].stage), wants[i].rep, i));
        Ok(Self {
            state: vec![State::Pending; wants.len()],
            wants,
            budget,
            method_of,
            methods,
            downstream,
            order,
            rss: HashMap::new(),
            ready_since: HashMap::new(),
            draining: false,
            halted: None,
            live: Live::default(),
            starve_ms: STARVE_MS,
            acct: Acct::default(),
            at: 0,
            hold: Hold::Nothing,
        })
    }

    /// Where task `i` is.
    pub fn state(&self, i: usize) -> &State {
        &self.state[i]
    }

    /// The tasks running now.
    pub fn running(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.state.len()).filter(|&i| self.is_running(i))
    }

    /// On resume, in one change: the pending tasks `done` were done in an
    /// earlier run, and the pending tasks `adopted` are still running from
    /// one, on no GPU.
    pub fn resume(&mut self, now: u64, done: &[usize], adopted: &[usize]) -> Result<()> {
        for &i in done.iter().chain(adopted) {
            self.pending(i)?;
        }
        self.event(now, |p| {
            for &i in done {
                p.state[i] = State::Ended(Outcome::Done);
            }
            for &i in adopted {
                p.ready_since.remove(&i);
                p.state[i] = State::Running(Vec::new());
            }
        });
        Ok(())
    }

    /// Threads booked by running tasks.
    pub fn booked(&self) -> u32 {
        let states = self.state.iter().zip(&self.wants);
        states
            .filter(|(s, _)| s.is_running())
            .map(|(_, w)| w.threads)
            .sum()
    }

    /// The tasks to launch at `now`, with the machine as `live` says.
    pub fn next(&mut self, now: u64, live: &Live) -> Vec<Launch> {
        self.advance(now);
        self.live = live.clone();
        self.pass(now, true)
    }

    /// `launched`, from [`Pool::next`] at `now`, did not start after all
    /// (the executor refused them): they are pending again, and keep how
    /// long they have waited.
    pub fn unlaunch(&mut self, now: u64, launched: &[Launch]) -> Result<()> {
        if let Some(l) = launched.iter().find(|l| !self.is_running(l.task)) {
            bail!("{} was not launched", self.wants[l.task].id);
        }
        self.event(now, |p| {
            for l in launched {
                p.state[l.task] = State::Pending;
            }
        });
        Ok(())
    }

    /// Task `i`, running, ended at `now` with `outcome`.
    pub fn ended(&mut self, now: u64, i: usize, outcome: Outcome) -> Result<()> {
        if !self.is_running(i) {
            bail!("{} ended but was not running", self.wants[i].id);
        }
        self.event(now, |p| {
            p.rss.remove(&i);
            p.end(i, outcome);
        });
        Ok(())
    }

    /// A person stopped pending task `i` at `now`.
    pub fn stop_pending(&mut self, now: u64, i: usize) -> Result<()> {
        self.pending(i)?;
        self.event(now, |p| p.end(i, Outcome::Stopped));
        Ok(())
    }

    /// On resume: pending task `i` was done in an earlier run.
    pub fn reuse(&mut self, i: usize) -> Result<()> {
        self.pending(i)?;
        self.event(self.at, |p| p.state[i] = State::Ended(Outcome::Done));
        Ok(())
    }

    /// On resume at `now`: pending task `i` is still running from an
    /// earlier run, on devices `gpus`, which must be in the budget.
    pub fn adopt(&mut self, now: u64, i: usize, gpus: Vec<u32>) -> Result<()> {
        self.pending(i)?;
        if let Some(d) = gpus.iter().find(|d| !self.budget.gpus.contains(d)) {
            bail!(
                "{} runs on GPU {d}, which is not in the budget",
                self.wants[i].id
            );
        }
        self.event(now, |p| {
            p.ready_since.remove(&i);
            p.state[i] = State::Running(gpus);
        });
        Ok(())
    }

    /// Running task `i` is resident at `gb`.
    pub fn rss(&mut self, i: usize, gb: f64) {
        self.event(self.at, |p| {
            p.rss.insert(i, gb);
        });
    }

    /// From `now`, launch nothing more.
    pub fn drain(&mut self, now: u64) {
        self.event(now, |p| p.draining = true);
    }

    /// From `now`, book at most `cores`. Running tasks are never stopped:
    /// over the new limit, nothing starts until enough of them end.
    pub fn set_cores(&mut self, now: u64, cores: u32) {
        self.event(now, |p| p.budget.cores = cores);
    }

    /// Nothing runs, and nothing more will start.
    pub fn finished(&self) -> bool {
        let running = self.state.iter().any(State::is_running);
        !running && (self.stopped() || !self.state.contains(&State::Pending))
    }

    /// Why the benchmark halted, once it has.
    pub fn halted(&self) -> Option<&str> {
        self.halted.as_deref()
    }

    /// The halted methods, as (stage, method).
    pub fn halted_methods(&self) -> Vec<(String, Option<String>)> {
        let h = self.methods.iter().filter(|m| m.halted);
        h.map(|m| (m.stage.clone(), m.name.clone())).collect()
    }

    /// The accounting up to `now`.
    pub fn account(&mut self, now: u64) -> Acct {
        self.advance(now);
        self.acct
    }

    /// A change at `now`: the accounting up to it at the class before, the
    /// change, and the class after.
    fn event(&mut self, now: u64, change: impl FnOnce(&mut Self)) {
        self.advance(now);
        change(self);
        self.pass(self.at, false);
    }

    fn is_running(&self, i: usize) -> bool {
        self.state[i].is_running()
    }

    /// Task `i` is pending and every task upstream of it is done.
    fn is_ready(&self, i: usize) -> bool {
        self.state[i] == State::Pending
            && self.wants[i]
                .upstream
                .iter()
                .all(|&j| self.state[j] == State::Ended(Outcome::Done))
    }

    fn pending(&self, i: usize) -> Result<()> {
        if self.state[i] != State::Pending {
            bail!("{} is not pending", self.wants[i].id);
        }
        Ok(())
    }

    fn stopped(&self) -> bool {
        self.draining || self.halted.is_some()
    }

    /// What running tasks hold.
    fn usage(&self) -> Usage {
        let mut u = Usage {
            methods: vec![0; self.methods.len()],
            devices: self.budget.gpus.iter().map(|&d| (d, 0)).collect(),
            ..Usage::default()
        };
        for (i, st) in self.state.iter().enumerate() {
            if let State::Running(gpus) = st {
                let rss = self.rss.get(&i).copied().unwrap_or(0.0);
                u.take(&self.wants[i], self.method_of[i], rss, gpus);
            }
        }
        u
    }

    /// One pass over the ready tasks at `now`, best first, launching those
    /// that fit when `launch`; then the class of the free cores. A task
    /// held by cores or blocked (memory, a GPU, the disk), ready longer
    /// than `starve_ms` while tasks run, reserves what they free: nothing
    /// behind it starts.
    fn pass(&mut self, now: u64, launch: bool) -> Vec<Launch> {
        let mut u = self.usage();
        let mut out = Vec::new();
        let mut best = Hold::Nothing;
        let mut reserving = false;
        for k in 0..self.order.len() {
            let i = self.order[k];
            if !self.is_ready(i) {
                continue;
            }
            let mut hold = self.hold_of(i, &u);
            if reserving && hold == Hold::Fits {
                hold = Hold::Cores;
            }
            if hold == Hold::Fits && launch {
                let gpus = u.devices(self.wants[i].gpus, self.budget.gpu_slots);
                u.take(&self.wants[i], self.method_of[i], 0.0, &gpus);
                self.state[i] = State::Running(gpus.clone());
                out.push(Launch { task: i, gpus });
                continue;
            }
            best = best.min(hold);
            if self.stopped() {
                continue;
            }
            let since = *self.ready_since.entry(i).or_insert(now);
            let starved = now.saturating_sub(since) >= self.starve_ms;
            if starved && u.tasks > 0 && matches!(hold, Hold::Cores | Hold::Blocked) {
                reserving = true;
            }
        }
        self.hold = best;
        out
    }

    /// Why ready task `i` would not start, with `u` held.
    fn hold_of(&self, i: usize, u: &Usage) -> Hold {
        if self.stopped() {
            return Hold::Capped;
        }
        let w = &self.wants[i];
        if u.threads + w.threads > self.budget.cores {
            return Hold::Cores;
        }
        let k = self.method_of[i];
        if self.methods[k]
            .max_running
            .is_some_and(|cap| u.methods[k] >= cap)
        {
            return Hold::Capped;
        }
        let floor = |avail: f64| u.grow_gb + w.mem_gb + FLOOR_GB > avail;
        if u.mem_gb + w.mem_gb > self.budget.mem_gb
            || (w.gpus > 0 && u.free_devices(self.budget.gpu_slots) < w.gpus as usize)
            || self.live.disk_low
            || self.live.mem_available_gb.is_some_and(floor)
        {
            return Hold::Blocked;
        }
        Hold::Fits
    }

    /// Task `i` ends with `outcome`: its method's tally, its downstream,
    /// and a halt.
    fn end(&mut self, i: usize, outcome: Outcome) {
        self.state[i] = State::Ended(outcome);
        self.ready_since.remove(&i);
        if outcome != Outcome::Done {
            self.skip_downstream(i);
        }
        if outcome == Outcome::Stopped {
            return;
        }
        let k = self.method_of[i];
        let m = &mut self.methods[k];
        m.ended += 1;
        if outcome == Outcome::Done {
            return;
        }
        m.failed += 1;
        let first_three = m.ended == 3 && m.failed == 3;
        let half = m.ended >= 10 && 2 * m.failed >= m.ended;
        if m.halted || !(first_three || half) {
            return;
        }
        m.halted = true;
        for j in 0..self.wants.len() {
            if self.method_of[j] == k && self.state[j] == State::Pending {
                self.state[j] = State::Skipped { upstream: None };
                self.skip_downstream(j);
            }
        }
        let stage = &self.methods[k].stage;
        let mut of_stage = self.methods.iter().filter(|m| &m.stage == stage);
        if self.halted.is_none() && of_stage.all(|m| m.halted) {
            self.halted = Some(format!("halted: every method of stage {stage} failed"));
        }
    }

    /// Pending tasks reading `i`, at any depth, skipped for it.
    fn skip_downstream(&mut self, i: usize) {
        let mut stack = self.downstream[i].clone();
        while let Some(j) = stack.pop() {
            if self.state[j] == State::Pending {
                self.state[j] = State::Skipped { upstream: Some(i) };
                stack.extend(self.downstream[j].iter().copied());
            }
        }
    }

    /// The accounting from the last event to `now`, at the class then.
    fn advance(&mut self, now: u64) {
        let dt = now.saturating_sub(self.at);
        self.at = self.at.max(now);
        let booked = self.booked();
        let free = u64::from(self.budget.cores.saturating_sub(booked));
        self.acct.booked += u64::from(booked) * dt;
        let class = match self.hold {
            Hold::Fits => &mut self.acct.slack,
            Hold::Cores => &mut self.acct.no_fit,
            Hold::Capped => &mut self.acct.capped,
            Hold::Blocked => &mut self.acct.blocked,
            Hold::Nothing => &mut self.acct.idle,
        };
        *class += free * dt;
    }
}

/// What running tasks hold.
#[derive(Debug, Default)]
struct Usage {
    tasks: usize,
    threads: u32,
    mem_gb: f64,
    /// What they may still grow by: `mem_gb` less resident, summed.
    grow_gb: f64,
    /// Running tasks in each method.
    methods: Vec<u32>,
    /// Tasks on each of the budget's devices.
    devices: BTreeMap<u32, u32>,
}

impl Usage {
    /// `w`, of method `method`, resident at `rss_gb`, on `gpus`, runs too.
    fn take(&mut self, w: &Want, method: usize, rss_gb: f64, gpus: &[u32]) {
        self.tasks += 1;
        self.threads += w.threads;
        self.mem_gb += w.mem_gb;
        self.grow_gb += (w.mem_gb - rss_gb).max(0.0);
        self.methods[method] += 1;
        for d in gpus {
            if let Some(n) = self.devices.get_mut(d) {
                *n += 1;
            }
        }
    }

    /// Devices with fewer than `slots` tasks.
    fn free_devices(&self, slots: u32) -> usize {
        self.devices.values().filter(|&&u| u < slots).count()
    }

    /// `n` distinct devices with fewer than `slots` tasks, least used
    /// first, in id order.
    fn devices(&self, n: u32, slots: u32) -> Vec<u32> {
        let mut free: Vec<(u32, u32)> = self
            .devices
            .iter()
            .filter(|&(_, &u)| u < slots)
            .map(|(&d, &u)| (u, d))
            .collect();
        free.sort();
        let mut out: Vec<u32> = free.into_iter().take(n as usize).map(|(_, d)| d).collect();
        out.sort();
        out
    }
}
