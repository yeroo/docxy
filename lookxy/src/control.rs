//! The lookxy control surface: maps [`ctlcore`] verbs onto the **live**
//! mailbox, so an external agent (e.g. Claude Code in a sibling agwinterm
//! pane) can read and triage the open mailbox without a separate Graph
//! session of its own.
//!
//! Reads serialize `App::store` directly, so they always reflect the local,
//! offline-first source of truth (including optimistically-applied
//! changes). Triage verbs go through the **same** two-step every UI key
//! uses — an optimistic `Store` write, then a `SyncCommand` down
//! `App::sync::cmd_tx` — so an agent's action lands in the outbox and
//! repaints the panes live, exactly as a keypress would. Unlike the UI keys
//! (which act on whatever's highlighted), every triage verb here addresses
//! its target by the **id in its own arguments**, not the current
//! selection.
//!
//! ## Verbs
//!
//! | Verb | Args | Result |
//! |---|---|---|
//! | `mail.status` | — | `{account, sync_state, folders, unread_total, pending_ops, selected_folder?, selected_message?}` |
//! | `mail.folders` | — | `{folders:[{id, name, unread, total, well_known?}]}` |
//! | `mail.list` | `{folder?, limit?, offset?}` | `{folder, total, messages:[…]}` |
//! | `mail.read` | `{id}` | `{id, folder, subject, from, to, cc, received, is_read, is_flagged, has_attachments, body_text, body_pending?}` |
//! | `mail.search` | `{query, limit?}` | `{query, count, messages:[…]}` |
//! | `mail.mark` | `{id, read}` | `{id, is_read}` |
//! | `mail.flag` | `{id, flagged}` | `{id, is_flagged}` |
//! | `mail.move` | `{id, dest}` | `{id, folder}` |
//! | `mail.delete` | `{id}` | `{id, deleted:true}` |
//! | `mail.attachments` | `{id}` | `{id, attachments:[{id, name, content_type, size}]}` |
//! | `mail.save-attachment` | `{id, attachment, dest?}` | `{queued:true, dest}` |
//! | `mail.select` | `{folder?, id?}` | `{selected_folder?, selected_message?}` |
//! | `mail.refresh` | — | `{refreshing:true}` |

use std::path::{Path, PathBuf};

use crate::app::{App, downloads_dir, sanitize_filename};

use ctlcore::json::Json;
use mailcore::graph::model::Body;
use mailcore::htmlrender;
use mailcore::store::{MessageRow, Store};
use mailcore::sync::engine::{SyncCommand, SyncState};

/// The directory where lookxy publishes its control discovery files:
/// `<config>/lookxy/ctl` (see [`ctlcore::config_ctl_dir`]).
pub fn control_dir() -> Option<std::path::PathBuf> {
    ctlcore::config_ctl_dir("lookxy")
}

/// This instance's control name: `lookxy-<AGWINTERM_SESSION_ID|pid>` (see
/// [`ctlcore::instance_name`]).
pub fn instance_name() -> String {
    ctlcore::instance_name("lookxy")
}

/// Route one control verb against the live mailbox, returning the JSON
/// result or an error message. A triage verb (`mark`/`flag`/`move`/
/// `delete`/`save-attachment`) additionally flashes this pane's agwinterm
/// status dot via [`ctlcore::signal_activity`], so a watcher sees the
/// mailbox being worked on.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "mail.status" => Ok(status(app)),
        "mail.folders" => Ok(folders(app)),
        "mail.list" => list(app, args),
        "mail.read" => read(app, args),
        "mail.search" => search(app, args),
        "mail.mark" => mark(app, args),
        "mail.flag" => flag(app, args),
        "mail.move" => move_msg(app, args),
        "mail.delete" => delete(app, args),
        "mail.attachments" => attachments(app, args),
        "mail.save-attachment" => save_attachment(app, args),
        "mail.select" => select(app, args),
        "mail.refresh" => Ok(refresh(app)),
        other => Err(format!("unknown verb '{other}'")),
    };
    if out.is_ok()
        && matches!(
            verb,
            "mail.mark" | "mail.flag" | "mail.move" | "mail.delete" | "mail.save-attachment"
        )
    {
        ctlcore::signal_activity();
    }
    out
}

