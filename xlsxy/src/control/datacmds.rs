//! The Sort & Filter verbs (#690, #691): `filter.*`, `range.sort`,
//! `sheet.rows` and `wb.clock`. Each mutating verb is one undo step
//! ([`App::filter_command`], [`App::sort_command`]).

use super::{App, Json, sheet_arg};
use crate::datacmd::Area;
use gridcore::edit::{BUILTIN_SORT_LISTS, SORT_WARNING, SortLevel, SortOn, SortOptions};
use gridcore::filter::{AdvancedFilter, ByCell, ColumnFilter, DateGroup, FilterOutcome};
use gridcore::sheet::{Workbook, cell_name, parse_cell_name, parse_col, parse_range_name};

/// `A1:C9` or `B4` (one cell), 0-based.
fn area(s: &str) -> Result<Area, String> {
    let s = s.trim().replace('$', "");
    parse_range_name(&s)
        .or_else(|| parse_cell_name(&s).map(|(r, c)| (r, c, r, c)))
        .ok_or_else(|| format!("bad range '{s}'"))
}

/// `Sheet2!A1:B3` (or a range on `default`) as (sheet, area).
fn qualified(wb: &Workbook, s: &str, default: usize) -> Result<(usize, Area), String> {
    match s.rsplit_once('!') {
        Some((sheet, refs)) => {
            let name = sheet.trim().trim_matches('\'').replace("''", "'");
            let si = wb
                .sheets
                .iter()
                .position(|x| x.name.eq_ignore_ascii_case(&name))
                .ok_or_else(|| format!("no sheet named '{name}'"))?;
            Ok((si, area(refs)?))
        }
        None => Ok((default, area(s)?)),
    }
}

fn area_name((r1, c1, r2, c2): Area) -> String {
    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
}

fn outcome_json(o: &FilterOutcome) -> Json {
    Json::obj(vec![
        ("shown", Json::Num(o.shown as f64)),
        ("total", Json::Num(o.total as f64)),
        ("status", Json::Str(gridcore::filter::status_text(o))),
    ])
}

/// A column of `area` (or of the sheet's filter): its header's text, or a
/// column letter.
fn column_in(wb: &Workbook, si: usize, (r1, c1, _, c2): Area, col: &str) -> Result<u32, String> {
    let s = &wb.sheets[si];
    let by_header = (c1..=c2).find(|&c| {
        s.cell(r1, c).is_some_and(|cl| {
            gridcore::sheet::format_with(&wb.styles.xf(cl.style), &cl.value, wb.date1904)
                .trim()
                .eq_ignore_ascii_case(col.trim())
        })
    });
    by_header
        .or_else(|| {
            // A column letter, whole.
            let col = col.trim().to_ascii_uppercase();
            parse_col(&col)
                .filter(|&(_, n)| n == col.len())
                .map(|(c, _)| c)
        })
        .ok_or_else(|| format!("no column '{col}'"))
}

fn filter_range(wb: &Workbook, si: usize) -> Result<Area, String> {
    wb.sheets[si]
        .auto_filter
        .as_ref()
        .map(|a| a.range)
        .ok_or_else(|| gridcore::filter::FilterError::NoFilter.to_string())
}

/// `"FF00B050"` / `"00B050"` / `null` (No Fill, automatic font).
fn rgb_arg(j: &Json) -> Result<Option<(u8, u8, u8)>, String> {
    let Some(s) = j.as_str() else {
        return match j {
            Json::Null => Ok(None),
            _ => Err("a colour is a hex string like \"FF00B050\", or null".into()),
        };
    };
    let hex = s.trim().trim_start_matches('#');
    let hex = if hex.len() == 8 { &hex[2..] } else { hex };
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("x"), 16);
    match (hex.len(), byte(0), byte(2), byte(4)) {
        (6, Ok(r), Ok(g), Ok(b)) => Ok(Some((r, g, b))),
        _ => Err(format!("bad colour '{s}'")),
    }
}

/// What `filter.set`'s `criteria` asks for.
enum Criteria {
    Set(Option<ColumnFilter>),
    Search { pattern: String, add: bool },
}

