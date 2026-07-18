//! The docxy control surface: maps [`ctlcore`] verbs onto the **live** editor
//! buffer, so an external agent (e.g. Claude Code in a sibling agwinterm pane)
//! can read and edit the open document without touching the file on disk.
//!
//! Every mutating verb goes through [`docxcore::editor::Editor`], so an agent's
//! edits land on the *same* undo stack as keyboard edits and repaint the view
//! live; reads serialize `editor.doc`, so they always reflect unsaved changes.
//!
//! Addressing is by **top-level block index** (position in `doc.body`): a
//! paragraph or table. `doc.read` / `doc.outline` report each block's `kind`, so
//! a client knows which indices are paragraphs (the ones the edit verbs accept).
//!
//! ## Verbs
//!
//! | Verb | Args | Result |
//! |---|---|---|
//! | `doc.path` | — | `{path, format, modified, blocks, protection?, watermark?}` |
//! | `doc.outline` | — | `{headings:[{index, level, text}]}` |
//! | `doc.read` | `{start?, end?, range?}` | `{total, start, end, text, blocks:[…]}` |
//! | `doc.find` | `{query, case_sensitive?}` | `{query, count, matches:[…]}` |
//! | `doc.replace-range` | `{start, end?, text}` | `{replaced, total}` |
//! | `doc.insert` | `{at, text}` | `{total}` |
//! | `doc.append` | `{text}` | `{total}` |
//! | `doc.save` | — | `{path, …}` |
//! | `doc.reload` | — | `{path, …}` |
//! | `doc.open` | `{path}` | `{path, …}` |
//! | `doc.export` | `{format:"markdown"\|"text"}` | `{format, text}` — the live buffer |
//! | `doc.comments` | — | `{comments:[{id,author,initials,date,text,anchor}]}` |
//! | `doc.notes` | — | `{notes:[{id,kind:"footnote"\|"endnote",text}]}` |
//! | `doc.header` / `doc.footer` | — | `{blocks:[{index,kind,text}]}` (empty if none) |
//! | `doc.metadata` | — | present-if-set keys: `{title?,author?,subject?,keywords?,comments?,last_saved_by?,revision?,created?,modified?}` |
//! | `doc.stats` | — | `{words, chars, paragraphs, blocks}` |
//! | `doc.replace-all` | `{query, text, case_sensitive?}` | `{replaced}` |
//! | `doc.undo` / `doc.redo` | — | `{done}` (false = nothing to undo/redo) |
//! | `doc.export-pdf` | `{path}` | `{path}` (absolutized; refuses to overwrite) |

use crate::{App, DocFormat};
use ctlcore::json::Json;
use docxcore::agent;
use docxcore::export::{PdfOptions, to_pdf};
use docxcore::model::Block;
use std::path::Path;

/// The directory where docxy publishes its control discovery files:
/// `<config>/docxy/ctl` (see [`ctlcore::config_ctl_dir`]).
pub fn control_dir() -> Option<std::path::PathBuf> {
    ctlcore::config_ctl_dir("docxy")
}

/// This editor's control instance name: `docxy-<AGWINTERM_SESSION_ID|pid>`
/// (see [`ctlcore::instance_name`]).
pub fn instance_name() -> String {
    ctlcore::instance_name("docxy")
}

/// Route one control verb against the live document, returning the JSON result
/// or an error message. Mutating verbs set `modified`; every verb requests a
/// repaint so pane B reflects the change immediately.
pub fn dispatch(app: &mut App, verb: &str, args: &Json) -> Result<Json, String> {
    let out = match verb {
        "doc.path" => Ok(path_info(app)),
        "doc.outline" => Ok(outline(app)),
        "doc.read" => read(app, args),
        "doc.find" => find(app, args),
        "doc.replace-range" => replace_range(app, args),
        "doc.insert" => insert(app, args),
        "doc.append" => append(app, args),
        "doc.export" => export(app, args),
        "doc.comments" => Ok(comments(app)),
        "doc.notes" => Ok(notes(app)),
        // Default section variant only — first-page/even-page headers and
        // footers (`app.headers.first`/`.even`, `app.footers.first`/`.even`)
        // are not surfaced by this verb. Tasks 3/8 (docxwasm, MCP tools) must
        // mirror this default-only choice, not add `first`/`even` on their own.
        "doc.header" => Ok(header_footer(&app.headers.default)),
        "doc.footer" => Ok(header_footer(&app.footers.default)),
        "doc.metadata" => Ok(metadata(app)),
        "doc.stats" => Ok(stats(app)),
        "doc.replace-all" => replace_all(app, args),
        "doc.undo" => Ok(undo(app)),
        "doc.redo" => Ok(redo(app)),
        "doc.export-pdf" => export_pdf(app, args),
        "doc.save" => {
            app.save();
            Ok(path_info(app))
        }
        "doc.reload" => {
            let p = app.path.clone();
            app.open_path(Path::new(&p));
            Ok(path_info(app))
        }
        "doc.open" => {
            let p = args
                .get_str("path")
                .ok_or("doc.open needs a 'path' string")?
                .to_string();
            app.open_path(Path::new(&p));
            Ok(path_info(app))
        }
        other => Err(format!("unknown verb '{other}'")),
    };
    // Any successful control interaction repaints the view; the edit verbs
    // additionally mark the document modified (inside their handlers).
    if out.is_ok() {
        app.dirty = true;
        // A content edit flashes this pane's agent-status dot, so a watcher sees
        // the document being worked on.
        if matches!(verb, "doc.replace-range" | "doc.insert" | "doc.append") {
            ctlcore::signal_activity();
        }
        // doc.replace-all/doc.undo/doc.redo can each legitimately no-op (no
        // match found; empty undo/redo stack), so they signal activity
        // themselves (see replace_all()/undo()/redo() below), gated on
        // whether anything actually changed — a no-op must not look like an
        // edit occurred.
    }
    out
}

