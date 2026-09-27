//! View › Data › Outline: Show/Hide Subtasks. A collapsed summary hides its
//! whole subtree from the grid and the Gantt. It is view state: not in the
//! model, the undo history or a saved file, and a new document starts with
//! every summary expanded. The selected row is never hidden.
use super::*;
use std::collections::BTreeSet;

impl Editor {
    /// Whether `uid` is a summary whose subtasks are hidden.
    pub fn is_collapsed(&self, uid: i32) -> bool {
        self.collapsed.contains(&uid)
            && self
                .index(uid)
                .is_ok_and(|i| self.proj.is_outline_summary(i))
    }

    /// Hide or show a summary's subtasks; `Ok(false)` when it already was.
    /// Only a summary can be collapsed. Collapsing moves a selection inside
    /// the subtree to the summary. Not an edit: nothing is dirtied or undone.
    pub fn set_collapsed(&mut self, uid: i32, collapsed: bool) -> Result<bool, String> {
        let i = self.index(uid)?;
        if !self.proj.is_outline_summary(i) {
            return Err("Only a summary task has subtasks to show or hide".into());
        }
        if !collapsed {
            return Ok(self.collapsed.remove(&uid));
        }
        if !self.collapsed.insert(uid) {
            return Ok(false);
        }
        if (i + 1..subtree_end(&self.proj, i)).contains(&self.sel) {
            self.sel = i;
            self.sel_uid = Some(uid);
        }
        Ok(true)
    }

    /// Flip a summary's collapsed state, returning the new one.
    pub fn toggle_collapsed(&mut self, uid: i32) -> Result<bool, String> {
        let collapsed = !self.is_collapsed(uid);
        self.set_collapsed(uid, collapsed)?;
        Ok(collapsed)
    }

    /// Hide Subtasks as Project does it: a summary collapses; any other task
    /// collapses its summary, which takes the selection. Returns the summary.
    pub fn hide_subtasks(&mut self, uid: i32) -> Result<i32, String> {
        let i = self.index(uid)?;
        let summary = if self.proj.is_outline_summary(i) {
            uid
        } else {
            let task = &self.proj.tasks[i];
            if task.is_null {
                return Err("A blank row has no subtasks to hide".into());
            }
            let parent = self.proj.tasks[..i]
                .iter()
                .rfind(|t| !t.is_null && t.outline_level < task.outline_level)
                .ok_or("The task has no subtasks to hide")?;
            parent.uid
        };
        self.set_collapsed(summary, true)?;
        Ok(summary)
    }

    /// Task indexes in outline order, without the rows collapsed summaries hide.
    pub fn visible_rows(&self) -> Vec<usize> {
        self.hidden_owners()
            .iter()
            .enumerate()
            .filter_map(|(i, owner)| owner.is_none().then_some(i))
            .collect()
    }

    /// The visible row `delta` visible rows from `from`, stopping at the first
    /// and last. 0 in a plan without tasks.
    pub fn visible_step(&self, from: usize, delta: isize) -> usize {
        let rows = self.visible_rows();
        let Some(last) = rows.len().checked_sub(1) else {
            return 0;
        };
        let at = rows.iter().rposition(|&r| r <= from).unwrap_or(0);
        rows[at.saturating_add_signed(delta).min(last)]
    }

    /// Select the last visible row, keeping every summary collapsed.
    pub fn select_last_visible(&mut self) {
        if let Some(&last) = self.visible_rows().last() {
            self.select(last);
        }
    }

    /// For each row, the outermost collapsed summary hiding it.
    pub(super) fn hidden_owners(&self) -> Vec<Option<usize>> {
        hidden_owners(&self.proj, &self.collapsed)
    }

    /// Expand every collapsed summary that hides row `index`.
    pub(super) fn reveal(&mut self, index: usize) {
        while let Some(owner) = self.hidden_owners().get(index).copied().flatten() {
            self.collapsed.remove(&self.proj.tasks[owner].uid);
        }
    }

    /// Forget UIDs that are no longer summaries, so a task that becomes one
    /// again later starts expanded.
    pub(super) fn prune_collapsed(&mut self) {
        let proj = &self.proj;
        let summaries: BTreeSet<i32> = (0..proj.tasks.len())
            .filter(|&i| proj.is_outline_summary(i))
            .map(|i| proj.tasks[i].uid)
            .collect();
        self.collapsed.retain(|uid| summaries.contains(uid));
    }
}

/// The end of the rows row `i` owns in the positional outline: itself, then
/// every following row deeper than it. Blank rows are outside the outline:
/// one between two descendants goes with it; one after the last stays.
pub(super) fn subtree_end(proj: &Project, i: usize) -> usize {
    let task = &proj.tasks[i];
    if task.is_null {
        return i + 1;
    }
    let mut end = i + 1;
    for (k, row) in proj.tasks.iter().enumerate().skip(i + 1) {
        if row.is_null {
            continue;
        }
        if row.outline_level <= task.outline_level {
            break;
        }
        end = k + 1;
    }
    end
}

/// For each row of `proj`, the outermost summary in `collapsed` whose
/// subtree hides it, or `None` for a visible row. Collapsed summaries inside
/// a hidden subtree are hidden with it.
pub(super) fn hidden_owners(proj: &Project, collapsed: &BTreeSet<i32>) -> Vec<Option<usize>> {
    let mut owners = vec![None; proj.tasks.len()];
    let mut i = 0;
    while i < proj.tasks.len() {
        if collapsed.contains(&proj.tasks[i].uid) && proj.is_outline_summary(i) {
            let end = subtree_end(proj, i);
            owners[i + 1..end].fill(Some(i));
            i = end;
        } else {
            i += 1;
        }
    }
    owners
}

#[cfg(test)]
mod tests;
