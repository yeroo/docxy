#!/usr/bin/env python3
"""Run every uiharness case script on Linux, each in a fresh suite instance.

Each script gets its own private Xvfb + Openbox display (scripts/ui-linux.py)
and its own `uiharness run`, so no script inherits another's tabs or view
settings. Results are checked against uiharness/cases/expected-failures.txt:
the run fails on a failure that is not listed and on a listed case that passes.

    python3 scripts/ui-linux-sweep.py                      # every script
    python3 scripts/ui-linux-sweep.py uiharness/cases/doc-state.uit

Each script's transcript, X11 logs and harness evidence land in
<run>/<script>/; the table is also written to <run>/summary.txt and
<run>/summary.json.
"""
import argparse
from dataclasses import dataclass, field
import datetime
import json
from pathlib import Path
import re
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parent.parent
CASES = REPO / 'uiharness' / 'cases'
EXPECTED = CASES / 'expected-failures.txt'
UI_LINUX = REPO / 'scripts' / 'ui-linux.py'

# Statuses, best first. Only PASS and XFAIL leave the exit code at 0.
PASS, XFAIL, XPASS, FAIL, ERROR = 'PASS', 'XFAIL', 'XPASS', 'FAIL', 'ERROR'
KINDS = ('fail', 'error')


class ListError(Exception):
    """The expected-failures list is malformed or stale."""


def strip_comment(line):
    """Drop a `#` comment outside double quotes, as uiharness's script parser does.

    The parser keeps a `#` that starts a border colour, but only on `assert
    border` lines; a `test` line never has one.
    """
    in_quote = False
    for i, ch in enumerate(line):
        if ch == '"':
            in_quote = not in_quote
        elif ch == '#' and not in_quote:
            return line[:i]
    return line


def script_cases(text):
    """The case names a .uit script declares, in order (`test <name>` lines)."""
    names = []
    for raw in text.splitlines():
        # Split on any whitespace, as the parser's split_word does.
        words = strip_comment(raw).split(None, 1)
        if len(words) == 2 and words[0].lower() == 'test':
            names.append(words[1].strip())
    return names


@dataclass(frozen=True)
class Entry:
    script: str
    case: str
    issue: str
    kind: str
    line: int


def parse_expected(text):
    """Parse `<script>.uit | <case name> | #<issue> [| error]` lines."""
    entries = []
    for n, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith('#'):
            continue
        fields = [f.strip() for f in line.split('|')]
        if len(fields) not in (3, 4):
            raise ListError(f'line {n}: expected "<script>.uit | <case> | #<issue> [| error]", '
                            f'got {raw!r}')
        script, case, issue = fields[:3]
        kind = fields[3] if len(fields) == 4 else 'fail'
        if not script.endswith('.uit') or '/' in script:
            raise ListError(f'line {n}: {script!r} is not a script file name like doc-rulers.uit')
        if not case:
            raise ListError(f'line {n}: the case name is empty')
        if not re.fullmatch(r'#\d+', issue):
            raise ListError(f'line {n}: {issue!r} is not a tracking issue like #808')
        if kind not in KINDS:
            raise ListError(f'line {n}: kind {kind!r} is not one of {", ".join(KINDS)}')
        entries.append(Entry(script, case, issue, kind, n))
    return entries


def validate_expected(entries, cases_dir):
    """Every entry must name a script in cases_dir and a case it declares, once."""
    errors = []
    seen = set()
    for e in entries:
        key = (e.script, e.case)
        if key in seen:
            errors.append(f'line {e.line}: {e.script} | {e.case} is listed twice')
        seen.add(key)
        path = cases_dir / e.script
        if not path.is_file():
            errors.append(f'line {e.line}: no script {path}')
        elif e.case not in script_cases(path.read_text(encoding='utf-8')):
            errors.append(f'line {e.line}: {e.script} has no case {e.case!r}')
    if errors:
        raise ListError('\n'.join(errors))


@dataclass
class CaseRun:
    ok: bool
    fail_steps: int = 0
    error_steps: int = 0


@dataclass
class Transcript:
    cases: dict = field(default_factory=dict)
    duplicates: list = field(default_factory=list)
    # (total, passed, failed) from `N cases — P passed, F failed`, if present.
    summary: tuple = None


# A failing run's report goes to stderr behind `uiharness: `, which lands on
# its first line, `suite:`; case and summary lines are unprefixed either way.
CASE_RE = re.compile(r'^case: (.*) — (ok|FAILED)$')
STEP_RE = re.compile(r'^  (ok|FAIL|ERROR) ')
SUMMARY_RE = re.compile(r'^(\d+) cases? — (\d+) passed, (\d+) failed$')


