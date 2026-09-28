#!/usr/bin/env python3
"""Generate Project-written MPP/XML work-week pairs for #440.

Requires Microsoft Project and pywin32. The temporary MSPDI seed is never the
oracle: Project's XML export of the saved plan is. Run with no open Project
instance so the script only closes projects it opened itself.
"""

import os
import sys
from datetime import datetime, timezone
from xml.etree import ElementTree as ET

import pywintypes
import win32com.client as win32


HERE = os.path.dirname(__file__)
OUT = os.path.abspath(os.path.join(HERE, "..", "mpp", "workweeks"))
SOURCE = os.path.abspath(os.path.join(HERE, "..", "mspdi", "28-work-weeks.xml"))
NS = "http://schemas.microsoft.com/project"
ET.register_namespace("", NS)
Q = lambda tag: f"{{{NS}}}{tag}"


def date(day):
    return datetime(2026, 3, day, tzinfo=timezone.utc)


def child(parent, tag, text=None):
    node = ET.SubElement(parent, Q(tag))
    if text is not None:
        node.text = str(text)
    return node


def week(parent, name, first, last, days):
    item = child(parent, "WorkWeek")
    period = child(item, "TimePeriod")
    child(period, "FromDate", f"2026-03-{first:02}T00:00:00")
    child(period, "ToDate", f"2026-03-{last:02}T23:59:00")
    if name is not None:
        child(item, "Name", name)
    weekdays = child(item, "WeekDays")
    for day_type, times in days.items():
        day = child(weekdays, "WeekDay")
        child(day, "DayType", day_type)
        child(day, "DayWorking", int(bool(times)))
        if times:
            working = child(day, "WorkingTimes")
            for start, finish in times:
                slot = child(working, "WorkingTime")
                child(slot, "FromTime", start)
                child(slot, "ToTime", finish)


SUMMER = [("08:00:00", "12:00:00"), ("13:00:00", "16:00:00")]
FIVE = [("00:00:00", "01:00:00"), ("02:00:00", "03:00:00"),
        ("04:00:00", "05:00:00"), ("06:00:00", "07:00:00"),
        ("08:00:00", "09:00:00")]


def seed(path, slug, definitions, task_calendar=None, exception=False):
    root = ET.parse(SOURCE).getroot()
    root.find(Q("Name")).text = slug
    tasks = root.find(Q("Tasks"))
    for old in list(tasks):
        tasks.remove(old)
    task = child(tasks, "Task")
    for tag, value in (("UID", 1), ("ID", 1), ("Name", "Work week span"),
                       ("OutlineLevel", 1), ("Summary", 0), ("Milestone", 0),
                       ("Duration", "PT16H0M0S"), ("DurationFormat", 7),
                       ("Start", "2026-03-06T08:00:00"),
                       ("Finish", "2026-03-10T09:00:00"),
                       ("ConstraintType", 4),
                       ("ConstraintDate", "2026-03-06T08:00:00")):
        child(task, tag, value)
    if task_calendar:
        child(task, "CalendarUID", task_calendar)
    calendars = root.find(Q("Calendars"))
    if slug == "w12-task-calendar":
        task_cal = child(calendars, "Calendar")
        child(task_cal, "UID", 5)
        child(task_cal, "Name", "Task Summer")
        child(task_cal, "IsBaseCalendar", 1)
    for calendar in list(calendars):
        uid = int(calendar.find(Q("UID")).text)
        old = calendar.find(Q("WorkWeeks"))
        if old is not None:
            calendar.remove(old)
        old = calendar.find(Q("Exceptions"))
        if old is not None and not exception:
            calendar.remove(old)
        if not exception:
            weekdays = calendar.find(Q("WeekDays"))
            if weekdays is not None:
                for day in list(weekdays):
                    if day.findtext(Q("DayType")) == "0":
                        weekdays.remove(day)
        if uid in definitions:
            weeks = child(calendar, "WorkWeeks")
            for spec in definitions[uid]:
                week(weeks, *spec)
    ET.ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)


def cases():
    summer = {day: SUMMER for day in range(2, 7)}
    return [
        ("w1-summer", {1: [("Summer", 9, 20, summer)]}, None, False),
        ("w2-partial", {1: [("Partial", 9, 20, {2: SUMMER})]}, None, False),
        ("w3-weekend", {1: [("Weekend", 9, 20, {6: [], 7: SUMMER})]}, None, False),
        ("w4-five-periods", {1: [("Five", 9, 20, {2: FIVE})]}, None, False),
        ("w5-daylong", {1: [("Daylong", 9, 20, {2: [("00:00:00", "00:00:00")]})]}, None, False),
        ("w6-two", {1: [("Earlier", 9, 13, {2: SUMMER}),
                         ("Later", 16, 20, {2: FIVE})]}, None, False),
        ("w7-derived", {3: [("Crew", 9, 20, {2: SUMMER})]}, 3, False),
        ("w8-inherited", {1: [("Inherited", 9, 20, {3: SUMMER})]}, 2, False),
        ("w9-unicode", {1: [("Fête 日本語", 9, 20, {2: SUMMER})]}, None, False),
        ("w10-unnamed", {1: [(None, 9, 20, {2: SUMMER})]}, None, False),
        ("w11-exception", {1: [("With holiday", 9, 20, {2: SUMMER})]}, None, True),
        ("w12-task-calendar", {5: [("Task summer", 9, 20, summer)]}, 5, False),
    ]


def save_pair(app, slug):
    for ext, fmt in ((".xml", "MSProject.XML"), (".mpp", "MSProject.MPP")):
        target = os.path.join(OUT, slug + ext)
        if os.path.exists(target):
            os.remove(target)
        app.CalculateProject()
        app.FileSaveAs(Name=target, FormatID=fmt)
        if not os.path.isfile(target):
            raise RuntimeError(f"Project did not write {target}")
    print("wrote " + slug, flush=True)


def com_case(app, slug, definitions):
    app.FileNew()
    try:
        project = app.ActiveProject
        project.ProjectStart = date(6)
        project.NewTasksCreatedAsManual = False
        task = project.Tasks.Add("COM work week span")
        task.Duration = 960
        calendar = project.BaseCalendars("Standard")
        for name, first, last, days in definitions:
            item = calendar.WorkWeeks.Add(Name=name, Start=date(first), Finish=date(last))
            for day_type, times in days.items():
                day = item.WeekDays(day_type)
                day.Working = bool(times)
                for idx, (start, finish) in enumerate(times, 1):
                    shift = getattr(day, f"Shift{idx}")
                    shift.Start = start[:5]
                    shift.Finish = finish[:5]
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
        for slug, definitions, task_calendar, exception in cases():
            source = os.path.join(OUT, "seed-" + slug + ".xml")
            seed(source, slug, definitions, task_calendar, exception)
            try:
                before = app.Projects.Count
                if not app.FileOpenEx(source, True) or app.Projects.Count != before + 1:
                    raise RuntimeError(f"Project did not open {slug} seed")
                try:
                    save_pair(app, slug)
                finally:
                    app.FileCloseEx(0)
            finally:
                os.remove(source)
        com_case(app, "m1-com-summer", [("COM Summer", 9, 20,
                 {day: SUMMER for day in range(2, 7)})])
        com_case(app, "m2-com-out-of-order", [
            ("Later", 16, 20, {2: FIVE}),
            ("Earlier", 9, 13, {2: SUMMER, 6: [], 7: SUMMER}),
        ])
    finally:
        if app.Projects.Count:
            print("Project still has open projects; leaving the instance running.", flush=True)
        else:
            app.Quit(0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
