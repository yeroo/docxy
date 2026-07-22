//! The host-agnostic editing session: everything the wasm ABI exposes, written
//! as plain Rust so it can be unit-tested natively (`cargo test -p docxwasm`).
//!
//! A [`Session`] wraps a loaded [`Package`] (kept whole for lossless save) and an
//! [`Editor`] over its document. The webview drives it through two channels:
//!
//! - **render** — [`Session::view_json`] turns the document into styled lines
//!   (the same [`render`] engine the TUI uses) plus the caret's screen position,
//!   serialized as compact JSON the webview paints onto a monospace grid.
//! - **commands** — [`Session::dispatch`] applies one edit/navigation command,
//!   encoded as a tab-delimited string (no JSON parser needed on this side).
//!
//! The view is a **continuous flow** (`page_view = false`): no pagination,
//! headers, or footers — the document reads like text in an editor tab, which is
//! exactly the fidelity target for the VS Code host.

use std::rc::Rc;

use docxcore::agent;
use docxcore::editor::{Caret, Clip, Editor};
use docxcore::export::{PdfOptions, to_pdf};
use docxcore::load::{Relationships, parse_rels_xml};
use docxcore::model::{Align, Block, Inline};
use docxcore::numbering::{Numbering, compute_markers, parse_numbering_xml};
use docxcore::package::{Package, load_package, save_package};
use docxcore::render::{self, Color, ImageBox, LineMap, RenderOptions};
use docxcore::styles::{StyleSheet, parse_styles_xml};

use crate::json;

/// Convert Markdown source to `.docx` bytes — the same conversion the terminal
/// app does on `Save As` to a `.docx` name. Stateless (no session): the VS Code
/// host calls this to turn a `.md` file into a Word document.
pub fn markdown_to_docx(md: &str) -> Vec<u8> {
    use docxcore::markdown::from_markdown;
    use docxcore::package::new_markdown_package;
    save_package(&new_markdown_package(from_markdown(md)))
}

/// Convert `.docx` bytes to Markdown source (list markers resolved from the
/// package's numbering), or `None` if the container can't be parsed. Stateless.
pub fn docx_to_markdown(bytes: &[u8]) -> Option<String> {
    let pkg = load_package(bytes).ok()?;
    let numbering = pkg
        .part("word/numbering.xml")
        .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
        .unwrap_or_default();
    let markers = compute_markers(&pkg.document, &numbering);
    Some(docxcore::markdown::to_markdown_with(
        &pkg.document,
        &markers,
    ))
}

/// A live editing session over one `.docx`.
pub struct Session {
    /// The whole package, retained so save preserves unmodeled parts byte-for-
    /// faithful. Its `document` is synced from the editor only at save time.
    pkg: Package,
    editor: Editor,
    styles: Rc<StyleSheet>,
    numbering: Numbering,
    /// document.xml relationships (rId → media target), for resolving images.
    rels: Relationships,
    /// Wrap width in grid columns (the webview reports its viewport width).
    width: usize,
    /// Unsaved-changes flag, mirrored to the host's dirty indicator.
    dirty: bool,
    /// Caret maps from the most recent render, used to resolve clicks and
    /// vertical movement (both are screen-position → model-offset lookups).
    maps: Vec<LineMap>,
}

