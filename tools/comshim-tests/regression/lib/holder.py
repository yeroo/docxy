"""Second-client test, client 1 (the holder) and client 2 share this script.

  python holder.py hold   word|excel <out-file> <ready-file> <go-file>
  python holder.py second word|excel <out-file>

hold:   create the Application + a document, write 'before' content, signal
        ready, block until go-file appears (max 90 s), then keep using the SAME
        Application object (write 'after', SaveAs, Close, Quit).
second: create the Application, make + save a document, Quit, exit.
Prints STEP lines; exit 0 only if every step worked.
"""
import os
import sys
import time

mode, kind, out = sys.argv[1], sys.argv[2], sys.argv[3]
import win32com.client as wc  # noqa: E402

progid = "Word.Application" if kind == "word" else "Excel.Application"


def step(msg):
    print("STEP %.2f %s" % (time.time(), msg), flush=True)


def begin(tag):
    app = wc.Dispatch(progid)
    step("created " + progid)
    if kind == "word":
        doc = app.Documents.Add()
        app.Selection.TypeText(tag + "-before ")
    else:
        doc = app.Workbooks.Add()
        doc.Worksheets(1).Range("A1").Value = tag + "-before"
    step("wrote before-content")
    return app, doc


def end(app, doc, tag):
    if kind == "word":
        app.Selection.TypeText(tag + "-after")
        doc.SaveAs2(out)
        doc.Close()
    else:
        doc.Worksheets(1).Range("B1").Value = tag + "-after"
        doc.SaveAs(out, 51)
        doc.Close(False)
    step("saved " + out)
    app.Quit()
    step("quit")


try:
    if mode == "hold":
        ready, go = sys.argv[4], sys.argv[5]
        app, doc = begin("holder")
        open(ready, "w").write(str(os.getpid()))
        step("ready; waiting for go")
        t0 = time.time()
        while not os.path.exists(go):
            if time.time() - t0 > 90:
                raise RuntimeError("timed out waiting for go")
            time.sleep(0.2)
        step("go received")
        end(app, doc, "holder")
    else:
        app, doc = begin("second")
        end(app, doc, "second")
    del app, doc
    step("released")
    print("RESULT: OK")
    sys.exit(0)
except Exception as e:
    print("RESULT: FAIL %s: %s" % (type(e).__name__, e), flush=True)
    sys.exit(1)
