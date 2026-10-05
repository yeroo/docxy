//! AutoFilter: the criteria of each filtered column, read from and written
//! back to `<filterColumn>`, and their evaluation against the cells. The
//! commands that turn a filter on, change it, reapply, clear it and run an
//! Advanced Filter are in [`apply`]; the drop-down's model is in [`menu`].
//!
//! A filter is not live: rows are hidden or shown only when a command
//! applies it, never when a cell changes (Excel's behaviour).

use opccore::xml::{Event, XmlParser};

use crate::sheet::{CellValue, Dxf, Sheet, Styles};

mod advanced;
mod apply;
mod dates;
mod menu;

pub use advanced::{ADVANCED_OTHER_SHEET, AdvancedFilter, advanced};
pub(crate) use apply::shown_text;
pub use apply::{
    ByCell, FilterError, FilterOutcome, auto_filter_off, auto_filter_on, auto_filter_on_range,
    clear, filter_by_cell, reapply, search, set_criterion, status_text,
};
pub use dates::DYNAMIC_KINDS;
pub use menu::{FilterMenu, MENU_LIMIT, MenuItem, Submenu, menu};

/// Parse a filter criteria into `(operator, operand)`: ">500", "<=100", "<>X",
/// "=Laptop", or a plain value. Unlike the CF parser, the default operator is
/// `equal` (picking a value is the common filter case).
pub fn parse(s: &str) -> Option<(&'static str, String)> {
    let s = s.trim();
    let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
        ("greaterThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<=") {
        ("lessThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<>") {
        ("notEqual", r)
    } else if let Some(r) = s.strip_prefix('>') {
        ("greaterThan", r)
    } else if let Some(r) = s.strip_prefix('<') {
        ("lessThan", r)
    } else if let Some(r) = s.strip_prefix('=') {
        ("equal", r)
    } else {
        ("equal", s)
    };
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some((op, rest.to_string()))
    }
}

/// Whether a cell value satisfies `(op, operand)`. Numbers compare numerically
/// (when the operand parses as a number); text compares case-insensitively
/// (equality) or lexicographically (ordering). Blank cells satisfy only
/// `notEqual`.
pub fn matches(value: Option<&CellValue>, op: &str, operand: &str) -> bool {
    match value {
        Some(CellValue::Number(n)) => match operand.parse::<f64>() {
            Ok(o) => match op {
                "greaterThan" => *n > o,
                "greaterThanOrEqual" => *n >= o,
                "lessThan" => *n < o,
                "lessThanOrEqual" => *n <= o,
                "equal" => *n == o,
                "notEqual" => *n != o,
                _ => false,
            },
            // A number cell can't equal a non-numeric operand.
            Err(_) => op == "notEqual",
        },
        Some(CellValue::Text(t)) => {
            let (a, b) = (t.trim().to_lowercase(), operand.trim().to_lowercase());
            match op {
                "equal" => a == b,
                "notEqual" => a != b,
                "greaterThan" => a > b,
                "greaterThanOrEqual" => a >= b,
                "lessThan" => a < b,
                "lessThanOrEqual" => a <= b,
                _ => false,
            }
        }
        Some(CellValue::Bool(v)) => {
            let s = if *v { "true" } else { "false" };
            match op {
                "equal" => s.eq_ignore_ascii_case(operand.trim()),
                "notEqual" => !s.eq_ignore_ascii_case(operand.trim()),
                _ => false,
            }
        }
        _ => op == "notEqual", // blank / empty
    }
}

/// A saved `<autoFilter>` (a worksheet's or a table's): its range and the
/// criteria of each filtered column.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutoFilter {
    /// (r1, c1, r2, c2), 0-based, header row included.
    pub range: (u32, u32, u32, u32),
    /// (column offset from `range`'s left, criteria).
    pub columns: Vec<(u32, ColumnFilter)>,
}

/// One `<dateGroupItem>` of a value checklist: a whole year, a month of it,
/// or a day of that month.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateGroup {
    pub year: i64,
    pub month: Option<u32>,
    pub day: Option<u32>,
}

impl DateGroup {
    /// Whether the date `d` falls in this group.
    pub fn covers(&self, d: &crate::sheet::DateParts) -> bool {
        self.year == d.year
            && self.month.is_none_or(|m| m == d.month)
            && self.day.is_none_or(|x| x == d.day)
    }
}

