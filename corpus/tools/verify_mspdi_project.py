#!/usr/bin/env python3
"""Check the MSPDI corpus oracles (corpus/mspdi/) against Microsoft Project (#74).

For each fixture this opens a temporary copy in a licensed Microsoft Project
over COM, lets Project schedule it, and compares every task's Start, Finish,
Total Slack and Critical with the oracle embedded in the fixture.

The copy has every task's <Start>, <Finish>, <TotalSlack> and <Critical>
removed, so Project must schedule from the inputs (durations, links, lags,
constraints, calendars) and cannot read the oracle back. Before comparing
dates, it checks that Project imported those inputs as the file states them
(input fidelity), so an import problem is not mistaken for a scheduling fact.

Exit status is 1 on any fidelity failure or schedule mismatch, 2 if
Microsoft Project is already running, 0 otherwise.
The tool never writes into corpus/mspdi/ and never saves in Project.

Usage (from the repo root, Windows):
    python corpus/tools/verify_mspdi_project.py [fixture.xml ...]
With no arguments it checks every corpus/mspdi/*.xml. Requires Microsoft
Project desktop and pywin32; not run in CI. Project must not be running: it is
a single-instance COM server, so the script would attach to your session, hide
it and close its projects without saving. The script refuses to start instead.
Close Project gracefully if a run is interrupted; never force-kill WINPROJ.EXE
(it wedges COM activation).
"""

import glob
import os
import re
import sys
import tempfile
import xml.etree.ElementTree as ET

import pywintypes
import win32com.client as win32

CORPUS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mspdi")
NS = {"p": "http://schemas.microsoft.com/project"}
ORACLE_TAGS = ("Start", "Finish", "TotalSlack", "Critical")
# COM TaskDependency.Type uses the same codes as MSPDI's link <Type>
# (observed on fixtures 02-05 and 14), so no translation table is needed.
LINK = {0: "FF", 1: "FS", 2: "SF", 3: "SS"}


def minutes(iso):
    """MSPDI duration 'PT16H0M0S' -> 960 minutes."""
    m = re.fullmatch(r"PT(\d+)H(\d+)M(\d+)S", iso)
    return int(m[1]) * 60 + int(m[2]) + int(m[3]) // 60


def text(el, tag, default=None):
    child = el.find(f"p:{tag}", NS)
    return default if child is None else child.text


