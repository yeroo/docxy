"""pywin32 conformance test for the Word shim (wordcomshim).

pywin32 (`win32com.client`) binds late over IDispatch and introspects each object's
**type information**, so the shim implements `IDispatch::GetTypeInfo` per object,
sourced from our authored docxy-word.tlb dispinterfaces (Word's real dispids +
property/method kinds). Drives the create + formatting path and validates the
produced .docx directly (no Word needed to open it).

Run (with the shim installed + typelib registered):  python pywin32_conformance.py
Exit code 0 = PASS. Requires `pip install pywin32`; skips cleanly if absent.
"""
import os
import sys
import tempfile
import zipfile

try:
    import win32com.client as win32
    from win32com.client import gencache
except ImportError:
    print("SKIP: pywin32 not installed (pip install pywin32)")
    sys.exit(0)


def build(path):
    wd = win32.Dispatch("Word.Application")
    wd.Visible = False
    doc = wd.Documents.Add()
    sel = wd.Selection
    sel.Font.Bold = True
    sel.TypeText("PyWin32BoldTitle")
    sel.TypeParagraph()
    sel.Font.Bold = False
    sel.Font.Size = 14
    sel.TypeText("PyWin32SizedBody")
    sel.TypeParagraph()
    # Set alignment BEFORE the paragraph is finalized (its mark carries alignment).
    sel.ParagraphFormat.Alignment = 1  # wdAlignParagraphCenter
    sel.TypeText("PyWin32Centered")
    sel.TypeParagraph()
    doc.SaveAs2(path)
    doc.Close()
    wd.Quit()


def validate(path):
    if not os.path.exists(path):
        return "no file produced"
    z = zipfile.ZipFile(path)
    names = z.namelist()
    if "word/document.xml" not in names:
        return "missing word/document.xml"
    x = z.read("word/document.xml").decode("utf-8", "replace")
    checks = {
        "bold title text": "PyWin32BoldTitle" in x,
        "sized body text": "PyWin32SizedBody" in x,
        "centered text": "PyWin32Centered" in x,
        "bold run (<w:b)": "<w:b" in x,
        "font size (<w:sz)": "<w:sz" in x,
        "center alignment": 'w:val="center"' in x,
    }
    # gridcore/docxcore never write docProps/app.xml; real Word always does.
    if "docProps/app.xml" in names:
        return "docProps/app.xml present -> real Word served, not the shim"
    missing = [k for k, v in checks.items() if not v]
    return "ok" if not missing else "missing: " + ", ".join(missing)


def main():
    gencache.Rebuild()
    path = os.path.join(tempfile.gettempdir(), "pywin32-word-conformance.docx")
    if os.path.exists(path):
        os.remove(path)
    try:
        build(path)
    except Exception as e:
        print("  FAIL: raised %s: %s" % (type(e).__name__, e))
        sys.exit(1)
    r = validate(path)
    print("  %s: %s (%s)"
          % ("PASS" if r == "ok" else "FAIL", r,
             "%d bytes" % os.path.getsize(path) if os.path.exists(path) else "no file"))
    print("PYWIN32 WORD CONFORMANCE:", "PASS" if r == "ok" else "FAIL")
    sys.exit(0 if r == "ok" else 1)


if __name__ == "__main__":
    main()
