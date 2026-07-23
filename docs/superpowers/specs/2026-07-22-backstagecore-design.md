# backstagecore — shared File backstage + start dialog

**Date:** 2026-07-22
**Status:** Approved (design)

## Goal

Extract the File "backstage" (the full-screen File menu: New / Open / Info /
Save / Save As / Export / Exit, with the Open folder-browser + document
preview) and the no-file **start dialog** into one shared crate,
`backstagecore`, used by **docxy, xlsxy, yppxy**. Change one place, all three
apps change (except format-specific content and accent color). This mirrors
the completed `ribboncore` consolidation.

lookxy is **out of scope** for now: its backstage is mail-specific (Automatic
Replies / Settings / Exit, no file browser). It keeps its own backstage; it may
be folded in later.

## Why

Today the backstage lives as ~95%-identical copies:

- `docxy/src/backstage.rs` (281), `xlsxy/src/backstage.rs` (280),
  `yppxy/src/backstage.rs` (255) — near-identical state + folder browser; only
  the file extension(s), one accessor name, and a test name differ.
- ~700 lines of backstage interaction/rendering in each app's `main.rs`
  (`draw_backstage`, `draw_bs_open`, `draw_bs_save_as`, `draw_bs_info`,
  `backstage_key`, `bs_mouse`, `bs_menu_activate`, `save_as_key`, …).
- Three divergent start screens (docxy 4 items, xlsxy 3 label+desc pairs,
  yppxy a different structure).

A fix to the browser or Save-As typing must currently be made three times.

## Architecture

`backstagecore` owns **all** backstage state, navigation, the folder browser,
Save-As name editing, preview scrolling, layout, and rendering. Each app
provides only the **format-specific content** through a `BackstageHost` trait
and acts on a small **event** enum with its own file I/O.

```
backstagecore
  Item, Entry, Pane, Backstage (state + browser + save-as + layout)
  BackstageHost (trait: what the app supplies)
  BackstageEvent (what the app acts on)
  draw(f, area, &mut Backstage, host)         // menu + Open/SaveAs/Info panes
  Start (centered accent card: state + draw + key/mouse -> chosen index)

app (docxy/xlsxy/yppxy)
  impl BackstageHost { extensions, default_save_name, preview_lines,
                       info_lines, accent }
  main loop: match bs.key(..)/bs.mouse(..) -> BackstageEvent { New/Open/Save/… }
             (its own docxcore/gridcore/projcore I/O)
```

### Menu items (shared, identical across the three apps)

```
New / Open / Info / Save / Save As / Export / Exit    (7 items)
```

docxy's separate **Print** row is dropped — its `Export` already produces the
same PDF, so folding Print→Export makes all three menus identical (the user's
"Open / Save / Save As / Exit the same" goal).

### State — ported from docxy `backstage.rs`, generalized

```rust
pub enum Item { New, Open, Info, Save, SaveAs, Export, Exit }
pub const ITEMS: [Item; 7] = [ … ];
impl Item { pub fn label(self) -> &'static str }

pub struct Entry { pub name: String, pub is_dir: bool, pub is_parent: bool,
                   pub size: u64, pub locked: bool }
impl Entry { pub fn size_str(&self) -> String }   // "12 KB", blank for dirs

pub enum Pane { Menu, Browser, Preview, SaveAs }

pub struct Backstage {
    pub item: Item, pub pane: Pane,
    pub dir: PathBuf, pub entries: Vec<Entry>, pub sel: usize,
    exts: &'static [&'static str],       // ["docx"] / ["xlsx","csv","tsv"] / ["xml","yppx","mpp"]
    pub preview: Vec<String>, pub preview_path: Option<PathBuf>,
    pub preview_w: usize, pub preview_scroll: usize,
    pub name_input: String, pub name_cursor: usize, pub name_focus: bool,
    layout: BackstageLayout,             // rects recorded by draw(), read by mouse()
}
impl Backstage {
    pub fn open(dir: PathBuf, exts: &'static [&'static str]) -> Backstage;
    pub fn refresh(&mut self);                       // subfolders + files matching exts, folders first
    pub fn selected(&self) -> Option<&Entry>;
    pub fn selected_file(&self) -> Option<PathBuf>;  // was selected_docx
    pub fn move_sel(&mut self, down: bool);
    pub fn enter(&mut self) -> Option<PathBuf>;       // open file, or navigate folder
    pub fn go_up(&mut self);
    pub fn menu_move(&mut self, down: bool);
}
```

The `~$…` lock-file rule and the folders-first sort are preserved verbatim.
`exts` matching is case-insensitive and replaces each app's `is_openable`.

### `BackstageHost` trait — the only per-app surface

