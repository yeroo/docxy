use super::*;
use crate::sheet_ribbon::{resolve_on, tab_def};
use core::prelude::v1::test;

const KINDS: [(Kind, &str); 3] = [
    (Kind::Docx, "docx"),
    (Kind::Xlsx, "xlsx"),
    (Kind::Project, "project"),
];

#[test]
fn every_ribbon_kind_ends_with_help() {
    for (kind, name) in KINDS {
        let set = ribbon_tab_set(kind);
        let last = set.last().unwrap();
        assert!(last.0 == Some(RibbonTab::Help), "{name}");
        assert_eq!((last.1, last.2), ("Help", "Y"), "{name}");
        // Word's and Excel's Alt+Y is Help's alone.
        assert_eq!(set.iter().filter(|t| t.2 == "Y").count(), 1, "{name}");
        // KeyTips and the drawn body index `ribbon_for` by the tab set's
        // position: Help must be last in both.
        let drawn: Vec<&str> = ribbon_for(kind).tabs.iter().map(|t| t.name).collect();
        let named: Vec<&str> = set[1..].iter().map(|t| t.1).collect();
        assert_eq!(drawn, named, "{name}");
        let keys: Vec<&str> = ribbon_for(kind).tabs.iter().map(|t| t.key_tip).collect();
        let tips: Vec<&str> = set[1..].iter().map(|t| t.2).collect();
        assert_eq!(keys, tips, "{name}");
        assert!(valid_ribbon_tab(kind, RibbonTab::Help, false, false, false) == RibbonTab::Help);
    }
}

#[test]
fn the_help_tab_has_the_help_and_about_groups() {
    let tab = help_tab();
    let groups: Vec<&str> = tab.groups.iter().map(|g| g.title).collect();
    assert_eq!(groups, ["Help", "About"]);
    let mut labels = Vec::new();
    for g in &tab.groups {
        for c in &g.items {
            match c {
                Control::Large(cmd) => labels.push((cmd.label, cmd.act)),
                Control::Column(cmds) => labels.extend(cmds.iter().map(|c| (c.label, c.act))),
                _ => panic!("the Help tab has only buttons"),
            }
        }
    }
    let names: Vec<&str> = labels.iter().map(|l| l.0).collect();
    assert_eq!(
        names,
        [
            "Help",
            "Contact Support",
            "Feedback",
            "Show Training",
            "What's New",
            "About docxy suite"
        ]
    );
    for g in &tab.groups {
        for c in &g.items {
            let cmds: Vec<&rs::Cmd<Act>> = match c {
                Control::Large(cmd) => vec![cmd],
                Control::Column(cmds) => cmds.iter().collect(),
                _ => vec![],
            };
            for cmd in cmds {
                let svg = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/icons")
                    .join(format!("{}.svg", cmd.icon.0));
                assert!(svg.exists(), "{}", cmd.icon.0);
                assert!(!cmd.key_tip.is_empty(), "{}", cmd.label);
            }
        }
    }
    for (label, act) in labels {
        let on = !matches!(label, "Show Training" | "What's New");
        assert_eq!(act_enabled(act), on, "{label}");
    }
}

#[test]
fn the_sheet_help_tab_resolves_each_command() {
    let tab = tab_def(RibbonTab::Help);
    assert!(tab.tab == RibbonTab::Help, "a workbook would draw Home");
    for (query, act) in [
        ("Help", HelpAct::Help),
        ("Contact Support", HelpAct::ContactSupport),
        ("Feedback", HelpAct::Feedback),
        ("About docxy suite", HelpAct::About),
    ] {
        let cmd = resolve_on(tab, "Help", query, |_| false).unwrap();
        assert_eq!(cmd.act, SheetAct::Help(act), "{query}");
    }
    for query in ["Show Training", "What's New"] {
        assert_eq!(
            resolve_on(tab, "Help", query, |_| false).err(),
            Some(format!("'{query}' is not implemented"))
        );
    }
}

