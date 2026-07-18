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
use docxcore::load::{Relationships, parse_rels_xml};
use docxcore::model::{Align, Block};
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
        let (lines, maps, images) = render::render_with_images(&self.editor.doc, &opts);
        let (cl, cc) = caret_screen(&maps, &self.editor.caret);
        self.maps = maps;

        let mut out = String::with_capacity(lines.len() * 48 + 64);
        out.push_str("{\"lines\":[");
        for (li, line) in lines.iter().enumerate() {
            if li > 0 {
                out.push(',');
            }
            out.push('[');
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

    /// `{start, end?, text}` -> `{replaced, total, undoSteps}`
    ///
    /// `undoSteps` is an **internal** field for the extension host only: the
    /// number of native undo checkpoints this edit pushed (2 for a normal
    /// delete-then-insert, 1 when the replaced range was a single empty
    /// paragraph and no delete happened). `CtlServer.callWasm` strips it before
    /// the reply hits the TCP wire — a VS Code tab's `doc.replace-range` reply
    /// must be byte-for-byte a terminal docxy's `{replaced, total}` — and hands
    /// the count to `host.onMutated` so the tab replays exactly that many
    /// wasm undos per VS Code undo (see `docxcore::agent::replace_range`).
    fn ctl_replace_range(&mut self, args: &json::Json) -> Result<String, String> {
        let start = args
            .get_usize("start")
            .ok_or("doc.replace-range needs a 'start' index")?;
        let end = args.get_usize("end").unwrap_or(start);
        let text = args
            .get_str("text")
            .ok_or("doc.replace-range needs 'text'")?;
        let (replaced, undo_steps) = agent::replace_range(&mut self.editor, start, end, text)?;
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

    /// `{at, text}` -> `{total}`
    fn ctl_insert(&mut self, args: &json::Json) -> Result<String, String> {
        let at = args
            .get_usize("at")
            .ok_or("doc.insert needs an 'at' index")?;
        let text = args.get_str("text").ok_or("doc.insert needs 'text'")?;
        agent::insert(&mut self.editor, at, text)?;
        self.finish_ctl_edit();
        let mut out = String::from("{\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push('}');
        Ok(out)
    }

    /// `{text}` -> `{total}`
    fn ctl_append(&mut self, args: &json::Json) -> Result<String, String> {
        let text = args.get_str("text").ok_or("doc.append needs 'text'")?;
        agent::append(&mut self.editor, text);
        self.finish_ctl_edit();
        let mut out = String::from("{\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push('}');
        Ok(out)
    }

    /// `{}` -> `{total, modified}` (the host composes this with URI info for
    /// its own `doc.path`-equivalent reply).
    fn ctl_blocks(&self) -> String {
        let mut out = String::from("{\"total\":");
        out.push_str(&self.editor.doc.body.len().to_string());
        out.push_str(",\"modified\":");
        out.push_str(if self.dirty { "true" } else { "false" });
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
/// completing the success envelope.
fn ctl_ok(body: String) -> String {
    let mut s = body;
    s.pop(); // trailing '}'
    s.push_str(",\"ok\":true}");
    s
}

/// The ctl failure envelope: `{"ok":false,"error":"…"}`.
fn ctl_err(msg: &str) -> String {
    let mut out = String::from("{\"ok\":false,\"error\":");
    json::push_str(&mut out, msg);
    out.push('}');
    out
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
}
