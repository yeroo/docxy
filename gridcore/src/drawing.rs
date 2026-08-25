//! Parse `xl/drawings/*.xml` anchors into [`Drawing`]s (pictures + charts), and
//! the cached data of `xl/charts/*.xml`. Enough to render a floating overlay in
//! the grid — not to edit the artwork.

use opccore::xml::{Event, XmlParser};

use crate::sheet::{ChartData, ChartSeries, Drawing, DrawingKind};

/// The local (namespace-stripped) part of an XML name.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Where an element name stops, by the same rule [`XmlParser`] uses. The raw
/// scanners below index anchors that `parse_drawings` numbered with the parser,
/// so the two have to agree: a name this stopped at `>`/` `/`/` alone would run
/// past `<xdr:twoCellAnchor\neditAs="oneCell">` and skip an anchor the parser
/// counted, shifting every later index — a move or a delete would then land on
/// someone else's picture.
fn is_name_end(c: char) -> bool {
    c.is_whitespace() || c == '>' || c == '/' || c == '='
}

/// `XmlParser::text()` hands back the RAW source slice, entities and all. Chart
/// strings used to be cosmetic (the part round-tripped verbatim), but an edited
/// chart is now regenerated through `esc_attr`, so an undecoded `&amp;` would
/// gain a level of escaping per save — and a sheet name inside a `<c:f>` would
/// stop resolving, in Excel and in our own `rename_sheet` matching alike.
fn decoded(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

/// Office's fixed EMU-per-pixel, plus rough default cell pixel sizes — used only
/// to estimate a `oneCellAnchor`/`absoluteAnchor` extent in whole cells.
const EMU_PER_PX: i64 = 9525;
const DEFAULT_COL_PX: i64 = 64;
const DEFAULT_ROW_PX: i64 = 20;

/// Parse a worksheet drawing part. `resolve_rid` maps a relationship id to its
/// `(lowercased relationship type, resolved part path)`; `get_part` reads a part's
/// text (used to pull in a referenced chart).
pub fn parse_drawings(
    xml: &str,
    resolve_rid: &impl Fn(&str) -> Option<(String, String)>,
    get_part: &impl Fn(&str) -> Option<String>,
) -> Vec<Drawing> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    let mut from: Option<(u32, u32)> = None;
    let mut to: Option<(u32, u32)> = None;
    let mut ext: Option<(i64, i64)> = None;
    let mut name = String::new();
    let mut kind: Option<DrawingKind> = None;
    // Counts every anchor, including the ones we skip, so each Drawing can point
    // back at the element it came from.
    let mut anchor_ix = 0usize;
    let mut seen = 0usize;
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" => {
                    anchor_ix = seen;
                    seen += 1;
                    from = None;
                    to = None;
                    ext = None;
                    name.clear();
                    kind = None;
                }
                "from" => from = Some(parse_anchor_cell(&mut p)),
                "to" => to = Some(parse_anchor_cell(&mut p)),
                "ext" => {
                    let cx = p.attr("cx").trim().parse::<i64>().unwrap_or(0);
                    let cy = p.attr("cy").trim().parse::<i64>().unwrap_or(0);
                    ext = Some((cx, cy));
                }
                "cNvPr" => {
                    let n = p.attr("name");
                    if !n.is_empty() {
                        name = n.to_string();
                    }
                }
                // A picture: <a:blip r:embed="rId#"> points at the media part.
                "blip" => {
                    if let Some(rid) = rel_attr(&p, "embed") {
                        if let Some((ty, part)) = resolve_rid(&rid) {
                            if ty.contains("image") || kind.is_none() {
                                kind = Some(DrawingKind::Image {
                                    part,
                                    name: if name.is_empty() {
                                        "Picture".to_string()
                                    } else {
                                        name.clone()
                                    },
                                });
                            }
                        }
                    }
                }
                // A chart: <c:chart r:id="rId#"> points at the chart part.
                "chart" => {
                    if let Some(rid) = rel_attr(&p, "id") {
                        if let Some((_, part)) = resolve_rid(&rid) {
                            if let Some(cxml) = get_part(&part) {
                                let mut cd = parse_chart(&cxml);
                                // Remember where it came from, so an edit can be
                                // written back into that part.
                                cd.part = Some(part);
                                kind = Some(DrawingKind::Chart(cd));
                            }
                        }
                    }
                }
                _ => {}
            },
            Event::End => {
                if matches!(
                    local(p.name()),
                    "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor"
                ) {
                    if let (Some(f), Some(k)) = (from, kind.take()) {
                        let t = to.or_else(|| ext.map(|e| estimate_to(f, e))).unwrap_or(f);
                        out.push(Drawing {
                            anchor_ix,
                            from: f,
                            to: t,
                            kind: k,
                        });
                    }
                    from = None;
                    to = None;
                    ext = None;
                }
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    out
}

/// Read an `r:`-prefixed attribute (`r:embed`, `r:id`) by its local name.
fn rel_attr(p: &XmlParser, local_name: &str) -> Option<String> {
    p.attrs()
        .iter()
        .find(|a| local(a.name) == local_name)
        .map(|a| a.value.to_string())
}

/// Parse a `<xdr:from>`/`<xdr:to>` block into a `(row, col)` cell.
fn parse_anchor_cell(p: &mut XmlParser) -> (u32, u32) {
    let (mut row, mut col) = (0u32, 0u32);
    let mut field = String::new();
    loop {
        match p.next() {
            Event::Start => field = local(p.name()).to_string(),
            Event::Text => {
                // Ignore inter-element whitespace (a failed parse keeps the value).
                if let Ok(v) = p.text().trim().parse::<u32>() {
                    match field.as_str() {
                        "col" => col = v,
                        "row" => row = v,
                        _ => {}
                    }
                }
            }
            Event::End if matches!(local(p.name()), "from" | "to") => break,
            Event::Eof => break,
            _ => {}
        }
    }
    // A crafted `<xdr:col>`/`<xdr:row>` parses as any u32, and consumers add to
    // it to bound a card's span (`from.1 + 1`, `ac + 256`): near u32::MAX that
    // panics a debug build and wraps a release one. `estimate_to` already
    // guards its own arithmetic; clamping here covers every consumer at once.
    (
        row.min(crate::sheet::MAX_ROWS - 1),
        col.min(crate::sheet::MAX_COLS - 1),
    )
}

/// One drawing's new home: its anchor index in the part, and the `(row, col)`
/// cells its `<from>` and `<to>` now sit on.
pub type AnchorMove = (usize, (u32, u32), (u32, u32));

/// Edit a drawing part in place: move the `<from>`/`<to>` cells of the anchors
/// in `moves`, drop the anchor elements listed in `drop`, and leave everything
/// else — other anchors, offsets, artwork — byte for byte as it was. Both are
/// keyed by [`Drawing::anchor_ix`], which counts every anchor in the part
/// (including the ones we don't model). A `oneCellAnchor` has no `<to>`; only
/// its `<from>` moves, and its extent rides along.
///
/// An anchor we can't find the end of means our indices no longer line up with
/// the part's, so the whole rewrite is abandoned and `xml` comes back unchanged
/// — better a move that didn't persist than moves applied to the wrong artwork.
pub fn rewrite_anchors(xml: &str, moves: &[AnchorMove], drop: &[usize]) -> String {
    if moves.is_empty() && drop.is_empty() {
        return xml.to_string();
    }
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    let mut ix = 0usize;
    while let Some((cut, tag)) = next_anchor(rest) {
        out.push_str(&rest[..cut]);
        rest = &rest[cut..];
        let Some(end) = find_close(rest, tag) else {
            return xml.to_string();
        };
        let (element, after) = rest.split_at(end);
        if !drop.contains(&ix) {
            match moves.iter().find(|(i, _, _)| *i == ix) {
                Some((_, from, to)) => out.push_str(&move_anchor(element, *from, *to)),
                None => out.push_str(element),
            }
        }
        ix += 1;
        rest = after;
    }
    out.push_str(rest);
    out
}

/// How many anchors a drawing part holds — the index the next one spliced in
/// at the end will occupy. Counts every anchor, including the ones we can't
/// render, exactly as [`parse_drawings`] numbers them.
pub fn count_anchors(xml: &str) -> usize {
    let mut n = 0;
    let mut rest = xml;
    while let Some((cut, tag)) = next_anchor(rest) {
        rest = &rest[cut..];
        let Some(end) = find_close(rest, tag) else {
            break;
        };
        n += 1;
        rest = &rest[end..];
    }
    n
}

/// The next anchor element start in `xml`: its offset and its full tag name.
fn next_anchor(xml: &str) -> Option<(usize, &str)> {
    let mut at = 0usize;
    while let Some(rel) = xml[at..].find('<') {
        let start = at + rel;
        // Non-element markup is skipped WHOLE. Scanning only past its opening
        // token would let `<!-- <xdr:twoCellAnchor> -->` count as an anchor
        // here but not in `parse_drawings`, and every index after it — which is
        // how a move or a delete finds its element — would be off by one.
        let skip = |tok: &str, end: &str| {
            xml[start..].starts_with(tok).then(|| {
                xml[start + tok.len()..]
                    .find(end)
                    .map(|i| start + tok.len() + i + end.len())
                    .unwrap_or(xml.len())
            })
        };
        if let Some(past) = skip("<!--", "-->")
            .or_else(|| skip("<![CDATA[", "]]>"))
            .or_else(|| skip("<?", "?>"))
            .or_else(|| skip("<!", ">"))
        {
            at = past;
            continue;
        }
        let name_end = xml[start + 1..].find(is_name_end)? + start + 1;
        let name = &xml[start + 1..name_end];
        // Closing tags share the local name, so only openers count.
        let l = if name.starts_with('/') {
            ""
        } else {
            local(name)
        };
        if matches!(l, "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor") {
            return Some((start, name));
        }
        at = name_end;
    }
    None
}

/// Rewrite one anchor element's `<from>`/`<to>` cells.
fn move_anchor(element: &str, from: (u32, u32), to: (u32, u32)) -> String {
    let mut out = String::with_capacity(element.len());
    let mut rest = element;
    let mut at = 0usize;
    while let Some(rel) = rest[at..].find('<') {
        let start = at + rel;
        let Some(name_end) = rest[start + 1..].find(is_name_end).map(|i| i + start + 1) else {
            break;
        };
        let name = &rest[start + 1..name_end];
        let side = if name.starts_with(['/', '!', '?']) {
            None
        } else {
            match local(name) {
                "from" => Some(from),
                "to" => Some(to),
                _ => None,
            }
        };
        match side.and_then(|cell| find_close(&rest[start..], name).map(|e| (cell, start + e))) {
            Some(((row, col), end)) => {
                out.push_str(&rest[..start]);
                out.push_str(&set_cell_fields(&rest[start..end], row, col));
                rest = &rest[end..];
                at = 0;
            }
            None => at = name_end,
        }
    }
    out.push_str(rest);
    out
}

/// The offset just past `</tag>` in `xml`, which must start at `<tag…`.
fn find_close(xml: &str, tag: &str) -> Option<usize> {
    // `<xdr:twoCellAnchor/>` closes itself, and `parse_drawings` counts it like
    // any other. Looking for `</…>` here would swallow everything up to the NEXT
    // anchor's close and merge two elements into one index — a later move or
    // delete would then land on the wrong artwork.
    let gt = xml.find('>')?;
    if xml[..gt].ends_with('/') {
        return Some(gt + 1);
    }
    let close = format!("</{tag}>");
    xml.find(&close).map(|i| i + close.len())
}

