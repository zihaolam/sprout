//! Integration tests: run the real binary against throwaway repos.
//!
//! Each test gets its own temp repo; worktrees land in <repo>/.sprout,
//! which is on the same APFS volume by construction (clonefile requires it).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct TestEnv {
    root: PathBuf, // temp root (also HOME)
    repo: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("sprout-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        // macOS temp_dir is /var/..., a symlink; git reports /private/var/...
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let env = TestEnv { root, repo };

        env.git(&["init", "-qb", "main"]);
        env.git(&["config", "user.email", "t@t"]);
        env.git(&["config", "user.name", "t"]);
        env.write("src/lib.ts", "export const x = 1\n");
        env.write(".gitignore", "node_modules/\n.env\ndist/\n");
        env.git(&["add", "-A"]);
        env.git(&["commit", "-qm", "init"]);

        // pnpm-ish ignored state
        env.write(
            "node_modules/.pnpm/foo@1.0.0/node_modules/foo/index.js",
            "module.exports = 1\n",
        );
        std::os::unix::fs::symlink(
            ".pnpm/foo@1.0.0/node_modules/foo",
            env.repo.join("node_modules/foo"),
        )
        .unwrap();
        env.write("node_modules/.bin/foo", "shim\n");
        env.write("node_modules/.cache/babel/x.json", "stale\n");
        env.write(".env", "SECRET=1\n");
        env.write("dist/out.js", "built\n");
        env
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(&self.repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn sprout(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sprout"))
            .current_dir(&self.repo)
            .env("HOME", &self.root)
            .args(args)
            .output()
            .unwrap()
    }

    /// Does the main repo still have a local branch by this name?
    fn branch_exists(&self, name: &str) -> bool {
        Command::new("git")
            .current_dir(&self.repo)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ])
            .status()
            .unwrap()
            .success()
    }

    /// Run `sprout`, assert success, return trimmed stdout.
    fn sprout_ok(&self, args: &[&str]) -> String {
        let out = self.sprout(args);
        assert!(
            out.status.success(),
            "sprout {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn new_clones_ignored_state_and_stdout_is_only_the_path() {
    let env = TestEnv::new();
    let stdout = env.sprout_ok(&["new", "feat"]);

    // stdout contract: exactly one line, the worktree path
    assert_eq!(stdout.lines().count(), 1, "stdout must be only the path");
    let wt = PathBuf::from(&stdout);
    assert!(wt.starts_with(env.repo.join(".sprout")));

    // tracked files via git worktree, ignored state via clonefile
    assert!(wt.join("src/lib.ts").is_file());
    assert!(
        wt.join(".git").is_file(),
        ".git must be a worktree pointer file"
    );
    assert!(wt.join("node_modules/.pnpm").is_dir());
    assert!(wt.join("node_modules/.bin/foo").is_file());
    assert!(wt.join(".env").is_file());
    assert!(wt.join("dist/out.js").is_file());

    // relative symlink resolves inside the clone
    let target = fs::read_link(wt.join("node_modules/foo")).unwrap();
    assert!(target.is_relative());
    assert!(wt.join("node_modules").join(&target).exists());

    // no .sproutignore -> nothing scrubbed
    assert!(wt.join("node_modules/.cache/babel/x.json").is_file());
}

#[test]
fn new_fails_if_worktree_exists() {
    let env = TestEnv::new();
    env.sprout_ok(&["new", "feat"]);
    let out = env.sprout(&["new", "feat"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
}

#[test]
fn sproutignore_scrubs_only_matching_cloned_paths() {
    let env = TestEnv::new();
    env.write(".sproutignore", "node_modules/.cache\n");
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));

    assert!(!wt.join("node_modules/.cache").exists(), "cache scrubbed");
    assert!(wt.join("node_modules/.pnpm").is_dir(), ".pnpm untouched");
    assert!(wt.join("node_modules/.bin/foo").is_file(), ".bin untouched");
    // source worktree is never scrubbed
    assert!(env.repo.join("node_modules/.cache/babel/x.json").is_file());
}

#[test]
fn scrub_removes_symlink_not_its_target() {
    let env = TestEnv::new();
    // symlink inside node_modules pointing OUTSIDE the worktree
    let outside = env.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "keep").unwrap();
    std::os::unix::fs::symlink(&outside, env.repo.join("node_modules/escape")).unwrap();
    env.write(".sproutignore", "node_modules/escape\n");

    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    assert!(!wt.join("node_modules/escape").exists(), "link removed");
    assert!(outside.join("keep.txt").is_file(), "target must survive");
}

