#!/usr/bin/env python3
"""Generate paired Microsoft Project MPP/MSPDI cases for task constraints (#502).

Run on Windows with Project and pywin32: python corpus/tools/gen_mpp_constraint_cases.py
Each named case may also be passed alone. Output is ignored under
corpus/mpp/constraints/. Project's XML is the oracle for the binary records.
"""

import os
import sys
import xml.etree.ElementTree as ET

import win32com.client as win32


ANCHOR = "3/2/2026 8:00 AM"
OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mpp", "constraints"))
FORMATS = ((".xml", "MSProject.XML"), (".mpp", "MSProject.MPP"))
NS = "{http://schemas.microsoft.com/project}"


def new_plan(app, start=ANCHOR):
    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = start
    p.NewTasksCreatedAsManual = False
    return p


def add(p, name, days=2):
    t = p.Tasks.Add(name)
    t.Duration = f"{days}d"
    return t


def constrain(t, kind, date=None):
    t.ConstraintType = kind
    if date is not None:
        t.ConstraintDate = date


def save(app, slug, expected):
    app.CalculateProject()
    for ext, fmt in FORMATS:
        path = os.path.join(OUT, slug + ext)
        if os.path.exists(path):
            os.remove(path)
        app.FileSaveAs(Name=path, FormatID=fmt)
    root = ET.parse(os.path.join(OUT, slug + ".xml")).getroot()
    tasks = {}
    for t in root.findall(f"{NS}Tasks/{NS}Task"):
        name = t.findtext(f"{NS}Name")
        if name:
            tasks[name] = {child.tag.removeprefix(NS): child.text for child in t}
    for name, code in expected:
        got = tasks.get(name, {}).get("ConstraintType")
        if got != str(code):
            raise AssertionError(f"{slug}: {name}.ConstraintType = {got!r}, wanted {code}")
    app.FileCloseEx(0)
    print("wrote", slug)
    return tasks


def all_types(app):
    """One auto task per constraint code, each after a 2-day lead task."""
    p = new_plan(app)
    lead = add(p, "Lead", 2)
    cases = (
        ("ASAP", 0, None),
        ("ALAP", 1, None),
        ("MSO", 2, "3/11/2026 8:00 AM"),
        ("MFO", 3, "3/13/2026 5:00 PM"),
        ("SNET", 4, "3/9/2026 1:00 PM"),
        ("SNLT", 5, "3/12/2026 8:00 AM"),
        ("FNET", 6, "3/16/2026 5:00 PM"),
        ("FNLT", 7, "3/20/2026 5:00 PM"),
    )
    expected = []
    for name, code, date in cases:
        t = add(p, name, 1)
        t.Predecessors = str(lead.ID)
        constrain(t, code, date)
        expected.append((name, code))
    # ALAP only moves a task that has slack: give it a later sibling finish.
    add(p, "Tail", 10)
    save(app, "k1-all-types", expected)


def rows(app):
    """Summary, manual and unlinked rows, with a project start after the anchor."""
    p = new_plan(app, "3/4/2026 8:00 AM")
    summary = add(p, "Summary")
    child = add(p, "Child", 1)
    child.OutlineIndent()
    constrain(summary, 4, "3/9/2026 8:00 AM")
    manual = add(p, "Manual", 1)
    manual.Manual = True
    constrain(manual, 6, "3/18/2026 5:00 PM")
    free = add(p, "Free SNLT", 1)
    constrain(free, 5, "3/25/2026 8:00 AM")
    save(app, "k2-rows", [("Summary", 4), ("Manual", 6), ("Free SNLT", 5), ("Child", 0)])


def main():
    os.makedirs(OUT, exist_ok=True)
    cases = (all_types, rows)
    selected = sys.argv[1:]
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        for case in (c for c in cases if not selected or c.__name__ in selected):
            case(app)
    finally:
        for call in (lambda: app.FileCloseAll(0), lambda: app.Quit(0)):
            try:
                call()
            except Exception:
                pass


if __name__ == "__main__":
    main()
