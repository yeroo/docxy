//! Width and visible range for document tabs in the suite title bar.

pub const TAB_MIN_W: f32 = 90.0;
pub const TAB_MAX_W: f32 = 200.0;
pub const TAB_FLOOR_W: f32 = 48.0;
pub const TAB_GAP: f32 = 4.0;
pub const ARROW_W: f32 = 22.0;
pub const MORE_W: f32 = 28.0;
pub const DRAG_MIN_W: f32 = 40.0;

#[derive(Debug, Clone, Copy)]
pub struct TitleInsets {
    pub left: f32,
    pub right: f32,
}

/// Account for the Root shadow/border, TitleBar's own padding and its controls.
/// All arguments are logical pixels; the result is the TitleBar content width.
pub fn title_content_w(
    viewport_w: f32,
    insets: TitleInsets,
    title_left_pad: f32,
    caption_w: f32,
    fullscreen_pad: f32,
) -> f32 {
    (viewport_w - insets.right - caption_w - insets.left - title_left_pad - fullscreen_pad).max(0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    None,
    Fit,
    Shrink,
    Overflow,
    MoreOnly,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Fit => "fit",
            Self::Shrink => "shrink",
            Self::Overflow => "overflow",
            Self::MoreOnly => "more-only",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StripLayout {
    pub mode: Mode,
    pub first: usize,
    pub end: usize,
    pub tab_w: f32,
    pub arrows: bool,
    pub more: bool,
    /// Width of all controls and visible chips, including their internal gaps.
    pub width: f32,
}

impl StripLayout {
    pub fn active_visible(self, active: usize) -> bool {
        self.first <= active && active < self.end
    }
}

/// `avail` is the remaining logical-pixel width after the chrome and drag area.
/// The active tab stays in view whenever even a floor-width chip can fit.
pub fn layout(avail: f32, n: usize, active: usize, first: usize) -> StripLayout {
    let avail = if avail.is_finite() {
        avail.max(0.0)
    } else {
        0.0
    };
    if n == 0 {
        return StripLayout {
            mode: Mode::None,
            first: 0,
            end: 0,
            tab_w: 0.0,
            arrows: false,
            more: false,
            width: 0.0,
        };
    }
    let active = active.min(n - 1);
    let gaps = TAB_GAP * (n - 1) as f32;
    if n as f32 * TAB_MIN_W + gaps <= avail {
        let tab_w = ((avail - gaps) / n as f32).min(TAB_MAX_W);
        return StripLayout {
            mode: if tab_w >= TAB_MAX_W {
                Mode::Fit
            } else {
                Mode::Shrink
            },
            first: 0,
            end: n,
            tab_w,
            arrows: false,
            more: false,
            width: n as f32 * tab_w + gaps,
        };
    }
    if avail < MORE_W + TAB_FLOOR_W + TAB_GAP {
        return StripLayout {
            mode: if avail >= MORE_W {
                Mode::MoreOnly
            } else {
                Mode::None
            },
            first: active,
            end: active,
            tab_w: 0.0,
            arrows: false,
            more: avail >= MORE_W,
            width: if avail >= MORE_W { MORE_W } else { 0.0 },
        };
    }
    let arrows = avail >= ARROW_W * 2.0 + MORE_W + TAB_FLOOR_W + TAB_GAP * 3.0;
    let fixed = MORE_W
        + if arrows {
            ARROW_W * 2.0 + TAB_GAP * 2.0
        } else {
            0.0
        };
    let chip_space = (avail - fixed - TAB_GAP).max(0.0);
    let capacity = (((chip_space + TAB_GAP) / (TAB_MIN_W + TAB_GAP)).floor() as usize)
        .max(1)
        .min(n);
    let tab_w = if capacity == 1 {
        chip_space.clamp(TAB_FLOOR_W, TAB_MIN_W)
    } else {
        ((chip_space - TAB_GAP * (capacity - 1) as f32) / capacity as f32).min(TAB_MIN_W)
    };
    let mut first = first.min(n - capacity);
    if active < first {
        first = active;
    }
    if active >= first + capacity {
        first = active + 1 - capacity;
    }
    let end = first + capacity;
    let width = fixed + TAB_GAP + capacity as f32 * tab_w + TAB_GAP * (capacity - 1) as f32;
    StripLayout {
        mode: Mode::Overflow,
        first,
        end,
        tab_w,
        arrows,
        more: true,
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_fit_shrink_and_overflow_at_minimum() {
        let fit = layout(700.0, 3, 0, 0);
        assert_eq!(fit.mode, Mode::Fit);
        assert_eq!(fit.tab_w, TAB_MAX_W);
        let shrink = layout(450.0, 3, 0, 0);
        assert_eq!(shrink.mode, Mode::Shrink);
        assert!(shrink.tab_w >= TAB_MIN_W && shrink.tab_w < TAB_MAX_W);
        let threshold = 3.0 * TAB_MIN_W + 2.0 * TAB_GAP;
        assert_ne!(layout(threshold, 3, 0, 0).mode, Mode::Overflow);
        assert_eq!(layout(threshold - 0.1, 3, 0, 0).mode, Mode::Overflow);
    }

    #[test]
    fn active_is_visible_and_trailing_space_is_removed() {
        let l = layout(350.0, 20, 19, 0);
        assert!(l.active_visible(19));
        assert_eq!(l.end, 20);
        let l = layout(350.0, 5, 4, 19);
        assert!(l.active_visible(4));
        assert_eq!(l.end, 5);
        assert_eq!(layout(0.0, 0, 0, 99).first, 0);
    }

    #[test]
    fn widths_and_modes_hold_across_narrow_and_wide_windows() {
        for n in 0usize..40 {
            for avail in [
                -10.0, 0.0, 20.0, 28.0, 50.0, 81.0, 140.0, 250.0, 600.0, 1200.0,
            ] {
                for active in [0, n.saturating_sub(1)] {
                    let l = layout(avail, n, active, 99);
                    assert!(l.width <= avail.max(0.0) + 0.01, "{n} {avail}: {l:?}");
                    assert!(l.tab_w >= 0.0);
                    assert!(l.end <= n);
                    if l.end > l.first {
                        assert!(l.active_visible(active));
                    }
                }
            }
        }
    }

    #[test]
    fn title_content_reserves_client_shadow_and_border_on_both_sides() {
        let server = title_content_w(
            600.0,
            TitleInsets {
                left: 0.0,
                right: 0.0,
            },
            12.0,
            102.0,
            0.0,
        );
        assert_eq!(server, 486.0);
        let client = title_content_w(
            600.0,
            TitleInsets {
                left: 13.0,
                right: 13.0,
            },
            12.0,
            102.0,
            0.0,
        );
        assert_eq!(client, 460.0);
        let tiled_right = title_content_w(
            600.0,
            TitleInsets {
                left: 13.0,
                right: 0.0,
            },
            12.0,
            102.0,
            12.0,
        );
        assert_eq!(tiled_right, 461.0);
    }
}