#[test]
fn switch_is_idempotent() {
    let env = TestEnv::new();
    let first = env.sprout_ok(&["switch", "feat"]);
    let second = env.sprout_ok(&["switch", "feat"]);
    assert_eq!(first, second);
}

#[test]
fn slashed_branch_names_nest_and_invalid_names_are_rejected() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["switch", "feat/login-page"]));
    assert!(wt.ends_with("feat/login-page"));
    assert!(wt.join("src/lib.ts").is_file());

    for bad in ["../evil", "a/../../evil", ".hidden", "a//b", "feat/"] {
        let out = env.sprout(&["switch", bad]);
        assert!(!out.status.success(), "{bad:?} must be rejected");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not a valid branch name"),
            "{bad:?} must fail name validation"
        );
    }
    // nothing escaped the namespace
    assert!(!env.root.join("evil").exists());
    assert!(!env.repo.join("evil").exists());
}

#[test]
fn rm_guards_dirty_tracked_files_and_prunes_empty_parents() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat/x"]));

    // untracked files alone must NOT block removal... but dirty tracked do
    fs::write(wt.join("src/lib.ts"), "changed\n").unwrap();
    let out = env.sprout(&["rm", "feat/x"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("uncommitted changes"));

    env.sprout_ok(&["rm", "feat/x", "--force"]);
    assert!(!wt.exists());
    // empty `feat/` parent pruned, and `.sprout` itself goes with the
    // last worktree — no empty dir left lying around in the project
    assert!(!wt.parent().unwrap().exists());
    assert!(!env.repo.join(".sprout").exists());
}

#[test]
fn sprout_dir_is_git_excluded_and_never_cloned_into_worktrees() {
    let env = TestEnv::new();
    env.sprout_ok(&["new", "feat"]);

    // .sprout was auto-added to .git/info/exclude: status stays clean
    let out = Command::new("git")
        .current_dir(&env.repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "",
        ".sprout must not show up as untracked state"
    );
    // repeated runs must not duplicate the exclude entry
    env.sprout_ok(&["new", "feat2"]);
    let exclude = fs::read_to_string(env.repo.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches("/.sprout/").count(), 1);

    // a new worktree must never contain the other worktrees
    let wt2 = PathBuf::from(env.sprout_ok(&["path", "feat2"]));
    assert!(
        !wt2.join(".sprout").exists(),
        "worktrees must not nest recursively"
    );
}

#[test]
fn rm_allows_worktree_with_untracked_files_when_tracked_clean() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    assert!(wt.join("node_modules").is_dir(), "has untracked files");
    env.sprout_ok(&["rm", "feat"]); // no --force needed
    assert!(!wt.exists());
}

/// Commit a change to a tracked file *inside* a worktree (not the main repo),
/// leaving that worktree's branch ahead of — and unmerged into — main.
fn commit_in(wt: &std::path::Path, rel: &str, content: &str) {
    fs::write(wt.join(rel), content).unwrap();
    let out = Command::new("git")
        .current_dir(wt)
        .args(["commit", "-qam", "work"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rm_deletes_the_branch_by_default() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat/x"]));
    assert!(
        env.branch_exists("feat/x"),
        "branch created with the worktree"
    );
    env.sprout_ok(&["rm", "feat/x"]);
    assert!(!wt.exists(), "worktree removed");
    assert!(
        !env.branch_exists("feat/x"),
        "rm must delete the branch too"
    );
}

#[test]
fn rm_keep_branch_leaves_the_branch() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    env.sprout_ok(&["rm", "feat", "--keep-branch"]);
    assert!(!wt.exists(), "worktree still removed");
    assert!(
        env.branch_exists("feat"),
        "--keep-branch must preserve the branch"
    );
}

#[test]
fn rm_keeps_unmerged_branch_without_force_and_warns() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    commit_in(&wt, "src/lib.ts", "export const x = 42\n");

    // Safe delete (`git branch -d`) refuses an unmerged branch. The worktree —
    // what you asked to remove — still goes; the branch is kept with a warning.
    let out = env.sprout(&["rm", "feat"]);
    assert!(out.status.success(), "worktree removal must still succeed");
    assert!(!wt.exists(), "worktree removed");
    assert!(env.branch_exists("feat"), "unmerged branch must be kept");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("kept branch"),
        "must warn the branch was kept: {stderr}"
    );
}

