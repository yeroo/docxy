"""Write corpus/mpx/variants/: MPX shapes the MSPDI corpus does not produce (#309).

MPXJ 16.8.0 (as gen_mpx_corpus.py) writes each from a corpus file with one
change, so the reader is tested on MPXJ's own spelling:

- month-units.mpx: 06-lag with B's duration 1 month and its lag 1 elapsed
  month (MPXJ writes `1mon`, `1FS+1emon`).
- weekday-dates.mpx: 06-lag with the date format EEE_DD_MM_YY (record 12
  field 8 = 9; dates written `Mon 03/02/26`).
- multiline-note.mpx: 02-link-fs with A's notes on two lines (MPXJ writes
  the line break as 0x7F).

    python corpus/tools/gen_mpx_variants.py
"""
import os

import jpype
import mpxj  # noqa: F401

jpype.startJVM("-Duser.language=en", "-Duser.country=US")

from org.mpxj import DateOrder, Duration, ProjectDateFormat, Relation, RelationType, TimeUnit  # noqa: E402
from org.mpxj.mpx import MPXWriter  # noqa: E402
from org.mpxj.reader import UniversalProjectReader  # noqa: E402

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
dst = os.path.join(root, "mpx", "variants")
os.makedirs(dst, exist_ok=True)


def load(name):
    project = UniversalProjectReader().read(os.path.join(root, "mspdi", name))
    project.getProjectProperties().setDateOrder(DateOrder.MDY)
    return project


def write(project, name):
    writer = MPXWriter()
    writer.setUseLocaleDefaults(False)
    writer.write(project, os.path.join(dst, name))
    print(name)


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
