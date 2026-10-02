//! The Word ribbon's Mailings tab (#628): Create, Start Mail Merge, Write &
//! Insert Fields, Preview Results and Finish, over `docxcore::merge`.
//!
//! A document's mail-merge state lives on its tab ([`MailState`]): the main
//! document type, the attached recipient list, Match Fields, the previewed
//! record and the Highlight / Preview toggles. Commands that need a list are
//! disabled until one is attached. A document that names a data source in
//! its `w:mailMerge` keeps the path but never reads it at open: the first
//! command that needs the data asks first, as Word's "Opening this document
//! will run the following SQL command" does.
//!
//! Preview only rewrites merge fields' displayed text through the body
//! editor ([`Editor::set_merge_preview`]): saving, undo and copy see the
//! fields as they are. Commands Word has that this PR leaves for a follow-up
//! are drawn disabled with a tip that says so.
use super::*;
use docxcore::merge::labels::LabelSpec;
use docxcore::merge::{FieldMap, MergeFieldKind, MergePreview, Recipients};
use docxcore::package::{MailMerge, MainDocType};

/// A Mailings tab command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MailAct {
    /// A drop-down button: open its menu.
    Menu(MailMenu),
    /// Create ▸ Envelopes…: add an envelope to the document.
    Envelopes,
    /// Create ▸ Labels…: a new document of labels.
    Labels,
    /// Start Mail Merge ▸ Letters, E-mail Messages, Directory.
    DocType(MainDocType),
    /// Start Mail Merge ▸ Envelopes…: the document becomes an envelope.
    StartEnvelopes,
    /// Start Mail Merge ▸ Labels…: the document becomes a sheet of labels.
    StartLabels,
    /// Start Mail Merge ▸ Normal Word Document.
    NormalDocument,
    /// Select Recipients ▸ Use an Existing List…
    UseExistingList,
    EditRecipients,
    Highlight,
    AddressBlock,
    GreetingLine,
    /// Insert Merge Field ▸ a column of the list (its index).
    Field(u16),
    /// Rules ▸ Next Record, Merge Record #, Merge Sequence #.
    Rule(Rule),
    MatchFields,
    UpdateLabels,
    Preview,
    First,
    Previous,
    Next,
    Last,
    /// The record box's menu ▸ a data-source row.
    Record(u16),
    FindRecipient,
    CheckErrors,
    /// Finish & Merge ▸ Edit Individual Documents…
    EditIndividual,
    /// A command Word has whose work is a follow-up: drawn disabled.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MailMenu {
    StartMailMerge,
    SelectRecipients,
    InsertMergeField,
    Rules,
    Record,
    Finish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    NextRecord,
    MergeRecord,
    MergeSequence,
}

impl Rule {
    fn kind(self) -> MergeFieldKind {
        match self {
            Rule::NextRecord => MergeFieldKind::Next,
            Rule::MergeRecord => MergeFieldKind::MergeRec,
            Rule::MergeSequence => MergeFieldKind::MergeSeq,
        }
    }
}

/// A tab's mail merge.
#[derive(Debug, Clone, Default)]
pub(crate) struct MailState {
    /// The main document type; `None` is a Normal Word Document.
    pub doc_type: Option<MainDocType>,
    pub recipients: Option<Recipients>,
    pub map: FieldMap,
    /// The data-source row Preview Results shows.
    pub record: usize,
    pub preview: bool,
    pub highlight: bool,
    /// The data source the document's `w:mailMerge` names, not read yet.
    pub pending_source: Option<String>,
    /// The command to run once the confirm before reading it says Yes.
    pub after_attach: Option<MailAct>,
    /// A document Finish & Merge or Create ▸ Labels made, for the app to
    /// open in a new tab: its package and title.
    pub new_tab: Option<(Package, String)>,
}

impl MailState {
    /// The state of a document as it opens: its main-document type, and the
    /// data source it names (recorded, not read).
    pub fn from_pkg(pkg: Option<&Package>) -> MailState {
        let mm = pkg.and_then(Package::mail_merge);
        MailState {
            doc_type: mm.as_ref().map(|m| m.doc_type),
            pending_source: mm.and_then(|m| m.source),
            ..MailState::default()
        }
    }

