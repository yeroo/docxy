#!/usr/bin/env python3
"""Generate the MSPDI (MS Project XML) seed corpus (corpus/mspdi/).

Unlike the xlsx corpus, there is no free high-fidelity oracle for project
scheduling (MS Project is the reference implementation and isn't scriptable
here). So each file is made *self-oracling*: it embeds the Start/Finish that
Project itself would compute, hand-verified against a standard 8h/day Mon-Fri
calendar anchored at Monday 2026-03-02 08:00. `projcore/tests/corpus.rs` reads
each file, runs the CPM scheduler, and asserts the computed dates match the
embedded ones — so the corpus validates the scheduler without needing Project.

Every file isolates exactly ONE feature (one link type, one constraint, one
rollup rule) so a failing assertion points at a single code path, mirroring
gen_xlsx_corpus.py.

Usage (from the repo root):
    python3 corpus/tools/gen_mspdi_corpus.py
Requires: python3 only (pure stdlib).
"""

import json
import os

OUT_DIR = os.path.join("corpus", "mspdi")

# Link type codes (MSPDI's own, non-intuitive numbering).
FF, FS, SF, SS = 0, 1, 2, 3
# Constraint type codes.
ASAP, ALAP, MSO, MFO, SNET, SNLT, FNET, FNLT = range(8)


def iso(minutes):
    """Working minutes -> ISO-8601 duration string (PT..H..M..S)."""
    h, m = divmod(minutes, 60)
    return f"PT{h}H{m}M0S"


def task(uid, name, dur_min, start, finish, *, oid=None, outline=1,
         summary=False, milestone=False, preds=(), ctype=None, cdate=None,
         calendar=None):
    """One <Task>. `preds` is a list of (uid, type_code, lag_tenths_of_min).
    `start`/`finish` are the embedded oracle values (MSPDI datetime strings)."""
    oid = uid if oid is None else oid
    lines = [
        "    <Task>",
        f"      <UID>{uid}</UID><ID>{oid}</ID>",
        f"      <Name>{name}</Name>",
        f"      <OutlineLevel>{outline}</OutlineLevel>",
        f"      <Summary>{1 if summary else 0}</Summary>",
        f"      <Milestone>{1 if milestone else 0}</Milestone>",
        f"      <Duration>{iso(dur_min)}</Duration><DurationFormat>7</DurationFormat>",
        f"      <Start>{start}</Start><Finish>{finish}</Finish>",
    ]
    if ctype is not None:
        lines.append(f"      <ConstraintType>{ctype}</ConstraintType>")
        if cdate is not None:
            lines.append(f"      <ConstraintDate>{cdate}</ConstraintDate>")
    if calendar is not None:
        lines.append(f"      <CalendarUID>{calendar}</CalendarUID>")
    for (puid, ptype, lag) in preds:
        lines += [
            "      <PredecessorLink>",
            f"        <PredecessorUID>{puid}</PredecessorUID>",
            f"        <Type>{ptype}</Type>",
            f"        <LinkLag>{lag}</LinkLag><LagFormat>7</LagFormat>",
            "      </PredecessorLink>",
        ]
    lines.append("    </Task>")
    return "\n".join(lines)


def weekday(day_type, working, times):
    """One <WeekDay>. day_type: 1=Sun..7=Sat. times: list of (from, to) 'HH:MM:SS'."""
    out = [f"    <WeekDay><DayType>{day_type}</DayType>"
           f"<DayWorking>{1 if working else 0}</DayWorking>"]
    if working and times:
        out.append("      <WorkingTimes>")
        for (f, t) in times:
            out.append(f"        <WorkingTime><FromTime>{f}</FromTime>"
                       f"<ToTime>{t}</ToTime></WorkingTime>")
        out.append("      </WorkingTimes>")
    out.append("    </WeekDay>")
    return "\n".join(out)


SHIFT = [("08:00:00", "12:00:00"), ("13:00:00", "17:00:00")]