/// One `<filterColumn>`'s criteria.
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnFilter {
    /// `<filters>`: the checked values (compared with the cell's displayed
    /// text), whether blanks are checked, and the checked date groups
    /// (`<dateGroupItem>`), which a date cell matches by its calendar date.
    Values {
        vals: Vec<String>,
        blank: bool,
        dates: Vec<DateGroup>,
    },
    /// `<customFilters>`: one or two `(operator, value)` conditions, AND'd
    /// when `and`, else OR'd. Begins with / ends with / contains are stored
    /// as Excel stores them, `a*` / `*a` / `*a*`, and "does not contain" as
    /// `notEqual *a*`.
    Custom {
        and: bool,
        conds: Vec<(String, String)>,
    },
    /// `<top10>`: the top (or bottom) `val` items (or percent). `filter_val`
    /// is the cut-off worked out when the filter was applied, so the rows it
    /// shows stay put until it is applied again.
    Top10 {
        top: bool,
        percent: bool,
        val: f64,
        filter_val: Option<f64>,
    },
    /// `<dynamicFilter>`: Above/Below Average (`val` holds the average) or a
    /// date period (`val` / `max_val` hold the window, `[val, max_val)`), both
    /// worked out when applied. `M1`–`M12` and `Q1`–`Q4` need no window: they
    /// match that month or quarter in every year.
    Dynamic {
        kind: String,
        val: Option<f64>,
        max_val: Option<f64>,
    },
    /// `<colorFilter>`: the cell's fill (`cell`) or font colour. `rgb` `None`
    /// is No Fill (or the automatic font colour). `dxf_id` is the `<dxf>` the
    /// file holds it in; `None` until a save gives a new one its own.
    Color {
        cell: bool,
        rgb: Option<(u8, u8, u8)>,
        dxf_id: Option<u32>,
    },
    /// `<iconFilter>`: the conditional-formatting icon (`iconSet`, `iconId`).
    Icon { set: String, id: u32 },
    /// A `<filterColumn>` we don't model (a date group by the hour, a column
    /// with a hidden button, an empty one, an extension), the whole element
    /// as the file had it. It is written back as it is, and never shows or
    /// hides a row on its own.
    Raw(String),
}

impl ColumnFilter {
    /// A [`ColumnFilter::Raw`] column with criteria we can't evaluate (not
    /// one that only hides its button): which rows it hides is unknown.
    pub fn is_opaque(&self) -> bool {
        match self {
            // The start tag, and an end tag unless self-closing: anything
            // more is a child, so a criterion.
            ColumnFilter::Raw(xml) => xml.matches('<').count() > 2,
            _ => false,
        }
    }

    /// A checklist of `vals` (no blanks, no date groups).
    pub fn values(vals: Vec<String>) -> ColumnFilter {
        ColumnFilter::Values {
            vals,
            blank: false,
            dates: Vec::new(),
        }
    }
}

/// The top-level `<autoFilter>` of a worksheet or table part (not one inside
/// a custom sheet view), if it has a range.
pub fn parse_auto_filter(xml: &str, dxfs: &[Dxf]) -> Option<AutoFilter> {
    let mut p = XmlParser::new(xml);
    let mut depth = 0usize;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                let name = p.name().rsplit(':').next().unwrap_or("");
                if depth == 2 && name == "autoFilter" {
                    let range = crate::sheet::parse_range_name(p.attr("ref"))?;
                    let start = p.start_pos();
                    let end = element_end(xml, &mut p)?;
                    let columns = filter_columns(&xml[start..end], dxfs);
                    return Some(AutoFilter { range, columns });
                }
            }
            Event::End => depth = depth.saturating_sub(1),
            Event::Eof => return None,
            Event::Text => {}
        }
    }
}

/// With the parser on an element's start tag, where that element ends.
fn element_end(xml: &str, p: &mut XmlParser) -> Option<usize> {
    if xml[..p.pos()].ends_with("/>") {
        // A self-closing tag still reports its end event.
        if let Event::End = p.next() {
            return Some(p.pos());
        }
        return None;
    }
    let mut depth = 1usize;
    loop {
        match p.next() {
            Event::Start => depth += 1,
            Event::End => {
                depth -= 1;
                if depth == 0 {
                    return Some(p.pos());
                }
            }
            Event::Eof => return None,
            Event::Text => {}
        }
    }
}

