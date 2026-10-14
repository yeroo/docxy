//! Protected View for downloaded workbooks (#894): what opens in it, what
//! is refused there, what still works, Enable Editing, and the backstop.
//! Only Windows marks files, so most tests swap [`App::zone_of`] for
//! [`fake_zone`], which marks every file whose name starts `downloaded`.

use super::*;
use gridcore::sheet::Cell;
use gridcore::xlsx::{load_xlsx, save_xlsx};

/// Zone 3 (Internet) for a file named `downloaded…`, none for any other.
fn fake_zone(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .starts_with("downloaded")
        .then_some(3)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xlsxy-pv-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A two-sheet workbook, A1 = `a1`, written to `dir/name`.
fn write_book(dir: &Path, name: &str, a1: f64) -> PathBuf {
    let mut pkg = new_xlsx();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(a1));
    pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("=A1"));
    pkg.add_sheet("Second");
    let path = dir.join(name);
    std::fs::write(&path, save_xlsx(&pkg)).unwrap();
    path
}

/// An app opened on `dir/downloaded.xlsx` from the command line, which is
/// in Protected View.
fn protected_app(name: &str) -> (PathBuf, PathBuf, App) {
    let dir = scratch(name);
    let book = write_book(&dir, "downloaded.xlsx", 1.0);
    let pkg = load_xlsx(&std::fs::read(&book).unwrap()).unwrap();
    let mut app = App::new(pkg, book.to_str().unwrap());
    app.os_clip = None;
    app.zone_of = fake_zone;
    app.decide_protected_view(&book);
    assert!(
        app.protected_view.is_some(),
        "a downloaded file opens protected"
    );
    (dir, book, app)
}

/// Everything a save would write, to compare before and after.
fn fingerprint(app: &App) -> Vec<u8> {
    save_xlsx(&app.pkg)
}

fn a1(app: &App) -> CellValue {
    app.pkg.workbook.sheets[0]
        .cell(0, 0)
        .map(|c| c.value.clone())
        .unwrap_or(CellValue::Empty)
}

/// Every file in `dir` with its bytes.
fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let bytes = std::fs::read(e.path()).unwrap_or_default();
            (e.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect();
    out.sort();
    out
}

fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, m)
}

fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(app, f)).unwrap();
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| {
            (0..w)
                .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol()))
                .collect()
        })
        .collect()
}

