//! The window registry: every `Docxy` window of this run, in creation order
//! (#587). A window owns its tabs; documents are never shared between windows.
//! New Window moves a tab into a new window, Arrange All resizes every window
//! to its strip of the calling window's display, and the harness routes every
//! verb to the registry's SELECTED window. The registry also owns the control
//! servers' links, so closing any window — the first included — leaves the
//! control surface up for the run.
//!
//! The registry is a gpui `Global` set once in `main`, before the first
//! window. Window ids start at 1 and are never reused; `selected == 0` means
//! no window is registered yet. Each entry keeps the `PersistTab`s its window
//! last wrote (never the union — appending another window's snapshot to a
//! session write must not nest snapshots inside snapshots).
//!
//! ⚠️ Never hold the global borrowed across `open_window` / `update_window`
//! (re-entrancy): take a descriptor snapshot, drop the borrow, then act.

use gpui::{AnyWindowHandle, App, Bounds, Entity, Global, Pixels, Point, Size, WeakEntity, px};

use crate::{Docxy, PersistTab};

/// `display` tiled into `n` equal vertical strips, left to right, in window
/// creation order. gpui at the pinned rev has no cross-platform way to MOVE a
/// window, so Arrange All resizes windows to their strip's SIZE and leaves
/// positions alone. Remainder pixels go to the last strip so the tiling
/// covers the display exactly.
pub(crate) fn tile_bounds(display: Bounds<Pixels>, n: usize) -> Vec<Bounds<Pixels>> {
    if n == 0 {
        return Vec::new();
    }
    let base = (display.size.width.as_f32() / n as f32).floor();
    let right = display.origin.x.as_f32() + display.size.width.as_f32();
    let mut tiles = Vec::with_capacity(n);
    let mut x = display.origin.x.as_f32();
    for i in 0..n {
        let w = if i + 1 == n {
            right - x
        } else {
            base.min(right - x)
        };
        tiles.push(Bounds::new(
            Point::new(px(x), display.origin.y),
            Size::new(px(w), display.size.height),
        ));
        x += w;
    }
    tiles
}

/// Where a new window opens: the source's bounds offset by a 30px cascade
/// step, clamped to stay inside `display`.
pub(crate) fn cascade(source: Bounds<Pixels>, display: Bounds<Pixels>) -> Bounds<Pixels> {
    const STEP: f32 = 30.;
    let origin = display.origin;
    let max_x = origin.x.as_f32() + display.size.width.as_f32() - source.size.width.as_f32();
    let max_y = origin.y.as_f32() + display.size.height.as_f32() - source.size.height.as_f32();
    let x =
        (source.origin.x.as_f32() + STEP).clamp(origin.x.as_f32(), max_x.max(origin.x.as_f32()));
    let y =
        (source.origin.y.as_f32() + STEP).clamp(origin.y.as_f32(), max_y.max(origin.y.as_f32()));
    Bounds::new(Point::new(px(x), px(y)), source.size)
}

/// A registered window. Generic over the handle types so the bookkeeping
/// (ids, selection, snapshots) unit-tests without a gpui app.
pub(crate) struct Entry<V, H> {
    pub(crate) id: u64,
    pub(crate) view: V,
    pub(crate) handle: H,
    seq: usize,
    persisted: Vec<PersistTab>,
}

pub(crate) struct Windows<V, H> {
    entries: Vec<Entry<V, H>>,
    selected: u64,
    next_id: u64,
    next_seq: usize,
    // The control servers and their pumps, owned for the whole run: kept in
    // a window's view they would drop — and take the discovery file down —
    // when that window closed while another lived (#587 r1 M4).
    links: Vec<crate::control::ControlLink>,
    // Every registered window's handle, shared with the control pump: it
    // tries each in turn per request, so a window opened or closed by the
    // UI between verbs is always in the candidate set (#587 r2 M1). Rc, not
    // the entries vec: the pump must read the CURRENT set without borrowing
    // the App (a native dialog's modal loop may hold it).
    handles: std::rc::Rc<std::cell::RefCell<Vec<H>>>,
    // The run-wide untitled-document counter (#587 r2 m10): "DocumentN" is
    // minted once for the whole run, however many windows create one.
    next_title: u32,
}

impl<V, H> Default for Windows<V, H> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            next_id: 0,
            next_seq: 0,
            links: Vec::new(),
            handles: Default::default(),
            next_title: 1,
        }
    }
}

