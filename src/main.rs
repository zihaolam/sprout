mod clonefile;
mod repo;
mod scrub;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::fs;
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
        /// Base ref when creating a new branch (defaults to HEAD)
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
    List,
    /// Print a worktree's path (for `cd "$(sprout path <name>)"`)
    Path { name: String },
    /// Print a worktree's path, creating it first if it doesn't exist.
    /// With shell integration installed, this cd's into it.
    Switch {
        name: String,
        /// Base ref if a new branch is created (defaults to HEAD)
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
        Cmd::Switch { name, base } => cmd_switch(&name, base.as_deref()),
        Cmd::ShellInit => cmd_shell_init(),
    }
}

fn cmd_new(name: &str, base: Option<&str>) -> Result<()> {
    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    let dest = repo::worktree_dir(&main_root, name)?;

    if dest.exists() {
        bail!("worktree already exists at {}", dest.display());
    }
    fs::create_dir_all(dest.parent().context("destination has no parent")?)?;

    // 1. Worktree: git carries tracked files + shares objects/refs.
    // Capture stdout — ours must stay clean so `cd "$(sprout new x)"` works.
    let dest_str = dest.to_string_lossy();
    let out = if repo::branch_exists(&source, name) {
        repo::git(&source, &["worktree", "add", &dest_str, name])?
    } else {
        let base = base.unwrap_or("HEAD");
        repo::git(&source, &["worktree", "add", "-b", name, &dest_str, base])?
    };
    if !out.is_empty() {
        eprintln!("{out}");
    }

    // 2. CoW-clone everything git ignores (node_modules, caches, .env, ...).
    let started = Instant::now();
    let entries = repo::ignored_entries(&source)?;
    let mut cloned = 0u64;
    let mut failed = 0u64;
    for rel in &entries {
        let src = source.join(rel);
        let dst = dest.join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        match clonefile::clone(&src, &dst) {
            Ok(()) => cloned += 1,
            // Already present — e.g. carried along when a parent was cloned.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                failed += 1;
                let hint = clonefile::explain(&err)
                    .map(|h| format!(" ({h})"))
                    .unwrap_or_default();
                eprintln!("warn: could not clone {}: {err}{hint}", rel.display());
            }
        }
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

    let dest_str = dest.to_string_lossy();
    repo::git_passthrough(&source, &["worktree", "remove", "--force", &dest_str])?;
    repo::prune_empty_parents(&repo::repo_namespace(&main_root)?, &dest);
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

fn cmd_switch(name: &str, base: Option<&str>) -> Result<()> {
    let source = repo::worktree_root()?;
    let main_root = repo::main_repo_root(&source)?;
    let dest = repo::worktree_dir(&main_root, name)?;
    if dest.exists() {
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
    new|switch)
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
