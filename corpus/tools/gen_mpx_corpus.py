"""Write corpus/mpx/*.mpx: MPXJ's MPX for each corpus/mspdi/*.xml (#309).

The MPX files are the oracle of projcore's MPX reader: each must read to
the same schedule as its MSPDI twin. MPXJ (Apache-2.0) writes them, as the
issue's reproduction did; Project itself no longer saves MPX.

    pip install mpxj==16.8.0 JPype1    # needs a Java runtime (JAVA_HOME)
    python corpus/tools/gen_mpx_corpus.py [MDY|DMY|YMD]

`order` is the date order the files are written in (record 12): MDY
(the default, as US Project writes) to corpus/mpx/, DMY or YMD to
corpus/mpx/<order lower-case>/.
"""
import os
import sys

import jpype
import mpxj  # noqa: F401  (puts the MPXJ jars on the class path)

order = (sys.argv[1] if len(sys.argv) > 1 else "MDY").upper()
jpype.startJVM("-Duser.language=en", "-Duser.country=US")

from org.mpxj.mpx import MPXWriter  # noqa: E402
from org.mpxj.reader import UniversalProjectReader  # noqa: E402
from org.mpxj import DateOrder  # noqa: E402

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
src = os.path.join(root, "mspdi")
dst = os.path.join(root, "mpx")
if order != "MDY":
    dst = os.path.join(dst, order.lower())
os.makedirs(dst, exist_ok=True)
for name in sorted(os.listdir(src)):
    if not name.endswith(".xml"):
        continue
    project = UniversalProjectReader().read(os.path.join(src, name))
    project.getProjectProperties().setDateOrder(DateOrder.valueOf(order))
    writer = MPXWriter()
    writer.setUseLocaleDefaults(False)  # write the project's date order
    writer.write(project, os.path.join(dst, name[:-4] + ".mpx"))
    print(name)
