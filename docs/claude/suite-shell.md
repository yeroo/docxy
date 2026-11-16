# Suite shell

Notes moved from `CLAUDE.md` (#1903). Read them before changing the suite's window chrome: the start screen, the title bar label, the accent and the theme command. This file is not loaded automatically: the `CLAUDE.md` index (Area notes) points here. Paths are from the repository root. Add this area's new notes here as `##` sections.

## Start screen (#806)

- A suite launch with nothing to restore and no file on the command line has
  no tab and the File > New page up (`Docxy::show_start_screen`; `startup_tabs`
  decides, `Startup::StartScreen`), brought up through `open_backstage` so the
  lists are filled. A restored session keeps its tabs; files on the command
  line open over it (a file that cannot be read opens as a failed-load tab, so
  one tab always exists). With no tab, Back on File goes from New to
  its default page (Recent, Settings) and stay in File, Close, Save and Save
  As do nothing, and the startup plugin notice waits in `pending_status` for the first
  tab (`land_status`; a tab's own status wins). A failed Open pick with no tab
  keeps File up and says why under Recent (`recent_message`). The New page's cards are `NEW_CARDS`
  (the Spreadsheet one reads "Blank workbook").
- A harness instance keeps the sample document as tab 0 (the cases assume it)
  unless the script says `start screen`: the runner writes the marker
  `<config root>/docxy/start-screen` into the sandbox, which only a harness
  instance reads (`harness::start_screen_requested`). `start-screen.uit` drives
  it (`backstage` read reports `new` and `cards`, `new-card` clicks one).
- `Docxy` cannot be built in a unit test (no gpui test context), so the
  zero-tab guards are checked by reading the sites that index
  `self.tabs[self.active]`: each follows a `tabs.get`/`is_empty` guard, and
  `save_project_to`, the one that did not, now refuses with "no tab is open".

## Title label and accent per tab type (#1473)

- The suite's title bar names the selected tab's type in one word (`doc`,
  `sheet`, `project`; Mail `mail`, no tab `doc`), not the document, whose name
  stays on its tab. The OS window title is unchanged (`doc_name::window_title`).
- `suite/docxy/src/accent.rs` holds the accent per type and theme (doc keeps
  `BRAND`, sheet green, project yellow; the sheet and project shades hold white
  File text at 4.5:1). `Docxy::render` calls `accent::set` before building any
  chrome and the ribbon, backstage and title bar read `accent::brand()`; every
  other `BRAND` use (dialogs, comment and find bars, carets, Gantt) is not
  accent. A `.hover` style is built at once, so it reads the accent during
  render too. `accent_now` takes the theme from the preference, as render sets
  the accent before `apply_theme`. `tab-list` reports `title_label`, `accent`
  (from the tab) and `drawn_accent` (what this window's last render set); `uiharness/cases/title-accent.uit` asserts them.
- The Light/Dark/Auto theme button is not in the title bar: View > Theme
  (doc `Act::DarkMode`, `SheetAct::Theme`, `ProjectAct::Theme`) and Backstage's
  Settings Theme row (always reachable: the File button is drawn in every state,
  and the only place for Mail, a failed load, no tab, Protected View and a
  minimised ribbon, where `ribbon_body_hidden` draws no View tab) call
  `cycle_theme`, which says the new setting on the status line.
  `title_left_caps` no longer reserves a theme width.

## Open File's native picker (#2192)

- `Docxy::pick_open_file` (File > Open…, Ctrl+O, the Backstage Open rail, Open
  Read-Only…, Open as Copy…, Open and Repair…, Recover Text…) dispatches on
  `open_mode::open_pick_route()`. The arms have no `cfg`, so every OS compiles
  all three, and the route is the one place that decides:
  - **Linux, `Background`:** rfd's portal pick runs on the background executor,
    so the window keeps dispatching (Wayland pings). The result comes back
    through `update_in`.
  - **Windows, `Deferred`:** `IFileDialog::Show` runs a modal loop on the main
    thread, and gpui_windows runs foreground tasks from its messages. Shown
    inside a listener, the App `RefCell` stays borrowed, so the first task that
    updates an entity through `AsyncApp` (gpui-component's timers, for one)
    panics with "RefCell already borrowed" and aborts in `window_procedure`.
    The dialog is therefore shown from a task spawned with `spawn_in`, which
    holds no borrow, as gpui's own `prompt_for_paths` does. It is owned by the
    suite window (`set_parent`), which is disabled while it is up. Never call
    `pick_file` before the spawn.
  - **macOS, `Inline`:** synchronous, under `native_modal`'s menu-bar swap.
    Not reported to crash, but **not verified safe**: gpui_macos runs
    foreground runnables on the main dispatch queue, which may drain in the
    panel's modal run loop (follow-up).
- `set_parent` disables only its owner. Another suite window, such as a
  torn-off tab's (#1873), stays live and can open a second picker, nested in
  the first one's modal loop. That is safe, since no borrow is held, but the
  results come back last-in first-out. If the owner closes first, `update_in`
  fails and the result is dropped.
- Every other rfd dialog (the Save As pickers, Export, header/footer Insert
  Picture, Mailings, the close warning, the macro-free question) still runs
  synchronously inside a listener, so on Windows it is open to the same crash
  (follow-up). The unit test `open_picker_route_per_os` pins the route only:
  no case runs the real Windows picker yet (`OPEN_HARNESS` still refuses Open
  in a harness instance).
