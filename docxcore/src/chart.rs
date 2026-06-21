//! Parse DrawingML chart parts (`word/charts/chartN.xml`) and render them as
//! text for the terminal.
//!
//! Word charts embed their plotted data as cached values right in the chart
//! part (`c:numCache` / `c:strCache`), so we can read categories and values
//! without cracking the embedded spreadsheet. The terminal can't draw real
//! chart graphics, so [`render_chart`] produces a compact bar/pie text view.

use crate::xml::{Event, XmlParser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Bar,
    Line,
    Pie,
    Area,
    Radar,
    Scatter,
    Bubble,
    Other,
}

impl ChartKind {
    fn label(self) -> &'static str {
        match self {
            ChartKind::Bar => "Bar",
            ChartKind::Line => "Line",
            ChartKind::Pie => "Pie",
            ChartKind::Area => "Area",
            ChartKind::Radar => "Radar",
            ChartKind::Scatter => "Scatter",
            ChartKind::Bubble => "Bubble",
            ChartKind::Other => "Chart",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    pub name: String,
    pub cats: Vec<String>,
    pub vals: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    pub title: Option<String>,
    pub series: Vec<Series>,
}

// `Inline` derives `Eq`; chart values are f64. Charts come from parsed cached
// data (never NaN), so treating them as `Eq` is sound for our use.
impl Eq for Chart {}
impl Eq for Series {}

fn kind_of(el: &str) -> Option<ChartKind> {
    Some(match el {
        "c:barChart" | "c:bar3DChart" => ChartKind::Bar,
        "c:lineChart" | "c:line3DChart" | "c:stockChart" => ChartKind::Line,
        "c:pieChart" | "c:pie3DChart" | "c:ofPieChart" | "c:doughnutChart" => ChartKind::Pie,
        "c:areaChart" | "c:area3DChart" => ChartKind::Area,
        "c:radarChart" => ChartKind::Radar,
        "c:scatterChart" => ChartKind::Scatter,
        "c:bubbleChart" => ChartKind::Bubble,
        _ => return None,
    })
}

/// The nearest enclosing data section of the path (series name / categories /
/// values), so a `<c:v>` text node can be attributed correctly.
fn section<'a>(path: &[&'a str]) -> Option<&'a str> {
    path.iter()
        .rev()
        .copied()
        .find(|n| matches!(*n, "c:tx" | "c:cat" | "c:val" | "c:xVal" | "c:yVal"))
}

