//! Where tasks run. The pool ([`super::pool`]) decides what may start;
//! an executor runs it, on this machine or through a batch scheduler,
//! and says when each task ends. The runner loops on [`round`].

use super::pool::{Launch, Live, Outcome, Pool};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A task's `exit.json`: how its attempt ended, as the wrapper writes it
/// and every executor reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskExit {
    pub task: String,
    pub key: String,
    /// From 1.
    pub attempt: u32,
    /// The end the pool counts, decided by the wrapper alone.
    pub outcome: Outcome,
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stopped: bool,
    /// The tool's log says memory ran out (evidence only).
    pub oom: bool,
    /// When the wrapper started, in seconds since 1970.
    pub started: f64,
    pub secs: f64,
    pub cpu_secs: f64,
    pub max_rss_mb: f64,
    pub node: String,
}

/// What an executor found of an earlier run, as [`Executor::resume`]
/// says it.
#[derive(Debug, Default, PartialEq)]
pub struct Resumed {
    /// Tasks done in an earlier run, with their key.
    pub done: Vec<usize>,
    /// Tasks of an earlier run still queued or running, now watched.
    pub adopted: Vec<usize>,
    /// Tasks an earlier run finished, or left queued, under another key:
    /// ones the spec changed since, which would run again.
    pub rekeyed: Vec<usize>,
}

/// Runs a plan's tasks, by their index in the plan.
pub trait Executor {
    /// What an earlier run of the plan left, changing nothing: the tasks
    /// done, reused, those still running, adopted, and those it left under
    /// another key. Nothing, for an executor that keeps nothing between
    /// runs.
    fn resume(&mut self) -> Result<Resumed> {
        Ok(Resumed::default())
    }
    /// The attempts still queued of the tasks [`Executor::resume`] found
    /// rekeyed, cancelled, once the run goes ahead with them.
    fn cancel_rekeyed(&mut self) -> Result<()> {
        Ok(())
    }
    /// Its scheduler holds a task until its upstream tasks end, so the
    /// pool launches tasks ahead of them ([`Pool::queue_ahead`]).
    fn queues_ahead(&self) -> bool {
        false
    }
    /// Cancels those of `tasks` that have not started; one that has ends as
    /// it does. For an executor that [`Executor::queues_ahead`].
    fn cancel_queued(&mut self, _tasks: &[usize]) -> Result<()> {
        Ok(())
    }
    /// The machine now. An executor whose scheduler places memory itself
    /// leaves `mem_available_gb` empty.
    fn live(&mut self) -> Result<Live>;
    /// Starts `tasks`, one batch, so a scheduler can take them together:
    /// all of them, or on an error none (an executor that started some
    /// cancels them).
    fn launch(&mut self, tasks: &[Launch]) -> Result<()>;
    /// The tasks that ended since the last call.
    fn poll(&mut self) -> Result<Vec<Ended>>;
    /// Stops `tasks`; each ends later through [`Executor::poll`] as
    /// [`Outcome::Stopped`].
    fn cancel(&mut self, tasks: &[usize]) -> Result<()>;
}

/// Seconds since 1970, as [`TaskExit::started`] gives them.
pub(crate) fn unix_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A task that ended: how, and its `exit.json` when its wrapper wrote one.
#[derive(Clone, Debug, PartialEq)]
pub struct Ended {
    pub task: usize,
    pub outcome: Outcome,
    pub exit: Option<TaskExit>,
}

impl Ended {
    /// Task `task` ended with `outcome`, without a word from a wrapper.
    pub fn bare(task: usize, outcome: Outcome) -> Self {
        Ended {
            task,
            outcome,
            exit: None,
        }
    }
}

/// What one [`round`] did: the ends it gave the pool, what it launched,
/// and the first error, after which the round did no more.
#[derive(Debug, Default)]
pub struct Round {
    pub ended: Vec<Ended>,
    pub launched: Vec<Launch>,
    pub error: Option<anyhow::Error>,
}

/// One round: what ended goes to the pool, at the time `clock` reads
/// once [`Executor::poll`] returns, and what the pool then lets start is
/// launched. Every end is applied before an error stops the round; tasks
/// the executor refuses to launch are pending again. A round that fails
/// may be tried again.
pub fn round(pool: &mut Pool, ex: &mut dyn Executor, clock: &mut dyn FnMut() -> u64) -> Round {
    let mut r = Round::default();
    match ex.poll() {
        Ok(ended) => r.ended = ended,
        Err(e) => {
            r.error = Some(e);
            return r;
        }
    }
    let now = clock();
    for e in &r.ended {
        if let Err(err) = pool.ended(now, e.task, e.outcome) {
            r.error.get_or_insert(err);
        }
    }
    if r.error.is_some() {
        return r;
    }
    // Kept until cancelled, so a round that fails here tries again.
    if !pool.cancels().is_empty() {
        let doomed = pool.cancels().to_vec();
        if let Err(e) = ex.cancel_queued(&doomed) {
            r.error = Some(e);
            return r;
        }
        pool.cancelled();
    }
    let launch = match ex.live() {
        Ok(live) => pool.next(now, &live),
        Err(e) => {
            r.error = Some(e);
            return r;
        }
    };
    if launch.is_empty() {
        return r;
    }
    if let Err(e) = ex.launch(&launch) {
        r.error = Some(pool.unlaunch(now, &launch).err().unwrap_or(e));
        return r;
    }
    r.launched = launch;
    r
}
