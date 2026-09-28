#!/usr/bin/env python3
"""Generate paired Microsoft Project MPP/MSPDI cases for task fields (#161).

Run on Windows with Project and pywin32: python corpus/tools/gen_mpp_task_field_cases.py
Each named case may also be passed alone. Output is ignored under
corpus/mpp/task-fields/. Project's XML is the oracle for the binary records.
"""

import os
import sys
import xml.etree.ElementTree as ET

import win32com.client as win32


ANCHOR = "3/2/2026 8:00 AM"
OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mpp", "task-fields"))
FORMATS = ((".xml", "MSProject.XML"), (".mpp", "MSProject.MPP"))
NS = "{http://schemas.microsoft.com/project}"


def new_plan(app):
    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    return p


def add(p, name, days=2, before=None):
    t = p.Tasks.Add(name) if before is None else p.Tasks.Add(name, before)
    t.Duration = f"{days}d"
    return t


def save(app, slug, expected, null_min=0):
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
    nulls = [t for t in root.findall(f"{NS}Tasks/{NS}Task") if t.findtext(f"{NS}IsNull") == "1"]
    if len(nulls) < null_min:
        raise AssertionError(f"{slug}: {len(nulls)} null rows, wanted {null_min}")
    for name, field, value in expected:
        got = tasks.get(name, {}).get(field)
        if value is not None and got != value:
            raise AssertionError(f"{slug}: {name}.{field} = {got!r}, wanted {value!r}")
        if value is None and got is None:
            raise AssertionError(f"{slug}: {name}.{field} absent")
    app.FileCloseEx(0)
    print("wrote", slug)


def flags(app):
    p = new_plan(app)
    add(p, "Baseline")
    changes = (
        ("Inactive", "Active", False, "0"),
        ("Effort driven", "EffortDriven", True, "1"),
        ("Estimated", "Estimated", True, "1"),
        ("No level assignments", "LevelIndividualAssignments", False, "0"),
        ("No split", "LevelingCanSplit", False, "0"),
        ("Ignore calendar", "IgnoreResourceCalendar", True, "1"),
        ("Hide bar", "HideBar", True, "1"),
        ("Rollup", "Rollup", True, "1"),
        ("Milestone", "Milestone", True, "1"),
    )
    checks = []
    for name, prop, value, xml in changes:
        t = add(p, name)
        if prop == "IgnoreResourceCalendar":
            app.BaseCalendarCreate(Name="Task calendar")
            t.Calendar = "Task calendar"
        setattr(t, prop, value)
        checks.append((name, "LevelAssignments" if prop == "LevelIndividualAssignments" else prop, xml))
    save(app, "f1-flags", checks)


def values(app):
    p = new_plan(app)
    add(p, "Baseline")
    checks = []
    for name, code in (("Fixed units", 0), ("Fixed duration", 1), ("Fixed work", 2)):
        t = add(p, name)
        t.Type = code
        checks.append((name, "Type", str(code)))
    for value in (0, 500, 1000, 317):
        name = f"Priority {value}"
        t = add(p, name)
        t.Priority = value
        checks.append((name, "Priority", str(value)))
    t = add(p, "Deadline")
    t.Deadline = "3/20/2026 5:00 PM"
    checks.append(("Deadline", "Deadline", None))
    t = add(p, "Level delay")
    t.LevelingDelay = "2d"
    checks.append(("Level delay", "LevelingDelay", None))
    t = add(p, "Elapsed delay")
    t.LevelingDelay = "2ed"
    checks.append(("Elapsed delay", "LevelingDelayFormat", None))
    t = add(p, "Physical EV")
    t.EarnedValueMethod = 1
    checks.append(("Physical EV", "EarnedValueMethod", "1"))
    t = add(p, "WBS custom")
    t.WBS = "ABC.42"
    checks.append(("WBS custom", "WBS", "ABC.42"))
    save(app, "f2-values", checks)


def blanks(app):
    p = new_plan(app)
    add(p, "Summary")
    child = add(p, "Child")
    child.OutlineIndent()
    add(p, "After")
    app.SelectRow(Row=2, RowRelative=False)
    app.RowInsert()
    app.SelectRow(Row=4, RowRelative=False)
    app.RowInsert()
    former = add(p, "Former task")
    former.Delete()
    app.SelectRow(Row=6, RowRelative=False)
    app.RowInsert()
    save(app, "f3-blanks", [], null_min=3)


def overflow(app):
    p = new_plan(app)
    r = p.Resources.Add("Shared")
    r.MaxUnits = 0.5
    for name in ("A", "B"):
        t = add(p, name)
        t.Assignments.Add(ResourceID=r.ID).Units = 1
    safe = p.Resources.Add("Safe")
    t = add(p, "C safe")
    t.Assignments.Add(ResourceID=safe.ID).Units = 1
    save(app, "f4-overallocated", [("A", "OverAllocated", "1"), ("C safe", "OverAllocated", "0")])


def subprojects(app):
    p = new_plan(app)
    add(p, "Child task")
    save(app, "f6-child", [])
    child = os.path.join(OUT, "f6-child.mpp")
    p = new_plan(app)
    add(p, "Other child task")
    save(app, "f6-child-b", [])
    other = os.path.join(OUT, "f6-child-b.mpp")
    p = new_plan(app)
    add(p, "Normal")
    s = add(p, "Sub A")
    s.Subproject = child
    r = add(p, "Sub B")
    r.Subproject = other
    r.SubProjectReadOnly = True
    save(app, "f6-subprojects", [("Sub A", "IsSubproject", "1"), ("Sub B", "IsSubprojectReadOnly", "1")])


def external(app):
    p = new_plan(app)
    add(p, "Remote predecessor")
    save(app, "f7-source", [])
    source = os.path.join(OUT, "f7-source.mpp")
    p = new_plan(app)
    t = add(p, "Local successor")
    app.OptionsViewEx(DisplayExternalPredecessors=True)
    t.Predecessors = source + "\\1"
    if not any(task.ExternalTask for task in p.Tasks if task is not None):
        raise AssertionError("cross-project link did not create an external task")
    save(app, "f7-external", [])


def main():
    os.makedirs(OUT, exist_ok=True)
    cases = (flags, values, blanks, overflow, subprojects, external)
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
