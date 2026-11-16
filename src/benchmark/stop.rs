//! A stop asked of this process: TERM, INT (Ctrl-C) or HUP, noted for
//! the wrapper or the runner to act on, never acted on in the handler.

use std::sync::atomic::{AtomicU32, Ordering};

/// How many stops have been asked.
static STOPS: AtomicU32 = AtomicU32::new(0);

extern "C" fn on_stop(_: libc::c_int) {
    STOPS.fetch_add(1, Ordering::SeqCst);
}

/// TERM, INT and HUP to this process noted, for [`asked`]; a child's own
/// handlers are reset by its exec.
pub(crate) fn catch() {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: a handler that only stores to an atomic, installed with
        // an empty mask.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_stop as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Whether a stop has been asked.
pub(crate) fn asked() -> bool {
    asked_times() > 0
}

/// How many stops have been asked.
pub(crate) fn asked_times() -> u32 {
    STOPS.load(Ordering::SeqCst)
}