def parse_transcript(text):
    """Read case results out of `uiharness run` output (runner.rs `report`).

    The suite's own output is inherited and may interleave, so lines that are
    none of the report's are skipped rather than trusted or rejected.
    """
    t = Transcript()
    current = None
    for line in text.splitlines():
        line = line.rstrip('\r')
        m = CASE_RE.match(line)
        if m:
            name = m.group(1)
            if name in t.cases:
                t.duplicates.append(name)
            current = t.cases[name] = CaseRun(ok=m.group(2) == 'ok')
            continue
        m = SUMMARY_RE.match(line)
        if m:
            t.summary = tuple(int(g) for g in m.groups())
            current = None
            continue
        m = STEP_RE.match(line)
        if m and current is not None:
            if m.group(1) == 'FAIL':
                current.fail_steps += 1
            elif m.group(1) == 'ERROR':
                current.error_steps += 1
    return t


@dataclass
class Result:
    script: str
    status: str
    passed: int = 0
    failed: int = 0
    failed_cases: list = field(default_factory=list)
    expected: list = field(default_factory=list)
    new_failures: list = field(default_factory=list)
    unexpected_pass: list = field(default_factory=list)
    reason: str = ''
    exit_code: int = None
    seconds: float = 0.0
    dir: str = ''

    def note(self):
        if self.reason:
            return self.reason
        parts = []
        if self.new_failures:
            parts.append('NEW: ' + '; '.join(self.new_failures))
        if self.unexpected_pass:
            parts.append('UNEXPECTED PASS: ' + '; '.join(self.unexpected_pass))
        if self.expected:
            parts.append('expected: ' + '; '.join(self.expected))
        return ' | '.join(parts)


def classify(script, transcript, exit_code, declared, expected, timed_out=False):
    """Decide a script's status.

    declared: the case names the script itself declares.
    expected: {case name: Entry} for this script.
    """
    r = Result(script, ERROR, exit_code=exit_code)

    def error(reason):
        r.reason = reason
        return r

    if timed_out:
        return error('timed out')
    t = transcript
    if not t.cases:
        return error(f'no case results (exit {exit_code}); see transcript')
    if t.duplicates:
        return error('cases reported twice: ' + '; '.join(t.duplicates))
    if t.summary is None:
        return error('no summary line; the report is incomplete')
    r.passed = sum(c.ok for c in t.cases.values())
    r.failed = len(t.cases) - r.passed
    if t.summary != (len(t.cases), r.passed, r.failed):
        return error(f'summary {t.summary} does not match the {len(t.cases)} case lines')
    if set(t.cases) != set(declared):
        missing = sorted(set(declared) - set(t.cases))
        extra = sorted(set(t.cases) - set(declared))
        return error(f'reported cases differ from the script: missing {missing}, extra {extra}')
    if exit_code != 0 and r.failed == 0:
        return error(f'exit {exit_code} although every case passed')
    if exit_code == 0 and r.failed:
        return error('exit 0 although a case failed')

    for name in declared:
        c = t.cases[name]
        entry = expected.get(name)
        if c.ok:
            if entry:
                r.unexpected_pass.append(f'{name} ({entry.issue})')
            continue
        r.failed_cases.append(name)
        if entry is None:
            r.new_failures.append(name)
        elif entry.kind == 'fail' and (c.error_steps or not c.fail_steps):
            # The entry covers an expectation mismatch; a step that could not
            # run at all is a different, unlisted problem.
            r.new_failures.append(f'{name} (step error)')
        elif entry.kind == 'error' and not c.error_steps:
            r.new_failures.append(f'{name} (failed without the listed step error)')
        else:
            r.expected.append(f'{name} ({entry.issue})')
    if r.new_failures:
        r.status = FAIL
    elif r.unexpected_pass:
        r.status = XPASS
    elif r.expected:
        r.status = XFAIL
    else:
        r.status = PASS
    return r


def table(results):
    rows = [('script', 'cases', 'pass', 'fail', 'status', 'secs', 'note')]
    for r in results:
        rows.append((r.script, str(r.passed + r.failed), str(r.passed), str(r.failed),
                     r.status, f'{r.seconds:.0f}', r.note()))
    widths = [max(len(row[i]) for row in rows) for i in range(len(rows[0]) - 1)]
    lines = []
    for row in rows:
        cells = [c.rjust(w) if 1 <= i <= 3 or i == 5 else c.ljust(w)
                 for i, (c, w) in enumerate(zip(row, widths))]
        lines.append('  '.join(cells + [row[-1]]).rstrip())
    counts = {s: sum(r.status == s for r in results) for s in (PASS, XFAIL, FAIL, XPASS, ERROR)}
    lines.append('')
    lines.append(f'{len(results)} script{"" if len(results) == 1 else "s"}: '
                 f'{counts[PASS]} pass, {counts[XFAIL]} xfail, {counts[FAIL]} fail, '
                 f'{counts[XPASS]} xpass, {counts[ERROR]} error')
    return '\n'.join(lines) + '\n'


def run_one(cmd, transcript_path, timeout):
    """Run cmd with its output in transcript_path; return (exit code, timed out).

    ui-linux.py stays in our process group, so a Ctrl-C reaches it directly,
    and turns SIGTERM into stopping its command, Openbox and Xvfb.
    """
    with transcript_path.open('wb') as out:
        proc = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, cwd=REPO)
        try:
            return proc.wait(timeout=timeout), False
        except subprocess.TimeoutExpired:
            stop(proc)
            return proc.returncode, True
        except KeyboardInterrupt:
            # A terminal Ctrl-C reached ui-linux.py too, and it is already
            # tearing down; it ignores further SIGTERM/SIGINT until that is
            # done (#985), so wait for it before trying SIGTERM.
            stop(proc, signalled=True)
            raise


