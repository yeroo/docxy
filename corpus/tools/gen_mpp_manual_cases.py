#!/usr/bin/env python3
"""Generate the .mpp cases for issue #122: task mode and manual dates.

- m0-toggle-auto / m0-toggle-manual: one plan saved twice, identical except
  that one unlinked task is switched auto -> manual, so the task records
  differ only in the mode;
- m1-manual-and-auto: an auto task, a manual task linked FS after it but
  started before it finishes, and a plain auto task; new tasks are auto;
- m2-new-tasks-manual: m1 with only the project's new-task default flipped
  to manual;
- m3*: attempts to make ManualStart/ManualFinish/ManualDuration differ from
  Start/Finish/Duration.

DIVERGENCE: Project 2024 normalized every m3 attempt. Its XML writes the
manual fields equal to Start/Finish/Duration for a manual task switched back to
auto after its link moved it (m3a), a typed finish (m3b), an estimated duration
(m3c), and tasks with blank dates (m3d). The .mpp stores the manual fields
separately (Fixed2Data), and stores them only for manual tasks, apart from
stale bytes on moved rows.

Drives a licensed Microsoft Project over COM. Each case is saved as Project's
native .mpp plus Project's own MSPDI .xml export (the oracle) into
corpus/mpp/manual/, which is git-ignored like every other .mpp/.xml there.
mppread/tests/oracle_corpus.rs and real_mpp.rs compare the decoder against
these when present.

Usage (from the repo root, Windows):
    python corpus/tools/gen_mpp_manual_cases.py
Requires: Microsoft Project desktop and pywin32. Close Project gracefully if a
run is interrupted; never force-kill WINPROJ.EXE (it wedges COM activation).
"""

import os
import sys

import win32com.client as win32

ANCHOR = "3/2/2026 8:00 AM"  # Monday; Project rejects ISO date strings
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mpp", "manual")
PJ_MPP, PJ_XML = "MSProject.MPP", "MSProject.XML"


def new_plan(app, manual_default=False):
    app.FileNew()  # NO arguments: a False here is read as a filename
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = manual_default
    return p


def add(p, name, days):
    t = p.Tasks.Add(name)
    t.Duration = f"{days}d"
    return t


def save(app, slug):
    for ext, fmt in ((".xml", PJ_XML), (".mpp", PJ_MPP)):
        path = os.path.abspath(os.path.join(OUT, slug + ext))
        if os.path.exists(path):
            os.remove(path)
        app.CalculateProject()
        app.FileSaveAs(Name=path, FormatID=fmt)
    app.FileCloseEx(0)
    print("wrote " + slug)


def toggle(app):
    for slug, manual in (("m0-toggle-auto", False), ("m0-toggle-manual", True)):
        p = new_plan(app)
        add(p, "A", 2)
        b = add(p, "B toggled", 3)
        add(p, "C", 1)
        if manual:
            b.Manual = True
        save(app, slug)


def manual_and_auto(app, slug, manual_default):
    p = new_plan(app)
    a = add(p, "A auto", 3)
    m = add(p, "M manual", 2)
    add(p, "P plain auto", 1)
    m.Predecessors = str(a.ID)
    m.Manual = True
    # Pull the manual task back before A finishes: a link cannot move it.
    m.Start = "3/3/2026 8:00 AM"
    p.NewTasksCreatedAsManual = manual_default
    save(app, slug)


def switched_to_auto(app):
    p = new_plan(app)
    a = add(p, "A", 2)
    s = add(p, "S switched", 2)
    s.Predecessors = str(a.ID)
    s.Manual = True
    s.Start = ANCHOR
    a.Duration = "4d"  # the link would now push S; as a manual task it stays
    s.Manual = False  # Project reschedules S after A
    save(app, "m3a-switched-to-auto")


def typed_finish(app):
    p = new_plan(app)
    f = add(p, "F typed finish", 2)
    f.Manual = True
    f.Start = ANCHOR
    f.Finish = "3/9/2026 5:00 PM"
    save(app, "m3b-typed-finish")


def estimated(app):
    p = new_plan(app)
    e = p.Tasks.Add("E estimated")
    e.Manual = True
    e.Duration = "2d?"
    save(app, "m3c-estimated")


def blank_dates(app):
    # Created under the manual default with only a name (no start, finish or
    # duration), and one with only a duration: Project shows blank dates.
    p = new_plan(app, manual_default=True)
    p.Tasks.Add("N name only")
    d = p.Tasks.Add("D duration only")
    d.Duration = "3d"
    save(app, "m3d-blank-dates")


def main():
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        toggle(app)
        manual_and_auto(app, "m1-manual-and-auto", False)
        manual_and_auto(app, "m2-new-tasks-manual", True)
        for case in (switched_to_auto, typed_finish, estimated, blank_dates):
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
