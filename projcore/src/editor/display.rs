//! The grid's sort and AutoFilter (#1533). Both are view state, like
//! `collapsed`: outside the model, the undo history and saved files.
//!
//! [`Editor::visible_rows`] stays in storage order (collapse only), and every
//! edit acts on task indexes, so no edit can move the wrong task. What a grid
//! draws and a cursor steps through is [`Editor::display_rows`]: those rows,
//! filtered, then sorted among siblings with each subtree kept together
//! (Project's "Keep outline structure"). Pinned: the project summary row
//! (UID 0, when shown) stays first, and a blank row is never filtered; it
//! sorts after its siblings.
use super::*;
use std::collections::BTreeSet;

/// A column sort: the field and whether it runs high to low.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sort {
    pub field: Field,
    pub descending: bool,
}

/// One column's checklist: the display texts that stay shown.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Filter {
    field: Field,
    allowed: BTreeSet<String>,
}

/// A distinct value of a column, for the AutoFilter checklist.
#[derive(Clone, PartialEq, Debug)]
pub struct ValueItem {
    /// What the cell shows; empty for a blank cell. The filter matches on it.
    pub text: String,
    /// `(year, month, day)` of a date column's value, for the Year > Month >
    /// Day tree.
    pub date: Option<(i64, u32, u32)>,
}

impl Editor {
    /// The active sort.
    pub fn sort(&self) -> Option<Sort> {
        self.sort
    }

    /// Sort by `field` (`None` clears the sort, back to ID order). A change
    /// moves a selection that the order hides nothing from, so it only bumps
    /// [`Self::revision`].
    pub fn set_sort(&mut self, sort: Option<Sort>) {
        if self.sort != sort {
            self.sort = sort;
            self.revision += 1;
        }
    }

    /// The texts column `field` is filtered to, if it is filtered.
    pub fn filter(&self, field: Field) -> Option<&BTreeSet<String>> {
        self.filters
            .iter()
            .find(|f| f.field == field)
            .map(|f| &f.allowed)
    }

    /// The filtered columns, in the order they were first filtered.
    pub fn filtered_fields(&self) -> Vec<Field> {
        self.filters.iter().map(|f| f.field).collect()
    }

    /// Whether any column is filtered.
    pub fn is_filtered(&self) -> bool {
        !self.filters.is_empty()
    }

    /// Filter `field` to the rows whose display text is in `allowed`
    /// (`None` clears that column's filter). Filters on several columns AND.
    /// A selection the filter hides moves to the first displayed row.
    pub fn set_filter(&mut self, field: Field, allowed: Option<BTreeSet<String>>) {
        let before = self.filters.clone();
        match allowed {
            Some(allowed) => match self.filters.iter_mut().find(|f| f.field == field) {
                Some(f) => f.allowed = allowed,
                None => self.filters.push(Filter { field, allowed }),
            },
            None => self.filters.retain(|f| f.field != field),
        }
        if self.filters != before {
            self.revision += 1;
            self.sel_into_display();
        }
    }

    /// Clear every column's filter.
    pub fn clear_filters(&mut self) {
        if !self.filters.is_empty() {
            self.filters.clear();
            self.revision += 1;
        }
    }

    /// A structural edit that depends on where a row sits (insert above or
    /// below, indent, outdent) is refused while the grid is sorted: the order
    /// shown is not the order the edit would act in.
    pub(super) fn refuse_while_sorted(&self) -> Result<(), String> {
        match self.sort {
            Some(_) => Err("Clear the sort first".into()),
            None => Ok(()),
        }
    }

    /// Put a selection the filter hides on a displayed row, the invariant
    /// every operation keeps: the next displayed row after it in the order
    /// (sorted, unfiltered) it was in, else the one before, a blank row only
    /// when nothing else shows. Nothing displayed leaves it where it is, for
    /// a host to meet with its entry row.
    pub(super) fn sel_into_display(&mut self) {
        let rows = self.display_rows();
        if rows.contains(&self.sel) || self.sel_pinned {
            return;
        }
        let tasks = &self.proj.tasks;
        let shown = |i: &usize| rows.contains(i) && !tasks[*i].is_null;
        let order = self.ordered_rows(false, false);
        let at = order.iter().position(|&i| i == self.sel);
        let near = at.and_then(|at| {
            order[at + 1..]
                .iter()
                .find(|i| shown(i))
                .or_else(|| order[..at].iter().rev().find(|i| shown(i)))
        });
        let pick = near
            .or_else(|| rows.iter().find(|&&i| !tasks[i].is_null))
            .or_else(|| rows.first());
        if let Some(&pick) = pick {
            self.sel = pick;
            self.sel_uid = Some(self.proj.tasks[pick].uid);
        }
    }

