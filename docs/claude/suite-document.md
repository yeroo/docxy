# Suite document tab

Notes moved from `CLAUDE.md` (#1903). Read them before changing the suite's document tab: save-as names, the Font boxes, File > Info, the comment editor, pictures and run styles. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Document save-as names (#807)

- The harness's `save-as` on a document (`save_as_target`) refuses, before
  anything is resolved or written, a name whose last piece (after `/` or `\`,
  on every platform) holds `: * ? " < > |`, with `DOC_BAD_NAME` ("file name is
  not valid"); a relative `a:name` is refused, not read as a drive, as is a
  Windows drive-relative `c:sub\x.docx` (which would replace the folder). A
  path that resolves to no name (`..`) gets no extension appended. On Unix a
  resolved name holding `\` is refused too. A `format` that the name's own
  extension does not save as (or that names none) appends the format's
  extension, as Word does: `notes.txt` + docx is `notes.txt.docx`, html
  appends `.html`. Workbooks and plans still refuse a mismatch. The native
  dialog is unchanged.

## Document Home ribbon Font / Font Size boxes (#1361)

- The document's boxes are the same `Docxy::font_box` (`FontBoxEdit` with
  `sheet: None`, tied to the tab only), opened by a click on `combo_box` or by
  `Act::FontName` / `FontSize` (the harness's `ribbon-click`; the combos have no KeyTips) through `font_box_open_doc`,
  which closes the Find and comment bars. `font_box_live` is surface-aware, so
  `sheet_field_live` and `entry_owns_keys` cover it. `on_key` hands it every
  key after the KeyTips and `document_key` (Ctrl+W, Ctrl+Tab, F12 still act),
  Ctrl+S saving and closing it. `dispatch_act` closes it for any other command;
  Find and a new comment close it too.
- Enter sets `set_font` (name cut to 31 characters) or `set_font_size`
  (`font_box::parse_doc_size`: 1 to 1638 pt, rounded to a half point; a bad one
  keeps the box and says so), on the selection as one undo step (a bare caret has no run to format and
  the editor keeps no pending font or size: the box closes and the status says
  to select text), and the document gets the keyboard back, as it does on
  Escape. Enter on a box nothing was typed in (`edited`
  false: the fill is the caret run's direct font or size, Calibri / 11 when it has
  none) is Escape too, so it never writes a style's size or a theme font as
  direct formatting; typing the value shown is an edit and applies. The old chip strips (`PickKind::FontName`
  / `FontSize`) are gone. `uiharness/cases/doc-font-box.uit` covers what
  `doc-inputs-typing.uit` does not.

## File > Info Properties (#1364)

- `Docxy::info_page` has a Properties section: Title (`dc:title`), Tags
  (`cp:keywords`) and Author (`dc:creator`) as one-line fields
  (`suite/docxy/src/info_props.rs`, `InfoEdit` on `field_edit` like the
  Font box, probes `info-prop:<title|tags|author>`). A click opens one with
  the value selected; Enter, Tab (to the next field), a click elsewhere on
  the page, or leaving Info (applied by the next render) applies it, Escape
  restores it. Backstage's `on_key` hands every key to a live field before
  its page-scroll keys. Protected View and final documents refuse the click
  with a note under Properties.
- The values are editor state, like `DocSettings`: `docxcore::editor::CoreFields`
  rides in every `Snapshot`, `Editor::set_core_field` is one undo step named
  Title/Tags/Author (a no-op for an equal value) and shows the text in the
  body's bound content controls inside it. `DocTab::set_dirty` calls
  `sync_core_fields`: what `Editor::core_unsynced` (editor vs the last value
  the host wrote) lists goes to `docProps/core.xml` (`BoundProp::write`) and
  to the open header/footer's controls, so an edit, undo and redo all reach
  the file through that one place. A control typed in (`sync_bound_property`)
  tells the editor with `Editor::file_property_changed`, which notes the field
  after the controls follow; a header/footer-typed value with no body control
  to step also rewrites every undo/redo step's field, or a body undo would
  write the old one back. A file with no core part (most
  files other tools make, every `uiharness/fixtures` document) gets an empty
  one on the first edit (`Package::ensure_core_properties`: content-type
  override and package-root relationship; it stays after an undo). A document with
  no package yet (a new one) is given one on its first Info edit
  (`DocTab::ensure_package`, shared with `mailings_tab::package`: the
  package a save would make plus `Package::add_markdown_numbering`, because
  a tab that holds a package never gets a numbering part later and Bullets
  and Numbering use `numId`s 1 and 2; its styles part is untouched), so the editor never holds values
  the package lacks. A tab whose save writes no package (Markdown, Plain
  Text, Rich Text, or a new document saved as an editable-HTML page, which
  has none) refuses the edit with a note; an opened HTML page keeps them in
  the package it embeds.
- `BoundProp::write` and `parse_core_props` spell the elements as the root
  binds their namespaces (`field::CoreNames`: the prefix bound to the URI, none
  for a default namespace as System.IO.Packaging writes, else a conventional
  prefix with its `xmlns` declaration added to the root when an element is
  appended); a prefixed file is written byte-identically to before.
- Anything that changes core.xml other than an edit must call
  `Editor::reseed_core_fields`, which also rewrites every undo and redo step,
  or a later sync or undo writes the old values back: Inspect Document's
  Remove All Properties does. `DocTab::seed_core_fields` seeds an editor made
  without them. Out of scope: Comments, Related People, Subject, Company,
  `lastModifiedBy` (#872), sheets (#600), terminal docxy, wasm editors.
- `info-title`, `info-tags` and `info-author` are harness inputs
  (`inputs.rs`; `Reach::Missing` is gone); `backstage-inputs-typing.uit`
  types into them. Backstage swallows Ctrl+Z, so its undo steps close File.

## Document comment editor (#1362)

- The comment bar (`comment_key` / `comment_bar`) edits at a caret through
  `field_edit` like the Find bar: `comment_caret/sel/len` with `comment_state`
  and `comment_edit`, Ctrl+A/C/X/V in `comment_chord` (paste is one line),
  and `field-read comment-editor` reports the caret and the `[0,len]`
  selection. Click-to-place and Shift+arrows are not there.

## Suite inline pictures (#1432)

- The suite's `paragraph_el` Raw arm draws an inline `w:drawing` picture as
  `gpui::img` (`ObjectFit::Fill`) inside the same one-offset box #768 drew as a
  frame; box size, offsets, caret and `block_height_est` are unchanged. The
  border shows only when no pixels do: the rid does not resolve, or the bytes
  are not PNG/JPEG/GIF/BMP by `docxcore::image::sniff` (WMF/EMF/SVG/TIFF keep
  the frame). Pixels that sniff but fail to decode in gpui draw nothing.
- `docxcore::image::inline_embed` is the rid (same inline-only guards as
  `inline_extent`, and only when `a:graphicData` is a picture: a group or shape
  holding blips is not one). `suite/docxy/src/pictures.rs` resolves it through
  the story part's own relationships (`Package::part_rel_target`: relative to
  the part's own folder, `..`, absolute; external or escaping targets none): the same rid names
  different media in `document.xml.rels` and a header's, so `RenderCtx::pics` /
  `pictures::Scope` carry the story's part name (the body's, the open
  header/footer's `HfEdit::part_name`, or the page's header/footer part).
- `DocTab::pictures` keeps the `Arc<gpui::Image>`s per (part, rid) across
  frames, so only a miss reads, copies and hashes media. It is valid for one
  `Package::parts_stamp` (a fresh value from a process-wide counter on every
  mutable access to the parts, equal in an unchanged clone): a package change
  starts a new map, and a miss whose bytes the map before held takes that
  image back, so gpui keeps its decode (#1639). What two changes leave
  undrawn is retired; `Docxy::render` releases retired images from gpui's
  asset cache (`Image::remove_asset`), `close_tab_with` all of a closed
  tab's, and a closing window (its quit answers, before Don't Save drops
  tabs) all of its tabs'. An image's gpui id is its bytes' hash, so one any
  window's tab still holds is kept (`pictures::releasable`, the other
  windows read through `windows::entries_snapshot`; the updating window is
  its own `self`). With a window at hand (render, tab close) the image's
  atlas tile is dropped first, from every window (`pictures::forget`:
  `get_render_image` returns gpui's cached decode, then `App::drop_image`),
  as every decode gets a fresh `ImageId` and an orphan tile is never reused;
  a closing window's quit answers have no window, so only the decodes are
  dropped there (no tile), its own atlas going with it. A reload in place (Reopen, the reload dialog, recovery) hands
  the old cache to the fresh tab.
- A document copy keeps its pictures' media (`DocClip::media`,
  `pictures::capture`, from the story part `tab_edit_target_part` names): the
  blip's and every `r:embed` inside it (`asvg:svgBlip`, `a14:imgLayer`:
  `image::inline_embeds`), any kind
  of media (EMF, WMF, TIFF, SVG too: the part name's extension, the content
  type from the source's `[Content_Types].xml`, else
  `image::media_content_type`, else the bytes; `Package::add_part_media`
  writes it), in `Inline::Raw` and `Inline::SmartArt` runs at
  the clip's top level (not inside a tracked change or a hyperlink). A paste
  of the window's own clip runs `pictures::carry` into the part it lands in:
  a rid that names the same bytes there is kept, else a relationship of that
  part to identical bytes is taken (`Package::part_rel_targets`, the rels
  read once per paste), else the media is added
  (`add_part_media`) and the rid rewritten (`image::replace_inline_embeds`,
  `Clip::remap_embeds`). So a paste in the same story changes nothing and
  repeated pastes add one part. As Insert Picture, the part is added outside
  the undo step: an undone paste leaves it. A document with no package gets
  one (`carry_into_tab`, `ensure_package`); a tab whose save writes none
  (`saves_as_word`: Markdown, text, RTF) pastes the rids as they were. An
  `a:hlinkClick` `r:id` on a picture is not carried (a hyperlink, not media).
- A crop to a shape (#1926: Word's Crop to Shape, or an HTML `img`'s
  `border-radius` from the import, `docs/claude/html-import.md`) is read by
  `docxcore::image::inline_shape` from the picture's `pic:spPr/a:prstGeom`
  (`ellipse`; `roundRect` with its `adj` guide; anything else a rectangle)
  and drawn as gpui's rounded clip (`Img` hands `corner_radii` to
  `paint_image`): `pictures::picture_radius`, half the shorter side for an
  ellipse (a stadium when not square: gpui's corners are circular) and
  `min(w, h) * adj / 100000` for a `roundRect`, at most that half. The
  frame-only fallback's border takes the same corners. The harness's
  `doc {}` `flow.pictures` lists each body picture the last frame built
  (`pictures::Scope::recording`, `Docxy::drawn_pictures`, cleared at the
  frame's start like `painted_tables`): `width`, `height`, `radius`,
  `shape`, `round`; `uiharness/cases/html-rounded-images.uit` drives it.
- Out of scope: anchored pictures, `a:srcRect` cropping, other preset
  shapes, rotation, alt text, the flow view's non-inline cases, PDF export,
  wasm hosts.

## Run styles (#1427)

- The suite draws a run through `run_look::RunStyler` (sheet, the cell's table
  `RunLook`, the paragraph's style), i.e. `StyleSheet::shown_run`: table style <
  paragraph style (basedOn) < character style (basedOn) < direct, for bold,
  italic, underline, strike, colour, size and font family. It starts from
  nothing: `docDefaults` and the default paragraph style (Normal) are NOT
  applied, except to a paragraph whose `pStyle` names Normal; the paragraph
  chain stops before Normal when reached through `basedOn` (a visible change to every document, left for its own issue). A
  direct explicit-off (`<w:b w:val="0"/>`, in `raw_props`) beats every style;
  the character style beats the paragraph style outright (Word's toggle XOR
  between them is not modelled). `emit_words` and `Measurer::width_in` use the
  run's font family and `run_look::run_size_px` (a style's or direct `w:sz`,
  scaled by zoom), so tab positions match what is drawn; the family goes
  through `font_subst` first (see Font substitution (#1900)), never to gpui
  unresolved.
- `DocTab::run_styles` (`run_look::Cache`) parses `word/styles.xml` once per
  change of the part (a moved `Package::parts_stamp` with other part text does
  not reparse). `Docxy::render` hands the same text to the body and open
  header/footer editor with `Editor::set_styles_xml`.
- With a styles context, Bold/Italic read and write over the styles
  (`Editor::toggle_run_prop`, per run): off over a style that makes the text
  bold writes `<w:b w:val="0"/>` (also at a caret, via `PendingFormat`'s
  `over`: `Over::Off`), on again drops it and inherits. Without a context (terminal
  `docxy`, wasm hosts) toggles are unchanged. `Editor::caret_props_shown` is
  the ribbon on-state; the harness `run` reports `shown_bold`/`shown_italic`.
  Twins: turning on writes `w:b` with `w:bCs` when the run had the twin; off
  over a style keeps the twin's `w:val="0"` in the shadow only (the primary's
  also in `raw_props`, as the loader keeps it); inheriting drops both. The
  sheet has no complex-script model, so off then on under a style with `w:b`
  but no `w:bCs` drops the run's direct `w:bCs` (known gap).
  Out of scope: underline/strike explicit-off, caps/highlight/spacing from
  styles, table style size/font, `block_height_est` for styled sizes.
  `uiharness/cases/doc-run-styles.uit` was written without being run (macOS
  cannot drive the window).

## Font substitution (#1900)

- gpui caches a failed family lookup but rebuilds an `anyhow::Error` from it on
  every `font_id` call, and `resolve_font` then walks its fallback stack
  (`.ZedMono`, `.ZedSans`, Helvetica, Segoe UI, ...), most of which fail on
  Linux too: per run, per layout pass. With `RUST_BACKTRACE=1` each error
  captures a backtrace (100% CPU, a frozen embedding app). So no family is
  handed to gpui unresolved: `suite/docxy/src/font_subst.rs` resolves each
  once per process.
- A document's or plugin's family (`font_subst::family`): the family itself
  (any case, drawn in its installed spelling), then
  `docxcore::fonts::substitutes` (metric-compatible first: Calibri → Carlito,
  Cambria → Caladea, Arial → Liberation Sans, Times New Roman → Liberation
  Serif, Courier New → Liberation Mono, Aptos → Liberation Sans), then a
  generic serif or mono list picked from the name, else `None`: the inherited
  theme font. `pdffont` (terminal docxy's PDF export; the suite has no PDF
  export) uses the same table, so screen and PDF pick the same substitute.
- The theme's own families: gpui-component's default is `.SystemUIFont`, which
  gpui maps to IBM Plex Sans on Linux. The suite does not embed it, so all UI
  text failed per pass. `font_subst::apply_to_theme` (in `run` after
  `gpui_component::init`, and after every `Theme::change`) replaces an
  unloadable sans with the first installed of gpui's own fallback order
  (Ubuntu, Adwaita Sans, Cantarell, Noto Sans, DejaVu Sans, ...): the face
  drawn is unchanged, it just stops failing first. The mono family takes its
  substitutes or the mono list. macOS and Windows keep theirs.
- The embedded `ext::DocView` (#1873) calls `font_subst::init` from
  `DocView::new` and gives its root `.font_family(ui_family(inherited family))`
  (whatever the host's ancestors set) without touching the host's theme;
  `Measurer::new` resolves the same inherited family, so measured = drawn.
- Installed or not: one probe per name (`resolve_font` + `get_font_for_id`;
  gpui's `font_id` is private), since `TextSystem::all_font_names` lists
  gpui's own fallback stack whether installed or not. A substitute or generic
  candidate not in that list never reaches gpui. The family asked for is
  probed even when not listed, and drawn as asked when it loads: the list holds
  one name per family (Linux: a face's first family name; Windows: one
  localized name), while gpui also loads "Calibri Light", "Segoe UI Semibold"
  or a localized name. So a missing family costs one failed lookup per process.
  `init` snapshots the list: it runs after any `add_fonts`, and fonts added
  later are not seen. Before `init` (unit tests) every family passes through
  unchanged.
- `font_subst::tests::every_family_reaching_gpui_is_resolved` scans
  `suite/docxy/src` for the usual ways of setting a family (`sets_family`:
  `.font_family(`, `font_family` / `.family` assignments and fields,
  `.family.clone_from(`, `&mut <x>.family`, free `font(` calls, a `family`
  field of a `Font {` literal tracked by brace depth, shorthand included;
  comments ignored): each needs `font_subst::` in code on its line or the two
  above, or an allowlist entry, which names the resolver line its file must
  hold. It does not prove that a `Font` built elsewhere and passed whole
  (gpui's `Styled::font(Font)`, a `TextRun`'s `font`) was resolved, nor that
  the nearby `font_subst::` call resolves the value set: review those.
- Not done: a real platform-match tier (fontconfig `FcFontMatch`, CoreText,
  DirectWrite fallback) for unknown families; the generic serif/mono lists
  stand in.

## Link colours (#1906)

- `emit_words` draws a link run in its shown colour (`run_ink`: direct over
  character style over paragraph/table style), as Word does; only a link run
  without one (or one `hex_rgb` rejects) takes `Pal::link`. The underline
  stays.
- A tracked change inside a link (`<w:hyperlink><w:ins>`) draws as link
  text too: `emit_revision_text` takes the item's `in_link`, and
  `revision_runs` flags each run under the change.
- `Pal::link` follows the ground the palette draws on: `Pal::of` takes
  `theme_link(t.mode.is_dark())`; `page_pal` and `cell_ink` (a filled cell)
  take `design_tab::page_link`, which uses `page_ink`'s light/dark cutoff
  (`dark_ground`): `LINK` `#2F6FDB` on a light ground, `LINK_DARK` `#6CA0F0`
  on a dark one.
- Only clearly light or dark grounds read: `LINK` is 4.75:1 on white but 4.25:1
  on `#F2F2F2`, and on a mid grey near the cutoff neither blue (nor either
  ink) reaches 4.5:1. Not claimed for every page colour or fill.
- Not done: the Help manuals' links (`help_view/render.rs`, `LINK` on the
  theme ground) and the sheet's hyperlink cells (`0x0563c1`).

## Table placement by `w:jc` (#1922)

- `table_el` places a table by its own `w:tblPr/w:jc`
  (`table_view::h_align`: `center`; `right`/`end`; anything else, a table
  style's `jc` and right-to-left are not read) with auto side margins
  (`mx_auto` / `ml_auto`) on its root, in Web and Print Layout, headers,
  footers, cells and `ext::DocView`.
- Only when the table fits its room: `fit_width` (the flow view's text
  width or a fitted cell's), else `line_width` (Print Layout's column, a
  cell's), else `hf_width`. gpui centres a wider box past both edges, so a
  wider table (or one with no known room) stays at the text's left, where
  #1895's sideways scroll reaches it.
- The probe, the table pen's `TableGeom` (its `left` comes from the row
  canvas) and the #1477 margin rows are measured where the cells are
  drawn, so they follow the table.
- The harness: `doc {}`'s `flow.tables.N.place` is `left`, `center`,
  `right` or `other` across `flow.text_column` (Web Layout's column, else
  Print Layout's first page's first band, whose rect is 2 px narrower than
  the column its blocks draw in: the page's border; 2.5 px tolerance).
  `uiharness/cases/table-jc.uit`.
