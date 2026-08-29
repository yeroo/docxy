# Render DOCX table and cell properties

## Overview

Promote high-impact table and cell properties from opaque round-trip XML into
typed display data: actual borders, widths/layout/alignment, shading, margins,
and vertical alignment. Preserve raw OOXML fidelity and existing span/merge
behavior while making terminal tables resemble their Word source.

## Scope boundaries

- Cover `w:tblPr`, `w:tblGrid`, row properties needed for layout, and `w:tcPr`
  in docxcore plus terminal rendering in docxy.
- Include table/cell borders and conflict resolution, table/cell widths, table
  alignment/indent/layout, cell shading/margins/vAlign, and row height/header
  hints that affect visible terminal layout.
- Do not implement floating tables, nested text flow, exact print pagination,
  every conditional table-style region, or a full table-formatting UI.
- Coordinate with the row-content-control representation if that plan has landed;
  do not flatten its boundaries.

## Acceptance criteria

- Imported table and cell border sides/styles/colors visibly drive the grid;
  shared-edge conflicts resolve once and merged/spanned cells stay aligned.
- Fixed, percentage, auto, and grid widths produce deterministic clamped terminal
  columns; table alignment/indent and cell margins affect available text space.
- Cell shading and vertical alignment render without obscuring selection/caret or
  changing logical text/copy order.
- Row height/header hints are represented where meaningful and documented where
  terminal/page-layout limits prevent exact behavior.
- Untouched and edited tables save/reload without dropping unknown table/cell
  properties or duplicating modeled XML.

## Implementation steps

### Task 1: Structure table, row, and cell visual properties

- [ ] inventory current raw `tblPr`/`trPr`/`tcPr`, grid/span/merge parsing,
      serializer regeneration, style inputs, and renderer geometry
- [ ] add typed width, alignment, indent/layout, margin, shading, vertical-align,
      row-height/header, and rich border structures at their correct scopes
- [ ] retain unknown attributes/children and original raw state required for
      round-trip fidelity and row content-control boundaries
- [ ] define direct/table-style/default precedence and shared-edge border conflict
      rules based on WordprocessingML semantics
- [ ] add parser/model tests for every value kind, side, merge/span combination,
      missing/auto values, theme colors, and unknown neighbors
- [ ] run `cargo test -p docxcore` before Task 2

### Task 2: Serialize typed properties coherently

- [ ] emit modeled table/row/cell properties in schema order without duplicating
      their prior raw elements
- [ ] preserve unmodeled properties verbatim and keep no-edit save/reload stable
- [ ] make existing/new table operations initialize and update typed widths,
      merges, and raw state consistently
- [ ] add idempotence, edit-save-reload, nesting, and row-control regression tests
- [ ] run `cargo test -p docxcore` before Task 3

### Task 3: Resolve terminal table geometry

- [ ] build a pure geometry pass that combines viewport/page width, table indent
      and alignment, layout mode, grid, preferred table/cell widths, spans,
      borders, and margins
- [ ] clamp impossible inputs deterministically while retaining at least one cell
      for content where space exists
- [ ] keep horizontal scrolling/clipping, nested tables, wide/combining text, and
      span/vertical-merge ownership stable
- [ ] add table-driven tests for auto/fixed/percentage widths, over/under
      constrained grids, margins, spans, nesting, and tiny/large viewports
- [ ] run `cargo test -p docxy` before Task 4

### Task 4: Render borders, shading, and vertical alignment

- [ ] resolve shared edges once and map border style/width/color to deterministic
      terminal glyph/color fallbacks, including outside versus inside borders
- [ ] paint table/cell shading with correct precedence beneath text and interactive
      cues
- [ ] place cell content at top/center/bottom within resolved row height while
      keeping caret and mouse hit testing mapped to logical content
- [ ] repeat or identify header rows in page view only where the existing page
      model can do so without inventing print pagination
- [ ] add renderer-state/input tests for conflicting sides, merged cells, shading,
      vAlign, selection, caret, nested tables, and clipping
- [ ] run `cargo test -p docxy` before Task 5

### Task 5: Add realistic fixtures and interoperability checks

- [ ] add DOCX fixtures combining table style/direct overrides, colored borders,
      widths, shading, margins, vAlign, row heights, merged cells, nested tables,
      unknown XML, and row-level content controls
- [ ] verify geometry/render state, selection/click mapping, edit/save/reload,
      wrapper retention, and stable package XML
- [ ] capture representative page/non-page table views with the existing harness
      if useful without expanding into OCR/golden-image infrastructure
- [ ] run `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `cargo fmt --check`, and `git diff --check`

### Task 6: [Final] Document table fidelity and limits

- [ ] update DOCX support/rendering docs with precedence, width/border policies,
      terminal approximations, and deliberate floating/pagination/style gaps
- [ ] record final test counts and fixture/evidence paths in this plan

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*