#[test]
fn f1_alone_is_help() {
    let key = |s: &str| Keystroke::parse(s).unwrap();
    assert!(is_help_key(&key("f1")));
    assert!(
        !is_help_key(&key("ctrl-f1")),
        "Ctrl+F1 collapses the ribbon"
    );
    assert!(!is_help_key(&key("shift-f1")));
    assert!(!is_help_key(&key("f2")));
}

/// Every command id a ribbon control carries: buttons, toggles, split
/// primaries and menus, drop-downs and their items, row cells and galleries
/// (a gallery's items have no id). A group launcher runs an act and carries no
/// id, so it has nothing to resolve.
fn control_ids(c: &Control<Act>, out: &mut Vec<&'static str>) {
    match c {
        Control::Large(cmd) | Control::Toggle(cmd) => out.push(cmd.id),
        Control::Column(cmds) => out.extend(cmds.iter().map(|c| c.id)),
        Control::Split { primary, menu } => {
            out.push(primary.id);
            out.extend(menu.iter().map(|c| c.id));
        }
        Control::Dropdown { cmd, items } => {
            out.push(cmd.id);
            out.extend(items.iter().map(|c| c.id));
        }
        Control::Gallery(g) => out.push(g.id),
        Control::Rows(rows) => {
            for cell in rows.iter().flatten() {
                out.push(match cell {
                    rs::Cell::Btn(cmd) | rs::Cell::Combo { cmd, .. } => cmd.id,
                });
            }
        }
        Control::Separator => {}
    }
}

/// `(command id, tab name)` for every command a kind's ribbon draws: the
/// document's and the plan's `ribbon_for` tabs, their contextual tabs and the
/// tabs `active_ribbon_tab_def` builds apart; the sheet's `SHEET_RIBBON`
/// (`ribbon_for(Xlsx)` names the tabs but draws the document's commands).
fn ribbon_command_ids(kind: Kind) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if kind == Kind::Xlsx {
        for tab in crate::sheet_ribbon::SHEET_RIBBON {
            let name = ribbon_tab_name(tab.tab);
            out.extend(tab.commands().into_iter().map(|c| (c.id, name)));
        }
        return out;
    }
    let ribbon = ribbon_for(kind);
    let mut tabs: Vec<(&'static str, Vec<rs::Group<Act>>)> = ribbon
        .tabs
        .into_iter()
        .map(|t| (t.name, t.groups))
        .collect();
    tabs.extend(ribbon.contextual.into_iter().map(|t| (t.name, t.groups)));
    let extra = if kind == Kind::Project {
        vec![crate::gantt_format_tab()]
    } else {
        vec![
            crate::table_tab::table_design_tab(),
            crate::table_tab::table_layout_tab(),
            crate::hf_tab::hf_tab(),
        ]
    };
    tabs.extend(extra.into_iter().map(|t| (t.name, t.groups)));
    for (name, groups) in tabs {
        let mut ids = Vec::new();
        for g in &groups {
            for c in &g.items {
                control_ids(c, &mut ids);
            }
        }
        out.extend(ids.into_iter().map(|id| (id, name)));
    }
    out
}