#[test]
fn rm_force_deletes_even_an_unmerged_branch() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    commit_in(&wt, "src/lib.ts", "export const x = 42\n");
    env.sprout_ok(&["rm", "feat", "--force"]);
    assert!(!wt.exists(), "worktree removed");
    assert!(
        !env.branch_exists("feat"),
        "--force must delete the unmerged branch"
    );
}

#[test]
fn complete_lists_branches_for_switch_and_worktrees_for_rm() {
    let env = TestEnv::new();
    env.git(&["branch", "develop"]);
    env.sprout_ok(&["new", "feat"]);

    // switch/new complete against every local branch (switch creates the
    // worktree on demand, so any branch is a valid target).
    let branches: Vec<String> = env
        .sprout_ok(&["__complete", "switch"])
        .lines()
        .map(str::to_string)
        .collect();
    for expected in ["main", "develop", "feat"] {
        assert!(
            branches.iter().any(|b| b == expected),
            "switch completion should include '{expected}': {branches:?}"
        );
    }

    // rm/path complete only against existing sprout worktrees — not `main` or
    // `develop`, which have no worktree to remove.
    let worktrees: Vec<String> = env
        .sprout_ok(&["__complete", "rm"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        worktrees,
        vec!["feat"],
        "rm must complete only sprout worktrees, not every branch"
    );
}

#[test]
fn complete_outside_a_repo_is_silent() {
    let env = TestEnv::new();
    // env.root is a bare temp dir, not a git repo: completion runs on every
    // keystroke, so it must exit clean and print nothing rather than erroring.
    let out = Command::new(env!("CARGO_BIN_EXE_sprout"))
        .current_dir(&env.root)
        .env("HOME", &env.root)
        .args(["__complete", "switch"])
        .output()
        .unwrap();
    assert!(out.status.success(), "must exit 0 even outside a repo");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "",
        "no candidates outside a repo"
    );
}

#[test]
fn shell_init_registers_completion_for_both_shells() {
    let env = TestEnv::new();
    let init = env.sprout_ok(&["shell-init"]);
    assert!(
        init.contains("compdef _sprout sprout"),
        "zsh completion missing"
    );
    assert!(
        init.contains("complete -F _sprout sprout"),
        "bash completion missing"
    );
    assert!(
        init.contains("__complete"),
        "completion must call back into the binary"
    );
}

#[test]
fn new_branches_from_default_branch_not_current_branch() {
    let env = TestEnv::new();
    // Diverge on another branch: `main` still has x = 1, `other` has x = 999.
    env.git(&["checkout", "-qb", "other"]);
    env.write("src/lib.ts", "export const x = 999\n");
    env.git(&["commit", "-qam", "other work"]);

    // Invoked from `other`, a new worktree must still branch off `main`.
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    let content = fs::read_to_string(wt.join("src/lib.ts")).unwrap();
    assert_eq!(
        content, "export const x = 1\n",
        "new branch must be created from main, not the current branch"
    );
}

#[test]
fn main_prints_main_worktree_from_anywhere() {
    let env = TestEnv::new();
    // From the main checkout, `main` prints the main checkout itself.
    assert_eq!(env.sprout_ok(&["main"]), env.repo.to_string_lossy());

    // From inside a linked worktree, `main` still points back to the main
    // checkout — not the worktree you're standing in.
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    let from_wt = Command::new(env!("CARGO_BIN_EXE_sprout"))
        .current_dir(&wt)
        .env("HOME", &env.root)
        .args(["main"])
        .output()
        .unwrap();
    assert!(from_wt.status.success());
    assert_eq!(
        String::from_utf8_lossy(&from_wt.stdout).trim(),
        env.repo.to_string_lossy(),
    );
}

#[test]
fn base_flag_branches_from_ref() {
    let env = TestEnv::new();
    env.write("src/lib.ts", "export const x = 2\n");
    env.git(&["commit", "-qam", "second"]);
    let wt = PathBuf::from(env.sprout_ok(&["new", "old", "--base", "HEAD~1"]));
    let content = fs::read_to_string(wt.join("src/lib.ts")).unwrap();
    assert_eq!(content, "export const x = 1\n", "checked out from HEAD~1");
}

