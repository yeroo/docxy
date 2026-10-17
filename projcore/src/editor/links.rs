//! Links an outline edit gives a new meaning (#376). Indenting a task under
//! its predecessor, or outdenting one over its successor, makes a link
//! between a summary and its own subtask; an outline change can also close a
//! dependency cycle through a summary's links. #310 refuses both for a new
//! link. For an outline edit, as in Project, the summary link is dropped; an
//! edit that makes a cycle-free plan cyclic is refused. A plan that already
//! has a cycle (as loaded) is not checked for more.
use super::*;

const OUTLINE_CYCLE: &str = "This change would create a circular relationship between linked tasks";

/// Make `next`, the tasks an edit of `prev` produced, keep the link rules
/// when the edit changed the outline: a link the edit turned into one between
/// a task and its own outline ancestor is removed, and the edit is refused
/// when `next` has a dependency cycle and `prev` has none (a `prev` that is
/// already cyclic refuses nothing). Links `prev` already had between a
/// summary and its subtask stay, as #310 keeps them.
///
/// A blank row is outside the outline, so a blank row that becomes a task
/// (any setter on it, a link edit included) gains its ancestry here: a link
/// it or another task stores to it can be dropped, or close a cycle that
/// refuses the edit.
pub(super) fn follow_outline(prev: &[Task], next: &mut [Task]) -> Result<(), String> {
    if same_outline(prev, next) {
        return Ok(());
    }
    drop_new_summary_links(prev, next);
    if has_link_cycle(next) && !has_link_cycle(prev) {
        return Err(OUTLINE_CYCLE.into());
    }
    Ok(())
}

/// Whether the rows, their blank state and levels are the same. Only a fast
/// path: links are judged one by one when the outline changed. `renumber_wbs`
/// answers with it too.
pub(super) fn same_outline(a: &[Task], b: &[Task]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            (x.uid, x.is_null, x.outline_level) == (y.uid, y.is_null, y.outline_level)
        })
}

/// The outline in one pass (`outline::subtree_end_in` per row would be
/// quadratic on a deeply nested outline): where each row's subtree ends, as
/// that gives it, each task's parent, and the row of each non-blank UID.
struct Outline {
    ends: Vec<usize>,
    rows: std::collections::HashMap<i32, usize>,
    /// The nearest non-blank row above with a lower level.
    parents: Vec<Option<usize>>,
}

impl Outline {
    fn new(tasks: &[Task]) -> Self {
        let mut ends: Vec<usize> = (1..=tasks.len()).collect();
        let mut parents = vec![None; tasks.len()];
        let mut rows = std::collections::HashMap::new();
        let mut stack: Vec<usize> = Vec::new();
        // The last task seen; read only while the stack holds one.
        let mut last = 0;
        for (i, task) in tasks.iter().enumerate() {
            if task.is_null {
                continue;
            }
            // Every row still open is deeper than or level with those below
            // it on the stack; one this row closes ends after the last task.
            while let Some(&top) = stack.last() {
                if tasks[top].outline_level < task.outline_level {
                    break;
                }
                ends[top] = last + 1;
                stack.pop();
            }
            parents[i] = stack.last().copied();
            rows.insert(task.uid, i);
            stack.push(i);
            last = i;
        }
        for top in stack {
            ends[top] = last + 1;
        }
        Self {
            ends,
            rows,
            parents,
        }
    }

    /// Whether rows `a` and `b` are a task and its own outline ancestor,
    /// either way round.
    fn related(&self, a: usize, b: usize) -> bool {
        (a < b && b < self.ends[a]) || (b < a && a < self.ends[b])
    }

    /// Whether two UIDs are tasks and one is the other's outline ancestor.
    fn related_uids(&self, a: i32, b: i32) -> bool {
        match (self.rows.get(&a), self.rows.get(&b)) {
            (Some(&a), Some(&b)) => self.related(a, b),
            _ => false,
        }
    }
}

/// Remove each link `next` has between a task and its own outline ancestor
/// that `prev` did not have as one: either end was elsewhere in the outline
/// or a blank row there.
fn drop_new_summary_links(prev: &[Task], next: &mut [Task]) {
    let before = Outline::new(prev);
    let after = Outline::new(next);
    for task in next.iter_mut().filter(|t| !t.is_null) {
        let uid = task.uid;
        task.predecessors
            .retain(|p| !after.related_uids(uid, p.uid) || before.related_uids(uid, p.uid));
    }
}

