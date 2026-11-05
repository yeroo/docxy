# Clipboard with other apps

Notes moved from `CLAUDE.md` (#1903). Read them before changing rich (RTF/HTML) copy and paste between docxy and other apps, documents and sheets. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Rich copy and paste with other apps (#1074)

- A document copy puts RTF beside its plain text (`Clip::to_rtf` in
  `docxcore/src/clip_rtf.rs`, the Save As writer with the tab's styles), added
  after gpui's text write by `suite/docxy/src/rich_clip.rs`: macOS
  `public.rtf`, Windows "Rich Text Format", nothing on Linux. It is written
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
  nothing on Linux), under the same `rtf_follows` read-back gate as the RTF. On
  a Windows OS clipboard the text ends records with CRLF (`os_text`); the
  recorded text and the harness's private clipboard stay LF.
- A sheet paste takes the live grid clip, else another app's HTML table, else
  its RTF table (`foreign_table`: cell texts only, each read as typed through
  `fields_block`), else the text as TSV. A table counts only when it is the
  whole copy (no other text, one table), and docxy's own table (marked
  `data-source=docxy`) never does: its exact TSV, with the `'` a text cell like
  `007` needs, is what pastes. Paste Special still reads the text.
