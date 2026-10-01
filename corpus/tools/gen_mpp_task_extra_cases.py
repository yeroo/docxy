#!/usr/bin/env python3
"""Generate paired Microsoft Project MPP/MSPDI cases for the #417 task fields (#521).

Run on Windows with Project and pywin32: python corpus/tools/gen_mpp_task_extra_cases.py
Each named case may also be passed alone. Output is ignored under
corpus/mpp/task-extra/. Fields COM cannot set are seeded through a temporary
MSPDI file opened in Project. Every case is saved as .mpp, closed, reopened
from that .mpp and only then exported as .xml, so the oracle shows what the
binary kept rather than what Project still held in memory. Close Project
gracefully if a run is interrupted; never force-kill WINPROJ.EXE.
"""

import os
import sys
import tempfile
import xml.etree.ElementTree as ET

import pywintypes
import win32com.client as win32


ANCHOR = "3/2/2026 8:00 AM"
OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mpp", "task-extra"))
NSU = "http://schemas.microsoft.com/project"
NS = "{" + NSU + "}"
ET.register_namespace("", NSU)

# Seeded MSPDI fields: (slug, element, value). Each seed holds a plain task
# and one task, named after the element, carrying the value.
SEEDS = (
    ("e3-display-as-summary", "DisplayAsSummary", "1"),
    ("e4-wbs-level", "WBSLevel", "Level 2"),
    ("e5-commitment-start", "CommitmentStart", "2026-03-03T08:00:00"),
    ("e6-commitment-finish", "CommitmentFinish", "2026-03-04T17:00:00"),
    ("e7-commitment-type", "CommitmentType", "2"),
    ("e8-pre-leveled-start", "PreLeveledStart", "2026-03-05T08:00:00"),
    ("e9-pre-leveled-finish", "PreLeveledFinish", "2026-03-06T17:00:00"),
    ("e10-status-manager", "StatusManager", "Alice"),
)
# Seeded fields Project desktop drops on an .mpp save (README: Known decode gaps).
GAPS = {"StatusManager"}


def new_plan(app):
    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    return p


def add(p, name, days=2):
    t = p.Tasks.Add(name)
    t.Duration = f"{days}d"
    return t


def open_file(app, path):
    before = app.Projects.Count
    if not app.FileOpenEx(path, True) or app.Projects.Count != before + 1:
        raise RuntimeError(f"Project did not open {path}")


def save(app, slug):
    """Save the active plan as .mpp, reopen it, export .xml and return its tasks."""
    mpp, xml = (os.path.join(OUT, slug + ext) for ext in (".mpp", ".xml"))
    for path in (mpp, xml):
        if os.path.exists(path):
            os.remove(path)
    app.CalculateProject()
    app.FileSaveAs(Name=mpp, FormatID="MSProject.MPP")
    app.FileCloseEx(0)
    open_file(app, mpp)
    try:
        app.FileSaveAs(Name=xml, FormatID="MSProject.XML")
    finally:
        app.FileCloseEx(0)
    tasks = {}
    for t in ET.parse(xml).getroot().findall(f"{NS}Tasks/{NS}Task"):
        tasks[t.findtext(f"{NS}Name")] = {c.tag.removeprefix(NS): c.text for c in t}
    print("wrote", slug, flush=True)
    return tasks


def expect(slug, tasks, name, field, value):
    got = tasks.get(name, {}).get(field)
    if got != value:
        raise AssertionError(f"{slug}: {name}.{field} = {got!r}, wanted {value!r}")


def contact(app):
    p = new_plan(app)
    add(p, "Plain")
    add(p, "Contact").Contact = "Site lead"
    add(p, "Long contact").Contact = "Jürgen Ölmann, Строитель"
    tasks = save(app, "e1-contact")
    expect("e1-contact", tasks, "Plain", "Contact", None)
    expect("e1-contact", tasks, "Contact", "Contact", "Site lead")
    expect("e1-contact", tasks, "Long contact", "Contact", "Jürgen Ölmann, Строитель")