impl<V, H> Windows<V, H> {
    pub(crate) fn register(&mut self, view: V, handle: H) -> u64
    where
        H: PartialEq + Clone,
    {
        self.next_id += 1;
        let id = self.next_id;
        let seq = self.next_seq;
        self.next_seq += 1;
        if let std::result::Result::Ok(mut handles) = self.handles.try_borrow_mut() {
            if !handles.contains(&handle) {
                handles.push(handle.clone());
            }
        }
        self.entries.push(Entry {
            id,
            view,
            handle,
            seq,
            persisted: Vec::new(),
        });
        self.selected = id;
        id
    }

    /// Removes the window. The selection falls back to the most recently
    /// registered survivor, so verbs keep working after the selected window
    /// closes. Returns the new selection; `None` when no window is left.
    pub(crate) fn unregister(&mut self, id: u64) -> Option<u64>
    where
        H: PartialEq,
    {
        if let Some(handle) = self.entries.iter().find(|e| e.id == id).map(|e| &e.handle) {
            if let std::result::Result::Ok(mut handles) = self.handles.try_borrow_mut() {
                handles.retain(|h| h != handle);
            }
        }
        self.entries.retain(|e| e.id != id);
        if self.selected == id {
            self.selected = self.entries.last().map(|e| e.id).unwrap_or(0);
        }
        (self.selected != 0).then_some(self.selected)
    }

    pub(crate) fn select(&mut self, id: u64) -> Result<(), String> {
        if self.entries.iter().any(|e| e.id == id) {
            self.selected = id;
            Ok(())
        } else {
            Err(format!("no window {id}"))
        }
    }

    pub(crate) fn selected(&self) -> Option<u64> {
        (self.selected != 0).then_some(self.selected)
    }

    /// Whether `id` is the only registered window: its close is the app's
    /// quit and keeps the single-window behaviour exactly.
    pub(crate) fn is_alone(&self, id: u64) -> bool {
        self.entries.len() == 1 && self.entries[0].id == id
    }

    pub(crate) fn count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn seq_of(&self, id: u64) -> usize {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map_or(0, |e| e.seq)
    }

    /// What every OTHER window last persisted, for the union a session write
    /// appends after its own tabs.
    pub(crate) fn others_persisted(&self, id: u64) -> Vec<PersistTab> {
        self.entries
            .iter()
            .filter(|e| e.id != id)
            .flat_map(|e| e.persisted.clone())
            .collect()
    }

    /// The window's own tabs alone, never the union it wrote.
    pub(crate) fn set_persisted(&mut self, id: u64, tabs: Vec<PersistTab>) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.persisted = tabs;
        }
    }

    pub(crate) fn get(&self, id: u64) -> Option<&Entry<V, H>> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The shared handle list the control pump clones: always current,
    /// readable without borrowing the App.
    pub(crate) fn handle_list(&self) -> std::rc::Rc<std::cell::RefCell<Vec<H>>>
    where
        H: Clone,
    {
        self.handles.clone()
    }

    pub(crate) fn entries(&self) -> &[Entry<V, H>] {
        &self.entries
    }
}

/// The run's registry: a `WeakEntity` to update the view, a handle to update
/// its window.
pub(crate) type Registry = Windows<WeakEntity<Docxy>, AnyWindowHandle>;

impl Global for Registry {}

fn with<R>(cx: &App, f: impl FnOnce(&Registry) -> R) -> Option<R> {
    cx.try_global::<Registry>().map(f)
}

/// The registry is set in `main` before the first window; a missing one is a
/// programming error, so the mutation helpers take the infallible
/// `global_mut` after `try_global` proved it there (there is no
/// `try_global_mut` at the pinned gpui rev).
fn update<R>(cx: &mut App, f: impl FnOnce(&mut Registry) -> R) -> Option<R> {
    cx.try_global::<Registry>()?;
    Some(f(cx.global_mut::<Registry>()))
}

pub(crate) fn register(cx: &mut App, view: &Entity<Docxy>, handle: AnyWindowHandle) -> u64 {
    update(cx, |w| w.register(view.downgrade(), handle)).unwrap_or(0)
}

pub(crate) fn unregister(cx: &mut App, id: u64) -> Option<u64> {
    update(cx, |w| w.unregister(id)).flatten()
}

pub(crate) fn select(cx: &mut App, id: u64) -> Result<(), String> {
    update(cx, |w| w.select(id)).unwrap_or_else(|| Err("the app is gone".into()))
}

