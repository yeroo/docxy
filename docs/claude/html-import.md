# HTML import

Notes moved from `CLAUDE.md` (#1903). Read them before changing `docxcore::import::html`: tables, CSS, images, rounded images, nested cards, links, the page and Outlook mail (spacing, list items and markers). This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## HTML import tables (#1878)

- An HTML table is collected as an `import::html_table::RawTable` (each
  cell's blocks and the `CellSpec` its element computed, the table's
  `TableSpec` from its attributes) and laid out by `html_table::layout`
  when the outermost table ends. A nested table waits in its cell's blocks
  at a placeholder (`Block::Raw("")`, builder-internal, always replaced) and
  is laid out top-down in that cell's width (less its stated left and right
  margin), so a `width=100%` table in a column fills the column. Nesting is
  kept to `MAX_TABLE_NESTING` (8); a deeper table's cells are its enclosing
  cell's text. The `Builder` holds a stack of tables (`begin_table`,
  `begin_cell`); RTF never calls them and keeps its one even 6.5" grid.
- Cells go into a slot grid as a browser's table model places them:
  `colspan` (1..=1000, cut at `MAX_COLUMNS`) as `gridSpan`, `rowspan` (0 to
  the last row of its row group, clipped there: a group is a `thead`,
  `tbody`, `tfoot`, or the rows after one closes) as a `vMerge` restart and continuations
  (empty, the same `tcPr` but `<w:vMerge/>`), rows padded to the grid; a
  cell with no free slot joins the row's previous cell, else the cell
  covering the row. A begun cell is ended by its row's or table's end even
  when empty (a spacer `<td width=20>` without `</td>`).
- Widths (`width` attribute: digits as px, `N%`, `Npx`; CSS `width`, `auto`
  none): a column takes its widest colspan-1 cell's (percentages of the
  table, scaled down to 100 in all); a spanned cell's width spreads what its
  columns lack over those without one; the rest share what is left, each at
  least 15 twips. The table is its own width, else its columns' when all
  are known, else the room it has (outermost, the page's 11790-twip body,
  #1928). Declared columns
  narrower than a declared table grow to it; wider ones widen the grid
  (tblW stays as declared). `tblW` is dxa, pct (fiftieths) or auto; every
  cell's `tcW` is its columns' sum. Web Layout and `ext::DocView` fit a pct or auto
  table to the view's width when they draw it (#1895,
  `docs/claude/docx-editing.md`); the model keeps these widths.
