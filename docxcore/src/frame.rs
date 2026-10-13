//! Where a header or footer's framed paragraph (`w:framePr`) sits on its page
//! (#767): Insert › Page Number › Page Margins places the number in one, and
//! the Print Layout view and the PDF export both draw it from [`frame_rect`],
//! out of the header/footer band's flow ([`split_framed`]).
//!
//! All values are twips (1/1440 inch), measured down and right from the
//! page's top-left corner.

use crate::model::{Block, FramePr, Inline, Paragraph};

/// The page a frame is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameGeom {
    pub page_w: i32,
    pub page_h: i32,
    /// The page's side margins as drawn: after mirror margins and with the
    /// gutter added where it sits, so `hAnchor="margin"` follows them.
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
    /// Where the band holding the paragraph starts its flow: the header
    /// distance in a header, the footer's top in a footer. `vAnchor="text"`
    /// measures from it.
    pub band_top: i32,
    /// An even page: `inside` is the right side there, `outside` the left.
    pub even: bool,
}

/// A frame's rectangle on its page, twips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// The frame of a paragraph that leaves the band's flow: a `w:framePr` that
/// is not a drop cap (a drop cap stays on its paragraph's lines).
pub fn placed_frame(b: &Block) -> Option<&FramePr> {
    let Block::Paragraph(p) = b else { return None };
    let f = p.props.frame.as_ref()?;
    match f.drop_cap.as_deref() {
        Some("drop" | "margin") => None,
        _ => Some(f),
    }
}

/// A header or footer's blocks split into those its band flows (with the
/// content-control boundaries) and the framed paragraphs drawn at their
/// frame, each with its block index (an editor path).
pub fn split_framed(blocks: &[Block]) -> (Vec<(usize, &Block)>, Vec<(usize, &Paragraph)>) {
    let mut flow = Vec::new();
    let mut framed = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        match b {
            Block::Paragraph(p) if placed_frame(b).is_some() => framed.push((i, p)),
            _ => flow.push((i, b)),
        }
    }
    (flow, framed)
}

/// One line of a paragraph with no frame height: 1.2 x its largest run or
/// field result size (11 pt when none sets one), twips.
pub fn line_height(p: &Paragraph) -> i32 {
    let half_pts = p
        .content
        .iter()
        .filter_map(|i| match i {
            Inline::Run(r) => r.props.size_half_pts,
            Inline::Field { raw, .. } => crate::load::field_result_props(raw).size_half_pts,
            _ => None,
        })
        .max()
        .unwrap_or(22);
    // half-points x 10 = twips; x 1.2 for the line.
    (half_pts as i32) * 12
}

