//! A single-line CoW-clone progress spinner, rendered to stderr.
//!
//! Cloning is O(metadata), but a big `node_modules` is still one blocking
//! `clonefile` syscall that can take a beat — so the spinner runs on its own
//! thread and keeps ticking even while the main thread is parked in that
//! syscall. It only animates when stderr is a TTY, so piped/CI output (and the
//! `cd "$(sprout new x)"` capture, which redirects only stdout) stays clean.
//! All drawing is on stderr; stdout is never touched.

use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);
/// Keep the whole status line comfortably within one terminal row.
const MAX_LABEL: usize = 48;

/// Shared state: the worker updates it, the spinner thread reads it.
pub struct Reporter {
    total: usize,
    tty: bool,
    done: AtomicUsize,
    stop: AtomicBool,
    label: Mutex<String>,
}

impl Reporter {
    /// Mark the entry we're about to clone (shown next to the spinner).
    pub fn set(&self, label: impl Display) {
        if !self.tty {
            return;
        }
        let mut s = label.to_string();
        if s.chars().count() > MAX_LABEL {
            let head: String = s.chars().take(MAX_LABEL - 1).collect();
            s = format!("{head}…");
        }
        *self.label.lock().unwrap() = s;
    }

    /// One more entry finished — drives the `done/total` counter only.
    pub fn inc(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    /// Emit a message on its own line without the spinner clobbering it.
    pub fn warn(&self, msg: impl Display) {
        if self.tty {
            // Wipe the spinner's line, print the message, drop to a fresh
            // line — the spinner redraws cleanly below it.
            eprint!("\r\x1b[K{msg}\n");
        } else {
            eprintln!("{msg}");
        }
    }
}

/// Run `work`, animating a spinner on a background thread while it runs.
/// Returns whatever `work` returns.
pub fn with_spinner<T>(total: usize, work: impl FnOnce(&Reporter) -> T) -> T {
    let reporter = Reporter {
        total,
        tty: io::stderr().is_terminal(),
        done: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        label: Mutex::new(String::new()),
    };

    // Nothing to show, or nowhere to show it: just run the work.
    if total == 0 || !reporter.tty {
        return work(&reporter);
    }

    // Scoped threads let the spinner borrow `reporter` directly — no Arc.
    thread::scope(|s| {
        s.spawn(|| animate(&reporter));
        let out = work(&reporter);
        reporter.stop.store(true, Ordering::Relaxed);
        out
    })
}

fn animate(r: &Reporter) {
    let mut frame = 0usize;
    while !r.stop.load(Ordering::Relaxed) {
        let done = r.done.load(Ordering::Relaxed);
        let label = r.label.lock().unwrap().clone();
        {
            // Lock per tick (never across the sleep) so `warn` can grab
            // stderr between frames.
            let mut err = io::stderr().lock();
            let _ = write!(
                err,
                "\r\x1b[K{} cloning [{}/{}] {}",
                FRAMES[frame % FRAMES.len()],
                done,
                r.total,
                label,
            );
            let _ = err.flush();
        }
        frame += 1;
        thread::sleep(TICK);
    }
    // Leave the line clean for whatever prints next (the summary).
    let mut err = io::stderr().lock();
    let _ = write!(err, "\r\x1b[K");
    let _ = err.flush();
}