def standard_calendar(uid=1, name="Standard", saturday=False):
    days = []
    # DayType 1=Sunday .. 7=Saturday.
    for dt in range(1, 8):
        if dt == 1:  # Sunday
            days.append(weekday(dt, False, []))
        elif dt == 7:  # Saturday
            days.append(weekday(dt, saturday, SHIFT if saturday else []))
        else:  # Mon..Fri
            days.append(weekday(dt, True, SHIFT))
    return (f"  <Calendar>\n    <UID>{uid}</UID><Name>{name}</Name>"
            f"<IsBaseCalendar>1</IsBaseCalendar>\n"
            f"    <WeekDays>\n" + "\n".join(days) + "\n    </WeekDays>\n  </Calendar>")


def project(name, tasks_xml, *, resources_xml="", assignments_xml="",
            calendars=None):
    calendars = calendars or [standard_calendar()]
    parts = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<Project xmlns="http://schemas.microsoft.com/project">',
        f"  <Name>{name}</Name>",
        "  <MinutesPerDay>480</MinutesPerDay>",
        "  <MinutesPerWeek>2400</MinutesPerWeek>",
        "  <CalendarUID>1</CalendarUID>",
        f"  <StartDate>2026-03-02T08:00:00</StartDate>",
        "  <Tasks>",
        tasks_xml,
        "  </Tasks>",
    ]
    if resources_xml:
        parts += ["  <Resources>", resources_xml, "  </Resources>"]
    if assignments_xml:
        parts += ["  <Assignments>", assignments_xml, "  </Assignments>"]
    parts += ["  <Calendars>"] + calendars + ["  </Calendars>", "</Project>"]
    return "\n".join(parts) + "\n"


D = 480  # one working day in minutes

# Anchor Mon 2026-03-02 08:00. Working days: Mon2 Tue3 Wed4 Thu5 Fri6 (Sat7/Sun8
# off) Mon9 ... Each task day runs 08:00-17:00.
def dt(day, hm="08:00:00"):
    return f"2026-03-{day:02d}T{hm}"


CORPUS = []


def add(fname, tags, desc, xml):
    CORPUS.append({"file": fname, "category": tags[0], "tags": tags, "desc": desc})
    with open(os.path.join(OUT_DIR, fname), "w") as fh:
        fh.write(xml)


