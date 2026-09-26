#!/usr/bin/env python3
"""Generate the MSPDI (MS Project XML) seed corpus (corpus/mspdi/).

Unlike the xlsx corpus, there is no free high-fidelity oracle for project
scheduling (MS Project is the reference implementation and isn't scriptable
in CI). Each file embeds Start/Finish, TotalSlack and Critical for a standard
8h/day Mon-Fri calendar anchored at Monday 2026-03-02 08:00. Every value was
checked against Project 2024 by corpus/tools/verify_mspdi_project.py (#74),
which schedules a copy with these oracle elements removed. Earlier owner runs
covered 05 and 14 (#53), 16 (#58), 17 (#60) and 18 (#59); the script
reproduces them. `projcore/tests/corpus.rs` reads each file, runs the CPM
scheduler, and asserts the computed values match the embedded ones, so the
corpus validates the scheduler without needing Project. Rerun the script
whenever a fixture changes. Exceptions: file 19's manual-task expectations
(#77), file 20's task-field expectations (#80) and file 22's progress
expectations (#81) are hand-derived from our scheduler and not yet verified in
Project, as is file 23's derived calendar (#83). Files 24 and 25 (calendar
exceptions, #126) were verified against Project Professional 2024 (build
16.0.17932.21000), as generated and as projcore's write_mspdi writes them, as
was file 26 (percentage and elapsed lags, #104), whose plan and values come
from Project itself (gen_mpp_lag_cases.py), and file 27 (manual summaries,
#124).

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


def task(uid, name, dur_min, start, finish, *, slack, critical, oid=None,
         outline=1, summary=False, milestone=False, preds=(), ctype=None,
         cdate=None, calendar=None, baselines=(), manual=None, manual_start=None,
         manual_finish=None, manual_duration=None, fields=()):
    """One <Task>. `preds` is a list of (uid, type_code, link_lag) or
    (uid, type_code, link_lag, lag_format): LinkLag is tenths of a minute, or
    the percentage itself for LagFormat 19; the format defaults to 7 (days).
    `start`/`finish` are the embedded oracle values (MSPDI datetime strings);
    `slack` (working minutes) and `critical` are Project's TotalSlack and
    Critical. They are required so a new task cannot omit its oracle.
    `manual` (0/1) and the manual_* fields are written only when given.
    `fields` is a list of (element, text) for the stored task fields of #80."""
    oid = uid if oid is None else oid
    lines = [
        "    <Task>",
        f"      <UID>{uid}</UID><ID>{oid}</ID>",
        f"      <Name>{name}</Name>",
    ]
    if manual is not None:
        lines.append(f"      <Manual>{manual}</Manual>")
    lines += [
        f"      <OutlineLevel>{outline}</OutlineLevel>",
        f"      <Summary>{1 if summary else 0}</Summary>",
        f"      <Milestone>{1 if milestone else 0}</Milestone>",
        f"      <Duration>{iso(dur_min)}</Duration><DurationFormat>7</DurationFormat>",
        f"      <Start>{start}</Start><Finish>{finish}</Finish>",
        # MSPDI TotalSlack is in tenths of a minute.
        f"      <TotalSlack>{slack * 10}</TotalSlack><Critical>{1 if critical else 0}</Critical>",
    ]
    lines += [f"      <{tag}>{value}</{tag}>" for tag, value in fields]
    for tag, value in [("ManualStart", manual_start), ("ManualFinish", manual_finish),
                       ("ManualDuration",
                        iso(manual_duration) if manual_duration is not None else None)]:
        if value is not None:
            lines.append(f"      <{tag}>{value}</{tag}>")
    if ctype is not None:
        lines.append(f"      <ConstraintType>{ctype}</ConstraintType>")
        if cdate is not None:
            lines.append(f"      <ConstraintDate>{cdate}</ConstraintDate>")
    if calendar is not None:
        lines.append(f"      <CalendarUID>{calendar}</CalendarUID>")
    for (puid, ptype, lag, *fmt) in preds:
        lines += [
            "      <PredecessorLink>",
            f"        <PredecessorUID>{puid}</PredecessorUID>",
            f"        <Type>{ptype}</Type>",
            f"        <LinkLag>{lag}</LinkLag><LagFormat>{fmt[0] if fmt else 7}</LagFormat>",
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


def blank_row(uid, oid, fields=()):
    """One blank row (`<IsNull>1</IsNull>`): Project keeps it as an entry with
    a UID and ID but it is not a task, so it carries no oracle. No Project
    file with a blank row was available; this shape is ours, not Project's."""
    lines = ["    <Task>", f"      <UID>{uid}</UID><ID>{oid}</ID>"]
    lines += [f"      <{tag}>{value}</{tag}>" for tag, value in fields]
    lines += ["      <IsNull>1</IsNull>", "    </Task>"]
    return "\n".join(lines)


def working_times(times):
    return (["      <WorkingTimes>"]
            + [f"        <WorkingTime><FromTime>{f}</FromTime><ToTime>{t}</ToTime></WorkingTime>"
               for (f, t) in times]
            + ["      </WorkingTimes>"])


def weekday(day_type, working, times):
    """One <WeekDay>. day_type: 1=Sun..7=Sat. times: list of (from, to) 'HH:MM:SS'."""
    out = [f"    <WeekDay><DayType>{day_type}</DayType>"
           f"<DayWorking>{1 if working else 0}</DayWorking>"]
    if working and times:
        out += working_times(times)
    out.append("    </WeekDay>")
    return "\n".join(out)


SHIFT = [("08:00:00", "12:00:00"), ("13:00:00", "17:00:00")]


def exception_legacy(ex):
    """An exception's legacy form: a `DayType 0` <WeekDay> over its dates."""
    _name, first, last, times = ex
    out = [f"    <WeekDay><DayType>0</DayType><DayWorking>{1 if times else 0}</DayWorking>",
           f"      <TimePeriod><FromDate>{first}T00:00:00</FromDate>"
           f"<ToDate>{last}T23:59:00</ToDate></TimePeriod>"]
    if times:
        out += working_times(times)
    out.append("    </WeekDay>")
    return "\n".join(out)


def exceptions(exs):
    """<Exceptions> for date-range exceptions (name, first date, last date,
    working times or [] for a day off), in the shape Project writes them."""
    out = ["    <Exceptions>"]
    for name, first, last, times in exs:
        out += ["    <Exception><EnteredByOccurrences>0</EnteredByOccurrences>",
                f"      <TimePeriod><FromDate>{first}T00:00:00</FromDate>"
                f"<ToDate>{last}T23:59:00</ToDate></TimePeriod>",
                f"      <Occurrences>1</Occurrences><Name>{name}</Name><Type>1</Type>"
                f"<DayWorking>{1 if times else 0}</DayWorking>"]
        if times:
            out += working_times(times)
        out.append("    </Exception>")
    out.append("    </Exceptions>")
    return "\n".join(out)


def standard_calendar(uid=1, name="Standard", saturday=False, exs=()):
    """Standard's week. `exs` are its date-range exceptions, written in both
    forms as Project writes them: legacy `DayType 0` weekdays and
    <Exceptions>."""
    days = []
    # DayType 1=Sunday .. 7=Saturday.
    for dt in range(1, 8):
        if dt == 1:  # Sunday
            days.append(weekday(dt, False, []))
        elif dt == 7:  # Saturday
            days.append(weekday(dt, saturday, SHIFT if saturday else []))
        else:  # Mon..Fri
            days.append(weekday(dt, True, SHIFT))
    days += [exception_legacy(ex) for ex in exs]
    return (f"  <Calendar>\n    <UID>{uid}</UID><Name>{name}</Name>"
            f"<IsBaseCalendar>1</IsBaseCalendar>\n"
            f"    <WeekDays>\n" + "\n".join(days) + "\n    </WeekDays>\n"
            + (exceptions(exs) + "\n" if exs else "") + "  </Calendar>")


def project(name, tasks_xml, *, resources_xml="", assignments_xml="",
            calendars=None, new_tasks_are_manual=None):
    calendars = calendars or [standard_calendar()]
    parts = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<Project xmlns="http://schemas.microsoft.com/project">',
        f"  <Name>{name}</Name>",
        "  <MinutesPerDay>480</MinutesPerDay>",
        "  <MinutesPerWeek>2400</MinutesPerWeek>",
        "  <CalendarUID>1</CalendarUID>",
        f"  <StartDate>2026-03-02T08:00:00</StartDate>",
        # Without this, Project treats the file as externally edited, ignores
        # <Duration> and rederives it from Start/Finish, which zeroes tasks that
        # start at the project start (#74).
        "  <ProjectExternallyEdited>0</ProjectExternallyEdited>",
    ]
    if new_tasks_are_manual is not None:
        parts.append(f"  <NewTasksAreManual>{new_tasks_are_manual}</NewTasksAreManual>")
    parts += [
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
# Project 2024 (#74) gives most tasks no total slack and marks them critical.
CRIT = {"slack": 0, "critical": True}

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
                task(1, "Dig foundation", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT)))

    # 02 — finish-to-start dependency.
    add("02-link-fs.xml", ["link", "link-fs"], "Finish-to-start dependency.",
        project("link-fs", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
            task(2, "B", 2 * D, dt(4), dt(5, "17:00:00"), **CRIT, preds=[(1, FS, 0)]),
        ])))

    # 03 — start-to-start dependency.
    add("03-link-ss.xml", ["link", "link-ss"], "Start-to-start dependency.",
        project("link-ss", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
            task(2, "B", 3 * D, dt(2), dt(4, "17:00:00"), **CRIT, preds=[(1, SS, 0)]),
        ])))

    # 04 — finish-to-finish dependency.
    add("04-link-ff.xml", ["link", "link-ff"], "Finish-to-finish dependency.",
        project("link-ff", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
            task(2, "B", 1 * D, dt(3), dt(3, "17:00:00"), **CRIT, preds=[(1, FF, 0)]),
        ])))

    # 05 — start-to-finish dependency (predecessor pinned by SNET so the
    # successor's finish lands on a real working boundary, not the anchor).
    add("05-link-sf.xml", ["link", "link-sf"], "Start-to-finish dependency.",
        project("link-sf", "\n".join([
            task(1, "A", 2 * D, dt(4), dt(5, "17:00:00"), **CRIT, ctype=SNET, cdate=dt(4)),
            task(2, "B", 1 * D, dt(3), dt(4), slack=2 * D, critical=False, preds=[(1, SF, 0)]),
        ])))

    # 06 — FS with +2 day lag (LinkLag is tenths of a minute: 2d = 2*480*10).
    add("06-lag.xml", ["link", "lag"], "FS link with +2 working-day lag.",
        project("lag", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
            task(2, "B", 1 * D, dt(6), dt(6, "17:00:00"), **CRIT,
                 preds=[(1, FS, 2 * D * 10)]),
        ])))

    # 07 — FS with -1 day lead (negative lag: overlap).
    add("07-lead.xml", ["link", "lead"], "FS link with -1 working-day lead.",
        project("lead", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
            task(2, "B", 1 * D, dt(3), dt(3, "17:00:00"), **CRIT,
                 preds=[(1, FS, -1 * D * 10)]),
        ])))

    # 08 — a milestone (zero duration) with a Start-No-Earlier-Than constraint.
    add("08-milestone.xml", ["milestone", "constraint", "constraint-snet"],
        "Zero-duration milestone with SNET constraint.",
        project("milestone",
                task(1, "Permit approved", 0, dt(5), dt(5), **CRIT, milestone=True,
                     ctype=SNET, cdate=dt(5))))

    # 09 — a Start-No-Earlier-Than constraint on a normal task.
    add("09-constraint-snet.xml", ["constraint", "constraint-snet"],
        "Start-No-Earlier-Than constraint delays the start.",
        project("constraint-snet",
                task(1, "Delayed", 2 * D, dt(5), dt(6, "17:00:00"), **CRIT,
                     ctype=SNET, cdate=dt(5))))

    # 10 — a summary task with two children (outline rollup).
    add("10-summary.xml", ["summary"], "Summary task rolling up two children.",
        project("summary", "\n".join([
            task(1, "Phase", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT, summary=True, outline=1),
            task(2, "A", 1 * D, dt(2), dt(2, "17:00:00"), **CRIT, oid=2, outline=2),
            task(3, "B", 1 * D, dt(3), dt(3, "17:00:00"), **CRIT, oid=3, outline=2,
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
                task(1, "Build", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
                resources_xml=res, assignments_xml=asn))

    # 12 — a custom 6-day calendar (Saturday working) changes the finish date.
    # A 6-day task on the Standard calendar would finish Mon Mar 9; with
    # Saturday working it finishes Sat Mar 7.
    add("12-calendar-6day.xml", ["calendar", "calendar-6day"],
        "Custom 6-day calendar (Saturday working) shortens the schedule.",
        project("calendar-6day",
                task(1, "Six days", 6 * D, dt(2), dt(7, "17:00:00"), **CRIT, calendar=2),
                calendars=[standard_calendar(1),
                           standard_calendar(2, "SixDay", saturday=True)]))

    # 13 — resource identity/rates and all three kinds survive saving (#52),
    # with each rate's display unit (a standard rate shown per day and an
    # overtime rate shown per week),
    # booking type, flags and stored work, and an assignment's contour, flags,
    # own dates and regular work (#84); the resource's e-mail, availability,
    # overtime work, cost, notes, custom field, baseline, availability period
    # and rate tables A and B, and the assignment's cost, rate table, delays,
    # notes, overtime, custom field and timephased work (#199); and every other
    # child of Microsoft's Resource and Assignment (#267): GUIDs, hyperlinks,
    # actual and remaining overtime, earned value, owners, an outline code and
    # timephased Baseline work on the resource, and the assignment's progress,
    # flags, budget and a Baseline. The #267 values are synthetic (not
    # re-verified in Project) and only stored, so the schedule is unchanged:
    # the delays are zero, the period covers the whole plan, and the resource
    # calendar is the Standard base calendar. Elements follow the Project
    # 2010+ sequence, as Project 2024 writes it.
    rich_res = (
        "    <Resource><UID>1</UID><GUID>0B7E4C2A-1D3F-4E5A-8B6C-7D8E9F0A1B2C</GUID>"
        "<ID>1</ID><Name>Alice</Name><Type>1</Type><IsNull>0</IsNull>"
        "<Initials>A</Initials><Phonetics>arisu</Phonetics><NTAccount>SITE\\alice</NTAccount>"
        "<Code>C7</Code><Group>Eng</Group><WorkGroup>1</WorkGroup>"
        "<EmailAddress>alice@example.com</EmailAddress>"
        "<Hyperlink>Profile</Hyperlink><HyperlinkAddress>https://example.com/alice</HyperlinkAddress>"
        "<HyperlinkSubAddress>skills</HyperlinkSubAddress>"
        "<MaxUnits>1</MaxUnits><PeakUnits>1</PeakUnits><OverAllocated>0</OverAllocated>"
        "<AvailableFrom>1984-01-01T00:00:00</AvailableFrom>"
        "<AvailableTo>2049-12-31T23:59:00</AvailableTo>"
        f"<Start>{dt(2)}</Start><Finish>{dt(3, '17:00:00')}</Finish>"
        "<CanLevel>1</CanLevel><AccrueAt>3</AccrueAt><Work>PT16H0M0S</Work>"
        "<RegularWork>PT16H0M0S</RegularWork><OvertimeWork>PT0H0M0S</OvertimeWork>"
        "<ActualWork>PT4H0M0S</ActualWork>"
        "<RemainingWork>PT16H0M0S</RemainingWork>"
        "<ActualOvertimeWork>PT1H0M0S</ActualOvertimeWork>"
        "<RemainingOvertimeWork>PT2H0M0S</RemainingOvertimeWork>"
        "<PercentWorkComplete>25</PercentWorkComplete>"
        "<StandardRate>50</StandardRate><StandardRateFormat>3</StandardRateFormat>"
        "<Cost>970</Cost>"
        "<OvertimeRate>75</OvertimeRate><OvertimeRateFormat>4</OvertimeRateFormat>"
        "<OvertimeCost>150</OvertimeCost>"
        "<CostPerUse>10</CostPerUse>"
        "<ActualCost>210</ActualCost><ActualOvertimeCost>75</ActualOvertimeCost>"
        "<RemainingCost>760</RemainingCost><RemainingOvertimeCost>155</RemainingOvertimeCost>"
        "<WorkVariance>60000</WorkVariance><CostVariance>12.5</CostVariance>"
        "<SV>-25</SV><CV>5</CV><ACWP>205</ACWP>"
        "<CalendarUID>1</CalendarUID>"
        "<Notes>Site lead &amp; first aider</Notes>"
        "<BCWS>235</BCWS><BCWP>210.5</BCWP>"
        "<IsGeneric>1</IsGeneric><IsInactive>0</IsInactive><IsEnterprise>0</IsEnterprise>"
        "<BookingType>1</BookingType>"
        "<ActualWorkProtected>PT3H0M0S</ActualWorkProtected>"
        "<ActualOvertimeWorkProtected>PT0H30M0S</ActualOvertimeWorkProtected>"
        "<ActiveDirectoryGUID>7C9E6679-7425-40DE-944B-E07FC1F90AE7</ActiveDirectoryGUID>"
        "<CreationDate>2026-02-20T09:15:00</CreationDate>"
        "<CostCenter>CC-410</CostCenter>"
        "<AssnOwner>Alice</AssnOwner>"
        "<AssnOwnerGuid>0B7E4C2A-1D3F-4E5A-8B6C-7D8E9F0A1B2C</AssnOwnerGuid>"
        "<ExtendedAttribute><FieldID>205520904</FieldID><Value>Ops</Value></ExtendedAttribute>"
        "<Baseline><Number>0</Number><Work>PT16H0M0S</Work><Cost>970</Cost></Baseline>"
        "<OutlineCode><FieldID>205521406</FieldID><ValueID>3</ValueID>"
        "<ValueGUID>4A5B6C7D-8E9F-4A0B-9C1D-2E3F4A5B6C7D</ValueGUID></OutlineCode>"
        "<AvailabilityPeriods><AvailabilityPeriod>"
        "<AvailableFrom>1984-01-01T00:00:00</AvailableFrom>"
        "<AvailableTo>2049-12-31T23:59:00</AvailableTo>"
        "<AvailableUnits>1</AvailableUnits></AvailabilityPeriod></AvailabilityPeriods>"
        "<Rates>"
        "<Rate><RatesFrom>1984-01-01T00:00:00</RatesFrom><RatesTo>2049-12-31T23:59:00</RatesTo>"
        "<RateTable>0</RateTable><StandardRate>50</StandardRate>"
        "<StandardRateFormat>3</StandardRateFormat><OvertimeRate>75</OvertimeRate>"
        "<OvertimeRateFormat>4</OvertimeRateFormat><CostPerUse>10</CostPerUse></Rate>"
        "<Rate><RatesFrom>1984-01-01T00:00:00</RatesFrom><RatesTo>2049-12-31T23:59:00</RatesTo>"
        "<RateTable>1</RateTable><StandardRate>60</StandardRate>"
        "<StandardRateFormat>2</StandardRateFormat><OvertimeRate>90</OvertimeRate>"
        "<OvertimeRateFormat>2</OvertimeRateFormat><CostPerUse>10</CostPerUse></Rate>"
        "</Rates>"
        f"<TimephasedData><Type>7</Type><UID>1</UID><Start>{dt(2)}</Start>"
        f"<Finish>{dt(3)}</Finish><Unit>2</Unit><Value>PT7H0M0S</Value></TimephasedData>"
        f"<TimephasedData><Type>8</Type><UID>1</UID><Start>{dt(2)}</Start>"
        f"<Finish>{dt(3)}</Finish><Unit>2</Unit><Value>485</Value></TimephasedData>"
        "</Resource>\n"
        "    <Resource><UID>2</UID><ID>2</ID><Name>Licence</Name><Type>0</Type>"
        "<MaxUnits>1</MaxUnits><IsCostResource>1</IsCostResource>"
        "<IsBudget>1</IsBudget></Resource>\n"
        "    <Resource><UID>3</UID><ID>3</ID><Name>Concrete</Name><Type>0</Type>"
        "<MaterialLabel>tonnes</MaterialLabel><MaxUnits>1</MaxUnits>"
        "<IsInactive>1</IsInactive></Resource>"
    )
    rich_asn = ("    <Assignment><UID>1</UID><GUID>5D6E7F80-9A1B-4C2D-8E3F-405162738495</GUID>"
                "<TaskUID>1</TaskUID><ResourceUID>1</ResourceUID>"
                "<PercentWorkComplete>0</PercentWorkComplete>"
                "<ActualCost>205</ActualCost>"
                f"<ActualFinish>{dt(3, '12:00:00')}</ActualFinish>"
                "<ActualOvertimeCost>70</ActualOvertimeCost>"
                "<ActualOvertimeWork>PT0H45M0S</ActualOvertimeWork>"
                f"<ActualStart>{dt(2)}</ActualStart><ActualWork>PT3H0M0S</ActualWork>"
                "<ACWP>200</ACWP><Confirmed>1</Confirmed>"
                "<Cost>970</Cost><CostRateTable>1</CostRateTable><RateScale>2</RateScale>"
                "<CostVariance>-7.5</CostVariance><CV>2.5</CV><Delay>0</Delay>"
                f"<Finish>{dt(3, '17:00:00')}</Finish><FinishVariance>0</FinishVariance>"
                "<Hyperlink>Pour plan</Hyperlink>"
                "<HyperlinkAddress>https://example.com/pour</HyperlinkAddress>"
                "<HyperlinkSubAddress>day1</HyperlinkSubAddress>"
                "<WorkVariance>0</WorkVariance>"
                "<HasFixedRateUnits>1</HasFixedRateUnits>"
                "<FixedMaterial>0</FixedMaterial>"
                "<LevelingDelay>0</LevelingDelay><LevelingDelayFormat>7</LevelingDelayFormat>"
                "<LinkedFields>0</LinkedFields><Milestone>0</Milestone>"
                "<Notes>Pour on day one</Notes><Overallocated>0</Overallocated>"
                "<OvertimeCost>140</OvertimeCost><OvertimeWork>PT0H0M0S</OvertimeWork>"
                "<PeakUnits>1</PeakUnits>"
                "<RegularWork>PT16H0M0S</RegularWork>"
                "<RemainingCost>765</RemainingCost>"
                "<RemainingOvertimeCost>145</RemainingOvertimeCost>"
                "<RemainingOvertimeWork>PT1H30M0S</RemainingOvertimeWork>"
                f"<RemainingWork>PT16H0M0S</RemainingWork><ResponsePending>0</ResponsePending>"
                f"<Start>{dt(2)}</Start>"
                f"<Stop>{dt(2, '12:00:00')}</Stop><Resume>{dt(2, '13:00:00')}</Resume>"
                "<StartVariance>0</StartVariance><Summary>0</Summary><SV>-20</SV>"
                "<Units>1</Units><UpdateNeeded>0</UpdateNeeded><VAC>-15</VAC>"
                "<Work>PT16H0M0S</Work><WorkContour>0</WorkContour>"
                "<BCWS>225</BCWS><BCWP>202.5</BCWP><BookingType>0</BookingType>"
                "<ActualWorkProtected>PT2H0M0S</ActualWorkProtected>"
                "<ActualOvertimeWorkProtected>PT0H15M0S</ActualOvertimeWorkProtected>"
                "<CreationDate>2026-02-21T10:30:00</CreationDate>"
                "<AssnOwner>Alice</AssnOwner>"
                "<AssnOwnerGuid>0B7E4C2A-1D3F-4E5A-8B6C-7D8E9F0A1B2C</AssnOwnerGuid>"
                "<BudgetCost>1000</BudgetCost><BudgetWork>PT18H0M0S</BudgetWork>"
                "<ExtendedAttribute><FieldID>255852547</FieldID><Value>12.5</Value>"
                "</ExtendedAttribute>"
                f"<Baseline><Number>0</Number><Start>{dt(2)}</Start>"
                f"<Finish>{dt(3, '17:00:00')}</Finish><Work>PT16H0M0S</Work><Cost>970</Cost>"
                "</Baseline>"
                f"<TimephasedData><Type>1</Type><UID>1</UID><Start>{dt(2)}</Start>"
                f"<Finish>{dt(3)}</Finish><Unit>2</Unit><Value>PT8H0M0S</Value></TimephasedData>"
                f"<TimephasedData><Type>1</Type><UID>1</UID><Start>{dt(3)}</Start>"
                f"<Finish>{dt(3, '17:00:00')}</Finish><Unit>2</Unit><Value>PT8H0M0S</Value>"
                "</TimephasedData>"
                "</Assignment>")
    add("13-resource-fields.xml", ["resource", "resource-fields", "round-trip"],
        "Work resource identity, rates and their display units, booking type and flags, "
        "availability and cost rate tables, Cost and Material resources, and an assignment's "
        "contour, flags, dates, delays, cost and timephased work survive saving, with every "
        "other Resource and Assignment child (GUIDs, hyperlinks, overtime, earned value, "
        "owners, outline codes, budget, timephased Baseline).",
        project("resource-fields",
                task(1, "Build", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT),
                resources_xml=rich_res, assignments_xml=rich_asn))

    # 14 — Project 2024 places the SF successor before the project start (#53).
    add("14-link-sf-before-start.xml", ["link", "link-sf", "before-start"],
        "Start-to-finish successor begins before the project start.",
        project("link-sf-before-start", "\n".join([
            task(1, "A", 2 * D, "2026-02-26T08:00:00", dt(2), slack=D, critical=False,
                 preds=[(2, SF, 0)]),
            task(2, "B", 1 * D, dt(2), dt(2, "17:00:00"), **CRIT),
        ])))

    # 15 — recorded baseline plans differ from current task dates/durations (#55).
    add("15-baseline-slots.xml", ["baseline", "round-trip"],
        "Distinct baseline slots and recorded durations, including an omitted Duration.",
        project("baseline-slots", task(1, "Build", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT,
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
        "Project 2024 (#58): three 8-hour duration days finish after 24 continuous hours.",
        project("24-hour-calendar", task(1, "Build", 3 * D, dt(2), dt(3), **CRIT, calendar=3),
                calendars=[standard_calendar(), full_calendar]))

    # 17 — Project 2024 honors FNLT over the FS link and reports -5d slack (#60).
    add("17-constraint-fnlt-conflict.xml", ["constraint", "constraint-fnlt", "negative-slack"],
        "Project 2024 (#60): FNLT overrides the FS link; both tasks have -5d total slack.",
        project("constraint-fnlt-conflict", "\n".join([
            task(1, "A", 5 * D, dt(2), dt(6, "17:00:00"), slack=-5 * D, critical=True),
            task(2, "B", 5 * D, dt(2), dt(6, "17:00:00"), slack=-5 * D, critical=True,
                 preds=[(1, FS, 0)], ctype=FNLT, cdate=dt(6, "17:00:00")),
        ])))

    # 18 — Project 2024 places FS milestones at the predecessor's finish (#59).
    add("18-milestone-after-fs.xml", ["milestone", "link", "link-fs"],
        "Project 2024 (#59): FS milestones keep the predecessor finish instant.",
        project("milestone-after-fs", "\n".join([
            task(1, "A", 2 * D, dt(2), dt(3, "17:00:00"), slack=4 * D, critical=False),
            task(2, "Sign-off", 0, dt(3, "17:00:00"), dt(3, "17:00:00"),
                 slack=4 * D, critical=False, milestone=True, preds=[(1, FS, 0)]),
            task(3, "Chain A", D, dt(2), dt(2, "17:00:00"), **CRIT),
            task(4, "M1", 0, dt(2, "17:00:00"), dt(2, "17:00:00"), **CRIT,
                 milestone=True, preds=[(3, FS, 0)]),
            task(5, "B", 5 * D, dt(3), dt(9, "17:00:00"), **CRIT, preds=[(4, FS, 0)]),
            task(6, "M2", 0, dt(9, "17:00:00"), dt(9, "17:00:00"), **CRIT,
                 milestone=True, preds=[(5, FS, 0)]),
        ])))

    # 19 — manually scheduled tasks (#77) stay at their pinned dates: one pinned
    # before its FS link allows (the link wants Thu 5), one pinned after it
    # (Mon 9), and an auto successor that follows the pinned finish.
    add("19-manual-tasks.xml", ["manual", "link", "link-fs", "summary", "round-trip"],
        "Manual tasks keep their pinned dates; an auto successor follows them.",
        project("manual-tasks", "\n".join([
            task(1, "Phase", 9 * D, dt(2), dt(12, "17:00:00"), slack=-2 * D,
                 critical=True, summary=True, manual=0),
            task(2, "Design", 3 * D, dt(2), dt(4, "17:00:00"), slack=2 * D,
                 critical=False, outline=2, manual=0),
            # Pinned Tue 3, the link wants Thu 5: the violation is -2d of slack.
            task(3, "Review", D, dt(3), dt(3, "17:00:00"), slack=-2 * D, critical=True,
                 outline=2, preds=[(2, FS, 0)], manual=1, manual_start=dt(3),
                 manual_duration=D),
            task(4, "Vendor", 2 * D, dt(9), dt(10, "17:00:00"), **CRIT, outline=2,
                 preds=[(2, FS, 0)], manual=1, manual_start=dt(9),
                 manual_finish=dt(10, "17:00:00"), manual_duration=2 * D),
            task(5, "Build", 2 * D, dt(11), dt(12, "17:00:00"), **CRIT, outline=2,
                 preds=[(4, FS, 0)], manual=0),
        ]), new_tasks_are_manual=1))

    # 20 — task fields Project writes that the model keeps (#80): task type,
    # effort-driven, estimated, active, priority, deadline, levelling, display
    # flags, WBS, GUID/CreateDate, stored Work/Cost, and a blank row between
    # two linked tasks under a summary. Values follow a Project 2024 corpus:
    # most tasks estimated, Priority 500 or 900, LevelingDelayFormat 8.
    # Hand-derived like file 19, not verified in Project: docxy still
    # schedules the inactive task, and the blank row's shape is ours.
    common = [("LevelAssignments", 1), ("LevelingCanSplit", 1),
              ("LevelingDelay", 0), ("LevelingDelayFormat", 8),
              ("IgnoreResourceCalendar", 0), ("HideBar", 0), ("EarnedValueMethod", 0),
              ("Recurring", 0), ("OverAllocated", 0), ("ExternalTask", 0),
              ("IsSubproject", 0), ("IsSubprojectReadOnly", 0)]
    created = "2026-02-27T09:30:00"

    def ident(n):
        return [("GUID", f"0B6F1C20-4D3E-4A51-9C7B-00000000000{n}"),
                ("CreateDate", created)]

    add("20-task-fields.xml", ["task-fields", "round-trip", "blank-row", "summary", "link",
                               "link-fs"],
        "Task type, estimate, active, deadline, levelling and a blank row survive saves.",
        project("task-fields", "\n".join([
            task(1, "Phase", 3 * D, dt(2), dt(4, "17:00:00"), **CRIT, summary=True,
                 fields=ident(1) + [("Active", 1), ("Type", 1), ("WBS", "1"),
                                    ("Priority", 500), ("Estimated", 0), ("Rollup", 0)]
                 + common),
            task(2, "Excavate", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT, outline=2,
                 fields=ident(2) + [("Active", 1), ("Type", 0), ("WBS", "1.1"),
                                    ("Priority", 900), ("Estimated", 1),
                                    ("EffortDriven", 1), ("Work", iso(2 * D)),
                                    ("Cost", "1250.50"), ("Rollup", 1)] + common),
            blank_row(3, 3, ident(3)),
            # Its link from the blank row is ignored: Pour follows Excavate.
            task(4, "Pour", D, dt(4), dt(4, "17:00:00"), **CRIT, outline=2,
                 preds=[(2, FS, 0), (3, FS, 0)],
                 fields=ident(4) + [("Active", 1), ("Type", 2), ("WBS", "1.2"),
                                    ("Priority", 500), ("Estimated", 1),
                                    ("EffortDriven", 0), ("Work", iso(0)),
                                    ("Deadline", dt(20, "17:00:00")),
                                    ("LevelAssignments", 0), ("LevelingCanSplit", 0),
                                    ("LevelingDelay", 4800), ("LevelingDelayFormat", 7),
                                    ("IgnoreResourceCalendar", 1), ("HideBar", 1),
                                    ("EarnedValueMethod", 1), ("Rollup", 0)]),
            # Inactive: Project drops it from the schedule; docxy does not yet.
            task(5, "Inspect", D, dt(2), dt(2, "17:00:00"), slack=2 * D, critical=False,
                 fields=ident(5) + [("Active", 0), ("Type", 1), ("WBS", "2"),
                                    ("Priority", 500), ("Estimated", 1)] + common),
        ])))

    # 21 — B misses its Deadline by 5 days. The deadline bounds late finish
    # only: dates stay put and A and B both get -5d total slack (#100).
    add("21-deadline-missed.xml", ["deadline", "link", "link-fs", "negative-slack"],
        "Project 2024 (#100): a missed Deadline gives B and its driver A -5d total slack.",
        project("deadline-missed", "\n".join([
            task(1, "A", 5 * D, dt(2), dt(6, "17:00:00"), slack=-5 * D, critical=True),
            task(2, "B", 5 * D, dt(9), dt(13, "17:00:00"), slack=-5 * D, critical=True,
                 preds=[(1, FS, 0)], fields=[("Deadline", dt(6, "17:00:00"))]),
        ])))

    # 22 — recorded progress survives saves (#81): a complete task, an
    # in-progress task stopped Thu 5 and resuming Fri 6, and a not-started
    # task, each with an assignment carrying its actuals; the in-progress
    # assignment also has two baseline slots. Shapes follow a Project 2024
    # tracked plan (Stop == Resume == finish once complete, Resume at the next
    # working moment after Stop). The actual dates equal the scheduled ones, so
    # the oracle holds while the scheduler ignores progress. Hand-derived like
    # files 19 and 20, not verified in Project; no Project file with an
    # assignment <Baseline> was available, so that shape follows the schema.
    progress_res = ("    <Resource><UID>1</UID><ID>1</ID><Name>Alice</Name>"
                    "<Type>1</Type><MaxUnits>1</MaxUnits><StandardRate>50</StandardRate>"
                    "</Resource>")

    def assignment(uid, task_uid, work, fields=(), baselines=()):
        # Children in Project's Assignment sequence.
        lines = ["    <Assignment>",
                 f"      <UID>{uid}</UID><TaskUID>{task_uid}</TaskUID>"
                 "<ResourceUID>1</ResourceUID>"]
        lines += [f"      <{tag}>{value}</{tag}>" for tag, value in fields]
        lines.append(f"      <Units>1</Units><Work>{iso(work)}</Work>")
        for number, children in baselines:
            lines += ["      <Baseline>", f"        <Number>{number}</Number>"]
            lines += [f"        <{tag}>{value}</{tag}>" for tag, value in children]
            lines.append("      </Baseline>")
        lines.append("    </Assignment>")
        return "\n".join(lines)

    progress_asn = "\n".join([
        assignment(1, 1, 2 * D, [
            ("PercentWorkComplete", 100), ("ActualCost", "800"),
            ("ActualFinish", dt(3, "17:00:00")), ("ActualStart", dt(2)),
            ("ActualWork", iso(2 * D)), ("CostVariance", "0"), ("FinishVariance", 0),
            ("WorkVariance", "0.0"), ("Stop", dt(3, "17:00:00")),
            ("Resume", dt(3, "17:00:00")), ("StartVariance", 0)],
            baselines=[(0, [("Start", dt(2)), ("Finish", dt(3, "17:00:00")),
                            ("Work", iso(2 * D)), ("Cost", "800")])]),
        assignment(2, 2, 4 * D, [
            ("PercentWorkComplete", 50), ("ActualCost", "800"), ("ActualStart", dt(4)),
            ("ActualWork", iso(2 * D)), ("CostVariance", "400"),
            ("FinishVariance", 4800), ("WorkVariance", "480000.0"),
            ("RemainingCost", "800"), ("RemainingWork", iso(2 * D)),
            ("Stop", dt(5, "17:00:00")), ("Resume", dt(6)), ("StartVariance", 0)],
            baselines=[(0, [("Start", dt(4)), ("Finish", dt(6, "17:00:00")),
                            ("Work", iso(3 * D)), ("Cost", "1200")]),
                       (1, [("Work", iso(5 * D))])]),
        assignment(3, 3, D, [
            ("PercentWorkComplete", 0), ("RemainingCost", "400"),
            ("RemainingWork", iso(D))]),
    ])
    add("22-progress.xml", ["progress", "assignment", "round-trip", "link", "link-fs"],
        "Percent complete, actuals, stop/resume, remaining values, variances and "
        "assignment baselines survive saves.",
        project("progress", "\n".join([
            task(1, "Excavate", 2 * D, dt(2), dt(3, "17:00:00"), **CRIT, fields=[
                ("Stop", dt(3, "17:00:00")), ("Resume", dt(3, "17:00:00")),
                ("StartVariance", 0), ("FinishVariance", 0), ("WorkVariance", "0.0"),
                ("PercentComplete", 100), ("PercentWorkComplete", 100),
                ("ActualStart", dt(2)), ("ActualFinish", dt(3, "17:00:00")),
                ("ActualDuration", iso(2 * D)), ("ActualCost", "800"),
                ("ActualWork", iso(2 * D)), ("PhysicalPercentComplete", 0)]),
            task(2, "Pour", 4 * D, dt(4), dt(9, "17:00:00"), **CRIT, preds=[(1, FS, 0)],
                 fields=[
                ("Stop", dt(5, "17:00:00")), ("Resume", dt(6)),
                ("StartVariance", 0), ("FinishVariance", 4800),
                ("WorkVariance", "480000.0"), ("PercentComplete", 50),
                ("PercentWorkComplete", 50), ("ActualStart", dt(4)),
                ("ActualDuration", iso(2 * D)), ("ActualCost", "800"),
                ("ActualWork", iso(2 * D)), ("RemainingDuration", iso(2 * D)),
                ("RemainingCost", "800"), ("RemainingWork", iso(2 * D)),
                ("PhysicalPercentComplete", 40)]),
            task(3, "Cure", D, dt(10), dt(10, "17:00:00"), **CRIT, preds=[(2, FS, 0)],
                 fields=[
                ("StartVariance", 4800), ("FinishVariance", 4800),
                ("PercentComplete", 0), ("PercentWorkComplete", 0),
                ("RemainingDuration", iso(D)), ("RemainingCost", "400"),
                ("RemainingWork", iso(D)), ("PhysicalPercentComplete", 0)]),
        ]), resources_xml=progress_res, assignments_xml=progress_asn))

    # 23 — a derived calendar keeps its base through a save (#83): "Crew"
    # derives from Standard (BaseCalendarUID 1), is flagged IsBaselineCalendar
    # and states only Friday, which it takes off; every other day is Standard's.
    # A resource uses it, as Project's resource calendars do, and so does task
    # Frame, whose 5 days run Mon 2-Thu 5, skip Friday and finish Mon 9. Project's
    # UI offers only base calendars to tasks, so a task on a derived calendar is
    # our shape, not Project's; hand-derived, not verified in Project.
    crew = ("  <Calendar>\n    <UID>2</UID><Name>Crew</Name>"
            "<IsBaseCalendar>0</IsBaseCalendar><IsBaselineCalendar>1</IsBaselineCalendar>"
            "<BaseCalendarUID>1</BaseCalendarUID>\n"
            "    <WeekDays>\n" + weekday(6, False, []) + "\n    </WeekDays>\n  </Calendar>")
    crew_res = ("    <Resource><UID>1</UID><ID>1</ID><Name>Alice</Name>"
                "<Type>1</Type><MaxUnits>1</MaxUnits><CalendarUID>2</CalendarUID></Resource>")
    add("23-derived-calendar.xml", ["calendar", "derived-calendar", "round-trip", "link",
                                    "link-fs"],
        "A calendar derived from Standard keeps its base, its own Friday off and "
        "IsBaselineCalendar through saves; a task on it skips Friday.",
        project("derived-calendar", "\n".join([
            task(1, "Frame", 5 * D, dt(2), dt(9, "17:00:00"), **CRIT, calendar=2),
            task(2, "Paint", D, dt(10), dt(10, "17:00:00"), **CRIT, preds=[(1, FS, 0)]),
        ]), resources_xml=crew_res, calendars=[standard_calendar(), crew]))

    # 24 — calendar exceptions (#126): Standard takes Wed 4 off ("Founders
    # day") and works Saturday 14 08:00-12:00 ("Stocktake"), both written in
    # the two forms Project writes. Pour's 3 days skip the holiday (Mon 2, Tue
    # 3, Thu 5); Cure runs Fri 6 to Fri 13; Inspect's day is Saturday's four
    # hours and Monday 16's morning.
    holiday_exs = [("Founders day", "2026-03-04", "2026-03-04", []),
                   ("Stocktake", "2026-03-14", "2026-03-14", [("08:00:00", "12:00:00")])]
    add("24-calendar-holiday.xml", ["calendar", "calendar-exception", "round-trip", "link",
                                    "link-fs"],
        "A holiday inside a task pushes its finish out a day; a working Saturday "
        "with changed hours carries a later task; both exceptions survive saves.",
        project("calendar-holiday", "\n".join([
            task(1, "Pour", 3 * D, dt(2), dt(5, "17:00:00"), **CRIT),
            task(2, "Cure", 6 * D, dt(6), dt(13, "17:00:00"), **CRIT, preds=[(1, FS, 0)]),
            task(3, "Inspect", D, dt(14), dt(16, "12:00:00"), **CRIT, preds=[(2, FS, 0)]),
        ]), calendars=[standard_calendar(exs=holiday_exs)]))

    # 25 — a derived calendar and its base's holidays (#126). Standard takes
    # Wed 4 and Wed 11 off. Alice's resource calendar derives from it, states
    # Wednesday as 07:00-15:00 and works Wed 11 07:00-15:00 as an exception of
    # its own. Project 2024 resolves a date to the first exception down the
    # chain, then the first stated weekday: the base's Wed 4 holiday beats
    # Alice's own Wednesday, and her own Wed 11 exception beats the base's
    # holiday. Frame (on Alice) runs Mon 2, Tue 3 and Thu 5; Paint (Standard)
    # Fri 6; Seal (Alice) Mon 9, Tue 10 and Wed 11 until 15:00. Project keeps a
    # derived calendar's name only when it is its resource's name.
    own_wed = [("07:00:00", "15:00:00")]
    alice_exs = [("Inventory", "2026-03-11", "2026-03-11", own_wed)]
    alice = ("  <Calendar>\n    <UID>2</UID><Name>Alice</Name>"
             "<IsBaseCalendar>0</IsBaseCalendar><BaseCalendarUID>1</BaseCalendarUID>\n"
             "    <WeekDays>\n" + weekday(4, True, own_wed) + "\n"
             + "\n".join(exception_legacy(ex) for ex in alice_exs)
             + "\n    </WeekDays>\n" + exceptions(alice_exs) + "\n  </Calendar>")
    add("25-derived-calendar-holiday.xml", ["calendar", "calendar-exception",
                                            "derived-calendar", "link", "link-fs"],
        "A resource calendar derived from Standard keeps Standard's holiday on a "
        "weekday it states itself, and its own exception overrides Standard's.",
        project("derived-calendar-holiday", "\n".join([
            task(1, "Frame", 3 * D, dt(2), dt(5, "17:00:00"), **CRIT, calendar=2),
            task(2, "Paint", D, dt(6), dt(6, "17:00:00"), **CRIT, preds=[(1, FS, 0)]),
            task(3, "Seal", 3 * D, dt(9), dt(11, "15:00:00"), **CRIT, calendar=2,
                 preds=[(2, FS, 0)]),
        ]), resources_xml=crew_res, calendars=[
            standard_calendar(exs=[holiday_exs[0],
                                   ("Founders week", "2026-03-11", "2026-03-11", [])]),
            alice]))

    # 26 — lag kinds (#104): a percentage of the predecessor's duration
    # (LagFormat 19, LinkLag is the percentage), elapsed calendar time
    # (LagFormat 8, tenths of a minute), an estimated elapsed week (42) and a
    # working lag in hours (5). A (4d) finishes Thu 5 17:00 and P (6d) Mon 9
    # 17:00. The plan and every value are Project 2024's own, generated by
    # gen_mpp_lag_cases.py (l1-percent-elapsed) and checked with
    # verify_mspdi_project.py. An elapsed lag counts wall-clock time from the
    # predecessor's instant; the link then acts as a zero-lag one from there:
    # starts snap to the next working time, while a milestone (M) and an FF
    # finish (G) keep the Saturday instant itself.
    ed, ew = 1440 * 10, 7 * 1440 * 10
    add("26-lag-percent-elapsed.xml", ["link", "lag", "lag-percent", "lag-elapsed"],
        "Percentage, elapsed and estimated-elapsed lags on FS, SS, FF and SF links, "
        "forwards and backwards across a weekend, as Project 2024 schedules them.",
        project("lag-percent-elapsed", "\n".join([
            task(1, "A", 4 * D, dt(2), dt(5, "17:00:00"), **CRIT),
            task(2, "B", D, dt(10), dt(10, "17:00:00"), slack=3 * D, critical=False,
                 preds=[(1, FS, 50, 19)]),
            task(3, "C", D, dt(5), dt(5, "17:00:00"), slack=6 * D, critical=False,
                 preds=[(1, FS, -25, 19)]),
            task(4, "D", D, dt(9), dt(9, "17:00:00"), slack=4 * D, critical=False,
                 preds=[(1, FS, 2 * ed, 8)]),
            task(5, "E", D, dt(5), dt(5, "17:00:00"), slack=6 * D, critical=False,
                 preds=[(1, FS, -1 * ed, 8)]),
            task(6, "F", D, dt(9), dt(9, "17:00:00"), slack=4 * D, critical=False,
                 preds=[(1, SS, 5 * ed, 8)]),
            task(7, "G", D, dt(6), dt(7, "17:00:00"), slack=5 * D, critical=False,
                 preds=[(1, FF, 2 * ed, 8)]),
            task(8, "H", D, dt(13), dt(13, "17:00:00"), **CRIT, preds=[(1, FS, ew, 42)]),
            task(9, "P", 6 * D, dt(2), dt(9, "17:00:00"), **CRIT),
            task(10, "Q", D, dt(9), dt(9, "17:00:00"), slack=4 * D, critical=False,
                 preds=[(9, FS, -2 * ed, 8)]),
            task(11, "M", 0, dt(7, "17:00:00"), dt(7, "17:00:00"), slack=5 * D,
                 critical=False, milestone=True, preds=[(1, FS, 2 * ed, 8)]),
            task(12, "S", D, dt(3), dt(4), slack=8 * D, critical=False,
                 preds=[(1, SF, 2 * ed, 8)]),
            task(13, "W", D, dt(6, "11:00:00"), dt(9, "11:00:00"), slack=2220,
                 critical=False, preds=[(1, FS, 180 * 10, 5)]),
            task(14, "X", D, dt(13), dt(13, "17:00:00"), **CRIT, preds=[(9, SS, 150, 19)]),
        ])))

    # 27 — manually scheduled summaries (#124) keep their own dates instead
    # of rolling up. S1 (3/2-3/4) is shorter than its subtasks A and B, which
    # run on to 3/6 (Project's warning), and auto summary P rolls up through
    # it: S1's own span is fixed, so P has no slack though S1 and its
    # subtasks have 9 days. S2's manual finish (3/20) is the project finish, so every task is
    # measured to it; its start (3/9) floors unconstrained D, E's link to Z
    # pushes E past the floor, and F's must-start-on (3/3) ignores it. A
    # manual summary's Start/Finish are its ManualStart/ManualFinish, and its
    # <Duration> is the rolled-up span, as Project writes them.
    add("27-manual-summary.xml", ["manual", "summary", "manual-summary", "link", "constraint"],
        "Manual summaries keep their own dates: a short one under an auto summary, one "
        "whose finish is the project finish, a start floor a link overrides and a "
        "constrained subtask ignores.",
        project("manual-summary", "\n".join([
            task(1, "P", 5 * D, dt(2), dt(6, "17:00:00"), **CRIT, summary=True),
            task(2, "S1", 5 * D, dt(2), dt(4, "17:00:00"), slack=9 * D, critical=False,
                 outline=2, summary=True, manual=1, manual_start=dt(2),
                 manual_finish=dt(4, "17:00:00"), manual_duration=3 * D),
            task(3, "A", 2 * D, dt(2), dt(3, "17:00:00"), slack=9 * D, critical=False,
                 outline=3),
            task(4, "B", 3 * D, dt(4), dt(6, "17:00:00"), slack=9 * D, critical=False,
                 outline=3, preds=[(3, FS, 0)]),
            task(5, "C", D, dt(9), dt(9, "17:00:00"), slack=9 * D, critical=False,
                 preds=[(4, FS, 0)]),
            task(6, "S2", 10 * D, dt(9), dt(20, "17:00:00"), **CRIT, summary=True,
                 manual=1, manual_start=dt(9), manual_finish=dt(20, "17:00:00"),
                 manual_duration=10 * D),
            task(7, "D", 2 * D, dt(9), dt(10, "17:00:00"), slack=8 * D, critical=False,
                 outline=2),
            task(8, "E", D, dt(16), dt(16, "17:00:00"), slack=4 * D, critical=False,
                 outline=2, preds=[(10, FS, 0)]),
            task(9, "F", 2 * D, dt(3), dt(4, "17:00:00"), **CRIT, outline=2, ctype=MSO,
                 cdate=dt(3)),
            task(10, "Z", 10 * D, dt(2), dt(13, "17:00:00"), slack=4 * D, critical=False),
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