    /// Whether a recipient list is attached or named by the document.
    fn has_data(&self) -> bool {
        self.recipients.is_some() || self.pending_source.is_some()
    }

    /// The included rows' first and last.
    fn bounds(&self) -> Option<(usize, usize)> {
        let rows = self.recipients.as_ref()?.included_rows();
        Some((*rows.first()?, *rows.last()?))
    }

    /// The record box's text: the previewed row's number.
    pub fn record_text(&self) -> String {
        match &self.recipients {
            Some(r) if !r.rows.is_empty() => (self.record + 1).to_string(),
            _ => String::new(),
        }
    }
}

/// The tip on a command whose work is a follow-up.
const LATER_PRINT: &str = "Not available yet: printing merged documents is a follow-up.";
const LATER_EMAIL: &str = "Not available yet: sending merged e-mail is a follow-up.";
const LATER_LIST: &str = "Not available yet: typing a list and Outlook contacts are a follow-up.";
const LATER_WIZARD: &str = "Not available yet: the Mail Merge Wizard is a follow-up.";
const LATER_RULE: &str =
    "Not available yet: rules with nested fields (Ask, Fill-in, If, Skip) are a follow-up.";

/// One row of a Mailings menu.
enum Row {
    Item {
        id: &'static str,
        label: &'static str,
        act: MailAct,
        tip: &'static str,
    },
    Separator,
}

fn item(id: &'static str, label: &'static str, act: MailAct) -> Row {
    Row::Item {
        id,
        label,
        act,
        tip: "",
    }
}

fn later(id: &'static str, label: &'static str, tip: &'static str) -> Row {
    Row::Item {
        id,
        label,
        act: MailAct::Unavailable,
        tip,
    }
}

/// A static menu's rows, in Word's order. Insert Merge Field and the record
/// box list the attached columns and rows instead (see [`menu_items`]).
fn menu_rows(menu: MailMenu) -> Vec<Row> {
    use MailAct as M;
    match menu {
        MailMenu::StartMailMerge => vec![
            item("mm-letters", "Letters", M::DocType(MainDocType::Letters)),
            item(
                "mm-email",
                "E-mail Messages",
                M::DocType(MainDocType::Email),
            ),
            item("mm-envelopes", "Envelopes...", M::StartEnvelopes),
            item("mm-labels", "Labels...", M::StartLabels),
            item(
                "mm-directory",
                "Directory",
                M::DocType(MainDocType::Directory),
            ),
            Row::Separator,
            item("mm-normal", "Normal Word Document", M::NormalDocument),
            Row::Separator,
            later(
                "mm-wizard",
                "Step-by-Step Mail Merge Wizard...",
                LATER_WIZARD,
            ),
        ],
        MailMenu::SelectRecipients => vec![
            later("rcpt-new", "Type a New List...", LATER_LIST),
            item(
                "rcpt-existing",
                "Use an Existing List...",
                M::UseExistingList,
            ),
            later(
                "rcpt-outlook",
                "Choose from Outlook Contacts...",
                LATER_LIST,
            ),
        ],
        MailMenu::Rules => vec![
            later("rule-ask", "Ask...", LATER_RULE),
            later("rule-fillin", "Fill-in...", LATER_RULE),
            later("rule-if", "If...Then...Else...", LATER_RULE),
            item(
                "rule-mergerec",
                "Merge Record #",
                M::Rule(Rule::MergeRecord),
            ),
            item(
                "rule-mergeseq",
                "Merge Sequence #",
                M::Rule(Rule::MergeSequence),
            ),
            item("rule-next", "Next Record", M::Rule(Rule::NextRecord)),
            later("rule-nextif", "Next Record If...", LATER_RULE),
            later("rule-bookmark", "Set Bookmark...", LATER_RULE),
            later("rule-skipif", "Skip Record If...", LATER_RULE),
        ],
        MailMenu::Finish => vec![
            item(
                "finish-edit",
                "Edit Individual Documents...",
                M::EditIndividual,
            ),
            later("finish-print", "Print Documents...", LATER_PRINT),
            later("finish-email", "Send Email Messages...", LATER_EMAIL),
        ],
        MailMenu::InsertMergeField | MailMenu::Record => Vec::new(),
    }
}

