//! Parse `xl/drawings/*.xml` anchors into [`Drawing`]s (pictures + charts), and
//! the cached data of `xl/charts/*.xml`. Enough to render a floating overlay in
//! the grid — not to edit the artwork.

use opccore::xml::{Event, XmlParser};

use crate::sheet::{ChartData, ChartSeries, Drawing, DrawingKind};

/// The local (namespace-stripped) part of an XML name.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
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
    (row, col)
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

/// The next anchor element start in `xml`: its offset and its full tag name.
fn next_anchor(xml: &str) -> Option<(usize, &str)> {
    let mut at = 0usize;
    while let Some(rel) = xml[at..].find('<') {
        let start = at + rel;
        let name_end = xml[start + 1..].find(['>', ' ', '/'])? + start + 1;
        let name = &xml[start + 1..name_end];
        // Closing tags share the local name, so only openers count.
        let l = if name.starts_with(['/', '!', '?']) {
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
        let Some(name_end) = rest[start + 1..]
            .find(['>', ' ', '/'])
            .map(|i| i + start + 1)
        else {
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
        let Some(name_end) = tail[1..].find(['>', ' ', '/']).map(|i| i + 1) else {
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
    (from.0 + rows.max(1), from.1 + cols.max(1))
}

/// Parse the cached data of a chart part (`c:chartSpace`).
fn parse_chart(xml: &str) -> ChartData {
    let mut cd = ChartData::default();
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
    let mut ser_depth = 0i32;
    loop {
        match p.next() {
            Event::Start => {
                let name = local(p.name());
                match name {
                    n if n.ends_with("Chart") && cd.kind.is_empty() => {
                        // `barChart` covers BOTH orientations — the following
                        // <c:barDir val="col|bar"/> decides. Default to "column"
                        // (OOXML's own default is col) and refine on barDir.
                        cd.kind = match n.trim_end_matches("Chart") {
                            "bar" => {
                                in_bar = true;
                                "column".to_string()
                            }
                            other => other.to_string(),
                        };
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
                    "title" => in_title = true,
                    "ser" => {
                        cd.series.push(ChartSeries::default());
                        ser_depth += 1;
                    }
                    "tx" => mode = 1,
                    "cat" => mode = 2,
                    "val" => mode = 3,
                    "v" => in_v = true,
                    "f" => in_f = true,
                    "t" if in_title => in_title_text = true,
                    "srgbClr" if ser_depth == 1 && mode == 0 => {
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
                if in_v {
                    let t = p.text().trim();
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
                            if cd.series.len() <= 1 && !t.is_empty() {
                                cd.categories.push(t.to_string());
                            }
                        }
                        3 => {
                            if let (Ok(x), Some(s)) = (t.parse::<f64>(), cd.series.last_mut()) {
                                s.values.push(x);
                            }
                        }
                        _ => {}
                    }
                // Only a <c:f> INSIDE a <c:ser> names data. `mode` alone isn't
                // enough: a chart title and every axis title are <c:tx> too, and
                // one linked to a cell would otherwise land on the last series'
                // name and widen the chart's box to cover the title cell.
                } else if in_f && ser_depth == 1 {
                    let raw = p.text().trim().to_string();
                    if let Some(src) = crate::sheet::ChartSource::parse_f_ref(&raw) {
                        // Each ref belongs to whatever block it sits in, so a
                        // series can later be re-pointed on its own; their union
                        // is the chart's overall box.
                        match mode {
                            1 => {
                                if let Some(sr) = cd.series.last_mut() {
                                    sr.name_ref = Some(raw);
                                }
                            }
                            2 => cd.categories_ref = Some(src.clone()),
                            3 => {
                                if let Some(sr) = cd.series.last_mut() {
                                    sr.values_ref = Some(src.clone());
                                }
                            }
                            _ => {}
                        }
                        match &mut cd.source {
                            Some(cur) if cur.sheet == src.sheet => cur.union(&src),
                            Some(_) => {}
                            slot => *slot = Some(src),
                        }
                    }
                } else if in_title_text {
                    cd.title.push_str(p.text());
                }
            }
            Event::End => match local(p.name()) {
                "title" => in_title = false,
                "t" => in_title_text = false,
                "v" => in_v = false,
                "f" => in_f = false,
                "ser" => ser_depth -= 1,
                "tx" | "cat" | "val" => mode = 0,
                _ => {}
            },
            Event::Eof => break,
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
}
