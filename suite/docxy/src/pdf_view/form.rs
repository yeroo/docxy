//! Links, comments and the form in the PDF view (#2255), without gpui:
//! what is under the pointer, a link followed, a destination gone to, and
//! filling the form's fields by mouse and keyboard.
//!
//! The view never draws a field's value itself, except while it is typed
//! into: a value goes to the worker as [`Op::SetField`], which fills the
//! working document (PdfCraft's `set_value`) and names the pages whose
//! fields changed; those are drawn again, the old raster shown until then.

use super::worker::{APP_NAME, FieldKind, FieldMeta, FieldValue, LinkGo, Op};
use super::*;
use crate::field_edit;
use std::ops::Range;

/// The field that has the keys, and the widget it was entered by.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FormFocus {
    pub field: usize,
    pub widget: usize,
    /// The text being typed: a text field, or an editable combo box.
    pub edit: Option<Field>,
    /// A combo box's list is open.
    pub open: bool,
    /// The option a combo box's open list or a list box is on.
    pub cursor: usize,
    /// The first option an open combo box's list shows: it shows
    /// [`LIST_ROWS`] at a time, scrolled by the wheel and following the
    /// cursor.
    pub top: usize,
}

/// The rows an open combo box's list shows at a time.
pub(crate) const LIST_ROWS: usize = 12;

/// The schemes a link may open in the browser or mail program.
pub(crate) fn opens(uri: &str) -> bool {
    let scheme = uri
        .split_once(':')
        .map(|(s, _)| s.trim().to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https" | "mailto"))
}

/// A worker's [`LinkGo::Other`] text with the app's name
/// ([`crate::brand::short`]) where it names the app.
pub(crate) fn named(what: &str) -> String {
    what.replace(APP_NAME, crate::brand::short())
}

/// What a link says on hover.
pub(crate) fn link_tip(go: &LinkGo, labels: &[PageMeta]) -> String {
    match go {
        LinkGo::Uri { uri } => uri.clone(),
        LinkGo::Page { page, .. } => {
            let label = labels
                .get(*page)
                .map(|p| p.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| (page + 1).to_string());
            format!("Go to page {label}")
        }
        LinkGo::Other { what } => format!("Not followed here: {}", named(what)),
    }
}

fn contains(r: [f32; 4], x: f32, y: f32) -> bool {
    x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3]
}

impl PdfView {
    pub(crate) fn field(&self, f: usize) -> Option<&FieldMeta> {
        self.form.fields.get(f)
    }

    /// The page and point (points, view space) at content `(x, y)`, only
    /// when it is on a page.
    pub(crate) fn on_page(&self, x: f32, y: f32) -> Option<(usize, f32, f32)> {
        let (p, px, py) = self.hit(x, y)?;
        let m = self.meta.pages.get(p)?;
        (px >= 0. && py >= 0. && px <= m.w && py <= m.h).then_some((p, px, py))
    }

    /// What is at content `(x, y)`: a field's widget (not a hidden one)
    /// before a comment, before a link.
    pub(crate) fn under_at(&self, x: f32, y: f32) -> Under {
        let Some((page, px, py)) = self.on_page(x, y) else {
            return Under::Nothing;
        };
        for &(f, w) in self.widgets[page].iter().rev() {
            if let Some(widget) = self.form.fields[f].widgets.get(w)
                && !widget.hidden
                && contains(widget.rect, px, py)
            {
                return Under::Widget(f, w);
            }
        }
        if let Some(i) = self.notes[page]
            .iter()
            .rposition(|n| contains(n.rect, px, py))
        {
            return Under::Note(page, i);
        }
        if let Some(i) = self.links[page].iter().rposition(|l| {
            if l.quads.is_empty() {
                contains(l.rect, px, py)
            } else {
                l.quads
                    .iter()
                    .any(|q| pdfcraft_render::quad_contains(q, px, py))
            }
        }) {
            return Under::Link(page, i);
        }
        Under::Nothing
    }

