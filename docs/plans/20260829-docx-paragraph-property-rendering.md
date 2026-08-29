# Render remaining DOCX paragraph properties

## Overview

Render the high-impact paragraph properties that docxcore currently preserves
but mostly ignores: shading, complete borders, outline level, and vertical
spacing/line-spacing effects. Keep unsupported Word pagination flags lossless and
document terminal-specific approximations instead of pretending at pixel parity.

## Scope boundaries

- Cover paragraph properties in `docxcore` parsing/model/serialization and the
  `docxy` terminal renderer.
- Include paragraph shading; top/bottom/left/right/between borders with style,
  width, color, spacing, and shadow/frame metadata needed for rendering; outline
  level; space before/after; and line-spacing behavior representable in rows.
- Do not absorb table/cell properties, character shaping, full pagination,
  widow/orphan control, `keepNext`, or print-layout fidelity.
- Untouched OOXML must remain lossless; rendering fields may normalize only when
  a modeled property is edited.

## Acceptance criteria

- Imported paragraph shading and each border side are visibly distinct from
  ordinary text and adjacent paragraph/table borders without corrupting content.
- Border style/color/width degrade through a deterministic terminal palette and
  glyph policy; unsupported variants remain preserved and documented.
- Before/after and auto/line spacing affect layout consistently within terminal
  granularity, including selection/caret/mouse mapping across inserted display
  rows.
- `w:outlineLvl` contributes to outline/navigation independently of style-based
  headings without changing body text or saved values.
- Parse-save-parse is stable for modeled and raw neighboring properties.

## Implementation steps

### Task 1: Structure paragraph visual properties

- [ ] inventory `ParProps`, styles/cascade, raw property ordering, editor setters,
      outline generation, and renderer assumptions
- [ ] add typed shading, outline level, and full per-side paragraph border data
      while retaining unknown attributes/children needed for lossless save
- [ ] define inheritance/override behavior between style and direct properties
      and terminal fallbacks for unsupported border patterns/colors
- [ ] add model/parser tests for auto/theme colors, nil/none borders, all sides,
      between/bar variants, spacing attributes, outline values, and raw neighbors
- [ ] run `cargo test -p docxcore` before Task 2

### Task 2: Serialize modeled properties without fidelity loss

- [ ] emit modeled shading, outline, borders, and spacing in CT_PPr schema order
      while preserving unmodeled children and unknown attributes
- [ ] keep untouched imported XML semantically stable and make edit-generated XML
      deterministic across repeated saves
- [ ] update relevant editor/ribbon setters to keep typed and preserved property
      state coherent rather than emitting duplicates
- [ ] add edit-save-reload tests plus schema-order and idempotence assertions
- [ ] run `cargo test -p docxcore` before Task 3

### Task 3: Apply spacing in terminal layout

- [ ] convert before/after, beforeLines/afterLines, auto-spacing, exact/atLeast,
      and multiple-line spacing into a documented row-granularity layout policy
- [ ] integrate display-only spacer rows with viewport height, scrolling, page
      breaks, tables, caret visibility, selection, and mouse hit testing
- [ ] ensure spacer rows never appear in copied/exported text or logical offsets
- [ ] add pure conversion tests and renderer/input tests for consecutive, first,
      last, empty, list, and table-adjacent paragraphs
- [ ] run `cargo test -p docxy` before Task 4

### Task 4: Render shading and complete borders

- [ ] paint paragraph shading across the paragraph content area using resolved
      colors without overriding selection, caret, field, link, or revision cues
- [ ] render top/bottom/left/right/between borders with deterministic conflict and
      collapse rules at adjacent paragraphs and table boundaries
- [ ] degrade width/style/shadow/space safely in narrow terminals and page view
- [ ] add renderer-state tests for all sides, adjacent conflicts, selection,
      clipped viewports, Unicode, and default/no-property cases
- [ ] run `cargo test -p docxy` before Task 5

### Task 5: Integrate outline level and regression fixtures

- [ ] include valid direct/style-derived outline levels in outline/navigation,
      with clear precedence relative to existing heading levels
- [ ] add DOCX fixtures combining shading, complex borders, spacing, outline,
      lists, tables, and unknown pPr children
- [ ] verify rendering state, outline results, editing, save/reload, and unchanged
      raw property preservation
- [ ] run `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `cargo fmt --check`, and `git diff --check`

### Task 6: [Final] Document terminal paragraph fidelity

- [ ] update DOCX support/rendering docs with supported properties, style cascade,
      terminal approximations, outline behavior, and deliberate pagination gaps
- [ ] record final test counts and fixture/evidence paths in this plan

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*

