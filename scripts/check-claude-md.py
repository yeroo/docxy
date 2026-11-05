#!/usr/bin/env python3
"""Keep CLAUDE.md under budget and its area-note index whole (#1903).

CLAUDE.md is loaded into every Claude Code session, which refuses a memory
file over 150,000 characters. It holds only what every session needs and an
index of area files under docs/claude/; a feature's notes go into its area
file. This script checks, each failure one line on stderr, exit 1 if any:

1. CLAUDE.md is at most --budget characters (UTF-8 decoded);
2. every docs/claude/*.md has a bullet of its own in CLAUDE.md's Area notes
   section (a line starting "- `docs/claude/<name>.md`");
3. every docs/claude/<name>.md named in CLAUDE.md exists;
4. no `## ` heading appears twice across CLAUDE.md and docs/claude/*.md;
5. CLAUDE.md's `## ` headings are exactly KEPT (as a set).

`--moved-from [REV]` also compares the `## ` sections of CLAUDE.md at REV
(default: the merge base of HEAD and origin/main) with the files now: an old
heading missing or found twice fails; a new heading is reported; a moved
section whose body changed is printed as a diff, for the reviewer.

Standard library only; runs as `python3` on Linux and `python` on Windows.
"""

import argparse
import difflib
import re
import subprocess
import sys
from pathlib import Path

BUDGET = 40_000

# The `##` sections CLAUDE.md may hold. A feature's notes go into its area
# file; adding a heading here is a deliberate decision that every session
# needs it.
KEPT = [
    "## How work is done (every machine, every session)",
    "## Area notes",
    "## Build and test",
    "## Cross-crate invariants",
    "## DOCX Bidi Rendering",
    "## Text input (dead keys, IME)",
    "## Build info",
    "## Dialogs and typed input (#1029)",
]

AREA_DIR = "docs/claude"
INDEX_HEADING = "## Area notes"
# Any mention of an area file (check 3: it must exist).
INDEX_REF = re.compile(r"docs/claude/([A-Za-z0-9_.-]+\.md)")
# An index entry (check 2): a bullet of the Area notes section whose first
# item is the file, so a cross-pointer inside another file's bullet, or a
# mention elsewhere in CLAUDE.md, does not count.
INDEX_ENTRY = re.compile(r"(?m)^- `docs/claude/([A-Za-z0-9_.-]+\.md)`")
FENCE = re.compile(r"^\s*(```|~~~)")


def read(path):
    """The file's text, newlines normalised (a Windows checkout's CRLF too)."""
    with open(path, encoding="utf-8") as f:
        return f.read()


def headings(text):
    """The `## ` heading lines of a Markdown text, outside fenced code blocks."""
    out = []
    fenced = False
    for line in text.split("\n"):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if not fenced and line.startswith("## "):
            out.append(line.rstrip())
    return out


def sections(text):
    """Heading -> section text (heading line included, trailing blank lines
    stripped), for each `## ` section outside fenced code blocks. A heading
    seen twice keeps its first section."""
    out = {}
    current = None
    lines = []
    fenced = False

    def flush():
        if current is not None and current not in out:
            out[current] = "\n".join(lines).rstrip("\n")

    for line in text.split("\n"):
        if FENCE.match(line):
            fenced = not fenced
        elif not fenced and line.startswith("## "):
            flush()
            current = line.rstrip()
            lines = []
        lines.append(line)
    flush()
    return out


def area_files(root):
    folder = Path(root) / AREA_DIR
    if not folder.is_dir():
        return []
    return sorted(p for p in folder.glob("*.md") if p.is_file())


def check(root, budget=BUDGET):
    """The failures (one message each) of checks 1-5 on the tree at root,
    and a summary line."""
    root = Path(root)
    errors = []
    claude_path = root / "CLAUDE.md"
    if not claude_path.is_file():
        return ["CLAUDE.md not found in %s" % root], ""
    claude = read(claude_path)

    chars = len(claude)
    if chars > budget:
        errors.append(
            "CLAUDE.md is %d characters, over the %d budget: move feature notes to "
            "their area file under %s/" % (chars, budget, AREA_DIR)
        )

    files = area_files(root)
    named = set(INDEX_REF.findall(claude))
    index = sections(claude).get(INDEX_HEADING)
    if index is None:
        errors.append(
            "CLAUDE.md has no %r section: the area files' index lives there"
            % INDEX_HEADING
        )
        indexed = set()
    else:
        indexed = set(INDEX_ENTRY.findall(index))
    for p in files:
        if p.name not in indexed:
            errors.append(
                "%s/%s is not in CLAUDE.md's Area notes index (a bullet starting "
                "`%s/%s`)" % (AREA_DIR, p.name, AREA_DIR, p.name)
            )
    present = {p.name for p in files}
    for name in sorted(named - present):
        errors.append(
            "CLAUDE.md names %s/%s, which does not exist" % (AREA_DIR, name)
        )

    seen = {}
    for label, text in [("CLAUDE.md", claude)] + [
        ("%s/%s" % (AREA_DIR, p.name), read(p)) for p in files
    ]:
        for h in headings(text):
            seen.setdefault(h, []).append(label)
    for h, where in seen.items():
        if len(where) > 1:
            errors.append(
                "heading %r appears %d times (%s): a section lives in one place"
                % (h, len(where), ", ".join(where))
            )

    kept = set(KEPT)
    own = headings(claude)
    for h in own:
        if h not in kept:
            errors.append(
                "CLAUDE.md has the section %r: move it to its area file (%s/...) "
                "or add it to KEPT in scripts/check-claude-md.py" % (h, AREA_DIR)
            )
    for h in KEPT:
        if h not in own:
            errors.append(
                "CLAUDE.md lacks the kept section %r (KEPT in scripts/check-claude-md.py)"
                % h
            )

    summary = "CLAUDE.md: %d / %d characters, %d area files in %s/" % (
        chars,
        budget,
        len(files),
        AREA_DIR,
    )
    return errors, summary


