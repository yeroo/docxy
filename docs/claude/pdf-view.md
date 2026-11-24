# PDF view

Notes for the suite's PDF view: opening a PDF as it looks; its links,
comments and form, the only thing it writes back (#2255).

## PDF view: the renderer and its patch table (#2245)

- The view draws, reads and searches pages with PdfCraft's `pdfcraft-render`
  (storytold/pdfcraft, `MIT OR Apache-2.0`), a git dependency of
  `suite/docxy` pinned by `rev`. It is in the **suite workspace only**: hayro
  needs Rust 1.92 and PdfCraft 1.95, the root workspace's MSRV is 1.88.
- `pdfcraft-render` is built against PdfCraft's own patched copies of hayro,
  hayro-interpret, hayro-syntax, hayro-cmap, hayro-jbig2, lopdf, vello_common
  and vello_cpu (its `vendor/`), and hayro-postscript at a craft-libs rev.
  Cargo never applies a dependency's `[patch]`, so `suite/Cargo.toml`
  repeats PdfCraft's root `[patch.crates-io]` (minus its egui/winit entries)
  as git sources at the same rev. **A bump of the rev moves every entry**:
  compare the two tables, and keep lopdf's (its fuzz fixes are what the
  worker's hostile inputs need). Those crates get `opt-level = 2` in dev
  builds: unoptimized, a page draws 25-40x slower and the corpus tests crawl.
- Slice 2 (#2255) adds `-cos` (the working document, `write_incremental`),
  `-annot` (appearances for comments without one), `-forms` (fields,
  `set_value`, field appearances, password values) and `-model` (a page's
  `/Annots`, for Hidden flags), same rev; they bring `-content`, nothing to
  patch. `stream_limited` is `pub(crate)` in craft-pdf, so `incremental` in
  `info` reads the public `reconstructed`, `encryption_changed` and
  `full_save_required` only. Not used yet: `-crypt`, `-print`, `-export`.

## PDF view: the worker child (#2245)

- Nothing of a PDF is parsed in the suite's process: `suite --pdf-worker
  <file> [<ms>]` (`pdf_view/worker.rs`, dispatched at the top of `run` beside
  `convert_child`) serves one PDF, one child per view, under
  `convert_child::spawn_limited` (no console; the Windows Job's memory
  limit). One JSON line per request (`info`, `pixels {page, scale}`,
  `text {page}`) and per answer; a raster's straight-BGRA bytes follow its
  line (`len`). The child writes a hello line first and the parent skips
  anything before it: a test binary run as the child prints its own words.
- The parent (`Worker`) asks one question at a time and waits at most the
  timeout (`convert_child::TIMEOUT`); past it, or when the child dies or
  answers nonsense, the child is killed and the next question starts
  another. The page that cost it is failed with the reason and never asked
  again in that view. The child ends at the end of its stdin (the suite
  closed it or died), and by its own watchdog when one request runs past
  twice its timeout; on Linux it writes 1000 to its `oom_score_adj`.
- `Engine` runs the worker on its own thread; the view's scheduler
  (`PdfView::next_job`) sends the next question when the last is answered:
  pages showing at this zoom, their text, the page after, then the text Find
  or Ctrl+A needs. A page scrolled past or an old zoom is never asked for.
- Unit tests and `DOCXY_CONVERT_IN_PROCESS=1` serve in process over a pipe
  pair (`Spawn::InProcess`), the same `serve`. The worker's own tests run the
  test binary as the child (`worker::tests::child_entry`, misbehaving by
  `DOCXY_PDF_WORKER_TEST_MODE`).

## PDF view: links, comments and forms (#2255)

- The child draws from a **display copy** (`worker/doc.rs`), as PdfCraft's
  own app does (`pdfcraft-engine`'s `display_bytes`): the working document
  (`pdfcraft_cos::Document`, opened lazily) plus
  `with_missing_appearances_cached` and `with_field_appearances_cached`,
  written with `write_incremental`; the bytes as they are when neither adds
  anything. A fill marks it stale and it is built again before the next
  `pixels`/`text`, so ten fills build it once. hayro already draws `/AP`
  appearances, and a Highlight without one by itself; every other kind
  without `/AP` is blank but for PdfCraft's appearance.
- Protocol 2: `info` adds `links` (view space, quads too: `LinkGo::Uri`,
  `Page` with an optional point (none for `/Fit`, a bare page or `/FitH
  null`: the page's displayed top on any `/Rotate`), `/XYZ` zoom and
  `/FitH`, `Other` for a
  script, launch, layers, another file), `notes` (comments with text, not
  replies, free text or Hidden/NoView ones; `shown_rect`) and `form`
  (`FieldMeta`s with widgets in view space, `signed`, `incremental`). Every
  geometry is turned into view space in the child (`PageInfo::user_to_view`,
  `dest_fraction`). `set-field` / `set-fields` (`set_value` / `set_values`)
  answer the fields and the pages whose fields changed; `save` hands the
  bytes over after the line (`bytes`), password values cleared on a copy
  (ISO 32000-2 Table 231; `restore_password_values` after `saved`), and the
  child rebases only on `saved`. A form op runs in `catch_unwind`; a panic
  answers `fatal` and the child ends (its working copy may be half changed).
- The parent's `Worker` writes the file (`Op::SaveTo`, on the engine
  thread, `opccore::fsio::write_atomic`), then sends `saved`, follows Save
  As and clears its edit list. It keeps the fills since the last save and
  gives a new child them as one `set-fields` before anything else; a replay
  that fails in any way, or a file whose length or time changed (`Stamp`),
  drops them for good (`take_lost`, `Done::lost`: the view reads `fields`
  again, every page with a field is drawn again, `VALUES_LOST`). A save in
  place onto a changed file refuses (`CHANGED`); Save As still works.
- The view (`pdf_view/form.rs`): hit order widget, comment, link, text;
  a press on a link follows it on release unless the pointer moved past
  `DRAG_SLOP` (then the text drag selects). Only `http`, `https`, `mailto`
  open (`cx.open_url`). Form ops (`form_ops`) go before any page. Each page
  has a revision: a raster or text older than it is kept and asked again;
  a page's text read again replaces its Find hits and drops a selection on
  it. `form_rev` / `saved_rev` say unsaved; the host mirrors them into
  `DocTab.dirty` each frame (`pdf_settle_tabs`, after the active view took
  its answers in, also finishes saves; `pdf_pump_behind` pumps the tabs
  behind it), and a close or the window's close reads them at once
  (`commit_pdf_tab`). Tab order is the widgets' `tab`, one stop per widget
  (found by field and widget), a radio group one stop, read-only and hidden
  ones skipped. A field's editor starts from, and commits against, the
  value of its last fill still on its way (`expected_text`), so entering it
  again before the answer neither shows nor sends a stale value. The
  focused text field is drawn as an editor over the raster (bullets for a
  password); MaxLen refuses what does not fit. A combo box's open list shows
  `LIST_ROWS` (12) at a time, following the keys' cursor and the wheel.
  Up/Down between a multi-line field's lines is not done.
- Save (`pdf_save_step`): commits the typed text; nothing filled since the
  last read or save writes nothing (`PDF_NO_CHANGES`) but Save As writes the
  copy; a signed PDF asks `PDF_SIGNED_SAVE` first (`SIGNED_LOST_QUESTION`
  when the file would be written anew). `after` runs when the worker's
  answer comes, written or not (#2194). Save As: the native dialog, PDF
  only (`pdf_save_as_target`: `.pdf` added to a name without an extension,
  any other refused); refused in a harness (`PDF_SAVE_AS_HARNESS`), whose
  `save-as` verb queues it; `close-tab {"answer":"save"}` closes a PDF tab
  from its save's `after`. The close prompt is `PDF_SAVE_ON_CLOSE` (plain, in place). Hot
  exit cannot keep the values: with "ask before closing" off, a window close
  or Quit with a dirty PDF tab stays and opens that tab's close prompt
  (`pdf_refusal`, as #2260's extension tabs).
- Harness: `pdf-read` adds `links`, `notes`, `popup`, `popup_pinned`,
  `tooltip`, `fields`, `values` (by name), `focus`, `focus_open`,
  `unsaved`, `saving`, `signed`, `incremental`;
  regions `pdf-field:<name>[:<widget>]`, `pdf-link:<page>:<n>`,
  `pdf-annot:<page>:<n>`, `pdf-option:<n>`; input `pdf-form-field`.
  `uiharness/cases/pdf-forms.uit` drives them.

## PDF view: the tab (#2245)

- `Kind::Pdf` / `Surface::Pdf(PdfView)`. A File-only ribbon, as a Help tab.
  `pdf_view::host::pdf_view_tab` is called where a fresh document tab is
  built (`doc_tab_from_path_mode`, `tab_from_path`, session restore), never
  in `load_doc_file`: a restored `Converted::Pdf` tab, Protected View's
  rollback and Recover Text keep converting. A file named `.pdf` that does
  not sniff as one (or cannot be read) opens as a document would.
- Only form values are written (#2255, above); no other key edits. Protected View is ignored (nothing to hold back);
  `converted_pdf_tab` (Convert to editable document) opens today's
  converted tab after the view, protected when the file's zone says so. A
  second open of the PDF focuses its PDF tab (open, failed or pending), not
  its converted or recovered tab (`open_tab_index`).
- Session: a document row (`kind: "Docx"`) with `pdf_view: {page, zoom}`, so
  an older build still reads `session.json` (and converts the PDF). A row
  without it (`converted: "pdf"`) restores converted.
  A restored PDF row is `Surface::PdfPending(PdfAt)` until it is first in
  front (`finish_pending_conversion`, also run by `pdf_tick`), as a pending
  conversion is (#633): a startup starts no worker for a tab behind, and a
  pending tab persists its `PdfAt` unchanged. When it comes in front a file
  that cannot be read keeps it pending with `load_failed` and a `read
  error` (the session keeps its place; the next activation tries again,
  never each frame), and so does a worker that cannot read it; a file that
  reads but is no PDF any more opens as a document. Every way a tab comes
  in front (`select_tab`, a reopen's focus, a close, a failed New Window)
  goes through `activate_pending`. Every `Kind::Pdf` tab (open,
  failed, pending) has the File-only ribbon (`ribbon_tab_set`), the PDF
  status hint and the Save refusals.
- Find searches a page when its text comes in and splices its hits in page
  order (the current hit stays the same); only a query or option change
  searches every page again.
- Each frame (`Docxy::pdf_tick`, on whichever window holds the tab, so a
  torn-off tab goes on drawing) frees the GPU images views let go of
  (`cx.drop_image`; the raster cache's LRU budget and a dropped view queue
  them), lays the view out from the body's last bounds, takes the answers in
  and sends the next question; a poll runs only while an answer is due.
- Rasters are capped at 4096 px a side and 16 MP (the page is drawn
  stretched past them; tiles are a later slice). Pages stay white in the
  dark theme. 100 % is the printed size (96/72 px a point).
- The Find box and page box are one-line fields on `field_edit::field_key`
  (#2116), registered inputs (`pdf-find`, `pdf-page-box`) typed into by
  `uiharness/cases/pdf-view.uit`. The outline pane is drawn as the
  Navigation pane (`pane:navigation`).
- Harness: `pdf-read` (pages and their state, outline, selection, Find,
  `busy`) asks again for `until_drawn`, `until_text` (page numbers) or
  `until_idle`, up to 5 s. `pdf-<part>` is any of the view's probes
  (`pdf-zoom-in`, `pdf-bookmark:<n>`, `pdf-convert`, `pdf-find-case`, …) and
  `pdf-text:<page>:<start|end>:<text>` a glyph, for `pointer-click` and
  `pointer-drag`.

## PDF view: the corpus and its tolerance (#2245)

- `corpus/pdf-view/` (README there): generated PDFs and `pdftoppm -r 72`
  references, made by `scripts/make-pdf-view-corpus.py` (pycairo for Unicode,
  pictures and bookmarks; Ghostscript with embedded fonts for CMYK and
  `/Rotate`). Tests never run the tools.
- The reference test compares 3 x 3 pixel blocks' means (a block differs
  when a channel differs by more than 48; at most 1 % of blocks may), and
  checks a blank page fails against every reference. Single pixels do not
  separate a good render from a blank one: anti-aliasing and fractional
  text positions differ between hayro and poppler on every edge. The
  measured numbers are in the test's comment; raise the bound only with new
  measurements there.
- `worker::tests::no_corpus_file_or_mutation_crashes_or_hangs_the_parent`
  runs every corpus file and 21 seeded mutations of each (truncated, bytes
  flipped, a span zeroed) through a real child: `info`, then three pages'
  pixels and text, each an answer or a failure with a reason, in time; a
  file with a form also gets a `set-field` and a `save` (#2255).
- Slice 2's files (#2255) are written object by object by the generator's
  `RawPdf` (deterministic): `links`, `annots` (every kind with `/AP`; its
  reference is in the reference test), `annots-noap` (the same without, on a
  blank page), `forms`, `signed` (a placeholder signature). Run
  `python3 scripts/make-pdf-view-corpus.py links annots annots-noap forms
  signed` to write only them; the slice 1 files are not written byte for
  byte again by cairo and gs.
