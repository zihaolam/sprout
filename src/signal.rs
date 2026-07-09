//! Ctrl-C handling for the worktree-building path.
//!
//! `sprout new`/`switch` create a worktree and then CoW-clone a potentially
//! large ignored tree for it. If that's interrupted, the default SIGINT
//! (terminate) leaves a half-built worktree behind — which `switch` would then
//! happily `cd` into next time. So we install a handler that flips a flag the
//! clone loop polls, letting us back out cleanly: abandon the staged clone to
//! a detached sweeper, remove the tracked-files-only checkout, and exit —
//! near-instant. A *second* Ctrl-C restores the default handler, so it
//! hard-quits.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle(_sig: libc::c_int) {
    // Only async-signal-safe work here: flip a flag and re-arm the default
    // handler so a second Ctrl-C terminates immediately if we're wedged.
    INTERRUPTED.store(true, Ordering::SeqCst);
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
}

/// Install the SIGINT handler. Without `SA_RESTART`, a blocking `clonefile`
/// syscall returns `EINTR` on Ctrl-C, so we bail promptly instead of only
/// after the whole tree finishes cloning.
pub fn install() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_flags = 0; // no SA_RESTART: let blocking syscalls see EINTR
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

/// Has Ctrl-C been pressed since `install`?
pub fn triggered() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
