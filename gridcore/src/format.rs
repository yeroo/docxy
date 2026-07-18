//! Cell-format patches: pure translation between the `cell.format` wire
//! shape (`numFmt`/`bold`/`italic`/`fontColor`/`fillColor`/`align` key-value
//! pairs) and [`crate::sheet::Xf`] — shared by every host that exposes an
//! agent-facing format verb (xlsxy's terminal `control.rs`, gridwasm's
//! `grid_ctl`) so the wire vocabulary, validation, and the `Xf`-to-wire
//! read-back mapping live in exactly one place.
//!
//! Deliberately JSON-free, per the crate's headless-first rule: a host
//! builds `&[(String, String)]` wire pairs from its own JSON/argument type
//! and hands them to [`FormatPatch::parse`]; the reverse mapping
//! ([`xf_format_fields`]) returns typed [`FormatValue`]s that a host
//! serializes into its own JSON for `cell.get`-style read-back.

use crate::sheet::{Align, NumFmt, Xf, classify_format_code};

/// A `cell.format` patch: each field `Some` means the wire patch set that
/// key; `None` means the key was absent from the patch, so that aspect of
/// the cell's existing style is left untouched by [`apply_patch_to_xf`].
/// Field types match the corresponding [`Xf`] field exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FormatPatch {
    /// A raw number-format code, pre-validated by
    /// [`crate::numfmt::parse_format`].
    pub num_fmt: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub font_color: Option<(u8, u8, u8)>,
    pub fill_color: Option<(u8, u8, u8)>,
    pub align: Option<Align>,
}

impl FormatPatch {
    /// Parse wire key/value pairs into a [`FormatPatch`].
    ///
    /// Errors (all `String`, meant to surface to the agent verbatim):
    /// - no pairs at all → `"patch needs at least one key"`
    /// - an unrecognized key → names the key
    /// - a `numFmt` code [`crate::numfmt::parse_format`] rejects
    /// - a `bold`/`italic` value that isn't `"true"`/`"false"`
    /// - a color that isn't `"#RRGGBB"`
    /// - an `align` that isn't `left`/`center`/`right`
    pub fn parse(pairs: &[(String, String)]) -> Result<FormatPatch, String> {
        if pairs.is_empty() {
            return Err("patch needs at least one key".to_string());
        }
        let mut patch = FormatPatch::default();
        for (key, value) in pairs {
            match key.as_str() {
                "numFmt" => {
                    if crate::numfmt::parse_format(value).is_none() {
                        return Err(format!("bad numFmt code '{value}'"));
                    }
                    patch.num_fmt = Some(value.clone());
                }
                "bold" => patch.bold = Some(parse_bool("bold", value)?),
                "italic" => patch.italic = Some(parse_bool("italic", value)?),
                "fontColor" => patch.font_color = Some(parse_hex_color(value)?),
                "fillColor" => patch.fill_color = Some(parse_hex_color(value)?),
                "align" => patch.align = Some(parse_align(value)?),
                other => return Err(format!("unknown patch key '{other}'")),
            }
        }
        Ok(patch)
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("'{key}' must be true or false, got '{other}'")),
    }
}