    /// The tooltip for what the pointer is over: a link's address or page,
    /// a field's tooltip and whether it is required.
    pub(crate) fn tooltip(&self) -> Option<String> {
        match self.under {
            Under::Link(p, i) => Some(link_tip(&self.links[p][i].go, &self.meta.pages)),
            Under::Widget(f, _) => {
                let field = self.field(f)?;
                let mut tip = field.tooltip.clone().unwrap_or_default();
                if field.required && field.fillable() {
                    if !tip.is_empty() {
                        tip.push_str(" · ");
                    }
                    tip.push_str("Required");
                }
                (!tip.is_empty()).then_some(tip)
            }
            _ => None,
        }
    }

    /// The pointer moved to content `(x, y)` (no button down): what it is
    /// over, and the comment pop-up that follows it. `true` when something
    /// shown changed.
    pub(crate) fn hover(&mut self, x: f32, y: f32) -> bool {
        let under = self.under_at(x, y);
        let before = (self.under, self.popup);
        self.pointer = (x, y);
        self.under = under;
        if !self.popup.is_some_and(|p| p.pinned) {
            self.popup = match under {
                Under::Note(page, note) => Some(NotePopup {
                    page,
                    note,
                    pinned: false,
                }),
                _ => None,
            };
        }
        before != (self.under, self.popup) || self.tooltip().is_some()
    }

    // ---- pressing ---------------------------------------------------------------

    /// A press on the pages at content `(x, y)`: a widget takes it, then a
    /// comment (its pop-up pinned or let go), then a link (followed on the
    /// release unless a drag starts), then the text. `ctrl` adds a list
    /// box's option to its picks.
    pub(crate) fn press_at(&mut self, x: f32, y: f32, clicks: usize, shift: bool, ctrl: bool) {
        self.link_press = None;
        match self.under_at(x, y) {
            Under::Widget(f, w) if self.field(f).is_some_and(FieldMeta::fillable) => {
                self.sel = None;
                self.drag = None;
                self.press_widget(f, w, x, y, ctrl);
                return;
            }
            Under::Note(page, note) => {
                self.blur();
                let pinned = self
                    .popup
                    .is_some_and(|p| p.pinned && p.page == page && p.note == note);
                self.popup = (!pinned).then_some(NotePopup {
                    page,
                    note,
                    pinned: true,
                });
                return;
            }
            Under::Link(page, link) => {
                self.link_press = Some(LinkPress {
                    page,
                    link,
                    at: (x, y),
                });
            }
            _ => {}
        }
        self.blur();
        if self.popup.is_some_and(|p| p.pinned) {
            self.popup = None;
        }
        self.press(x, y, clicks, shift);
    }

    /// The pointer moved with the button down: past [`DRAG_SLOP`] a press
    /// on a link is a drag that selects.
    pub(crate) fn drag_at(&mut self, x: f32, y: f32) {
        if let Some(lp) = self.link_press
            && ((x - lp.at.0).abs() > DRAG_SLOP || (y - lp.at.1).abs() > DRAG_SLOP)
        {
            self.link_press = None;
        }
        if self.link_press.is_none() {
            self.drag_to(x, y);
        }
    }

    /// The button came up: a press on a link that did not drag follows it.
    pub(crate) fn release_at(&mut self) {
        self.release();
        if let Some(lp) = self.link_press.take() {
            self.sel = None;
            self.follow_link(lp.page, lp.link);
        }
    }

    /// Follow link `link` of `page`: an address opens (the host opens it)
    /// when it is a web or mail one, a place in the document is gone to,
    /// anything else says why it was not followed.
    pub(crate) fn follow_link(&mut self, page: usize, link: usize) {
        let Some(l) = self.links.get(page).and_then(|l| l.get(link)) else {
            return;
        };
        match l.go.clone() {
            LinkGo::Uri { uri } if opens(&uri) => {
                self.notice = Some(format!("Opening {uri}"));
                self.open_url = Some(uri);
            }
            LinkGo::Uri { uri } => {
                self.notice = Some(format!(
                    "The link was not opened: {uri} is not a web or mail address"
                ));
            }
            LinkGo::Page {
                page,
                x,
                y,
                zoom,
                fit_width,
            } => self.go_dest(page, x, y, zoom, fit_width),
            LinkGo::Other { what } => {
                self.notice = Some(format!("The link was not followed: it is {}", named(&what)));
            }
        }
    }

