#!/usr/bin/env python3
"""Generate the .mpp cases for issue #181: task progress fields.

Each case separates progress fields that the generated snapshots leave equal
or zero:

- p1-states: not started, in progress and complete tasks under a summary, and
  a manual task with progress (its Fixed2 manual fields and its FixedData
  progress on one row);
- p2-work: a work resource, so ActualWork/RemainingWork are nonzero, with
  PercentWorkComplete made to differ from PercentComplete by entering actual
  work; a task on the physical % complete earned-value method with a nonzero
  PhysicalPercentComplete;
- p3-cost: a fixed-cost task and a cost-resource task in progress, so
  ActualCost/RemainingCost are nonzero;
- p4-variance: a baseline saved, then one task started earlier and one later
  than planned, and one task's work and one task's duration changed, so the
  Start/Finish/Work variances are nonzero and of both signs;
- p5-split: an in-progress task split after its stop, so Resume moves past the
  split rather than to the next working morning;
- p6-fractional: a sub-cent fixed cost and rate and odd percentages, so costs,
  work, durations and Stop/Resume fall between whole cents, minutes and
  seconds. Project's XML writes costs rounded to two decimals and seconds on
  Stop/Resume.

The Start/Finish/Work variances in p4 are not stored in the .mpp: Project
derives them at export from the baseline (Var2Data keeps the baseline
duration, start and finish), so mppread leaves them unread.

Drives a licensed Microsoft Project over COM. Each case is saved as Project's
native .mpp plus Project's own MSPDI .xml export (the oracle) into
corpus/mpp/progress/, which is git-ignored like every other .mpp/.xml there.
mppread/tests/oracle_corpus.rs compares the decoder against these when present.

Usage (from the repo root, Windows):
    python corpus/tools/gen_mpp_progress_cases.py
Requires: Microsoft Project desktop and pywin32. Close Project gracefully if a
run is interrupted; never force-kill WINPROJ.EXE (it wedges COM activation).
"""

import os
import sys

import win32com.client as win32

ANCHOR = "3/2/2026 8:00 AM"  # Monday; Project rejects ISO date strings
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mpp", "progress")
PJ_MPP, PJ_XML = "MSProject.MPP", "MSProject.XML"
PJ_COST_RESOURCE = 2
PJ_PHYSICAL_PERCENT_COMPLETE = 1


def new_plan(app):
    app.FileNew()  # NO arguments: a False here is read as a filename
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    # Project's own default here is manual; every case but p1's M is auto.
    p.NewTasksCreatedAsManual = False
    return p


def add(p, name, days, outline=None):
    t = p.Tasks.Add(name)
    t.Duration = f"{days}d"
    if outline is not None:
        t.OutlineLevel = outline
    return t


def work_resource(p, name, rate):
    r = p.Resources.Add(name)
    r.StandardRate = rate
    return r


def assign(t, r):
    return t.Assignments.Add(ResourceID=r.ID)


def save(app, slug):
    for ext, fmt in ((".xml", PJ_XML), (".mpp", PJ_MPP)):
        path = os.path.abspath(os.path.join(OUT, slug + ext))
        if os.path.exists(path):
            os.remove(path)
        app.CalculateProject()
        app.FileSaveAs(Name=path, FormatID=fmt)
    app.FileCloseEx(0)
    print("wrote " + slug)


def states(app):
    p = new_plan(app)
    p.Tasks.Add("Phase")
    n = add(p, "N not started", 2, outline=2)
    h = add(p, "H half done", 4, outline=2)
    c = add(p, "C complete", 2, outline=2)
    m = add(p, "M manual", 3, outline=2)
    h.Predecessors = str(c.ID)
    n.Predecessors = str(h.ID)
    c.PercentComplete = 100
    h.PercentComplete = 50
    m.Manual = True
    m.Start = ANCHOR
    m.PercentComplete = 25
    save(app, "p1-states")


def work(app):
    p = new_plan(app)
    r = work_resource(p, "R worker", "50/h")
    w = add(p, "W work entered", 3)
    assign(w, r)
    # Actual work below the duration's share makes PercentWorkComplete differ
    # from PercentComplete.
    w.PercentComplete = 60
    w.ActualWork = "6h"
    e = add(p, "E physical", 4)
    assign(e, r)
    e.EarnedValueMethod = PJ_PHYSICAL_PERCENT_COMPLETE
    e.PercentComplete = 25
    e.PhysicalPercentComplete = 40
    add(p, "U untouched", 1)
    save(app, "p2-work")


def cost(app):
    p = new_plan(app)
    f = add(p, "F fixed cost", 4)
    f.FixedCost = 1000
    f.PercentComplete = 50
    k = p.Resources.Add("K travel")
    k.Type = PJ_COST_RESOURCE
    c = add(p, "C cost resource", 2)
    a = assign(c, k)
    a.Cost = 300
    c.PercentComplete = 50
    save(app, "p3-cost")


def variance(app):
    p = new_plan(app)
    r = work_resource(p, "R worker", "40/h")
    a = add(p, "A first", 2)
    early = add(p, "E started early", 3)
    late = add(p, "L started late", 2)
    grown = add(p, "G work grown", 2)
    shrunk = add(p, "S shortened", 4)
    early.Predecessors = str(a.ID)
    grown.Predecessors = str(late.ID)
    assign(grown, r)
    app.BaselineSave(All=True)
    # E was planned after A (Wednesday); it actually started Monday.
    early.ActualStart = ANCHOR
    early.PercentComplete = 30
    # L was planned for Monday; it actually started Thursday.
    late.ActualStart = "3/5/2026 8:00 AM"
    late.PercentComplete = 50
    grown.Work = "24h"
    shrunk.Duration = "2d"
    save(app, "p4-variance")


def split(app):
    p = new_plan(app)
    s = add(p, "S split", 4)
    add(p, "B beside", 4)
    s.PercentComplete = 25  # stops at Monday 5 PM
    # Nothing on Tuesday and Wednesday: the rest resumes Thursday morning.
    s.Split("3/3/2026 8:00 AM", "3/4/2026 5:00 PM")
    save(app, "p5-split")


def fractional(app):
    p = new_plan(app)
    f = add(p, "F fractional fixed cost", 3)
    f.FixedCost = 10.555
    f.PercentComplete = 33
    r = work_resource(p, "R odd rate", "10.333/h")
    w = add(p, "W fractional work", 1)
    assign(w, r)
    w.PercentComplete = 37
    save(app, "p6-fractional")


def main():
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        for case in (states, work, cost, variance, split, fractional):
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
