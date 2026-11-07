# DOCX editing

Notes moved from `CLAUDE.md` (#1903). Read them before changing the document editor's lists, hyphenation, sections, settings, headers/footers and page fields. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Lists: numId 0, indent at the margin, old levels (#1653, #1728)

- `ParProps::in_list()` (a nonzero `numId`) is the list test everywhere:
  `w:numId="0"` is "no numbering" (render, PDF, Markdown, RTF, AutoFormat,
  the suite's markers and new-document base). The serializer still writes
  the 0 (it cancels a style's numbering) and compare's merge keeps it.
- `Editor::change_indent`: a list item with a direct left indent that
  reaches 0 writes `w:left="0"` (`ParProps::pin_left_indent_zero`,
  idempotent, attribute order kept), so the level's indent does not come
  back and it stays on the direct path in session as after a reload. A
  step that changes no paragraph (`step_indent` on a clone) records no undo.
  The pin also covers an item numbered through its paragraph style (no direct
  `numId`, `style_num_pr` set, a styles context) and `Editor::set_indent(0, ..)`,
  both only when the item had a direct left indent when called (the Paragraph
  dialog passes 0 back for items it did not change) (#1792). In
  Increase/Decrease a style item counts only a loaded (or pinned) left, and
  it stays pinned as it moves (`ParProps::pin_left_indent`); the indent an
  in-session Increase wrote on a style item with no loaded left is not pinned
  at 0, as it cannot be told from none. `set_indent(0, ..)` is an explicit
  request for the margin, so on any list item it also counts an indent
  written in session. The suite's Left-marker ruler drag sets the indent on
  every move, so it takes `Editor::indent_base()` at the drag start (from the
  editor it edits) and calls `set_indent_from` with it, which reads "had a
  direct left" from that base (by paragraph path), not from an earlier move
  of the same drag (#1814); a move whose editor is no longer the one the base
  came from (a header closed mid-drag) passes an empty base.
- A non-list paragraph whose style chain (`StyleSheet::style_left_indent`:
  `basedOn`, the default paragraph style, then `w:pPrDefault`; twips and
  `w:leftChars`/`w:startChars` inherited apart, a nonzero count winning)
  gives a nonzero left pins `w:left="0"` the same way (#1815,
  `IndentFallback::StyleLeft`; over a count the pin keeps or adds
  `w:leftChars="0"`, which a lone `w:left` would lose to): at 0 in Decrease when
  its left was loaded (it stays loaded as it moves), in `set_indent` when it
  was direct at the start, and any `set_indent` above 0 on it counts as
  loaded. The steps are not relative to the style's indent (an Increase from
  none writes 720 over a 720 style; a follow-up). Needs the styles context.
- `Package::repair_list_indents` gives docxy's lists (9990/9991) the level
  indents an older build left out, as `ensure_list` does. Every host's
  Increase/Decrease Indent runs it (terminal `docxy` and `docxwasm` before
  the edit, re-parsing the numbering; the suite after an indent that made
  a step: Shift+Tab, Ctrl+M, the ribbon/KeyTips act, Repeat; the suite
  draws an item with no left of its own at its level from `ilvl`, so there
  it is for the saved file). Not an undo step; nothing repairs at load.
- The suite's `eff_indent` adds the level step `ilvl × 300` only to a list
  item with no left indent of its own: an explicit numId 0 takes none, nor
  does an item with a direct left (`ParProps::has_direct_left_indent`:
  `indent > 0` or a loaded `w:left`, a `w:left="0"` pin included). A
  direct `w:left` replaces the level's
  indent, as Word reads the file, and the session follows it, so an item
  draws where a save and reload puts it (#1986 r1: a rule telling a
  session-written left from a loaded one cannot survive the reload, which
  sees only `w:left`). The ruler's Left drag (`RulerChange::Left { indent:
  marker }`) writes each selected paragraph `marker − caret step + its own
  step` (`ruler_left_for`, `Editor::set_indents_from`; the steps are
  `RulerIndentBase::steps`, taken at the drag start, #1986 r2): one
  paragraph gets the marker, and a selection of list items keeps its
  nesting, each moving by the drag as before #1986. `dragged_left_marker`
  still clamps at the caret's level step, so none goes left of its own
  level. Paragraphs with different lefts of their own still all get the
  same one, as before.

## Manual hyphenation (#1266)

- Layout > Hyphenation > Manual (`suite/docxy/src/manual_hyphen.rs`) walks the
  words that may take an optional hyphen, from the caret or over the selection,
  one "Manual Hyphenation" dialog each (Yes inserts, No skips, Cancel stops; the
  `-` in the "Hyphenate at:" box can be moved). It proposes every eligible word,
  not only those at a line end as Word does: docxy's layout never hyphenates
  and wrapping differs per host. The candidate is the editor's selection; the
  dialog keeps the walk's end and the count in hidden labels.
- `docxcore::hyphenate` is a dependency-free English-oriented syllable
  heuristic, not a dictionary. `Editor::insert_soft_hyphen` puts U+00AD in a run
  of its own with `hyphen_elements`, so it saves as `w:softHyphen` (a plain run's
  U+00AD would stay literal text), as one undo step.

## Page Setup / Columns: Apply to Selected text (#1272)

- `Editor::wrap_selection_in_section` wraps a text selection inside one section
  in section breaks and edits only the middle section, as one undo step with
  the selection kept on its text. The middle and the section after start as
  the dialog says (Continuous for Columns, NextPage for Page Setup, whose
  Section start overrides the middle only when the person changed it). A
  selection edge at a paragraph edge cuts by the neighbouring paragraph's mark,
  never an empty paragraph (except beside a table, before the text or
  ending it, which cannot carry a sectPr); an edge already at the section's edge adds no
  break (and keeps the section's existing start type).
- "Selected text" is offered, and the default, only when
  `selection_section_wrappable` (not a whole section, not across sections or in
  a table cell). Line Numbers keeps its own Apply to without it.

## Layout settings.xml edits are undo steps (#1273)

- `DocSettings` (docxcore) holds `w:evenAndOddHeaders`, `w:autoHyphenation`,
  `w:mirrorMargins` and `w:gutterAtTop`; snapshots carry them and
  `DocTab::set_dirty` writes them to the package. Layout > Margins, Hyphenation
  and Page Setup's OK write the package first (a settings part docxy cannot edit
  refuses with nothing changed), then set the editor inside one `one_step`. The
  Hyphenation dialog's caps, zone and limit still write the package without
  undo; terminal `docxy` has no settings sync.

## Header & Footer leftovers (#1433)

- `docxcore::ptab` reads a `w:ptab` (alignment, relativeTo, leader) and holds
  the stop and gap maths both hosts share, in the unit each counts: cells in
  `render.rs`, px in the suite (`ptab_stop`/`ptab_gap` wrap it with the 4 px
  right inset). The terminal fills the gap with the leader glyph (cells carry
  no editor offset). The suite lays a ptab out wherever the line width is
  known: `RenderCtx::hf_width` (header/footer) or `line_width` (a Print
  Layout column or a table cell, which wins in a header table cell); the flow view has none, so a body ptab
  there stays a zero-width object. `ptab_gap_el` draws the leader.
- A delete over a floating drawing or `w:ptab` (zero-width: no offset reaches
  it) records each as its own `w:del` while tracking
  (`Editor::drop_or_record_objects`, from `spanned_object_indices`). A cut
  over 3+ paragraphs keeps the zero-width objects of the paragraphs it
  removes whole, re-inserted at the seam: the clip cannot carry them.
- `sdt::BoundProp` (was `CoreProp`) adds Company and Manager, bound to
  `docProps/app.xml` under the extended-properties store item
  (`{55AF091B-…}`; unmeasured against Word). `DocTab::sync_bound_property`
  (in `set_dirty`) writes only an edit of a control's text
  (`Editor::take_bound_edits`: each control, paired by its `w:id`, whose text
  differs from the host's last look, one per property; formatting, inserting,
  removing or reordering controls and package-only changes such as Inspect
  Document's Remove All are not), from the open header/footer and the body,
  to the part (`BoundProp::write`). The editor the edit came from shows it in
  its other controls with `Editor::set_bound_property` as part of the edit's
  own undo step; the other editor takes it as an undo step of its own (so
  its Ctrl+Z, like the source's, undoes the property everywhere through the
  next sync). That step is one per step of the source editor, whatever properties it
  changed (`PropSource`: reused in place while it is the newest, no redo
  waits and the source's newest step is the one it answered; undo and redo forget it), and it
  advances the serial counter like any step, so the host refreshes the source's
  Repeat record after the sync (`keep_current`). Controls
  in a header or footer that is not open, and a package without the part,
  are not reached; terminal `docxy` has no package sync.
- Insert > Text has Date & Time... and Quick Parts (the H&F tab's commands,
  acting on the body when no header or footer is open; KeyTips D and K, as Q
  is Field's). Alt+Shift+D / T insert a DATE / TIME field
  (`doc_fields::doc_insert_key`).

## Live page fields (#1437)

- `docxcore::page_num` is the one rule for a page's PAGE text: `PageNumFmt`
  (`w:fmt`; roman and letters, anything else decimal), `MAX_PAGE_START`
  (a `w:start` clamps to 0..32767, read wide: `sect::page_start` for the
  suite, `export.rs` its own reader), `chapter_prefix`, `page_chapters` (the first heading of the
  chapStyle level on the page, else the last one before it, carried across
  sections; an unnumbered heading is `None`, so the plain number shows),
  `page_text` and `page_field_kind`. `export.rs` uses them, and
  `export::page_labels` is the oracle the suite is tested against.
  `numbering::heading_chapters` is `heading_numbers` for the top-level
  headings by body block (a heading in a table cell moves the numbering but
  sets no page's chapter, as in the export).
- The suite's `Page` carries its `number` (restarts and fillers included).
  `page_fields::page_values` turns a `PageFlow` plus the headings into a
  `PageVals` per page; the page view's headers and footers (the editable one
  and the read-only copies) draw them through `RenderCtx::page_vals` /
  `field_live`, which changes only the drawn text and width (the cached
  result, the field's one offset and the caret are untouched). Field codes
  still show the code, body fields keep their cached text (`None`), and the
  chapter pass (`page_fields::chapter_headings`: numbering part parse,
  `heading_chapters`) runs per frame only when some section has `chapStyle`,
  over the markup view the flow was made from (its block count can differ
  from the live body's). An auto-width frame is measured with the live value
  too (`frame_content_w`, through the `TextWidth` seam).
- Different Odd & Even headers (`hf::page_slots`, `demote_filler_firsts`) and
  `FrameGeom.even` use the page's number, not its place in the document, as
  export.rs does, so `w:start="2"` makes the first page even. No harness verb
  reads drawn header text, so there is no `.uit` case.

## Web Layout fits tables to the view (#1895)

- In the flow view (a document tab with Print Layout off, and `ext::DocView`)
  `table_view::fitted_grid` lays a table out against the text width it sits
  in: `pct` (fiftieths, or the schema's `"NN%"`; 0 or unreadable counts as
  auto; above 100% is kept and scrolls) takes its share, shrinking or
  growing; `auto`, `nil` or no `w:tblW` shrinks to it and never grows; `dxa`
  keeps its grid. The columns (and `gridBefore`) scale in proportion,
  summing exactly to the target (the remainder to the last); a column at
  least `MIN_FIT_COL_TW` (432) wide stops there, a narrower one (an HTML
  spacer) scales freely, so a table that cannot shrink comes out wider and
  scrolls. It is a drawing-time decision: the model, Print Layout, headers
  and footers (`fit_width: None`) are untouched, and a resize keeps the
  scroll and selection. Min-content widths (a long word) are not measured.
- `RenderCtx::fit_width` (px) carries the width; `table_el` turns it into
  twips at the zoom and gives each cell its own text width, so a nested
  table fits its cell. While a table pen is on nothing fits (`TableGeom` is
  the grid's, in model twips): a pct table shows its grid until the pen is
  off.
- The width is the last paint's: the doc tab's `RulerProbe::draft_text_w`
  (written beside `draft_column` by the flow view's last in-flow probe,
  never cleared), DocView's `text_w`. `fit_width_probe` asks for one more
  frame (`request_animation_frame`; `window.refresh()` is a no-op while
  drawing) when the width moved more than half a pixel, so the first frame
  and the frame after a resize draw the old width. A harness read of
  painted geometry follows a `shot`, which waits for the frame after.
- A table's root is as wide as its widest row (`w`, not stretched): gpui's
  scroll extent is the union of the scroll area's direct children, so a
  stretched root would hide a wide table from it. Both flow scroll areas
  scroll on both axes under a 12 px horizontal bar (`doc-hscrollbar`,
  `ext-doc-hscrollbar`); the doc tab's `ruler_scroll_view(.., shrink:
  true)` sets `min_w(0)`, else the flex item would widen to the table.
  Print Layout passes false and still widens to a wide sheet.
- The flow view's caret follow is y only (#1895 r1): gpui's
  `scroll_to_item` also moves x on a scroll area that scrolls sideways
  (back to a wide block's left on every key), so `scroll_to_caret` only
  sets `Docxy::doc_follow`, and the flow probe's prepaint (after the scroll
  area recorded this layout's children, so a new or grown block is where it
  is now drawn) runs `follow_item_y` (`follow_y`: gpui's FirstVisible for
  y) and asks for one more frame when it moved: the follow shows a frame
  after the edit. The last layout's bounds are not used: after an Enter or
  a delete they can be another block's. A caret in a cell scrolled out of
  view sideways does not scroll x. Print Layout keeps `scroll_to_item`
  (its scroll area scrolls in y only).
- `fitted_grid` caps a pct at 10000% (`MAX_FIT_PCT`) and the target at
  `MAX_FIT_TW` (1,000,000 twips), so a mail body's `w:w="4000000000"` or
  `"1e30%"` overflows nothing; cell sums saturate.
- Harness: `doc`'s `flow` (`scroll_x`, `max_scroll_x`, `text_width`, and
  `tables`: each top-level table's `block`, `width`, `left` / `right` from
  the view's left, `fits_text`, `right_in_view`, Print Layout's too) and
  `pointer-wheel`'s `"shift": true`; `uiharness/cases/web-layout-table-fit.uit`
  and `ext-doc-view.uit` drive them.

## The flow view draws an imported mail as a browser does (#1986)

- In the flow view (a document tab's Draft and Web Layout, and
  `ext::DocView`; `RenderCtx::flow`) a paragraph whose direct `w:spacing`
  sets all of `before`, `after` and `line` (`fully_spaced`; every imported
  HTML paragraph is, #1979) is drawn exactly (`para_box`): no 2 px padding,
  no heading `mt_2`, no block gap beside it, its own before/after, and a
  line of `round(size × SINGLE_LINE × multiple)` px for `auto`
  (`SINGLE_LINE` 1.15: a browser's `line-height: normal` and Word's single
  for Arial, Liberation Sans and Times; whole pixels so lines do not drift),
  the `w:line` for `exact`, the larger for `atLeast`, at least one line
  high. `size` is its largest shown run (links' too), else its mark's (an
  empty `<div><br></div>` imports as an empty paragraph), else the base.
  So a 12pt mail's lines are 18 px apart, as in Chrome.
- Any other paragraph, and every paragraph in Print Layout, headers and
  footers, draws as before: 2 px where its spacing gives none, `mt_2` on a
  heading, `base × 1.35 × multiple`, at least `base + 6`. Only all three
  count: a Word paragraph with only `w:after` leans on its style's line
  spacing, which the suite does not resolve. Word files that set all three
  directly on every paragraph (Google Docs and LibreOffice exports often
  do) now draw exactly in the flow view too. Cells keep `flow`, so an HTML
  table's cells draw tight.
- The flow columns have no `gap_1`: top-level block i takes the 4 px as
  its own bottom margin (`RenderCtx::gap_below`, `flow_gap_below`) when a
  block follows and neither is fully spaced, so two other blocks stay 4 px
  apart. A table's root has `my_2`, which a later `mb` replaces: it takes
  `mb(8 + 4)`; the zero-height `SectionProperties` / `Raw` divs take it
  too. A margin, not a wrapper: a wrapper would stretch a wide table's root
  (#1895) and move `bounds_for_item(b)` off block b. Cells reset it. The
  last block's 4 px above the width probe is gone (scroll extent only).
- A list number is drawn and measured in its paragraph mark's run
  properties as the paragraph's styles show them (`shown_mark`,
  `ParProps::mark_run_props`, `marker_look`): size, weight, slant, family
  through `font_subst` (measured by the raw name, `width_in` substitutes),
  ink through `run_ink`, in every view; with no mark `w:rPr`, the style's
  run look, else the base size and `pal.fg`, no longer `pal.dim`. In a
  hanging indent its trailing space goes ("1." not "1. "): the gap after it
  separates it, and "1. " fills a browser's 17.8 px hang exactly, pushing
  the text to the next stop. The numbering level's own `w:rPr` is not read.
- A nested HTML list sits at its own indent (600 twips per list): an item
  with a left of its own takes no level step (Lists, above).
- Harness (test extension): `ext-doc-load {"html": ...}` imports a page
  into the view; `ext-doc-layout` reads the last frame
  (`DocView::layout_json`, recorded only in a harness instance through
  `RenderCtx::paras`): per top-level paragraph its `block`, `top`,
  `height`, `text_x`, `marker` (`text`, `x`, `right`, `size`, `color`),
  `lines` (`y`, `x`, `text`, `size`, `color` of its first word; a word left
  of the one before starts a line), `marker_matches_text` and
  `marker_fits`, and the document's `pitch_min` / `pitch_max` (an empty
  paragraph counts as a line at its top); px rounded, x from the text
  column, y from the first paragraph; colours `rrggbb` with no `#` (a
  case's `#` starts a comment). Words join their runs by caret offset. It
  says `painted: false` until a frame of the document last loaded has
  painted. `uiharness/cases/ext-doc-outlook-mail.uit` drives it, and opens
  the same mail (`fixtures/outlook-mail.html`) in a document tab.
- Known limits: explicit black text (an Outlook mail's `rgb(0,0,0)`), and
  now its numbers, stay black on a dark page; Outlook lightens them. Style
  spacing (docDefaults, Normal, the style chain) is still not resolved, so
  Word paragraphs keep the stand-ins. `ext::DocView`'s page margin is #1987.
