#!/usr/bin/env python3
"""Keep suite/vendor/gpui_linux equal to upstream gpui_linux plus its patch (#1966).

The suite carries gpui_linux with a patch to its Wayland clipboard
(suite/vendor/README.md). The vendored tree is upstream's `crates/gpui_linux`,
at the zed rev suite/Cargo.lock pins for `gpui`, with
`suite/vendor/gpui_linux.patch` applied, and nothing else.

    python3 scripts/vendor-gpui-linux.py --check       # exit 1 on any drift
    python3 scripts/vendor-gpui-linux.py --update      # re-vendor after a gpui bump
    python3 scripts/vendor-gpui-linux.py --make-patch  # after editing the vendored copy

Upstream is read from cargo's git checkout
($CARGO_HOME/git/checkouts/zed-*/<rev[:7]>/crates/gpui_linux; CARGO_HOME
defaults to ~/.cargo), which `cargo fetch --manifest-path suite/Cargo.toml
--locked` makes. `--upstream DIR` names another copy. Needs git (for
`git apply` and `git diff --no-index`).
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / 'suite' / 'Cargo.lock'
VENDOR = ROOT / 'suite' / 'vendor' / 'gpui_linux'
PATCH = ROOT / 'suite' / 'vendor' / 'gpui_linux.patch'
ZED = 'https://github.com/zed-industries/zed'


class VendorError(Exception):
    """A problem the script reports in one line and exits 2 on."""


def zed_rev(lock_text):
    """The zed commit the lock pins for package `gpui`."""
    revs = set()
    for block in lock_text.split('[[package]]'):
        if not re.search(r'^name = "gpui"$', block, re.M):
            continue
        found = re.search(r'^source = "git\+' + re.escape(ZED) + r'(?:\?[^#"]*)?#([0-9a-f]{40})"$',
                          block, re.M)
        if found:
            revs.add(found.group(1))
    if len(revs) != 1:
        raise VendorError(f'expected one zed rev for package gpui in the lock, found {sorted(revs)}')
    return revs.pop()


def upstream_dir(rev, cargo_home):
    """cargo's checkout of `crates/gpui_linux` at `rev`."""
    hits = sorted(Path(cargo_home, 'git', 'checkouts').glob(f'zed-*/{rev[:7]}/crates/gpui_linux'))
    if not hits:
        raise VendorError(
            f'no zed checkout at {rev[:7]} under {cargo_home}/git/checkouts: run '
            '`cargo fetch --manifest-path suite/Cargo.toml --locked` first')
    return hits[0]


def copy_tree(src, dst):
    """`src` copied to `dst`, symlinks dereferenced (upstream's LICENSE-APACHE is one)."""
    shutil.copytree(src, dst, symlinks=False)


def files(tree):
    """Every file under `tree`, as paths relative to it."""
    tree = Path(tree)
    return {p.relative_to(tree).as_posix() for p in tree.rglob('*') if p.is_file()}


def tree_diff(want, have):
    """What differs between the trees `want` (upstream + patch) and `have`."""
    want_files, have_files = files(want), files(have)
    out = [f'missing: {f}' for f in sorted(want_files - have_files)]
    out += [f'extra: {f}' for f in sorted(have_files - want_files)]
    out += [f'differs: {f}' for f in sorted(want_files & have_files)
            if Path(want, f).read_bytes() != Path(have, f).read_bytes()]
    return out


def git(args, cwd, ok=(0,)):
    """Run git in `cwd`; its stdout, or a VendorError with its stderr."""
    done = subprocess.run(['git', *args], cwd=cwd, capture_output=True)
    if done.returncode not in ok:
        raise VendorError(f'git {" ".join(args)}: {done.stderr.decode(errors="replace").strip()}')
    return done.stdout


def patched(upstream, patch, work):
    """Upstream copied into `work`/tree with `patch` applied: the tree to vendor."""
    if not Path(patch).is_file():
        raise VendorError(f'no patch at {patch}')
    tree = Path(work, 'tree')
    copy_tree(upstream, tree)
    if Path(patch).read_bytes().strip():
        git(['apply', '--whitespace=nowarn', str(Path(patch).resolve())], tree)
    return tree


def make_patch(upstream, vendor):
    """The patch from `upstream` to `vendor`: unified, paths relative to the crate
    behind `a/` and `b/` (the trees are copied to directories of those names, and
    `--no-prefix` adds none of its own), so `git apply` takes it in upstream's copy.
    """
    with tempfile.TemporaryDirectory() as work:
        copy_tree(upstream, Path(work, 'a'))
        copy_tree(vendor, Path(work, 'b'))
        return git(['-c', 'core.autocrlf=false', 'diff', '--no-index', '--no-color', '--binary',
                    '--no-prefix', 'a', 'b'], work, ok=(0, 1))


def check(upstream, patch, vendor):
    """Lines saying how `vendor` differs from `upstream` + `patch` (none: it is equal)."""
    with tempfile.TemporaryDirectory() as work:
        try:
            want = patched(upstream, patch, work)
        except VendorError as err:
            return [f'patch does not apply: {err}']
        return tree_diff(want, vendor)


def update(upstream, patch, vendor):
    """Write upstream + `patch` over `vendor`."""
    with tempfile.TemporaryDirectory() as work:
        want = patched(upstream, patch, work)
        shutil.rmtree(vendor, ignore_errors=True)
        shutil.copytree(want, vendor)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--check', action='store_true')
    mode.add_argument('--update', action='store_true')
    mode.add_argument('--make-patch', action='store_true')
    parser.add_argument('--upstream', type=Path, help='upstream gpui_linux (default: cargo checkout)')
    parser.add_argument('--vendor', type=Path, default=VENDOR)
    parser.add_argument('--patch', type=Path, default=PATCH)
    parser.add_argument('--lock', type=Path, default=LOCK)
    args = parser.parse_args(argv)
    try:
        upstream = args.upstream
        if upstream is None:
            cargo_home = os.environ.get('CARGO_HOME') or Path.home() / '.cargo'
            upstream = upstream_dir(zed_rev(args.lock.read_text()), cargo_home)
        if args.check:
            problems = check(upstream, args.patch, args.vendor)
            for line in problems:
                print(f'{args.vendor}: {line}', file=sys.stderr)
            if problems:
                print('the vendored gpui_linux is not upstream + the patch: edit the vendored '
                      'copy, then --make-patch (or --update after a gpui bump)', file=sys.stderr)
                return 1
            print(f'{args.vendor}: upstream ({upstream}) + {args.patch.name}')
        elif args.update:
            update(upstream, args.patch, args.vendor)
            print(f'{args.vendor}: rewritten from {upstream} + {args.patch.name}')
        else:
            args.patch.write_bytes(make_patch(upstream, args.vendor))
            print(f'{args.patch}: written')
    except VendorError as err:
        print(f'vendor-gpui-linux: {err}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
