//! The pool: which of a benchmark's cells may start now. It is pure: no
//! process, clock or file. The runner tells it the time (in ms), what the
//! machine has free, and when each cell ends; it answers with the cells
//! to launch and which devices each gets. Where a cell runs is the
//! executor's business ([`super::executor`]).
//!
//! - **Resources.** The threads of running cells never sum past `cores`,
//!   nor their `mem_gb` past `mem_gb`; each GPU device holds at most
//!   `gpu_slots` cells, and a cell gets distinct devices. A lane's
//!   `max_running` caps its running cells; a stage's is each of its
//!   lanes' default, as for `threads`.
//! - **Memory floor.** With a reading of the machine's available memory,
//!   a cell starts only if what running cells may still grow by (their
//!   `mem_gb` less their resident size), plus its own `mem_gb`, plus 4 GB,
//!   is available.
//! - **Order.** Deepest stage first, then rep-major, then plan order. A
//!   cell that does not fit is passed over (backfill). One held by cores
//!   or memory that has waited [`Pool::starve_ms`] while the sweep's cells
//!   run reserves what they free: nothing behind it starts until it fits,
//!   and the cores held for it count as `frag`.
//! - **Halts.** A lane (a stage's method) halts when its first 3 counted
//!   attempts all fail, or once at least 10 have ended and at least half
//!   failed. A person's stop is not counted. A halted lane's pending cells
//!   are skipped. The sweep halts when every lane of a stage with methods
//!   has halted, or a stage without methods halts; then nothing starts.
//! - **Downstream** of a cell that did not end done is skipped.
//! - **Accounting**, in core-ms between events: `booked` (threads
//!   running) and the free cores, as `slack` when a ready cell fits but
//!   waits for the next round, `frag` when a ready cell lacks only cores,
//!   `capped` when one is held by a lane cap, a drain or a halt, `blocked`
//!   when one is held by memory, a GPU or the disk, and `idle` when no
//!   cell is ready. The first of these that applies names the free cores.

use super::plan::Cell;
use anyhow::{bail, Result};
use std::collections::{BTreeMap, HashMap};

/// Free memory kept beyond what running cells may still grow by, in GB.
pub const FLOOR_GB: f64 = 4.0;

/// How long a cell may be passed over before nothing behind it starts.
pub const STARVE_MS: u64 = 10 * 60 * 1000;

/// One cell as the pool sees it.
#[derive(Clone, Debug)]
pub struct Want {
    pub id: String,
    /// The stage's depth: its index in the spec.
    pub stage: usize,
    pub stage_name: String,
    /// The method, for a stage with methods.
    pub lane: Option<String>,
    pub rep: u32,
    pub threads: u32,
    pub mem_gb: f64,
    pub gpus: u32,
    pub max_running: Option<u32>,
    /// The cells (indices) this one reads.
    pub upstream: Vec<usize>,
}

