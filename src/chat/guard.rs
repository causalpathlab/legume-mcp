//! What keeps the chat's children and its terminal safe: the children's
//! logs in the project's own private folder, never a shared one; the
//! children's process groups, stopped whole as the chat ends or is ended
//! (a hangup or a terminate does not run destructors); and text from a
//! model or a tool shown without the control sequences a terminal obeys.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// A log file for child `name`: `.legume/logs/<name>-<pid>.log` in the
/// project, the folder private to this user, the file new and never
/// through a symlink.
pub(crate) fn log(project: &Path, name: &str) -> (PathBuf, Stdio, Stdio) {
    let dir = project.join(".legume/logs");
    let path = dir.join(format!("{name}-{}.log", std::process::id()));
    let open = || -> std::io::Result<std::fs::File> {
        crate::watch::private_dir(&dir)?;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        o.open(&path)
    };
    match open().and_then(|f| Ok((f.try_clone()?, f))) {
        Ok((a, b)) => (path, a.into(), b.into()),
        Err(e) => {
            log::warn!("chat log {}: {e}", path.display());
            (path, Stdio::null(), Stdio::null())
        }
    }
}

/// The process groups of the chat's children, for the signal handler.
static GROUPS: [AtomicI32; 32] = [const { AtomicI32::new(0) }; 32];

/// Child `pid`, which leads its own process group, is stopped with the
/// chat.
pub(crate) fn keep(pid: u32) {
    let pid = pid as i32;
    // The first free slot takes it; past 32 children, none.
    let _ = GROUPS.iter().find(|g| {
        g.compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    });
}

fn forget(pid: u32) {
    for g in &GROUPS {
        let _ = g.compare_exchange(pid as i32, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// Stop the process group `pid` leads: asked to end, then made to after
/// `grace`. Its leader is reaped by `wait`.
pub(crate) fn stop_group(child: &mut std::process::Child, grace: Duration) {
    let pid = child.id();
    #[cfg(unix)]
    // SAFETY: kill has no memory effects; a group gone already is ESRCH.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    let start = Instant::now();
    while start.elapsed() < grace {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    #[cfg(unix)]
    // SAFETY: as above.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    forget(pid);
}

/// On a hangup or a terminate, the children's groups are asked to end
/// before the chat does: they are in groups of their own, out of the
/// terminal's reach.
#[cfg(unix)]
pub(crate) fn on_hangup() {
    extern "C" fn end(sig: libc::c_int) {
        for g in &GROUPS {
            let pid = g.load(Ordering::SeqCst);
            if pid > 0 {
                // SAFETY: kill is async-signal-safe.
                unsafe {
                    libc::kill(-pid, libc::SIGTERM);
                }
            }
        }
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(128 + sig) }
    }
    // SAFETY: the handler only calls async-signal-safe functions.
    unsafe {
        libc::signal(libc::SIGHUP, end as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, end as *const () as libc::sighandler_t);
    }
}

/// `text` as the terminal shows it: no control characters but line breaks
/// and tabs, so no escape sequence from a model or a tool moves the cursor,
/// hides text or sets the clipboard.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn escape_sequences_do_not_reach_the_terminal() {
        assert_eq!(
            super::clean("ok\u{1b}]52;c;ZXZpbA==\u{7}\u{1b}[2J done\n\tnext"),
            "ok]52;c;ZXZpbA==[2J done\n\tnext"
        );
    }
}