pub(crate) fn selected(cx: &App) -> Option<u64> {
    with(cx, |w| w.selected()).flatten()
}

/// The selected window's view, for asking it to persist after another
/// window closed.
pub(crate) fn selected_view(cx: &App) -> Option<WeakEntity<Docxy>> {
    let sel = selected(cx)?;
    with(cx, |w| w.get(sel).map(|e| e.view.clone())).flatten()
}

pub(crate) fn is_alone(cx: &App, id: u64) -> bool {
    with(cx, |w| w.is_alone(id)).unwrap_or(true)
}

pub(crate) fn count(cx: &App) -> usize {
    with(cx, |w| w.count()).unwrap_or(0)
}

pub(crate) fn seq_of(cx: &App, id: u64) -> usize {
    with(cx, |w| w.seq_of(id)).unwrap_or(0)
}

pub(crate) fn others_persisted(cx: &App, id: u64) -> Vec<PersistTab> {
    with(cx, |w| w.others_persisted(id)).unwrap_or_default()
}

pub(crate) fn set_persisted(cx: &mut App, id: u64, tabs: Vec<PersistTab>) {
    update(cx, |w| w.set_persisted(id, tabs));
}

/// Take ownership of a control server link for the whole run (see the
/// field): attach stores it here, not in a window's view.
pub(crate) fn keep_link(cx: &mut App, link: crate::control::ControlLink) {
    update(cx, |w| w.links.push(link));
}

/// The always-current handle list for the control pump's candidate chain
/// (#587 r2 M1). A fresh Rc when no registry exists.
pub(crate) fn handle_list(cx: &App) -> std::rc::Rc<std::cell::RefCell<Vec<AnyWindowHandle>>> {
    with(cx, |w| w.handle_list())
        .unwrap_or_else(|| std::rc::Rc::new(std::cell::RefCell::new(Vec::new())))
}

/// Whether the window is still registered.
pub(crate) fn is_registered(cx: &App, id: u64) -> bool {
    with(cx, |w| w.get(id).is_some()).unwrap_or(false)
}

/// Mint the next untitled document title for the whole run (#587 r2 m10):
/// one counter, so two windows cannot mint the same "DocumentN". Without a
/// registry (tests) a throwaway counter answers.
pub(crate) fn next_document_title(cx: &mut App) -> String {
    let mut next = 1;
    update(cx, |w| {
        let title = crate::doc_name::next_document_title(&mut w.next_title);
        next = w.next_title;
        title
    })
    .unwrap_or_else(|| crate::doc_name::next_document_title(&mut next))
}

/// Raise the run-wide title counter to at least `at_least`: the first
/// window seeds it from the restored session's untitled documents.
pub(crate) fn seed_document_titles(cx: &mut App, at_least: u32) {
    update(cx, |w| {
        if w.next_title < at_least {
            w.next_title = at_least;
        }
    });
}

/// A descriptor copy for iterating without holding the global borrowed.
pub(crate) fn entries_snapshot(cx: &App) -> Vec<(u64, WeakEntity<Docxy>, AnyWindowHandle)> {
    with(cx, |w| {
        w.entries()
            .iter()
            .map(|e| (e.id, e.view.clone(), e.handle))
            .collect()
    })
    .unwrap_or_default()
}

/// The selected window's dispatch target.
pub(crate) struct Target {
    pub(crate) view: WeakEntity<Docxy>,
    pub(crate) handle: AnyWindowHandle,
}