/// Each `<filterColumn>` of an `<autoFilter>` element, in document order, as
/// (`colId`, criteria). Every element yields one entry: what we don't model
/// is [`ColumnFilter::Raw`].
pub fn filter_columns(element: &str, dxfs: &[Dxf]) -> Vec<(u32, ColumnFilter)> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(element);
    let mut depth = 0usize;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                if depth == 2 && p.name().rsplit(':').next() == Some("filterColumn") {
                    // `colId` defaults to 0 as the position reader reads it.
                    let id = p.attr("colId").parse().unwrap_or(0);
                    let start = p.start_pos();
                    let Some(end) = element_end(element, &mut p) else {
                        break;
                    };
                    depth -= 1;
                    out.push((id, parse_filter_column(&element[start..end], dxfs)));
                }
            }
            Event::End => depth = depth.saturating_sub(1),
            Event::Eof => break,
            Event::Text => {}
        }
    }
    out
}

/// The criteria one `<filterColumn>` element holds.
pub fn parse_filter_column(element: &str, dxfs: &[Dxf]) -> ColumnFilter {
    parse_modelled(element, dxfs).unwrap_or_else(|| ColumnFilter::Raw(element.to_string()))
}

fn parse_modelled(element: &str, dxfs: &[Dxf]) -> Option<ColumnFilter> {
    let mut p = XmlParser::new(element);
    let mut depth = 0usize;
    let mut out: Option<ColumnFilter> = None;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                let name = p.name().rsplit(':').next().unwrap_or("");
                match (depth, name) {
                    // Only `colId`: a hidden or shown button is not modelled.
                    (1, "filterColumn") => {
                        if p.attrs().iter().any(|a| a.name != "colId") {
                            return None;
                        }
                    }
                    (2, "filters") => {
                        if p.attrs()
                            .iter()
                            .any(|a| !matches!(a.name, "blank" | "calendarType"))
                            || !matches!(p.attr("calendarType"), "" | "gregorian")
                        {
                            return None;
                        }
                        out = Some(ColumnFilter::Values {
                            vals: Vec::new(),
                            blank: flag(p.attr("blank")),
                            dates: Vec::new(),
                        });
                    }
                    (3, "filter") => {
                        let Some(ColumnFilter::Values { vals, .. }) = out.as_mut() else {
                            return None;
                        };
                        let mut v = String::new();
                        XmlParser::append_decoded(p.attr("val"), &mut v);
                        vals.push(v);
                    }
                    (3, "dateGroupItem") => {
                        let Some(ColumnFilter::Values { dates, .. }) = out.as_mut() else {
                            return None;
                        };
                        let num = |a: &str| p.attr(a).parse::<u32>().ok();
                        let year = p.attr("year").parse::<i64>().ok()?;
                        let g = match p.attr("dateTimeGrouping") {
                            "year" => DateGroup {
                                year,
                                month: None,
                                day: None,
                            },
                            "month" => DateGroup {
                                year,
                                month: Some(num("month")?),
                                day: None,
                            },
                            "day" => DateGroup {
                                year,
                                month: Some(num("month")?),
                                day: Some(num("day")?),
                            },
                            // Hours, minutes and seconds are not modelled.
                            _ => return None,
                        };
                        dates.push(g);
                    }
                    (2, "customFilters") => {
                        out = Some(ColumnFilter::Custom {
                            and: flag(p.attr("and")),
                            conds: Vec::new(),
                        });
                    }
                    (3, "customFilter") => {
                        let Some(ColumnFilter::Custom { conds, .. }) = out.as_mut() else {
                            return None;
                        };
                        let op = match p.attr("operator") {
                            "" => "equal".to_string(),
                            o => o.to_string(),
                        };
                        let mut v = String::new();
                        XmlParser::append_decoded(p.attr("val"), &mut v);
                        conds.push((op, v));
                    }
                    (2, "top10") => {
                        out = Some(ColumnFilter::Top10 {
                            top: p.attr("top").is_empty() || flag(p.attr("top")),
                            percent: flag(p.attr("percent")),
                            val: p.attr("val").parse().ok()?,
                            filter_val: p.attr("filterVal").parse().ok(),
                        });
                    }
                    (2, "dynamicFilter") => {
                        let kind = p.attr("type");
                        if !DYNAMIC_KINDS.contains(&kind) {
                            return None;
                        }
                        out = Some(ColumnFilter::Dynamic {
                            kind: kind.to_string(),
                            val: p.attr("val").parse().ok(),
                            max_val: p.attr("maxVal").parse().ok(),
                        });
                    }
                    (2, "colorFilter") => {
                        let id: u32 = p.attr("dxfId").parse().ok()?;
                        let dxf = dxfs.get(id as usize)?;
                        let cell = p.attr("cellColor").is_empty() || flag(p.attr("cellColor"));
                        // A theme or indexed colour isn't one we can match:
                        // the column stays as the file has it.
                        let unresolved = if cell {
                            dxf.fill.is_none() && dxf.fill_unresolved
                        } else {
                            dxf.color.is_none() && dxf.color_unresolved
                        };
                        if unresolved {
                            return None;
                        }
                        out = Some(ColumnFilter::Color {
                            cell,
                            rgb: if cell { dxf.fill } else { dxf.color },
                            dxf_id: Some(id),
                        });
                    }
                    (2, "iconFilter") => {
                        out = Some(ColumnFilter::Icon {
                            set: p.attr("iconSet").to_string(),
                            id: p.attr("iconId").parse().ok()?,
                        });
                    }
                    _ => return None,
                }
            }
            Event::End => depth = depth.saturating_sub(1),
            Event::Eof => break,
            Event::Text => {}
        }
    }
    // Two criteria in one column, or none, is not something we model.
    out
}