- `tcPr`, in schema order: `tcW`, `gridSpan`, `vMerge`, `shd` (the cell's
  own `bgcolor` / CSS background, CSS over the attribute, else its row's,
  row group's, then table's; `background: none|transparent` on a table part
  clears its own), `tcMar` (the cell's CSS padding, sides given only), and
  `vAlign` always (`valign` / `vertical-align` on a cell, row or row group,
  inherited through the row and reset at a `table`; none is `center`, a
  browser's middle; `baseline`, `super`, `sub` on a cell are `top` and no
  longer make the cell's runs super- or subscript). `tblPr`: `tblW`, `jc`
  (the table's float `align`, auto margins or container alignment: see
  "HTML import centring (#1922)"; none of them aligns the text inside;
  `align` on a row aligns its cells' text), `tblCellSpacing` (half the HTML
  `cellspacing`, unmeasured against Word), `tblBorders` (`border=N`: outer
  `single` N px × 6 eighths clamped 2..96, inside 1 px; a bare or unreadable
  `border` is 1; `0` or none writes none, the layout-table case),
  `tblCellMar` (`cellpadding`, also 0). `raw_tcpr`/`raw_tblpr` hold them,
  `grid_span`/`v_merge` match, so a save writes them unchanged. A `div`
  with a background in a cell is a one-cell table of its own: see "HTML
  import nested cards (#1927)". Two tables back to back in a cell (nested
  or cards) get an empty paragraph between them, or Word joins them
  (`end_html_table`).
- Known limits: CSS borders, `frame`/`rules`, `border-color`, row heights,
  `<col>` widths, `max-width`, `th` bold/centre, background images; a table
  directly in a `tr`/`table` (no cell) joins the next cell (a browser puts
  it before the table).

## HTML import centring (#1922)

- A browser's model, part of the computed `Look`: `text-align`'s
  `-webkit-center` / `-webkit-right` (and `-moz-`) align the text and also
  place block-level children; they inherit, and any plain `text-align`
  replaces them (one property). `Look::block_align` holds that place
  (`Center`/`Right`, else `None`). Its presentational sources: `<center>`
  and `align` (`center`, `middle`, `right`; `left`/`justify` clear it) on a
  `div`, `p`, `caption`, `td`, `th`, `tr`, `thead`, `tbody`, `tfoot`.
  `align` on `h1`-`h6`, `li` and the rest is plain (measured in headless
  Chrome: `<p align=center>` centres a block image, `<h2 align=center>`
  does not). A `table` start tag closes an open `p`, so `<p
  align=center><table>` is not centred.
- A block-level box's place (`block_position`): both side margins `auto`
  centre it, `margin-left: auto` alone right, `margin-right: auto` alone
  left; neither, its parent's `block_align`. `Cascade::margin_auto` keeps
  the auto sides beside `margin` (`css::box_autos`, longhands too; a later
  valid declaration replaces a side), as `box_sides` folds `auto` into
  none. The result is `Look::own_place` (not inherited).
- Tables: `table_spec`'s `jc` is `right` for `align=right` and none for
  `align=left` (floats, which nothing moves), else the table's place
  (`center` / `right`). `<table align=center>` is the hint
  `margin-left: auto; margin-right: auto`, so a page's `margin: 0` on it
  leaves it at the left. A table's own `align` and margins never align its
  cells' text; its own `text-align`, like its ancestors', still does (it
  is not its place).
- Images: `display: block` on an `img` (`Look::own_block`; any other
  `display` but `none` clears it, so an e-mail's `img {display: block}`
  reset is undone by a later `inline`) makes the picture, or an unsized
  unresolved image's alt text, a paragraph of its own: the text before it
  ends (`begin_block_object`), and its paragraph ends at once aligned to
  its place, `Left` with none (`end_para_with`). A `</p>` right after it
  ends only a paragraph with content (`after_block_object`), so
  `<p><img style="display:block"></p>` leaves no empty paragraph; `<p></p>`
  is still one. In a list item, the paragraphs past the item's numbered
  one are unnumbered continuations (any block, since #1979: see "HTML
  import Outlook mail"); a picture opening its item carries the number.
- Fixed-width blocks: a `Kind::Block` with an absolute CSS `width` (not a
  percentage or `auto`; a negative or invalid one leaves the width
  before it) and both side margins auto gets
  `max(0, (BODY_TWIPS - indent - indent_right - width) / 2)` more indent on
  each side, after its own box is added: a content box, so its padding is
  outside the width. Its text alignment is unchanged. Not inside a cell
  (`Look::in_cell`): a cell's width is known only at layout.
- Drawing: the suite draws a `w:jc` table where it sits (#1922,
  `docs/claude/suite-document.md`).
- Known limits: a block's auto margins inside a cell; an outermost
  `width=100%` table inside a centred fixed-width block is still laid out
  in `BODY_TWIPS` with no `tblInd`, so it spans the body while the block's
  text is indented; PDF export does not place a table by `w:jc`; `max-width`;
  CSS `float`; `<img align>`; `box-sizing`; right-to-left.

## HTML import CSS (#1872)

- `docxcore::import::html` reads inline `style`, `<font color face size>` and
  every `<style>` block of the page (a pre-pass, `collect_styles`, outside
  comments and raw-text elements, so a style in `<!--[if mso]>` or a script
  does not apply). `import/css.rs` (dependency-free) parses declarations,
  sheets of simple selectors (type, `*`, `.class`, `#id`, compounds,
  descendant and child; anything else drops that selector alone, every
  at-rule is skipped with its block) and the values. Names compare ASCII
  case-insensitively. A sheet keeps at most 10 000 rules, a selector 32
  compounds; matching is right to left over reach sets, O(compounds ×
  depth), all charged to the import `Budget`.
- One stack of open elements (`Elem`: key, `Kind`, computed `Look`) beside
  the structural `blocks`/`lists`; a look is computed once, at open, in CSS
  order: parent, user agent (`b`, `center`, blockquote's 40 px), presentational
  attributes, rules by specificity then order, inline, important rules,
  inline important. Runs: colour (`windowtext`/`auto` reset it), font family
  (first non-generic; generics map to Times New Roman / Arial / Courier New),
  size (half-points, 1..1638 pt), weight, style, decoration, vertical align,
  an inline element's background as run `w:shd`, `mso-highlight` (Word's 17
  names only, `highlight::canonical_name`), `display:none` /
  `mso-hide:all` as `vanish`. Under a heading an explicit not-bold is
  `<w:b w:val="0"/>`. Paragraphs: alignment, line
  height (multiple → `auto`, length → `atLeast`, `exact` with
  `mso-line-height-rule:exactly`; none is single, #1979), the innermost
  block's own top/bottom box as spacing (with the user agent's margins and
  a browser's collapse, #1979), every enclosing block's left/right box summed as indent
  (negative clamps at 0), `text-indent`, a block's background as `w:shd`.
- Boundaries: `html`/`body` box and background are the page's, never read; a
  `td`/`th` starts a fresh indent, right indent, first line and shading (its
  padding is the cell's `tcMar`, below), while inherited run properties and
  line height pass in; `table` boxes are not counted. A list's box is
  counted (#1979): see "HTML import Outlook mail" for its items' indent. A
  `MsoList…` paragraph (never an `li`) keeps its own CSS.
- Malformed input: a block (and `ul ol dl table li dt dd …`) closes an open
  `p`, an `li` the open `li` of its list, `dt`/`dd` an open `dt`/`dd`, never
  across a cell or table, ending the paragraph only if it has content; a
  list's end ends its open `li` and any list left open in it (`</ol>` over
  an open `ul`), a `</li>` under an open list is ignored (list-item scope),
  and any close ends the blocks and lists opened inside the element
  (`Elem::blocks_at`/`lists_at`, the stacks below it). A row group's end,
  like its start, ends the open cell and row. Omitted table
  end tags are implied (`close_open`, through `close_tag` so the cell or row
  ends in the document): a `td`/`th` ends the open cell of its row, a `tr`
  the open cell and row, a `thead`/`tbody`/`tfoot` those and the open row
  group, never across a nested table. HTML5 containers (`section article main
  header footer nav aside figure form …`, `Kind::Container`) and `hr` close
  an open `p` too; a container (or a custom tag over an open paragraph)
  ends the paragraph when it closes. `html`/`body` never close, and no close
  reaches across an open table or cell (`out_of_scope`: `</ul>` or
  `</div>` in a cell is ignored, as in a browser). An `Elem` keeps where its
  attributes are in the page, so looks can be computed again (`restyle`):
  an inline element closed under an open block (`<b><p>x</b>y`) is taken
  out alone and what is above it restyled, so it ends at once. Any other
  truncation (not of a table or cell) reopens the HTML formatting elements
  left open above it (`a b big code em font i nobr s small strike strong tt
  u`), as a browser's active formatting elements: `<p><b>a</p><p>b` keeps
  `b` bold, `<b><i>x</b>y` keeps `y` italic. None is reopened when three
  alike (tag and attributes) are already open below, back to the nearest
  table or cell (Noah's Ark), so an unclosed `<b>` per paragraph does not
  pile up. Under a heading only a not-bold declared on it or inside it
  is the explicit off (an ancestor's `font:` shorthand is not), and
  negative top/bottom boxes write 0. Known limits: quirks mode's table-font reset
  (browsers do not inherit font size/weight/style into a `table` there; we
  do), no borders, no `dir`/`letter-spacing`/`text-transform`, no `+ ~
  [attr] :pseudo`. The suite does not draw run shading yet.

## HTML import images (#1880)

- `import::import_html_with(bytes, &mut resolver)` converts `<img>`;
  `import_html` is unchanged (no images: a `Document` alone holds no media).
  `HtmlImport { document, media }`: pictures name import-local rids
  (`rIdHtmlImage<n>`), and `HtmlImport::package` puts each medium in a
  Markdown package as `word/media/imageN.<ext>` and renames them.
- `html_image`: a `data:` src is decoded in house (percent, then base64
  when the header ends `;base64`, white space skipped); any other src goes
  to `ImageResolver` (`FnMut(&str, usize) -> Option<Vec<u8>>`), once per
  distinct src, at most `MAX_IMAGES` (1000) calls, told the most bytes it
  may return (budget room, `MAX_IMAGE_BYTES` 64 MiB, what `MAX_MEDIA_BYTES`
  128 MiB leaves); a longer answer is dropped. Kind and extension come from
  `image::sniff` only (PNG/JPEG/GIF/BMP); equal bytes are one medium.
- Size (twips, ×635 to EMU): CSS `width`/`height` lengths over the
  attributes, `auto` clears a side, a CSS percentage leaves the attribute's,
  an attribute percentage is ignored; one side gives the other by the
  pixels' aspect, none the pixels; scaled down to the body (11790 twips,
  #1928).
  Only `img` reads `height`.
- Unresolved (refused, no room, not an image): sized, a picture over one
  shared 1×1 `#F2F2F2` PNG with an `A6A6A6` 1 px line and the `alt` as
  `wp:docPr descr` (colours by estimate, unmeasured); unsized, the `alt`
  text as a run; neither, nothing. Hidden (`display:none`, `mso-hide:all`),
  zero-sized and list-marker (`<![if !supportLists]>`) images make nothing
  and are not resolved. Word's `<![if !vml]>` `<img>` is read (its VML
  twin sits in a comment); nothing else in a downlevel block is.
- A picture is paragraph content (`Builder::has_object`: not cleared as
  white space, ends its paragraph before a block), and
  `take_leading_marker` never drains an object.
- The suite's converting child (`convert_child::convert_bytes`, which now
  takes the page's path) resolves only a relative src under the page's
  folder, as Word, Outlook and Save Page write them (`page_files/…`;
  `image_src_parts`, #1880 r1, a deliberate tightening of the plan's
  absolute and `file:` paths): no URI scheme (`file:` included), no
  absolute, rooted, drive, UNC or device path, no `:`, `?`, `*`, NUL (or
  `<>|"`) once escapes are decoded, no `..` out of the folder, no name
  ending in a dot or space. An absolute path is refused because looking at
  one can reach the network (autofs or macOS's default `/net/<host>`,
  `/afs`), as Windows' `\??\UNC\…` does. The page folder is made absolute
  (`std::path::absolute`), and `read_local_image` walks each step below it
  with `symlink_metadata`, refusing a symbolic link or (Windows) any
  reparse point, then reads a regular file within the limit, its size
  checked first. The page folder itself may be on a share or behind a link.
  So opening a page never reaches the network. The child writes the
  package with its media (`ConvertedDoc::docx`).
- Out of scope: `<base href>`, MHTML, `<a>` around images, `srcset`,
  `<picture>`, background images, SVG/WebP (placeholders), `max-width`,
  float/align, the suite drawing a placeholder's alt text.

## HTML import rounded images (#1926)

- `border-radius` on an `<img>` is the picture's crop, written as Word's
  Picture Format > Crop to Shape writes it, the picture's
  `pic:spPr/a:prstGeom` (`image::inline_picture_run_shaped`,
  `image::PictureShape`): `ellipse` (`<a:avLst/>`), or `roundRect` with
  `<a:gd name="adj" fmla="val N"/>` (the corner is `min(w, h) * N / 100000`,
  N in 0..=50000). No radius, zero, or corners that differ write `rect`, the
  run exactly as before. So it saves unchanged (the run is `Inline::Raw`),
  Word and LibreOffice draw it, and the suite draws it (and a Word
  document's own oval or rounded crop) clipped
  (`docs/claude/suite-document.md`, "Suite inline pictures").
- The cascade: only an `img` reads `border-radius` and the four longhands
  (`Cascade::radius`, per corner, top left, top right, bottom right,
  bottom left; not inherited, so a card's radius never reaches the picture
  in it). Only the horizontal radius counts: the shorthand's part before
  `/` (`css::radius_corners`), a longhand's first value
  (`css::radius_corner`); the vertical part is checked and dropped. A
  negative or unreadable value is invalid (ignored, the earlier one stands);
  `initial`/`unset` is square. `Cascade::finish` resolves the corners with
  the element's final font (`image_radius`: an `em` declared before the
  `font-size` takes the new size) into `Look::own_radius`
  (`html_image::Radius`: `Pct`, `Twips`, or `NonUniform`; a corner left
  unset is a square one, and zero lengths and zero percentages are equal).
- The mapping (`html_image::shape`), measured in the box the page declares
  (`html_image::declared_box`, before `fit_to_body` scales a wide picture
  to the body; the scale is uniform, so the ratio holds): a percentage of
  50 or more is the ellipse, square or not; under 50 a `roundRect` of the
  corner's short radius, `p% * min(w, h)` (adj `p * 1000`); a length on a
  square box reaching half its side is the ellipse (a browser clamps there),
  else a `roundRect` of `r / min(w, h)`, at most 50000 (a browser's
  stadium). The unresolved placeholder takes the shape too; its outline
  follows it in Word.
- Every other element's `border-radius` (a card's cell, `div`, `span`) is
  ignored as before: its background and padding import, its radius is
  never text.
- Known limits: corners that differ (`8px 8px 0 0`, Word's
  `round2SameRect` and others not mapped), elliptical corners (a browser's
  non-square percentage corner is `p%w x p%h`; the long radius is lost),
  a wrapper's `overflow: hidden` rounding the image inside it, a rounded
  card background, PDF export and the editable-HTML page drawing the crop.

## HTML import nested cards (#1927)

- E-mail digests nest "cards": shaded, padded `td`s and `div`s two or three
  deep. A `td`'s own fill and padding were already its `w:shd` and
  `w:tcMar` (#1878). A `div` with its own background inside a table cell
  is a card: a synthetic one-cell table through the builder's HTML table
  path (`Html::begin_card`, `Builder::begin_table`/`begin_cell`), so it is
  laid out top-down in its enclosing cell's width less that cell's
  margins, like any nested table. Its `tcPr`: `w:shd` of its background,
  `w:tcMar` of its padding (all four sides, a side not given 0, as in a
  browser), `vAlign` top. Its `tblPr`: `tblW` of its CSS `width` (absolute
  dxa, percentage pct; none is `5000 pct`), `jc` from its place (its auto
  margins, else its container's `-webkit-` alignment: `own_place`, #1922;
  `align=center` on the div itself centres its content, not it),
  `tblCellMar` 0, no borders. Paragraphs in it start a fresh block look
  and structure, as in a `td`: no `w:shd`, indent 0, no heading or list
  item from outside.
- Deciding, one cascade: `open_tag` asks `look_of` with `CardMode::Maybe`
  for a `div` whose innermost open table part is a cell (not a `div`
  straight in a `table`, row group or row: a browser moves it out, and the
  builder's innermost table has no cell open; #1927 r1), when visible, its
  `Elem` and `blocks` entry are pushed (`MAX_DEPTH`), the builder holds the
  enclosing table and one more fits (`table_depth < MAX_TABLE_NESTING`).
  `Cascade::finish` makes it a card (`Look::own_card`) when it has a
  background, is not hidden (`run.vanish`, inherited too) and its own
  `display` is `block` or unset (an inline or inline-block div is none).
  `background: none|transparent` clears an earlier background for a
  candidate, as for table parts. Its `width` (`block_width`, a percentage
  too) resolves in `finish` with its final font, as #1922's fixed-width
  centring does (which ignores a percentage). The `Elem` keeps `Yes(depth)`
  (the builder's `table_depth` of its table) or `Declined` (`Elem::card`),
  which `restyle` passes back, so a restyle never flips a card whose table
  is begun.
- Ending: `end_cards_from(i)` ends every card still open in `elems[i..]`,
  innermost first, its open paragraph with the stacks intact, then its row
  and table into the enclosing cell. Called from `close_elem` before it cuts
  `blocks`/`lists`, from `truncate_elems` (so `implicit_close`, which cuts
  after it), at the top of `close_tag` for a table part's name (the cards
  above the innermost real table or cell: a browser's `</td>`, `<td>`,
  `</tr>`, `</table>` or a row group's end closes a div left open) and at
  the end of the page. A card is `Kind::Block`, not a table, so `</div>`
  across a real table in it is still ignored and an `<li>` closes an open
  `li` across it (div is no scope boundary). A real table left open in a
  card at the end of the page ends with it (only tables the builder holds:
  one past the nesting limit is text). Before the end, a builder table
  above a card with no `Elem` (opened past `MAX_DEPTH`) owns its own
  close: a table part's close leaves the card open, and a card whose `div`
  goes first waits as an orphan (`Html::orphan_cards`) until that table's
  `</table>` brings `table_depth` back to it (`table_closed`, #1927 r2).
- Empty cards: one with nothing in it (no text after the white-space rule,
  `&nbsp;` too, no object, no table; only empty paragraphs) is dropped
  (`Builder::end_card`): an e-mail's 1 px divider `div` makes nothing, as
  before, not an empty shaded row.
- Not cards (as before): a `div` outside any table (paragraph shading and
  indent), any other block (`p`, `blockquote`, headings), padding without
  a background, hidden ones, past `MAX_TABLE_NESTING`.
- Known limits: a card's side margins other than `auto` and its vertical
  margins (stacked cards are one empty paragraph apart); `max-width`;
  `border-radius` (#1926) and borders on a card; a card in a list item or
  heading loses the item's indent and the heading level; a background
  `div` outside tables stays paragraph shading. The suite's document view
  does not draw `w:tcMar` (a fixed inset), so imported padding is in the
  model but not on screen there.

## HTML import links (#1879)

- `<a href>` is an `Inline::Hyperlink` (`raw: None`, no `rel_id`): an
  absolute `http`, `https`, `ftp` or `mailto` URL as `target`, `#name` as
  `anchor` (percent-decoded; `#` alone is no link). `link_of` reads the
  `href` (references already decoded by `attrs_of`) as a browser's URL
  parser does: leading and trailing controls and spaces trimmed, tabs and
  line breaks inside removed (`java&#9;script:` is `javascript:`), then an
  allow-list (`LINK_SCHEMES`), so any other scheme however spelled
  (`javascript:`, `vbscript:`, `data:`, `file:`, `about:`), a relative
  `href` (`page.html`, `/x`, `//host/x`, `?q`), an empty one or one holding
  any other control character leaves plain text, as before. The scheme is
  lowercased; space, `"`, `<`, `>` and `` ` `` are percent-encoded (the RTF
  export quotes the target in `HYPERLINK "…"`). Over `MAX_HREF` (4096 bytes,
  encoded) is no link; past twice that the `href` is refused unread.
- The link is part of the computed `Look` (`link: Option<Rc<Link>>`, shared
  so a clone is O(1)), set by `Cascade::presentational` for an `a` (none
  for an `a` that is no link) and inherited, so `restyle`, mis-nesting and
  the formatting-element reopen (`a` is in `FORMATTING`) carry it: a link
  across `<p>` boundaries is a link in each paragraph. An `a` closes the
  open `a` first (`close_tag("a")`, a no-op with none open or across a
  table or cell), and the formatting open inside it is reopened under the
  new one, as the adoption agency does.
- `Builder::link_text` writes text in a link: it joins the last inline when
  that is a link the importer made to the same place (`Link::made`), so two
  adjacent `<a>`s to one target are one link; a different target never
  joins. The owed space keeps the link it was owed in. A break (`br`, a
  `pre` line), tab (`pre` or Word's `mso-tab-count` span) or picture inside
  a link is written outside it: the link goes on after it as another link
  to the same place, so every imported link is a runs-only one. An
  unresolved, unsized image's `alt` text is the link's text.
- A link has the user-agent look (`underline`, `LINK_COLOR` `0563C1`, Word's
  Hyperlink colour) as direct formatting, set before the page's rules, which
  override it (Word's own `a:link` rule is a pseudo-class `css.rs` drops; a
  page's plain `a` rule applies). `color: inherit` / `currentcolor`
  (`Color::Keep`) now gives back the parent's colour
  (`Cascade::parent_color`) over a user-agent or presentational one.
- For the empty-paragraph rule a link counts by its text (`is_object`
  excludes `Inline::Hyperlink`: `<p><a href=u>&nbsp;</a></p>` is an empty
  paragraph), and `take_leading_marker` never cuts into a link.
- Relationships: none are made at import. `save_package` mints an external
  relationship for every target-only link (`collect_external_links`, cells
  too), so the Markdown package `HtmlImport::package` builds, the suite's
  converting child and an `import_html` caller's save all keep the URLs.
- Known limits: CSS text decorations propagate in a browser (a
  `text-decoration:none` span inside a link still shows the link's
  underline), but here underline is an inherited run property, so that
  span's text loses it. No bookmarks for `<a name>`/`id` (an anchor link
  has nowhere to jump yet), no `Hyperlink` character style (`w:rStyle`; the
  Markdown package defines none), no `<base href>` or relative links, no
  hyperlinked pictures, no `title` (`w:tooltip`) or `target`
  (`w:tgtFrame`).

## HTML import page margins (#1928)

- An HTML import ends in its own final section
  (`html_table::page_section`, set in `import_html_within` after
  `Builder::finish`, so every caller gets it: `import_html`,
  `import_html_with`, the converting child): US Letter written out
  (`w:pgSz` 12240 x 15840, so no reader's locale makes it A4) and
  Outlook-like margins, 15 px (`PAGE_MARGIN_TWIPS`, 225 twips) on every
  side instead of Word's 1": `w:pgMar` with all seven attributes, header and
  footer 225, gutter 0. A browser's body margin is 8 px; Outlook's reading
  pane is about 10-15.
- `BODY_TWIPS` is that page's body, `PAGE_W_TWIPS - 2 * PAGE_MARGIN_TWIPS`
  (11790, 786 px): outermost tables, wide pictures and fixed-width
  centring are laid out in exactly the body the section gives, so a 600 px
  card sits in a 786 px body as in a mail client. RTF keeps its own even
  6.5" grid (`import/mod.rs`) and, like PDF and recovered text, no section
  of its own: the package's default page.
- Where it shows: Print Layout (`final_page_geom`). A Web Page still opens
  in Web Layout (`page_view` is off by default), which, like
  `ext::DocView`, keeps its fixed 48 / 28 px padding and never reads page
  margins (#1895, `docs/claude/docx-editing.md`).
  `uiharness/cases/html-page-margins.uit` reads the ruler's insets after
  View > Print Layout.
- A later margin change (Page Setup > Margins via `apply_page_setup`,
  Layout > Margins via `edit_section_setups`, a ruler drag via
  `set_page_margins`) keeps header, footer and gutter: `SectionSetup::apply`
  writes all seven `w:pgMar` attributes from the parsed margins, and only
  Page Setup's own fields change those three.
- Known limits: 15 px is inside most printers' unprintable band (about
  0.17-0.25"), so Word warns about the margins when printing; a page's own
  `@page` margins (Word-saved HTML's `@page WordSection1 {margin: ...}`)
  are not read, as `css.rs` skips every at-rule; every import gets the same
  fixed page.

## HTML import Outlook mail (#1979)

A simple mail from Outlook on the web, new Outlook or classic Outlook
imports as a browser lays it out. Slice 1 of #1979: the model. The
suite's flow view drawing it (marker look, spacing, line height) is
#1986 (`docs/claude/docx-editing.md`), and `ext::DocView`'s page margin
is #1987.

- Spacing comes from CSS only. Every HTML paragraph writes `w:spacing`:
  - `before`/`after` from its innermost block's top/bottom box, else 0.
  - `line 240 auto` unless `line-height` says otherwise (`normal` and an
    unreadable or non-positive value are single).
  - So no Normal style's space after or 1.08 lines shows. The suite's
    Styles gallery (`gallery_style_selected`) therefore shows an imported
    `div` paragraph as No Spacing.
  - A heading keeps its Heading style; its direct spacing overrides the
    style's.
- The user agent's vertical margins (`Cascade::user_agent`,
  `ua_vmargin`) apply where a side is not declared
  (`margin_declared`, per side, so `margin-top` alone keeps the UA bottom):
  - `p`, `blockquote`, `pre`: 1em.
  - `h1`-`h6`: 0.67, 0.83, 1, 1.33, 1.67, 2.33em. That em is the
    heading's own declared size, else its UA size (2, 1.5, 1.17, 1, 0.83,
    0.67 × the parent's). So an `<h1>` under 12pt is 322 twips.
  - Every other block (`div`, `li`, `dd`, ...): none.
  - `Look::ua_before`/`ua_after` keep the UA part, for the quirk below.
- Adjacent margins collapse as a browser's do (Word and LibreOffice add
  them; the suite adds them too):
  - Only margins collapse, not padding (#1979 r1). A paragraph's written
    `before` is `pad_top + max(0, margin_top − the margin_bottom of the
    paragraph before it in its flow)` (`Html::flow_spacing`, `Look`'s
    `margin_before`/`pad_before`/`margin_after`). The paragraph above
    keeps its whole `after`. A negative top margin is only summed with
    the padding.
  - The margin above is the flow's own (`Builder::tail`, a `FlowTail` set
    with `set_tail` after each paragraph, #1979 r2). The flow is the open
    cell's blocks or the body. A tail counts only while its paragraph is
    still the flow's last block, so there is no collapse across a table,
    into a cell's or card's first paragraph, or from a dropped empty
    card's paragraph.
- Quirks mode (`quirks_mode`: no doctype, a non-`html` one, or the HTML
  Standard's old public ids, its common entries; `<!DOCTYPE html>` and
  XHTML are standards). Mail, Word and Outlook pages have none, and so do
  clipboard fragments.
  - The margin quirk: a block straight in `body` or a `td`/`th` (its
    parent element is `html`, `body`, `td` or `th`: `Html::para_edges`;
    not in a `div` or a card) loses its UA top margin when it is its
    flow's first, and its UA bottom margin when it is its flow's last.
  - The last is settled at the flow's end (`Html::flow_end_props`, at
    `</td>`, `</tr>`, `</table>`, a card's end and the page's end). The
    text in progress is ended first, as any paragraph, and then the flow's
    last paragraph (`FlowTail::quirk_bottom`) is patched in place. The
    props it returns are only for the paragraph an empty cell gets.
  - A declared margin and padding stay.
- List geometry, through the cascade:
  - `ul ol dir menu` have a UA `padding-left: 40px`. `Kind::List` adds
    its left and right box to `Look::indent`/`indent_right`, so an item's
    text is at 600 twips per list (1200 nested), `<style>` rules and
    `restyle` included.
  - Every HTML item paragraph writes that `w:ind`, numbered or not. A
    numbered one at the margin writes `ZERO_IND` (`w:left="0"
    w:hanging="0"`, set as its load shadow, so an edit replaces it), or
    Word and LibreOffice would take the level's 720 (#1979 r1). The suite
    draws an item with a left of its own (600 per list, or that pin) with
    no level step, so a nested list sits at its own indent (#1986).
  - A numbered paragraph hangs a browser's outside marker with its space:
    `DECIMAL_HANG` 1.1125em for an `ol`, `BULLET_HANG` 1.375em for a
    `ul`, measured in Chrome. The em is the `li`'s own font (`item_elem`),
    not a `div`'s in it: 267 / 330 twips at 12pt.
  - The hang is clamped to the indent: `padding-left: 0` hangs nothing.
    Word draws the number at `left − hang` and tabs to `left`.
  - The Markdown numbering's own `w:ind` (720 per level) is overridden.
- Item continuations (`end_para_with`): once an item has ended its
  numbered paragraph, every later paragraph in it is an unnumbered
  continuation at its text indent, `first_line 0`. That covers
  `<li><div>a</div><div>b</div></li>`, `<li><p>a</p><p>b</p></li>`, and
  text after a nested list or table in the item.
  - The first paragraph with a line (text, a break, an object or a typed
    marker) takes the number and sets `item_numbered`.
    - An empty paragraph before it holds the number in the meantime
      (`Block::item_held`, its flow's depth and index). The line's
      paragraph takes the number from it (`release_number`), leaving the
      empty one an unnumbered paragraph at the text indent.
    - So `<li><p></p><p>x</p></li>` numbers x, and
      `<li><p></p></li><li>b</li>` still counts the first item: b is 2,
      as in a browser (#1979 r2).
    - `<div><br></div>` is a line, so it keeps the number.
  - A heading in an item is numbered too, keeping its Heading style
    (#1979 r2). A typed-marker heading is unchanged.
  - The shared steps are in `number_item`: number, level, the `li`'s
    mark, and the zero indent.
  - A flow's end ends its text in progress as any paragraph
    (`flow_end_props` calls `end_para_if_any`, #1979 r1). So `<ol><li>a<li>b`
    at the page's end and `<td><ul><li>x</td>` are numbered too.
  - An `li` is an HTML item whatever its class. Classic Outlook's
    `<li class=MsoListParagraph style="margin-left:0in">` sits at the
    `ul`'s 600 with a hang, and the blocks in an item are never
    `mso_list`. Filtered HTML's `<p class=MsoList…>` with typed markers is
    unchanged.
- The paragraph mark (`mark_rpr`, through `serialize::rpr_xml`): a
  `w:rPr` in `raw_props`, written only when one of these is set: font,
  size, colour, bold, italic.
  - For a numbered item it comes from the `li`'s look, as a browser's
    `::marker` (an item with none writes none); for any other paragraph,
    from its paragraph look's run.
  - No underline, strike, shading, highlight, `vanish` or link look.
  - Word and LibreOffice draw the number in it, and an empty
    `<div><br></div>`'s line in the div's size. It saves and loads
    unchanged.
- Tests: `outlook_tests` in `import/html.rs`; the issue's mail is
  `outlook_simple_mail_imports_as_a_browser_lays_it_out`.
- Known limits:
  - The lists' own vertical margins (1em when not nested).
  - Parent-child margin collapse (a `div`'s bottom with its last `p`'s,
    through a card's top).
  - An empty `<p></p>`'s margins collapsing through it.
  - `<div>text<br></div>`'s trailing break.
  - A number wider than its hang (10 and up).
  - `list-style-type`; `<ol start>` and separate `<ol>`s sharing numId 2.
  - A page's `em` margin on a heading resolves at the parent's size, as
    before.
  - The HTML Standard's rarer quirks public ids and the IBM system id.