/// The fixture manual of a kind's editor, until the spec repos publish the
/// real ones into `docs/manual/`.
fn fixture_manual(kind: Kind) -> helpdocs::Manual {
    let editor = match kind {
        Kind::Xlsx => "excel",
        Kind::Project => "project",
        _ => "word",
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../helpdocs/tests/fixture")
        .join(editor);
    helpdocs::Manual::load(&dir).expect("fixture manual")
}

/// Every ribbon command resolves to a page, its own or its area's, never an
/// error (#1022). A report: it fails only where a manual marks an area strict
/// (the word fixture's `help`), so a Help command added without a page fails.
#[test]
fn every_ribbon_command_has_a_help_target() {
    for (kind, name) in KINDS {
        let ids = ribbon_command_ids(kind);
        assert!(ids.len() > 20, "{name}: {} ids", ids.len());
        let manual = fixture_manual(kind);
        assert_eq!(manual.problems, vec![], "{name}");
        let c = helpdocs::coverage(&manual, ids.iter().copied());
        println!("{name}: {}", c.report());
        assert!(c.passes(), "{name}: {}", c.report());
    }
    // The strict area bites: the document's Help tab is covered command by
    // command, so its six ids are exact.
    let ids = ribbon_command_ids(Kind::Docx);
    let help: Vec<_> = ids
        .iter()
        .filter(|(_, tab)| *tab == "Help")
        .copied()
        .collect();
    assert_eq!(help.len(), 6, "{help:?}");
    let c = helpdocs::coverage(&fixture_manual(Kind::Docx), help);
    assert_eq!(c.exact.len(), 6);
    // An unknown Help command would fail.
    let c = helpdocs::coverage(&fixture_manual(Kind::Docx), [("help-new", "Help")]);
    assert!(!c.passes());
}

// ---- F1 and Help open the manual (#1855) -----------------------------------

fn word() -> helpdocs::Manual {
    crate::help_manual::tests::fixture_bundle(HelpEditor::Word).manual
}

fn page_of(ask: &HelpAsk, m: &helpdocs::Manual) -> (String, helpdocs::Via) {
    let t = ask.resolve(m).expect("a page");
    (t.page, t.via)
}

#[test]
fn f1_opens_the_area_page_of_the_ribbon_tab_shown() {
    use helpdocs::Via;
    let m = word();
    let ask = f1_ask(Some(Kind::Docx), "Home", None);
    assert_eq!(ask.editor, HelpEditor::Word);
    assert_eq!(page_of(&ask, &m), ("home".into(), Via::Area));
    let ask = f1_ask(Some(Kind::Docx), "Insert", None);
    assert_eq!(page_of(&ask, &m), ("insert".into(), Via::Area));
    // An area the manual has no page for: its root.
    let ask = f1_ask(Some(Kind::Docx), "Mailings", None);
    assert_eq!(page_of(&ask, &m), ("welcome".into(), Via::Root));
    // Each tab kind reads its own editor's manual.
    assert_eq!(
        f1_ask(Some(Kind::Xlsx), "Home", None).editor,
        HelpEditor::Excel
    );
    assert_eq!(
        f1_ask(Some(Kind::Project), "Task", None).editor,
        HelpEditor::Project
    );
    assert_eq!(f1_ask(None, "Home", None).editor, HelpEditor::Word);
    // On a Help tab, that manual's root, whatever ribbon tab was shown.
    let ask = f1_ask(Some(Kind::Help), "Home", Some(HelpEditor::Excel));
    assert_eq!((ask.editor, ask.area.as_deref()), (HelpEditor::Excel, None));
    assert_eq!(
        page_of(
            &f1_ask(Some(Kind::Help), "Home", Some(HelpEditor::Word)),
            &m
        )
        .0,
        "welcome"
    );
}

#[test]
fn help_opens_the_help_commands_page() {
    use helpdocs::Via;
    let ask = help_command_ask(Some(Kind::Docx), None);
    assert_eq!(page_of(&ask, &word()), ("help".into(), Via::Exact));
    assert_eq!(
        help_command_ask(Some(Kind::Xlsx), None).editor,
        HelpEditor::Excel
    );
    // On an Excel or Project Help tab (the macOS menu bar's Help reaches
    // one), that tab's manual, not Word's.
    for editor in [HelpEditor::Excel, HelpEditor::Project, HelpEditor::Word] {
        assert_eq!(
            help_command_ask(Some(Kind::Help), Some(editor)).editor,
            editor
        );
    }
}

// ---- F1 on a hovered ribbon command (#1869) ---------------------------------

fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
}