/// Replace the `<col>`/`<row>` values inside one anchor-cell block.
fn set_cell_fields(block: &str, row: u32, col: u32) -> String {
    let mut out = String::with_capacity(block.len());
    let mut rest = block;
    while let Some(rel) = rest.find('<') {
        let (head, tail) = rest.split_at(rel);
        out.push_str(head);
        let Some(name_end) = tail[1..].find(is_name_end).map(|i| i + 1) else {
            out.push_str(tail);
            return out;
        };
        let name = &tail[1..name_end];
        let value = match local(name) {
            "col" => Some(col),
            "row" => Some(row),
            _ => None,
        };
        match value.zip(find_close(tail, name)) {
            Some((v, end)) => {
                out.push_str(&format!("<{name}>{v}</{name}>"));
                rest = &tail[end..];
            }
            None => {
                out.push_str(&tail[..name_end]);
                rest = &tail[name_end..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Estimate a bottom-right cell from a top-left anchor plus an EMU extent.
fn estimate_to(from: (u32, u32), ext: (i64, i64)) -> (u32, u32) {
    let cols = (ext.0 / (DEFAULT_COL_PX * EMU_PER_PX)).max(0) as u32;
    let rows = (ext.1 / (DEFAULT_ROW_PX * EMU_PER_PX)).max(0) as u32;
    // A crafted `<a:ext>` can push either past u32 — a debug build would panic
    // on the add, a release build would wrap the card to the top-left.
    (
        from.0.saturating_add(rows.max(1)),
        from.1.saturating_add(cols.max(1)),
    )
}

/// How many cached points one chart part may hold, all its series and its
/// categories together. `<c:pt idx="…">` is an untrusted attribute that sizes a
/// `Vec`, and a per-series cap is no cap at all: `<c:ser>` may repeat freely, so
/// a ~1 MB chart part of tiny stanzas each holding one `<c:pt idx="1048575"/>`
/// would ask for tens of GB while merely opening the workbook. A full column's
/// worth across the whole part is far more than any real chart caches.
const MAX_CACHE_POINTS: usize = 1 << 20;

/// Grow `v` so index `i` exists, spending the part's remaining point budget.
/// `false` when the budget won't stretch that far, and the point is dropped.
fn fit_cache<T: Clone + Default>(v: &mut Vec<T>, i: usize, budget: &mut usize) -> bool {
    let Some(need) = (i + 1).checked_sub(v.len()) else {
        return true;
    };
    if need > *budget {
        return false;
    }
    *budget -= need;
    v.resize(i + 1, T::default());
    true
}

/// Parse the cached data of a chart part (`c:chartSpace`).
/// [`parse_chart`] for sibling modules' tests, so a writer can be checked
/// against the reader that has to understand it.
#[cfg(test)]
pub(crate) fn parse_chart_for_test(xml: &str) -> ChartData {
    parse_chart(xml)
}

/// Which way round a parsed chart reads its range.
///
/// SpreadsheetML has no orientation element, so this asks the question Excel
/// asks: what SHAPE is each series' `<c:val>`? One row across several columns
/// (`$B$2:$D$2`) is a row series; one column down several rows (`$B$2:$B$5`) is
/// a column series. A SCATTER's and a BUBBLE's numbers live in `<c:xVal>` /
/// `<c:yVal>` / `<c:bubbleSize>` instead, so their `ChartSeries::point_refs`
/// are measured the same way and vote alongside — without them those kinds have
/// no `<c:val>` at all and every one of them would answer "column" by default,
/// which `chart_set_kind`'s re-derivation now acts on. Only a MULTI-cell point
/// ref votes, and unlike a `<c:val>` cell a single-cell one is not kept as
/// stacked-cell evidence either: a one-point scatter is two single cells, and
/// stacked down one column they would otherwise satisfy every clause of the
/// stacked-cell rule below and answer `by_row` for a chart that says nothing
/// about orientation.
///
/// Ambiguity falls back to "column", because that is what every chart written
/// before orientation existed is, and what the panel can always show. What
/// counts as ambiguous:
///
/// - **A single cell** (`$B$2`) is one row AND one column at once — it fits
///   both readings, so it votes for neither. Several `<c:val>` cells can still
///   settle it between them; see below — point-ref cells cannot, per above.
/// - **A series with no readable ref**: `<c:numLit>` values, or a ref this
///   model cannot hold (`Sheet1!$B:$B`, a defined name) — in either slot, so a
///   scatter's unheld points (`ChartSeries::points_unheld`) are silent here too.
///   There is no shape to measure, so no vote — though such a chart's `<c:cat>` may still settle it
///   below, since the categories are read whatever the series are made of.
/// - **A rectangle** spanning both ways (`$B$2:$D$5`) is a shape neither
///   reading produces — a hand-authored or foreign chart. No vote.
/// - **Series that disagree**: a row vote must be UNANIMOUS to win. A chart
///   mixing the two is one this model cannot re-derive either way round, and
///   the column reading is the safer of the two to hand the user, since
///   `ChartSeries::col` and `ChartSource::cat_col` both assume it.
///
/// With no votes at all — the chart has no series, or none with a ref — the
/// CATEGORIES are asked next (see below), and only if they have no shape of
/// their own is the answer column.
///
/// One shape LOOKS ambiguous cell by cell and isn't: several single-cell series
/// STACKED DOWN ONE COLUMN (`$B$2`, `$B$3`, `$B$4`). That is what a row chart
/// over a two-column range comes to — one label column and one numeric column,
/// so every row series is one cell wide — and the column DERIVATION cannot
/// produce it, since it emits one series PER COLUMN and two series therefore
/// never share a column. The model can still reach it another way: nothing stops
/// a user re-pointing two column series at single cells one above the other,
/// since `series_values_shape_err` only refuses a ref spanning several columns.
/// So the categories are consulted first — a column of labels is the column
/// reading's, a row of them the row reading's, and the panel's
/// `categories_shape_err` refuses the other line on either orientation so that
/// this stays true of anything docxy itself commits.
///
/// The stacked-cell rule therefore decides only when the categories offer
/// nothing: one cell, or absent. Neither branch is a proof, and both are
/// guesses the rule takes knowingly:
///
/// - ONE CELL is what the rule is FOR — a row chart's labels are one cell when
///   its range is two columns wide — but it is not exclusive to that case. A
///   COLUMN chart one data row deep has one-cell labels too (`chart_from_columns`
///   over `Item|Qty|Price|Total` × one row gives `A2`), and `categories_shape_err`
///   admits a single cell on either orientation, so re-pointing that chart's
///   series down a column walks it into the stacked shape with its category
///   unchanged.
/// - ABSENT is the same story without the labels: `chart_from_columns` leaves
///   `categories_ref` `None` whenever every column in the range is numeric
///   (`Year | Sales`), so an all-numeric COLUMN chart whose every series has
///   been re-pointed at a single cell lands here too.
///
/// Both residues are narrower than they sound — one series left with a multi-row
/// ref votes column and the early return above settles it — and what
/// `categories_shape_err` really closes is the multi-cell LINE, not the single
/// cell: a chart docxy wrote can never come back with its labels running the
/// other way, only with them saying nothing.
///
/// The LOAD itself survives the wrong answer — `parse_chart`
/// builds one `ChartSeries` per `<c:ser>` and never folds — but the orientation
/// does not: the panel comes back on the wrong reading, and committing DATA
/// RANGE, which re-derives the box the way the chart already reads it, folds
/// the N one-point series into one N-point series. `Switch Row/Column` is the
/// one re-derivation that cannot fold, because it inverts the flag rather than
/// keeping it — on a wrongly-inferred chart the first press hands back the
/// reading the chart should have had all along, so the button LOOKS like it did
/// nothing and the fold waits for the second press. So they are counted as row
/// evidence.
/// A single one on its own stays ambiguous (a 2x2 range), as does a row of them
/// (a column chart over a range one data row deep).
fn infer_by_row(cd: &ChartData) -> bool {
    let (mut rows, mut cols) = (0usize, 0usize);
    // The single-cell series, in the order they appear. Each keeps the SHEET it
    // was read from: "stacked in one column" is a claim about one grid, and two
    // cells on different sheets are not stacked however their coordinates line
    // up.
    let mut cells: Vec<(&str, u32, u32)> = Vec::new();
    for s in &cd.series {
        // A scatter's and a bubble's numbers arrive under `<c:xVal>`/`<c:yVal>`
        // rather than `<c:val>`, so `values_ref` is `None` for every series they
        // have and the tally would otherwise see NOTHING — dropping through to
        // the empty-`cells` fallback and answering `false` for every one of
        // them, whichever way round it was laid out. That answer is no longer
        // just the panel's reading: `chart_set_kind` re-derives such a chart
        // through `chart_from_range`, so a row-laid scatter read column-wise
        // comes back as N one-point series.
        //
        // The point refs have a shape to read like any other, and both of a
        // series' refs run the same way — an X row is paired with a Y row — so
        // they vote together. Only MULTI-cell ones are counted: a one-point
        // scatter's X and Y are two single cells side by side, which says
        // nothing about orientation and would be read as un-stacked evidence by
        // the fallback below.
        for v in &s.point_refs {
            let (r1, c1, r2, c2) = v.range;
            match (r1 == r2, c1 == c2) {
                (true, false) => rows += 1,
                (false, true) => cols += 1,
                _ => {}
            }
        }
        let Some(v) = &s.values_ref else { continue };
        let (r1, c1, r2, c2) = v.range;
        match (r1 == r2, c1 == c2) {
            (true, true) => cells.push((v.sheet.as_str(), r1, c1)),
            (true, false) => rows += 1,
            (false, true) => cols += 1,
            _ => {}
        }
    }
    if rows > 0 || cols > 0 {
        return rows > 0 && cols == 0;
    }
    // Nothing had a shape of its own. The categories settle it if THEY have
    // one: labels running down a column are the column reading's label column
    // (a row chart's labels are its header ROW), and labels running along a row
    // are the row reading's. The panel's `categories_shape_err` refuses the
    // other line on either orientation, so a chart docxy wrote never contradicts
    // itself here. A single-cell or absent `<c:cat>` says nothing.
    if let Some(cat) = &cd.categories_ref {
        let (r1, c1, r2, c2) = cat.range;
        match (r1 == r2, c1 == c2) {
            (false, true) => return false,
            (true, false) => return true,
            _ => {}
        }
    }
    // Still nothing. Stacked single cells are row evidence — on ONE sheet,
    // compared the way a sheet name resolves everywhere else.
    cells.len() > 1
        && cells
            .iter()
            .all(|c| c.0.eq_ignore_ascii_case(cells[0].0) && c.2 == cells[0].2)
        && cells.iter().any(|c| c.1 != cells[0].1)
}

/// Fold one reference into the chart's overall box: the first ref seeds it, a
/// later one on the SAME sheet stretches it, and one naming another sheet is
/// skipped.
///
/// Skipping rather than replacing is what keeps the box describing the CHART:
/// [`ChartSource::union`](crate::sheet::ChartSource::union) keeps the receiver's
/// sheet, so unioning across sheets would leave the box naming one sheet and
/// covering the other's cells. The panel's `rebuild_source` folds the same way
/// in the same order, which is what makes a chart read identically before and
/// after a save.
///
/// `pub` for exactly that reason: the suite's `rebuild_source` CALLS this
/// rather than keeping its own copy. The two must decide a cross-sheet ref
/// identically or a chart reads one way before a save and another after it,
/// and an invariant two crates have to satisfy byte for byte is not one to
/// maintain by hand in two places. The slot ORDER stays with each caller;
/// only the one-ref-into-the-box decision is shared.
pub fn fold_source(box_: &mut Option<crate::sheet::ChartSource>, src: crate::sheet::ChartSource) {
    match box_ {
        // Case-insensitively, because that is how a sheet name resolves
        // everywhere else — `Budget!$B$2` and `budget!$B$3` are one sheet's
        // cells, and comparing them byte for byte would drop the second.
        Some(cur) if cur.sheet.eq_ignore_ascii_case(&src.sheet) => cur.union(&src),
        Some(_) => {}
        slot => *slot = Some(src),
    }
}

fn parse_chart(xml: &str) -> ChartData {
    let mut cd = ChartData::default();
    let mut budget = MAX_CACHE_POINTS;
    let mut p = XmlParser::new(xml);
    let mut in_title = false;
    let mut in_title_text = false;
    let mut in_v = false;
    // Inside a <c:barChart> — its <c:barDir> refines "column" vs "bar".
    let mut in_bar = false;
    let mut mode = 0u8; // 1 = series name (tx), 2 = category (cat), 3 = value (val)
    // <c:f> holds the range a cat/val block reads from; their union is the
    // chart's source range. Only fills directly on a <c:ser> count as the
    // series colour (a fill nested in the data points or the plot area is a
    // different thing).
    let mut in_f = false;
    // Inside the open series' own `<c:spPr>` — see the `"spPr"` arm below.
    let mut in_ser_fill = false;
    let mut ser_depth = 0i32;
    // Inside an axis (<c:catAx>/<c:valAx>/…). Every axis may carry a <c:title>
    // of its own, and its text is not the chart's.
    let mut in_axis = false;
    // How many `*Chart` plot groups the plot area holds, and the first group's
    // `<c:grouping val="…"/>`. Together they decide `cd.complex`.
    let mut groups = 0usize;
    let mut grouping = String::new();
    // A `<c:f>` inside a series that `parse_f_ref` couldn't read.
    let mut unparsed_ref = false;
    // Inside a `<c:cat><c:multiLvlStrRef>`.
    let mut multi_lvl = false;
    // Element depth, and the depth of the open `<c:ser>` (-1 outside one). What
    // a series PLOTS is named by its direct children; `<c:dLbls>`, `<c:errBars>`
    // and `<c:trendline>` sit at that same level with refs of their own, and
    // the first and last wrap a `<c:tx>` that would otherwise read as the
    // series' name.
    let mut depth = 0i32;
    let mut ser_at = -1i32;
    // Inside the point arrays a scatter or bubble chart plots instead of
    // `<c:cat>`/`<c:val>`.
    let mut in_pts = false;
    // Whether the OPEN `<c:xVal>`/`<c:yVal>`/`<c:bubbleSize>` yielded a ref we
    // could hold. Its close is the only place that knows the element carried
    // points nothing on the series records — a `<c:numLit>` has no `<c:f>` to
    // fail on, so there is no per-ref moment to catch it at.
    let mut pts_held = false;
    // Whether the OPEN point element carried any points AT ALL — an `<c:f>`
    // (held or refused), a `<c:pt>`, or a positive `<c:ptCount>`. A
    // schema-legal EMPTY source (`<c:numLit><c:ptCount val="0"/></c:numLit>`,
    // which is what a series added with no data looks like) holds nothing to
    // lose, so it must not be marked `points_unheld` alongside the literal
    // scatter that does: the mark sends `chart_set_kind` down the re-author
    // door, and a chart that plots nothing would be refused there for a box it
    // has no reason to need.
    let mut pts_seen = false;
    // Whether the OPEN point element named CELLS this model could not hold — an
    // `<c:f>` with text in it that `parse_f_ref` refused. That is the half of
    // `points_unheld` which says the chart's box is short of its plot: the ref
    // names cells and the fold skipped every one of them. A `<c:numLit>` names
    // no cells and leaves this false, which is what lets the panel's re-author
    // door tell "no box could have covered these" from "the box misses half the
    // plot". See `ChartSeries::points_ref_unheld`.
    let mut pts_ref_bad = false;
    // Every series' CATEGORY and NAME ref, folded into the box only once the
    // loop has ended — see `fold_source` and the two folds after it. What a
    // chart IS is the numbers it plots, so `<c:val>` (and a scatter's points)
    // decide which sheet the box names; labels and headers stretch it after.
    let mut cat_boxes: Vec<crate::sheet::ChartSource> = Vec::new();
    let mut name_boxes: Vec<crate::sheet::ChartSource> = Vec::new();
    // The `idx` of the open `<c:pt>`. Excel writes SPARSE caches — a blank or
    // non-numeric source cell simply has no `<c:pt>` — so appending in document
    // order shifts everything after a gap one place left, and an edited chart
    // would then write that shift back to disk.
    let mut pt_idx: Option<usize> = None;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                let name = local(p.name());
                // A direct child of the open `<c:ser>`.
                let plotted = depth == ser_at + 1;
                match name {
                    n if n.ends_with("Chart") => {
                        // One plot area may hold SEVERAL of these — a combo
                        // chart, bars and a line together. `kind` records the
                        // first; the count is what tells the writer to keep its
                        // hands off (see `ChartData::complex`).
                        groups += 1;
                        if cd.kind.is_empty() {
                            // `barChart` covers BOTH orientations — the
                            // following <c:barDir val="col|bar"/> decides.
                            // Default to "column" (OOXML's own default is col)
                            // and refine on barDir.
                            cd.kind = match n.trim_end_matches("Chart") {
                                "bar" => {
                                    in_bar = true;
                                    "column".to_string()
                                }
                                other => other.to_string(),
                            };
                        }
                    }
                    // How the series sit against each other. The writer emits
                    // `clustered` (bar/column) or `standard` (line) and nothing
                    // else, so anything stacked has to round-trip verbatim.
                    "grouping" if grouping.is_empty() => {
                        grouping = p.attr("val").trim().to_string();
                    }
                    // Orientation of the enclosing barChart: col = vertical
                    // columns, bar = horizontal bars.
                    "barDir" if in_bar => {
                        cd.kind = if p.attr("val") == "bar" {
                            "bar"
                        } else {
                            "column"
                        }
                        .to_string();
                    }
                    n if n.ends_with("Ax") && n.len() > 2 => in_axis = true,
                    // Only the chart's own title names the chart: an axis title
                    // would otherwise be appended to it ("Sales" + "Quarter").
                    "title" => in_title = !in_axis,
                    "ser" => {
                        cd.series.push(ChartSeries::default());
                        ser_depth += 1;
                        ser_at = depth;
                    }
                    // A `<c:spPr>` that is a DIRECT child of the open series.
                    // Everything under it — `<a:solidFill>` for a bar's fill,
                    // `<a:ln><a:solidFill>` for a line's stroke — is the series'
                    // own colour.
                    "spPr" if depth == ser_at + 1 => in_ser_fill = true,
                    "tx" if plotted => mode = 1,
                    "cat" if plotted => mode = 2,
                    // The ELEMENT is what says a category is multi-level, not
                    // the shape of its `<c:f>`: one category over two levels
                    // names a LINE (`Sales!$A$2:$B$2`), which no shape test can
                    // tell from an ordinary row of labels.
                    "multiLvlStrRef" if mode == 2 => multi_lvl = true,
                    "val" if plotted => mode = 3,
                    // A scatter's/bubble's points: plotted cells, but under
                    // names `mode` doesn't cover.
                    "xVal" | "yVal" | "bubbleSize" if plotted => {
                        in_pts = true;
                        pts_held = false;
                        pts_seen = false;
                        pts_ref_bad = false;
                    }
                    // A cache or literal that says how many points it holds.
                    // Read only to tell an EMPTY point element from one whose
                    // points went unheld; `<c:pt>` is what actually carries them.
                    "ptCount" if in_pts => {
                        pts_seen |= p.attr("val").trim().parse::<u64>().is_ok_and(|n| n > 0)
                    }
                    // Capped: `idx` is an untrusted attribute, and it sizes a
                    // Vec. No cache can outgrow the sheet it reads.
                    "pt" => {
                        pts_seen |= in_pts;
                        pt_idx = p
                            .attr("idx")
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .filter(|&i| i < crate::sheet::MAX_ROWS as usize)
                    }
                    "v" => in_v = true,
                    // Whether it holds a POINT is a question about its text, so
                    // `pts_seen` waits for the `Event::Text` arm below: `<c:f/>`
                    // and `<c:f></c:f>` open and close here naming no cells at
                    // all, and neither has a plot to lose.
                    "f" => in_f = true,
                    "t" if in_title => in_title_text = true,
                    // The series' OWN fill, which is the one `chart_space_xml`
                    // writes back. `<c:dPt>`, `<c:marker>`, `<c:dLbls>`,
                    // `<c:trendline>` and `<c:errBars>` all carry `<c:spPr>` of
                    // their own; taking a colour from one of those would render
                    // the card wrong and then persist a single point's colour as
                    // the whole series' fill on the next edit.
                    "srgbClr" if in_ser_fill => {
                        let val = p.attr("val");
                        let val = val.trim();
                        if let Some(rgb) =
                            u32::from_str_radix(val, 16).ok().filter(|_| val.len() == 6)
                        {
                            if let Some(sr) = cd.series.last_mut() {
                                sr.color.get_or_insert(rgb);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text => {
                // A cached value only says something about a series when it sits
                // INSIDE one. A chart or axis title linked to a cell caches its
                // text in a <c:v> under <c:tx> too, and would otherwise be read
                // as the last series' name.
                // (`in_f` and `in_title_text` need their own elements open, so
                // neither can be true here — dropping through costs nothing.)
                if in_v && ser_depth == 1 {
                    let t = decoded(p.text());
                    let t = t.trim();
                    match mode {
                        1 => {
                            if let Some(s) = cd.series.last_mut() {
                                if s.name.is_empty() {
                                    s.name = t.to_string();
                                }
                            }
                        }
                        // Categories are shared across series; take the first set.
                        2 => {
                            if cd.series.len() <= 1 {
                                let i = pt_idx.unwrap_or(cd.categories.len());
                                // First wins PER INDEX. A single-level cache
                                // writes each `idx` once, so this is a no-op for
                                // it; a `<c:multiLvlStrCache>` restarts `idx` at
                                // 0 for every `<c:lvl>`, and last-wins would let
                                // the outermost level (the quarters) overwrite
                                // the innermost (the months) and leave
                                // `Q1, Q1, Q1, Q2, …` on the card.
                                if fit_cache(&mut cd.categories, i, &mut budget)
                                    && cd.categories[i].is_empty()
                                {
                                    cd.categories[i] = t.to_string();
                                }
                            }
                        }
                        3 => {
                            if let (Ok(x), Some(s)) = (t.parse::<f64>(), cd.series.last_mut()) {
                                let i = pt_idx.unwrap_or(s.values.len());
                                if fit_cache(&mut s.values, i, &mut budget) {
                                    s.values[i] = x;
                                }
                            }
                        }
                        _ => {}
                    }
                // Only a <c:f> INSIDE a <c:ser> names data. `mode` alone isn't
                // enough: a chart title and every axis title are <c:tx> too, and
                // one linked to a cell would otherwise land on the last series'
                // name and widen the chart's box to cover the title cell.
                } else if in_f && ser_depth == 1 {
                    let raw = decoded(p.text()).trim().to_string();
                    // A NON-EMPTY formula under `<c:xVal>`/`<c:yVal>`/
                    // `<c:bubbleSize>` is point content whether or not
                    // `parse_f_ref` can hold it: held, it fills `point_refs`
                    // below; unheld, it is exactly the plot the close arm marks
                    // `points_unheld` for. An empty one names nothing and must
                    // leave the element as blank as `<c:ptCount val="0"/>` does.
                    pts_seen |= in_pts && !raw.is_empty();
                    // Excel's MULTI-LEVEL category (`<c:multiLvlStrRef>`, one
                    // `<c:lvl>` per level) names every level's cells in one
                    // `<c:f>`, where this model holds a single line of labels.
                    // `parse_f_ref` takes it happily, and regenerating the part
                    // would then write a one-level `<c:strRef>` naming EVERY
                    // level's cells beside a cache holding one level's labels —
                    // the ref and its cache in different orders, which is
                    // exactly what the panel's `categories_shape_err` refuses to
                    // let a user type. Import is the other door into that state;
                    // shut it the same way, by calling the ref one this model
                    // can't hold.
                    //
                    // Keyed on the ELEMENT, not the range: the usual multi-level
                    // `<c:f>` is a rectangle, but one category over two levels
                    // is the line `Sales!$A$2:$B$2` and one level over several
                    // categories is an ordinary line too. A shape test would
                    // take the first and there is nothing in the range to tell
                    // them apart. The rectangle stays refused on its own account
                    // — a `<c:cat>` spanning both ways is a shape neither
                    // reading produces however it was written.
                    let held = crate::sheet::ChartSource::parse_f_ref(&raw).filter(|s| {
                        mode != 2
                            || (!multi_lvl && (s.range.0 == s.range.2 || s.range.1 == s.range.3))
                    });
                    if let Some(src) = held {
                        // Each ref belongs to whatever block it sits in, so a
                        // series can later be re-pointed on its own; their union
                        // is the chart's overall box.
                        match mode {
                            1 => {
                                if let Some(sr) = cd.series.last_mut() {
                                    // First wins, like the cached name above: a
                                    // cell-linked data label (`<c:dLbl><c:tx>`)
                                    // opens `<c:tx>` inside the series too, and
                                    // last-wins would let it overwrite the
                                    // series' own name ref.
                                    sr.name_ref.get_or_insert(raw);
                                }
                            }
                            // Pair the ref with the cache we actually captured
                            // (the FIRST series' categories, above). Last-wins
                            // here would hand every series the last series'
                            // `<c:f>` alongside the first series' `<c:strCache>`
                            // — a silent re-point, and a cache contradicting its
                            // own ref.
                            2 => {
                                cd.categories_ref.get_or_insert_with(|| src.clone());
                            }
                            3 => {
                                if let Some(sr) = cd.series.last_mut() {
                                    sr.values_ref = Some(src.clone());
                                }
                            }
                            _ => {}
                        }
                        // Only what is PLOTTED belongs in the chart's box: the
                        // name cell, the categories, the values, and a
                        // scatter's/bubble's points. A `<c:ser>` also holds
                        // `<c:errBars>` and `<c:trendline>`, each with a `<c:f>`
                        // of its own and neither carrying a mode — folding those
                        // in widens the DATA RANGE the panel shows, and
                        // `chart_space_xml` derives a ref-less series' cells
                        // from `source`, so an edited chart would be written
                        // back plotting them.
                        //
                        // The CATEGORIES and the series' NAME cells wait until
                        // after the loop (see `cat_boxes`/`name_boxes`). Both
                        // are labels, not numbers, and Excel is happy for
                        // either to sit on another sheet than the values — so
                        // letting one seed the box would hand a single
                        // cross-sheet `<c:cat>` or `<c:tx>` the whole chart's
                        // sheet. `<c:val>` and a scatter's points fold here, in
                        // document order, and decide the sheet between them.
                        if mode == 1 {
                            name_boxes.push(src);
                        } else if mode == 2 {
                            cat_boxes.push(src);
                        } else if mode != 0 {
                            fold_source(&mut cd.source, src);
                        } else if in_pts {
                            // A scatter's/bubble's points are its numbers, but
                            // they arrive in their own elements rather than in
                            // `<c:val>`, so `values_ref` stays empty for them.
                            // Keep them on the series as well as in the box:
                            // the panel rebuilds the box from the slots it can
                            // see, and a scatter whose points were only ever
                            // folded here would rebuild out of its label cells
                            // alone.
                            if let Some(sr) = cd.series.last_mut() {
                                sr.point_refs.push(src.clone());
                            }
                            pts_held = true;
                            fold_source(&mut cd.source, src);
                        }
                    } else if mode != 0 {
                        // A ref this model can't hold: a whole column
                        // (`Sheet1!$B:$B`), a defined name, a multi-area ref, or
                        // the multi-level `<c:cat>` above.
                        // The slot stays empty, and regenerating the part would
                        // write the cached numbers back as `<c:numLit>` —
                        // turning a live, sheet-linked series into frozen
                        // literals. Keep the part verbatim instead; picking a
                        // type in the panel is still the way to author it
                        // afresh, exactly as for a stacked or combo chart.
                        unparsed_ref = true;
                    } else if in_pts && !raw.is_empty() {
                        // The same refusal under a point element, where there is
                        // no `mode` for the arm above to catch it by. It names
                        // cells the fold could not take, so the box comes out
                        // short of the plot — the one thing that separates this
                        // from a `<c:numLit>`, and what the re-author door
                        // refuses on. `complex` is deliberately NOT set here:
                        // unlike a `<c:cat>`/`<c:val>` the writer would flatten
                        // to literals, a scatter's points are already held back
                        // by `chart_would_lose_points`, which reads the mark
                        // this sets on the close.
                        pts_ref_bad = true;
                    }
                } else if in_title_text {
                    cd.title.push_str(&decoded(p.text()));
                }
            }
            Event::End => {
                match local(p.name()) {
                    n if n.ends_with("Ax") && n.len() > 2 => in_axis = false,
                    "title" => in_title = false,
                    "t" => in_title_text = false,
                    "v" => in_v = false,
                    "f" => in_f = false,
                    "pt" => pt_idx = None,
                    "spPr" if depth == ser_at + 1 => in_ser_fill = false,
                    "ser" => {
                        ser_depth -= 1;
                        ser_at = -1;
                        in_ser_fill = false;
                    }
                    "tx" | "val" => mode = 0,
                    "cat" => {
                        mode = 0;
                        multi_lvl = false;
                    }
                    // Points this model could not hold — literal `<c:numLit>`
                    // ones, or an `<c:f>` `parse_f_ref` refused. Nothing else on
                    // the series would remember them (a scatter has no
                    // `values_ref`, no `col` and no cached numbers either way),
                    // so say so here: an unmarked one is indistinguishable from
                    // the empty series "+ Series" pushes, and picking a writable
                    // type would then relabel it and let the next save write
                    // `<c:ptCount val="0"/>` over a plot it can never re-read.
                    //
                    // `pts_seen` keeps an EMPTY point element out of that: one
                    // holding no `<c:f>`, no `<c:pt>` and `<c:ptCount val="0"/>`
                    // has no plot to destroy, and marking it would send a chart
                    // that draws nothing down the re-author door to be refused
                    // for a box it never needed.
                    "xVal" | "yVal" | "bubbleSize" => {
                        if in_pts {
                            if let Some(sr) = cd.series.last_mut() {
                                if pts_seen && !pts_held {
                                    sr.points_unheld = true;
                                }
                                // The narrower mark, which only a refused `<c:f>`
                                // sets: it says the plot reaches cells the box
                                // does not, where the wider one also covers
                                // literals that reach no cells at all.
                                sr.points_ref_unheld |= pts_ref_bad;
                            }
                        }
                        in_pts = false;
                    }
                    _ => {}
                }
                depth -= 1;
            }
            Event::Eof => break,
        }
    }
    // The CATEGORIES, then the series' NAME cells, both held back above. They
    // belong in the box — leaving the categories out drops the label column and
    // leaving the names out drops the header row, shrinking a chart's DATA
    // RANGE from `A1:D5` to `B2:D5` — but neither may DECIDE which sheet the
    // box names. `<c:cat>` is one line of LABELS and `<c:tx>` one HEADER cell,
    // and Excel is happy for either to sit on another sheet than the numbers;
    // folded in document order (the writer emits `{tx}{cat}{val}`) a single
    // foreign one would seed the box, and every local `<c:val>` after it would
    // then be skipped for the sheet mismatch, collapsing the whole chart's box
    // onto that one foreign line. Folded after, they stretch the box the
    // plotted numbers already decided, or seed it only when nothing else did —
    // categories before names, so a chart whose series are all literal still
    // takes its sheet from its labels rather than from a header cell.
    for src in cat_boxes.into_iter().chain(name_boxes) {
        fold_source(&mut cd.source, src);
    }
    // A pie with several `<c:ser>` is the shape the writer cannot reproduce
    // without losing data: `chart_space_xml`'s pie arm emits `series.first()`
    // only. Nothing docxy derives can reach it — four of the five panel doors
    // ask `chart_kind_series_err` first, and `series_add` applies the same rule
    // in its own words (`n > 0` before the push) — but a foreign file may
    // already be there,
    // so hold it back rather than let the next edit regenerate it one slice
    // group short. Picking a type in the panel is still the way out, and that
    // door is guarded too.
    cd.complex = groups > 1
        || unparsed_ref
        || !matches!(grouping.as_str(), "" | "clustered" | "standard")
        || (cd.kind == "pie" && cd.series.len() > 1);
    cd.by_row = infer_by_row(&cd);
    // Which column the box calls its LABEL column, said outright rather than
    // inherited from whichever `<c:f>` happened to seed the box — the fold
    // order is about which SHEET wins, and leaning on it for `cat_col` too made
    // one answer hostage to the other.
    //
    // A COLUMN chart takes it from `<c:cat>`: that is where the labels really
    // live, so say so whenever the chart told us.
    //
    // A ROW chart cannot. Its labels run along a ROW and `cat_col` is a column
    // index, so `categories_ref.range.1` would be merely the left end of that
    // label row — the first CATEGORY's column, never the labels' own. What
    // `chart_from_rows` puts there is the column the SERIES NAMES come from, so
    // take it from the first series' name cell, which is that column.
    let cat_col = if cd.by_row {
        cd.series
            .first()
            .and_then(|s| s.name_ref.as_deref())
            .and_then(crate::sheet::ChartSource::parse_f_ref)
    } else {
        cd.categories_ref.clone()
    };
    if let (Some(src), Some(from)) = (cd.source.as_mut(), cat_col) {
        if src.sheet.eq_ignore_ascii_case(&from.sheet) {
            src.cat_col = from.range.1;
        }
    }
    cd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_cell_anchor_picture() {
        let xml = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
              <xdr:to><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>10</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
              <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Logo"/></xdr:nvPicPr>
                <xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic>
            </xdr:twoCellAnchor></xdr:wsDr>"#;
        let resolve = |rid: &str| {
            (rid == "rId1").then(|| ("image/png".to_string(), "xl/media/image1.png".to_string()))
        };
        let get = |_: &str| None;
        let ds = parse_drawings(xml, &resolve, &get);
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].from, (2, 1));
        assert_eq!(ds[0].to, (10, 5));
        match &ds[0].kind {
            DrawingKind::Image { part, name } => {
                assert_eq!(part, "xl/media/image1.png");
                assert_eq!(name, "Logo");
            }
            _ => panic!("expected image"),
        }
    }

    /// A crafted anchor cell is clamped to the sheet's own limits. Consumers add
    /// to it to bound a card's span (`from.1 + 1`, `ac + 256`), which near
    /// u32::MAX panics a debug build and wraps a release one.
    #[test]
    fn anchor_cells_are_clamped_to_the_sheet() {
        let xml = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>4294967295</xdr:col><xdr:row>4294967295</xdr:row></xdr:from>
              <xdr:to><xdr:col>4294967295</xdr:col><xdr:row>4294967295</xdr:row></xdr:to>
              <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Logo"/></xdr:nvPicPr>
                <xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic>
            </xdr:twoCellAnchor></xdr:wsDr>"#;
        let resolve = |rid: &str| {
            (rid == "rId1").then(|| ("image/png".to_string(), "xl/media/image1.png".to_string()))
        };
        let get = |_: &str| None;
        let ds = parse_drawings(xml, &resolve, &get);
        assert_eq!(ds.len(), 1);
        assert_eq!(
            ds[0].from,
            (crate::sheet::MAX_ROWS - 1, crate::sheet::MAX_COLS - 1)
        );
        assert_eq!(ds[0].to, ds[0].from);
        // The bound every consumer relies on: adding to the anchor can't overflow.
        assert!(ds[0].from.1.checked_add(256).is_some());
        assert!(ds[0].from.0.checked_add(1024).is_some());
    }

    /// Two anchors; the first holds a shape we don't model, so the picture's
    /// `anchor_ix` is 1 even though it is the only Drawing parsed.
    const TWO_ANCHORS: &str = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>0</xdr:col><xdr:colOff>7</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>9</xdr:rowOff></xdr:from>
              <xdr:to><xdr:col>2</xdr:col><xdr:row>2</xdr:row></xdr:to>
              <xdr:sp/>
            </xdr:twoCellAnchor>
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
              <xdr:to><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>10</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
              <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Logo"/></xdr:nvPicPr>
                <xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic>
            </xdr:twoCellAnchor></xdr:wsDr>"#;

    fn png_resolve(rid: &str) -> Option<(String, String)> {
        (rid == "rId1").then(|| ("image/png".to_string(), "xl/media/image1.png".to_string()))
    }

    #[test]
    fn rewrite_anchors_moves_the_indexed_anchor_and_nothing_else() {
        let get = |_: &str| None;
        let ds = parse_drawings(TWO_ANCHORS, &png_resolve, &get);
        assert_eq!(ds.len(), 1);
        assert_eq!(
            ds[0].anchor_ix, 1,
            "the unmodelled shape still occupies anchor 0"
        );

        // Move it three rows down and one column right.
        let out = rewrite_anchors(TWO_ANCHORS, &[(1, (5, 2), (13, 6))], &[]);
        let moved = parse_drawings(&out, &png_resolve, &get);
        assert_eq!(moved[0].from, (5, 2));
        assert_eq!(moved[0].to, (13, 6));
        // The untouched anchor and the offsets inside the moved one survive.
        assert!(out.contains("<xdr:col>0</xdr:col><xdr:colOff>7</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>9</xdr:rowOff>"));
        assert!(out.contains("<xdr:sp/>"));
        assert!(out.contains(r#"<xdr:cNvPr id="2" name="Logo"/>"#));
        assert!(
            out.contains("<xdr:colOff>0</xdr:colOff>"),
            "the moved anchor keeps its offsets"
        );
    }

    #[test]
    fn the_anchor_scanner_and_the_parser_agree_on_comments_and_self_closed_anchors() {
        // `rewrite_anchors` finds its element by counting anchors; `parse_drawings`
        // hands it the index. The two must count the same things, or a move (or a
        // chart delete) lands on somebody else's artwork.
        let get = |_: &str| None;
        // A comment mentioning an anchor is not an anchor.
        let commented = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
            <!-- <xdr:twoCellAnchor> was here -->
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>1</xdr:col><xdr:row>2</xdr:row></xdr:from>
              <xdr:to><xdr:col>5</xdr:col><xdr:row>10</xdr:row></xdr:to>
              <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Logo"/></xdr:nvPicPr>
                <xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic>
            </xdr:twoCellAnchor></xdr:wsDr>"#;
        let ds = parse_drawings(commented, &png_resolve, &get);
        assert_eq!(ds[0].anchor_ix, 0, "the comment is not an anchor");
        let out = rewrite_anchors(commented, &[(0, (4, 3), (12, 7))], &[]);
        let moved = parse_drawings(&out, &png_resolve, &get);
        assert_eq!((moved[0].from, moved[0].to), ((4, 3), (12, 7)));

        // A self-closed anchor still occupies an index of its own.
        let selfclosed = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b"><xdr:twoCellAnchor/>
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>1</xdr:col><xdr:row>2</xdr:row></xdr:from>
              <xdr:to><xdr:col>5</xdr:col><xdr:row>10</xdr:row></xdr:to>
              <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Logo"/></xdr:nvPicPr>
                <xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic>
            </xdr:twoCellAnchor></xdr:wsDr>"#;
        let ds = parse_drawings(selfclosed, &png_resolve, &get);
        assert_eq!(ds[0].anchor_ix, 1);
        let out = rewrite_anchors(selfclosed, &[(1, (4, 3), (12, 7))], &[]);
        let moved = parse_drawings(&out, &png_resolve, &get);
        assert_eq!((moved[0].from, moved[0].to), ((4, 3), (12, 7)));
        // Dropping the empty one leaves the picture behind.
        let out = rewrite_anchors(selfclosed, &[], &[0]);
        assert_eq!(parse_drawings(&out, &png_resolve, &get).len(), 1);
    }

    #[test]
    fn rewrite_anchors_with_nothing_to_do_is_byte_for_byte() {
        assert_eq!(rewrite_anchors(TWO_ANCHORS, &[], &[]), TWO_ANCHORS);
        // An index nobody claims leaves every anchor as it was.
        assert_eq!(
            rewrite_anchors(TWO_ANCHORS, &[(9, (1, 1), (2, 2))], &[]),
            TWO_ANCHORS
        );
    }

    #[test]
    fn rewrite_anchors_drops_only_the_listed_anchor() {
        let get = |_: &str| None;
        let culled = rewrite_anchors(TWO_ANCHORS, &[], &[1]);
        assert!(parse_drawings(&culled, &png_resolve, &get).is_empty());
        assert!(culled.contains("<xdr:sp/>"), "the shape's anchor stays");
        assert!(!culled.contains("Logo"));
        assert_eq!(culled.matches("<xdr:twoCellAnchor>").count(), 1);
    }

    #[test]
    fn rewrite_anchors_moves_a_one_cell_anchor_by_its_from_alone() {
        // A `oneCellAnchor` has no `<to>` — its size is the `<xdr:ext>`, which
        // must ride along untouched when the anchor moves.
        let xml = r#"<xdr:wsDr xmlns:xdr="a">
            <xdr:oneCellAnchor>
              <xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
              <xdr:ext cx="2857500" cy="1428750"/>
              <xdr:sp/>
            </xdr:oneCellAnchor></xdr:wsDr>"#;
        let out = rewrite_anchors(xml, &[(0, (7, 3), (99, 99))], &[]);
        assert!(
            out.contains("<xdr:col>3</xdr:col>"),
            "the from column moved"
        );
        assert!(out.contains("<xdr:row>7</xdr:row>"), "the from row moved");
        assert!(
            out.contains(r#"<xdr:ext cx="2857500" cy="1428750"/>"#),
            "the extent is untouched"
        );
        assert!(
            !out.contains("99"),
            "there is no <to> to write the far corner into"
        );
    }

    #[test]
    fn rewrite_anchors_abandons_the_whole_rewrite_on_an_anchor_it_cannot_close() {
        // No `</xdr:twoCellAnchor>`: past this point our anchor indices and the
        // part's no longer agree, so nothing may be rewritten at all.
        let xml = r#"<xdr:wsDr xmlns:xdr="a">
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>
            </xdr:wsDr>"#;
        assert_eq!(rewrite_anchors(xml, &[(0, (5, 5), (9, 9))], &[]), xml);
        assert_eq!(rewrite_anchors(xml, &[], &[0]), xml);
    }

    #[test]
    fn chart_refs_and_series_colours_round_trip() {
        // A range-backed chart: the refs give the source box, the caches give
        // the values, and one series carries an explicit fill.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Budget!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:spPr><a:solidFill><a:srgbClr val="C0705A"/></a:solidFill></c:spPr>
            <c:cat><c:strRef><c:f>Budget!$A$2:$A$3</c:f><c:strCache><c:pt idx="0"><c:v>Laptop</c:v></c:pt><c:pt idx="1"><c:v>Dock</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.kind, "column");
        assert_eq!(cd.categories, vec!["Laptop", "Dock"]);
        assert_eq!(cd.series[0].values, vec![2.0, 5.0]);
        assert_eq!(cd.series[0].color, Some(0xC0705A));
        // The refs union into the whole box, header row and labels included.
        let src = cd.source.expect("source range");
        assert_eq!(src.sheet, "Budget");
        assert_eq!(src.range, (0, 0, 2, 1));
    }

    #[test]
    fn a_data_points_fill_is_not_the_series_colour() {
        // A `<c:dPt>` colours ONE bar. `chart_space_xml` writes `s.color` back
        // as the whole series' fill, so taking it here would make the first
        // subsequent edit persist one point's colour over the lot. Same for the
        // marker, the labels, the trendline and the error bars.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:dPt><c:idx val="1"/><c:spPr><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></c:spPr></c:dPt>
            <c:dLbls><c:spPr><a:solidFill><a:srgbClr val="00FF00"/></a:solidFill></c:spPr></c:dLbls>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        assert_eq!(parse_chart(xml).series[0].color, None);
    }

    #[test]
    fn a_line_series_takes_its_colour_from_its_own_stroke() {
        // A line's colour lives at `<c:spPr><a:ln><a:solidFill>`, one level
        // deeper than a bar's fill — the series' own `<c:spPr>` is what makes it
        // the series' colour, not the depth it sits at.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:lineChart>
          <c:ser><c:idx val="0"/>
            <c:spPr><a:ln w="28575"><a:solidFill><a:srgbClr val="4472C4"/></a:solidFill></a:ln></c:spPr>
            <c:marker><c:spPr><a:solidFill><a:srgbClr val="ED7D31"/></a:solidFill></c:spPr></c:marker>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#;
        assert_eq!(parse_chart(xml).series[0].color, Some(0x4472C4));
    }

    #[test]
    fn a_charts_caches_cannot_outgrow_the_parts_point_budget() {
        // `idx` is an untrusted attribute that sizes a Vec, and `<c:ser>` may
        // repeat freely — so a per-series cap is none at all. A handful of tiny
        // stanzas each claiming the last row must not allocate GBs.
        let ser = format!(
            r#"<c:ser><c:val><c:numRef><c:numCache><c:pt idx="{}"><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>"#,
            crate::sheet::MAX_ROWS - 1
        );
        let xml = format!(
            r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart>{}</c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ser.repeat(8)
        );
        let cd = parse_chart(&xml);
        let total: usize = cd.series.iter().map(|s| s.values.len()).sum();
        assert_eq!(cd.series.len(), 8);
        assert!(total <= MAX_CACHE_POINTS, "{total} points cached");
    }

    #[test]
    fn a_sparse_cache_keeps_its_points_where_excel_put_them() {
        // Excel omits the `<c:pt>` for a blank or non-numeric source cell, so a
        // cache is sparse. Appending in document order shifts everything after
        // the gap one place left — and an edited chart writes that shift back.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:cat><c:strRef><c:f>Budget!$A$2:$A$5</c:f><c:strCache><c:ptCount val="4"/><c:pt idx="0"><c:v>Jan</c:v></c:pt><c:pt idx="3"><c:v>Apr</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$5</c:f><c:numCache><c:ptCount val="4"/><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="2"><c:v>30</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.series[0].values, vec![10.0, 0.0, 30.0]);
        assert_eq!(cd.categories, vec!["Jan", "", "", "Apr"]);
        // An `idx` past the sheet is not a point, and must not size a Vec.
        let huge = xml.replace("idx=\"3\"", "idx=\"4294967295\"");
        assert_eq!(parse_chart(&huge).categories, vec!["Jan", "Apr"]);
    }

    #[test]
    fn a_ref_this_model_cant_hold_keeps_the_whole_part_verbatim() {
        // A whole-column ref is a shape `parse_f_ref` declines, so the series
        // would keep its cached numbers but lose its link — and regenerating
        // the part would write those numbers back as `<c:numLit>`, freezing a
        // live chart. `complex` is the existing "keep it as Excel wrote it"
        // escape hatch.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:val><c:numRef><c:f>Budget!$B:$B</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.series[0].values, vec![2.0], "the cache still draws it");
        assert_eq!(cd.series[0].values_ref, None);
        assert!(cd.complex, "so the part is not regenerated");
        assert!(!crate::xlsx::chart_is_writable(&cd));
        // An ordinary ref leaves the chart writable.
        let ok = xml.replace("$B:$B", "$B$2:$B$3");
        assert!(!parse_chart(&ok).complex);
    }

    #[test]
    fn a_pie_that_arrives_with_two_series_is_kept_as_excel_wrote_it() {
        // The writer's pie arm emits `series.first()` only, so regenerating
        // this part would drop the second series without a word. No panel door
        // can build one — four ask `chart_kind_series_err`, and `series_add`
        // asks the same question inline — but the schema permits it, so a
        // foreign file can arrive already there.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:pieChart>
          <c:ser><c:idx val="0"/>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser>
          <c:ser><c:idx val="1"/>
            <c:val><c:numRef><c:f>Budget!$C$2:$C$3</c:f><c:numCache><c:pt idx="0"><c:v>4</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.kind, "pie");
        assert_eq!(cd.series.len(), 2, "both are loaded and both are drawn");
        assert!(cd.complex, "so the part is not regenerated");
        assert!(!crate::xlsx::chart_is_writable(&cd));
        // One series is the ordinary pie, and stays editable.
        let one = xml.replace(
            r#"<c:ser><c:idx val="1"/>
            <c:val><c:numRef><c:f>Budget!$C$2:$C$3</c:f><c:numCache><c:pt idx="0"><c:v>4</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser>"#,
            "",
        );
        let cd = parse_chart(&one);
        assert_eq!(cd.series.len(), 1);
        assert!(!cd.complex);
    }

    #[test]
    fn the_source_box_takes_its_label_column_from_the_category_ref() {
        // A column chart takes `cat_col` from `<c:cat>` — column A here — not
        // from whichever `<c:f>` seeded the box, which is the values ref in
        // column B. `chart_space_xml` derives `<c:cat>` from `cat_col` when a
        // chart has no category ref of its own, so it must be the LABEL column.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Budget!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Budget!$A$2:$A$3</c:f><c:strCache><c:pt idx="0"><c:v>Laptop</c:v></c:pt><c:pt idx="1"><c:v>Dock</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.source.expect("source").cat_col, 0, "column A, not B");
    }

    #[test]
    fn per_series_refs_survive_a_parse_write_parse_round_trip() {
        use crate::sheet::{ChartSeries, ChartSource};
        // Two series reading different columns, plus their own category ref.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Budget!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Budget!$A$2:$A$3</c:f><c:strCache><c:pt idx="0"><c:v>Laptop</c:v></c:pt><c:pt idx="1"><c:v>Dock</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser>
          <c:ser><c:idx val="1"/>
            <c:tx><c:strRef><c:f>Budget!$D$1</c:f><c:strCache><c:pt idx="0"><c:v>Total</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Budget!$A$2:$A$3</c:f><c:strCache><c:pt idx="0"><c:v>Laptop</c:v></c:pt><c:pt idx="1"><c:v>Dock</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$D$2:$D$3</c:f><c:numCache><c:pt idx="0"><c:v>2398</c:v></c:pt><c:pt idx="1"><c:v>358</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.series.len(), 2);
        // Each series kept its OWN values range, not the chart's overall box.
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|v| v.range),
            Some((1, 1, 2, 1))
        );
        assert_eq!(
            cd.series[1].values_ref.as_ref().map(|v| v.range),
            Some((1, 3, 2, 3))
        );
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("Budget!$B$1"));
        assert_eq!(cd.series[1].name_ref.as_deref(), Some("Budget!$D$1"));
        assert_eq!(
            cd.categories_ref.as_ref().map(|v| v.range),
            Some((1, 0, 2, 0))
        );
        // Their union is still the whole box the panel shows.
        assert_eq!(cd.source.as_ref().map(|v| v.range), Some((0, 0, 2, 3)));

        // Writing and re-reading keeps every one of them.
        let again = parse_chart(&crate::xlsx::chart_space_xml(&cd));
        assert_eq!(
            again.series[0].values_ref.as_ref().map(|v| v.range),
            Some((1, 1, 2, 1))
        );
        assert_eq!(
            again.series[1].values_ref.as_ref().map(|v| v.range),
            Some((1, 3, 2, 3))
        );
        assert_eq!(
            again.categories_ref.as_ref().map(|v| v.range),
            Some((1, 0, 2, 0))
        );
        assert_eq!(again.series[1].name_ref.as_deref(), Some("Budget!$D$1"));
        assert_eq!(again.series[1].values, vec![2398.0, 358.0]);

        // A series with no ref of its own still falls back to the chart's box.
        let derived = ChartData {
            series: vec![ChartSeries {
                name: "Qty".into(),
                values: vec![1.0],
                col: Some(1),
                ..Default::default()
            }],
            categories: vec!["Laptop".into()],
            source: Some(ChartSource {
                sheet: "Budget".into(),
                range: (0, 0, 1, 1),
                cat_col: 0,
            }),
            ..Default::default()
        };
        let out = crate::xlsx::chart_space_xml(&derived);
        assert!(
            out.contains("<c:f>Budget!$B$2:$B$2</c:f>"),
            "derived value ref: {out}"
        );
        assert!(
            out.contains("<c:f>Budget!$A$2:$A$2</c:f>"),
            "derived category ref: {out}"
        );
    }

    #[test]
    fn typed_in_categories_are_not_given_a_reference_they_never_had() {
        // Categories written as `<c:strLit>` came from nobody's cells, so the
        // chart never said which column its labels live in and `source.cat_col`
        // falls back to a column some series plots — so deriving
        // `<c:cat><c:strRef>` from it would hand Excel a ref to refresh the
        // user's typed labels away from.
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Sheet1!$A$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strLit><c:ptCount val="2"/><c:pt idx="0"><c:v>North</c:v></c:pt><c:pt idx="1"><c:v>South</c:v></c:pt></c:strLit></c:cat>
            <c:val><c:numRef><c:f>Sheet1!$C$2:$C$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.categories, vec!["North", "South"]);
        assert_eq!(cd.categories_ref, None);
        // The box spans A..C. With no `<c:cat>` ref to take it from, `cat_col`
        // is C — the column the series plots, which `claimed_col` then refuses
        // to write out as the labels' own.
        assert_eq!(cd.source.as_ref().map(|s| (s.range, s.cat_col)), {
            Some(((0, 0, 2, 2), 2))
        });
        let out = crate::xlsx::chart_space_xml(&cd);
        assert!(out.contains("<c:cat><c:strLit>"), "kept literal: {out}");
        assert!(
            !out.contains("<c:f>Sheet1!$A$2:$A$3</c:f>"),
            "invented a category ref: {out}"
        );
    }

    #[test]
    fn chart_source_refs_are_absolute_and_skip_the_header() {
        use crate::sheet::ChartSource;
        let src = ChartSource {
            sheet: "Budget".into(),
            range: (0, 0, 4, 3),
            cat_col: 0,
        };
        assert_eq!(src.f_ref(0, 0, true), "Budget!$A$2:$A$5");
        assert_eq!(src.f_ref(2, 2, true), "Budget!$C$2:$C$5");
        assert_eq!(src.header_ref(2), "Budget!$C$1");
        // A sheet name with a space has to be quoted for Excel to accept it.
        let spaced = ChartSource {
            sheet: "My Sheet".into(),
            range: (0, 0, 2, 1),
            cat_col: 0,
        };
        assert_eq!(spaced.f_ref(1, 1, true), "'My Sheet'!$B$2:$B$3");
        // Parsing is the inverse.
        assert_eq!(
            ChartSource::parse_f_ref("Budget!$A$2:$A$5").unwrap().range,
            (1, 0, 4, 0)
        );
    }

    #[test]
    fn sheet_names_needing_quotes_round_trip_through_a_chart_ref() {
        use crate::sheet::{ChartSource, quote_sheet_name};
        // Bare identifiers stay bare; everything else is quoted, and an
        // apostrophe inside the name is doubled. Excel calls the file corrupt
        // otherwise and drops the chart.
        assert_eq!(quote_sheet_name("Sheet1"), "Sheet1");
        assert_eq!(quote_sheet_name("My Sheet"), "'My Sheet'");
        assert_eq!(quote_sheet_name("Q1-Actuals"), "'Q1-Actuals'");
        assert_eq!(quote_sheet_name("Data (raw)"), "'Data (raw)'");
        assert_eq!(quote_sheet_name("2026Budget"), "'2026Budget'");
        assert_eq!(quote_sheet_name("Bob's data"), "'Bob''s data'");
        // A name shaped like a cell reference needs quoting too.
        assert_eq!(quote_sheet_name("A1"), "'A1'");
        // No sheet part at all stays absent.
        assert_eq!(quote_sheet_name(""), "");

        for name in ["Sheet1", "My Sheet", "Q1-Actuals", "Bob's data", "A1"] {
            let src = ChartSource {
                sheet: name.into(),
                range: (0, 1, 3, 1),
                cat_col: 1,
            };
            let back = ChartSource::parse_f_ref(&src.to_ref())
                .unwrap_or_else(|| panic!("{name}: {}", src.to_ref()));
            assert_eq!(back.sheet, name, "{}", src.to_ref());
            assert_eq!(back.range, src.range);
        }
    }

    #[test]
    fn a_hand_typed_series_name_is_written_as_text_not_as_a_ref() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // `name_ref: None` means the name was typed. Deriving the header cell
        // for it would make Excel show whatever B1 says the next time it
        // refreshes, and the typed name would survive only as a stale cache.
        let cd = ChartData {
            series: vec![ChartSeries {
                name: "Revenue".into(),
                values: vec![1.0, 2.0],
                col: Some(1),
                ..Default::default()
            }],
            categories: vec!["Jan".into(), "Feb".into()],
            source: Some(ChartSource {
                sheet: "Budget".into(),
                range: (0, 0, 2, 1),
                cat_col: 0,
            }),
            ..Default::default()
        };
        let out = crate::xlsx::chart_space_xml(&cd);
        assert!(
            out.contains("<c:tx><c:v>Revenue</c:v></c:tx>"),
            "literal name: {out}"
        );
        assert!(
            !out.contains("<c:f>Budget!$B$1</c:f>"),
            "no derived name ref"
        );
        // Reading it back gives the same literal name and no link.
        let again = parse_chart(&out);
        assert_eq!(again.series[0].name, "Revenue");
        assert_eq!(again.series[0].name_ref, None);
    }

    #[test]
    fn a_one_column_source_does_not_name_its_own_numbers_as_the_categories() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // Re-pointing one series of a chart that had no refs at all makes the
        // chart's box that series' single column, so its `cat_col` IS the
        // numbers. A derived <c:cat> would tell Excel to label the bars with the
        // values they plot.
        let one_col = ChartSource {
            sheet: "Budget".into(),
            range: (1, 1, 3, 1),
            cat_col: 1,
        };
        let cd = ChartData {
            series: vec![ChartSeries {
                name: "Qty".into(),
                values: vec![1.0, 2.0, 3.0],
                col: Some(1),
                values_ref: Some(one_col.clone()),
                ..Default::default()
            }],
            categories: vec!["a".into(), "b".into(), "c".into()],
            source: Some(one_col),
            ..Default::default()
        };
        let out = crate::xlsx::chart_space_xml(&cd);
        assert!(
            out.contains("<c:cat><c:strLit"),
            "literal categories: {out}"
        );
        assert!(
            !out.contains("<c:cat><c:strRef"),
            "no fabricated category ref: {out}"
        );
        // The values ref is untouched by the guard.
        assert!(out.contains("<c:f>Budget!$B$2:$B$4</c:f>"), "{out}");
    }

    #[test]
    fn a_column_a_series_plots_is_never_named_as_the_categories() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource};
        // For a chart whose categories are literals, no ref names the label
        // column at all, so `cat_col` falls back to a column some series plots
        // — and a derived <c:cat> would then tell Excel to label each bar with
        // the number it plots.
        let col = |c: u32| ChartSource {
            sheet: "Budget".into(),
            range: (1, c, 3, c),
            cat_col: c,
        };
        let cd = ChartData {
            series: vec![
                ChartSeries {
                    name: "Qty".into(),
                    values: vec![1.0, 2.0, 3.0],
                    values_ref: Some(col(1)),
                    ..Default::default()
                },
                ChartSeries {
                    name: "Price".into(),
                    values: vec![4.0, 5.0, 6.0],
                    values_ref: Some(col(2)),
                    ..Default::default()
                },
            ],
            categories: vec!["a".into(), "b".into(), "c".into()],
            // The union of the two series' refs: two columns wide, and `cat_col`
            // still the first ref's column — which series 1 plots.
            source: Some(ChartSource {
                sheet: "Budget".into(),
                range: (1, 1, 3, 2),
                cat_col: 1,
            }),
            ..Default::default()
        };
        let out = crate::xlsx::chart_space_xml(&cd);
        assert!(
            out.contains("<c:cat><c:strLit") && !out.contains("<c:cat><c:strRef"),
            "categories stay literal: {out}"
        );
        assert_eq!(parse_chart(&out).categories, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_linked_chart_title_is_not_read_as_series_data() {
        // A title (chart or axis) linked to a cell is a <c:tx> too, and axis
        // titles come AFTER the series in the part. Attributing its <c:f> would
        // rename the last series and stretch the chart's box over the title
        // cell — a chart the user never touched, corrupted on the next save.
        let xml = "<c:chartSpace><c:chart>\
<c:title><c:tx><c:strRef><c:f>Budget!$H$20</c:f><c:strCache><c:pt idx=\"0\"><c:v>Spend</c:v></c:pt></c:strCache></c:strRef></c:tx></c:title>\
<c:plotArea><c:barChart>\
<c:ser><c:tx><c:strRef><c:f>Budget!$B$1</c:f><c:strCache><c:pt idx=\"0\"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>\
<c:cat><c:strRef><c:f>Budget!$A$2:$A$3</c:f><c:strCache><c:pt idx=\"0\"><c:v>Laptop</c:v></c:pt></c:strCache></c:strRef></c:cat>\
<c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx=\"0\"><c:v>3</c:v></c:pt></c:numCache></c:numRef></c:val>\
</c:ser></c:barChart>\
<c:valAx><c:title><c:tx><c:strRef><c:f>Budget!$H$21</c:f><c:strCache><c:pt idx=\"0\"><c:v>Units</c:v></c:pt></c:strCache></c:strRef></c:tx></c:title></c:valAx>\
</c:plotArea></c:chart></c:chartSpace>";
        let cd = parse_chart(xml);
        assert_eq!(cd.series.len(), 1);
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("Budget!$B$1"));
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|v| v.range),
            Some((1, 1, 2, 1))
        );
        // The box covers the data only — H20/H21 are nowhere near it.
        assert_eq!(cd.source.as_ref().map(|v| v.range), Some((0, 0, 2, 1)));
    }

    #[test]
    fn a_series_trimmings_are_not_the_cells_it_plots() {
        // Error bars, a trendline and a cell-linked data label all sit INSIDE
        // <c:ser> with a <c:f> of their own. Folded into the box they widen the
        // DATA RANGE the panel shows, and `chart_space_xml` derives a ref-less
        // series' cells from it — so an edited chart would be written back
        // plotting error-bar cells. The label's <c:tx> is worse: last-wins on
        // `name_ref` repointed the series' name at it.
        let xml = "<c:chartSpace><c:chart><c:plotArea><c:barChart>\
<c:ser><c:tx><c:strRef><c:f>Data!$B$1</c:f></c:strRef></c:tx>\
<c:dLbls><c:dLbl><c:tx><c:strRef><c:f>Data!$Y$9</c:f></c:strRef></c:tx></c:dLbl></c:dLbls>\
<c:errBars><c:plus><c:numRef><c:f>Data!$Z$1:$Z$50</c:f></c:numRef></c:plus></c:errBars>\
<c:trendline><c:trendlineLbl><c:tx><c:strRef><c:f>Data!$W$1</c:f></c:strRef></c:tx></c:trendlineLbl></c:trendline>\
<c:cat><c:strRef><c:f>Data!$A$2:$A$3</c:f></c:strRef></c:cat>\
<c:val><c:numRef><c:f>Data!$B$2:$B$3</c:f></c:numRef></c:val>\
</c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>";
        let cd = parse_chart(xml);
        assert_eq!(cd.series.len(), 1);
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("Data!$B$1"));
        // A1:B3 — the header row, the labels and the numbers. Nothing in W, Y
        // or Z, and nothing down at row 50.
        assert_eq!(cd.source.as_ref().map(|v| v.range), Some((0, 0, 2, 1)));
    }

    #[test]
    fn a_stacked_or_combo_plot_area_is_not_ours_to_rewrite() {
        // `kind` records the FIRST plot group and the writer emits one
        // clustered/standard group. So a stacked chart would come back
        // clustered, and a combo chart would fold its line series onto the bar
        // axis. Both are irreversible; both round-trip verbatim instead.
        let ser = "<c:ser><c:val><c:numRef><c:f>Data!$B$2:$B$3</c:f></c:numRef></c:val></c:ser>";
        let plot = |body: &str| {
            format!(
                "<c:chartSpace><c:chart><c:plotArea>{body}</c:plotArea></c:chart></c:chartSpace>"
            )
        };

        let clustered = parse_chart(&plot(&format!(
            "<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/>{ser}</c:barChart>"
        )));
        assert_eq!(clustered.kind, "column");
        assert!(!clustered.complex, "what the writer itself emits");

        let stacked = parse_chart(&plot(&format!(
            "<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"stacked\"/>{ser}</c:barChart>"
        )));
        assert_eq!(stacked.kind, "column");
        assert!(stacked.complex);

        let combo = parse_chart(&plot(&format!(
            "<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/>{ser}</c:barChart>\
<c:lineChart><c:grouping val=\"standard\"/>{ser}</c:lineChart>"
        )));
        assert_eq!(combo.kind, "column", "the first group still names it");
        assert!(combo.complex);
        assert_eq!(combo.series.len(), 2, "both groups' series are still read");

        // A line chart's own grouping is `standard`, and pie has none at all.
        let line = parse_chart(&plot(&format!(
            "<c:lineChart><c:grouping val=\"standard\"/>{ser}</c:lineChart>"
        )));
        assert!(!line.complex);
        let pie = parse_chart(&plot(&format!("<c:pieChart>{ser}</c:pieChart>")));
        assert!(!pie.complex);
    }

    #[test]
    fn an_anchor_whose_attributes_start_on_the_next_line_still_counts() {
        // `parse_drawings` numbers anchors with `XmlParser`, which ends a name
        // at ANY whitespace; the raw rewrite scanners index into that numbering.
        // A scanner stopping only at `>`/` `/`/` skipped this anchor, shifting
        // every later index — a move or a delete then landed on the wrong
        // picture.
        let xml = "<xdr:wsDr xmlns:xdr=\"a\" xmlns:r=\"b\">\
<xdr:twoCellAnchor\neditAs=\"oneCell\">\
<xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
<xdr:to><xdr:col>5</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>10</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>\
<xdr:pic><xdr:nvPicPr><xdr:cNvPr id=\"2\" name=\"Logo\"/></xdr:nvPicPr>\
<xdr:blipFill><a:blip r:embed=\"rId1\"/></xdr:blipFill></xdr:pic>\
</xdr:twoCellAnchor></xdr:wsDr>";
        let resolve = |rid: &str| {
            (rid == "rId1").then(|| ("image/png".to_string(), "xl/media/image1.png".to_string()))
        };
        let ds = parse_drawings(xml, &resolve, &|_: &str| None);
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].anchor_ix, 0);
        // The move lands on THIS anchor, not past it.
        let out = rewrite_anchors(xml, &[(0, (7, 3), (15, 7))], &[]);
        assert!(out.contains("<xdr:row>7</xdr:row>"), "from moved: {out}");
        assert!(out.contains("<xdr:col>7</xdr:col>"), "to moved: {out}");
        assert!(out.contains("editAs=\"oneCell\""), "attributes kept: {out}");
        // And a delete removes it rather than something else.
        assert!(!rewrite_anchors(xml, &[], &[0]).contains("twoCellAnchor"));
    }

    #[test]
    fn axis_titles_belong_to_neither_the_chart_title_nor_a_series() {
        // Excel omits <c:tx> on a series it has no name for, and writes each
        // axis its own <c:title>. Both used to land on the chart: the axis
        // title's cached <c:v> named the last series, and its rich text was
        // appended to the chart's own title ("SalesQuarterUnits").
        let xml = "<c:chartSpace><c:chart>\
<c:title><c:tx><c:rich><a:p><a:r><a:t>Sales</a:t></a:r></a:p></c:rich></c:tx></c:title>\
<c:plotArea><c:barChart>\
<c:ser>\
<c:cat><c:strRef><c:strCache><c:pt idx=\"0\"><c:v>Laptop</c:v></c:pt></c:strCache></c:strRef></c:cat>\
<c:val><c:numRef><c:numCache><c:pt idx=\"0\"><c:v>3</c:v></c:pt></c:numCache></c:numRef></c:val>\
</c:ser></c:barChart>\
<c:catAx><c:title><c:tx><c:rich><a:p><a:r><a:t>Quarter</a:t></a:r></a:p></c:rich></c:tx></c:title></c:catAx>\
<c:valAx><c:title><c:tx><c:strRef><c:strCache><c:pt idx=\"0\"><c:v>Units</c:v></c:pt></c:strCache></c:strRef></c:tx></c:title></c:valAx>\
</c:plotArea></c:chart></c:chartSpace>";
        let cd = parse_chart(xml);
        assert_eq!(cd.title, "Sales");
        assert_eq!(cd.series.len(), 1);
        assert_eq!(cd.series[0].name, "");
        assert_eq!(cd.series[0].values, vec![3.0]);
        assert_eq!(cd.categories, vec!["Laptop".to_string()]);
    }

    #[test]
    fn parses_chart_graphic_frame() {
        let drawing = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>
              <xdr:to><xdr:col>8</xdr:col><xdr:row>15</xdr:row></xdr:to>
              <xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id="rId2"/></a:graphicData></a:graphic></xdr:graphicFrame>
            </xdr:twoCellAnchor></xdr:wsDr>"#;
        let chart = r#"<c:chartSpace><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales</a:t></a:r></a:p></c:rich></c:tx></c:title>
            <c:plotArea><c:barChart>
              <c:ser><c:tx><c:strRef><c:strCache><c:pt><c:v>Q1</c:v></c:pt></c:strCache></c:strRef></c:tx>
                <c:cat><c:strRef><c:strCache><c:pt><c:v>North</c:v></c:pt><c:pt><c:v>South</c:v></c:pt></c:strCache></c:strRef></c:cat>
                <c:val><c:numRef><c:numCache><c:pt><c:v>10</c:v></c:pt><c:pt><c:v>20</c:v></c:pt></c:numCache></c:numRef></c:val>
              </c:ser>
            </c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let resolve = |rid: &str| {
            (rid == "rId2").then(|| ("chart".to_string(), "xl/charts/chart1.xml".to_string()))
        };
        let get = |part: &str| (part == "xl/charts/chart1.xml").then(|| chart.to_string());
        let ds = parse_drawings(drawing, &resolve, &get);
        assert_eq!(ds.len(), 1);
        match &ds[0].kind {
            DrawingKind::Chart(c) => {
                // No <c:barDir> — OOXML's default orientation is col (vertical).
                assert_eq!(c.kind, "column");
                assert_eq!(c.title, "Sales");
                assert_eq!(c.categories, vec!["North", "South"]);
                assert_eq!(c.series.len(), 1);
                assert_eq!(c.series[0].name, "Q1");
                assert_eq!(c.series[0].values, vec![10.0, 20.0]);
            }
            _ => panic!("expected chart"),
        }
    }

    #[test]
    fn bar_dir_decides_column_vs_bar() {
        // A `barChart` is BOTH orientations; <c:barDir> decides. Getting this wrong
        // made every loaded column chart render as horizontal bars.
        let drawing = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
            <xdr:twoCellAnchor>
              <xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>
              <xdr:to><xdr:col>8</xdr:col><xdr:row>15</xdr:row></xdr:to>
              <xdr:graphicFrame><a:graphic><a:graphicData><c:chart r:id="rId2"/></a:graphicData></a:graphic></xdr:graphicFrame>
            </xdr:twoCellAnchor></xdr:wsDr>"#;
        let chart_with = |dir: &str| {
            format!(
                r#"<c:chartSpace><c:chart><c:plotArea><c:barChart><c:barDir val="{dir}"/>
                <c:ser><c:val><c:numRef><c:numCache><c:pt><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
                </c:barChart></c:plotArea></c:chart></c:chartSpace>"#
            )
        };
        for (dir, want) in [("col", "column"), ("bar", "bar")] {
            let chart = chart_with(dir);
            let resolve = |rid: &str| {
                (rid == "rId2").then(|| ("chart".to_string(), "xl/charts/chart1.xml".to_string()))
            };
            let get = |part: &str| (part == "xl/charts/chart1.xml").then(|| chart.clone());
            let ds = parse_drawings(drawing, &resolve, &get);
            match &ds[0].kind {
                DrawingKind::Chart(c) => assert_eq!(c.kind, want, "barDir={dir}"),
                _ => panic!("expected chart"),
            }
        }
        // Other chart types keep their own element-derived kind.
        let pie = r#"<c:chartSpace><c:chart><c:plotArea><c:pieChart>
            <c:ser><c:val><c:numRef><c:numCache><c:pt><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
            </c:pieChart></c:plotArea></c:chart></c:chartSpace>"#;
        let resolve = |rid: &str| {
            (rid == "rId2").then(|| ("chart".to_string(), "xl/charts/chart1.xml".to_string()))
        };
        let get = |part: &str| (part == "xl/charts/chart1.xml").then(|| pie.to_string());
        match &parse_drawings(drawing, &resolve, &get)[0].kind {
            DrawingKind::Chart(c) => assert_eq!(c.kind, "pie"),
            _ => panic!("expected chart"),
        }
    }

    /// The Overview's table read the OTHER way round: one series per PRODUCT
    /// row, the header row as categories. Nothing in the file says so — the
    /// only evidence is that each `<c:val>` spans one row and three columns —
    /// so the loader has to work it out, or the panel comes back showing the
    /// wrong button state for a chart Excel is drawing correctly.
    #[test]
    fn a_row_oriented_chart_is_recognised_by_the_shape_of_its_refs() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Budget!$A$2</c:f><c:strCache><c:pt idx="0"><c:v>Laptop</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Budget!$B$1:$D$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt><c:pt idx="1"><c:v>Unit price</c:v></c:pt><c:pt idx="2"><c:v>Total</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$2:$D$2</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>1199</c:v></c:pt><c:pt idx="2"><c:v>2398</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser>
          <c:ser><c:idx val="1"/>
            <c:tx><c:strRef><c:f>Budget!$A$3</c:f><c:strCache><c:pt idx="0"><c:v>Monitor</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Budget!$B$1:$D$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt><c:pt idx="1"><c:v>Unit price</c:v></c:pt><c:pt idx="2"><c:v>Total</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$3:$D$3</c:f><c:numCache><c:pt idx="0"><c:v>4</c:v></c:pt><c:pt idx="1"><c:v>249.5</c:v></c:pt><c:pt idx="2"><c:v>998</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert!(cd.by_row, "one row across three columns is a row series");
        let names: Vec<&str> = cd.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Laptop", "Monitor"]);
        assert_eq!(cd.series[0].values, vec![2.0, 1199.0, 2398.0]);
        assert_eq!(cd.series[1].values, vec![4.0, 249.5, 998.0]);
        assert_eq!(cd.categories, vec!["Qty", "Unit price", "Total"]);
        // Each series kept its own row rectangle, and the categories their row.
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|v| v.range),
            Some((1, 1, 1, 3))
        );
        assert_eq!(
            cd.series[1].values_ref.as_ref().map(|v| v.range),
            Some((2, 1, 2, 3))
        );
        assert_eq!(
            cd.categories_ref.as_ref().map(|v| v.range),
            Some((0, 1, 0, 3))
        );
        // The box is the whole table, and `cat_col` names the column the SERIES
        // NAMES come from — column A, taken outright from the first series'
        // name cell (`Budget!$A$2`), which is where `chart_from_rows` puts
        // them. A row chart's labels run along a ROW, so `<c:cat>` cannot
        // supply a column index at all.
        assert_eq!(
            cd.source.as_ref().map(|s| (s.range, s.cat_col)),
            Some(((0, 0, 2, 3), 0))
        );
    }

    /// The regression that matters: every chart written before orientation
    /// existed is column-oriented, and must still parse as one.
    #[test]
    fn a_column_oriented_chart_is_still_column_oriented() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Budget!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Budget!$A$2:$A$3</c:f><c:strCache><c:pt idx="0"><c:v>Laptop</c:v></c:pt><c:pt idx="1"><c:v>Dock</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Budget!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert!(!cd.by_row, "one column down two rows is a column series");
        // And the `cat_col` fixup still runs for it: the labels are in A.
        assert_eq!(cd.source.as_ref().map(|s| s.cat_col), Some(0));
    }

    /// Excel groups category labels by writing `<c:multiLvlStrRef>` with one
    /// `<c:lvl>` per level and an `<c:f>` naming every level's cells — usually
    /// a rectangle, which is the shape this covers; the line case is the test
    /// below. This model holds a single line of labels, so that ref is one it
    /// cannot hold: taking it
    /// would let the writer regenerate a one-level `<c:strRef>` naming every
    /// level's cells beside a one-level cache — the ref-vs-cache contradiction
    /// the panel's own `categories_shape_err` refuses. It must instead mark the
    /// chart complex, so the part is kept exactly as Excel wrote it, and the
    /// rectangle must stay out of both `categories_ref` and the chart's box.
    #[test]
    fn a_multi_level_category_ref_is_one_this_model_cannot_hold() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:cat><c:multiLvlStrRef><c:f>Sales!$A$2:$B$4</c:f><c:multiLvlStrCache><c:ptCount val="3"/>
              <c:lvl><c:pt idx="0"><c:v>Jan</c:v></c:pt><c:pt idx="1"><c:v>Feb</c:v></c:pt><c:pt idx="2"><c:v>Mar</c:v></c:pt></c:lvl>
              <c:lvl><c:pt idx="0"><c:v>Q1</c:v></c:pt></c:lvl>
            </c:multiLvlStrCache></c:multiLvlStrRef></c:cat>
            <c:val><c:numRef><c:f>Sales!$C$2:$C$4</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt><c:pt idx="2"><c:v>9</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert!(cd.complex, "a multi-level category keeps the part verbatim");
        assert!(
            cd.categories_ref.is_none(),
            "the rectangle must not become the label ref"
        );
        // Nor may it widen the box: only the values ref did.
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((1, 2, 3, 2)));
        // The innermost level is the one the card shows; the outer level does
        // NOT overwrite it by restarting `idx` at 0.
        assert_eq!(cd.categories, vec!["Jan", "Feb", "Mar"]);
    }

    /// Two levels over ONE category name a line (`Sales!$A$2:$B$2`), not a
    /// rectangle, so the shape of the `<c:f>` cannot be what decides: this is
    /// indistinguishable from an ordinary row of labels by range alone. The
    /// `<c:multiLvlStrRef>` element is what says so.
    #[test]
    fn a_multi_level_category_over_one_category_is_refused_too() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:cat><c:multiLvlStrRef><c:f>Sales!$A$2:$B$2</c:f><c:multiLvlStrCache><c:ptCount val="1"/>
              <c:lvl><c:pt idx="0"><c:v>Jan</c:v></c:pt></c:lvl>
              <c:lvl><c:pt idx="0"><c:v>Q1</c:v></c:pt></c:lvl>
            </c:multiLvlStrCache></c:multiLvlStrRef></c:cat>
            <c:val><c:numRef><c:f>Sales!$C$2:$C$2</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert!(cd.complex, "the part is kept exactly as Excel wrote it");
        assert!(
            cd.categories_ref.is_none(),
            "and the line is not the label ref"
        );
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((1, 2, 1, 2)));
        // An ordinary one-level `<c:strRef>` over the same line is held, so it
        // really is the element and not the range doing the work.
        let plain = xml
            .replace("multiLvlStrRef", "strRef")
            .replace("multiLvlStrCache", "strCache")
            .replace("<c:lvl>", "")
            .replace("</c:lvl>", "");
        let cd = parse_chart(&plain);
        assert!(!cd.complex);
        assert_eq!(
            cd.categories_ref.as_ref().map(|s| s.range),
            Some((1, 0, 1, 1))
        );
    }

    /// A series' NAME and the CATEGORIES may each sit on another sheet than the
    /// numbers — Excel writes such a `<c:tx>` or `<c:cat>` happily, and docxy's
    /// own panel commits both (`target_takes_foreign_sheet`). Folded in document
    /// order they are the first two refs the chart offers, so either would seed
    /// the box and every local `<c:val>` after it would be skipped for the sheet
    /// mismatch: a chart plotting `Sheet1!A1:C5` would come back naming one cell
    /// of `Budget`. Both are folded after the numbers for exactly this reason.
    #[test]
    fn a_cross_sheet_label_ref_does_not_take_the_box_from_the_cells_plotted() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Budget!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>Qty</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:cat><c:strRef><c:f>Sheet1!$A$2:$A$3</c:f><c:strCache><c:ptCount val="2"/><c:pt idx="0"><c:v>North</c:v></c:pt><c:pt idx="1"><c:v>South</c:v></c:pt></c:strCache></c:strRef></c:cat>
            <c:val><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        // The plotted cells decide the sheet, and the foreign name is skipped
        // rather than collapsing the box onto `Budget`.
        assert_eq!(
            cd.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Sheet1", (1, 0, 2, 1)))
        );
        // The ref itself is kept — it is a legal cross-sheet name, and only its
        // claim on the BOX was refused.
        assert_eq!(
            cd.series[0].name_ref.as_deref(),
            Some("Budget!$B$1"),
            "the name ref survives"
        );

        // A LOCAL name, by contrast, still stretches the box up over the header
        // row — the reason name cells are folded in at all.
        let local = xml.replace("Budget!$B$1", "Sheet1!$B$1");
        assert_eq!(
            parse_chart(&local).source.as_ref().map(|s| s.range),
            Some((0, 0, 2, 1))
        );

        // The mirror case, one slot over: the writer emits `{tx}{cat}{val}`, so
        // with `<c:tx>` held back a foreign `<c:cat>` would be the first ref
        // folded and would seize the box the same way. The categories are held
        // back too, so the numbers still decide — the box keeps `Sheet1`, and
        // only column A, which now lives elsewhere, is missing from it.
        let cat = local.replace("Sheet1!$A$2:$A$3", "Budget!$A$2:$A$3");
        let cd = parse_chart(&cat);
        assert_eq!(
            cd.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Sheet1", (0, 1, 2, 1))),
            "the foreign categories ref is skipped, not made the box"
        );
        assert_eq!(
            cd.categories_ref
                .as_ref()
                .map(|s| (s.sheet.as_str(), s.range)),
            Some(("Budget", (1, 0, 2, 0))),
            "the categories ref survives"
        );
    }

    /// A scatter plots from `<c:xVal>`/`<c:yVal>` rather than `<c:val>`, so
    /// `values_ref` is empty for it however live the series is. Those refs are
    /// still the chart's NUMBERS: they fold into the box here, and they are
    /// kept on the series as `point_refs` so the panel's `rebuild_source` can
    /// see the same cells. Blind to them it would rebuild a scatter's box out
    /// of its lone `<c:tx>` cell and collapse the DATA RANGE it shows.
    #[test]
    fn a_scatters_point_refs_are_kept_on_its_series() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="lineMarker"/>
          <c:ser><c:idx val="0"/>
            <c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:pt idx="0"><c:v>S</c:v></c:pt></c:strCache></c:strRef></c:tx>
            <c:xVal><c:numRef><c:f>Sheet1!$A$2:$A$3</c:f><c:numCache><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:xVal>
            <c:yVal><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>4</c:v></c:pt><c:pt idx="1"><c:v>9</c:v></c:pt></c:numCache></c:numRef></c:yVal>
          </c:ser></c:scatterChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(xml);
        assert_eq!(cd.kind, "scatter");
        // No `<c:val>` anywhere, so the values slot stays empty — that is the
        // blindness `point_refs` exists to close, not a parse failure.
        assert!(
            cd.series[0].values_ref.is_none(),
            "a scatter has no <c:val>"
        );
        assert_eq!(
            cd.series[0]
                .point_refs
                .iter()
                .map(|s| (s.sheet.as_str(), s.range))
                .collect::<Vec<_>>(),
            vec![("Sheet1", (1, 0, 2, 0)), ("Sheet1", (1, 1, 2, 1))],
            "both point arrays, in document order"
        );
        // And they are still the box, header cell included.
        assert_eq!(
            cd.source.as_ref().map(|s| (s.sheet.as_str(), s.range)),
            Some(("Sheet1", (0, 0, 2, 1)))
        );

        // A chart that plots from `<c:val>` carries none — every kind the
        // writer authors takes that path, so the new slot stays empty there.
        let bar = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/>
            <c:val><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt><c:pt idx="1"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val>
          </c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#;
        assert!(parse_chart(bar).series[0].point_refs.is_empty());
        assert!(!parse_chart(bar).series[0].points_unheld);
    }

    /// The points this model cannot turn into a ref — literal `<c:numLit>` ones,
    /// or an `<c:f>` `parse_f_ref` refuses. They leave `point_refs` EMPTY, which
    /// from the panel is indistinguishable from the empty series "+ Series"
    /// pushes: no refs, no `col`, no cached numbers either way. So the loader
    /// says outright that the series held points, and `chart_would_lose_points`
    /// reads it — without which picking a writable type relabels such a chart
    /// and the next save writes `<c:ptCount val="0"/>` over a plot that can
    /// never be read back.
    #[test]
    fn a_scatter_whose_points_cannot_be_held_says_so_on_the_series() {
        // Literal points: no `<c:f>` at all, so there is no ref to fail on.
        let lit = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="lineMarker"/>
          <c:ser><c:idx val="0"/><c:tx><c:v>Speed</c:v></c:tx>
            <c:xVal><c:numLit><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numLit></c:xVal>
            <c:yVal><c:numLit><c:pt idx="0"><c:v>4</c:v></c:pt><c:pt idx="1"><c:v>9</c:v></c:pt></c:numLit></c:yVal>
          </c:ser></c:scatterChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(lit);
        assert!(cd.series[0].point_refs.is_empty());
        assert!(cd.series[0].points_unheld);
        // Literal points name NO cells, so nothing here says the chart's box is
        // short of its plot — only the wider mark is set.
        assert!(!cd.series[0].points_ref_unheld);
        // Nothing folded a box either, so the panel's re-author door has the
        // clean "no data range" answer to give rather than a silent empty plot.
        assert!(cd.source.is_none());

        // A whole-column ref: `parse_f_ref` refuses it, and under `<c:xVal>`
        // there is no `mode` for the `unparsed_ref` arm to catch it by.
        let col = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="lineMarker"/>
          <c:ser><c:idx val="0"/>
            <c:xVal><c:numRef><c:f>Sheet1!$A:$A</c:f></c:numRef></c:xVal>
            <c:yVal><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f></c:numRef></c:yVal>
          </c:ser></c:scatterChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(col);
        // The half it COULD hold is still held — the mark is per element, and
        // one unheld half is enough to make the series a loss.
        assert_eq!(cd.series[0].point_refs.len(), 1);
        assert!(cd.series[0].points_unheld);
        // And the NARROWER mark with it: this element named cells, so the box
        // the fold came out with is short of the plot. That is what the panel's
        // re-author door refuses on, where the literal above goes through.
        assert!(cd.series[0].points_ref_unheld);

        // An EMPTY point element is not the same thing. A series added with no
        // data is schema-legal (`<c:ptCount val="0"/>`, or nothing at all) and
        // holds no plot to destroy, so it must arrive unmarked — marked, it
        // would send a chart that draws nothing down the re-author door and be
        // refused there for a box it never needed.
        let none = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="lineMarker"/>
          <c:ser><c:idx val="0"/><c:tx><c:v>Speed</c:v></c:tx>
            <c:xVal><c:numLit><c:ptCount val="0"/></c:numLit></c:xVal>
            <c:yVal/>
          </c:ser></c:scatterChart></c:plotArea></c:chart></c:chartSpace>"#;
        let cd = parse_chart(none);
        assert!(cd.series[0].point_refs.is_empty());
        assert!(!cd.series[0].points_unheld);
        assert!(!cd.series[0].points_ref_unheld);

        // Nor is an EMPTY `<c:f>`, which is the same empty element written the
        // other legal way round: a `<c:numRef>` whose formula names no cells
        // holds no plot either, and the open tag alone must not stand in for
        // one. Marked, this chart would be refused a writable type for a box it
        // never needed.
        let empty_f = none
            .replace(
                r#"<c:numLit><c:ptCount val="0"/></c:numLit>"#,
                "<c:numRef><c:f/></c:numRef>",
            )
            .replace(
                "<c:yVal/>",
                "<c:yVal><c:numRef><c:f>  </c:f></c:numRef></c:yVal>",
            );
        let cd = parse_chart(&empty_f);
        assert!(cd.series[0].point_refs.is_empty());
        assert!(!cd.series[0].points_unheld);
        // An empty `<c:f>` names no cells either, so it must not claim the box
        // misses any: marked, every such chart would be refused a writable type
        // for a plot it hasn't got.
        assert!(!cd.series[0].points_ref_unheld);
        assert!(cd.source.is_none());

        // One that names cells it cannot hold still marks, text or no cache:
        // that is the plot the mark exists for.
        let unheld = empty_f.replace("<c:f/>", "<c:f>Sheet1!$A:$A</c:f>");
        assert!(parse_chart(&unheld).series[0].points_unheld);
        assert!(parse_chart(&unheld).series[0].points_ref_unheld);
        // A whole-column `<c:f>` under a POINT element is not `complex`: the
        // series is already held back from the writer by its points, and marking
        // the chart complex would shut the re-author door that is the way out.
        assert!(!parse_chart(&unheld).complex);
    }

    /// A scatter has no `<c:val>` for `infer_by_row` to measure, so without its
    /// point refs every one of them reads "column" whichever way it was laid
    /// out — and `chart_set_kind` now re-derives on that answer, turning a
    /// row-laid scatter into N one-point series.
    #[test]
    fn a_row_laid_scatter_is_inferred_by_row_from_its_point_refs() {
        let rows = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="lineMarker"/>
          <c:ser><c:idx val="0"/>
            <c:xVal><c:numRef><c:f>Sheet1!$B$2:$F$2</c:f></c:numRef></c:xVal>
            <c:yVal><c:numRef><c:f>Sheet1!$B$3:$F$3</c:f></c:numRef></c:yVal>
          </c:ser></c:scatterChart></c:plotArea></c:chart></c:chartSpace>"#;
        assert!(parse_chart(rows).by_row);

        // The column layout is the same evidence the other way up.
        let cols = rows
            .replace("$B$2:$F$2", "$A$2:$A$6")
            .replace("$B$3:$F$3", "$B$2:$B$6");
        assert!(!parse_chart(&cols).by_row);

        // A ONE-point scatter is two single cells, and one legal way to lay a
        // single coordinate out is stacked down a column (X above Y). That says
        // nothing about orientation, and must not be read as the stacked-cell
        // evidence the fallback looks for — which is exactly what it would be
        // without the multi-cell guard on the `point_refs` tally above: two
        // single cells in one column with different rows satisfies every clause
        // of it, and this chart would come back `by_row`, to be re-derived the
        // wrong way round on a type click.
        let one = rows
            .replace("$B$2:$F$2", "$A$2")
            .replace("$B$3:$F$3", "$A$3");
        assert!(!parse_chart(&one).by_row);

        // Side by side is the other layout, and the fallback answers it `false`
        // on its own account (different columns are not stacked) rather than the
        // guard doing so. Kept because both layouts must come out the same way.
        let side = rows
            .replace("$B$2:$F$2", "$A$2")
            .replace("$B$3:$F$3", "$B$2");
        assert!(!parse_chart(&side).by_row);
    }

    /// Two single-cell series stacked down one column are read as a row chart
    /// (`infer_by_row`), because the column derivation emits one series per
    /// column and so can never produce two sharing one. Cells on DIFFERENT
    /// sheets are not stacked, however their coordinates line up.
    #[test]
    fn single_cells_on_different_sheets_are_not_stacked() {
        let chart = |a: &str, b: &str| {
            format!(
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>
          <c:ser><c:idx val="0"/><c:val><c:numRef><c:f>{a}</c:f><c:numCache><c:pt idx="0"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
          <c:ser><c:idx val="1"/><c:val><c:numRef><c:f>{b}</c:f><c:numCache><c:pt idx="0"><c:v>5</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>
          </c:barChart></c:plotArea></c:chart></c:chartSpace>"#
            )
        };
        // One sheet, one column, different rows: stacked, so a row chart.
        assert!(parse_chart(&chart("Data!$B$2", "Data!$B$3")).by_row);
        // Same coordinates, two sheets: no stack, so the safe column reading.
        assert!(!parse_chart(&chart("Data!$B$2", "Budget!$B$3")).by_row);
        // Case is not a sheet difference — Excel resolves names that way.
        assert!(parse_chart(&chart("Data!$B$2", "data!$B$3")).by_row);
    }

    /// Every shape that fits both readings, or neither, answers "column" — the
    /// orientation every pre-existing chart has, and the only one the rest of
    /// the model (`ChartSeries::col`, `ChartSource::cat_col`) can describe.
    #[test]
    fn ambiguous_series_shapes_default_to_column_orientation() {
        let chart = |sers: &str| {
            format!(
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/>{sers}</c:barChart></c:plotArea></c:chart></c:chartSpace>"#
            )
        };
        let val = |f: &str| {
            format!(
                r#"<c:ser><c:idx val="0"/><c:val><c:numRef><c:f>{f}</c:f><c:numCache><c:ptCount val="1"/><c:pt idx="0"><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>"#
            )
        };

        // A single cell is one row AND one column: it fits both readings.
        assert!(!parse_chart(&chart(&val("Budget!$B$2"))).by_row);

        // A rectangle is a shape neither reading produces.
        assert!(!parse_chart(&chart(&val("Budget!$B$2:$D$5"))).by_row);

        // Literal values: no ref at all, so nothing to measure.
        let lit = r#"<c:ser><c:idx val="0"/><c:val><c:numLit><c:ptCount val="2"/><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numLit></c:val></c:ser>"#;
        let cd = parse_chart(&chart(lit));
        assert_eq!(cd.series.len(), 1);
        assert!(!cd.by_row);

        // A ref this model cannot hold — a whole column — is no shape either.
        let whole = parse_chart(&chart(&val("Budget!$B:$B")));
        assert!(!whole.by_row);
        assert!(whole.complex, "an unparsed ref keeps the part verbatim");

        // Series that DISAGREE: one row-shaped, one column-shaped. A row vote
        // has to be unanimous, so this reads as a column chart.
        let mixed = parse_chart(&chart(&format!(
            "{}{}",
            val("Budget!$B$2:$D$2"),
            val("Budget!$B$3:$B$5")
        )));
        assert_eq!(mixed.series.len(), 2);
        assert!(!mixed.by_row, "a mixed chart falls back to column");

        // No series at all — nothing votes, and the answer is still column.
        assert!(!parse_chart(&chart("")).by_row);
    }

    /// The Overview's table, one row per item — the sheet the round-trips below
    /// chart both ways round.
    fn overview_sheet() -> crate::sheet::Sheet {
        use crate::sheet::{Cell, Sheet, parse_cell_name};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Unit price")),
            ("D1", Cell::text("Total")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(1199.0)),
            ("D2", Cell::number(2398.0)),
            ("A3", Cell::text("Monitor")),
            ("B3", Cell::number(4.0)),
            ("C3", Cell::number(249.5)),
            ("D3", Cell::number(998.0)),
            ("A4", Cell::text("Keyboard")),
            ("B4", Cell::number(6.0)),
            ("C4", Cell::number(39.99)),
            ("D4", Cell::number(239.94)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        sh
    }

    /// Orientation is the one thing about a chart that no element of
    /// SpreadsheetML records: it lives only in the SHAPE of the refs the chart
    /// writes. So the load→save→load path is where it can actually be lost, and
    /// build → `chart_space_xml` → `parse_chart` is the whole of the evidence.
    #[test]
    fn a_row_oriented_chart_survives_a_write_and_reload() {
        let sh = overview_sheet();
        let cd = crate::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", true)
            .expect("chart");
        let again = parse_chart(&crate::xlsx::chart_space_xml(&cd));

        assert!(again.by_row, "row orientation survived the file");
        let names: Vec<&str> = again.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Laptop", "Monitor", "Keyboard"]);
        assert_eq!(again.categories, vec!["Qty", "Unit price", "Total"]);
        assert_eq!(again.series[0].values, vec![2.0, 1199.0, 2398.0]);
        assert_eq!(again.series[1].values, vec![4.0, 249.5, 998.0]);
        assert_eq!(again.series[2].values, vec![6.0, 39.99, 239.94]);

        // Every ref came back the rectangle it went out as — which is exactly
        // why the orientation came back too.
        for (i, s) in again.series.iter().enumerate() {
            assert_eq!(
                s.values_ref.as_ref().map(|v| v.range),
                cd.series[i].values_ref.as_ref().map(|v| v.range),
                "series {i} values ref"
            );
            assert_eq!(s.name_ref, cd.series[i].name_ref, "series {i} name ref");
            assert!(s.col.is_none(), "series {i} names no column");
        }
        assert_eq!(
            again.categories_ref.as_ref().map(|v| v.range),
            Some((0, 1, 0, 3)),
            "the label ROW"
        );
        // The box is the whole table, and `cat_col` names the column the SERIES
        // NAMES come from — taken outright from the first series' name cell,
        // not from `<c:cat>`, which for a row chart is a ROW of labels and so
        // names no column. This is the row branch's only round-trip guard.
        assert_eq!(
            again.source.as_ref().map(|s| (s.range, s.cat_col)),
            Some(((0, 0, 3, 3), 0))
        );
        // And it is stable: a second trip through the file changes nothing, so
        // opening and saving repeatedly cannot drift the chart column-wards.
        let third = parse_chart(&crate::xlsx::chart_space_xml(&again));
        assert!(third.by_row);
        assert_eq!(third.categories, again.categories);
        assert_eq!(
            third
                .series
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            names
        );
    }

    /// The regression guard: every chart in every file written before
    /// orientation existed is column-oriented, and inference must not flip one.
    #[test]
    fn a_column_oriented_chart_survives_a_write_and_reload() {
        let sh = overview_sheet();
        let cd = crate::sheet::chart_from_range(&sh, "Budget", (0, 0, 3, 3), "column", false)
            .expect("chart");
        let again = parse_chart(&crate::xlsx::chart_space_xml(&cd));

        assert!(!again.by_row, "still column-oriented");
        let names: Vec<&str> = again.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Qty", "Unit price", "Total"]);
        assert_eq!(again.categories, vec!["Laptop", "Monitor", "Keyboard"]);
        assert_eq!(again.series[0].values, vec![2.0, 4.0, 6.0]);
        assert_eq!(again.series[1].values, vec![1199.0, 249.5, 39.99]);
        assert_eq!(again.series[2].values, vec![2398.0, 998.0, 239.94]);
        for (i, s) in again.series.iter().enumerate() {
            assert_eq!(
                s.values_ref.as_ref().map(|v| v.range),
                cd.series[i].values_ref.as_ref().map(|v| v.range),
                "series {i} values ref"
            );
            assert_eq!(s.name_ref, cd.series[i].name_ref, "series {i} name ref");
        }
        assert_eq!(
            again.categories_ref.as_ref().map(|v| v.range),
            Some((1, 0, 3, 0)),
            "the label COLUMN"
        );
        // A COLUMN chart takes `cat_col` from `<c:cat>` instead, which here is
        // the label column.
        assert_eq!(
            again.source.as_ref().map(|s| (s.range, s.cat_col)),
            Some(((0, 0, 3, 3), 0))
        );
    }

    /// The awkward shape: a 2x2 range, where a series is a SINGLE CELL either
    /// way round. Both readings plot the same one bar, and the file holds no
    /// evidence of which was meant — so what must survive is the data, and the
    /// orientation falls to the documented default.
    #[test]
    fn a_two_by_two_range_keeps_its_data_but_not_its_orientation() {
        use crate::sheet::{Cell, Sheet, chart_from_range, parse_cell_name};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }

        // By column: the series is Qty, the category Laptop. It round-trips
        // whole, orientation included, because column IS the default.
        let by_col = chart_from_range(&sh, "Budget", (0, 0, 1, 1), "column", false).expect("cols");
        assert_eq!(by_col.series[0].name, "Qty");
        assert_eq!(by_col.categories, vec!["Laptop"]);
        let again = parse_chart(&crate::xlsx::chart_space_xml(&by_col));
        assert!(!again.by_row);
        assert_eq!(again.series.len(), 1);
        assert_eq!(again.series[0].name, "Qty");
        assert_eq!(again.series[0].values, vec![2.0]);
        assert_eq!(again.categories, vec!["Laptop"]);

        // By row: the series is Laptop, the category Qty. Everything the chart
        // DRAWS survives — the name, the number, the label...
        let by_row = chart_from_range(&sh, "Budget", (0, 0, 1, 1), "column", true).expect("rows");
        assert!(by_row.by_row);
        assert_eq!(by_row.series[0].name, "Laptop");
        assert_eq!(by_row.categories, vec!["Qty"]);
        assert_eq!(
            by_row.series[0].values_ref.as_ref().map(|v| v.to_ref()),
            Some("Budget!$B$2:$B$2".to_string())
        );
        let again = parse_chart(&crate::xlsx::chart_space_xml(&by_row));
        assert_eq!(again.series.len(), 1);
        assert_eq!(again.series[0].name, "Laptop");
        assert_eq!(again.series[0].values, vec![2.0]);
        assert_eq!(again.categories, vec!["Qty"]);
        assert_eq!(again.source.as_ref().map(|s| s.range), Some((0, 0, 1, 1)));

        // ...but the button state does not, and cannot: `$B$2:$B$2` is one row
        // and one column at once, so `infer_by_row` has nothing to read and
        // answers with the default. Excel is in the same position. The picture
        // on screen is unaffected; only re-deriving from the box would now
        // choose the column reading, which draws the same single bar.
        assert!(!again.by_row, "a one-cell series is evidence of neither");
    }

    /// The same one-cell series, but SEVERAL of them: a row chart over a range
    /// one label column plus one numeric column wide. Cell by cell each series
    /// is ambiguous; stacked down one column they are not, because the column
    /// reading emits one series per column and so never repeats a column.
    /// Losing this would not change what THIS load builds — three series either
    /// way round — but the chart would come back column-oriented, and the next
    /// re-derivation from its box would fold the three one-point series into one
    /// three-point series.
    #[test]
    fn stacked_one_cell_series_are_read_as_rows_not_as_one_column() {
        use crate::sheet::{Cell, Sheet, chart_from_range, parse_cell_name};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("A3", Cell::text("Monitor")),
            ("B3", Cell::number(4.0)),
            ("A4", Cell::text("Keyboard")),
            ("B4", Cell::number(6.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }

        let by_row = chart_from_range(&sh, "Budget", (0, 0, 3, 1), "column", true).expect("rows");
        assert_eq!(by_row.series.len(), 3);
        // Every series really is one cell wide — this is the shape at issue.
        let refs: Vec<String> = by_row
            .series
            .iter()
            .map(|s| s.values_ref.as_ref().expect("ref").to_ref())
            .collect();
        assert_eq!(
            refs,
            vec!["Budget!$B$2:$B$2", "Budget!$B$3:$B$3", "Budget!$B$4:$B$4"]
        );

        let again = parse_chart(&crate::xlsx::chart_space_xml(&by_row));
        assert!(
            again.by_row,
            "three cells down one column read as three rows"
        );
        let names: Vec<&str> = again.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Laptop", "Monitor", "Keyboard"]);
        assert_eq!(again.categories, vec!["Qty"]);
        assert_eq!(again.series[1].values, vec![4.0]);

        // The transpose is the column chart over the same box, and it must not
        // be dragged along: its ONE series spans several rows, which is a shape
        // of its own and votes column outright.
        let by_col = chart_from_range(&sh, "Budget", (0, 0, 3, 1), "column", false).expect("cols");
        let again = parse_chart(&crate::xlsx::chart_space_xml(&by_col));
        assert!(!again.by_row);
        assert_eq!(again.series.len(), 1);
        assert_eq!(again.series[0].values, vec![2.0, 4.0, 6.0]);
    }

    /// The stacked-cell rule must not fire on a COLUMN chart whose series were
    /// re-pointed at single cells by hand. `series_values_shape_err` refuses
    /// only a ref spanning several columns, so a one-cell `<c:val>` commits, and
    /// two of them one above the other are the same cell-by-cell shape the row
    /// case produces. The categories tell the two apart: a column chart's labels
    /// run DOWN a column, and that is a shape the row reading never writes.
    #[test]
    fn a_column_chart_re_pointed_at_stacked_cells_stays_a_column_chart() {
        use crate::sheet::{Cell, Sheet, chart_from_range, parse_cell_name};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Unit price")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(900.0)),
            ("A3", Cell::text("Monitor")),
            ("B3", Cell::number(4.0)),
            ("C3", Cell::number(150.0)),
            ("A4", Cell::text("Keyboard")),
            ("B4", Cell::number(6.0)),
            ("C4", Cell::number(40.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }

        let mut cd = chart_from_range(&sh, "Budget", (0, 0, 3, 2), "column", false).expect("cols");
        assert_eq!(cd.series.len(), 2);
        // What the panel does when a user types `B2` into one VALUES field and
        // `B3` into the other: both pass the shape guard, and both come out of
        // `to_ref` as one-cell refs stacked down column B.
        for (i, r) in [1u32, 2].into_iter().enumerate() {
            let vr = cd.series[i].values_ref.as_mut().expect("ref");
            vr.range = (r, 1, r, 1);
            cd.series[i].values = crate::sheet::range_numbers(&sh, (r, 1, r, 1));
        }
        let refs: Vec<String> = cd
            .series
            .iter()
            .map(|s| s.values_ref.as_ref().expect("ref").to_ref())
            .collect();
        assert_eq!(refs, vec!["Budget!$B$2:$B$2", "Budget!$B$3:$B$3"]);

        let again = parse_chart(&crate::xlsx::chart_space_xml(&cd));
        // Pinned so `!again.by_row` cannot pass on a parse that produced
        // nothing: with no series at all the categories are never reached and
        // the fall-through answers column for the wrong reason.
        assert_eq!(again.series.len(), 2);
        assert!(
            !again.by_row,
            "the labels down A2:A4 are a column chart's, whatever the values' shape"
        );
    }

    /// The categories are trusted BECAUSE the panel refuses to point them the
    /// other way round (`categories_shape_err`). These pin the two shapes it
    /// does accept — the line each reading's own derivation writes, and a single
    /// cell — reading back as the orientation they were written on, so
    /// re-pointing the labels can never flip a chart on reload.
    #[test]
    fn re_pointing_the_labels_the_way_the_chart_reads_keeps_its_orientation() {
        use crate::sheet::{Cell, Sheet, chart_from_range, parse_cell_name};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Price")),
            ("D1", Cell::text("Total")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(900.0)),
            ("D2", Cell::number(1800.0)),
            ("A3", Cell::text("Monitor")),
            ("B3", Cell::number(4.0)),
            ("C3", Cell::number(150.0)),
            ("D3", Cell::number(600.0)),
            ("A4", Cell::text("Keyboard")),
            ("B4", Cell::number(6.0)),
            ("C4", Cell::number(40.0)),
            ("D4", Cell::number(240.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }

        // A COLUMN chart one data row deep: three series, each one cell, side
        // by side along row 2. Its labels are the one cell A2, and the only
        // other shape the panel takes here is a COLUMN — which is what it
        // already has, so nothing a user can commit turns them into a row.
        let cols = chart_from_range(&sh, "Budget", (0, 0, 1, 3), "column", false).expect("cols");
        assert_eq!(cols.series.len(), 3);
        assert_eq!(
            cols.categories_ref.as_ref().expect("cat").range,
            (1, 0, 1, 0)
        );
        let again = parse_chart(&crate::xlsx::chart_space_xml(&cols));
        assert_eq!(again.series.len(), 3);
        assert!(
            !again.by_row,
            "three cells along a row are a column chart's"
        );

        // A ROW chart over a range two columns wide: three series, each one
        // cell, stacked down column B. Its labels are the one cell B1, and
        // widening them stays a ROW (`B1:D1`) — never the column that would
        // read back as the other orientation.
        let mut rows = chart_from_range(&sh, "Budget", (0, 0, 3, 1), "column", true).expect("rows");
        assert_eq!(rows.series.len(), 3);
        assert_eq!(
            rows.categories_ref.as_ref().expect("cat").range,
            (0, 1, 0, 1)
        );
        let cat = rows.categories_ref.as_mut().expect("cat");
        cat.range = (0, 1, 0, 3);
        rows.categories = crate::sheet::range_labels(&sh, (0, 1, 0, 3));
        let again = parse_chart(&crate::xlsx::chart_space_xml(&rows));
        assert_eq!(again.series.len(), 3);
        assert!(
            again.by_row,
            "a row of labels is the row reading's, and the stacked cells agree"
        );
    }

    /// The mirror shape: one-cell series side by side ALONG one row, which is
    /// what a column chart over a range one data row deep comes to. The column
    /// reading produces it, so it stays the default rather than becoming row
    /// evidence.
    #[test]
    fn one_cell_series_along_a_row_stay_column_oriented() {
        use crate::sheet::{Cell, Sheet, chart_from_range, parse_cell_name};
        let mut sh = Sheet {
            name: "Budget".into(),
            ..Sheet::default()
        };
        for (addr, cell) in [
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Total")),
            ("A2", Cell::text("Laptop")),
            ("B2", Cell::number(2.0)),
            ("C2", Cell::number(2398.0)),
        ] {
            let (r, c) = parse_cell_name(addr).unwrap();
            sh.set_cell(r, c, cell);
        }
        let by_col = chart_from_range(&sh, "Budget", (0, 0, 1, 2), "column", false).expect("cols");
        assert_eq!(by_col.series.len(), 2);
        let again = parse_chart(&crate::xlsx::chart_space_xml(&by_col));
        assert!(
            !again.by_row,
            "two cells along one row are not row evidence"
        );
        let names: Vec<&str> = again.series.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["Qty", "Total"]);
        assert_eq!(again.categories, vec!["Laptop"]);
    }
}
