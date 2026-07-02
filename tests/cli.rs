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

#[test]
fn base_flag_branches_from_ref() {
    let env = TestEnv::new();
    env.write("src/lib.ts", "export const x = 2\n");
    env.git(&["commit", "-qam", "second"]);
    let wt = PathBuf::from(env.sprout_ok(&["new", "old", "--base", "HEAD~1"]));
    let content = fs::read_to_string(wt.join("src/lib.ts")).unwrap();
    assert_eq!(content, "export const x = 1\n", "checked out from HEAD~1");
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