#[test]
fn ribbon_cmd_at_finds_the_command_under_the_pointer() {
    let frame = vec![
        ("ribbon-group:Font".to_string(), rect(0., 0., 200., 90.)),
        (ribbon_cmd_probe("bold"), rect(10., 10., 22., 22.)),
        (ribbon_cmd_probe("italic"), rect(34., 10., 22., 22.)),
        ("ribbon-split:paste".to_string(), rect(300., 0., 40., 90.)),
    ];
    let at = |x: f32, y: f32| Some(point(px(x), px(y)));
    assert_eq!(ribbon_cmd_at(&frame, at(20., 20.)), Some("bold"));
    assert_eq!(ribbon_cmd_at(&frame, at(40., 30.)), Some("italic"));
    // In the group but on no command, on another probe, outside all.
    assert_eq!(ribbon_cmd_at(&frame, at(100., 60.)), None);
    assert_eq!(ribbon_cmd_at(&frame, at(310., 40.)), None);
    assert_eq!(ribbon_cmd_at(&frame, at(900., 500.)), None);
    // The pointer out of the window: no command, wherever it last was.
    assert_eq!(ribbon_cmd_at(&frame, None), None);
    // Two commands under the pointer (a flyout drawn over the ribbon): the
    // last drawn, painted on top.
    let mut over = frame.clone();
    over.push((ribbon_cmd_probe("fontcolor"), rect(0., 0., 60., 60.)));
    assert_eq!(ribbon_cmd_at(&over, at(20., 20.)), Some("fontcolor"));
    // An extension's own probe is not a second name for the command.
    let ext = vec![(crate::ext_cmd_probe("fx.reply"), rect(0., 0., 50., 50.))];
    assert_eq!(ribbon_cmd_at(&ext, at(5., 5.)), None);
}

#[test]
fn f1_on_a_hovered_command_opens_its_page() {
    use helpdocs::Via;
    let m = word();
    // The document ribbon's Bold is `b` (the sheet's is `bold`).
    let ask = f1_command_ask(Some(Kind::Docx), "b", "Home");
    assert_eq!(ask.editor, HelpEditor::Word);
    assert_eq!(page_of(&ask, &m), ("home-font".into(), Via::Exact));
    let ask = f1_command_ask(Some(Kind::Docx), "table", "Insert");
    assert_eq!(page_of(&ask, &m), ("tables".into(), Via::Exact));
    // A command no page keys: its area's page, never an error.
    let ask = f1_command_ask(Some(Kind::Docx), "symbol", "Insert");
    assert_eq!(page_of(&ask, &m), ("insert".into(), Via::Area));
    // Nor an area page: the root.
    let ask = f1_command_ask(Some(Kind::Docx), "mailmerge", "Mailings");
    assert_eq!(page_of(&ask, &m), ("welcome".into(), Via::Root));
    // A disabled command has its page too.
    let ask = f1_command_ask(Some(Kind::Docx), "showtraining", "Help");
    assert_eq!(page_of(&ask, &m), ("help".into(), Via::Exact));
    // The sheet's commands read the Excel manual.
    let x = crate::help_manual::tests::fixture_bundle(HelpEditor::Excel).manual;
    let ask = f1_command_ask(Some(Kind::Xlsx), "bold", "Home");
    assert_eq!(ask.editor, HelpEditor::Excel);
    assert_eq!(page_of(&ask, &x), ("home".into(), Via::Exact));
    let ask = f1_command_ask(Some(Kind::Xlsx), "freeze-panes", "View");
    assert_eq!(page_of(&ask, &x), ("excel".into(), Via::Root));
    assert_eq!(
        f1_command_ask(Some(Kind::Project), "gantt", "Task").editor,
        HelpEditor::Project
    );
}

#[test]
fn f1_over_a_dialog_opens_its_page() {
    use helpdocs::Via;
    let m = word();
    let ask = dialog_ask(Some(Kind::Docx), "insert-table", "Insert");
    assert_eq!(ask.id.as_deref(), Some("dialog:insert-table"));
    assert_eq!(page_of(&ask, &m), ("tables".into(), Via::Exact));
    // A dialog no page names: the ribbon tab's area page.
    let ask = dialog_ask(Some(Kind::Docx), "page-setup", "Home");
    assert_eq!(page_of(&ask, &m), ("home".into(), Via::Area));
}

