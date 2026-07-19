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
//! | `doc.replace-range` | `{start, end?, text, markdown?}` | `{replaced, total}` |
//! | `doc.insert` | `{at, text, markdown?}` | `{total}` |
//! | `doc.append` | `{text, markdown?}` | `{total}` |
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
//! | `doc.format` | `{start, end?, patch}` | `{formatted}` — one undo checkpoint; `patch` keys: `bold`/`italic`/`underline`/`strike`/`color`/`highlight`/`font`/`size` (≥1 required), set-to-value semantics |
//! | `doc.set-style` | `{start, end?, style?, align?}` | `{styled}` — one undo checkpoint; ≥1 of `style`/`align` required |

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
        "doc.format" => format(app, args),
        "doc.set-style" => set_style(app, args),
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
        if matches!(
            verb,
            "doc.replace-range" | "doc.insert" | "doc.append" | "doc.format" | "doc.set-style"
        ) {
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
    // checkpoint count `agent::replace_range`/`replace_range_blocks` also
    // report; its wire reply stays exactly `{replaced, total}` either way.
    let replaced = if markdown_flag(args) {
        // Validate the target range BEFORE `prepare_markdown_blocks` does any
        // package mutation (ensure_list/ensure_styles): a rejected
        // out-of-bounds/non-paragraph range must leave the package exactly as
        // it was, not permanently gain a numbering/styles part nothing ends
        // up using. See `agent::validate_replace_range`'s doc comment.
        agent::validate_replace_range(&app.editor.doc, start, end)?;
        let blocks = prepare_markdown_blocks(app, text)?;
        agent::replace_range_blocks(&mut app.editor, start, end, blocks)?.0
    } else {
        agent::replace_range(&mut app.editor, start, end, text)?.0
    };
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
    if markdown_flag(args) {
        // Same validate-before-mutate ordering as `replace_range` above.
        agent::validate_insert_at(&app.editor.doc, at)?;
        let blocks = prepare_markdown_blocks(app, text)?;
        agent::insert_blocks(&mut app.editor, at, blocks)?;
    } else {
        agent::insert(&mut app.editor, at, text)?;
    }
    finish_edit(app);
    Ok(Json::obj(vec![(
        "total",
        Json::Num(app.editor.doc.body.len() as f64),
    )]))
}

fn append(app: &mut App, args: &Json) -> Result<Json, String> {
    let text = args.get_str("text").ok_or("doc.append needs 'text'")?;
    if markdown_flag(args) {
        let blocks = prepare_markdown_blocks(app, text)?;
        agent::append_blocks(&mut app.editor, blocks);
    } else {
        agent::append(&mut app.editor, text);
    }
    finish_edit(app);
    Ok(Json::obj(vec![(
        "total",
        Json::Num(app.editor.doc.body.len() as f64),
    )]))
}

/// Whether a `doc.insert`/`doc.replace-range`/`doc.append` call opted into
/// Markdown-formatted splicing via the optional `markdown` arg (default
/// `false` — byte-identical to the plain-text behavior).
fn markdown_flag(args: &Json) -> bool {
    args.get("markdown")
        .and_then(Json::as_bool)
        .unwrap_or(false)
}

