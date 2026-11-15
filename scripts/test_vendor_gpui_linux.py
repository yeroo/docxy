"""Tests for scripts/vendor-gpui-linux.py on temporary trees (no cargo, no network).

    python3 -m unittest discover -s scripts -p 'test_*.py' -v
"""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location('vendor_gpui_linux', HERE / 'vendor-gpui-linux.py')
vendor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(vendor)

REV = '82766871481bfc126f8fd42a3b9f51afc9b2decd'
LOCK = f'''
[[package]]
name = "gpui"
version = "0.2.2"
source = "git+https://github.com/zed-industries/zed#{REV}"

[[package]]
name = "gpui_linux"
version = "0.1.0"

[[package]]
name = "gpui_macos"
version = "0.1.0"
source = "git+https://github.com/zed-industries/zed#{'0' * 40}"
'''


def write(root, files):
    for name, text in files.items():
        path = Path(root, name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


class VendorGpuiLinux(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.upstream = root / 'upstream'
        self.vendor = root / 'vendor'
        self.patch = root / 'gpui_linux.patch'
        write(self.upstream, {
            'Cargo.toml': '[package]\nname = "gpui_linux"\nedition.workspace = true\n',
            'src/lib.rs': 'mod a;\n',
            'src/a.rs': 'pub fn a() {}\n',
            'LICENSE-APACHE': 'Apache\n',
        })
        # The vendored copy: upstream, edited, with a new file.
        vendor.copy_tree(self.upstream, self.vendor)
        write(self.vendor, {
            'Cargo.toml': '[package]\nname = "gpui_linux"\nedition = "2024"\n',
            'src/lib.rs': '// Modified.\n\nmod a;\npub mod b;\n',
            'src/b.rs': 'pub fn b() {}\n',
        })
        self.patch.write_bytes(vendor.make_patch(self.upstream, self.vendor))

    def check(self):
        return vendor.check(self.upstream, self.patch, self.vendor)

    def run_main(self, *args):
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            code = vendor.main([*args, '--upstream', str(self.upstream), '--vendor',
                                str(self.vendor), '--patch', str(self.patch)])
        return code, err.getvalue()

    def test_the_rev_is_gpuis(self):
        self.assertEqual(vendor.zed_rev(LOCK), REV)
        with self.assertRaises(vendor.VendorError):
            vendor.zed_rev('[[package]]\nname = "gpui"\nversion = "0.2.2"\n')

    def test_a_missing_checkout_says_to_fetch(self):
        with self.assertRaisesRegex(vendor.VendorError, 'cargo fetch'):
            vendor.upstream_dir(REV, self.tmp.name)
        found = Path(self.tmp.name, 'git', 'checkouts', 'zed-abc', REV[:7], 'crates', 'gpui_linux')
        found.mkdir(parents=True)
        self.assertEqual(vendor.upstream_dir(REV, self.tmp.name), found)

    def test_a_clean_tree_checks(self):
        self.assertEqual(self.check(), [])
        self.assertEqual(self.run_main('--check')[0], 0)

    def test_an_edit_without_the_patch_is_named(self):
        write(self.vendor, {'src/a.rs': 'pub fn a() { /* drift */ }\n'})
        self.assertEqual(self.check(), ['differs: src/a.rs'])
        code, err = self.run_main('--check')
        self.assertEqual(code, 1)
        self.assertIn('differs: src/a.rs', err)

    def test_a_missing_or_extra_file_is_named(self):
        os.remove(self.vendor / 'src' / 'b.rs')
        write(self.vendor, {'src/c.rs': '\n'})
        self.assertEqual(self.check(), ['missing: src/b.rs', 'extra: src/c.rs'])

    def test_a_patch_that_does_not_apply_fails(self):
        write(self.upstream, {'src/lib.rs': 'mod changed_upstream;\n'})
        problems = self.check()
        self.assertEqual(len(problems), 1)
        self.assertIn('patch does not apply', problems[0])

    def test_make_patch_then_check_round_trips(self):
        write(self.vendor, {'src/a.rs': 'pub fn a() { /* ours */ }\n'})
        self.assertNotEqual(self.check(), [])
        self.assertEqual(self.run_main('--make-patch')[0], 0)
        self.assertEqual(self.check(), [])

    def test_update_rewrites_the_vendored_tree(self):
        write(self.vendor, {'src/a.rs': 'drift\n', 'src/stray.rs': '\n'})
        self.assertEqual(self.run_main('--update')[0], 0)
        self.assertEqual(self.check(), [])
        self.assertEqual((self.vendor / 'src' / 'b.rs').read_text(), 'pub fn b() {}\n')


if __name__ == '__main__':
    unittest.main()
