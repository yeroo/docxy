//! The model of a column's filter drop-down: which typed submenu it offers
//! (Text, Number or Date Filters), and its value checklist.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::apply::{Area, FilterError, cell_date, grown_range, passes, shown_text, test_for};
use super::{ColumnFilter, DateGroup, is_blank_value};
use crate::sheet::{CellValue, Workbook};

/// At most this many values are listed; a longer list says so.
pub const MENU_LIMIT: usize = 10_000;

/// The typed submenu, chosen by the column's dominant type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submenu {
    Text,
    Number,
    Date,
}

impl Submenu {
    pub fn label(self) -> &'static str {
        match self {
            Submenu::Text => "Text Filters",
            Submenu::Number => "Number Filters",
            Submenu::Date => "Date Filters",
        }
    }
}

/// One line of the checklist: a value, `(Blanks)`, or a node of the date
/// tree (a year at depth 0, its months at 1, their days at 2).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub depth: u8,
    pub checked: bool,
    /// The date group a tree node stands for.
    pub date: Option<DateGroup>,
    /// The `(Blanks)` line.
    pub blank: bool,
}

/// A column's drop-down.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterMenu {
    /// The absolute column, and its header's text.
    pub col: u32,
    pub header: String,
    pub submenu: Submenu,
    pub items: Vec<MenuItem>,
    /// More values than [`MENU_LIMIT`]: not every item is listed.
    pub truncated: bool,
    /// How many lines the full list has.
    pub total: usize,
    /// The column has criteria.
    pub filtered: bool,
    /// Filter by Color / Sort by Color's choices: the cell colours, then
    /// the font colours, then the icons the listed records show, each once,
    /// in the order met. Empty when none shows a colour or an icon (only No
    /// Fill and the automatic font colour). A colour we can't read is not
    /// offered.
    pub colors: Vec<ColorChoice>,
}

/// A colour or icon to filter or sort by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorChoice {
    /// A fill; `None` is No Fill.
    Fill(Option<(u8, u8, u8)>),
    /// A font colour; `None` is the automatic colour.
    Font(Option<(u8, u8, u8)>),
    Icon(String, u32),
}

impl ColorChoice {
    /// How a host lists it: `Cell Color 00B050`, `Cell Color No Fill`,
    /// `Font Color Automatic`, `Icon 3Arrows/2`.
    pub fn label(&self) -> String {
        let hex = |(r, g, b): (u8, u8, u8)| format!("{r:02X}{g:02X}{b:02X}");
        match self {
            ColorChoice::Fill(Some(c)) => format!("Cell Color {}", hex(*c)),
            ColorChoice::Fill(None) => "Cell Color No Fill".into(),
            ColorChoice::Font(Some(c)) => format!("Font Color {}", hex(*c)),
            ColorChoice::Font(None) => "Font Color Automatic".into(),
            ColorChoice::Icon(set, id) => format!("Icon {set}/{id}"),
        }
    }

    /// The criterion that keeps the cells showing it.
    pub fn criterion(&self) -> ColumnFilter {
        match self {
            ColorChoice::Fill(rgb) => ColumnFilter::Color {
                cell: true,
                rgb: *rgb,
                dxf_id: None,
            },
            ColorChoice::Font(rgb) => ColumnFilter::Color {
                cell: false,
                rgb: *rgb,
                dxf_id: None,
            },
            ColorChoice::Icon(set, id) => ColumnFilter::Icon {
                set: set.clone(),
                id: *id,
            },
        }
    }

