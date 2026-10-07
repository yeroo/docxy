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
