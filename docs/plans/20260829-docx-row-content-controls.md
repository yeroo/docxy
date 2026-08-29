# Preserve row-level DOCX content controls

## Overview

Make repeating-section and other row-level structured document tags (`w:sdt`
inside `w:tbl`) lossless without hiding their table rows from the editor. Block
and inline content controls already use raw wrapper boundaries; table rows need
an equivalent schema-aware boundary because `parse_sdt_rows` currently unwraps
the rows and discards `w:sdtPr`, `w:sdtEndPr`, nesting, and empty controls.

## Scope boundaries

- Work in `docxcore` model, loader, serializer, editor helpers, and focused
  package/corpus tests.
- Preserve arbitrary content-control properties and nesting verbatim while rows
  remain ordinary visible/editable `Row` values.
- Do not add a form-filling UI, content-control property editor, or protection
  enforcement; those are separate work streams.
- Do not regress the existing block/inline raw-boundary strategy.

## Acceptance criteria

- Loading and saving a table with one or more row-level controls preserves each
  `w:sdt` wrapper, its property children, row membership, order, and nesting.
- Empty controls and controls containing unknown children round-trip without
  becoming visible rows or malformed XML.
- Editing a row inside a control changes row content while retaining the wrapper
  around the same logical row group.
- Row insertion/deletion at a control boundary has deterministic documented
  ownership and never crosses or unbalances wrapper boundaries.
- Generated XML remains schema ordered and reloading it produces the same model.

## Implementation steps

### Task 1: Specify row wrapper boundaries in the model

- [x] add a compact table-child or row-boundary representation capable of
      expressing raw `w:sdt` open/close boundaries, nesting, and empty controls
      without turning visible rows into opaque XML
- [x] define invariants for balanced boundaries, row ownership, cloning, plain
      text, equality, and default/newly-created tables
- [x] document how inserts and deletes at the first/last row of a control behave
- [x] add model-level tests for plain rows, one controlled row, multiple rows,
      nested controls, adjacent controls, and an empty control
- [x] run `cargo test -p docxcore` before Task 2

### Task 2: Parse row-level controls losslessly

- [x] replace the flattening `parse_sdt_rows` path with parsing that captures
      `w:sdtPr`, `w:sdtEndPr`, wrapper boundaries, unknown children, and content
      in original document order
- [x] retain normally parsed `w:tr` rows inside the captured boundaries so they
      stay visible and editable
- [x] handle nested, adjacent, empty, and malformed/truncated controls without a
      panic or accidental row loss
- [x] add loader fixtures asserting both visible row data and exact wrapper
      metadata for all boundary cases
- [x] run `cargo test -p docxcore` before Task 3

### Task 3: Serialize balanced schema-valid table children

- [x] emit table properties/grid and the mixed row/boundary sequence in legal
      WordprocessingML order
- [x] preserve captured wrapper/property XML verbatim unless a modeled row edit
      requires only the row payload to change
- [x] prevent unbalanced raw boundaries from producing invalid output; validate
      or normalize them at the narrowest responsible layer
- [x] add parse-save-parse tests for nesting, empty controls, unknown properties,
      Unicode content, and multiple controlled row groups
- [x] run `cargo test -p docxcore` before Task 4

### Task 4: Make row editing boundary-safe

- [ ] audit table row insertion, deletion, split/merge, copy/paste, and cloning
      helpers for assumptions that `Table::rows` is the complete child sequence
- [ ] preserve control membership for edits within a group and apply the Task 1
      boundary rule for edits at its edges
- [ ] ensure deleting every visible row does not silently delete an otherwise
      non-empty control definition unless that is the documented operation
- [ ] add focused editor tests including undo/redo where the public editor exposes
      the affected table operation
- [ ] run `cargo test -p docxcore` before Task 5

### Task 5: Add realistic package and regression coverage

- [ ] add a minimal DOCX/package fixture with repeating-section properties,
      nested/adjacent controls, row properties, merged cells, and unknown XML
- [ ] verify package round-trip retains wrapper counts/order and document text,
      and that a targeted cell edit stays inside its original control
- [ ] rerun existing block- and inline-content-control tests to prove their raw
      boundaries and invisible rendering remain unchanged
- [ ] run `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `cargo fmt --check`, and `git diff --check`

### Task 6: [Final] Record the row-control contract

- [ ] update the relevant DOCX support/gap documentation with the representation,
      boundary-edit behavior, validation evidence, and deliberate UI deferrals
- [ ] record final test counts and fixture paths in this plan

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*
