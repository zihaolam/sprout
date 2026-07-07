mod clonefile;
mod config;
mod progress;
mod repo;
mod scrub;
mod signal;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::Path;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "sprout",
    about = "git worktrees with CoW-cloned working state (macOS/APFS)",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a worktree and CoW-clone all git-ignored files into it
    New {
        /// Branch / worktree name
        name: String,
        /// Base ref when creating a new branch (defaults to the repo's default branch, e.g. main)
        #[arg(long)]
        base: Option<String>,
    },
    /// Remove a worktree created by `sprout new`
    Rm {
        name: String,
        /// Remove even if there are uncommitted changes to tracked files
        #[arg(long)]
        force: bool,
    },
    /// List this repo's worktrees
    #[command(visible_alias = "ls")]
    List,
    /// Print a worktree's path (for `cd "$(sprout path <name>)"`)
    Path { name: String },
    /// Print the main worktree's path. With shell integration, cd's back to it.
    Main,
    /// Print a worktree's path, creating it first if it doesn't exist.
    /// With shell integration installed, this cd's into it.
    Switch {
        name: String,
        /// Base ref if a new branch is created (defaults to the repo's default branch, e.g. main)
        #[arg(long)]
        base: Option<String>,
    },
    /// Print shell integration; add `eval "$(sprout shell-init)"` to ~/.zshrc
    ShellInit,
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::New { name, base } => cmd_new(&name, base.as_deref()),
        Cmd::Rm { name, force } => cmd_rm(&name, force),
        Cmd::List => cmd_list(),
        Cmd::Path { name } => cmd_path(&name),
        Cmd::Main => cmd_main(),
        Cmd::Switch { name, base } => cmd_switch(&name, base.as_deref()),
        Cmd::ShellInit => cmd_shell_init(),
    }
}

fn cmd_new(name: &str, base: Option<&str>) -> Result<()> {
    // Arm Ctrl-C handling up front so an interrupt anywhere in here tears the
    // half-built worktree down instead of leaving a partial tree behind (or
    // letting `switch` cd into it later).
    signal::install();

    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    let dest = repo::worktree_dir(&main_root, name)?;

    if dest.exists() {
        bail!("worktree already exists at {}", dest.display());
    }
    repo::ensure_sprout_excluded(&main_root)?;
    fs::create_dir_all(dest.parent().context("destination has no parent")?)?;

    // 1. Worktree: git carries tracked files + shares objects/refs.
    // Capture stdout — ours must stay clean so `cd "$(sprout new x)"` works.
    let dest_str = dest.to_string_lossy();
    let added = if repo::branch_exists(&source, name) {
        // A branch's starting point is fixed when it's created, so neither an
        // explicit `--base` nor the configured default can apply now. Say so
        // rather than silently checking out from wherever the branch points.
        if let Some(b) = base {
            eprintln!(
                "warning: branch '{name}' already exists — ignoring --base {b} \
                 (--base only applies when creating a new branch; \
                 use a new name, or delete '{name}' to recreate it from {b})"
            );
        } else if let Some(b) = config::base(&main_root)? {
            eprintln!(
                "warning: branch '{name}' already exists — checking it out as-is, \
                 not from the configured base '{b}' (a branch's base is fixed at \
                 creation; use a new name, or delete '{name}' to recut it from '{b}')"
            );
        }
        repo::git(&source, &["worktree", "add", &dest_str, name])
    } else {
        let base = resolve_base(base, &source, &main_root)?;
        repo::git(&source, &["worktree", "add", "-b", name, &dest_str, &base])
    };
    // A Ctrl-C during `git worktree add` shows up as a git failure; back out
    // cleanly rather than surfacing it as an error.
    if signal::triggered() {
        abort_worktree(&source, &main_root, &dest);
    }
    let out = added?;
    if !out.is_empty() {
        eprintln!("{out}");
    }

    // 2. CoW-clone everything git ignores (node_modules, caches, .env, ...),
    //    with a spinner on stderr so slow clones don't look frozen.
    let started = Instant::now();
    let entries = repo::ignored_entries(&source)?;
    let (cloned, failed, interrupted) =
        progress::with_spinner(entries.len(), |p| -> Result<(u64, u64, bool)> {
            let mut cloned = 0u64;
            let mut failed = 0u64;
            for rel in &entries {
                // Ctrl-C between entries: stop and let the caller clean up.
                if signal::triggered() {
                    return Ok((cloned, failed, true));
                }
                p.set(rel.display());
                let src = source.join(rel);
                let dst = dest.join(rel);
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                match clonefile::clone(&src, &dst) {
                    Ok(()) => cloned += 1,
                    // Already present — e.g. carried along when a parent was cloned.
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    // Ctrl-C interrupts the clonefile syscall (EINTR); that's not
                    // a real failure — bail rather than warn about it.
                    Err(_) if signal::triggered() => {
                        return Ok((cloned, failed, true));
                    }
                    Err(err) => {
                        failed += 1;
                        let hint = clonefile::explain(&err)
                            .map(|h| format!(" ({h})"))
                            .unwrap_or_default();
                        p.warn(format!("warn: could not clone {}: {err}{hint}", rel.display()));
                    }
                }
                p.inc();
            }
            // Catch a Ctrl-C that landed during the final clone without the
            // clonefile syscall returning EINTR — otherwise we'd finish the
            // loop and report success as if nothing happened.
            Ok((cloned, failed, signal::triggered()))
        })?;

    if interrupted {
        abort_worktree(&source, &main_root, &dest);
    }

    // 3. Scrub whatever .sproutignore says to drop.
    let mut scrubbed = 0u64;
    if let Some(matcher) = scrub::load(&source, &dest)? {
        for rel in &entries {
            scrubbed += scrub::scrub_entry(&matcher, &dest, rel)?;
        }
    }

    eprintln!(
        "cloned {cloned} ignored entr{} in {:.2?}{}{}",
        if cloned == 1 { "y" } else { "ies" },
        started.elapsed(),
        if scrubbed > 0 {
            format!(", scrubbed {scrubbed}")
        } else {
            String::new()
        },
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        },
    );
    // Path on stdout so `cd "$(sprout new foo)"` works.
    println!("{}", dest.display());
    Ok(())
}