fn criteria_arg(j: &Json) -> Result<Criteria, String> {
    if matches!(j, Json::Null) {
        return Ok(Criteria::Set(None));
    }
    let set = |f| Ok(Criteria::Set(Some(f)));
    if let Some(v) = j.get("values") {
        let vals = v
            .as_array()
            .ok_or("'values' is a list")?
            .iter()
            .map(|x| match x {
                Json::Str(s) => s.clone(),
                Json::Num(n) => gridcore::sheet::fmt_general(*n),
                Json::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                _ => String::new(),
            })
            .collect();
        let dates = match j.get("dates").and_then(Json::as_array) {
            Some(ds) => ds
                .iter()
                .map(|d| {
                    let num = |k| d.get(k).and_then(Json::as_i64);
                    Ok(DateGroup {
                        year: num("year").ok_or("a date group needs a 'year'")?,
                        month: num("month").map(|m| m as u32),
                        day: num("day").map(|x| x as u32),
                    })
                })
                .collect::<Result<_, String>>()?,
            None => Vec::new(),
        };
        let blank = j.get("blanks").and_then(Json::as_bool).unwrap_or(false);
        return set(ColumnFilter::Values { vals, blank, dates });
    }
    if let Some(c) = j.get("custom") {
        let and = c.get("and").and_then(Json::as_bool).unwrap_or(false);
        let mut conds = Vec::new();
        for cond in c
            .get("conds")
            .and_then(Json::as_array)
            .ok_or("'custom' needs 'conds'")?
        {
            let pair = cond.as_array().ok_or("a condition is [operator, value]")?;
            let op = pair
                .first()
                .and_then(Json::as_str)
                .ok_or("a condition needs an operator")?;
            let val = match pair.get(1) {
                Some(Json::Str(s)) => s.clone(),
                Some(Json::Num(n)) => gridcore::sheet::fmt_general(*n),
                _ => String::new(),
            };
            conds.push(match op {
                "beginsWith" => ("equal".to_string(), format!("{val}*")),
                "endsWith" => ("equal".to_string(), format!("*{val}")),
                "contains" => ("equal".to_string(), format!("*{val}*")),
                "notContains" => ("notEqual".to_string(), format!("*{val}*")),
                "notBeginsWith" => ("notEqual".to_string(), format!("{val}*")),
                "notEndsWith" => ("notEqual".to_string(), format!("*{val}")),
                "equal" | "notEqual" | "greaterThan" | "greaterThanOrEqual" | "lessThan"
                | "lessThanOrEqual" => (op.to_string(), val),
                o => return Err(format!("unknown operator '{o}'")),
            });
        }
        if conds.is_empty() || conds.len() > 2 {
            return Err("'custom' takes one or two conditions".into());
        }
        return set(ColumnFilter::Custom { and, conds });
    }
    if let Some(t) = j.get("top") {
        let val = t.get("n").and_then(Json::as_f64).ok_or("'top' needs 'n'")?;
        return set(ColumnFilter::Top10 {
            top: !t.get("bottom").and_then(Json::as_bool).unwrap_or(false),
            percent: t.get("percent").and_then(Json::as_bool).unwrap_or(false),
            val,
            filter_val: None,
        });
    }
    if let Some(k) = j.get_str("dynamic") {
        if !gridcore::filter::DYNAMIC_KINDS.contains(&k) {
            return Err(format!("unknown dynamic filter '{k}'"));
        }
        return set(ColumnFilter::Dynamic {
            kind: k.to_string(),
            val: None,
            max_val: None,
        });
    }
    for (key, cell) in [("cellColor", true), ("fontColor", false)] {
        if let Some(c) = j.get(key) {
            return set(ColumnFilter::Color {
                cell,
                rgb: rgb_arg(c)?,
                dxf_id: None,
            });
        }
    }
    if let Some(i) = j.get("icon") {
        return set(ColumnFilter::Icon {
            set: i.get_str("set").ok_or("'icon' needs a 'set'")?.to_string(),
            id: i.get_usize("id").ok_or("'icon' needs an 'id'")? as u32,
        });
    }
    if let Some(p) = j.get_str("search") {
        return Ok(Criteria::Search {
            pattern: p.to_string(),
            add: j.get("add").and_then(Json::as_bool).unwrap_or(false),
        });
    }
    Err(
        "unknown criteria: give values, custom, top, dynamic, cellColor, fontColor, icon or search"
            .into(),
    )
}

