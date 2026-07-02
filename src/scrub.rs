//! Post-clone scrub driven by `.sproutignore` (gitignore syntax) at the repo
//! root. No built-in defaults — the user decides what gets scrubbed.
//!
//! Only the CoW-cloned entries are walked, so scrub patterns can never touch
//! tracked files. Removing cloned files is metadata-only and near-instant.

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs;
use std::path::Path;

pub const IGNORE_FILE: &str = ".sproutignore";

/// Load `.sproutignore` from the source worktree root, if present.
pub fn load(source_root: &Path, dest_root: &Path) -> Result<Option<Gitignore>> {
    let file = source_root.join(IGNORE_FILE);
    if !file.is_file() {
        return Ok(None);
    }
    // Root the matcher at the destination so patterns match paths there.
    let mut builder = GitignoreBuilder::new(dest_root);
    if let Some(err) = builder.add(&file) {
        return Err(err).context("failed to parse .sproutignore");
    }
    Ok(Some(builder.build()?))
}

/// Scrub one cloned entry (recursively). Returns how many paths were removed.
pub fn scrub_entry(matcher: &Gitignore, dest_root: &Path, rel: &Path) -> Result<u64> {
    let abs = dest_root.join(rel);
    let Ok(meta) = fs::symlink_metadata(&abs) else {
        return Ok(0); // vanished or never cloned
    };
    let is_dir = meta.is_dir(); // symlinks are not followed

    if matcher.matched(rel, is_dir).is_ignore() {
        if is_dir {
            fs::remove_dir_all(&abs)
                .with_context(|| format!("failed to remove {}", abs.display()))?;
        } else {
            fs::remove_file(&abs).with_context(|| format!("failed to remove {}", abs.display()))?;
        }
        return Ok(1);
    }

    let mut removed = 0;
    if is_dir {
        for child in fs::read_dir(&abs)? {
            let child = child?;
            removed += scrub_entry(matcher, dest_root, &rel.join(child.file_name()))?;
        }
    }
    Ok(removed)
}
