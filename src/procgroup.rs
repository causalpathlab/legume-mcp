//! A child started as the leader of its own process group, so what it
//! starts goes with it: a cancel asks the whole group with TERM, then
//! KILLs it after a grace, and processes a child leaves behind holding its
//! pipes can be killed once it exits.
//!
//! The leader is seen to exit without being reaped (`waitid` with
//! `WNOWAIT`): while it is a zombie its pid, and so the group's id, cannot
//! be taken by another process, so a signal to the group reaches only what
//! the child started. Off Unix the child alone is killed, as before.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

/// How often a stopping group's leader is looked at.
#[cfg(unix)]
const LOOK: Duration = Duration::from_millis(20);

/// A child that leads its own process group.
pub(crate) struct Group(pub Child);

impl Group {
    /// `cmd` started in a new process group, which it leads.
    pub fn spawn(cmd: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(cmd, 0);
        cmd.spawn().map(Self)
    }

    /// The leader has exited. On Unix it is left unreaped, for [`Self::kill`].
    #[cfg(unix)]
    pub fn exited(&mut self) -> io::Result<bool> {
        // SAFETY: an all-zero siginfo_t is valid; waitid fills it in.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        // SAFETY: `info` is a valid siginfo_t to write to.
        let r = unsafe { libc::waitid(libc::P_PID, self.0.id(), &mut info, options) };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: waitid succeeded, so si_pid is set: 0 while it runs.
        Ok(unsafe { info.si_pid() } != 0)
    }

    #[cfg(not(unix))]
    pub fn exited(&mut self) -> io::Result<bool> {
        Ok(self.0.try_wait()?.is_some())
    }

    /// KILL to every process left in the group.
    pub fn kill(&mut self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
        #[cfg(not(unix))]
        let _ = self.0.kill();
    }

    /// TERM to the group, up to `grace` for the leader to exit, then KILL
    /// to what is left of it; the leader reaped.
    pub fn stop(&mut self, grace: Duration) {
        #[cfg(unix)]
        {
            self.signal(libc::SIGTERM);
            let start = std::time::Instant::now();
            while start.elapsed() < grace && !self.exited().unwrap_or(true) {
                std::thread::sleep(LOOK);
            }
        }
        #[cfg(not(unix))]
        let _ = grace;
        self.kill();
        let _ = self.0.wait();
    }

    /// How the leader ended, reaped.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.0.wait()
    }

    /// `sig` to the group. It is not reaped yet (only [`Self::stop`] and
    /// [`Self::wait`] reap), so its id is still the group's.
    #[cfg(unix)]
    fn signal(&self, sig: libc::c_int) {
        // SAFETY: a plain kill of the group this child leads.
        unsafe { libc::kill(-(self.0.id() as libc::pid_t), sig) };
    }
}
