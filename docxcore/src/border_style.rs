//! How a border side is stroked (`w:val`, ECMA-376 §17.18.2): the one model
//! of compound lines (double, triple, thin-thick) that the PDF export's page
//! borders and the suite's table borders both draw from (#765).

/// How a border side is stroked.
#[derive(Debug, Clone, PartialEq)]
pub enum BorderStyle {
    /// One solid line: `single`, `thick`, and the styles drawn as it for now
    /// (`wave`, `doubleWave`, `dashDotStroked`, the 3-D styles).
    Single,
    /// Parallel solid lines, innermost first: whether each is thin (a third of
    /// the side's width), and the gap between them in widths of the thin line
    /// (or of the line, when none is thin).
    Lines { thin: Vec<bool>, gap: f32 },
    /// A dash array in multiples of the line width.
    Dashes(Vec<f32>),
    /// An art border (`apples`, `basicBlackDots`, …): a dashed band, since
    /// the artwork isn't drawn.
    Art,
}

impl BorderStyle {
    /// The style for `w:val`; `None` for no border.
    pub fn parse(val: &str) -> Option<BorderStyle> {
        let lines = |thin: &[bool], gap: f32| BorderStyle::Lines {
            thin: thin.to_vec(),
            gap,
        };
        Some(match val {
            "none" | "nil" => return None,
            "single" | "thick" | "wave" | "doubleWave" | "dashDotStroked" | "threeDEmboss"
            | "threeDEngrave" | "outset" | "inset" => BorderStyle::Single,
            // Gaps of one line width.
            "double" => lines(&[false, false], 1.0),
            "triple" => lines(&[false, false, false], 1.0),
            "dotted" => BorderStyle::Dashes(vec![1.0, 1.0]),
            "dashed" => BorderStyle::Dashes(vec![3.0, 2.0]),
            "dashSmallGap" => BorderStyle::Dashes(vec![3.0, 1.0]),
            "dotDash" => BorderStyle::Dashes(vec![3.0, 2.0, 1.0, 2.0]),
            "dotDotDash" => BorderStyle::Dashes(vec![3.0, 2.0, 1.0, 2.0, 1.0, 2.0]),
            // Our convention: the first-named line is the inner one (nearer the
            // text); the gap is 1, 2 or 3 thin widths.
            _ => {
                let (kind, gap) = if let Some(k) = val.strip_suffix("SmallGap") {
                    (k, 1.0)
                } else if let Some(k) = val.strip_suffix("MediumGap") {
                    (k, 2.0)
                } else if let Some(k) = val.strip_suffix("LargeGap") {
                    (k, 3.0)
                } else {
                    return Some(BorderStyle::Art);
                };
                match kind {
                    "thinThick" => lines(&[true, false], gap),
                    "thickThin" => lines(&[false, true], gap),
                    "thinThickThin" => lines(&[true, false, true], gap),
                    "thickThinThick" => lines(&[false, true, false], gap),
                    _ => BorderStyle::Art,
                }
            }
        })
    }

    /// The solid strokes a side of line width `w` is drawn with, innermost
    /// first: each stroke's width and the distance of its centre line from
    /// the first one's. One stroke for every style but [`BorderStyle::Lines`]
    /// (a dashed style dashes that one stroke).
    pub fn strokes(&self, w: f32) -> Vec<(f32, f32)> {
        let BorderStyle::Lines { thin, gap } = self else {
            return vec![(w, 0.0)];
        };
        let thin_w = (w / 3.0).max(0.25);
        let widths: Vec<f32> = thin.iter().map(|&t| if t { thin_w } else { w }).collect();
        // Double and triple lines are spaced by their own width; the
        // thin-thick families by thin widths.
        let gap = if thin.iter().any(|&t| t) {
            gap * thin_w
        } else {
            gap * w
        };
        let mut d = 0.0;
        let mut out = Vec::new();
        for (k, &sw) in widths.iter().enumerate() {
            if k > 0 {
                d += widths[k - 1] / 2.0 + gap + sw / 2.0;
            }
            out.push((sw, d));
        }
        out
    }

    /// The whole band the strokes of [`BorderStyle::strokes`] cover, from the
    /// first stroke's inner edge to the last one's outer edge.
    pub fn total(&self, w: f32) -> f32 {
        let s = self.strokes(w);
        match (s.first(), s.last()) {
            (Some(&(first, _)), Some(&(last, d))) => first / 2.0 + d + last / 2.0,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_lines_stroke_apart() {
        let s = |v: &str| BorderStyle::parse(v).unwrap().strokes(3.0);
        assert_eq!(s("single"), [(3.0, 0.0)]);
        assert_eq!(s("dashed"), [(3.0, 0.0)]);
        // Double: two lines a line width apart.
        assert_eq!(s("double"), [(3.0, 0.0), (3.0, 6.0)]);
        assert_eq!(s("triple"), [(3.0, 0.0), (3.0, 6.0), (3.0, 12.0)]);
        // Thin-thick, small gap: a thin line (a third), one thin width, the line.
        assert_eq!(s("thinThickSmallGap"), [(1.0, 0.0), (3.0, 3.0)]);
        assert_eq!(s("thickThinLargeGap"), [(3.0, 0.0), (1.0, 5.0)]);
        assert_eq!(BorderStyle::parse("double").unwrap().total(3.0), 9.0);
        assert_eq!(BorderStyle::parse("single").unwrap().total(3.0), 3.0);
        assert_eq!(BorderStyle::parse("nil"), None);
        assert_eq!(BorderStyle::parse("apples"), Some(BorderStyle::Art));
    }
}
