use super::*;
use crate::dialog::Value;
use crate::dialog_host::dialog_click;
use core::prelude::v1::test;
use ctlcore::json::Json;
use docxcore::model::{Paragraph, Run};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../uiharness/fixtures")
        .join(name)
}

/// A .docx tab whose body is "Dear " and the caret at its end.
fn letter_tab() -> DocTab {
    let mut t = tab_from_path(&fixture("basic.docx"));
    let ed = body_editor(&mut t).unwrap();
    let mut doc = ed.doc.clone();
    let keep = doc.content_block_count();
    doc.body.splice(
        0..keep,
        [Block::Paragraph(Paragraph {
            content: vec![Inline::Run(Run {
                text: "Dear ".into(),
                ..Run::default()
            })],
            ..Default::default()
        })],
    );
    let mut ed = Editor::new(doc);
    ed.caret.offset = 5;
    t.surface = Surface::Doc(ed);
    t
}

fn attached() -> DocTab {
    let mut t = letter_tab();
    attach(&mut t, &fixture("recipients.csv")).unwrap();
    t
}

fn body_text(t: &mut DocTab) -> String {
    body_editor(t)
        .unwrap()
        .doc
        .plain_text()
        .replace(docxcore::merge::preview::EMPTY_PREVIEW, "")
}

fn open_dialog(t: &mut DocTab, act: MailAct) {
    let d = crate::mailings_dialogs::open(t, act).unwrap();
    t.dialogs.push(d);
}

fn set(t: &mut DocTab, control: &str, value: Json) {
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap();
}

