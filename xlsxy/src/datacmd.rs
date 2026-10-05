//! Data › Sort & Filter as the TUI and the control verbs run it: each
//! command is one undo step over gridcore's filter and sort engines, and
//! leaves its outcome in the status line.

use gridcore::edit::{SortLevel, SortOn, SortOptions};
use gridcore::filter::{ColumnFilter, DateGroup, FilterError, FilterMenu, FilterOutcome};
use gridcore::sheet::Workbook;
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RLine, Span as RSpan};
use ratatui::widgets::{Clear, Paragraph};

use crate::{App, now_serial};

/// Area (r1, c1, r2, c2), 0-based.
pub type Area = (u32, u32, u32, u32);

/// `A1:C9` or `B4` (one cell), 0-based; `$` anchors are ignored.
pub(crate) fn area(s: &str) -> Result<Area, String> {
    let s = s.trim().replace('$', "");
    gridcore::sheet::parse_range_name(&s)
        .or_else(|| gridcore::sheet::parse_cell_name(&s).map(|(r, c)| (r, c, r, c)))
        .ok_or_else(|| format!("bad range '{s}'"))
}

/// `Sheet2!A1:B3` or `'It''s'!A1` (or a bare range on `default`) as
/// (sheet, area).
pub(crate) fn qualified(wb: &Workbook, s: &str, default: usize) -> Result<(usize, Area), String> {
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

/// "Today" for the date filters: the local clock, or `wb.clock`'s date.
pub fn today() -> f64 {
    now_serial().unwrap_or(0.0)
}

impl App {
    /// Run filter command `op` (given "today") as one undo step; the status
    /// line says `<n> of <m> records found`. A command that changed nothing
    /// (Clear with nothing filtered) puts nothing on the undo stack and
    /// leaves the workbook unmodified; so does a refused one, whose reason is
    /// the error and the status. Also whether it changed anything.
    pub(crate) fn filter_command(
        &mut self,
        op: impl FnOnce(&mut Workbook, usize, f64) -> Result<FilterOutcome, FilterError>,
    ) -> Result<(FilterOutcome, bool), String> {
        let si = self.sheet;
        self.filter_command_on(si, op)
    }

    /// [`Self::filter_command`] on sheet `si`.
    pub(crate) fn filter_command_on(
        &mut self,
        si: usize,
        op: impl FnOnce(&mut Workbook, usize, f64) -> Result<FilterOutcome, FilterError>,
    ) -> Result<(FilterOutcome, bool), String> {
        let today = today();
        let mut got = None;
        let res = self.try_structural_if_changed(|wb| {
            let o = op(wb, si, today).map_err(|e| e.to_string())?;
            got = Some(o);
            Ok(())
        });
        match res.and_then(|changed| Ok((got.ok_or_else(String::new)?, changed))) {
            Ok((o, changed)) => {
                self.status = Some(gridcore::filter::status_text(&o));
                Ok((o, changed))
            }
            Err(e) => {
                self.status = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Turn AutoFilter on (around the cursor, or over a selected range) or
    /// off — Data › Filter, Ctrl+Shift+L. One undo step.
    pub(crate) fn toggle_filter(&mut self, range: Option<Area>) -> Result<bool, String> {
        let si = self.sheet;
        let at = self.cur;
        let on = self.pkg.workbook.sheets[si].auto_filter.is_some();
        self.try_structural_if_changed(|wb| {
            if on {
                gridcore::filter::auto_filter_off(wb, si);
                return Ok(());
            }
            match range {
                Some(r) => gridcore::filter::auto_filter_on_range(wb, si, r),
                None => gridcore::filter::auto_filter_on(wb, si, at),
            }
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .inspect_err(|e| self.status = Some(e.clone()))?;
        self.status = Some(if on { "Filter off" } else { "Filter on" }.to_string());
        Ok(!on)
    }

    /// Sort `area` by `levels` as one undo step (none when no cell moved).
    /// A refusal (a cut spill, merged cells of different sizes) changes
    /// nothing; its message is the error and the status. How many rows
    /// (columns) it sorted, and whether anything moved.
    pub(crate) fn sort_command(
        &mut self,
        si: usize,
        area: Area,
        levels: &[SortLevel],
        opts: &SortOptions,
    ) -> Result<(usize, bool), String> {
        let mut n = 0;
        let changed = self
            .try_structural_if_changed(|wb| {
                n = gridcore::edit::sort_range(wb, si, area, levels, opts)
                    .map_err(|e| e.message().to_string())?;
                Ok(())
            })
            .inspect_err(|e| self.status = Some(e.clone()))?;
        Ok((n, changed))
    }
}

// ---------------------------------------------------------------------------
// The TUI's filter drop-down, prompts and sort flow
// ---------------------------------------------------------------------------

/// The lines above the checklist: sort, clear, the typed submenu.
const PICKER_ACTIONS: usize = 4;

/// A column's filter drop-down (Alt+Down on a filter button).
pub(crate) struct FilterPicker {
    pub col: u32,
    pub menu: FilterMenu,
    /// Each item's check, with `(Select All)` first.
    pub checks: Vec<bool>,
    pub search: String,
    /// "Add current selection to filter" for a search.
    pub add: bool,
    pub sel: usize,
}

impl FilterPicker {
    fn lines(&self) -> usize {
        PICKER_ACTIONS + 1 + self.menu.items.len()
    }
}

/// A sort waiting on the Sort Warning's answer.
pub(crate) struct PendingSort {
    pub selection: Area,
    pub region: Area,
    pub region_header: bool,
    /// The levels, by absolute column; a quick sort's is the cursor column.
    pub levels: Vec<SortLevel>,
    pub opts: SortOptions,
    pub header_given: bool,
}

/// The Custom AutoFilter / Top 10 / date-period prompt's text as criteria:
/// `>10 and <=30`, `begins a`, `contains x or ends y`, `top 3`,
/// `bottom 25%`, `above average`, `this week`, `M3`, ….
pub(crate) fn parse_filter_text(text: &str) -> Result<ColumnFilter, String> {
    let t = text.trim();
    let lower = t.to_lowercase();
    let squash: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(k) = gridcore::filter::DYNAMIC_KINDS
        .iter()
        .find(|k| k.to_lowercase() == squash && **k != "null")
    {
        return Ok(ColumnFilter::Dynamic {
            kind: k.to_string(),
            val: None,
            max_val: None,
        });
    }
    for (word, top) in [("top", true), ("bottom", false)] {
        // `top 3`, `top3`, `bottom 25%`; not a value such as `topaz` or
        // `top shelf`.
        let rest = lower
            .strip_prefix(word)
            .filter(|r| r.trim_start().starts_with(|c: char| c.is_ascii_digit()));
        if let Some(rest) = rest {
            let rest = rest.trim().trim_end_matches("items").trim();
            let (num, percent) = match rest.strip_suffix('%') {
                Some(n) => (n.trim(), true),
                None => match rest.strip_suffix("percent") {
                    Some(n) => (n.trim(), true),
                    None => (rest, false),
                },
            };
            let val: f64 = num
                .parse()
                .map_err(|_| format!("Top 10: bad number '{num}'"))?;
            return Ok(ColumnFilter::Top10 {
                top,
                percent,
                val,
                filter_val: None,
            });
        }
    }
    // One or two conditions joined by `and` / `or`.
    let (parts, and) = if let Some((a, b)) = split_word(t, " and ") {
        (vec![a, b], true)
    } else if let Some((a, b)) = split_word(t, " or ") {
        (vec![a, b], false)
    } else {
        (vec![t], false)
    };
    let conds = parts
        .into_iter()
        .map(parse_condition)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ColumnFilter::Custom { and, conds })
}

fn split_word<'a>(t: &'a str, word: &str) -> Option<(&'a str, &'a str)> {
    let i = t.to_lowercase().find(word)?;
    Some((&t[..i], &t[i + word.len()..]))
}

fn parse_condition(c: &str) -> Result<(String, String), String> {
    let c = c.trim();
    let lower = c.to_lowercase();
    for (word, op, pat) in [
        ("!contains ", "notEqual", "*{}*"),
        ("contains ", "equal", "*{}*"),
        ("begins ", "equal", "{}*"),
        ("ends ", "equal", "*{}"),
    ] {
        if lower.starts_with(word) {
            let v = c[word.len()..].trim();
            return Ok((op.to_string(), pat.replace("{}", v)));
        }
    }
    match gridcore::filter::parse(c) {
        Some((op, v)) => Ok((op.to_string(), v)),
        None => Err("Filter: enter a condition such as >10 and <=30".into()),
    }
}

/// The Sort prompt's text: keys separated by `,` — `B asc`, `B desc`,
/// `A list:Jan/Feb/Mar` (or `list:months`, `list:days`), `A fill:FF00B050
/// top`, `B font:FF0000 bottom`, `C icon:3Arrows/2 top` — then options
/// `/case`, `/ltr` (keys are row numbers), `/header`, `/noheader`.
pub(crate) fn parse_sort_text(
    text: &str,
) -> Result<(Vec<SortLevel>, SortOptions, Option<bool>), String> {
    let mut opts = SortOptions::default();
    let mut header = None;
    let mut keys = text;
    // Options come last, each starting ` /`.
    if let Some(i) = text.find(" /") {
        keys = &text[..i];
        for o in text[i..]
            .split(" /")
            .map(str::trim)
            .filter(|o| !o.is_empty())
        {
            match o.to_lowercase().as_str() {
                "case" => opts.case_sensitive = true,
                "ltr" => opts.left_to_right = true,
                "header" => header = Some(true),
                "noheader" => header = Some(false),
                o => return Err(format!("Sort: unknown option /{o}")),
            }
        }
    }
    let mut levels = Vec::new();
    for k in keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        let mut words = k.split_whitespace();
        let at = words.next().ok_or("Sort: a key needs a column")?;
        let key = if opts.left_to_right {
            at.parse::<u32>()
                .ok()
                .and_then(|r| r.checked_sub(1))
                .ok_or_else(|| format!("Sort: '{at}' is not a row number"))?
        } else {
            let up = at.to_ascii_uppercase();
            match gridcore::sheet::parse_col(&up) {
                Some((c, n)) if n == up.len() => c,
                _ => return Err(format!("Sort: '{at}' is not a column")),
            }
        };
        let rest: Vec<&str> = words.collect();
        let top = !rest.iter().any(|w| w.eq_ignore_ascii_case("bottom"));
        let first = rest.first().copied().unwrap_or("asc");
        let lower = first.to_lowercase();
        let on = if let Some(v) = lower.strip_prefix("fill:") {
            SortOn::CellColor {
                rgb: if v == "none" {
                    None
                } else {
                    Some(gridcore::format::hex_rgb(v).ok_or("Sort: bad colour")?)
                },
                top,
            }
        } else if let Some(v) = lower.strip_prefix("font:") {
            SortOn::FontColor {
                rgb: if v == "auto" {
                    None
                } else {
                    Some(gridcore::format::hex_rgb(v).ok_or("Sort: bad colour")?)
                },
                top,
            }
        } else if lower.starts_with("icon:") {
            let (set, id) = first[5..]
                .split_once('/')
                .ok_or("Sort: icon:<set>/<id>, e.g. icon:3Arrows/2")?;
            SortOn::Icon {
                set: set.to_string(),
                id: id.parse().map_err(|_| "Sort: bad icon id")?,
                top,
            }
        } else if lower.starts_with("list:") {
            let items = &first[5..];
            let list = gridcore::edit::builtin_sort_list(items)
                .unwrap_or_else(|| items.split('/').map(|s| s.trim().to_string()).collect());
            let asc = !rest.iter().skip(1).any(|w| w.eq_ignore_ascii_case("desc"));
            SortOn::Value {
                asc,
                list: Some(list),
            }
        } else {
            match lower.as_str() {
                "asc" | "a-z" => SortOn::Value {
                    asc: true,
                    list: None,
                },
                "desc" | "z-a" => SortOn::Value {
                    asc: false,
                    list: None,
                },
                o => return Err(format!("Sort: unknown order '{o}'")),
            }
        };
        levels.push(SortLevel { key, on });
    }
    if levels.is_empty() {
        return Err("Sort: enter columns, e.g. \"B asc, C desc\"".into());
    }
    Ok((levels, opts, header))
}

/// Excel's guess at a header: the first row holds text over a number in
/// some column.
fn guess_header(wb: &Workbook, si: usize, (r1, c1, r2, c2): Area) -> bool {
    use gridcore::sheet::CellValue;
    let s = &wb.sheets[si];
    (c1..=c2).any(|c| {
        matches!(s.cell(r1, c).map(|x| &x.value), Some(CellValue::Text(_)))
            && (r1 + 1..=r2)
                .any(|r| matches!(s.cell(r, c).map(|x| &x.value), Some(CellValue::Number(_))))
    })
}

impl App {
    /// Sort ↑ / ↓: the cursor's column ascending or descending.
    pub(crate) fn quick_sort(&mut self, asc: bool) {
        let levels = vec![SortLevel {
            key: self.cur.1,
            on: SortOn::Value { asc, list: None },
        }];
        self.sort_flow(levels, SortOptions::default(), None);
    }

    /// The Sort prompt (`B asc, C desc`, …).
    pub(crate) fn commit_sort_text(&mut self, text: &str) {
        match parse_sort_text(text) {
            Ok((levels, opts, header)) => self.sort_flow(levels, opts, header),
            Err(e) => self.status = Some(e),
        }
    }

    /// Sort what is selected, or the list around the cursor. A selection
    /// inside a wider list asks first (the Sort Warning prompt).
    fn sort_flow(&mut self, levels: Vec<SortLevel>, mut opts: SortOptions, header: Option<bool>) {
        let si = self.sheet;
        let sel = self.selection();
        let wb = &self.pkg.workbook;
        let single = (sel.0, sel.1) == (sel.2, sel.3);
        let area = if single {
            match gridcore::edit::sort_region(wb, si, self.cur) {
                Some((region, _)) => region,
                None => {
                    self.status = Some("Sort: put the cursor in the data".into());
                    return;
                }
            }
        } else {
            if !opts.left_to_right {
                if let Some((region, region_header)) = gridcore::edit::sort_warning(wb, si, sel) {
                    self.pending_sort = Some(PendingSort {
                        selection: sel,
                        region,
                        region_header,
                        levels,
                        opts,
                        header_given: header.is_some(),
                    });
                    self.open_prompt(crate::PromptKind::SortWarning);
                    return;
                }
            }
            sel
        };
        opts.header = header.unwrap_or_else(|| !opts.left_to_right && guess_header(wb, si, area));
        self.run_sort(area, &levels, &opts);
    }

    fn run_sort(&mut self, area: Area, levels: &[SortLevel], opts: &SortOptions) {
        let si = self.sheet;
        if let Ok((n, _)) = self.sort_command(si, area, levels, opts) {
            self.status = Some(format!(
                "Sorted {n} {}",
                if opts.left_to_right {
                    "columns"
                } else {
                    "rows"
                }
            ));
        }
    }

    /// The Sort Warning's answer: `e` expands to the whole list, `c` sorts
    /// the selection alone.
    pub(crate) fn answer_sort_warning(&mut self, text: &str) {
        let Some(p) = self.pending_sort.take() else {
            return;
        };
        let mut opts = p.opts;
        let si = self.sheet;
        let area = match text.trim().to_lowercase().chars().next() {
            Some('e') => {
                if !p.header_given {
                    opts.header = p.region_header && guess_header(&self.pkg.workbook, si, p.region);
                }
                p.region
            }
            Some('c') => {
                if !p.header_given {
                    opts.header = false;
                }
                p.selection
            }
            _ => {
                self.status = Some("Sort cancelled".into());
                return;
            }
        };
        self.run_sort(area, &p.levels, &opts);
    }

    /// Alt+Down on a filter button: the column's drop-down.
    pub(crate) fn open_filter_picker(&mut self) -> bool {
        let si = self.sheet;
        let Some(af) = self.pkg.workbook.sheets[si].auto_filter.as_ref() else {
            return false;
        };
        let (r1, c1, _, c2) = af.range;
        let (r, c) = self.cur;
        if r != r1 || c < c1 || c > c2 {
            return false;
        }
        match gridcore::filter::menu(&self.pkg.workbook, si, c, None) {
            Ok(menu) => {
                let mut checks = vec![menu.items.iter().all(|i| i.checked)];
                checks.extend(menu.items.iter().map(|i| i.checked));
                self.filter_picker = Some(FilterPicker {
                    col: c,
                    menu,
                    checks,
                    search: String::new(),
                    add: false,
                    sel: PICKER_ACTIONS,
                });
                true
            }
            Err(e) => {
                self.status = Some(e.to_string());
                false
            }
        }
    }

    /// Re-read the drop-down's list for its search text.
    fn refilter_picker(&mut self) {
        let si = self.sheet;
        let Some(p) = self.filter_picker.as_mut() else {
            return;
        };
        let search = (!p.search.is_empty()).then_some(p.search.as_str());
        if let Ok(menu) = gridcore::filter::menu(&self.pkg.workbook, si, p.col, search) {
            // Search results start all checked, as in Excel.
            let all = search.is_some() || menu.items.iter().all(|i| i.checked);
            p.checks = vec![all];
            p.checks
                .extend(menu.items.iter().map(|i| search.is_some() || i.checked));
            p.menu = menu;
            p.sel = p.sel.min(p.lines() - 1);
        }
    }

    pub(crate) fn filter_picker_key(&mut self, code: KeyCode) {
        let Some(p) = self.filter_picker.as_mut() else {
            return;
        };
        let n = p.lines();
        match code {
            KeyCode::Esc => self.filter_picker = None,
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(n - 1),
            KeyCode::PageUp => p.sel = p.sel.saturating_sub(10),
            KeyCode::PageDown => p.sel = (p.sel + 10).min(n - 1),
            KeyCode::Home => p.sel = 0,
            KeyCode::End => p.sel = n - 1,
            KeyCode::Tab => p.add = !p.add,
            KeyCode::Char(' ') if p.sel >= PICKER_ACTIONS => {
                let i = p.sel - PICKER_ACTIONS;
                let on = !p.checks[i];
                if i == 0 {
                    p.checks.iter_mut().for_each(|c| *c = on);
                } else {
                    // A tree node takes its children with it.
                    let depth = p.menu.items[i - 1].depth;
                    p.checks[i] = on;
                    for j in i..p.menu.items.len() {
                        if p.menu.items[j].depth <= depth {
                            break;
                        }
                        p.checks[j + 1] = on;
                    }
                    p.checks[0] = p.checks[1..].iter().all(|c| *c);
                }
            }
            KeyCode::Backspace => {
                p.search.pop();
                self.refilter_picker();
            }
            KeyCode::Char(ch) => {
                p.search.push(ch);
                self.refilter_picker();
            }
            KeyCode::Enter => self.picker_enter(),
            _ => {}
        }
    }

    fn picker_enter(&mut self) {
        let Some(p) = self.filter_picker.take() else {
            return;
        };
        let col = p.col;
        match p.sel {
            0 | 1 => {
                let si = self.sheet;
                let range = self.pkg.workbook.sheets[si]
                    .auto_filter
                    .as_ref()
                    .map(|a| a.range);
                if let Some((r1, c1, r2, c2)) = range {
                    let opts = SortOptions {
                        header: true,
                        ..SortOptions::default()
                    };
                    let levels = [SortLevel {
                        key: col,
                        on: SortOn::Value {
                            asc: p.sel == 0,
                            list: None,
                        },
                    }];
                    self.run_sort((r1, c1, r2, c2), &levels, &opts);
                }
            }
            2 => {
                let _ = self.filter_command(|wb, si, today| {
                    gridcore::filter::clear(wb, si, Some(col), today)
                });
            }
            3 => {
                self.custom_filter_col = Some(col);
                self.open_prompt(crate::PromptKind::CustomFilter);
            }
            _ if !p.search.is_empty() => {
                let (pattern, add) = (p.search.clone(), p.add);
                let _ = self.filter_command(|wb, si, today| {
                    gridcore::filter::search(wb, si, col, &pattern, add, today)
                });
            }
            _ => {
                let f = if p.checks[0] && !p.menu.truncated {
                    None
                } else {
                    let mut vals = Vec::new();
                    let mut dates = Vec::new();
                    let mut blank = false;
                    for (item, on) in p.menu.items.iter().zip(&p.checks[1..]) {
                        if !on {
                            continue;
                        }
                        match item.date {
                            Some(g @ DateGroup { day: Some(_), .. }) => dates.push(g),
                            Some(_) => {}
                            None if item.blank => blank = true,
                            None => vals.push(item.label.clone()),
                        }
                    }
                    Some(ColumnFilter::Values { vals, blank, dates })
                };
                let _ = self.filter_command(|wb, si, today| {
                    gridcore::filter::set_criterion(wb, si, col, f, today)
                });
            }
        }
    }

    /// The Custom AutoFilter / Top 10 / period prompt for the picker's column.
    pub(crate) fn commit_custom_filter(&mut self, text: &str) {
        let Some(col) = self.custom_filter_col.take() else {
            return;
        };
        match parse_filter_text(text) {
            Ok(f) => {
                let _ = self.filter_command(|wb, si, today| {
                    gridcore::filter::set_criterion(wb, si, col, Some(f), today)
                });
            }
            Err(e) => self.status = Some(e),
        }
    }

    /// Filter by Selected Cell's `v`alue, `c`olor, `f`ont colour or `i`con.
    pub(crate) fn commit_filter_by_cell(&mut self, text: &str) {
        use gridcore::filter::ByCell;
        let by = match text.trim().to_lowercase().chars().next() {
            Some('v') => ByCell::Value,
            Some('c') => ByCell::CellColor,
            Some('f') => ByCell::FontColor,
            Some('i') => ByCell::Icon,
            _ => {
                self.status = Some("Filter by: v(alue), c(olor), f(ont colour) or i(con)".into());
                return;
            }
        };
        let at = self.cur;
        let _ = self.filter_command(|wb, si, today| {
            gridcore::filter::filter_by_cell(wb, si, at, by, today)
        });
    }

    /// Advanced Filter: `list=A1:C9 criteria=E1:E2 copy=H1:I1 unique` (the
    /// list defaults to the region around the cursor; `copy` may name
    /// another sheet, which is refused as in Excel).
    pub(crate) fn commit_advanced_filter(&mut self, text: &str) {
        match self.advanced_from_text(text) {
            Ok(a) => {
                let _ = self.filter_command(|wb, si, _| gridcore::filter::advanced(wb, si, &a));
            }
            Err(e) => self.status = Some(e),
        }
    }

    fn advanced_from_text(&self, text: &str) -> Result<gridcore::filter::AdvancedFilter, String> {
        let si = self.sheet;
        let wb = &self.pkg.workbook;
        let mut a = gridcore::filter::AdvancedFilter::default();
        let mut list = None;
        for w in text.split_whitespace() {
            let (k, v) = w.split_once('=').unwrap_or((w, ""));
            let bad = |e: String| format!("Advanced: {k}: {e}");
            match k.to_lowercase().as_str() {
                "list" => list = Some(area(v).map_err(bad)?),
                "criteria" => a.criteria = Some(qualified(wb, v, si).map_err(bad)?),
                "copy" => a.copy_to = Some(qualified(wb, v, si).map_err(bad)?),
                "unique" => a.unique = true,
                _ => return Err(format!("Advanced: unknown '{w}'")),
            }
        }
        a.list = match list {
            Some(l) => l,
            None => {
                gridcore::edit::sort_region(wb, si, self.cur)
                    .ok_or("Advanced: put the cursor in the list, or give list=")?
                    .0
            }
        };
        Ok(a)
    }
}

/// The filter drop-down under its button: the sort and clear items, the
/// typed submenu, `(Select All)`, the checklist (dates indented by depth),
/// the search line and, when the list is cut short, Excel's note.
pub(crate) fn draw_filter_picker(app: &App, p: &FilterPicker, f: &mut Frame, grid: Rect) {
    let header = if p.menu.header.is_empty() {
        gridcore::sheet::col_name(p.col)
    } else {
        p.menu.header.clone()
    };
    let mut lines: Vec<(String, Option<bool>)> = vec![
        ("Sort A to Z".into(), None),
        ("Sort Z to A".into(), None),
        (format!("Clear Filter From \"{header}\""), None),
        (format!("{} ▸", p.menu.submenu.label()), None),
        (
            if p.search.is_empty() {
                "(Select All)".into()
            } else {
                "(Select All Search Results)".into()
            },
            Some(p.checks[0]),
        ),
    ];
    for (item, on) in p.menu.items.iter().zip(&p.checks[1..]) {
        let pad = "  ".repeat(item.depth as usize);
        lines.push((format!("{pad}{}", item.label), Some(*on)));
    }
    let widest = lines
        .iter()
        .map(|(t, _)| t.chars().count())
        .max()
        .unwrap_or(10)
        + 6;
    let w = (widest.max(30) as u16)
        .min(grid.width.saturating_sub(2))
        .max(10);
    let footer = 1 + u16::from(p.menu.truncated);
    let h = (lines.len() as u16 + 1 + footer).min(grid.height.max(4));
    let cell_x = app
        .vis_cols
        .iter()
        .find(|&&(col, ..)| col == p.col)
        .map(|&(_, x, _)| x)
        .unwrap_or(grid.x);
    let cell_y = app
        .vis_rows
        .iter()
        .position(|&r| r == app.cur.0)
        .map(|i| grid.y + i as u16)
        .unwrap_or(grid.y);
    let x = cell_x.min(grid.x + grid.width.saturating_sub(w));
    let y = if cell_y + 1 + h <= grid.y + grid.height {
        cell_y + 1
    } else {
        grid.y
    };
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);
    let width = w as usize;
    let mut out: Vec<RLine> = vec![RLine::from(RSpan::styled(
        crate::fit(&format!(" {header}"), width, false),
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    ))];
    let vis = (h as usize).saturating_sub(1 + footer as usize);
    let top = p.sel.saturating_sub(vis.saturating_sub(1));
    for (i, (text, check)) in lines.iter().enumerate().skip(top).take(vis) {
        let mark = match check {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let style = if i == p.sel {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new()
        };
        out.push(RLine::from(RSpan::styled(
            crate::fit(&format!(" {mark}{text}"), width, false),
            style,
        )));
    }
    if p.menu.truncated {
        out.push(RLine::from(RSpan::styled(
            crate::fit(" Not all items showing", width, false),
            Style::new().add_modifier(Modifier::ITALIC),
        )));
    }
    let add = if p.add { " [+add]" } else { "" };
    out.push(RLine::from(RSpan::styled(
        crate::fit(&format!(" Search: {}▏{add}", p.search), width, false),
        Style::new().add_modifier(Modifier::DIM),
    )));
    f.render_widget(
        Paragraph::new(out).style(Style::new().bg(Color::Black).fg(Color::White)),
        area,
    );
}

/// A filter button on a header cell: `▾`, or `▼` once the column filters.
pub(crate) fn filter_button(wb: &Workbook, si: usize, row: u32, col: u32) -> Option<char> {
    let af = wb.sheets.get(si)?.auto_filter.as_ref()?;
    let (r1, c1, _, c2) = af.range;
    (row == r1 && (c1..=c2).contains(&col)).then(|| {
        if af.criteria.iter().any(|(c, _)| *c == col) {
            '▼'
        } else {
            '▾'
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PromptKind;
    use gridcore::sheet::Cell;
    use gridcore::xlsx::new_xlsx;

    #[test]
    fn the_filter_prompt_grammar() {
        let custom = |and, c: &[(&str, &str)]| ColumnFilter::Custom {
            and,
            conds: c
                .iter()
                .map(|(o, v)| (o.to_string(), v.to_string()))
                .collect(),
        };
        assert_eq!(
            parse_filter_text(">10 and <=30"),
            Ok(custom(
                true,
                &[("greaterThan", "10"), ("lessThanOrEqual", "30")]
            ))
        );
        assert_eq!(
            parse_filter_text("begins a OR contains ~?"),
            Ok(custom(false, &[("equal", "a*"), ("equal", "*~?*")]))
        );
        assert_eq!(
            parse_filter_text("!contains x"),
            Ok(custom(false, &[("notEqual", "*x*")]))
        );
        assert_eq!(
            parse_filter_text("top 3"),
            Ok(ColumnFilter::Top10 {
                top: true,
                percent: false,
                val: 3.0,
                filter_val: None
            })
        );
        assert_eq!(
            parse_filter_text("bottom 25%"),
            Ok(ColumnFilter::Top10 {
                top: false,
                percent: true,
                val: 25.0,
                filter_val: None
            })
        );
        for (t, k) in [
            ("above average", "aboveAverage"),
            ("This Week", "thisWeek"),
            ("m3", "M3"),
        ] {
            assert_eq!(
                parse_filter_text(t),
                Ok(ColumnFilter::Dynamic {
                    kind: k.into(),
                    val: None,
                    max_val: None
                }),
                "{t}"
            );
        }
        assert!(parse_filter_text("").is_err());
        // A value that starts like `top` is a value.
        for v in ["topaz", "top shelf", "bottomline"] {
            assert_eq!(
                parse_filter_text(v),
                Ok(custom(false, &[("equal", v)])),
                "{v}"
            );
        }
    }

    #[test]
    fn the_sort_prompt_grammar() {
        let (levels, opts, header) =
            parse_sort_text("A list:Jan/Feb/Mar, B fill:FF00B050 top, C font:FF0000 bottom, D icon:3Arrows/2, E desc /case /noheader")
                .unwrap();
        assert!(opts.case_sensitive && !opts.left_to_right);
        assert_eq!(header, Some(false));
        assert_eq!(
            levels[0].on,
            SortOn::Value {
                asc: true,
                list: Some(vec!["Jan".into(), "Feb".into(), "Mar".into()])
            }
        );
        assert_eq!(
            levels[1].on,
            SortOn::CellColor {
                rgb: Some((0, 176, 80)),
                top: true
            }
        );
        assert_eq!(
            levels[2].on,
            SortOn::FontColor {
                rgb: Some((255, 0, 0)),
                top: false
            }
        );
        assert_eq!(
            levels[3].on,
            SortOn::Icon {
                set: "3Arrows".into(),
                id: 2,
                top: true
            }
        );
        assert_eq!(
            (levels[4].key, levels[4].on.clone()),
            (
                4,
                SortOn::Value {
                    asc: false,
                    list: None
                }
            )
        );
        let (levels, opts, _) = parse_sort_text("1 asc /ltr").unwrap();
        assert!(opts.left_to_right);
        assert_eq!(levels[0].key, 0);
        let (levels, ..) = parse_sort_text("A list:months").unwrap();
        assert!(matches!(&levels[0].on, SortOn::Value { list: Some(l), .. } if l.len() == 12));
        assert!(parse_sort_text("A sideways").is_err());
        // A colour that is not ASCII hex is refused, never a crash.
        for bad in ["A fill:1é234", "A font:€12345", "A fill:+1+2+3"] {
            assert_eq!(
                parse_sort_text(bad).err().as_deref(),
                Some("Sort: bad colour"),
                "{bad}"
            );
        }
    }

    /// DAT-CASE-037's list A1:C6 in the TUI.
    fn regions() -> App {
        let mut a = App::new(new_xlsx(), "t.xlsx");
        a.os_clip = None;
        let s = &mut a.pkg.workbook.sheets[0];
        for (c, h) in ["Region", "Rep", "Amount"].iter().enumerate() {
            s.set_cell(0, c as u32, Cell::text(h));
        }
        for (i, (g, r, n)) in [
            ("West", "Eve", 5.0),
            ("East", "Ann", 1.0),
            ("North", "Dan", 4.0),
            ("East", "Cara", 3.0),
            ("South", "Bob", 2.0),
        ]
        .into_iter()
        .enumerate()
        {
            let row = i as u32 + 1;
            s.set_cell(row, 0, Cell::text(g));
            s.set_cell(row, 1, Cell::text(r));
            s.set_cell(row, 2, Cell::number(n));
        }
        a.rebuild_engine();
        a
    }

    fn col(a: &App, c: u32) -> Vec<String> {
        (1..=5).map(|r| text_at(a, r, c)).collect()
    }

    fn text_at(a: &App, r: u32, c: u32) -> String {
        match a.sheet().cell(r, c).map(|x| &x.value) {
            Some(gridcore::sheet::CellValue::Text(t)) => t.clone(),
            Some(gridcore::sheet::CellValue::Number(n)) => n.to_string(),
            _ => String::new(),
        }
    }

    #[test]
    fn a_partial_selection_asks_the_sort_warning() {
        let mut a = regions();
        a.anchor = Some((1, 1));
        a.cur = (5, 1); // B2:B6
        a.quick_sort(true);
        assert!(matches!(
            a.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::SortWarning)
        ));
        assert_eq!(text_at(&a, 1, 1), "Eve", "nothing moved yet");
        a.prompt = None;
        a.answer_sort_warning("c");
        assert_eq!(col(&a, 1), ["Ann", "Bob", "Cara", "Dan", "Eve"]);
        assert_eq!(col(&a, 0), ["West", "East", "North", "East", "South"]);
        let mut a = regions();
        a.anchor = Some((1, 1));
        a.cur = (5, 1);
        a.quick_sort(true);
        a.prompt = None;
        a.answer_sort_warning("e");
        assert_eq!(col(&a, 0), ["East", "South", "East", "North", "West"]);
        assert_eq!(a.status.as_deref(), Some("Sorted 5 rows"));
        // Merged cells of different sizes refuse, with Excel's words.
        let mut a = regions();
        a.pkg.workbook.sheets[0].merges.push((1, 0, 1, 1));
        a.cur = (2, 2);
        a.anchor = None;
        a.quick_sort(true);
        assert_eq!(a.status.as_deref(), Some(gridcore::edit::SORT_MERGED));
    }

    #[test]
    fn advanced_and_by_cell_prompts() {
        let mut a = regions();
        a.pkg.workbook.sheets[0].set_cell(0, 5, Cell::text("Region"));
        a.pkg.workbook.sheets[0].set_cell(1, 5, Cell::text("East"));
        a.commit_advanced_filter("list=A1:C6 criteria=F1:F2");
        assert_eq!(a.status.as_deref(), Some("2 of 5 records found"));
        a.pkg.add_sheet("Other");
        a.commit_advanced_filter("list=A1:C6 criteria=F1:F2 copy=Other!A1");
        assert_eq!(
            a.status.as_deref(),
            Some(gridcore::filter::ADVANCED_OTHER_SHEET)
        );
        a.ribbon_act(crate::ribbon::Act::ClearFilter);
        assert_eq!(a.status.as_deref(), Some("5 of 5 records found"));
        // A quoted sheet name, its quote doubled.
        let its = a.pkg.add_sheet("It's");
        a.pkg.workbook.sheets[its].set_cell(0, 0, Cell::text("Region"));
        a.pkg.workbook.sheets[its].set_cell(1, 0, Cell::text("Nowhere"));
        a.commit_advanced_filter("list=A1:C6 criteria='It''s'!A1:A2");
        assert_eq!(a.status.as_deref(), Some("0 of 5 records found"));
        a.ribbon_act(crate::ribbon::Act::ClearFilter);
        a.cur = (1, 0); // West
        a.commit_filter_by_cell("v");
        assert_eq!(a.status.as_deref(), Some("1 of 5 records found"));
        assert!(a.sheet().auto_filter.is_some());
    }
}
