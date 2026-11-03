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
    let mut tabs: Vec<(&'static str, Vec<rs::Group<Act>>)> =
        ribbon.tabs.into_iter().map(|t| (t.name, t.groups)).collect();
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
    let help: Vec<_> = ids.iter().filter(|(_, tab)| *tab == "Help").copied().collect();
    assert_eq!(help.len(), 6, "{help:?}");
    let c = helpdocs::coverage(&fixture_manual(Kind::Docx), help);
    assert_eq!(c.exact.len(), 6);
    // An unknown Help command would fail.
    let c = helpdocs::coverage(&fixture_manual(Kind::Docx), [("help-new", "Help")]);
    assert!(!c.passes());
}
