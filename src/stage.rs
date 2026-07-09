//! Staging directories and lazy cleanup — the package-manager trick.
//!
//! Deleting a big cloned tree is O(files); renaming it is one syscall. So
//! sprout never deletes anything large on the hot path: partial builds are
//! *abandoned* in a staging directory and finished worktrees being removed
//! are *renamed* into one, then a detached sweeper process deletes them
//! behind the scenes. Ctrl-C and `sprout rm` both return instantly.
//!
//! Orphans (power loss, a killed sweeper) are reaped by three independent
//! layers, so nothing relies on any single OS behavior:
//!   1. `discard` spawns a detached deleter immediately.
//!   2. `sweep`, run at the start of mutating commands, reaps entries whose
//!      owning pid is gone (or that are simply old).
//!   3. When staging lives in the system temp dir (preferred — chosen when
//!      it shares a filesystem with the repo), the OS's own temp cleaning
//!      (macOS purges after ~3 days; systemd-tmpfiles on many Linux distros)
//!      is the collector of last resort, even if sprout never runs again.
//!
//! Entries are named `{pid}.{name}` so a sweep can tell "owned by a live
//! sprout" from garbage without any lockfile.

use anyhow::{Context, Result};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Staging root inside the system temp dir (used when it shares a
/// filesystem with the repo, so both `clonefile` and `rename` work).
const TEMP_ROOT: &str = "sprout-staging";
/// Fallback staging root inside the repo's `.sprout` namespace (always on
/// the repo's filesystem by construction).
const LOCAL_ROOT: &str = ".staging";
/// Entries older than this are reaped even if their owner pid looks alive
/// (pid recycling) or their name doesn't parse. No sprout run lives this long.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Reserve a fresh path for a staged build or trashed worktree: the staging
/// root is created, the returned leaf is not. Callers either `create_dir` it
/// (staged build) or `rename` onto it (trash).
pub fn slot(namespace: &Path, name: &str) -> Result<PathBuf> {
    let repo_fs = namespace.parent().unwrap_or(namespace);
    let root = if same_device(&env::temp_dir(), repo_fs) {
        env::temp_dir().join(TEMP_ROOT)
    } else {
        namespace.join(LOCAL_ROOT)
    };
    fs::create_dir_all(&root)
        .with_context(|| format!("failed to create staging dir {}", root.display()))?;
    let leaf = format!("{}.{}", std::process::id(), name.replace(['/', '\\'], "-"));
    let mut path = root.join(&leaf);
    // A leftover with our pid means the pid was recycled; step around it.
    let mut n = 0u32;
    while fs::symlink_metadata(&path).is_ok() {
        n += 1;
        path = root.join(format!("{leaf}.{n}"));
    }
    Ok(path)
}

/// Hand `path` to a detached deleter and return immediately. If the spawn
/// fails the entry is simply left behind — `sweep` (or the OS temp cleaner)
/// picks it up later; correctness never depends on this succeeding.
pub fn discard(path: &Path) {
    let _ = spawn_sweeper(std::slice::from_ref(&path.to_path_buf()));
}

/// Reap abandoned staging entries (dead owner pid, or just old) from both
/// staging roots. Costs two readdirs — usually of empty or absent dirs — and
/// the deletion itself runs detached, so this is safe on every command.
pub fn sweep(namespace: &Path) {
    let mut stale: Vec<PathBuf> = Vec::new();
    for root in [env::temp_dir().join(TEMP_ROOT), namespace.join(LOCAL_ROOT)] {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_stale(&path) {
                stale.push(path);
            }
        }
    }
    if !stale.is_empty() {
        let _ = spawn_sweeper(&stale);
    }
}

/// Remove the local staging root if it's empty, so it never blocks removal
/// of `.sprout` itself after the last worktree is gone.
pub fn tidy_local_root(namespace: &Path) {
    let _ = fs::remove_dir(namespace.join(LOCAL_ROOT));
}

/// Body of the hidden `__sweep` subcommand: delete the given entries.
/// Only paths that sit directly inside a staging root are honored, so a
/// stray invocation can't be talked into deleting arbitrary directories.
pub fn run_sweeper(paths: &[PathBuf]) {
    for p in paths {
        if in_staging_root(p) {
            let _ = fs::remove_dir_all(p);
        }
    }
}

fn in_staging_root(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == OsStr::new(TEMP_ROOT) || d == OsStr::new(LOCAL_ROOT))
}

/// An entry is stale when its owning process is gone, or when it's old
/// enough that a live-looking pid must be a recycle (and an unparseable
/// name must be abandoned).
fn is_stale(path: &Path) -> bool {
    let expired = fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > MAX_AGE);
    match owner_pid(path) {
        Some(pid) if pid_alive(pid) => expired,
        Some(_) => true,
        None => expired,
    }
}

/// Parse the owning pid out of a `{pid}.{name}` entry name.
fn owner_pid(path: &Path) -> Option<i32> {
    path.file_name()?.to_str()?.split('.').next()?.parse().ok()
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // kill(0) probes existence without signaling. EPERM still means "exists"
    // (it's just not ours to signal); only ESRCH means gone.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    true // can't probe cheaply — the MAX_AGE check reaps instead
}

#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_device(_a: &Path, _b: &Path) -> bool {
    false // can't tell cheaply — stage next to the repo, where rename is safe
}

/// Re-exec ourselves as `sprout __sweep <paths...>`, detached: stdio to
/// /dev/null and out of the terminal's foreground process group, so a
/// follow-up Ctrl-C (delivered to the whole group) can't kill the deletion.
fn spawn_sweeper(paths: &[PathBuf]) -> std::io::Result<()> {
    let exe = env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("__sweep")
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_pid_reads_leading_component_only() {
        assert_eq!(
            owner_pid(Path::new("/x/sprout-staging/123.feat")),
            Some(123)
        );
        // dots in the branch name don't confuse the parse
        assert_eq!(owner_pid(Path::new("/x/.staging/42.v1.2.3")), Some(42));
        // collision suffix keeps the pid first
        assert_eq!(
            owner_pid(Path::new("/x/sprout-staging/42.feat.1")),
            Some(42)
        );
        assert_eq!(owner_pid(Path::new("/x/sprout-staging/not-ours")), None);
    }

    #[test]
    fn sweeper_only_touches_staging_roots() {
        assert!(in_staging_root(Path::new("/tmp/sprout-staging/1.x")));
        assert!(in_staging_root(Path::new("/repo/.sprout/.staging/1.x")));
        assert!(!in_staging_root(Path::new("/home/user/project")));
        assert!(!in_staging_root(Path::new("/")));
    }

    #[test]
    fn dead_pid_is_stale_live_pid_is_not() {
        let dir = env::temp_dir().join(format!("sprout-stale-test-{}", std::process::id()));
        let live = dir.join(format!("{}.feat", std::process::id()));
        // i32::MAX is far above any real pid space (macOS ~1e5, Linux ~4e6)
        let dead = dir.join(format!("{}.feat", i32::MAX));
        fs::create_dir_all(&live).unwrap();
        fs::create_dir_all(&dead).unwrap();
        assert!(!is_stale(&live));
        assert!(is_stale(&dead));
        let _ = fs::remove_dir_all(&dir);
    }
}
