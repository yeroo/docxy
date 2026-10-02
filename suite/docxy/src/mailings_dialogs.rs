//! The Mailings tab's dialogs (#628): Envelopes, Labels, Envelope and Label
//! Options, Edit Recipient List, Address Block, Greeting Line, Match Fields,
//! Find Recipient, Check for Errors, Merge to New Document, and the confirm
//! before a document's own data source is read.
//!
//! Each is an ordinary [`Dialog`] on the tab's stack, driven by the pointer,
//! the keyboard and the harness alike. Their accept buttons come here
//! ([`click`]) rather than to `apply_dialog`, because they act on the whole
//! tab: its merge state, its body, and the documents they open.
use super::*;
use crate::dialog::{
    Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Reaction, Value,
};
use crate::mailings_tab::{
    MailAct, attach, body_editor, doc_type_name, insert_inline, label_choices, label_spec,
    sync_preview,
};
use docxcore::merge::envelope::{
    ENVELOPE_SIZES, EnvelopeSpec, envelope_blocks, envelope_sect_pr, insert_envelope,
};
use docxcore::merge::labels::{LabelFill, LabelSpec, insert_labels};
use docxcore::merge::{
    AddressField, GreetingName, MergeContext, MergeFieldKind, MergeOptions, MergeRange, Recipients,
};
use docxcore::package::MainDocType;

/// The names of Match Fields' dropdowns, one per [`AddressField::ALL`].
const MATCH_NAMES: [&str; 14] = [
    "title", "first", "middle", "last", "suffix", "nickname", "company", "address1", "address2",
    "city", "state", "postal", "country", "email",
];

