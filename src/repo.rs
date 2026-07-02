//! Git plumbing and path layout.
//!
//! Worktrees live at `~/.sprout/{slug}/{name}` where `slug` is
//! `{repo-dir-name}-{fnv1a(main repo path)}` — readable, collision-free.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a git command, returning trimmed stdout on success.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("failed to spawn git")?;
    if !out.status.success() {
        bail!(
            "git {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a git command, inheriting stdio (for user-facing output).
pub fn git_passthrough(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .context("failed to spawn git")?;
    if !status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(())
}

/// Root of the worktree we were invoked from (clone source).
pub fn worktree_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let root =
        git(&cwd, &["rev-parse", "--show-toplevel"]).context("not inside a git repository")?;
    Ok(PathBuf::from(root))
}

/// Root of the *main* repository, even when invoked from a linked worktree —
/// keeps the slug stable no matter where `sprout` is run from.
pub fn main_repo_root(worktree_root: &Path) -> Result<PathBuf> {
    let common = git(
        worktree_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(common);
    common
        .parent()
        .map(Path::to_path_buf)
        .context("could not determine main repository root")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `~/.sprout/{repo-dir-name}-{hash}` for this repository.
pub fn repo_namespace(main_root: &Path) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let canonical = main_root
        .canonicalize()
        .unwrap_or_else(|_| main_root.to_path_buf());
    let dir_name = canonical
        .file_name()
        .context("repository root has no name")?
        .to_string_lossy()
        .to_string();
    let hash = fnv1a(canonical.as_os_str().as_encoded_bytes());
    Ok(PathBuf::from(home)
        .join(".sprout")
        .join(format!("{dir_name}-{hash:08x}")))
}

/// Destination directory for a named worktree. Slashes in the branch name
/// become nested directories, mirroring how git stores refs
/// (`feat/foo` → `~/.sprout/{slug}/feat/foo`).
pub fn worktree_dir(main_root: &Path, name: &str) -> Result<PathBuf> {
    // Delegate name validation to git itself: rejects "..", leading dots,
    // empty components, trailing slashes, control chars, etc. This is what
    // keeps the joined path from escaping the namespace.
    let ok = Command::new("git")
        .args(["check-ref-format", "--branch", name])
        .output()
        .context("failed to spawn git")?
        .status
        .success();
    if !ok {
        bail!("'{name}' is not a valid branch name");
    }
    Ok(repo_namespace(main_root)?.join(name))
}

/// After removing a nested worktree (e.g. `feat/foo`), prune now-empty
/// parent directories up to (but not including) the repo namespace.
pub fn prune_empty_parents(namespace: &Path, dest: &Path) {
    let mut dir = dest.parent();
    while let Some(d) = dir {
        if d == namespace || std::fs::remove_dir(d).is_err() {
            break; // not empty, or hit the namespace — stop
        }
        dir = d.parent();
    }
}

/// Everything git ignores in the source worktree, as relative paths.
/// Directories are collapsed (`node_modules/`, not their contents), so each
/// entry maps to exactly one `clonefile` call.
pub fn ignored_entries(worktree_root: &Path) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree_root)
        .args([
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ])
        .output()
        .context("failed to spawn git ls-files")?;
    if !out.status.success() {
        bail!(
            "git ls-files failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let entries = out
        .stdout
        .split(|b| *b == 0)
        .filter(|e| !e.is_empty())
        .map(|e| {
            // git appends '/' to collapsed directories; strip it.
            let e = e.strip_suffix(b"/").unwrap_or(e);
            PathBuf::from(OsStr::from_bytes(e))
        })
        .collect();
    Ok(dedupe_nested(entries))
}

/// `git ls-files --directory` can emit both an ignored directory and paths
/// inside it (seen in real monorepos with nested ignore rules and symlinked
/// package dirs). Cloning the parent already carries the children, so drop
/// every entry nested under another.
fn dedupe_nested(mut entries: Vec<PathBuf>) -> Vec<PathBuf> {
    // Sorting guarantees a parent sorts before all of its descendants.
    entries.sort();
    let mut kept: Vec<PathBuf> = Vec::with_capacity(entries.len());
    for e in entries {
        // Path::starts_with is component-wise, so `a/b-x` does NOT match
        // `a/b` — only real ancestors (and exact duplicates) are dropped.
        if !kept.iter().any(|k| e.starts_with(k)) {
            kept.push(e);
        }
    }
    kept
}

pub fn branch_exists(root: &Path, name: &str) -> bool {
    git(
        root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::dedupe_nested;
    use std::path::PathBuf;

    fn paths(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn drops_children_duplicates_keeps_lookalike_siblings() {
        // Shape observed in a real pnpm monorepo: parent dirs emitted
        // together with entries nested inside them.
        let got = dedupe_nested(paths(&[
            "apps/chrome-extension/dist",
            "apps/chrome-extension/dist/assets", // child of an entry
            "apps/chrome-extension/dist-x",      // sibling that shares the prefix string
            "packages/api/packages/api",
            "packages/api/packages/api/node_modules", // child
            "packages/api/packages/api",              // exact duplicate
            "node_modules",
        ]));
        assert_eq!(
            got,
            paths(&[
                "apps/chrome-extension/dist",
                "apps/chrome-extension/dist-x",
                "node_modules",
                "packages/api/packages/api",
            ])
        );
    }
}
