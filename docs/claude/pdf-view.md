# PDF view

Notes for the suite's PDF view: opening a PDF as it looks; its links,
comments and form, the only thing it writes back (#2255); encrypted,
updated and damaged files (#2256); converting it to an editable document
with its layout (#2257); printing and exporting it as drawn (#2258).

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
  `full_save_required` only. Not used yet: `-crypt` (`-print`: #2258).
- Slice 4 (#2257) adds `-edit`, `-create` and `-export`, same rev, for the
  conversion; no patch entry (their craft-libs crates are git
  dependencies, PdfCraft's root patches crates.io only). `-edit`'s
  `icu_segmenter` 2.3 moved the lock's icu family from 2.2 to 2.4 (gpui's
  `url` shares it); `-create` brings `jpeg-encoder` (IJG, credited in
  NOTICE).

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

## PDF view: encrypted, incremental and damaged files (#2256)

- Protocol 3: `unlock {password}` (`Op::Unlock`, `Worker::unlock`); `info`
  or `unlock` answers `locked: needs|wrong` (`Answer::Locked`) only for
  PdfCraft's `NeedsPassword`/`WrongPassword`; anything else (an
  unsupported handler) is a failure with its reason, never the prompt.
  `DocMeta.security` (`SecurityMeta`: cipher, owner, the permissions) for
  an encrypted file.
- The child opens the working copy first with the typed password (user or
  owner), then gives the renderer `recovered_user_password()` when there is
  one (`doc.rs` `render_password`): hayro authenticates only the user
  password of revisions 2-4 (RC4, AES-128), so an owner password typed for
  one of those is refused by `inspect` without it (PdfCraft's engine does
  the same, `render_password_for`). The display copy is written with the
  file's own encryption, so the renderer takes the same password for it.
  `saved` reopens with `open_with_same_credentials` (the key, no password).
- `Secret` (`zeroize::Zeroizing<String>`): `Debug` is `Secret(..)`, cleared
  on drop; the request line holding it is cleared after its write. It goes
  on stdin only. The parent holds it only in that view's `Worker`, which
  sends `unlock` first in `connect` to every new child, before the form
  replay (so the clean child after a failed replay is unlocked too).
  The prompt's typed text moves into the `Secret` on a try, is cleared when
  the `PdfLock` drops, is never cloned to draw (bullets from its length),
  and a pasted line is cleared after its insert. Not cleared: the freed
  buffers typing leaves behind as the field's `String` grows
  (`field_edit`'s inserts reallocate), the clipboard's own copy, and
  PdfCraft's copies (`LoadOptions`, `RenderConfig`'s `Arc<str>`).
- Permissions: shown (status `; restricted: printing, copying, changes`,
  `pdf-read` `permissions`), not enforced, but for filling: a file whose
  password does not allow `fill_forms` has every field read-only, and the
  child refuses `set-field(s)` (`NO_FILLING`) whatever sends it.
- The view: a locked file is a `Surface::Pdf` whose `PdfView::lock`
  (`PdfLock`: the `Worker`, the masked `Field`, `wrong`) is set and that
  has no pages and no `Engine` (not a `Surface` of its own: every PDF arm
  already handles it). Its body is the prompt (`pdf-password`, `pdf-unlock`
  probes); `try_unlock` is synchronous, as the open is; the field is
  emptied each try. `pdf_tick` skips a locked view, so `restore_page` and
  `zoom` (the session's `PdfAt`) wait for the pages; Save refuses
  (`PDF_NOT_OPEN`); the harness's `open` takes a locked view as opened.
  `field-read {"input":"pdf-password"}` says only `focused` and `empty`.
- On Linux the child's `run` (never `serve`, the in-process path) sets
  `RLIMIT_DATA` to `MEMORY_LIMIT` and `RLIMIT_CORE` to 0 (`limit_memory`):
  an allocation past it aborts the child, and the parent fails the page.
  The `limits` test child reads them back from `/proc/self/limits`.
- A failed PDF tab draws "This PDF can't be opened" and its reason
  (`pdf_failed_reason`: the status line, so Save and Save As refusals,
  `note_save_refusal`, leave a failed PDF tab's status alone; `main.rs`
  `pdf_failed_body`); `pdf-read` gives it as `failed`. A try the reader
  cannot answer leaves the tab `PdfPending` with `load_failed` where the
  view was (`unlock_tab`), as a restored tab whose file cannot be read. The
  harness's `open` refuses a failed load by design, so no case opens
  `damaged-unreadable.pdf`.
- PdfCraft finds the objects of a file whose xref offsets are only wrong
  without a rebuild (no repair log): its save appends. One with no xref or
  trailer is rebuilt from its object headers and saved in full.
- Corpus: `enc-*`, `incremental`, `objstm`, `damaged-*` (README there);
  every one but `damaged-unreadable` has a reference, the damaged ones drawn
  from the file before its damage. The mutation test adds them (encrypted
  ones unlocked) and `structural` mutations; `a_flate_bomb_answers_in_time`
  makes its bombs in the test (`zero_bomb`).

## PDF view: convert to an editable document with the layout (#2257)

- `pdf_view/convert.rs`, run by the converting child's `What::Pdf`
  (`convert_child::convert_bytes`; the child sets the worker's
  `limit_memory` for a PDF): `read_pages` builds `pdfcraft_export::Page`s
  as `pdfcraft-engine`'s `export_pages_within` does (the engine crate is
  not used: it brings JS, OCR and signing), copying the engine's
  `pub(crate)` `weight_and_slant` and `display_unit`; pictures through
  one `ImageBudget`, within `docxcore::import::html_image`'s
  `MAX_IMAGE_BYTES`/`MAX_MEDIA_BYTES` (the suite loads the result).
  `docx_with_language` writes the document; it is loaded with
  `load_package` and becomes `ConvertedDoc { doc, base: Some(pkg) }`.
- `convert_with` runs that step in `catch_unwind`: an error, a panic or a
  file that keeps nothing (no text, no picture: a blank PDF stays a `load
  error`) goes to the text importer (`import_pdf`, #951), whose encrypted
  message covers a file that needs its password (the convert child has
  none; an owner-only file converts). Tests inject the step.
- Time and memory (r1, r2): the layout step's `Limits` from its start.
  Reading (after `inspect`, each page, each picture, before the export)
  stops at `READ_TIME` (8 s); the export and its loading are checked after
  each against `DONE_TIME` (12 s); more than `MAX_LAYOUT_BLOCKS` (20,000)
  text blocks stops it at once. Each gives an `Err` and the text importer
  runs in the same child. Measured in release on this laptop: 300 pages of
  text read in 0.5 s, 600 in 1.6 s (reading grows faster than the pages;
  the export, load and save of those under 0.2 s); 300 pages of table
  cells (86,700 blocks) export in 0.3 s, load in 7.0 s, save in 0.6 s,
  about 0.1 ms a block, so the cap keeps that to about 2.3 s; the text
  importer took 0.2 s on each. Not bounded: one page or one `inspect` that
  alone runs past the 20 s (PdfCraft's own decode budgets bound them), and
  the work after `DONE_TIME`'s last check (the passes and the child's
  save, about 0.1 s for 20,000 blocks). Then the parent kills the child,
  and the tab says it took too long; that is not retried, as the suite
  waits on its UI thread. An abort (an allocation past `limit_memory`'s
  `RLIMIT_DATA`, which `catch_unwind` cannot reach) kills the child: the
  parent (`convert_child::convert`, `retrying`) tries a PDF whose child
  died (`DIED`) once more as `What::PdfText` (`--convert-import
  pdf-text`, `import_pdf` alone, no limit set), within what is left of
  the same `TIMEOUT` (none under `RETRY_MIN`, 1 s).
- Running headers and footers (`convert/running.rs`), on the pages before
  the export: an unrotated block wholly in the top or bottom 12 % whose
  text, with digit runs as `#`, repeats on at least 2 and more than half
  the pages, middles within 6 pt and an edge (left, centre, right) within
  12 pt, and each digit run the same on every page, each page's number or
  the page count (a date cell, an amount or a count not starting at 1
  stays in the body). Removed from every page; the first page's pieces
  become a header and a footer part (Header/Footer style, centre and
  right tab stops of the text width, hand-written run XML in schema
  order), referenced from
  the first section only; digit runs that are each page's number are
  `PAGE`, the page count `NUMPAGES`. No running piece on page 1 of 3 or
  more pages: `w:titlePg` with empty first-page parts. `pgMar
  w:header/w:footer` from the pieces' positions.
- Columns (`convert/columns.rs`): PdfCraft flows every page as one column,
  and takes lines side by side at shared baselines for a table (its
  `tables()` clusters short blocks' left edges). So a page in 2 or 3
  equal columns (gutters at least `max(12 pt, body)` in the middle 80 %;
  each band a fifth of the text height and half filled; no rule in the
  region) is split before the export into **virtual pages**: the band
  above (the real page's size), then the region with its columns stacked
  on a page whose PdfCraft text area (`min(w/8, 1 in)` margins) is one
  column wide. One section per virtual page comes out; the region's gets
  `w:cols`, a continuous start and the real page's `pgSz`/`pgMar` back.
  Where a column begins is its highest block or picture; a block above
  where every column has begun is lifted into the band (a heading over
  the first column; from another column only when the band's own text is
  over it), never a column's last block.
  The export sizes headings against each virtual page, so a band's
  paragraphs get their level again against the real page's body
  (`heading_level`, PdfCraft's `level` rule). A positioned page
  (`positioned`, copied: rotated text, overlaps), text between column
  runs, unequal or more columns stay as PdfCraft flows them. The section
  break is PdfCraft's own 1 pt paragraph.
- Fonts and colours: `pdfcraft_export::Block` has no font. Each block's
  colour is replaced by a unique code (`code_color`, 1 to 2^24 - 1, never
  black) before the export, which only copies colours (`chr`, a cell's
  colour); after loading, every coded run (body, cells, text boxes,
  hyperlinks) gets its block's real colour (none for black) and
  `fonts::family` (PdfCraft's `font_family`, then the base 14 as Word's
  Arial/Times New Roman/Courier New, DejaVu's installed names). A text
  box is written twice (DrawingML and a VML `mc:Fallback`) and docxcore
  saves edited blocks into the first copy only, so `twin_copies` rewrites
  every identical copy with `blocks_to_xml`. The tests check the saved
  file's every part for a code colour.
- Corpus: `convert.pdf` (RawPdf, README there), checked by what it
  converts to, before and after a save and reload; `columns.pdf`,
  `scanned.pdf` and `rotated.pdf` (a positioned page) too, and every corpus
  file converts or says why in time. It is in the worker's mutation test
  (`UNREFERENCED`).
- Not done: links as hyperlinks, form values as text, footnotes,
  even/odd headers, floating pictures, unequal or more than 3 columns,
  text below columns, fonts by metrics, page numbers that do not start at
  1 as a field; a page whose forms run past PdfCraft's reading limits, or
  a picture past the budgets, converts without it and says nothing
  (`export_pages_within`'s `truncated` is not carried across the child);
  terminal docxy and wasm (slice 6).

## PDF view: print and export (#2258)

- Crates (same rev, no patch entry): `pdfcraft-print` (`impose`, `layout`,
  `select_pages`, `PAPERS`, `matching_paper`, `paper_media_option`,
  `add_cups_orientation`, `spool::{printers, submit, lp_args}`,
  `printer_default_paper`; through craft-print and craft-cms at craft-libs
  `e2dc681a3`) and `pdfcraft-organize` (`extract_pages`). craft-print has
  an **unconditional** git dependency on storytold/winprint.rs
  (`craft-winprint-gdi`, a source of its own in the lock even on Linux);
  `winprint` (twice: craft-print's rev and pdfcraft-print's) and its
  build-dependencies (reqwest, sigstore, aws-lc) are Windows only.
  `suite/docxy/build.rs` delay-loads `windows.data.pdf.dll` and
  `prntvpt.dll` (`rustc-link-arg`, every executable: the tests link
  winprint too), as PdfCraft's apps do; not buildable on Linux.
- Protocol 4 (`worker.rs`): `print {job}` (`PrintJob`, our serde mirror of
  `Settings` and `spool::Job`: pages 0-based, paper name and size,
  `Sizing`, `Orient`, `PrintContent`, printer, copies, title, `Spool`),
  `png {page, dpi}` and `extract-pages {pages}` (bytes after the line);
  the parent's `Op::ExportTo {export, path}` asks the child per page
  (each PNG its own request and timeout) and writes every file or none
  (r2: one file, a PDF or a page's PNG, through `fsio::export_atomic`;
  several through `Staged`, by fsio's rules (`write_destination`: a
  symlink writes its target, a read-only file is refused up front, the
  replaced file's permissions kept; `temp_sibling`: exclusive temporary
  names never shared between writers). Each page is written and synced
  as it comes, so the suite holds paths, not pictures. `commit` renames
  in order, a file already at a name moved aside first but at the last
  name, whose rename replaces it atomically; on a failure it puts every
  done name back, best effort, and the error says where a file it could
  not put back is; a file moved aside that cannot be removed after is
  named in the status (`Exported::left`). The temporary files go on drop;
  the same file twice is refused.) A page named twice is one picture
  (`distinct`, where the export is built). No export replaces the PDF the
  view reads (`EXPORT_OVER_ITSELF`, `fsio::same_file`).
  `Op::timeout`: print and extract get `JOB_FACTOR` (3) × the child's
  timeout, in the parent's `exchange` and in the child's watchdog
  (`Busy` holds each request's limit, twice it). A print or export goes
  in `form_ops` (after the fills before it, before any page); the view's
  pages wait while it runs (one question at a time).
- The child (`worker/doc.rs` `output`): refuses pages past its own count;
  then the permissions, as PdfCraft's engine (`NO_PRINTING` `print()`,
  `NO_COPYING` `copy()` for a PNG, `NO_EXTRACTING` `extract_pages()`, which
  is assembly and copying); a file only the renderer reads (no working
  copy) refuses too (`NOT_READ`), never the raw bytes. `shown()` is the
  display document (`with_field_appearances_cached(
  with_missing_appearances_cached(cos))`, or `cos`): the display copy's
  bytes are written from it, and print and extract take it, so unsaved
  values and PdfCraft's appearances print. Print: `impose`, `layout` (the
  pages' `display_size`, as `impose` reads them) for
  `add_cups_orientation` (CUPS only: nothing on Windows, whose print
  ticket turns each sheet), the job's options `paper_media_option` (as
  PdfCraft's Print dialog), `Scaling::Exact`, duplex left to the printer.
  PNG: dpi clamped 18–1200 and capped by `effective_scale` (the answer
  says the dpi drawn; the status says so when lower). `extract_pages`
  output is never encrypted (`Document::new_empty`).
- The parent (`print.rs`, `output.rs`): Ctrl+P (`is_print_chord`, before
  the PDF keys, on any `Kind::Pdf` tab) and File > Print (`bs-pdf-print`)
  list the printers on a thread of their own (`discover`: `lpstat` and the
  PPD take seconds), then open `pdf-print`; never on the view's worker
  thread. The range is parsed in the parent (`select_pages` with `info`'s
  labels, `PrintError::said(in_english)`); typing a range picks Pages, a
  percent Custom scale (`react`, which reads the control's new value: the
  dialog hands it the old one). Paper: the printer's default paper, else
  the first page's `matching_paper`, else US Letter. File > Export
  (`bs-pdf-export`, the `bs_export` page drawn by `pdf_export_page` on a
  PDF tab) has tiles `pdf-export-png` / `pdf-export-pdf` (probes), each a
  dialog (`pdf-export-png`: pages, 72/150/300/600 dpi; `pdf-export-pdf`)
  then the native Save dialog (`with_extension`, shared with Save As's
  `pdf_save_as_target`: the extension added, any other refused;
  `picked_export_target` refuses a name the extension was added to that
  is a file already). PNG names: one page the name picked, several
  `<stem>_Page_<n>.png` zero-padded to the highest page's digits
  (`png_names`); one of those taken (but the picked one) refuses all.
  The rail filter has `pdf_only` (`backstage_rail_items(project, doc,
  pdf)`); a PDF tab not open refuses with `PDF_NOT_OPEN_OUTPUT`.
- Harness: never spools. Its printers are `harness_printers` (Test
  Printer, the default, and Second Printer), its jobs `Spool::Dir`
  `<config root>/print-jobs` (`job-<n>.pdf`, `job-<n>.json` with the job,
  `options` and `lp_args`); `pdf-read` adds `printed` (the last job's
  JSON and `jobs`), `export_waiting` and `exported` (the last export's
  file names). An export's Save dialog is the `save-as` verb: Save… keeps
  the export on the view (`export_waiting`), and `save-as` on that tab
  writes it instead of a Save As (one queued export, one path); a name it
  refuses keeps the export waiting. `printers_arrived` opens the dialog
  only on the tab in front and clears "Looking for printers…" either way.
  `uiharness/cases/pdf-print.uit`.
- Tests (`pdf_view/print_tests.rs`): sheets' size per paper and
  orientation, the order chosen, sizing measured on the sheet as drawn
  (ink width), Actual size on paper the page's size against the corpus's
  `ref/` (the block test, `text` and `rotated`), unsaved values in the
  print and the PDF, Form fields only and Document, permissions on
  `enc-owner-only` and `enc-aes-128` (user and owner), PNG sizes and the
  72-dpi PNG against `ref/`, export names and all-or-none, the job file;
  `worker/tests.rs`: a print past a page's timeout answered, a hung one
  given up at its own deadline; the mutation test prints and exports the
  originals and the first `OUTPUT_MUTATIONS` (7) mutations of each file.
- Not done: print preview, pages per sheet, booklet, poster, print as
  image, grayscale, duplex choice, printer properties; lifting an
  owner-only file's restrictions (no UI takes its owner password);
  printing documents, sheets or projects; terminal docxy and wasm
  (slice 6).
