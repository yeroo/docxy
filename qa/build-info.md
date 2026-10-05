# Build info (`--version`, File › Account, `app-info`) — manual cases

What the automated tests cannot reach: a real rebuild around a real commit, and a
CI or release artifact. Run by a person on a scratch clone, never your working tree.

`uiharness/cases/build-info.uit` and `cargo test -p buildinfo` cover the parsers,
the git-less fallback, the JSON and the dialog's state. These cases cover what
only a build can: that the stamp follows the tree.

**Setup for every case:** a scratch clone, `cargo build --manifest-path suite/Cargo.toml`
(the suite is its own workspace), and for the terminal editors `cargo build -p docxy`.
Run the suite normally; the `commit:` line of `--version` (and the short SHA on
File › Account's summary line) must match `git rev-parse HEAD`.

None of these cases has been mutation-proven yet. The **Fails when** lines name the
regression each one is meant to catch.

## A dirty local build says Manual build and names the commit

**Guards:** #1023 requires every local or dirty build to be marked, with the exact commit.

**Steps:**
1. In the scratch clone, edit a tracked file (add a blank comment line to
   `suite/docxy/src/about.rs`), then `git add -u` so the index changes. (Cargo reruns the
   build script on HEAD, the branch ref and the index, not on a first edit to a clean,
   unstaged tree; `touch buildinfo/build.rs` does the same.)
2. `cargo build --manifest-path suite/Cargo.toml`, then `suite/target/debug/suite --version`.
3. Start the suite, open File › Account, press **About docxy suite**, then **Copy**
   and paste into a text editor.

**Expect:**
- `--version` shows `dirty: yes`, `kind: local`, a `manual build` line, and the
  `commit:` equal to `git rev-parse HEAD`.
- File › Account shows `v… · <short sha> · after #<PR> · local · manual build` and
  a visible **Manual build** badge. The About dialog lists every field, also with the
  badge, and the pasted text is the `--version` block plus a `summary:` line.
- Untracked scratch files (a new `notes.txt`) do not set `dirty`: only tracked changes do.

**Fails when:** the badge is missing on a dirty or local build, the SHA is not
`git rev-parse HEAD`, or Copy pastes something other than the `--version` text.

## A commit and a rebuild update the SHA

**Guards:** #1023 requires the SHA to follow the tree: `build.rs` must rerun.

**Steps:**
1. After the case above, `git commit -am scratch`. Rebuild and run `--version` again.
2. `git checkout --detach HEAD~1`, rebuild, run `--version`.

**Expect:** after step 1 the `commit:` is the new commit and `dirty: no` (the kind is
still `local`, so it is still a manual build). After step 2 the branch line is
`unknown` (a detached HEAD outside CI) and the commit is the parent.

**Fails when:** the SHA is stale after a commit because `build.rs` did not rerun
(its `rerun-if-changed` lost `.git/HEAD`, the ref, or `.git/index`).

## A source tarball builds and reports unknown

**Guards:** #1023 requires `build.rs` not to fail without git.

**Steps:**
1. `git archive HEAD | tar -x -C /tmp/nogit` (no `.git`), then
   `cd /tmp/nogit && GIT_CEILING_DIRECTORIES=/tmp cargo build -p docxy`.
2. `target/debug/docxy --version`.

**Expect:** the build succeeds. `commit:`, `branch:` and `commit date:` read `unknown`,
`last PR:` reads `none`, `kind: local`, and the block ends with `manual build`.

**Fails when:** the build script panics or errors without git.

## A CI or release artifact shows no badge

**Guards:** #1023 requires a release build to read `release` with no Manual build badge.

**Steps:**
1. Download a release asset (or build with `DOCXY_BUILD_KIND=release` from a clean,
   committed tree). Run `docxy --version` and open File › Account in the suite.

**Expect:** `kind: release`, `dirty: no`, no `manual build` line, no badge, and
`last PR:` names the newest merged PR on that commit's first-parent history (release
jobs check out the full history; a depth-1 checkout would only see the commit itself).
A CI build of a pull request checks out a synthetic `Merge <sha> into <sha>` commit with
depth 1, so it says `kind: ci`, no badge when clean, `last PR: none` and no commits ahead. `suite --version` and
`app-info` agree.

**Fails when:** `release.yml` stops setting `DOCXY_BUILD_KIND=release`, or `ci.yml`
sets a kind on the `ui-sweep-linux` job (its `.uit` asserts `local`).