```rust
pub trait BackstageHost {
    fn extensions(&self) -> &'static [&'static str];
    fn default_save_name(&self) -> String;                 // "untitled.docx"
    fn preview_lines(&self, path: &Path, width: usize) -> Vec<String>;
    fn info_lines(&self) -> Vec<ratatui::text::Line<'static>>;
    fn accent(&self) -> ratatui::style::Color;
}
```

`preview_lines` is called by `draw`/`key`/`mouse` when the highlighted file or
the render width changes; the result is cached in `Backstage::preview`.

### Interaction — one event enum the app acts on

```rust
pub enum BackstageEvent {
    None,               // fully handled internally (navigation, scroll, typing)
    Close,              // Esc — leave the backstage
    New,                // create a new document
    Open(PathBuf),      // open this file
    Save,               // Save the current document
    SaveAs(PathBuf),    // commit Save As to this path
    Export,             // export/print
    Exit,               // request app exit
}

impl Backstage {
    pub fn key(&mut self, key: KeyEvent, host: &dyn BackstageHost) -> BackstageEvent;
    pub fn mouse(&mut self, x: u16, y: u16, host: &dyn BackstageHost) -> BackstageEvent;
}
```

All navigation (menu up/down, browser, preview scroll incl. PageUp/Down/Home/
End, Save-As name typing + caret, Tab focus between name field and browser,
the **guarded-New second-click** rule) lives in the core. The core mutates its
own state and returns `None` for those; it returns a document-level event only
when the app must run file I/O. `mouse` reads the rects that `draw` recorded in
`self.layout`, so click targets always match what was drawn.

### Rendering

```rust
pub fn draw(f: &mut Frame, area: Rect, bs: &mut Backstage, host: &dyn BackstageHost);
```

Draws the left menu column + the right content pane (Open browser + preview /
Save As folder-list + name box + Save button / Info), all in `host.accent()`,
and records click rects into `bs.layout`. Row 0 (the ribbon tab strip) is drawn
by the app via the already-shared `ribboncore` (`render_tabs_as(0)`), so
`draw` renders from row 1 down. `area` excludes row 0.

### Start dialog — shared centered card, app-supplied items

```rust
pub struct StartItem { pub label: String, pub desc: Option<String> }
pub struct Start { sel: usize, items: Vec<StartItem>, btns: Vec<Rect>, accent: Color, title: String }
pub enum StartEvent { None, Choose(usize), Quit }

impl Start {
    pub fn new(title: impl Into<String>, items: Vec<StartItem>, accent: Color) -> Start;
    pub fn key(&mut self, key: KeyEvent) -> StartEvent;   // ↑↓, 1-9, Enter, Esc/q → Quit
    pub fn mouse(&mut self, x: u16, y: u16) -> StartEvent; // click a row → Choose(i)
    pub fn draw(&mut self, f: &mut Frame, area: Rect);     // centered accent card
}
```

The *look* (centered accent-bordered card, highlighted row, optional dim
description, number hotkeys) is shared. The **items and their dispatch stay app
data**, because they genuinely differ:

- docxy: `New .docx`, `New Markdown (.md)`, `Open an existing file…`, `Quit`
- xlsxy: `New workbook`, `Open…`, `Quit`
- yppxy: its current New/Open/Quit set

The app matches `Choose(i)` against the index in the list it passed.

## Migration (per app)

1. Replace `src/backstage.rs` with a thin re-export of `backstagecore` types
   (or delete it and import directly), keeping every call site compiling.
2. `App` implements `BackstageHost` (extensions, default_save_name,
   preview_lines via its core, info_lines, accent).
3. The ~700 lines of `backstage_key` / `bs_mouse` / `bs_menu_activate` /
   `save_as_key` collapse to: route the key/mouse to `bs.key`/`bs.mouse`, then
   `match` the returned `BackstageEvent` and call the app's existing
   `save` / `open_path` / `new_document` / `export_pdf` / `request_exit`.
4. `draw_backstage` / `draw_bs_*` collapse to one `backstagecore::draw(..)`
   call (the app still draws its ribbon tab strip on row 0).
5. The app's start screen is rebuilt on `backstagecore::Start` with its own
   item list; `start_activate` becomes a `match Choose(i)`.

## Testing

- **backstagecore unit tests** (ported + extended): item order/labels; browser
  lists folders-first, filters by `exts` (multi-ext), excludes lock files, `..`
  present; `size_str` formatting; `enter`/`go_up` navigation; guarded-New needs
  a second activation; Save-As name typing (insert/backspace/caret clamp);
  preview re-render triggers on path/width change; `Start` navigation returns
  the right `Choose(i)` / `Quit`.
- **Per-app tests** stay green (each app's existing backstage/open/save tests).
- Workspace `clippy -D warnings` and `fmt` clean.

## Non-goals

- No change to file formats, load/save logic, or preview *content* (still each
  app's core).
- lookxy's mail backstage is untouched.
- No new backstage features — pure consolidation with identical behavior.
