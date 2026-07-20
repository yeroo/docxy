# lookxy Folder Tree + Pane Navigation & Help — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development
> or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.
> (Executed inline this session — subagent cap reached.)

**Goal:** Add reverse pane navigation + an F1/`?` help overlay, and render the
folder pane as a collapsible tree with persisted expand state.

**Architecture:** Feature A (navigation/help) touches only `lookxy` (focus keys
in `ui/mod.rs`, a new `ui/help.rs` overlay, `App::help` state). Feature B adds an
`is_expanded` column in `mailcore`'s store, a tree-flatten step in `lookxy`, and
folder-pane keys + indented/chevron rendering. Build A first (unblocking), then B.

**Tech Stack:** Rust 2024, ratatui/crossterm, rusqlite. No new deps.

## Global Constraints

- MSRV 1.88; `mailcore` headless, `lookxy` TUI.
- Full workspace green, `clippy --all-targets -D warnings` clean, `fmt` clean.
- Idempotent migration pattern: `let _ = conn.execute("ALTER TABLE … ADD COLUMN …", []);`
- Build via `bash "$LCARGO" <args>` (sandbox-disabled Bash). Each task committed.
- Spec: `docs/superpowers/specs/2026-07-20-lookxy-folder-tree-and-nav-design.md`.

---

## Task A1: Reverse pane navigation

**Files:**
- Modify: `lookxy/src/ui/mod.rs` (`handle_key`, add `cycle_focus_back`, focus-back arm)
- Test: `lookxy/src/ui/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `App::focus: Pane`, `cycle_focus`.
- Produces: `Shift+Tab`, `←`/`h`, and `Esc` move focus backward (Reading→List→Folders).

- [ ] **Step 1 — failing tests.** In `ui/mod.rs` tests:

```rust
#[test]
fn shift_tab_reverse_cycles_panes() {
    let mut app = App::for_test_with_seeded_store();
    app.focus = Pane::Reading;
    handle_key(&mut app, KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.focus, Pane::List);
    handle_key(&mut app, KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.focus, Pane::Folders);
    handle_key(&mut app, KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.focus, Pane::Reading);
}