/// `filter.set {sheet?, range?, col, criteria}`: turn the filter on over
/// `range` when the sheet's isn't already there, then set (or with `null`
/// clear) column `col`'s criteria and apply the filter.
pub(super) fn filter_set(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let range = args.get_str("range").map(area).transpose()?;
    let col = args
        .get_str("col")
        .ok_or("filter.set needs a 'col'")?
        .to_string();
    let crit = criteria_arg(args.get("criteria").unwrap_or(&Json::Null))?;
    let o = app.filter_command_on(si, |wb, si, today| {
        use gridcore::filter::FilterError;
        let have = wb.sheets[si].auto_filter.as_ref().map(|a| a.range);
        match range {
            Some(r) if have != Some(r) => {
                gridcore::filter::auto_filter_on_range(wb, si, r)?;
            }
            None if have.is_none() => return Err(FilterError::NoFilter),
            _ => {}
        }
        let r = wb.sheets[si]
            .auto_filter
            .as_ref()
            .map(|a| a.range)
            .ok_or(FilterError::NoFilter)?;
        let c = column_in(wb, si, r, &col).map_err(|_| FilterError::NotInFilter)?;
        match crit {
            Criteria::Set(f) => gridcore::filter::set_criterion(wb, si, c, f, today),
            Criteria::Search { pattern, add } => {
                gridcore::filter::search(wb, si, c, &pattern, add, today)
            }
        }
    })?;
    Ok(outcome_json(&o))
}

/// `filter.reapply {sheet?}`.
pub(super) fn filter_reapply(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let o = app.filter_command_on(si, gridcore::filter::reapply)?;
    Ok(outcome_json(&o))
}

/// `filter.clear {sheet?, col?}`: one column's criteria, or (no `col`)
/// every row shown with the filter kept.
pub(super) fn filter_clear(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let col = match args.get_str("col") {
        Some(c) => {
            let r = filter_range(&app.pkg.workbook, si)?;
            Some(column_in(&app.pkg.workbook, si, r, c)?)
        }
        None => None,
    };
    let o = app.filter_command_on(si, |wb, si, today| {
        gridcore::filter::clear(wb, si, col, today)
    })?;
    Ok(outcome_json(&o))
}

/// `filter.off {sheet?}`: no AutoFilter remains; every row of its range
/// shows.
pub(super) fn filter_off(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let range = filter_range(&app.pkg.workbook, si)?;
    app.filter_command_on(si, |wb, si, _| {
        gridcore::filter::auto_filter_off(wb, si);
        let total = range.2.saturating_sub(range.0) as usize;
        Ok(FilterOutcome {
            shown: total,
            total,
        })
    })?;
    app.status = Some("Filter off".into());
    Ok(Json::obj(vec![
        ("off", Json::Bool(true)),
        ("range", Json::Str(area_name(range))),
    ]))
}

/// `filter.menu {sheet?, col, search?}`: the column's drop-down — the typed
/// submenu, the checklist (dates as a tree), whether it is cut short.
pub(super) fn filter_menu(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let wb = &app.pkg.workbook;
    let r = filter_range(wb, si)?;
    let col = column_in(
        wb,
        si,
        r,
        args.get_str("col").ok_or("filter.menu needs a 'col'")?,
    )?;
    let m =
        gridcore::filter::menu(wb, si, col, args.get_str("search")).map_err(|e| e.to_string())?;
    let items = m
        .items
        .iter()
        .map(|i| {
            Json::obj(vec![
                ("label", Json::Str(i.label.clone())),
                ("depth", Json::Num(f64::from(i.depth))),
                ("checked", Json::Bool(i.checked)),
            ])
        })
        .collect();
    Ok(Json::obj(vec![
        ("col", Json::Str(gridcore::sheet::col_name(m.col))),
        ("header", Json::Str(m.header)),
        ("submenu", Json::Str(m.submenu.label().into())),
        ("items", Json::Arr(items)),
        ("truncated", Json::Bool(m.truncated)),
        ("total", Json::Num(m.total as f64)),
        ("filtered", Json::Bool(m.filtered)),
    ]))
}

