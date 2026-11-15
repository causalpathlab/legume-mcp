//! Where tasks run. The pool ([`super::pool`]) decides what may start;
//! an executor runs it, on this machine or through a batch scheduler,
//! and says when each task ends. The runner loops on [`round`].

use super::pool::{Budget, Launch, Live, Outcome, Pool};
use anyhow::Result;
use std::time::Duration;

/// Runs a plan's tasks, by their index in the plan.
pub trait Executor {
    /// What the pool may book.
    fn budget(&self) -> Budget;
    /// The machine now. An executor whose scheduler places memory itself
    /// leaves `mem_available_gb` empty.
    fn live(&mut self) -> Result<Live>;
    /// Starts `tasks`, one batch, so a scheduler can take them together:
    /// all of them, or on an error none (an executor that started some
    /// cancels them).
    fn launch(&mut self, tasks: &[Launch]) -> Result<()>;
    /// The tasks that ended since the last call, waiting up to `wait`
    /// for the first.
    fn poll(&mut self, wait: Duration) -> Result<Vec<(usize, Outcome)>>;
    /// Stops `tasks`; each ends later through [`Executor::poll`] as
    /// [`Outcome::Stopped`].
    fn cancel(&mut self, tasks: &[usize]) -> Result<()>;
}

/// One round: what ended goes to the pool, at the time `clock` reads
/// once [`Executor::poll`] returns, and what the pool then lets start is
/// launched. Returns the launched tasks. Every end is applied before the
/// first error is returned; tasks the executor refuses to launch are
/// pending again.
pub fn round(
    pool: &mut Pool,
    ex: &mut dyn Executor,
    clock: &mut dyn FnMut() -> u64,
    wait: Duration,
) -> Result<Vec<Launch>> {
    let ended = ex.poll(wait)?;
    let now = clock();
    let mut first = None;
    for (task, outcome) in ended {
        if let Err(e) = pool.ended(now, task, outcome) {
            first.get_or_insert(e);
        }
    }
    if let Some(e) = first {
        return Err(e);
    }
    let live = ex.live()?;
    let launch = pool.next(now, &live);
    if launch.is_empty() {
        return Ok(launch);
    }
    if let Err(e) = ex.launch(&launch) {
        pool.unlaunch(now, &launch)?;
        return Err(e);
    }
    Ok(launch)
}
