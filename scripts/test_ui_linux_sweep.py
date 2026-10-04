"""Unit tests for scripts/ui-linux-sweep.py's parsing and classification.

    python3 -m unittest discover -s scripts -p 'test_*.py' -v
"""
import importlib.util
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location('ui_linux_sweep', HERE / 'ui-linux-sweep.py')
sweep = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sweep)

PASSING = """\
Private X11 display :99; logs: /tmp/x
suite:    /repo/suite/target/release/suite
/repo/uiharness/cases/a.uit
case: first — ok
  ok    open ../fixtures/basic.xlsx

case: second — ok
  ok    drag A1 -> C5

2 cases — 2 passed, 0 failed
evidence: /tmp/run
sandbox:  /tmp/run/sandbox-1-2
"""

# A failing run goes to stderr behind `uiharness: `; suite output interleaves.
FAILING = """\
Private X11 display :99; logs: /tmp/x
some suite log line
uiharness: suite:    /repo/suite/target/release/suite
/repo/uiharness/cases/a.uit
case: first — ok
  ok    open ../fixtures/basic.xlsx

case: second — FAILED
  ok    drag A1 -> C5
  FAIL  assert no fill preview
        expected: no fill armed and nothing previewed
        observed: filling=true, fill_preview=A1:C5

2 cases — 1 passed, 1 failed
evidence: /tmp/run
"""

STEP_ERROR = FAILING.replace('  FAIL  assert no fill preview',
                             '  ERROR shot grid\n        capture failed')


def entry(case, kind='fail', issue='#1'):
    return sweep.Entry('a.uit', case, issue, kind, 1)


def classify(text, code, expected=(), declared=('first', 'second'), timed_out=False):
    return sweep.classify('a.uit', sweep.parse_transcript(text), code, list(declared),
                          {e.case: e for e in expected}, timed_out)


class ScriptCases(unittest.TestCase):
    def test_reads_test_lines_like_the_parser(self):
        text = ('# a comment\n'
                'test plain name\n'
                '  open x.xlsx\n'
                '   TEST   indented and shouted   # trailing comment\n'
                'Test non-breaking space\n'
                'test "quoted # kept" tail # dropped\n'
                'test\n'
                'testing is not a case\n')
        self.assertEqual(sweep.script_cases(text), [
            'plain name', 'indented and shouted', 'non-breaking space', '"quoted # kept" tail'])

    def test_every_committed_script_declares_cases(self):
        for path in sorted(sweep.CASES.glob('*.uit')):
            self.assertTrue(sweep.script_cases(path.read_text(encoding='utf-8')), path.name)


class Transcript(unittest.TestCase):
    def test_parses_cases_steps_and_summary(self):
        t = sweep.parse_transcript(FAILING)
        self.assertEqual(list(t.cases), ['first', 'second'])
        self.assertTrue(t.cases['first'].ok)
        self.assertFalse(t.cases['second'].ok)
        self.assertEqual((t.cases['second'].fail_steps, t.cases['second'].error_steps), (1, 0))
        self.assertEqual(t.summary, (2, 1, 1))

    def test_the_error_prefix_lands_on_the_suite_line_only(self):
        t = sweep.parse_transcript('uiharness: suite:    /repo/suite\n'
                                   '/repo/uiharness/cases/a.uit\n'
                                   'case: only — FAILED\n  ERROR x\n\n'
                                   '1 case — 0 passed, 1 failed\n')
        self.assertEqual(list(t.cases), ['only'])
        self.assertEqual(t.cases['only'].error_steps, 1)
        self.assertEqual(t.summary, (1, 0, 1))

    def test_case_name_containing_the_dash(self):
        t = sweep.parse_transcript('case: a — b — ok\n1 case — 1 passed, 0 failed\n')
        self.assertEqual(list(t.cases), ['a — b'])


