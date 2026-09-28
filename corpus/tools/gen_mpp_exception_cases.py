#!/usr/bin/env python3
"""Generate Project-written calendar-exception .mpp/.xml oracle pairs for #431.

Run on Windows with Microsoft Project and pywin32. Seed XML is temporary;
the saved XML, not the seed, is the oracle. Close Project gracefully if a run
is interrupted; never force-kill WINPROJ.EXE.
"""

import os
import sys
import tempfile
from datetime import datetime, timezone
from xml.etree import ElementTree as ET

import pywintypes
import win32com.client as win32


OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mpp", "exceptions"))
NS = "http://schemas.microsoft.com/project"
PJ_MPP, PJ_XML = "MSProject.MPP", "MSProject.XML"
ET.register_namespace("", NS)


def project_date(year, month, day, hour=0):
    # pywin32 converts datetimes to VT_DATE through UTC. Tag the intended
    # Project wall-clock components as UTC to keep them unchanged in COM.
    return datetime(year, month, day, hour, tzinfo=timezone.utc)


def child(parent, tag, value=None):
    node = ET.SubElement(parent, f"{{{NS}}}{tag}")
    if value is not None:
        node.text = str(value)
    return node


def exception(parent, spec):
    item = child(parent, "Exception")
    child(item, "EnteredByOccurrences", spec.get("entered", 0))
    dates = child(item, "TimePeriod")
    child(dates, "FromDate", spec["from"] + "T00:00:00")
    child(dates, "ToDate", spec["to"] + "T23:59:00")
    child(item, "Occurrences", spec.get("occurrences", 1))
    if spec["name"] is not None:
        child(item, "Name", spec["name"])
    child(item, "Type", spec.get("type", 1))
    for field, key in (("Period", "period"), ("DaysOfWeek", "days"),
                       ("MonthItem", "month_item"), ("MonthPosition", "position"),
                       ("Month", "month"), ("MonthDay", "month_day")):
        if key in spec:
            child(item, field, spec[key])
    periods = spec.get("times", [])
    child(item, "DayWorking", int(bool(periods)))
    if periods:
        slots = child(item, "WorkingTimes")
        for start, finish in periods:
            slot = child(slots, "WorkingTime")
            child(slot, "FromTime", start)
            child(slot, "ToTime", finish)


def calendar(parent, uid, name, specs, base_uid=None):
    cal = child(parent, "Calendar")
    child(cal, "UID", uid)
    child(cal, "Name", name)
    child(cal, "IsBaseCalendar", int(base_uid is None))
    if base_uid is not None:
        child(cal, "BaseCalendarUID", base_uid)
    if specs:
        items = child(cal, "Exceptions")
        for spec in specs:
            exception(items, spec)
    return cal


def seed(path, slug, specs, derived=False, weekend=False):
    root = ET.Element(f"{{{NS}}}Project")
    for tag, value in (("Name", slug), ("MinutesPerDay", 480),
                       ("MinutesPerWeek", 2400), ("CalendarUID", 1),
                       ("StartDate", "2026-03-02T08:00:00"),
                       ("ProjectExternallyEdited", 0), ("NewTasksAreManual", 0)):
        child(root, tag, value)
    tasks = child(root, "Tasks")
    task = child(tasks, "Task")
    for tag, value in (("UID", 1), ("ID", 1), ("Name", "Exception span"),
                       ("Start", "2026-03-06T08:00:00" if weekend else "2026-03-02T08:00:00"),
                       ("Duration", "PT16H0M0S" if weekend else "PT80H0M0S")):
        child(task, tag, value)
    calendars = child(root, "Calendars")
    calendar(calendars, 1, "Standard", [] if derived else specs)
    if derived:
        calendar(calendars, 5, "Alice", specs, 1)
        resources = child(root, "Resources")
        resource = child(resources, "Resource")
        for tag, value in (("UID", 1), ("ID", 1), ("Name", "Alice"),
                           ("Type", 1), ("MaxUnits", 1), ("CalendarUID", 5)):
            child(resource, tag, value)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def cases():
    def ex(name, first="2026-03-04", last=None, **fields):
        return dict(name=name, **{"from": first, "to": last or first}, **fields)

    return [
        ("e1-range", [ex("Range", last="2026-03-05")], False, False),
        ("e2-weekend-working", [ex("Weekend shift", "2026-03-07",
                                    times=[("07:00:00", "10:00:00"),
                                           ("11:00:00", "15:00:00")])], False, True),
        ("e3-daily-n", [ex("Every third day", last="2026-03-18",
                            type=7, period=3, entered=1, occurrences=5)], False, False),
        ("e4-weekly-days", [ex("Mon and Thu", last="2026-04-30",
                               type=6, period=1, days=18)], False, False),
        ("e5-monthly-day", [ex("Monthly fourth", last="2026-09-04",
                               type=4, period=1, month_day=4)], False, False),
        ("e6-monthly-position", [ex("Second Tuesday", last="2026-09-08",
                                    type=5, period=1, month_item=5, position=1)], False, False),
        ("e7-yearly-date", [ex("Annual date", last="2030-03-04",
                              type=2, period=1, month=2, month_day=4)], False, False),
        ("e8-yearly-position", [ex("Annual second Tuesday", last="2030-03-12",
                                  type=3, period=1, month=2, month_item=5, position=1)], False, False),
        ("e9-derived", [ex("Alice vacation", first="2026-03-04")], True, False),
        ("e10-several-unicode", [ex("Fête 日本語", first="2026-03-05"),
                                  ex("Earlier", first="2026-03-03"),
                                  ex("Later", first="2026-03-09")], False, False),
        ("e11-unnamed", [ex(None)], False, False),
    ]