/// Parse `text` as Markdown into blocks ready to splice, ensuring this
/// package's `Package` carries numbering/style definitions for any list or
/// style the parsed content references before the caller splices it in.
///
/// **List numbering**: [`docxcore::markdown::from_markdown`] always marks
/// bullet items `numId` 1 and ordered items `numId` 2 — ids fixed for a
/// *fresh* markdown package (see `new_markdown_package`), which can collide
/// with a numbering id an existing `.docx` already defines for something
/// else. So this remaps: [`agent::referenced_numbering_kinds`] detects which
/// kind(s) are actually referenced, then for each one this calls
/// [`docxcore::package::Package::ensure_list`] (the same call the
/// Bullets/Numbering ribbon commands use), which returns a reserved high id
/// unlikely to collide with the document's own lists, and rewrites the parsed
/// blocks' `numId` to match — then reparses the package's numbering so the
/// live view picks up any newly created part.
///
/// **Paragraph styles**: unlike numbering ids, `HeadingN`/`Quote`/`SourceCode`
/// are fixed, well-known names — there's nothing to remap, only to ensure
/// present. [`agent::referenced_style_ids`] finds which of them the parsed
/// blocks reference and [`docxcore::package::Package::ensure_styles`] defines
/// exactly those that the target package doesn't already have, leaving any
/// pre-existing same-named style (e.g. a third-party document's own
/// `Heading1`) untouched. Without this, `<w:pStyle w:val="HeadingN"/>` (or
/// `Quote`/`SourceCode`) referencing an undefined style renders as plain
/// Normal text in Word.
///
/// **Ordering**: this only ever ADDS package parts/definitions, never
/// removes or rewrites unrelated ones — but callers must still validate the
/// splice position (`agent::validate_insert_at`/`validate_replace_range`)
/// BEFORE calling this, so a call that's ultimately going to be rejected for
/// bad bounds never leaves the mutation behind. See those functions' doc
/// comments.
fn prepare_markdown_blocks(app: &mut App, text: &str) -> Result<Vec<Block>, String> {
    let mut blocks = agent::parse_markdown_blocks(text)?;

    let (needs_bullet, needs_decimal) = agent::referenced_numbering_kinds(&blocks);
    if needs_bullet || needs_decimal {
        let bullet_id = needs_bullet.then(|| app.pkg.ensure_list(true));
        let decimal_id = needs_decimal.then(|| app.pkg.ensure_list(false));
        for b in blocks.iter_mut() {
            if let Block::Paragraph(p) = b {
                if let (Some(id), true) = (bullet_id, p.props.num_id == Some(1)) {
                    p.props.num_id = Some(id);
                }
                if let (Some(id), true) = (decimal_id, p.props.num_id == Some(2)) {
                    p.props.num_id = Some(id);
                }
            }
        }
        app.reparse_numbering();
    }

    let style_ids = agent::referenced_style_ids(&blocks);
    if !style_ids.is_empty() {
        app.pkg.ensure_styles(&style_ids);
    }

    Ok(blocks)
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

/// Build `agent::RunPatch`'s wire pairs from the `patch` object's own JSON
/// values — docxcore stays JSON-free, so scalars are stringified here
/// (`true`/`false` for booleans, the raw text for strings, the number's text
/// for numbers). Mirrors xlsxy's `control.rs::patch_pairs` for
/// `cell.format`, byte-for-byte (a sibling crate this one can't depend on).
fn patch_pairs(patch: &Json) -> Result<Vec<(String, String)>, String> {
    let Json::Obj(pairs) = patch else {
        return Err("doc.format needs a 'patch' object".to_string());
    };
    Ok(pairs
        .iter()
        .map(|(k, v)| {
            let text = match v {
                Json::Str(s) => s.clone(),
                Json::Bool(b) => b.to_string(),
                Json::Num(n) => n.to_string(),
                Json::Null | Json::Arr(_) | Json::Obj(_) => String::new(),
            };
            (k.clone(), text)
        })
        .collect())
}

/// `doc.format {start, end?, patch}` → `{formatted}`. `patch` is the same
/// wire shape as `cell.format`'s (a JSON object, ≥1 key), routed through
/// `agent::RunPatch::parse` for validation/typing, then applied over the
/// block range as ONE undo checkpoint via `agent::format_range` — see that
/// function's doc comment for the set-to-value semantics.
fn format(app: &mut App, args: &Json) -> Result<Json, String> {
    let start = args
        .get_usize("start")
        .ok_or("doc.format needs a 'start' index")?;
    let end = args.get_usize("end").unwrap_or(start);
    let patch_arg = args.get("patch").ok_or("doc.format needs a 'patch'")?;
    let pairs = patch_pairs(patch_arg)?;
    let patch = agent::RunPatch::parse(&pairs)?;
    let formatted = agent::format_range(&mut app.editor, start, end, &patch)?;
    finish_edit(app);
    Ok(Json::obj(vec![("formatted", Json::Num(formatted as f64))]))
}

/// Parse `left`/`center`/`right`/`justify` into `docxcore::model::Align` for
/// `doc.set-style`'s `align` key — the wire vocabulary matching what
/// `Editor::set_align` accepts. Unlike gridcore's cell-format `Align` (no
/// `Justify` variant), docxcore's `Align` enum has one, so it's included
/// here per the design spec's "include it unless the core enum lacks it"
/// rule.
fn parse_align(s: &str) -> Result<docxcore::model::Align, String> {
    use docxcore::model::Align;
    match s {
        "left" => Ok(Align::Left),
        "center" => Ok(Align::Center),
        "right" => Ok(Align::Right),
        "justify" => Ok(Align::Justify),
        other => Err(format!(
            "bad align '{other}' (want left/center/right/justify)"
        )),
    }
}

/// `doc.set-style {start, end?, style?, align?}` → `{styled}`. At least one
/// of `style`/`align` is required. `style` is one of the Wave-2 markdown
/// paragraph style ids (`Heading1`-`Heading6`/`Quote`/`SourceCode`) or
/// `Normal` (clears the paragraph style). Applying a markdown-set id ensures
/// its definition in this package's `word/styles.xml` first (Wave-2's
/// `ensure_styles`, strictly additive) — but only AFTER
/// `agent::validate_set_style_range` confirms the call would otherwise
/// succeed (Wave-2's validate-before-mutate ordering, matching
/// `prepare_markdown_blocks`'s doc comment above), and never for `Normal` or
/// an align-only call.
fn set_style(app: &mut App, args: &Json) -> Result<Json, String> {
    let start = args
        .get_usize("start")
        .ok_or("doc.set-style needs a 'start' index")?;
    let end = args.get_usize("end").unwrap_or(start);
    let style = args.get_str("style");
    let align = match args.get_str("align") {
        Some(a) => Some(parse_align(a)?),
        None => None,
    };

    agent::validate_set_style_range(&app.editor.doc, start, end, style, align)?;
    if let Some(s) = style {
        if s != "Normal" {
            app.pkg.ensure_styles(&[s]);
        }
    }
    let styled = agent::set_style_range(&mut app.editor, start, end, style, align)?;
    finish_edit(app);
    Ok(Json::obj(vec![("styled", Json::Num(styled as f64))]))
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
    use docxcore::package::{load_package, new_package, save_package};

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
    fn markdown_flag_absent_or_false_matches_plain_text_insert() {
        let mut app_plain = app_with(&["A", "B", "C"]);
        insert(
            &mut app_plain,
            &args(vec![
                ("at", Json::Num(1.0)),
                ("text", Json::Str("X".into())),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app_plain), vec!["A", "X", "B", "C"]);

        // Explicit `markdown:false` is byte-identical to the flag being absent.
        let mut app_flag_false = app_with(&["A", "B", "C"]);
        insert(
            &mut app_flag_false,
            &args(vec![
                ("at", Json::Num(1.0)),
                ("text", Json::Str("X".into())),
                ("markdown", Json::Bool(false)),
            ]),
        )
        .unwrap();
        assert_eq!(paras(&app_flag_false), paras(&app_plain));
    }

    #[test]
    fn markdown_insert_round_trips_table_list_and_link() {
        let mut app = app_with(&["Existing"]);
        let at = app.editor.doc.body.len();
        let md = "# Notes\n\n- item one\n- item two\n\n\
                   | A | B |\n| --- | --- |\n| 1 | 2 |\n\n\
                   See [docs](https://example.com).";
        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(at as f64)),
                ("text", Json::Str(md.into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert!(app.modified);
        // The original paragraph is untouched, ahead of the spliced blocks.
        assert_eq!(app.editor.doc.body[0].plain_text(), "Existing");

        let out = export(&app, &args(vec![("format", Json::Str("markdown".into()))])).unwrap();
        let text = out.get_str("text").unwrap();
        assert!(text.contains("# Notes"), "heading missing: {text}");
        assert!(
            text.contains("item one") && text.contains("item two"),
            "list items missing: {text}"
        );
        assert!(text.contains("| A | B |"), "table row missing: {text}");
        assert!(text.contains("| 1 | 2 |"), "table row missing: {text}");
        assert!(
            text.contains("[docs](https://example.com)"),
            "link missing: {text}"
        );
    }

    #[test]
    fn empty_markdown_insert_errors_and_leaves_doc_unmodified() {
        let mut app = app_with(&["A", "B"]);
        let err = insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(1.0)),
                ("text", Json::Str("   \n".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "empty markdown");
        assert_eq!(paras(&app), vec!["A", "B"]);
        assert!(!app.modified, "an errored splice must not mark modified");
        assert!(
            !app.editor.undo(),
            "nothing was spliced, so nothing was checkpointed"
        );
    }

    #[test]
    fn dispatch_empty_markdown_via_replace_range_is_an_error_and_doc_stays_clean() {
        let mut app = app_with(&["A"]);
        let err = dispatch(
            &mut app,
            "doc.replace-range",
            &args(vec![
                ("start", Json::Num(0.0)),
                ("text", Json::Str("".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "empty markdown");
        assert!(!app.modified);
        assert_eq!(paras(&app), vec!["A"]);
    }

    #[test]
    fn markdown_list_ensures_numbering_and_remaps_off_the_bare_ids() {
        let mut app = app_with(&["Existing"]);
        assert!(
            !app.pkg.part_names().contains(&"word/numbering.xml"),
            "fixture must start without numbering"
        );
        append(
            &mut app,
            &args(vec![
                (
                    "text",
                    Json::Str("- one\n- two\n\n1. first\n2. second".into()),
                ),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap();
        assert!(
            app.pkg.part_names().contains(&"word/numbering.xml"),
            "numbering part must be created on demand: {:?}",
            app.pkg.part_names()
        );
        let num_ids: Vec<i32> = app
            .editor
            .doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => p.props.num_id,
                _ => None,
            })
            .collect();
        // A bullet list (2 items) and an ordered list (2 items) were both spliced.
        assert_eq!(num_ids.len(), 4, "{num_ids:?}");
        // Remapped away from markdown's bare 1/2 (which `ensure_list`'s reserved
        // ids exist precisely to avoid colliding with) onto real definitions.
        assert!(
            num_ids.iter().all(|&id| id != 1 && id != 2),
            "list paragraphs must be remapped onto ensure_list's ids: {num_ids:?}"
        );
    }

    #[test]
    fn markdown_nested_list_ensures_all_indent_levels_not_just_the_top_one() {
        // Regression test for the bug `Package::ensure_list` used to define
        // only `ilvl=0` — a nested item spliced into an EXISTING (non-
        // markdown-created) package would reference an undefined level,
        // rendering a stray decimal marker in the TUI / no marker in Word.
        let mut app = app_with(&["Existing"]);
        append(
            &mut app,
            &args(vec![
                ("text", Json::Str("- a\n  - b".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap();
        let numbering =
            String::from_utf8_lossy(app.pkg.part("word/numbering.xml").unwrap()).into_owned();
        assert!(
            numbering.contains("w:ilvl=\"1\""),
            "nested item's level must be defined: {numbering}"
        );
        // The nested paragraph's own ilvl survived the splice untouched
        // (only numId gets remapped, never ilvl).
        let nested_ilvl = app
            .editor
            .doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) if p.props.num_id.is_some() => Some(p.props.ilvl),
                _ => None,
            })
            .max();
        assert_eq!(nested_ilvl, Some(1), "{:?}", app.editor.doc.body);
    }

    #[test]
    fn out_of_bounds_markdown_list_insert_leaves_the_package_untouched() {
        // Regression guard: `prepare_markdown_blocks`'s ensure_list/ensure_styles
        // mutation must never run before the splice position itself is known
        // to be valid — otherwise a rejected out-of-bounds call leaves behind
        // a numbering part a later, unrelated `doc.save` would persist even
        // though nothing was actually inserted.
        let mut app = app_with(&["A"]);
        let before: Vec<String> = app.pkg.part_names().into_iter().map(String::from).collect();
        assert!(!before.iter().any(|n| n == "word/numbering.xml"));

        let err = insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(99.0)),
                ("text", Json::Str("- item".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("out of bounds"), "{err}");

        let after: Vec<String> = app.pkg.part_names().into_iter().map(String::from).collect();
        assert_eq!(
            after, before,
            "a rejected splice must not mutate the package"
        );
        assert!(!app.modified);
        assert_eq!(paras(&app), vec!["A"]);
    }

    #[test]
    fn markdown_heading_into_a_fresh_package_ensures_heading1_and_persists_it() {
        let mut app = app_with(&["Existing"]);
        let styles_before =
            String::from_utf8_lossy(app.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(!styles_before.contains("Heading1"), "{styles_before}");

        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(0.0)),
                ("text", Json::Str("# Title".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap();

        let styles = String::from_utf8_lossy(app.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(styles.contains("w:styleId=\"Heading1\""), "{styles}");

        // Persists through save/reload, with the document still referencing
        // it — i.e. it renders styled (bold/large) once Word resolves the
        // style, not as plain Normal text. `App::save` syncs `pkg.document`
        // from the live `editor.doc` before calling `save_package`; mirror
        // that here rather than driving a real file write.
        app.pkg.document = app.editor.doc.clone();
        let bytes = save_package(&app.pkg);
        let reloaded = load_package(&bytes).expect("reload");
        let reloaded_styles =
            String::from_utf8_lossy(reloaded.part("word/styles.xml").unwrap()).into_owned();
        assert!(
            reloaded_styles.contains("w:styleId=\"Heading1\""),
            "{reloaded_styles}"
        );
        let doc_xml =
            String::from_utf8_lossy(reloaded.part("word/document.xml").unwrap()).into_owned();
        assert!(doc_xml.contains("w:pStyle w:val=\"Heading1\""), "{doc_xml}");
    }

    #[test]
    fn markdown_heading_write_leaves_a_pre_existing_heading1_byte_unchanged() {
        let mut app = app_with(&["Existing"]);
        // A third-party Heading1 already defined, visibly different from ours.
        let custom = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="ThirdPartyHeading"/></w:style></w:styles>"#;
        app.pkg
            .set_part("word/styles.xml", custom.as_bytes().to_vec());

        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(0.0)),
                ("text", Json::Str("# Title".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap();

        let styles = String::from_utf8_lossy(app.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(
            styles.contains(
                r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="ThirdPartyHeading"/></w:style>"#
            ),
            "{styles}"
        );
        assert_eq!(
            styles.matches("w:styleId=\"Heading1\"").count(),
            1,
            "must not append a second, competing Heading1: {styles}"
        );
    }

    #[test]
    fn non_style_referencing_markdown_leaves_styles_xml_untouched() {
        let mut app = app_with(&["Existing"]);
        let before = app.pkg.part("word/styles.xml").unwrap().to_vec();

        insert(
            &mut app,
            &args(vec![
                ("at", Json::Num(0.0)),
                ("text", Json::Str("plain paragraph, **bold** even".into())),
                ("markdown", Json::Bool(true)),
            ]),
        )
        .unwrap();

        assert_eq!(
            app.pkg.part("word/styles.xml").unwrap(),
            before.as_slice(),
            "markdown with no pStyle-referencing construct must not touch styles.xml"
        );
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

    // -------------------------------------------------------------------
    // doc.format
    // -------------------------------------------------------------------

    fn patch_arg(pairs: Vec<(&str, Json)>) -> Json {
        args(vec![("patch", Json::obj(pairs))])
    }

    fn run_bold_flags(app: &App, block: usize) -> Vec<bool> {
        let Block::Paragraph(p) = &app.editor.doc.body[block] else {
            panic!("expected a paragraph")
        };
        p.content
            .iter()
            .map(|i| match i {
                docxcore::model::Inline::Run(r) => r.props.bold,
                other => panic!("expected a run: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn format_bold_over_range_sets_every_run_bold() {
        let mut app = app_with(&["A", "B"]);
        let mut a = patch_arg(vec![("bold", Json::Bool(true))]);
        if let Json::Obj(pairs) = &mut a {
            pairs.push(("start".to_string(), Json::Num(0.0)));
            pairs.push(("end".to_string(), Json::Num(1.0)));
        }
        let r = format(&mut app, &a).unwrap();
        assert_eq!(r.get_usize("formatted"), Some(2));
        assert_eq!(run_bold_flags(&app, 0), vec![true]);
        assert_eq!(run_bold_flags(&app, 1), vec![true]);
        assert!(app.modified);
    }

    #[test]
    fn format_patch_needs_at_least_one_key() {
        let mut app = app_with(&["A"]);
        let err = format(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("patch", Json::obj(vec![])),
            ]),
        )
        .unwrap_err();
        assert_eq!(err, "patch needs at least one key");
        assert!(!app.modified);
    }

    #[test]
    fn format_unknown_patch_key_is_named() {
        let mut app = app_with(&["A"]);
        let mut a = patch_arg(vec![("frobnicate", Json::Bool(true))]);
        if let Json::Obj(pairs) = &mut a {
            pairs.push(("start".to_string(), Json::Num(0.0)));
        }
        let err = format(&mut app, &a).unwrap_err();
        assert!(err.contains("frobnicate"), "{err}");
        assert!(!app.modified);
    }

    #[test]
    fn format_bad_color_is_rejected() {
        let mut app = app_with(&["A"]);
        let mut a = patch_arg(vec![("color", Json::Str("red".into()))]);
        if let Json::Obj(pairs) = &mut a {
            pairs.push(("start".to_string(), Json::Num(0.0)));
        }
        let err = format(&mut app, &a).unwrap_err();
        assert!(err.contains("color"), "{err}");
        assert!(!app.modified);
    }

    #[test]
    fn format_needs_a_patch_object() {
        let mut app = app_with(&["A"]);
        let err = format(&mut app, &args(vec![("start", Json::Num(0.0))])).unwrap_err();
        assert!(err.contains("patch"), "{err}");
    }

    #[test]
    fn format_is_one_undo_group() {
        let mut app = app_with(&["A"]);
        let before = app.editor.doc.clone();
        let mut a = patch_arg(vec![
            ("bold", Json::Bool(true)),
            ("italic", Json::Bool(true)),
            ("color", Json::Str("#00FF00".into())),
        ]);
        if let Json::Obj(pairs) = &mut a {
            pairs.push(("start".to_string(), Json::Num(0.0)));
        }
        format(&mut app, &a).unwrap();
        assert_ne!(app.editor.doc, before);
        assert!(app.editor.undo());
        assert_eq!(
            app.editor.doc, before,
            "one undo restores exact prior props"
        );
        assert!(!app.editor.undo());
    }

    #[test]
    fn dispatch_routes_doc_format() {
        let mut app = app_with(&["A"]);
        let mut a = patch_arg(vec![("bold", Json::Bool(true))]);
        if let Json::Obj(pairs) = &mut a {
            pairs.push(("start".to_string(), Json::Num(0.0)));
        }
        assert!(dispatch(&mut app, "doc.format", &a).is_ok());
        assert_eq!(run_bold_flags(&app, 0), vec![true]);
    }

    // -------------------------------------------------------------------
    // doc.set-style
    // -------------------------------------------------------------------

    #[test]
    fn set_style_needs_style_or_align() {
        let mut app = app_with(&["A"]);
        let err = set_style(&mut app, &args(vec![("start", Json::Num(0.0))])).unwrap_err();
        assert_eq!(err, "set-style needs 'style' or 'align'");
        assert!(!app.modified);
    }

    #[test]
    fn set_style_unknown_style_lists_the_accepted_set() {
        let mut app = app_with(&["A"]);
        let err = set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("style", Json::Str("Bogus".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("Bogus"), "{err}");
        for id in docxcore::agent::MARKDOWN_PARAGRAPH_STYLE_IDS {
            assert!(err.contains(*id), "{err} missing {id}");
        }
        assert!(err.contains("Normal"), "{err}");
        assert!(!app.modified);
    }

    #[test]
    fn set_style_bad_align_is_rejected() {
        let mut app = app_with(&["A"]);
        let err = set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("align", Json::Str("middle".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("middle"), "{err}");
    }

    #[test]
    fn set_style_heading1_ensures_the_style_part_on_a_bare_package() {
        let mut app = app_with(&["Title"]);
        let styles_before =
            String::from_utf8_lossy(app.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(!styles_before.contains("Heading1"), "{styles_before}");

        let r = set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("style", Json::Str("Heading1".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("styled"), Some(1));

        let styles = String::from_utf8_lossy(app.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(styles.contains("w:styleId=\"Heading1\""), "{styles}");
        assert!(app.modified);
    }

    #[test]
    fn set_style_normal_does_not_ensure_styles() {
        let mut app = app_with(&["A"]);
        let before = app.pkg.part("word/styles.xml").unwrap().to_vec();

        set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("style", Json::Str("Normal".into())),
            ]),
        )
        .unwrap();

        assert_eq!(
            app.pkg.part("word/styles.xml").unwrap(),
            before.as_slice(),
            "Normal must not touch styles.xml"
        );
    }

    #[test]
    fn set_style_align_only_does_not_ensure_styles() {
        let mut app = app_with(&["A"]);
        let before = app.pkg.part("word/styles.xml").unwrap().to_vec();

        let r = set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("align", Json::Str("center".into())),
            ]),
        )
        .unwrap();
        assert_eq!(r.get_usize("styled"), Some(1));

        assert_eq!(
            app.pkg.part("word/styles.xml").unwrap(),
            before.as_slice(),
            "an align-only call must not touch styles.xml"
        );
        assert!(app.modified);
    }

    #[test]
    fn set_style_out_of_bounds_leaves_the_package_untouched() {
        let mut app = app_with(&["A"]);
        let before = app.pkg.part("word/styles.xml").unwrap().to_vec();
        let err = set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("end", Json::Num(9.0)),
                ("style", Json::Str("Heading1".into())),
            ]),
        )
        .unwrap_err();
        assert!(err.contains("out of bounds"), "{err}");
        assert_eq!(app.pkg.part("word/styles.xml").unwrap(), before.as_slice());
        assert!(!app.modified);
    }

    #[test]
    fn dispatch_routes_doc_set_style() {
        let mut app = app_with(&["A"]);
        assert!(
            dispatch(
                &mut app,
                "doc.set-style",
                &args(vec![
                    ("start", Json::Num(0.0)),
                    ("style", Json::Str("Quote".into())),
                ]),
            )
            .is_ok()
        );
        let Block::Paragraph(p) = &app.editor.doc.body[0] else {
            panic!()
        };
        assert_eq!(p.props.style_id.as_deref(), Some("Quote"));
    }

    #[test]
    fn format_and_set_style_round_trip_through_markdown_export() {
        let mut app = app_with(&["Title", "body text"]);
        set_style(
            &mut app,
            &args(vec![
                ("start", Json::Num(0.0)),
                ("style", Json::Str("Heading1".into())),
            ]),
        )
        .unwrap();
        let mut a = patch_arg(vec![("bold", Json::Bool(true))]);
        if let Json::Obj(pairs) = &mut a {
            pairs.push(("start".to_string(), Json::Num(1.0)));
        }
        format(&mut app, &a).unwrap();

        let out = export(&app, &args(vec![("format", Json::Str("markdown".into()))])).unwrap();
        let text = out.get_str("text").unwrap();
        assert!(text.contains("# Title"), "heading missing: {text}");
        assert!(text.contains("**body text**"), "bold missing: {text}");
    }
}
