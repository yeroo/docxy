#!/usr/bin/env python3
"""Generate the .mpp cases for issue #68 that the other corpora lack.

- blank (null) task rows between real tasks;
- rows inserted, moved or re-indented after creation, so the task UID order
  differs from the task ID (row) order.

Drives a licensed Microsoft Project over COM. Each case is saved as Project's
native .mpp plus Project's own MSPDI .xml export (the oracle) into
corpus/mpp/order/, which is git-ignored like every other .mpp/.xml there.
mppread/tests/oracle_corpus.rs compares the decoder against these when present.

Usage (from the repo root, Windows):
    python corpus/tools/gen_mpp_order_cases.py
Requires: Microsoft Project desktop and pywin32. Close Project gracefully if a
run is interrupted; never force-kill WINPROJ.EXE (it wedges COM activation).
"""

import os
import sys

import win32com.client as win32

ANCHOR = "3/2/2026 8:00 AM"  # Monday; Project rejects ISO date strings
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mpp", "order")
PJ_MPP, PJ_XML = "MSProject.MPP", "MSProject.XML"


def new_plan(app):
    app.FileNew()  # NO arguments: a False here is read as a filename
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    return p


def add(p, name, days, before=None):
    t = p.Tasks.Add(name) if before is None else p.Tasks.Add(name, before)
    t.Duration = f"{days}d"
    return t


def insert_blank_row(app, row):
    """Insert an empty (null) task row above grid row `row`."""
    app.SelectRow(Row=row, RowRelative=False)
    app.RowInsert()


def save(app, slug):
    for ext, fmt in ((".xml", PJ_XML), (".mpp", PJ_MPP)):
        path = os.path.abspath(os.path.join(OUT, slug + ext))
        if os.path.exists(path):
            os.remove(path)
        app.CalculateProject()
        app.FileSaveAs(Name=path, FormatID=fmt)
    app.FileCloseEx(0)
    print("wrote " + slug)


def blank_rows(app):
    p = new_plan(app)
    a, b, c = add(p, "A", 2), add(p, "B", 2), add(p, "C", 1)
    b.Predecessors = str(a.ID)
    c.Predecessors = str(b.ID)
    insert_blank_row(app, 2)  # blank above B
    insert_blank_row(app, 4)  # blank above C
    save(app, "o1-blank-rows")


def inserted_task(app):
    p = new_plan(app)
    a, b = add(p, "A", 2), add(p, "B", 2)
    c = add(p, "C inserted", 1, before=2)  # UID 3, ID 2
    c.Predecessors = str(a.ID)
    b.Predecessors = str(c.ID)
    save(app, "o2-inserted-task")


def inserted_hierarchy(app):
    p = new_plan(app)
    ph, x, y = add(p, "Phase", 1), add(p, "X", 2), add(p, "Y", 3)
    x.OutlineIndent()
    y.OutlineIndent()
    y.Predecessors = str(x.ID)
    w = add(p, "W inserted child", 1, before=x.ID)
    if w.OutlineLevel < 2:
        w.OutlineIndent()
    x.Predecessors = str(w.ID)
    z = add(p, "Z inserted top", 1, before=1)
    while z.OutlineLevel > 1:
        z.OutlineOutdent()
    save(app, "o3-inserted-hierarchy")


def moved_rows(app):
    p = new_plan(app)
    for n in "ABCD":
        add(p, n, 1)
    # Project reissues UIDs on this cut/paste path: rows become D, A, C, B.
    app.SelectRow(Row=4, RowRelative=False)
    app.EditCut()
    app.SelectRow(Row=1, RowRelative=False)
    app.EditPaste()
    app.SelectRow(Row=3, RowRelative=False)
    app.EditCut()
    app.SelectRow(Row=4, RowRelative=False)
    app.EditPaste()
    save(app, "o4-moved-rows")


def main():
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        for case in (blank_rows, inserted_task, inserted_hierarchy, moved_rows):
            case(app)
    finally:
        for call in (lambda: app.FileCloseAll(0), lambda: app.Quit(0)):
            try:
                call()
            except Exception:
                pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
