//! A child started as the leader of its own process group, so what it
//! starts goes with it: [`Group::stop`] asks the whole group with TERM,
//! then KILLs what is left of it after a grace, and a group dropped before
//! it is stopped is KILLed.
//!
//! The leader is seen to exit without being reaped (`waitid` with
//! `WNOWAIT`): while it is a zombie its pid, and so the group's id, cannot
//! be taken by another process, so a signal to the group reaches only what
//! the child started. [`Group::stop`] reaps it to see the rest of the group
//! go, and KILLs only a group it has just seen there; once it is done,
//! nothing more is sent to the group. Off Unix the child alone is killed,
//! as before.

use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::time::Duration;

/// How often a stopping group is looked at.
pub(crate) const LOOK: Duration = Duration::from_millis(20);

/// A child that leads its own process group.
pub(crate) struct Group {
    child: Child,
    /// [`Self::stop`] has reaped the leader and is done with the group: its
    /// id may be another's now, so nothing more is sent to it.
    reaped: bool,
}

impl Group {
    /// `cmd` started in a new process group, which it leads.
    pub fn spawn(cmd: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(cmd, 0);
        let child = cmd.spawn()?;
        Ok(Self {
            child,
            reaped: false,
        })
    }

    /// The leader's stdout, when it is piped and not taken yet.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// The leader's stderr, when it is piped and not taken yet.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// The leader has exited. On Unix it is left unreaped, for [`Self::stop`].
    #[cfg(unix)]
    pub fn exited(&mut self) -> io::Result<bool> {
        // SAFETY: an all-zero siginfo_t is valid; waitid fills it in.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        // SAFETY: `info` is a valid siginfo_t to write to.
        let r = unsafe { libc::waitid(libc::P_PID, self.child.id(), &mut info, options) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: waitid succeeded, so si_pid is set: 0 while it runs.
        Ok(unsafe { info.si_pid() } != 0)
    }

    #[cfg(not(unix))]
    pub fn exited(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    /// TERM to the group, up to `grace` for the whole group to go, then
    /// KILL to what is left of it; how the leader ended, reaped.
    ///
    /// The leader is reaped once it exits, as an unreaped leader keeps its
    /// group there; the rest of the group is then seen to go with signal 0.
    /// While any of the group is left its id is not given to another group,
    /// so the KILL, sent only to a group just seen to be there, reaches
    /// only what the child started.
    #[cfg(unix)]
    pub fn stop(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        self.signal(libc::SIGTERM);
        let start = std::time::Instant::now();
        // Once reaped, try_wait gives the leader's status again.
        while self.child.try_wait()?.is_none() || self.signal(0) {
            if start.elapsed() >= grace {
                self.kill();
                break;
            }
            std::thread::sleep(LOOK);
        }
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    #[cfg(not(unix))]
    pub fn stop(&mut self, _grace: Duration) -> io::Result<ExitStatus> {
        self.kill();
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    /// KILL to every process left in the group, and to the leader, which
    /// may have left it, until [`Self::stop`] is done with it.
    fn kill(&mut self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
        // Unreaped, the leader's pid is still its own.
        if !self.reaped {
            let _ = self.child.kill();
        }
    }

    /// `sig` to the group, and whether it is there (an unreaped leader is
    /// one of it); nothing once [`Self::stop`] is done with it. Its id is
    /// still the group's: the leader is not reaped yet, or [`Self::stop`]
    /// has just seen the group there.
    #[cfg(unix)]
    fn signal(&self, sig: libc::c_int) -> bool {
        if self.reaped {
            return false;
        }
        // SAFETY: a plain kill of the group this child leads.
        let r = unsafe { libc::kill(-(self.child.id() as libc::pid_t), sig) };
        r == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

impl Drop for Group {
    /// A group not stopped, as when its run returned early or panicked,
    /// is KILLed and its leader reaped, so none of it is left behind.
    fn drop(&mut self) {
        if !self.reaped {
            self.kill();
            let _ = self.child.wait();
        }
    }
}
