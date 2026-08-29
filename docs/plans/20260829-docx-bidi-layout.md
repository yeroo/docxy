# Implement true bidirectional DOCX layout

## Overview

Replace the current `w:bidi` right-alignment approximation with Unicode
Bidirectional Algorithm layout for terminal paragraphs. Preserve logical editor
offsets while rendering mixed RTL/LTR text, numbers, neutrals, combining marks,
and wide glyphs in visual order with correct caret, selection, and mouse mapping.

## Scope boundaries

- Target the `docxy` terminal document renderer and its shared layout/hit-test
  helpers; keep `docxcore` text storage and editor offsets logical.
- Structure paragraph and run direction from OOXML, but keep the actual UBA
  dependency in the UI crate so `docxcore` remains std-only.
- Do not implement complex-script shaping, font fallback, or exact Word line
  breaking. Clearly distinguish those limitations from bidi reordering.
- Preserve non-page and page-view behavior, tables, fields, hyperlinks, tracked
  revisions, and terminal width semantics.

## Acceptance criteria

- Pure Hebrew/Arabic, pure LTR, and mixed paragraphs render in UBA visual order;
  embedded numbers and punctuation retain correct relative ordering.
- Paragraph `w:bidi`, run direction/overrides, explicit Unicode controls, and
  neutral base-direction inference have deterministic precedence.
- Logical offsets remain the editor API. Moving, extending selection, clicking,
  dragging, Home/End, wrapping, and vertical movement map through a tested
  logical-to-visual projection.
- Combining sequences and double-width terminal glyphs do not split, overlap,
  or place the caret inside an invalid display cell.
- Serialization and plain-text copy/export remain logical and unchanged.

## Implementation steps

### Task 1: Specify bidi inputs and choose the layout dependency

- [x] inventory paragraph/run direction data, current wrapping/cell-width logic,
      caret/selection mapping, mouse hit testing, tables, and page-view paths
- [x] select a maintained UBA implementation compatible with Rust 1.88 and the
      repository's licensing/dependency policy; add it only to the appropriate UI
      crate and record why it is needed
- [x] structure run-level RTL/override data currently hidden in raw run properties
      without breaking lossless serialization
- [x] define base-direction and override precedence for OOXML and Unicode controls
- [x] add loader/model tests for paragraph and run direction round-trips
- [x] run `cargo test -p docxcore` before Task 2

Task 1 notes:

- Direction inputs: direct paragraph `w:bidi` is modeled as `ParProps.rtl`;
  direct run `w:rtl` is modeled as `RunProps.rtl`; explicit-off `w:bidi` and
  `w:rtl` stay in `raw_props` so absence, style inheritance, and direct off
  remain distinguishable for save and layout. `styles.xml` now resolves
  run-level `w:rtl` and paragraph-style `w:bidi` through `StyleSheet`.
- Current rendering inventory: `docxcore/src/render.rs` flattens logical inlines
  in `flatten_para`, measures with `char_width`/`str_width`/`glyph_w`, wraps in
  `wrap_glyphs`, and derives editable columns in `glyph_extent`/`LineMap`.
  `w:bidi` was only a right-alignment cue; no UBA projection exists yet.
- Mapping inventory: docxy keeps editor offsets logical. `docxy/src/main.rs`
  consumes `LineMap` in `caret_screen`, `move_vert`, `click_caret`, `link_at`,
  mouse drag selection, and keyboard selection extension; table rendering shifts
  cell maps in `render_table`; page view frames/paginates body maps and drops
  header/footer maps so body clicks do not collide with repeated page chrome.
- Dependency decision: use `unicode-bidi` 0.3.18 in `docxy` only. `cargo info`
  reports license `MIT OR Apache-2.0` and `rust-version` 1.47.0, which is
  compatible with the workspace MIT license and Rust 1.88 MSRV while preserving
  the std-only `docxcore` boundary.
- Base-direction precedence for layout: direct paragraph `w:bidi` on wins;
  direct paragraph `w:bidi` off blocks style inheritance; otherwise paragraph
  style chain `w:bidi` applies; otherwise infer from the line's first strong
  directional character with LTR as the no-strong fallback.
- Run-direction precedence for layout: Unicode bidi controls remain in logical
  text and are interpreted by the UBA implementation; OOXML directional
  containers such as `w:bdo`/`w:dir`, when parsed by the projection layer, create
  explicit override/embedding ranges; direct run `w:rtl` wins over run style or
  document defaults; run style/default `w:rtl` wins over paragraph base for that
  run. Editor storage, copy/export, undo/redo, and public control offsets remain
  logical.

### Task 2: Build a reusable visual-line projection

- [ ] create a pure layout type mapping logical text/run offsets to visual glyph
      clusters and terminal cell spans for one wrapped line
- [ ] apply UBA levels/reordering before cell placement while keeping style,
      hyperlink, field, revision, and source-offset ownership attached to clusters
- [ ] handle combining marks, emoji sequences supported by the existing width
      policy, zero-width controls, tabs, and wide characters without invalid maps
- [ ] expose visual-to-logical hit testing plus logical caret-leading/trailing
      positions with documented boundary behavior
- [ ] add table-driven tests from standard bidi examples and focused mixed-script
      terminal-width cases
- [ ] run `cargo test -p docxy` before Task 3

### Task 3: Integrate bidi with wrapping and alignment

- [ ] make line wrapping produce logical ranges and a visual projection per line,
      with paragraph base direction applied independently after each wrap
- [ ] align RTL and explicitly aligned paragraphs without double-reversing or
      treating right alignment as reordering
- [ ] integrate projections in ordinary paragraphs, list labels, table cells,
      headers/footers, page view, fields, hyperlinks, and revision display spans
- [ ] preserve clipping, scrolling, tiny viewport, and horizontal offset behavior
- [ ] add renderer-state tests for wrapped LTR/RTL/mixed text in body and tables
- [ ] run `cargo test -p docxy` before Task 4

### Task 4: Route navigation, selection, and mouse input through the map

- [ ] update caret drawing, left/right visual movement, Home/End, vertical desired
      column, selection painting, mouse click, and drag selection to use the same
      projection rather than duplicate index arithmetic
- [ ] retain logical word/document operations, copy order, undo/redo, and control
      API offsets; document where visual arrow movement crosses bidi runs
- [ ] ensure selections spanning several directional runs paint every visual cell
      once and do not include padding or control characters
- [ ] add key/mouse tests for both paragraph directions, boundaries, wrapping,
      wide/combining glyphs, and mixed numeric text
- [ ] run `cargo test -p docxy` before Task 5

### Task 5: Add regression fixtures and live evidence

- [ ] add minimal DOCX fixtures for Hebrew, Arabic, mixed Latin/numbers/neutrals,
      explicit run overrides, lists, tables, headers, and tracked revisions
- [ ] verify visual projection, logical copy/export, save/reload, and unchanged XML
      direction properties
- [ ] capture representative page and non-page views with the existing UI harness
      if possible without adding OCR/golden-image infrastructure
- [ ] run `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `cargo fmt --check`, and `git diff --check`

### Task 6: [Final] Document bidi behavior and limits

- [ ] update DOCX support/rendering docs with direction precedence, navigation
      semantics, dependency rationale, shaping limitation, and evidence paths
- [ ] record final test counts and deliberate deferrals in this plan

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*
