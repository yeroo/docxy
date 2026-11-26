# Clipboard with other apps

Notes moved from `CLAUDE.md` (#1903). Read them before changing rich (RTF/HTML) copy and paste between docxy and other apps, documents and sheets. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Rich copy and paste with other apps (#1074)

- A document copy puts RTF beside its plain text (`Clip::to_rtf` in
  `docxcore/src/clip_rtf.rs`, the Save As writer with the tab's styles), added
  after gpui's text write by `suite/docxy/src/rich_clip.rs`: macOS
  `public.rtf`, Windows "Rich Text Format", X11 `text/rtf` (#1950), Wayland
  `text/rtf` (#1966). It is written
  only when the text write took (`rtf_follows`). A document paste takes the
  window's own clip while the clipboard still holds its text, else the
  clipboard's RTF, else its HTML (#1950), else its text (`doc_paste_from`). A
  partial copy's RTF ends
  without `\par`; the paste takes the trailing mark from the plain text.
  Font colour and highlight cross both ways: highlight goes out as
  `\highlightN` (Word) and `\cbN` (Cocoa), and comes in as the nearest Word
  name (`docxcore::highlight`, the one name → RGB table); white is none.
- The harness's private clipboard holds the RTF beside the text, and any
  plain write drops it. `clipboard {"action":"write","rtf":…}` plays another
  app's rich copy; `read` reports `rtf` and `rtf_bold`.

## Sheet copy and paste with other apps (#1248)

- A sheet copy or cut puts an HTML table (`gridcore::clip_html::html_table`:
  display text and inline cell styles, never formulas) beside its TSV, through
  `rich_clip::write_html` (macOS `public.html`, Windows "HTML Format" as CF_HTML,
  X11 `text/html` (#1950), Wayland `text/html` (#1966)), under the same `rtf_follows` read-back gate as the RTF. On
  a Windows OS clipboard the text ends records with CRLF (`os_text`); the
  recorded text and the harness's private clipboard stay LF.
- A sheet paste takes the live grid clip, else another app's HTML table, else
  its RTF table (`foreign_table`: cell texts only, each read as typed through
  `fields_block`), else the text as TSV. A table counts only when it is the
  whole copy (no other text, one table), and docxy's own table (marked
  `data-source=docxy`) never does: its exact TSV, with the `'` a text cell like
  `007` needs, is what pastes. Paste Special still reads the text.

## HTML and RTF on Linux, and a document paste of HTML (#1950)

- On X11 (`gpui::guess_compositor() == "X11"`) `rich_clip/x11.rs` owns the
  `CLIPBOARD` selection on the suite's own x11rb connection: gpui's X11
  clipboard is private to it and knows text and images only. An RTF (or
  HTML) write reads the selection's `UTF8_STRING` back first and takes
  ownership only while it is the copy's text (`same_text`); gpui then drops
  its item and reads through the server like any other app. The suite serves
  `TARGETS`, `SAVE_TARGETS`, gpui's text targets (`STRING` as Latin-1) and
  its extras, and refuses the rest. gpui's next copy takes the selection
  back. A payload past one request (`maximum_request_bytes`) is not offered:
  no `INCR` sending. Reads ask `TARGETS` first (`text/rtf`, `application/rtf`,
  `text/richtext`; `text/html` or `text/html;…`) and follow `INCR`.
- Two connections: the owner's only owns and serves, on its own thread; each
  read opens its own (gpui's way), so their events never share a queue.
  Ownership is always `GetSelectionOwner`, never a flag: a `SelectionClear`
  comes in late. The held data is cleared on `SelectionClear` only while the
  server says another client owns it, under the same lock a write takes.
- At quit (`on_app_quit`) `rich_clip::hand_over` gives the suite's copy to the
  clipboard manager (`SAVE_TARGETS`, 100 ms at most), as gpui does for its
  own: once the suite owns the selection, gpui's handover has nothing to
  hand. A manager that takes `CLIPBOARD` itself after a copy (CopyQ, Klipper
  with sync) keeps only the formats it copied.
- The X11 tests each start their own `Xvfb -displayfd` (never `DISPLAY`, and
  never one display for two tests); with no Xvfb they skip with a note,
  except under `CI`, where they fail. The app's global clipboard is never
  opened by a test.
- Wayland is #1966's (below); headless does nothing.
- A document paste takes our clip, else the RTF, else the HTML, else the text
  (`doc_paste_from`, on every platform): a browser's copy has HTML and no
  RTF. `Clip::from_html` (`docxcore/src/clip_html.rs`) maps the HTML import
  as `from_rtf` does, from the clipboard's text as it is (`import_html_str_within`):
  no charset is read from it, since an Office page's windows-1252 meta (or a
  `charset=` in its words) says nothing about these UTF-8 characters; X11's
  UTF-16 page is converted where it is read. An RTF or HTML with no text
  beside it is never our clip (`doc_clip_ours`'s `rich`). The HTML is read only when neither our clip nor the
  RTF pastes. Paste Special offers an HTML source as "HTML Format". A sheet
  copy pasted into a document now takes its table's display text, tab-joined
  (no `'` the TSV gives a text cell like `007`). Lists are kept (#1967,
  below). The harness reports the caret's hyperlink target as `run.link`.

## Lists in another app's paste (#1967)

- A `Clip` carries each paragraph's list membership beside its inlines
  (`Clip::list(i)`: `ClipList { num_id, ilvl }`), filled only by `clip_of`
  (`from_rtf`, `from_html`) from the importers' `num_id`/`ilvl`: 1 bullets,
  2 decimal, the suite's `NUM_BULLET`/`NUM_DECIMAL`, which the paste maps
  to the document's own lists (`numbering_for_paste`, below, #2087). The
  level is clamped to 0..=8 (the HTML importer's nesting is not). `lists`
  stays canonical (`set_lists` drops trailing `None`s), so a clip with no
  list equals `Clip::new`'s, which every other clip is built with: our own
  copy, text and internal clips paste as before. A copy writes no lists
  (`to_rtf`).
- The paste (`paste_at_caret`) follows Word's rule that a paragraph's
  formatting rides on its mark: each piece but the last brings its mark and
  its list; the last joins the text after the caret and is a list item only
  when nothing is there (editor length 0, a picture or field counts). So
  one item pasted into a sentence is text, and a browser's list (no newline
  after its last item) pasted into an empty paragraph keeps its last item.
  A piece with no list leaves the props as before (the target's, in the
  middle). The list is set untracked, as `set_list` is: while tracking, a
  Reject of the paste leaves the list on the caret's paragraph (a gap).
- A paste with lists puts its items in the package's own lists
  (`doc_paste::numbering_for_paste`, #2087 r3): the importers' 1
  (bullets) and 2 (decimal) become `ListIds`' (1 and 2 where the numbering
  part is docxy's own, else docxy's reserved 9990 / 9991, never a Word
  document's own 1 or 2, #2180, `docs/claude/docx-editing.md` §Lists), and
  each kind pasted is defined (`ListIds::define`: a package without a
  numbering part gets `add_markdown_numbering`'s): `Docxy::do_paste` for a
  tab that `saves_as_word`, and the ext DocView's `paste_clip`. A tab with no package gets it from
  `blank_base` at its save, which sees every list item: a table cell's,
  a nested table's and a text box's (in a paragraph, a tracked change or
  a link) included (`names_a_list`, #2181). A `Block::Raw`'s are not.
- Only list membership crosses: an HTML item's indent and marker look, and
  other paragraph formatting, do not. The RTF importer reads `\pnlvlbody`
  / `\pnlvlblt` outside a group only, not in Word 95's `{\*\pn …}`.

## A test X client's last request (#2008)

- An X client in a test that sends a request (above all a `SendEvent`) and
  then drops its connection makes a checked request (or one with a reply)
  first: the server may close a client that hung up before it reads what
  that client last sent, so a flush alone is not enough. The test clipboard
  manager's `SelectionNotify` was lost that way about one run in five
  (`at_quit_our_copy_goes_to_the_clipboard_manager`); it now ends on
  `send_event(…).check()`, as `uiharness`'s `request_close` does. `Peer`
  ends on a `DestroyNotify` it waits for, and a `Reader` read that succeeds
  on a reply; one it gives up on leaves only requests on its own window,
  which nobody waits for.

## HTML and RTF on Wayland: the patched gpui_linux (#1966)

- Under Wayland (`gpui::guess_compositor() == "Wayland"`) `rich_clip`'s Linux
  `imp` goes through `gpui_linux::wayland_clipboard`, which the suite's own
  copy of gpui_linux adds (`suite/vendor/gpui_linux`: upstream at the zed rev
  the lock pins plus `suite/vendor/gpui_linux.patch`; `suite/vendor/README.md`
  says why and how to bump). Nothing outside gpui can do it: mutter keeps one
  data device per client (a second one on gpui's connection takes gpui's
  selection and drag-and-drop events), a second connection gets no selection
  events and no serial, and GNOME has no data-control.
- A copy: gpui writes its text as before; `add_formats(text, formats)` then
  sets the selection again, with gpui's serial, to a source offering the text
  mimes, `text/rtf` (or `text/html`, `html_page`) and gpui's `pid/<pid>`, but
  only while gpui's own source is still the selection (not `cancelled`) and
  holds `text`. gpui serves the new source (a format's bytes for its mime, the
  text for anything else); its next copy drops the formats. Nothing flushes
  between the two `set_selection`s, so the compositor (and a data-control
  clipboard manager) never sees the text-only source alive.
- gpui's `read_from_clipboard` now returns its own copy while that source is
  alive. Before, a read right after a write saw the previous offer (the
  compositor's `selection` echo is not dispatched yet): after another app's
  copy, `rtf_follows` failed and `recorded_after_write` recorded the other
  app's text. `Clipboard::set` forgets the source (a write skipped for want of
  a serial leaves none), `set_source` follows `set_selection`, a `cancelled`
  forgets it only when it is the tracked one, and so does a `selection` that
  is not gpui's own (none, or no `pid/<pid>`): mutter refuses an older serial
  without a `cancelled`. Such a refused write answers as gpui's own until the
  next `selection` event. The formats stay until gpui's next copy, for an own
  offer (`pid/`) that follows.
- A paste: `mime_types()` (gpui's own copy's while alive, else the offer's),
  `pick_mime` (`RTF_TYPES` in order, the same names X11's `read_rtf` lists
  as its own atoms; `text/html`, else a `text/html;…` by `is_html_type`,
  which X11 shares, as it does `utf8_page`), then `read(mime, MAX_RTF)`: gpui's
  own copy from memory (never a pipe: gpui serves pipes on the waiting
  thread), another app's through the offer's pipe, bounded, waiting up to
  gpui's 4 s as its text paste does. HTML through `utf8_page`. `hand_over`
  does nothing on Wayland.
- All three calls do nothing (`false` / `None`) off the main thread, on X11
  or headless, or while gpui's state is borrowed.
- Tests: the patch's logic (`clipboard/formats.rs`: formats, source
  tracking, who answers a read, the bounded pipe read) and `pick_mime`; no
  end-to-end Wayland test (a headless compositor gives no keyboard focus, so
  `set_selection` is refused). CI runs `scripts/vendor-gpui-linux.py --check`
  and `cargo test -p gpui_linux --lib wayland::` (a path dependency outside
  the suite's workspace, which `exclude`s it; the vendored manifest has an
  empty `[workspace]`, so `cargo fmt --all` does not climb to the root one).

## Images on the Linux clipboard (#2282)

- gpui's `write_to_clipboard` of an item with a `ClipboardEntry::Image`
  serves each image as its own mime (`ImageFormat::mime_type`), in entry
  order, the first of each mime kept, in the vendored gpui_linux
  (`suite/vendor/README.md`). The text mimes come only with text, or when
  there is no image: an item of images alone offers no text (another app's
  paste gets the image, not an empty string). A text item, and one with
  neither text nor image (an empty `UTF8_STRING`), are written as before.
  PRIMARY stays text only.
- X11 (`x11/clipboard.rs`): `Clipboard::set_item` writes `item_data`'s list:
  `UTF8_STRING` (`TARGETS` adds its two `text/plain;charset=` names) and the
  images' atoms. `set_image` stores its own format, not always PNG. A
  payload past one `ChangeProperty` (`maximum_request_bytes` less 1024, as
  `rich_clip/x11.rs`) is left out of `TARGETS` and refused: sending it would
  end `serve_requests` and every later copy with it. No `INCR` sending.
- Wayland (`wayland/clipboard.rs`): `Clipboard::set` merges the images into
  `OwnCopy`'s formats, so `send`, `mime_types`, `read_format` and
  `add_formats`' merge serve them like RTF; `own_mime_types` leaves the text
  mimes out for an item of images alone (`offers_text`), and gpui's write
  offers that list.
- Tests: X11's `x11/clipboard/tests.rs` against a reader on a private Xvfb
  (`Inner::new(Some(display))`, the `Clipboard` built on it with no third
  owner, so its `Drop` does nothing); Wayland's `formats.rs` rules. The two
  clients' write calls are untested: neither client builds headless.