/// `filter.by-cell {sheet?, ref, by: value|cellColor|fontColor|icon}`.
pub(super) fn filter_by_cell(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let at = super::ref_arg(args)?;
    let by = match args.get_str("by").unwrap_or("value") {
        "value" => ByCell::Value,
        "cellColor" | "cell-color" => ByCell::CellColor,
        "fontColor" | "font-color" => ByCell::FontColor,
        "icon" => ByCell::Icon,
        o => return Err(format!("unknown 'by' '{o}'")),
    };
    let o = app.filter_command_on(si, |wb, si, today| {
        gridcore::filter::filter_by_cell(wb, si, at, by, today)
    })?;
    Ok(outcome_json(&o))
}

/// `filter.advanced {sheet?, list, criteria?, copyTo?, unique?}`: an
/// Advanced Filter in place, or copied (on the list's sheet only).
pub(super) fn filter_advanced(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let wb = &app.pkg.workbook;
    let list = area(
        args.get_str("list")
            .ok_or("filter.advanced needs a 'list'")?,
    )?;
    let criteria = args
        .get_str("criteria")
        .map(|c| qualified(wb, c, si))
        .transpose()?;
    let copy_to = args
        .get_str("copyTo")
        .map(|c| qualified(wb, c, si))
        .transpose()?;
    let a = AdvancedFilter {
        list,
        criteria,
        copy_to,
        unique: args.get("unique").and_then(Json::as_bool).unwrap_or(false),
    };
    let o = app.filter_command_on(si, |wb, si, _| gridcore::filter::advanced(wb, si, &a))?;
    Ok(outcome_json(&o))
}

/// `sheet.rows {sheet?, range}`: each row's visibility, and why a hidden
/// one is hidden (`filter`, or `hand`).
pub(super) fn sheet_rows(app: &App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let (r1, _, r2, _) = area(args.get_str("range").ok_or("sheet.rows needs a 'range'")?)?;
    let s = &app.pkg.workbook.sheets[si];
    let rows = (r1..=r2.min(r1.saturating_add(100_000)))
        .map(|r| {
            let hidden = s.row_hidden(r);
            let by = if !hidden {
                Json::Null
            } else if s.row_filtered(r) {
                Json::Str("filter".into())
            } else {
                Json::Str("hand".into())
            };
            Json::obj(vec![
                ("row", Json::Num(f64::from(r + 1))),
                ("hidden", Json::Bool(hidden)),
                ("hiddenBy", by),
            ])
        })
        .collect();
    Ok(Json::obj(vec![("rows", Json::Arr(rows))]))
}

/// A `range.sort` key's level.
fn level_arg(
    wb: &Workbook,
    si: usize,
    range: Area,
    ltr: bool,
    k: &Json,
) -> Result<SortLevel, String> {
    let key = if ltr {
        let row = k.get("row").ok_or("a left-to-right key needs a 'row'")?;
        match row {
            Json::Num(_) => (row.as_usize().ok_or("bad 'row'")? as u32)
                .checked_sub(1)
                .ok_or("rows count from 1")?,
            _ => return Err("'row' is a row number".into()),
        }
    } else {
        let col = k.get_str("col").ok_or("a key needs a 'col'")?;
        column_in(wb, si, range, col)?
    };
    let top = k.get_str("position").unwrap_or("top") != "bottom";
    let on = match k.get_str("on").unwrap_or("value") {
        "value" => {
            // `asc`, `desc`, or `list:<items>` (a custom list: comma-separated,
            // or `days`, `weekdays`, `months`, `monthnames`), reversed by
            // `direction:"desc"`.
            let order = k.get_str("order").unwrap_or("asc");
            let builtin = |i: usize| {
                BUILTIN_SORT_LISTS[i]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            };
            let list = order.strip_prefix("list:").map(|items| match items.trim() {
                "days" => builtin(0),
                "weekdays" => builtin(1),
                "months" => builtin(2),
                "monthnames" => builtin(3),
                items => items.split(',').map(|s| s.trim().to_string()).collect(),
            });
            let asc = order != "desc" && k.get_str("direction") != Some("desc");
            SortOn::Value { asc, list }
        }
        "cell-color" | "cellColor" => SortOn::CellColor {
            rgb: rgb_arg(k.get("color").unwrap_or(&Json::Null))?,
            top,
        },
        "font-color" | "fontColor" => SortOn::FontColor {
            rgb: rgb_arg(k.get("color").unwrap_or(&Json::Null))?,
            top,
        },
        "icon" => {
            let i = k.get("icon").ok_or("an icon key needs 'icon':{set,id}")?;
            SortOn::Icon {
                set: i.get_str("set").ok_or("'icon' needs a 'set'")?.to_string(),
                id: i.get_usize("id").ok_or("'icon' needs an 'id'")? as u32,
                top,
            }
        }
        o => return Err(format!("unknown 'on' '{o}'")),
    };
    Ok(SortLevel { key, on })
}

