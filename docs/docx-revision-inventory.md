# DOCX tracked-change support and review semantics

docxy imports WordprocessingML tracked changes as reviewable document state, and
records new ones while Track Changes is on (see [Track Changes](#track-changes)).
Display for Review (see [below](#display-for-review)) shows them as Word does.
Untouched records retain their source metadata and unknown XML, while supported
records can be accepted or rejected from the terminal Review ribbon or through
the control/MCP surface. Review changes survive DOCX save and reload.

## Supported records

| Classification | WordprocessingML forms | Review behavior |
| --- | --- | --- |
| Inline insertion | `w:ins` | Accept unwraps and keeps the inserted content; reject removes the wrapper and its content. |
| Inline deletion | `w:del`, including `w:delText` and `w:delInstrText` | Accept removes the wrapper and its content; reject restores ordinary text/content and removes only the display cue contributed by the deletion wrapper. |
| Paragraph mark | `w:pPr/w:rPr/w:ins` or `w:del` | Keeping the mark (accept an insertion, reject a deletion) drops the record. Removing it (accept a deletion, reject an insertion) merges the next paragraph of the same container into this one, with the next paragraph's properties, as Word does; with no paragraph to merge with (the container's last paragraph, or a table next) only the record is dropped; a section break on the removed mark goes with it, so the merged text joins the following section. A mark inserted by one reviewer and deleted by another carries both records: each is listed and acted on separately, removing the mark also resolves the other, and Accept All / Reject All both remove the paragraph. Listed after the paragraph's content and located at its end. Editing keeps the records with the physical mark: Enter, a multi-paragraph paste or a section break leaves them on the last part, and joining two paragraphs keeps the later paragraph's records. |
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

An enforced tracked-changes-only protection mode still fails closed for
ordinary edits and for accept/reject actions. Move tracking, tracked formatting
changes (`rPrChange` and the other property records), revision balloons,
real-time collaboration, and per-reviewer filters are out of scope.

## Track Changes

Review ▸ Track (terminal) and Review ▸ Tracking ▸ Track Changes (suite) turn
recording on; `doc.track-changes-set` / `docxy_track_changes_set` do the same.
The state is the document's `<w:trackRevisions/>` setting (written at its
`CT_Settings` position, removed when turned off), so a file saved with it on
opens with it on, and the status bar says `Track Changes: On`. Edits are
recorded as the review identity (the OS user, or the suite's configured name),
stamped with the time of the edit (UTC), with a `w:id` above every revision and
comment-marker id in the document.

- **Typing, paste and Replace** record a tracked insertion: ordinary editable
  text whose runs carry `RunProps::tracked_insert`. Consecutive typing extends
  one insertion; a save writes it as one `<w:ins>`; it is listed as one
  revision and Accept / Reject act on all its runs. An insertion that is
  split (Enter, a table or a section break in the middle of it, a
  multi-paragraph paste) or interrupted (untracked text typed inside it)
  becomes one insertion per stretch, each with its own `w:id`, so a reload
  lists the same revisions. Text loaded from a file keeps its `Inline::Revision` wrapper instead, so
  loaded `w:ins` text is still not addressable by the caret.
- **Deletions** (Backspace, Delete, Cut, a selection) move the text into a
  `<w:del>` with `<w:delText>`; the editor counts it as zero-width, so the caret
  steps over it, and adjacent deletions by the author merge into one.
  Deleting text inside the author's own insertion removes it outright, as Word
  does.
- **Undo** of a recorded edit restores the exact prior document in one step
  (typing coalesces into one step as it always has).
- **Not recorded**, and edited as without tracking: paragraph marks (Enter, and
  Backspace / Delete across a paragraph boundary), text inside a hyperlink, a
  field or other one-unit inline, formatting, table structure and a table cell
  range's Delete. A selection across paragraphs records the deletion of its
  text in each paragraph and keeps the paragraph marks. Header and footer edits
  are not recorded. A copy of recorded text carries no record. Markdown
  documents refuse Track Changes.
- The editor never saves its own `RunProps::tracked_insert` as formatting: the
  underline shown for an insertion is a display cue, not written.

## Display for Review

All Markup (as loaded), Simple Markup, No Markup and Original are a view built
on a clone (`docxcore::markup::MarkupView`, `Document::markup_view`): never
saved, never changing the revision list. Simple Markup drops deletions and
unmarks insertions in place, keeping the document's paragraphs and offsets, so
it can be edited; No Markup is the text as if every change were accepted and
Original as if every change were rejected, which merge paragraphs and restore
text, so they are view-only: edits are refused with a status
(`ProtectionDenial::DisplayMode`, `protection_denied:display_mode`) until All or
Simple Markup is chosen. The suite draws tracked changes (underline for
insertions, strike for deletions) in All Markup; before #625 it drew them as an
`[image]` chip.

## Compare

Review ▸ Compare (`docxcore::compare`, the terminal Review tab, `doc.compare`,
`docxy compare`) builds a copy of the *revised* package whose body carries
`w:ins`/`w:del` records: Accept All yields the revised text and Reject All the
original. Both inputs are compared in their accepted state. Blocks are aligned
with a Myers diff; similar paragraphs get a word-level diff (unchanged and
inserted text keep the revised run formatting, deleted text the original's),
and whole-paragraph insertions/deletions also mark their paragraph mark. A
container's final mark cannot be marked, so the mark before a trailing run of
changes is marked instead (Word's convention) — unless there is no paragraph
before the run (it opens the container or follows a table) or a section break is
involved (the borrowed mark or the run ends a section). Then the change is
compared in place and reported as a skipped `paragraph-mark`: resolving it
leaves an extra empty paragraph. An inserted paragraph left that way that ends a
section also gets a `w:sectPrChange` with no prior properties, so Reject All
removes its break. After a merge the surviving paragraph takes the later
paragraph's properties (Word's rule), so when a mark is borrowed the run's last
paragraph also carries a `w:pPrChange`: Reject All of an inserted run, and Accept
All of a deleted one, leave the borrowed-from paragraph with its own properties.

Comparison is by text: formatting-only differences, headers/footers, notes and
comments are not compared (they come from the revised package). Zero-width
markers (bookmarks, comment ranges, field characters) come from the revised
side only. Deleted paragraphs lose section breaks and any style or list the
revised package lacks. The result's root also declares the original's namespace
prefixes (and `mc:Ignorable` tokens) so deleted original markup stays bound; markup
using a prefix the two documents bind differently is dropped (reported as skipped
`formatting` for property children, `object` for objects). A simple field
(`w:fldSimple`) inside an insertion or deletion is written as the equivalent
complex field, since tracked-change wrappers hold only run-level content. What cannot be expressed is reported as skipped: a table
whose shape changed (kept as revised), a deleted object or hyperlink target
that references the original package, a deleted note reference, and input
revisions that cannot be accepted (moves, custom-XML ranges, cell records).

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
