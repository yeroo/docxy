"""Write corpus/mpx/variants/: MPX shapes the MSPDI corpus does not produce (#309).

MPXJ 16.8.0 (as gen_mpx_corpus.py) writes each from a corpus file with one
change, so the reader is tested on MPXJ's own spelling:

- month-units.mpx: 06-lag with B's duration 1 month and its lag 1 elapsed
  month (MPXJ writes `1mon`, `1FS+1emon`).
- weekday-dates.mpx: 06-lag with the date format EEE_DD_MM_YY (record 12
  field 8 = 9; dates written `Mon 03/02/26`).
- multiline-note.mpx: 02-link-fs with A's notes on two lines (MPXJ writes
  the line break as 0x7F).
- month-names-*.mpx (#1805): 09-constraint-snet in the date formats that
  spell the month: `full` DD_MMMMM_YYYY (3, MDY, `March 05 2026`),
  `full-time` DD_MMMMM_YYYY_HH_MM (2, MDY, `March 05 2026 08:00 am`),
  `short` DD_MMM_YY (5, DMY, `05 Mar '26`), `weekday` EEE_DD_MMM_YY (10, YMD,
  `Thu '26 Mar 05`).
- locale-<l>.mpx (#1805): one plan written by MPXJ in each locale its MPX
  writer knows but Chinese (en de fr it pt sv es ru), with the locale's own
  defaults (words, units, link types, date format, separators, code page):
  20-task-fields with Inspect linked SS+1d from Excavate and FF+1ed from
  Pour, held by a Start No Earlier Than constraint, and a resource Crew
  (Accrue At End, $50.00/h) assigned to Excavate. Each localised file is read
  back with MPXJ's reader in its locale and must give the English file's
  tasks, or the script fails.

    python corpus/tools/gen_mpx_variants.py
"""
import os

import jpype
import mpxj  # noqa: F401

jpype.startJVM("-Duser.language=en", "-Duser.country=US")

from java.time import LocalDateTime  # noqa: E402
from java.util import Locale  # noqa: E402
from org.mpxj import (  # noqa: E402
    AccrueType,
    CostRateTableEntry,
    ConstraintType,
    DateOrder,
    Duration,
    ProjectDateFormat,
    Rate,
    Relation,
    RelationType,
    TimeUnit,
)
from org.mpxj.mpx import MPXReader, MPXWriter  # noqa: E402
from org.mpxj.reader import UniversalProjectReader  # noqa: E402

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
dst = os.path.join(root, "mpx", "variants")
os.makedirs(dst, exist_ok=True)


def load(name):
    project = UniversalProjectReader().read(os.path.join(root, "mspdi", name))
    project.getProjectProperties().setDateOrder(DateOrder.MDY)
    return project


def write(project, name, locale=None):
    writer = MPXWriter()
    if locale is None:
        writer.setUseLocaleDefaults(False)
    else:
        writer.setLocale(Locale(locale))
        writer.setUseLocaleDefaults(True)
    writer.write(project, os.path.join(dst, name))
    print(name)


def link(task, predecessor, kind, lag):
    task.addPredecessor(
        Relation.Builder().predecessorTask(predecessor).successorTask(task).type(kind).lag(lag)
    )


project = load("06-lag.xml")
a, b = project.getTaskByID(jpype.JInt(1)), project.getTaskByID(jpype.JInt(2))
b.setDuration(Duration.getInstance(1, TimeUnit.MONTHS))
old = b.getPredecessors().get(0)
b.removePredecessor(old.getPredecessorTask(), old.getType(), old.getLag())
b.addPredecessor(
    Relation.Builder()
    .predecessorTask(a)
    .successorTask(b)
    .type(RelationType.FINISH_START)
    .lag(Duration.getInstance(1, TimeUnit.ELAPSED_MONTHS))
)
write(project, "month-units.mpx")

project = load("06-lag.xml")
project.getProjectProperties().setDateFormat(ProjectDateFormat.EEE_DD_MM_YY)
write(project, "weekday-dates.mpx")

project = load("02-link-fs.xml")
project.getTaskByID(jpype.JInt(1)).setNotes("Pour slab\nCheck rebar")
write(project, "multiline-note.mpx")

for fmt, order, name in [
    (ProjectDateFormat.DD_MMMMM_YYYY, DateOrder.MDY, "full"),
    (ProjectDateFormat.DD_MMMMM_YYYY_HH_MM, DateOrder.MDY, "full-time"),
    (ProjectDateFormat.DD_MMM_YY, DateOrder.DMY, "short"),
    (ProjectDateFormat.EEE_DD_MMM_YY, DateOrder.YMD, "weekday"),
]:
    project = load("09-constraint-snet.xml")
    project.getProjectProperties().setDateFormat(fmt)
    project.getProjectProperties().setDateOrder(order)
    write(project, f"month-names-{name}.mpx")


def locale_plan():
    project = load("20-task-fields.xml")
    excavate = project.getTaskByID(jpype.JInt(2))
    pour = project.getTaskByID(jpype.JInt(4))
    inspect = project.getTaskByID(jpype.JInt(5))
    link(inspect, excavate, RelationType.START_START, Duration.getInstance(1, TimeUnit.DAYS))
    link(inspect, pour, RelationType.FINISH_FINISH, Duration.getInstance(1, TimeUnit.ELAPSED_DAYS))
    inspect.setConstraintType(ConstraintType.START_NO_EARLIER_THAN)
    inspect.setConstraintDate(LocalDateTime.of(2026, 3, 4, 8, 0))
    crew = project.addResource()
    crew.setName("Crew")
    crew.setAccrueAt(AccrueType.END)
    table = crew.getCostRateTable(0)
    table.clear()
    rates = jpype.JArray(Rate)([Rate(50.0, TimeUnit.HOURS), Rate(0.0, TimeUnit.HOURS)])
    table.add(CostRateTableEntry(LocalDateTime.MIN, LocalDateTime.MAX, jpype.JInt(0), rates))
    crew.setID(jpype.JInt(1))
    crew.setUniqueID(jpype.JInt(1))
    assignment = excavate.addResourceAssignment(crew)
    assignment.setUnits(jpype.JDouble(100.0))
    assignment.setWork(Duration.getInstance(16, TimeUnit.HOURS))
    return project


def tasks_of(project):
    return [
        (
            str(t.getName()),
            str(t.getDuration()),
            str(t.getConstraintType()),
            str(t.getPriority()),
            [(int(r.getPredecessorTask().getID()), str(r.getType()), str(r.getLag()))
             for r in t.getPredecessors()],
        )
        for t in project.getTasks()
    ]


def read_back(name, locale):
    reader = MPXReader()
    reader.setLocale(Locale(locale))
    return tasks_of(reader.read(os.path.join(dst, name)))


for locale in ["en", "de", "fr", "it", "pt", "sv", "es", "ru"]:
    write(locale_plan(), f"locale-{locale}.mpx", locale)
english = read_back("locale-en.mpx", "en")
for locale in ["de", "fr", "it", "pt", "sv", "es", "ru"]:
    got = read_back(f"locale-{locale}.mpx", locale)
    if got != english:
        raise SystemExit(f"locale-{locale}.mpx: MPXJ reads {got}, not {english}")