// ---------------------------------------------------------------------------
// Read-only verbs
// ---------------------------------------------------------------------------

fn path_info(app: &App) -> Json {
    let fmt = match app.format {
        DocFormat::Docx => "docx",
        DocFormat::Markdown => "markdown",
    };
    let mut fields = vec![
        ("path", Json::Str(app.path.clone())),
        ("format", Json::Str(fmt.to_string())),
        ("modified", Json::Bool(app.modified)),
        ("blocks", Json::Num(app.editor.doc.body.len() as f64)),
    ];
    // Only present when the package actually carries the state — an
    // unprotected, unwatermarked document must not gain these keys at all.
    if let Some(p) = &app.doc_protection {
        fields.push(("protection", Json::Str(p.clone())));
    }
    if let Some(w) = &app.doc_watermark {
        fields.push(("watermark", Json::Str(w.clone())));
    }
    Json::obj(fields)
}

fn outline(app: &App) -> Json {
    let items = agent::outline(&app.editor.doc)
        .into_iter()
        .map(|h| {
            Json::obj(vec![
                ("index", Json::Num(h.index as f64)),
                ("level", Json::Num(h.level as f64)),
                ("text", Json::Str(h.text)),
            ])
        })
        .collect();
    Json::obj(vec![("headings", Json::Arr(items))])
}

fn read(app: &App, args: &Json) -> Result<Json, String> {
    let n = app.editor.doc.body.len();
    let (start, end) = range_args(args, n)?;
    let blocks = agent::read(&app.editor.doc, start, end)?;
    let joined = blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let arr = blocks
        .into_iter()
        .map(|b| {
            let mut fields = vec![
                ("index", Json::Num(b.index as f64)),
                ("kind", Json::Str(b.kind.to_string())),
                ("text", Json::Str(b.text)),
            ];
            if let Some(level) = b.heading {
                fields.push(("heading", Json::Num(level as f64)));
            }
            Json::obj(fields)
        })
        .collect();
    Ok(Json::obj(vec![
        ("total", Json::Num(n as f64)),
        ("start", Json::Num(start as f64)),
        ("end", Json::Num(end as f64)),
        ("text", Json::Str(joined)),
        ("blocks", Json::Arr(arr)),
    ]))
}

fn find(app: &App, args: &Json) -> Result<Json, String> {
    let query = args.get_str("query").ok_or("doc.find needs a 'query'")?;
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let body = &app.editor.doc.body;
    let mut matches = Vec::new();
    for m in agent::find(&app.editor.doc, query, case_sensitive) {
        let mut f = vec![
            (
                "path",
                Json::Arr(m.path.iter().map(|&x| Json::Num(x as f64)).collect()),
            ),
            ("start", Json::Num(m.start as f64)),
            ("end", Json::Num(m.end as f64)),
        ];
        // Top-level paragraph matches carry a direct block index + full text,
        // which a client can feed straight back to `doc.replace-range`.
        if m.path.len() == 1 {
            f.push(("block", Json::Num(m.path[0] as f64)));
            if let Some(Block::Paragraph(p)) = body.get(m.path[0]) {
                f.push(("text", Json::Str(p.plain_text())));
            }
        }
        matches.push(Json::obj(f));
    }
    Ok(Json::obj(vec![
        ("query", Json::Str(query.to_string())),
        ("count", Json::Num(matches.len() as f64)),
        ("matches", Json::Arr(matches)),
    ]))
}

/// `doc.export`: the live buffer as Markdown or plain text, on the same
/// terms an agent would read the saved file — except this reflects unsaved
/// edits.
fn export(app: &App, args: &Json) -> Result<Json, String> {
    let format = args
        .get_str("format")
        .ok_or("doc.export needs a 'format' (markdown|text)")?;
    let text = match format {
        "markdown" => docxcore::markdown::to_markdown(&app.editor.doc),
        "text" => app.editor.doc.plain_text(),
        other => return Err(format!("unknown format '{other}' (markdown|text)")),
    };
    Ok(Json::obj(vec![
        ("format", Json::Str(format.to_string())),
        ("text", Json::Str(text)),
    ]))
}

