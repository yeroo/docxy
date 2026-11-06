# Clipboard with other apps

Notes moved from `CLAUDE.md` (#1903). Read them before changing rich (RTF/HTML) copy and paste between docxy and other apps, documents and sheets. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Rich copy and paste with other apps (#1074)

- A document copy puts RTF beside its plain text (`Clip::to_rtf` in
  `docxcore/src/clip_rtf.rs`, the Save As writer with the tab's styles), added
  after gpui's text write by `suite/docxy/src/rich_clip.rs`: macOS
  `public.rtf`, Windows "Rich Text Format", X11 `text/rtf` (#1950), nothing
  on Wayland. It is written
  only when the text write took (`rtf_follows`). A document paste takes the
  window's own clip while the clipboard still holds its text, else the
  clipboard's RTF, else its text (`doc_paste_clip`). A partial copy's RTF ends
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
  X11 `text/html` (#1950), nothing on Wayland), under the same `rtf_follows` read-back gate as the RTF. On
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
- Wayland (#1966) and headless do nothing, as before.
- A document paste takes our clip, else the RTF, else the HTML, else the text
  (`doc_paste_from`, on every platform): a browser's copy has HTML and no
  RTF. `Clip::from_html` (`docxcore/src/clip_html.rs`) maps the HTML import
  as `from_rtf` does. The HTML is read only when neither our clip nor the
  RTF pastes. Paste Special offers an HTML source as "HTML Format". A sheet
  copy pasted into a document now takes its table's display text, tab-joined
  (no `'` the TSV gives a text cell like `007`). Lists are not kept: a `Clip`
  has no paragraph properties (#1967). The harness reports the caret's
  hyperlink target as `run.link`.