/// Resolve the selected window for one control request. `None` when no
/// window is registered — the app is gone.
pub(crate) fn selected_target(cx: &App) -> Option<Target> {
    let sel = selected(cx)?;
    with(cx, |w| {
        w.get(sel).map(|e| Target {
            view: e.view.clone(),
            handle: e.handle,
        })
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(Point::new(px(x), px(y)), Size::new(px(w), px(h)))
    }

    fn sidecar(title: &str) -> PersistTab {
        PersistTab {
            kind: crate::Kind::Docx,
            title: title.into(),
            path: None,
            dirty: true,
            hot: None,
            unreadable: vec![],
            markdown: false,
            load_failed: None,
            read_only: false,
            protected: false,
            repaired: false,
            stamp: None,
            converted: None,
            binary_source: false,
            compat: false,
        }
    }

    #[test]
    fn tile_bounds_splits_width_evenly_and_gives_remainder_to_last() {
        let tiles = tile_bounds(display(0., 0., 1920., 1080.), 3);
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[0].size.width, px(640.));
        assert_eq!(tiles[1].size.width, px(640.));
        assert_eq!(tiles[2].size.width, px(640.));
        assert_eq!(tiles[0].origin.x, px(0.));
        assert_eq!(tiles[1].origin.x, px(640.));
        assert_eq!(tiles[2].origin.x, px(1280.));
        for t in &tiles {
            assert_eq!(t.size.height, px(1080.));
            assert_eq!(t.origin.y, px(0.));
        }
        let tiles = tile_bounds(display(0., 0., 1000., 700.), 3);
        assert_eq!(tiles[0].size.width, px(333.));
        assert_eq!(tiles[1].size.width, px(333.));
        assert_eq!(tiles[2].size.width, px(334.));
        assert_eq!(tiles[2].origin.x, px(666.));
        assert_eq!(tiles[2].origin.x + tiles[2].size.width, px(1000.));
    }

    #[test]
    fn tile_bounds_zero_and_one() {
        assert!(tile_bounds(display(0., 0., 1920., 1080.), 0).is_empty());
        let one = tile_bounds(display(0., 0., 1920., 1080.), 1);
        assert_eq!(one, vec![display(0., 0., 1920., 1080.)]);
    }

    #[test]
    fn cascade_offsets_and_stays_inside_display() {
        let screen = display(100., 100., 1600., 900.);
        let source = display(200., 150., 1180., 800.);
        let moved = cascade(source, screen);
        assert_eq!(moved.origin, Point::new(px(230.), px(180.)));
        assert_eq!(moved.size, source.size);
        // Clamped at the display's right/bottom edge, size intact.
        let moved = cascade(display(1490., 850., 1180., 800.), screen);
        assert_eq!(moved.origin, Point::new(px(520.), px(200.)));
        assert_eq!(moved.size, source.size);
    }

    #[test]
    fn ids_are_never_reused_and_seq_is_stable() {
        let mut w: Windows<u8, u8> = Windows::default();
        let a = w.register(0, 0);
        let b = w.register(0, 0);
        assert_eq!((a, b), (1, 2));
        assert_eq!(w.unregister(a), Some(b));
        let c = w.register(0, 0);
        assert_eq!(c, 3, "ids are never reused");
        assert_eq!(
            w.seq_of(c),
            2,
            "seq keeps counting so sidecar names never collide"
        );
        assert_eq!(w.seq_of(b), 1);
    }

    #[test]
    fn unregister_selects_the_most_recent_survivor() {
        let mut w: Windows<u8, u8> = Windows::default();
        let a = w.register(0, 0);
        let b = w.register(0, 0);
        let c = w.register(0, 0);
        assert_eq!(w.selected(), Some(c));
        assert_eq!(w.unregister(c), Some(b));
        assert_eq!(w.unregister(b), Some(a));
        assert_eq!(w.unregister(a), None, "no window is left");
    }

    #[test]
    fn select_refuses_unknown_ids() {
        let mut w: Windows<u8, u8> = Windows::default();
        let a = w.register(0, 0);
        assert!(w.select(a).is_ok());
        assert!(w.select(999).is_err());
    }

    #[test]
    fn others_persisted_excludes_the_asker() {
        let mut w: Windows<u8, u8> = Windows::default();
        let a = w.register(0, 0);
        let b = w.register(0, 0);
        w.set_persisted(a, vec![sidecar("a1"), sidecar("a2")]);
        w.set_persisted(b, vec![sidecar("b1")]);
        let others = w.others_persisted(a);
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].title, "b1");
        let others = w.others_persisted(b);
        assert_eq!(others.len(), 2);
        assert!(others.iter().all(|p| p.title.starts_with('a')));
    }

    /// The pump's candidate chain reads the shared handle list, which
    /// register/unregister keep current — a window opened or closed by the
    /// UI between verbs is always reachable (#587 r2 M1).
    #[test]
    fn the_shared_handle_list_tracks_register_and_unregister() {
        let mut w: Windows<u8, u8> = Windows::default();
        let list = w.handle_list();
        w.register(0, 7);
        w.register(0, 9);
        assert_eq!(*list.borrow(), vec![7, 9]);
        w.unregister(1);
        assert_eq!(*list.borrow(), vec![9]);
        w.unregister(2);
        assert!(list.borrow().is_empty());
    }
}