/// The frame's rectangle on the page described by `g`. `line_h` is the
/// height used when the frame sets none (see [`line_height`]).
///
/// - Width: `w`, else the text width (the page less its side margins).
/// - Height: `h` (exact or at least), else `line_h`.
/// - Horizontal: measured from the page (`hAnchor="page"`), else from the
///   margins (`margin`, `text`, `column` and none: the suite's header and
///   footer bands are one column). `xAlign` wins over `x`, as the schema
///   says; with neither the frame sits at the anchor's left edge.
///   `inside` is left on odd pages and right on even ones, `outside` the
///   opposite.
/// - Vertical: from the page (`vAnchor="page"`), the margins (`margin`), or
///   the band's flow start (`text` and none). `yAlign` wins over `y`;
///   `inside` is top, `outside` bottom, `inline` the anchor's top; with
///   neither the frame sits at the anchor's top.
pub fn frame_rect(f: &FramePr, g: &FrameGeom, line_h: i32) -> FrameRect {
    let text_w = (g.page_w - g.left - g.right).max(1);
    let w = f.w.filter(|w| *w > 0).unwrap_or(text_w);
    let h = f.h.filter(|h| *h > 0).unwrap_or(line_h).max(1);
    let (ax, aw) = match f.h_anchor.as_deref() {
        Some("page") => (0, g.page_w),
        _ => (g.left, text_w),
    };
    let x = match f.x_align.as_deref() {
        Some(a) => {
            let right = match a {
                "right" => true,
                "inside" => g.even,
                "outside" => !g.even,
                _ => false,
            };
            if a == "center" {
                ax + (aw - w) / 2
            } else if right {
                ax + aw - w
            } else {
                ax
            }
        }
        None => ax + f.x.unwrap_or(0),
    };
    let (ay, ah) = match f.v_anchor.as_deref() {
        Some("page") => (0, g.page_h),
        Some("margin") => (g.top, (g.page_h - g.top - g.bottom).max(1)),
        _ => (g.band_top, (g.page_h - g.band_top).max(1)),
    };
    let y = match f.y_align.as_deref() {
        Some("center") => ay + (ah - h) / 2,
        Some("bottom" | "outside") => ay + ah - h,
        Some(_) => ay,
        None => ay + f.y.unwrap_or(0),
    };
    FrameRect { x, y, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParProps, Run, RunProps};

    /// Letter, 1" top and bottom, 1.25" left, 0.75" right, header at 0.5".
    const G: FrameGeom = FrameGeom {
        page_w: 12240,
        page_h: 15840,
        left: 1800,
        right: 1080,
        top: 1440,
        bottom: 1440,
        band_top: 720,
        even: false,
    };

    fn frame(h_anchor: Option<&str>, v_anchor: Option<&str>) -> FramePr {
        FramePr {
            w: Some(600),
            h: Some(400),
            h_anchor: h_anchor.map(str::to_string),
            v_anchor: v_anchor.map(str::to_string),
            ..Default::default()
        }
    }

    fn x_of(f: &FramePr, g: &FrameGeom) -> i32 {
        frame_rect(f, g, 0).x
    }

    fn y_of(f: &FramePr) -> i32 {
        frame_rect(f, &G, 0).y
    }

    #[test]
    fn x_is_measured_from_the_page_or_the_margins() {
        for (anchor, origin) in [
            (Some("page"), 0),
            (Some("margin"), 1800),
            (Some("text"), 1800),
            (Some("column"), 1800),
            (None, 1800),
        ] {
            let mut f = frame(anchor, None);
            // Neither x nor xAlign: the anchor's left edge.
            assert_eq!(x_of(&f, &G), origin, "{anchor:?}");
            f.x = Some(250);
            assert_eq!(x_of(&f, &G), origin + 250, "{anchor:?}");
        }
    }

    #[test]
    fn x_align_wins_over_x() {
        let mut f = frame(Some("page"), None);
        f.x = Some(5000);
        for (align, x) in [
            ("left", 0),
            ("center", (12240 - 600) / 2),
            ("right", 12240 - 600),
            ("inside", 0),
            ("outside", 12240 - 600),
        ] {
            f.x_align = Some(align.into());
            assert_eq!(x_of(&f, &G), x, "{align}");
        }
        // Margin-anchored: within the text width.
        let mut f = frame(Some("margin"), None);
        f.x_align = Some("right".into());
        assert_eq!(x_of(&f, &G), 12240 - 1080 - 600);
        f.x_align = Some("center".into());
        assert_eq!(x_of(&f, &G), 1800 + (12240 - 1800 - 1080 - 600) / 2);
    }

    #[test]
    fn inside_and_outside_swap_on_even_pages() {
        let even = FrameGeom { even: true, ..G };
        let mut f = frame(Some("page"), None);
        f.x_align = Some("inside".into());
        assert_eq!(x_of(&f, &even), 12240 - 600);
        f.x_align = Some("outside".into());
        assert_eq!(x_of(&f, &even), 0);
    }

    #[test]
    fn margin_anchor_follows_mirrored_margins() {
        // Mirror margins on an even page: the caller swaps left and right.
        let mirrored = FrameGeom {
            left: G.right,
            right: G.left,
            even: true,
            ..G
        };
        let mut f = frame(Some("margin"), None);
        assert_eq!(x_of(&f, &mirrored), 1080);
        f.x_align = Some("right".into());
        assert_eq!(x_of(&f, &mirrored), 12240 - 1800 - 600);
    }

    #[test]
    fn y_is_measured_from_the_page_margins_or_band() {
        for (anchor, origin) in [
            (Some("page"), 0),
            (Some("margin"), 1440),
            (Some("text"), 720),
            (None, 720),
        ] {
            let mut f = frame(None, anchor);
            assert_eq!(y_of(&f), origin, "{anchor:?}");
            f.y = Some(300);
            assert_eq!(y_of(&f), origin + 300, "{anchor:?}");
        }
    }

    #[test]
    fn y_align_wins_over_y() {
        let mut f = frame(None, Some("page"));
        f.y = Some(9000);
        for (align, y) in [
            ("top", 0),
            ("center", (15840 - 400) / 2),
            ("bottom", 15840 - 400),
            ("inside", 0),
            ("outside", 15840 - 400),
            ("inline", 0),
        ] {
            f.y_align = Some(align.into());
            assert_eq!(y_of(&f), y, "{align}");
        }
        let mut f = frame(None, Some("margin"));
        f.y_align = Some("bottom".into());
        assert_eq!(y_of(&f), 15840 - 1440 - 400);
    }

    #[test]
    fn a_missing_width_takes_the_text_width_and_a_missing_height_one_line() {
        let f = FramePr {
            h_anchor: Some("margin".into()),
            ..Default::default()
        };
        let r = frame_rect(&f, &G, 264);
        assert_eq!(r.w, 12240 - 1800 - 1080);
        assert_eq!(r.h, 264);
    }

    #[test]
    fn the_line_height_follows_the_largest_run() {
        let run = |size| {
            Inline::Run(Run {
                text: "1".into(),
                props: RunProps {
                    size_half_pts: size,
                    ..Default::default()
                },
            })
        };
        let p = |content| Paragraph {
            content,
            ..Default::default()
        };
        assert_eq!(line_height(&p(vec![run(None)])), 264);
        assert_eq!(line_height(&p(vec![run(Some(56)), run(Some(24))])), 672);
    }

    #[test]
    fn framed_paragraphs_leave_the_flow_and_drop_caps_stay() {
        let para = |frame: Option<FramePr>| {
            Block::Paragraph(Paragraph {
                props: ParProps {
                    frame,
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        let drop_cap = FramePr {
            drop_cap: Some("drop".into()),
            lines: Some(3),
            ..Default::default()
        };
        let not_drop = FramePr {
            drop_cap: Some("none".into()),
            ..frame(Some("page"), Some("page"))
        };
        let blocks = vec![
            Block::Raw("<w:sdt><w:sdtPr/><w:sdtContent>".into()),
            para(Some(frame(Some("page"), Some("page")))),
            Block::Raw("</w:sdtContent></w:sdt>".into()),
            para(None),
            para(Some(drop_cap)),
            para(Some(not_drop)),
        ];
        let (flow, framed) = split_framed(&blocks);
        assert_eq!(
            flow.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 2, 3, 4]
        );
        assert_eq!(
            framed.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![1, 5]
        );
    }
}