/// `doc.comments`: the review comments parsed at load (or last reload), in
/// anchor order.
fn comments(app: &App) -> Json {
    let items = app
        .comments
        .iter()
        .map(|c| {
            Json::obj(vec![
                ("id", Json::Str(c.id.clone())),
                ("author", Json::Str(c.author.clone())),
                ("initials", Json::Str(c.initials.clone())),
                ("date", Json::Str(c.date.clone())),
                ("text", Json::Str(c.text.clone())),
                ("anchor", Json::Str(c.quoted.clone())),
            ])
        })
        .collect();
    Json::obj(vec![("comments", Json::Arr(items))])
}

/// `doc.notes`: footnotes then endnotes, in file order.
fn notes(app: &App) -> Json {
    let items = app
        .notes
        .iter()
        .map(|n| {
            let kind = if n.endnote { "endnote" } else { "footnote" };
            Json::obj(vec![
                ("id", Json::Num(n.id as f64)),
                ("kind", Json::Str(kind.to_string())),
                ("text", Json::Str(n.text.clone())),
            ])
        })
        .collect();
    Json::obj(vec![("notes", Json::Arr(items))])
}

/// `doc.header` / `doc.footer`: the default section header/footer's block
/// content, empty when the document has none. Callers pass
/// `app.headers.default`/`app.footers.default` only — see the dispatch note
/// at the `doc.header`/`doc.footer` match arms for why first/even variants
/// are out of scope for this verb.
fn header_footer(blocks: &[Block]) -> Json {
    let items = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            Json::obj(vec![
                ("index", Json::Num(i as f64)),
                ("kind", Json::Str(agent::block_kind(b).to_string())),
                ("text", Json::Str(b.plain_text())),
            ])
        })
        .collect();
    Json::obj(vec![("blocks", Json::Arr(items))])
}

/// `doc.metadata`: `docProps/core.xml`, present-if-set (empty strings and
/// unparsed dates are omitted rather than sent as empty/null).
fn metadata(app: &App) -> Json {
    let props = app
        .pkg
        .part("docProps/core.xml")
        .map(|b| docxcore::field::parse_core_props(std::str::from_utf8(b).unwrap_or("")))
        .unwrap_or_default();
    let mut fields: Vec<(&str, Json)> = Vec::new();
    for (key, val) in [
        ("title", &props.title),
        ("author", &props.author),
        ("subject", &props.subject),
        ("keywords", &props.keywords),
        ("comments", &props.comments),
        ("last_saved_by", &props.last_saved_by),
        ("revision", &props.revision),
    ] {
        if !val.is_empty() {
            fields.push((key, Json::Str(val.clone())));
        }
    }
    if let Some(dt) = &props.created {
        fields.push(("created", Json::Str(docxcore::field::format_iso(dt))));
    }
    if let Some(dt) = &props.modified {
        fields.push(("modified", Json::Str(docxcore::field::format_iso(dt))));
    }
    Json::obj(fields)
}

/// `doc.stats`: word/char/paragraph/block counts over the live buffer.
fn stats(app: &App) -> Json {
    let (words, chars, paragraphs, blocks) = agent::stats(&app.editor.doc);
    Json::obj(vec![
        ("words", Json::Num(words as f64)),
        ("chars", Json::Num(chars as f64)),
        ("paragraphs", Json::Num(paragraphs as f64)),
        ("blocks", Json::Num(blocks as f64)),
    ])
}

// ---------------------------------------------------------------------------
// Mutating verbs (undoable, via the Editor)
// ---------------------------------------------------------------------------

fn replace_range(app: &mut App, args: &Json) -> Result<Json, String> {
    let start = args
        .get_usize("start")
        .ok_or("doc.replace-range needs a 'start' index")?;
    let end = args.get_usize("end").unwrap_or(start);
    let text = args
        .get_str("text")
        .ok_or("doc.replace-range needs 'text'")?;
    // The terminal app doesn't drive a host undo stack, so it ignores the
    // checkpoint count `agent::replace_range` now also reports; its wire reply
    // stays exactly `{replaced, total}`.
    let (replaced, _undo_steps) = agent::replace_range(&mut app.editor, start, end, text)?;
    finish_edit(app);
    Ok(Json::obj(vec![
        ("replaced", Json::Num(replaced as f64)),
        ("total", Json::Num(app.editor.doc.body.len() as f64)),
    ]))
}

fn insert(app: &mut App, args: &Json) -> Result<Json, String> {
    let at = args
        .get_usize("at")
        .ok_or("doc.insert needs an 'at' index")?;
    let text = args.get_str("text").ok_or("doc.insert needs 'text'")?;
    agent::insert(&mut app.editor, at, text)?;
    finish_edit(app);
    Ok(Json::obj(vec![(
        "total",
        Json::Num(app.editor.doc.body.len() as f64),
    )]))
}

fn append(app: &mut App, args: &Json) -> Result<Json, String> {
    let text = args.get_str("text").ok_or("doc.append needs 'text'")?;
    agent::append(&mut app.editor, text);
    finish_edit(app);
    Ok(Json::obj(vec![(
        "total",
        Json::Num(app.editor.doc.body.len() as f64),
    )]))
}