/// Give the repo a `development` branch whose lib.ts differs from main's, so
/// tests can tell which base a new worktree was cut from.
fn add_development_branch(env: &TestEnv) {
    env.git(&["checkout", "-qb", "development"]);
    env.write("src/lib.ts", "export const x = 99\n");
    env.git(&["commit", "-qam", "development work"]);
    env.git(&["checkout", "-q", "main"]);
}

#[test]
fn config_json_sets_default_base() {
    let env = TestEnv::new();
    add_development_branch(&env);
    // Without config, `new` would branch from main (x = 1); the config redirects
    // the default to development (x = 99).
    env.write(".sprout/config.json", "{\n  \"base\": \"development\"\n}\n");

    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    let content = fs::read_to_string(wt.join("src/lib.ts")).unwrap();
    assert_eq!(
        content, "export const x = 99\n",
        "new branch must be created from the configured base 'development'"
    );
}

#[test]
fn base_flag_overrides_config_default() {
    let env = TestEnv::new();
    add_development_branch(&env);
    env.write(".sprout/config.json", "{\"base\": \"development\"}\n");

    // Explicit --base wins over the configured default.
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat", "--base", "main"]));
    let content = fs::read_to_string(wt.join("src/lib.ts")).unwrap();
    assert_eq!(
        content, "export const x = 1\n",
        "--base must override .sprout/config.json"
    );
}

#[test]
fn malformed_config_is_a_clear_error() {
    let env = TestEnv::new();
    env.write(".sprout/config.json", "{ not valid json");
    let out = env.sprout(&["new", "feat"]);
    assert!(!out.status.success(), "malformed config must fail loudly");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("config.json"),
        "error must name the file: {stderr}"
    );
}

#[test]
fn explicit_base_on_existing_branch_warns_and_checks_out_as_is() {
    let env = TestEnv::new();
    add_development_branch(&env);
    // `feat` already exists, pointing at main.
    env.git(&["branch", "feat", "main"]);

    let out = env.sprout(&["switch", "feat", "--base", "development"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists") && stderr.contains("ignoring --base"),
        "must warn that --base was ignored: {stderr}"
    );
    // Checked out the existing branch (main's content), not development's.
    let wt = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let content = fs::read_to_string(wt.join("src/lib.ts")).unwrap();
    assert_eq!(
        content, "export const x = 1\n",
        "existing branch checked out as-is"
    );
}

#[test]
fn configured_base_on_existing_branch_warns() {
    let env = TestEnv::new();
    add_development_branch(&env);
    env.write(".sprout/config.json", "{\"base\": \"development\"}\n");
    // `feat` already exists (from main); no --base flag, just the config default.
    env.git(&["branch", "feat", "main"]);

    let out = env.sprout(&["switch", "feat"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists") && stderr.contains("configured base"),
        "must warn the configured base was not applied: {stderr}"
    );
}

#[test]
fn ls_is_an_alias_for_list() {
    let env = TestEnv::new();
    env.sprout_ok(&["new", "feat"]);
    let list = env.sprout_ok(&["list"]);
    let ls = env.sprout_ok(&["ls"]);
    assert_eq!(ls, list, "`ls` must behave exactly like `list`");
    assert!(ls.contains("feat"), "worktree should appear in output");
}

#[test]
fn explicit_base_on_existing_worktree_warns() {
    let env = TestEnv::new();
    env.sprout_ok(&["switch", "feat"]); // create the worktree once
    let out = env.sprout(&["switch", "feat", "--base", "development"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already exists") && stderr.contains("ignoring --base"),
        "must warn --base is ignored when the worktree exists: {stderr}"
    );
}

#[test]
fn clone_is_cow_writes_do_not_leak_back_to_source() {
    let env = TestEnv::new();
    let wt = PathBuf::from(env.sprout_ok(&["new", "feat"]));
    fs::write(
        wt.join("node_modules/.pnpm/foo@1.0.0/node_modules/foo/index.js"),
        "module.exports = 999\n",
    )
    .unwrap();
    let src = fs::read_to_string(
        env.repo
            .join("node_modules/.pnpm/foo@1.0.0/node_modules/foo/index.js"),
    )
    .unwrap();
    assert_eq!(src, "module.exports = 1\n", "source must be unaffected");
}
