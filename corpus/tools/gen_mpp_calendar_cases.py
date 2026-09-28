#!/usr/bin/env python3
"""Create five calendar probes as Project-written .mpp/.xml pairs for #195.

Seed MSPDI is opened in Microsoft Project, then Project itself writes both
outputs. The seed lives in a temporary directory; only the five pairs land in
corpus/mpp/calendar/ (git-ignored). Run on Windows with Project and pywin32:

    python corpus/tools/gen_mpp_calendar_cases.py

If interrupted, close Project gracefully; do not force-kill WINPROJ.EXE.
"""

import os
import sys
import tempfile
from xml.etree import ElementTree as ET

import win32com.client as win32


OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "mpp", "calendar"))
PJ_MPP, PJ_XML = "MSProject.MPP", "MSProject.XML"
NS = "http://schemas.microsoft.com/project"
ET.register_namespace("", NS)


def child(parent, tag, value=None):
    elem = ET.SubElement(parent, f"{{{NS}}}{tag}")
    if value is not None:
        elem.text = str(value)
    return elem


def calendar(parent, uid, name, week, base_uid=None):
    cal = child(parent, "Calendar")
    child(cal, "UID", uid)
    child(cal, "Name", name)
    child(cal, "IsBaseCalendar", 0 if base_uid is not None else 1)
    if base_uid is not None:
        child(cal, "BaseCalendarUID", base_uid)
    days = child(cal, "WeekDays")
    for day, times in sorted(week.items()):
        weekday = child(days, "WeekDay")
        child(weekday, "DayType", day)  # MSPDI: Sunday=1 through Saturday=7
        child(weekday, "DayWorking", 1 if times else 0)
        if times:
            slots = child(weekday, "WorkingTimes")
            for start, finish in times:
                slot = child(slots, "WorkingTime")
                child(slot, "FromTime", start)
                child(slot, "ToTime", finish)
    return cal


STANDARD_SHIFT = [("08:00:00", "12:00:00"), ("13:00:00", "17:00:00")]
STANDARD_WEEK = {day: (STANDARD_SHIFT if day in range(2, 7) else [])
                 for day in range(1, 8)}


def seed(path, slug, default_uid, extra_calendars, resource_calendar=None):
    root = ET.Element(f"{{{NS}}}Project")
    child(root, "Name", slug)
    child(root, "MinutesPerDay", 480)
    child(root, "MinutesPerWeek", 2400)
    child(root, "CalendarUID", default_uid)
    child(root, "StartDate", "2026-03-02T08:00:00")
    child(root, "ProjectExternallyEdited", 0)
    child(root, "NewTasksAreManual", 0)
    tasks = child(root, "Tasks")
    task = child(tasks, "Task")
    for tag, value in (("UID", 1), ("ID", 1), ("Name", "Calendar probe"),
                       ("Start", "2026-03-02T08:00:00"),
                       ("Finish", "2026-03-02T17:00:00"),
                       ("Duration", "PT8H0M0S")):
        child(task, tag, value)
    if resource_calendar is not None:
        resources = child(root, "Resources")
        resource = child(resources, "Resource")
        for tag, value in (("UID", 1), ("ID", 1), ("Name", resource_calendar[0]),
                           ("Type", 1), ("MaxUnits", 1),
                           ("CalendarUID", resource_calendar[1])):
            child(resource, tag, value)
    calendars = child(root, "Calendars")
    calendar(calendars, 1, "Standard", STANDARD_WEEK)
    for uid, name, week, base_uid in extra_calendars:
        calendar(calendars, uid, name, week, base_uid)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def cases():
    night = {day: ([("18:00:00", "22:00:00")] if day in range(2, 7) else [])
             for day in range(1, 8)}
    edited = {day: list(times) for day, times in STANDARD_WEEK.items()}
    edited[4] = [("09:00:00", "12:00:00"), ("13:00:00", "18:00:00")]
    five = {day: list(times) for day, times in STANDARD_WEEK.items()}
    five[3] = [(f"{hour:02d}:00:00", f"{hour + 1:02d}:00:00")
               for hour in (6, 8, 10, 12, 14)]
    return [
        ("c1-default-night", 5, [(5, "Night", night, None)], None),
        ("c2-edited-base", 1, [(5, "Edited Standard", edited, None)], None),
        ("c3-resource-override", 1,
         [(5, "Alice", {3: [("07:00:00", "12:00:00")], 6: []}, 1)],
         ("Alice", 5)),
        ("c4-unicode-five-periods", 1, [(5, "Équipe 日本語", five, None)], None),
        ("c5-derived-24-hour", 1,
         [(5, "Night worker", {4: [("00:00:00", "00:00:00")]}, 1)],
         ("Night worker", 5)),
    ]


def main():
    os.makedirs(OUT, exist_ok=True)
    app = win32.Dispatch("MSProject.Application")
    try:
        app.Visible = False
        app.DisplayAlerts = False
        with tempfile.TemporaryDirectory(prefix="mpp-calendars-") as temp:
            for slug, default_uid, extra, resource in cases():
                source = os.path.join(temp, slug + ".xml")
                seed(source, slug, default_uid, extra, resource)
                before = app.Projects.Count
                if not app.FileOpenEx(source, True) or app.Projects.Count != before + 1:
                    raise RuntimeError(f"Project did not open {slug} seed")
                try:
                    for ext, fmt in ((".xml", PJ_XML), (".mpp", PJ_MPP)):
                        target = os.path.join(OUT, slug + ext)
                        if os.path.exists(target):
                            os.remove(target)
                        app.CalculateProject()
                        app.FileSaveAs(Name=target, FormatID=fmt)
                        if not os.path.isfile(target):
                            raise RuntimeError(f"Project did not write {target}")
                finally:
                    app.FileCloseEx(0)
                print("wrote " + slug)
    finally:
        for call in (lambda: app.FileCloseAll(0), lambda: app.Quit(0)):
            try:
                call()
            except Exception:
                pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
