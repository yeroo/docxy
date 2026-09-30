//! Data › Text to Columns (#692): Excel's Convert Text to Columns Wizard as a
//! form dialog on the sheet tab's [`crate::dialog::DialogStack`], so the
//! harness's `dialog-read`/`dialog-set`/`dialog-tab`/`dialog-click` drive it
//! like any other dialog.
//!
//! The wizard's three steps are its tabs: the data type (delimited or fixed
//! width); the delimiters, Other, consecutive-as-one and the qualifier, or the
//! fixed-width break positions; then each column's format, the destination
//! and the Advanced separators. A preview grid follows every change. Finish
//! converts through [`gridcore::edit::text_to_columns`], first asking Excel's
//! "Do you want to replace the contents of the destination cells?" in a
//! message box on top when that overwrites data: OK converts, Cancel goes
//! back to the wizard.

use crate::dialog::{
    Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Reaction, Value,
};
use crate::{DocTab, Surface};
use gridcore::edit::{TTC_REPLACE, TtcSource, text_to_columns, ttc_would_overwrite};
use gridcore::textio::{ColFormat, DateOrder, Delimiters, SplitKind, TextParse, split_value};

const KINDS: [&str; 2] = ["Delimited", "Fixed width"];
const QUALIFIERS: [&str; 3] = ["\"", "'", "{none}"];
const FORMATS: [&str; 4] = ["General", "Text", "Date", "Do not import column (skip)"];
/// The rows the preview shows.
const PREVIEW_ROWS: usize = 8;
/// The delimiter-step controls, shown for Delimited only.
const DELIMITED: [&str; 8] = [
    "tab",
    "semicolon",
    "comma",
    "space",
    "other",
    "other_char",
    "consecutive",
    "qualifier",
];

fn on_page(mut c: Control, page: usize) -> Control {
    c.page = Some(page);
    c
}

fn check(name: &'static str, label: &str, on: bool, page: usize) -> Control {
    on_page(
        Control::new(name, label, ControlKind::Checkbox, Value::Bool(on)),
        page,
    )
}

fn field(name: &'static str, label: &str, text: &str, page: usize) -> Control {
    on_page(
        Control::new(name, label, ControlKind::Text, Value::Text(text.into())),
        page,
    )
}

fn choice(
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: &[&str],
    page: usize,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(Some(0)));
    c.items = items.iter().map(|s| s.to_string()).collect();
    on_page(c, page)
}

