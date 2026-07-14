#!/usr/bin/env bash
# Fetch the generated .mpp corpus from the private github.com/yeroo/mpp-corpus
# repo into corpus/mpp/ (git-ignored here — see corpus/mpp/.gitignore).
#
# Mirrors the docxy-corpus clone pattern documented in corpus/README.md:
# shallow-clone the separate repo to a temp dir, copy the payload in, discard
# the clone. Nothing here is needed to build or test the crates — only
# mppread's real-file tests and manual corpus exploration use it.
#
# Usage (from anywhere, run from the repo root or not):
#   corpus/tools/fetch-mpp-corpus.sh
#
# Requires: git, with credentials that can read the PRIVATE mpp-corpus repo
# (it's plain first-party content kept private out of caution, not public).
# Anonymous HTTPS access will fail. Easiest setup: `gh auth login` once, then
# `gh auth setup-git` so plain `git clone https://...` picks up the token.

set -euo pipefail

REPO_URL="https://github.com/yeroo/mpp-corpus.git"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEST_DIR="$REPO_ROOT/corpus/mpp"

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is not on PATH — install git and re-run." >&2
  exit 1
fi

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

echo "Cloning $REPO_URL (shallow, depth 1) ..."
if ! git clone --depth 1 --quiet "$REPO_URL" "$TMP_DIR/mpp-corpus" 2>"$TMP_DIR/clone.log"; then
  cat "$TMP_DIR/clone.log" >&2
  cat >&2 <<EOF

error: could not clone $REPO_URL.

This repo is PRIVATE (first-party content, kept private out of caution).
Anonymous access will not work — you need credentials with read access:
  gh auth login          # once, if you haven't already
  gh auth setup-git       # lets plain \`git clone https://...\` use the gh token
or configure a personal access token in your git credential store.
EOF
  exit 1
fi

CLONE_DIR="$TMP_DIR/mpp-corpus"
if [ ! -d "$CLONE_DIR/snapshots" ] || [ ! -f "$CLONE_DIR/manifest.json" ]; then
  echo "error: clone succeeded but snapshots/ or manifest.json is missing —" \
       "the corpus repo layout may have changed." >&2
  exit 1
fi

mkdir -p "$DEST_DIR/snapshots"
cp -r "$CLONE_DIR/snapshots/." "$DEST_DIR/snapshots/"
cp "$CLONE_DIR/manifest.json" "$DEST_DIR/manifest.json"

FILE_COUNT="$(find "$DEST_DIR/snapshots" -type f | wc -l | tr -d ' ')"
TOTAL_SIZE="$(du -sh "$DEST_DIR/snapshots" 2>/dev/null | cut -f1)"

echo ""
echo "Done. Copied into $DEST_DIR:"
echo "  snapshots/    $FILE_COUNT files ($TOTAL_SIZE)"
echo "  manifest.json"
echo ""
echo "Both stay git-ignored (see corpus/mpp/.gitignore)."