def stop(proc, signalled=False):
    """Stop ui-linux.py, letting its SIGTERM/SIGINT teardown run where it can."""
    sends = [proc.terminate]
    if signalled:
        # Already signalled: first wait, and only then send SIGTERM.
        sends.insert(0, lambda: None)
    for send in sends:
        send()
        try:
            proc.wait(timeout=15)
            return
        except subprocess.TimeoutExpired:
            pass
    # Last resort: SIGKILL skips ui-linux.py's teardown and may leave its
    # Xvfb and Openbox running.
    proc.kill()
    proc.wait()


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('scripts', nargs='*', type=Path,
                        help='scripts to run (default: every uiharness/cases/*.uit)')
    parser.add_argument('--suite', type=Path, default=REPO / 'suite/target/release/suite')
    parser.add_argument('--uiharness', type=Path, default=REPO / 'target/release/uiharness')
    parser.add_argument('--run', type=Path,
                        help='run directory (default: uiharness-runs/sweep-<timestamp>)')
    parser.add_argument('--expected', type=Path, default=EXPECTED)
    parser.add_argument('--timeout', type=float, default=600,
                        help='seconds per script (default: 600)')
    args = parser.parse_args(argv)

    # #975: uiharness resolves relative paths inconsistently, so hand it none.
    suite = args.suite.resolve()
    uiharness = args.uiharness.resolve()
    for exe, build in ((suite, 'cargo build --release --manifest-path suite/Cargo.toml'),
                       (uiharness, 'cargo build --release -p uiharness')):
        if not exe.is_file():
            parser.error(f'{exe} is missing; build it with: {build}')

    # The list is checked against every script, even on a subset run, so a
    # stale entry is caught whichever scripts someone happens to run.
    try:
        entries = parse_expected(args.expected.read_text(encoding='utf-8'))
        validate_expected(entries, CASES)
    except (OSError, ListError) as exc:
        parser.error(f'{args.expected}:\n{exc}')

    scripts = [p.resolve() for p in args.scripts] or sorted(CASES.glob('*.uit'))
    declared = {}
    for path in scripts:
        if path.suffix != '.uit' or not path.is_file():
            parser.error(f'{path} is not a .uit script')
        if path.stem in {p.stem for p in declared}:
            parser.error(f'two scripts are named {path.stem}; their run directories would clash')
        declared[path] = script_cases(path.read_text(encoding='utf-8'))

    stamp = datetime.datetime.now().strftime('%Y%m%d-%H%M%S')
    run = (args.run or REPO / 'uiharness-runs' / f'sweep-{stamp}').resolve()
    if run.exists() and any(run.iterdir()):
        parser.error(f'{run} is not empty; pass a new --run directory')
    run.mkdir(parents=True, exist_ok=True)

    results = []
    interrupted = False
    start = time.monotonic()
    for i, path in enumerate(scripts, 1):
        out = run / path.stem
        out.mkdir()
        print(f'[{i}/{len(scripts)}] {path.stem} ... ', end='', flush=True)
        cmd = [sys.executable, str(UI_LINUX), '--logs', str(out / 'x11'), '--',
               str(uiharness), 'run', str(path), '--suite', str(suite),
               '--run', str(out / 'harness')]
        began = time.monotonic()
        try:
            code, timed_out = run_one(cmd, out / 'transcript.txt', args.timeout)
        except KeyboardInterrupt:
            # Keep the scripts that finished: their summary is still written.
            print('interrupted')
            interrupted = True
            break
        mine = {e.case: e for e in entries if CASES / e.script == path}
        text = (out / 'transcript.txt').read_text(encoding='utf-8', errors='replace')
        r = classify(path.name, parse_transcript(text), code, declared[path], mine, timed_out)
        r.seconds = time.monotonic() - began
        r.dir = str(out)
        results.append(r)
        print(f'{r.status} {r.seconds:.0f}s' + (f'  {r.note()}' if r.status != PASS else ''),
              flush=True)

    summary = table(results)
    (run / 'summary.txt').write_text(summary, encoding='utf-8')
    (run / 'summary.json').write_text(json.dumps([{
        'script': r.script, 'status': r.status, 'passed': r.passed, 'failed': r.failed,
        'failed_cases': r.failed_cases, 'expected': r.expected,
        'new_failures': r.new_failures, 'unexpected_pass': r.unexpected_pass,
        'reason': r.reason, 'exit_code': r.exit_code, 'seconds': round(r.seconds, 1),
        'dir': r.dir,
    } for r in results], indent=2) + '\n', encoding='utf-8')
    print()
    print(summary, end='')
    print(f'wall time {time.monotonic() - start:.0f}s; run directory: {run}')
    if interrupted:
        return 130
    return 0 if all(r.status in (PASS, XFAIL) for r in results) else 1


if __name__ == '__main__':
    sys.exit(main())
