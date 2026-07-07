//! Optional per-repo config at `{repo}/.sprout/config.json`.
//!
//! It lives inside the `.sprout/` namespace, which sprout keeps out of git, so
//! it's personal and untracked. Currently it holds a single key, `base`: the
//! branch new worktrees are created from when `--base` isn't given, overriding
//! the auto-detected mainline.
//!
//! ```json
//! { "base": "development" }
//! ```

use crate::repo;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

const FILE: &str = "config.json";

/// The configured default base branch, if `.sprout/config.json` sets one.
/// Absent file or absent/null `base` → `None`; malformed JSON or a non-string
/// `base` is a hard error, so a typo'd config never silently does nothing.
pub fn base(main_root: &Path) -> Result<Option<String>> {
    let file = repo::repo_namespace(main_root).join(FILE);
    if !file.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", file.display()))?;
    match value.get("base") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => bail!("\"base\" in {} must be a non-empty string", file.display()),
    }
}
