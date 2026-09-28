#!/usr/bin/env python3
"""Create Project-written MPP/XML probes for assignment baseline import (#445).

Requires Microsoft Project desktop and pywin32. Generated files are ignored.
Close Project gracefully if interrupted; never force-kill WINPROJ.EXE.
"""

import os
import sys
import xml.etree.ElementTree as ET

import win32com.client as win32

OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mpp", "assnbaseline"))
ANCHOR = "3/2/2026 8:00 AM"


def add(p, name, days):
    task = p.Tasks.Add(name)
    task.Duration = f"{days}d"
    return task


def save(app, slug):
    app.CalculateProject()
    for ext, fmt in ((".xml", "MSProject.XML"), (".mpp", "MSProject.MPP")):
        path = os.path.join(OUT, slug + ext)
        if os.path.exists(path):
            os.remove(path)
        app.FileSaveAs(Name=path, FormatID=fmt)
    root = ET.parse(os.path.join(OUT, slug + ".xml")).getroot()
    exported = root.findall(".//{*}Assignment")
    if slug == "a0-work":
        assert len(exported) == 4
    if slug == "a1-slots-progress":
        assert len(exported) == 1
        assert [b.findtext("{*}Number") for b in exported[0].findall("{*}Baseline")] == ["0", "1", "10"]
        assert len({b.findtext("{*}Cost") for b in exported[0].findall("{*}Baseline")}) == 3
        assert float(exported[0].findtext("{*}BCWS")) > 0
        assert float(exported[0].findtext("{*}BCWP")) > 0
    if slug == "a2-material-cost-unassigned":
        assert len(exported) == 3
        resources = {r.findtext("{*}Name"): (r.findtext("{*}Type"), r.findtext("{*}IsCostResource"))
                     for r in root.findall(".//{*}Resource")}
        assert resources["Concrete"] == ("0", "0")
        assert resources["Travel"] == ("0", "1")
    for assn in root.findall(".//{*}Assignment"):
        uid = assn.findtext("{*}UID")
        fields = {name: assn.findtext("{*}" + name) for name in
                  ("TaskUID", "ResourceUID", "Units", "Work", "Start", "Finish", "BCWS", "BCWP")}
        baselines = []
        for baseline in assn.findall("{*}Baseline"):
            baselines.append({name: baseline.findtext("{*}" + name) for name in
                              ("Number", "Start", "Finish", "Work", "Cost", "BCWS", "BCWP")})
        assert all(b["BCWS"] is None and b["BCWP"] is None for b in baselines)
        print(slug, "XML assignment", uid, fields, "baselines", baselines, flush=True)
    for task in app.ActiveProject.Tasks:
        if task is None:
            continue
        for assn in task.Assignments:
            print(slug, "COM baseline", assn.UniqueID,
                  assn.BaselineStart, assn.BaselineFinish, assn.BaselineWork, assn.BaselineCost,
                  assn.Baseline1Cost, assn.Baseline10Cost, flush=True)
    app.FileCloseEx(0)


def cases(app):
    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    alice = p.Resources.Add("Alice")
    alice.StandardRate = "35/h"
    bob = p.Resources.Add("Bob")
    bob.StandardRate = "55/h"
    one = add(p, "One", 2)
    two = add(p, "Two", 3)
    three = add(p, "Three", 1)
    for task, resource in ((one, alice), (one, bob), (two, alice), (three, bob)):
        assignment = task.Assignments.Add(ResourceID=resource.ID)
        print("COM assignment", assignment.UniqueID, task.UniqueID, resource.UniqueID,
              assignment.Work, assignment.Cost, flush=True)
    app.BaselineSave(All=True)
    save(app, "a0-work")

    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    r = p.Resources.Add("Worker")
    r.StandardRate = "50/h"
    t = add(p, "Changing", 2)
    t.Assignments.Add(ResourceID=r.ID)
    app.BaselineSave(All=True)
    t.Duration = "3d"
    app.BaselineSave(All=True, Copy=win32.constants.pjCopyCurrent,
                     Into=win32.constants.pjIntoBaseline1)
    t.Duration = "4d"
    app.BaselineSave(All=True, Copy=win32.constants.pjCopyCurrent,
                     Into=win32.constants.pjIntoBaseline10)
    t.PercentComplete = 50
    p.StatusDate = "3/3/2026 5:00 PM"
    save(app, "a1-slots-progress")

    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    material = p.Resources.Add("Concrete")
    material.Type = win32.constants.pjResourceTypeMaterial
    material.StandardRate = "12"
    cost = p.Resources.Add("Travel")
    cost.Type = win32.constants.pjResourceTypeCost
    tm = add(p, "Material", 2)
    tc = add(p, "Cost", 2)
    tm.Assignments.Add(ResourceID=material.ID)
    ca = tc.Assignments.Add(ResourceID=cost.ID)
    ca.Cost = 300
    add(p, "Unassigned", 1)
    app.BaselineSave(All=True)
    save(app, "a2-material-cost-unassigned")


def main():
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        cases(app)
    finally:
        for call in (lambda: app.FileCloseAll(0), lambda: app.Quit(0)):
            try:
                call()
            except Exception:
                pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
