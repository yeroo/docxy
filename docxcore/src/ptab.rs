//! Alignment tabs (`w:ptab`, #768): where one aligns the text after it, what
//! fills the gap, and the geometry both hosts share. The terminal renderer
//! counts the units in cells, the suite in px; the maths is the same.

use crate::model::TabAlign;

/// What fills the gap an alignment tab leaves (`w:leader`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PtabLeader {
    #[default]
    None,
    Dot,
    Hyphen,
    Underscore,
    MiddleDot,
}

impl PtabLeader {
    /// The glyph that fills the gap; none for a blank gap.
    pub fn glyph(self) -> Option<char> {
        match self {
            PtabLeader::None => None,
            PtabLeader::Dot => Some('.'),
            PtabLeader::Hyphen => Some('-'),
            PtabLeader::Underscore => Some('_'),
            PtabLeader::MiddleDot => Some('\u{b7}'),
        }
    }
}

/// An alignment tab: where it aligns the text after it, whether it measures
/// from the paragraph's indents rather than the margins, and its leader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ptab {
    pub align: TabAlign,
    pub indent: bool,
    pub leader: PtabLeader,
}

/// The alignment tab a raw run holds, if it holds one.
pub fn parse(raw: &str) -> Option<Ptab> {
    if !raw.starts_with("<w:r") {
        return None;
    }
    let at = raw.find("<w:ptab")?;
    let end = at + raw[at..].find('>')?;
    let el = &raw[at..end];
    let attr = |name| crate::load::xml_attr_value(el, name);
    let align = match attr("w:alignment").as_deref() {
        Some("center") => TabAlign::Center,
        Some("right") => TabAlign::Right,
        _ => TabAlign::Left,
    };
    let leader = match attr("w:leader").as_deref() {
        Some("dot") => PtabLeader::Dot,
        Some("hyphen") => PtabLeader::Hyphen,
        Some("underscore") => PtabLeader::Underscore,
        Some("middleDot") => PtabLeader::MiddleDot,
        _ => PtabLeader::None,
    };
    Some(Ptab {
        align,
        indent: attr("w:relativeTo").as_deref() == Some("indent"),
        leader,
    })
}

/// Where a tab's stop is, from the left margin, on a line `width` wide whose
/// paragraph is indented `left` and `right`. A right stop sits `inset` inside
/// the edge, so the segment ending there does not wrap the row.
pub fn stop(p: Ptab, width: f32, left: f32, right: f32, inset: f32) -> f32 {
    let (lo, hi) = if p.indent {
        (left, (width - right).max(left))
    } else {
        (0.0, width)
    };
    match p.align {
        TabAlign::Left => lo,
        TabAlign::Center => (lo + hi) / 2.0,
        TabAlign::Right => (hi - inset).max(lo),
    }
}

/// The space an alignment tab at `x` (from the margin) leaves so the segment
/// after it, `seg` wide, starts at, centres on or ends at `stop`; none when
/// the text has already passed it (it does not wrap).
pub fn gap(align: TabAlign, stop: f32, x: f32, seg: f32) -> f32 {
    match align {
        TabAlign::Center => stop - seg / 2.0 - x,
        TabAlign::Right => stop - seg - x,
        TabAlign::Left => stop - x,
    }
    .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(attrs: &str) -> Option<Ptab> {
        parse(&format!("<w:r><w:ptab {attrs}/></w:r>"))
    }

    #[test]
    fn reads_alignment_relative_to_and_leader() {
        let p =
            tab("w:relativeTo=\"indent\" w:alignment=\"center\" w:leader=\"middleDot\"").unwrap();
        assert_eq!(p.align, TabAlign::Center);
        assert!(p.indent);
        assert_eq!(p.leader, PtabLeader::MiddleDot);
        assert_eq!(p.leader.glyph(), Some('\u{b7}'));
        let p = tab("w:relativeTo=\"margin\" w:alignment=\"right\" w:leader=\"none\"").unwrap();
        assert!(!p.indent);
        assert_eq!(p.leader.glyph(), None);
        assert!(parse("<w:r><w:t>x</w:t></w:r>").is_none());
        assert!(parse("<w:ptab/>").is_none());
    }

    #[test]
    fn stops_and_gaps() {
        let right = tab("w:relativeTo=\"margin\" w:alignment=\"right\"").unwrap();
        assert_eq!(stop(right, 80.0, 10.0, 5.0, 1.0), 79.0);
        let centre = tab("w:relativeTo=\"indent\" w:alignment=\"center\"").unwrap();
        assert_eq!(stop(centre, 80.0, 10.0, 10.0, 1.0), 40.0);
        let left = tab("w:relativeTo=\"indent\" w:alignment=\"left\"").unwrap();
        assert_eq!(stop(left, 80.0, 10.0, 10.0, 1.0), 10.0);
        // The segment ends at / centres on / starts at the stop; past it, no gap.
        assert_eq!(gap(TabAlign::Right, 79.0, 5.0, 10.0), 64.0);
        assert_eq!(gap(TabAlign::Center, 40.0, 5.0, 10.0), 30.0);
        assert_eq!(gap(TabAlign::Left, 10.0, 5.0, 10.0), 5.0);
        assert_eq!(gap(TabAlign::Right, 79.0, 75.0, 10.0), 0.0);
    }
}