fn flag(v: &str) -> bool {
    matches!(v, "1" | "true")
}

/// A number as the file writes it: `5`, not `5.0`.
fn num_attr(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// The `<filterColumn>` element for criteria `f` at `col_id` (offset from the
/// filter's left column). `dxf_id` gives a colour criterion's `<dxf>`; a
/// colour with no id is left out. A [`ColumnFilter::Raw`] element comes back
/// as it was, renumbered.
pub fn filter_column_xml(col_id: u32, f: &ColumnFilter, dxf_id: Option<u32>) -> Option<String> {
    let esc = crate::xlsx::esc_attr;
    let body = match f {
        ColumnFilter::Raw(xml) => return Some(set_col_id(xml, col_id)),
        ColumnFilter::Values { vals, blank, dates } => {
            let mut s = String::from("<filters");
            if *blank {
                s.push_str(" blank=\"1\"");
            }
            if vals.is_empty() && dates.is_empty() {
                s.push_str("/>");
            } else {
                s.push('>');
                for v in vals {
                    s.push_str(&format!("<filter val=\"{}\"/>", esc(v)));
                }
                for g in dates {
                    s.push_str(&format!("<dateGroupItem year=\"{}\"", g.year));
                    if let Some(m) = g.month {
                        s.push_str(&format!(" month=\"{m}\""));
                    }
                    if let Some(d) = g.day {
                        s.push_str(&format!(" day=\"{d}\""));
                    }
                    let grouping = match (g.month, g.day) {
                        (_, Some(_)) => "day",
                        (Some(_), None) => "month",
                        _ => "year",
                    };
                    s.push_str(&format!(" dateTimeGrouping=\"{grouping}\"/>"));
                }
                s.push_str("</filters>");
            }
            s
        }
        ColumnFilter::Custom { and, conds } => {
            let mut s = String::from("<customFilters");
            if *and {
                s.push_str(" and=\"1\"");
            }
            s.push('>');
            for (op, v) in conds {
                s.push_str("<customFilter");
                if op != "equal" {
                    s.push_str(&format!(" operator=\"{}\"", esc(op)));
                }
                s.push_str(&format!(" val=\"{}\"/>", esc(v)));
            }
            s.push_str("</customFilters>");
            s
        }
        ColumnFilter::Top10 {
            top,
            percent,
            val,
            filter_val,
        } => {
            let mut s = String::from("<top10");
            if !*top {
                s.push_str(" top=\"0\"");
            }
            if *percent {
                s.push_str(" percent=\"1\"");
            }
            s.push_str(&format!(" val=\"{}\"", num_attr(*val)));
            if let Some(fv) = filter_val {
                s.push_str(&format!(" filterVal=\"{}\"", num_attr(*fv)));
            }
            s.push_str("/>");
            s
        }
        ColumnFilter::Dynamic { kind, val, max_val } => {
            let mut s = format!("<dynamicFilter type=\"{}\"", esc(kind));
            if let Some(v) = val {
                s.push_str(&format!(" val=\"{}\"", num_attr(*v)));
            }
            if let Some(v) = max_val {
                s.push_str(&format!(" maxVal=\"{}\"", num_attr(*v)));
            }
            s.push_str("/>");
            s
        }
        ColumnFilter::Color { cell, .. } => {
            let id = dxf_id?;
            let mut s = format!("<colorFilter dxfId=\"{id}\"");
            if !*cell {
                s.push_str(" cellColor=\"0\"");
            }
            s.push_str("/>");
            s
        }
        ColumnFilter::Icon { set, id } => {
            format!("<iconFilter iconSet=\"{}\" iconId=\"{id}\"/>", esc(set))
        }
    };
    Some(format!(
        "<filterColumn colId=\"{col_id}\">{body}</filterColumn>"
    ))
}

/// `xml` (a `<filterColumn>` element) with its `colId` set to `id`.
fn set_col_id(xml: &str, id: u32) -> String {
    let gt = xml.find('>').unwrap_or(xml.len());
    let head = &xml[..gt];
    for q in ['"', '\''] {
        let pat = format!("colId={q}");
        if let Some(at) = head.find(&pat) {
            let vs = at + pat.len();
            if let Some(len) = head[vs..].find(q) {
                return format!("{}{id}{}", &xml[..vs], &xml[vs + len..]);
            }
        }
    }
    // No `colId` (it defaults to 0): add one after the name.
    let name_end = head
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(head.len());
    format!("{} colId=\"{id}\"{}", &xml[..name_end], &xml[name_end..])
}

/// The `<dxf>` a colour criterion is saved in: a solid fill, or a font colour
/// (`None`: no fill, or the automatic font colour).
pub fn color_dxf_xml(cell: bool, rgb: Option<(u8, u8, u8)>) -> String {
    match (cell, rgb) {
        (true, Some((r, g, b))) => format!(
            "<dxf><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FF{r:02X}{g:02X}{b:02X}\"/><bgColor rgb=\"FF{r:02X}{g:02X}{b:02X}\"/></patternFill></fill></dxf>"
        ),
        (true, None) => "<dxf><fill><patternFill patternType=\"none\"/></fill></dxf>".to_string(),
        (false, Some((r, g, b))) => {
            format!("<dxf><font><color rgb=\"FF{r:02X}{g:02X}{b:02X}\"/></font></dxf>")
        }
        (false, None) => "<dxf><font><color auto=\"1\"/></font></dxf>".to_string(),
    }
}

/// The rows an applied auto-filter hides, as the file left them: rows below
/// its header that are hidden *and* fail its criteria. A hidden row that
/// passes them was hidden by hand. When a column's criteria can't be checked
/// against the cells alone (a checklist with date groups, Top 10, a dynamic,
/// colour or icon filter, or an opaque [`ColumnFilter::Raw`]), every hidden
/// row in the range counts as filtered.
pub fn filtered_rows(sheet: &Sheet, styles: &Styles, date1904: bool, af: &AutoFilter) -> Vec<u32> {
    let (r1, c1, r2, _) = af.range;
    let unchecked = af.columns.iter().any(|(_, f)| match f {
        ColumnFilter::Values { dates, .. } => !dates.is_empty(),
        ColumnFilter::Custom { .. } => false,
        ColumnFilter::Raw(_) => f.is_opaque(),
        _ => true,
    });
    ((r1 + 1)..=r2)
        .filter(|&r| sheet.row_hidden(r))
        .filter(|&r| {
            unchecked
                || !af.columns.iter().all(|(off, f)| {
                    let cell = sheet.cell(r, c1 + off);
                    let value = cell.map(|c| &c.value);
                    let shown = || {
                        cell.map(|c| {
                            crate::sheet::format_with(&styles.xf(c.style), &c.value, date1904)
                        })
                        .unwrap_or_default()
                    };
                    match f {
                        ColumnFilter::Values { vals, blank, .. } => {
                            values_pass(vals, *blank, value, &shown)
                        }
                        ColumnFilter::Custom { and, conds } => {
                            custom_pass(*and, conds, value, &shown)
                        }
                        _ => true,
                    }
                })
        })
        .collect()
}

/// Whether a cell is blank for filtering: empty, or empty text.
pub(crate) fn is_blank_value(value: Option<&CellValue>) -> bool {
    matches!(value, None | Some(CellValue::Empty))
        || matches!(value, Some(CellValue::Text(t)) if t.is_empty())
}

/// A value checklist without date groups: a blank passes when blanks are
/// checked, anything else when its displayed text is.
fn values_pass(
    vals: &[String],
    blank: bool,
    value: Option<&CellValue>,
    shown: &dyn Fn() -> String,
) -> bool {
    if is_blank_value(value) {
        return blank;
    }
    let text = shown();
    let text = text.trim();
    vals.iter()
        .any(|v| v.trim().to_lowercase() == text.to_lowercase())
}

/// Custom AutoFilter conditions. A number cell compares numerically with a
/// numeric operand (`equals 5` matches a cell showing `5.00`); otherwise the
/// cell's displayed text is compared, case-insensitively, `equal` and
/// `notEqual` through the `*` `?` `~` wildcards.
pub(crate) fn custom_pass(
    and: bool,
    conds: &[(String, String)],
    value: Option<&CellValue>,
    shown: &dyn Fn() -> String,
) -> bool {
    let hit = |(op, val): &(String, String)| cond_pass(op, val, value, shown);
    if and {
        conds.iter().all(hit)
    } else {
        conds.iter().any(hit)
    }
}

fn cond_pass(op: &str, val: &str, value: Option<&CellValue>, shown: &dyn Fn() -> String) -> bool {
    use std::cmp::Ordering;
    let ord_ok = |o: Ordering| match op {
        "equal" => o == Ordering::Equal,
        "notEqual" => o != Ordering::Equal,
        "greaterThan" => o == Ordering::Greater,
        "greaterThanOrEqual" => o != Ordering::Less,
        "lessThan" => o == Ordering::Less,
        "lessThanOrEqual" => o != Ordering::Greater,
        _ => false,
    };
    if let (Some(CellValue::Number(n)), Ok(o)) = (value, val.trim().parse::<f64>()) {
        return n.partial_cmp(&o).is_some_and(ord_ok);
    }
    let blank = is_blank_value(value);
    let text = if blank { String::new() } else { shown() };
    match op {
        "equal" | "notEqual" => {
            let hit = crate::formula::wildcard_match(val, &text);
            hit == (op == "equal")
        }
        // A blank is neither greater nor less than anything.
        _ if blank => false,
        _ => ord_ok(text.to_lowercase().cmp(&val.to_lowercase())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::CellValue;

    #[test]
    fn parse_defaults_to_equal() {
        assert_eq!(parse("Laptop"), Some(("equal", "Laptop".into())));
        assert_eq!(parse(">500"), Some(("greaterThan", "500".into())));
        assert_eq!(parse("<>0"), Some(("notEqual", "0".into())));
        assert_eq!(parse("  "), None);
    }

    #[test]
    fn matches_numbers_and_text() {
        let n = CellValue::Number(700.0);
        assert!(matches(Some(&n), "greaterThan", "500"));
        assert!(!matches(Some(&n), "lessThan", "500"));
        let t = CellValue::Text("Laptop".into());
        assert!(matches(Some(&t), "equal", "laptop"));
        assert!(!matches(Some(&t), "equal", "Dock"));
        assert!(matches(None, "notEqual", "x"));
        assert!(!matches(None, "equal", "x"));
    }

    fn round_trip(f: ColumnFilter) {
        let xml = filter_column_xml(2, &f, Some(0)).unwrap();
        let dxfs = [Dxf {
            fill: Some((0, 176, 80)),
            ..Dxf::default()
        }];
        let back = parse_filter_column(&xml, &dxfs);
        assert_eq!(back, f, "{xml}");
    }

    #[test]
    fn every_modelled_criterion_round_trips() {
        round_trip(ColumnFilter::Values {
            vals: vec!["A & B".into(), "5.00".into()],
            blank: true,
            dates: vec![
                DateGroup {
                    year: 2024,
                    month: Some(3),
                    day: None,
                },
                DateGroup {
                    year: 2023,
                    month: None,
                    day: None,
                },
                DateGroup {
                    year: 2025,
                    month: Some(1),
                    day: Some(9),
                },
            ],
        });
        round_trip(ColumnFilter::values(Vec::new()));
        round_trip(ColumnFilter::Custom {
            and: true,
            conds: vec![
                ("greaterThan".into(), "10".into()),
                ("lessThanOrEqual".into(), "30".into()),
            ],
        });
        round_trip(ColumnFilter::Custom {
            and: false,
            conds: vec![("equal".into(), "a*".into())],
        });
        round_trip(ColumnFilter::Top10 {
            top: false,
            percent: true,
            val: 25.0,
            filter_val: Some(85.5),
        });
        round_trip(ColumnFilter::Dynamic {
            kind: "thisWeek".into(),
            val: Some(45361.0),
            max_val: Some(45368.0),
        });
        round_trip(ColumnFilter::Dynamic {
            kind: "M3".into(),
            val: None,
            max_val: None,
        });
        round_trip(ColumnFilter::Color {
            cell: true,
            rgb: Some((0, 176, 80)),
            dxf_id: Some(0),
        });
        round_trip(ColumnFilter::Icon {
            set: "3Arrows".into(),
            id: 2,
        });
    }

    #[test]
    fn what_we_do_not_model_is_kept_verbatim_and_renumbered() {
        for xml in [
            r#"<filterColumn colId="1" hiddenButton="1"/>"#,
            r#"<filterColumn colId="1"><filters><dateGroupItem year="2024" month="3" day="1" hour="9" dateTimeGrouping="hour"/></filters></filterColumn>"#,
            r#"<filterColumn colId="1"><extLst><ext uri="x"/></extLst></filterColumn>"#,
            r#"<filterColumn colId="1"/>"#,
        ] {
            let f = parse_filter_column(xml, &[]);
            assert_eq!(f, ColumnFilter::Raw(xml.to_string()));
            let out = filter_column_xml(4, &f, None).unwrap();
            assert_eq!(out, xml.replace(r#"colId="1""#, r#"colId="4""#));
        }
    }

    #[test]
    fn wildcards_and_tilde_in_custom_conditions() {
        let t = |s: &str| CellValue::Text(s.into());
        let pass = |op: &str, val: &str, v: &str| {
            let cell = t(v);
            cond_pass(op, val, Some(&cell), &|| v.to_string())
        };
        // `a~*2` is the literal `a*2`.
        assert!(pass("equal", "a~*2", "a*2"));
        assert!(!pass("equal", "a~*2", "ab2"));
        // `?` is any one character; `~?` a literal question mark.
        assert!(pass("equal", "*?*", "AB12"));
        assert!(pass("equal", "*~?*", "B?3"));
        assert!(!pass("equal", "*~?*", "AB12"));
        // `~~` is one tilde, with no other wildcard in the operand.
        assert!(pass("equal", "a~~b", "a~b"));
        assert!(!pass("equal", "a~~b", "a~~b"));
        // Begins with, case-insensitive.
        for v in ["A-1", "a*2", "AB12"] {
            assert!(pass("equal", "a*", v), "{v}");
        }
        assert!(!pass("equal", "a*", "B?3"));
        // Does not contain.
        assert!(pass("notEqual", "*x*", "abc"));
        assert!(!pass("notEqual", "*b*", "abc"));
    }

    #[test]
    fn numbers_compare_by_value_and_text_by_display() {
        let five = CellValue::Number(5.0);
        assert!(cond_pass("equal", "5", Some(&five), &|| "5.00".into()));
        // A non-numeric operand compares the displayed text.
        assert!(cond_pass("equal", "5.0*", Some(&five), &|| "5.00".into()));
        assert!(!cond_pass("greaterThan", "1", None, &String::new));
        assert!(cond_pass("notEqual", "1", None, &String::new));
    }
}