#[test]
fn left_and_esc_step_focus_back_from_reading_and_list() {
    let mut app = App::for_test_with_seeded_store();
    app.focus = Pane::Reading;
    handle_key(&mut app, KeyEvent::from(KeyCode::Char('h')));
    assert_eq!(app.focus, Pane::List);
    handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.focus, Pane::Folders);
    // At Folders, h/← is reserved for tree collapse — focus stays put.
    handle_key(&mut app, KeyEvent::from(KeyCode::Left));
    assert_eq!(app.focus, Pane::Folders);
}
```

- [ ] **Step 2 — run, expect FAIL** (`BackTab` unhandled; `h` falls to `on_key_char`).
- [ ] **Step 3 — implement.** Add a `cycle_focus_back` and route the keys.
  In `handle_key`'s `match key.code`:
  - `KeyCode::BackTab => cycle_focus_back(app),`
  - Add a focus-back arm for `←`/`h` guarded to `app.focus != Pane::Folders`
    (so the Folders pane keeps `h`/`←` for collapse in B3), placed **before**
    the `Char('h')`→`on_key_char` path and the folder/list `Left` movement.
  - For `Esc`: add, **after** the existing reminder-dismiss and category-filter
    Esc handlers and the modal routers, an arm — when `app.focus != Pane::Folders`
    and `!app.is_capturing_text()` — that steps focus back.

```rust
fn cycle_focus_back(app: &mut App) {
    app.focus = match app.focus {
        Pane::Folders => Pane::Reading,
        Pane::List => Pane::Folders,
        Pane::Reading => Pane::List,
    };
}
fn focus_back(app: &mut App) {
    app.focus = match app.focus {
        Pane::Reading => Pane::List,
        Pane::List | Pane::Folders => Pane::Folders,
    };
}
```

  Wire `←`/`h` (when not in Folders) and the guarded `Esc` to `focus_back`.

- [ ] **Step 4 — run, expect PASS.** Full: `bash "$LCARGO" test -p lookxy`.
- [ ] **Step 5 — commit:** `lookxy: reverse pane navigation (Shift-Tab, ←/h, Esc)`.

---

## Task A2: Help overlay (F1 / ?)

**Files:**
- Create: `lookxy/src/ui/help.rs`
- Modify: `lookxy/src/ui/mod.rs` (`pub mod help;`, route + draw), `lookxy/src/app.rs`
  (`help: bool`, `open_help`/`close_help`, `is_capturing_text` includes help)

**Interfaces:**
- Consumes: `App`, `centered_rect`.
- Produces: `App::help: bool`; `help::draw(f, app)`; `help::handle_key`.

- [ ] **Step 1 — failing tests.** In `help.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn f1_opens_and_esc_closes() {
        let mut app = App::for_test_with_seeded_store();
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::F(1)));
        assert!(app.help);
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(!app.help);
    }

    #[test]
    fn question_mark_does_not_open_help_while_searching() {
        let mut app = App::for_test_with_seeded_store();
        app.start_search();
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('?')));
        assert!(!app.help);
    }

    #[test]
    fn q_over_help_closes_it_rather_than_quitting() {
        let mut app = App::for_test_with_seeded_store();
        app.open_help();
        assert!(app.is_capturing_text()); // guards global q-quit
    }

    #[test]
    fn draw_lists_group_headers() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::for_test_with_seeded_store();
        app.open_help();
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Global"));
        assert!(text.contains("Message list"));
    }
}
```

- [ ] **Step 2 — run, expect FAIL** (`App::help` etc. missing).
- [ ] **Step 3 — implement.**
  - `app.rs`: add `pub help: bool` (init `false`); `pub fn open_help(&mut self){self.help=true;}`
    / `close_help`; add `|| self.help` to `is_capturing_text`.
  - `help.rs`: a `const HELP: &[(&str,&[(&str,&str)])]` of (group, [(keys, desc)])
    per the spec's list; `draw(f,&App)` returns early unless `app.help`, renders a
    `centered_rect(70,80,...)` bordered panel titled `"Help — Esc to close"` with
    one `Line` per group header + its rows; `handle_key(app,key)` closes on
    `Esc | F(1) | Char('?') | Char('q')`.
  - `ui/mod.rs`: `pub mod help;`. In `handle_key`, **before** the pane match and
    modal routers: if `app.help { help::handle_key(app,key); return; }`. Then an
    opener: `KeyCode::F(1) => app.open_help()`, and in the `Char('?')` path open
    help only when `!app.is_capturing_text()`. In `draw`, call `help::draw(f,&*app)`
    last (top-most) in both Mail and Calendar branches.
  - `F1`/`?` must open from both Mail and Calendar modes — route the opener ahead
    of the `mode == Calendar` early return.

- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: F1/? help overlay with grouped shortcuts`.

---

## Task B1: Store `is_expanded` column

**Files:**
- Modify: `mailcore/src/store/mod.rs` (migration, `FolderRow.is_expanded`,
  `folders()` select, `set_folder_expanded`; `upsert_folder` unchanged — must
  NOT touch the column)
