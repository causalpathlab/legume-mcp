//! A child started as the leader of its own process group, so what it
//! starts goes with it: a cancel asks the whole group with TERM, then
//! KILLs what is left of it after a grace, and processes a child leaves
//! behind holding its pipes can be killed once it exits.
//!
//! The leader is seen to exit without being reaped (`waitid` with
//! `WNOWAIT`): while it is a zombie its pid, and so the group's id, cannot
//! be taken by another process, so a signal to the group reaches only what
//! the child started. A cancel reaps it to see the rest of the group go,
//! and KILLs only a group it has just seen there. Off Unix the child alone
//! is killed, as before.

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

    /// TERM to the group, up to `grace` for the whole group to go, then
    /// KILL to what is left of it; the leader reaped.
    ///
    /// The leader is reaped once it exits, as an unreaped leader keeps its
    /// group there; the rest of the group is then seen to go with signal 0.
    /// While any of the group is left its id is not given to another group,
    /// so the KILL, sent only to a group just seen to be there, reaches
    /// only what the child started.
    #[cfg(unix)]
    pub fn stop(&mut self, grace: Duration) {
        self.signal(libc::SIGTERM);
        let start = std::time::Instant::now();
        let mut reaped = false;
        while start.elapsed() < grace {
            if !reaped && self.exited().unwrap_or(true) {
                let _ = self.0.wait();
                reaped = true;
            }
            if reaped && !self.present() {
                return;
            }
            std::thread::sleep(LOOK);
        }
        if !reaped || self.present() {
            self.kill();
        }
        let _ = self.0.wait();
    }

    #[cfg(not(unix))]
    pub fn stop(&mut self, _grace: Duration) {
        self.kill();
        let _ = self.0.wait();
    }

    /// How the leader ended, reaped.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.0.wait()
    }

    /// `sig` to the group. Its id is still the group's: the leader is not
    /// reaped yet (only [`Self::stop`] and [`Self::wait`] reap), or
    /// [`Self::stop`] has just seen the group there.
    #[cfg(unix)]
    fn signal(&self, sig: libc::c_int) {
        // SAFETY: a plain kill of the group this child leads.
        unsafe { libc::kill(-(self.0.id() as libc::pid_t), sig) };
    }

    /// Some process is left in the group (an unreaped leader is one).
    #[cfg(unix)]
    fn present(&self) -> bool {
        // SAFETY: signal 0 only checks that the group exists.
        let r = unsafe { libc::kill(-(self.0.id() as libc::pid_t), 0) };
        r == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}
