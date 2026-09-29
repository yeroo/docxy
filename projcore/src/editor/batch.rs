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
}

impl Editor {
    /// Run `edit` as ONE undo step. Every setter inside it validates and
    /// snapshots as usual, onto a private history, so [`Self::append_row`]
    /// and the undo cap work unchanged; the batch then records the state
    /// before it once, which clears redo. A batch that ends where it began
    /// records nothing and keeps redo. On `Err` every piece of editor state
    /// (the plan, history, redo, dirty flag, selection, collapsed summaries
    /// and the derived schedule) is restored exactly, whatever `edit` had
    /// already applied.
    pub fn batch<T>(
        &mut self,
        edit: impl FnOnce(&mut Editor) -> Result<T, String>,
    ) -> Result<T, String> {
        debug_assert!(!self.batching, "batches do not nest");
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
        };
        let undo = std::mem::take(&mut self.undo);
        let redo = std::mem::take(&mut self.redo);
        self.batching = true;
        let result = edit(self);
        self.batching = false;
        let changed = self.pushes != saved.pushes && self.proj != saved.proj;
        match result {
            Ok(value) if changed => {
                self.undo = undo;
                self.pushes = saved.pushes;
                // Clears redo, caps history and counts one snapshot.
                self.push_undo(saved.proj);
                // Pair the edit's refresh with the state before the batch,
                // as after any single edit (see `pending`).
                self.pending = Some(saved.sched);
                Ok(value)
            }
            result => {
                self.restore(saved, undo, redo);
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
        self.undo = undo;
        self.redo = redo;
    }
}

#[cfg(test)]
mod tests;
