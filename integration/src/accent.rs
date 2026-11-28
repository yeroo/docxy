//! Which of a plugin's accent colours (`branding`'s `accents`) a host may
//! draw: WCAG 2.x relative luminance and contrast ratio, and the rule each
//! role is held to (`docs/integration-plugins.md`).

use crate::protocol::Rgb;

/// WCAG AA's contrast for text.
pub const MIN_CONTRAST: f64 = 4.5;

pub const BLACK: Rgb = Rgb(0, 0, 0);
pub const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);

/// What a host draws an accent colour for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A fill with text on it: primary buttons, the selection.
    Primary,
    /// Text on the background.
    Link,
    /// A small mark beside text on the background.
    Highlight,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Primary => "primary",
            Role::Link => "link",
            Role::Highlight => "highlight",
        }
    }
}

/// A colour a host may draw for its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accepted {
    pub color: Rgb,
    /// For [`Role::Primary`], the text drawn on the fill: black or white,
    /// whichever has the higher contrast. `None` for the other roles.
    pub text: Option<Rgb>,
}

/// WCAG 2.x relative luminance of `c`, from 0 (black) to 1 (white).
pub fn relative_luminance(c: Rgb) -> f64 {
    let linear = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2)
}

/// WCAG 2.x contrast ratio of `a` and `b`, from 1 to 21, in either order.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (light, dark) = if la >= lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// `color` for `role` on a theme whose background is `background`, or why
/// the host keeps its own colour. A primary is always taken: the better of
/// black and white text on it always reaches [`MIN_CONTRAST`].
pub fn check(role: Role, color: Rgb, background: Rgb) -> Result<Accepted, String> {
    match role {
        Role::Primary => {
            let (on_black, on_white) = (contrast_ratio(color, BLACK), contrast_ratio(color, WHITE));
            let (text, ratio) = if on_black >= on_white {
                (BLACK, on_black)
            } else {
                (WHITE, on_white)
            };
            if ratio >= MIN_CONTRAST {
                Ok(Accepted {
                    color,
                    text: Some(text),
                })
            } else {
                Err(format!(
                    "the primary accent {} gives its text at most {ratio:.2}:1, below {MIN_CONTRAST}:1",
                    color.to_hex()
                ))
            }
        }
        Role::Link | Role::Highlight => {
            let ratio = contrast_ratio(color, background);
            if ratio >= MIN_CONTRAST {
                Ok(Accepted { color, text: None })
            } else {
                Err(format!(
                    "the {} accent {} is {ratio:.2}:1 on the background {}, below {MIN_CONTRAST}:1",
                    role.as_str(),
                    color.to_hex(),
                    background.to_hex()
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_and_contrast_follow_wcag() {
        assert_eq!(relative_luminance(BLACK), 0.0);
        assert!((relative_luminance(WHITE) - 1.0).abs() < 1e-12);
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 1e-9);
        assert_eq!(contrast_ratio(WHITE, BLACK), contrast_ratio(BLACK, WHITE));
        // WCAG's own examples: #767676 is the lightest grey that holds 4.5:1
        // on white, #777777 the first that does not.
        let grey = |v| Rgb(v, v, v);
        assert!((contrast_ratio(grey(0x76), WHITE) - 4.54).abs() < 0.01);
        assert!((contrast_ratio(grey(0x77), WHITE) - 4.48).abs() < 0.01);
        // The channels weigh 0.2126, 0.7152 and 0.0722.
        assert!((relative_luminance(Rgb(0xFF, 0, 0)) - 0.2126).abs() < 1e-12);
        assert!((relative_luminance(Rgb(0, 0xFF, 0)) - 0.7152).abs() < 1e-12);
        assert!((relative_luminance(Rgb(0, 0, 0xFF)) - 0.0722).abs() < 1e-12);
        // Below the linear segment's end (10/255) and above it (11/255).
        assert!((relative_luminance(grey(10)) - 10.0 / 255.0 / 12.92).abs() < 1e-12);
        let s: f64 = 11.0 / 255.0;
        let above = ((s + 0.055) / 1.055).powf(2.4);
        assert!((relative_luminance(grey(11)) - above).abs() < 1e-12);
    }

    #[test]
    fn a_link_or_a_highlight_needs_4_5_to_1_on_the_background() {
        let grey = |v| Rgb(v, v, v);
        for role in [Role::Link, Role::Highlight] {
            assert_eq!(
                check(role, grey(0x76), WHITE),
                Ok(Accepted {
                    color: grey(0x76),
                    text: None
                })
            );
            let refused = check(role, grey(0x77), WHITE).unwrap_err();
            assert!(refused.contains(role.as_str()), "{refused}");
            assert!(refused.contains("#777777"), "{refused}");
            assert!(refused.contains("4.48:1"), "{refused}");
            // The same colour against a dark background.
            assert!(check(role, grey(0x77), BLACK).is_ok());
            assert!(check(role, grey(0x40), BLACK).is_err());
        }
    }

    #[test]
    fn a_primary_fill_takes_the_text_with_the_higher_contrast() {
        let fill = |c| check(Role::Primary, c, WHITE).map(|a| a.text);
        assert_eq!(fill(Rgb(0x10, 0x20, 0x80)), Ok(Some(WHITE)));
        assert_eq!(fill(Rgb(0xF0, 0xE0, 0x40)), Ok(Some(BLACK)));
        // Each side of the point where black and white text tie.
        assert_eq!(fill(Rgb(0x75, 0x75, 0x75)), Ok(Some(WHITE)));
        assert_eq!(fill(Rgb(0x76, 0x76, 0x76)), Ok(Some(BLACK)));
        // The background does not matter for a fill.
        assert_eq!(
            check(Role::Primary, Rgb(0x10, 0x20, 0x80), Rgb(0x10, 0x20, 0x80)),
            check(Role::Primary, Rgb(0x10, 0x20, 0x80), WHITE)
        );
    }

    /// The two ratios of a colour with black and with white multiply to 21,
    /// so the higher is at least its square root, 4.58: every fill has a text
    /// colour, and the primary's refusal cannot happen.
    #[test]
    fn every_fill_has_a_text_colour() {
        for r in (0..=255).step_by(5) {
            for g in (0..=255).step_by(5) {
                for b in (0..=255).step_by(5) {
                    let c = Rgb(r, g, b);
                    let product = contrast_ratio(c, BLACK) * contrast_ratio(c, WHITE);
                    assert!((product - 21.0).abs() < 1e-9, "{c:?}");
                    assert!(check(Role::Primary, c, WHITE).is_ok(), "{c:?}");
                }
            }
        }
    }
}
