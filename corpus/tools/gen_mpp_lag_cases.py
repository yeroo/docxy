#!/usr/bin/env python3
"""Generate the .mpp link-lag cases for issue #104: percentage and elapsed lags.

A link lag in Project is working time (`1FS+2d`), elapsed calendar time
(`1FS+2ed`) or a percentage of the predecessor's duration (`1FS+50%`), and can
be marked estimated (`1FS+1ew?`). Each kind has its own LagFormat, and for a
percentage LinkLag holds the percentage itself rather than tenths of a minute.

Drives a licensed Microsoft Project over COM. Each case is saved as Project's
native .mpp plus Project's own MSPDI .xml export (the oracle) into
corpus/mpp/lag/, which is git-ignored like every other .mpp/.xml there.
mppread/tests/oracle_corpus.rs compares the decoder against these when present,
and the scheduled dates in the .xml are what corpus/mspdi/26-lag-percent-elapsed.xml
embeds. The script also prints each task's lag as Project reports it over COM
and its scheduled dates, so a run shows the oracle without opening the files.

Usage (from the repo root, Windows):
    python corpus/tools/gen_mpp_lag_cases.py [case_function ...]
Requires: Microsoft Project desktop and pywin32. Project must not be running.
Close Project gracefully if a run is interrupted; never force-kill WINPROJ.EXE
(it wedges COM activation).
"""

import os
import sys

import pywintypes
import win32com.client as win32

ANCHOR = "3/2/2026 8:00 AM"  # Monday; Project rejects ISO date strings
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mpp", "lag")
PJ_MPP, PJ_XML = "MSProject.MPP", "MSProject.XML"


def new_plan(app):
    app.FileNew()  # NO arguments: a False here is read as a filename
    p = app.ActiveProject
    p.ProjectStart = ANCHOR
    p.NewTasksCreatedAsManual = False
    return p


def add(p, name, duration, preds=None):
    t = p.Tasks.Add(name)
    t.Duration = duration
    if preds is not None:
        # Project respells what it stores ("1FS+2ed" reads back "1FS+2 edays");
        # report() prints the text, lag and LagType it kept.
        t.Predecessors = preds
        if not t.Predecessors:
            raise RuntimeError(f"{name}: Project refused {preds!r}")
    return t


def report(p):
    for t in p.Tasks:
        if t is None:
            continue
        deps = [(d.From.ID, d.Type, d.Lag, d.LagType)
                for d in t.TaskDependencies if d.To.UniqueID == t.UniqueID]
        print(f"  {t.ID} {t.Name:<4} {t.Predecessors or '-':<12} "
              f"start={t.Start} finish={t.Finish} slack={t.TotalSlack} "
              f"free={t.FreeSlack} deps={deps}")


def save(app, slug):
    report(app.ActiveProject)
    for ext, fmt in ((".xml", PJ_XML), (".mpp", PJ_MPP)):
        path = os.path.abspath(os.path.join(OUT, slug + ext))
        if os.path.exists(path):
            os.remove(path)
        app.CalculateProject()
        app.FileSaveAs(Name=path, FormatID=fmt)
    app.FileCloseEx(0)
    print("wrote " + slug)


def percent_elapsed(app):
    """The issue's plan plus the elapsed variants: A (4d) finishes Thu 5 17:00
    and P (6d) finishes Mon 9 17:00, so elapsed lags cross a weekend both
    forwards and backwards."""
    p = new_plan(app)
    add(p, "A", "4d")                    # 1
    add(p, "B", "1d", "1FS+50%")         # 2  +50% of 4d = +2d
    add(p, "C", "1d", "1FS-25%")         # 3  -25% of 4d = -1d
    add(p, "D", "1d", "1FS+2ed")         # 4  Sat 7 17:00
    add(p, "E", "1d", "1FS-1ed")         # 5  Wed 4 17:00
    add(p, "F", "1d", "1SS+5ed")         # 6  Sat 7 08:00
    add(p, "G", "1d", "1FF+2ed")         # 7  finish >= Sat 7 17:00
    add(p, "H", "1d", "1FS+1ew?")        # 8  estimated elapsed week
    add(p, "P", "6d")                    # 9
    add(p, "Q", "1d", "9FS-2ed")         # 10 Sat 7 17:00, backwards over the weekend
    add(p, "M", "0d", "1FS+2ed")         # 11 a milestone after an elapsed lag
    add(p, "S", "1d", "1SF+2ed")         # 12 finish >= Wed 4 08:00
    add(p, "W", "1d", "1FS+3h")          # 13 a working lag in hours (format 5)
    add(p, "X", "1d", "9SS+150%")        # 14 +150% of 6d = +9d
    save(app, "l1-percent-elapsed")


def elapsed_free_slack(app):
    """A's only successor is 2 elapsed days after it, and the unrelated Z sets
    the project finish, so A's free slack is how far it can slip before D
    must move across the weekend."""
    p = new_plan(app)
    add(p, "A", "4d")                    # 1
    add(p, "D", "1d", "1FS+2ed")         # 2
    add(p, "Z", "10d")                   # 3
    save(app, "l2-elapsed-free-slack")


def main():
    try:
        win32.GetActiveObject("MSProject.Application")
    except pywintypes.com_error:
        pass  # not running: Dispatch below starts a private instance
    else:
        print("Microsoft Project is already running; close it first.")
        return 2
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        cases = (percent_elapsed, elapsed_free_slack)
        only = sys.argv[1:]
        for case in (c for c in cases if not only or c.__name__ in only):
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
