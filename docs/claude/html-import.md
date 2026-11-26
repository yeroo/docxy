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
  are known, else the room it has (outermost, the page's body,
  `Page::body`, #1944; 11790 twips on #1928's default page). Declared columns
  narrower than a declared table grow to it; wider ones widen the grid
  (tblW stays as declared). `tblW` is dxa, pct (fiftieths) or auto; every
  cell's `tcW` is its columns' sum. Web Layout and `ext::DocView` fit a pct or auto
  table to the view's width when they draw it (#1895,
  `docs/claude/docx-editing.md`); the model keeps these widths.
- `tcPr`, in schema order: `tcW`, `gridSpan`, `vMerge`, `tcBorders` (its
  CSS border, #1894), `shd` (the cell's
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
  `cellspacing`, unmeasured against Word; none when collapsed, #1894),
  `tblBorders` (`border=N`: outer `single` N px × 6 eighths clamped 2..96,
  inside 1 px; a bare or unreadable `border` is 1; `0` or none writes none,
  the layout-table case; CSS borders, `frame`, `rules` and `bordercolor`
  write them too since #1894, below),
  `tblCellMar` (`cellpadding`, also 0). `raw_tcpr`/`raw_tblpr` hold them,
  `grid_span`/`v_merge` match, so a save writes them unchanged. A `div`
  with a background in a cell is a one-cell table of its own: see "HTML
  import nested cards (#1927)". Two tables back to back in a cell (nested
  or cards) get an empty paragraph between them, or Word joins them
  (`end_html_table`); in the body too since #1940 (a table or card right
  after another, an empty card dropped between them making none).
- Known limits: background images; a table directly in a `tr`/`table` (no
  cell) joins the next cell (a browser puts it before the table). CSS
  borders, `frame`/`rules`, `border-color`, row heights, `<col>` widths,
  `max-width` and `th` came with #1894 (below).

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
  is not its place), except that in quirks mode a table starts its text
  at its start (`table { text-align: start }`, #1894 r2): an ancestor's
  alignment, `-webkit-` ones and their block placing included, stops at
  the table, whose own place is still its parent's.
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
  `max(0, (body - indent - indent_right - width) / 2)` more indent on
  each side, after its own box is added: a content box, so its padding is
  outside the width (`body` is the page's, `Page::body`, #1944;
  `BODY_TWIPS`, 11790, on #1928's default page). Its text alignment is
  unchanged. Inside a cell or a
  card, centred in the cell's inner width when the table is laid out, only
  in a cell whose width the page declares (#1930: see "HTML import
  centring in cells").
- Drawing: the suite draws a `w:jc` table where it sits (#1922,
  `docs/claude/suite-document.md`).
- An outermost table inside a centred fixed-width block is laid out in
  that block's width and placed in it (#2114: see "HTML import margins,
  breaks, hangs and wrapper tables").
- Known limits: a table in a merely
  indented block keeps the body's width; a percentage `max-width` (an
  absolute one came with #1894);
  CSS `float`; `<img align>`; `box-sizing`;
  right-to-left.

## HTML import centring in cells (#1930)

- A cell's width is known only in `html_table::layout`, after its
  paragraphs exist. So in a cell (`Look::in_cell`, a card too) the
  fixed-width branch pushes an `html_table::CellBox` (its width, the look's
  indents with its own box in) onto `Look::cell_boxes` (inherited, cleared
  where a cell or card resets the indents) instead of indenting.
  `end_para_with` hands each paragraph ended inside one to
  `Builder::mark_cell_boxes` (a `CellCentring`, at its index of the cell's
  blocks) only when `end_para` really pushed it (the budget can refuse it).
  That is the only hook: every cell, row and card end takes its props from
  `flow_end_props`, which ends the text through `end_para_with` first.
  `end_cell` moves the records into `RawCell::boxes` (`absorb` shifts them,
  as `nested`).
- `layout` centres each paragraph in the cell's `inner` width (the one a
  nested table gets): box by box, outermost first, each in the room the
  enclosing ones' centring leaves, the body-level formula with `inner` for
  the page's body. A box wider than its room gets nothing. The indent stays
  at least 0 as at body level, and `ZERO_IND` goes once it moves.
- Paragraph lines: `Look::border_left_boxes` / `border_right_boxes` count
  the boxes the block drawing each line is in. A line moves with those
  boxes' centring only, so a line from a block outside the centred box
  keeps its place and its `w:space` grows (Word's 31 pt clamp), as at body
  level (`html_table::line_space`, shared with `par_props`). A cell as wide
  as the body gives what the body gives (`html_tests_1930.rs`
  `same_as_body`).
- Only in a cell whose width the page declares (`layout_in`'s
  `declared`, r1): the table's own width (a card's always), its room, the
  cell's own width or `<col>`s over all its columns; a percentage only of a
  declared table or cell. An auto table takes the whole room in
  `column_widths`, but a browser shrinks it to its content (the box, at
  the left), so its cells' boxes stay where they are; so do those of a
  nested table in such a cell.
- Known limits: a table inside a centred box in a cell still takes its
  cell's width (`centred_box` is body-only). A list item's hang is clipped
  against its indent before centring, so a list with an indent smaller
  than its hang (`padding-left: 0`) in a centred box in a cell starts at
  its box's edge with no hang.

## HTML import CSS (#1872)

- `docxcore::import::html` reads inline `style`, `<font color face size>` and
  every `<style>` block of the page (a pre-pass, `prescan`, outside
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
- Boundaries: `html`/`body` box is the page's, never read, and their
  background is the page colour (#1894, below), never shading; a
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
  negative top/bottom boxes write 0. The quirks table-font reset, block
  borders, `dir`, `letter-spacing`, `text-transform`, `visibility`,
  `white-space`, page breaks and `+ [attr] :first-child :link :root`
  came with #1883 (below); the suite draws run shading since #2249.

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
  `image::sniff` only (PNG/JPEG/GIF/BMP), but for SVG and WebP made PNG by
  a caller's converter (#1898, below); equal bytes are one medium. Since
  #1898 the src is resolved against the page's `<base href>` (or an MHTML
  root's location) first, and an MHTML part answers before the resolver.
- Size (twips, ×635 to EMU): CSS `width`/`height` lengths over the
  attributes, `auto` clears a side; a percentage width (CSS or attribute)
  is of the containing width and a percentage height is `auto` (#1898,
  below); one side gives the other by the pixels' aspect, none the
  pixels; `max-width`/`max-height` clamp it (#1898); scaled down to the
  page's body (`Page::body`, #1944; 11790 twips on #1928's default page).
  Of the rest, only `tr`, `td` and `th` read `height`, as a row height
  (#1894).
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
- Out of scope here, done since: `<a>` around images (#1999); `<base
  href>`, MHTML, `srcset`, `<picture>`, SVG/WebP, percentage sizes,
  `max-width`, the suite drawing a placeholder's alt text (#1898, below).
  Still out: background images, float/align (#1898's "Not done").

## HTML import images, leftovers (#1898)

- Percentage widths: an `<img>`'s `width="50%"` or CSS `width:50%` is a
  `Width::Pct` in its look, resolved in `Html::image` against
  `Html::image_room`, at import time (no layout refit): outside a cell the
  body less the paragraph look's `indent`/`indent_right` (so a centred
  `max-width:600px; margin:0 auto` wrapper, which #1922/#1894 turn into
  indents, gives 600 px); inside one the innermost cell's (or card's,
  #1927) content width less the indents of the blocks inside it, walked
  outside in: a table is its own width (a percentage of the room around
  it), else that room; a cell its own width (a percentage of its table's),
  else its table's, less its `own_padding` left and right (not the table's
  `cellpadding`); a card its own width, else the room around it, less its
  padding. Every step is clamped to the body. A cell of a many-column
  table without widths is taken as its whole table (an upper bound; the
  fit to the body still caps it). A percentage `height` (either way) is
  `auto`, as a browser computes it against an auto-height container, so it
  clears the attribute's. Two expectations changed with it:
  `css_lengths_beat_the_attributes_and_auto_and_a_percent_is_of_the_body`
  and `cid-message.html`'s `width=150 style="width:100%;max-width:600px"`
  (now 600 x 200, as a browser draws it).
- `max-width` (absolute, or a percentage of the same room; `none` clears)
  and `max-height` (absolute; a percentage is none) on an `img`
  (`Look::own_max_width`, `own_max_height`) clamp the declared box before
  `fit_to_body` (`html_image::MaxBox::clamp`): a side over its bound takes
  it, and the other side scales with it only when it came from the
  aspect (or a placeholder's square), not when declared, as a browser
  clamps a replaced element. The crop (#1926) is measured in the clamped
  box. An unsized placeholder has no box to clamp: its alt text stays a
  run.
- `<base href>`: the first `<base>` with an `href`, head or body, found
  by the `prescan` pre-pass (so it applies to an `<img>` before
  it; `skip_head` is unchanged), unless empty, over `MAX_BASE` (4096)
  bytes, `data:` or `javascript:` (`html_image::page_base`). Every src but
  a `data:` one is resolved against it (`html_image::resolve_ref`, RFC
  3986 section 5.2: a src with a scheme, `cid:` included, is itself) before
  the cache and the resolver. A relative base gives a relative src (a
  leading `..` it cannot remove is kept, and the suite refuses one that
  leaves the page's folder); an absolute one an absolute src, which the
  suite refuses. No base: srcs as before. `<base>` for links (`<a href>`)
  is not read.
- `srcset` and `<picture>`: an `<img>` whose `src` gives nothing (empty,
  refused, not an image) tries its own `srcset`, then the `<source>`s of
  the `<picture>` that is its innermost open element (`Html::picture`, by
  the picture's serial; at most `MAX_SOURCES` (16), each a `<source>`
  without `media` and with no `type` or a PNG/JPEG/GIF/BMP/WebP/SVG one,
  `html_image::usable_type`; each kept source's list parsed once, when it
  is kept, into `PictureSources`; the `<img>`'s own list only once its
  `src` gave nothing). `html_image::srcset_candidates` splits a
  list as the HTML standard's parser does (a URL runs to white space, so a
  `data:` URI keeps its commas) and orders it `1x`/bare first, then as
  written; at most `MAX_CANDIDATES` (16) tries, each a resolver call within
  `MAX_IMAGES`. The first that gives an image wins, its CSS size its
  pixels over its density (`2x`: half); a `w` descriptor counts as `1x`
  (`sizes` is not read). A `src` that resolves asks for no candidate.
- MHTML (`import/mhtml.rs`): `import_html_converting` (behind both
  `import_html_within` and `import_html_with_converter`) first tries
  `mhtml::parse`. A top header block (unfolded, within `MAX_HEADER` 16 KiB
  up to its blank line) whose `Content-Type` is `multipart/related` with a
  `boundary` is a message; its parts are split at `--boundary` lines (the
  line break before one is the delimiter's), at most `MAX_PARTS` (4096).
  The root is the part `start` names by Content-ID, else the first part of
  the `type` parameter's type, else the first `text/html` one, and must be
  HTML; it is decoded (base64, quoted-printable with soft breaks, else as
  it is; charged as decoded) and read in its part's `charset` when this
  reads it (`decode_with`: a byte-order mark first, then the transport
  charset, then the page's `<meta>`), its `Content-Location` the page's
  URL, which the page's `<base href>` and srcs resolve against. The import
  wraps the caller's resolver: a src that is a part's `Content-Location`
  (exactly, or both percent-decoded), or a `cid:` naming a part's
  Content-ID (percent-decoded, no brackets or case), takes that part's
  body (`Mhtml::part_for`: decoded then, refused unread when its decoded
  bound passes the image's limit); only a src no part holds reaches the
  caller (absolute, so the suite refuses it). Anything malformed is not
  MHTML: the bytes are read as a page. `sniff` takes MHTML by its header
  block (`mhtml::looks_like`) and by `.mht`/`.mhtml`; the suite's Open
  lists both. Parts other than images (stylesheets, frames) are not read.
- SVG and WebP: `html_image::convertible` tells them apart (`<svg` after a
  BOM, white space, XML declaration, processing instructions, comments
  and doctype within the first 4 KiB; `RIFF`…`WEBP`). `image::sniff` is not
  extended: its callers mean "gpui and PDF export can draw this". They go
  to a caller's `ImageConverter` (`FnMut(ConvertKind, &[u8], limit) ->
  Option<ConvertedImage>`: PNG bytes and the CSS size), given only through
  `import_html_with_converter`; without one (`import_html_with` and every
  other entry) they stay placeholders and every medium sniffs. With one, a
  WebP is a PNG medium; an SVG a PNG medium and the SVG kept as its own
  (`svg`, `image/svg+xml`), the run's blip naming the PNG with Word's
  `asvg:svgBlip` extension naming the SVG (`image::with_svg_blip`, over
  the `image::svg_blip_ext` that `mermaid_embed` uses too); `embed_tags`
  renames that `r:embed` in `package()`. Each medium keeps its CSS size
  apart from its sniffed pixels (`Images::css`; an SVG rasterised at 2x
  takes its intrinsic size, a srcset density divides it). Conversions are
  cached by source bytes. An answer that is not a nonzero-sized PNG within
  the limit is dropped (a placeholder).
- The suite's converter (`suite/docxy/src/image_convert.rs`, in the
  converting child): WebP through `image` (`webp` feature) within
  `Limits` (16384 px a side, 256 MiB); SVG through `resvg` 0.46 (gpui's,
  no package added to the lock) at twice its intrinsic size (its
  `width`/`height`, else its `viewBox`, else, as usvg sizes one with
  neither, its content's box from the origin), the long side at most 2048
  px. usvg's default `ImageHrefResolver` reads any file path that exists,
  absolute ones too, so the converter's resolves `data:` URLs only: an
  SVG's `<image>` never reaches the disk or the network. Its text uses the
  system fonts, loaded once on the first SVG.
- Not done: floating pictures (`float`, `align=left|right` as `wp:anchor`:
  the suite draws no anchored picture, so they would vanish from the view;
  they stay inline), CSS `background-image` and `<td>`/`<body
  background>` (no picture fill for a cell, block or page in the model or
  the suite), percentage `min-*`, `object-fit`, `<base>` for links, MHTML
  parts other than images.

## HTML import rounded images (#1926)

- `border-radius` on an `<img>` is the picture's crop, written as Word's
  Picture Format > Crop to Shape writes it, the picture's
  `pic:spPr/a:prstGeom` (`image::inline_picture_run_shaped`,
  `image::PictureShape`): `ellipse` (`<a:avLst/>`), or `roundRect` with
  `<a:gd name="adj" fmla="val N"/>` (the corner is `min(w, h) * N / 100000`,
  N in 0..=50000), and since #1936 the presets for corners that differ. No
  radius, zero, or corners no preset draws write `rect`, the run exactly as
  before. So it saves unchanged (the run is `Inline::Raw`), Word and
  LibreOffice draw it, and the suite draws it (and a Word document's own
  oval or rounded crop) clipped (`docs/claude/suite-document.md`, "Suite
  inline pictures").
- The cascade (`Cascade::radius`, per corner, top left, top right, bottom
  right, bottom left; not inherited, so a card's radius never reaches the
  picture in it): a negative or unreadable value is invalid (ignored, the
  earlier one stands); `initial`/`unset` is square. `Cascade::finish`
  resolves the corners with the element's final font (`image_radius`: an
  `em` declared before the `font-size` takes the new size) into
  `Look::own_radius`. Since #1936 every element reads it, each corner keeps
  its vertical radius, and it is `html_image::Radius([(RLen, RLen); 4])`
  (the #1936 section).
- The mapping (`html_image::clipped_shape` since #1936, which holds the
  rule), measured in the box the page declares (`html_image::declared_box`,
  before `fit_to_body` scales a wide picture to the body; the scale is
  uniform, so the ratio holds). Uniform corners map as #1926 did: a
  percentage of 50 or more is the ellipse, square or not; under 50 a
  `roundRect` of the corner's short radius, `p% * min(w, h)` (adj
  `p * 1000`); a length on a square box reaching half its side is the
  ellipse (a browser clamps there), else a `roundRect` of `r / min(w, h)`,
  at most 50000 (a browser's stadium). The unresolved placeholder takes the
  shape too; its outline follows it in Word.
- Another element's `border-radius` (a card's cell, `div`, `span`) crops
  nothing unless it is a clipping wrapper the picture fills (#1936): its
  background and padding import, its radius is never text.
- Known limits (#1926; most lifted by #1936, below): corners that differ,
  elliptical corners, a wrapper's `overflow: hidden` rounding the image
  inside it and the editable-HTML page drawing the crop are #1936's. Still
  open: a rounded card background and PDF export (see #1936's limits).

## HTML import corners that differ, rounded wrappers, the editable page (#1936)

- Presets (`image::PictureShape`): besides `ellipse` and `roundRect`,
  `round1Rect` (`Round1 { adj }`, the top-right corner only),
  `round2SameRect` (`Round2Same { adj1, adj2 }`: both top corners, both
  bottom ones) and `round2DiagRect` (`Round2Diag { adj1, adj2 }`: top left
  and bottom right, top right and bottom left), each guide in
  `0..=50000`, written as `<a:gd name="adj1" fmla="val N"/>`... and read
  by name (`image::guide_value`), the preset's default when absent or
  unreadable (`adj`/`adj1` 16667, `adj2` 0, ECMA-376's), clamped.
  `PictureShape::corner_radii(w, h)` gives each corner's circular radius,
  `min(w, h) * adj / 100000`, an ellipse half the shorter side: the one
  geometry the suite and docxwasm draw from.
- The cascade keeps each corner's vertical radius: the shorthand's part
  after `/` (each half expanded one to four values alone; none: the
  horizontal one) and a longhand's second value (`css::radius_corners`,
  `css::radius_corner`, `css::CornerRadius`). `image_radius` resolves both
  halves in the final font into `html_image::Radius` (`[(RLen, RLen); 4]`,
  `RLen::Pct` / `RLen::Twips`), `None` when every corner is square (a
  corner square in either half is square). Every element reads
  `border-radius` now (`Look::own_radius`, boxed, not inherited); it is
  still never text, and only an `img`'s or a clipping wrapper's counts.
- The mapping (`html_image::clipped_shape`, in the declared box): each
  corner `(rx, ry)` (a percentage of the width, of the height), then CSS's
  overlap clamp, one factor for all, the smallest `side / sum` under 1 of
  the four sides (each radius capped at 1e300, so a page's digits spelling
  infinity make no NaN). Every corner at half of both sides (within 1e-9)
  is the ellipse. Else each corner is circular, `min(rx, ry)`, as `adj =
  round(100000 * r / min(w, h))`, at most 50000: all 0 a rectangle, all
  equal `roundRect`, top pair and bottom pair each equal `round2SameRect`,
  the diagonal pairs `round2DiagRect`, only the top right `round1Rect`.
  Anything else is a rectangle: a left or right side pair, a lone corner
  other than the top right, three corners (none has a preset without
  flipping or rotating, which turns the picture too). So `8px 8px 0 0` is
  `round2SameRect`, `50% / 10%` on a square `roundRect` 10000 (it was the
  ellipse), and `border-bottom-left-radius: 8px 2px` with three 8 px
  corners a rectangle (it was `roundRect`: the corner really is 8 x 2).
- A rounded wrapper (`Cascade::clip_box`, `Html::wrapper_clip`): an `img`
  takes the corners of its nearest open ancestor with a non-square radius
  of its own when that ancestor clips: `overflow` (or `overflow-x` and
  `-y`) `hidden` or `clip` on both axes (`auto`, `scroll` are not a crop
  here); a block container (`Cascade::container`: a block, cell, list or
  HTML5 container unless its own `display` is not one; an inline element
  or a custom tag only with `display: inline-block`, `block` and the like,
  `block_container`); no border (`own_borders`) and no non-zero padding of
  its own; an absolute declared width and height (a cell's attributes or
  CSS, another element's CSS `width`/`height`, `Cascade::css_box`, in its
  final font; a percentage or `auto` is none) the picture's declared box
  matches within a CSS pixel, 15 twips, each way (`FILL_SLACK`,
  `Look::own_clip`, `Clip`). Every open element between the two is
  `Kind::Inline` (an `<a>` link avatar). The
  picture comes first in it: `Builder::shown` (bumped by
  `flush_marks(true)`: non-blank text, an object, a page break) is what it
  was when the wrapper opened (`Elem::shown_at`; `u64::MAX` for a reopened
  element), and the img and each element below the wrapper are their
  parents' first element child (`key.index == 0`: no `<br>` before it).
  A cell whose own CSS padding leaves a side to the table needs its
  table's `cellpadding` absent (a browser's 1) or at most 1
  (`thin_cellpadding`). Each corner is then the larger of the picture's
  own and the wrapper's, both in the picture's box (the intersection a
  browser shows; an img's `border-radius: 0` never undoes the clip). The
  nearest rounded ancestor decides: when it does not clip, an outer one is
  not looked at. No look-ahead: content after the picture in a wrapper
  its size is clipped out of view in a browser anyway (here it stays, as
  before).
- The editable-HTML page (`htmlbundle/web/`, no area file of its own):
  docxwasm's richdoc `img` segment has `"radius"`, a CSS `border-radius`,
  when the picture is cropped (`richdoc::crop_radius`): `50%` for an
  ellipse (a true oval in a browser), else the four `corner_radii` of its
  `cx` x `cy` in px (9525 EMUs a px) to a tenth, `"8px 8px 0px 0px"`.
  `app.js` puts it on the `<img>` as `style="border-radius:..."`
  (escaped); a rectangle's segment and markup are as before. No JS test
  runs, so `htmlbundle`'s tests check `app.js`'s `case 'img'` source.
- Known limits: a rounded card background (a card is a one-cell table
  with `w:shd`; Word has no rounded cell or paragraph shading); PDF
  export (it draws no inline body pictures at all, only a header's picture
  watermarks, so there is nothing to clip); Word's presets are circular,
  so an elliptical corner is its short radius; patterns with no preset are
  rectangles; `<img style="width:100%;height:100%">` in a fixed-size `div`
  wrapper is a rectangle (`image_room` resolves an img's percentage only
  against a cell or card, and a percentage height is none); the suite
  draws a non-square ellipse as a stadium (gpui's corners are circular),
  the editable page as a true oval.

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
- Not cards (as before): any other block (`p`, `blockquote`, headings),
  padding without a background, hidden ones, past `MAX_TABLE_NESTING`.
- Known limits: a card's side margins other than `auto` in a cell and its
  vertical margins (stacked cards are one empty paragraph apart);
  `border-radius` (#1926) and borders on a card; a card in a list item or
  heading in a cell loses the item's indent and the heading level (in the
  body and a body card, neither is a card: #1940). The
  suite's document view and PDF export do not draw `w:tcMar` (a fixed
  inset), so imported padding is in the model, Word and the saved file
  but not on screen there.

## HTML import cards in the body (#1940)

- A background `div` outside every table is a card too, through the same
  path (`open_tag`'s candidacy: no table part or card open and
  `table_depth` 0), when it is a box of its own: some padding above 0, a
  `width` or a `max-width` (`Cascade::own_box`, checked in `finish` while
  `look.in_cell` is still false). A background alone stays paragraph
  shading: new Outlook writes a white `div` per line
  (`elementToProof`, `background-color: rgb(255,255,255)`), which a
  table per line would ruin, and with no padding the shading is what a
  browser draws. So does one with a border line of its own on any side
  (`own_borders`, an alert's left rule, a boxed note): its paragraphs
  draw the lines (#1883), a card would drop them. In a cell, #1927's
  card as before. The whole newsletter in `newsletter.html` (a grey,
  padded `.wrapper`) is one card holding the content table.
- Guards (`Html::body_card_fits`), where the paragraph model carries
  something a card's table would drop, so the `div` stays paragraph
  shading: in a list (`in_list`: an item's number and indent), in a
  heading (the open block entry's `heading`: its level; r3), under a
  parent whose look has shading or side lines (a quoted reply's left
  rule, a shaded block that is no card), or under any open block with a
  top or bottom line of its own (`own_top`/`own_bottom`: Outlook's reply
  header). In a body card (and a card nested in one: the inherited
  `Look::body_card`, reset by a real `td`/`th`) a `div` takes the body's
  rule, own box, no own lines and these guards over what is open inside
  the card (`body_card_fits(from)`; r2): the newsletter's padded wrapper
  around an alert or Outlook's white lines. In a real cell, #1927's.
- Room: a body card is laid out in the room the blocks around it leave
  (`Look::card_room`, from the inherited indents plus its own side
  margins other than `auto`, set in `finish` before the card resets the
  look; `None` when all are 0), as an outermost table in a centred box
  is (#2114: `body_room`, shared with `table_spec`). A blockquote's
  40 px margins, a centred `max-width` wrapper and `margin: 0 24px` place
  it (`jc center` when it fills a centred room, else `tblInd`).
  `image_room` takes a percentage picture's width from the same room.
- `max-width` (absolute only, as for a block, #1894) is the card's
  table's `max_width`, in a cell too: `max-width: 600px; margin: 0 auto`
  is a 9000-twip table, centred.
- A table part's close (`</td>`, `</tr>`, `</table>`, a row group's) with
  no table part open is ignored, as in a browser: it never ends a body
  card or its row (`close_tag`). So is a stray `td`/`th`/`tr`/row group
  start there (`open_element`, r2): the card's cell keeps its fill
  (`Html::in_bare_card`, both).
- Known limits: a body card's vertical margins are lost (the shaded
  paragraphs kept them as spacing before and after; a table has none),
  and so is a `page-break-before`/`-after` on it (it lands in its cell,
  where Word ignores it). A padless, widthless background `div` still
  leaves a table inside it unfilled. Padding shows in Word, not in the
  suite or PDF (above).

## HTML import links (#1879)

- `<a href>` is an `Inline::Hyperlink` (`raw: None` unless it has a
  `title` or `target`, whose opener it keeps, #1912; no `rel_id`): an
  absolute `http`, `https`, `ftp` or `mailto` URL as `target`, `#name` as
  `anchor` (percent-decoded; `#` alone is no link). `link_of` reads the
  `href` (references already decoded by `attrs_of`) as a browser's URL
  parser does: leading and trailing controls and spaces trimmed, tabs and
  line breaks inside removed (`java&#9;script:` is `javascript:`), then an
  allow-list (`LINK_SCHEMES`), so any other scheme however spelled
  (`javascript:`, `vbscript:`, `data:`, `file:`, `about:`), a relative
  `href` (`page.html`, `/x`, `//host/x`, `?q`) with no `http`, `https` or
  `ftp` base to resolve against (#1912, below), an empty one or one holding
  any other control character leaves plain text, as before. Since #1912
  `#name` goes through `bookmark_name`. The scheme is
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
  to the same place, so every imported link is a runs-only one. A picture
  in a URL link carries the link itself, as its click link (#1999, below).
  An unresolved, unsized image's `alt` text is the link's text.
- A link has the user-agent look (`underline`, `LINK_COLOR` `0563C1`, Word's
  Hyperlink colour) as direct formatting, set before the page's rules, which
  override it (Word's own `a:link` rule applies since #1883, as a page's
  plain `a` rule does). `color: inherit` / `currentcolor`
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
  span's text loses it. Bookmarks, the `Hyperlink` style, relative links,
  `title` and `target` came with #1912 (below).

## HTML import picture links (#1999)

- With images on, an `<img>` whose look has a URL link (`Link::Url`, the
  enclosing `<a href>`) keeps it as Word keeps a picture's hyperlink: an
  `a:hlinkClick r:id` as the first child of the run's `wp:inline`'s
  `wp:docPr` (`image::with_click_link`, which takes only the self-closing
  docPr this crate writes, and declares `xmlns:a` on the `hlinkClick`
  itself: `a:graphic`, which declares it, is a later sibling). The
  picture from bytes and the sized placeholder alike; the unsized,
  unresolved one is still its alt as the link's text. The picture stays an
  ordinary picture run between the link's text, which stays two
  `Inline::Hyperlink`s: wrapping the drawing in `w:hyperlink` would put a
  picture in `Hyperlink.runs`, which every link consumer reads as text.
- The `r:id` is import-local, `rIdHtmlLink{n}`, one per distinct URL
  (`Images::link_rid`, charged to the budget), at most
  `MAX_PICTURE_LINKS` (1000) URLs: a picture linked to a later one is a
  plain picture (sized placeholders never reach the resolver, so
  `MAX_IMAGES` does not bound them). `HtmlImport::links`
  (`ImportedLink { rid, url }`) lists them. `HtmlImport::package` adds an
  external hyperlink relationship to `word/document.xml` for each, in one
  pass over the rels (`Package::add_part_hyperlinks`: one call a link
  would re-read the growing part each time, quadratic, r1) and renames the pictures' `r:id`s with
  the media's in one map (`rename_rids`: `image::replace_click_link` beside
  `replace_inline_embeds`). A caller that builds its own package from
  `media` must map `links` too, or its pictures name relationships it
  does not have. `save_package` copies the rels as they are, so the link
  survives a save; `image::inline_click_link` reads it (the docPr's first
  child only; `pic:cNvPr`'s copy and an anchored drawing are not read).
- An anchor link (`#name`) on a picture stays as before (the picture
  outside the link): DrawingML's `hlinkClick` has no bookmark form, and
  Word wraps the run in `w:hyperlink w:anchor` instead. `<a title>` is
  the `hlinkClick`'s `tooltip` since #1912. Not done: `a:hlinkHover`, the e-mail
  export writing such a picture back as `<a href><img></a>`, and a paste
  carrying the link's relationship (`docs/claude/suite-document.md`).
- Tests: `link_tests` in `import/html.rs` (`…_1999`), the `with_click_link`
  / `inline_click_link` / `replace_click_link` tests in `image.rs`,
  `add_part_hyperlinks_…` in `package.rs`.

## HTML import links, leftovers (#1912)

- Bookmarks: `<a name>` always makes one, and any element's `id` makes
  one only when the page links to it. `prescan` (the scan that finds the
  `<style>`s and `<base>`; it replaces #1898's `collect_styles`) collects
  the mapped names of every `<a href="#…">`, so a table of contents ahead
  of its sections finds them. Outlook mail is full of unlinked ids, which
  would clutter Insert > Bookmark. A void or raw-text element's linked id
  (`<img id>`, `<br id>`, `<svg id>`) is one too (r1): `open_void` reads
  its attributes for it only when the page has an anchor link at all.
  `prescan` reads each `<a>` tag once; one that never closes is read to the
  end of the page, as the tokenizer reads it (the rest is its attributes),
  and ends the reading of `<a>` tags, so many unclosed `<a` stay linear
  (r1; the `<style>` and `<base>` scan goes on as before).
- One mapping, `html::bookmark_name`, serves a link's `w:anchor` (in
  `link_of`) and the bookmark's `w:name`, so `href="#my%20section"` jumps
  to `id="my section"`, both `my_section`. Word's rule as far as it can be
  kept: each character that is not a letter, a digit or `_` is `_`; a
  digit-led name gets a leading `_` (a hidden bookmark, as `_Toc…`); a
  `_`-led one (Outlook's `_MailEndCompose`) is kept; cut to 40 characters.
  A mapped name already made is not made again (first wins).
- A bookmark is a point, two `Inline::Raw`s (`<w:bookmarkStart w:id="N"
  w:name="…"/><w:bookmarkEnd w:id="N"/>`, `w:id`s 0, 1, … in the import),
  queued by `Builder::bookmark` when its element opens and written right
  before the next content once its paragraph holds content that keeps it
  (`Builder::keeps`: text not all white space, an object, a page break;
  after that a tab or line break takes them too), in a cell when that
  content is. A paragraph of only spaces, tabs or line breaks, which the
  import drops (`clear_para`), so takes none (r2: each such paragraph once
  took every waiting mark and gave it back, marks × paragraphs): the mark
  of `<p id=x>&nbsp;</p>` or a linked `<div id=x><br></div>` goes to the
  next text. It never makes or keeps a paragraph: the Builder's content
  checks pass over it (`is_mark`: `is_object`, `shows`,
  `drop_trailing_break` counts the rest, `take_leading_marker` keeps it
  in front). A kept paragraph a typed list marker was all of can still be
  cleared; `clear_para` then puts its marks back from `para_marks`, the
  record of what went in, charged, not parsed. What is still
  queued at the end ends the body's last paragraph, or is dropped after a
  table. At most `MAX_BOOKMARKS` (10 000), charged to the budget.
- `Hyperlink` style: a Markdown package style now (`markdown_style_def`,
  so `markdown_styles_xml` and `ensure_styles`): character, colour
  `0563C1`, single underline, `uiPriority` 99, `unhideWhenUsed`. Link
  text takes it (`Builder::set_link_style`, in `link_text`, after the
  page's rules) only when its props are underlined and coloured and name
  no style: the direct formatting then holds all the style sets, so
  nothing drawn changes (`shown_run` ORs underline and takes the style's
  colour only for a run with none). A link the page restyled (no
  underline, `color:inherit` to none) gets no style, as before.
- Relative links: `link_of(href, base)` resolves an `href` with no scheme
  against the page's base (`Html::base`: `<base href>`, else an MHTML
  root's `Content-Location`, the base images use since #1898) with
  `html_image::resolve_ref`, then the allow-list and encoding as before.
  Only an `http`, `https` or `ftp` base with an authority resolves
  (`hierarchical_base`): a relative base, `mailto:` or `cid:` (a browser's
  cannot-be-a-base URL; `mailto:a@b` + `x` must not be `mailto:x`) leaves
  plain text, and so does a `file:` base (the `href` stays relative: no
  link; resolved it would be a `file:` one, which the allow-list refuses).
  `#name` stays an
  anchor link whatever the base (Word's and Outlook's reading). A page
  opened from disk does not resolve against its own path (that would make
  `file:` links).
- `title` and `target`: `Link` is a struct now (`to: LinkTo` `Url` /
  `Anchor`, `tooltip`, `frame`). `title` is trimmed, characters XML cannot
  hold dropped, cut to 255 characters (`MAX_LINK_ATTR`, Word's ScreenTip
  limit); `target` is trimmed and none when empty, `_self`, longer than
  255 (a cut name would name another frame) or holding a control
  character. A link with either keeps the opener the importer writes as
  its `raw` (`<w:hyperlink [w:anchor] [w:tooltip] [w:tgtFrame]/>`,
  `content_changed: true`): serialize writes it, then the runs, and
  `save_package` writes its minted `r:id` into it (`with_opener_rel_id`),
  as for a loaded link edited (#1952). No new `Hyperlink` fields: a loaded
  link keeps these in its opener already. `Link::made` compares that
  opener, so a titled link joins only its twin, its continuation after a
  break or picture included. A picture in a titled URL link: the title is
  its `hlinkClick`'s `tooltip` (`image::with_titled_click_link`; the
  public `with_click_link` the suite calls is the untitled one), after its
  `r:id`, escaped for either quote, so `inline_click_link` and
  `replace_click_link` never read a title spelling `r:id="…"`.
- A clipboard paste (`import_html_str_within`, only `Clip::from_html`)
  makes no bookmarks and no `Hyperlink` style (`import_decoded`'s
  `package: false`): nothing renumbers a paste's bookmark `w:id`s or
  dedups names against the document it goes into (`_GoBack` is usually
  `w:id="0"`), and that document may define no `Hyperlink`. Its anchor
  links stay anchor links, through the mapping.
- Markdown (r1-pre1, `markdown.rs`): `to_markdown` writes a destination
  holding a space, a parenthesis, `<`, `>` or a control character as
  `[text](<dest>)` (`md_destination`: `<`, `>` and `\` escaped, a line
  break `%0A`/`%0D`), and `parse_link` reads that form
  (`bracketed_destination`; unclosed, the bare reading of old).
  Percent-encoding instead would not round-trip (`from_markdown` does not
  decode). `from_markdown` reads `#name` as an anchor link, so a save
  mints no external relationship with `Target="#name"`.
- Not done: range bookmarks around an `<a name>`'s text, `a:hlinkHover`,
  the e-mail export writing tooltips, targets or bookmarks back, the
  suite showing a link's tooltip on hover, explicit-off underline or
  colour so a restyled link could carry the style too, bookmarks and the
  style on a paste.
- Tests: `import/html_tests_1912.rs` (`…_r1_1912` for the review's), the `…_1912` Builder tests in
  `import/mod.rs`, `markdown.rs`, `image.rs` and `package.rs`.

## HTML import page margins (#1928)

- An HTML import ends in its own final section
  (`html_table::page_section`, set in `import_html_within` after
  `Builder::finish`, so every caller gets it: `import_html`,
  `import_html_with`, the converting child): US Letter written out
  (`w:pgSz` 12240 x 15840, so no reader's locale makes it A4) and a narrow
  page margin, 15 px (`PAGE_MARGIN_TWIPS`, 225 twips) on every side
  instead of Word's 1": `w:pgMar` with all seven attributes, header and
  footer 225, gutter 0. That is the printed page's margin, shown in Print
  Layout. A browser's body margin is 8 px. The embedded view's
  Outlook-like flow margin is the host's inset instead, about 20 px from
  the pane edge as measured in #1979 (#1987, `docs/claude/suite-extensions.md`).
- `BODY_TWIPS` is that page's body, `PAGE_W_TWIPS - 2 * PAGE_MARGIN_TWIPS`
  (11790, 786 px): outermost tables, wide pictures and fixed-width
  centring are laid out in exactly the body the section gives, so a 600 px
  card sits in a 786 px body as in a mail client. RTF keeps its own even
  6.5" grid (`import/mod.rs`) and, like PDF and recovered text, no section
  of its own: the package's default page.
- Where it shows: Print Layout (`final_page_geom`). A Web Page still opens
  in Web Layout (`page_view` is off by default), which keeps its fixed
  48 / 28 px padding and never reads page margins (#1895,
  `docs/claude/docx-editing.md`). `ext::DocView`'s Web Layout defaults to
  the same padding, which its host can set (`DocView::set_page_inset`,
  #2003), and never reads page margins either; its Print Layout (#1888)
  draws each section's own.
  `uiharness/cases/html-page-margins.uit` reads the ruler's insets after
  View > Print Layout.
- A later margin change (Page Setup > Margins via `apply_page_setup`,
  Layout > Margins via `edit_section_setups`, a ruler drag via
  `set_page_margins`) keeps header, footer and gutter: `SectionSetup::apply`
  writes all seven `w:pgMar` attributes from the parsed margins, and only
  Page Setup's own fields change those three.
- Known limits: 15 px is inside most printers' unprintable band (about
  0.17-0.25"), so Word warns about the margins when printing. A page that
  declares its own `@page` gets that page instead since #1944 (next
  section); every other import gets this fixed page.

## HTML import page rule (#1944)

- A page's own `@page` gives the import's section and the body the reader
  lays out in, in place of #1928's page (`html_table::Page`,
  `Page::DEFAULT` is #1928's; `BODY_TWIPS` stays the default page's body).
  So a Word Web Page (`@page WordSection1 {size:8.5in 11.0in; margin:...}`
  with `div.WordSection1 {page:WordSection1}`) keeps Word's page:
  `corpus/word-import/source.htm` imports with `source.docx`'s `pgSz` and
  `pgMar`.
- `css.rs` keeps unnamed and named `@page` rules (`StyleSheet::pages`, at
  most `MAX_PAGE_RULES`); a page selector (`:first`, `name:left`), a
  prelude of two names, margin boxes and an `@page` inside `@media` are
  skipped with every other at-rule. `StyleSheet::page_decls`: the unnamed
  pages' declarations, then the selected named page's, then the
  `!important` ones of both, each in source order (a later one wins). The
  selected page is the one named by the first `page` declaration (in
  source order, of the blocks that may select one) that names a kept
  `@page` (ASCII case-insensitive); `auto` and unknown names are passed
  over, and with none left no page is selected. Whether an element matches that
  rule is not checked: Word's `div.WordSection1` wraps the whole body, and
  the import has one section, so a page of several WordSections takes the
  first selected page throughout.
- `Page::from_decls` reads `size` (one length, a square, or two; `auto`;
  `letter`, `legal`, `ledger`, `a3`-`a5`, `b4`, `b5`; `portrait` /
  `landscape` with or without one, in either order), `margin` and its
  longhands, and Word's `mso-header-margin` / `mso-footer-margin`. Without
  those two, header and footer are the top and bottom margins, at most
  0.5": ours, not CSS (Word's filtered HTML drops them; Word's default is
  0.5"). Lenient: an unreadable `size` is ignored whole, a side of it
  outside 1"..22" alone; a percentage, negative or unreadable margin is
  ignored alone; margins leaving a body under 0.5" wide or high are all
  ignored (the default 225 then). A page wider than tall writes
  `w:orient="landscape"` with `w`/`h` as given (as Word stores it).
- The body follows the page: `Html::set_page` sets `Html::page` and
  `Builder::set_body` (an outermost table's room); `Ctx::body` /
  `Cascade::body` centre a fixed-width box outside a cell and give
  `body_room`; `table_spec` takes the body; `html_image::fit_to_body` and
  `Html::image_room` read it.
- #1928's page stays for:
  - a classic-Outlook mail: the first `<meta name=Generator>` (in
    `prescan`) says `(filtered medium)`. Outlook composes in Word, so its
    mail carries Word's default `@page` with 1" margins that the sender
    never chose and its reading pane ignores. Word's Save as Web Page
    writes `(filtered)` or nothing after the version.
  - a paste (`import_html_str_within`): it goes into a document with a
    page of its own, and Word's clipboard HTML carries the source's `@page`.
  - `@page` rules and `page` declarations in a `<style>` after the page's
    first `<body` tag (`Prescan::head_css`, `StyleSheet::parse_split`): a
    quoted mail's styles sit in the body. Its other rules still apply.
- Known limits: a classic-Outlook mail quoted by another client whose
  styles end up in the head (no Outlook Generator) gives Word's default
  page. Not read: `@page :first`/`:left`/`:right`, margin boxes,
  `mso-gutter-margin`, `mso-paper-source`, page borders and columns,
  `page` in a `style=""` attribute, and several `@page`s as several
  sections. Web Layout and `ext::DocView`'s Web Layout never show page
  margins; Print Layout does.
- Tests: `import/html_tests_1944.rs`, the `…_1944` tests in `css.rs`,
  `word_web_page_keeps_word_page` (`tests/word_import.rs`),
  `tests/html_page.rs`, `uiharness/cases/html-page-rule.uit` (121 / 97 /
  574 px against `html-page-margins.uit`'s 16 / 16 / 784).

## HTML import Outlook mail (#1979)

A simple mail from Outlook on the web, new Outlook or classic Outlook
imports as a browser lays it out. Slice 1 of #1979: the model. The
suite's flow view drawing it (marker look, spacing, line height) is
#1986 (`docs/claude/docx-editing.md`). `ext::DocView`'s page margin is
the host's, `DocView::set_page_inset` (#2003); #1987 checks an
Outlook-like 20 px inset on this mail (`docs/claude/suite-extensions.md`).

- Spacing comes from CSS only. Every HTML paragraph writes `w:spacing`:
  - `before`/`after` from the top/bottom boxes of the blocks and lists it
    is the first/last paragraph of, else 0 (#2114: see "HTML import
    margins, breaks, hangs and wrapper tables").
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
  - Only margins collapse, not padding (#1979 r1). A paragraph's
    written `before` adds what of its top margins the margin group open
    below the paragraph above does not cover (`Html::top_spacing`,
    `FlowTail::open`). The paragraph above keeps its whole `after`. A
    negative margin is only summed.
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
  - The margin quirk, at the body's or a `td`/`th`'s edges (not a
    card's), measured in Chrome (#2114 r2):
    - The top: when its paragraph is its flow's first, every block and
      list from the outermost in loses its UA top margin, up to and
      including the first with top padding (`Html::top_spacing`). So
      `<div dir=ltr><ol><li>` and `<td><div><p>` start at 0, while
      `<div style="padding-top:1px"><ol>` keeps the list's 1em.
    - The bottom: a block's UA bottom margin is dropped when its paragraph
      is its flow's last and no bottom padding separates it from the body
      or cell (`Html::bottom_unseparated`), through a `div` too. A list's
      is never dropped (`<td><ol><li>` keeps its 1em below; at the body's
      end, unmeasured, it is kept too).
  - The last is settled at the flow's end (`Html::flow_end_props`, at
    `</td>`, `</tr>`, `</table>`, a card's end and the page's end). The
    text in progress is ended first, as any paragraph, and then the flow's
    last paragraph takes the bottom it would have without the quirky
    margins (`FlowTail::bare`, folded beside the real one), in place. The
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
    `DECIMAL_HANG` 1.1125em for an `ol` (one digit; #2114 adds
    `DIGIT_HANG` per digit past it), `BULLET_HANG` 1.375em for a `ul`,
    measured in Chrome. The em is the `li`'s own font (`item_elem`),
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
  - Margins through a card's top.
  - An empty `<p></p>`'s margins collapsing through it.
  - `list-style-type`. (`<ol start>` and each `<ol>`'s own count: #1989.)
  - A page's `em` margin on a heading resolves at the parent's size, as
    before.
  - The HTML Standard's rarer quirks public ids and the IBM system id.

## HTML import margins, breaks, hangs and wrapper tables (#2114)

A batch from #1989 and #1930 (plan-2), import only.

- Trailing `<br>` (`Builder::drop_trailing_break`, from `end_para_with`):
  a paragraph's last line break goes when something comes before it, as
  a browser's `<br>` that ends a block adds no line.
  - Covers `<div>a<br></div>`, `<p>a<br>\n</p>`, `<td>a<br></td>`, a
    `<br>` before a nested block, and `pre`'s last newline.
  - `<div><br></div>` stays one empty paragraph: its lone break is white
    space, cleared as before. Content after a break keeps it; of two, one
    stays.
- Wide hangs: each open list counts its `li`s (`OpenList::next`, from its
  `start` since #1989), and an item keeps its number
  (`Block::item_ordinal`, as a browser counts).
  - `decimal_hang(n)` is `DECIMAL_HANG + (digits − 1) × DIGIT_HANG`
    (0.5555em; Chrome's "10. " is 1.668em), still clamped to the indent.
- A `dl` ends the paragraph in progress at its start and its end, as any
  block (`a<dl>b</dl>c` is three lines).
- Vertical boxes of blocks and lists, one mechanism:
  - Each block and list's `Elem::flow_at` is its flow's depth and length
    when it opened (the paragraph in progress already ended). It is
    fresh while that flow still has that length.
  - Top (`Html::top_spacing`): the fresh blocks and lists from the
    innermost table, cell or card out (inline elements, containers and
    `body` are see-through) fold their top boxes outermost first. Positive
    margins collapse into the group open below the paragraph above
    (`FlowTail::open`), padding closes the group, negative margins are
    summed, and the result clamps at 0.
    - Freshness is monotonic, so the walk stops at the first stale one.
    - A block's second anonymous paragraph gets no top: in
      `<div style="margin:30px 0">a<p>b</p>c</div>`, a has 450 before and c
      450 after.
  - Bottom (`Html::apply_bottom`, `fold_bottom`): each paragraph ends with
    `after` 0. When a block or list leaves the stack (`truncate_elems`,
    innermost first, before reopening), and the flow's last paragraph
    ended inside it, its bottom box is added to that paragraph in place.
    - Padding closes the open group, then its margin opens a new one. With
      no padding, the margin collapses into the open group.
    - `FlowTail::after` keeps the signed running value; the written
      `after` is at least 0.
  - Flow ends (`flow_end_props`: cell, row, table, card, page) apply the
    bottoms of the blocks and lists still open in that flow first, once
    (`flow_at` taken), then quirks mode's bottom patch. Only that flow's:
    a card is declined before its flow ends, so the walk can pass it into
    the enclosing flow's blocks, which keep their `flow_at` (#2114 r1).
  - Quirks: see the margin quirk above. The bottom fold is kept twice,
    with and without the quirky UA bottoms (`FlowTail::bare`).
  - Both walks charge the import's work budget.
- Lists' user-agent margins: `ul ol dir menu dl` have 1em top and bottom
  in their own font, none when nested in another list at any depth, across
  tables too (`Look::in_list`, inherited). Declared sides win per side.
- Wrapper tables (#1930 plan-2):
  - A block centred by its fixed width and auto margins marks its look
    `centred_box` (inherited, not in a cell). An outermost table in it
    gets an `html_table::Room`: the box between its indents, and whether
    that room is centred in the body (equal indents).
  - `end_html_table` passes the room's width to `layout` as its `avail`
    (a nested table drops a room: it takes its cell's width).
    `place_in_room` writes a
    percentage `tblW` as twips, since Word resolves `pct` against the body
    and would stretch it back.
  - A table that fills the room exactly, or fits and is centred in it, in
    a centred room gets `jc center`, whatever its `align`.
  - Otherwise it gets no `jc` and a `w:tblInd` of its left edge: the
    room's left, right or middle. A table wider than its room is
    over-constrained in a browser and starts at the room's left (r2).
  - The Markdown package has no `compatibilityMode`, so Word lays it out
    in an older mode, which measures `tblInd` to the first cell's text.
    The left cell margin is therefore added: `cellpadding`, else 108.
  - The suite does not draw `tblInd`, so such a table stays at the body's
    left there, as before.
- Tests: `batch_2114_tests` in `import/html.rs`;
  `a_table_in_a_room_is_laid_out_and_placed_there` in `html_table.rs`.
- Known limits:
  - A block's bottom box is lost when it ends in a table (it used to sit
    on the paragraph above the table), and a block whose first child is a
    table gives no top.
  - A table in an indented block that is not a centred fixed-width box
    keeps the body's width.
  - The suite's `list_markers` restarts its count at an item's unnumbered
    continuation (a follow-up).

## HTML mail as Word draws it (#2249)

- Hidden text: `display:none` / `mso-hide:all` make the runs `vanish`
  and now the paragraph mark too (`mark_rpr` copies `vanish`), so a
  preheader block is hidden whole. The suite collapses such a paragraph
  (see `docs/claude/suite-extensions.md`).
- `text-decoration:none` keeps an explicit `<w:u w:val="none"/>` in the
  run's `raw_props` (with `underline` false), so a link styled as plain
  text loses the link's own underline in the view, as in Word.
  `text-decoration:underline`, `<u>` and `<ins>` drop it again.
- An inline `background` was already run shading (`w:shd`); the view
  now paints it.
- Column widths: a column without a width that no colspan-1 cell sits in
  (only a span or an empty slot reaches it, e.g. a one-cell row over a
  `colspan=2` footer) gets `MIN_COL` when every span reaching it also
  reaches a column with its own cell or width (a span of bare columns
  only, such as `<td colspan=2>` beside a cell, is that cell's room and is
  not pinned), and the content columns share the
  rest. When every unknown column is bare, they share it evenly as before.
- Tests: `hidden_text_is_vanish`,
  `text_decoration_none_is_an_explicit_no_underline_2249` and
  `card_tests::a_column_only_a_span_reaches_takes_next_to_no_width_2249`
  in `import/html.rs`.

## HTML import CSS leftovers (#1883)

- Quirks mode's table font: a `table` resets `font-size` (to the import's
  unsized 12 pt), `font-weight`, `font-style` and `font-variant` at the
  user-agent level (`Cascade::user_agent`, `Ctx::quirks`), so the table's
  own and its cells' CSS still apply; family and colour inherit. No
  explicit off is written (`bold_off`/`italic_off` false). Clipboard
  fragments are quirks too: a pasted table loses a body-inherited size, as
  a browser shows the fragment. Its `text-align: start` came with #1894 r2
  (a `th` centres only over a start, so one under a `text-align:left`
  block depended on it; see "HTML import centring"). Not done: the same
  rule's `line-height` and `white-space` (they move measured mail
  layouts).
- `font-style: normal` under a heading is `<w:i w:val="0"/>` (`ITALIC_OFF`,
  `Look::italic_off`), as `font-weight: normal` is the bold off: declared on
  or inside the heading only.
- `dir` (any element, `auto` ignored, any case) and `direction` set the
  inherited `Look::rtl`: the paragraph's `w:bidi`. Word reads a bidi
  paragraph's `w:jc` left/right as start/end, so a visual `left`/`right`
  (CSS or `align`) is swapped there and a logical `start`/`end` is not
  (`Look::align_logical`); none stays none. Outlook's `<p dir=RTL
  style='text-align:right'>` is rtl Left. Not done: `render.rs` and
  `export_email` draw rtl+Right on the right (only rtl+Left is swapped
  there); the suite ignores paragraph direction altogether; a table's
  `dir` (`w:bidiVisual`).
- `letter-spacing`: run `<w:spacing w:val>` in `raw_props`, in the
  element's own final font, inherited as that length; `normal` removes it.
  The suite does not draw it (Word does).
- `text-transform: uppercase` is `caps` (`none`, `lowercase`,
  `capitalize` clear it: Word has no lower case); `font-variant`,
  `font-variant-caps` and the `font` shorthand's `small-caps` are
  `small_caps` (the shorthand resets it otherwise).
- Hiding is last-wins per element (`Cascade::display_none`, `mso_hide`,
  `visibility`): `display:none` or `mso-hide:all` sets the inherited
  `Look::display_hidden`, which nothing below undoes; `visibility:
  hidden|collapse` sets the inherited `visibility_hidden`, which a
  `visible` below undoes; `vanish` is either. So `.h{display:none}` with
  an inline `display:block` shows (it stayed hidden before).
- `white-space: pre|pre-wrap|break-spaces` (inherited `Look::pre`) keeps
  spaces and line feeds as in a `pre`; `normal`, `nowrap` and `pre-line`
  collapse (Not done: `pre-line`'s kept line feeds).
- Page breaks: `page-break-before`/`break-before` (`always page left right
  recto verso`) on a block or list is `<w:pageBreakBefore/>` on the first
  paragraph ended in it (`Html::page_break_before`, the same fresh walk as
  the top spacing), and `-after`, or a break-before on a block nothing
  ended in, sets `Html::break_pending` for the next body paragraph. Never
  in a cell (`look.in_cell`), never on the body's first paragraph; a table
  opening drops a pending one (Word ignores the break in a first cell).
  Word's `<br clear=all style='page-break-before:always'>` is a page break
  run (`br_breaks_page`, its look computed only with attributes or a sheet
  rule that could match a `br`); `Builder::has_page_break` keeps its
  paragraph from being dropped as empty. A paragraph that shows nothing
  (hidden) takes no break: it passes it on (`break_pending`) to the next
  that shows (r4). A hidden block forces none (r3). A break before a block
  whose first content is a table is lost (Word ignores one in a first
  cell).
- Paragraph borders (`css::border_shorthand`, `border_sides`,
  `Cascade::border_decl`, `own_borders`): `border`, `border-<side>` and
  the `-width`/`-style`/`-color` longhands, on a block only (cards, lists,
  images and inline elements ignore theirs; a cell or card starts with
  none; a cell's and a table's are its `tcBorders` / `tblBorders` since
  #1894, never paragraph borders). solid/groove/ridge/inset/outset are single, double,
  dotted and dashed their own; `sz` is the width in eighths of a point
  (2..96); the colour is the element's text colour unless given,
  `windowtext` automatic, `transparent` or a zero alpha no line, a partial
  alpha blended over white (`rgba(0,0,0,0.1)` is `E6E6E6`; r3; `hsl()` /
  `hsla()` read since r4). A
  block's left and right lines go on every paragraph in it, an inner
  block's own over the outer's (`ParBorders::over`): Outlook's quote line
  goes on through its header, and Word's `<p style='border:none'>` in a
  bordered `div` keeps the box. Its top line goes only on the first
  paragraph in it that shows anything (`Html::take_top`,
  `Elem::top_placed`: past a table, a card or a hidden paragraph before
  it; the innermost block's wins, and the outer's is placed with it) and
  its bottom line on the last such when it closes (`apply_bottom`,
  `Builder::last_shown_para_mut`: past a table or a hidden paragraph
  after it; unless a block inside closed first and gave that paragraph
  its own), so a box around a list or a ruled title is still one box (r3,
  r4). A paragraph shows anything (`import::shows`) unless its mark is
  `vanish` and all its content too, as Word and the suite collapse one
  (r5: a shown `p` holding only hidden text is a line). Geometry as a browser's box: a
  left/right line moves the text in by its width; each paragraph's
  left/right `w:space` is its indent less the line's place
  (`Look::border_left_x`) and width, in whole points up to 31, so an item
  or padded block inside keeps its text where it was and the line where
  the box is; a top/bottom line takes that side's padding as `w:space`
  (rounded to whole points) instead of spacing, and separates margins as
  padding does (`Look::border_before`/`border_after`: `fold_bottom`'s
  `sep`, `top_spacing`, `bottom_unseparated`, the quirks drop). Known
  limits: adjacent blocks with equal borders and indents group into one
  box, as Word (and classic Outlook, which draws mail through it) and the
  suite draw them, where a browser draws them apart (r2: no docx mark is
  read by both as "a separate box, drawing nothing", so none is written);
  a box's first, middle and last paragraphs have different borders, so
  they are separate border groups in the suite (and in Word): the side
  lines break across the spacing between them (the suite draws a group's
  strips inside its own spacing only); a margin inside a top/bottom line
  is spacing outside it in Word; nested boxes are one box; a transparent
  line's width is not kept as room; a box holding only a table (or card)
  loses its lines (cell and table borders from a box are out of scope),
  and one that starts with a table draws its top under it.
- Selectors (`css.rs`): attribute selectors (`[a]`, `=`, `~=`, `|=`, `^=`,
  `$=`, `*=`, `i`/`s`; the HTML Standard's `CASELESS_ATTRS` compare values
  without case, so `td[align=center]` matches `align=Center`), `+`,
  `:first-child`, `:link`/`:any-link`, `:root` (`html` only); each counts
  as a class. An `ElemKey` carries its `index` among its parent's element
  children, `link`, and only the attributes the sheet names
  (`StyleSheet::attr_names`, at most 32 names: a selector naming another
  is dropped), each value up to `MAX_ATTR_VALUE` (1 KiB: a longer one is
  present for `[a]` but fails every value test, r1) and lowercased once
  when a selector compares that name without case
  (`StyleSheet::caseless`); a value test is charged a unit per 16 bytes
  and the kept attributes held only while their element is open or kept
  as a sibling. Every open `Elem` (and
  `Html::root_kids`) keeps its children's count and, only when the sheet
  has a `+`, the serials and keys of its last 32 (`Kids`, charged as
  work): a `+` chain never looks further back, and an open
  ancestor is found among its parent's children by its `Elem::serial`.
  A void or skipped element's attributes are read once (`open_void`). Every start tag
  is a child, void and raw-text elements and `head` included. Matching's
  reach set holds at most one place per depth (`Path::prev`), so it stays
  O(compounds × depth). Not done: `~` and `:first-of-type` (the whole
  sibling list, quadratic on long pages), `:last-child`, `:nth-*`,
  `:not()`, `:hover`/`:visited`, pseudo-elements, escapes.
- Colours (`css::rgb`, `color_args`, r5): a partial alpha in any colour
  (`color`, `background`, a border) is the colour over white, as on a
  white page (`rgba(0,0,0,.5)` is `808080`, a 4% black shading
  `F5F5F5`); a zero alpha is none; `hsl()`/`hsla()` are read; a
  non-finite number makes the colour none.
- A `head` ends as a browser ends it (`Html::skip_head`, r3, r4; a fix
  to code older than #1883), in one forward scan charged as scanned:
  white space, comments, `<!…>`, `HEAD_VOID` tags and raw-text elements
  (skipped whole, so a `<body>` in a script is none) are passed over, a
  stray end tag is ignored, `</head>` ends it, and any other start tag
  (`<body`, a `<p>`) or text ends it and opens. The `<style>` pre-pass
  (`prescan`) reads a head's styles either way.
- Tests (`import/html.rs` unless named): `quirks_tables_reset_font_size_weight_and_style_1883`,
  `italic_off_is_explicit_under_a_heading_only_1883`,
  `dir_rtl_is_paragraph_bidi_and_swaps_alignment_1883`,
  `letter_spacing_is_run_spacing_1883`, `text_transform_and_small_caps_1883`,
  `visibility_and_display_are_last_wins_vanish_1883`,
  `white_space_pre_keeps_spaces_and_breaks_1883`,
  `page_break_before_and_after_on_blocks_1883`,
  `br_with_page_break_before_is_a_page_break_1883`,
  `gmail_blockquote_border_left_is_a_paragraph_border_1883`,
  `outlook_reply_header_border_top_1883`,
  `word_s_ruled_paragraph_keeps_its_box_1883`,
  `border_none_and_missing_style_draw_nothing_1883`,
  `cell_card_and_inline_borders_stay_ignored_1883`,
  `top_and_bottom_lines_separate_margins_1883`,
  `style_block_sibling_attribute_and_pseudo_rules_apply_1883`,
  `void_and_raw_text_elements_are_siblings_1883`,
  `long_sibling_runs_stay_within_the_budget_1883`,
  `long_attribute_values_are_present_but_never_compared_1883`,
  `sibling_bordered_blocks_group_as_word_draws_them_1883`,
  `a_box_s_top_and_bottom_are_on_its_first_and_last_paragraphs_1883`,
  `an_unclosed_head_ends_at_the_body_1883`,
  `many_heads_are_skipped_in_linear_time_1883`,
  `a_box_s_lines_pass_over_tables_cards_and_hidden_paragraphs_1883`,
  `a_paragraph_with_only_hidden_text_still_shows_1883`,
  `partial_alpha_colours_blend_over_white_1883`,
  `many_elements_with_sibling_and_attribute_rules_import_1883`; `css.rs`:
  `a_value_past_the_cap_is_present_but_never_matches_a_test_1883`,
  `borders_shorthands_and_longhands_1883`,
  `transparent_and_alpha_border_colours_1883`, `attribute_selectors_1883`,
  `next_sibling_first_child_link_and_root_1883`,
  `next_sibling_chains_are_polynomial_1883`,
  `a_sheet_keeps_at_most_32_attribute_names_1883`; `export_email.rs`:
  `caps_small_caps_and_rtl_round_trip_1883`; `tests/html_css.rs`'s mail
  fixtures check the quote lines.

## HTML import table borders, heights, columns, `th` and the page colour (#1894)

The leftovers of #1878, measured in headless Chrome (the same in quirks
and standards mode). p3 (centring under `center`, `align`, auto margins)
was #1922's.

- Borders (`Cascade::table_border_hints`, `frame_lines`, `table_spec`):
  - A `table`'s and a cell's CSS border (#1883's `border_decl`, every
    shorthand and longhand, sheet rules too) is `Look::own_frame`:
    `tblBorders` top/left/bottom/right and `tcBorders`, written by
    `serialize::pbdr_side_xml` with `w:space="0"`. A side that draws
    nothing (none, hidden, 0, transparent) writes nothing, never `nil`:
    a browser's `none` loses the collapse conflict to its neighbour's
    line, and a Word `nil` on an edge cell would erase the table's frame.
  - `border`, `frame` and `bordercolor` seed the table's outer sides as
    presentational hints, so CSS replaces them side by side: the sides
    `frame` names (all for a `border` above 0 alone, none for `rules`
    alone), `border` px wide (1 without one), `bordercolor` else
    automatic. A `border=N` table with no CSS is written as before.
  - Inside lines (`TableSpec::inside`): `rules` (`rows`, `cols`, `all`;
    `groups` and `none` none), else both for a `border` above 0; 1 px
    `single`, in the table's computed top (`insideH`) and left
    (`insideV`) border colour (`Look::own_rule_ink`): a browser's cells
    of such a table inherit the table's `border-color`, so CSS
    `border-color` on the table colours them too.
  - Edge cells (`html_table::layout`): Word draws a cell's line on the
    table's edge instead of the table's, so where the table draws that
    outer side and its cells are not spaced apart, a cell side there is
    written only when at least as wide (CSS collapse: the wider wins, a
    cell a tie). A merged (`rowspan`) cell writes its top on its first
    slot, its sides on each and its bottom on its last.
  - `border-collapse` (`Look::collapse`) and a known `rules` value
    (`none`, `groups`, `rows`, `cols`, `all`: its hint; another is
    ignored) drop `tblCellSpacing`, as the collapsing model has no
    spacing: a mail's `table { border-collapse: collapse }` reset drops it
    too. Only a table's own: the user agent's `table { border-collapse:
    separate }` resets the inherited value at every table (r1), so a
    collapsed outer table or a `div`'s collapse does not reach a table
    in it.
- Row heights: `height` on a `tr` (attribute: digits or px; CSS: an
  absolute length; a percentage none, `auto` clears) is the row's; on a
  `td`/`th` it is the content box, so its top and bottom margins (its own
  padding, else `cellpadding`) are added; a cell spanning rows sets none.
  The largest is `w:trHeight w:hRule="atLeast"` in the row's `raw_props`.
- Columns: `col` (void; its attributes and look computed in `open_void`) and
  `colgroup` in a table's own content (`Html::at_table_level`, not in a
  cell or row) give `RawTable::cols` (`Builder::begin_colgroup`,
  `table_col`, `end_colgroup`): `span` (1..=1000) copies of the `col`'s
  width (its computed look's, as a `colgroup`'s: CSS, sheet rules too,
  over its attribute; `auto` none, over the attribute too), else its
  group's.
  A `colgroup` with no `col` stands for its span; it ends at
  `</colgroup>`, a cell, a row group or the table's end. Total cut at
  `MAX_COLUMNS`; columns past the grid add none. `column_widths` takes
  them as colspan-1 cells' widths.
- `max-width`: on a table (`TableSpec::max_width`, a length or a
  percentage of its room) it cuts a wider width to it, written in twips,
  and an auto table is laid out in no more than it (`tblW` stays auto);
  a nested table's room is its cell. On a block an absolute one bounds
  the width its auto margins centre (#1922), or is that width with none,
  so the #2114 room follows; a percentage one is not read.
- `th`: bold (the user agent's), and centred only over an inherited
  start alignment (none, or a logical `start`): its own `align` or
  `text-align`, or an inherited explicit one (`<tr align=left>`, a
  `div`'s `text-align:left`), win, as Chrome's `-internal-center`. The
  centre places no block child (`block_align` is unchanged). No
  `tblHeader`.
- The page colour: the `html` element's background, else the `body`'s
  (`bgcolor`, CSS `background`/`background-color`; `none`/`transparent`
  clear the attribute's), white none, is `HtmlImport::page_color`
  (`RRGGBB`, a public field), which `HtmlImport::package` writes with
  `Package::set_page_background` (`w:background` and
  `w:displayBackgroundShape`). `import_html` (a `Document`) has none. An
  HTML file opened in the suite (`convert_child`) and the harness's
  `ext-doc-load` get it; on a dark page Word's dark mode maps a light
  page colour as a fill (`dark_page::page_sheet`, #2079), so it is drawn
  dark.
- Tests: `table_1894_tests` in `import/html.rs`;
  `image_tests::height_still_means_nothing_on_a_table` now checks a row's
  height too; `centering_tests::webkit_center_ancestors_centre_tables` and
  `a_centred_table_keeps_its_cell_text_alignment` follow quirks mode's
  `text-align: start` at a table (r2, measured in Chrome).
- Not done: background images (docx has no picture fill for a cell or
  page; the shorthand's colour is read); `rules=groups`; a separated
  model's doubled lines between bordered cells (Word draws one); a cell's
  border widths in its row's height; a percentage `max-width` on a block,
  `min-width`, `height` on a `table`; `th`'s
  `scope` and header rows (`tblHeader`).

## HTML import whitespace-only paragraphs (#1973)

- A paragraph whose text is only white space is dropped as an empty
  paragraph (Word's `<p>&nbsp;</p>`, `<o:p>&nbsp;</o:p>`), unless a look
  a browser draws across a space holds it: a blank to fill in.
  `import::shows_on_space` is that look: `underline`, `strike`,
  `highlight` or the run shading (`<w:shd`) an inline `background`
  writes, and not `vanish`. A font or size alone draws nothing, and an
  explicit underline off (`<w:u w:val="none"/>`, `text-decoration:none`,
  how Word writes an underlined empty paragraph's `&nbsp;`) is no
  underline. A block's own underline is its runs', and counts.
- It is decided when the content is written, not at the paragraph's end:
  `Builder::link_text` and `Builder::tab` set `keeps` for white space or a
  tab in such a look (outside a link: a link of spaces alone stays empty,
  #1912), so a kept blank takes its waiting bookmarks and counts in
  `Builder::shown` (#1936); `take_leading_marker`'s recount agrees.
  `end_para_with` clears the paragraph when `!Builder::keeps()`.
- An underlined tab alone (`mso-tab-count` or a `pre` tab) is kept as its
  `Inline::Tab`: Word's usual blank line, which `export_email` writes as a
  tab span in the run's span.
- An empty paragraph's mark takes its line's look (`empty_line_mark`):
  of its own visible runs, tabs and line breaks (not a link's; a lone
  `<br>`, Gmail's empty line, too), the one with the largest
  `size_half_pts`, the first among ties, else the first; `mark_rpr` of it
  replaces the block's. In standards mode the size is at least the
  block's own (`para_look`'s), a line box's strut; quirks mode (Outlook's
  mail, no doctype) has none, so a smaller span sizes the line. None
  visible leaves the block's mark. A list item's mark stays its marker's
  (#1979). The runs inherit the block's look, so a span with no look of
  its own gives the block's mark as before; a span that turns the block's
  look off (`font-weight:normal` in a bold block) gives a plain line, and
  the block's mark goes with nothing in its place.
- A typed list marker's spacer (`take_leading_marker`'s gap) stops before
  white space in a look drawn on it: `1.<span>&nbsp;&nbsp;</span><u>&nbsp;…</u>`
  keeps the underlined blank. The inline the marker ends in is cut as
  before.
- ASCII spaces alone never reach the builder outside `pre`: `Html::text`
  owes a space (`self.space`) and pays it only before a non-space
  character (`pay_space` before an object or alt text), so they stay no
  line.
- Tests: `import/html_tests_1973.rs`;
  `export_email::tests::underlined_spaces_alone_round_trip_1973`.

## HTML ordered lists: their own count, start and value (#1989)

- Each `<ol>` counts on its own, from its `start`; an `<li value>` is
  that item's number and the next ones go on from it, as a browser numbers
  them. Before, every `<ol>` item was numId 2, one count across the page,
  `start` and `value` ignored.
- A list instance: an `<ol>`, or the run of its items that a `value` other
  than the next number starts. Each one is its own `w:num` on its **own**
  `w:abstractNum` (the Markdown decimal levels, level `level`'s `w:start`
  its first number). No `w:lvlOverride/w:startOverride`: siblings on one
  abstract continue or restart differently in Word and LibreOffice, and
  docxcore's `Numbering` and the suite's `ListKey` share a count per
  abstract.
  - `Html::new_list(level, start)`, allocated at the instance's first
    `li` (`OpenList::num`, lazily, so `<ol><li value=3>` is one instance
    starting at 3). The first one starting at 1 is numId 2 on abstract
    101, so a page with one plain `<ol>` imports byte for byte as before,
    and the suite's `ListIds::of` still finds docxy's own decimal list. Any
    other is numId 3, 4, … on abstract `num_id + 99` (102, …).
  - `MAX_HTML_LISTS` (256) instances, `LIST_WORK` charged each; past the
    cap a list is numId 2 again.
  - The item's numId is `Block::item_num` (1 for a bullet), copied into
    the blocks in the `li`; `number_item` writes it. A nested `<ol>` is an
    instance of its own at its level. Filtered HTML's typed `MsoList`
    markers stay 1 and 2.
- `start` and `value` (`list_number`): HTML's rules for parsing integers
  (leading white space, a sign, the leading digits: `5abc` is 5; none is
  ignored), clamped to `0..=32767`, Word's range. A browser's negative
  numbers are 0. A `ul`'s `start` and its items' `value` are nothing.
- The definitions: `HtmlImport::lists` (`ImportedList { num_id,
  abstract_id, level, start }`), defined by `HtmlImport::package` through
  `Package::add_html_lists(&[(num_id, abstract_id, level, start)])`. That
  is one splice of the numbering part, adding the Markdown part first when
  there is none. It refuses (false, nothing changed) when the part already
  has one of the ids, and the caller folds.
- A bare `Document` cannot carry them: `import_html` and
  `import_html_str_within` (the clipboard's `Clip::from_html`) fold them
  back to numId 2 (`HtmlImport::fold_lists`, table cells too). A caller
  that builds its own package from `HtmlImport::document` defines `lists`
  or folds, or the suite draws an undefined numId past 2 as a bullet.
- Readers: docxcore's `compute_markers`, `export_email` (a new numId
  reopens the list with its `start`, so a `value` split exports as a
  second `<ol start>`) and Markdown export needed no change. The suite's
  `list_markers` reads each level's `w:start` (`docs/claude/docx-editing.md`).
  docxcore's `fill` draws any decimal counter at 0 as 0, where it drew 1:
  a `w:start 0` level, and a parent level not yet advanced (`%1.%2`
  before any level-0 item is "0.1"), as Word draws them; letters and
  roman numerals at 0 still draw as 1 (`docs/claude/docx-editing.md`).
- Not done: `type` / `list-style-type`, `<ol reversed>`. An `li` that
  never gets a numbered paragraph (`<li><table>…</table></li>`, whose
  cells reset `Block`) still advances the count, so Word skips a number
  the browser shows (as before). The suite's restart at an item's
  unnumbered continuation (#2114) restarts to the list's start.
- Tests: `import/html_tests_1989.rs`; the suite's
  `list_markers_count_from_the_levels_start_1989` and
  `imported_html_lists_draw_their_own_counts_1989`.