/// `doc.replace-all`: replace every occurrence of `query` with `text` across
/// the whole document (case-insensitive unless `case_sensitive:true`). The
/// terminal app ignores the checkpoint count `agent::replace_all` also
/// reports (see its doc comment: always 1 when something was replaced, 0
/// otherwise) — it doesn't drive a host undo stack, so its wire reply stays
/// exactly `{replaced}`. Unlike `doc.replace-range` (which is bounds-checked
/// and so never has a genuine no-op case), `replace-all` can legitimately
/// match nothing — a `query` that isn't found leaves the document byte-for-
/// byte unchanged and pushes zero undo checkpoints, so it must not be
/// reported as an edit: `finish_edit`/`signal_activity` only fire when
/// `replaced > 0`, same no-op guard as `doc.undo`/`doc.redo` below.
fn replace_all(app: &mut App, args: &Json) -> Result<Json, String> {
    let query = args
        .get_str("query")
        .ok_or("doc.replace-all needs a 'query'")?;
    let text = args.get_str("text").ok_or("doc.replace-all needs 'text'")?;
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let (replaced, _undo_steps) = agent::replace_all(&mut app.editor, query, text, case_sensitive);
    if replaced > 0 {
        finish_edit(app);
        ctlcore::signal_activity();
    }
    Ok(Json::obj(vec![("replaced", Json::Num(replaced as f64))]))
}

/// `doc.undo`: unwind the last edit, if any. A no-op (`{done:false}`, empty
/// undo stack) must not mark the document modified or flash the agent-status
/// dot — nothing actually changed.
fn undo(app: &mut App) -> Json {
    let done = agent::undo(&mut app.editor);
    if done {
        finish_edit(app);
        ctlcore::signal_activity();
    }
    Json::obj(vec![("done", Json::Bool(done))])
}

/// `doc.redo`: replay the last undone edit, if any. Same no-op guard as
/// [`undo`].
fn redo(app: &mut App) -> Json {
    let done = agent::redo(&mut app.editor);
    if done {
        finish_edit(app);
        ctlcore::signal_activity();
    }
    Json::obj(vec![("done", Json::Bool(done))])
}

/// `doc.export-pdf`: render the live buffer to a PDF at `path` (absolutized
/// against this process's cwd) and write it — refusing to overwrite an
/// existing file. Copies the exclusive-create pattern and error-string family
/// (`already exists:`/`bad path:`/`create failed:`), including the parent-
/// directory creation step, from `ctlcore::client::new_file` verbatim, since
/// docxy doesn't depend on `ctlcore`'s fs helpers. Doesn't touch `app.editor`,
/// so it neither marks the document modified nor is part of the undo stack.
fn export_pdf(app: &App, args: &Json) -> Result<Json, String> {
    let path = args
        .get_str("path")
        .ok_or("doc.export-pdf needs a 'path'")?;
    let abs = std::path::absolute(Path::new(path)).map_err(|e| format!("bad path: {e}"))?;
    if abs.exists() {
        return Err(format!("already exists: {}", abs.display()));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create failed: {e}"))?;
    }
    let pdf = to_pdf(
        &app.editor.doc,
        &PdfOptions {
            styles: app.styles.clone(),
            ..PdfOptions::default()
        },
    );
    // create_new: exclusive-create, so a file appearing between the exists
    // check above and this open errors instead of being truncated.
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&abs)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("already exists: {}", abs.display())
            } else {
                format!("create failed: {e}")
            }
        })?;
    std::io::Write::write_all(&mut f, &pdf).map_err(|e| format!("create failed: {e}"))?;
    Ok(Json::obj(vec![(
        "path",
        Json::Str(abs.display().to_string()),
    )]))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Clear the transient selection, re-validate the caret, and mark the document
/// modified after a mutating edit.
fn finish_edit(app: &mut App) {
    app.editor.clear_selection();
    app.editor.clamp();
    app.modified = true;
    app.dirty = true;
}

/// Resolve an optional block range from `{start, end}` or `{range:"a..b"}`,
/// defaulting to the whole document.
fn range_args(args: &Json, n: usize) -> Result<(usize, usize), String> {
    if n == 0 {
        return Err("document is empty".into());
    }
    let mut start = args.get_usize("start");
    let mut end = args.get_usize("end");
    if let Some(r) = args.get_str("range") {
        let (a, b) = parse_range_str(r)?;
        start = start.or(a);
        end = end.or(b);
    }
    let start = start.unwrap_or(0);
    let end = end.unwrap_or(n - 1);
    agent::bounds(start, end, n)?;
    Ok((start, end))
}