    /// [`Self::select`] for a row an edit made, which a filter may hide: the
    /// selection ends on a displayed row either way, and the answer is
    /// whether that is `index` itself.
    pub fn select_shown(&mut self, index: usize) -> bool {
        self.select(index);
        let shown = self.display_rows().contains(&self.sel);
        if !shown {
            self.sel_into_display();
        }
        shown
    }

    /// The selected task's uid when it is displayed: what a host acts on, so
    /// that nothing acts on a row the view hides.
    pub fn shown_selected_uid(&self) -> Option<i32> {
        let uid = self.selected_uid()?;
        (!self.is_view_active() || self.display_rows().contains(&self.sel)).then_some(uid)
    }

    /// Whether a sort or a filter is on.
    pub fn is_view_active(&self) -> bool {
        self.sort.is_some() || !self.filters.is_empty()
    }

    /// The rows a grid draws, as task indexes: [`Self::visible_rows`], then
    /// the filter, then the sort. With neither, exactly `visible_rows`.
    pub fn display_rows(&self) -> Vec<usize> {
        self.ordered_rows(true, true)
    }

    /// The rows in display order: with `collapse` those collapsed summaries
    /// leave shown, with `filter` those the filters leave shown; sorted if a
    /// sort is on. Find walks it with neither.
    pub(super) fn ordered_rows(&self, collapse: bool, filter: bool) -> Vec<usize> {
        let rows: Vec<usize> = if collapse {
            self.visible_rows()
        } else {
            (0..self.proj.tasks.len())
                .filter(|&i| self.show_summary || !self.proj.tasks[i].is_project_summary())
                .collect()
        };
        let filtering = filter && !self.filters.is_empty();
        if self.sort.is_none() && !filtering {
            return rows;
        }
        let tasks = &self.proj.tasks;
        let reader = FieldReader::new(self);
        let mut rows = rows;
        if filtering {
            let shown = self.filter_pass(&reader, None);
            rows.retain(|&i| shown[i]);
        }
        if let Some(sort) = self.sort {
            let keys: Vec<SortKey> = (0..tasks.len())
                .map(|i| SortKey::of(&tasks[i], &reader.read(&tasks[i], sort.field)))
                .collect();
            let order = sorted_order(&self.proj, &keys, sort.descending);
            let mut rank = vec![0usize; tasks.len()];
            for (r, &i) in order.iter().enumerate() {
                rank[i] = r;
            }
            rows.sort_by_key(|&i| rank[i]);
        }
        rows
    }

    /// For each task, whether the filters show it: a task passes when every
    /// filtered column's text is allowed, a summary also when a task below it
    /// passes; blank rows and the project summary row always pass. `skip`
    /// leaves one column's filter out.
    fn filter_pass(&self, reader: &FieldReader, skip: Option<Field>) -> Vec<bool> {
        let tasks = &self.proj.tasks;
        let filters: Vec<&Filter> = self
            .filters
            .iter()
            .filter(|f| Some(f.field) != skip)
            .collect();
        if filters.is_empty() {
            return vec![true; tasks.len()];
        }
        let own: Vec<bool> = tasks
            .iter()
            .map(|t| {
                t.is_null
                    || t.is_project_summary()
                    || filters
                        .iter()
                        .all(|f| f.allowed.contains(&reader.read(t, f.field).text))
            })
            .collect();
        (0..tasks.len())
            .map(|i| {
                own[i]
                    || (self.proj.is_outline_summary(i)
                        && (i + 1..subtree_end(&self.proj, i)).any(|j| !tasks[j].is_null && own[j]))
            })
            .collect()
    }

    /// The display texts `specs` name in column `field`'s checklist, for a
    /// host with no checklist to click (a command line, an agent). A spec is
    /// a value as the cell shows it (any case), `(Blanks)` for a blank cell,
    /// or on a date column `YYYY`, `YYYY-MM` or `YYYY-MM-DD`: every value
    /// of that year, month or day, as ticking a node of the Year > Month >
    /// Day tree does. A spec that names nothing is the error.
    pub fn filter_texts(&self, field: Field, specs: &[String]) -> Result<BTreeSet<String>, String> {
        let values = self.column_values(field);
        let mut out = BTreeSet::new();
        for spec in specs {
            let spec = spec.trim();
            let exact = |v: &&ValueItem| {
                v.text.eq_ignore_ascii_case(spec)
                    || (v.text.is_empty() && spec.eq_ignore_ascii_case("(Blanks)"))
            };
            let before = out.len();
            if let Some(v) = values.iter().find(exact) {
                out.insert(v.text.clone());
                continue;
            }
            let parts: Vec<&str> = spec.split('-').collect();
            let nums: Option<Vec<i64>> = parts.iter().map(|p| p.parse().ok()).collect();
            if let Some(n) = nums.filter(|n| (1..=3).contains(&n.len())) {
                for v in &values {
                    let Some((y, m, d)) = v.date else { continue };
                    let ymd = [y, i64::from(m), i64::from(d)];
                    if n.iter().zip(ymd).all(|(a, b)| *a == b) {
                        out.insert(v.text.clone());
                    }
                }
            }
            if out.len() == before {
                return Err(format!("No value '{spec}' in {}", field.name()));
            }
        }
        Ok(out)
    }