- Test: `mailcore/src/store/mod.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `FolderRow.is_expanded: bool`; `Store::set_folder_expanded(&str,bool)`.

- [ ] **Step 1 — failing tests.**

```rust
#[test]
fn folder_expanded_persists_and_survives_resync() {
    let s = Store::open_in_memory().unwrap();
    s.upsert_folder(&folder("F1", "Inbox", None)).unwrap();
    assert!(!s.folders().unwrap()[0].is_expanded); // default collapsed
    s.set_folder_expanded("F1", true).unwrap();
    assert!(s.folders().unwrap()[0].is_expanded);
    // A re-sync upsert must NOT reset the flag.
    s.upsert_folder(&folder("F1", "Inbox", None)).unwrap();
    assert!(s.folders().unwrap()[0].is_expanded);
}
```
(Use the module's existing folder-construction helper / `MailFolder { .. }` literal.)

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.**
  - In the migration block: `let _ = self.conn.execute("ALTER TABLE folders ADD COLUMN is_expanded INTEGER NOT NULL DEFAULT 0", []);`
  - `FolderRow`: add `pub is_expanded: bool` (place after `well_known_name`); update
    the `folders()` SELECT to include `is_expanded` and set it via
    `row.get::<_, i64>(N)? != 0`; add the column index carefully (shift `sort_order`).
  - Add `pub fn set_folder_expanded(&self, id:&str, expanded:bool) -> Result<(),StoreError>`
    running `UPDATE folders SET is_expanded=?2 WHERE id=?1` with `expanded as i64`.
  - Leave `upsert_folder` as-is (no `is_expanded` in INSERT or ON CONFLICT).
  - Ripple: every `FolderRow { .. }` literal in the codebase gains
    `is_expanded: false` (compiler backstop lists them; most are in lookxy tests).

- [ ] **Step 4 — run, expect PASS:** `bash "$LCARGO" test -p mailcore folder`.
- [ ] **Step 5 — commit:** `mailcore: persist folder is_expanded (preserved across sync)`.

---

## Task B2: Tree flatten + visible-row state

**Files:**
- Create: `lookxy/src/ui/foldertree.rs` (`VisibleFolder`, `build_visible`)
- Modify: `lookxy/src/app.rs` (`visible_folders`, rebuild in `reload_folders`,
  `folder_index` semantics, selection clamp)
- Modify: `lookxy/src/ui/mod.rs` (`pub mod foldertree;`)

**Interfaces:**
- Produces:
  ```rust
  pub struct VisibleFolder { pub row: FolderRow, pub depth: usize,
                             pub has_children: bool, pub expanded: bool }
  pub fn build_visible(folders: &[FolderRow]) -> Vec<VisibleFolder>;
  ```
  `App::visible_folders: Vec<VisibleFolder>`; `App::rebuild_visible_folders()`.

- [ ] **Step 1 — failing tests** (`foldertree.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::store::FolderRow;
    fn fr(id:&str, parent:Option<&str>, exp:bool) -> FolderRow {
        FolderRow { id:id.into(), parent_id:parent.map(Into::into), display_name:id.into(),
            total_count:0, unread_count:0, delta_link:None, well_known_name:None,
            sort_order:None, is_expanded:exp }
    }
    #[test]
    fn nests_children_under_expanded_parents_and_hides_collapsed() {
        // Inbox(expanded) -> [EPAM(collapsed) -> ADPT], Sent(top, leaf)
        let rows = vec![
            fr("Inbox", None, true), fr("EPAM", Some("Inbox"), false),
            fr("ADPT", Some("EPAM"), false), fr("Sent", None, false),
        ];
        let v = build_visible(&rows);
        let names: Vec<_> = v.iter().map(|x|(x.row.id.as_str(), x.depth, x.has_children)).collect();
        assert_eq!(names, vec![("Inbox",0,true), ("EPAM",1,true), ("Sent",0,false)]);
        // ADPT hidden: EPAM collapsed.
    }
    #[test]
    fn top_level_when_parent_outside_set() {
        let rows = vec![ fr("Inbox", Some("msgroot"), false) ]; // parent not a row
        assert_eq!(build_visible(&rows).len(), 1);
        assert_eq!(build_visible(&rows)[0].depth, 0);
    }
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement `build_visible`:** index children by `parent_id`;
  roots = rows whose `parent_id` is `None` or not a known id, in input order;
  DFS pushing each root then (if `expanded`) recursing children in input order;
  `has_children` = any row lists this id as `parent_id`; `depth` from recursion.
- [ ] **Step 4 — app wiring + tests.** In `app.rs`: add `visible_folders`;
  `rebuild_visible_folders()` calls `build_visible(&self.folders)`; call it at the
  end of `reload_folders`. `folder_index` now indexes `visible_folders`; update
  `move_selection`'s Folders arm and `selected_folder` derivation to read
  `visible_folders[folder_index].row.id`. After a rebuild, if `selected_folder`
  isn't among the visible rows, clamp `folder_index` to the nearest valid row.
  Add an app test: seed Inbox(expanded)+child+Sent, assert `visible_folders` ids.
- [ ] **Step 5 — run, expect PASS.**
- [ ] **Step 6 — commit:** `lookxy: flatten folders into a visible tree (depth + chevrons)`.

---

## Task B3: Folder-pane expand/collapse keys

**Files:**
- Modify: `lookxy/src/app.rs` (`toggle_selected_folder`, `expand_selected`,
  `collapse_or_parent`), `lookxy/src/ui/mod.rs` (Folders-pane key arms)

**Interfaces:**
- Consumes: B1 `set_folder_expanded`, B2 `visible_folders`/`rebuild_visible_folders`.
- Produces: expand/collapse behavior on `→`/`l`, `←`/`h`, `Space` in Folders pane.

- [ ] **Step 1 — failing tests** (`app.rs`): seed Inbox(collapsed)+EPAM child.