impl Want {
    /// The pool's view of `cells`, a plan, whose stages in spec order are
    /// `stages`: why not, when a cell names a stage not in `stages` or
    /// reads a cell not in the plan.
    pub fn of_plan(cells: &[Cell], stages: &[&str]) -> Result<Vec<Self>> {
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (i, c) in cells.iter().enumerate() {
            if index.insert(c.id.as_str(), i).is_some() {
                bail!("{}: two cells have this id", c.id);
            }
        }
        cells
            .iter()
            .map(|c| {
                let Some(stage) = stages.iter().position(|s| *s == c.stage) else {
                    bail!("{}: stage {} is not in the spec", c.id, c.stage);
                };
                let upstream = c
                    .upstream
                    .iter()
                    .map(|u| match index.get(u.as_str()) {
                        Some(&i) => Ok(i),
                        None => bail!("{}: reads {u}, which is not in the plan", c.id),
                    })
                    .collect::<Result<_>>()?;
                Ok(Self {
                    id: c.id.clone(),
                    stage,
                    stage_name: c.stage.clone(),
                    lane: c.method.clone(),
                    rep: c.rep,
                    threads: c.threads,
                    mem_gb: c.mem_gb,
                    gpus: c.gpus,
                    max_running: c.max_running,
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
    /// Cells a GPU device holds at once.
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

/// A cell to launch, and the GPU devices it gets.
#[derive(Clone, Debug, PartialEq)]
pub struct Launch {
    pub cell: usize,
    pub gpus: Vec<u32>,
}

/// How a cell ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Failed,
    Timeout,
    Oom,
    /// The executor lost it: gone without a word from its shim.
    Lost,
    /// A person stopped it; not counted against its lane.
    Stopped,
}

/// Where a cell is.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Pending,
    /// On these GPU devices.
    Running(Vec<u32>),
    Ended(Outcome),
    /// Never run: `upstream` is the cell whose end or skip caused it, or
    /// `None` when its own lane halted.
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
    pub frag: u64,
    pub capped: u64,
    pub blocked: u64,
    pub idle: u64,
}

/// Why a ready cell does not start, best first: the free cores take the
/// class of the best hold among ready cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Hold {
    Fits,
    Cores,
    Capped,
    Blocked,
    Nothing,
}

/// A lane: a stage's method, or a stage without methods.
#[derive(Clone, Debug)]
struct Lane {
    stage: String,
    method: Option<String>,
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
    /// Each cell's lane, an index into `lanes`.
    lane: Vec<usize>,
    lanes: Vec<Lane>,
    /// Cells reading each cell.
    downstream: Vec<Vec<usize>>,
    /// Every cell, best first: deepest stage, then rep, then plan order.
    order: Vec<usize>,
    /// Resident GB of running cells, as last read.
    rss: HashMap<usize, f64>,
    /// When each cell was first seen ready and not started; cleared when
    /// it ends or is adopted.
    ready_since: HashMap<usize, u64>,
    draining: bool,
    halted: Option<String>,
    live: Live,
    /// How long a cell may be passed over; [`STARVE_MS`] by default.
    pub starve_ms: u64,
    acct: Acct,
    /// When the accounting last advanced, and the class of free cores
    /// since then.
    at: u64,
    hold: Hold,
}

impl Pool {
    /// The pool over `wants` with `budget`: why not, when a cell asks more
    /// than the budget, more GPUs than there are devices, reads a cell
    /// that is not before it (so the plan has no cycle), or two cells of
    /// one lane give different `max_running`.
    pub fn new(wants: Vec<Want>, budget: Budget) -> Result<Self> {
        let mut downstream = vec![Vec::new(); wants.len()];
        let mut lanes: Vec<Lane> = Vec::new();
        let mut lane = Vec::with_capacity(wants.len());
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
                        "{}: reads cell {u}, which is not before it in the plan",
                        w.id
                    );
                }
                downstream[u].push(i);
            }
            let of = |l: &Lane| l.stage == w.stage_name && l.method == w.lane;
            let k = match lanes.iter().position(of) {
                Some(k) if lanes[k].max_running != w.max_running => {
                    bail!("{}: max_running differs from its lane's other cells", w.id)
                }
                Some(k) => k,
                None => {
                    lanes.push(Lane {
                        stage: w.stage_name.clone(),
                        method: w.lane.clone(),
                        max_running: w.max_running,
                        ended: 0,
                        failed: 0,
                        halted: false,
                    });
                    lanes.len() - 1
                }
            };
            lane.push(k);
        }
        let mut order: Vec<usize> = (0..wants.len()).collect();
        order.sort_by_key(|&i| (std::cmp::Reverse(wants[i].stage), wants[i].rep, i));
        Ok(Self {
            state: vec![State::Pending; wants.len()],
            wants,
            budget,
            lane,
            lanes,
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

    /// Where cell `i` is.
    pub fn state(&self, i: usize) -> &State {
        &self.state[i]
    }

    /// Threads booked by running cells.
    pub fn booked(&self) -> u32 {
        let states = self.state.iter().zip(&self.wants);
        states
            .filter(|(s, _)| s.is_running())
            .map(|(_, w)| w.threads)
            .sum()
    }

    /// The cells to launch at `now`, with the machine as `live` says.
    pub fn next(&mut self, now: u64, live: &Live) -> Vec<Launch> {
        self.advance(now);
        self.live = live.clone();
        self.pass(now, true)
    }

    /// `launched`, from [`Pool::next`] at `now`, did not start after all
    /// (the executor refused them): they are pending again, and keep how
    /// long they have waited.
    pub fn unlaunch(&mut self, now: u64, launched: &[Launch]) -> Result<()> {
        if let Some(l) = launched.iter().find(|l| !self.is_running(l.cell)) {
            bail!("{} was not launched", self.wants[l.cell].id);
        }
        self.event(now, |p| {
            for l in launched {
                p.state[l.cell] = State::Pending;
            }
        });
        Ok(())
    }

    /// Cell `i`, running, ended at `now` with `outcome`.
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

    /// A person stopped pending cell `i` at `now`.
    pub fn stop_pending(&mut self, now: u64, i: usize) -> Result<()> {
        self.pending(i)?;
        self.event(now, |p| p.end(i, Outcome::Stopped));
        Ok(())
    }

    /// On resume: pending cell `i` was done in an earlier run.
    pub fn reuse(&mut self, i: usize) -> Result<()> {
        self.pending(i)?;
        self.event(self.at, |p| p.state[i] = State::Ended(Outcome::Done));
        Ok(())
    }

    /// On resume at `now`: pending cell `i` is still running from an
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

    /// Running cell `i` is resident at `gb`.
    pub fn rss(&mut self, i: usize, gb: f64) {
        self.event(self.at, |p| {
            p.rss.insert(i, gb);
        });
    }

    /// From `now`, launch nothing more.
    pub fn drain(&mut self, now: u64) {
        self.event(now, |p| p.draining = true);
    }

    /// From `now`, book at most `cores`. Running cells are never stopped:
    /// over the new limit, nothing starts until enough of them end.
    pub fn set_cores(&mut self, now: u64, cores: u32) {
        self.event(now, |p| p.budget.cores = cores);
    }

    /// Nothing runs, and nothing more will start.
    pub fn finished(&self) -> bool {
        let running = self.state.iter().any(State::is_running);
        !running && (self.stopped() || !self.state.contains(&State::Pending))
    }

    /// Why the sweep halted, once it has.
    pub fn halted(&self) -> Option<&str> {
        self.halted.as_deref()
    }

    /// The halted lanes, as (stage, method).
    pub fn halted_lanes(&self) -> Vec<(String, Option<String>)> {
        let h = self.lanes.iter().filter(|l| l.halted);
        h.map(|l| (l.stage.clone(), l.method.clone())).collect()
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

    /// Cell `i` is pending and every cell upstream of it is done.
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

    /// What running cells hold.
    fn usage(&self) -> Usage {
        let mut u = Usage {
            lanes: vec![0; self.lanes.len()],
            devices: self.budget.gpus.iter().map(|&d| (d, 0)).collect(),
            ..Usage::default()
        };
        for (i, st) in self.state.iter().enumerate() {
            if let State::Running(gpus) = st {
                let rss = self.rss.get(&i).copied().unwrap_or(0.0);
                u.take(&self.wants[i], self.lane[i], rss, gpus);
            }
        }
        u
    }

    /// One pass over the ready cells at `now`, best first, launching those
    /// that fit when `launch`; then the class of the free cores. A cell
    /// held by cores or memory, ready longer than `starve_ms` while cells
    /// run, reserves what they free: nothing behind it starts.
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
                u.take(&self.wants[i], self.lane[i], 0.0, &gpus);
                self.state[i] = State::Running(gpus.clone());
                out.push(Launch { cell: i, gpus });
                continue;
            }
            best = best.min(hold);
            if self.stopped() {
                continue;
            }
            let since = *self.ready_since.entry(i).or_insert(now);
            let starved = now.saturating_sub(since) >= self.starve_ms;
            if starved && u.cells > 0 && matches!(hold, Hold::Cores | Hold::Blocked) {
                reserving = true;
            }
        }
        self.hold = best;
        out
    }