def save_pair(app, slug):
    for ext, fmt in ((".xml", PJ_XML), (".mpp", PJ_MPP)):
        target = os.path.join(OUT, slug + ext)
        if os.path.exists(target):
            os.remove(target)
        app.CalculateProject()
        app.FileSaveAs(Name=target, FormatID=fmt)
        if not os.path.isfile(target):
            raise RuntimeError(f"Project did not write {target}")
    print("wrote " + slug, flush=True)


def com_cases():
    # These are created through Project's object model, not by opening MSPDI.
    # Type numbers and pattern fields match Project's PjExceptionType values.
    return [
        ("k1-one-off", 1, project_date(2026, 3, 4), project_date(2026, 3, 4), {}),
        ("k2-daily-n", 7, project_date(2026, 3, 4), None,
         {"Period": 3, "Occurrences": 5}),
        ("k3-weekly-days", 6, project_date(2026, 3, 4), project_date(2026, 4, 30),
         {"Period": 1, "DaysOfWeek": 18}),
        ("k4-monthly-day", 4, project_date(2026, 3, 4), project_date(2026, 9, 4),
         {"Period": 1, "MonthDay": 4}),
        ("k5-monthly-position", 5, project_date(2026, 3, 4), project_date(2026, 9, 8),
         {"Period": 1, "MonthPosition": 1, "MonthItem": 5}),
        ("k6-yearly-date", 2, project_date(2026, 3, 4), project_date(2030, 3, 4),
         {"Month": 3, "MonthDay": 4}),
        ("k7-yearly-position", 3, project_date(2026, 3, 4), project_date(2030, 3, 12),
         {"Month": 3, "MonthPosition": 1, "MonthItem": 5}),
        ("k8-period-300", 7, project_date(2026, 3, 4), project_date(2027, 8, 20),
         {"Period": 300}),
        ("k9-unnamed", 1, project_date(2026, 3, 4), project_date(2026, 3, 4), {}),
    ]


def com_case(app, slug, kind, start, finish, fields):
    app.FileNew()
    project = app.ActiveProject
    try:
        project.ProjectStart = project_date(2026, 3, 2, 8)
        project.NewTasksCreatedAsManual = False
        task = project.Tasks.Add("Exception span")
        task.Duration = 4800
        calendar = project.BaseCalendars("Standard")
        kwargs = {"Type": kind, "Start": start, **fields}
        if slug != "k9-unnamed":
            kwargs["Name"] = slug
        if finish is not None:
            kwargs["Finish"] = finish
        calendar.Exceptions.Add(**kwargs)
        save_pair(app, slug)
    finally:
        app.FileCloseEx(0)


def main():
    try:
        win32.GetActiveObject("MSProject.Application")
    except pywintypes.com_error:
        pass
    else:
        print("Microsoft Project is already running; close it first.")
        return 2
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        with tempfile.TemporaryDirectory(prefix="seed-", dir=OUT) as temp:
            for slug, specs, derived, weekend in cases():
                source = os.path.join(temp, slug + ".xml")
                seed(source, slug, specs, derived, weekend)
                before = app.Projects.Count
                if not app.FileOpenEx(source, True) or app.Projects.Count != before + 1:
                    raise RuntimeError(f"Project did not open {slug} seed")
                try:
                    save_pair(app, slug)
                finally:
                    app.FileCloseEx(0)
        for slug, kind, start, finish, fields in com_cases():
            try:
                com_case(app, slug, kind, start, finish, fields)
            except pywintypes.com_error as error:
                if slug != "k8-period-300":
                    raise
                for ext in (".xml", ".mpp"):
                    target = os.path.join(OUT, slug + ext)
                    if os.path.exists(target):
                        os.remove(target)
                print(f"Project refused optional {slug}: {error}", flush=True)
    finally:
        if app.Projects.Count:
            print("Project still has open projects; leaving the instance running.", flush=True)
        else:
            app.Quit(0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