/// Parse `"a..b"`, `"a.."`, `"..b"`, or `"a"` into optional bounds.
fn parse_range_str(s: &str) -> Result<(Option<usize>, Option<usize>), String> {
    let s = s.trim();
    let parse = |t: &str| -> Result<Option<usize>, String> {
        let t = t.trim();
        if t.is_empty() {
            Ok(None)
        } else {
            t.parse::<usize>()
                .map(Some)
                .map_err(|_| format!("bad range bound '{t}'"))
        }
    };
    match s.split_once("..") {
        Some((a, b)) => Ok((parse(a)?, parse(b)?)),
        None => {
            let v = parse(s)?;
            Ok((v, v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::{Block, Document, ParProps, Paragraph, Run, RunProps};
    use docxcore::package::new_package;

    /// A document of simple text paragraphs.
    fn doc_with(paras: &[&str]) -> Document {
        let body = paras
            .iter()
            .map(|t| {
                Block::Paragraph(Paragraph {
                    props: ParProps::default(),
                    content: vec![docxcore::model::Inline::Run(Run {
                        text: t.to_string(),
                        props: RunProps::default(),
                    })],
                })
            })
            .collect();
        Document { body }
    }

    fn app_with(paras: &[&str]) -> App {
        App::new(new_package(doc_with(paras)), "ctl-test.docx", false)
    }

    fn paras(app: &App) -> Vec<String> {
        app.editor.doc.body.iter().map(|b| b.plain_text()).collect()
    }

    fn args(pairs: Vec<(&str, Json)>) -> Json {
        Json::obj(pairs)
    }

    #[test]
    fn path_reports_format_and_block_count() {
        let app = app_with(&["A", "B"]);
        let r = path_info(&app);
        assert_eq!(r.get_str("path"), Some("ctl-test.docx"));
        assert_eq!(r.get_str("format"), Some("docx"));
        assert_eq!(r.get("modified").unwrap().as_bool(), Some(false));
        assert_eq!(r.get_usize("blocks"), Some(2));
    }

    #[test]
    fn read_returns_range_text_and_blocks() {
        let app = app_with(&["Alpha", "Beta", "Gamma"]);
        let r = read(
            &app,
            &args(vec![("start", Json::Num(1.0)), ("end", Json::Num(2.0))]),
        )
        .unwrap();
        assert_eq!(r.get_str("text"), Some("Beta\n\nGamma"));
        assert_eq!(r.get("blocks").unwrap().as_array().unwrap().len(), 2);
        // Whole-document default range.
        let all = read(&app, &Json::Null).unwrap();
        assert_eq!(all.get_usize("total"), Some(3));
        assert_eq!(all.get_str("text"), Some("Alpha\n\nBeta\n\nGamma"));
    }

    #[test]
    fn read_range_string_form() {
        let app = app_with(&["A", "B", "C", "D"]);
        let r = read(&app, &args(vec![("range", Json::Str("1..2".into()))])).unwrap();
        assert_eq!(r.get_str("text"), Some("B\n\nC"));
    }

    #[test]
    fn read_out_of_bounds_errors() {
        let app = app_with(&["A"]);
        assert!(
            read(
                &app,
                &args(vec![("start", Json::Num(0.0)), ("end", Json::Num(9.0))])
            )
            .is_err()
        );
    }

    #[test]
    fn outline_lists_headings_with_indices() {
        let mut app = app_with(&["Title", "body", "Section", "more"]);
        // Promote blocks 0 and 2 to headings.
        for (i, lvl) in [(0usize, 1u8), (2, 2)] {
            if let Block::Paragraph(p) = &mut app.editor.doc.body[i] {
                p.props.heading_level = Some(lvl);
            }
        }
        let r = outline(&app);
        let hs = r.get("headings").unwrap().as_array().unwrap();
        assert_eq!(hs.len(), 2);
        assert_eq!(hs[0].get_usize("index"), Some(0));
        assert_eq!(hs[0].get_usize("level"), Some(1));
        assert_eq!(hs[0].get_str("text"), Some("Title"));
        assert_eq!(hs[1].get_usize("index"), Some(2));
        assert_eq!(hs[1].get_usize("level"), Some(2));
    }

    #[test]
    fn replace_range_single_paragraph() {
        let mut app = app_with(&["A", "B", "C"]);
        let r = replace_range(
            &mut app,
            &args(vec![
                ("start", Json::Num(1.0)),
                ("text", Json::Str("X".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "C"]);
        assert_eq!(r.get_usize("replaced"), Some(1));
        assert!(app.modified);
    }

    #[test]
    fn replace_range_multiple_paragraphs_with_multiline_text() {
        let mut app = app_with(&["A", "B", "C", "D"]);
        replace_range(
            &mut app,
            &args(vec![
                ("start", Json::Num(1.0)),
                ("end", Json::Num(2.0)),
                ("text", Json::Str("X\nY".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "Y", "D"]);
    }

    #[test]
    fn replace_range_shrinks_and_grows() {
        // Replace two paragraphs with one.
        let mut app = app_with(&["A", "B", "C", "D"]);
        replace_range(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("end", Json::Num(1.0)),
                ("text", Json::Str("Z".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["Z", "C", "D"]);
    }

    #[test]
    fn insert_before_a_paragraph() {
        let mut app = app_with(&["A", "B", "C"]);
        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(1.0)),
                ("text", Json::Str("X".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "B", "C"]);
    }

    #[test]
    fn insert_multiline_before() {
        let mut app = app_with(&["A", "B"]);
        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(1.0)),
                ("text", Json::Str("X\nY".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "Y", "B"]);
    }

    #[test]
    fn insert_at_end_appends() {
        let mut app = app_with(&["A", "B"]);
        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(2.0)),
                ("text", Json::Str("C".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "B", "C"]);
    }

    #[test]
    fn append_adds_paragraphs() {
        let mut app = app_with(&["A"]);
        append(&mut app, &args(vec![("text", Json::Str("B\nC".into()))])).unwrap();
        assert_eq!(paras(&app), vec!["A", "B", "C"]);
    }

    #[test]
    fn edits_are_undoable() {
        let mut app = app_with(&["A", "B", "C"]);
        replace_range(
            &mut app,
            &args(vec![
                ("start", Json::Num(1.0)),
                ("text", Json::Str("X".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "C"]);
        // A replace is a delete-then-insert, exactly like a paste over a selection
        // in the UI, so it unwinds in those two native undo steps back to the
        // original — proving agent edits sit on the same undo stack as keystrokes.
        assert!(app.editor.undo());
        assert!(app.editor.undo());
        assert_eq!(paras(&app), vec!["A", "B", "C"]);
    }

    #[test]
    fn insert_is_a_single_undo_step() {
        let mut app = app_with(&["A", "B"]);
        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(1.0)),
                ("text", Json::Str("X".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "B"]);
        assert!(app.editor.undo());
        assert_eq!(paras(&app), vec!["A", "B"]);
    }

    #[test]
    fn find_reports_block_and_text() {
        let app = app_with(&["hello world", "goodbye world"]);
        let r = find(&app, &args(vec![("query", Json::Str("world".into()))])).unwrap();
        assert_eq!(r.get_usize("count"), Some(2));
        let ms = r.get("matches").unwrap().as_array().unwrap();
        assert_eq!(ms[0].get_usize("block"), Some(0));
        assert_eq!(ms[0].get_str("text"), Some("hello world"));
        assert_eq!(ms[1].get_usize("block"), Some(1));
    }

    #[test]
    fn edit_verbs_reject_non_paragraph_and_oob() {
        let mut app = app_with(&["A"]);
        assert!(
            replace_range(
                &mut app,
                &args(vec![
                    ("start", Json::Num(5.0)),
                    ("text", Json::Str("x".into()))
                ])
            )
            .is_err()
        );
        assert!(
            insert(
                &mut app,
                &args(vec![
                    ("at", Json::Num(9.0)),
                    ("text", Json::Str("x".into()))
                ])
            )
            .is_err()
        );
    }

    #[test]
    fn export_returns_live_markdown() {
        let mut app = app_with(&["Title", "body text"]);
        if let Block::Paragraph(p) = &mut app.editor.doc.body[0] {
            p.props.heading_level = Some(1);
        }
        let r = export(&app, &args(vec![("format", Json::Str("markdown".into()))])).unwrap();
        let text = r.get_str("text").unwrap();
        assert!(text.contains("Title"), "{text}");
        assert_eq!(r.get_str("format"), Some("markdown"));
    }

    #[test]
    fn export_returns_live_plain_text() {
        let app = app_with(&["Alpha", "Beta"]);
        let r = export(&app, &args(vec![("format", Json::Str("text".into()))])).unwrap();
        assert_eq!(r.get_str("format"), Some("text"));
        assert_eq!(r.get_str("text"), Some("Alpha\nBeta\n"));
    }

    #[test]
    fn export_rejects_unknown_format() {
        let app = app_with(&["x"]);
        let err = export(&app, &args(vec![("format", Json::Str("rtf".into()))])).unwrap_err();
        assert!(err.contains("unknown format"), "{err}");
    }

    #[test]
    fn export_requires_format() {
        let app = app_with(&["x"]);
        let err = export(&app, &Json::Null).unwrap_err();
        assert!(err.contains("format"), "{err}");
    }

    #[test]
    fn comments_empty_shape_on_plain_fixture() {
        let app = app_with(&["x"]);
        let r = comments(&app);
        assert_eq!(r.get("comments").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn notes_empty_shape_on_plain_fixture() {
        let app = app_with(&["x"]);
        let r = notes(&app);
        assert_eq!(r.get("notes").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn header_empty_when_no_header_part() {
        let app = app_with(&["x"]);
        let r = header_footer(&app.headers.default);
        assert_eq!(r.get("blocks").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn footer_empty_when_no_footer_part() {
        let app = app_with(&["x"]);
        let r = header_footer(&app.footers.default);
        assert_eq!(r.get("blocks").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn metadata_omits_unset_keys_on_plain_fixture() {
        let app = app_with(&["x"]);
        let r = metadata(&app);
        // new_package() carries no docProps/core.xml part at all, so every
        // present-if-set key is absent.
        assert_eq!(r, Json::obj(vec![]));
    }

    /// Marshalling-level coverage for `metadata()`/`format_iso()`'s actual
    /// transformation (empty-string filtering, date formatting) — the
    /// empty-shape test above only proves the omission path, not that a set
    /// property round-trips or that `format_iso`'s `y-m-d-h-m-s` field order
    /// is correct. Pins the exact ISO string so a swapped month/day (or any
    /// other field-order bug) would fail loudly.
    #[test]
    fn metadata_populated_pins_wire_shape_and_omits_empty_fields() {
        use docxcore::package::load_package;
        use docxcore::zipwrite::write_zip;

        let core_xml = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="b" xmlns:dc="a" xmlns:dcterms="c">
            <dc:creator>Ann</dc:creator>
            <dc:title>Q3 Report</dc:title>
            <dc:subject></dc:subject>
            <dcterms:created>2020-01-02T03:04:05Z</dcterms:created>
        </cp:coreProperties>"#;
        let document_xml =
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body><w:p/></w:body></w:document>";
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let bytes = write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), rels.into()),
            ("word/document.xml".into(), document_xml.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("docProps/core.xml".into(), core_xml.into()),
        ]);
        let mut app = app_with(&["x"]);
        app.pkg = load_package(&bytes).expect("load");

        let r = metadata(&app);
        assert_eq!(r.get_str("title"), Some("Q3 Report"));
        assert_eq!(r.get_str("author"), Some("Ann"));
        // Pins format_iso's exact y-m-d-h-m-s field order: every component is
        // a distinct digit, so a swapped field would produce a different
        // string, not a coincidentally-matching one.
        assert_eq!(r.get_str("created"), Some("2020-01-02T03:04:05Z"));
        // dc:subject is present but empty in the source XML -> the
        // present-if-set filter must omit it entirely, not emit `""`.
        assert!(r.get("subject").is_none(), "{r}");
    }

    #[test]
    fn stats_counts_words_chars_paragraphs() {
        let app = app_with(&["one two", "three"]);
        let r = stats(&app);
        assert_eq!(r.get("words").and_then(Json::as_i64), Some(3));
        assert_eq!(r.get("paragraphs").and_then(Json::as_i64), Some(2));
        assert_eq!(r.get("blocks").and_then(Json::as_i64), Some(2));
        assert_eq!(r.get("chars").and_then(Json::as_i64), Some(12));
    }

    #[test]
    fn path_has_no_protection_or_watermark_keys_when_unset() {
        let app = app_with(&["x"]);
        let r = path_info(&app);
        assert!(r.get("protection").is_none());
        assert!(r.get("watermark").is_none());
    }

    #[test]
    fn path_reports_protection_and_watermark_when_set() {
        let mut app = app_with(&["x"]);
        app.doc_protection = Some("read-only".to_string());
        app.doc_watermark = Some("CONFIDENTIAL".to_string());
        let r = path_info(&app);
        assert_eq!(r.get_str("protection"), Some("read-only"));
        assert_eq!(r.get_str("watermark"), Some("CONFIDENTIAL"));
    }

    #[test]
    fn replace_all_replaces_every_occurrence_case_insensitive_by_default() {
        let mut app = app_with(&["Foo and foo", "another foo"]);
        let r = replace_all(
            &mut app,
            &args(vec![
                ("query", Json::Str("foo".into())),
                ("text", Json::Str("X".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("replaced"), Some(3));
        assert_eq!(paras(&app), vec!["X and X", "another X"]);
        assert!(app.modified);
    }

    #[test]
    fn replace_all_case_sensitive_flag_is_respected() {
        let mut app = app_with(&["Foo and foo"]);
        let r = replace_all(
            &mut app,
            &args(vec![
                ("query", Json::Str("foo".into())),
                ("text", Json::Str("X".into())),
                ("case_sensitive", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("replaced"), Some(1));
        assert_eq!(paras(&app), vec!["Foo and X"]);
    }

    #[test]
    fn replace_all_with_no_matches_leaves_no_tracks() {
        // Unlike doc.replace-range (bounds-checked, never a genuine no-op),
        // doc.replace-all can legitimately match nothing. A no-match call
        // must not look like an edit: no modified flag, no content change,
        // and (per agent::replace_all's doc comment) no undo checkpoint was
        // even pushed, so there is nothing to undo.
        let mut app = app_with(&["hello world"]);
        let r = replace_all(
            &mut app,
            &args(vec![
                ("query", Json::Str("xyz".into())),
                ("text", Json::Str("BAR".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("replaced"), Some(0));
        assert!(
            !app.modified,
            "a no-match replace-all must not mark modified"
        );
        assert_eq!(paras(&app), vec!["hello world"]);
        assert!(
            !app.editor.undo(),
            "no checkpoint was pushed, so there is nothing to undo"
        );
    }

    #[test]
    fn replace_all_is_undoable_in_exactly_the_reported_undo_steps() {
        // agent::replace_all always reports 1 undo step when it replaces
        // anything (see its doc comment) — pin that a single Editor::undo
        // restores the pre-replace text, regardless of match count.
        let mut app = app_with(&["a foo b foo c"]);
        replace_all(
            &mut app,
            &args(vec![
                ("query", Json::Str("foo".into())),
                ("text", Json::Str("BAR".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["a BAR b BAR c"]);
        assert!(app.editor.undo());
        assert_eq!(paras(&app), vec!["a foo b foo c"]);
    }

    #[test]
    fn undo_reports_done_false_on_fresh_doc_and_true_after_an_edit() {
        let mut app = app_with(&["A"]);
        let r = undo(&mut app);
        assert_eq!(r.get("done").unwrap().as_bool(), Some(false));
        assert!(!app.modified, "a no-op undo must not mark modified");

        replace_all(
            &mut app,
            &args(vec![
                ("query", Json::Str("A".into())),
                ("text", Json::Str("B".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["B"]);
        let r = undo(&mut app);
        assert_eq!(r.get("done").unwrap().as_bool(), Some(true));
        assert_eq!(paras(&app), vec!["A"]);
    }

    #[test]
    fn redo_reports_done_false_without_a_prior_undo_and_true_after_one() {
        let mut app = app_with(&["A"]);
        let r = redo(&mut app);
        assert_eq!(r.get("done").unwrap().as_bool(), Some(false));
        assert!(!app.modified, "a no-op redo must not mark modified");

        replace_all(
            &mut app,
            &args(vec![
                ("query", Json::Str("A".into())),
                ("text", Json::Str("B".into())),
            ]),
        )
        .unwrap();
        undo(&mut app);
        assert_eq!(paras(&app), vec!["A"]);
        let r = redo(&mut app);
        assert_eq!(r.get("done").unwrap().as_bool(), Some(true));
        assert_eq!(paras(&app), vec!["B"]);
        // The redo stack is empty again.
        let r = redo(&mut app);
        assert_eq!(r.get("done").unwrap().as_bool(), Some(false));
        assert!(
            app.modified,
            "modified stays true from the earlier real edit; a later no-op redo doesn't clear it"
        );
    }

    #[test]
    fn export_pdf_writes_nonempty_file_and_reports_absolutized_path() {
        let tmp = std::env::temp_dir().join(format!("docxy_ctl_pdf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("out.pdf");

        let app = app_with(&["hello world"]);
        let r = export_pdf(
            &app,
            &args(vec![("path", Json::Str(out.display().to_string()))]),
        )
        .unwrap();
        let reported = r.get_str("path").unwrap();
        assert!(
            Path::new(reported).is_absolute(),
            "reply path must be absolutized: {reported}"
        );
        let bytes = std::fs::read(&out).expect("pdf written");
        assert!(!bytes.is_empty(), "exported PDF must be nonempty");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn export_pdf_creates_missing_parent_directories() {
        // Matches ctlcore::client::new_file's create_dir_all step: a target
        // path whose parent doesn't exist yet must still succeed, not error.
        let tmp = std::env::temp_dir().join(format!("docxy_ctl_pdf_mkdir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let out = tmp.join("newdir").join("nested").join("x.pdf");
        assert!(!tmp.exists(), "precondition: nothing exists yet");

        let app = app_with(&["hello"]);
        let r = export_pdf(
            &app,
            &args(vec![("path", Json::Str(out.display().to_string()))]),
        )
        .unwrap();
        let expected = out.display().to_string();
        assert_eq!(r.get_str("path"), Some(expected.as_str()));
        assert!(out.exists(), "parent dirs must be created on demand");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn export_pdf_refuses_to_overwrite_an_existing_file() {
        let tmp =
            std::env::temp_dir().join(format!("docxy_ctl_pdf_overwrite_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("existing.pdf");
        std::fs::write(&out, b"OLD").unwrap();

        let app = app_with(&["hello"]);
        let err = export_pdf(
            &app,
            &args(vec![("path", Json::Str(out.display().to_string()))]),
        )
        .unwrap_err();
        assert!(err.starts_with("already exists: "), "{err}");
        assert_eq!(
            std::fs::read(&out).unwrap(),
            b"OLD",
            "existing file untouched"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dispatch_routes_new_mutating_verbs() {
        let tmp =
            std::env::temp_dir().join(format!("docxy_ctl_dispatch_pdf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("dispatch.pdf");

        let mut app = app_with(&["hello foo"]);
        assert!(
            dispatch(
                &mut app,
                "doc.replace-all",
                &args(vec![
                    ("query", Json::Str("foo".into())),
                    ("text", Json::Str("bar".into())),
                ]),
            )
            .is_ok()
        );
        assert!(dispatch(&mut app, "doc.undo", &Json::Null).is_ok());
        assert!(dispatch(&mut app, "doc.redo", &Json::Null).is_ok());
        assert!(
            dispatch(
                &mut app,
                "doc.export-pdf",
                &args(vec![("path", Json::Str(out.display().to_string()))]),
            )
            .is_ok()
        );
        assert!(out.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dispatch_routes_new_read_verbs() {
        let mut app = app_with(&["hello"]);
        for verb in [
            "doc.export",
            "doc.comments",
            "doc.notes",
            "doc.header",
            "doc.footer",
            "doc.metadata",
            "doc.stats",
        ] {
            let a = if verb == "doc.export" {
                args(vec![("format", Json::Str("text".into()))])
            } else {
                Json::Null
            };
            assert!(dispatch(&mut app, verb, &a).is_ok(), "{verb}");
        }
    }

    #[test]
    fn dispatch_routes_and_reports_unknown() {
        let mut app = app_with(&["A"]);
        assert!(dispatch(&mut app, "doc.path", &Json::Null).is_ok());
        let err = dispatch(&mut app, "doc.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }
}
