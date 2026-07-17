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
//! | `doc.path` | — | `{path, format, modified, blocks}` |
//! | `doc.outline` | — | `{headings:[{index, level, text}]}` |
//! | `doc.read` | `{start?, end?, range?}` | `{total, start, end, text, blocks:[…]}` |
//! | `doc.find` | `{query, case_sensitive?}` | `{query, count, matches:[…]}` |
//! | `doc.replace-range` | `{start, end?, text}` | `{replaced, total}` |
//! | `doc.insert` | `{at, text}` | `{total}` |
//! | `doc.append` | `{text}` | `{total}` |
//! | `doc.save` | — | `{path, …}` |
//! | `doc.reload` | — | `{path, …}` |
//! | `doc.open` | `{path}` | `{path, …}` |

use crate::{App, DocFormat};
use ctlcore::json::Json;
use docxcore::editor::{Caret, Clip};
use docxcore::model::Block;
use std::path::Path;

/// The directory where docxy publishes its control discovery files, alongside
/// its other config: `<config>/docxy/ctl` (`%APPDATA%` on Windows, else
/// `$XDG_CONFIG_HOME` / `~/.config`). An agent reads `<dir>/<instance>.json` to
/// find the port + token.
pub fn control_dir() -> Option<std::path::PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })
    }?;
    Some(base.join("docxy").join("ctl"))
}

/// This editor's control instance name. Inside an agwinterm pane it is
/// `docxy-<AGWINTERM_SESSION_ID>` — the pane id an agent sees in `agwintermctl
/// tree`, so it can address exactly this editor — otherwise `docxy-<pid>`.
pub fn instance_name() -> String {
    match std::env::var("AGWINTERM_SESSION_ID") {
        Ok(id) if !id.is_empty() => format!("docxy-{id}"),
        _ => format!("docxy-{}", std::process::id()),
    }
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
    Json::obj(vec![
        ("path", Json::Str(app.path.clone())),
        ("format", Json::Str(fmt.to_string())),
        ("modified", Json::Bool(app.modified)),
        ("blocks", Json::Num(app.editor.doc.body.len() as f64)),
    ])
}

fn outline(app: &App) -> Json {
    let mut items = Vec::new();
    for (i, b) in app.editor.doc.body.iter().enumerate() {
        if let Block::Paragraph(p) = b {
            if let Some(level) = p.props.heading_level {
                items.push(Json::obj(vec![
                    ("index", Json::Num(i as f64)),
                    ("level", Json::Num(level as f64)),
                    ("text", Json::Str(p.plain_text())),
                ]));
            }
        }
    }
    Json::obj(vec![("headings", Json::Arr(items))])
}