/// A menu's button id.
fn menu_id(menu: MailMenu) -> &'static str {
    match menu {
        MailMenu::StartMailMerge => "startmailmerge",
        MailMenu::SelectRecipients => "selectrecipients",
        MailMenu::InsertMergeField => "insertmergefield",
        MailMenu::Rules => "mailrules",
        MailMenu::Record => "mailrecord",
        MailMenu::Finish => "finishmerge",
    }
}

/// The menu a Mailings drop-down button (or the record box) opens, by id.
pub(crate) fn menu_of(id: &str) -> Option<MailMenu> {
    [
        MailMenu::StartMailMerge,
        MailMenu::SelectRecipients,
        MailMenu::InsertMergeField,
        MailMenu::Rules,
        MailMenu::Record,
        MailMenu::Finish,
    ]
    .into_iter()
    .find(|&m| menu_id(m) == id)
}

fn cmd(
    id: &'static str,
    icon: &'static str,
    label: &'static str,
    act: MailAct,
    tip: &'static str,
) -> rs::Cmd<Act> {
    rs::cmd(id, icon, label, Act::Mail(act)).tip(label, tip, "")
}

/// A drop-down whose `items` carry its static menu for `ribbon-read`.
fn dropdown(
    menu: MailMenu,
    icon: &'static str,
    label: &'static str,
    key: &'static str,
) -> Control<Act> {
    Control::Dropdown {
        cmd: cmd(menu_id(menu), icon, label, MailAct::Menu(menu), "").key(key),
        items: menu_rows(menu)
            .into_iter()
            .filter_map(|row| match row {
                Row::Item {
                    id,
                    label,
                    act,
                    tip,
                } => Some(cmd(id, icon, label, act, tip)),
                Row::Separator => None,
            })
            .collect(),
    }
}

/// The Mailings ribbon tab.
pub(crate) fn mailings_tab() -> rs::Tab<Act> {
    use MailAct as M;
    rs::tab(
        "Mailings",
        "M",
        vec![
            rs::group(
                "Create",
                20,
                vec![
                    Control::Large(
                        cmd("envelopes", "mail-envelope", "Envelopes", M::Envelopes, "").key("E"),
                    ),
                    Control::Large(cmd("labels", "mail-labels", "Labels", M::Labels, "").key("L")),
                ],
            ),
            rs::group(
                "Start Mail Merge",
                40,
                vec![
                    dropdown(
                        MailMenu::StartMailMerge,
                        "mail-merge",
                        "Start Mail Merge",
                        "S",
                    ),
                    dropdown(
                        MailMenu::SelectRecipients,
                        "mail-recipients",
                        "Select Recipients",
                        "R",
                    ),
                    Control::Large(
                        cmd(
                            "editrecipients",
                            "mail-edit-list",
                            "Edit Recipient List",
                            M::EditRecipients,
                            "",
                        )
                        .key("D"),
                    ),
                ],
            ),
            rs::group(
                "Write & Insert Fields",
                50,
                vec![
                    Control::Large(
                        cmd(
                            "highlightfields",
                            "highlight",
                            "Highlight Merge Fields",
                            M::Highlight,
                            "",
                        )
                        .key("H"),
                    ),
                    Control::Large(
                        cmd(
                            "addressblock",
                            "mail-address-block",
                            "Address Block",
                            M::AddressBlock,
                            "",
                        )
                        .key("A"),
                    ),
                    Control::Large(
                        cmd(
                            "greetingline",
                            "mail-greeting",
                            "Greeting Line",
                            M::GreetingLine,
                            "",
                        )
                        .key("G"),
                    ),
                    dropdown(
                        MailMenu::InsertMergeField,
                        "mail-field",
                        "Insert Merge Field",
                        "I",
                    ),
                    dropdown(MailMenu::Rules, "mail-rules", "Rules", "U"),
                    rs::column(vec![
                        cmd(
                            "matchfields",
                            "mail-match",
                            "Match Fields",
                            M::MatchFields,
                            "",
                        )
                        .key("T"),
                        cmd(
                            "updatelabels",
                            "mail-update",
                            "Update Labels",
                            M::UpdateLabels,
                            "",
                        )
                        .key("B"),
                    ]),
                ],
            ),
            rs::group(
                "Preview Results",
                60,
                vec![
                    Control::Large(
                        cmd(
                            "previewresults",
                            "mail-preview",
                            "Preview Results",
                            M::Preview,
                            "",
                        )
                        .key("P"),
                    ),
                    rs::rows(vec![
                        vec![
                            rs::btn(cmd("mailfirst", "mail-first", "First Record", M::First, "")),
                            rs::btn(cmd(
                                "mailprev",
                                "mail-prev",
                                "Previous Record",
                                M::Previous,
                                "",
                            )),
                            rs::combo(
                                cmd(
                                    "mailrecord",
                                    "list-numbered",
                                    "Go to Record",
                                    M::Menu(MailMenu::Record),
                                    "",
                                ),
                                false,
                            ),
                            rs::btn(cmd("mailnext", "mail-next", "Next Record", M::Next, "")),
                            rs::btn(cmd("maillast", "mail-last", "Last Record", M::Last, "")),
                        ],
                        vec![
                            rs::btn(
                                cmd(
                                    "findrecipient",
                                    "find",
                                    "Find Recipient",
                                    M::FindRecipient,
                                    "",
                                )
                                .key("N"),
                            ),
                            rs::btn(
                                cmd(
                                    "checkerrors",
                                    "mail-check",
                                    "Check for Errors",
                                    M::CheckErrors,
                                    "",
                                )
                                .key("K"),
                            ),
                        ],
                    ]),
                ],
            ),
            rs::group(
                "Finish",
                30,
                vec![dropdown(
                    MailMenu::Finish,
                    "mail-finish",
                    "Finish & Merge",
                    "F",
                )],
            ),
        ],
    )
}

