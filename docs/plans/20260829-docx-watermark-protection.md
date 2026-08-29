# Render DOCX watermarks and enforce document protection

## Overview

Replace the current status-only DOCX watermark/protection hints with honest
behavior in the terminal editor: text watermarks are visible in page view and
enforced Word document-protection modes constrain every docxy mutation path.
Keep advisory write protection distinct from enforced restrictions.

## Scope boundaries

- Cover `docxcore::Package` metadata plus the `docxy` TUI, control socket, and
  MCP surface that routes through it.
- Centralize policy in one structured protection model instead of comparing
  human-readable strings.
- Text watermarks get a terminal-appropriate page overlay. Picture watermarks
  retain a clear fallback indicator unless an existing image path can render
  them without a separate header-layout project.
- Do not implement password removal, protection bypass, form-field editing, or
  automatic tracked-change creation in this plan.
- `w:writeProtection` is advisory and must not silently become mandatory.

## Acceptance criteria

- Enforced `readOnly` documents reject content, structure, formatting, comment,
  header/footer, control/MCP, and indirect mutations without dirtying history.
- `comments` protection permits comment operations and blocks unrelated edits;
  formatting-only protection blocks style/format mutations while permitting
  content edits.
- Because docxy cannot yet perform conforming form-field-only or automatically
  tracked edits, enforced `forms` and `trackedChanges` modes fail closed with an
  accurate actionable status rather than writing nonconforming changes.
- Advisory write protection remains editable after a visible warning.
- A text watermark is visibly associated with every affected page in page view,
  without entering copy/export text or changing saved OOXML.
- All rejected paths return a stable reason and leave document, undo/redo,
  package parts, dirty state, and save state unchanged.

## Implementation steps

### Task 1: Model protection and watermark metadata structurally

- [ ] replace the string-only protection accessor with enums/structs covering
      enforcement, edit mode, formatting lock, advisory write protection, and
      the source metadata needed for user-facing explanations
- [ ] represent text versus picture/unknown watermarks and associate header
      metadata with the sections/pages it applies to when package relationships
      make that available
- [ ] keep a compatibility label helper for status text rather than making UI
      strings the policy API
- [ ] test boolean lexical forms, absent/disabled enforcement, every edit mode,
      formatting-only, advisory protection, text entity decoding, and picture
      fallback detection
- [ ] run `cargo test -p docxcore` before Task 2

### Task 2: Define one mutation authorization policy

- [ ] inventory every TUI, ribbon/dialog, Vim, find/replace, header/footer,
      comment, save-as conversion, control, and MCP route that can mutate a DOCX
- [ ] classify mutations as content, structure, formatting, comment, or package
      metadata and implement one `App`-level authorization decision used by all
      routes
- [ ] encode the acceptance-mode matrix, including fail-closed forms and tracked
      changes and advisory-only write protection
- [ ] return stable machine-readable control errors plus concise TUI status text
- [ ] test the full policy matrix independently of key bindings
- [ ] run `cargo test -p docxy` before Task 3

### Task 3: Gate all interactive mutation paths

- [ ] route typing, deletion, cut/paste, replace, formatting, insertions, table
      operations, comments, header/footer edits, dialogs, ribbon actions, and Vim
      operators through the central authorization decision
- [ ] ensure a denied edit neither calls editor mutation methods nor pushes undo,
      clears redo, changes dirty state, or partially updates package parts
- [ ] keep navigation, selection, copy, find, export, and inspection available
      under every protection mode
- [ ] add key/ribbon/Vim/dialog tests for allowed and denied representative paths
- [ ] run `cargo test -p docxy` before Task 4

### Task 4: Gate control and MCP mutations consistently

- [ ] classify all mutating control verbs and apply the same policy before their
      implementation runs; keep read-only verbs available
- [ ] verify MCP-generated calls inherit those checks rather than maintaining a
      second policy table
- [ ] standardize errors so automation can distinguish protection denial from
      invalid arguments or unsupported operations
- [ ] add dispatch tests for each mutation class and prove rejected calls do not
      alter the document or package
- [ ] run `cargo test -p docxy` before Task 5

### Task 5: Render terminal-safe watermark overlays

- [ ] add a deterministic page-view overlay for text watermarks using muted,
      non-interactive cells that never participate in document hit testing,
      selection, copy, export, or caret mapping
- [ ] apply section/header inheritance correctly enough that first/even/default
      headers do not paint a watermark on unrelated pages
- [ ] keep document text legible and make clipping/tiny-page behavior safe; do
      not claim rotation or opacity that the terminal cannot provide
- [ ] show a specific fallback indicator for picture/unsupported watermarks
- [ ] add pure layout tests and renderer state tests for multi-page, section,
      Unicode, tiny viewport, and no-watermark cases
- [ ] run `cargo test -p docxy` before Task 6

### Task 6: Verify end-to-end behavior

- [ ] add package fixtures for all protection modes, advisory protection, text
      watermark, picture watermark, and section header inheritance
- [ ] exercise TUI and control attempts against fixtures and verify allowed/denied
      behavior, history invariants, status/errors, overlay state, and lossless save
- [ ] capture a sandboxed page-view watermark fixture when the existing UI
      harness can do so without OCR/golden-image expansion
- [ ] run `cargo test`, `cargo clippy --all-targets -- -D warnings`,
      `cargo fmt --check`, and `git diff --check`

### Task 7: [Final] Document guarantees and limitations

- [ ] update DOCX support and control/MCP documentation with the protection
      matrix, advisory distinction, watermark display, and deliberate fail-closed
      modes
- [ ] record final test counts, fixture/evidence paths, and follow-on dependencies
      on tracked-change and form-field editing in this plan

*Note: Ralphex moves a completed plan to `docs/plans/completed/`.*