```rust
#[test]
fn expand_and_collapse_selected_folder() {
    let mut app = App::for_test_with_seeded_store_tree(); // helper seeds Inbox->EPAM
    app.focus = Pane::Folders; app.folder_index = 0; // Inbox
    app.expand_selected();
    assert!(app.visible_folders.iter().any(|v| v.row.display_name == "EPAM"));
    app.collapse_or_parent();
    assert!(!app.visible_folders.iter().any(|v| v.row.display_name == "EPAM"));
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Each method reads the selected `visible_folders`
  row, calls `set_folder_expanded(id, ...)` on the store, then
  `rebuild_visible_folders()`:
  - `expand_selected` — if `has_children && !expanded` set expanded true.
  - `collapse_or_parent` — if `has_children && expanded` set false; else move
    `folder_index` to the parent row's index (find by `row.parent_id`).
  - `toggle_selected_folder` — flip when `has_children`.
  In `ui/mod.rs`, Folders-pane arms (only when `app.focus == Pane::Folders`):
  `Right|Char('l') => app.expand_selected()`, `Left|Char('h') => app.collapse_or_parent()`,
  `Char(' ') => app.toggle_selected_folder()`. Ensure these precede the generic
  `Char(c) => on_key_char` and the A1 focus-back `←`/`h` arm (which is guarded to
  non-Folders, so no conflict).
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: folder-pane expand/collapse keys (→/l, ←/h, Space)`.

---

## Task B4: Indented + chevron rendering

**Files:**
- Modify: `lookxy/src/ui/folders.rs` (render `visible_folders` with indent+chevron)

- [ ] **Step 1 — failing render test** (`folders.rs`): seed Inbox(expanded)+EPAM
  child; draw; assert the buffer contains a chevron (`▾`/`▸`) and the child name
  appears after indentation (e.g. assert a line containing `"  "` + `"EPAM"`).
- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Iterate `app.visible_folders` (not `app.folders`):
  `format!("{}{} {}{}", "  ".repeat(v.depth), chevron(v), name, count)` where
  `chevron` = `▾` expanded / `▸` collapsed / ` ` leaf, and count = `" (N)"` when
  `unread_count>0`. `ListState` selects `folder_index`.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: render the folder pane as an indented tree`.

---

## Task B5: First-run default (Inbox expanded)

**Files:**
- Modify: `lookxy/src/config.rs` (`folder_tree_initialized` flag + persist),
  `lookxy/src/app.rs` (one-time expand of Inbox in `reload_folders`/init)

**Interfaces:**
- Consumes: B1 `set_folder_expanded`, config persistence pattern (`persist_threaded_to`).

- [ ] **Step 1 — failing tests** (`app.rs`): with a fresh store+config, after the
  first `reload_folders`, the Inbox folder is expanded exactly once; simulate the
  user collapsing it and reloading again — it stays collapsed (flag consumed).

```rust
#[test]
fn inbox_expands_once_on_first_run_then_respects_user() {
    let mut app = App::for_test_with_seeded_store_tree_collapsed();
    app.reload_folders();
    assert!(inbox(&app).is_expanded);
    app.collapse_or_parent(); // user collapses inbox
    app.reload_folders();
    assert!(!inbox(&app).is_expanded); // not re-expanded
}
```

- [ ] **Step 2 — run, expect FAIL.**
- [ ] **Step 3 — implement.** Add `folder_tree_initialized: bool` (default false)
  to `Config` + `persist_folder_tree_initialized_to(path,bool)` (mirror
  `persist_threaded_to`). In `reload_folders` (after loading folders, before
  rebuild): if `!config.folder_tree_initialized`, find the folder with
  `well_known_name == "inbox"`, `set_folder_expanded(id,true)`, set the flag true
  and persist (best-effort when `config_path` is `None`). Reload the folder rows
  so the expand is reflected, then `rebuild_visible_folders`.
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit:** `lookxy: expand Inbox by default on first run`.

---

## Self-Review

- **Spec coverage:** A1 (reverse nav) ✓, A2 (help) ✓, B1 (persist) ✓, B2 (tree) ✓,
  B3 (keys) ✓, B4 (render) ✓, B5 (first-run) ✓. Every spec section maps to a task.
- **Type consistency:** `VisibleFolder`/`build_visible` names match across B2–B4;
  `set_folder_expanded`/`is_expanded` match across B1/B3/B5; `focus_back`/
  `cycle_focus_back` local to A1.
- **Ripple risk:** B1 adds a non-Default field to `FolderRow` — every literal
  breaks; fix with the compiler backstop (`is_expanded: false`), mostly lookxy
  tests. Note in B1.
- After each task: `bash "$LCARGO" test --workspace`, `clippy --all-targets -D warnings`,
  `fmt --all -- --check`. Final whole-branch review (direct), then push to PR #21.