/// Whether a Mailings command can run on a document in `state`. With no list
/// attached (or named), only Create, Start Mail Merge and Select Recipients
/// can; the follow-up placeholders never can.
pub(crate) fn mail_enabled(state: &MailState, act: MailAct) -> bool {
    use MailAct as M;
    let data = state.has_data();
    match act {
        M::Envelopes
        | M::Labels
        | M::DocType(_)
        | M::StartEnvelopes
        | M::StartLabels
        | M::NormalDocument
        | M::UseExistingList
        | M::Menu(MailMenu::StartMailMerge | MailMenu::SelectRecipients) => true,
        M::Unavailable => false,
        M::UpdateLabels => data && state.doc_type == Some(MainDocType::Labels),
        M::First | M::Previous => {
            data && state.bounds().is_none_or(|(first, _)| state.record > first)
        }
        M::Next | M::Last => data && state.bounds().is_none_or(|(_, last)| state.record < last),
        M::EditRecipients
        | M::Highlight
        | M::AddressBlock
        | M::GreetingLine
        | M::Field(_)
        | M::Rule(_)
        | M::MatchFields
        | M::Preview
        | M::Record(_)
        | M::FindRecipient
        | M::CheckErrors
        | M::EditIndividual
        | M::Menu(_) => data,
    }
}

/// Whether a Mailings command shows checked: the two toggles and the main
/// document type.
pub(crate) fn mail_checked(state: &MailState, act: MailAct) -> bool {
    match act {
        MailAct::Highlight => state.highlight,
        MailAct::Preview => state.preview,
        MailAct::DocType(t) => state.doc_type == Some(t),
        MailAct::StartEnvelopes => state.doc_type == Some(MainDocType::Envelopes),
        MailAct::StartLabels => state.doc_type == Some(MainDocType::Labels),
        MailAct::NormalDocument => state.doc_type.is_none(),
        MailAct::Record(r) => state.preview && state.record == r as usize,
        _ => false,
    }
}

