# PDF view

Notes for the suite's PDF view: opening a PDF as it looks, read-only.

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
- Not used yet: `-forms`, `-annot`, `-crypt` (forms, annotations, encrypted
  files: later slices), `-content`, `-print`, `-export`.

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

## PDF view: the tab (#2245)

- `Kind::Pdf` / `Surface::Pdf(PdfView)`. A File-only ribbon, as a Help tab.
  `pdf_view::host::pdf_view_tab` is called where a fresh document tab is
  built (`doc_tab_from_path_mode`, `tab_from_path`, session restore), never
  in `load_doc_file`: a restored `Converted::Pdf` tab, Protected View's
  rollback and Recover Text keep converting. A file named `.pdf` that does
  not sniff as one opens as before.
- Read-only: Save and Save As refuse with `PDF_NO_SAVE` / `PDF_NO_SAVE_AS`;
  no key edits. Protected View is ignored (nothing to hold back);
  `converted_pdf_tab` (Convert to editable document) opens today's
  converted tab after the view, protected when the file's zone says so. A
  second open of the PDF focuses the view, not its converted tab.
- Session: a document row (`kind: "Docx"`) with `pdf_view: {page, zoom}`, so
  an older build still reads `session.json` (and converts the PDF). A row
  without it (`converted: "pdf"`) restores converted.
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
  pixels and text, each an answer or a failure with a reason, in time.
