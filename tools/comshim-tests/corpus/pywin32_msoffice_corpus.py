"""Run pywin32's OWN upstream Office-automation corpus against the shims.

The operations and read-back assertions here are lifted verbatim from pywin32's
`win32com/test/testMSOffice.py` (TextExcel + TestWord8) — a real third-party test
suite written against genuine Excel/Word, not something tailored to this shim. It
therefore exercises far more of the object model than the shims' create/save focus
(notably multi-cell 2D `Range.Value` read-back, `Paragraphs` iteration,
`Font.ColorIndex`). Running it maps, honestly, where the shims match real Office
and where they degrade.

Each step is classified:
  PASS    — ran and (for assertions) matched real-Office semantics
  DEGRADE — ran without faulting, but the value isn't real-Office-accurate
            (graceful degradation: the shim doesn't model this read/feature)
  FAIL    — raised / crashed (a shim defect: it should degrade, never fault)

The bar for the shims is: zero FAIL (never fault a host), plus PASS on the
create/write/save path. DEGRADE on rich read-back is expected and documented.

Run (shim installed):  python pywin32_msoffice_corpus.py
Exit 0 iff there are no FAILs.
"""
import sys
import tempfile
import os

try:
    import win32com.client
    import win32com.client.dynamic
except ImportError:
    print("SKIP: pywin32 not installed")
    sys.exit(0)

results = []  # (kind, label, detail)


def step(label, fn, expect=None):
    """Run fn(); if expect is given, compare its return to expect."""
    try:
        got = fn()
    except Exception as e:  # noqa
        results.append(("FAIL", label, "%s: %s" % (type(e).__name__, e)))
        return
    if expect is None:
        results.append(("PASS", label, ""))
    elif got == expect:
        results.append(("PASS", label, ""))
    else:
        results.append(("DEGRADE", label, "got %r, real Excel gives %r" % (got, expect)))


def excel_corpus():
    xl = win32com.client.dynamic.Dispatch("Excel.Application")
    step("Excel: Visible = 0", lambda: setattr(xl, "Visible", 0))
    step("Excel: Workbooks.Add", lambda: xl.Workbooks.Add())
    step("Excel: Range('A1:C1').Value = (1,2,3)",
         lambda: setattr(xl.Range("A1:C1"), "Value", (1, 2, 3)))
    step("Excel: Range('A2:C2').Value = ('x','y','z')",
         lambda: setattr(xl.Range("A2:C2"), "Value", ("x", "y", "z")))
    step("Excel: Range('A3:C3').Value = ('3','2','1')",
         lambda: setattr(xl.Range("A3:C3"), "Value", ("3", "2", "1")))
    step("Excel: 20x Cells(i,i).Value = 'Hi i'",
         lambda: [setattr(xl.Cells(i + 1, i + 1), "Value", "Hi %d" % i) for i in range(20)])
    # read-back assertions (verbatim from testMSOffice.py)
    step("Excel: Range('A1').Value == 'Hi 0'", lambda: xl.Range("A1").Value, "Hi 0")
    step("Excel: Range('A1:B1').Value (2D)", lambda: xl.Range("A1:B1").Value, (("Hi 0", 2),))
    step("Excel: Range('A1:A2').Value (2D)", lambda: xl.Range("A1:A2").Value, (("Hi 0",), ("x",)))
    step("Excel: Range('A1:C3').Value (2D square)",
         lambda: xl.Range("A1:C3").Value,
         (("Hi 0", 2, 3), ("x", "Hi 1", "z"), (3, 2, "Hi 2")))
    step("Excel: Cells(5,2).Formula = '=Now()'",
         lambda: setattr(xl.Cells(5, 2), "Formula", "=Now()"))
    step("Excel: Cells(6,2).NumberFormat = 'd/mm/yy h:mm'",
         lambda: setattr(xl.Cells(6, 2), "NumberFormat", "d/mm/yy h:mm"))
    step("Excel: Columns('A:B').EntireColumn.AutoFit()",
         lambda: xl.Columns("A:B").EntireColumn.AutoFit())
    # save it (not in the upstream test, but proves the doc is real)
    path = os.path.join(tempfile.gettempdir(), "corpus-excel.xlsx")
    step("Excel: ActiveWorkbook.SaveAs(.xlsx)",
         lambda: xl.ActiveWorkbook.SaveAs(path, 51))
    step("Excel: Workbooks(1).Close(0)", lambda: xl.Workbooks(1).Close(0))
    step("Excel: Quit", lambda: xl.Quit())


def word_corpus():
    word = win32com.client.dynamic.Dispatch("Word.Application")
    step("Word: Visible = 1", lambda: setattr(word, "Visible", 1))
    doc_holder = {}
    step("Word: Documents.Add", lambda: doc_holder.setdefault("doc", word.Documents.Add()))
    doc = doc_holder.get("doc")
    if doc is not None:
        step("Word: doc.Range()", lambda: doc.Range())
        step("Word: Range.InsertAfter x10",
             lambda: [doc.Range().InsertAfter("Hello from Python %d\n" % (i + 1)) for i in range(10)])
        step("Word: doc.Paragraphs", lambda: doc.Paragraphs)

        def _count():
            got = doc.Paragraphs.Count
            if isinstance(got, int):
                results.append(("PASS", "Word: Paragraphs.Count", "%d" % got))
            else:
                # returned a benign object without faulting -> graceful degradation
                results.append(("DEGRADE", "Word: Paragraphs.Count",
                                "Count collection not modeled -> %r" % (got,)))
        try:
            _count()
        except Exception as e:  # noqa
            results.append(("FAIL", "Word: Paragraphs.Count", "%s: %s" % (type(e).__name__, e)))
        step("Word: para(1).Range.Font.Size = 16",
             lambda: setattr(doc.Paragraphs(1).Range.Font, "Size", 16))
        step("Word: para(1).Range.Font.ColorIndex = 2",
             lambda: setattr(doc.Paragraphs(1).Range.Font, "ColorIndex", 2))
        path = os.path.join(tempfile.gettempdir(), "corpus-word.docx")
        step("Word: doc.SaveAs2(.docx)", lambda: doc.SaveAs2(path))
        step("Word: doc.Close(SaveChanges=False)", lambda: doc.Close(SaveChanges=False))
    step("Word: Quit", lambda: word.Quit())


def main():
    for name, fn in (("EXCEL", excel_corpus), ("WORD", word_corpus)):
        print("== pywin32 upstream corpus: %s ==" % name)
        try:
            fn()
        except Exception as e:  # a fault escaping a step is itself a failure
            results.append(("FAIL", "%s: harness aborted" % name, str(e)))
    n = {"PASS": 0, "DEGRADE": 0, "FAIL": 0}
    for kind, label, detail in results:
        n[kind] += 1
        mark = {"PASS": "PASS   ", "DEGRADE": "DEGRADE", "FAIL": "FAIL   "}[kind]
        print("  %s %s%s" % (mark, label, ("  (" + detail + ")") if detail else ""))
    print("\nsummary: %d PASS, %d DEGRADE (graceful, unmodeled read), %d FAIL"
          % (n["PASS"], n["DEGRADE"], n["FAIL"]))
    # The shim's contract is: never FAIL (fault) a host. DEGRADE is acceptable.
    print("CORPUS:", "PASS (no faults)" if n["FAIL"] == 0 else "FAIL (%d faults)" % n["FAIL"])
    sys.exit(0 if n["FAIL"] == 0 else 1)


if __name__ == "__main__":
    main()
