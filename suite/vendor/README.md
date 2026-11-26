# Vendored crates

## gpui_linux (#1966)

`gpui_linux/` is gpui's Linux platform crate, `crates/gpui_linux` of
[zed](https://github.com/zed-industries/zed) at the commit `suite/Cargo.lock`
pins for `gpui`, with `gpui_linux.patch` applied, and nothing else.
`suite/Cargo.toml`'s `[patch."https://github.com/zed-industries/zed"]` points
gpui (through `gpui_platform`) and the suite at it.

**Why.** gpui's Wayland clipboard offers and reads text (and images) only, so
a copy from the suite could offer other apps no RTF or HTML, and a paste could
not read theirs. Nothing outside gpui can add them: a compositor keeps one
data device per client (mutter's `get_data_device` unlinks the client's older
one, so a second device on gpui's connection would take the selection and
drag-and-drop events away from gpui's), a second connection gets no selection
events and has no input serial for `set_selection`, and GNOME has no
data-control protocol. The patch keeps gpui's own data device, serial and
event loop, and adds `gpui_linux::wayland_clipboard` (`add_formats`,
`mime_types`, `read`), which `suite/docxy/src/rich_clip.rs` uses
(`docs/claude/clipboard.md`, #1966).

**What the patch changes.** The manifest's zed workspace keys are made
explicit (it builds as a path dependency here), and the Wayland clipboard
(`src/linux/wayland/clipboard.rs`, `client.rs`, and the new
`clipboard/formats.rs` and `client/rich_clipboard.rs`) gains the formats beside
a copy's text, its source kept until the compositor cancels it, and a read of
gpui's own copy while that source is alive. A copy's images are written as
their own mime on both backends (#2282): Wayland's source offers them (and the
text mimes only with text), and X11 (`src/linux/x11/clipboard.rs`, `client.rs`
and the new `clipboard/tests.rs`) serves them, leaves a payload past one
request out, and opens its connections on a given display for the tests. Each
modified file says so on its first line, except `src/linux/x11/clipboard.rs`,
which says so right after its licence header (kept first); there the header's
closing `*/` is also indented one space: rustfmt on Windows rewrites a
column-0 `*/`, and `cargo fmt --all` reaches this crate.

**Licence.** Apache-2.0, as upstream: `gpui_linux/LICENSE-APACHE` is
upstream's licence file (a symlink there, a copy here).

**Checking it.** `python3 scripts/vendor-gpui-linux.py --check` (CI runs it)
fails when the tree is not upstream + the patch. It reads upstream from cargo's
zed checkout, so run `cargo fetch --manifest-path suite/Cargo.toml --locked`
first.

**Changing the patch.** Edit `gpui_linux/`, then
`python3 scripts/vendor-gpui-linux.py --make-patch`, and commit both.

**Bumping gpui.** `cargo update --manifest-path suite/Cargo.toml -p gpui`
moves the zed commit, then `python3 scripts/vendor-gpui-linux.py --update`
re-vendors upstream + the patch at it. When a hunk no longer applies, copy
upstream over `gpui_linux/` (`--update` with an empty patch file), redo the
change by hand, then `--make-patch`. Make the manifest's keys match the new
zed root `Cargo.toml`, and check that `suite/Cargo.lock`'s other packages
did not move.