/// Parse `"#RRGGBB"` (case-insensitive hex digits) into the same `(r, g, b)`
/// triple [`Xf::color`]/[`Xf::fill`] store. The TUI's own font/fill color
/// pickers choose from a fixed named palette (see xlsxy's `COLOR_OPTIONS`)
/// rather than parsing free-form hex, so there is no picker-level string
/// parser to reuse — this matches their `(u8, u8, u8)` storage exactly,
/// which is the part that actually needs to line up.
fn parse_hex_color(s: &str) -> Result<(u8, u8, u8), String> {
    let bad = || format!("bad color '{s}' (want \"#RRGGBB\")");
    let hex = s.strip_prefix('#').ok_or_else(bad)?;
    if hex.len() != 6 || !hex.is_ascii() {
        return Err(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match (byte(0), byte(2), byte(4)) {
        (Some(r), Some(g), Some(b)) => Ok((r, g, b)),
        _ => Err(bad()),
    }
}

fn parse_align(s: &str) -> Result<Align, String> {
    match s {
        "left" => Ok(Align::Left),
        "center" => Ok(Align::Center),
        "right" => Ok(Align::Right),
        other => Err(format!("bad align '{other}' (want left/center/right)")),
    }
}

/// Apply `patch` over `base`, returning the resulting [`Xf`]; keys absent
/// from the patch keep `base`'s value for that aspect. Mirrors the TUI's own
/// `apply_format`/`apply_picker` mutation exactly: `numFmt` sets both `code`
/// and the [`classify_format_code`] classification together, same as the
/// number-format picker.
pub fn apply_patch_to_xf(base: &Xf, patch: &FormatPatch) -> Xf {
    let mut xf = base.clone();
    if let Some(code) = &patch.num_fmt {
        xf.numfmt = classify_format_code(code);
        xf.code = Some(code.clone());
    }
    if let Some(b) = patch.bold {
        xf.bold = b;
    }
    if let Some(i) = patch.italic {
        xf.italic = i;
    }
    if let Some(c) = patch.font_color {
        xf.color = Some(c);
    }
    if let Some(c) = patch.fill_color {
        xf.fill = Some(c);
    }
    if let Some(a) = patch.align {
        xf.align = a;
    }
    xf
}

/// One field of a `cell.format`-shaped read-back, typed per its wire kind so
/// a host can serialize it into its own JSON type without gridcore knowing
/// JSON exists.
#[derive(Clone, Debug, PartialEq)]
pub enum FormatValue {
    Str(String),
    Bool(bool),
}

/// The reverse of [`FormatPatch::parse`] / [`apply_patch_to_xf`]: every wire
/// key whose value in `xf` differs from the workbook's default rendering, in
/// patch order (`numFmt`, `bold`, `italic`, `fontColor`, `fillColor`,
/// `align`) — the `cell.get` `format` read-back shape. An unstyled `Xf`
/// yields an empty vec, i.e. no `format` key on the wire.
///
/// `bold`/`italic`/`fontColor`/`fillColor`/`align` are compared against
/// [`Xf::default`] (`false`/`false`/`None`/`None`/`Align::General`): nothing
/// in `crate::xlsx`'s `<cellXfs>` parser synthesizes a non-default value for
/// these five when the XML doesn't specify one — a `<font>` with no `<b/>`/
/// `<i/>`/`<color>` element, an unset/`"none"` fill, and a missing
/// `horizontal` attribute all parse to exactly their `Xf::default()` value,
/// loaded or not.
///
/// `numFmt` is different, and handled by its OWN rule rather than a
/// `Xf::code`/`Xf::default().code` comparison: `crate::xlsx`'s parser
/// SYNTHESIZES `Xf::code = Some("General")` for the extremely common
/// `numFmtId="0"` (via `crate::numfmt::builtin_code(0)`), purely so
/// `save_xlsx` can round-trip the numFmtId reference byte-faithfully — that
/// is a save-fidelity concern, not a sign the cell is "styled". Comparing
/// `code` against `Xf::default().code == None` would therefore treat EVERY
/// loaded workbook's untouched cells (and every `cell.format` patch that
/// didn't touch `numFmt`, since [`apply_patch_to_xf`] clones the base
/// `code`) as carrying an explicit numFmt, which they don't. The correct
/// rule instead keys off [`Xf::numfmt`]'s CLASSIFICATION: a code is only
/// wire-representable/meaningful when it classifies as something other than
/// [`NumFmt::General`] (`classify_format_code` maps `"General"`/`"general"`/
/// an empty code to `General` too, so this also makes an agent's own
/// `numFmt:"General"` patch — a no-op deliberate reset — correctly echo
/// nothing afterward, matching how the other five fields already behave
/// when explicitly reset to their default). `numFmt` still only round-trips
/// when [`Xf::code`] carries the raw format string (as it does for anything
/// [`apply_patch_to_xf`] itself wrote, or that the loader mapped via
/// `crate::numfmt::builtin_code`); a cell whose non-General `numfmt`
/// classification came from a built-in format id with no stored code string
/// (outside `builtin_code`'s table) has no wire-representable `numFmt` and
/// is silently skipped — a known, narrower, pre-existing gap, unrelated to
/// this rule.
pub fn xf_format_fields(xf: &Xf) -> Vec<(&'static str, FormatValue)> {
    let default = Xf::default();
    let mut out = Vec::new();
    if xf.numfmt != NumFmt::General {
        if let Some(code) = &xf.code {
            out.push(("numFmt", FormatValue::Str(code.clone())));
        }
    }
    if xf.bold != default.bold {
        out.push(("bold", FormatValue::Bool(xf.bold)));
    }
    if xf.italic != default.italic {
        out.push(("italic", FormatValue::Bool(xf.italic)));
    }
    if xf.color != default.color {
        if let Some(c) = xf.color {
            out.push(("fontColor", FormatValue::Str(hex_color(c))));
        }
    }
    if xf.fill != default.fill {
        if let Some(c) = xf.fill {
            out.push(("fillColor", FormatValue::Str(hex_color(c))));
        }
    }
    if xf.align != default.align {
        let wire = match xf.align {
            Align::Left => Some("left"),
            Align::Center => Some("center"),
            Align::Right => Some("right"),
            Align::General => None,
        };
        if let Some(wire) = wire {
            out.push(("align", FormatValue::Str(wire.to_string())));
        }
    }
    out
}

/// `(r, g, b)` → `"#RRGGBB"` (uppercase hex digits).
fn hex_color((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_patch_errors() {
        let err = FormatPatch::parse(&[]).unwrap_err();
        assert_eq!(err, "patch needs at least one key");
    }

    #[test]
    fn unknown_key_names_itself() {
        let err = FormatPatch::parse(&[("wrap".to_string(), "true".to_string())]).unwrap_err();
        assert!(err.contains("wrap"), "{err}");
    }

    #[test]
    fn bad_num_fmt_is_rejected() {
        let err = FormatPatch::parse(&[("numFmt".to_string(), "[[[".to_string())]).unwrap_err();
        assert!(err.contains("numFmt") || err.contains("[[["), "{err}");
    }

    #[test]
    fn bad_bool_is_rejected() {
        let err = FormatPatch::parse(&[("bold".to_string(), "yes".to_string())]).unwrap_err();
        assert!(err.contains("bold"), "{err}");
    }

    #[test]
    fn bad_color_forms_are_rejected() {
        for bad in ["FF0000", "#FF00", "#GGGGGG", "red"] {
            let err =
                FormatPatch::parse(&[("fontColor".to_string(), bad.to_string())]).unwrap_err();
            assert!(err.contains("color"), "input '{bad}': {err}");
        }
    }

    #[test]
    fn bad_align_is_rejected() {
        let err = FormatPatch::parse(&[("align".to_string(), "middle".to_string())]).unwrap_err();
        assert!(err.contains("middle"), "{err}");
    }

    #[test]
    fn parses_every_key_and_applies_over_a_base_xf() {
        let pairs = vec![
            ("numFmt".to_string(), "0.00%".to_string()),
            ("bold".to_string(), "true".to_string()),
            ("italic".to_string(), "false".to_string()),
            ("fontColor".to_string(), "#ff0000".to_string()),
            ("fillColor".to_string(), "#00FF00".to_string()),
            ("align".to_string(), "center".to_string()),
        ];
        let patch = FormatPatch::parse(&pairs).unwrap();
        let xf = apply_patch_to_xf(&Xf::default(), &patch);
        assert!(xf.bold);
        assert!(!xf.italic);
        assert_eq!(xf.color, Some((255, 0, 0)));
        assert_eq!(xf.fill, Some((0, 255, 0)));
        assert_eq!(xf.align, Align::Center);
        assert_eq!(xf.code.as_deref(), Some("0.00%"));
    }

    #[test]
    fn keys_absent_from_the_patch_leave_the_base_untouched() {
        let base = Xf {
            bold: true,
            color: Some((1, 2, 3)),
            ..Xf::default()
        };
        let patch = FormatPatch::parse(&[("italic".to_string(), "true".to_string())]).unwrap();
        let xf = apply_patch_to_xf(&base, &patch);
        assert!(xf.bold, "untouched by the patch");
        assert_eq!(xf.color, Some((1, 2, 3)), "untouched by the patch");
        assert!(xf.italic, "set by the patch");
    }

    #[test]
    fn xf_format_fields_empty_for_default_xf() {
        assert!(xf_format_fields(&Xf::default()).is_empty());
    }

    #[test]
    fn xf_format_fields_round_trips_a_patch() {
        let pairs = vec![
            ("bold".to_string(), "true".to_string()),
            ("fontColor".to_string(), "#123ABC".to_string()),
            ("align".to_string(), "right".to_string()),
        ];
        let patch = FormatPatch::parse(&pairs).unwrap();
        let xf = apply_patch_to_xf(&Xf::default(), &patch);
        let fields = xf_format_fields(&xf);
        assert_eq!(
            fields,
            vec![
                ("bold", FormatValue::Bool(true)),
                ("fontColor", FormatValue::Str("#123ABC".to_string())),
                ("align", FormatValue::Str("right".to_string())),
            ]
        );
    }

    #[test]
    fn xf_format_fields_treats_an_explicit_general_numfmt_patch_as_a_no_op_echo() {
        // An agent deliberately resetting numFmt to General is, semantically,
        // resetting to the default — it should echo nothing afterward, same
        // as any of the other five fields explicitly reset to their default.
        let patch = FormatPatch::parse(&[("numFmt".to_string(), "General".to_string())]).unwrap();
        let xf = apply_patch_to_xf(&Xf::default(), &patch);
        assert!(
            xf_format_fields(&xf).is_empty(),
            "explicit numFmt:General must echo nothing: {xf:?}"
        );
    }

    /// The bug this test pins (see `xf_format_fields`'s doc comment):
    /// `crate::xlsx`'s `<cellXfs>` parser synthesizes `Xf::code =
    /// Some("General")` for `numFmtId="0"` (round-trip fidelity), which
    /// differs from `Xf::default().code == None`. Comparing raw `code`
    /// against `Xf::default()` would spuriously echo `numFmt:"General"` for
    /// every untouched cell of EVERY real loaded `.xlsx` (not just this
    /// crate's own in-memory `new_xlsx()`, which never round-trips and so
    /// never surfaced this).
    #[test]
    fn xf_format_fields_is_empty_for_a_loaded_workbooks_untouched_default_style() {
        let pkg = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&crate::xlsx::new_xlsx()))
            .expect("round trip");
        let default_style = pkg.workbook.styles.xf(0);
        assert_eq!(
            default_style.code.as_deref(),
            Some("General"),
            "sanity check: confirms the loader really does synthesize this code"
        );
        assert!(
            xf_format_fields(&default_style).is_empty(),
            "an untouched cell's style, read back from a REAL loaded workbook, \
             must yield no format fields: {default_style:?}"
        );
    }

    #[test]
    fn xf_format_fields_still_echoes_a_genuinely_non_general_numfmt() {
        // Only the General/builtin-id-0 case is treated as absent — a
        // genuinely different number format (e.g. a loaded date format)
        // must still echo its code.
        let xf = Xf {
            numfmt: crate::sheet::NumFmt::Date,
            code: Some("m/d/yyyy".to_string()),
            ..Xf::default()
        };
        assert_eq!(
            xf_format_fields(&xf),
            vec![("numFmt", FormatValue::Str("m/d/yyyy".to_string()))]
        );
    }

    #[test]
    fn xf_format_fields_a_bold_only_patch_over_a_loaded_default_style_does_not_leak_general() {
        let pkg = crate::xlsx::load_xlsx(&crate::xlsx::save_xlsx(&crate::xlsx::new_xlsx()))
            .expect("round trip");
        let base = pkg.workbook.styles.xf(0);
        let patch = FormatPatch::parse(&[("bold".to_string(), "true".to_string())]).unwrap();
        let xf = apply_patch_to_xf(&base, &patch);
        assert_eq!(
            xf_format_fields(&xf),
            vec![("bold", FormatValue::Bool(true))],
            "a patch that never touches numFmt must not inherit the loaded \
             default style's synthesized 'General' code: {xf:?}"
        );
    }
}