    /// The distinct values of column `field` for its checklist, sorted by the
    /// column's own type, a blank last. Rows other columns' filters hide are
    /// left out; the column's own filter is ignored, so it can be widened.
    pub fn column_values(&self, field: Field) -> Vec<ValueItem> {
        let reader = FieldReader::new(self);
        let shown = self.filter_pass(&reader, Some(field));
        let tasks = &self.proj.tasks;
        let mut found: Vec<(SortKey, ValueItem)> = Vec::new();
        for (i, task) in tasks.iter().enumerate() {
            if task.is_null || !shown[i] || (task.is_project_summary() && !self.show_summary) {
                continue;
            }
            let read = reader.read(task, field);
            if found.iter().any(|(_, v)| v.text == read.text) {
                continue;
            }
            let date = match read.value {
                FieldValue::Date(d) => {
                    let p = d.parts();
                    Some((p.year, p.month, p.day))
                }
                _ => None,
            };
            found.push((
                SortKey::of(task, &read),
                ValueItem {
                    text: read.text,
                    date,
                },
            ));
        }
        found.sort_by(|a, b| a.0.compare(&b.0));
        found.into_iter().map(|(_, v)| v).collect()
    }
}

/// What a sort compares: a blank, a number or a lower-cased text.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum SortKey {
    Blank,
    Number(f64),
    Text(String),
}

impl SortKey {
    pub(super) fn of(task: &Task, read: &FieldRead) -> SortKey {
        if task.is_null {
            return SortKey::Blank;
        }
        match &read.value {
            FieldValue::Null => match read.text.trim() {
                "" => SortKey::Blank,
                t => SortKey::Text(t.to_lowercase()),
            },
            FieldValue::Bool(b) => SortKey::Number(f64::from(u8::from(*b))),
            FieldValue::Int(n) | FieldValue::Minutes(n) => SortKey::Number(*n as f64),
            FieldValue::Money(n) | FieldValue::Number(n) => SortKey::Number(*n),
            FieldValue::Date(d) => SortKey::Number(d.minutes() as f64),
            FieldValue::Text(t) if t.trim().is_empty() => SortKey::Blank,
            FieldValue::Text(t) => SortKey::Text(t.to_lowercase()),
        }
    }

    /// Ascending; a blank is after everything. Numbers sort before texts.
    pub(super) fn compare(&self, other: &SortKey) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        match (self, other) {
            (SortKey::Blank, SortKey::Blank) => Equal,
            (SortKey::Blank, _) => Greater,
            (_, SortKey::Blank) => Less,
            (SortKey::Number(a), SortKey::Number(b)) => a.total_cmp(b),
            (SortKey::Number(_), SortKey::Text(_)) => Less,
            (SortKey::Text(_), SortKey::Number(_)) => Greater,
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),
        }
    }

    /// The order a sort uses: reversed for `descending`, a blank still last.
    fn directed(&self, other: &SortKey, descending: bool) -> std::cmp::Ordering {
        let blank = |k: &SortKey| matches!(k, SortKey::Blank);
        if blank(self) || blank(other) || !descending {
            self.compare(other)
        } else {
            other.compare(self)
        }
    }
}

/// Every task index in sorted order: siblings under each parent sorted by
/// `keys` (stable), each with its subtree right after it. The project summary
/// row, when shown, stays first and its tasks sort as its siblings would.
fn sorted_order(proj: &Project, keys: &[SortKey], descending: bool) -> Vec<usize> {
    fn siblings(
        proj: &Project,
        keys: &[SortKey],
        descending: bool,
        from: usize,
        to: usize,
        out: &mut Vec<usize>,
    ) {
        let mut groups: Vec<(usize, usize)> = Vec::new();
        let mut i = from;
        while i < to {
            let end = subtree_end(proj, i).min(to).max(i + 1);
            groups.push((i, end));
            i = end;
        }
        groups.sort_by(|a, b| keys[a.0].directed(&keys[b.0], descending));
        for (head, end) in groups {
            out.push(head);
            siblings(proj, keys, descending, head + 1, end, out);
        }
    }
    let mut out = Vec::with_capacity(proj.tasks.len());
    let mut from = 0;
    if proj.tasks.first().is_some_and(|t| t.is_project_summary()) {
        out.push(0);
        from = 1;
    }
    siblings(proj, keys, descending, from, proj.tasks.len(), &mut out);
    out
}

#[cfg(test)]
mod tests;