def build():
    os.makedirs(OUT_DIR, exist_ok=True)

    # 01 — a single 2-day task.
    add("01-single-task.xml", ["basic"], "One 2-day task; the irreducible minimum.",
        project("single-task",
                task(1, "Dig foundation", 2 * D, dt(2), dt(3, "17:00:00"))))

    # 02 — finish-to-start dependency.
    add("02-link-fs.xml", ["link", "link-fs"], "Finish-to-start dependency.",
        project("link-fs", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00")),
            task(2, "B", 2 * D, dt(4), dt(5, "17:00:00"), preds=[(1, FS, 0)]),
        ])))

    # 03 — start-to-start dependency.
    add("03-link-ss.xml", ["link", "link-ss"], "Start-to-start dependency.",
        project("link-ss", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00")),
            task(2, "B", 3 * D, dt(2), dt(4, "17:00:00"), preds=[(1, SS, 0)]),
        ])))

    # 04 — finish-to-finish dependency.
    add("04-link-ff.xml", ["link", "link-ff"], "Finish-to-finish dependency.",
        project("link-ff", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00")),
            task(2, "B", 1 * D, dt(3), dt(3, "17:00:00"), preds=[(1, FF, 0)]),
        ])))

    # 05 — start-to-finish dependency (predecessor pinned by SNET so the
    # successor's finish lands on a real working boundary, not the anchor).
    add("05-link-sf.xml", ["link", "link-sf"], "Start-to-finish dependency.",
        project("link-sf", "\n".join([
            task(1, "A", 2 * D, dt(4), dt(5, "17:00:00"), ctype=SNET, cdate=dt(4)),
            task(2, "B", 1 * D, dt(3), dt(3, "17:00:00"), preds=[(1, SF, 0)]),
        ])))

    # 06 — FS with +2 day lag (LinkLag is tenths of a minute: 2d = 2*480*10).
    add("06-lag.xml", ["link", "lag"], "FS link with +2 working-day lag.",
        project("lag", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00")),
            task(2, "B", 1 * D, dt(6), dt(6, "17:00:00"),
                 preds=[(1, FS, 2 * D * 10)]),
        ])))

    # 07 — FS with -1 day lead (negative lag: overlap).
    add("07-lead.xml", ["link", "lead"], "FS link with -1 working-day lead.",
        project("lead", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00")),
            task(2, "B", 1 * D, dt(3), dt(3, "17:00:00"),
                 preds=[(1, FS, -1 * D * 10)]),
        ])))

    # 08 — a milestone (zero duration) with a Start-No-Earlier-Than constraint.
    add("08-milestone.xml", ["milestone", "constraint", "constraint-snet"],
        "Zero-duration milestone with SNET constraint.",
        project("milestone",
                task(1, "Permit approved", 0, dt(5), dt(5), milestone=True,
                     ctype=SNET, cdate=dt(5))))

    # 09 — a Start-No-Earlier-Than constraint on a normal task.
    add("09-constraint-snet.xml", ["constraint", "constraint-snet"],
        "Start-No-Earlier-Than constraint delays the start.",
        project("constraint-snet",
                task(1, "Delayed", 2 * D, dt(5), dt(6, "17:00:00"),
                     ctype=SNET, cdate=dt(5))))

    # 10 — a summary task with two children (outline rollup).
    add("10-summary.xml", ["summary"], "Summary task rolling up two children.",
        project("summary", "\n".join([
            task(1, "Phase", 2 * D, dt(2), dt(3, "17:00:00"), summary=True, outline=1),
            task(2, "A", 1 * D, dt(2), dt(2, "17:00:00"), oid=2, outline=2),
            task(3, "B", 1 * D, dt(3), dt(3, "17:00:00"), oid=3, outline=2,
                 preds=[(2, FS, 0)]),
        ])))

    # 11 — a resource assigned to a task (units x work).
    res = ("    <Resource><UID>1</UID><ID>1</ID><Name>Alice</Name>"
           "<Type>1</Type><MaxUnits>1</MaxUnits></Resource>")
    asn = ("    <Assignment><UID>1</UID><TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>"
           "<Units>1</Units><Work>PT16H0M0S</Work></Assignment>")
    add("11-resource-assignment.xml", ["resource", "assignment"],
        "One work resource assigned to a task at 100% units.",
        project("resource-assignment",
                task(1, "Build", 2 * D, dt(2), dt(3, "17:00:00")),
                resources_xml=res, assignments_xml=asn))

    # 12 — a custom 6-day calendar (Saturday working) changes the finish date.
    # A 6-day task on the Standard calendar would finish Mon Mar 9; with
    # Saturday working it finishes Sat Mar 7.
    add("12-calendar-6day.xml", ["calendar", "calendar-6day"],
        "Custom 6-day calendar (Saturday working) shortens the schedule.",
        project("calendar-6day",
                task(1, "Six days", 6 * D, dt(2), dt(7, "17:00:00"), calendar=2),
                calendars=[standard_calendar(1),
                           standard_calendar(2, "SixDay", saturday=True)]))

    manifest = {
        "anchor": "2026-03-02T08:00:00",
        "calendar": "Standard 8h/day Mon-Fri (08:00-12:00, 13:00-17:00)",
        "note": "Start/Finish embedded in each file are the CPM oracle.",
        "files": CORPUS,
    }
    with open(os.path.join(OUT_DIR, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")

    print(f"wrote {len(CORPUS)} MSPDI files + manifest.json to {OUT_DIR}/")


if __name__ == "__main__":
    build()