// ---------------------------------------------------------------------------
// Read-only verbs
// ---------------------------------------------------------------------------

fn sync_state_str(s: &SyncState) -> &'static str {
    match s {
        SyncState::Idle => "idle",
        SyncState::Syncing => "syncing",
        SyncState::Offline => "offline",
        SyncState::PendingOps(_) => "pending_ops",
        SyncState::SignInRequired => "signin_required",
    }
}

fn status(app: &App) -> Json {
    let folders = app.store.folders().unwrap_or_default();
    let unread_total: i64 = folders.iter().map(|f| f.unread_count).sum();
    let pending_ops = app.store.pending_ops().map(|v| v.len()).unwrap_or(0);
    let mut fields = vec![
        (
            "account",
            app.account.clone().map(Json::Str).unwrap_or(Json::Null),
        ),
        (
            "sync_state",
            Json::Str(sync_state_str(&app.status).to_string()),
        ),
        ("folders", Json::Num(folders.len() as f64)),
        ("unread_total", Json::Num(unread_total as f64)),
        ("pending_ops", Json::Num(pending_ops as f64)),
    ];
    if let Some(f) = &app.selected_folder {
        fields.push(("selected_folder", Json::Str(f.clone())));
    }
    if let Some(m) = &app.selected_msg {
        fields.push(("selected_message", Json::Str(m.clone())));
    }
    Json::obj(fields)
}

fn folders(app: &App) -> Json {
    let rows = app.store.folders().unwrap_or_default();
    let arr = rows
        .into_iter()
        .map(|f| {
            let mut fields = vec![
                ("id", Json::Str(f.id)),
                ("name", Json::Str(f.display_name)),
                ("unread", Json::Num(f.unread_count as f64)),
                ("total", Json::Num(f.total_count as f64)),
            ];
            if let Some(wk) = f.well_known_name {
                fields.push(("well_known", Json::Str(wk)));
            }
            Json::obj(fields)
        })
        .collect();
    Json::obj(vec![("folders", Json::Arr(arr))])
}

/// Serializes one `MessageRow` into the summary shape both `mail.list` and
/// `mail.search` report.
fn message_summary(m: &MessageRow) -> Json {
    Json::obj(vec![
        ("id", Json::Str(m.id.clone())),
        ("from_name", Json::Str(m.from_name.clone())),
        ("from_addr", Json::Str(m.from_addr.clone())),
        ("subject", Json::Str(m.subject.clone())),
        ("received", Json::Str(m.received_at.clone())),
        ("is_read", Json::Bool(m.is_read)),
        ("is_flagged", Json::Bool(m.is_flagged)),
        ("has_attachments", Json::Bool(m.has_attachments)),
        ("preview", Json::Str(m.preview.clone())),
    ])
}

fn list(app: &App, args: &Json) -> Result<Json, String> {
    let folder = args
        .get_str("folder")
        .map(str::to_string)
        .or_else(|| app.selected_folder.clone())
        .ok_or("mail.list has no 'folder' and no folder is currently selected")?;
    let limit = args.get_usize("limit").unwrap_or(50) as i64;
    let offset = args.get_usize("offset").unwrap_or(0) as i64;
    let rows = app
        .store
        .messages_in_folder(&folder, limit, offset)
        .map_err(|e| e.to_string())?;
    // `total` is the folder's Graph-reported item count (independent of
    // `limit`/`offset`), not just how many rows this page returned — so a
    // client can tell there's more beyond the page it asked for.
    let total = app
        .store
        .folders()
        .unwrap_or_default()
        .into_iter()
        .find(|f| f.id == folder)
        .map(|f| f.total_count)
        .unwrap_or(rows.len() as i64);
    Ok(Json::obj(vec![
        ("folder", Json::Str(folder)),
        ("total", Json::Num(total as f64)),
        (
            "messages",
            Json::Arr(rows.iter().map(message_summary).collect()),
        ),
    ]))
}

