//! Word's Hyphenation dialog (#1265): the Layout tab's Hyphenation Options...,
//! on the settings part's `w:autoHyphenation`, `w:doNotHyphenateCaps`
//! (shown the right way round, as "Hyphenate words in CAPS"),
//! `w:hyphenationZone` (twips, shown in inches; Word's 0.25" when absent) and
//! `w:consecutiveHyphenLimit` (a blank box is "No limit", as is a written 0).
//! OK writes only the controls the person changed, so an untouched OK leaves
//! the package and a clean tab alone (a zone of 361 twips shows as 0.25 and is
//! not rewritten as 360). Like the Automatic/None menu items it has no undo,
//! and docxy's own layout does not hyphenate: the values are kept for Word.
use crate::dialog::catalog;
use crate::dialog::{Control, ControlKind, Dialog, DialogOwner, Value};
use crate::page_setup::{inches, ok_cancel};
use crate::{DocTab, Surface};
use docxcore::package::Package;
use docxcore::sect::TWIPS_PER_INCH;

/// Word's zone when the settings carry none: a quarter inch.
const DEFAULT_ZONE: i32 = 360;

/// The Hyphenation dialog on the document's settings.
pub(crate) fn hyphenation_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let pkg = match (&tab.surface, tab.pkg.as_ref()) {
        (Surface::Doc(_), Some(pkg)) => pkg,
        (Surface::Doc(_), None) => return Err("Hyphenation needs a .docx (not Markdown)".into()),
        _ => return Err("Hyphenation needs a document".into()),
    };
    let mut d = Dialog::message(
        catalog::HYPHENATION,
        "Hyphenation",
        String::new(),
        &[],
        DialogOwner::Hyphenation,
    );
    d.text = None;
    let zone = pkg.hyphenation_zone().unwrap_or(DEFAULT_ZONE).max(0);
    let limit = pkg
        .consecutive_hyphen_limit()
        .filter(|&n| n > 0)
        .map(|n| n.to_string())
        .unwrap_or_default();
    d.controls = vec![
        Control::new(
            "auto",
            "&Automatically hyphenate document",
            ControlKind::Checkbox,
            Value::Bool(pkg.has_auto_hyphenation()),
        ),
        Control::new(
            "caps",
            "Hyphenate words in &CAPS",
            ControlKind::Checkbox,
            Value::Bool(pkg.hyphenate_caps()),
        ),
        Control::new(
            "zone",
            "Hyphenation &zone:",
            ControlKind::Number,
            Value::Text(inches(zone)),
        ),
        Control::new(
            "limit",
            "Limit &consecutive hyphens to (blank: no limit):",
            ControlKind::Number,
            Value::Text(limit),
        ),
    ];
    d.buttons = ok_cancel();
    d.mark_opened();
    Ok(d)
}