/// The base ref for a *new* branch, in precedence order: an explicit `--base`,
/// else the repo's configured default (`.sprout/config.json`), else the
/// auto-detected mainline (`main`/`master`/`origin/HEAD`).
fn resolve_base(flag: Option<&str>, source: &Path, main_root: &Path) -> Result<String> {
    if let Some(b) = flag {
        return Ok(b.to_string());
    }
    if let Some(b) = config::base(main_root)? {
        return Ok(b);
    }
    Ok(repo::default_base(source))
}

/// Tear down a worktree we were partway through building (Ctrl-C) and exit.
/// Nothing lands on stdout, so the shell wrapper's `cd` never fires. Leaves the
/// tree as tidy as `rm` would: no orphaned `.sprout` entry behind.
fn abort_worktree(source: &Path, main_root: &Path, dest: &Path) -> ! {
    // Interrupted before the worktree was even created (during setup): nothing
    // to tear down.
    if !dest.exists() {
        eprintln!("aborted");
        std::process::exit(130);
    }
    let dest_str = dest.to_string_lossy();
    // Prefer git so the `.git/worktrees/<name>` admin entry goes too; fall back
    // to a manual wipe + prune if `add` was interrupted mid-registration.
    if repo::git(source, &["worktree", "remove", "--force", &dest_str]).is_err() {
        let _ = fs::remove_dir_all(dest);
        let _ = repo::git(source, &["worktree", "prune"]);
    }
    let namespace = repo::repo_namespace(main_root);
    repo::prune_empty_parents(&namespace, dest);
    let _ = fs::remove_dir(&namespace); // only if this was the last worktree
    eprintln!("aborted: removed partial worktree {}", dest.display());
    std::process::exit(130);
}

fn cmd_rm(name: &str, force: bool) -> Result<()> {
    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    let dest = repo::worktree_dir(&main_root, name)?;

    if !dest.exists() {
        bail!("no worktree at {}", dest.display());
    }

    // Our worktrees always contain untracked files (that's the point), so
    // `git worktree remove` always needs --force. Guard on *tracked* changes
    // ourselves instead.
    if !force {
        let dirty = repo::git(&dest, &["status", "--porcelain", "--untracked-files=no"])?;
        if !dirty.is_empty() {
            bail!(
                "worktree has uncommitted changes to tracked files (use --force to remove anyway):\n{dirty}"
            );
        }
    }

    // Removing the tree (a big cloned node_modules and friends) can take a
    // beat, so animate a spinner. Capture git's output to keep the line clean.
    let dest_str = dest.to_string_lossy();
    progress::with_message(&format!("removing {name}"), || {
        repo::git(&source, &["worktree", "remove", "--force", &dest_str])
    })?;
    let namespace = repo::repo_namespace(&main_root);
    repo::prune_empty_parents(&namespace, &dest);
    // Drop `.sprout` itself once the last worktree is gone (fails if non-empty).
    let _ = fs::remove_dir(&namespace);
    eprintln!("removed {}", dest.display());
    Ok(())
}

fn cmd_list() -> Result<()> {
    let source = repo::worktree_root()?;
    repo::git_passthrough(&source, &["worktree", "list"])
}

fn cmd_path(name: &str) -> Result<()> {
    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    let dest = repo::worktree_dir(&main_root, name)?;
    if !dest.exists() {
        bail!("no worktree at {}", dest.display());
    }
    println!("{}", dest.display());
    Ok(())
}

fn cmd_main() -> Result<()> {
    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    // Path on stdout so the shell wrapper can cd back to the main checkout.
    println!("{}", main_root.display());
    Ok(())
}

fn cmd_switch(name: &str, base: Option<&str>) -> Result<()> {
    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    let dest = repo::worktree_dir(&main_root, name)?;
    if dest.exists() {
        // Nothing is created when the worktree already exists, so an explicit
        // `--base` can't take effect — flag that instead of ignoring it.
        if let Some(b) = base {
            eprintln!(
                "warning: worktree '{name}' already exists — ignoring --base {b} \
                 (nothing to create; `sprout rm {name}` first to recreate it from {b})"
            );
        }
        println!("{}", dest.display());
        return Ok(());
    }
    cmd_new(name, base) // prints the path itself
}

fn cmd_shell_init() -> Result<()> {
    // Wrap the binary in a function so `new` and `switch` land you in the
    // worktree. A child process can't change the parent shell's cwd.
    print!(
        r#"sprout() {{
  case "$1" in
    new|switch|main)
      local out
      out="$(command sprout "$@")" || return $?
      builtin cd "$out"
      ;;
    *)
      command sprout "$@"
      ;;
  esac
}}
"#
    );
    Ok(())
}