/// The wizard over the sheet's selected column, or why it cannot open.
pub(crate) fn dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Text to Columns needs a spreadsheet".into());
    };
    let src = TtcSource::new(v.active, v.range()).map_err(str::to_string)?;
    let wb = &v.pkg.workbook;
    let sheet = v.sheet();
    let last = sheet.used_size().0.saturating_sub(1).min(src.r2);
    let sample: Vec<Vec<String>> = (src.r1..=last)
        .filter_map(|r| sheet.cell(r, src.col))
        .map(|c| gridcore::sheet::format_with(&wb.styles.xf(c.style), &c.value, wb.date1904))
        .filter(|t| !t.is_empty())
        .take(PREVIEW_ROWS)
        .map(|t| vec![t])
        .collect();
    let mut d = Dialog::message(
        "text-to-columns",
        "Convert Text to Columns Wizard",
        String::new(),
        &[],
        DialogOwner::TextToColumns {
            sheet: src.sheet,
            col: src.col,
            r1: src.r1,
            r2: src.r2,
        },
    );
    d.text = None;
    d.tabs = vec![
        "Step 1: Data type".into(),
        "Step 2: Delimiters".into(),
        "Step 3: Column data format".into(),
    ];
    let mut source = on_page(
        Control::new("source", "Source", ControlKind::Grid, Value::Rows(sample)),
        2,
    );
    source.columns = vec!["Text".into()];
    source.visible = false;
    let mut breaks = field("breaks", "Break lines at:", "", 1);
    breaks.visible = false;
    let mut preview = on_page(
        Control::new(
            "preview",
            "Data preview",
            ControlKind::Grid,
            Value::Rows(Vec::new()),
        ),
        2,
    );
    preview.enabled = false;
    d.controls = vec![
        choice("kind", "Original data type:", ControlKind::Radio, &KINDS, 0),
        check("tab", "&Tab", true, 1),
        check("semicolon", "Se&micolon", false, 1),
        check("comma", "&Comma", false, 1),
        check("space", "&Space", false, 1),
        check("other", "&Other", false, 1),
        field("other_char", "Other delimiter:", "", 1),
        check(
            "consecutive",
            "Treat consecutive delimiters as one",
            false,
            1,
        ),
        choice(
            "qualifier",
            "Text &qualifier:",
            ControlKind::Dropdown,
            &QUALIFIERS,
            1,
        ),
        breaks,
        choice("column", "Column:", ControlKind::Dropdown, &["1"], 2),
        choice(
            "format",
            "Column data format:",
            ControlKind::Radio,
            &FORMATS,
            2,
        ),
        choice(
            "date_order",
            "Date order:",
            ControlKind::Dropdown,
            &DateOrder::ALL.map(DateOrder::name),
            2,
        ),
        field("formats", "Column formats:", "", 2),
        field(
            "destination",
            "&Destination:",
            &gridcore::sheet::cell_name(src.r1, src.col),
            2,
        ),
        field("decimal", "Decimal separator:", ".", 2),
        field("thousands", "Thousands separator:", ",", 2),
        check(
            "trailing_minus",
            "Trailing minus for negative numbers",
            true,
            2,
        ),
        preview,
        source,
    ];
    d.buttons = vec![
        Button {
            default: true,
            ..Button::new("&Finish", ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ];
    d.react = Some(Reaction(react));
    refresh(&mut d);
    d.mark_opened();
    Ok(d)
}

fn index_of(d: &Dialog, name: &str) -> usize {
    d.controls
        .iter()
        .position(|c| c.name == name)
        .expect("a Text to Columns control")
}

fn chosen(d: &Dialog, name: &str) -> usize {
    match d.value(name) {
        Some(Value::Choice(Some(i))) => *i,
        _ => 0,
    }
}

fn is_on(d: &Dialog, name: &str) -> bool {
    matches!(d.value(name), Some(Value::Bool(true)))
}

fn text_of(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn one_char(d: &Dialog, name: &str, what: &str) -> Result<char, String> {
    let t = text_of(d, name);
    let mut cs = t.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(format!("The {what} must be one character.")),
    }
}

/// The column formats as the "Column formats" field lists them
/// (`general, text, date:dmy, skip`).
fn formats(d: &Dialog) -> Result<Vec<ColFormat>, String> {
    let text = text_of(d, "formats");
    let mut entries: Vec<&str> = text.split(',').map(str::trim).collect();
    // Trailing blanks say nothing; a blank in the middle is General, so the
    // columns after it keep their places.
    while entries.last().is_some_and(|s| s.is_empty()) {
        entries.pop();
    }
    entries
        .iter()
        .enumerate()
        .map(|(i, s)| match *s {
            "" => Ok(ColFormat::General),
            s => {
                ColFormat::parse(s).ok_or_else(|| format!("Column {}: unknown format '{s}'", i + 1))
            }
        })
        .collect()
}

/// The options the wizard stages, or what is wrong with them.
pub(crate) fn parse(d: &Dialog) -> Result<TextParse, String> {
    let kind = if chosen(d, "kind") == 1 {
        let breaks = text_of(d, "breaks")
            .split([',', ' '])
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<usize>()
                    .map_err(|_| format!("'{s}' is not a break position"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        SplitKind::Fixed { breaks }
    } else {
        let other = if is_on(d, "other") {
            Some(one_char(d, "other_char", "Other delimiter")?)
        } else {
            None
        };
        SplitKind::Delimited {
            delims: Delimiters {
                tab: is_on(d, "tab"),
                semicolon: is_on(d, "semicolon"),
                comma: is_on(d, "comma"),
                space: is_on(d, "space"),
                other,
            },
            consecutive: is_on(d, "consecutive"),
        }
    };
    let decimal = one_char(d, "decimal", "decimal separator")?;
    let thousands = one_char(d, "thousands", "thousands separator")?;
    if decimal == thousands {
        return Err("The decimal and thousands separators must differ.".into());
    }
    Ok(TextParse {
        kind,
        qualifier: [Some('"'), Some('\''), None][chosen(d, "qualifier").min(2)],
        start_row: 1,
        columns: formats(d)?,
        decimal,
        thousands,
        trailing_minus: is_on(d, "trailing_minus"),
    })
}

/// The Destination cell, as typed (`B1`, `$B$1` or `=B1`).
fn destination(d: &Dialog) -> Result<(u32, u32), String> {
    let t = text_of(d, "destination");
    let cell: String = t.trim().trim_start_matches('=').replace('$', "");
    gridcore::sheet::parse_cell_name(&cell)
        .ok_or_else(|| format!("The destination '{t}' is not a cell reference."))
}

/// Keep the controls in step: the delimiter step shows what the data type
/// needs, the column's format controls follow the chosen column, and the
/// preview follows everything.
fn react(d: &mut Dialog, i: usize, _before: &Value) {
    let name = d.controls[i].name;
    let col = chosen(d, "column");
    let mut list = formats(d).unwrap_or_default();
    match name {
        "format" | "date_order" => {
            let f = match chosen(d, "format") {
                1 => ColFormat::Text,
                2 => ColFormat::Date(DateOrder::ALL[chosen(d, "date_order").min(5)]),
                3 => ColFormat::Skip,
                _ => ColFormat::General,
            };
            if list.len() <= col {
                list.resize(col + 1, ColFormat::General);
            }
            list[col] = f;
            let names: Vec<String> = list.iter().map(|f| f.name()).collect();
            let at = index_of(d, "formats");
            d.controls[at].value = Value::Text(names.join(", "));
        }
        "column" => {
            let f = list.get(col).copied().unwrap_or_default();
            let (fi, oi) = match f {
                ColFormat::General => (0, None),
                ColFormat::Text => (1, None),
                ColFormat::Date(o) => (2, DateOrder::ALL.iter().position(|x| *x == o)),
                ColFormat::Skip => (3, None),
            };
            let at = index_of(d, "format");
            d.controls[at].value = Value::Choice(Some(fi));
            if let Some(oi) = oi {
                let at = index_of(d, "date_order");
                d.controls[at].value = Value::Choice(Some(oi));
            }
        }
        _ => {}
    }
    refresh(d);
}

/// Show the data type's controls and redraw the preview.
fn refresh(d: &mut Dialog) {
    let fixed = chosen(d, "kind") == 1;
    for c in &mut d.controls {
        if DELIMITED.contains(&c.name) {
            c.visible = !fixed;
        }
        if c.name == "breaks" {
            c.visible = fixed;
        }
    }
    let Ok(opts) = parse(d) else {
        return;
    };
    let source = match d.value("source") {
        Some(Value::Rows(rows)) => rows.iter().filter_map(|r| r.first().cloned()).collect(),
        _ => Vec::<String>::new(),
    };
    let rows: Vec<Vec<String>> = source.iter().map(|t| split_value(t, &opts)).collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let heads: Vec<String> = (0..width)
        .map(|c| match opts.column(c) {
            ColFormat::General => "General".to_string(),
            ColFormat::Text => "Text".to_string(),
            ColFormat::Date(o) => o.name().to_string(),
            ColFormat::Skip => "Skip".to_string(),
        })
        .collect();
    let rows = rows
        .into_iter()
        .map(|mut r| {
            r.resize(width, String::new());
            r
        })
        .collect();
    let at = index_of(d, "preview");
    d.controls[at].columns = heads;
    d.controls[at].value = Value::Rows(rows);
    let at = index_of(d, "column");
    d.controls[at].items = (1..=width).map(|c| c.to_string()).collect();
    if chosen(d, "column") >= width {
        d.controls[at].value = Value::Choice(Some(width - 1));
    }
}

fn presses_accept(d: &Dialog, button: &str) -> bool {
    d.buttons.iter().any(|b| {
        b.role == ButtonRole::Accept
            && b.label
                .replace('&', "")
                .trim()
                .eq_ignore_ascii_case(button.trim())
    })
}

/// A button press the wizard handles itself: its Finish, and OK on the
/// replace question over it. `None` for any other press.
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if !presses_accept(top, button) {
        return None;
    }
    match top.owner {
        DialogOwner::TextToColumns { .. } => Some(finish(tab)),
        DialogOwner::TextToColumnsReplace => Some(replace(tab)),
        _ => None,
    }
}

/// The source and options the wizard `d` stages.
fn staged(d: &Dialog) -> Result<(TtcSource, TextParse), String> {
    let DialogOwner::TextToColumns { sheet, col, r1, r2 } = d.owner else {
        return Err("not a Text to Columns dialog".into());
    };
    let opts = parse(d)?;
    let src = TtcSource {
        sheet,
        col,
        r1,
        r2,
        dest: destination(d)?,
    };
    Ok((src, opts))
}

/// Finish: convert, or ask first when data would be replaced.
fn finish(tab: &mut DocTab) -> Result<(), String> {
    let (src, opts) = staged(tab.dialogs.top().ok_or(crate::dialog::NONE_OPEN)?)?;
    let Surface::Sheet(v) = &tab.surface else {
        return Err("Text to Columns needs a spreadsheet".into());
    };
    if ttc_would_overwrite(&v.pkg.workbook, &src, &opts) {
        tab.dialogs.push(Dialog::message(
            "text-to-columns-replace",
            "Text to Columns",
            TTC_REPLACE.into(),
            &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
            DialogOwner::TextToColumnsReplace,
        ));
        return Ok(());
    }
    convert(tab, &src, &opts);
    tab.dialogs.pop();
    Ok(())
}

/// OK to the replace question: convert with the wizard under it, and close
/// both.
fn replace(tab: &mut DocTab) -> Result<(), String> {
    let wizard = tab
        .dialogs
        .under_top()
        .ok_or("the Text to Columns wizard is gone")?;
    let (src, opts) = staged(wizard)?;
    convert(tab, &src, &opts);
    tab.dialogs.pop();
    tab.dialogs.pop();
    Ok(())
}

/// Text to Columns as one undo step.
fn convert(tab: &mut DocTab, src: &TtcSource, opts: &TextParse) {
    let Surface::Sheet(v) = &mut tab.surface else {
        return;
    };
    v.push_undo();
    let today = v.engine.clock;
    let n = text_to_columns(&mut v.pkg.workbook, src, opts, today);
    v.engine = crate::sheet_engine(&v.pkg.workbook);
    tab.dirty = true;
    tab.status = format!(
        "Text to Columns: converted {n} row{}",
        if n == 1 { "" } else { "s" }
    )
    .into();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, new_sheet_surface};
    use ctlcore::json::Json;
    use gridcore::sheet::{Cell, CellValue};

    fn tab(cells: &[(u32, u32, &str)]) -> DocTab {
        let mut surface = new_sheet_surface();
        if let Surface::Sheet(v) = &mut surface {
            for (r, c, t) in cells {
                v.pkg.workbook.sheets[0].set_cell(*r, *c, Cell::text(t));
            }
            v.engine = crate::sheet_engine(&v.pkg.workbook);
        }
        DocTab {
            kind: Kind::Xlsx,
            title: "book.xlsx".into(),
            path: None,
            surface,
            dirty: false,
            status: "".into(),
            comments: vec![],
            pkg: None,
            notes: vec![],
            markdown: false,
            hf_edit: None,
            bundle_html: None,
            load_failed: false,
            dialogs: crate::dialog::DialogStack::default(),
        }
    }

    fn open(t: &mut DocTab, sel: (u32, u32, u32, u32)) {
        if let Surface::Sheet(v) = &mut t.surface {
            v.anchor = (sel.0, sel.1);
            v.sel = (sel.2, sel.3);
        }
        let d = dialog(t).unwrap();
        t.dialogs.push(d);
    }

    fn set(t: &mut DocTab, tab_label: &str, control: &str, value: Json) {
        t.dialogs.select_tab(tab_label).unwrap();
        t.dialogs
            .set(control, &Json::obj(vec![("value", value)]))
            .unwrap();
    }

    fn cell(t: &DocTab, r: u32, c: u32) -> CellValue {
        let Surface::Sheet(v) = &t.surface else {
            panic!("a sheet")
        };
        v.sheet()
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn press(t: &mut DocTab, button: &str) {
        crate::dialog_host::dialog_click(t, button).unwrap();
    }

    /// #692: comma plus qualifier gives four fields; a Text column keeps
    /// the leading zeros; Finish converts and closes.
    #[test]
    fn the_wizard_splits_with_a_qualifier_and_column_formats() {
        let mut t = tab(&[(0, 0, "Pen,4,\"Blue, fine\",0012")]);
        open(&mut t, (0, 0, 0, 0));
        set(&mut t, "Step 2: Delimiters", "tab", Json::Bool(false));
        set(&mut t, "Step 2: Delimiters", "comma", Json::Bool(true));
        set(
            &mut t,
            "Step 3: Column data format",
            "column",
            Json::Str("4".into()),
        );
        set(
            &mut t,
            "Step 3: Column data format",
            "format",
            Json::Str("Text".into()),
        );
        let top = t.dialogs.top().unwrap();
        assert_eq!(
            top.value("formats"),
            Some(&Value::Text("general, general, general, text".into()))
        );
        let Some(Value::Rows(rows)) = top.value("preview") else {
            panic!("a preview")
        };
        assert_eq!(rows[0], ["Pen", "4", "Blue, fine", "0012"]);
        press(&mut t, "Finish");
        assert!(!t.dialogs.is_open());
        assert_eq!(cell(&t, 0, 0), CellValue::Text("Pen".into()));
        assert_eq!(cell(&t, 0, 1), CellValue::Number(4.0));
        assert_eq!(cell(&t, 0, 2), CellValue::Text("Blue, fine".into()));
        assert_eq!(cell(&t, 0, 3), CellValue::Text("0012".into()));
        assert!(t.dirty);
    }

    /// #692: fixed width, a date column, a skipped one and the Advanced
    /// separators.
    #[test]
    fn the_wizard_reads_fixed_width_dates_skips_and_separators() {
        let mut t = tab(&[(0, 0, "03/04/2024xx1.234,5-")]);
        open(&mut t, (0, 0, 0, 0));
        set(
            &mut t,
            "Step 1: Data type",
            "kind",
            Json::Str("Fixed width".into()),
        );
        let top = t.dialogs.top().unwrap();
        assert!(!top.controls[index_of(top, "comma")].visible);
        set(
            &mut t,
            "Step 2: Delimiters",
            "breaks",
            Json::Str("10, 12".into()),
        );
        set(
            &mut t,
            "Step 3: Column data format",
            "formats",
            Json::Str("date:dmy, skip".into()),
        );
        set(
            &mut t,
            "Step 3: Column data format",
            "decimal",
            Json::Str(",".into()),
        );
        set(
            &mut t,
            "Step 3: Column data format",
            "thousands",
            Json::Str(".".into()),
        );
        press(&mut t, "Finish");
        let apr3 = gridcore::sheet::parts_to_serial(2024, 4, 3, 0, false);
        assert_eq!(cell(&t, 0, 0), CellValue::Number(apr3));
        assert_eq!(cell(&t, 0, 1), CellValue::Number(-1234.5));
        assert_eq!(cell(&t, 0, 2), CellValue::Empty);
    }

    /// #692: data in the way asks first; Cancel goes back to the wizard
    /// with nothing changed, OK converts and closes both.
    #[test]
    fn finish_over_data_asks_before_replacing() {
        let mut t = tab(&[(0, 0, "a\tb"), (0, 1, "keep")]);
        open(&mut t, (0, 0, 0, 0));
        press(&mut t, "Finish");
        let top = t.dialogs.top().unwrap();
        assert_eq!(top.id, "text-to-columns-replace");
        assert_eq!(top.text.as_deref(), Some(TTC_REPLACE));
        press(&mut t, "Cancel");
        assert_eq!(t.dialogs.top().unwrap().id, "text-to-columns");
        assert_eq!(cell(&t, 0, 1), CellValue::Text("keep".into()));
        press(&mut t, "Finish");
        press(&mut t, "OK");
        assert!(!t.dialogs.is_open());
        assert_eq!(cell(&t, 0, 0), CellValue::Text("a".into()));
        assert_eq!(cell(&t, 0, 1), CellValue::Text("b".into()));
    }

    /// #692: several delimiters at once, consecutive as one, and a refused
    /// bad setting that leaves the wizard open.
    #[test]
    fn several_delimiters_and_a_bad_setting() {
        let mut t = tab(&[(0, 0, "a;;b c")]);
        open(&mut t, (0, 0, 0, 0));
        set(&mut t, "Step 2: Delimiters", "semicolon", Json::Bool(true));
        set(&mut t, "Step 2: Delimiters", "space", Json::Bool(true));
        set(
            &mut t,
            "Step 2: Delimiters",
            "consecutive",
            Json::Bool(true),
        );
        set(
            &mut t,
            "Step 3: Column data format",
            "decimal",
            Json::Str(",".into()),
        );
        let err = crate::dialog_host::dialog_click(&mut t, "Finish").unwrap_err();
        assert!(err.contains("must differ"), "{err}");
        assert!(t.dialogs.is_open());
        set(
            &mut t,
            "Step 3: Column data format",
            "thousands",
            Json::Str(".".into()),
        );
        press(&mut t, "Finish");
        assert_eq!(cell(&t, 0, 0), CellValue::Text("a".into()));
        assert_eq!(cell(&t, 0, 1), CellValue::Text("b".into()));
        assert_eq!(cell(&t, 0, 2), CellValue::Text("c".into()));
    }

    /// A blank entry in the formats list is General; later columns keep
    /// their places.
    #[test]
    fn a_blank_format_entry_is_general() {
        let mut t = tab(&[(0, 0, "a\t0012\t0034")]);
        open(&mut t, (0, 0, 0, 0));
        set(
            &mut t,
            "Step 3: Column data format",
            "formats",
            Json::Str("general, , text,".into()),
        );
        press(&mut t, "Finish");
        assert_eq!(cell(&t, 0, 1), CellValue::Number(12.0));
        assert_eq!(cell(&t, 0, 2), CellValue::Text("0034".into()));
    }

    #[test]
    fn two_columns_are_refused() {
        let mut t = tab(&[(0, 0, "a,b")]);
        if let Surface::Sheet(v) = &mut t.surface {
            v.anchor = (0, 0);
            v.sel = (0, 1);
        }
        assert_eq!(dialog(&t).unwrap_err(), gridcore::edit::TTC_ONE_COLUMN);
    }
}