fn ok_cancel(ok: &str) -> Vec<Button> {
    vec![
        Button {
            default: true,
            ..Button::new(ok, ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ]
}

fn form(
    id: &'static str,
    title: &str,
    owner: DialogOwner,
    controls: Vec<Control>,
    buttons: Vec<Button>,
) -> Dialog {
    let mut d = Dialog::message(id, title, String::new(), &[], owner);
    d.text = None;
    d.controls = controls;
    d.buttons = buttons;
    d.mark_opened();
    d
}

fn text(name: &'static str, label: &str, value: &str) -> Control {
    Control::new(name, label, ControlKind::Text, Value::Text(value.into()))
}

fn number(name: &'static str, label: &str, value: &str) -> Control {
    Control::new(name, label, ControlKind::Number, Value::Text(value.into()))
}

fn check(name: &'static str, label: &str, on: bool) -> Control {
    Control::new(name, label, ControlKind::Checkbox, Value::Bool(on))
}

fn label(name: &'static str, label: &str, value: &str) -> Control {
    Control::new(name, label, ControlKind::Label, Value::Text(value.into()))
}

fn choice(
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: Vec<String>,
    at: usize,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(Some(at)));
    c.items = items;
    c
}

fn get_text(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn get_bool(d: &Dialog, name: &str) -> bool {
    matches!(d.value(name), Some(Value::Bool(true)))
}

fn get_choice(d: &Dialog, name: &str) -> usize {
    match d.value(name) {
        Some(Value::Choice(Some(i))) => *i,
        _ => 0,
    }
}

fn get_num(d: &Dialog, name: &str) -> Result<f32, String> {
    let t = get_text(d, name);
    t.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("'{t}' is not a number"))
}

/// Address lines typed into a one-line field: a line break or `;` ends one.
fn lines(s: &str) -> String {
    s.split(['\n', ';'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn index_of(d: &Dialog, name: &str) -> Option<usize> {
    d.controls.iter().position(|c| c.name == name)
}

/// The Labels dialogs' custom-size fields follow the product: enabled only
/// for Custom.
fn labels_react(d: &mut Dialog, _i: usize, _before: &Value) {
    let custom = get_choice(d, "product") == docxcore::merge::labels::LABEL_PRESETS.len();
    for name in ["width", "height", "across", "down"] {
        if let Some(i) = index_of(d, name) {
            d.controls[i].enabled = custom;
        }
    }
}

fn label_controls() -> Vec<Control> {
    let mut v = vec![choice(
        "product",
        "Product &number:",
        ControlKind::Dropdown,
        label_choices(),
        0,
    )];
    for (name, lab, value) in [
        ("width", "Label &width (in):", "2.625"),
        ("height", "Label &height (in):", "1"),
        ("across", "Labels &across:", "3"),
        ("down", "Labels &down:", "10"),
    ] {
        let mut c = number(name, lab, value);
        c.enabled = false;
        v.push(c);
    }
    v
}

fn spec_of(d: &Dialog) -> Result<LabelSpec, String> {
    let choice = get_choice(d, "product");
    if choice < docxcore::merge::labels::LABEL_PRESETS.len() {
        return label_spec(choice, 0.0, 0.0, 0, 0);
    }
    label_spec(
        choice,
        get_num(d, "width")?,
        get_num(d, "height")?,
        get_num(d, "across")? as usize,
        get_num(d, "down")? as usize,
    )
}

fn sizes() -> Vec<String> {
    ENVELOPE_SIZES.iter().map(|s| s.name.to_string()).collect()
}

fn recipients(tab: &DocTab) -> Result<&Recipients, String> {
    tab.mail
        .recipients
        .as_ref()
        .ok_or_else(|| "Select recipients first".to_string())
}

/// One recipient's values, for a dialog's preview line.
fn row_text(r: &Recipients, row: usize) -> String {
    r.rows
        .get(row)
        .map(|v| {
            v.iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" \u{00b7} ")
        })
        .unwrap_or_default()
}

/// Edit Recipient List keeps the Recipient number, the Include box, the
/// grid's Include column and the values line in step.
fn recipients_react(d: &mut Dialog, i: usize, _before: &Value) {
    let (Some(grid), Some(rec), Some(inc), Some(vals)) = (
        index_of(d, "recipients"),
        index_of(d, "record"),
        index_of(d, "include"),
        index_of(d, "values"),
    ) else {
        return;
    };
    let Value::Rows(rows) = d.controls[grid].value.clone() else {
        return;
    };
    let n = get_text(d, "record").trim().parse::<usize>().unwrap_or(1);
    let row = n.clamp(1, rows.len().max(1)) - 1;
    if i == inc {
        let on = get_bool(d, "include");
        if let Value::Rows(rows) = &mut d.controls[grid].value {
            if let Some(r) = rows.get_mut(row) {
                r[0] = if on { "Yes" } else { "No" }.into();
            }
        }
    } else if i == rec || i == grid {
        let on = rows.get(row).is_some_and(|r| r[0] != "No");
        d.controls[inc].value = Value::Bool(on);
    }
    let shown = rows
        .get(row)
        .map(|r| {
            r[1..]
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" \u{00b7} ")
        })
        .unwrap_or_default();
    d.controls[vals].value = Value::Text(shown);
}

/// Merge to New Document's From and To follow the From choice.
fn merge_react(d: &mut Dialog, _i: usize, _before: &Value) {
    let from = get_choice(d, "records") == 2;
    for name in ["from", "to"] {
        if let Some(i) = index_of(d, name) {
            d.controls[i].enabled = from;
        }
    }
}

/// Address Block's preview follows Insert company name.
fn address_react(d: &mut Dialog, _i: usize, _before: &Value) {
    let src = if get_bool(d, "company") {
        "with"
    } else {
        "without"
    };
    let shown = get_text(d, src);
    if let Some(i) = index_of(d, "preview") {
        d.controls[i].value = Value::Text(shown);
    }
}

/// The dialog a Mailings command opens.
pub(crate) fn open(tab: &mut DocTab, act: MailAct) -> Result<Dialog, String> {
    if tab.markdown {
        return Err("Mail merge needs a .docx (not Markdown)".into());
    }
    Ok(match act {
        MailAct::Envelopes => {
            // The selection is the delivery address, as Word fills it in.
            let selected = body_editor(tab)?
                .copy()
                .map(|c| c.to_text())
                .unwrap_or_default();
            form(
                "mail-envelopes",
                "Envelopes and Labels",
                DialogOwner::MailEnvelopes,
                vec![
                    text("delivery", "&Delivery address:", &lines(&selected)),
                    text("return", "&Return address:", ""),
                    check("omit", "&Omit return address", false),
                    choice("size", "Envelope &size:", ControlKind::Dropdown, sizes(), 0),
                ],
                ok_cancel("Add to Document"),
            )
        }
        MailAct::StartEnvelopes => form(
            "mail-envelope-options",
            "Envelope Options",
            DialogOwner::MailEnvelopeOptions,
            vec![choice(
                "size",
                "Envelope &size:",
                ControlKind::Dropdown,
                sizes(),
                0,
            )],
            ok_cancel("OK"),
        ),
        MailAct::Labels => {
            let mut controls = vec![text("address", "&Address:", "")];
            controls.extend(label_controls());
            let mut d = form(
                "mail-labels",
                "Envelopes and Labels",
                DialogOwner::MailLabels,
                controls,
                ok_cancel("New Document"),
            );
            d.react = Some(Reaction(labels_react));
            d
        }
        MailAct::StartLabels => {
            let mut d = form(
                "mail-label-options",
                "Label Options",
                DialogOwner::MailLabelOptions,
                label_controls(),
                ok_cancel("OK"),
            );
            d.react = Some(Reaction(labels_react));
            d
        }
        MailAct::EditRecipients => {
            let r = recipients(tab)?;
            let mut grid = Control::new(
                "recipients",
                "Recipients:",
                ControlKind::Grid,
                Value::Rows(
                    r.rows
                        .iter()
                        .enumerate()
                        .map(|(i, row)| {
                            let inc = r.included.get(i).copied().unwrap_or(true);
                            let mut v = vec![if inc { "Yes" } else { "No" }.to_string()];
                            v.extend(row.iter().cloned());
                            v
                        })
                        .collect(),
                ),
            );
            grid.columns = std::iter::once("Include".to_string())
                .chain(r.headers.iter().cloned())
                .collect();
            let row = tab.mail.record.min(r.rows.len().saturating_sub(1));
            let mut d = form(
                "mail-recipients",
                "Mail Merge Recipients",
                DialogOwner::MailRecipients,
                vec![
                    grid,
                    number("record", "&Recipient:", &(row + 1).to_string()),
                    check(
                        "include",
                        "&Include this recipient",
                        r.included.get(row).copied().unwrap_or(true),
                    ),
                    label("values", "", &row_text(r, row)),
                ],
                ok_cancel("OK"),
            );
            d.react = Some(Reaction(recipients_react));
            d
        }
        MailAct::AddressBlock => {
            let r = recipients(tab)?;
            let ctx = MergeContext {
                recipients: r,
                map: &tab.mail.map,
                row: tab.mail.record,
                seq: 1,
            };
            let show = |company: bool| {
                docxcore::merge::eval(
                    &MergeFieldKind::AddressBlock {
                        format: None,
                        exclude_country: None,
                        company,
                    },
                    &ctx,
                )
                .unwrap_or_default()
                .replace('\n', " / ")
            };
            let (with, without) = (show(true), show(false));
            let mut hidden_with = label("with", "", &with);
            hidden_with.visible = false;
            let mut hidden_without = label("without", "", &without);
            hidden_without.visible = false;
            let mut d = form(
                "mail-address-block",
                "Insert Address Block",
                DialogOwner::MailAddressBlock,
                vec![
                    check("company", "Insert &company name", true),
                    label("preview", "Preview:", &with),
                    hidden_with,
                    hidden_without,
                ],
                ok_cancel("OK"),
            );
            d.react = Some(Reaction(address_react));
            d
        }
        MailAct::GreetingLine => form(
            "mail-greeting-line",
            "Insert Greeting Line",
            DialogOwner::MailGreetingLine,
            vec![
                choice(
                    "salutation",
                    "&Greeting:",
                    ControlKind::Dropdown,
                    vec!["Dear".into(), "To".into(), "(none)".into()],
                    0,
                ),
                choice(
                    "name",
                    "&Name format:",
                    ControlKind::Dropdown,
                    GreetingName::ALL
                        .iter()
                        .map(|g| g.label().to_string())
                        .collect(),
                    0,
                ),
                choice(
                    "punctuation",
                    "&Punctuation:",
                    ControlKind::Dropdown,
                    vec![",".into(), ":".into(), "(none)".into()],
                    0,
                ),
                text(
                    "fallback",
                    "Greeting line for &invalid recipient names:",
                    docxcore::merge::fields::DEFAULT_GREETING_FALLBACK,
                ),
            ],
            ok_cancel("OK"),
        ),
        MailAct::MatchFields => {
            let r = recipients(tab)?;
            let items: Vec<String> = std::iter::once("(not matched)".to_string())
                .chain(r.headers.iter().cloned())
                .collect();
            let controls = AddressField::ALL
                .iter()
                .zip(MATCH_NAMES)
                .map(|(f, name)| {
                    let at = tab.mail.map.get(*f).map_or(0, |c| c + 1);
                    choice(
                        name,
                        &format!("{}:", f.label()),
                        ControlKind::Dropdown,
                        items.clone(),
                        at,
                    )
                })
                .collect();
            form(
                "mail-match-fields",
                "Match Fields",
                DialogOwner::MailMatchFields,
                controls,
                ok_cancel("OK"),
            )
        }
        MailAct::FindRecipient => {
            let r = recipients(tab)?;
            let items = std::iter::once("All fields".to_string())
                .chain(r.headers.iter().cloned())
                .collect();
            form(
                "mail-find",
                "Find Entry",
                DialogOwner::MailFind,
                vec![
                    text("find", "F&ind:", ""),
                    choice("field", "&Look in:", ControlKind::Dropdown, items, 0),
                ],
                vec![
                    Button {
                        default: true,
                        ..Button::new("Find Next", ButtonRole::Apply)
                    },
                    Button::new("Cancel", ButtonRole::Cancel),
                ],
            )
        }
        MailAct::CheckErrors => form(
            "mail-check-errors",
            "Checking and Reporting Errors",
            DialogOwner::MailCheckErrors,
            vec![choice(
                "mode",
                "",
                ControlKind::Radio,
                vec![
                    "Simulate the merge and report errors".into(),
                    "Complete the merge, pausing to report each error".into(),
                    "Complete the merge without pausing; report errors".into(),
                ],
                0,
            )],
            ok_cancel("OK"),
        ),
        MailAct::EditIndividual => {
            let r = recipients(tab)?;
            let n = r.rows.len().max(1).to_string();
            let mut from = number("from", "&From:", "1");
            from.enabled = false;
            let mut to = number("to", "&To:", &n);
            to.enabled = false;
            let mut d = form(
                "mail-merge-new",
                "Merge to New Document",
                DialogOwner::MailMergeToNew,
                vec![
                    choice(
                        "records",
                        "Merge records",
                        ControlKind::Radio,
                        vec!["All".into(), "Current record".into(), "From".into()],
                        0,
                    ),
                    from,
                    to,
                ],
                ok_cancel("OK"),
            );
            d.react = Some(Reaction(merge_react));
            d
        }
        _ => return Err("That command has no dialog".into()),
    })
}

/// Word's question before a document's own data source is read.
pub(crate) fn attach_confirm(source: &str, then: MailAct) -> Dialog {
    Dialog::message(
        "mail-attach",
        "Microsoft Word",
        format!(
            "Opening this document will run the following SQL command:\n\n\
             SELECT * FROM {source}\n\n\
             Data from your database will be placed in the document. Do you want to continue?"
        ),
        &[("Yes", ButtonRole::Accept), ("No", ButtonRole::Cancel)],
        DialogOwner::MailAttach { then },
    )
}

fn report(title: &str, text: String) -> Dialog {
    Dialog::message(
        "mail-report",
        title,
        text,
        &[("OK", ButtonRole::Cancel)],
        DialogOwner::MailReport,
    )
}

/// "Mail merge will delete the contents of this document" before Start Mail
/// Merge ▸ Envelopes or Labels replaces a document that has text.
fn replace_confirm(owner: DialogOwner) -> Dialog {
    Dialog::message(
        "mail-replace",
        "Microsoft Word",
        "Mail merge will replace the contents of this document. Do you want to continue?".into(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        owner,
    )
}

/// Whether the body has any text or table: replacing it needs a confirm.
fn has_content(tab: &mut DocTab) -> Result<bool, String> {
    let ed = body_editor(tab)?;
    Ok(ed.doc.body.iter().any(|b| match b {
        Block::Paragraph(p) => !p.plain_text().trim().is_empty(),
        Block::Table(_) => true,
        _ => false,
    }))
}

fn is_mail(owner: DialogOwner) -> bool {
    matches!(
        owner,
        DialogOwner::MailEnvelopes
            | DialogOwner::MailEnvelopeOptions
            | DialogOwner::MailEnvelopesReplace(_)
            | DialogOwner::MailLabels
            | DialogOwner::MailLabelOptions
            | DialogOwner::MailLabelsReplace(_)
            | DialogOwner::MailRecipients
            | DialogOwner::MailAddressBlock
            | DialogOwner::MailGreetingLine
            | DialogOwner::MailMatchFields
            | DialogOwner::MailFind
            | DialogOwner::MailCheckErrors
            | DialogOwner::MailMergeToNew
            | DialogOwner::MailAttach { .. }
            | DialogOwner::MailReport
    )
}

/// Press a button on a Mailings dialog: `None` for any other dialog, and for
/// a cancel button, which the generic stack handles. An accept button closes
/// its dialog before it applies (it may open another: a confirm, a report),
/// and reopens it with its staged values when the apply refuses (not the
/// confirm before reading the document's own list: it has nothing to retry).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    if !is_mail(top.owner) {
        return None;
    }
    let want = button.replace('&', "");
    let b = top
        .buttons
        .iter()
        .find(|b| b.label.replace('&', "").eq_ignore_ascii_case(want.trim()))?;
    if !b.enabled {
        return None;
    }
    let role = b.role;
    let dialog = top.clone();
    match role {
        ButtonRole::Accept => {
            tab.dialogs.pop();
            let done = apply(tab, &dialog);
            // Yes on reading the document's own list has nothing to retry:
            // the error says why the read failed.
            if done.is_err() && !matches!(dialog.owner, DialogOwner::MailAttach { .. }) {
                tab.dialogs.push(dialog);
            }
            Some(done)
        }
        ButtonRole::Apply => Some(apply(tab, &dialog)),
        _ => None,
    }
}

fn apply(tab: &mut DocTab, d: &Dialog) -> Result<(), String> {
    match d.owner {
        DialogOwner::MailEnvelopes => {
            let size = ENVELOPE_SIZES[get_choice(d, "size").min(ENVELOPE_SIZES.len() - 1)];
            let ret = if get_bool(d, "omit") {
                String::new()
            } else {
                lines(&get_text(d, "return"))
            };
            let spec = EnvelopeSpec::from_text(size, &lines(&get_text(d, "delivery")), &ret);
            insert_envelope(body_editor(tab)?, &spec);
            tab.dirty = true;
            tab.status = format!("Envelope added: {}", size.name).into();
        }
        DialogOwner::MailEnvelopeOptions => {
            let size = ENVELOPE_SIZES[get_choice(d, "size").min(ENVELOPE_SIZES.len() - 1)];
            if has_content(tab)? {
                tab.dialogs
                    .push(replace_confirm(DialogOwner::MailEnvelopesReplace(size)));
            } else {
                envelope_main(tab, size)?;
            }
        }
        DialogOwner::MailEnvelopesReplace(size) => envelope_main(tab, size)?,
        DialogOwner::MailLabels => {
            let spec = spec_of(d)?;
            let address = lines(&get_text(d, "address"));
            // The new document is this one's package (styles, theme,
            // numbering) with the sheet as its body, in a section of its own
            // (no header or footer), and none of its comments or merge.
            let Surface::Doc(ed) = &tab.surface else {
                return Err("Labels need a document".into());
            };
            let bytes = doc_to_docx(
                &ed.export_doc(),
                &live_comments(tab, &ed.doc),
                tab.pkg.as_ref(),
            );
            let mut pkg = docxcore::package::load_package(&bytes).map_err(|e| e.to_string())?;
            let mut sheet = Editor::new(docxcore::model::Document {
                body: vec![Block::SectionProperties(
                    docxcore::model::SectionProperties {
                        raw: "<w:sectPr></w:sectPr>".into(),
                        property_change: None,
                    },
                )],
            });
            insert_labels(&mut sheet, &spec, &LabelFill::Same(address));
            pkg.document = sheet.doc;
            for c in docxcore::comments::parse_comments(&pkg) {
                if let Ok(id) = c.id.parse() {
                    pkg.remove_comment(id);
                }
            }
            pkg.set_mail_merge(None);
            tab.mail.new_tab = Some((pkg, "Labels1.docx".into()));
            tab.status = format!("Labels: {}", spec.name).into();
        }
        DialogOwner::MailLabelOptions => {
            let spec = spec_of(d)?;
            if has_content(tab)? {
                tab.dialogs
                    .push(replace_confirm(DialogOwner::MailLabelsReplace(spec)));
            } else {
                labels_main(tab, spec)?;
            }
        }
        DialogOwner::MailLabelsReplace(spec) => labels_main(tab, spec)?,
        DialogOwner::MailRecipients => {
            let Some(Value::Rows(rows)) = d.value("recipients") else {
                return Err("the recipient grid is missing".into());
            };
            let r = tab
                .mail
                .recipients
                .as_mut()
                .ok_or("Select recipients first")?;
            r.included = rows.iter().map(|row| row[0] != "No").collect();
            r.included.resize(r.rows.len(), true);
            let included = r.included_rows();
            let n = included.len();
            if !included.contains(&tab.mail.record) {
                tab.mail.record = included.first().copied().unwrap_or(0);
            }
            sync_preview(tab);
            tab.status = format!("{n} recipients included").into();
        }
        DialogOwner::MailAddressBlock => {
            let company = get_bool(d, "company");
            insert_inline(tab, |p| docxcore::merge::address_block_field(company, p))?;
            tab.status = "Inserted Address Block".into();
        }
        DialogOwner::MailGreetingLine => {
            let salutation = match get_choice(d, "salutation") {
                0 => "Dear",
                1 => "To",
                _ => "",
            };
            let name = GreetingName::ALL[get_choice(d, "name").min(GreetingName::ALL.len() - 1)];
            let punctuation = match get_choice(d, "punctuation") {
                0 => ",",
                1 => ":",
                _ => "",
            };
            let fallback = get_text(d, "fallback");
            insert_inline(tab, |p| {
                docxcore::merge::greeting_line_field(salutation, name, punctuation, &fallback, p)
            })?;
            tab.status = "Inserted Greeting Line".into();
        }
        DialogOwner::MailMatchFields => {
            for (f, name) in AddressField::ALL.iter().zip(MATCH_NAMES) {
                let c = get_choice(d, name);
                tab.mail.map.set(*f, c.checked_sub(1));
            }
            sync_preview(tab);
            tab.status = "Match Fields updated".into();
        }
        DialogOwner::MailFind => {
            let want = get_text(d, "find").trim().to_lowercase();
            if want.is_empty() {
                return Err("Type what to find".into());
            }
            let r = recipients(tab)?;
            let col = get_choice(d, "field").checked_sub(1);
            let rows = r.included_rows();
            let start = rows.iter().position(|&x| x > tab.mail.record).unwrap_or(0);
            let hit = rows[start..]
                .iter()
                .chain(&rows[..start])
                .copied()
                .find(|&row| {
                    let vals = &r.rows[row];
                    match col {
                        Some(c) => vals
                            .get(c)
                            .is_some_and(|v| v.to_lowercase().contains(&want)),
                        None => vals.iter().any(|v| v.to_lowercase().contains(&want)),
                    }
                })
                .ok_or_else(|| format!("No recipient matches '{}'", get_text(d, "find")))?;
            tab.mail.record = hit;
            tab.mail.preview = true;
            sync_preview(tab);
            tab.status = format!("Found in record {}", hit + 1).into();
        }
        DialogOwner::MailCheckErrors => {
            let r = recipients(tab)?;
            // Only the fields' instructions count, so a preview does not matter.
            let Surface::Doc(ed) = &tab.surface else {
                return Err("Mail merge needs a document".into());
            };
            let missing = docxcore::merge::check_errors(&ed.doc, r, &tab.mail.map);
            let unmerged = docxcore::merge::unmerged_fields(&ed.doc);
            let mut parts = Vec::new();
            if !missing.is_empty() {
                parts.push(format!(
                    "These merge fields name no column of the recipient list:\n\n{}",
                    missing.join("\n")
                ));
            }
            if !unmerged.is_empty() {
                parts.push(format!(
                    "These merge fields are inside a tracked move or an unsupported \
                     wrapper and are not merged; accept the change first:\n\n{}",
                    unmerged.join("\n")
                ));
            }
            let n = missing.len() + unmerged.len();
            let text = if parts.is_empty() {
                "No mail merge errors have been found.".to_string()
            } else {
                parts.join("\n\n")
            };
            tab.status = if n == 0 {
                "No mail merge errors".to_string()
            } else {
                format!("{n} merge field errors")
            }
            .into();
            tab.dialogs
                .push(report("Checking and Reporting Errors", text));
        }
        DialogOwner::MailMergeToNew => {
            let range = match get_choice(d, "records") {
                0 => MergeRange::All,
                1 => MergeRange::Current(tab.mail.record),
                _ => {
                    let from = get_num(d, "from")?.max(1.0) as usize;
                    let to = get_num(d, "to")?.max(1.0) as usize;
                    if to < from {
                        return Err("'To' comes before 'From'".into());
                    }
                    MergeRange::FromTo(from - 1, to - 1)
                }
            };
            merge_to_new(tab, range)?;
        }
        DialogOwner::MailAttach { then } => {
            let source = tab
                .mail
                .pending_source
                .clone()
                .ok_or("The document names no data source")?;
            attach(tab, std::path::Path::new(&source))?;
            tab.mail.after_attach = Some(then);
        }
        _ => return Err("not a mail merge dialog".into()),
    }
    Ok(())
}

/// The document becomes an envelope (Start Mail Merge ▸ Envelopes…).
fn envelope_main(
    tab: &mut DocTab,
    size: docxcore::merge::envelope::EnvelopeSize,
) -> Result<(), String> {
    let blocks = envelope_blocks(&EnvelopeSpec {
        size,
        delivery: Vec::new(),
        return_address: Vec::new(),
    });
    body_editor(tab)?.replace_body(blocks, |_| envelope_sect_pr(size));
    tab.mail.doc_type = Some(MainDocType::Envelopes);
    set_type(tab)?;
    tab.status = format!("Main document: Envelopes, {}", size.name).into();
    Ok(())
}

/// The document becomes a sheet of labels (Start Mail Merge ▸ Labels…).
fn labels_main(tab: &mut DocTab, spec: LabelSpec) -> Result<(), String> {
    insert_labels(body_editor(tab)?, &spec, &LabelFill::Merge);
    tab.mail.doc_type = Some(MainDocType::Labels);
    set_type(tab)?;
    tab.status = format!("Main document: Labels, {}", spec.name).into();
    Ok(())
}

/// Record the new main document type in the settings.
fn set_type(tab: &mut DocTab) -> Result<(), String> {
    let t = tab.mail.doc_type.unwrap_or_default();
    crate::mailings_tab::mail_apply(tab, MailAct::DocType(t))?;
    sync_preview(tab);
    Ok(())
}

/// Finish & Merge ▸ Edit Individual Documents: merge the document as it is
/// now (unsaved edits too, without the preview) into a new document.
pub(crate) fn merge_to_new(tab: &mut DocTab, range: MergeRange) -> Result<(), String> {
    let r = recipients(tab)?.clone();
    let Surface::Doc(ed) = &tab.surface else {
        return Err("Mail merge needs a document".into());
    };
    let bytes = doc_to_docx(
        &ed.export_doc(),
        &live_comments(tab, &ed.doc),
        tab.pkg.as_ref(),
    );
    let main = docxcore::package::load_package(&bytes).map_err(|e| e.to_string())?;
    let doc_type = tab.mail.doc_type.unwrap_or_default();
    let merged = docxcore::merge::merge_package(
        &main,
        &r,
        &MergeOptions {
            range,
            doc_type,
            map: tab.mail.map.clone(),
        },
    )?;
    let n = docxcore::merge::merge_rows(&r, range).len();
    let stem = match doc_type {
        MainDocType::Letters => "Letters",
        MainDocType::Email => "Messages",
        _ => doc_type_name(doc_type),
    };
    tab.mail.new_tab = Some((merged, format!("{stem}1.docx")));
    tab.status = format!("Merged {n} records").into();
    Ok(())
}