/// Every command a group holds, by label, in order.
fn labels(group: &rs::Group<Act>) -> Vec<&'static str> {
    let mut out = Vec::new();
    for c in &group.items {
        match c {
            Control::Large(cmd) | Control::Toggle(cmd) => out.push(cmd.label),
            Control::Dropdown { cmd, .. } => out.push(cmd.label),
            Control::Column(cmds) => out.extend(cmds.iter().map(|c| c.label)),
            Control::Rows(rows) => {
                for cell in rows.iter().flatten() {
                    match cell {
                        rs::Cell::Btn(cmd) | rs::Cell::Combo { cmd, .. } => out.push(cmd.label),
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn the_tab_has_words_groups_between_layout_and_review() {
    let names: Vec<&str> = ribbon_for(Kind::Docx).tabs.iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        ["Home", "Insert", "Layout", "Mailings", "Review", "View"]
    );
    assert_eq!(ribbon_tab_set(Kind::Docx)[4].1, "Mailings");
    for kind in [Kind::Xlsx, Kind::Project] {
        assert!(ribbon_for(kind).tabs.iter().all(|t| t.name != "Mailings"));
        assert!(ribbon_tab_set(kind).iter().all(|t| t.1 != "Mailings"));
    }
    let tab = mailings_tab();
    assert_eq!(tab.key_tip, "M");
    let groups: Vec<&str> = tab.groups.iter().map(|g| g.title).collect();
    assert_eq!(
        groups,
        [
            "Create",
            "Start Mail Merge",
            "Write & Insert Fields",
            "Preview Results",
            "Finish"
        ]
    );
    assert_eq!(labels(&tab.groups[0]), ["Envelopes", "Labels"]);
    assert_eq!(
        labels(&tab.groups[1]),
        [
            "Start Mail Merge",
            "Select Recipients",
            "Edit Recipient List"
        ]
    );
    assert_eq!(
        labels(&tab.groups[2]),
        [
            "Highlight Merge Fields",
            "Address Block",
            "Greeting Line",
            "Insert Merge Field",
            "Rules",
            "Match Fields",
            "Update Labels"
        ]
    );
    assert_eq!(
        labels(&tab.groups[3]),
        [
            "Preview Results",
            "First Record",
            "Previous Record",
            "Go to Record",
            "Next Record",
            "Last Record",
            "Find Recipient",
            "Check for Errors"
        ]
    );
    assert_eq!(labels(&tab.groups[4]), ["Finish & Merge"]);
}

#[test]
fn menus_have_words_items_and_the_follow_ups_are_disabled_with_a_tip() {
    let state = MailState::default();
    let items = |m| -> Vec<(String, bool)> {
        menu_items(&state, m)
            .into_iter()
            .filter_map(|i| match i {
                menu::MenuItem::Item(e) => Some((e.label, e.enabled)),
                _ => None,
            })
            .collect()
    };
    let start = items(MailMenu::StartMailMerge);
    assert_eq!(
        start.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(),
        [
            "Letters",
            "E-mail Messages",
            "Envelopes...",
            "Labels...",
            "Directory",
            "Normal Word Document",
            "Step-by-Step Mail Merge Wizard..."
        ]
    );
    assert!(start[..6].iter().all(|i| i.1));
    assert!(!start[6].1, "the wizard is a follow-up");
    let recipients = items(MailMenu::SelectRecipients);
    assert_eq!(recipients[1], ("Use an Existing List...".into(), true));
    assert!(!recipients[0].1 && !recipients[2].1);
    let finish = items(MailMenu::Finish);
    assert_eq!(finish[0].0, "Edit Individual Documents...");
    assert!(
        !finish[1].1 && !finish[2].1,
        "print and e-mail are follow-ups"
    );
    // Every placeholder says it is a follow-up, in ribbon-read's tip.
    let tab = mailings_tab();
    let mut placeholders = 0;
    for g in &tab.groups {
        for c in &g.items {
            if let Control::Dropdown { items, .. } = c {
                for cmd in items
                    .iter()
                    .filter(|c| matches!(c.act, Act::Mail(MailAct::Unavailable)))
                {
                    assert!(cmd.tip.body.contains("follow-up"), "{}", cmd.label);
                    assert!(!act_enabled(cmd.act), "{}", cmd.label);
                    placeholders += 1;
                }
            }
        }
    }
    assert_eq!(placeholders, 11);
}

#[test]
fn data_commands_wait_for_a_recipient_list() {
    let none = MailState::default();
    use MailAct as M;
    for act in [
        M::EditRecipients,
        M::Highlight,
        M::AddressBlock,
        M::GreetingLine,
        M::Menu(MailMenu::InsertMergeField),
        M::Menu(MailMenu::Rules),
        M::MatchFields,
        M::Preview,
        M::First,
        M::Previous,
        M::Next,
        M::Last,
        M::Menu(MailMenu::Record),
        M::FindRecipient,
        M::CheckErrors,
        M::Menu(MailMenu::Finish),
        M::EditIndividual,
        M::UpdateLabels,
    ] {
        assert!(!mail_enabled(&none, act), "{act:?} with no list");
        assert!(!act_enabled(Act::Mail(act)), "{act:?} in the static ribbon");
    }
    for act in [
        M::Envelopes,
        M::Labels,
        M::Menu(MailMenu::StartMailMerge),
        M::Menu(MailMenu::SelectRecipients),
        M::UseExistingList,
    ] {
        assert!(mail_enabled(&none, act), "{act:?}");
    }
    let t = attached();
    for act in [
        M::EditRecipients,
        M::Preview,
        M::EditIndividual,
        M::Next,
        M::Last,
    ] {
        assert!(mail_enabled(&t.mail, act), "{act:?} with a list");
    }
    // On the first record there is nothing before it.
    assert!(!mail_enabled(&t.mail, M::First) && !mail_enabled(&t.mail, M::Previous));
    // Update Labels needs a labels document.
    assert!(!mail_enabled(&t.mail, M::UpdateLabels));
}

#[test]
fn attaching_a_list_writes_the_settings_and_lists_the_columns() {
    let mut t = letter_tab();
    assert!(!t.dirty);
    attach(&mut t, &fixture("recipients.csv")).unwrap();
    assert!(t.dirty);
    assert_eq!(t.status.as_ref(), "Recipients: 3 from recipients.csv");
    assert_eq!(t.mail.doc_type, Some(MainDocType::Letters));
    let mm = t.pkg.as_ref().unwrap().mail_merge().unwrap();
    assert!(mm.source.unwrap().ends_with("recipients.csv"));
    let fields: Vec<String> = menu_items(&t.mail, MailMenu::InsertMergeField)
        .into_iter()
        .filter_map(|i| match i {
            menu::MenuItem::Item(e) if e.enabled => Some(e.label),
            _ => None,
        })
        .collect();
    assert_eq!(
        fields,
        [
            "First Name",
            "Last Name",
            "Company",
            "Address 1",
            "City",
            "State",
            "Postal Code"
        ]
    );
    // Not a list.
    let mut t = letter_tab();
    assert!(attach(&mut t, &fixture("basic.docx")).is_err());
    assert!(t.mail.recipients.is_none());
}

#[test]
fn preview_and_next_show_the_records_values_in_the_document() {
    let mut t = attached();
    mail_apply(&mut t, MailAct::Field(0)).unwrap();
    assert_eq!(body_text(&mut t), "Dear \u{AB}First Name\u{BB}\n");
    mail_apply(&mut t, MailAct::Preview).unwrap();
    assert_eq!(body_text(&mut t), "Dear Jane\n");
    mail_apply(&mut t, MailAct::Next).unwrap();
    assert_eq!(body_text(&mut t), "Dear John\n");
    assert_eq!(t.mail.record_text(), "2");
    assert_eq!(t.status.as_ref(), "Record 2 of 3");
    mail_apply(&mut t, MailAct::Last).unwrap();
    assert_eq!(body_text(&mut t), "Dear Amy\n");
    assert!(mail_apply(&mut t, MailAct::Next).is_err());
    mail_apply(&mut t, MailAct::Record(0)).unwrap();
    assert_eq!(body_text(&mut t), "Dear Jane\n");
    // A field inserted while previewing shows the record too.
    mail_apply(&mut t, MailAct::Field(4)).unwrap();
    assert_eq!(body_text(&mut t), "Dear JaneSpringfield\n");
    mail_apply(&mut t, MailAct::Preview).unwrap();
    assert_eq!(
        body_text(&mut t),
        "Dear \u{AB}First Name\u{BB}\u{AB}City\u{BB}\n"
    );
    // The previewed record is not saved.
    let ed = body_editor(&mut t).unwrap();
    let xml = docxcore::serialize::document_to_xml(&ed.doc);
    assert!(!xml.contains("Jane"), "{xml}");
}

#[test]
fn finish_edit_individual_documents_makes_one_copy_per_recipient() {
    let mut t = attached();
    mail_apply(&mut t, MailAct::Field(0)).unwrap();
    mail_apply(&mut t, MailAct::Preview).unwrap();
    open_dialog(&mut t, MailAct::EditIndividual);
    assert_eq!(t.dialogs.top_id(), "mail-merge-new");
    dialog_click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
    let (pkg, title) = t.mail.new_tab.take().unwrap();
    assert_eq!(title, "Letters1.docx");
    assert_eq!(t.status.as_ref(), "Merged 3 records");
    let text: Vec<String> = pkg.document.body[..pkg.document.content_block_count()]
        .iter()
        .map(|b| b.plain_text())
        .collect();
    assert_eq!(text, ["Dear Jane", "Dear John", "Dear Amy"]);
    assert_eq!(pkg.mail_merge(), None);
    // From 2 to 3.
    open_dialog(&mut t, MailAct::EditIndividual);
    set(&mut t, "records", Json::Str("From".into()));
    set(&mut t, "from", Json::Num(2.0));
    set(&mut t, "to", Json::Num(3.0));
    dialog_click(&mut t, "OK").unwrap();
    let (pkg, _) = t.mail.new_tab.take().unwrap();
    assert_eq!(pkg.document.plain_text(), "Dear John\nDear Amy\n");
}

#[test]
fn a_documents_own_source_is_read_only_after_yes() {
    // The document names the list in its settings; opening reads nothing.
    let mut t = attached();
    let source = t.mail.recipients.as_ref().unwrap().source.clone().unwrap();
    let pkg = t.pkg.clone().unwrap();
    let state = MailState::from_pkg(Some(&pkg));
    assert!(state.recipients.is_none(), "not read at open");
    assert_eq!(state.pending_source.as_deref(), Some(source.as_str()));
    assert_eq!(state.doc_type, Some(MainDocType::Letters));
    // Data commands are on: the first one asks.
    assert!(mail_enabled(&state, MailAct::Preview));
    t.mail = state;
    t.dialogs.push(crate::mailings_dialogs::attach_confirm(
        &source,
        MailAct::Preview,
    ));
    dialog_click(&mut t, "No").unwrap();
    assert!(t.mail.recipients.is_none());
    t.dialogs.push(crate::mailings_dialogs::attach_confirm(
        &source,
        MailAct::Preview,
    ));
    assert!(
        t.dialogs
            .top()
            .unwrap()
            .text
            .as_deref()
            .unwrap()
            .contains("SELECT * FROM")
    );
    dialog_click(&mut t, "Yes").unwrap();
    assert_eq!(t.mail.recipients.as_ref().unwrap().rows.len(), 3);
    assert_eq!(t.mail.after_attach, Some(MailAct::Preview));
    // A relative or non-list source is never offered.
    assert!(
        pending_problem("list.csv")
            .unwrap()
            .contains("not a full path")
    );
    assert!(
        pending_problem("C:\\data\\x.mdb")
            .unwrap()
            .contains("not a .csv")
    );
    assert_eq!(
        pending_problem(&fixture("recipients.csv").display().to_string()),
        None
    );
}

/// r1 M4: the checks before the question touch no file system, so a source
/// on another machine is refused before any dialog (asking whether a UNC
/// path exists would reach its server); a missing local file is asked about
/// and reported by the read after Yes.
#[test]
fn a_remote_source_is_refused_before_the_question() {
    for remote in [
        "\\\\attacker\\share\\x.csv",
        "\\\\?\\UNC\\attacker\\share\\x.csv",
        "\\\\?\\C:\\data\\x.csv",
        "\\\\.\\pipe\\x.csv",
        "//attacker/share/x.csv",
    ] {
        let why = pending_problem(remote).unwrap_or_else(|| panic!("{remote} offered"));
        assert!(why.contains("not on this computer"), "{remote}: {why}");
    }
    let missing = fixture("nope.csv").display().to_string();
    assert_eq!(pending_problem(&missing), None, "asked about, not checked");
    let mut t = letter_tab();
    t.mail.pending_source = Some(missing.clone());
    t.dialogs.push(crate::mailings_dialogs::attach_confirm(
        &missing,
        MailAct::Preview,
    ));
    let err = dialog_click(&mut t, "Yes").unwrap_err();
    assert!(err.contains("cannot read"), "{err}");
    assert!(t.mail.recipients.is_none());
    assert!(!t.dialogs.is_open(), "nothing to retry");
}

#[test]
fn edit_recipient_list_excludes_a_recipient() {
    let mut t = attached();
    open_dialog(&mut t, MailAct::EditRecipients);
    // The Recipient box picks a row; the Include box follows it.
    set(&mut t, "record", Json::Str("2".into()));
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.value("include"), Some(&Value::Bool(true)));
    assert_eq!(
        d.value("values"),
        Some(&Value::Text(
            "John · Smith · 9 Elm Rd · Rome · NY · 13440".into()
        ))
    );
    set(&mut t, "include", Json::Bool(false));
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(
        t.mail.recipients.as_ref().unwrap().included,
        [true, false, true]
    );
    assert_eq!(t.status.as_ref(), "2 recipients included");
    mail_apply(&mut t, MailAct::Next).unwrap();
    assert_eq!(t.mail.record, 2, "Next skips the excluded one");
}

#[test]
fn address_block_greeting_line_and_match_fields() {
    let mut t = attached();
    open_dialog(&mut t, MailAct::AddressBlock);
    assert_eq!(
        t.dialogs.top().unwrap().value("preview"),
        Some(&Value::Text(
            "Jane Doe / Acme / 1 Main St / Springfield, IL 62701".into()
        ))
    );
    set(&mut t, "company", Json::Bool(false));
    assert_eq!(
        t.dialogs.top().unwrap().value("preview"),
        Some(&Value::Text(
            "Jane Doe / 1 Main St / Springfield, IL 62701".into()
        ))
    );
    dialog_click(&mut t, "OK").unwrap();
    open_dialog(&mut t, MailAct::GreetingLine);
    set(&mut t, "punctuation", Json::Str(":".into()));
    dialog_click(&mut t, "OK").unwrap();
    mail_apply(&mut t, MailAct::Preview).unwrap();
    assert_eq!(
        body_text(&mut t),
        "Dear Jane Doe, 1 Main St, Springfield, IL 62701Dear Jane Doe:\n"
    );
    // Match Fields: the Company column as the last name.
    open_dialog(&mut t, MailAct::MatchFields);
    set(&mut t, "last", Json::Str("Company".into()));
    dialog_click(&mut t, "OK").unwrap();
    assert!(
        body_text(&mut t).ends_with("Dear Jane Acme:\n"),
        "{}",
        body_text(&mut t)
    );
}

#[test]
fn find_recipient_and_check_for_errors() {
    let mut t = attached();
    open_dialog(&mut t, MailAct::FindRecipient);
    set(&mut t, "find", Json::Str("austin".into()));
    dialog_click(&mut t, "Find Next").unwrap();
    assert_eq!(t.mail.record, 2);
    assert!(t.mail.preview);
    assert!(t.dialogs.is_open(), "Find Next keeps the dialog open");
    set(&mut t, "find", Json::Str("nobody".into()));
    assert!(dialog_click(&mut t, "Find Next").is_err());
    dialog_click(&mut t, "Cancel").unwrap();
    // A field the list has no column for.
    insert_inline(&mut t, |p| docxcore::merge::merge_field("Phone", p)).unwrap();
    open_dialog(&mut t, MailAct::CheckErrors);
    dialog_click(&mut t, "OK").unwrap();
    let report = t.dialogs.top().unwrap();
    assert_eq!(report.id, "mail-report");
    assert!(report.text.as_deref().unwrap().contains("Phone"));
    dialog_click(&mut t, "OK").unwrap();
    assert!(!t.dialogs.is_open());
}

#[test]
fn envelopes_and_labels() {
    // Create ▸ Envelopes adds a section at the start.
    let mut t = letter_tab();
    open_dialog(&mut t, MailAct::Envelopes);
    set(&mut t, "delivery", Json::Str("Jane Doe;1 Main St".into()));
    set(&mut t, "return", Json::Str("Acme\n9 Elm Rd".into()));
    dialog_click(&mut t, "Add to Document").unwrap();
    let ed = body_editor(&mut t).unwrap();
    assert_eq!(ed.sections().len(), 2);
    assert_eq!(
        ed.doc.plain_text(),
        "Acme\n9 Elm Rd\nJane Doe\n1 Main St\nDear \n"
    );
    assert!(t.dirty);
    // Create ▸ Labels makes a new document.
    open_dialog(&mut t, MailAct::Labels);
    set(&mut t, "address", Json::Str("Jane".into()));
    dialog_click(&mut t, "New Document").unwrap();
    let (pkg, title) = t.mail.new_tab.take().unwrap();
    assert_eq!(title, "Labels1.docx");
    let Block::Table(table) = &pkg.document.body[0] else {
        panic!("a label table")
    };
    assert_eq!(table.rows.len(), 10);
    // r1 m6: the new document is this one's package, styles and all, with
    // the sheet's page and no header or footer.
    let styles = |p: &Package| p.part("word/styles.xml").map(<[u8]>::to_vec);
    assert!(styles(&pkg).is_some());
    assert_eq!(styles(&pkg), styles(t.pkg.as_ref().unwrap()));
    let sect = pkg.sect_pr();
    assert!(!sect.contains("headerReference"), "{sect}");
    let setup = docxcore::sect::SectionSetup::parse(sect);
    assert_eq!((setup.page.w, setup.margins.top), (12240, 720));
    assert_eq!(pkg.mail_merge(), None);
    // A custom size is checked.
    open_dialog(&mut t, MailAct::Labels);
    set(&mut t, "product", Json::Str("Custom".into()));
    set(&mut t, "width", Json::Str("9".into()));
    assert!(dialog_click(&mut t, "New Document").is_err());
    assert_eq!(
        t.dialogs.top_id(),
        "mail-labels",
        "a refusal keeps the dialog"
    );
    dialog_click(&mut t, "Cancel").unwrap();
    // Start Mail Merge ▸ Labels asks before it replaces the text.
    let mut t = attached();
    open_dialog(&mut t, MailAct::StartLabels);
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(t.dialogs.top_id(), "mail-replace");
    dialog_click(&mut t, "OK").unwrap();
    assert_eq!(t.mail.doc_type, Some(MainDocType::Labels));
    assert!(matches!(
        body_editor(&mut t).unwrap().doc.body[0],
        Block::Table(_)
    ));
    assert_eq!(
        t.pkg.as_ref().unwrap().mail_merge().unwrap().doc_type,
        MainDocType::Labels
    );
    // Type in the first label, then Update Labels.
    mail_apply(&mut t, MailAct::Field(0)).unwrap();
    assert!(mail_enabled(&t.mail, MailAct::UpdateLabels));
    mail_apply(&mut t, MailAct::UpdateLabels).unwrap();
    mail_apply(&mut t, MailAct::Preview).unwrap();
    let text = body_text(&mut t);
    // Spacer columns between the labels; three records fill the first row.
    assert!(
        text.starts_with("Jane\t\tJohn\t\tAmy\n\t\t\t\t\n"),
        "{text}"
    );
}

#[test]
fn highlight_and_normal_document() {
    let mut t = attached();
    mail_apply(&mut t, MailAct::Highlight).unwrap();
    assert!(mail_checked(&t.mail, MailAct::Highlight));
    assert!(mail_checked(
        &t.mail,
        MailAct::DocType(MainDocType::Letters)
    ));
    mail_apply(&mut t, MailAct::DocType(MainDocType::Directory)).unwrap();
    assert_eq!(
        t.pkg.as_ref().unwrap().mail_merge().unwrap().doc_type,
        MainDocType::Directory
    );
    mail_apply(&mut t, MailAct::NormalDocument).unwrap();
    assert!(t.mail.recipients.is_none());
    assert_eq!(t.pkg.as_ref().unwrap().mail_merge(), None);
    assert!(mail_checked(&t.mail, MailAct::NormalDocument));
    assert!(mail_apply(&mut t, MailAct::Preview).is_err());
}