    /// Go to a destination: page `page`'s view-space point `(x, y)` at the
    /// body's top-left, at `zoom` (fit width when `fit_width`). A coordinate
    /// not given keeps the view's left, or goes to the page's top.
    pub(crate) fn go_dest(
        &mut self,
        page: usize,
        x: Option<f32>,
        y: Option<f32>,
        zoom: Option<f32>,
        fit_width: bool,
    ) {
        if page >= self.page_count() {
            return;
        }
        if fit_width {
            self.fit_width();
        } else if let Some(z) = zoom {
            self.set_zoom(z);
        }
        let l = self.layout();
        let r = l.pages[page];
        let k = self.zoom * PX_PER_PT;
        let (cx, _) = self.pending_or_top();
        let nx = x.map_or(cx, |x| r.x + x * k);
        let ny = y.map_or(r.y - GAP / 2., |y| r.y + y * k);
        self.scroll_to_xy(nx, ny);
    }

    // ---- focus and values ---------------------------------------------------------

    /// Give widget `widget` of field `field` the keys: a text field (or an
    /// editable combo box) to type in, its value selected. A read-only or
    /// hidden one takes no focus. The field that had it is committed.
    pub(crate) fn focus_widget(&mut self, field: usize, widget: usize) -> bool {
        let Some(f) = self.field(field) else {
            return false;
        };
        let Some(w) = f.widgets.get(widget) else {
            return false;
        };
        if !f.fillable() || w.read_only || w.hidden {
            return false;
        }
        if self
            .focus
            .as_ref()
            .is_some_and(|x| x.field == field && x.widget == widget)
        {
            return true;
        }
        let edit = matches!(f.kind, FieldKind::Text) || (f.kind == FieldKind::Combo && f.edit);
        let cursor = f.picked.first().copied().unwrap_or(0);
        // What the field will hold once the values on their way are in: a
        // field entered again before its fill was answered shows it.
        let text = self.expected_text(field);
        self.blur();
        let mut e = Field {
            text,
            ..Field::default()
        };
        e.focus_all();
        self.focus = Some(FormFocus {
            field,
            widget,
            edit: edit.then_some(e),
            open: false,
            cursor,
            top: cursor.saturating_sub(LIST_ROWS - 1),
        });
        self.find.field.focused = false;
        self.page_box = Field::default();
        self.reveal(field, widget);
        true
    }

    /// Fill field `field` with `value`: sent to the worker, which refuses a
    /// read-only field too.
    pub(crate) fn fill(&mut self, field: usize, value: FieldValue) {
        let Some(f) = self.field(field) else { return };
        if !f.fillable() {
            self.notice = Some(format!("{} is read-only", f.name));
            return;
        }
        let name = f.name.clone();
        self.form_ops.push_back(Op::SetField { name, value });
    }

    /// The typed text goes in, when it differs from the field's value.
    pub(crate) fn commit_focus(&mut self) {
        let Some(focus) = self.focus.as_ref() else {
            return;
        };
        let Some(edit) = focus.edit.as_ref() else {
            return;
        };
        let field = focus.field;
        if edit.text != self.expected_text(field) {
            let value = FieldValue::Text {
                text: edit.text.clone(),
            };
            self.fill(field, value);
        }
    }

    /// The text field `field`'s editor compares with: the text the last
    /// fill of it on its way sets (queued, or out with the worker), else
    /// what it holds. A combo box's picked option shows its text.
    pub(crate) fn expected_text(&self, field: usize) -> String {
        let Some(f) = self.field(field) else {
            return String::new();
        };
        match self.expected_value(field) {
            Some(FieldValue::Text { text }) => text.clone(),
            Some(FieldValue::Choice { picked }) => picked
                .first()
                .and_then(|i| f.options.get(*i))
                .map(|o| o.1.clone())
                .unwrap_or_default(),
            _ => shown_text(f),
        }
    }

