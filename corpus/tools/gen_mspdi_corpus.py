#!/usr/bin/env python3
"""Generate the MSPDI (MS Project XML) seed corpus (corpus/mspdi/).

Unlike the xlsx corpus, there is no free high-fidelity oracle for project
scheduling (MS Project is the reference implementation and isn't scriptable
here). Each file embeds hand-derived Start/Finish expectations for a standard
8h/day Mon-Fri calendar anchored at Monday 2026-03-02 08:00. The owner checked
the SF shapes in files 05 and 14 against Project 2021 (issue #53), the
24-hour calendar in file 16 (issue #58), and the FNLT conflict in file 17
(issue #60); the other expectations have not been independently verified
against Project.
`projcore/tests/corpus.rs` reads
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
         calendar=None, baselines=()):
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
    for number, baseline_start, baseline_finish, duration in baselines:
        lines += ["      <Baseline>", f"        <Number>{number}</Number>"]
        for tag, value in [("Start", baseline_start), ("Finish", baseline_finish),
                           ("Duration", iso(duration) if duration is not None else None)]:
            if value is not None:
                lines.append(f"        <{tag}>{value}</{tag}>")
        lines.append("      </Baseline>")
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
    with open(os.path.join(OUT_DIR, fname), "w", encoding="utf-8", newline="\n") as fh:
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
            task(2, "B", 1 * D, dt(3), dt(4), preds=[(1, SF, 0)]),
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

    # 13 — resource identity/rates and all three kinds survive saving (#52).
    rich_res = (
        "    <Resource><UID>1</UID><ID>1</ID><Name>Alice</Name><Type>1</Type>"
        "<Initials>A</Initials><Code>C7</Code><Group>Eng</Group><MaxUnits>1</MaxUnits>"
        "<AccrueAt>3</AccrueAt><StandardRate>50</StandardRate>"
        "<OvertimeRate>75</OvertimeRate><CostPerUse>10</CostPerUse></Resource>\n"
        "    <Resource><UID>2</UID><ID>2</ID><Name>Licence</Name><Type>0</Type>"
        "<MaxUnits>1</MaxUnits><IsCostResource>1</IsCostResource></Resource>\n"
        "    <Resource><UID>3</UID><ID>3</ID><Name>Concrete</Name><Type>0</Type>"
        "<MaterialLabel>tonnes</MaterialLabel><MaxUnits>1</MaxUnits></Resource>"
    )
    add("13-resource-fields.xml", ["resource", "resource-fields", "round-trip"],
        "Work resource identity and rates, Cost resource, and Material label survive saving.",
        project("resource-fields",
                task(1, "Build", 2 * D, dt(2), dt(3, "17:00:00")),
                resources_xml=rich_res, assignments_xml=asn))

    # 14 — Project 2021 places the SF successor before the project start (#53).
    add("14-link-sf-before-start.xml", ["link", "link-sf", "before-start"],
        "Start-to-finish successor begins before the project start.",
        project("link-sf-before-start", "\n".join([
            task(1, "A", 2 * D, "2026-02-26T08:00:00", dt(2), preds=[(2, SF, 0)]),
            task(2, "B", 1 * D, dt(2), dt(2, "17:00:00")),
        ])))

    # 15 — recorded baseline plans differ from current task dates/durations (#55).
    add("15-baseline-slots.xml", ["baseline", "round-trip"],
        "Distinct baseline slots and recorded durations, including an omitted Duration.",
        project("baseline-slots", task(1, "Build", 2 * D, dt(2), dt(3, "17:00:00"),
                baselines=[(0, dt(4), dt(6, "17:00:00"), 3 * D),
                           (1, dt(9), dt(13, "17:00:00"), 5 * D),
                           (2, dt(16), dt(17, "17:00:00"), None)])))

    # 16 — Project's full working day is encoded midnight to midnight (#58).
    full_days = "\n".join(weekday(day, True, [("00:00:00", "00:00:00")])
                          for day in range(1, 8))
    full_calendar = ("  <Calendar><UID>3</UID><Name>24 Hours</Name>"
                     "<IsBaseCalendar>1</IsBaseCalendar><WeekDays>\n"
                     + full_days + "\n</WeekDays></Calendar>")
    add("16-24-hour-calendar.xml", ["calendar", "calendar-24hour", "round-trip"],
        "Project 2021 (#58): three 8-hour duration days finish after 24 continuous hours.",
        project("24-hour-calendar", task(1, "Build", 3 * D, dt(2), dt(3), calendar=3),
                calendars=[standard_calendar(), full_calendar]))

    # 17 — Project 2021 honors FNLT over the FS link and reports -5d slack (#60).
    add("17-constraint-fnlt-conflict.xml", ["constraint", "constraint-fnlt", "negative-slack"],
        "Project 2021 (#60): FNLT overrides the FS link; both tasks have -5d total slack.",
        project("constraint-fnlt-conflict", "\n".join([
            task(1, "A", 5 * D, dt(2), dt(6, "17:00:00")),
            task(2, "B", 5 * D, dt(2), dt(6, "17:00:00"),
                 preds=[(1, FS, 0)], ctype=FNLT, cdate=dt(6, "17:00:00")),
        ])))

    manifest = {
        "anchor": "2026-03-02T08:00:00",
        "calendar": "Standard 8h/day Mon-Fri (08:00-12:00, 13:00-17:00)",
        "note": "Start/Finish embedded in each file are the CPM oracle.",
        "files": CORPUS,
    }
    with open(os.path.join(OUT_DIR, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")

    print(f"wrote {len(CORPUS)} MSPDI files + manifest.json to {OUT_DIR}/")


if __name__ == "__main__":
    build()