fn click(app: &mut App, x: u16, y: u16) {
    handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn unmarked_file_and_new_workbook_are_not_protected() {
    let dir = scratch("unmarked");
    let plain = write_book(&dir, "plain.xlsx", 1.0);
    let marked = write_book(&dir, "downloaded.xlsx", 2.0);
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.zone_of = fake_zone;
    app.open_workbook(plain.to_str().unwrap());
    assert!(app.protected_view.is_none(), "an unmarked file is editable");
    app.open_workbook(marked.to_str().unwrap());
    assert!(app.protected_view.is_some(), "a marked file is protected");
    app.open_workbook(plain.to_str().unwrap());
    assert!(
        app.protected_view.is_none(),
        "opening another file leaves Protected View"
    );
    app.open_workbook(marked.to_str().unwrap());
    handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert!(
        app.protected_view.is_none(),
        "Ctrl+N's new workbook is editable"
    );
    assert_eq!(app.path, "untitled.xlsx");
    // The local zones (0-2) open editable.
    for zone in 0..=2u32 {
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.zone_of = match zone {
            0 => |_: &Path| Some(0),
            1 => |_: &Path| Some(1),
            _ => |_: &Path| Some(2),
        };
        app.open_workbook(plain.to_str().unwrap());
        assert!(app.protected_view.is_none(), "zone {zone}");
    }
    // Restricted (4) is protected too.
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.zone_of = |_: &Path| Some(4);
    app.open_workbook(plain.to_str().unwrap());
    assert!(app.protected_view.is_some(), "zone 4");
    std::fs::remove_dir_all(dir).unwrap();
}

/// The real mark: a `Zone.Identifier` stream with ZoneId=3, read through
/// the default [`App::zone_of`].
#[cfg(windows)]
#[test]
fn zone_marked_file_opens_in_protected_view() {
    let dir = scratch("ads");
    let book = write_book(&dir, "book.xlsx", 1.0);
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.open_workbook(book.to_str().unwrap());
    assert!(app.protected_view.is_none());
    let marked = std::fs::write(
        opccore::zone::zone_stream(&book),
        b"[ZoneTransfer]\r\nZoneId=3\r\n",
    );
    if let Err(e) = marked {
        eprintln!("SKIP zone_marked_file_opens_in_protected_view: no ADS here ({e})");
        std::fs::remove_dir_all(dir).unwrap();
        return;
    }
    app.open_workbook(book.to_str().unwrap());
    assert!(app.protected_view.is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn protected_view_refuses_every_editing_key() {
    let (dir, book, mut app) = protected_app("keys");
    let before_file = std::fs::read(&book).unwrap();
    // A copy to paste: Ctrl+C is allowed.
    handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.clip.is_some(), "Ctrl+C copies");
    let before = fingerprint(&app);
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;
    let alt = KeyModifiers::ALT;
    let mut keys = vec![
        key(KeyCode::Char('a'), none),
        key(KeyCode::Char('1'), none),
        key(KeyCode::Char('='), none),
        key(KeyCode::Char(' '), none),
        key(KeyCode::F(2), none),
        key(KeyCode::F(2), shift),
        key(KeyCode::Backspace, none),
        key(KeyCode::Delete, none),
        key(KeyCode::Delete, shift),
        key(KeyCode::Char('v'), ctrl | alt),
        key(KeyCode::F(4), none),
        key(KeyCode::F(5), none),
        key(KeyCode::F(5), shift),
        key(KeyCode::F(6), none),
        key(KeyCode::F(6), shift),
        key(KeyCode::F(7), none),
        key(KeyCode::F(8), none),
        key(KeyCode::F(11), shift),
        key(KeyCode::F(12), none),
        key(KeyCode::Char('+'), ctrl),
        key(KeyCode::Char('='), ctrl | shift),
        key(KeyCode::Char('-'), ctrl),
        key(KeyCode::Down, alt),
        key(KeyCode::Right, alt | shift),
        key(KeyCode::Left, alt | shift),
        key(KeyCode::Char('l'), ctrl | shift),
        key(KeyCode::Enter, alt),
        key(KeyCode::Enter, ctrl),
    ];
    for c in "xvzydrbie1thpms".chars() {
        keys.push(key(KeyCode::Char(c), ctrl));
    }
    for k in keys {
        handle_key(&mut app, k);
        assert_eq!(
            app.status.as_deref(),
            Some(PROTECTED_STATUS),
            "{k:?} is refused with the status"
        );
        assert!(app.edit.is_none(), "{k:?} starts no edit");
        assert!(app.prompt.is_none(), "{k:?} opens no prompt");
        assert!(app.confirm.is_none(), "{k:?} asks nothing");
        assert!(
            app.format_dialog.is_none() && app.format_picker.is_none(),
            "{k:?} opens no dialog"
        );
        assert!(
            app.pivot_edit.is_none() && app.model_view.is_none(),
            "{k:?} opens no editor"
        );
        assert!(!app.modified, "{k:?} changes nothing");
        assert!(app.undo.is_empty(), "{k:?} leaves no undo step");
        assert_eq!(fingerprint(&app), before, "{k:?} changes nothing");
    }
    assert_eq!(std::fs::read(&book).unwrap(), before_file);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn protected_view_keeps_navigation_find_and_copy() {
    let (dir, _book, mut app) = protected_app("nav");
    let before = fingerprint(&app);
    let none = KeyModifiers::NONE;
    let ctrl = KeyModifiers::CONTROL;
    let shift = KeyModifiers::SHIFT;
    handle_key(&mut app, key(KeyCode::Down, none));
    assert_eq!(app.cur, (1, 0));
    handle_key(&mut app, key(KeyCode::Right, shift));
    assert_eq!((app.cur, app.anchor), ((1, 1), Some((1, 0))));
    handle_key(&mut app, key(KeyCode::Esc, none));
    assert_eq!(app.anchor, None);
    handle_key(&mut app, key(KeyCode::Home, ctrl));
    assert_eq!(app.cur, (0, 0));
    handle_key(&mut app, key(KeyCode::Enter, none));
    assert_eq!(app.cur, (1, 0), "Enter moves");
    handle_key(&mut app, key(KeyCode::Tab, none));
    assert_eq!(app.cur, (1, 1));
    handle_key(&mut app, key(KeyCode::PageDown, none));
    handle_key(&mut app, key(KeyCode::PageUp, none));
    handle_key(&mut app, key(KeyCode::End, ctrl));
    handle_key(&mut app, key(KeyCode::Char(' '), shift));
    handle_key(&mut app, key(KeyCode::Char('a'), ctrl));
    handle_key(&mut app, key(KeyCode::Char('c'), ctrl));
    assert!(app.clip.is_some(), "Ctrl+C copies");
    handle_key(&mut app, key(KeyCode::PageDown, ctrl));
    assert_eq!(app.sheet, 1, "Ctrl+PageDown switches sheets");
    handle_key(&mut app, key(KeyCode::PageUp, ctrl));
    assert_eq!(app.sheet, 0);
    handle_key(&mut app, key(KeyCode::Char('f'), ctrl));
    assert!(matches!(
        app.prompt.as_ref().map(|p| &p.kind),
        Some(PromptKind::Find)
    ));
    handle_key(&mut app, key(KeyCode::Esc, none));
    handle_key(&mut app, key(KeyCode::F(3), none));
    assert!(app.prompt.is_some(), "F3 finds");
    handle_key(&mut app, key(KeyCode::Esc, none));
    handle_key(&mut app, key(KeyCode::Char('g'), ctrl));
    assert!(matches!(
        app.prompt.as_ref().map(|p| &p.kind),
        Some(PromptKind::GoTo)
    ));
    handle_key(&mut app, key(KeyCode::Esc, none));
    handle_key(&mut app, key(KeyCode::F(4), shift));
    assert!(app.sheet_picker.is_some(), "Shift+F4 lists the sheets");
    handle_key(&mut app, key(KeyCode::Esc, none));
    let view = app.formula_view;
    handle_key(&mut app, key(KeyCode::Char('`'), ctrl));
    assert_ne!(app.formula_view, view, "Ctrl+` shows formulas");
    handle_key(&mut app, key(KeyCode::Char('u'), ctrl | shift));
    assert!(app.fx_expanded, "Ctrl+Shift+U expands the formula bar");
    handle_key(&mut app, key(KeyCode::F(9), shift));
    assert_eq!(
        app.status.as_deref(),
        Some("Recalculated; pivots kept their cached values")
    );
    handle_key(&mut app, key(KeyCode::F(9), none));
    assert_ne!(
        app.ribbon_focus,
        ribbon::Focus::None,
        "F9 engages the ribbon"
    );
    handle_key(&mut app, key(KeyCode::F(9), none));
    handle_key(&mut app, key(KeyCode::Char('f'), KeyModifiers::ALT));
    assert!(app.backstage.is_some(), "Alt+F opens File");
    handle_key(&mut app, key(KeyCode::Esc, none));
    handle_key(&mut app, key(KeyCode::Char('o'), ctrl));
    assert!(app.backstage.is_some(), "Ctrl+O opens File › Open");
    handle_key(&mut app, key(KeyCode::Esc, none));
    assert!(!app.modified);
    assert_eq!(fingerprint(&app), before);
    assert!(app.protected_view.is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn protected_view_vim_refuses_edits_and_keeps_motions() {
    let (dir, book, mut app) = protected_app("vim");
    let before_file = std::fs::read(&book).unwrap();
    app.vim = Some(VimState {
        mode: VimMode::Normal,
        pending: '\0',
        cmdline: None,
    });
    let before = fingerprint(&app);
    let press = |app: &mut App, c: char| handle_key(app, key(KeyCode::Char(c), KeyModifiers::NONE));
    press(&mut app, 'j');
    assert_eq!(app.cur, (1, 0), "j moves");
    press(&mut app, 'k');
    press(&mut app, 'l');
    press(&mut app, 'h');
    press(&mut app, 'G');
    press(&mut app, 'g');
    press(&mut app, 'g');
    assert_eq!(app.cur, (0, 0), "gg moves");
    press(&mut app, 'g');
    press(&mut app, 't');
    assert_eq!(app.sheet, 1, "gt switches sheets");
    press(&mut app, 'g');
    press(&mut app, 'T');
    assert_eq!(app.sheet, 0);
    press(&mut app, 'y');
    press(&mut app, 'y');
    assert!(app.clip.is_some(), "yy copies");
    for c in ['x', 'i', 'a', 'c', 'p', 'u', 'd'] {
        press(&mut app, c);
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS), "{c}");
        assert!(app.edit.is_none(), "{c} starts no edit");
    }
    for k in [
        key(KeyCode::Enter, KeyModifiers::NONE),
        key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        key(KeyCode::Char('s'), KeyModifiers::CONTROL),
    ] {
        handle_key(&mut app, k);
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS), "{k:?}");
    }
    press(&mut app, 'v');
    press(&mut app, 'j');
    press(&mut app, 'd');
    assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS), "visual d");
    handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(fingerprint(&app), before);
    assert!(!app.modified);
    for cmd in ["w", "wq", "x"] {
        press(&mut app, ':');
        for c in cmd.chars() {
            press(&mut app, c);
        }
        let quit = handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!quit, ":{cmd} does not quit");
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS), ":{cmd}");
    }
    assert_eq!(std::fs::read(&book).unwrap(), before_file);
    press(&mut app, ':');
    press(&mut app, 'q');
    assert!(
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE)),
        ":q quits"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn every_ribbon_act_is_refused_or_harmless_in_protected_view() {
    let (dir, book, mut app) = protected_app("ribbon");
    let before_file = std::fs::read(&book).unwrap();
    let before = fingerprint(&app);
    let mut acts = ribbon::all_acts();
    acts.extend([ribbon::Act::Save, ribbon::Act::SaveAs]);
    let mut allowed = 0;
    for act in acts {
        app.status = None;
        app.ribbon_act(act);
        if ribbon::allowed_in_protected_view(act) {
            allowed += 1;
            assert_ne!(
                app.status.as_deref(),
                Some(PROTECTED_STATUS),
                "{act:?} is allowed"
            );
        } else {
            assert_eq!(
                app.status.as_deref(),
                Some(PROTECTED_STATUS),
                "{act:?} is refused"
            );
            assert!(app.prompt.is_none() && app.confirm.is_none(), "{act:?}");
        }
        assert!(!app.modified, "{act:?} changes nothing");
        assert_eq!(fingerprint(&app), before, "{act:?} changes nothing");
        // Close whatever an allowed command showed.
        app.backstage = None;
        app.prompt = None;
    }
    assert!(allowed >= 10, "the view commands are on the ribbon");
    assert_eq!(std::fs::read(&book).unwrap(), before_file);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Protected View writes nothing, Save As included (#610, #894): the
/// file's bytes and its folder's listing stay as they were.
#[test]
fn protected_view_writes_nothing() {
    let (dir, book, mut app) = protected_app("writes");
    let before = listing(&dir);
    let path = app.path.clone();
    let refused = |app: &App, what: &str| {
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS), "{what}");
    };
    handle_key(&mut app, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
    refused(&app, "Ctrl+S");
    app.save();
    refused(&app, "save");
    assert_eq!(app.save_current(), Err(PROTECTED_STATUS.to_string()));
    assert!(!app.vim_run_command("w"));
    assert!(!app.vim_run_command("wq"), ":wq does not quit");
    assert!(!app.vim_run_command("x"));
    let other = dir.join("other.xlsx").to_string_lossy().into_owned();
    assert!(!app.save_as(other.clone()));
    refused(&app, "save_as");
    app.request_save_as(other.clone());
    refused(&app, "request_save_as");
    app.save_as_type(dir.join("o.csv").to_string_lossy().into_owned(), 1);
    refused(&app, "save_as_type");
    app.commit_save_as(dir.clone(), "copy".to_string());
    refused(&app, "commit_save_as");
    app.export_csv();
    refused(&app, "export_csv");
    app.export_pdf();
    refused(&app, "export_pdf");
    app.save_pdf(dir.join("o.pdf").to_string_lossy().into_owned());
    refused(&app, "save_pdf");
    assert_eq!(app.convert(true).err().as_deref(), Some(PROTECTED_STATUS));
    for ev in [
        backstage::BackstageEvent::Save,
        backstage::BackstageEvent::SaveAs {
            dir: dir.clone(),
            name: "copy2".to_string(),
        },
        backstage::BackstageEvent::Export,
        backstage::BackstageEvent::ExportExtra(0),
    ] {
        app.open_backstage();
        app.apply_backstage_event(ev.clone());
        refused(&app, &format!("{ev:?}"));
    }
    // Every write stops at the one choke point too.
    assert!(app.guard_write(Path::new(&other)).is_err());
    for (verb, args) in [
        ("wb.save", ctlcore::json::Json::Null),
        ("wb.convert", ctlcore::json::Json::Null),
        (
            "wb.export-pdf",
            ctlcore::json::Json::obj(vec![("path", ctlcore::json::Json::Str(other.clone()))]),
        ),
    ] {
        let err = run_control(&mut app, verb, &args).unwrap_err();
        assert_eq!(err, PROTECTED_STATUS, "{verb}");
    }
    assert_eq!(app.path, path, "no Save As moved the binding");
    assert_eq!(listing(&dir), before, "nothing was written");
    assert!(book.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn protected_view_refuses_editing_clicks() {
    let (dir, _book, mut app) = protected_app("clicks");
    let before = fingerprint(&app);
    screen(&mut app, 80, 24);
    let g = app.grid_area;
    let (_, x, _) = app.vis_cols[0];
    let y = g.y;
    click(&mut app, x, y);
    assert_eq!(app.cur, (0, 0), "a click selects");
    click(&mut app, x, y);
    assert!(app.edit.is_none(), "a double-click starts no edit");
    assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
    // A drag extends the selection.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: x,
            row: y + 1,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!((app.cur, app.anchor), ((1, 0), Some((0, 0))));
    // The outline buttons edit the outline.
    app.status = None;
    app.outline_show_level(gridcore::outline::Axis::Rows, 1);
    assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
    assert_eq!(fingerprint(&app), before);
    assert!(!app.modified);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The bar under the ribbon: Excel's label and warning, the button always
/// whole, and the grid a row shorter.
#[test]
fn protected_view_bar_says_so_and_takes_a_row() {
    let (dir, _book, mut app) = protected_app("bar");
    let lines = screen(&mut app, 120, 24);
    let bar = lines
        .iter()
        .find(|l| l.contains(PROTECTED_LABEL))
        .expect("the bar is drawn");
    assert!(bar.contains("Be careful"), "{bar}");
    assert!(bar.trim_end().ends_with(ENABLE_EDITING), "{bar}");
    let protected_h = app.grid_area.height;
    // Narrow: the warning is cut, the button is whole.
    let lines = screen(&mut app, 40, 24);
    let bar = lines.iter().find(|l| l.contains(PROTECTED_LABEL)).unwrap();
    assert!(bar.contains(ENABLE_EDITING), "{bar}");
    app.enable_editing().unwrap();
    let lines = screen(&mut app, 120, 24);
    assert!(!lines.iter().any(|l| l.contains(PROTECTED_LABEL)));
    assert_eq!(app.grid_area.height, protected_h + 1);
    assert!(app.pv_button.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn title_says_protected_view_over_read_only() {
    assert_eq!(
        window_title("xlsxy", "book.xlsx", false, true, false, true),
        "xlsxy - book.xlsx [Protected View]"
    );
    assert_eq!(
        window_title("xlsxy", "book.xls", false, false, true, true),
        "xlsxy - book.xls [Protected View] [Compatibility Mode]"
    );
    assert_eq!(
        window_title("xlsxy", "book.xlsx", false, true, false, false),
        "xlsxy - book.xlsx [Read-Only]"
    );
}

#[test]
fn enable_editing_from_bar_info_and_control_unlocks_edits() {
    let (dir, book, mut app) = protected_app("enable");
    app.set_read_only(book.to_str().unwrap());
    // The bar's button.
    screen(&mut app, 120, 24);
    let (y, x1, _) = app.pv_button.expect("the button was drawn");
    click(&mut app, x1, y);
    assert!(
        app.protected_view.is_none(),
        "the bar's button enables editing"
    );
    assert_eq!(app.status.as_deref(), Some("Editing enabled"));
    assert!(app.bound_read_only(), "--read-only stays as it was");
    handle_key(&mut app, key(KeyCode::Char('7'), KeyModifiers::NONE));
    assert!(app.edit.is_some(), "typing edits again");
    app.cancel_edit();
    // File › Info's row, from the keyboard.
    app.enter_protected_view(&book, None);
    app.open_backstage();
    app.backstage.as_mut().unwrap().focus_info(0);
    let info = screen(&mut app, 100, 30);
    assert!(
        info.iter().any(|l| l.contains("Enable Editing")),
        "{info:#?}"
    );
    // A property row is refused, and says why.
    handle_key(&mut app, key(KeyCode::Down, KeyModifiers::NONE));
    handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.backstage.as_ref().unwrap().info_message.as_deref(),
        Some(PROTECTED_STATUS)
    );
    assert!(app.prompt.is_none());
    handle_key(&mut app, key(KeyCode::Up, KeyModifiers::NONE));
    handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.protected_view.is_none(), "Info's row enables editing");
    assert!(app.backstage.is_none());
    // The control surface.
    app.enter_protected_view(&book, None);
    let reply = run_control(&mut app, "wb.enable-editing", &ctlcore::json::Json::Null).unwrap();
    assert!(matches!(
        reply.get("protected"),
        Some(ctlcore::json::Json::Bool(false))
    ));
    assert!(
        run_control(&mut app, "wb.enable-editing", &ctlcore::json::Json::Null).is_err(),
        "not in Protected View: an error"
    );
    app.read_only = None;
    let set = ctlcore::json::Json::obj(vec![
        ("ref", ctlcore::json::Json::Str("A1".into())),
        ("text", ctlcore::json::Json::Str("5".into())),
    ]);
    run_control(&mut app, "cell.set", &set).unwrap();
    assert_eq!(a1(&app), CellValue::Number(5.0));
    app.save();
    assert!(!app.modified, "saves work again");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Enable Editing trusts the file for the session: reloading or reopening
/// it unchanged opens it editable; once it is replaced, it opens protected.
#[test]
fn enabled_file_reloads_editable_until_it_changes() {
    let (dir, book, mut app) = protected_app("trusted");
    app.enable_editing().unwrap();
    run_control(&mut app, "wb.reload", &ctlcore::json::Json::Null).unwrap();
    assert!(app.protected_view.is_none(), "a reload stays editable");
    app.open_workbook(book.to_str().unwrap());
    assert!(app.protected_view.is_none(), "a reopen stays editable");
    // Replaced since: a new length (and time) is not trusted.
    write_book(&dir, "downloaded.xlsx", 123_456_789.0);
    let pkg = std::fs::read(&book).unwrap();
    std::fs::write(&book, [pkg.as_slice(), b"padding"].concat()).unwrap();
    app.open_workbook(book.to_str().unwrap());
    assert!(
        app.protected_view.is_some(),
        "a replaced file is protected again"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// A downloaded `.csv` reloads through the text path, which installs the
/// workbook directly: it stays protected.
#[test]
fn reload_of_a_protected_csv_stays_protected() {
    let dir = scratch("csv");
    let csv = dir.join("downloaded.csv");
    std::fs::write(&csv, "a,1\nb,2\n").unwrap();
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.zone_of = fake_zone;
    app.open_workbook(csv.to_str().unwrap());
    assert!(app.protected_view.is_some());
    assert!(app.bound_text_type().is_some(), "bound to the csv as text");
    run_control(&mut app, "wb.reload", &ctlcore::json::Json::Null).unwrap();
    assert!(app.protected_view.is_some(), "the reload is protected too");
    let before = std::fs::read(&csv).unwrap();
    app.save();
    assert_eq!(std::fs::read(&csv).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A finished Text Import Wizard decides from the file it read.
#[test]
fn text_import_wizard_finish_decides_protected_view() {
    let dir = scratch("wizard");
    let txt = dir.join("downloaded.txt");
    std::fs::write(&txt, "a\t1\nb\t2\n").unwrap();
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.zone_of = fake_zone;
    app.open_workbook(txt.to_str().unwrap());
    assert!(app.text_dialog.is_some(), "the wizard opens");
    assert!(app.protected_view.is_none(), "nothing is installed yet");
    for _ in 0..3 {
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        if app.text_dialog.is_none() {
            break;
        }
    }
    if app.text_dialog.is_some() {
        app.finish_text_dialog();
    }
    assert!(app.text_dialog.is_none(), "the wizard finished");
    assert!(app.protected_view.is_some(), "the import is protected");
    std::fs::remove_dir_all(dir).unwrap();
}

/// A dialog, prompt or editor open when a control verb opens a downloaded
/// file closes: its OK would edit the protected workbook.
#[test]
fn entering_protected_view_closes_what_could_edit() {
    let dir = scratch("overlays");
    let plain = write_book(&dir, "plain.xlsx", 1.0);
    let marked = write_book(&dir, "downloaded.xlsx", 2.0);
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.os_clip = None;
    app.zone_of = fake_zone;
    app.open_workbook(plain.to_str().unwrap());
    app.open_format_dialog();
    let open = ctlcore::json::Json::obj(vec![(
        "path",
        ctlcore::json::Json::Str(marked.to_string_lossy().into_owned()),
    )]);
    run_control(&mut app, "wb.open", &open).unwrap();
    assert!(app.protected_view.is_some());
    assert!(app.format_dialog.is_none(), "Format Cells closed");
    let before = fingerprint(&app);
    handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(fingerprint(&app), before);
    assert!(!app.modified);
    // A Rename Sheet prompt closes; Find stays.
    app.open_workbook(plain.to_str().unwrap());
    app.open_prompt(PromptKind::RenameSheet);
    run_control(&mut app, "wb.open", &open).unwrap();
    assert!(app.prompt.is_none(), "Rename Sheet closed");
    app.open_workbook(plain.to_str().unwrap());
    app.open_prompt(PromptKind::Find);
    run_control(&mut app, "wb.open", &open).unwrap();
    assert!(
        matches!(app.prompt.as_ref().map(|p| &p.kind), Some(PromptKind::Find)),
        "Find only reads, and stays"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backstop_rolls_back_an_edit_that_slipped_past() {
    let (dir, book, mut app) = protected_app("backstop");
    let before_file = std::fs::read(&book).unwrap();
    let before = fingerprint(&app);
    app.pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(99.0));
    app.pkg.add_sheet("Slipped");
    app.modified = true;
    app.protected_backstop();
    assert_eq!(a1(&app), CellValue::Number(1.0), "A1 is back");
    assert_eq!(fingerprint(&app), before);
    assert!(!app.modified);
    assert!(app.protected_view.is_some(), "still protected");
    assert!(
        app.status.as_deref().unwrap().starts_with(PROTECTED_STATUS),
        "{:?}",
        app.status
    );
    assert_eq!(std::fs::read(&book).unwrap(), before_file);
    // Nothing to roll back: the backstop leaves the workbook alone.
    app.status = None;
    app.protected_backstop();
    assert_eq!(app.status, None);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The XLSTART folder is a trusted location in Excel: its workbooks never
/// open protected. The alternate startup folder is not.
#[test]
fn xlstart_workbooks_are_trusted_and_the_alternate_folder_is_not() {
    let xlstart = scratch("xlstart");
    let alt = scratch("alt-startup");
    let in_xlstart = write_book(&xlstart, "downloaded.xlsx", 1.0);
    let in_alt = write_book(&alt, "downloaded.xlsx", 1.0);
    let mut app = App::new(new_xlsx(), "untitled.xlsx");
    app.zone_of = fake_zone;
    app.xlstart = Some(xlstart.clone());
    app.open_startup_workbooks(std::slice::from_ref(&in_xlstart));
    assert_eq!(app.path, in_xlstart.to_string_lossy());
    assert!(app.protected_view.is_none(), "XLSTART is trusted");
    app.open_startup_workbooks(std::slice::from_ref(&in_alt));
    assert_eq!(app.path, in_alt.to_string_lossy());
    assert!(app.protected_view.is_some(), "the alternate folder is not");
    std::fs::remove_dir_all(xlstart).unwrap();
    std::fs::remove_dir_all(alt).unwrap();
}

#[test]
fn control_verbs_outside_the_allow_list_are_refused() {
    use ctlcore::json::Json;
    let (dir, book, mut app) = protected_app("control");
    let before = fingerprint(&app);
    let before_file = std::fs::read(&book).unwrap();
    let set = Json::obj(vec![
        ("ref", Json::Str("A1".into())),
        ("text", Json::Str("5".into())),
    ]);
    for (verb, args) in [
        ("cell.set", set),
        ("sheet.add", Json::Null),
        ("wb.save", Json::Null),
        (
            "wb.set-properties",
            Json::obj(vec![("title", Json::Str("x".into()))]),
        ),
    ] {
        let err = run_control(&mut app, verb, &args).unwrap_err();
        assert_eq!(err, PROTECTED_STATUS, "{verb}");
    }
    assert_eq!(fingerprint(&app), before);
    assert!(!app.modified);
    assert_eq!(std::fs::read(&book).unwrap(), before_file);
    // Reads work.
    let get = Json::obj(vec![("ref", Json::Str("A1".into()))]);
    run_control(&mut app, "cell.get", &get).unwrap();
    run_control(&mut app, "sheet.read", &Json::Null).unwrap();
    run_control(
        &mut app,
        "find",
        &Json::obj(vec![("query", Json::Str("1".into()))]),
    )
    .unwrap();
    run_control(&mut app, "wb.export-csv", &Json::Null).unwrap();
    run_control(&mut app, "wb.recalc", &Json::Null).unwrap();
    let path = run_control(&mut app, "wb.path", &Json::Null).unwrap();
    assert!(matches!(path.get("protected"), Some(Json::Bool(true))));
    // An unknown verb is still unknown.
    let err = run_control(&mut app, "no.such", &Json::Null).unwrap_err();
    assert!(err.contains("unknown verb"), "{err}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// `control::VERBS` names exactly the verbs `dispatch` routes, so a new
/// verb has to be classified for Protected View before it can ship.
#[test]
fn every_control_verb_is_classified_for_protected_view() {
    let src = include_str!("control.rs");
    let start = src.find("pub fn dispatch(").unwrap();
    let end = start + src[start..].find("other => Err(").unwrap();
    let mut routed: Vec<&str> = src[start..end]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let arms = l
                .strip_suffix("=> {")
                .or_else(|| l.split_once(" => ").map(|(a, _)| a))?;
            arms.split('|')
                .map(|a| a.trim().strip_prefix('"')?.strip_suffix('"'))
                .collect::<Option<Vec<_>>>()
        })
        .flatten()
        .collect();
    routed.sort_unstable();
    routed.dedup();
    let mut listed: Vec<&str> = control::VERBS.iter().map(|&(v, _)| v).collect();
    listed.sort_unstable();
    assert_eq!(routed, listed);
    assert!(routed.len() > 60, "{routed:?}");
}

/// Enable Editing trusts the file as it was when it opened: one replaced
/// while it was protected is not trusted with it, and reopens protected.
#[test]
fn enable_editing_trusts_the_file_as_opened_not_as_replaced() {
    let (dir, book, mut app) = protected_app("stamp");
    let bytes = std::fs::read(&book).unwrap();
    std::fs::write(&book, [bytes.as_slice(), b"replaced"].concat()).unwrap();
    app.enable_editing().unwrap();
    app.open_workbook(book.to_str().unwrap());
    assert!(
        app.protected_view.is_some(),
        "the replaced file was never the one enabled"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