fn text_of(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn is_on(d: &Dialog, name: &str) -> bool {
    matches!(d.value(name), Some(Value::Bool(true)))
}

/// Apply an accepted Hyphenation dialog: whether any setting changed. The
/// changed zone and limit are validated before anything is written, so a
/// refused OK leaves the package untouched.
pub(crate) fn apply_hyphenation(pkg: &mut Package, d: &Dialog) -> Result<bool, String> {
    let zone = if d.changed("zone") {
        let inches: f64 = text_of(d, "zone")
            .parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .ok_or("Hyphenation zone takes a number of inches")?;
        if inches < 0.0 {
            return Err("Hyphenation zone cannot be negative".into());
        }
        let tw = (inches * TWIPS_PER_INCH as f64).round();
        if tw > f64::from(i32::MAX) {
            return Err("Hyphenation zone is too large".into());
        }
        Some(tw as i32)
    } else {
        None
    };
    let limit = if d.changed("limit") {
        let text = text_of(d, "limit");
        if text.is_empty() {
            Some(None)
        } else {
            let n: i64 = text.parse().map_err(|_| {
                "Limit consecutive hyphens to takes a whole number, or blank for no limit"
            })?;
            if n < 0 {
                return Err("Limit consecutive hyphens to cannot be negative".into());
            }
            if n > i64::from(i32::MAX) {
                return Err("Limit consecutive hyphens to is too large".into());
            }
            Some((n > 0).then_some(n as i32))
        }
    } else {
        None
    };
    let mut changed = false;
    if d.changed("auto") {
        pkg.set_auto_hyphenation(is_on(d, "auto"));
        changed = true;
    }
    if d.changed("caps") {
        changed |= pkg.set_hyphenate_caps(is_on(d, "caps"));
    }
    if let Some(tw) = zone {
        changed |= pkg.set_hyphenation_zone(Some(tw));
    }
    if let Some(limit) = limit {
        changed |= pkg.set_consecutive_hyphen_limit(limit);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::hyphenation_dialog;
    use crate::DocTab;
    use crate::dialog::catalog;
    use crate::layout_tab::tests::three_sections;
    use ctlcore::json::Json;
    use docxcore::package::{load_package, save_package};

    fn open(t: &mut DocTab) {
        let d = hyphenation_dialog(t).unwrap();
        t.dialogs.push(d);
    }

    fn set(t: &mut DocTab, control: &str, value: Json) {
        t.dialogs
            .set(control, &Json::obj(vec![("value", value)]))
            .unwrap_or_else(|e| panic!("{control}: {e}"));
    }

    fn s(v: &str) -> Json {
        Json::Str(v.into())
    }

    fn ok(t: &mut DocTab) -> Result<(), String> {
        crate::dialog_host::dialog_click(t, "OK")
    }

    fn shown(t: &DocTab, name: &str) -> String {
        t.dialogs
            .top()
            .unwrap()
            .controls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no control {name}"))
            .text()
    }

    fn pkg(t: &mut DocTab) -> &mut docxcore::package::Package {
        t.pkg.as_mut().unwrap()
    }

    /// Criteria 1 and 2: a catalogued dialog; a document without the
    /// settings opens on Word's defaults.
    #[test]
    fn opens_on_word_defaults() {
        let mut t = three_sections();
        open(&mut t);
        let d = t.dialogs.top().unwrap();
        assert_eq!(d.title, "Hyphenation");
        assert_eq!(d.id, catalog::HYPHENATION);
        assert_eq!(shown(&t, "auto"), "unchecked");
        assert_eq!(shown(&t, "caps"), "checked");
        assert_eq!(shown(&t, "zone"), "0.25");
        assert_eq!(shown(&t, "limit"), "");
    }

    /// Criterion 2: the package's values show, a written 0 limit as none.
    #[test]
    fn opens_on_the_saved_values() {
        let mut t = three_sections();
        pkg(&mut t).set_auto_hyphenation(true);
        pkg(&mut t).set_hyphenate_caps(false);
        pkg(&mut t).set_hyphenation_zone(Some(720));
        pkg(&mut t).set_consecutive_hyphen_limit(Some(4));
        open(&mut t);
        assert_eq!(shown(&t, "auto"), "checked");
        assert_eq!(shown(&t, "caps"), "unchecked");
        assert_eq!(shown(&t, "zone"), "0.5");
        assert_eq!(shown(&t, "limit"), "4");
        t.dialogs.pop();
        pkg(&mut t).set_consecutive_hyphen_limit(Some(0));
        open(&mut t);
        assert_eq!(shown(&t, "limit"), "");
    }

    /// Criterion 3: an untouched OK writes nothing, a zone that the inches
    /// display rounds included.
    #[test]
    fn untouched_ok_writes_nothing_and_stays_clean() {
        let mut t = three_sections();
        pkg(&mut t).set_hyphenation_zone(Some(361));
        pkg(&mut t).set_consecutive_hyphen_limit(Some(0));
        t.dirty = false;
        let before = save_package(pkg(&mut t));
        open(&mut t);
        ok(&mut t).unwrap();
        assert!(!t.dirty);
        assert_eq!(save_package(pkg(&mut t)), before);
        assert_eq!(pkg(&mut t).hyphenation_zone(), Some(361));
        assert_eq!(pkg(&mut t).consecutive_hyphen_limit(), Some(0));
    }

    /// Criteria 3 and 6: each changed value is written, the tab turns dirty
    /// and a save reloads with the same values.
    #[test]
    fn ok_writes_each_value_and_survives_a_round_trip() {
        let mut t = three_sections();
        t.dirty = false;
        open(&mut t);
        set(&mut t, "auto", Json::Bool(true));
        set(&mut t, "caps", Json::Bool(false));
        set(&mut t, "zone", s("0.5"));
        set(&mut t, "limit", s("3"));
        ok(&mut t).unwrap();
        assert!(t.dirty);
        assert!(t.dialogs.top().is_none());
        let p = pkg(&mut t);
        assert!(p.has_auto_hyphenation() && !p.hyphenate_caps());
        assert_eq!(p.hyphenation_zone(), Some(720));
        assert_eq!(p.consecutive_hyphen_limit(), Some(3));
        let back = load_package(&save_package(p)).unwrap();
        assert!(back.has_auto_hyphenation() && !back.hyphenate_caps());
        assert_eq!(back.hyphenation_zone(), Some(720));
        assert_eq!(back.consecutive_hyphen_limit(), Some(3));
        // Blank and 0 both mean no limit; caps back on drops its element.
        open(&mut t);
        set(&mut t, "limit", s(""));
        set(&mut t, "caps", Json::Bool(true));
        ok(&mut t).unwrap();
        assert_eq!(pkg(&mut t).consecutive_hyphen_limit(), None);
        assert!(pkg(&mut t).hyphenate_caps());
        pkg(&mut t).set_consecutive_hyphen_limit(Some(2));
        open(&mut t);
        set(&mut t, "limit", s("0"));
        ok(&mut t).unwrap();
        assert_eq!(pkg(&mut t).consecutive_hyphen_limit(), None);
    }

    /// Criterion 5: an invalid zone or limit refuses OK, leaves the dialog
    /// open and the document untouched.
    #[test]
    fn invalid_values_refuse_ok_and_change_nothing() {
        for (name, bad) in [
            ("zone", ""),
            ("zone", "-1"),
            ("zone", "9999999999"),
            ("limit", "-1"),
            ("limit", "1.5"),
            ("limit", "99999999999"),
        ] {
            let mut t = three_sections();
            t.dirty = false;
            let before = save_package(pkg(&mut t));
            open(&mut t);
            // A valid change beside it must not be half-applied.
            set(&mut t, "auto", Json::Bool(true));
            set(&mut t, name, s(bad));
            assert!(ok(&mut t).is_err(), "{name} {bad:?}");
            assert!(t.dialogs.top().is_some(), "{name} {bad:?} left it open");
            assert!(!t.dirty, "{name} {bad:?}");
            assert_eq!(save_package(pkg(&mut t)), before, "{name} {bad:?}");
        }
    }
}
