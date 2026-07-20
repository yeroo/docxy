//! Flattens the store's folder list into a *visible* tree for the folder
//! pane. Each `FolderRow` already carries its `parent_id` and persisted
//! `is_expanded`; `build_visible` groups children under parents and emits only
//! the rows reachable through expanded ancestors, in depth-first render order.

use mailcore::store::FolderRow;
use std::collections::{HashMap, HashSet};

/// One row in the rendered folder tree: the folder plus its display metadata.
// `depth`/`has_children`/`expanded` are consumed by the folder-pane keys and
// the indented rendering in the following tasks; allow the gap until then.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct VisibleFolder {
    pub row: FolderRow,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// Depth-first flatten of `folders` (already globally ordered: well-known rank,
/// then display name) into the rows currently visible. A folder is a **root**
/// when its `parent_id` is `None` or points outside the set (Graph's
/// `msgfolderroot` is never one of our rows). Children of a collapsed folder
/// are omitted. Sibling order follows the input order.
pub fn build_visible(folders: &[FolderRow]) -> Vec<VisibleFolder> {
    let ids: HashSet<&str> = folders.iter().map(|f| f.id.as_str()).collect();
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in folders.iter().enumerate() {
        if let Some(p) = f.parent_id.as_deref() {
            if ids.contains(p) {
                children.entry(p).or_default().push(i);
            }
        }
    }
    let mut out = Vec::new();
    for (i, f) in folders.iter().enumerate() {
        let is_root = match f.parent_id.as_deref() {
            None => true,
            Some(p) => !ids.contains(p),
        };
        if is_root {
            push_subtree(folders, &children, i, 0, &mut out);
        }
    }
    out
}

fn push_subtree(
    folders: &[FolderRow],
    children: &HashMap<&str, Vec<usize>>,
    i: usize,
    depth: usize,
    out: &mut Vec<VisibleFolder>,
) {
    let f = &folders[i];
    let kids = children.get(f.id.as_str());
    let has_children = kids.is_some_and(|v| !v.is_empty());
    let expanded = f.is_expanded;
    out.push(VisibleFolder {
        row: f.clone(),
        depth,
        has_children,
        expanded,
    });
    if expanded {
        if let Some(kids) = kids {
            for &c in kids {
                push_subtree(folders, children, c, depth + 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fr(id: &str, parent: Option<&str>, exp: bool) -> FolderRow {
        FolderRow {
            id: id.into(),
            parent_id: parent.map(Into::into),
            display_name: id.into(),
            total_count: 0,
            unread_count: 0,
            delta_link: None,
            well_known_name: None,
            sort_order: None,
            is_expanded: exp,
        }
    }

    #[test]
    fn nests_children_under_expanded_parents_and_hides_collapsed() {
        // Inbox(expanded) -> [EPAM(collapsed) -> ADPT], Sent(top, leaf)
        let rows = vec![
            fr("Inbox", None, true),
            fr("EPAM", Some("Inbox"), false),
            fr("ADPT", Some("EPAM"), false),
            fr("Sent", None, false),
        ];
        let v = build_visible(&rows);
        let shape: Vec<_> = v
            .iter()
            .map(|x| (x.row.id.as_str(), x.depth, x.has_children))
            .collect();
        // ADPT hidden: EPAM is collapsed.
        assert_eq!(
            shape,
            vec![("Inbox", 0, true), ("EPAM", 1, true), ("Sent", 0, false)]
        );
    }

    #[test]
    fn top_level_when_parent_outside_set() {
        let rows = vec![fr("Inbox", Some("msgroot"), false)]; // parent not a row
        let v = build_visible(&rows);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].depth, 0);
    }
}