fn read(app: &App, args: &Json) -> Result<Json, String> {
    let body = &app.editor.doc.body;
    let n = body.len();
    let (start, end) = range_args(args, n)?;
    let mut arr = Vec::new();
    let mut joined = String::new();
    for i in start..=end {
        let b = &body[i];
        let text = b.plain_text();
        if i > start {
            joined.push_str("\n\n");
        }
        joined.push_str(&text);
        let mut fields = vec![
            ("index", Json::Num(i as f64)),
            ("kind", Json::Str(block_kind(b).to_string())),
            ("text", Json::Str(text)),
        ];
        if let Block::Paragraph(p) = b {
            if let Some(level) = p.props.heading_level {
                fields.push(("heading", Json::Num(level as f64)));
            }
        }
        arr.push(Json::obj(fields));
    }
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
    for m in app.editor.find_all(query, case_sensitive) {
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

// ---------------------------------------------------------------------------
// Mutating verbs (undoable, via the Editor)
// ---------------------------------------------------------------------------

fn replace_range(app: &mut App, args: &Json) -> Result<Json, String> {
    let n = app.editor.doc.body.len();
    let start = args
        .get_usize("start")
        .ok_or("doc.replace-range needs a 'start' index")?;
    let end = args.get_usize("end").unwrap_or(start);
    let text = args
        .get_str("text")
        .ok_or("doc.replace-range needs 'text'")?;
    bounds(start, end, n)?;
    require_para(&app.editor.doc.body, start)?;
    require_para(&app.editor.doc.body, end)?;

    // Select paragraphs [start..=end] (anchor at the head, caret at the true end
    // of the last, in the editor's own offset units) then paste — `paste` deletes
    // the selection first, so this is one undoable replace.
    app.editor.anchor = None;
    app.editor.caret = Caret::top(end, 0);
    app.editor.move_end();
    app.editor.anchor = Some(Caret::top(start, 0));
    app.editor.paste(&Clip::from_text(text));
    finish_edit(app);

    Ok(Json::obj(vec![
        ("replaced", Json::Num((end - start + 1) as f64)),
        ("total", Json::Num(app.editor.doc.body.len() as f64)),
    ]))
}

fn insert(app: &mut App, args: &Json) -> Result<Json, String> {
    let n = app.editor.doc.body.len();
    let at = args.get_usize("at").ok_or("doc.insert needs an 'at' index")?;
    let text = args.get_str("text").ok_or("doc.insert needs 'text'")?;
    if at > n {
        return Err(format!("'at' {at} out of bounds (0..={n})"));
    }
    if at == n {
        // Insert at the very end == append.
        return append(app, args);
    }
    require_para(&app.editor.doc.body, at)?;
    // Paste `text\n` at the head of block `at`: the trailing newline pushes the
    // original paragraph down, so `text` lands as its own paragraph(s) before it.
    app.editor.anchor = None;
    app.editor.caret = Caret::top(at, 0);
    app.editor.paste(&Clip::from_text(&format!("{text}\n")));
    finish_edit(app);
    Ok(Json::obj(vec![(
        "total",
        Json::Num(app.editor.doc.body.len() as f64),
    )]))
}

fn append(app: &mut App, args: &Json) -> Result<Json, String> {
    let text = args.get_str("text").ok_or("doc.append needs 'text'")?;
    // Paste `\ntext` at the document end: the leading newline starts a fresh
    // paragraph, so `text` lands as new paragraph(s) after the current last one.
    app.editor.anchor = None;
    app.editor.move_doc_end();
    app.editor.paste(&Clip::from_text(&format!("\n{text}")));
    finish_edit(app);
    Ok(Json::obj(vec![(
        "total",
        Json::Num(app.editor.doc.body.len() as f64),
    )]))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn block_kind(b: &Block) -> &'static str {
    match b {
        Block::Paragraph(_) => "paragraph",
        Block::Table(_) => "table",
        Block::Raw(_) => "raw",
    }
}

/// Clear the transient selection, re-validate the caret, and mark the document
/// modified after a mutating edit.
fn finish_edit(app: &mut App) {
    app.editor.clear_selection();
    app.editor.clamp();
    app.modified = true;
    app.dirty = true;
}

fn require_para(body: &[Block], i: usize) -> Result<(), String> {
    match body.get(i) {
        Some(Block::Paragraph(_)) => Ok(()),
        Some(_) => Err(format!("block {i} is not a paragraph; edit verbs need one")),
        None => Err(format!("block {i} out of bounds")),
    }
}

fn bounds(start: usize, end: usize, n: usize) -> Result<(), String> {
    if n == 0 {
        return Err("document is empty".into());
    }
    if start >= n || end >= n {
        return Err(format!("range {start}..{end} out of bounds (0..{})", n - 1));
    }
    if start > end {
        return Err(format!("start {start} is after end {end}"));
    }
    Ok(())
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
    bounds(start, end, n)?;
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
    use docxcore::model::{Block, Document, Paragraph, ParProps, Run, RunProps};
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
        app.editor
            .doc
            .body
            .iter()
            .map(|b| b.plain_text())
            .collect()
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
        let r = read(&app, &args(vec![("start", Json::Num(1.0)), ("end", Json::Num(2.0))])).unwrap();
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
        assert!(read(&app, &args(vec![("start", Json::Num(0.0)), ("end", Json::Num(9.0))])).is_err());
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
            &args(vec![("start", Json::Num(1.0)), ("text", Json::Str("X".into()))]),
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
            &args(vec![("at", Json::Num(1.0)), ("text", Json::Str("X".into()))]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "B", "C"]);
    }

    #[test]
    fn insert_multiline_before() {
        let mut app = app_with(&["A", "B"]);
        insert(
            &mut app,
            &args(vec![("at", Json::Num(1.0)), ("text", Json::Str("X\nY".into()))]),
        )
        .unwrap();
        assert_eq!(paras(&app), vec!["A", "X", "Y", "B"]);
    }

    #[test]
    fn insert_at_end_appends() {
        let mut app = app_with(&["A", "B"]);
        insert(
            &mut app,
            &args(vec![("at", Json::Num(2.0)), ("text", Json::Str("C".into()))]),
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
            &args(vec![("start", Json::Num(1.0)), ("text", Json::Str("X".into()))]),
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
            &args(vec![("at", Json::Num(1.0)), ("text", Json::Str("X".into()))]),
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
        assert!(replace_range(&mut app, &args(vec![("start", Json::Num(5.0)), ("text", Json::Str("x".into()))])).is_err());
        assert!(insert(&mut app, &args(vec![("at", Json::Num(9.0)), ("text", Json::Str("x".into()))])).is_err());
    }

    #[test]
    fn dispatch_routes_and_reports_unknown() {
        let mut app = app_with(&["A"]);
        assert!(dispatch(&mut app, "doc.path", &Json::Null).is_ok());
        let err = dispatch(&mut app, "doc.frobnicate", &Json::Null).unwrap_err();
        assert!(err.contains("unknown verb"));
    }
}