def read_fixture(path):
    """The tasks as the file states them: inputs and embedded oracle."""
    root = ET.parse(path).getroot()
    calendars = {text(c, "UID"): text(c, "Name")
                 for c in root.findall("p:Calendars/p:Calendar", NS)}
    tasks = []
    for t in root.findall("p:Tasks/p:Task", NS):
        cal = text(t, "CalendarUID")
        tasks.append({
            "uid": int(text(t, "UID")),
            "name": text(t, "Name"),
            "summary": text(t, "Summary") == "1",
            # Manually scheduled (#77); an absent <Manual> means auto.
            "manual": text(t, "Manual") == "1",
            "duration": minutes(text(t, "Duration")),
            "ctype": int(text(t, "ConstraintType", "0")),
            "cdate": text(t, "ConstraintDate"),
            "calendar": "None" if cal is None else calendars[cal],
            # (predecessor UID, type code, lag in minutes); LinkLag is tenths.
            "preds": sorted((int(text(l, "PredecessorUID")), int(text(l, "Type")),
                             int(text(l, "LinkLag", "0")) // 10)
                            for l in t.findall("p:PredecessorLink", NS)),
            "start": text(t, "Start"),
            "finish": text(t, "Finish"),
            "slack": int(text(t, "TotalSlack")) // 10,
            "critical": text(t, "Critical") == "1",
        })
    return tasks


def strip_oracle(xml):
    """Remove each task's own oracle elements; baselines keep their dates."""
    def one_task(m):
        parts = re.split(r"(<Baseline>.*?</Baseline>)", m[0], flags=re.S)
        for i in range(0, len(parts), 2):
            for tag in ORACLE_TAGS:
                parts[i] = re.sub(rf"<{tag}>[^<]*</{tag}>", "", parts[i])
        return "".join(parts)
    return re.sub(r"<Task>.*?</Task>", one_task, xml, flags=re.S)


def when(value):
    """COM date -> MSPDI string. pywin32 tags Project's local wall-clock times
    with a timezone; format them as they are, never convert."""
    return value.strftime("%Y-%m-%dT%H:%M:%S") if hasattr(value, "strftime") else None


def pred_text(preds):
    return ",".join(f"{u}{LINK.get(k, k)}{lag:+d}m" for u, k, lag in preds) or "-"


def check(app, path, tmpdir):
    """Print one fixture's comparison; return (fidelity failures, mismatches)."""
    name = os.path.basename(path)
    expected = {t["uid"]: t for t in read_fixture(path)}
    copy = os.path.join(tmpdir, name)
    with open(path, encoding="utf-8") as fh:
        stripped = strip_oracle(fh.read())
    with open(copy, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(stripped)

    # With alerts off a failed open can return False instead of raising, and
    # ActiveProject is then some other project, which must not be touched.
    # An imported XML opens as an unsaved project named after the file stem.
    try:
        before = app.Projects.Count
        opened = (app.FileOpenEx(copy, True) and app.Projects.Count == before + 1
                  and app.ActiveProject.Name == os.path.splitext(name)[0])
    except pywintypes.com_error:
        opened = False
    if not opened:
        print(f"{name}\n  FIDELITY Project did not open the copy")
        return 1, 0
    try:
        project = app.ActiveProject
        tasks = [t for t in project.Tasks if t is not None]
        # Tasks the file marks manual stay manual (#77); every other task is
        # scheduled automatically, whatever Project's import default.
        for t in tasks:
            if t.Manual and not expected.get(t.UniqueID, {}).get("manual"):
                t.Manual = False
        app.CalculateProject()

        fidelity, mismatches = [], []
        print(name)
        if sorted(t.UniqueID for t in tasks) != sorted(expected):
            fidelity.append(f"  FIDELITY task UIDs {sorted(t.UniqueID for t in tasks)}"
                            f" != file {sorted(expected)}")
        for t in tasks:
            exp = expected.get(t.UniqueID)
            if exp is None:
                continue
            label = f"{t.UniqueID} {t.Name}"
            got_preds = sorted((d.From.UniqueID, d.Type, d.Lag)
                               for d in t.TaskDependencies if d.To.UniqueID == t.UniqueID)
            inputs = [
                ("preds", pred_text(exp["preds"]), pred_text(got_preds)),
                ("constraint", exp["ctype"], t.ConstraintType),
                ("constraint date", exp["cdate"], when(t.ConstraintDate)),
                ("calendar", exp["calendar"], t.Calendar),
            ]
            if not exp["summary"]:  # a summary's duration is rolled up, not input
                inputs.insert(0, ("duration", exp["duration"], t.Duration))
            for field, want, got in inputs:
                if want != got:
                    fidelity.append(f"  FIDELITY {label}: {field} file={want} project={got}")

            got = {"start": when(t.Start), "finish": when(t.Finish),
                   "slack": t.TotalSlack, "critical": bool(t.Critical)}
            cells, bad = [], []
            for field in ("start", "finish", "slack", "critical"):
                ok = got[field] == exp[field]
                cells.append(f"{field}={got[field]}" + ("" if ok else f" (oracle {exp[field]})"))
                if not ok:
                    bad.append(f"{label} {field}")
            mismatches += bad
            print(f"  {'MISMATCH' if bad else 'ok      '} {label}: " + "  ".join(cells))
        for line in fidelity:
            print(line)
        return len(fidelity), len(mismatches)
    finally:
        app.FileCloseEx(0)


def main(argv):
    paths = argv or sorted(glob.glob(os.path.join(CORPUS, "*.xml")))
    paths = [p if os.path.exists(p) else os.path.join(CORPUS, p) for p in paths]
    try:
        win32.GetActiveObject("MSProject.Application")
    except pywintypes.com_error:
        pass  # not running: Dispatch below starts a private instance
    else:
        print("Microsoft Project is already running; close it first. The check would "
              "attach to that instance and close its projects without saving.")
        return 2
    app = win32.Dispatch("MSProject.Application")
    fidelity = mismatches = 0
    try:
        app.Visible = False
        app.DisplayAlerts = False
        with tempfile.TemporaryDirectory(prefix="verify_mspdi_") as tmpdir:
            for path in paths:
                f, m = check(app, os.path.abspath(path), tmpdir)
                fidelity += f
                mismatches += m
    finally:
        for call in (lambda: app.FileCloseAll(0), lambda: app.Quit(0)):
            try:
                call()
            except Exception:
                pass
    print(f"{len(paths)} files: {fidelity} fidelity failures, {mismatches} schedule mismatches")
    return 1 if fidelity or mismatches else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
