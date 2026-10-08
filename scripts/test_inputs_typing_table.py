"""Unit tests for scripts/inputs-typing-table.py (#1029).

    python3 -m unittest discover -s scripts -p 'test_*.py' -v
"""
import importlib.util
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location('inputs_typing_table', HERE / 'inputs-typing-table.py')
tbl = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tbl)

TRANSCRIPT = """\
uiharness/cases/inputs-typing-doc-app.uit
case: doc user-name: matches the catalogue — ok
  ok    open ../fixtures/basic.docx

case: doc user-name/user-name: types — ok
  ok    open ../fixtures/basic.docx

case: sheet text-to-columns/breaks: OK applies — FAILED
  FAIL  assert dialog is not text-to-columns

case: dark theme doc page-setup/top: types — ok
  ok    open ../fixtures/basic.docx

3 cases — 2 passed, 1 failed
"""


class RowsTest(unittest.TestCase):
    def test_a_case_name_splits_into_surface_dialog_field_and_step(self):
        self.assertEqual(tbl.rows(TRANSCRIPT), [
            ('doc', 'user-name', '', 'matches the catalogue', 'pass'),
            ('doc', 'user-name', 'user-name', 'types', 'pass'),
            ('sheet', 'text-to-columns', 'breaks', 'OK applies', 'FAIL'),
            ('doc (dark)', 'page-setup', 'top', 'types', 'pass'),
        ])

    def test_a_run_dir_reads_only_the_inputs_typing_transcripts_and_fails_on_a_failure(self):
        with tempfile.TemporaryDirectory() as d:
            for name, text in [('inputs-typing-doc-app', TRANSCRIPT),
                               ('sheet-edit', 'case: doc x/y: z — FAILED\n')]:
                (Path(d) / name).mkdir()
                (Path(d) / name / 'transcript.txt').write_text(text, encoding='utf-8')
            self.assertEqual(len(list(tbl.transcripts([d]))), 1)
            self.assertEqual(tbl.main([d]), 1)

    def test_the_table_lines_up_its_columns(self):
        out = tbl.table([('doc', 'goto', 'reference', 'types', 'pass')]).splitlines()
        self.assertEqual(out[0], 'surface | dialog | field     | step  | result')
        self.assertEqual(out[2], 'doc     | goto   | reference | types | pass')


if __name__ == '__main__':
    unittest.main()