/// Locates a message by id across every folder in the store. The store has
/// no direct "message by id" lookup (messages are only queried per-folder),
/// so this is the one place `control` needs to scan every folder's list —
/// acceptable for a mailbox-sized folder count, and adding a new `Store`
/// method is out of scope here (no mailcore changes, per the design spec).
fn find_message(store: &Store, id: &str) -> Option<MessageRow> {
    for f in store.folders().unwrap_or_default() {
        if let Ok(rows) = store.messages_in_folder(&f.id, i64::MAX, 0) {
            if let Some(row) = rows.into_iter().find(|m| m.id == id) {
                return Some(row);
            }
        }
    }
    None
}

/// The wrap width `mail.read` renders the body to. A control-surface
/// consumer isn't constrained by a terminal column, so this is generous
/// rather than tuned to any real screen.
const CONTROL_BODY_WRAP_WIDTH: usize = 100;

/// Renders a message body to plain text via `mailcore::htmlrender` — the
/// same HTML→text engine the reading pane uses (see `ui::reading`) — so
/// `mail.read`'s `body_text` matches what a human sees there, just without
/// ratatui styling.
fn render_body_plain(body: &Body) -> String {
    let lines = if body.content_type.eq_ignore_ascii_case("html") {
        htmlrender::render_html(&body.content, CONTROL_BODY_WRAP_WIDTH)
    } else {
        htmlrender::render_text(&body.content, CONTROL_BODY_WRAP_WIDTH)
    };
    lines
        .iter()
        .map(|line| {
            let indent = " ".repeat(line.indent as usize * htmlrender::INDENT_SPACES);
            let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            format!("{indent}{text}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read(app: &App, args: &Json) -> Result<Json, String> {
    let id = args.get_str("id").ok_or("mail.read needs an 'id'")?;
    let row = find_message(&app.store, id).ok_or_else(|| format!("no such message '{id}'"))?;
    let mut fields = vec![
        ("id", Json::Str(row.id.clone())),
        ("folder", Json::Str(row.folder_id.clone())),
        ("subject", Json::Str(row.subject.clone())),
        (
            "from",
            Json::Str(format!("{} <{}>", row.from_name, row.from_addr)),
        ),
        ("to", Json::Str(row.to_recipients.clone())),
        ("cc", Json::Str(row.cc_recipients.clone())),
        ("received", Json::Str(row.received_at.clone())),
        ("is_read", Json::Bool(row.is_read)),
        ("is_flagged", Json::Bool(row.is_flagged)),
        ("has_attachments", Json::Bool(row.has_attachments)),
    ];
    match app.store.get_body(id).map_err(|e| e.to_string())? {
        Some(body) => fields.push(("body_text", Json::Str(render_body_plain(&body)))),
        None => {
            fields.push(("body_text", Json::Str(String::new())));
            fields.push(("body_pending", Json::Bool(true)));
            let _ = app
                .sync
                .cmd_tx
                .send(SyncCommand::FetchBody { id: id.to_string() });
        }
    }
    Ok(Json::obj(fields))
}

fn search(app: &App, args: &Json) -> Result<Json, String> {
    let query = args.get_str("query").ok_or("mail.search needs a 'query'")?;
    let limit = args.get_usize("limit").unwrap_or(50) as i64;
    let rows = app.store.search(query, limit).map_err(|e| e.to_string())?;
    Ok(Json::obj(vec![
        ("query", Json::Str(query.to_string())),
        ("count", Json::Num(rows.len() as f64)),
        (
            "messages",
            Json::Arr(rows.iter().map(message_summary).collect()),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Triage verbs — optimistic Store write + SyncCommand, same two-step as the
// UI's own triage keys (see `App::mark_read`/`toggle_flag`/`confirm_move`/
// `delete_selected`), except keyed by the id in `args` rather than the
// highlighted row.
// ---------------------------------------------------------------------------

fn mark(app: &mut App, args: &Json) -> Result<Json, String> {
    let id = args
        .get_str("id")
        .ok_or("mail.mark needs an 'id'")?
        .to_string();
    let read = args
        .get("read")
        .and_then(Json::as_bool)
        .ok_or("mail.mark needs a boolean 'read'")?;
    app.store.set_read(&id, read);
    app.reload_messages();
    let _ = app.sync.cmd_tx.send(SyncCommand::MarkRead {
        id: id.clone(),
        read,
    });
    Ok(Json::obj(vec![
        ("id", Json::Str(id)),
        ("is_read", Json::Bool(read)),
    ]))
}

fn flag(app: &mut App, args: &Json) -> Result<Json, String> {
    let id = args
        .get_str("id")
        .ok_or("mail.flag needs an 'id'")?
        .to_string();
    let flagged = args
        .get("flagged")
        .and_then(Json::as_bool)
        .ok_or("mail.flag needs a boolean 'flagged'")?;
    app.store.set_flag(&id, flagged);
    app.reload_messages();
    let _ = app.sync.cmd_tx.send(SyncCommand::SetFlag {
        id: id.clone(),
        flagged,
    });
    Ok(Json::obj(vec![
        ("id", Json::Str(id)),
        ("is_flagged", Json::Bool(flagged)),
    ]))
}

fn move_msg(app: &mut App, args: &Json) -> Result<Json, String> {
    let id = args
        .get_str("id")
        .ok_or("mail.move needs an 'id'")?
        .to_string();
    let dest = args
        .get_str("dest")
        .ok_or("mail.move needs a 'dest' folder id")?
        .to_string();
    app.store
        .move_message(&id, &dest)
        .map_err(|e| e.to_string())?;
    app.reload_messages();
    let _ = app.sync.cmd_tx.send(SyncCommand::Move {
        id: id.clone(),
        dest: dest.clone(),
    });
    Ok(Json::obj(vec![
        ("id", Json::Str(id)),
        ("folder", Json::Str(dest)),
    ]))
}

fn delete(app: &mut App, args: &Json) -> Result<Json, String> {
    let id = args
        .get_str("id")
        .ok_or("mail.delete needs an 'id'")?
        .to_string();
    app.store.delete_message(&id).map_err(|e| e.to_string())?;
    app.reload_messages();
    let _ = app.sync.cmd_tx.send(SyncCommand::Delete { id: id.clone() });
    Ok(Json::obj(vec![
        ("id", Json::Str(id)),
        ("deleted", Json::Bool(true)),
    ]))
}

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

fn attachments(app: &App, args: &Json) -> Result<Json, String> {
    let id = args
        .get_str("id")
        .ok_or("mail.attachments needs an 'id'")?
        .to_string();
    let items = app.store.attachments(&id).map_err(|e| e.to_string())?;
    if items.is_empty() {
        if let Some(row) = find_message(&app.store, &id) {
            if row.has_attachments {
                let _ = app.sync.cmd_tx.send(SyncCommand::FetchAttachments {
                    message_id: id.clone(),
                });
            }
        }
    }
    let arr = items
        .iter()
        .map(|a| {
            Json::obj(vec![
                ("id", Json::Str(a.id.clone())),
                ("name", Json::Str(a.name.clone())),
                ("content_type", Json::Str(a.content_type.clone())),
                ("size", Json::Num(a.size as f64)),
            ])
        })
        .collect();
    Ok(Json::obj(vec![
        ("id", Json::Str(id)),
        ("attachments", Json::Arr(arr)),
    ]))
}

fn save_attachment(app: &App, args: &Json) -> Result<Json, String> {
    let id = args
        .get_str("id")
        .ok_or("mail.save-attachment needs an 'id'")?
        .to_string();
    let attachment_id = args
        .get_str("attachment")
        .ok_or("mail.save-attachment needs an 'attachment'")?
        .to_string();
    let dest = match args.get_str("dest") {
        Some(d) => resolve_explicit_dest(d),
        None => {
            let items = app.store.attachments(&id).map_err(|e| e.to_string())?;
            let att = items
                .iter()
                .find(|a| a.id == attachment_id)
                .ok_or_else(|| format!("no attachment '{attachment_id}' on message '{id}'"))?;
            downloads_dir().join(sanitize_filename(&att.name))
        }
    };
    let _ = app.sync.cmd_tx.send(SyncCommand::SaveAttachment {
        message_id: id,
        attachment_id,
        dest: dest.clone(),
    });
    Ok(Json::obj(vec![
        ("queued", Json::Bool(true)),
        ("dest", Json::Str(dest.display().to_string())),
    ]))
}

/// Resolves the `dest` argument of `mail.save-attachment` to a path that is
/// always confined to the Downloads directory. A caller-supplied `dest` is
/// interpreted as a *filename*: only its final path component is kept and run
/// through `sanitize_filename`, so a malicious or prompt-injected control
/// client can't turn "save an attachment" into an arbitrary-path write (e.g.
/// dropping an attacker-chosen file into a Startup folder for code execution
/// at next logon). M2.
fn resolve_explicit_dest(dest: &str) -> PathBuf {
    let final_component = Path::new(dest)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    downloads_dir().join(sanitize_filename(final_component))
}

// ---------------------------------------------------------------------------
// Selection / refresh
// ---------------------------------------------------------------------------

fn select(app: &mut App, args: &Json) -> Result<Json, String> {
    if let Some(folder) = args.get_str("folder") {
        let exists = app
            .store
            .folders()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|f| f.id == folder);
        if !exists {
            return Err(format!("no such folder '{folder}'"));
        }
        app.selected_folder = Some(folder.to_string());
        app.reload_messages();
    }
    if let Some(id) = args.get_str("id") {
        app.open_message(id);
    }
    let mut fields = Vec::new();
    if let Some(f) = &app.selected_folder {
        fields.push(("selected_folder", Json::Str(f.clone())));
    }
    if let Some(m) = &app.selected_msg {
        fields.push(("selected_message", Json::Str(m.clone())));
    }
    Ok(Json::obj(fields))
}

fn refresh(app: &App) -> Json {
    let _ = app.sync.cmd_tx.send(SyncCommand::Refresh);
    Json::obj(vec![("refreshing", Json::Bool(true))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::graph::model::{AttachmentMeta, MailFolder, Message, Recipient};

    fn args(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    #[test]
    fn explicit_save_dest_is_confined_to_downloads() {
        let dl = downloads_dir();
        // Absolute and traversal dests must all collapse to a single
        // sanitized file name directly under Downloads — never an
        // arbitrary-path write. M2. (Forward slashes only, so the parsing is
        // identical on Windows and the Unix CI targets.)
        for evil in [
            "/etc/cron.d/evil",
            "../../../../Startup/run.bat",
            "sub/dir/note.txt",
        ] {
            let resolved = resolve_explicit_dest(evil);
            assert_eq!(resolved.parent().unwrap(), dl.as_path());
            assert!(!resolved.to_string_lossy().contains(".."));
        }
        assert_eq!(
            resolve_explicit_dest("sub/dir/note.txt")
                .file_name()
                .unwrap(),
            std::ffi::OsStr::new("note.txt")
        );
    }

    fn seed_second_folder(app: &mut App) {
        app.store
            .upsert_folder(&MailFolder {
                id: "archive".into(),
                display_name: "Archive".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("archive".into()),
            })
            .expect("seed archive folder");
    }

    fn drain_last_command(app: &App) -> Option<SyncCommand> {
        let mut last = None;
        if let Some(rx) = &app.test_cmd_rx {
            while let Ok(cmd) = rx.try_recv() {
                last = Some(cmd);
            }
        }
        last
    }

    #[test]
    fn status_reports_account_folders_and_selection() {
        let app = App::for_test_with_seeded_store();
        let r = status(&app);
        assert_eq!(r.get("account").unwrap(), &Json::Null);
        assert_eq!(r.get_str("sync_state"), Some("idle"));
        assert_eq!(r.get_usize("folders"), Some(1));
        assert_eq!(r.get_usize("unread_total"), Some(1));
        assert_eq!(r.get_usize("pending_ops"), Some(0));
        assert_eq!(r.get_str("selected_folder"), Some("inbox"));
    }

    #[test]
    fn folders_lists_the_seeded_folder() {
        let app = App::for_test_with_seeded_store();
        let r = folders(&app);
        let fs = r.get("folders").unwrap().as_array().unwrap();
        assert_eq!(fs.len(), 1);
        assert_eq!(fs[0].get_str("id"), Some("inbox"));
        assert_eq!(fs[0].get_str("name"), Some("Inbox"));
        assert_eq!(fs[0].get_usize("unread"), Some(1));
        assert_eq!(fs[0].get_str("well_known"), Some("inbox"));
    }

    #[test]
    fn list_defaults_to_the_selected_folder() {
        let app = App::for_test_with_seeded_store();
        let r = list(&app, &Json::Null).unwrap();
        assert_eq!(r.get_str("folder"), Some("inbox"));
        let msgs = r.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].get_str("id"), Some("m1"));
        assert_eq!(msgs[0].get_str("subject"), Some("Hello"));
        assert_eq!(msgs[0].get("is_read").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn list_respects_explicit_folder_and_limit() {
        let mut app = App::for_test_with_seeded_store();
        seed_second_folder(&mut app);
        let r = list(
            &app,
            &args(vec![
                ("folder", Json::Str("archive".into())),
                ("limit", Json::Num(10.0)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_str("folder"), Some("archive"));
        assert!(r.get("messages").unwrap().as_array().unwrap().is_empty());
    }

    #[test]
    fn read_returns_cached_body() {
        let app = App::for_test_with_seeded_store();
        app.store
            .put_body(
                "m1",
                &Body {
                    content_type: "text".into(),
                    content: "hello body".into(),
                },
            )
            .unwrap();
        let r = read(&app, &args(vec![("id", Json::Str("m1".into()))])).unwrap();
        assert_eq!(r.get_str("id"), Some("m1"));
        assert_eq!(r.get_str("folder"), Some("inbox"));
        assert_eq!(r.get_str("body_text"), Some("hello body"));
        assert!(r.get("body_pending").is_none());
    }

    #[test]
    fn read_requests_a_fetch_when_body_is_missing() {
        let app = App::for_test_with_seeded_store();
        let r = read(&app, &args(vec![("id", Json::Str("m1".into()))])).unwrap();
        assert_eq!(r.get("body_pending").unwrap().as_bool(), Some(true));
        assert_eq!(r.get_str("body_text"), Some(""));
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::FetchBody { id }) if id == "m1"
        ));
    }

    #[test]
    fn read_errors_on_unknown_id() {
        let app = App::for_test_with_seeded_store();
        assert!(read(&app, &args(vec![("id", Json::Str("nope".into()))])).is_err());
    }

    #[test]
    fn search_finds_seeded_message_by_subject() {
        let app = App::for_test_with_seeded_store();
        let r = search(&app, &args(vec![("query", Json::Str("Hello".into()))])).unwrap();
        assert_eq!(r.get_usize("count"), Some(1));
        let msgs = r.get("messages").unwrap().as_array().unwrap();
        assert_eq!(msgs[0].get_str("id"), Some("m1"));
    }

    #[test]
    fn mark_writes_store_and_sends_command() {
        let mut app = App::for_test_with_seeded_store();
        let r = mark(
            &mut app,
            &args(vec![
                ("id", Json::Str("m1".into())),
                ("read", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("is_read").unwrap().as_bool(), Some(true));
        assert!(app.messages.iter().find(|m| m.id == "m1").unwrap().is_read);
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::MarkRead { id, read: true }) if id == "m1"
        ));
    }

    #[test]
    fn flag_writes_store_and_sends_command() {
        let mut app = App::for_test_with_seeded_store();
        let r = flag(
            &mut app,
            &args(vec![
                ("id", Json::Str("m1".into())),
                ("flagged", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("is_flagged").unwrap().as_bool(), Some(true));
        assert!(
            app.messages
                .iter()
                .find(|m| m.id == "m1")
                .unwrap()
                .is_flagged
        );
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::SetFlag { id, flagged: true }) if id == "m1"
        ));
    }

    #[test]
    fn move_relocates_and_sends_command() {
        let mut app = App::for_test_with_seeded_store();
        seed_second_folder(&mut app);
        let r = move_msg(
            &mut app,
            &args(vec![
                ("id", Json::Str("m1".into())),
                ("dest", Json::Str("archive".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_str("folder"), Some("archive"));
        assert!(
            app.store
                .messages_in_folder("inbox", 50, 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            app.store.messages_in_folder("archive", 50, 0).unwrap()[0].id,
            "m1"
        );
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::Move { id, dest }) if id == "m1" && dest == "archive"
        ));
    }

    #[test]
    fn delete_removes_and_sends_command() {
        let mut app = App::for_test_with_seeded_store();
        let r = delete(&mut app, &args(vec![("id", Json::Str("m1".into()))])).unwrap();
        assert_eq!(r.get("deleted").unwrap().as_bool(), Some(true));
        assert!(
            app.store
                .messages_in_folder("inbox", 50, 0)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::Delete { id }) if id == "m1"
        ));
    }

    #[test]
    fn attachments_returns_stored_metadata() {
        let app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .unwrap();
        let r = attachments(&app, &args(vec![("id", Json::Str("m1".into()))])).unwrap();
        let items = r.get("attachments").unwrap().as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get_str("name"), Some("notes.txt"));
    }

    #[test]
    fn attachments_requests_a_fetch_when_none_stored_but_graph_has_some() {
        let app = App::for_test_with_seeded_store();
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m1".into(),
                    conversation_id: "c1".into(),
                    subject: "Hello".into(),
                    from: Recipient {
                        name: "Alice".into(),
                        address: "alice@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T10:00:00Z".into(),
                    sent: "2026-07-16T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: true,
                    importance: "normal".into(),
                    preview: "hi there".into(),
                    is_draft: false,
                },
            )
            .unwrap();
        let r = attachments(&app, &args(vec![("id", Json::Str("m1".into()))])).unwrap();
        assert!(r.get("attachments").unwrap().as_array().unwrap().is_empty());
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::FetchAttachments { message_id }) if message_id == "m1"
        ));
    }

    #[test]
    fn save_attachment_defaults_dest_to_downloads() {
        let app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .unwrap();
        let r = save_attachment(
            &app,
            &args(vec![
                ("id", Json::Str("m1".into())),
                ("attachment", Json::Str("a1".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get("queued").unwrap().as_bool(), Some(true));
        let dest = r.get_str("dest").unwrap();
        assert!(dest.ends_with("notes.txt"));
        assert!(dest.contains("Downloads"));
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::SaveAttachment { dest, .. }) if dest.file_name().unwrap() == "notes.txt"
        ));
    }

    #[test]
    fn save_attachment_confines_explicit_dest_to_downloads() {
        // An explicit out-of-tree dest must be confined to Downloads (final
        // component only), not honored verbatim — otherwise the control
        // surface is an arbitrary-path write primitive. M2.
        let app = App::for_test_with_seeded_store();
        let r = save_attachment(
            &app,
            &args(vec![
                ("id", Json::Str("m1".into())),
                ("attachment", Json::Str("a1".into())),
                ("dest", Json::Str("C:/tmp/custom.txt".into())),
            ]),
        )
        .unwrap();
        let dest = r.get_str("dest").unwrap();
        assert_ne!(dest, "C:/tmp/custom.txt");
        assert!(dest.contains("Downloads"));
        assert!(dest.ends_with("custom.txt"));
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::SaveAttachment { dest, .. }) if dest.file_name().unwrap() == "custom.txt"
        ));
    }

    #[test]
    fn select_updates_folder_and_message() {
        let mut app = App::for_test_with_seeded_store();
        seed_second_folder(&mut app);
        let r = select(
            &mut app,
            &args(vec![
                ("folder", Json::Str("archive".into())),
                ("id", Json::Str("m1".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_str("selected_folder"), Some("archive"));
        assert_eq!(r.get_str("selected_message"), Some("m1"));
        assert_eq!(app.selected_folder.as_deref(), Some("archive"));
        assert_eq!(app.selected_msg.as_deref(), Some("m1"));
    }

    #[test]
    fn select_errors_on_unknown_folder() {
        let mut app = App::for_test_with_seeded_store();
        assert!(select(&mut app, &args(vec![("folder", Json::Str("nope".into()))])).is_err());
    }

    #[test]
    fn refresh_sends_refresh_command() {
        let app = App::for_test_with_seeded_store();
        let r = refresh(&app);
        assert_eq!(r.get("refreshing").unwrap().as_bool(), Some(true));
        assert!(matches!(
            drain_last_command(&app),
            Some(SyncCommand::Refresh)
        ));
    }

    #[test]
    fn dispatch_routes_a_known_verb_and_errors_on_unknown() {
        let mut app = App::for_test_with_seeded_store();
        assert!(dispatch(&mut app, "mail.status", &Json::Null).is_ok());
        let err = dispatch(&mut app, "mail.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }
}