class Classify(unittest.TestCase):
    def test_pass(self):
        r = classify(PASSING, 0)
        self.assertEqual((r.status, r.passed, r.failed), (sweep.PASS, 2, 0))

    def test_unlisted_failure_is_new(self):
        r = classify(FAILING, 1)
        self.assertEqual(r.status, sweep.FAIL)
        self.assertEqual(r.new_failures, ['second'])

    def test_listed_failure_is_expected(self):
        r = classify(FAILING, 1, [entry('second', issue='#808')])
        self.assertEqual(r.status, sweep.XFAIL)
        self.assertEqual(r.expected, ['second (#808)'])

    def test_listed_case_that_passes_is_unexpected(self):
        r = classify(PASSING, 0, [entry('second', issue='#976')])
        self.assertEqual(r.status, sweep.XPASS)
        self.assertEqual(r.unexpected_pass, ['second (#976)'])

    def test_new_failure_outranks_unexpected_pass(self):
        r = classify(FAILING, 1, [entry('first')])
        self.assertEqual(r.status, sweep.FAIL)
        self.assertEqual(r.unexpected_pass, ['first (#1)'])

    def test_step_error_is_not_covered_by_a_fail_entry(self):
        r = classify(STEP_ERROR, 1, [entry('second')])
        self.assertEqual(r.status, sweep.FAIL)
        self.assertEqual(r.new_failures, ['second (step error)'])

    def test_error_entry_covers_a_step_error_only(self):
        self.assertEqual(classify(STEP_ERROR, 1, [entry('second', 'error')]).status, sweep.XFAIL)
        self.assertEqual(classify(FAILING, 1, [entry('second', 'error')]).status, sweep.FAIL)

    def test_no_case_lines_is_an_error_even_when_listed(self):
        r = classify('ui-linux: Xvfb did not start\n', 1, [entry('second')])
        self.assertEqual(r.status, sweep.ERROR)

    def test_timeout_is_an_error(self):
        r = classify(PASSING, -15, timed_out=True)
        self.assertEqual((r.status, r.reason), (sweep.ERROR, 'timed out'))

    def test_missing_summary_is_an_error(self):
        text = FAILING.replace('2 cases — 1 passed, 1 failed\n', '')
        self.assertEqual(classify(text, 1, [entry('second')]).status, sweep.ERROR)

    def test_summary_disagreeing_with_case_lines_is_an_error(self):
        text = PASSING.replace('2 cases — 2 passed', '3 cases — 3 passed')
        self.assertEqual(classify(text, 0).status, sweep.ERROR)

    def test_reported_names_must_match_the_script(self):
        r = classify(PASSING, 0, declared=('first', 'second', 'third'))
        self.assertEqual(r.status, sweep.ERROR)
        self.assertIn("missing ['third']", r.reason)
        r = classify(PASSING, 0, declared=('first',))
        self.assertEqual(r.status, sweep.ERROR)
        self.assertIn("extra ['second']", r.reason)

    def test_duplicate_case_lines_are_an_error(self):
        text = PASSING.replace('case: second', 'case: first')
        self.assertEqual(classify(text, 0, declared=('first',)).status, sweep.ERROR)

    def test_exit_code_must_agree_with_the_cases(self):
        self.assertEqual(classify(PASSING, 1).status, sweep.ERROR)
        self.assertEqual(classify(FAILING, 0, [entry('second')]).status, sweep.ERROR)


class ExpectedList(unittest.TestCase):
    def test_parses_entries_comments_and_kind(self):
        entries = sweep.parse_expected(
            '# header\n\n'
            'doc-rulers.uit | ruler case | #808\n'
            'word-tables.uit | a case | #976 | error\n')
        self.assertEqual([(e.script, e.case, e.issue, e.kind, e.line) for e in entries], [
            ('doc-rulers.uit', 'ruler case', '#808', 'fail', 3),
            ('word-tables.uit', 'a case', '#976', 'error', 4)])

    def test_rejects_malformed_lines(self):
        for bad in ('doc-rulers.uit | case\n',             # no issue
                    'doc-rulers.uit | case | 808\n',       # issue without #
                    'doc-rulers.uit | case | #808 | odd\n',
                    'doc-rulers.uit | a | b | #808 | fail\n',  # a `|` in the case name
                    'doc-rulers | case | #808\n',
                    'doc-rulers.uit |  | #808\n'):
            with self.subTest(bad=bad), self.assertRaises(sweep.ListError):
                sweep.parse_expected(bad)

    def test_validation_finds_stale_and_duplicate_entries(self):
        with tempfile.TemporaryDirectory() as d:
            cases = Path(d)
            (cases / 'a.uit').write_text('test real case\n  ok\n', encoding='utf-8')
            ok = sweep.Entry('a.uit', 'real case', '#1', 'fail', 1)
            sweep.validate_expected([ok], cases)
            for entries, message in (
                    ([sweep.Entry('gone.uit', 'real case', '#1', 'fail', 2)], 'no script'),
                    ([sweep.Entry('a.uit', 'renamed case', '#1', 'fail', 2)], 'no case'),
                    ([ok, ok], 'listed twice')):
                with self.subTest(message=message):
                    with self.assertRaisesRegex(sweep.ListError, message):
                        sweep.validate_expected(entries, cases)

    def test_committed_list_is_current(self):
        entries = sweep.parse_expected(sweep.EXPECTED.read_text(encoding='utf-8'))
        sweep.validate_expected(entries, sweep.CASES)


class Table(unittest.TestCase):
    def test_counts_line(self):
        rs = [sweep.Result('a.uit', sweep.PASS), sweep.Result('b.uit', sweep.XFAIL),
              sweep.Result('c.uit', sweep.ERROR, reason='timed out')]
        text = sweep.table(rs)
        self.assertIn('timed out', text)
        self.assertTrue(text.endswith('3 scripts: 1 pass, 1 xfail, 0 fail, 0 xpass, 1 error\n'))


if __name__ == '__main__':
    unittest.main()
