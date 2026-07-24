"""pywin32 conformance test for the Excel shim (xlcomshim).

pywin32 (`win32com.client`) is an independent, non-.NET COM automation client and
the runtime many engineering apps embed. It binds late over IDispatch and, unlike
VBScript, introspects each object's **type information** — which is why the shim
implements `IDispatch::GetTypeInfo` per object, sourced from our authored
docxy-excel.tlb dispinterfaces (real dispids + property/method kinds).

Drives BOTH pywin32 binding modes end to end and validates the produced .xlsx
directly (no Excel needed to open it):
  * Dispatch          — pure late-bound; pywin32 re-introspects each returned object.
  * gencache.EnsureDispatch — early-bound via makepy over our typelib.

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

XL_XLSX = 51  # xlOpenXMLWorkbook


def build(app_factory, path):
    xl = app_factory("Excel.Application")
    xl.Visible = False
    wb = xl.Workbooks.Add()
    ws = wb.Worksheets(1)
    ws.Range("A1").Value = "PyWin32Conformance"
    ws.Cells(2, 1).Value = 10
    ws.Cells(3, 1).Value = 32.5
    ws.Range("A4").Formula = "=SUM(A2:A3)"
    ws.Range("A1").Font.Bold = True
    wb.SaveAs(path, XL_XLSX)
    wb.Close(False)
    xl.Quit()


def validate(path):
    if not os.path.exists(path):
        return "no file produced"
    z = zipfile.ZipFile(path)
    names = z.namelist()
    if "xl/workbook.xml" not in names:
        return "missing xl/workbook.xml"
    blob = b"".join(z.read(n) for n in names if n.endswith(".xml"))
    if b"PyWin32Conformance" not in blob:
        return "cell text not found"
    # gridcore never writes docProps/app.xml; real Excel always does. Its absence
    # proves the shim served, not a real Excel that happens to be installed.
    if "docProps/app.xml" in names:
        return "docProps/app.xml present -> real Excel served, not the shim"
    return "ok"


def main():
    tmp = tempfile.gettempdir()
    gencache.Rebuild()  # regenerate makepy wrappers from the currently-registered tlb
    ok = True
    for mode, factory in (
        ("Dispatch (late-bound)", win32.Dispatch),
        ("EnsureDispatch (early/makepy)", gencache.EnsureDispatch),
    ):
        path = os.path.join(tmp, "pywin32-xl-%s.xlsx" % mode.split()[0].lower())
        if os.path.exists(path):
            os.remove(path)
        try:
            build(factory, path)
        except Exception as e:
            print("  FAIL [%s]: raised %s: %s" % (mode, type(e).__name__, e))
            ok = False
            continue
        r = validate(path)
        print("  %s [%s]: %s (%s)"
              % ("PASS" if r == "ok" else "FAIL", mode, r,
                 "%d bytes" % os.path.getsize(path) if os.path.exists(path) else "no file"))
        ok = ok and r == "ok"

    print("PYWIN32 EXCEL CONFORMANCE:", "PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
