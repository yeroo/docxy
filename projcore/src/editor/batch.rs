//! Many setter calls as one undo step, as a paste over several cells makes.
use super::*;

/// Every piece of editor state an edit can change, saved before a batch.
struct Saved {
    proj: Project,
    sel: usize,
    sel_uid: Option<i32>,
    collapsed: std::collections::BTreeSet<i32>,
    dirty: bool,
    pushes: u64,
    pending: Option<Schedule>,
    sched: Schedule,
    level: Option<Leveled>,
    leveled: bool,
    revision: u64,
    highlights: Vec<display::Filter>,
    last_change: Option<Box<Editor>>,
}

impl Editor {
    /// Run `edit` as ONE undo step. Every setter inside it validates and
    /// snapshots as usual, onto a private history, so [`Self::append_row`]
    /// and the undo cap work unchanged; the batch then records the state
    /// before it once, which clears redo. A batch that ends where it began
    /// records nothing and keeps redo. On `Err` every piece of editor state
    /// (the plan, history, redo, dirty flag, selection, collapsed summaries,
    /// the derived schedule, the revision, the highlights and the last change's
    /// record) is restored exactly, whatever `edit` had already applied. A
    /// batch that ends Ok with the plan unchanged keeps a highlight it set,
    /// moving the revision for it. A batch opened inside another (a control verb that
    /// batches its own setters, run as one of a plugin command's ops) is
    /// part of the outer step: it runs `edit` and leaves the rollback to
    /// the batch that holds it.
    pub fn batch<T>(
        &mut self,
        edit: impl FnOnce(&mut Editor) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.batching {
            return edit(self);
        }
        let saved = Saved {
            proj: self.proj.clone(),
            sel: self.sel,
            sel_uid: self.sel_uid,
            collapsed: self.collapsed.clone(),
            dirty: self.dirty,
            pushes: self.pushes,
            pending: self.pending.clone(),
            sched: self.sched.clone(),
            level: self.level.clone(),
            leveled: self.leveled,
            revision: self.revision,
            highlights: self.highlights.clone(),
            last_change: self.last_change.take(),
        };
        let undo = std::mem::take(&mut self.undo);
        let redo = std::mem::take(&mut self.redo);
        self.batching = true;
        let result = edit(self);
        self.batching = false;
        let changed = self.pushes != saved.pushes && self.proj != saved.proj;
        match result {
            Ok(value) if changed => {
                // The batch is one change, from the state before it.
                let mut from = Editor::assembled(saved.proj.clone(), saved.sched.clone(), false);
                from.leveled = saved.leveled;
                from.level = saved.level;
                self.undo = undo;
                self.pushes = saved.pushes;
                // Clears redo, caps history and counts one snapshot.
                self.push_undo(saved.proj);
                // Pair the edit's refresh with the state before the batch,
                // as after any single edit (see `pending`).
                self.pending = Some(saved.sched);
                self.change_from = None;
                self.record_change(Box::new(from), Some(saved.last_change));
                Ok(value)
            }
            result => {
                // The leveling overlay is not plan state: a batch that only
                // switched it (a plugin's Level All, #1589) keeps the switch.
                let leveled = result.is_ok().then_some(self.leveled);
                // A highlight is view state too (#1667): kept by a batch that
                // ends Ok, moving the revision a host caches by.
                let highlights = result.is_ok().then(|| self.highlights.clone());
                self.restore(saved, undo, redo);
                if let Some(highlights) = highlights
                    && highlights != self.highlights
                {
                    self.highlights = highlights;
                    self.revision += 1;
                }
                if let Some(on) = leveled {
                    self.set_leveled(on);
                }
                result
            }
        }
    }

    fn restore(&mut self, saved: Saved, undo: Vec<Project>, redo: Vec<Project>) {
        self.proj = saved.proj;
        self.sel = saved.sel;
        self.sel_uid = saved.sel_uid;
        self.collapsed = saved.collapsed;
        self.dirty = saved.dirty;
        self.pushes = saved.pushes;
        self.pending = saved.pending;
        self.sched = saved.sched;
        self.level = saved.level;
        self.leveled = saved.leveled;
        // The revision too: a rolled-back batch changed nothing a host
        // caches by it (a plugin dialog's values, #1610).
        self.revision = saved.revision;
        self.highlights = saved.highlights;
        self.last_change = saved.last_change;
        self.change_from = None;
        self.undo = undo;
        self.redo = redo;
    }
}

#[cfg(test)]
mod tests;
