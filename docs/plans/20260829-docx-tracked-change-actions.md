# Add DOCX tracked-change accept and reject actions

## Overview

Turn imported tracked changes from display-only revision wrappers into reviewable
document state. Support accept/reject for insertion and deletion content plus
Word property-change records, with undoable TUI and automation actions that save
schema-valid OOXML.

## Scope boundaries

- Cover inline insertions/deletions and property changes in run, paragraph,
  table, row, cell, and section property scopes.
- Provide current/next/previous and accept/reject current/all workflows where a
  stable target exists.
- Preserve author/date/id and unknown revision XML until the revision is acted on.
- Do not implement real-time collaboration, revision balloons, comparison,
  authorship configuration, move tracking, or automatic tracking of new edits.

## Acceptance criteria

- Accept insertion unwraps its content; reject insertion removes it. Accept
  deletion removes it; reject deletion restores ordinary content with the
  revision-only display cue removed.
- Nested revisions are transformed deterministically from the innermost valid
  target outward without corrupting neighboring raw boundaries.
- Accepting a property change keeps current properties and removes its change
  record; rejecting restores the previous property snapshot in the correct
  scope while preserving unrelated and unknown properties.
- Review actions are one undoable transaction, update caret/selection safely,
  work through TUI and control/MCP, and survive save/reload.
- Unsupported move/custom revision records remain lossless and are reported as
  unsupported rather than being silently accepted or rejected.

## Implementation steps

### Task 1: Inventory and model revisions as actionable data

- [x] catalogue supported WordprocessingML revision forms in existing fixtures
      and distinguish inline content revisions, property-change children, and
      unsupported move/custom records
- [x] extend revision metadata/model nodes so identity, author/date, nesting,
      prior property snapshots, and unknown XML survive parsing and cloning
- [x] define stable document-order revision addressing that does not depend on a
      stale flat block index after an action
- [x] add model tests for insert/delete, nested revisions, every property scope,
      missing metadata, and unknown revision kinds
- [x] run `cargo test -p docxcore` before Task 2

### Task 2: Parse and serialize property-change semantics

- [ ] parse `w:rPrChange`, `w:pPrChange`, `w:tblPrChange`, `w:trPrChange`,
      `w:tcPrChange`, and `w:sectPrChange` into current/prior property state while
      retaining raw metadata not explicitly modeled
- [ ] serialize untouched revisions losslessly and acted-on properties in legal
      schema order without duplicating current or prior property children
- [ ] cover boolean toggles, absent values, direct versus style-derived values,
      raw unmodeled children, and malformed snapshots without panics
- [ ] add parse-save-parse fixtures for all supported property scopes
- [ ] run `cargo test -p docxcore` before Task 3

### Task 3: Implement pure accept/reject transforms

- [ ] add document-level operations for accept/reject by stable revision target
      and for accept/reject all in document order
- [ ] implement insertion/deletion unwrap/remove semantics and recursively strip
      revision display styling only when it came from the acted-on wrapper
- [ ] implement current-versus-prior replacement for each property-change scope
      while preserving unrelated properties and wrapper nesting
- [ ] return explicit outcomes for stale, unsupported, or malformed targets
- [ ] add exhaustive transform tests including adjacent/nested revisions, empty
      content, fields/hyperlinks, tables, raw boundaries, and mixed property edits
- [ ] run `cargo test -p docxcore` before Task 4

### Task 4: Integrate review operations with editor history

- [ ] expose revision enumeration/navigation at the editor layer and keep caret,
      anchor, table paths, and viewport valid after content disappears or unwraps
- [ ] make each current/all action a single undoable transaction with exact redo
- [ ] ensure normal edits next to an untouched revision do not accidentally
      consume, delete, or rewrite the wrapper
- [ ] add editor tests for caret boundaries, selections, undo/redo, mixed blocks,
      and no-op/unsupported actions
- [ ] run `cargo test -p docxcore` before Task 5

### Task 5: Add TUI Review actions and automation verbs

- [ ] add discoverable Review UI/status for previous/next, accept/reject current,
      and accept/reject all, including revision kind and metadata when present
- [ ] keep destructive all-actions behind the repository's normal confirmation
      pattern and make keyboard/ribbon behavior consistent
- [ ] add control verbs and MCP declarations/results using stable target IDs and
      structured unsupported/stale errors
- [ ] respect the central document-protection policy when it exists; tracked-only
      protection must not permit untracked ordinary mutations
- [ ] add TUI and dispatch tests for navigation, confirmation, actions, and errors
- [ ] run `cargo test -p docxy` before Task 6

### Task 6: Verify package round-trips and interoperability

- [ ] add realistic fixtures combining inline and property changes, nesting,
      unknown metadata, comments/content controls, tables, and section properties
- [ ] verify untouched, accept-current, reject-current, accept-all, reject-all,
      undo, redo, save, and reload outcomes against the fixture XML and visible text
- [ ] open produced artifacts with the existing independent/package validators
      available in the repository and record any deliberate unsupported records
- [ ] run `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `cargo fmt --check`, and `git diff --check`

### Task 7: [Final] Document review semantics

- [ ] update DOCX support and control/MCP docs with supported revision kinds,
      accept/reject rules, navigation/undo behavior, and deliberate exclusions
- [ ] record final test counts and fixture/evidence paths in this plan

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*