    /// The last fill of field `field` still on its way: queued, else out
    /// with the worker (newest first). What a key or click builds on, so a
    /// quick second one does not start again from the value before the
    /// first (#2255 r2).
    pub(crate) fn expected_value(&self, field: usize) -> Option<&FieldValue> {
        let f = self.field(field)?;
        self.form_ops
            .iter()
            .rev()
            .chain(self.in_flight.as_ref())
            .find_map(|op| match op {
                Op::SetField { name, value } if *name == f.name => Some(value),
                _ => None,
            })
    }

    /// Whether widget `w` of field `f` will be on once the fills on their
    /// way are in.
    fn widget_on_expected(&self, f: usize, w: usize) -> bool {
        match self.expected_value(f) {
            Some(FieldValue::Check { on }) => *on,
            Some(FieldValue::Widget { index }) => *index == w,
            _ => self
                .field(f)
                .and_then(|x| x.widgets.get(w))
                .is_some_and(|x| x.on),
        }
    }

    /// The options field `f` will have picked once the fills on their way
    /// are in.
    fn picked_expected(&self, f: usize) -> Vec<usize> {
        match self.expected_value(f) {
            Some(FieldValue::Choice { picked }) => picked.clone(),
            _ => self.field(f).map(|x| x.picked.clone()).unwrap_or_default(),
        }
    }

    /// The field lets the keys go, its typed text committed.
    pub(crate) fn blur(&mut self) {
        self.commit_focus();
        self.focus = None;
    }

    /// Esc: an open list closes; else the typed text is dropped and the
    /// field lets the keys go.
    pub(crate) fn cancel_focus(&mut self) {
        match self.focus.as_mut() {
            Some(f) if f.open => f.open = false,
            Some(_) => self.focus = None,
            None => {}
        }
    }

    /// Whether the tab has values the file does not: accepted, waiting for
    /// the worker, or typed into the focused field and not yet committed.
    pub(crate) fn has_unsaved(&self) -> bool {
        self.unsaved() || self.fills_waiting() || self.typed_uncommitted()
    }

    /// Text typed into the focused field that differs from its value.
    fn typed_uncommitted(&self) -> bool {
        self.focus.as_ref().is_some_and(|x| {
            x.edit
                .as_ref()
                .is_some_and(|e| e.text != self.expected_text(x.field))
        })
    }

    /// Whether a form operation is waiting (a value set, not yet answered).
    pub(crate) fn fills_waiting(&self) -> bool {
        self.form_ops
            .iter()
            .any(|op| matches!(op, Op::SetField { .. }))
            || matches!(self.in_flight, Some(Op::SetField { .. }))
    }

    /// A press on widget `w` of field `f` at content `(x, y)`.
    fn press_widget(&mut self, f: usize, w: usize, x: f32, y: f32, ctrl: bool) {
        let Some(field) = self.field(f).cloned() else {
            return;
        };
        let was_focused = self
            .focus
            .as_ref()
            .is_some_and(|x| x.field == f && x.widget == w);
        if !self.focus_widget(f, w) {
            return;
        }
        match field.kind {
            FieldKind::Check => self.toggle(f, w),
            FieldKind::Radio => self.toggle(f, w),
            FieldKind::Combo => {
                if let Some(focus) = self.focus.as_mut() {
                    // A click on a focused editable box types; a click on a
                    // plain one (or the first on an editable one) opens or
                    // closes its list.
                    if !(field.edit && was_focused) {
                        focus.open = !focus.open;
                    }
                    if was_focused && let Some(e) = focus.edit.as_mut() {
                        e.sel = false;
                        e.caret = e.text.chars().count();
                    }
                }
            }
            FieldKind::List => {
                if let Some(option) = self.list_row_at(f, w, x, y) {
                    self.pick(option, ctrl);
                }
            }
            FieldKind::Text => {
                // A second press places the caret at the end, to append.
                if was_focused && let Some(e) = self.focus.as_mut().and_then(|x| x.edit.as_mut()) {
                    e.sel = false;
                    e.caret = e.text.chars().count();
                }
            }
            FieldKind::Button | FieldKind::Signature => {}
        }
    }