/// A menu's items, with live enabled and checked states. Insert Merge Field
/// lists the attached columns, the record box the attached rows.
pub(crate) fn menu_items(state: &MailState, menu: MailMenu) -> Vec<menu::MenuItem> {
    let entry = |id: &str, label: &str, act: MailAct| {
        menu::MenuItem::Item(
            menu::Entry::new(id, label, "", Act::Mail(act), mail_enabled(state, act))
                .checked(mail_checked(state, act)),
        )
    };
    match menu {
        MailMenu::InsertMergeField => match &state.recipients {
            Some(r) => r
                .headers
                .iter()
                .enumerate()
                .take(u16::MAX as usize)
                .map(|(i, h)| entry(&format!("field-{i}"), h, MailAct::Field(i as u16)))
                .collect(),
            None => vec![menu::MenuItem::Item(menu::Entry::unavailable(
                "field-none",
                "(Select recipients first)",
            ))],
        },
        MailMenu::Record => match &state.recipients {
            Some(r) => r
                .rows
                .iter()
                .enumerate()
                .take(u16::MAX as usize)
                .map(|(i, row)| {
                    let first = row.first().map_or("", String::as_str);
                    entry(
                        &format!("record-{i}"),
                        &format!("{}  {first}", i + 1),
                        MailAct::Record(i as u16),
                    )
                })
                .collect(),
            None => Vec::new(),
        },
        _ => menu_rows(menu)
            .into_iter()
            .map(|row| match row {
                Row::Item {
                    id,
                    label,
                    act: MailAct::Unavailable,
                    ..
                } => menu::MenuItem::Item(menu::Entry::unavailable(id, label)),
                Row::Item { id, label, act, .. } => entry(id, label, act),
                Row::Separator => menu::MenuItem::Separator,
            })
            .collect(),
    }
}

/// The body editor of a document tab.
pub(crate) fn body_editor(tab: &mut DocTab) -> Result<&mut Editor, String> {
    match &mut tab.surface {
        Surface::Doc(ed) if !tab.markdown => Ok(ed),
        Surface::Doc(_) => Err("Mail merge needs a .docx (not Markdown)".into()),
        _ => Err("Mail merge needs a document".into()),
    }
}

/// The editor typing goes to: the open header or footer, else the body.
fn typing_editor(tab: &mut DocTab) -> Result<&mut Editor, String> {
    match (&mut tab.hf_edit, &mut tab.surface) {
        (Some(hf), _) => Ok(&mut hf.editor),
        (None, Surface::Doc(ed)) if !tab.markdown => Ok(ed),
        (None, Surface::Doc(_)) => Err("Mail merge needs a .docx (not Markdown)".into()),
        _ => Err("Mail merge needs a document".into()),
    }
}

/// The tab's package, made from the editor for a new document.
fn package(tab: &mut DocTab) -> Result<&mut Package, String> {
    if tab.markdown {
        return Err("Mail merge needs a .docx (not Markdown)".into());
    }
    if tab.pkg.is_none() {
        let Surface::Doc(ed) = &tab.surface else {
            return Err("Mail merge needs a document".into());
        };
        let bytes = doc_to_docx(&ed.doc, &tab.comments, None);
        tab.pkg = Some(docxcore::package::load_package(&bytes).map_err(|e| e.to_string())?);
    }
    Ok(tab.pkg.as_mut().expect("just made"))
}

/// Write the tab's mail merge into its package's settings.
fn write_settings(tab: &mut DocTab) -> Result<(), String> {
    let mm = tab.mail.doc_type.map(|doc_type| MailMerge {
        doc_type,
        source: tab
            .mail
            .recipients
            .as_ref()
            .and_then(|r| r.source.clone())
            .or_else(|| tab.mail.pending_source.clone()),
    });
    package(tab)?.set_mail_merge(mm.as_ref());
    tab.dirty = true;
    Ok(())
}

/// Show (or stop showing) the previewed record in the body's merge fields.
pub(crate) fn sync_preview(tab: &mut DocTab) {
    let preview = match (&tab.mail.recipients, tab.mail.preview) {
        (Some(r), true) => Some(MergePreview {
            recipients: r.clone(),
            map: tab.mail.map.clone(),
            row: tab.mail.record,
        }),
        _ => None,
    };
    if let Surface::Doc(ed) = &mut tab.surface {
        ed.set_merge_preview(preview);
    }
}

