#!/usr/bin/env bash
# Fetch the third-party docx/xlsx corpus from the public
# github.com/yeroo/docxy-corpus repo into corpus/ (git-ignored here -- see
# .gitignore and corpus/README.md).
#
# Shallow-clones the corpus repo to a temp dir, replaces corpus/files/ and
# corpus/xlsx-ext/ with its payload (so files removed upstream go away too),
# copies the manifests, and discards the clone. Run it again to update.
#
# The round-trip fidelity gate (docs/fidelity-gate.md) reads corpus/files/
# (docx) and corpus/xlsx-ext/ (xlsx); the compare launchers and verify sweeps
# use the rest. Nothing here is needed to build the crates.
#
# Usage (from anywhere):
#   corpus/tools/fetch-corpus.sh
#
# Offline (the clone fails), it prints a SKIP notice and exits 0, leaving any
# existing copy alone: the gate then runs the repo-tracked .docx and .xlsx only.

set -euo pipefail

REPO_URL="https://github.com/yeroo/docxy-corpus.git"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEST_DIR="$REPO_ROOT/corpus"

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is not on PATH -- install git and re-run." >&2
  exit 1
fi

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

echo "Cloning $REPO_URL (shallow, depth 1) ..."
if ! git clone --depth 1 --quiet "$REPO_URL" "$TMP_DIR/docxy-corpus" 2>"$TMP_DIR/clone.log"; then
  cat "$TMP_DIR/clone.log" >&2
  echo "" >&2
  echo "SKIP: could not clone $REPO_URL (offline?). corpus/ is unchanged." >&2
  exit 0
fi

CLONE_DIR="$TMP_DIR/docxy-corpus"
if [ ! -d "$CLONE_DIR/files" ] || [ ! -d "$CLONE_DIR/xlsx-ext" ]; then
  echo "error: clone succeeded but files/ or xlsx-ext/ is missing --" \
       "the corpus repo layout may have changed." >&2
  exit 1
fi

for dir in files xlsx-ext; do
  rm -rf "${DEST_DIR:?}/$dir"
  cp -r "$CLONE_DIR/$dir" "$DEST_DIR/$dir"
done
cp "$CLONE_DIR"/*.json "$DEST_DIR/"

DOCX_COUNT="$(find "$DEST_DIR/files" -name '*.docx' | wc -l | tr -d ' ')"
XLSX_COUNT="$(find "$DEST_DIR/xlsx-ext" -name '*.xlsx' | wc -l | tr -d ' ')"

echo ""
echo "Done. Copied into $DEST_DIR:"
echo "  files/       $DOCX_COUNT .docx"
echo "  xlsx-ext/    $XLSX_COUNT .xlsx"
echo "  *.json       manifests"