    /// The sort level that puts the cells showing it on top.
    pub fn sort_on(&self) -> crate::edit::SortOn {
        use crate::edit::SortOn;
        match self {
            ColorChoice::Fill(rgb) => SortOn::CellColor {
                rgb: *rgb,
                top: true,
            },
            ColorChoice::Font(rgb) => SortOn::FontColor {
                rgb: *rgb,
                top: true,
            },
            ColorChoice::Icon(set, id) => SortOn::Icon {
                set: set.clone(),
                id: *id,
                top: true,
            },
        }
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// The drop-down of absolute column `col` of the sheet's AutoFilter. The
/// values are those of the records the *other* columns' criteria keep,
/// distinct by displayed text: dates as a year › month › day tree first,
/// then numbers ascending, then text (case-insensitive), `(Blanks)` last.
/// With `search`, only the lines whose text contains it (`*` `?` `~`) are
/// listed. The typed submenu follows the type most of the column's cells
/// hold (ties: text, then number, then date).
pub fn menu(
    wb: &Workbook,
    sheet: usize,
    col: u32,
    search: Option<&str>,
) -> Result<FilterMenu, FilterError> {
    menu_up_to(wb, sheet, col, search, Some(MENU_LIMIT))
}

/// [`menu`] with every value listed, however many: what a host needs to
/// keep the values the drop-down didn't show as they are when a cut-short
/// list is applied.
pub fn menu_all(
    wb: &Workbook,
    sheet: usize,
    col: u32,
    search: Option<&str>,
) -> Result<FilterMenu, FilterError> {
    menu_up_to(wb, sheet, col, search, None)
}

fn menu_up_to(
    wb: &Workbook,
    sheet: usize,
    col: u32,
    search: Option<&str>,
    limit: Option<usize>,
) -> Result<FilterMenu, FilterError> {
    let af = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.auto_filter.as_ref())
        .ok_or(FilterError::NoFilter)?;
    // Rows typed directly below the list are in it, as applying it will take
    // them.
    let (r1, c1, r2, c2): Area = grown_range(wb, sheet, af.range);
    if col < c1 || col > c2 {
        return Err(FilterError::NotInFilter);
    }
    let others: Vec<_> = af
        .criteria
        .iter()
        .filter(|(c, _)| *c != col)
        .map(|(c, f)| (*c, test_for(f)))
        .collect();
    let mine = af.criteria.iter().find(|(c, _)| *c == col).map(|x| &x.1);
    let icons = crate::cf::Icons::new(wb, sheet);
    let mut fills: Vec<Option<(u8, u8, u8)>> = Vec::new();
    let mut fonts: Vec<Option<(u8, u8, u8)>> = Vec::new();
    let mut icon_list: Vec<(String, u32)> = Vec::new();
    let (mut n_date, mut n_num, mut n_text) = (0usize, 0usize, 0usize);
    let mut blanks = false;
    let mut days: BTreeSet<(i64, u32, u32)> = BTreeSet::new();
    let mut nums: BTreeMap<String, f64> = BTreeMap::new();
    let mut texts: HashMap<String, String> = HashMap::new();
    for r in r1 + 1..=r2 {
        let value = wb.sheets[sheet].cell(r, col).map(|c| &c.value);
        // The type counts take every record, filtered or not.
        match value {
            _ if is_blank_value(value) => {}
            Some(CellValue::Number(_)) if cell_date(wb, sheet, r, col).is_some() => n_date += 1,
            Some(CellValue::Number(_)) => n_num += 1,
            _ => n_text += 1,
        }
        if !others
            .iter()
            .all(|(c, t)| passes(wb, sheet, r, *c, t, &icons))
        {
            continue;
        }
        for (shown, list) in [
            (crate::cf::cell_fill(wb, sheet, r, col), &mut fills),
            (crate::cf::cell_font_color(wb, sheet, r, col), &mut fonts),
        ] {
            if let Some(rgb) = shown.rgb() {
                if !list.contains(&rgb) {
                    list.push(rgb);
                }
            }
        }
        if let Some(icon) = icons.icon(r, col) {
            if !icon_list.contains(&icon) {
                icon_list.push(icon);
            }
        }
        if is_blank_value(value) {
            blanks = true;
        } else if let Some(d) = cell_date(wb, sheet, r, col) {
            days.insert((d.year, d.month, d.day));
        } else {
            let t = shown_text(wb, sheet, r, col);
            match value {
                Some(CellValue::Number(n)) => {
                    nums.entry(t).or_insert(*n);
                }
                _ => {
                    texts.entry(t.to_lowercase()).or_insert(t);
                }
            }
        }
    }
    let submenu = if n_text >= n_num && n_text >= n_date {
        Submenu::Text
    } else if n_num >= n_date {
        Submenu::Number
    } else {
        Submenu::Date
    };
    let pat = search.map(|s| format!("*{s}*"));
    let shows = |label: &str| {
        pat.as_deref()
            .is_none_or(|p| crate::formula::wildcard_match(p, label))
    };
    let checked = |text: Option<&str>, group: Option<DateGroup>, blank: bool| match mine {
        None => true,
        Some(ColumnFilter::Values {
            vals,
            blank: b,
            dates,
        }) => {
            if blank {
                return *b;
            }
            // A node is checked when a checked group covers it.
            if let Some(g) = group {
                return dates.iter().any(|d| {
                    d.year == g.year
                        && d.month.is_none_or(|m| g.month == Some(m))
                        && d.day.is_none_or(|x| g.day == Some(x))
                });
            }
            text.is_some_and(|t| vals.iter().any(|v| v.trim().eq_ignore_ascii_case(t.trim())))
        }
        Some(_) => false,
    };
    let mut items: Vec<MenuItem> = Vec::new();
    // The date tree: a year and its months show when any day under them does.
    let mut last: (Option<i64>, Option<u32>) = (None, None);
    for &(y, m, d) in &days {
        let day_label = format!("{d:02}");
        let month_label = MONTHS[(m - 1) as usize];
        if !(shows(&y.to_string()) || shows(month_label) || shows(&day_label)) {
            continue;
        }
        if last.0 != Some(y) {
            let g = DateGroup {
                year: y,
                month: None,
                day: None,
            };
            items.push(MenuItem {
                label: y.to_string(),
                depth: 0,
                checked: checked(None, Some(g), false),
                date: Some(g),
                blank: false,
            });
            last = (Some(y), None);
        }
        if last.1 != Some(m) {
            let g = DateGroup {
                year: y,
                month: Some(m),
                day: None,
            };
            items.push(MenuItem {
                label: month_label.to_string(),
                depth: 1,
                checked: checked(None, Some(g), false),
                date: Some(g),
                blank: false,
            });
            last.1 = Some(m);
        }
        let g = DateGroup {
            year: y,
            month: Some(m),
            day: Some(d),
        };
        items.push(MenuItem {
            label: day_label,
            depth: 2,
            checked: checked(None, Some(g), false),
            date: Some(g),
            blank: false,
        });
    }
    let mut num_items: Vec<(f64, String)> = nums.into_iter().map(|(t, n)| (n, t)).collect();
    num_items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut text_items: Vec<(String, String)> = texts.into_iter().collect();
    text_items.sort();
    for label in num_items
        .into_iter()
        .map(|x| x.1)
        .chain(text_items.into_iter().map(|x| x.1))
    {
        if shows(&label) {
            let c = checked(Some(&label), None, false);
            items.push(MenuItem {
                label,
                depth: 0,
                checked: c,
                date: None,
                blank: false,
            });
        }
    }
    if blanks && pat.is_none() {
        items.push(MenuItem {
            label: "(Blanks)".to_string(),
            depth: 0,
            checked: checked(None, None, true),
            date: None,
            blank: true,
        });
    }
    let total = items.len();
    let truncated = limit.is_some_and(|l| total > l);
    if let Some(limit) = limit.filter(|_| truncated) {
        // `(Blanks)` stays listed at the end.
        let blank = items
            .last()
            .is_some_and(|i| i.blank)
            .then(|| items.pop())
            .flatten();
        items.truncate(limit - usize::from(blank.is_some()));
        items.extend(blank);
    }
    // Colours only when some record shows one: a column of No Fill and the
    // automatic font colour has nothing to filter by.
    let any = fills.iter().chain(&fonts).any(Option::is_some) || !icon_list.is_empty();
    let colors = if any {
        fills
            .into_iter()
            .map(ColorChoice::Fill)
            .chain(fonts.into_iter().map(ColorChoice::Font))
            .chain(icon_list.into_iter().map(|(s, i)| ColorChoice::Icon(s, i)))
            .collect()
    } else {
        Vec::new()
    };
    Ok(FilterMenu {
        col,
        header: shown_text(wb, sheet, r1, col),
        submenu,
        items,
        truncated,
        total,
        filtered: mine.is_some(),
        colors,
    })
}

/// The criteria a drop-down's checklist makes on OK: `listed` are the
/// lines it showed (a [`menu`]'s items, for `search`) and `checks` whether
/// each is checked. Every line checked clears the column (`None`), cut-short
/// list or not. Otherwise a value checklist of the checked values, dates and
/// `(Blanks)`; on a cut-short list the values it didn't show keep their
/// current state, read from [`menu_all`].
pub fn checklist_criteria(
    wb: &Workbook,
    sheet: usize,
    col: u32,
    search: Option<&str>,
    listed: &[MenuItem],
    checks: &[bool],
    truncated: bool,
) -> Result<Option<ColumnFilter>, FilterError> {
    if checks.iter().all(|c| *c) && checks.len() >= listed.len() {
        return Ok(None);
    }
    // Each line with its check: the listed ones as shown, the rest as the
    // column's criterion leaves them.
    let key = |i: &MenuItem| (i.label.clone(), i.date, i.blank);
    let lines: Vec<(MenuItem, bool)> = if truncated {
        let shown: HashMap<_, bool> = listed
            .iter()
            .zip(checks)
            .map(|(i, c)| (key(i), *c))
            .collect();
        menu_all(wb, sheet, col, search)?
            .items
            .into_iter()
            .map(|i| {
                let on = shown.get(&key(&i)).copied().unwrap_or(i.checked);
                (i, on)
            })
            .collect()
    } else {
        listed.iter().cloned().zip(checks.iter().copied()).collect()
    };
    let mut vals = Vec::new();
    let mut dates = Vec::new();
    let mut blank = false;
    for (item, on) in lines {
        if !on {
            continue;
        }
        match item.date {
            Some(g @ DateGroup { day: Some(_), .. }) => dates.push(g),
            Some(_) => {}
            None if item.blank => blank = true,
            None => vals.push(item.label),
        }
    }
    Ok(Some(ColumnFilter::Values { vals, blank, dates }))
}
