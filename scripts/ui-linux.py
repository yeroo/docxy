#!/usr/bin/env python3
"""Run a command on a private X11 display, leaving the user's desktop alone."""
import argparse
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def stop(process):
    if process is None:
        return
    # The command may have exited but left its suite child running.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def stop_all(processes):
    """Stop each (name, process) in turn; one failure must not orphan the rest."""
    for name, process in processes:
        try:
            stop(process)
        except OSError as exc:
            print(f'ui-linux: could not stop {name}: {exc}', file=sys.stderr, flush=True)


def ignore_signals():
    """Ignore SIGTERM/SIGINT, even if one arrives while the old handler is still armed."""
    while True:
        try:
            for sig in (signal.SIGTERM, signal.SIGINT):
                signal.signal(sig, signal.SIG_IGN)
            return
        except KeyboardInterrupt:
            pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--xvfb', default='Xvfb')
    parser.add_argument('--wm', default='openbox')
    parser.add_argument('--screen', default='1600x1000x24')
    parser.add_argument('--logs', type=Path)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ['--']:
        command = command[1:]
    if not command:
        parser.error('supply a command after --')
    def locate(tool):
        found = shutil.which(tool)
        if found:
            return found
        local = Path.home() / '.local/bin' / tool
        if '/' not in tool and local.is_file() and os.access(local, os.X_OK):
            return str(local)
        parser.error(f'{tool} is missing; install xvfb, openbox and x11-utils')

    args.xvfb = locate(args.xvfb)
    args.wm = locate(args.wm)
    xprop = locate('xprop')
    logs = args.logs or Path(tempfile.mkdtemp(prefix='docxy-x11-'))
    logs.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.pop('WAYLAND_DISPLAY', None)
    env.pop('XAUTHORITY', None)
    env['XDG_SESSION_TYPE'] = 'x11'
    xvfb = wm = child = None
    read_fd, write_fd = os.pipe()

    def interrupted(signum, _frame):
        raise KeyboardInterrupt

    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, interrupted)
    try:
        with (logs / 'xvfb.log').open('wb') as xlog, (logs / 'openbox.log').open('wb') as wlog:
            xvfb = subprocess.Popen(
                [args.xvfb, '-displayfd', str(write_fd), '-screen', '0',
                 args.screen, '-nolisten', 'tcp', '-ac'],
                pass_fds=(write_fd,), stdout=xlog, stderr=subprocess.STDOUT,
                start_new_session=True, env=env)
            os.close(write_fd)
            write_fd = None
            ready, _, _ = select.select([read_fd], [], [], 10)
            display = os.read(read_fd, 128).decode().strip() if ready else ''
            if not display.isdecimal() or xvfb.poll() is not None:
                raise RuntimeError('Xvfb did not start')
            env['DISPLAY'] = ':' + display
            wm = subprocess.Popen([args.wm, '--sm-disable'], env=env,
                                  stdout=wlog, stderr=subprocess.STDOUT,
                                  start_new_session=True)
            deadline = time.monotonic() + 10
            while True:
                if wm.poll() is not None:
                    raise RuntimeError('Openbox exited before becoming ready')
                prop = subprocess.run([xprop, '-root', '_NET_SUPPORTING_WM_CHECK'],
                                      env=env, capture_output=True, text=True, timeout=2)
                if prop.returncode == 0 and 'window id # 0x' in prop.stdout:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError('Openbox did not register as the window manager')
                time.sleep(0.05)
            print(f'Private X11 display {env["DISPLAY"]}; logs: {logs}', flush=True)
            child = subprocess.Popen(command, env=env, start_new_session=True)
            code = child.wait()
            return code if code >= 0 else 128 - code
    except KeyboardInterrupt:
        return 130
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
        print(f'ui-linux: {exc}; see {logs}', file=sys.stderr)
        return 1
    finally:
        # Teardown must finish: a second SIGTERM/SIGINT here would abort it
        # and orphan Openbox and Xvfb.
        ignore_signals()
        stop_all((('command', child), ('openbox', wm), ('xvfb', xvfb)))
        os.close(read_fd)
        if write_fd is not None:
            os.close(write_fd)


if __name__ == '__main__':
    sys.exit(main())
