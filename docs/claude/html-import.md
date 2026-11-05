# HTML import

Notes moved from `CLAUDE.md` (#1903). Read them before changing `docxcore::import::html`: tables, CSS, images and links. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

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
  are known, else the room it has (6.5" outermost). Declared columns
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
  `grid_span`/`v_merge` match, so a save writes them unchanged.
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
  one are unnumbered continuations at the level's indent (`Block`'s
  `item_numbered` / `item_split` on the `li`'s own entry, `open_item`, not
  across a cell), so the split adds no item and a nested list's items
  leave their parent's flags alone; a picture opening its item carries the
  number.
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
  `mso-line-height-rule:exactly`), the innermost block's own top/bottom box
  as spacing, every enclosing block's left/right box summed as indent
  (negative clamps at 0), `text-indent`, a block's background as `w:shd`.
- Boundaries: `html`/`body` box and background are the page's, never read; a
  `td`/`th` starts a fresh indent, right indent, first line and shading (its
  padding is the cell's `tcMar`, below), while inherited run properties and
  line height pass in; `ul`/`ol` and `table` boxes are not counted (the level
  indent stands for the list's). An HTML `li` keeps the level's hanging
  indent: with an enclosing box it writes `720*(lvl+1) + box`, first line
  −360, else no indent. A `MsoList…` paragraph keeps its own CSS.
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
  pixels' aspect, none the pixels; scaled down to the body (9360 twips).
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