#[test]
fn f1_opens_help_only_over_a_plain_form() {
    use crate::dialog::{ButtonRole, Dialog, DialogOwner, Popup};
    let ed = Editor::new(empty_doc());
    let form = crate::table_dialogs::insert_table_dialog(&ed).unwrap();
    assert!(dialog_takes_help(&form, false));
    assert!(!dialog_takes_help(&form, true), "a window close is asking");
    let mut popup = form.clone();
    popup.popup = Some(Popup {
        x: 0.,
        y: 0.,
        width: 1.,
        list_h: 1.,
    });
    assert!(!dialog_takes_help(&popup, false));
    let message = Dialog::message(
        crate::dialog::catalog::INSERT_TABLE,
        "Note",
        "Something happened".into(),
        &[("OK", ButtonRole::Accept)],
        DialogOwner::Test,
    );
    assert!(!dialog_takes_help(&message, false), "a message box");
    for owner in [
        DialogOwner::SaveOnClose { quit: false },
        DialogOwner::Message,
        DialogOwner::About,
    ] {
        let mut d = form.clone();
        d.owner = owner;
        assert!(!dialog_takes_help(&d, false), "{:?}", d.owner);
    }
}

#[test]
fn f1_is_a_plain_f1_only() {
    let plain = Modifiers::default();
    assert!(is_help_stroke("f1", plain));
    assert!(!is_help_stroke(
        "f1",
        Modifiers {
            shift: true,
            ..plain
        }
    ));
    assert!(!is_help_stroke("f2", plain));
}

/// Every catalogued dialog's `dialog:<id>` resolves to a page, its own or
/// the manual's root, never an error (#1855: F1 over a dialog and its Help
/// button ask for it). A report, like the commands' (no dialog area is
/// strict): the ids the fixtures key exactly are pinned.
#[test]
fn every_dialog_has_a_help_target() {
    use crate::dialog::catalog::{Surface as DSurface, catalog};
    for (kind, name) in KINDS {
        let surface = match kind {
            Kind::Xlsx => DSurface::Sheet,
            Kind::Project => DSurface::Project,
            _ => DSurface::Doc,
        };
        let ids: Vec<String> = catalog()
            .iter()
            .filter(|e| e.surface == surface)
            .map(|e| format!("dialog:{}", e.dialog.as_str()))
            .collect();
        assert!(!ids.is_empty(), "{name}");
        let manual = fixture_manual(kind);
        let c = helpdocs::coverage(&manual, ids.iter().map(|id| (id.as_str(), "")));
        println!("{name} dialogs: {}", c.report());
        // Not `passes`: the strict areas are ribbon tabs, which no dialog
        // id names, so they read as unused here.
        assert!(c.strict_failures.is_empty(), "{name}: {}", c.report());
        assert!(c.unresolved.is_empty(), "{name}: {:?}", c.unresolved);
    }
    // Insert Table is catalogued, walked above, and keyed by the fixture.
    let insert_table = format!("dialog:{}", crate::dialog::catalog::INSERT_TABLE.as_str());
    assert!(
        catalog()
            .iter()
            .any(|e| format!("dialog:{}", e.dialog.as_str()) == insert_table),
        "{insert_table} is catalogued"
    );
    let c = helpdocs::coverage(&fixture_manual(Kind::Docx), [(insert_table.as_str(), "")]);
    assert_eq!(c.exact.len(), 1, "{}", c.report());
}

/// #1855: a Help tab draws no ribbon, so the ribbon tab the document, sheet
/// or plan showed is kept while it is active, never reset to the default.
#[test]
fn a_help_tab_keeps_the_ribbon_tab_shown_behind_it() {
    for tab in [
        RibbonTab::Layout,
        RibbonTab::Design,
        RibbonTab::Mailings,
        RibbonTab::HeaderFooter,
        RibbonTab::TableDesign,
        RibbonTab::Resource,
        RibbonTab::GanttFormat,
        RibbonTab::Formulas,
    ] {
        assert!(
            crate::valid_ribbon_tab(Kind::Help, tab, false, false, false) == tab,
            "{}",
            ribbon_tab_name(tab)
        );
    }
    // A document still resets a tab that is not its own.
    assert!(
        crate::valid_ribbon_tab(Kind::Docx, RibbonTab::Resource, false, false, false)
            == RibbonTab::Home
    );
}
