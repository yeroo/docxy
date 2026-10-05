"""End-to-end tests for scripts/ui-linux.py against fake X tools.

The wrapper is run as a subprocess with fake Xvfb/Openbox/xprop on PATH, so no
real X server is needed. The fakes satisfy the contracts ui-linux.py relies on:
Xvfb writes a decimal display number to the -displayfd fd, and xprop answers the
_NET_SUPPORTING_WM_CHECK readiness probe.

    python3 -m unittest discover -s scripts -p 'test_*.py' -v
"""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

HERE = Path(__file__).resolve().parent
WRAPPER = HERE / 'ui-linux.py'
DEADLINE = 10.0

XVFB = """\
import os
import sys
import time
from pathlib import Path

FAKE = Path(os.environ['FAKE_DIR'])
fd = int(sys.argv[sys.argv.index('-displayfd') + 1])
pid = FAKE / 'xvfb.pid.tmp'
pid.write_text(str(os.getpid()))
os.replace(pid, FAKE / 'xvfb.pid')
os.write(fd, b'99\\n')
os.close(fd)
while True:
    time.sleep(3600)
"""

OPENBOX = """\
import os
import time
from pathlib import Path

FAKE = Path(os.environ['FAKE_DIR'])
pid = FAKE / 'wm.pid.tmp'
pid.write_text(str(os.getpid()))
os.replace(pid, FAKE / 'wm.pid')
while True:
    time.sleep(3600)
"""

XPROP = """\
print('_NET_SUPPORTING_WM_CHECK(WINDOW): window id # 0x1')
"""

# Keeps running after SIGTERM so stop() takes its 3 s wait: the window in
# which the second signal lands during teardown.
CHILD_STUBBORN = """\
import os
import signal
import time
from pathlib import Path

FAKE = Path(os.environ['FAKE_DIR'])

def on_term(signum, frame):
    tmp = FAKE / 'child.term.tmp'
    tmp.write_text('term')
    os.replace(tmp, FAKE / 'child.term')

signal.signal(signal.SIGTERM, on_term)
pid = FAKE / 'child.pid.tmp'
pid.write_text(str(os.getpid()))
os.replace(pid, FAKE / 'child.pid')
while True:
    time.sleep(3600)
"""

CHILD_EXIT7 = """\
import os
from pathlib import Path

FAKE = Path(os.environ['FAKE_DIR'])
pid = FAKE / 'child.pid.tmp'
pid.write_text(str(os.getpid()))
os.replace(pid, FAKE / 'child.pid')
raise SystemExit(7)
"""


@unittest.skipUnless(sys.platform.startswith('linux'), 'ui-linux.py is Linux-only')
class Teardown(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        tmp = Path(self._tmp.name)
        self.fake = tmp / 'fake'
        self.fake.mkdir()
        self.bin = tmp / 'bin'
        self.bin.mkdir()
        self.logs = tmp / 'logs'
        self._pids = []
        self.wrapper = None
        self._write_tool('fake-xvfb', XVFB)
        self._write_tool('fake-openbox', OPENBOX)
        self._write_tool('xprop', XPROP)
        env = os.environ.copy()
        env['PATH'] = str(self.bin) + os.pathsep + env['PATH']
        env['FAKE_DIR'] = str(self.fake)
        self.env = env

    def _write_tool(self, name, body):
        path = self.bin / name
        path.write_text(f'#!{sys.executable}\n' + textwrap.dedent(body))
        path.chmod(0o755)

    def _write_child(self, name, body):
        path = self.fake / name
        path.write_text(textwrap.dedent(body))
        return path

    def _start(self, child):
        self.wrapper = subprocess.Popen(
            [sys.executable, str(WRAPPER), '--xvfb', str(self.bin / 'fake-xvfb'),
             '--wm', str(self.bin / 'fake-openbox'), '--logs', str(self.logs),
             '--', sys.executable, str(child)],
            env=self.env, start_new_session=True,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return self.wrapper

    def _pid(self, name):
        path = self.fake / name
        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            if path.exists():
                pid = int(path.read_text().strip())
                self._pids.append(pid)
                return pid
            time.sleep(0.05)
        self.fail(f'{name} never appeared')

    def _wait_for(self, predicate, what):
        deadline = time.monotonic() + DEADLINE
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.05)
        self.fail(f'timed out waiting for {what}')

    def _gone(self, pid):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        except PermissionError:
            return False
        stat = Path(f'/proc/{pid}/stat')
        return stat.exists() and stat.read_text().split()[2] == 'Z'

    def _assert_all_gone(self):
        for pid in self._pids:
            self._wait_for(lambda p=pid: self._gone(p), f'pid {pid} to exit')

    def tearDown(self):
        if self.wrapper is not None and self.wrapper.poll() is None:
            self.wrapper.kill()
            self.wrapper.wait(timeout=5)
        pids = set(self._pids)
        for path in self.fake.glob('*.pid'):
            try:
                pids.add(int(path.read_text().strip()))
            except (ValueError, OSError):
                pass
        for pid in pids:
            if self._gone(pid):
                continue
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass

    def test_second_signal_during_teardown_still_stops_everything(self):
        wrapper = self._start(self._write_child('child.py', CHILD_STUBBORN))
        self._pid('xvfb.pid')
        self._pid('wm.pid')
        self._pid('child.pid')
        os.kill(wrapper.pid, signal.SIGINT)
        # child.term proves the wrapper is inside stop(child)'s 3 s wait.
        self._wait_for(lambda: (self.fake / 'child.term').exists(), 'child.term')
        os.kill(wrapper.pid, signal.SIGTERM)
        wrapper.wait(timeout=20)
        self._assert_all_gone()

    def test_single_sigterm_stops_everything(self):
        wrapper = self._start(self._write_child('child.py', CHILD_STUBBORN))
        self._pid('xvfb.pid')
        self._pid('wm.pid')
        self._pid('child.pid')
        os.kill(wrapper.pid, signal.SIGTERM)
        self.assertEqual(wrapper.wait(timeout=20), 130)
        self._assert_all_gone()

    def test_returns_child_exit_code(self):
        wrapper = self._start(self._write_child('child.py', CHILD_EXIT7))
        self._pid('xvfb.pid')
        self._pid('wm.pid')
        self._pid('child.pid')
        self.assertEqual(wrapper.wait(timeout=20), 7)
        self._assert_all_gone()


if __name__ == '__main__':
    unittest.main()
