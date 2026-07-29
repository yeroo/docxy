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
                                kind = Some(DrawingKind::Chart(parse_chart(&cxml)));
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

/// Edit a drawing part in place: move the `<from>`/`<to>` cells of the anchors
/// in `moves`, drop the anchor elements listed in `drop`, and leave everything
/// else — other anchors, offsets, artwork — byte for byte as it was. Both are
/// keyed by [`Drawing::anchor_ix`], which counts every anchor in the part
/// (including the ones we don't model). A `oneCellAnchor` has no `<to>`; only
/// its `<from>` moves, and its extent rides along.
pub fn rewrite_anchors(xml: &str, moves: &[(usize, (u32, u32), (u32, u32))], drop: &[usize]) -> String {
    if moves.is_empty() && drop.is_empty() {
        return xml.to_string();
    }
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    let mut ix = 0usize;
    while let Some((cut, tag)) = next_anchor(rest) {
        out.push_str(&rest[..cut]);
        rest = &rest[cut..];
        let Some(end) = find_close(rest, tag) else { break };
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
        let l = if name.starts_with(['/', '!', '?']) { "" } else { local(name) };
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
        let Some(name_end) = rest[start + 1..].find(['>', ' ', '/']).map(|i| i + start + 1) else { break };
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
        match value.and_then(|v| find_close(tail, name).map(|e| (v, e))) {
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
                        cd.kind = if p.attr("val") == "bar" { "bar" } else { "column" }.to_string();
                    }
                    "title" => in_title = true,
                    "ser" => cd.series.push(ChartSeries::default()),
                    "tx" => mode = 1,
                    "cat" => mode = 2,
                    "val" => mode = 3,
                    "v" => in_v = true,
                    "t" if in_title => in_title_text = true,
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
                } else if in_title_text {
                    cd.title.push_str(p.text());
                }
            }
            Event::End => match local(p.name()) {
                "title" => in_title = false,
                "t" => in_title_text = false,
                "v" => in_v = false,
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

    #[test]
    fn rewrite_anchors_moves_one_and_leaves_the_rest_alone() {
        // Two anchors; the first holds a shape we don't model, so the picture's
        // `anchor_ix` is 1 even though it is the only Drawing parsed.
        let xml = r#"<xdr:wsDr xmlns:xdr="a" xmlns:r="b">
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
        let resolve = |rid: &str| (rid == "rId1").then(|| ("image/png".to_string(), "xl/media/image1.png".to_string()));
        let get = |_: &str| None;
        let ds = parse_drawings(xml, &resolve, &get);
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].anchor_ix, 1, "the unmodelled shape still occupies anchor 0");

        // Move it three rows down and one column right.
        let out = rewrite_anchors(xml, &[(1, (5, 2), (13, 6))], &[]);
        let moved = parse_drawings(&out, &resolve, &get);
        assert_eq!(moved[0].from, (5, 2));
        assert_eq!(moved[0].to, (13, 6));
        // The untouched anchor and the offsets inside the moved one survive.
        assert!(out.contains("<xdr:col>0</xdr:col><xdr:colOff>7</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>9</xdr:rowOff>"));
        assert!(out.contains("<xdr:sp/>"));
        assert!(out.contains(r#"<xdr:cNvPr id="2" name="Logo"/>"#));
        assert!(out.contains("<xdr:colOff>0</xdr:colOff>"), "the moved anchor keeps its offsets");
        // Rewriting nothing is a byte-for-byte no-op.
        assert_eq!(rewrite_anchors(xml, &[], &[]), xml);

        // Dropping the picture's anchor leaves the shape's anchor behind.
        let culled = rewrite_anchors(xml, &[], &[1]);
        assert!(parse_drawings(&culled, &resolve, &get).is_empty());
        assert!(culled.contains("<xdr:sp/>"));
        assert!(!culled.contains("Logo"));
        assert_eq!(culled.matches("<xdr:twoCellAnchor>").count(), 1);
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
