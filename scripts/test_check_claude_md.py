"""Unit tests for scripts/check-claude-md.py (#1903).

The script is loaded as the module `check_claude_md`; each test builds a
small tree in a temporary folder, so no git is needed (`compare` takes texts).

    python3 -m unittest discover -s scripts -p 'test_check_claude_md.py' -v
"""
import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "check_claude_md", str(HERE / "check-claude-md.py")
)
ccm = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ccm)


def kept_claude(extra_index="", extra_sections="", pad=""):
    """A CLAUDE.md holding exactly the KEPT sections, with the index naming
    docs/claude/area.md (plus extra_index), then extra_sections, then pad."""
    parts = ["# CLAUDE.md\n\nIntro.\n\n"]
    for h in ccm.KEPT:
        body = "Body.\n"
        if h == "## Area notes":
            body = "- `docs/claude/area.md`: before changing the area.\n" + extra_index
        parts.append("%s\n\n%s\n" % (h, body))
    parts.append(extra_sections)
    parts.append(pad)
    return "".join(parts)


class Tree:
    def __init__(self, claude, areas=None):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.write("CLAUDE.md", claude)
        (self.root / "docs" / "claude").mkdir(parents=True)
        for name, text in (areas or {"area.md": "# Area\n\n## Feature (#1)\n\nNotes.\n"}).items():
            self.write("docs/claude/" + name, text)

    def write(self, rel, text):
        with open(self.root / rel, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)

    def errors(self, budget=ccm.BUDGET):
        return ccm.check(self.root, budget)[0]

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.tmp.cleanup()


