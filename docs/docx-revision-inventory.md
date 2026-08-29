# DOCX tracked-change support and review semantics

docxy imports WordprocessingML tracked changes as reviewable document state.
Untouched records retain their source metadata and unknown XML, while supported
records can be accepted or rejected from the terminal Review ribbon or through
the control/MCP surface. Review changes survive DOCX save and reload.

## Supported records

| Classification | WordprocessingML forms | Review behavior |
| --- | --- | --- |
| Inline insertion | `w:ins` | Accept unwraps and keeps the inserted content; reject removes the wrapper and its content. |
| Inline deletion | `w:del`, including `w:delText` and `w:delInstrText` | Accept removes the wrapper and its content; reject restores ordinary text/content and removes only the display cue contributed by the deletion wrapper. |
| Run properties | `w:rPrChange` | Accept keeps the current `w:rPr`; reject restores the prior `w:rPr` snapshot. |
| Paragraph properties | `w:pPrChange` | Accept keeps the current `w:pPr`; reject restores the prior `w:pPr` snapshot without consuming an independent section change. |
| Table properties | `w:tblPrChange` | Accept keeps current table properties; reject restores the prior property container. |
| Row properties | `w:trPrChange` | Accept keeps current row properties; reject restores the prior property container. |
| Cell properties | `w:tcPrChange` | Accept keeps current cell properties; reject restores the prior property container. |
| Section properties | `w:sectPrChange` | Accept keeps current section properties; reject restores the prior section snapshot. |

An absent prior property container means rejection restores the default/absent
state. Boolean toggles and direct values are restored from the snapshot rather
than recomputed from styles. Unrelated and unmodeled property children are
preserved. A malformed or scope-mismatched snapshot is reported as malformed
and left untouched rather than guessed at.

## Order, navigation, and history

Every record has a document-local `RevisionTarget` that remains stable when an
earlier change is removed or unwrapped. Enumeration and navigation recompute
document order after each action and report one-based ordinals, nesting depth,
parent target, kind, supported state, source `id`/`author`/`date` when present,
and an editor-safe start/end location. Stable targets are encoded as strings on
the control/MCP wire so all 64 identity bits survive JSON clients.

Previous/next navigation wraps at the document ends. The current change is the
one explicitly selected by review navigation, or the change at the caret when
there is no surviving review selection. If acted-on content disappears, the
caret, selection, table path, and viewport are repaired to a valid location.

Accept/reject-current is one undoable transaction when it applies. Accept/reject
all snapshots the initial revision list, transforms nested records
innermost-first, returns outcomes in the original document order, and creates
one undo transaction for all applied records. One undo restores the entire
action and redo reapplies it exactly. Stale, unsupported, and malformed targets
are structured no-ops and do not create a history entry; bulk actions continue
past them and report each skipped outcome.

In the terminal TUI, the Review ribbon provides Previous Change, Next Change,
Accept, Reject, Accept All, and Reject All. Navigation status includes revision
kind, stable target, and source metadata when present. The keyboard shortcuts
are Alt+Shift+Left/Right for previous/next and Alt+Shift+A/R for accept/reject
current. Accept All and Reject All require an explicit, default-No confirmation.

## Fidelity and deliberate exclusions

Modeled records retain optional source id, author, date, unknown attributes,
nesting, property snapshots, and their complete raw wrapper until acted on.
Normal edits beside an untouched revision preserve its wrapper boundary.
Comments, content controls, fields, hyperlinks, tables, raw boundaries, and
unknown producer extensions around or inside supported content remain intact.

Move revisions (`w:moveFrom`, `w:moveTo`, and their range markers), custom-XML
revision range markers, cell insert/delete/merge records, conflict revisions,
and future producer-specific revision kinds are deliberately unsupported. They
remain lossless and visible in enumeration with `supported:false`; an action
returns `unsupported_revision` and does not remove or reinterpret the record.
Malformed property snapshots similarly return `malformed_revision`. A target
removed by an earlier action returns `stale_revision`.

docxy reviews imported changes but does not record new edits as tracked changes.
Consequently, an enforced tracked-changes-only protection mode still fails
closed for ordinary edits and for accept/reject actions. Move tracking,
comparison, revision balloons, real-time collaboration, authorship
configuration, and automatic tracking of new edits are out of scope.

## Interoperability evidence

The realistic package fixture combines nested inline changes, all six property
scopes, comments, content controls, tables, section properties, unknown
attributes/children, and deliberate unsupported records:

- `docxcore/tests/fixtures/revision-package.xml`
- `docxcore/tests/fixtures/revision-comments.xml`
- `docxcore/tests/revision_package_roundtrip.rs`

The package suite verifies untouched, accept/reject-current, accept/reject-all,
undo, redo, save, and reload results. Every produced artifact is opened through
the independent OPC ZIP reader, checked for balanced XML and required package
relationships, and loaded by both DOCX loaders. The deliberate unsupported
evidence is `w:moveFromRangeStart` id 199 and
`w:customXmlInsRangeStart` id 198; every review workflow reports and preserves
both.