def compare(old_text, new_texts):
    """Compare CLAUDE.md's old sections with the new files' (name -> text).

    Returns (missing, duplicated, new, diffs): old headings found nowhere,
    old headings found in more than one place (with where), headings no old
    section had, and a unified diff per moved section whose body changed
    (trailing blank lines aside)."""
    old = sections(old_text)
    found = {}
    counts = {}  # heading -> files, a file once per occurrence
    for name, text in new_texts.items():
        for h, body in sections(text).items():
            found.setdefault(h, []).append((name, body))
        for h in headings(text):
            counts.setdefault(h, []).append(name)
    missing = [h for h in old if h not in counts]
    duplicated = [(h, counts[h]) for h in old if len(counts.get(h, [])) > 1]
    new = [h for h in counts if h not in old]
    diffs = []
    for h, body in old.items():
        if len(counts.get(h, [])) != 1:
            continue
        name, now = found[h][0]
        if now != body:
            diffs.append(
                "".join(
                    difflib.unified_diff(
                        (body + "\n").splitlines(True),
                        (now + "\n").splitlines(True),
                        "CLAUDE.md (old): " + h,
                        name + ": " + h,
                    )
                )
            )
    return missing, duplicated, new, diffs


def git(root, *args):
    return subprocess.run(
        ["git", "-C", str(root)] + list(args),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        universal_newlines=True,
        encoding="utf-8",
    )


def moved_from(root, rev):
    """Run --moved-from; returns the exit status."""
    root = Path(root)
    if rev is None:
        base = git(root, "merge-base", "HEAD", "origin/main")
        if base.returncode != 0:
            print(
                "--moved-from: no merge base of HEAD and origin/main (%s); "
                "fetch origin or name a revision" % base.stderr.strip(),
                file=sys.stderr,
            )
            return 1
        rev = base.stdout.strip()
    shown = git(root, "show", "%s:CLAUDE.md" % rev)
    if shown.returncode != 0:
        print(
            "--moved-from: cannot read CLAUDE.md at %s: %s"
            % (rev, shown.stderr.strip()),
            file=sys.stderr,
        )
        return 1
    old_text = shown.stdout.replace("\r\n", "\n")
    new_texts = {"CLAUDE.md": read(root / "CLAUDE.md")}
    for p in area_files(root):
        new_texts["%s/%s" % (AREA_DIR, p.name)] = read(p)
    missing, duplicated, new, diffs = compare(old_text, new_texts)
    old_count = len(sections(old_text))
    print("--moved-from %s: %d old sections" % (rev, old_count))
    for h in missing:
        print("MISSING   %s" % h)
    for h, where in duplicated:
        print("TWICE     %s (%s)" % (h, ", ".join(where)))
    for h in new:
        print("new       %s" % h)
    placed = old_count - len(missing) - len(duplicated)
    print(
        "%d of %d old sections found exactly once; %d body diff(s)"
        % (placed, old_count, len(diffs))
    )
    for d in diffs:
        print(d, end="" if d.endswith("\n") else "\n")
    return 1 if missing or duplicated else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--root",
        default=str(Path(__file__).resolve().parent.parent),
        help="repository root (default: the script's parent folder)",
    )
    ap.add_argument("--budget", type=int, default=BUDGET)
    ap.add_argument(
        "--moved-from",
        nargs="?",
        const="",
        default=None,
        metavar="REV",
        help="also compare with CLAUDE.md at REV (default: merge base with origin/main)",
    )
    args = ap.parse_args(argv)
    errors, summary = check(args.root, args.budget)
    for e in errors:
        print(e, file=sys.stderr)
    status = 1 if errors else 0
    if not errors:
        print(summary)
    if args.moved_from is not None:
        status |= moved_from(args.root, args.moved_from or None)
    return status


if __name__ == "__main__":
    sys.exit(main())