impl Session {
    /// Open a `.docx` from its raw bytes. Returns `None` if the container or the
    /// main document part can't be parsed.
    pub fn open(bytes: &[u8]) -> Option<Session> {
        let pkg = load_package(bytes).ok()?;
        let styles = pkg
            .part("word/styles.xml")
            .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let numbering = pkg
            .part("word/numbering.xml")
            .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let rels = pkg
            .part("word/_rels/document.xml.rels")
            .map(|b| parse_rels_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let editor = Editor::new(pkg.document.clone());
        Some(Session {
            pkg,
            editor,
            styles: Rc::new(styles),
            numbering,
            rels,
            width: 80,
            dirty: false,
            maps: Vec::new(),
        })
    }

    /// Raw bytes of the embedded media referenced by relationship `rid`
    /// (`word/media/imageN.*`), or `None` if it can't be resolved. The webview
    /// turns these into data URIs and paints them over the image placeholders.
    pub fn media(&self, rid: &str) -> Option<Vec<u8>> {
        let target = self.rels.target(rid)?;
        // Rel targets are relative to `word/` (or absolute with a leading `/`).
        let name = match target.strip_prefix('/') {
            Some(abs) => abs.to_string(),
            None => format!("word/{}", target.trim_start_matches("./")),
        };
        self.pkg.part(&name).map(<[u8]>::to_vec)
    }

    /// Continuous-flow render options (no pagination/headers/footers).
    fn options(&self) -> RenderOptions {
        RenderOptions {
            width: self.width.max(1),
            show_invisibles: false,
            page_view: false,
            borderless_tables: false,
            selection: self.editor.selection_spans(),
            styles: self.styles.clone(),
            list_markers: Rc::new(compute_markers(&self.editor.doc, &self.numbering)),
            page: self.pkg.page_geom(),
            headers: Default::default(),
            footers: Default::default(),
            title_page: false,
            even_odd: false,
        }
    }

    /// Render the document to the JSON view the webview consumes. `copied`, when
    /// set, carries text the host should place on the OS clipboard (from a copy
    /// or cut command).
    pub fn view_json(&mut self, copied: Option<&str>) -> String {
        let opts = self.options();
        let (lines, maps, images, mermaid) = render::render_with_images(&self.editor.doc, &opts);
        let (cl, cc) = caret_screen(&maps, &self.editor.caret);
        // Pulled out before `maps` moves into `self.maps` below: whether each line
        // belongs to a list-item paragraph, so the webview can scope its Markdown-
        // mode checkbox glyph to real list items (not any line whose text happens
        // to start with `[ ] `/`[x] `).
        let line_is_list: Vec<bool> = maps.iter().map(|m| m.list).collect();
        self.maps = maps;

        let mut out = String::with_capacity(lines.len() * 48 + 64);
        out.push_str("{\"lines\":[");
        for (li, line) in lines.iter().enumerate() {
            if li > 0 {
                out.push(',');
            }
            // Each line is `{"sp":[...spans...]}`, plus `"list":true` when this
            // line belongs to a list-item paragraph (see `LineMap::list`) — the
            // webview uses that flag to scope Markdown mode's task-list checkbox
            // glyph to real list items instead of any line whose text happens to
            // start with the literal `[ ] `/`[x] ` prefix.
            out.push_str("{\"sp\":[");
            for (si, span) in line.spans.iter().enumerate() {
                if si > 0 {
                    out.push(',');
                }
                out.push_str("{\"t\":");
                json::push_str(&mut out, &span.text);
                let st = &span.style;
                if st.bold {
                    out.push_str(",\"b\":1");
                }
                if st.italic {
                    out.push_str(",\"i\":1");
                }
                if st.underline {
                    out.push_str(",\"u\":1");
                }
                if st.strike {
                    out.push_str(",\"s\":1");
                }
                if st.dim {
                    out.push_str(",\"d\":1");
                }
                if st.highlight {
                    out.push_str(",\"h\":1");
                }
                if let Some(c) = st.color {
                    out.push_str(",\"c\":\"");
                    out.push_str(color_name(c));
                    out.push('"');
                }
                if let Some(l) = &span.link {
                    out.push_str(",\"lnk\":");
                    json::push_str(&mut out, l);
                }
                out.push('}');
            }
            out.push(']');
            if line_is_list.get(li).copied().unwrap_or(false) {
                out.push_str(",\"list\":true");
            }
            out.push('}');
        }
        out.push_str("],\"caret\":{\"line\":");
        out.push_str(&cl.to_string());
        out.push_str(",\"col\":");
        out.push_str(&cc.to_string());
        out.push_str("},\"selection\":");
        out.push_str(if self.editor.has_selection() {
            "1"
        } else {
            "0"
        });
        out.push_str(",\"dirty\":");
        out.push_str(if self.dirty { "true" } else { "false" });
        out.push_str(",\"width\":");
        out.push_str(&self.width.to_string());
        out.push_str(",\"images\":[");
        for (ii, ib) in images.iter().enumerate() {
            if ii > 0 {
                out.push(',');
            }
            push_image(&mut out, ib);
        }
        out.push_str("],\"mermaid\":[");
        for (mi, mb) in mermaid.iter().enumerate() {
            if mi > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"row\":{},\"col\":{},\"cols\":{},\"rows\":{},\"geo\":{},\"source\":",
                mb.row, mb.col, mb.cols, mb.rows, mb.geometry_json
            ));
            json::push_str(&mut out, &mb.source);
            out.push('}');
        }
        out.push(']');
        // Editable column ranges per visual line ([c0, c1) — c1 is one past the
        // last char), straight from the caret maps. Hosts that place edit guards
        // over decorations (list markers, table borders) consume this; the
        // webview ignores unknown fields.
        out.push_str(",\"segs\":[");
        for (li, m) in self.maps.iter().enumerate() {
            if li > 0 {
                out.push(',');
            }
            out.push('[');
            for (si, seg) in m.segs.iter().enumerate() {
                if si > 0 {
                    out.push(',');
                }
                let (a, b) = seg.col_range();
                out.push('[');
                out.push_str(&a.to_string());
                out.push(',');
                out.push_str(&b.to_string());
                out.push(']');
            }
            out.push(']');
        }
        out.push(']');
        if let Some(t) = copied {
            out.push_str(",\"copied\":");
            json::push_str(&mut out, t);
        }
        out.push('}');
        out
    }

    /// The caret's current screen position from the last render.
    fn caret_screen_now(&self) -> (usize, usize) {
        caret_screen(&self.maps, &self.editor.caret)
    }

    /// Apply one tab-delimited command. Returns `Some(text)` when the host should
    /// copy `text` to the OS clipboard (copy/cut); sets the dirty flag on any
    /// mutating command.
    pub fn dispatch(&mut self, cmd: &str) -> Option<String> {
        let mut it = cmd.splitn(2, '\t');
        let op = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("");
        let mut copied = None;
        let mut mutated = true; // default; navigation ops flip this to false

        match op {
            "insert" => self.editor.insert_str(rest),
            "newline" => self.editor.insert_newline(),
            "backspace" => self.editor.backspace(),
            "delete" => self.editor.delete_forward(),
            "bold" => self.editor.toggle_bold(),
            "italic" => self.editor.toggle_italic(),
            "underline" => self.editor.toggle_underline(),
            "strike" => self.editor.toggle_strike(),
            "heading" => {
                let n: u8 = rest.trim().parse().unwrap_or(0);
                if (1..=9).contains(&n) {
                    self.editor.set_para_style(Some(&format!("Heading{n}")));
                } else {
                    self.editor.set_para_style(None);
                }
            }
            "list" => self.toggle_list(rest.trim()),
            "align" => self.editor.set_align(match rest.trim() {
                "center" => Align::Center,
                "right" => Align::Right,
                "justify" => Align::Justify,
                _ => Align::Left,
            }),
            "indent" => self.editor.change_indent(rest.trim().parse().unwrap_or(0)),
            "fontsize" => self.editor.resize_font(rest.trim().parse().unwrap_or(0)),
            "color" => {
                let hex = rest.trim();
                self.editor
                    .set_color((!hex.is_empty()).then(|| hex.to_string()));
            }
            "undo" => {
                self.editor.undo();
            }
            "redo" => {
                self.editor.redo();
            }
            "paste" => self.editor.paste(&Clip::from_text(rest)),
            "replace" => {
                let mut p = rest.splitn(2, '\t');
                let find = p.next().unwrap_or("");
                let with = p.next().unwrap_or("");
                if find.is_empty() || self.editor.replace_all(find, with, false) == 0 {
                    mutated = false; // nothing matched — don't dirty the document
                }
            }
            "cut" => {
                let t = self.editor.selection_text();
                if t.is_empty() {
                    mutated = false;
                } else {
                    self.editor.delete_selection();
                    copied = Some(t);
                }
            }
            "copy" => {
                mutated = false;
                let t = self.editor.selection_text();
                if !t.is_empty() {
                    copied = Some(t);
                }
            }
            "selectall" => {
                mutated = false;
                self.editor.select_all();
            }
            "width" => {
                mutated = false;
                if let Ok(w) = rest.trim().parse::<usize>() {
                    self.width = w.max(1);
                }
            }
            "move" => {
                mutated = false;
                self.do_move(rest);
            }
            "click" => {
                mutated = false;
                self.do_click(rest);
            }
            "goto" => {
                mutated = false;
                self.do_goto(rest.trim());
            }
            _ => mutated = false,
        }

        if mutated {
            self.dirty = true;
        }
        copied
    }

    // ---- agent control surface (`docx_ctl`) --------------------------------
    //
    // Routes one control verb (see `docs/agent-control.md`) against this
    // session's live document. The reply shape is byte-for-byte the same as
    // docxy's control server (`docxy/src/control.rs`): the verb result object
    // plus `"ok":true` on success, or `{"ok":false,"error":"…"}` on failure.
    // Mutating verbs call straight into `docxcore::agent`, which edits via
    // `Editor::paste`/`Editor::insert_str` etc — the very same path
    // `dispatch`'s interactive `paste`/`insert` commands use — so an agent
    // edit lands on the same undo stack as a keystroke and unwinds with the
    // same `self.editor.undo()`. This also dirties the session exactly like a
    // mutating `dispatch` command does.

    /// Route one JSON control request (`{"verb":…,"args":{…}}`) and return the
    /// JSON reply. See the module note above for the reply envelope.
    pub fn ctl(&mut self, request_json: &str) -> String {
        let req = match json::Json::parse(request_json) {
            Ok(v) => v,
            Err(e) => return ctl_err(&format!("bad request: {e}")),
        };
        let verb = req.get_str("verb").unwrap_or("");
        let no_args = json::Json::Null;
        let args = req.get("args").unwrap_or(&no_args);
        let result = match verb {
            "doc.outline" => Ok(self.ctl_outline()),
            "doc.read" => self.ctl_read(args),
            "doc.find" => self.ctl_find(args),
            "doc.replace-range" => self.ctl_replace_range(args),
            "doc.insert" => self.ctl_insert(args),
            "doc.append" => self.ctl_append(args),
            "doc.blocks" => Ok(self.ctl_blocks()),
            "doc.export" => self.ctl_export(args),
            "doc.comments" => Ok(self.ctl_comments()),
            "doc.notes" => Ok(self.ctl_notes()),
            // Default section variant only — mirrors docxy control.rs's
            // doc.header/doc.footer exactly (see the dispatch note there for
            // why first/even-page variants are out of scope for this verb).
            "doc.header" => Ok(self.ctl_header_footer("headerReference")),
            "doc.footer" => Ok(self.ctl_header_footer("footerReference")),
            "doc.metadata" => Ok(self.ctl_metadata()),
            "doc.stats" => Ok(self.ctl_stats()),
            "doc.replace-all" => self.ctl_replace_all(args),
            "doc.format" => self.ctl_format(args),
            "doc.set-style" => self.ctl_set_style(args),
            "doc.undo" => Ok(self.ctl_undo()),
            "doc.redo" => Ok(self.ctl_redo()),
            "doc.export-pdf" => Ok(self.ctl_export_pdf()),
            other => Err(format!("unknown verb '{other}'")),
        };
        match result {
            Ok(body) => ctl_ok(body),
            Err(e) => ctl_err(&e),
        }
    }

    /// `{"headings":[{"index","level","text"}]}`
    fn ctl_outline(&self) -> String {
        let mut out = String::from("{\"headings\":[");
        for (i, h) in agent::outline(&self.editor.doc).into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"index\":");
            out.push_str(&h.index.to_string());
            out.push_str(",\"level\":");
            out.push_str(&h.level.to_string());
            out.push_str(",\"text\":");
            json::push_str(&mut out, &h.text);
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// `{start?, end?, range?}` -> `{total, start, end, text, blocks:[…]}`
    fn ctl_read(&self, args: &json::Json) -> Result<String, String> {
        let n = self.editor.doc.body.len();
        let (start, end) = ctl_range_args(args, n)?;
        let blocks = agent::read(&self.editor.doc, start, end)?;
        let joined = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut out = String::from("{\"total\":");
        out.push_str(&n.to_string());
        out.push_str(",\"start\":");
        out.push_str(&start.to_string());
        out.push_str(",\"end\":");
        out.push_str(&end.to_string());
        out.push_str(",\"text\":");
        json::push_str(&mut out, &joined);
        out.push_str(",\"blocks\":[");
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"index\":");
            out.push_str(&b.index.to_string());
            out.push_str(",\"kind\":");
            json::push_str(&mut out, b.kind);
            out.push_str(",\"text\":");
            json::push_str(&mut out, &b.text);
            if let Some(level) = b.heading {
                out.push_str(",\"heading\":");
                out.push_str(&level.to_string());
            }
            out.push('}');
        }
        out.push_str("]}");
        Ok(out)
    }

    /// `{query, case_sensitive?}` -> `{query, count, matches:[{path,start,end,block?,text?}]}`
    fn ctl_find(&self, args: &json::Json) -> Result<String, String> {
        let query = args.get_str("query").ok_or("doc.find needs a 'query'")?;
        let case_sensitive = args
            .get("case_sensitive")
            .and_then(json::Json::as_bool)
            .unwrap_or(false);
        let body = &self.editor.doc.body;
        let matches = agent::find(&self.editor.doc, query, case_sensitive);
        let mut out = String::from("{\"query\":");
        json::push_str(&mut out, query);
        out.push_str(",\"count\":");
        out.push_str(&matches.len().to_string());
        out.push_str(",\"matches\":[");
        for (i, m) in matches.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"path\":[");
            for (pi, p) in m.path.iter().enumerate() {
                if pi > 0 {
                    out.push(',');
                }
                out.push_str(&p.to_string());
            }
            out.push_str("],\"start\":");
            out.push_str(&m.start.to_string());
            out.push_str(",\"end\":");
            out.push_str(&m.end.to_string());
            // Top-level paragraph matches carry a direct block index + full
            // text, which a client can feed straight back to replace-range.
            if m.path.len() == 1 {
                out.push_str(",\"block\":");
                out.push_str(&m.path[0].to_string());
                if let Some(Block::Paragraph(p)) = body.get(m.path[0]) {
                    out.push_str(",\"text\":");
                    json::push_str(&mut out, &p.plain_text());
                }
            }
            out.push('}');
        }
        out.push_str("]}");
        Ok(out)
    }

    /// `{start, end?, text, markdown?}` -> `{replaced, total, undoSteps}`
    ///
    /// `undoSteps` is an **internal** field for the extension host only: the
    /// number of native undo checkpoints this edit pushed (2 for a normal
    /// delete-then-insert, 1 when the replaced range was a single empty
    /// paragraph and no delete happened). `CtlServer.callWasm` strips it before
    /// the reply hits the TCP wire — a VS Code tab's `doc.replace-range` reply
    /// must be byte-for-byte a terminal docxy's `{replaced, total}` — and hands
    /// the count to `host.onMutated` so the tab replays exactly that many
    /// wasm undos per VS Code undo (see `docxcore::agent::replace_range`).
    ///
    /// `markdown` (optional bool, default `false`) mirrors docxy control.rs's
    /// flag exactly: when true, `text` is parsed as Markdown and the resulting
    /// blocks are spliced in with the same undo-step accounting as the
    /// plain-text path (see `docxcore::agent::replace_range_blocks`). The
    /// splice position is validated BEFORE any numbering/style package
    /// mutation ([`Self::prepare_markdown_blocks`]) — see
    /// `docxcore::agent::validate_replace_range`'s doc comment for why.
    fn ctl_replace_range(&mut self, args: &json::Json) -> Result<String, String> {
        let start = args
            .get_usize("start")
            .ok_or("doc.replace-range needs a 'start' index")?;
        let end = args.get_usize("end").unwrap_or(start);
        let text = args
            .get_str("text")
            .ok_or("doc.replace-range needs 'text'")?;
        let (replaced, undo_steps) = if ctl_markdown_flag(args) {
            agent::validate_replace_range(&self.editor.doc, start, end)?;
            let blocks = self.prepare_markdown_blocks(text)?;
            agent::replace_range_blocks(&mut self.editor, start, end, blocks)?
        } else {
            agent::replace_range(&mut self.editor, start, end, text)?
        };
        self.finish_ctl_edit();
        let mut out = String::from("{\"replaced\":");
        out.push_str(&replaced.to_string());
        out.push_str(",\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push_str(",\"undoSteps\":");
        out.push_str(&undo_steps.to_string());
        out.push('}');
        Ok(out)
    }

    /// `{at, text, markdown?}` -> `{total}`. `markdown` mirrors docxy
    /// control.rs's flag exactly — see [`Self::ctl_replace_range`]'s doc
    /// comment for the validate-before-mutate ordering this shares.
    fn ctl_insert(&mut self, args: &json::Json) -> Result<String, String> {
        let at = args
            .get_usize("at")
            .ok_or("doc.insert needs an 'at' index")?;
        let text = args.get_str("text").ok_or("doc.insert needs 'text'")?;
        if ctl_markdown_flag(args) {
            agent::validate_insert_at(&self.editor.doc, at)?;
            let blocks = self.prepare_markdown_blocks(text)?;
            agent::insert_blocks(&mut self.editor, at, blocks)?;
        } else {
            agent::insert(&mut self.editor, at, text)?;
        }
        self.finish_ctl_edit();
        let mut out = String::from("{\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push('}');
        Ok(out)
    }

    /// `{text, markdown?}` -> `{total}`. `markdown` mirrors docxy
    /// control.rs's flag exactly — see [`Self::ctl_replace_range`]'s doc
    /// comment for the pattern (`append` has no position argument, so there's
    /// no bounds check to order against).
    fn ctl_append(&mut self, args: &json::Json) -> Result<String, String> {
        let text = args.get_str("text").ok_or("doc.append needs 'text'")?;
        if ctl_markdown_flag(args) {
            let blocks = self.prepare_markdown_blocks(text)?;
            agent::append_blocks(&mut self.editor, blocks);
        } else {
            agent::append(&mut self.editor, text);
        }
        self.finish_ctl_edit();
        let mut out = String::from("{\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push('}');
        Ok(out)
    }

    /// Parse `text` as Markdown into blocks ready to splice, ensuring this
    /// session's `Package` carries numbering/style definitions for any list
    /// or style the parsed content references before the caller splices it
    /// in — the docxwasm-side counterpart to `docxy::control`'s
    /// `prepare_markdown_blocks`, sharing its exact detection helpers
    /// (`docxcore::agent::referenced_numbering_kinds`/`referenced_style_ids`)
    /// and remap/ensure logic, just against `self.pkg` instead of `App::pkg`.
    ///
    /// **Ordering**: this only ever ADDS package parts/definitions, never
    /// removes or rewrites unrelated ones — but callers must still validate
    /// the splice position (`agent::validate_insert_at`/
    /// `validate_replace_range`) BEFORE calling this, exactly like
    /// `docxy::control::prepare_markdown_blocks` requires, so a call that's
    /// ultimately going to be rejected for bad bounds never leaves the
    /// mutation behind (`ctl_insert`/`ctl_replace_range` both do this).
    fn prepare_markdown_blocks(&mut self, text: &str) -> Result<Vec<Block>, String> {
        let mut blocks = agent::parse_markdown_blocks(text)?;

        let (needs_bullet, needs_decimal) = agent::referenced_numbering_kinds(&blocks);
        if needs_bullet || needs_decimal {
            let bullet_id = needs_bullet.then(|| self.pkg.ensure_list(true));
            let decimal_id = needs_decimal.then(|| self.pkg.ensure_list(false));
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
            self.reparse_numbering();
        }

        let style_ids = agent::referenced_style_ids(&blocks);
        if !style_ids.is_empty() {
            self.pkg.ensure_styles(&style_ids);
        }

        Ok(blocks)
    }

    /// `{}` -> `{total, modified, protection?, watermark?}` (the host composes
    /// this with URI info for its own `doc.path`-equivalent reply). The last
    /// two are present-if-set, straight off `Package::protection`/
    /// `Package::watermark` — the additive keys `doc.path` gains on every
    /// surface in this wave (mirrors docxy control.rs's `path_info`).
    fn ctl_blocks(&self) -> String {
        let mut out = String::from("{\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push_str(",\"modified\":");
        out.push_str(if self.dirty { "true" } else { "false" });
        if let Some(p) = self.pkg.protection() {
            out.push_str(",\"protection\":");
            json::push_str(&mut out, &p);
        }
        if let Some(w) = self.pkg.watermark() {
            out.push_str(",\"watermark\":");
            json::push_str(&mut out, &w);
        }
        out.push('}');
        out
    }

    /// `{format:"markdown"|"text"}` -> `{format, text}` — the live buffer, on
    /// the same terms as docxy control.rs's `export` (mirrors it exactly,
    /// including the error wording).
    fn ctl_export(&self, args: &json::Json) -> Result<String, String> {
        let format = args
            .get_str("format")
            .ok_or("doc.export needs a 'format' (markdown|text)")?;
        let text = match format {
            "markdown" => docxcore::markdown::to_markdown(&self.editor.doc),
            "text" => self.editor.doc.plain_text(),
            other => return Err(format!("unknown format '{other}' (markdown|text)")),
        };
        let mut out = String::from("{\"format\":");
        json::push_str(&mut out, format);
        out.push_str(",\"text\":");
        json::push_str(&mut out, &text);
        out.push('}');
        Ok(out)
    }

    /// `{}` -> `{comments:[{id,author,initials,date,text,anchor}]}`. Read
    /// straight off the package's `word/comments.xml` (not the live editor
    /// document — edits to body text don't touch comment anchors), same
    /// source docxy control.rs's `comments()` uses.
    fn ctl_comments(&self) -> String {
        let items = docxcore::comments::parse_comments(&self.pkg);
        let mut out = String::from("{\"comments\":[");
        for (i, c) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"id\":");
            json::push_str(&mut out, &c.id);
            out.push_str(",\"author\":");
            json::push_str(&mut out, &c.author);
            out.push_str(",\"initials\":");
            json::push_str(&mut out, &c.initials);
            out.push_str(",\"date\":");
            json::push_str(&mut out, &c.date);
            out.push_str(",\"text\":");
            json::push_str(&mut out, &c.text);
            out.push_str(",\"anchor\":");
            json::push_str(&mut out, &c.quoted);
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// `{}` -> `{notes:[{id,kind:"footnote"|"endnote",text}]}`. Footnotes then
    /// endnotes, in file order — mirrors docxy control.rs's `notes()`.
    fn ctl_notes(&self) -> String {
        let items = docxcore::notes::parse_notes(&self.pkg);
        let mut out = String::from("{\"notes\":[");
        for (i, n) in items.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let kind = if n.endnote { "endnote" } else { "footnote" };
            out.push_str("{\"id\":");
            out.push_str(&n.id.to_string());
            out.push_str(",\"kind\":");
            json::push_str(&mut out, kind);
            out.push_str(",\"text\":");
            json::push_str(&mut out, &n.text);
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// `{}` -> `{blocks:[{index,kind,text}]}` (empty when the document has
    /// none) — the DEFAULT section header/footer only, resolved via
    /// [`header_footer_blocks`](Self::header_footer_blocks). `kind` is
    /// `"headerReference"`/`"footerReference"`, matching docxy main.rs's
    /// `load_hdr_ftr`/`parts` convention.
    fn ctl_header_footer(&self, kind: &str) -> String {
        let blocks = self.header_footer_blocks(kind);
        let mut out = String::from("{\"blocks\":[");
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"index\":");
            out.push_str(&i.to_string());
            out.push_str(",\"kind\":");
            json::push_str(&mut out, agent::block_kind(b));
            out.push_str(",\"text\":");
            json::push_str(&mut out, &b.plain_text());
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// Resolve the DEFAULT section header/footer's block content from `kind`
    /// (`"headerReference"`/`"footerReference"`), empty when the document has
    /// none. Thin wrapper over `docxcore::load::resolve_header_footer` —
    /// shared with docxy main.rs's `load_hdr_ftr`, so the sectPr -> rels ->
    /// part -> parse resolution lives in exactly one place.
    fn header_footer_blocks(&self, kind: &str) -> Vec<Block> {
        docxcore::load::resolve_header_footer(&self.pkg, &self.rels, kind, "default")
    }

    /// `{}` -> present-if-set: `{title?,author?,subject?,keywords?,comments?,
    /// last_saved_by?,revision?,created?,modified?}` — `docProps/core.xml`,
    /// empty strings and unparsed dates omitted rather than sent empty/null.
    /// Mirrors docxy control.rs's `metadata()`/`format_iso()` field order
    /// exactly.
    fn ctl_metadata(&self) -> String {
        let props = self
            .pkg
            .part("docProps/core.xml")
            .map(|b| docxcore::field::parse_core_props(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let mut out = String::from("{");
        let mut first = true;
        for (key, val) in [
            ("title", props.title.as_str()),
            ("author", props.author.as_str()),
            ("subject", props.subject.as_str()),
            ("keywords", props.keywords.as_str()),
            ("comments", props.comments.as_str()),
            ("last_saved_by", props.last_saved_by.as_str()),
            ("revision", props.revision.as_str()),
        ] {
            if val.is_empty() {
                continue;
            }
            if !first {
                out.push(',');
            }
            first = false;
            json::push_str(&mut out, key);
            out.push(':');
            json::push_str(&mut out, val);
        }
        if let Some(dt) = &props.created {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str("\"created\":");
            json::push_str(&mut out, &docxcore::field::format_iso(dt));
        }
        if let Some(dt) = &props.modified {
            if !first {
                out.push(',');
            }
            out.push_str("\"modified\":");
            json::push_str(&mut out, &docxcore::field::format_iso(dt));
        }
        out.push('}');
        out
    }

    /// `{}` -> `{words, chars, paragraphs, blocks}` — word/character/
    /// paragraph/block counts over the live buffer (mirrors docxy
    /// control.rs's `stats()`; `chars` excludes block-separator newlines).
    fn ctl_stats(&self) -> String {
        let (words, chars, paragraphs, blocks) = agent::stats(&self.editor.doc);
        format!(
            "{{\"words\":{words},\"chars\":{chars},\"paragraphs\":{paragraphs},\"blocks\":{blocks}}}"
        )
    }

    /// `{query, text, case_sensitive?}` -> `{replaced, undoSteps}`.
    ///
    /// `undoSteps` is the internal field (see `ctl_replace_range`'s doc
    /// comment for the pattern): `1` when anything was replaced, `0` on a
    /// genuine no-match no-op — exactly `agent::replace_all`'s own reported
    /// count. Unlike `doc.replace-range` (bounds-checked, never a genuine
    /// no-op), `replace-all` can legitimately match nothing, and a no-match
    /// call must leave no tracks: no undo checkpoint, no dirty flag — mirrors
    /// docxy control.rs's `replace_all`'s no-op guard exactly.
    fn ctl_replace_all(&mut self, args: &json::Json) -> Result<String, String> {
        let query = args
            .get_str("query")
            .ok_or("doc.replace-all needs a 'query'")?;
        let text = args.get_str("text").ok_or("doc.replace-all needs 'text'")?;
        let case_sensitive = args
            .get("case_sensitive")
            .and_then(json::Json::as_bool)
            .unwrap_or(false);
        let (replaced, undo_steps) =
            agent::replace_all(&mut self.editor, query, text, case_sensitive);
        if replaced > 0 {
            self.finish_ctl_edit();
        }
        let mut out = String::from("{\"replaced\":");
        out.push_str(&replaced.to_string());
        out.push_str(",\"undoSteps\":");
        out.push_str(&undo_steps.to_string());
        out.push('}');
        Ok(out)
    }

    /// `{start, end?, patch}` -> `{formatted}`. `patch` is a JSON object (≥1
    /// key), routed through `agent::RunPatch::parse` for validation/typing,
    /// then applied over the block range as ONE undo checkpoint via
    /// `agent::format_range` — mirrors docxy control.rs's `format`
    /// byte-for-byte (same patch-pairs stringification, same error family,
    /// same `{formatted}` reply key). No `undoSteps` field: the default tab
    /// mapping (`steps=1`) is correct for a single-checkpoint verb, unlike
    /// `doc.replace-range`/`doc.replace-all` above.
    fn ctl_format(&mut self, args: &json::Json) -> Result<String, String> {
        let start = args
            .get_usize("start")
            .ok_or("doc.format needs a 'start' index")?;
        let end = args.get_usize("end").unwrap_or(start);
        let patch_arg = args.get("patch").ok_or("doc.format needs a 'patch'")?;
        let pairs = ctl_patch_pairs(patch_arg)?;
        let patch = agent::RunPatch::parse(&pairs)?;
        let formatted = agent::format_range(&mut self.editor, start, end, &patch)?;
        self.finish_ctl_edit();
        Ok(format!("{{\"formatted\":{formatted}}}"))
    }

    /// `{start, end?, style?, align?}` -> `{styled}`. At least one of
    /// `style`/`align` is required. Mirrors docxy control.rs's `set_style`
    /// byte-for-byte: `agent::validate_set_style_range` runs first (pure, no
    /// `Package`), THEN `Package::ensure_styles` for a markdown-set style id
    /// (never for `Normal` or an align-only call) — this session's own
    /// `Package` in place of docxy's `App::pkg` — THEN the real mutation via
    /// `agent::set_style_range`, which re-validates and pushes the ONE undo
    /// checkpoint. No `undoSteps` field, same reasoning as `ctl_format`.
    fn ctl_set_style(&mut self, args: &json::Json) -> Result<String, String> {
        let start = args
            .get_usize("start")
            .ok_or("doc.set-style needs a 'start' index")?;
        let end = args.get_usize("end").unwrap_or(start);
        let style = args.get_str("style");
        let align = match args.get_str("align") {
            Some(a) => Some(ctl_parse_align(a)?),
            None => None,
        };

        agent::validate_set_style_range(&self.editor.doc, start, end, style, align)?;
        if let Some(s) = style {
            if s != "Normal" {
                self.pkg.ensure_styles(&[s]);
            }
        }
        let styled = agent::set_style_range(&mut self.editor, start, end, style, align)?;
        self.finish_ctl_edit();
        Ok(format!("{{\"styled\":{styled}}}"))
    }

    /// `{}` -> `{done, undoSteps:0}`. Undo/redo are not themselves undoable
    /// edits, so `undoSteps` is always `0` regardless of `done` — Task 7's
    /// tab adaptation fires its own host-orchestrated inverse edit event
    /// (a NEW edit whose undo/redo replays this same wasm op) rather than
    /// replaying a wasm-undo-stack count. A no-op (`done:false`, empty undo
    /// stack) must not dirty the session — mirrors docxy control.rs's `undo`.
    fn ctl_undo(&mut self) -> String {
        let done = agent::undo(&mut self.editor);
        if done {
            self.finish_ctl_edit();
        }
        format!("{{\"done\":{done},\"undoSteps\":0}}")
    }

    /// `{}` -> `{done, undoSteps:0}`. Same contract and no-op guard as
    /// [`ctl_undo`](Self::ctl_undo), mirroring docxy control.rs's `redo`.
    fn ctl_redo(&mut self) -> String {
        let done = agent::redo(&mut self.editor);
        if done {
            self.finish_ctl_edit();
        }
        format!("{{\"done\":{done},\"undoSteps\":0}}")
    }

    /// `{}` -> internal shape `{"pdfBase64":"<base64>"}` (the `"ok":true` is
    /// added by `ctl_ok`). No `path` arg — unlike the terminal's
    /// `doc.export-pdf`, which writes the file itself, this hands raw PDF
    /// bytes to the host; the extension (Task 7) decodes `pdfBase64`, writes
    /// it, and produces the terminal-shaped `{path}` reply on the wire.
    /// Doesn't touch `self.editor`, so it neither dirties the session nor is
    /// part of the undo stack — mirrors docxy control.rs's `export_pdf`
    /// (`PdfOptions`/`styles` construction is identical).
    fn ctl_export_pdf(&self) -> String {
        let pdf = to_pdf(
            &self.editor.doc,
            &PdfOptions {
                styles: self.styles.clone(),
                ..PdfOptions::default()
            },
        );
        let mut out = String::from("{\"pdfBase64\":");
        json::push_str(&mut out, &json::to_base64(&pdf));
        out.push('}');
        out
    }

    /// Post-mutation bookkeeping shared by every ctl edit verb: clear the
    /// transient selection, re-validate the caret, and mark the session dirty
    /// — the same bookkeeping `dispatch`'s mutating commands perform.
    fn finish_ctl_edit(&mut self) {
        self.editor.clear_selection();
        self.editor.clamp();
        self.dirty = true;
    }

    /// Toggle a bulleted/numbered list on the selected paragraphs, or clear it.
    /// Mirrors the terminal app: `ensure_list` provisions a numbering definition
    /// in the package, then the paragraphs join or leave it; the numbering is
    /// re-parsed so list markers render.
    fn toggle_list(&mut self, kind: &str) {
        match kind {
            "bullet" | "number" => {
                let num_id = self.pkg.ensure_list(kind == "bullet");
                if self.editor.all_in_list(num_id) {
                    self.editor.set_list(None);
                } else {
                    self.editor.set_list(Some(num_id));
                }
            }
            _ => self.editor.set_list(None),
        }
        self.reparse_numbering();
    }

    /// Re-read `word/numbering.xml` from the package into `self.numbering` so
    /// the live view picks up a definition [`Package::ensure_list`] just
    /// created or extended — without this, a freshly-provisioned list part
    /// would render with no markers until the next reload. Shared by
    /// [`Self::toggle_list`] (the keyboard/ribbon list commands) and
    /// [`Self::prepare_markdown_blocks`] (the agent markdown-splice path);
    /// mirrors `docxy::App::reparse_numbering`.
    fn reparse_numbering(&mut self) {
        self.numbering = self
            .pkg
            .part("word/numbering.xml")
            .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
    }

    /// `move\t<dir>\t<select>` — arrow / word / document navigation.
    fn do_move(&mut self, rest: &str) {
        let mut a = rest.split('\t');
        let dir = a.next().unwrap_or("");
        let select = a.next() == Some("1");
        if select {
            self.editor.extend_selection(true);
        } else {
            self.editor.clear_selection();
        }
        match dir {
            "left" => self.editor.move_left(),
            "right" => self.editor.move_right(),
            "wordleft" => self.editor.move_word_left(),
            "wordright" => self.editor.move_word_right(),
            "home" => self.editor.move_home(),
            "end" => self.editor.move_end(),
            "docstart" => self.editor.move_doc_start(),
            "docend" => self.editor.move_doc_end(),
            "up" => self.move_vert(true),
            "down" => self.move_vert(false),
            _ => {}
        }
    }

    /// Vertical movement is view-dependent: keep the screen column and hop to the
    /// nearest editable segment one visual line up/down (using the last render's
    /// caret maps).
    fn move_vert(&mut self, up: bool) {
        let (line, col) = self.caret_screen_now();
        let target = if up {
            line.checked_sub(1)
        } else {
            Some(line + 1)
        };
        if let Some(t) = target {
            if let Some(m) = self.maps.get(t) {
                if let Some(seg) = m.nearest_seg(col) {
                    self.editor.caret = Caret::at(seg.path.clone(), seg.offset_for_col(col));
                }
            }
        }
    }

    /// `goto\t<anchor>` — move the caret to the block holding the named
    /// bookmark (a TOC entry's or cross-reference's target). No-op when the
    /// bookmark doesn't exist. Mirrors the TUI's `jump_to_anchor`, but moves
    /// the caret (host-agnostic) instead of the scroll position.
    fn do_goto(&mut self, anchor: &str) {
        if anchor.is_empty() {
            return;
        }
        let needle = format!("w:name=\"{anchor}\"");
        let Some(bi) = self
            .editor
            .doc
            .body
            .iter()
            .position(|b| block_has_bookmark(b, &needle))
        else {
            return;
        };
        // First rendered line of that block, via the caret maps.
        for m in &self.maps {
            if let Some(seg) = m.segs.iter().find(|s| s.path.first() == Some(&bi)) {
                self.editor.clear_selection();
                self.editor.caret = Caret::at(seg.path.clone(), seg.start);
                return;
            }
        }
    }

    /// `click\t<line>\t<col>\t<select>` — place (or extend to) the caret at a grid
    /// cell.
    fn do_click(&mut self, rest: &str) {
        let mut a = rest.split('\t');
        let line: usize = a.next().unwrap_or("").parse().unwrap_or(0);
        let col: usize = a.next().unwrap_or("").parse().unwrap_or(0);
        let select = a.next() == Some("1");
        if select {
            self.editor.extend_selection(true);
        } else {
            self.editor.clear_selection();
        }
        if let Some(m) = self.maps.get(line) {
            if let Some(seg) = m.nearest_seg(col) {
                self.editor.caret = Caret::at(seg.path.clone(), seg.offset_for_col(col));
            }
        }
    }

    /// Serialize the (edited) document back to `.docx` bytes, losslessly — every
    /// unmodeled part of the original package is preserved. Clears the dirty flag.
    pub fn save(&mut self) -> Vec<u8> {
        self.pkg.document = self.editor.doc.clone();
        self.dirty = false;
        save_package(&self.pkg)
    }

    #[cfg(test)]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

/// Splice `"ok":true` into a ctl verb's result object string (`{…}`),
/// completing the success envelope. Handles the genuinely-empty-object case
/// (`doc.metadata` on a package with no set properties returns `"{}"`) so the
/// splice doesn't leave a leading comma (`{,"ok":true}`, invalid JSON).
fn ctl_ok(body: String) -> String {
    let mut s = body;
    s.pop(); // trailing '}'
    s.push_str(if s.ends_with('{') {
        "\"ok\":true}"
    } else {
        ",\"ok\":true}"
    });
    s
}

/// The ctl failure envelope: `{"ok":false,"error":"…"}`.
fn ctl_err(msg: &str) -> String {
    let mut out = String::from("{\"ok\":false,\"error\":");
    json::push_str(&mut out, msg);
    out.push('}');
    out
}

/// Whether a `doc.insert`/`doc.replace-range`/`doc.append` ctl call opted
/// into Markdown-formatted splicing via the optional `markdown` arg (default
/// `false` — byte-identical to the plain-text behavior). Mirrors
/// `docxy::control`'s `markdown_flag`.
fn ctl_markdown_flag(args: &json::Json) -> bool {
    args.get("markdown")
        .and_then(json::Json::as_bool)
        .unwrap_or(false)
}

/// Build `agent::RunPatch`'s wire pairs from the `patch` object's own JSON
/// values — docxcore stays JSON-free, so scalars are stringified here
/// (`true`/`false` for booleans, the raw text for strings, the number's text
/// for numbers). Mirrors `docxy::control`'s `patch_pairs` byte-for-byte.
fn ctl_patch_pairs(patch: &json::Json) -> Result<Vec<(String, String)>, String> {
    let json::Json::Obj(pairs) = patch else {
        return Err("doc.format needs a 'patch' object".to_string());
    };
    Ok(pairs
        .iter()
        .map(|(k, v)| {
            let text = match v {
                json::Json::Str(s) => s.clone(),
                json::Json::Bool(b) => b.to_string(),
                json::Json::Num(n) => n.to_string(),
                json::Json::Null | json::Json::Arr(_) | json::Json::Obj(_) => String::new(),
            };
            (k.clone(), text)
        })
        .collect())
}

/// Parse `left`/`center`/`right`/`justify` into `Align` for `doc.set-style`'s
/// `align` key. Mirrors `docxy::control`'s `parse_align` byte-for-byte.
fn ctl_parse_align(s: &str) -> Result<Align, String> {
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

/// Resolve an optional block range from `{start, end}` or `{range:"a..b"}`,
/// defaulting to the whole document. Mirrors `docxy::control`'s `range_args`.
fn ctl_range_args(args: &json::Json, n: usize) -> Result<(usize, usize), String> {
    if n == 0 {
        return Err("document is empty".into());
    }
    let mut start = args.get_usize("start");
    let mut end = args.get_usize("end");
    if let Some(r) = args.get_str("range") {
        let (a, b) = ctl_parse_range_str(r)?;
        start = start.or(a);
        end = end.or(b);
    }
    let start = start.unwrap_or(0);
    let end = end.unwrap_or(n - 1);
    agent::bounds(start, end, n)?;
    Ok((start, end))
}

/// Parse `"a..b"`, `"a.."`, `"..b"`, or `"a"` into optional bounds.
fn ctl_parse_range_str(s: &str) -> Result<(Option<usize>, Option<usize>), String> {
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

/// Does a block (recursively, through table cells) hold a `<w:bookmarkStart>`
/// whose raw XML contains `needle` (the `w:name="…"` attribute)? Same logic as
/// the TUI's jump-to-anchor.
fn block_has_bookmark(b: &Block, needle: &str) -> bool {
    match b {
        Block::Paragraph(p) => p.content.iter().any(
            |i| matches!(i, Inline::Raw(s) if s.contains("bookmarkStart") && s.contains(needle)),
        ),
        Block::Table(t) => t.rows.iter().any(|r| {
            r.cells
                .iter()
                .any(|c| c.blocks.iter().any(|bb| block_has_bookmark(bb, needle)))
        }),
        Block::Raw(_) => false,
    }
}

/// Find the caret's screen `(line, col)` from a set of caret maps.
fn caret_screen(maps: &[LineMap], caret: &Caret) -> (usize, usize) {
    for (i, m) in maps.iter().enumerate() {
        if let Some(seg) = m.seg_for(&caret.path, caret.offset) {
            if let Some(col) = seg.col_for_offset(caret.offset) {
                return (i, col);
            }
        }
    }
    // No segment contains the offset — typically a trailing space the soft-wrap
    // consumed (it's in the model but never rendered). Pin the caret at the wrap
    // margin: the end of the caret paragraph's nearest segment at or below the
    // offset (or the paragraph's first segment when the offset precedes them
    // all), rather than teleporting to the document start.
    let mut below: Option<(usize, usize, usize)> = None; // (end_offset, line, col)
    let mut first: Option<(usize, usize, usize)> = None; // (start_offset, line, col0)
    for (i, m) in maps.iter().enumerate() {
        for seg in m.segs.iter().filter(|s| s.path == caret.path) {
            let end = seg.start + seg.nchars();
            if end <= caret.offset && below.is_none_or(|(be, _, _)| end >= be) {
                below = Some((end, i, seg.col_range().1));
            }
            if first.is_none_or(|(fs, _, _)| seg.start < fs) {
                first = Some((seg.start, i, seg.col0));
            }
        }
    }
    if let Some((_, line, col)) = below.or(first) {
        return (line, col);
    }
    (0, 0)
}

/// Serialize an image placeholder box for the webview: its grid position/size
/// (`row`,`col`,`w`,`h` in cells), whether it's bordered, the relationship id to
/// fetch the pixels with, and a text fallback label.
fn push_image(out: &mut String, ib: &ImageBox) {
    out.push_str("{\"rid\":");
    json::push_str(out, &ib.rid);
    out.push_str(",\"row\":");
    out.push_str(&ib.row.to_string());
    out.push_str(",\"col\":");
    out.push_str(&ib.col.to_string());
    out.push_str(",\"w\":");
    out.push_str(&ib.cols.to_string());
    out.push_str(",\"h\":");
    out.push_str(&ib.rows.to_string());
    out.push_str(",\"bordered\":");
    out.push_str(if ib.bordered { "1" } else { "0" });
    out.push_str(",\"label\":");
    json::push_str(out, &ib.label);
    out.push('}');
}

/// Map a rendered [`Color`] to a VS Code terminal ANSI palette name (the webview
/// resolves it to `--vscode-terminal-ansi<Name>`, so colors honor the theme).
fn color_name(c: Color) -> &'static str {
    match c {
        Color::Black => "Black",
        Color::Red => "Red",
        Color::Green => "Green",
        Color::Yellow => "Yellow",
        Color::Blue => "Blue",
        Color::Magenta => "Magenta",
        Color::Cyan => "Cyan",
        Color::White => "White",
        Color::Gray => "BrightBlack",
        Color::BrightRed => "BrightRed",
        Color::BrightGreen => "BrightGreen",
        Color::BrightYellow => "BrightYellow",
        Color::BrightBlue => "BrightBlue",
        Color::BrightMagenta => "BrightMagenta",
        Color::BrightCyan => "BrightCyan",
        Color::BrightWhite => "BrightWhite",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::Inline;
    use docxcore::package::{new_package, save_package};

    /// A tiny real `.docx` (one paragraph) built through the package layer, so
    /// tests exercise the same load path the webview uses.
    fn sample_docx(text: &str) -> Vec<u8> {
        let xml = format!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"
        );
        let doc = docxcore::load::parse_document_xml(&xml, &Default::default());
        save_package(&new_package(doc))
    }

    #[test]
    fn opens_and_renders_text() {
        let bytes = sample_docx("Hello world");
        let mut s = Session::open(&bytes).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("Hello world"), "render missing text: {v}");
        assert!(v.contains("\"caret\""));
        assert!(v.contains("\"dirty\":false"));
    }

    #[test]
    fn typing_inserts_and_marks_dirty() {
        let bytes = sample_docx("Hi");
        let mut s = Session::open(&bytes).expect("open");
        // caret starts at doc start; type "X"
        s.dispatch("insert\tX");
        assert!(s.is_dirty());
        let v = s.view_json(None);
        assert!(v.contains("XHi"), "expected inserted text: {v}");
        assert!(v.contains("\"dirty\":true"));
    }

    #[test]
    fn save_round_trips_edit() {
        let bytes = sample_docx("abc");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("insert\tZ");
        let out = s.save();
        assert!(!s.is_dirty(), "dirty should clear after save");
        // Re-open the saved bytes and confirm the edit survived the round-trip.
        let mut s2 = Session::open(&out).expect("reopen");
        let v = s2.view_json(None);
        assert!(v.contains("Zabc"), "edit not persisted: {v}");
    }

    #[test]
    fn copy_returns_clipboard_text() {
        let bytes = sample_docx("pick me");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("selectall");
        let copied = s.dispatch("copy");
        assert_eq!(copied.as_deref(), Some("pick me"));
        assert!(!s.is_dirty(), "copy must not dirty the document");
    }

    #[test]
    fn formatting_commands_apply_and_survive_save() {
        let bytes = sample_docx("Heading text");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("selectall");
        s.dispatch("heading\t1");
        s.dispatch("align\tcenter");
        s.dispatch("fontsize\t8");
        assert!(s.is_dirty());
        // Re-open the saved bytes: the heading style must round-trip losslessly.
        let out = s.save();
        let mut s2 = Session::open(&out).expect("reopen");
        let v = s2.view_json(None);
        assert!(v.contains("Heading text"), "text lost: {v}");
    }

    #[test]
    fn bullet_list_adds_a_marker() {
        let bytes = sample_docx("Item one");
        let mut s = Session::open(&bytes).expect("open");
        let before = s.view_json(None);
        s.dispatch("selectall");
        s.dispatch("list\tbullet");
        let after = s.view_json(None);
        assert!(s.is_dirty());
        // The list marker makes the paragraph's first line wider than before.
        assert_ne!(before, after, "list toggle changed nothing");
        assert!(after.contains("Item one"));
    }

    #[test]
    fn view_json_flags_list_item_lines() {
        // Plain paragraph: no `"list"` flag at all (LineMap::list defaults false,
        // and view_json only emits `"list":true` — never `"list":false`).
        let bytes = sample_docx("Item one");
        let mut s = Session::open(&bytes).expect("open");
        let plain = s.view_json(None);
        assert!(
            !plain.contains("\"list\""),
            "plain paragraph must not carry a list flag: {plain}"
        );

        // Same paragraph, toggled into a bulleted list: its line now carries
        // `"list":true` — this is what the webview scopes the Markdown-mode
        // checkbox glyph to (see Task 5's fix-wave, offxy-vscode/media/webview.js).
        s.dispatch("selectall");
        s.dispatch("list\tbullet");
        let listed = s.view_json(None);
        assert!(
            listed.contains("\"list\":true"),
            "list-item line missing its list flag: {listed}"
        );
    }

    #[test]
    fn view_json_emits_mermaid_geometry() {
        // Build a real `.docx` from a mermaid fence via the markdown path (the
        // same conversion `markdown_to_docx` gives the VS Code host), so the
        // document carries a `SmartArt` inline whose `raw` embeds the mermaid
        // source (`mermaid::source_of` recovers it from the `descr="mermaid:...`
        // marker `mermaid_para` writes).
        let bytes = markdown_to_docx("```mermaid\nflowchart TD\nA[Start]-->B[End]\n```\n");
        let mut s = Session::open(&bytes).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("\"mermaid\":["), "{v}");
        assert!(v.contains("\"geo\":{\"canvasW\":"), "{v}");
        assert!(v.contains("\"shape\":\"rect\""), "{v}");

        // A plain (non-mermaid) doc has an empty array.
        let bytes2 = markdown_to_docx("hello\n");
        let mut s2 = Session::open(&bytes2).expect("open");
        assert!(s2.view_json(None).contains("\"mermaid\":[]"));
    }

    #[test]
    fn view_json_sequence_kind() {
        // Same markdown → docx path as `view_json_emits_mermaid_geometry`, but
        // for a `sequenceDiagram` fence: the geometry JSON should be tagged
        // `"kind":"sequence"` (from `mermaid_seq::geometry`), not the flowchart
        // shape.
        let bytes = markdown_to_docx("```mermaid\nsequenceDiagram\nA->>B: hi\n```\n");
        let mut s = Session::open(&bytes).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("\"mermaid\":["), "{v}");
        assert!(v.contains("\"kind\":\"sequence\""), "{v}");
    }

    #[test]
    fn view_json_mermaid_carries_source() {
        // Same markdown → docx path as `view_json_emits_mermaid_geometry`: the
        // mermaid entry must also carry the raw source text (JSON-escaped) so
        // the webview can hand it to mermaid.js for live rendering.
        let bytes = markdown_to_docx("```mermaid\nflowchart TD\nA[Start]-->B[End]\n```\n");
        let mut s = Session::open(&bytes).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("\"mermaid\":["), "{v}");
        assert!(v.contains("\"source\":\"flowchart TD"), "{v}");

        // sequence source too
        let bytes2 = markdown_to_docx("```mermaid\nsequenceDiagram\nA->>B: hi\n```\n");
        let mut s2 = Session::open(&bytes2).expect("open");
        assert!(s2.view_json(None).contains("\"source\":\"sequenceDiagram"));
    }

    #[test]
    fn replace_all_rewrites_and_persists() {
        let bytes = sample_docx("red fish red fish");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("replace\tred\tblue");
        assert!(s.is_dirty());
        let v = s.view_json(None);
        assert!(v.contains("blue fish blue fish"), "replace failed: {v}");
        // No-match replace must not dirty a freshly-saved doc.
        let out = s.save();
        let mut s2 = Session::open(&out).expect("reopen");
        s2.dispatch("replace\tzzz\tqqq");
        assert!(!s2.is_dirty(), "no-match replace should not dirty");
    }

    #[test]
    fn markdown_converts_to_docx_and_back() {
        let md = "# Title\n\nHello **bold** and *italic*.\n\n- one\n- two\n";
        let docx = markdown_to_docx(md);
        // The produced bytes are a real package the session can open and render.
        let mut s = Session::open(&docx).expect("open converted docx");
        let v = s.view_json(None);
        assert!(
            v.contains("Title") && v.contains("Hello"),
            "converted render: {v}"
        );
        // Round-trip back to Markdown recovers the heading + emphasis + list.
        let back = docx_to_markdown(&docx).expect("to md");
        assert!(back.contains("# Title"), "heading lost: {back}");
        assert!(back.contains("**bold**"), "bold lost: {back}");
        assert!(
            back.contains("- one") && back.contains("- two"),
            "list lost: {back}"
        );
    }

    #[test]
    fn view_exposes_images_array_and_media_is_safe() {
        let bytes = sample_docx("no pictures here");
        let mut s = Session::open(&bytes).expect("open");
        let v = s.view_json(None);
        assert!(v.contains("\"images\":["), "images array missing: {v}");
        // An unknown relationship id must resolve to nothing, not panic.
        assert!(s.media("rIdNope").is_none());
    }

    /// Parse `"caret":{"line":N,"col":M}` out of a view JSON.
    fn caret_of(v: &str) -> (usize, usize) {
        let s = v
            .split("\"caret\":{\"line\":")
            .nth(1)
            .expect("caret in view");
        let (line, rest) = s.split_once(",\"col\":").expect("caret line");
        let col: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (line.parse().unwrap(), col.parse().unwrap())
    }

    #[test]
    fn trailing_space_keeps_caret_at_wrap_margin() {
        // A soft-wrap consumes trailing spaces, so the caret offset right after
        // a typed space belongs to no rendered segment. It must stay pinned at
        // the wrap margin, not jump to the document start (0,0).
        let bytes = sample_docx("The quick brown fox jumps over the lazy dog again and again");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("width\t30");
        s.dispatch("move\tdocend\t0");
        let before = caret_of(&s.view_json(None));
        assert_ne!(before, (0, 0), "caret should start at the paragraph end");
        for n in 1..=3 {
            s.dispatch("insert\t ");
            let after = caret_of(&s.view_json(None));
            assert_ne!(
                after,
                (0, 0),
                "caret jumped to doc start after {n} space(s)"
            );
            assert_eq!(after.0, before.0, "caret left its line after {n} space(s)");
        }
    }

    #[test]
    fn goto_jumps_to_a_bookmark() {
        let xml = "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body>\
             <w:p><w:r><w:t>First paragraph up top.</w:t></w:r></w:p>\
             <w:p><w:r><w:t>Middle filler paragraph.</w:t></w:r></w:p>\
             <w:p><w:bookmarkStart w:id=\"0\" w:name=\"target\"/><w:r><w:t>Landing zone.</w:t></w:r><w:bookmarkEnd w:id=\"0\"/></w:p>\
             </w:body></w:document>";
        let doc = docxcore::load::parse_document_xml(xml, &Default::default());
        let bytes = save_package(&new_package(doc));
        let mut s = Session::open(&bytes).expect("open");
        s.view_json(None); // populate the caret maps
        let before = caret_of(&s.view_json(None));
        s.dispatch("goto\ttarget");
        let after = caret_of(&s.view_json(None));
        assert!(
            after.0 > before.0,
            "goto should move the caret down: {before:?} -> {after:?}"
        );
        // Unknown anchors are a clean no-op.
        s.dispatch("goto\tnope");
        assert_eq!(caret_of(&s.view_json(None)), after);
    }

    #[test]
    fn segs_cover_a_plain_paragraph() {
        let bytes = sample_docx("Hello world");
        let mut s = Session::open(&bytes).expect("open");
        let v = s.view_json(None);
        // One visual line whose single editable segment spans the 11 chars.
        assert!(v.contains("\"segs\":[[[0,11]]"), "segs missing/wrong: {v}");
    }

    #[test]
    fn segs_follow_soft_wrap() {
        let bytes = sample_docx("aaaa bbbb cccc dddd eeee");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("width\t10");
        let v = s.view_json(None);
        let segs = v.split("\"segs\":").nth(1).expect("segs in view");
        // Wrapped into multiple lines, each line's segment starting at col 0.
        let per_line: Vec<&str> = segs.matches("[[0,").collect();
        assert!(per_line.len() >= 2, "expected >=2 wrapped lines: {v}");
    }

    #[test]
    fn segs_exclude_list_markers() {
        let bytes = sample_docx("Item one");
        let mut s = Session::open(&bytes).expect("open");
        s.dispatch("selectall");
        s.dispatch("list\tbullet");
        let v = s.view_json(None);
        let segs = v.split("\"segs\":").nth(1).expect("segs in view");
        // The marker occupies the leading columns: the first line's first seg
        // must start past column 0.
        assert!(
            !segs.starts_with("[[[0,"),
            "list marker columns must not be editable: {v}"
        );
        assert!(segs.starts_with("[[["), "first line should have a seg: {v}");
    }

    #[test]
    fn dispatch_render_after_move_is_stable() {
        let bytes = sample_docx("one two three");
        let mut s = Session::open(&bytes).expect("open");
        s.view_json(None); // populate caret maps
        s.dispatch("move\tright\t0");
        s.dispatch("move\twordright\t1"); // extend selection
        let v = s.view_json(None);
        assert!(
            v.contains("\"selection\":1"),
            "expected active selection: {v}"
        );
    }

    #[test]
    fn ctl_outline_and_read_match_contract() {
        let mut s = Session::open(&sample_docx("Hello world")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.read","args":{}}"#);
        assert!(
            out.contains("\"ok\":true")
                && out.contains("Hello world")
                && out.contains("\"kind\":\"paragraph\""),
            "{out}"
        );
    }

    #[test]
    fn ctl_replace_range_edits_and_is_undoable() {
        let mut s = Session::open(&sample_docx("first")).expect("open");
        s.ctl(r#"{"verb":"doc.append","args":{"text":"second"}}"#);
        let out = s.ctl(r#"{"verb":"doc.replace-range","args":{"start":1,"text":"better"}}"#);
        assert!(out.contains("\"replaced\":1"), "{out}");
        assert!(out.contains("\"ok\":true"), "{out}");
        // A replace over a non-empty paragraph is a delete-then-insert: the
        // reply reports 2 undo steps, and it unwinds in exactly two.
        assert!(out.contains("\"undoSteps\":2"), "{out}");
        s.dispatch("undo");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("second") && !v.contains("better"), "{v}");
    }

    #[test]
    fn ctl_replace_range_empty_paragraph_reports_one_undo_step() {
        // Replacing an EMPTY paragraph is an insert with no preceding delete:
        // one checkpoint. The reply must report `undoSteps:1` and one
        // `dispatch("undo")` must fully restore the prior content — a host
        // (VS Code tab) replaying two would over-unwind and destroy the edit
        // before it. This is the exact desync guarded here.
        let mut s = Session::open(&sample_docx("kept")).expect("open");
        // Append an empty paragraph (block 1), then replace it.
        s.ctl(r#"{"verb":"doc.append","args":{"text":""}}"#);
        let out = s.ctl(r#"{"verb":"doc.replace-range","args":{"start":1,"text":"filled"}}"#);
        assert!(out.contains("\"replaced\":1"), "{out}");
        assert!(
            out.contains("\"undoSteps\":1"),
            "empty-paragraph replace must report a single undo step: {out}"
        );
        let v = s.view_json(None);
        assert!(v.contains("filled"), "edit did not land: {v}");
        // One undo (== reported steps) restores the empty-paragraph state:
        // "filled" is gone but "kept" remains.
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("kept") && !v.contains("filled"), "{v}");
    }

    #[test]
    fn ctl_rejects_unknown_verbs_and_bad_args() {
        let mut s = Session::open(&sample_docx("x")).expect("open");
        assert!(
            s.ctl(r#"{"verb":"doc.nope","args":{}}"#)
                .contains("\"ok\":false")
        );
        assert!(
            s.ctl(r#"{"verb":"doc.replace-range","args":{"start":99,"text":"y"}}"#)
                .contains("out of bounds")
        );
    }

    #[test]
    fn ctl_insert_and_append_and_blocks() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.append","args":{"text":"B"}}"#);
        assert!(
            out.contains("\"total\":2") && out.contains("\"ok\":true"),
            "{out}"
        );
        let out = s.ctl(r#"{"verb":"doc.insert","args":{"at":1,"text":"X"}}"#);
        assert!(out.contains("\"total\":3"), "{out}");
        let out = s.ctl(r#"{"verb":"doc.blocks","args":{}}"#);
        assert!(
            out.contains("\"total\":3")
                && out.contains("\"modified\":true")
                && out.contains("\"ok\":true"),
            "{out}"
        );
    }

    #[test]
    fn ctl_find_reports_block_and_text() {
        let bytes = sample_docx("hello world");
        let mut s = Session::open(&bytes).expect("open");
        let out = s.ctl(r#"{"verb":"doc.find","args":{"query":"world"}}"#);
        assert!(
            out.contains("\"count\":1")
                && out.contains("\"block\":0")
                && out.contains("\"ok\":true"),
            "{out}"
        );
    }

    // ---- Wave-1 ctl verb mirrors (mirrors docxy control.rs byte-for-byte) --

    /// A `.docx` with several plain-text paragraphs (real bytes, so `Session::open`
    /// exercises the full load path). Mirrors `sample_docx` but multi-paragraph.
    fn sample_docx_multi(paras: &[&str]) -> Vec<u8> {
        let ps: String = paras
            .iter()
            .map(|t| format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"))
            .collect();
        let xml = format!(
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body>{ps}</w:body></w:document>"
        );
        let doc = docxcore::load::parse_document_xml(&xml, &Default::default());
        save_package(&new_package(doc))
    }

    fn view_text(s: &mut Session) -> String {
        s.view_json(None)
    }

    /// The live document's plain text, via `doc.export` — unlike
    /// `view_json`, this carries no transient view state (`dirty`, caret,
    /// selection), so it's the right comparison for "content restored"
    /// assertions where `dispatch("undo")`'s own dirty-flag side effect (it
    /// always marks the session dirty, whether or not the undo stack was
    /// actually non-empty — a pre-existing `dispatch` property, not part of
    /// this task's contract) would otherwise make an exact `view_json`
    /// equality check spuriously fail.
    fn doc_text(s: &mut Session) -> String {
        let out = s.ctl(r#"{"verb":"doc.export","args":{"format":"text"}}"#);
        let marker = "\"text\":\"";
        let start = out.find(marker).expect("text field") + marker.len();
        let end = out[start..].find('"').expect("closing quote") + start;
        out[start..end].to_string()
    }

    #[test]
    fn ctl_export_returns_live_markdown_and_text() {
        let bytes = sample_docx("Hello world");
        let mut s = Session::open(&bytes).expect("open");
        let out = s.ctl(r#"{"verb":"doc.export","args":{"format":"text"}}"#);
        assert!(
            out.contains("\"format\":\"text\"")
                && out.contains("Hello world")
                && out.contains("\"ok\":true"),
            "{out}"
        );
        let out_md = s.ctl(r#"{"verb":"doc.export","args":{"format":"markdown"}}"#);
        assert!(
            out_md.contains("\"format\":\"markdown\"") && out_md.contains("\"ok\":true"),
            "{out_md}"
        );
    }

    #[test]
    fn ctl_export_requires_format_and_rejects_unknown() {
        let mut s = Session::open(&sample_docx("x")).expect("open");
        let err = s.ctl(r#"{"verb":"doc.export","args":{}}"#);
        assert!(
            err.contains("\"ok\":false") && err.contains("format"),
            "{err}"
        );
        let err2 = s.ctl(r#"{"verb":"doc.export","args":{"format":"rtf"}}"#);
        assert!(err2.contains("unknown format 'rtf'"), "{err2}");
    }

    #[test]
    fn ctl_comments_notes_header_footer_empty_shape_on_plain_fixture() {
        let mut s = Session::open(&sample_docx("x")).expect("open");
        let c = s.ctl(r#"{"verb":"doc.comments","args":{}}"#);
        assert!(
            c.contains("\"comments\":[]") && c.contains("\"ok\":true"),
            "{c}"
        );
        let n = s.ctl(r#"{"verb":"doc.notes","args":{}}"#);
        assert!(
            n.contains("\"notes\":[]") && n.contains("\"ok\":true"),
            "{n}"
        );
        let h = s.ctl(r#"{"verb":"doc.header","args":{}}"#);
        assert!(
            h.contains("\"blocks\":[]") && h.contains("\"ok\":true"),
            "{h}"
        );
        let f = s.ctl(r#"{"verb":"doc.footer","args":{}}"#);
        assert!(
            f.contains("\"blocks\":[]") && f.contains("\"ok\":true"),
            "{f}"
        );
    }

    #[test]
    fn ctl_header_resolves_the_default_section_header_content() {
        // A real header part wired through sectPr -> document.xml.rels ->
        // word/header1.xml, proving `header_footer_blocks`'s call into the
        // shared `docxcore::load::resolve_header_footer` (also used by docxy
        // main.rs's `load_hdr_ftr`) actually resolves content, not just that
        // it degrades to the empty-shape default.
        let document_xml = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>body text</w:t></w:r></w:p>
            <w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr>
        </w:body></w:document>"#;
        let doc_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#;
        let header_xml = r#"<?xml version="1.0"?><w:hdr xmlns:w="x"><w:p><w:r><w:t>Confidential</w:t></w:r></w:p></w:hdr>"#;
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let root_rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let bytes = docxcore::zipwrite::write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), root_rels.into()),
            ("word/document.xml".into(), document_xml.into()),
            ("word/_rels/document.xml.rels".into(), doc_rels.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("word/header1.xml".into(), header_xml.into()),
        ]);
        let mut s = Session::open(&bytes).expect("open");
        let out = s.ctl(r#"{"verb":"doc.header","args":{}}"#);
        assert!(out.contains("Confidential"), "{out}");
        assert!(out.contains("\"kind\":\"paragraph\""), "{out}");
        assert!(out.contains("\"index\":0"), "{out}");
        // The footer verb must not pick up the header content.
        let f = s.ctl(r#"{"verb":"doc.footer","args":{}}"#);
        assert!(f.contains("\"blocks\":[]"), "{f}");
    }

    #[test]
    fn ctl_metadata_empty_on_plain_fixture() {
        let mut s = Session::open(&sample_docx("x")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.metadata","args":{}}"#);
        assert_eq!(out, "{\"ok\":true}", "{out}");
    }

    #[test]
    fn ctl_metadata_populated_pins_wire_shape_and_key_set() {
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
        let bytes = docxcore::zipwrite::write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), rels.into()),
            ("word/document.xml".into(), document_xml.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("docProps/core.xml".into(), core_xml.into()),
        ]);
        let mut s = Session::open(&bytes).expect("open");
        let out = s.ctl(r#"{"verb":"doc.metadata","args":{}}"#);
        assert!(out.contains("\"title\":\"Q3 Report\""), "{out}");
        assert!(out.contains("\"author\":\"Ann\""), "{out}");
        // Pins format_iso's y-m-d-h-m-s field order, matching control.rs exactly.
        assert!(
            out.contains("\"created\":\"2020-01-02T03:04:05Z\""),
            "{out}"
        );
        assert!(
            !out.contains("\"subject\""),
            "empty dc:subject must be omitted: {out}"
        );
        assert!(
            !out.contains("\"modified\""),
            "unset field must be omitted: {out}"
        );
        assert!(out.contains("\"ok\":true"), "{out}");
    }

    #[test]
    fn ctl_stats_counts_words_chars_paragraphs_blocks_exact_key_set() {
        let bytes = sample_docx_multi(&["one two", "three"]);
        let mut s = Session::open(&bytes).expect("open");
        let out = s.ctl(r#"{"verb":"doc.stats","args":{}}"#);
        assert!(out.contains("\"words\":3"), "{out}");
        assert!(out.contains("\"chars\":12"), "{out}");
        assert!(out.contains("\"paragraphs\":2"), "{out}");
        assert!(out.contains("\"blocks\":2"), "{out}");
        assert!(out.contains("\"ok\":true"), "{out}");
        let obj = out.trim_start_matches('{').trim_end_matches('}');
        let keys: Vec<&str> = obj
            .split(',')
            .map(|kv| kv.split(':').next().unwrap())
            .collect();
        assert_eq!(
            keys.len(),
            5,
            "exact key set (words,chars,paragraphs,blocks,ok): {out}"
        );
    }

    #[test]
    fn ctl_replace_all_undo_integrity_one_undo_restores_prestate() {
        let bytes = sample_docx_multi(&["a foo b foo c", "foo"]);
        let mut s = Session::open(&bytes).expect("open");
        let before = doc_text(&mut s);
        let out = s.ctl(r#"{"verb":"doc.replace-all","args":{"query":"foo","text":"BAR"}}"#);
        assert!(out.contains("\"replaced\":3"), "{out}");
        assert!(out.contains("\"undoSteps\":1"), "{out}");
        assert!(out.contains("\"ok\":true"), "{out}");
        // internal field must never carry over into a byte-identical reply
        // shape check against control.rs's `{replaced}` — proven separately
        // by ctlserver.ts stripping it; here we only assert it's present.
        let after = doc_text(&mut s);
        assert!(after.contains("BAR"), "{after}");
        // One dispatch("undo") (== reported undoSteps) restores pre-state.
        s.dispatch("undo");
        assert_eq!(doc_text(&mut s), before);
    }

    #[test]
    fn ctl_replace_all_zero_match_is_side_effect_free() {
        let bytes = sample_docx("hello world");
        let mut s = Session::open(&bytes).expect("open");
        let before = doc_text(&mut s);
        let was_dirty = s.is_dirty();
        let out = s.ctl(r#"{"verb":"doc.replace-all","args":{"query":"xyz","text":"BAR"}}"#);
        assert!(out.contains("\"replaced\":0"), "{out}");
        assert!(out.contains("\"undoSteps\":0"), "{out}");
        assert_eq!(
            s.is_dirty(),
            was_dirty,
            "a no-match replace-all must not dirty the session"
        );
        assert_eq!(doc_text(&mut s), before);
        // No checkpoint was pushed, so there is nothing to undo.
        assert_eq!(s.dispatch("undo"), None);
        assert_eq!(doc_text(&mut s), before);
    }

    #[test]
    fn ctl_undo_redo_report_done_and_revert_content() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let r = s.ctl(r#"{"verb":"doc.undo","args":{}}"#);
        assert!(
            r.contains("\"done\":false") && r.contains("\"undoSteps\":0"),
            "{r}"
        );

        s.ctl(r#"{"verb":"doc.replace-all","args":{"query":"A","text":"B"}}"#);
        assert!(view_text(&mut s).contains('B'));
        let r = s.ctl(r#"{"verb":"doc.undo","args":{}}"#);
        assert!(
            r.contains("\"done\":true") && r.contains("\"undoSteps\":0"),
            "{r}"
        );
        let v = view_text(&mut s);
        assert!(v.contains('A') && !v.contains('B'), "{v}");

        let r = s.ctl(r#"{"verb":"doc.redo","args":{}}"#);
        assert!(r.contains("\"done\":true") && r.contains("\"undoSteps\":0"));
        assert!(view_text(&mut s).contains('B'));
    }

    #[test]
    fn ctl_export_pdf_takes_no_path_and_returns_pdf_bytes_as_base64() {
        let mut s = Session::open(&sample_docx("hello world")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.export-pdf","args":{}}"#);
        assert!(out.contains("\"ok\":true"), "{out}");
        let marker = "\"pdfBase64\":\"";
        let start = out.find(marker).expect("pdfBase64 field") + marker.len();
        let end = out[start..].find('"').expect("closing quote") + start;
        let b64 = &out[start..end];
        assert!(!b64.is_empty());
        // Shared test-only decoder (json.rs::decode_base64_for_test) — the
        // inverse of `json::to_base64`, kept as a single copy rather than
        // duplicated per test module.
        let bytes = json::decode_base64_for_test(b64);
        assert!(
            bytes.starts_with(b"%PDF"),
            "decoded bytes don't start with %PDF: {:?}",
            &bytes[..bytes.len().min(16)]
        );
    }

    #[test]
    fn ctl_blocks_has_no_protection_or_watermark_keys_when_unset() {
        let mut s = Session::open(&sample_docx("x")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.blocks","args":{}}"#);
        assert!(!out.contains("\"protection\""), "{out}");
        assert!(!out.contains("\"watermark\""), "{out}");
    }

    // ---- Wave-2 markdown-formatted ctl writes (mirrors docxy control.rs) ---

    #[test]
    fn ctl_markdown_insert_round_trips_heading_list_table_link() {
        let mut s = Session::open(&sample_docx("Existing")).expect("open");
        let md = "# Notes\n\n- item one\n- item two\n\n\
                  | A | B |\n| --- | --- |\n| 1 | 2 |\n\n\
                  See [docs](https://example.com).";
        let out = s.ctl(&format!(
            r#"{{"verb":"doc.insert","args":{{"at":1,"text":{},"markdown":true}}}}"#,
            json::quote(md)
        ));
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(s.is_dirty());

        let exported = s.ctl(r#"{"verb":"doc.export","args":{"format":"markdown"}}"#);
        assert!(exported.contains("# Notes"), "heading missing: {exported}");
        assert!(
            exported.contains("item one") && exported.contains("item two"),
            "list items missing: {exported}"
        );
        assert!(exported.contains("| A | B |"), "table missing: {exported}");
        assert!(
            exported.contains("| 1 | 2 |"),
            "table row missing: {exported}"
        );
        assert!(
            exported.contains("[docs](https://example.com)"),
            "link missing: {exported}"
        );
        // The original paragraph stayed at index 0, ahead of the splice.
        assert_eq!(
            s.editor.doc.body[0].plain_text(),
            "Existing",
            "original paragraph must be untouched"
        );
    }

    #[test]
    fn ctl_markdown_flag_absent_or_false_matches_plain_text_insert() {
        let mut plain = Session::open(&sample_docx_multi(&["A", "B", "C"])).expect("open");
        plain.ctl(r#"{"verb":"doc.insert","args":{"at":1,"text":"X"}}"#);
        let plain_text = doc_text(&mut plain);

        let mut flag_false = Session::open(&sample_docx_multi(&["A", "B", "C"])).expect("open");
        flag_false.ctl(r#"{"verb":"doc.insert","args":{"at":1,"text":"X","markdown":false}}"#);
        assert_eq!(doc_text(&mut flag_false), plain_text);
    }

    #[test]
    fn ctl_markdown_insert_is_a_single_undo_step() {
        let mut s = Session::open(&sample_docx("existing")).expect("open");
        let before = doc_text(&mut s);
        let out = s.ctl(
            r###"{"verb":"doc.insert","args":{"at":0,"text":"## Heading","markdown":true}}"###,
        );
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(doc_text(&mut s).contains("Heading"));
        // One dispatch("undo") fully restores the prior document.
        s.dispatch("undo");
        assert_eq!(doc_text(&mut s), before);
    }

    #[test]
    fn ctl_markdown_append_is_a_single_undo_step() {
        let mut s = Session::open(&sample_docx("existing")).expect("open");
        let before = doc_text(&mut s);
        let out =
            s.ctl(r###"{"verb":"doc.append","args":{"text":"## Heading","markdown":true}}"###);
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(doc_text(&mut s).contains("Heading"));
        s.dispatch("undo");
        assert_eq!(doc_text(&mut s), before);
    }

    #[test]
    fn ctl_markdown_replace_range_reports_and_undoes_matching_step_counts() {
        // Non-empty range (two populated paragraphs) -> 2 undo steps.
        let mut s = Session::open(&sample_docx_multi(&["A", "B", "C", "D"])).expect("open");
        let before = doc_text(&mut s);
        let out = s.ctl(
            r##"{"verb":"doc.replace-range","args":{"start":1,"end":2,"text":"# X\n\nY","markdown":true}}"##,
        );
        assert!(out.contains("\"replaced\":2"), "{out}");
        assert!(out.contains("\"undoSteps\":2"), "{out}");
        assert!(doc_text(&mut s).contains('X') && doc_text(&mut s).contains('Y'));
        s.dispatch("undo");
        s.dispatch("undo");
        assert_eq!(doc_text(&mut s), before);

        // A single EMPTY paragraph range -> 1 undo step.
        let mut s2 = Session::open(&sample_docx_multi(&["keep", "", "tail"])).expect("open");
        let before2 = doc_text(&mut s2);
        let out2 = s2.ctl(
            r#"{"verb":"doc.replace-range","args":{"start":1,"text":"filled","markdown":true}}"#,
        );
        assert!(out2.contains("\"replaced\":1"), "{out2}");
        assert!(
            out2.contains("\"undoSteps\":1"),
            "empty-paragraph replace must report a single undo step: {out2}"
        );
        assert!(doc_text(&mut s2).contains("filled"));
        s2.dispatch("undo");
        assert_eq!(doc_text(&mut s2), before2);
    }

    #[test]
    fn ctl_empty_markdown_insert_errors_and_leaves_no_tracks() {
        let mut s = Session::open(&sample_docx_multi(&["A", "B"])).expect("open");
        let before = doc_text(&mut s);
        let was_dirty = s.is_dirty();
        let out = s.ctl(r#"{"verb":"doc.insert","args":{"at":1,"text":"   \n","markdown":true}}"#);
        assert!(out.contains("\"ok\":false"), "{out}");
        assert!(out.contains("empty markdown"), "{out}");
        assert_eq!(s.is_dirty(), was_dirty, "an errored splice must not dirty");
        assert_eq!(doc_text(&mut s), before);
        // Nothing was checkpointed.
        assert_eq!(s.dispatch("undo"), None);
        assert_eq!(doc_text(&mut s), before);
    }

    #[test]
    fn ctl_markdown_out_of_bounds_insert_leaves_package_untouched() {
        // Regression guard mirroring docxy control.rs's
        // `out_of_bounds_markdown_list_insert_leaves_the_package_untouched`:
        // validation must run before any numbering/style package mutation.
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let before: Vec<String> = s.pkg.part_names().into_iter().map(String::from).collect();
        assert!(!before.iter().any(|n| n == "word/numbering.xml"));

        let out =
            s.ctl(r#"{"verb":"doc.insert","args":{"at":99,"text":"- item","markdown":true}}"#);
        assert!(out.contains("out of bounds"), "{out}");

        let after: Vec<String> = s.pkg.part_names().into_iter().map(String::from).collect();
        assert_eq!(
            after, before,
            "a rejected splice must not mutate the package"
        );
        assert!(!s.is_dirty());
    }

    #[test]
    fn ctl_markdown_list_ensures_numbering_and_remaps_off_the_bare_ids() {
        let mut s = Session::open(&sample_docx("Existing")).expect("open");
        assert!(
            !s.pkg.part_names().contains(&"word/numbering.xml"),
            "fixture must start without numbering"
        );
        let out = s.ctl(
            r#"{"verb":"doc.append","args":{"text":"- one\n- two\n\n1. first\n2. second","markdown":true}}"#,
        );
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(
            s.pkg.part_names().contains(&"word/numbering.xml"),
            "numbering part must be created on demand: {:?}",
            s.pkg.part_names()
        );
        let num_ids: Vec<i32> = s
            .editor
            .doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => p.props.num_id,
                _ => None,
            })
            .collect();
        assert_eq!(num_ids.len(), 4, "{num_ids:?}");
        assert!(
            num_ids.iter().all(|&id| id != 1 && id != 2),
            "list paragraphs must be remapped off markdown's bare ids: {num_ids:?}"
        );
    }

    #[test]
    fn ctl_markdown_heading_into_a_fresh_package_ensures_heading1() {
        let mut s = Session::open(&sample_docx("Existing")).expect("open");
        let styles_before =
            String::from_utf8_lossy(s.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(!styles_before.contains("Heading1"), "{styles_before}");

        let out =
            s.ctl(r##"{"verb":"doc.insert","args":{"at":0,"text":"# Title","markdown":true}}"##);
        assert!(out.contains("\"ok\":true"), "{out}");

        let styles_after =
            String::from_utf8_lossy(s.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(
            styles_after.contains("Heading1"),
            "Heading1 must be ensured: {styles_after}"
        );
    }

    #[test]
    fn ctl_blocks_reports_protection_and_watermark_when_set() {
        // documentProtection (read-only enforcement) + a VML textpath watermark,
        // both read straight off `Package::protection`/`Package::watermark` —
        // the same core.rs surface docxy's `doc.path` uses, so `doc.blocks`
        // (which feeds the tab's `doc.path` composition) must mirror it.
        let document_xml =
            "<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body><w:p/></w:body></w:document>";
        let settings_xml = r#"<?xml version="1.0"?><w:settings xmlns:w="x"><w:documentProtection w:edit="readOnly" w:enforcement="1"/></w:settings>"#;
        let header_xml = r#"<?xml version="1.0"?><w:hdr xmlns:v="y"><w:p><v:textpath string="CONFIDENTIAL"/></w:p></w:hdr>"#;
        let ct = r#"<?xml version="1.0"?><Types/>"#;
        let rels = r#"<?xml version="1.0"?><Relationships><Relationship Id="rId1" Target="word/document.xml"/></Relationships>"#;
        let bytes = docxcore::zipwrite::write_zip(&[
            ("[Content_Types].xml".into(), ct.into()),
            ("_rels/.rels".into(), rels.into()),
            ("word/document.xml".into(), document_xml.into()),
            ("word/styles.xml".into(), "<w:styles/>".into()),
            ("word/settings.xml".into(), settings_xml.into()),
            ("word/header1.xml".into(), header_xml.into()),
        ]);
        let mut s = Session::open(&bytes).expect("open");
        let out = s.ctl(r#"{"verb":"doc.blocks","args":{}}"#);
        assert!(out.contains("\"protection\":\"read-only\""), "{out}");
        assert!(out.contains("\"watermark\":\"CONFIDENTIAL\""), "{out}");
    }

    // ---- Wave-3 doc.format / doc.set-style (mirrors docxy control.rs) -----

    /// The bold flags of every run in the first paragraph, in order — used to
    /// spot-check `doc.format`'s SET-to-value determinism (not toggle).
    fn run_bold_flags(s: &Session, block: usize) -> Vec<bool> {
        let Block::Paragraph(p) = &s.editor.doc.body[block] else {
            panic!("expected paragraph at {block}")
        };
        p.content
            .iter()
            .filter_map(|inl| match inl {
                Inline::Run(r) => Some(r.props.bold),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ctl_format_reply_is_exactly_formatted_and_ok_no_undo_steps() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"bold":true}}}"#);
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains("\"formatted\":1"), "{out}");
        assert!(
            !out.contains("undoSteps"),
            "doc.format must not carry an undoSteps field: {out}"
        );
        let obj = out.trim_start_matches('{').trim_end_matches('}');
        let keys: Vec<&str> = obj
            .split(',')
            .map(|kv| kv.split(':').next().unwrap())
            .collect();
        assert_eq!(keys.len(), 2, "exact key set (formatted, ok): {out}");
    }

    #[test]
    fn ctl_set_style_reply_is_exactly_styled_and_ok_no_undo_steps() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let out = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"style":"Quote"}}"#);
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains("\"styled\":1"), "{out}");
        assert!(
            !out.contains("undoSteps"),
            "doc.set-style must not carry an undoSteps field: {out}"
        );
        let obj = out.trim_start_matches('{').trim_end_matches('}');
        let keys: Vec<&str> = obj
            .split(',')
            .map(|kv| kv.split(':').next().unwrap())
            .collect();
        assert_eq!(keys.len(), 2, "exact key set (styled, ok): {out}");
    }

    #[test]
    fn ctl_format_bold_true_then_false_is_set_not_toggle() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"bold":true}}}"#);
        assert_eq!(run_bold_flags(&s, 0), vec![true]);
        // Applying bold:true again is a no-op — SET semantics, not toggle.
        s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"bold":true}}}"#);
        assert_eq!(run_bold_flags(&s, 0), vec![true]);
        s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"bold":false}}}"#);
        assert_eq!(run_bold_flags(&s, 0), vec![false]);
    }

    #[test]
    fn ctl_format_one_undo_restores_exact_prior_document() {
        let mut s = Session::open(&sample_docx_multi(&["A", "B"])).expect("open");
        let before = s.editor.doc.clone();
        let out = s.ctl(
            r##"{"verb":"doc.format","args":{"start":0,"end":1,"patch":{"bold":true,"italic":true,"color":"#00FF00"}}}"##,
        );
        assert!(out.contains("\"formatted\":2"), "{out}");
        assert_ne!(s.editor.doc, before);
        s.dispatch("undo");
        assert_eq!(
            s.editor.doc, before,
            "one undo must restore exact prior run props"
        );
    }

    #[test]
    fn ctl_set_style_one_undo_restores_exact_prior_document() {
        let mut s = Session::open(&sample_docx_multi(&["A", "B"])).expect("open");
        let before = s.editor.doc.clone();
        let out = s.ctl(
            r#"{"verb":"doc.set-style","args":{"start":0,"end":1,"style":"Heading1","align":"center"}}"#,
        );
        assert!(out.contains("\"styled\":2"), "{out}");
        assert_ne!(s.editor.doc, before);
        s.dispatch("undo");
        assert_eq!(
            s.editor.doc, before,
            "one undo must restore exact prior style/align"
        );
    }

    #[test]
    fn ctl_set_style_heading1_ensures_the_style_part_on_a_bare_package() {
        let mut s = Session::open(&sample_docx("Title")).expect("open");
        let styles_before =
            String::from_utf8_lossy(s.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(!styles_before.contains("Heading1"), "{styles_before}");

        let out = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"style":"Heading1"}}"#);
        assert!(out.contains("\"styled\":1"), "{out}");
        assert!(out.contains("\"ok\":true"), "{out}");

        let styles_after =
            String::from_utf8_lossy(s.pkg.part("word/styles.xml").unwrap()).into_owned();
        assert!(
            styles_after.contains("w:styleId=\"Heading1\""),
            "{styles_after}"
        );
        assert!(s.is_dirty());
    }

    #[test]
    fn ctl_set_style_normal_only_does_not_ensure_styles() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let before = s.pkg.part("word/styles.xml").unwrap().to_vec();
        let out = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"style":"Normal"}}"#);
        assert!(out.contains("\"ok\":true"), "{out}");
        assert_eq!(
            s.pkg.part("word/styles.xml").unwrap(),
            before.as_slice(),
            "Normal must not touch styles.xml"
        );
    }

    #[test]
    fn ctl_set_style_align_only_does_not_ensure_styles() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        let before = s.pkg.part("word/styles.xml").unwrap().to_vec();
        let out = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"align":"center"}}"#);
        assert!(out.contains("\"styled\":1"), "{out}");
        assert_eq!(
            s.pkg.part("word/styles.xml").unwrap(),
            before.as_slice(),
            "an align-only call must not touch styles.xml"
        );
    }

    #[test]
    fn ctl_format_and_set_style_dirty_flag_contract() {
        let mut s = Session::open(&sample_docx("A")).expect("open");
        assert!(!s.is_dirty());

        // A validation failure must leave the session untouched.
        let err = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{}}}"#);
        assert!(err.contains("\"ok\":false"), "{err}");
        assert!(
            !s.is_dirty(),
            "a rejected format must not dirty the session"
        );

        let err2 = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0}}"#);
        assert!(err2.contains("\"ok\":false"), "{err2}");
        assert!(
            !s.is_dirty(),
            "a rejected set-style must not dirty the session"
        );

        // Success dirties the session.
        let ok = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"bold":true}}}"#);
        assert!(ok.contains("\"ok\":true"), "{ok}");
        assert!(s.is_dirty());
    }

    #[test]
    fn ctl_format_error_strings_match_control_rs_family() {
        let mut s = Session::open(&sample_docx("A")).expect("open");

        let err = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{}}}"#);
        assert!(err.contains("patch needs at least one key"), "{err}");

        let err = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"nope":true}}}"#);
        assert!(err.contains("unknown patch key 'nope'"), "{err}");

        let err = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"color":"red"}}}"#);
        assert!(
            err.contains("bad color 'red' (want \\\"#RRGGBB\\\")"),
            "{err}"
        );

        let err =
            s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"highlight":"chartreuse"}}}"#);
        assert!(err.contains("bad highlight 'chartreuse'"), "{err}");
        for name in agent::HIGHLIGHT_NAMES {
            assert!(err.contains(name), "{err} missing {name}");
        }
        assert!(err.contains("or none"), "{err}");

        let err = s.ctl(r#"{"verb":"doc.format","args":{"start":0,"patch":{"size":"abc"}}}"#);
        assert!(err.contains("bad size 'abc'"), "{err}");
    }

    #[test]
    fn ctl_set_style_error_strings_match_control_rs_family() {
        let mut s = Session::open(&sample_docx("A")).expect("open");

        let err = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0}}"#);
        assert_eq!(err, ctl_err("set-style needs 'style' or 'align'"), "{err}");

        let err = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"style":"Bogus"}}"#);
        assert!(err.contains("Bogus"), "{err}");
        for id in agent::MARKDOWN_PARAGRAPH_STYLE_IDS {
            assert!(err.contains(id), "{err} missing {id}");
        }
        assert!(err.contains("Normal"), "{err}");

        let err = s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"align":"middle"}}"#);
        assert!(err.contains("bad align 'middle'"), "{err}");
        assert!(err.contains("left/center/right/justify"), "{err}");
    }

    #[test]
    fn ctl_format_and_set_style_round_trip_through_markdown_export() {
        let mut s = Session::open(&sample_docx_multi(&["Title", "body text"])).expect("open");
        s.ctl(r#"{"verb":"doc.set-style","args":{"start":0,"style":"Heading1"}}"#);
        s.ctl(r#"{"verb":"doc.format","args":{"start":1,"patch":{"bold":true}}}"#);
        let out = s.ctl(r#"{"verb":"doc.export","args":{"format":"markdown"}}"#);
        assert!(out.contains("\"ok\":true"), "{out}");
        assert!(out.contains("# Title"), "{out}");
        assert!(out.contains("**body text**"), "{out}");
    }
}
