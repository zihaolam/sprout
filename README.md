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

The installer also wires up your shell rc (`~/.zshrc`, or `~/.bash_profile`
for bash) so `sprout` is on your `PATH` and [shell integration](#shell-integration)
(auto-cd + tab completion) works out of the box. It's idempotent — it greps for
each line first, so re-running never duplicates anything, and a line you'd
already added by hand is left alone. Set `SPROUT_NO_MODIFY_RC=1` to skip this
and get the two lines printed for you to add yourself.

## Usage

```sh
sprout new feat/login            # worktree + clone ignored state, prints path
sprout new feat/login --base v2  # branch off a ref instead of the default branch
sprout switch feat/login         # print path, creating the worktree if needed
cd "$(sprout path feat/login)"   # jump to it
sprout main                      # jump back to the main worktree
sprout list                      # git worktree list (alias: sprout ls)
sprout rm feat/login             # remove worktree + delete branch
sprout rm feat/login --force     # ...even if dirty / branch unmerged
sprout rm feat/login --keep-branch  # remove worktree, keep the branch
```

`sprout rm` removes the worktree *and* deletes its branch, so you don't have to
follow up with `git branch -d`. It refuses if the worktree has uncommitted
changes to tracked files, and — like `git branch -d` — leaves the branch in
place (with a warning) if it isn't fully merged. `--force` overrides both,
force-deleting the branch (`git branch -D`); `--keep-branch` removes only the
worktree.

Interrupting a `new`/`switch` with Ctrl-C backs out immediately instead of
leaving a partial tree behind — so you never end up `cd`'d into an incomplete
checkout. Press it again to hard-quit. Like `bun install`, cancelling costs
nothing: ignored state is cloned into a staging directory and promoted into
the worktree with atomic renames only once complete, so aborting just abandons
staging to a detached background sweeper. `sprout rm` uses the same trick in
reverse — the worktree is renamed away instantly and deleted behind the
scenes, so the prompt returns immediately no matter how big `node_modules`
is. Abandoned staging entries are reaped on later runs (and, since staging
lives in the system temp dir, by the OS's own temp cleaning as a last
resort).

New branches are created from the repo's default branch (`main`, else
`master`, else `origin/HEAD`) — not whatever branch you're currently on — so a
new worktree always starts clean from mainline. Use `--base <ref>` to branch
off anything else, or set a per-repo default in
[`.sprout/config.json`](#default-branch).

`--base` only takes effect when a **new** branch is actually created. If you
name an existing branch (or an existing worktree), sprout checks it out as-is
and warns that `--base` was ignored — a branch's starting point is fixed when
it's first created. To recut an existing branch from a different base, delete
it first (or pick a new name).

`cd "$(sprout new feat/login)"` also works — the path is the only thing on
stdout. Slashed branch names nest, mirroring git's own ref storage.

### Shell integration

A CLI can't change its parent shell's directory, so `sprout new`,
`sprout switch`, and `sprout main` print the path instead. The installer adds
the line below to your shell rc for you; if you opted out (or build from
source), add it yourself to `~/.zshrc` (works in bash too):

```sh
eval "$(sprout shell-init)"
```

Then `sprout switch feat/login` creates the worktree if needed and cd's into
it, and `sprout main` drops you back in the main checkout.

Tab completion comes with it (zsh and bash). `sprout switch <TAB>`,
`sprout rm <TAB>`, and `sprout path <TAB>` all complete against the worktrees
sprout has created. (zsh completion needs `compinit` loaded — frameworks like
oh-my-zsh do this for you; on bare zsh add `autoload -Uz compinit && compinit`
before the `eval`.)

## What gets cloned

Everything `git ls-files --others --ignored --exclude-standard` reports in
the worktree you run `sprout new` from: `node_modules` at every level, build
output, caches, `.env` files. Tracked files are handled by `git worktree add`
(shared objects and refs, independent HEAD/index).

Worktrees live at `{repo}/.sprout/{worktree-name}`, inside the project itself.
sprout adds `/.sprout/` to `.git/info/exclude` automatically, so git never
sees them as untracked state and your `.gitignore` stays untouched. Removing
the last worktree removes the `.sprout` directory too.

One caveat of the in-project location: `git clean -fdx` in the main checkout
would delete `.sprout` along with everything else git ignores.

## Default branch

By default `sprout new`/`switch` cut new branches from the auto-detected
mainline (`main`, else `master`, else `origin/HEAD`). To point a repo at a
different default — say your team works off `development` — drop a
`.sprout/config.json` in the repo:

```json
{ "base": "development" }
```

It lives in the git-ignored `.sprout/` directory, so it's personal to your
checkout and never committed. The base is resolved in this order:

```
--base <ref>   >   .sprout/config.json   >   main / master / origin/HEAD
```

(A malformed `config.json`, or a non-string `base`, is a hard error rather than
a silent fallback — so a typo never quietly does nothing.)

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