/// `range.sort {sheet?, range, keys, caseSensitive?, orientation?, header?,
/// expand?}`. A selection inside a wider list is not sorted until `expand`
/// answers the Sort Warning: `true` sorts the whole list, `false` the
/// selection alone.
pub(super) fn range_sort(app: &mut App, args: &Json) -> Result<Json, String> {
    let si = sheet_arg(app, args)?;
    let wb = &app.pkg.workbook;
    let mut range = area(args.get_str("range").ok_or("range.sort needs a 'range'")?)?;
    let ltr = args.get_str("orientation") == Some("columns");
    let mut header = args.get("header").and_then(Json::as_bool);
    if !ltr {
        if let Some((region, has_header)) = gridcore::edit::sort_warning(wb, si, range) {
            match args.get("expand").and_then(Json::as_bool) {
                None => {
                    return Ok(Json::obj(vec![
                        ("sorted", Json::Bool(false)),
                        ("warning", Json::Str(SORT_WARNING.into())),
                        ("expanded", Json::Str(area_name(region))),
                    ]));
                }
                Some(true) => {
                    range = region;
                    header = header.or(Some(has_header));
                }
                Some(false) => {}
            }
        }
    }
    let keys = args
        .get("keys")
        .and_then(Json::as_array)
        .ok_or("range.sort needs 'keys'")?;
    let levels = keys
        .iter()
        .map(|k| level_arg(wb, si, range, ltr, k))
        .collect::<Result<Vec<_>, _>>()?;
    let opts = SortOptions {
        case_sensitive: args
            .get("caseSensitive")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        left_to_right: ltr,
        header: header.unwrap_or(false),
    };
    let n = app.sort_command(si, range, &levels, &opts)?;
    app.status = Some(format!(
        "Sorted {n} {}",
        if ltr { "columns" } else { "rows" }
    ));
    Ok(Json::obj(vec![
        ("sorted", Json::Bool(true)),
        ("range", Json::Str(area_name(range))),
        ("count", Json::Num(n as f64)),
    ]))
}

/// `wb.clock {date}`: fix "today" (TODAY, NOW, a date filter's periods,
/// a typed `3/4`'s year) at `YYYY-MM-DD` (optionally `THH:MM[:SS]`);
/// `null` goes back to the local clock. Not an edit: no undo step.
pub(super) fn wb_clock(app: &mut App, args: &Json) -> Result<Json, String> {
    let serial = match args.get("date") {
        None | Some(Json::Null) => None,
        Some(Json::Str(s)) => Some(parse_date(s).ok_or_else(|| format!("bad date '{s}'"))?),
        _ => return Err("'date' is \"YYYY-MM-DD\" or null".into()),
    };
    crate::set_clock_override(serial);
    app.engine.clock = crate::now_serial();
    app.engine.recalc_all(&mut app.pkg.workbook);
    Ok(Json::obj(vec![(
        "date",
        match args.get("date") {
            Some(Json::Str(s)) => Json::Str(s.clone()),
            _ => Json::Null,
        },
    )]))
}

/// `YYYY-MM-DD[THH:MM[:SS]]` as a serial in the 1900 date system.
fn parse_date(s: &str) -> Option<f64> {
    let (d, t) = s.trim().split_once('T').unwrap_or((s.trim(), ""));
    let mut p = d.split('-');
    let y: i64 = p.next()?.parse().ok()?;
    let m: u32 = p.next()?.parse().ok()?;
    let day: u32 = p.next()?.parse().ok()?;
    if p.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let mut secs = 0u32;
    if !t.is_empty() {
        let mut q = t.split(':');
        let h: u32 = q.next()?.parse().ok()?;
        let mi: u32 = q.next()?.parse().ok()?;
        let se: u32 = q.next().map_or(Some(0), |x| x.parse().ok())?;
        secs = h * 3600 + mi * 60 + se;
    }
    Some(gridcore::sheet::parts_to_serial(y, m, day, secs, false))
}

#[cfg(test)]
mod tests;
