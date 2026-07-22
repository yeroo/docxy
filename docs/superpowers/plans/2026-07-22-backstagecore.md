# backstagecore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the File backstage (menu + folder browser + preview + Save As) and the no-file start dialog into a shared `backstagecore` crate used by docxy, xlsxy, and yppxy.

**Architecture:** `backstagecore` owns all backstage state, folder browsing, Save-As name editing, preview caching/scroll, click-rect layout, and rendering. Each app supplies only format-specific content through a `BackstageHost` trait (extensions, default save name, preview lines, info lines, accent) and reacts to a `BackstageEvent` enum with its own file I/O. A `Start` type renders a shared centered accent card from an app-supplied item list and returns the chosen index. Mirrors the completed `ribboncore` refactor (thin wrappers, verbatim-port migration).

**Tech Stack:** Rust (edition 2024, workspace), ratatui 0.29, crossterm (re-exported by ratatui), unicode-width 0.2.

## Global Constraints

- Workspace edition 2024, MSRV 1.88. Build ONLY via `bash "$LCARGO"` with `dangerouslyDisableSandbox: true` (bare `cargo` → os error 448). `LCARGO` is already exported in the environment.
- New crate `backstagecore` uses a literal `version = "0.1.0"` in its `Cargo.toml` (NOT `version.workspace = true`) — matches the mailcore/ribboncore pattern; path deps reference it as `backstagecore = { version = "0.1.0", path = "../backstagecore" }`.
- Add `backstagecore` to the root `Cargo.toml` `[workspace] members` list.
- ratatui deps in `backstagecore/Cargo.toml`: `ratatui = "0.29"`, `unicode-width = "0.2"`.
- The shared menu is EXACTLY these 7 items in order: `New, Open, Info, Save, SaveAs, Export, Exit`. docxy's former separate `Print` is dropped (Export covers it).
- Preserve behavior verbatim: folders-first sort, `~$…` lock files listed-but-not-openable, case-insensitive extension match, guarded-New second-click, Save-As Tab-toggles-focus + typed-name caret editing, preview scroll (↑↓ PageUp/PageDown Home/End).
- Accent color is `host.accent()` everywhere the backstage currently hardcodes `Color::Cyan` — docxy is `Color::LightBlue`, xlsxy `Color::Green`, yppxy `Color::Yellow` (match each app's ribbon accent).
- Every task: `bash "$LCARGO" fmt` clean and `bash "$LCARGO" clippy -p <crate> -- -D warnings` clean before commit.
- Do NOT change any file format, load/save logic, or preview *content* — those stay in each app's core (docxcore/gridcore/projcore).
- lookxy is out of scope: leave `lookxy/src/ui/backstage.rs` untouched.

---

### Task 1: Create `backstagecore` crate — state + folder browser

**Files:**
- Create: `backstagecore/Cargo.toml`
- Create: `backstagecore/src/lib.rs` (module wiring + re-exports)
- Create: `backstagecore/src/state.rs` (Item, Entry, Pane, Backstage, browser)
- Modify: root `Cargo.toml` (add member)

**Interfaces:**
- Produces:
  - `enum Item { New, Open, Info, Save, SaveAs, Export, Exit }` + `pub const ITEMS: [Item; 7]` + `impl Item { pub fn label(self) -> &'static str }`
  - `struct Entry { pub name: String, pub is_dir: bool, pub is_parent: bool, pub size: u64, pub locked: bool }` + `impl Entry { pub fn size_str(&self) -> String }`
  - `enum Pane { Menu, Browser, Preview, SaveAs }`
  - `struct Backstage { pub item: Item, pub pane: Pane, pub dir: PathBuf, pub entries: Vec<Entry>, pub sel: usize, exts: &'static [&'static str], pub preview: Vec<String>, pub preview_path: Option<PathBuf>, pub preview_w: usize, pub preview_scroll: usize, pub name_input: String, pub name_cursor: usize, pub name_focus: bool, layout: BackstageLayout }`
  - `struct BackstageLayout { pub list_start: usize, pub save_btn: Rect, pub name_top: u16, pub name_x0: u16, pub preview_h: usize }` (Default-derived; filled by draw in Task 3, read by mouse in Task 2 — declared here so the struct compiles)
  - `impl Backstage`: `pub fn open(dir: PathBuf, exts: &'static [&'static str]) -> Backstage`, `pub fn refresh(&mut self)`, `pub fn selected(&self) -> Option<&Entry>`, `pub fn selected_file(&self) -> Option<PathBuf>`, `pub fn move_sel(&mut self, down: bool)`, `pub fn enter(&mut self) -> Option<PathBuf>`, `pub fn go_up(&mut self)`, `pub fn menu_move(&mut self, down: bool)`

- [ ] **Step 1: Create the crate manifest**

Create `backstagecore/Cargo.toml`:

```toml
[package]
name = "backstagecore"
version = "0.1.0"
edition = "2024"

[dependencies]
ratatui = "0.29"
unicode-width = "0.2"
```

- [ ] **Step 2: Register the crate in the workspace**

In the root `Cargo.toml`, add `"backstagecore"` to the `[workspace] members` array (alphabetically near `ribboncore`). Match the existing formatting exactly.

- [ ] **Step 3: Write `state.rs` with the ported, generalized state**

Create `backstagecore/src/state.rs`. Port docxy's `docxy/src/backstage.rs` lines 11–228 VERBATIM with these mechanical changes:
- Add `use ratatui::layout::Rect;` and the `BackstageLayout` struct (see Interfaces) with `#[derive(Debug, Clone, Copy, Default)]`.
- `Item`: keep `New, Open, Info, Save, SaveAs, Export, Exit` (delete the `Print` variant and its `label` arm); `ITEMS` becomes `[Item; 7]` without `Print`.
- `Backstage`: add fields `exts: &'static [&'static str]` and `layout: BackstageLayout` (both non-`pub`; add `pub layout` accessor is unnecessary — Task 2/3 are in-crate).
- `open(dir)` → `open(dir: PathBuf, exts: &'static [&'static str])`; store `exts`, init `layout: BackstageLayout::default()`.
- `refresh`: replace the hardcoded `name.to_ascii_lowercase().ends_with(".docx")` test with:

```rust
} else if self.exts.iter().any(|ext| {
    let dot = format!(".{}", ext.to_ascii_lowercase());
    name.to_ascii_lowercase().ends_with(&dot)
}) {
```

- Rename `selected_docx` → `selected_file` (body unchanged).
- Add `menu_move` (ported from docxy `main.rs` `bs_menu_move`, lines 1638–1652, but as a method on `Backstage` using `self.item`):

```rust
pub fn menu_move(&mut self, down: bool) {
    let i = ITEMS.iter().position(|x| *x == self.item).unwrap_or(0);
    let ni = if down { (i + 1).min(ITEMS.len() - 1) } else { i.saturating_sub(1) };
    self.item = ITEMS[ni];
}
```

- [ ] **Step 4: Write `lib.rs`**

Create `backstagecore/src/lib.rs`:

```rust
//! Shared File backstage (menu + folder browser + preview + Save As) and the
//! no-file start dialog, used by docxy/xlsxy/yppxy. The crate owns all state,
//! navigation, layout and rendering; each app supplies format-specific content
//! via [`BackstageHost`] and acts on the returned [`BackstageEvent`].
mod state;
pub use state::{Backstage, BackstageLayout, Entry, Item, Pane, ITEMS};
```

- [ ] **Step 5: Port the unit tests + add multi-extension coverage**

In `state.rs`, port docxy's `backstage.rs` tests (lines 230–281) with: `ITEMS.len()` → 7; `Backstage::open(tmp.clone())` → `Backstage::open(tmp.clone(), &["docx"])`. Then add a multi-extension test:

```rust
#[test]
fn lists_multiple_extensions_case_insensitively() {
    let tmp = std::env::temp_dir().join("bscore_multiext");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("a.XLSX"), b"x").unwrap();
    std::fs::write(tmp.join("b.csv"), b"x").unwrap();
    std::fs::write(tmp.join("c.docx"), b"x").unwrap();
    let bs = Backstage::open(tmp.clone(), &["xlsx", "csv"]);
    let names: Vec<&str> = bs.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"a.XLSX")); // case-insensitive
    assert!(names.contains(&"b.csv"));
    assert!(!names.contains(&"c.docx")); // not in ext list
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn menu_move_walks_and_clamps() {
    let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
    bs.item = Item::New;
    bs.menu_move(false); // already first — clamps
    assert_eq!(bs.item, Item::New);
    bs.menu_move(true);
    assert_eq!(bs.item, Item::Open);
}
```

- [ ] **Step 6: Build, test, lint**

Run: `bash "$LCARGO" test -p backstagecore` → all pass.
Run: `bash "$LCARGO" clippy -p backstagecore -- -D warnings` → clean.
Run: `bash "$LCARGO" fmt` → clean.

- [ ] **Step 7: Commit**

```bash
git add backstagecore/Cargo.toml backstagecore/src/state.rs backstagecore/src/lib.rs Cargo.toml
git commit -m "backstagecore: state + folder browser (ext-parameterized)"
```

---

### Task 2: Interaction — `BackstageHost` trait, `BackstageEvent`, key/mouse

**Files:**
- Create: `backstagecore/src/host.rs` (trait + event enum)
- Create: `backstagecore/src/input.rs` (key + mouse handlers, guarded-New, Save-As typing)
- Modify: `backstagecore/src/lib.rs` (wire modules + re-exports)

**Interfaces:**
- Consumes: `Backstage`, `Item`, `Pane`, `ITEMS`, `BackstageLayout` (Task 1).
- Produces:
  - `trait BackstageHost { fn extensions(&self) -> &'static [&'static str]; fn default_save_name(&self) -> String; fn preview_lines(&self, path: &std::path::Path, width: usize) -> Vec<String>; fn info_lines(&self) -> Vec<ratatui::text::Line<'static>>; fn accent(&self) -> ratatui::style::Color; }`
  - `enum BackstageEvent { None, Close, New, Open(PathBuf), Save, SaveAs { dir: PathBuf, name: String }, Export, Exit }`
  - `impl Backstage`: `pub fn key(&mut self, key: KeyEvent, host: &dyn BackstageHost) -> BackstageEvent`, `pub fn mouse(&mut self, x: u16, y: u16, host: &dyn BackstageHost) -> BackstageEvent`, `pub fn scroll_preview(&mut self, delta: isize)`, `pub fn refresh_preview(&mut self, host: &dyn BackstageHost, width: usize)`

**Design notes for the implementer:**
- `refresh_preview` replaces each app's `bs_update_preview`: if `selected_file()` differs from `preview_path` OR `width != preview_w`, call `host.preview_lines(path, width)` and store into `preview`/`preview_path`/`preview_w`, resetting `preview_scroll` to 0 when the path changed. When nothing is selected/openable, clear `preview` + `preview_path`.
- `key`/`mouse` port docxy's `backstage_key` (main.rs 1402–1491), `save_as_key`/`save_as_name_key`/`save_as_browser_key` (1700–1790), `bs_mouse` (1497–1627), `bs_menu_activate` (1655–1698), `bs_scroll_preview` (1629–1636) — with `self.backstage.as_mut()` unwrapping removed (methods are ON `Backstage`), `self.dirty = true` dropped (the app sets its own redraw flag), and each app-action arm returning a `BackstageEvent` instead of calling `self.save()` etc.:
  - Menu activate: `Open` → set `pane = Browser`, `refresh_preview`, return `None`; `Info` → `None`; `Save` → `BackstageEvent::Save`; `SaveAs` → prefill `name_input = host.default_save_name()`, `name_focus = true`, `pane = SaveAs`, return `None`; `New` → `BackstageEvent::New`; `Export` → `BackstageEvent::Export`; `Exit` → `BackstageEvent::Exit`.
  - Browser Enter / second-click on selected file: return `Open(path)` (do NOT clear state here — the app drops the backstage on `Open`).
  - Save-As commit (Enter in SaveAs / Save button click): return `SaveAs { dir: self.dir.clone(), name: self.name_input.trim().to_string() }`.
  - `Esc` → `BackstageEvent::Close`.
- `mouse` reads `self.layout` (filled by Task 3's draw): `layout.list_start`, `layout.save_btn`, `layout.name_top`, `layout.name_x0`. The tab-strip row (y==0) is handled by the APP, not the core — `mouse` is only called for y>=1, so the core's `mouse` does NOT handle the tab strip. (The app checks `y == 0` before delegating.)
- `byte_index` helper (docxy main.rs — a char→byte index into a `&str`) is needed by Save-As typing; add it as a private fn in `input.rs`:

```rust
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(s.len())
}
```

- [ ] **Step 1: Write the failing tests**

Add `backstagecore/src/input.rs` with a `#[cfg(test)]` module (create the file with only tests first so they fail to compile → RED). Tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backstage, Item, Pane};
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::style::Color;
    use ratatui::text::Line;
    use std::path::Path;

    struct TestHost;
    impl BackstageHost for TestHost {
        fn extensions(&self) -> &'static [&'static str] { &["docx"] }
        fn default_save_name(&self) -> String { "untitled.docx".into() }
        fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> { vec!["preview".into()] }
        fn info_lines(&self) -> Vec<Line<'static>> { vec![Line::raw("info")] }
        fn accent(&self) -> Color { Color::Cyan }
    }
    fn key(c: KeyCode) -> KeyEvent { KeyEvent::from(c) }

    #[test]
    fn esc_closes() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        assert!(matches!(bs.key(key(KeyCode::Esc), &TestHost), BackstageEvent::Close));
    }

    #[test]
    fn save_item_emits_save_event() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Save;
        bs.pane = Pane::Menu;
        assert!(matches!(bs.key(key(KeyCode::Enter), &TestHost), BackstageEvent::Save));
    }

    #[test]
    fn save_as_item_opens_dialog_prefilled() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::SaveAs;
        bs.pane = Pane::Menu;
        let e = bs.key(key(KeyCode::Enter), &TestHost);
        assert!(matches!(e, BackstageEvent::None));
        assert_eq!(bs.pane, Pane::SaveAs);
        assert_eq!(bs.name_input, "untitled.docx");
        assert!(bs.name_focus);
    }

    #[test]
    fn save_as_typing_edits_name_and_commits() {
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.pane = Pane::SaveAs;
        bs.name_focus = true;
        bs.name_input.clear();
        bs.name_cursor = 0;
        for c in "ab".chars() { bs.key(key(KeyCode::Char(c)), &TestHost); }
        bs.key(key(KeyCode::Backspace), &TestHost);
        assert_eq!(bs.name_input, "a");
        let e = bs.key(key(KeyCode::Enter), &TestHost);
        match e { BackstageEvent::SaveAs { name, .. } => assert_eq!(name, "a"), _ => panic!("{e:?}") }
    }

    #[test]
    fn guarded_new_needs_second_activation_via_mouse() {
        // First click on New (not yet selected) selects it but does NOT fire.
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Open;
        bs.layout.list_start = 0;
        // menu column is x<14; New is row idx 0 → y=1
        let first = bs.mouse(2, 1, &TestHost);
        assert!(matches!(first, BackstageEvent::None));
        assert_eq!(bs.item, Item::New);
        let second = bs.mouse(2, 1, &TestHost);
        assert!(matches!(second, BackstageEvent::New));
    }
}
```

(`BackstageEvent` must derive `Debug` for the `panic!("{e:?}")`.)

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `bash "$LCARGO" test -p backstagecore` → FAIL (host/input not implemented).

- [ ] **Step 3: Implement `host.rs`**

Create `backstagecore/src/host.rs` with `BackstageHost` (see Interfaces) and:

```rust
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum BackstageEvent {
    None,
    Close,
    New,
    Open(PathBuf),
    Save,
    SaveAs { dir: PathBuf, name: String },
    Export,
    Exit,
}
```

- [ ] **Step 4: Implement `input.rs`**

Above the test module, add `impl Backstage` with `key`, `mouse`, `scroll_preview`, `refresh_preview`, plus the private `byte_index`, following the Design notes. Port the docxy handlers verbatim, retargeting `self.backstage.as_mut()...` → `self`, dropping `self.dirty`, and returning `BackstageEvent`. The mouse handler's menu column keeps the guarded-New rule: on a menu click, set `self.item`; if the item is `New` and it was not already selected, set `pane = Menu` and return `None`; otherwise run the menu-activate logic and return its event.

- [ ] **Step 5: Wire modules + re-exports in `lib.rs`**

```rust
mod host;
mod input;
mod state;
pub use host::{BackstageEvent, BackstageHost};
pub use state::{Backstage, BackstageLayout, Entry, Item, Pane, ITEMS};
```

- [ ] **Step 6: Run tests, lint**

Run: `bash "$LCARGO" test -p backstagecore` → all pass.
Run: `bash "$LCARGO" clippy -p backstagecore -- -D warnings` → clean; `bash "$LCARGO" fmt`.

- [ ] **Step 7: Commit**

```bash
git add backstagecore/src/host.rs backstagecore/src/input.rs backstagecore/src/lib.rs
git commit -m "backstagecore: BackstageHost trait + event-returning key/mouse"
```

---

### Task 3: Rendering — `draw` + layout recording + preview caching

**Files:**
- Create: `backstagecore/src/render.rs` (draw + fit_width)
- Modify: `backstagecore/src/lib.rs` (wire module + export `draw`)

**Interfaces:**
- Consumes: `Backstage`, `BackstageHost`, `Item`, `Pane`, `ITEMS`, `BackstageLayout`.
- Produces: `pub fn draw(f: &mut ratatui::Frame, area: ratatui::layout::Rect, bs: &mut Backstage, host: &dyn BackstageHost)`

**Design notes:**
- `area` EXCLUDES the ribbon tab-strip row (the app draws that on row 0). `draw` splits `area` into a 14-wide menu column + content pane (port `draw_backstage` main.rs 2167–2227, dropping the tab-strip render at 2175–2183 and the `dim` "(click a tab…)" hint — those stay in the app).
- Replace `let accent = Style::default().fg(Color::Black).bg(Color::Cyan);` with `let accent = Style::default().fg(Color::Black).bg(host.accent());` and the preview-focus `Color::Cyan` with `host.accent()`. Do this in every draw helper.
- Before drawing Open, call `bs.refresh_preview(host, preview_inner_width)` so the cache is current, and set `bs.layout.preview_h = inner_ph`.
- Record click rects into `bs.layout` as they are computed: `list_start` (the scroll offset used for the list — compute so `sel` stays visible: `list_start = sel.saturating_sub(inner_h - 1)` clamped ≥0, matching docxy's existing `bs_list_start` computation in its draw), `save_btn`, `name_top`, `name_x0`.
- Port `draw_bs_open` (2229–2323), `draw_bs_save_as` (2325–~2430), `draw_bs_info` (2839–~2900) — but `draw_bs_info` renders `host.info_lines()` instead of docxy's inline properties.
- Move docxy's `fit_width` helper into `render.rs` as a private fn (find it in docxy main.rs; it truncates a string to a display width using `unicode_width`). If the exact body is unavailable, implement:

```rust
fn fit_width(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut w = 0; let mut out = String::new();
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max { break; }
        w += cw; out.push(c);
    }
    out
}
```

- [ ] **Step 1: Write a smoke test (renders without panic + fills layout)**

Add to `render.rs` a `#[cfg(test)]` test using `ratatui::backend::TestBackend`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Backstage, BackstageHost, Item};
    use ratatui::{backend::TestBackend, style::Color, text::Line, Terminal};
    use std::path::Path;

    struct H;
    impl BackstageHost for H {
        fn extensions(&self) -> &'static [&'static str] { &["docx"] }
        fn default_save_name(&self) -> String { "untitled.docx".into() }
        fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> { vec!["hello".into()] }
        fn info_lines(&self) -> Vec<Line<'static>> { vec![Line::raw("info")] }
        fn accent(&self) -> Color { Color::Green }
    }

    #[test]
    fn draws_open_pane_without_panic() {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Open;
        term.draw(|f| {
            let a = f.area();
            draw(f, a, &mut bs, &H);
        }).unwrap();
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `bash "$LCARGO" test -p backstagecore render` → FAIL (`draw` not found).

- [ ] **Step 3: Implement `render.rs`**

Write `draw` + the three helpers + `fit_width` per the Design notes, porting the docxy bodies with the accent/host substitutions and layout recording.

- [ ] **Step 4: Wire + export**

In `lib.rs`: add `mod render;` and `pub use render::draw;`.

- [ ] **Step 5: Run tests + lint**

Run: `bash "$LCARGO" test -p backstagecore` → all pass.
Run: `bash "$LCARGO" clippy -p backstagecore -- -D warnings` → clean; `fmt`.

- [ ] **Step 6: Commit**

```bash
git add backstagecore/src/render.rs backstagecore/src/lib.rs
git commit -m "backstagecore: draw() with host accent, preview cache + layout rects"
```

---

### Task 4: Shared start dialog

**Files:**
- Create: `backstagecore/src/start.rs`
- Modify: `backstagecore/src/lib.rs`

**Interfaces:**
- Produces:
  - `struct StartItem { pub label: String, pub desc: Option<String> }`
  - `enum StartEvent { None, Choose(usize), Quit }`
  - `struct Start { /* private */ }`
  - `impl Start`: `pub fn new(title: impl Into<String>, items: Vec<StartItem>, accent: ratatui::style::Color) -> Start`, `pub fn key(&mut self, key: KeyEvent) -> StartEvent`, `pub fn mouse(&mut self, x: u16, y: u16) -> StartEvent`, `pub fn draw(&mut self, f: &mut ratatui::Frame, area: ratatui::layout::Rect)`, `pub fn sel(&self) -> usize`

**Design notes:**
- `Start` holds `title: String`, `items: Vec<StartItem>`, `sel: usize`, `accent: Color`, `btns: Vec<Rect>` (one per item, filled by `draw`, read by `mouse`).
- `key`: `Up` → `sel = sel.saturating_sub(1)`; `Down` → `sel = (sel+1).min(len-1)`; `Char('1'..='9')` → set `sel` to that index if in range and return `Choose(idx)`; `Enter` → `Choose(sel)`; `Esc`/`Char('q')` → `Quit`; else `None`.
- `mouse`: if a click falls in `btns[i]`, set `sel = i` and return `Choose(i)`; else `None`.
- `draw`: a centered card (accent border) titled with `title`; one row per item — `label` in normal text, `desc` (if any) dimmed after it; the selected row reversed/accent; a "1..N to pick · ↑↓ · Enter · q quits" footer. Record each row's `Rect` into `btns`.

- [ ] **Step 1: Write the failing tests**

Create `backstagecore/src/start.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::style::Color;

    fn items() -> Vec<StartItem> {
        vec![
            StartItem { label: "New".into(), desc: None },
            StartItem { label: "Open".into(), desc: Some("browse".into()) },
            StartItem { label: "Quit".into(), desc: None },
        ]
    }
    fn key(c: KeyCode) -> KeyEvent { KeyEvent::from(c) }

    #[test]
    fn number_key_chooses_that_index() {
        let mut s = Start::new("xlsxy", items(), Color::Green);
        assert!(matches!(s.key(key(KeyCode::Char('2'))), StartEvent::Choose(1)));
    }

    #[test]
    fn arrows_then_enter_choose_selection() {
        let mut s = Start::new("xlsxy", items(), Color::Green);
        s.key(key(KeyCode::Down));
        s.key(key(KeyCode::Down));
        assert_eq!(s.sel(), 2);
        assert!(matches!(s.key(key(KeyCode::Enter)), StartEvent::Choose(2)));
    }

    #[test]
    fn esc_and_q_quit() {
        let mut s = Start::new("xlsxy", items(), Color::Green);
        assert!(matches!(s.key(key(KeyCode::Esc)), StartEvent::Quit));
        assert!(matches!(s.key(key(KeyCode::Char('q'))), StartEvent::Quit));
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `bash "$LCARGO" test -p backstagecore start` → FAIL.

- [ ] **Step 3: Implement `start.rs`** per the Design notes.

- [ ] **Step 4: Wire + export in `lib.rs`**: `mod start;` and `pub use start::{Start, StartEvent, StartItem};`

- [ ] **Step 5: Test + lint**

Run: `bash "$LCARGO" test -p backstagecore` → pass; `clippy -p backstagecore -- -D warnings`; `fmt`.

- [ ] **Step 6: Commit**

```bash
git add backstagecore/src/start.rs backstagecore/src/lib.rs
git commit -m "backstagecore: shared centered start dialog (Start/StartItem/StartEvent)"
```

---

### Task 5: Migrate docxy onto backstagecore

**Files:**
- Modify: `docxy/Cargo.toml` (add dep)
- Delete/replace: `docxy/src/backstage.rs` (becomes a thin re-export shim or is removed)
- Modify: `docxy/src/main.rs` (host impl; collapse handlers; one draw call; start on `Start`)

**Interfaces:**
- Consumes: everything from `backstagecore` (Tasks 1–4).

**Design notes:**
- `docxy/Cargo.toml`: add `backstagecore = { version = "0.1.0", path = "../backstagecore" }`.
- Replace `mod backstage;` usage: change `docxy/src/backstage.rs` to `pub use backstagecore::*;` (keeps `backstage::Item` etc. call sites compiling) OR delete the file and switch imports to `backstagecore`. Prefer the shim to minimize churn.
- Add `impl backstagecore::BackstageHost for App`:
  - `extensions` → `&["docx"]`
  - `default_save_name` → the current file's base name, or `"untitled.docx"` (port from `bs_menu_activate` SaveAs arm, main.rs 1675–1678).
  - `preview_lines(path, w)` → docxy's existing preview render (the body of `bs_update_preview` that renders the highlighted `.docx`/`.md` to `Vec<String>` at width `w`).
  - `info_lines` → docxy's Info content (port from `draw_bs_info`, main.rs ~2839+, as `Vec<Line<'static>>`).
  - `accent` → `Color::LightBlue`.
- Delete `bs_update_preview`, `bs_scroll_preview`, `bs_menu_move`, `bs_menu_activate`, `save_as_key`, `save_as_name_key`, `save_as_browser_key`, `commit_save_as`'s dispatch shell (keep the format-resolution body — call it from the `SaveAs { dir, name }` event), `draw_backstage`, `draw_bs_open`, `draw_bs_save_as`, `draw_bs_info`, and the `bs_list_start`/`bs_save_btn`/`bs_name_top`/`bs_name_x0`/`bs_preview_h` fields.
- `backstage_key` collapses to: `let ev = bs.key(key, self); self.dirty = true;` then `match ev` calling `self.save()`, `self.open_path(&p)` + drop backstage, `self.new_document()` + drop, `self.export_pdf()` + drop, `self.request_exit()`, `SaveAs{dir,name}` → the retained format-resolving save (`commit_save_as` body) then drop backstage, `Close` → drop backstage + restore ribbon.
- `bs_mouse` collapses similarly: keep the `y == 0` tab-strip handling in the app (unchanged), then for y>=1 `let ev = self.backstage.as_mut()?.mouse(x, y, self)` — but the borrow of `self` for both `bs` and `host` conflicts. Resolve by taking the backstage out: `let mut bs = self.backstage.take(); let ev = bs.as_mut().map(|b| b.mouse(x, y, self)); self.backstage = bs; match ev { .. }`. Apply the same take/put pattern in `backstage_key`.
- The draw site: keep the app's tab-strip render on row 0, then `backstagecore::draw(f, rows_below, bs, self)` — again using take/put if the borrow conflicts, or split so the host data needed is read before the mutable borrow.
- Rebuild the start screen on `backstagecore::Start` with docxy's 4 items (`New Word document (.docx)`, `New Markdown document (.md)`, `Open an existing file…`, `Quit`); `start_activate` becomes `match self.start.key(key) { Choose(0) => new docx, Choose(1) => new md, Choose(2) => open browser, Choose(3)|Quit => quit, _ => {} }`.

- [ ] **Step 1: Add the dependency + shim**

Edit `docxy/Cargo.toml` (add dep) and replace `docxy/src/backstage.rs` body with `pub use backstagecore::*;`.

- [ ] **Step 2: Confirm the pre-existing docxy backstage tests still enumerate**

Run: `bash "$LCARGO" test -p docxy --no-run` → expect compile errors (call sites reference removed methods). This is the working checklist for the migration.

- [ ] **Step 3: Implement `BackstageHost for App` + collapse the handlers + draw + start** per the Design notes.

- [ ] **Step 4: Build + test**

Run: `bash "$LCARGO" test -p docxy` → all existing docxy tests pass.
Run: `bash "$LCARGO" clippy -p docxy -- -D warnings` → clean; `fmt`.

- [ ] **Step 5: Commit**

```bash
git add docxy/Cargo.toml docxy/src/backstage.rs docxy/src/main.rs
git commit -m "docxy: migrate File backstage + start dialog onto backstagecore"
```

---

### Task 6: Migrate xlsxy onto backstagecore

**Files:**
- Modify: `xlsxy/Cargo.toml`, `xlsxy/src/backstage.rs`, `xlsxy/src/main.rs`

**Design notes:** Same shape as Task 5, with:
- `extensions` → `&["xlsx", "csv", "tsv"]`; `default_save_name` → `"untitled.xlsx"` (or current name).
- `preview_lines` → xlsxy's existing workbook/CSV preview render (main.rs ~441 `start_edit`/preview helper — the function that renders the first sheet/CSV to lines).
- `info_lines` → xlsxy's Info content.
- `accent` → `Color::Green`.
- Start items: `New workbook`, `Open…`, `Quit` (3).
- `SaveAs { dir, name }` → xlsxy's save path logic (default `.xlsx` if no known extension).
- Delete xlsxy's `is_openable` (replaced by `extensions`), and its backstage handler/draw functions, collapsing to `bs.key`/`bs.mouse`/`backstagecore::draw` with the take/put borrow pattern.

- [ ] **Step 1: Add dep + shim** (`xlsxy/Cargo.toml` dep; `xlsxy/src/backstage.rs` → `pub use backstagecore::*;`).
- [ ] **Step 2:** Run `bash "$LCARGO" test -p xlsxy --no-run` → compile errors map the call sites.
- [ ] **Step 3:** Implement `BackstageHost for App`, collapse handlers/draw, rebuild start.
- [ ] **Step 4:** `bash "$LCARGO" test -p xlsxy` → pass; `clippy -p xlsxy -- -D warnings`; `fmt`.
- [ ] **Step 5: Commit**

```bash
git add xlsxy/Cargo.toml xlsxy/src/backstage.rs xlsxy/src/main.rs
git commit -m "xlsxy: migrate File backstage + start dialog onto backstagecore"
```

---

### Task 7: Migrate yppxy onto backstagecore

**Files:**
- Modify: `yppxy/Cargo.toml`, `yppxy/src/backstage.rs`, `yppxy/src/main.rs`

**Design notes:** Same shape, with:
- `extensions` → `&["xml", "yppx", "mpp"]`; `default_save_name` → `"untitled.yppx"` (or current name).
- `preview_lines` → yppxy's existing project preview render.
- `info_lines` → yppxy's Info content.
- `accent` → `Color::Yellow`.
- Start items: yppxy's current New/Open/Quit set (port the labels from its `disp_start`/`draw_start`). yppxy's start screen is structurally different (`disp_start`/`start_key`/`draw_start` rather than `START_ITEMS`); rebuild it on `backstagecore::Start` with the same visible choices, and route `start_key` through `self.start.key`.
- yppxy renders the ribbon body always (RIBBON_H=7); the backstage still replaces the body when the File tab is active — keep that wiring, only the backstage internals change.

- [ ] **Step 1: Add dep + shim.**
- [ ] **Step 2:** `bash "$LCARGO" test -p yppxy --no-run` → compile errors map the sites.
- [ ] **Step 3:** Implement host impl, collapse handlers/draw, rebuild start.
- [ ] **Step 4:** `bash "$LCARGO" test -p yppxy` → pass; `clippy -p yppxy -- -D warnings`; `fmt`.
- [ ] **Step 5: Commit**

```bash
git add yppxy/Cargo.toml yppxy/src/backstage.rs yppxy/src/main.rs
git commit -m "yppxy: migrate File backstage + start dialog onto backstagecore"
```

---

### Task 8: Workspace green + cleanup

**Files:** none new — verification + any dead-code removal surfaced by the migrations.

- [ ] **Step 1:** Run `bash "$LCARGO" clippy --workspace -- -D warnings` → clean (fix any now-dead imports/fields in docxy/xlsxy/yppxy).
- [ ] **Step 2:** Run per-app tests (workspace test may exceed the 2-min aggregate compile budget; run individually as in the ribbon refactor):
  - `bash "$LCARGO" test -p backstagecore`
  - `bash "$LCARGO" test -p docxy`
  - `bash "$LCARGO" test -p xlsxy`
  - `bash "$LCARGO" test -p yppxy`
  - `bash "$LCARGO" test -p lookxy` (unchanged — regression guard)
  All green.
- [ ] **Step 3:** Run `bash "$LCARGO" fmt --check` → clean.
- [ ] **Step 4:** Manual sanity (describe in the commit, no code): each of docxy/xlsxy/yppxy opens the File tab → menu + Open browser + preview + Save As all render in the app's accent; launching with no file shows the start card.
- [ ] **Step 5: Commit** any cleanup:

```bash
git add -A
git commit -m "backstagecore: workspace clippy/fmt clean after migration"
```

---

## Self-Review

**Spec coverage:** state/browser (Task 1), BackstageHost + event + key/mouse (Task 2), draw + accent + preview cache + layout (Task 3), start dialog (Task 4), docxy/xlsxy/yppxy migration incl. identical 7-item menu + Print→Export (Tasks 5–7), workspace green + lookxy untouched (Task 8). All spec sections covered.

**Placeholder scan:** No TBD/TODO. Large verbatim ports name exact source line ranges + the mechanical transformations (accent→host, self.backstage.as_mut()→self, drop dirty, return event) — this is the ribbon migration technique, not a placeholder.

**Type consistency:** `selected_file`, `menu_move`, `BackstageEvent::{New,Open,Save,SaveAs{dir,name},Export,Exit,Close,None}`, `BackstageHost::{extensions,default_save_name,preview_lines,info_lines,accent}`, `Start::{new,key,mouse,draw,sel}`, `StartEvent::{None,Choose,Quit}`, `BackstageLayout::{list_start,save_btn,name_top,name_x0,preview_h}` are used consistently across tasks. The take/put borrow pattern is called out wherever `self` is both `bs` owner and `host`.