def published(app):
    # Snapshot rows all match `!summary && active`; flip both directions.
    p = new_plan(app)
    add(p, "Plain")
    add(p, "Unpublished").IsPublished = False
    # Project refuses IsPublished on a summary or inactive task, so set it
    # while the task is an ordinary one.
    add(p, "Published summary").IsPublished = True
    add(p, "Summary child").OutlineIndent()
    plain_summary = add(p, "Plain summary")
    plain_summary.OutlineOutdent()
    add(p, "Plain child").OutlineIndent()
    inactive = add(p, "Published inactive")
    inactive.OutlineOutdent()
    inactive.IsPublished = True
    inactive.Active = False
    add(p, "Plain inactive").Active = False
    tasks = save(app, "e2-published")
    # Project exports IsPublished=0 for every summary and inactive task, even
    # one published while it was ordinary: only the ordinary direction exports.
    for name, value in (("Plain", "1"), ("Unpublished", "0"), ("Published summary", "0"),
                        ("Plain summary", "0"), ("Published inactive", "0"),
                        ("Plain inactive", "0")):
        expect("e2-published", tasks, name, "IsPublished", value)
    expect("e2-published", tasks, "Published summary", "Summary", "1")
    expect("e2-published", tasks, "Published inactive", "Active", "0")


def leveled(app):
    # Real leveling: Project records the pre-leveled dates of the task it moves.
    p = new_plan(app)
    r = p.Resources.Add("Shared")
    for name in ("First", "Second"):
        add(p, name).Assignments.Add(ResourceID=r.ID).Units = 1
    add(p, "Unassigned")
    app.LevelNow(True)
    tasks = save(app, "e11-leveled")
    # Leveling records pre-leveled dates on every row; the delayed task's
    # differ from its leveled dates, which separates the two fields.
    for name, row in tasks.items():
        if not row.get("PreLeveledStart") or not row.get("PreLeveledFinish"):
            raise AssertionError(f"e11-leveled: {name} has no pre-leveled dates")
    if not any(row["PreLeveledStart"] != row["Start"] for row in tasks.values()):
        raise AssertionError("e11-leveled: leveling moved no task")


def child(parent, tag, value=None):
    node = ET.SubElement(parent, f"{NS}{tag}")
    if value is not None:
        node.text = str(value)
    return node


def seed(path, slug, field, value):
    root = ET.Element(f"{NS}Project")
    for tag, v in (("Name", slug), ("MinutesPerDay", 480), ("MinutesPerWeek", 2400),
                   ("CalendarUID", 1), ("StartDate", "2026-03-02T08:00:00"),
                   ("ProjectExternallyEdited", 0), ("NewTasksAreManual", 0)):
        child(root, tag, v)
    tasks = child(root, "Tasks")
    for uid, name, extra in ((1, "Plain", None), (2, field, (field, value))):
        task = child(tasks, "Task")
        for tag, v in (("UID", uid), ("ID", uid), ("Name", name),
                       ("Start", "2026-03-02T08:00:00"), ("Duration", "PT16H0M0S")):
            child(task, tag, v)
        if extra:
            child(task, *extra)
    cal = child(child(root, "Calendars"), "Calendar")
    for tag, v in (("UID", 1), ("Name", "Standard"), ("IsBaseCalendar", 1)):
        child(cal, tag, v)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def seeded(app):
    with tempfile.TemporaryDirectory(prefix="seed-", dir=OUT) as temp:
        for slug, field, value in SEEDS:
            source = os.path.join(temp, slug + ".xml")
            seed(source, slug, field, value)
            open_file(app, source)
            tasks = save(app, slug)
            got = tasks.get(field, {}).get(field)
            if field in GAPS:
                # Evidence for a decode gap: the field must not survive an .mpp save.
                if got is not None:
                    raise AssertionError(f"{slug}: Project now keeps {field}={got!r}; decode it")
                print(f"{slug}: Project did not keep {field}={value!r} through .mpp; "
                      "left as a decode gap", flush=True)
                continue
            expect(slug, tasks, field, field, value)
            # The plain neighbour must differ, or the diff locates nothing.
            if tasks["Plain"].get(field) == value:
                raise AssertionError(f"{slug}: Plain.{field} also = {value!r}")


def main():
    try:
        win32.GetActiveObject("MSProject.Application")
    except pywintypes.com_error:
        pass
    else:
        print("Microsoft Project is already running; close it first.")
        return 2
    os.makedirs(OUT, exist_ok=True)
    cases = (contact, published, seeded, leveled)
    selected = sys.argv[1:]
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        for case in (c for c in cases if not selected or c.__name__ in selected):
            case(app)
    finally:
        # Every case closes what it opened, so open projects are a failed case's
        # unsaved scratch plans.
        if app.Projects.Count:
            app.FileCloseAll(0)
        app.Quit(0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
