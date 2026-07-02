# sprout

git worktrees with copy-on-write working state. macOS / APFS only.

`git worktree add` gives you a clean checkout — and none of your
`node_modules`, build output, caches, or `.env`. Symlinking `node_modules`
across worktrees breaks in monorepos (workspace symlinks resolve back into
the *original* worktree, so you build against the wrong branch's code).

`sprout` fixes this with `clonefile(2)`: every git-ignored file and directory
in your current worktree is CoW-cloned into the new one. One syscall per
entry, O(metadata) time, zero extra disk until files diverge. The clone is a
real directory — every relative symlink inside `node_modules` resolves within
the new worktree.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/zihaolam/sprout/main/install.sh | sh
```

Installs a universal (arm64 + x86_64) binary from the latest GitHub release
to `/usr/local/bin` (or `~/.local/bin` if that's not writable). Pin a version
with `SPROUT_VERSION=v0.1.0`, change the destination with
`SPROUT_INSTALL_DIR=...`.

## Usage

```sh
sprout new feat/login            # worktree + clone ignored state, prints path
sprout new feat/login --base v2  # branch off a ref instead of HEAD
sprout switch feat/login         # print path, creating the worktree if needed
cd "$(sprout path feat/login)"   # jump to it
sprout list                      # git worktree list
sprout rm feat/login             # refuses if tracked files are dirty
sprout rm feat/login --force
```

`cd "$(sprout new feat/login)"` also works — the path is the only thing on
stdout. Slashed branch names nest, mirroring git's own ref storage.

### Shell integration

A CLI can't change its parent shell's directory, so `sprout new` /
`sprout switch` print the path instead. To land in the worktree
automatically, add this to `~/.zshrc` (works in bash too):

```sh
eval "$(sprout shell-init)"
```

Then `sprout switch feat/login` creates the worktree if needed and cd's into
it.

## What gets cloned

Everything `git ls-files --others --ignored --exclude-standard` reports in
the worktree you run `sprout new` from: `node_modules` at every level, build
output, caches, `.env` files. Tracked files are handled by `git worktree add`
(shared objects and refs, independent HEAD/index).

Worktrees live at `~/.sprout/{repo-name}-{hash}/{worktree-name}`. The hash is
derived from the main repo path, so identically-named repos don't collide.

## .sproutignore

To *exclude* things from the clone, put gitignore-style patterns in a
`.sproutignore` at the repo root. They're applied as a post-clone scrub —
useful for caches that key entries by absolute path:

```gitignore
node_modules/.cache
**/node_modules/.cache
**/node_modules/.vite
```

There are no built-in defaults; nothing is scrubbed unless you ask.
Scrub patterns only ever apply to cloned (git-ignored) paths — they can't
touch tracked files.

## Build from source

```sh
cargo build --release   # target/release/sprout
cargo test
```

## Releasing

Push a tag and CI does the rest — tests, universal binary via `lipo`,
checksums, GitHub release:

```sh
git tag v0.1.0 && git push origin v0.1.0
```