class Check(unittest.TestCase):
    def test_repository_passes(self):
        """The real repository passes. This deliberately ties the sweeps'
        "Sweep runner unit tests" step to the docs: a CLAUDE.md over budget
        or an unindexed area file fails there too."""
        errors, summary = ccm.check(HERE.parent)
        self.assertEqual(errors, [])
        self.assertIn("CLAUDE.md:", summary)

    def test_minimal_tree_passes(self):
        with Tree(kept_claude()) as t:
            self.assertEqual(t.errors(), [])

    def test_padded_claude_md_fails(self):
        base = kept_claude()
        with Tree(base) as t:
            budget = len(base) + 10
            # Exactly the budget in characters passes, though its UTF-8 bytes
            # are over it: the limit counts characters.
            t.write("CLAUDE.md", base + "é" * 10)
            self.assertGreater(len((base + "é" * 10).encode("utf-8")), budget)
            self.assertEqual(t.errors(budget), [])
            t.write("CLAUDE.md", base + "é" * 11)
            errors = t.errors(budget)
            self.assertEqual(len(errors), 1)
            self.assertIn("%d characters, over the %d budget" % (budget + 1, budget), errors[0])

    def test_unlisted_area_file_fails(self):
        areas = {
            "area.md": "# Area\n\n## Feature (#1)\n",
            "other.md": "# Other\n\n## Other (#2)\n",
        }
        with Tree(kept_claude(), areas) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("docs/claude/other.md is not in CLAUDE.md's Area notes index", errors[0])

    def test_area_file_named_only_outside_the_index_fails(self):
        # Named under Cross-crate invariants, not in Area notes: no index entry.
        claude = kept_claude().replace(
            "## Cross-crate invariants\n\nBody.\n",
            "## Cross-crate invariants\n\n- A rule (`docs/claude/other.md`).\n",
        )
        areas = {"area.md": "## Feature (#1)\n", "other.md": "## Other (#2)\n"}
        with Tree(claude, areas) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("docs/claude/other.md is not in CLAUDE.md's Area notes index", errors[0])

    def test_area_file_named_only_inside_another_bullet_fails(self):
        # A cross-pointer inside area.md's bullet is not other.md's own entry.
        claude = kept_claude(extra_index="  Its tier styles are in `docs/claude/other.md`.\n")
        areas = {"area.md": "## Feature (#1)\n", "other.md": "## Other (#2)\n"}
        with Tree(claude, areas) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("docs/claude/other.md is not in CLAUDE.md's Area notes index", errors[0])
        # Its own bullet makes it pass.
        claude = kept_claude(
            extra_index="  Its tier styles are in `docs/claude/other.md`.\n"
            "- `docs/claude/other.md`: before changing the other area.\n"
        )
        with Tree(claude, areas) as t:
            self.assertEqual(t.errors(), [])

    def test_missing_index_section_says_so(self):
        claude = kept_claude().replace("## Area notes\n", "## Areas\n")
        with Tree(claude) as t:
            errors = t.errors()
            self.assertTrue(
                any("has no '## Area notes' section" in e for e in errors), errors
            )
            self.assertTrue(any("docs/claude/area.md is not in" in e for e in errors), errors)

    def test_listed_missing_area_file_fails(self):
        claude = kept_claude(extra_index="- `docs/claude/gone.md`: before changing nothing.\n")
        with Tree(claude) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("names docs/claude/gone.md, which does not exist", errors[0])

    def test_duplicate_heading_fails(self):
        areas = {"area.md": "# Area\n\n## Build info\n\nAgain.\n"}
        with Tree(kept_claude(), areas) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("'## Build info' appears 2 times (CLAUDE.md, docs/claude/area.md)", errors[0])

    def test_duplicate_heading_across_area_files_fails(self):
        claude = kept_claude(extra_index="- `docs/claude/b.md`: b.\n")
        areas = {"area.md": "## Feature (#1)\n", "b.md": "## Feature (#1)\n"}
        with Tree(claude, areas) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("appears 2 times", errors[0])

    def test_heading_in_code_fence_is_not_a_heading(self):
        areas = {"area.md": "# Area\n\n```\n## Build info\n```\n\n~~~md\n## New (#9)\n~~~\n"}
        with Tree(kept_claude(), areas) as t:
            self.assertEqual(t.errors(), [])
        claude = kept_claude(extra_sections="```\n## Not a section\n```\n")
        with Tree(claude) as t:
            self.assertEqual(t.errors(), [])

    def test_new_section_in_claude_md_fails(self):
        claude = kept_claude(extra_sections="## Some feature (#9999)\n\nNotes.\n")
        with Tree(claude) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("'## Some feature (#9999)'", errors[0])
            self.assertIn("move it to its area file", errors[0])
            self.assertIn("add it to KEPT", errors[0])

    def test_missing_kept_section_fails(self):
        claude = kept_claude().replace("## Build info\n", "")
        with Tree(claude) as t:
            errors = t.errors()
            self.assertEqual(len(errors), 1)
            self.assertIn("lacks the kept section '## Build info'", errors[0])

    def test_kept_sections_in_any_order_pass(self):
        claude = kept_claude()
        a, b = "## Build info\n\nBody.\n\n", "## DOCX Bidi Rendering\n\nBody.\n\n"
        swapped = claude.replace(a, "@A@").replace(b, a).replace("@A@", b)
        self.assertNotEqual(swapped, claude)
        with Tree(swapped) as t:
            self.assertEqual(t.errors(), [])

    def run_main(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = ccm.main(list(argv))
        return status, out.getvalue(), err.getvalue()

    def test_main_exit_status(self):
        with Tree(kept_claude(extra_sections="## Extra\n")) as t:
            status, _, err = self.run_main("--root", str(t.root))
            self.assertEqual(status, 1)
            self.assertIn("'## Extra'", err)
        with Tree(kept_claude()) as t:
            status, out, err = self.run_main("--root", str(t.root))
            self.assertEqual((status, err), (0, ""))
            self.assertIn("1 area files", out)

    def test_moved_from_without_a_merge_base_says_so(self):
        # A folder that is no git repository has no origin/main: a clear
        # message and exit 1, not a traceback.
        with Tree(kept_claude()) as t:
            status, _, err = self.run_main("--root", str(t.root), "--moved-from")
            self.assertEqual(status, 1)
            self.assertIn("--moved-from: no merge base of HEAD and origin/main", err)
            self.assertIn("fetch origin or name a revision", err)


OLD = (
    "# CLAUDE.md\n\n"
    "## Kept\n\nk\n\n"
    "## One (#1)\n\nfirst\n\n"
    "## Two (#2)\n\nsecond\n\n"
    "## Three (#3)\n\nthird\n"
)


class Compare(unittest.TestCase):
    def test_moved_once_is_ok(self):
        new = {
            "CLAUDE.md": "# CLAUDE.md\n\n## Kept\n\nk\n",
            "a.md": "# A\n\n## One (#1)\n\nfirst\n\n## Three (#3)\n\nthird\n",
            "b.md": "# B\n\n## Two (#2)\n\nsecond\n",
        }
        self.assertEqual(ccm.compare(OLD, new), ([], [], [], []))

    def test_section_that_became_last_shows_no_diff(self):
        # "One" ended with a blank line before "Two"; last in its file it ends
        # with a single newline, and that is not a body change.
        new = {
            "CLAUDE.md": "## Kept\n\nk\n\n## Two (#2)\n\nsecond\n\n## Three (#3)\n\nthird\n\n\n",
            "a.md": "## One (#1)\n\nfirst\n",
        }
        self.assertEqual(ccm.compare(OLD, new)[3], [])

    def test_missing_section_is_reported(self):
        new = {"CLAUDE.md": "## Kept\n\nk\n", "a.md": "## One (#1)\n\nfirst\n\n## Two (#2)\n\nsecond\n"}
        missing, duplicated, _, _ = ccm.compare(OLD, new)
        self.assertEqual(missing, ["## Three (#3)"])
        self.assertEqual(duplicated, [])

    def test_section_in_two_files_is_reported(self):
        new = {
            "CLAUDE.md": "## Kept\n\nk\n\n## One (#1)\n\nfirst\n",
            "a.md": "## One (#1)\n\nfirst\n\n## Two (#2)\n\nsecond\n\n## Three (#3)\n\nthird\n",
        }
        _, duplicated, _, diffs = ccm.compare(OLD, new)
        self.assertEqual(duplicated, [("## One (#1)", ["CLAUDE.md", "a.md"])])
        self.assertEqual(diffs, [])

    def test_body_edit_is_a_diff_not_a_failure(self):
        new = {
            "CLAUDE.md": "## Kept\n\nk\n",
            "a.md": "## One (#1)\n\nfirst, edited\n\n## Two (#2)\n\nsecond\n\n## Three (#3)\n\nthird\n",
        }
        missing, duplicated, new_heads, diffs = ccm.compare(OLD, new)
        self.assertEqual((missing, duplicated, new_heads), ([], [], []))
        self.assertEqual(len(diffs), 1)
        self.assertIn("-first\n", diffs[0])
        self.assertIn("+first, edited\n", diffs[0])
        self.assertIn("a.md: ## One (#1)", diffs[0])

    def test_new_heading_is_reported(self):
        new = {
            "CLAUDE.md": "## Kept\n\nk\n\n## Area notes\n\n- x\n",
            "a.md": "## One (#1)\n\nfirst\n\n## Two (#2)\n\nsecond\n\n## Three (#3)\n\nthird\n",
        }
        self.assertEqual(ccm.compare(OLD, new), ([], [], ["## Area notes"], []))


if __name__ == "__main__":
    unittest.main()