/// Parse a chart part into a [`Chart`]. Returns `None` if it holds no plot type.
pub fn parse_chart_xml(xml: &str) -> Option<Chart> {
    let mut p = XmlParser::new(xml);
    let mut path: Vec<&str> = Vec::new();
    let mut kind: Option<ChartKind> = None;
    let mut title = String::new();
    let mut series: Vec<Series> = Vec::new();

    loop {
        match p.next() {
            Event::Start => {
                let n = p.name();
                if kind.is_none() {
                    if let Some(k) = kind_of(n) {
                        kind = Some(k);
                    }
                }
                if n == "c:ser" {
                    series.push(Series::default());
                }
                path.push(n);
            }
            Event::Text => {
                let cur = *path.last().unwrap_or(&"");
                if cur != "c:v" && cur != "a:t" {
                    continue;
                }
                let mut s = String::new();
                XmlParser::append_decoded(p.text(), &mut s);
                let in_ser = path.contains(&"c:ser");
                let in_title = path.contains(&"c:title");
                if in_title && !in_ser {
                    title.push_str(&s);
                } else if in_ser {
                    if let Some(ser) = series.last_mut() {
                        match section(&path) {
                            Some("c:tx") => {
                                if ser.name.is_empty() {
                                    ser.name.push_str(s.trim());
                                }
                            }
                            Some("c:cat") | Some("c:xVal") => ser.cats.push(s.trim().to_string()),
                            Some("c:val") | Some("c:yVal") => {
                                if let Ok(v) = s.trim().parse::<f64>() {
                                    ser.vals.push(v);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Event::End => {
                path.pop();
            }
            Event::Eof => break,
        }
    }

    let kind = kind?;
    series.retain(|s| !s.vals.is_empty());
    let title = {
        let t = title.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    Some(Chart {
        kind,
        title,
        series,
    })
}

// ---- rendering ----

const BLOCKS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// A horizontal bar of `barw` cells filled to `frac` (0..=1), with eighth-cell
/// precision at the tip.
fn block_bar(frac: f64, barw: usize) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let eighths = (frac * barw as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let rem = eighths % 8;
    let mut s = String::new();
    for _ in 0..full.min(barw) {
        s.push('█');
    }
    if full < barw && rem > 0 {
        s.push(BLOCKS[rem - 1]);
    }
    s
}

fn fmt_num(v: f64) -> String {
    if (v.fract()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn fit(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        format!("{s:<w$}")
    } else if w == 0 {
        String::new()
    } else {
        let mut out: String = s.chars().take(w - 1).collect();
        out.push('…');
        out
    }
}

fn bar_lines(items: &[(String, f64)], width: usize) -> Vec<String> {
    if items.is_empty() {
        return vec!["(no data)".to_string()];
    }
    let maxv = items
        .iter()
        .map(|(_, v)| v.abs())
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let labelw = items
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(3)
        .clamp(3, 18);
    let valw = items
        .iter()
        .map(|(_, v)| fmt_num(*v).len())
        .max()
        .unwrap_or(1);
    let barw = width.saturating_sub(labelw + valw + 2).max(4);
    items
        .iter()
        .map(|(label, v)| {
            let bar = block_bar(v.abs() / maxv, barw);
            format!(
                "{} {:<barw$} {:>valw$}",
                fit(label, labelw),
                bar,
                fmt_num(*v)
            )
        })
        .collect()
}

fn pie_lines(ser: &Series, width: usize) -> Vec<String> {
    let total: f64 = ser.vals.iter().map(|v| v.abs()).sum::<f64>().max(1e-9);
    let labelw = ser
        .cats
        .iter()
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(3)
        .clamp(3, 18);
    let barw = width.saturating_sub(labelw + 8).max(4);
    ser.vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let cat = ser
                .cats
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("{}", i + 1));
            let pct = v.abs() / total;
            format!(
                "{} {:<barw$} {:>5.1}%",
                fit(&cat, labelw),
                block_bar(pct, barw),
                pct * 100.0
            )
        })
        .collect()
}

/// Render a chart as text lines that fit within `width` columns. The first line
/// is a heading (kind + title); the rest are bars (or a pie legend).
pub fn render_chart(chart: &Chart, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut out = Vec::new();
    out.push(match &chart.title {
        Some(t) => format!("{} chart — {t}", chart.kind.label()),
        None => format!("{} chart", chart.kind.label()),
    });

    if chart.kind == ChartKind::Pie {
        if let Some(ser) = chart.series.first() {
            out.extend(pie_lines(ser, width));
            return out;
        }
    }

    let multi = chart.series.len() > 1;
    let mut items: Vec<(String, f64)> = Vec::new();
    for ser in &chart.series {
        for (i, v) in ser.vals.iter().enumerate() {
            let cat = ser
                .cats
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("{}", i + 1));
            let label = if multi && !ser.name.is_empty() {
                format!("{}/{cat}", ser.name)
            } else {
                cat
            };
            items.push((label, *v));
        }
    }
    out.extend(bar_lines(&items, width));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMN: &str = r#"<c:chartSpace xmlns:c="x" xmlns:a="y" xmlns:r="z">
      <c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>fruit</a:t></a:r></a:p></c:rich></c:tx></c:title>
      <c:plotArea><c:bar3DChart>
        <c:ser>
          <c:tx><c:strRef><c:strCache><c:pt idx="0"><c:v>Series 1</c:v></c:pt></c:strCache></c:strRef></c:tx>
          <c:cat><c:strRef><c:strCache>
            <c:pt idx="0"><c:v>apple</c:v></c:pt>
            <c:pt idx="1"><c:v>banana</c:v></c:pt>
            <c:pt idx="2"><c:v>pear</c:v></c:pt>
          </c:strCache></c:strRef></c:cat>
          <c:val><c:numRef><c:numCache>
            <c:pt idx="0"><c:v>4.6</c:v></c:pt>
            <c:pt idx="1"><c:v>2.9</c:v></c:pt>
            <c:pt idx="2"><c:v>3.9</c:v></c:pt>
          </c:numCache></c:numRef></c:val>
        </c:ser>
      </c:bar3DChart></c:plotArea></c:chart></c:chartSpace>"#;

    #[test]
    fn parses_kind_title_and_series() {
        let c = parse_chart_xml(COLUMN).unwrap();
        assert_eq!(c.kind, ChartKind::Bar);
        assert_eq!(c.title.as_deref(), Some("fruit"));
        assert_eq!(c.series.len(), 1);
        assert_eq!(c.series[0].name, "Series 1");
        assert_eq!(c.series[0].cats, ["apple", "banana", "pear"]);
        assert_eq!(c.series[0].vals, [4.6, 2.9, 3.9]);
    }

    #[test]
    fn renders_a_bar_per_category() {
        let c = parse_chart_xml(COLUMN).unwrap();
        let lines = render_chart(&c, 40);
        assert!(lines[0].contains("Bar chart") && lines[0].contains("fruit"));
        assert_eq!(lines.len(), 4); // heading + 3 bars
        assert!(lines[1].contains("apple"));
        // the largest value (apple, 4.6) gets the longest bar
        let bars: Vec<usize> = lines[1..].iter().map(|l| l.matches('█').count()).collect();
        assert!(bars[0] >= bars[1] && bars[0] >= bars[2]);
    }

    #[test]
    fn pie_shows_percentages() {
        let xml = r#"<c:chartSpace xmlns:c="x"><c:chart><c:plotArea><c:pieChart><c:ser>
          <c:cat><c:strCache><c:pt idx="0"><c:v>a</c:v></c:pt><c:pt idx="1"><c:v>b</c:v></c:pt></c:strCache></c:cat>
          <c:val><c:numCache><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>3</c:v></c:pt></c:numCache></c:val>
        </c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"#;
        let c = parse_chart_xml(xml).unwrap();
        assert_eq!(c.kind, ChartKind::Pie);
        let lines = render_chart(&c, 40);
        assert!(lines.iter().any(|l| l.contains("25.0%")));
        assert!(lines.iter().any(|l| l.contains("75.0%")));
    }

    #[test]
    fn non_chart_xml_is_none() {
        assert!(parse_chart_xml("<c:chartSpace/>").is_none());
    }

    #[test]
    fn real_chart_docx_renders_a_chart_box() {
        // End-to-end: load a real corpus file and confirm the chart reaches the
        // rendered output. Skips when the corpus copy isn't present.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../corpus/files/chart/chart-Column.docx"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let pkg = crate::package::load_package(&data).expect("load package");
        let opts = crate::render::RenderOptions {
            width: 60,
            ..Default::default()
        };
        let text: String = crate::render::render(&pkg.document, &opts)
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Bar chart") && text.contains("apple"),
            "chart not rendered; got:\n{}",
            &text[..text.len().min(500)]
        );
    }
}
