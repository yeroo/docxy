#!/usr/bin/env python3
"""Create Project-written MPP/XML probes for assignment baseline import (#445).

Requires Microsoft Project desktop and pywin32. Generated files are ignored.
Close Project gracefully if interrupted; never force-kill WINPROJ.EXE.
"""

import os
import re
import sys
import xml.etree.ElementTree as ET
from datetime import datetime, timedelta

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
        assert [b.findtext("{*}Number") for b in exported[0].findall("{*}Baseline")] == [str(n) for n in range(11)]
        for field in ("Start", "Finish", "Work", "Cost"):
            assert len({b.findtext("{*}" + field) for b in exported[0].findall("{*}Baseline")}) == 11
        assert float(exported[0].findtext("{*}BCWS")) > 0
        assert float(exported[0].findtext("{*}BCWP")) > 0
    if slug == "a2-material-cost-unassigned":
        assert len(exported) == 3
        resources = {r.findtext("{*}Name"): (r.findtext("{*}Type"), r.findtext("{*}IsCostResource"))
                     for r in root.findall(".//{*}Resource")}
        assert resources["Concrete"] == ("0", "0")
        assert resources["Travel"] == ("0", "1")
    if slug == "a3-deleted-rows":
        assert len(exported) == 1
        assert exported[0].findtext("{*}ResourceUID") == "1"
        unnamed = [r for r in root.findall(".//{*}Resource") if r.findtext("{*}UID") == "3"]
        assert len(unnamed) == 1 and unnamed[0].findtext("{*}Name") is None
        assert unnamed[0].findtext("{*}IsNull") == "0"
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
    by_uid = {int(a.findtext("{*}UID")): a for a in exported}
    resource_types = {r.findtext("{*}UID"): (r.findtext("{*}Type"),r.findtext("{*}IsCostResource"))
                      for r in root.findall(".//{*}Resource")}
    seen = set()
    for task in app.ActiveProject.Tasks:
        if task is None:
            continue
        for assn in task.Assignments:
            uid = int(assn.UniqueID) & 0xfffff  # Project's COM UID carries a high type tag.
            assert uid in by_uid, (slug, uid)
            seen.add(uid)
            for baseline in by_uid[uid].findall("{*}Baseline"):
                slot = int(baseline.findtext("{*}Number"))
                prefix = "Baseline" + (str(slot) if slot else "")
                for field in ("Start", "Finish"):
                    value = baseline.findtext("{*}" + field)
                    if value is not None:
                        assert str(getattr(assn, prefix + field))[:19].replace(" ", "T") == value, (slug, uid, slot, field)
                value = baseline.findtext("{*}Work")
                if value is not None:
                    hours, mins, secs = map(int, re.fullmatch(r"PT(\d+)H(\d+)M(\d+)S", value).groups())
                    scale = 60 if resource_types.get(by_uid[uid].findtext("{*}ResourceUID")) == ("0", "0") else 1
                    assert abs(float(getattr(assn, prefix + "Work")) * scale - (hours * 60 + mins + secs / 60)) < 0.02
                value = baseline.findtext("{*}Cost")
                if value is not None:
                    assert abs(float(getattr(assn, prefix + "Cost")) * 100 - float(value)) < 0.02
            print(slug, "COM baseline", assn.UniqueID,
                  assn.BaselineStart, assn.BaselineFinish, assn.BaselineWork, assn.BaselineCost,
                  assn.Baseline1Cost, assn.Baseline10Cost, flush=True)
    assert seen == {uid for uid, assn in by_uid.items() if assn.findtext("{*}ResourceUID") != "-65535"}
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
    for slot in range(1, 11):
        t.Start = (datetime(2026, 3, 2, 8) + timedelta(weeks=slot)).strftime("%m/%d/%Y %I:%M %p")
        t.Duration = f"{slot + 2}d"
        app.BaselineSave(All=True, Copy=win32.constants.pjCopyCurrent,
                         Into=win32.constants.pjIntoBaseline1 + slot - 1)
    t.PercentComplete = 50
    p.StatusDate = "5/18/2026 5:00 PM"
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

    app.FileNew()
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    keeper = p.Resources.Add("Keeper")
    removed = p.Resources.Add("Removed")
    task = add(p, "Keeps an assignment", 2)
    old = task.Assignments.Add(ResourceID=keeper.ID)
    old.Delete()
    task.Assignments.Add(ResourceID=keeper.ID)
    removed.Delete()
    # COM creates a live unnamed resource (IsNull=0), not a null sheet row.
    p.Resources.Add("")
    app.BaselineSave(All=True)
    save(app, "a3-deleted-rows")


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