    /// Why ready cell `i` would not start, with `u` held.
    fn hold_of(&self, i: usize, u: &Usage) -> Hold {
        if self.stopped() {
            return Hold::Capped;
        }
        let w = &self.wants[i];
        if u.threads + w.threads > self.budget.cores {
            return Hold::Cores;
        }
        let lane = self.lane[i];
        if self.lanes[lane]
            .max_running
            .is_some_and(|cap| u.lanes[lane] >= cap)
        {
            return Hold::Capped;
        }
        let floor = |avail: f64| u.grow_gb + w.mem_gb + FLOOR_GB > avail;
        if u.mem_gb + w.mem_gb > self.budget.mem_gb
            || u.free_devices(self.budget.gpu_slots) < w.gpus as usize
            || self.live.disk_low
            || self.live.mem_available_gb.is_some_and(floor)
        {
            return Hold::Blocked;
        }
        Hold::Fits
    }

    /// Cell `i` ends with `outcome`: its lane's tally, its downstream, and
    /// a halt.
    fn end(&mut self, i: usize, outcome: Outcome) {
        self.state[i] = State::Ended(outcome);
        self.ready_since.remove(&i);
        if outcome != Outcome::Done {
            self.skip_downstream(i);
        }
        if outcome == Outcome::Stopped {
            return;
        }
        let k = self.lane[i];
        let l = &mut self.lanes[k];
        l.ended += 1;
        if outcome == Outcome::Done {
            return;
        }
        l.failed += 1;
        let first_three = l.ended == 3 && l.failed == 3;
        let half = l.ended >= 10 && 2 * l.failed >= l.ended;
        if l.halted || !(first_three || half) {
            return;
        }
        l.halted = true;
        for j in 0..self.wants.len() {
            if self.lane[j] == k && self.state[j] == State::Pending {
                self.state[j] = State::Skipped { upstream: None };
                self.skip_downstream(j);
            }
        }
        let stage = &self.lanes[k].stage;
        let mut of_stage = self.lanes.iter().filter(|l| &l.stage == stage);
        if self.halted.is_none() && of_stage.all(|l| l.halted) {
            self.halted = Some(format!("halted: every lane of stage {stage} failed"));
        }
    }

