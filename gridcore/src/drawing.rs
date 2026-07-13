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
    loop {
        match p.next() {
            Event::Start => match local(p.name()) {
                "twoCellAnchor" | "oneCellAnchor" | "absoluteAnchor" => {
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
                        let t = to
                            .or_else(|| ext.map(|e| estimate_to(f, e)))
                            .unwrap_or(f);
                        out.push(Drawing { from: f, to: t, kind: k });
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
    let mut mode = 0u8; // 1 = series name (tx), 2 = category (cat), 3 = value (val)
    loop {
        match p.next() {
            Event::Start => {
                let name = local(p.name());
                match name {
                    n if n.ends_with("Chart") && cd.kind.is_empty() => {
                        cd.kind = n.trim_end_matches("Chart").to_string();
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
        let resolve =
            |rid: &str| (rid == "rId2").then(|| ("chart".to_string(), "xl/charts/chart1.xml".to_string()));
        let get = |part: &str| (part == "xl/charts/chart1.xml").then(|| chart.to_string());
        let ds = parse_drawings(drawing, &resolve, &get);
        assert_eq!(ds.len(), 1);
        match &ds[0].kind {
            DrawingKind::Chart(c) => {
                assert_eq!(c.kind, "bar");
                assert_eq!(c.title, "Sales");
                assert_eq!(c.categories, vec!["North", "South"]);
                assert_eq!(c.series.len(), 1);
                assert_eq!(c.series[0].name, "Q1");
                assert_eq!(c.series[0].values, vec![10.0, 20.0]);
            }
            _ => panic!("expected chart"),
        }
    }
}