    /// The option of list box `f`'s widget `w` at content `(x, y)`, by the
    /// rows PdfCraft draws it with.
    fn list_row_at(&self, f: usize, w: usize, x: f32, y: f32) -> Option<usize> {
        let field = self.field(f)?;
        let widget = field.widgets.get(w)?;
        let (_, _, py) = self.on_page(x, y)?;
        let rows = widget.list.unwrap_or(worker::ListRowsMeta {
            top: 0,
            inset: 2.,
            row: (field.font_size.max(9.) * 1.15).max(1.),
        });
        let off = py - widget.rect[1] - rows.inset;
        if off < 0. || rows.row <= 0. {
            return None;
        }
        let i = rows.top + (off / rows.row) as usize;
        (i < field.options.len()).then_some(i)
    }

    /// A check box turned over, or a radio button turned on.
    pub(crate) fn toggle(&mut self, f: usize, w: usize) {
        let Some(field) = self.field(f) else { return };
        if field.widgets.get(w).is_none() {
            return;
        }
        let value = match (field.kind, self.widget_on_expected(f, w)) {
            (FieldKind::Check, true) => FieldValue::Check { on: false },
            (FieldKind::Check | FieldKind::Radio, false) => FieldValue::Widget { index: w },
            // A radio button on stays on.
            _ => return,
        };
        self.fill(f, value);
    }

    /// The focused choice field picks option `option`: a combo box or a
    /// single-pick list takes it alone; a multi-pick list adds it to its
    /// picks (or takes it out) with `ctrl`, else takes it alone.
    pub(crate) fn pick(&mut self, option: usize, ctrl: bool) {
        let Some(focus) = self.focus.as_mut() else {
            return;
        };
        focus.cursor = option;
        focus.open = false;
        let field = focus.field;
        let Some(f) = self.field(field) else { return };
        let before = self.picked_expected(field);
        let mut picked = vec![option];
        if f.kind == FieldKind::List && f.multi && ctrl {
            picked = before.clone();
            match picked.iter().position(|p| *p == option) {
                Some(i) => {
                    picked.remove(i);
                }
                None => picked.push(option),
            }
            picked.sort_unstable();
        }
        if let Some(e) = self.focus.as_mut().and_then(|x| x.edit.as_mut())
            && let Some(f) = self.form.fields.get(field)
        {
            e.text = f
                .options
                .get(option)
                .map(|o| o.1.clone())
                .unwrap_or_default();
            e.focus_all();
        }
        if before != picked {
            self.fill(field, FieldValue::Choice { picked });
        }
    }