/// Whether the links make a dependency cycle between leaves, where a link
/// to or from a summary applies to all its leaves (as #310's `LinkGraph`
/// reads them). Linear in tasks and links: each task is an entry node and an
/// exit node; a leaf's entry leads to its exit, a summary's entry to its
/// children's, a child's exit to its summary's, and a link from its
/// predecessor's exit to its successor's entry. Links naming a blank or
/// missing row are ignored, as the scheduler ignores them; so are a link
/// between a task and its own outline ancestor (#310's other rule, which a
/// loaded file can still hold) and a task linked to itself.
fn has_link_cycle(tasks: &[Task]) -> bool {
    let outline = Outline::new(tasks);
    let entry = |i: usize| 2 * i;
    let exit = |i: usize| 2 * i + 1;
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); 2 * tasks.len()];
    for (i, task) in tasks.iter().enumerate() {
        if task.is_null {
            continue;
        }
        if outline.ends[i] == i + 1 {
            edges[entry(i)].push(exit(i));
        }
        if let Some(parent) = outline.parents[i] {
            edges[entry(parent)].push(entry(i));
            edges[exit(i)].push(exit(parent));
        }
        for p in &task.predecessors {
            if let Some(&pred) = outline.rows.get(&p.uid)
                && pred != i
                && !outline.related(pred, i)
            {
                edges[exit(pred)].push(entry(i));
            }
        }
    }
    // Kahn: a cycle leaves nodes that never reach in-degree zero.
    let mut indegree = vec![0usize; edges.len()];
    for to in edges.iter().flatten() {
        indegree[*to] += 1;
    }
    let mut ready: Vec<usize> = (0..edges.len()).filter(|&n| indegree[n] == 0).collect();
    let mut done = 0;
    while let Some(node) = ready.pop() {
        done += 1;
        for &to in &edges[node] {
            indegree[to] -= 1;
            if indegree[to] == 0 {
                ready.push(to);
            }
        }
    }
    done < edges.len()
}

impl Editor {
    /// The predecessors that set `uid`'s dates, as Project's Task Path
    /// marks its driving predecessors: the link's target (the predecessor's
    /// early finish for FS and FF, early start for SS and SF, moved by the
    /// lag on the successor's calendar) is the successor's early start (FS,
    /// SS) or early finish (FF, SF). Ties all drive. A manual task, an
    /// inactive predecessor and a lead (negative lag) drive nothing.
    pub fn driving_predecessors(&self, uid: i32) -> Vec<i32> {
        let proj = self.project();
        let (Some(task), Some(own)) = (proj.task(uid), self.schedule().get(uid)) else {
            return Vec::new();
        };
        if task.manual {
            return Vec::new();
        }
        let cal = crate::assign::task_calendar(proj, task);
        task.predecessors
            .iter()
            .filter(|p| {
                let (Some(pred), Some(pr)) = (proj.task(p.uid), self.schedule().get(p.uid)) else {
                    return false;
                };
                if pred.active == Some(false) {
                    return false;
                }
                let Some(lag) = p.lag_minutes(pred.duration_min) else {
                    return false;
                };
                if lag < 0 {
                    return false;
                }
                let anchor = match p.link {
                    LinkType::FinishStart | LinkType::FinishFinish => pr.early_finish,
                    LinkType::StartStart | LinkType::StartFinish => pr.early_start,
                };
                let to_finish = matches!(p.link, LinkType::FinishFinish | LinkType::StartFinish);
                let target = match p.lag_format.kind() {
                    LagKind::Elapsed if to_finish => Some(anchor.add_minutes(lag)),
                    LagKind::Elapsed => {
                        crate::assign::advance(&cal, anchor.add_minutes(lag), 0, false)
                    }
                    _ => crate::assign::advance(&cal, anchor, lag, to_finish),
                };
                let actual = if to_finish {
                    own.early_finish
                } else {
                    own.early_start
                };
                target == Some(actual)
            })
            .map(|p| p.uid)
            .collect()
    }

    /// `minutes` of working time on `uid`'s calendar after `from`, as a
    /// finish (an instant at a period's end stays there): where a chart ends
    /// the task's slack. `None` for an unknown task or a calendar without
    /// working time.
    pub fn working_time_after(&self, uid: i32, from: DateTime, minutes: i64) -> Option<DateTime> {
        let proj = self.project();
        let task = proj.task(uid)?;
        crate::assign::advance(
            &crate::assign::task_calendar(proj, task),
            from,
            minutes,
            true,
        )
    }
}

#[cfg(test)]
mod tests;
