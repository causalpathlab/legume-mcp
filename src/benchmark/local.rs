//! The local executor: each task run on this machine, under the wrapper
//! (`legume benchmark task`), as a child of the runner leading a process
//! group of its own ([`crate::procgroup::Group`]). The pool decides what
//! starts, within the cores, memory and GPUs the plan books; this starts
//! it, says when it ended (from the wrapper's `exit.json`), and stops it
//! with TERM, which the wrapper turns into a stop of its tool within the
//! grace. However the runner leaves, nothing it started is left running.

use super::executor::{Ended, Executor, Resumed, TaskExit};
use super::plan::Task;
use super::pool::{Launch, Live, Outcome};
use crate::procgroup::Group;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a runner that leaves waits for its wrappers to stop their
/// tools after TERM, before it KILLs what is left.
const LEAVING_GRACE: Duration = Duration::from_secs(10);

/// Runs a plan's tasks here.
pub struct Local {
    /// This legume, which runs each task's wrapper.
    legume: PathBuf,
    /// The plan the wrappers read.
    plan: PathBuf,
    tasks: Vec<Task>,
    /// Each running task's wrapper, by the task's index, in order.
    running: BTreeMap<usize, Group>,
    /// The tasks a stop was sent to.
    cancelled: HashSet<usize>,
    /// The wrappers' own output: the benchmark's `local.log`.
    log: File,
}

impl Local {
    /// The executor of `tasks`, planned in `plan`, its wrappers' own
    /// output appended to `log`.
    pub fn new(plan: PathBuf, tasks: Vec<Task>, log: PathBuf) -> Result<Self> {
        Ok(Local {
            legume: crate::own_exe().context("finding this legume")?,
            plan,
            tasks,
            running: BTreeMap::new(),
            cancelled: HashSet::new(),
            log: std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .with_context(|| format!("opening {}", log.display()))?,
        })
    }

    /// The wrapper of launch `l` started. A task without GPUs sees none.
    fn start(&self, l: &Launch) -> Result<Group> {
        let t = &self.tasks[l.task];
        let ids: Vec<String> = l.gpus.iter().map(u32::to_string).collect();
        let mut cmd = Command::new(&self.legume);
        cmd.args(["benchmark", "task", "--plan"])
            .arg(&self.plan)
            .args(["--task", &t.id])
            .env_remove("SLURM_ARRAY_TASK_ID")
            .env("CUDA_VISIBLE_DEVICES", ids.join(","))
            .stdin(Stdio::null())
            .stdout(self.log.try_clone()?)
            .stderr(self.log.try_clone()?);
        Group::spawn(&mut cmd).with_context(|| format!("starting the wrapper of {}", t.id))
    }

    /// The `exit.json` task `i`'s wrapper wrote for its key, if it did.
    fn exit_of(&self, i: usize) -> Option<TaskExit> {
        let t = &self.tasks[i];
        let bytes = std::fs::read(t.dir.join("exit.json")).ok()?;
        let exit: TaskExit = serde_json::from_slice(&bytes).ok()?;
        (exit.key == t.key).then_some(exit)
    }
}

impl Executor for Local {
    /// The tasks done in an earlier run (their `done.json` has their
    /// key). A local run adopts nothing: its wrappers ended with it.
    fn resume(&mut self) -> Result<Resumed> {
        Ok(Resumed {
            done: (0..self.tasks.len())
                .filter(|&i| self.tasks[i].done_in(&self.tasks[i].dir))
                .collect(),
            adopted: Vec::new(),
        })
    }

    fn live(&mut self) -> Result<Live> {
        Ok(Live {
            mem_available_gb: super::machine::mem_available_gb(),
            disk_low: false,
        })
    }

    fn launch(&mut self, tasks: &[Launch]) -> Result<()> {
        let mut started = Vec::with_capacity(tasks.len());
        for l in tasks {
            match self.start(l) {
                Ok(group) => started.push((l.task, group)),
                // All or none: those started are stopped again.
                Err(e) => {
                    for (_, mut group) in started {
                        let _ = group.stop(LEAVING_GRACE);
                    }
                    return Err(e);
                }
            }
        }
        self.running.extend(started);
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<Ended>> {
        let mut gone = Vec::new();
        for (&i, group) in &mut self.running {
            if group.exited()? {
                gone.push(i);
            }
        }
        let mut ended = Vec::with_capacity(gone.len());
        for i in gone {
            if let Some(mut group) = self.running.remove(&i) {
                // Reaped; anything it left in its group goes with it.
                group.stop(Duration::ZERO)?;
            }
            let stopped = self.cancelled.remove(&i);
            ended.push(match self.exit_of(i) {
                Some(exit) => Ended {
                    task: i,
                    outcome: exit.outcome,
                    exit: Some(exit),
                },
                // Gone without its word: stopped before it could say, or
                // lost.
                None if stopped => Ended::bare(i, Outcome::Stopped),
                None => Ended::bare(i, Outcome::Lost),
            });
        }
        Ok(ended)
    }

    fn cancel(&mut self, tasks: &[usize]) -> Result<()> {
        for i in tasks {
            if let Some(group) = self.running.get(i) {
                group.signal(libc::SIGTERM);
                self.cancelled.insert(*i);
            }
        }
        Ok(())
    }
}

impl Drop for Local {
    /// A runner leaving with tasks still running (a second Ctrl-C, an
    /// error) stops them: TERM to each, so its wrapper stops its tool, up
    /// to [`LEAVING_GRACE`] in all, then KILL to what is left.
    fn drop(&mut self) {
        for group in self.running.values() {
            group.signal(libc::SIGTERM);
        }
        let until = Instant::now() + LEAVING_GRACE;
        for group in self.running.values_mut() {
            let _ = group.stop(until.saturating_duration_since(Instant::now()));
        }
    }
}