    /// The fillable widgets in the document's tab order.
    pub(crate) fn tab_order(&self) -> Vec<(usize, usize)> {
        let mut order: Vec<(usize, usize, usize)> = self
            .form
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.fillable())
            .flat_map(|(fi, f)| {
                f.widgets
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| w.page.is_some() && !w.hidden && !w.read_only)
                    .map(move |(wi, w)| (w.tab, fi, wi))
            })
            .collect();
        order.sort_unstable();
        // A radio group is one stop: its first button, or the one on.
        let mut out: Vec<(usize, usize)> = Vec::new();
        for (_, f, w) in order {
            if self.form.fields[f].kind == FieldKind::Radio {
                if let Some(at) = out.iter().position(|(x, _)| *x == f) {
                    if self.form.fields[f].widgets[w].on {
                        out[at] = (f, w);
                    }
                    continue;
                }
            }
            out.push((f, w));
        }
        out
    }

    /// Tab (Shift+Tab): the next (previous) field in tab order, wrapping;
    /// from no field, the first (last). `false` when the form has none.
    pub(crate) fn tab_field(&mut self, back: bool) -> bool {
        let order = self.tab_order();
        if order.is_empty() {
            return false;
        }
        // The stop of the widget with the keys; a radio group's one stop
        // may be another of its buttons.
        let at = self.focus.as_ref().and_then(|x| {
            order
                .iter()
                .position(|&(f, w)| f == x.field && w == x.widget)
                .or_else(|| {
                    let radio = self
                        .field(x.field)
                        .is_some_and(|f| f.kind == FieldKind::Radio);
                    radio
                        .then(|| order.iter().position(|(f, _)| *f == x.field))
                        .flatten()
                })
        });
        let n = order.len();
        let next = match (at, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        let (f, w) = order[next];
        self.blur();
        self.focus_widget(f, w)
    }

    /// Bring widget `w` of field `f` into view.
    fn reveal(&mut self, f: usize, w: usize) {
        let Some(widget) = self.form.fields.get(f).and_then(|x| x.widgets.get(w)) else {
            return;
        };
        let Some(page) = widget.page.filter(|p| *p < self.page_count()) else {
            return;
        };
        let l = self.layout();
        let pr = l.pages[page];
        let k = self.zoom * PX_PER_PT;
        let (x, y) = self.pending_or_top();
        let (vw, vh) = self.viewport.get();
        let r = widget.rect;
        let (gx0, gy0, gx1, gy1) = (
            pr.x + r[0] * k,
            pr.y + r[1] * k,
            pr.x + r[2] * k,
            pr.y + r[3] * k,
        );
        let ny = if gy0 >= y && gy1 <= y + vh {
            y
        } else {
            gy0 - vh / 3.
        };
        let nx = if gx0 >= x && gx1 <= x + vw {
            x
        } else {
            gx0 - GAP
        };
        if (nx, ny) != (x, y) {
            self.scroll_to_xy(nx, ny);
        }
    }

    // ---- keys ---------------------------------------------------------------

    /// A one-line edit in the focused field's text (typed text, caret keys,
    /// Ctrl+A). In a field with a `/MaxLen`, typed or pasted text goes in
    /// only as far as there is room, as other readers take it: a full field
    /// refuses the keystroke, and nothing already in it is cut.
    pub(crate) fn edit_text(&mut self, op: field_edit::Op) {
        let max = self
            .focus
            .as_ref()
            .and_then(|x| self.field(x.field))
            .and_then(|f| f.max_len);
        let Some(e) = self.focus.as_mut().and_then(|x| x.edit.as_mut()) else {
            return;
        };
        match (op, max) {
            (field_edit::Op::Insert(t), Some(max)) => {
                let len = e.text.chars().count();
                // A selection is the whole text, which the insert replaces.
                let kept = if e.sel { 0 } else { len };
                let room = max.saturating_sub(kept);
                let fits: String = t.chars().take(room).collect();
                if !fits.is_empty() {
                    e.edit(field_edit::Op::Insert(&fits));
                }
            }
            (op, _) => e.edit(op),
        }
    }

    /// Enter in the focused field: a newline in a multi-line text field
    /// (Ctrl+Enter commits it), the open list's option picked, else the
    /// typed text committed (the field keeps the keys, its text selected).
    pub(crate) fn form_enter(&mut self, ctrl: bool) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        let Some(f) = self.field(focus.field).cloned() else {
            return;
        };
        if focus.open {
            return self.pick(focus.cursor, false);
        }
        if f.kind == FieldKind::Text && f.multiline && !ctrl {
            return self.edit_text(field_edit::Op::Insert("\n"));
        }
        match f.kind {
            FieldKind::Combo if !f.edit => {
                if let Some(x) = self.focus.as_mut() {
                    x.open = true;
                }
            }
            FieldKind::Check | FieldKind::Radio => self.toggle(focus.field, focus.widget),
            _ => {
                self.commit_focus();
                if let Some(e) = self.focus.as_mut().and_then(|x| x.edit.as_mut()) {
                    e.focus_all();
                }
            }
        }
    }

    /// A key the focused field takes besides the text ones: Space turns a
    /// check box or radio button, Up/Down move in a list box or combo box
    /// (picking in a closed combo box and a single-pick list), Space picks
    /// in a multi-pick list, Alt+Down and F4 open a combo box's list.
    /// `true` when it took the key.
    pub(crate) fn form_key(&mut self, key: &str, alt: bool) -> bool {
        let Some(focus) = self.focus.clone() else {
            return false;
        };
        let Some(f) = self.field(focus.field).cloned() else {
            return false;
        };
        let n = f.options.len();
        match (f.kind, key) {
            (FieldKind::Check | FieldKind::Radio, "space") => {
                self.toggle(focus.field, focus.widget);
                true
            }
            (FieldKind::Combo, "down") if alt => {
                self.set_open(true);
                true
            }
            (FieldKind::Combo, "f4") => {
                self.set_open(!focus.open);
                true
            }
            (FieldKind::Combo | FieldKind::List, "up" | "down") if n > 0 => {
                let cur = if focus.open || f.kind == FieldKind::List {
                    focus.cursor
                } else {
                    self.picked_expected(focus.field)
                        .first()
                        .copied()
                        .unwrap_or(focus.cursor)
                };
                let next = if key == "down" {
                    (cur + 1).min(n - 1)
                } else {
                    cur.saturating_sub(1)
                };
                let pick_now = !focus.open && !(f.kind == FieldKind::List && f.multi);
                if pick_now {
                    self.pick(next, false);
                } else if let Some(x) = self.focus.as_mut() {
                    x.cursor = next;
                }
                self.keep_cursor_shown();
                true
            }
            (FieldKind::List, "space") if f.multi => {
                self.pick(focus.cursor, true);
                true
            }
            _ => false,
        }
    }

    fn set_open(&mut self, open: bool) {
        if let Some(x) = self.focus.as_mut() {
            x.open = open;
        }
        self.keep_cursor_shown();
    }

    /// The options an open combo box's list shows: [`LIST_ROWS`] from its
    /// `top`. Empty when no list is open.
    pub(crate) fn list_window(&self) -> Range<usize> {
        let Some(x) = self.focus.as_ref().filter(|x| x.open) else {
            return 0..0;
        };
        let n = self.field(x.field).map_or(0, |f| f.options.len());
        let top = x.top.min(n.saturating_sub(LIST_ROWS));
        top..(top + LIST_ROWS).min(n)
    }

    /// The wheel over an open list: `rows` down (up when negative).
    pub(crate) fn list_scroll(&mut self, rows: i32) {
        let n = self
            .focus
            .as_ref()
            .and_then(|x| self.field(x.field))
            .map_or(0, |f| f.options.len());
        if let Some(x) = self.focus.as_mut() {
            let last = n.saturating_sub(LIST_ROWS) as i64;
            x.top = (x.top as i64 + i64::from(rows)).clamp(0, last.max(0)) as usize;
        }
    }

    /// The list's window follows its cursor.
    fn keep_cursor_shown(&mut self) {
        if let Some(x) = self.focus.as_mut() {
            if x.cursor < x.top {
                x.top = x.cursor;
            } else if x.cursor >= x.top + LIST_ROWS {
                x.top = x.cursor + 1 - LIST_ROWS;
            }
        }
    }
}

/// What a field's editor starts from: a text field's text, an editable
/// combo box's picked option (or typed text).
pub(crate) fn shown_text(f: &FieldMeta) -> String {
    match f.kind {
        FieldKind::Combo => f
            .picked
            .first()
            .and_then(|i| f.options.get(*i))
            .map(|o| o.1.clone())
            .or_else(|| f.value.first().cloned())
            .unwrap_or_default(),
        _ => f.value.first().cloned().unwrap_or_default(),
    }
}