/// Attach the recipient list in the file at `path` (Use an Existing List…,
/// the confirm before reading a document's own source, and the harness's
/// `mail-attach`). A letter is the main document type when none is set yet.
pub(crate) fn attach(tab: &mut DocTab, path: &std::path::Path) -> Result<(), String> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if !(lower.ends_with(".csv") || lower.ends_with(".txt")) {
        return Err(format!(
            "{}: recipient lists are .csv or .txt files",
            path.display()
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut recipients =
        Recipients::parse_csv(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    recipients.source = Some(path.display().to_string());
    package(tab)?;
    tab.mail.map = FieldMap::auto(&recipients);
    tab.mail.record = recipients.included_rows().first().copied().unwrap_or(0);
    let n = recipients.rows.len();
    tab.mail.recipients = Some(recipients);
    tab.mail.pending_source = None;
    tab.mail.doc_type.get_or_insert(MainDocType::Letters);
    write_settings(tab)?;
    sync_preview(tab);
    tab.status = format!(
        "Recipients: {n} from {}",
        path.file_name()
            .map_or(lower.clone(), |f| f.to_string_lossy().into_owned())
    )
    .into();
    Ok(())
}

/// Why the document's own data source will not be offered for reading, if
/// it will not: Word reads a local .csv/.txt by its full path.
///
/// Purely lexical, because it runs before the person has said Yes: nothing
/// here touches the file system. A network (UNC), verbatim (`\\?\`) or
/// device (`\\.\`) path is refused outright, since even asking whether it
/// exists would reach out to the server it names (and, for SMB, hand it the
/// person's credentials). A missing file is reported by the read after Yes.
pub(crate) fn pending_problem(source: &str) -> Option<String> {
    use std::path::{Component, Prefix};
    let path = std::path::Path::new(source);
    let lower = source.to_ascii_lowercase();
    if !(lower.ends_with(".csv") || lower.ends_with(".txt")) {
        return Some(format!(
            "The document's data source {source} is not a .csv or .txt list; use Select Recipients"
        ));
    }
    let remote = source.starts_with("\\\\")
        || source.starts_with("//")
        || matches!(
            path.components().next(),
            Some(Component::Prefix(p)) if !matches!(p.kind(), Prefix::Disk(_))
        );
    if remote {
        return Some(format!(
            "The document's data source {source} is not on this computer; use Select Recipients"
        ));
    }
    if !path.is_absolute() {
        return Some(format!(
            "The document's data source {source} is not a full path; use Select Recipients"
        ));
    }
    None
}

/// Insert inline content where the caret types, as one undo step.
pub(crate) fn insert_inline(
    tab: &mut DocTab,
    build: impl FnOnce(&RunProps) -> Inline,
) -> Result<(), String> {
    let ed = typing_editor(tab)?;
    let field = build(&ed.caret_props());
    ed.paste(&docxcore::editor::Clip {
        paras: vec![vec![field]],
    });
    // Only the body previews (a header or footer editor never does).
    if let Surface::Doc(body) = &mut tab.surface {
        body.refresh_merge_preview();
    }
    tab.dirty = true;
    Ok(())
}

/// Move the preview to data-source row `row` and turn Preview Results on, as
/// Word's record buttons do.
fn go_to(tab: &mut DocTab, row: usize) {
    tab.mail.record = row;
    tab.mail.preview = true;
    sync_preview(tab);
    let total = tab.mail.recipients.as_ref().map_or(0, |r| r.rows.len());
    tab.status = format!("Record {} of {total}", row + 1).into();
}

/// Run a Mailings command that changes the document or the merge state on a
/// tab (every one but the menus and dialogs). The status says what happened;
/// an error says why nothing did.
pub(crate) fn mail_apply(tab: &mut DocTab, act: MailAct) -> Result<(), String> {
    use MailAct as M;
    if !mail_enabled(&tab.mail, act) {
        return Err("That command is not available here".into());
    }
    match act {
        M::DocType(t) => {
            tab.mail.doc_type = Some(t);
            write_settings(tab)?;
            tab.status = format!("Main document: {}", doc_type_name(t)).into();
        }
        M::NormalDocument => {
            tab.mail.doc_type = None;
            tab.mail.recipients = None;
            tab.mail.pending_source = None;
            tab.mail.preview = false;
            sync_preview(tab);
            if tab.pkg.is_some() {
                write_settings(tab)?;
            }
            tab.status = "Normal Word Document: no mail merge".into();
        }
        M::Highlight => tab.mail.highlight = !tab.mail.highlight,
        M::Preview => {
            tab.mail.preview = !tab.mail.preview;
            sync_preview(tab);
            tab.status = if tab.mail.preview {
                format!("Previewing record {}", tab.mail.record + 1)
            } else {
                "Preview off".into()
            }
            .into();
        }
        M::First | M::Previous | M::Next | M::Last => {
            let rows = tab
                .mail
                .recipients
                .as_ref()
                .map(Recipients::included_rows)
                .unwrap_or_default();
            let cur = tab.mail.record;
            let row = match act {
                M::First => rows.first().copied(),
                M::Last => rows.last().copied(),
                M::Previous => rows.iter().rev().find(|&&r| r < cur).copied(),
                _ => rows.iter().find(|&&r| r > cur).copied(),
            };
            go_to(tab, row.ok_or("There are no more records")?);
        }
        M::Record(r) => go_to(tab, r as usize),
        M::Field(i) => {
            let name = tab
                .mail
                .recipients
                .as_ref()
                .and_then(|r| r.headers.get(i as usize))
                .cloned()
                .ok_or("The recipient list has no such column")?;
            insert_inline(tab, |p| docxcore::merge::merge_field(&name, p))?;
            tab.status = format!("Inserted merge field {name}").into();
        }
        M::Rule(rule) => {
            insert_inline(tab, |p| {
                docxcore::merge::rule_field(&rule.kind(), p).expect("a rule field")
            })?;
            tab.status = format!("Inserted {}", rule.kind().placeholder()).into();
        }
        M::UpdateLabels => {
            let ed = body_editor(tab)?;
            let changed = docxcore::merge::labels::update_labels_at_caret(ed)?;
            ed.refresh_merge_preview();
            tab.dirty |= changed;
            tab.status = if changed {
                "Labels updated"
            } else {
                "Labels already up to date"
            }
            .into();
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn doc_type_name(t: MainDocType) -> &'static str {
    match t {
        MainDocType::Letters => "Letters",
        MainDocType::Email => "E-mail Messages",
        MainDocType::Envelopes => "Envelopes",
        MainDocType::Labels => "Labels",
        MainDocType::Directory => "Directory",
    }
}

/// Whether the command reads the recipient list, so a document that only
/// names its source has to read it (after asking) first.
fn needs_list(act: MailAct) -> bool {
    use MailAct as M;
    !matches!(
        act,
        M::Envelopes
            | M::Labels
            | M::DocType(_)
            | M::StartEnvelopes
            | M::StartLabels
            | M::NormalDocument
            | M::UseExistingList
            | M::Highlight
            | M::Rule(_)
            | M::Unavailable
            | M::Menu(MailMenu::StartMailMerge | MailMenu::SelectRecipients | MailMenu::Rules)
    )
}

/// Label presets plus Custom, as the Labels dialogs list them.
pub(crate) fn label_choices() -> Vec<String> {
    let mut v: Vec<String> = docxcore::merge::labels::LABEL_PRESETS
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    v.push("Custom".into());
    v
}

/// A label spec from the Labels dialogs' choice and custom sizes (inches).
pub(crate) fn label_spec(
    choice: usize,
    w_in: f32,
    h_in: f32,
    across: usize,
    down: usize,
) -> Result<LabelSpec, String> {
    if let Some(p) = docxcore::merge::labels::LABEL_PRESETS.get(choice) {
        return Ok(*p);
    }
    if !(0.25..=8.5).contains(&w_in) || !(0.25..=11.0).contains(&h_in) {
        return Err("A custom label is 0.25\" to 8.5\" wide and 0.25\" to 11\" high".into());
    }
    if across == 0 || down == 0 || across > 10 || down > 40 {
        return Err("A custom sheet has 1 to 10 labels across and 1 to 40 down".into());
    }
    let spec = LabelSpec::custom(
        (w_in * 1440.0).round() as i32,
        (h_in * 1440.0).round() as i32,
        across,
        down,
    );
    if spec.side + spec.label_w * across as i32 > spec.page_w
        || spec.top + spec.label_h * down as i32 > spec.page_h
    {
        return Err("Those labels do not fit on a Letter sheet".into());
    }
    Ok(spec)
}

impl Docxy {
    /// Dispatch a Mailings command.
    pub(crate) fn mail_act(&mut self, act: MailAct, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.mail_dispatch(act) {
            let id = menu_id(menu);
            let at = split_menu_anchor(&self.probes.borrow(), id)
                .unwrap_or_else(|| point(px(240.), px(140.)));
            if let Err(e) = self.open_split_menu(id, at, cx) {
                self.set_status(e);
            }
            return;
        }
        self.refocus(window, cx);
    }

    /// Run a Mailings command on the active tab, all but opening a menu: the
    /// menu to open, if the command is a drop-down button.
    pub(crate) fn mail_dispatch(&mut self, act: MailAct) -> Option<MailMenu> {
        let tab = self.tabs.get_mut(self.active)?;
        // A document that names its data source reads it only after asking.
        if tab.mail.recipients.is_none() && needs_list(act) {
            if let Some(source) = tab.mail.pending_source.clone() {
                match pending_problem(&source) {
                    Some(why) => tab.status = why.into(),
                    None => tab
                        .dialogs
                        .push(crate::mailings_dialogs::attach_confirm(&source, act)),
                }
                return None;
            }
        }
        match act {
            MailAct::Menu(menu) => return Some(menu),
            MailAct::UseExistingList => self.pick_recipients(),
            MailAct::Unavailable => {}
            MailAct::Envelopes
            | MailAct::Labels
            | MailAct::StartEnvelopes
            | MailAct::StartLabels
            | MailAct::EditRecipients
            | MailAct::AddressBlock
            | MailAct::GreetingLine
            | MailAct::MatchFields
            | MailAct::FindRecipient
            | MailAct::CheckErrors
            | MailAct::EditIndividual => match crate::mailings_dialogs::open(tab, act) {
                Ok(d) => tab.dialogs.push(d),
                Err(e) => tab.status = e.into(),
            },
            _ => {
                if let Err(e) = mail_apply(tab, act) {
                    tab.status = e.into();
                }
            }
        }
        None
    }

    /// Select Recipients ▸ Use an Existing List…: the native file picker,
    /// never in a harness instance (its modal loop stops the control pump):
    /// there the `mail-attach` verb hands the path in instead.
    fn pick_recipients(&mut self) {
        if self.harness.is_some() {
            self.set_status(MAIL_ATTACH_HARNESS);
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Recipient list (*.csv, *.txt)", &["csv", "txt"])
            .pick_file()
        else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Err(e) = attach(tab, &path) {
                tab.status = e.into();
            }
        }
    }

    /// Open what a mail-merge dialog left for the app: a merged or labels
    /// document in a new tab, then the command a confirmed attach was for.
    pub(crate) fn take_mail_outputs(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let after = tab.mail.after_attach.take();
        if let Some((pkg, title)) = tab.mail.new_tab.take() {
            let mut loaded = load_bytes(&docxcore::package::save_package(&pkg));
            loaded.status = format!("{title}: a new document").into();
            self.tabs
                .push(loaded.into_tab(Kind::Docx, title.into(), None, true));
            self.active = self.tabs.len() - 1;
            self.drop_grid_state();
            self.persist();
        }
        // A drop-down's menu needs the pointer's place: the list is attached
        // now, and the button opens it on the next press.
        if let Some(act) = after {
            self.mail_dispatch(act);
        }
    }
}

/// The status a harness instance shows for Use an Existing List….
pub(crate) const MAIL_ATTACH_HARNESS: &str =
    "Use an Existing List opens a native file dialog: use the harness mail-attach verb";

#[cfg(test)]
pub(crate) mod tests;