    /// Pending cells reading `i`, at any depth, skipped for it.
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
            Hold::Cores => &mut self.acct.frag,
            Hold::Capped => &mut self.acct.capped,
            Hold::Blocked => &mut self.acct.blocked,
            Hold::Nothing => &mut self.acct.idle,
        };
        *class += free * dt;
    }
}

/// What running cells hold.
#[derive(Debug, Default)]
struct Usage {
    cells: usize,
    threads: u32,
    mem_gb: f64,
    /// What they may still grow by: `mem_gb` less resident, summed.
    grow_gb: f64,
    /// Running cells in each lane.
    lanes: Vec<u32>,
    /// Cells on each of the budget's devices.
    devices: BTreeMap<u32, u32>,
}

impl Usage {
    /// `w`, of lane `lane`, resident at `rss_gb`, on `gpus`, runs too.
    fn take(&mut self, w: &Want, lane: usize, rss_gb: f64, gpus: &[u32]) {
        self.cells += 1;
        self.threads += w.threads;
        self.mem_gb += w.mem_gb;
        self.grow_gb += (w.mem_gb - rss_gb).max(0.0);
        self.lanes[lane] += 1;
        for d in gpus {
            if let Some(n) = self.devices.get_mut(d) {
                *n += 1;
            }
        }
    }

    /// Devices with fewer than `slots` cells.
    fn free_devices(&self, slots: u32) -> usize {
        self.devices.values().filter(|&&u| u < slots).count()
    }

    /// `n` distinct devices with fewer than `slots` cells, least used
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
