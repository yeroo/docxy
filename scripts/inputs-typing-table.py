#!/usr/bin/env python3
"""Print the inputs-typing cases' results as a table (#1029).

    python3 scripts/inputs-typing-table.py <sweep run dir | transcript ...>

A sweep run dir (scripts/ui-linux-sweep.py --run) holds one
`<script>/transcript.txt` per script; the `inputs-typing-*` ones are read, or
the transcripts named. Each case `<surface> <dialog>/<field>: <step>` becomes a
row: surface, dialog, field, step, pass or fail. A dialog's own
`matches the catalogue` case has no field. Exits 1 when any row failed.
"""
import importlib.util
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location('ui_linux_sweep', HERE / 'ui-linux-sweep.py')
sweep = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sweep)

NAME_RE = re.compile(
    r'^(?:(?P<theme>dark|light) theme )?(?P<surface>\S+) (?P<dialog>[^/:]+)'
    r'(?:/(?P<field>[^:]+))?: (?P<step>.+)$'
)
HEAD = ('surface', 'dialog', 'field', 'step', 'result')


def rows(text):
    """The table rows for one transcript's cases, in its order."""
    out = []
    for name, run in sweep.parse_transcript(text).cases.items():
        m = NAME_RE.match(name)
        if not m:
            continue
        surface = m['surface'] if not m['theme'] else f"{m['surface']} ({m['theme']})"
        out.append((surface, m['dialog'], m['field'] or '', m['step'],
                    'pass' if run.ok else 'FAIL'))
    return out


def transcripts(args):
    for arg in map(Path, args):
        if arg.is_dir():
            yield from sorted(arg.glob('inputs-typing-*/transcript.txt'))
        else:
            yield arg


def table(all_rows):
    widths = [max(len(r[i]) for r in [HEAD, *all_rows]) for i in range(len(HEAD))]
    line = lambda r: ' | '.join(c.ljust(w) for c, w in zip(r, widths)).rstrip()
    return '\n'.join([line(HEAD), '-+-'.join('-' * w for w in widths),
                      *map(line, all_rows)])


def main(argv):
    if not argv:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    all_rows = [r for t in transcripts(argv)
                for r in rows(t.read_text(encoding='utf-8', errors='replace'))]
    if not all_rows:
        print('no inputs-typing cases found', file=sys.stderr)
        return 2
    print(table(all_rows))
    failed = sum(r[4] == 'FAIL' for r in all_rows)
    print(f'\n{len(all_rows)} steps: {len(all_rows) - failed} passed, {failed} failed')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
