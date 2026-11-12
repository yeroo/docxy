//! Insert, edit and remove a hyperlink at the caret (#1952): a host's link
//! dialog (the embeddable document view's) over the editor.
//!
//! Each is one undo step. A new link's text takes the link look: underline
//! and [`LINK_COLOR`] (unless the text has a colour of its own), HTML
//! import's `a:link` look and Word's Hyperlink style's, written as direct
//! formatting because the package's styles may not define that style.
//! Removing a link takes that look off again.
//!
//! The link "at the caret" is the top-level link of the caret paragraph that
//! typing there would go into ([`locate`]): a caret between two inlines
//! belongs to the one before it, so a caret at a link's end is on it and one
//! at its start (after other text) is not. Over a selection it is the link
//! holding the whole selection. Links inside tracked changes or content
//! controls are not found. Edit and remove are not recorded while tracking
//! changes; insert is (a paste).

use super::{
    Caret, Clip, EditKind, Editor, FIELD_CHAR, LINE_BREAK, display_text, inline_len, locate,
    para_mut, resolve_para, split_content,
};
use crate::model::{Hyperlink, Inline, Run, RunProps};
use crate::package::with_opener_attr;

/// A new link's colour, `RRGGBB`: Word's Hyperlink style's and HTML
/// import's.
pub const LINK_COLOR: &str = "0563C1";

/// The link at the caret ([`Editor::caret_link`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaretLink {
    /// The link's text as shown: a field's result in place of its stand-in.
    pub text: String,
    /// Its target URL, `#` and its bookmark for a link inside the
    /// document, or `target#anchor` for a link with both (a URL and its
    /// fragment, as Word stores it); empty for a link with neither.
    pub url: String,
}

/// Where a link to `url` goes, `(target, anchor)`: `#name` is the bookmark
/// `name`, anything else a target URL. `None` for a URL that is empty once
/// trimmed, a bare `#`, or one holding a control character or a
/// noncharacter a document cannot hold.
fn link_dest(url: &str) -> Option<(Option<String>, Option<String>)> {
    let url = url.trim();
    if url.is_empty()
        || url
            .chars()
            .any(|c| c.is_control() || c == '\u{FFFE}' || c == '\u{FFFF}')
    {
        return None;
    }
    match url.strip_prefix('#') {
        Some("") => None,
        Some(name) => Some((None, Some(name.to_string()))),
        None => Some((Some(url.to_string()), None)),
    }
}

/// A link's display text as one line a `w:t` can hold: line ends (`\r\n`,
/// `\r`, `\n`), tabs and [`LINE_BREAK`] become a space (a host may pass a
/// selection's text across paragraphs); other C0 controls, U+FFFE, U+FFFF
/// and a field's stand-in ([`FIELD_CHAR`]) are dropped.
pub fn link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' | '\t' | LINE_BREAK => out.push(' '),
            '\u{0}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}' | FIELD_CHAR => {}
            _ => out.push(ch),
        }
    }
    out
}

/// A link's URL as a host reads it: its target, `#` and its bookmark, or
/// both (`target#anchor`, how Word stores a URL's fragment: an `r:id` and a
/// `w:anchor` on one link).
fn link_url(h: &Hyperlink) -> String {
    let anchor = h.anchor.as_ref().map(|a| format!("#{a}"));
    match (&h.target, anchor) {
        (Some(t), Some(a)) => format!("{t}{a}"),
        (Some(t), None) => t.clone(),
        (None, a) => a.unwrap_or_default(),
    }
}

/// `h`'s content with its text (runs, tabs, breaks, fields, objects)
/// replaced by `run`, standing where the first of it stood, and its
/// zero-width inlines (bookmarks, comment ranges, content control edges)
/// kept in place; `runs` empty. A link with text in `runs` gets `run`
/// first.
fn retexted_content(h: &mut Hyperlink, run: Run) -> Vec<Inline> {
    let mut run = Some(Inline::Run(run));
    let mut out = Vec::with_capacity(h.content.len() + 1);
    if !h.runs.is_empty() {
        out.extend(run.take());
    }
    for inline in std::mem::take(&mut h.content) {
        if inline_len(&inline) == 0 {
            out.push(inline);
        } else if let Some(r) = run.take() {
            out.push(r);
        }
    }
    if let Some(r) = run {
        out.insert(0, r);
    }
    h.runs.clear();
    out
}

/// The link look on `props`: underlined and, unless it has a colour,
/// [`LINK_COLOR`].
fn give_link_look(props: &mut RunProps) {
    props.underline = true;
    if props.color.is_none() {
        props.color = Some(LINK_COLOR.to_string());
    }
}

/// The link look off `props`: a [`LINK_COLOR`] colour and the underline
/// that came with it, and a `Hyperlink` character style. Nothing else
/// (bold, italic, another colour) changes.
fn drop_link_look(props: &mut RunProps) {
    if props
        .color
        .as_deref()
        .is_some_and(|c| c.eq_ignore_ascii_case(LINK_COLOR))
    {
        props.color = None;
        props.underline = false;
    }
    if props
        .style_id
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("Hyperlink"))
    {
        props.style_id = None;
    }
}

/// How a host reads the top-level hyperlink `content[index]`: its visible
/// text and its URL, as [`Editor::caret_link`] gives them (the view's link
/// hover and follow use it too, #1916); `None` when `content[index]` is not
/// a link (or is past the end).
pub fn hyperlink_shown(content: &[Inline], index: usize) -> Option<CaretLink> {
    let Some(Inline::Hyperlink(h)) = content.get(index) else {
        return None;
    };
    let start = offset_of(content, index);
    let end = start + inline_len(&content[index]);
    Some(CaretLink {
        text: display_text(content, start, end, true, false),
        url: link_url(h),
    })
}

/// The caret offset where `content[i]` starts.
fn offset_of(content: &[Inline], i: usize) -> usize {
    content[..i].iter().map(inline_len).sum()
}

/// A whole zero-width marker element a new link may hold: a bookmark, a
/// comment range, a proofing mark or a permission range edge. Not a content
/// control's or a smart tag's opening or closing, whose other half would be
/// outside the link.
fn whole_marker(inline: &Inline) -> bool {
    const NAMES: &[&str] = &[
        "w:bookmarkStart",
        "w:bookmarkEnd",
        "w:commentRangeStart",
        "w:commentRangeEnd",
        "w:proofErr",
        "w:permStart",
        "w:permEnd",
    ];
    let Inline::Raw(raw) = inline else {
        return false;
    };
    let raw = raw.trim();
    let Some(rest) = raw.strip_prefix('<') else {
        return false;
    };
    raw.ends_with("/>")
        && raw.matches('<').count() == 1
        && NAMES.iter().any(|name| {
            rest.strip_prefix(name)
                .is_some_and(|r| r.starts_with(|c: char| c.is_whitespace() || c == '/'))
        })
}

/// Whether the inlines of `content` within `[lo, hi)` can become a link's
/// whole: runs, and whole markers ([`whole_marker`]) strictly inside. A
/// zero-width inline at `lo` or `hi` stays outside the link.
fn wrappable(content: &[Inline], lo: usize, hi: usize) -> bool {
    let mut acc = 0;
    for inline in content {
        let len = inline_len(inline);
        let inside = if len == 0 {
            lo < acc && acc < hi
        } else {
            acc < hi && lo < acc + len
        };
        if inside && !matches!(inline, Inline::Run(_)) && !(len == 0 && whole_marker(inline)) {
            return false;
        }
        acc += len;
    }
    true
}

impl Editor {
    /// The caret paragraph's path and the index of the link in its content
    /// that is at the caret (see the module comment).
    fn caret_link_index(&self) -> Option<(Vec<usize>, usize)> {
        let path = &self.caret.path;
        let p = resolve_para(&self.doc.body, path)?;
        let i = match self.selection_range() {
            None => locate(&p.content, self.caret.offset)?.0,
            Some((lo, hi)) => {
                if lo.path != *path || hi.path != *path {
                    return None;
                }
                let mut acc = 0;
                p.content.iter().position(|inline| {
                    let len = inline_len(inline);
                    let holds = acc <= lo.offset && hi.offset <= acc + len;
                    acc += len;
                    holds && matches!(inline, Inline::Hyperlink(_))
                })?
            }
        };
        matches!(p.content[i], Inline::Hyperlink(_)).then(|| (path.clone(), i))
    }

    /// The link at the caret, for a host's Edit Link dialog and its
    /// buttons; `None` off a link.
    pub fn caret_link(&self) -> Option<CaretLink> {
        self.caret_link_found().map(|found| found.shown)
    }

    /// The link at the caret, looked up once: where it is and how
    /// [`Editor::caret_link`] reads it.
    fn caret_link_found(&self) -> Option<FoundLink> {
        let (path, i) = self.caret_link_index()?;
        let p = resolve_para(&self.doc.body, &path)?;
        let shown = hyperlink_shown(&p.content, i)?;
        Some(FoundLink {
            path,
            index: i,
            shown,
        })
    }

    /// Insert a link to `url` showing `text` at the caret, replacing the
    /// selection, as one undo step "Hyperlink"; the caret ends after it.
    /// `url` is trimmed; `#name` links to the bookmark `name`. `text` is
    /// made one line ([`link_text`]); with nothing but spaces left the URL
    /// is the text, as in Word.
    ///
    /// When `text` is the selection's own text and the selection is runs of
    /// one paragraph (bookmarks and comment ranges inside it allowed), those
    /// runs become the link and keep their formatting, with the link look
    /// added. Otherwise, and always while tracking changes, the selection is
    /// replaced by one run of `text` in the formatting typing at the caret
    /// would give, plus the link look; inside a link that lands between the
    /// link's two halves, so links never nest.
    ///
    /// False, with nothing done, for a URL [`link_dest`] refuses.
    pub fn insert_hyperlink(&mut self, text: &str, url: &str) -> bool {
        let Some((target, anchor)) = link_dest(url) else {
            return false;
        };
        let mut text = link_text(text);
        if text.trim().is_empty() {
            text = url.trim().to_string();
        }
        let link = Hyperlink {
            target,
            anchor,
            ..Hyperlink::default()
        };
        if self.wrap_in_link(&text, &link) {
            return true;
        }
        if self.pastes_into_a_link() {
            return false;
        }
        self.drop_collapsed_anchor();
        let pending = self.pending_format().cloned();
        self.one_step("Hyperlink", |ed| {
            ed.delete_selection_to_replace();
            let mut props = ed.typing_props_here(pending.as_ref());
            give_link_look(&mut props);
            let link = Hyperlink {
                runs: vec![Run { text, props }],
                ..link
            };
            ed.paste(&Clip {
                paras: vec![vec![Inline::Hyperlink(link)]],
            });
        });
        self.anchor = None;
        true
    }

    /// Whether a paste replacing the selection could land inside a link: at
    /// the caret or either end of the selection there is a top-level link
    /// holding a content control, and a paste at an empty control goes into
    /// it (`insert_inlines`), so a new link would nest in the old one. Asked
    /// before anything changes: deleting the selection may empty a control
    /// that has text now.
    fn pastes_into_a_link(&self) -> bool {
        let ends = match self.selection_range() {
            Some((lo, hi)) => vec![lo, hi, self.caret.clone()],
            None => vec![self.caret.clone()],
        };
        ends.iter().any(|end| {
            resolve_para(&self.doc.body, &end.path)
                .is_some_and(|p| at_link_with_a_control(&p.content, end.offset))
        })
    }

    /// [`Editor::insert_hyperlink`]'s wrap: the selected runs become `link`
    /// when the selection's text is `text`. False, changing nothing, when
    /// it cannot (see there).
    fn wrap_in_link(&mut self, text: &str, link: &Hyperlink) -> bool {
        if self.track.is_some() || self.cell_range().is_some() {
            return false;
        }
        let Some((lo, hi)) = self.selection_range() else {
            return false;
        };
        let wraps = lo.path == hi.path
            && self.selection_text() == text
            && resolve_para(&self.doc.body, &lo.path)
                .is_some_and(|p| wrappable(&p.content, lo.offset, hi.offset));
        if !wraps {
            return false;
        }
        self.one_step("Hyperlink", |ed| {
            ed.checkpoint(EditKind::Structural);
            let Some(p) = para_mut(&mut ed.doc.body, &lo.path) else {
                return;
            };
            let mut tail = split_content(&mut p.content, hi.offset);
            // The zero-width inlines at `hi` stay after the link.
            let keep = p
                .content
                .iter()
                .rposition(|inline| inline_len(inline) > 0)
                .map_or(0, |k| k + 1);
            tail.splice(0..0, p.content.drain(keep..).collect::<Vec<_>>());
            let mut inside = split_content(&mut p.content, lo.offset);
            for inline in &mut inside {
                if let Inline::Run(r) = inline {
                    give_link_look(&mut r.props);
                }
            }
            let mut link = link.clone();
            if inside.iter().all(|inline| matches!(inline, Inline::Run(_))) {
                link.runs = inside
                    .into_iter()
                    .filter_map(|inline| match inline {
                        Inline::Run(r) => Some(r),
                        _ => None,
                    })
                    .collect();
            } else {
                link.content = inside;
            }
            p.content.push(Inline::Hyperlink(link));
            p.content.extend(tail);
        });
        self.anchor = None;
        self.caret = hi;
        true
    }

    /// Edit the link at the caret: link it to `url` and, when `text` (made
    /// one line, [`link_text`]) is not blank and differs from the link's
    /// text made one line too, show `text` instead, in the formatting of the
    /// link's first run. The link's own text and URL as [`Editor::caret_link`]
    /// reads them change nothing, so a link holding a tab or a field, or
    /// a URL with a fragment (`target#anchor`), passed back as it is, stays
    /// as it is. New text replaces the link's text (its tabs, breaks and
    /// fields too) and keeps its zero-width inlines, its bookmarks among
    /// them. One undo step "Edit Hyperlink"; the caret ends after the link.
    /// A link loaded from a file keeps its other attributes (tooltip, target
    /// frame): its opening tag is rewritten, and save gives a new target a
    /// relationship of its own. True with nothing to change, taking no step;
    /// false, with nothing done, off a link or for a URL [`link_dest`]
    /// refuses. Not recorded while tracking changes.
    pub fn edit_hyperlink(&mut self, text: &str, url: &str) -> bool {
        let Some((target, anchor)) = link_dest(url) else {
            return false;
        };
        let Some(FoundLink {
            path,
            index: i,
            shown,
            ..
        }) = self.caret_link_found()
        else {
            return false;
        };
        // The link's own URL, as `caret_link` reads it (a URL and its
        // fragment too), is no new target.
        let retarget = url.trim() != shown.url;
        let text = link_text(text);
        let new_text = (!text.trim().is_empty() && text != link_text(&shown.text)).then_some(text);
        if !retarget && new_text.is_none() {
            return true;
        }
        self.one_step("Edit Hyperlink", |ed| {
            ed.checkpoint(EditKind::Structural);
            let Some(p) = para_mut(&mut ed.doc.body, &path) else {
                return;
            };
            let start = offset_of(&p.content, i);
            let Some(Inline::Hyperlink(h)) = p.content.get_mut(i) else {
                return;
            };
            if retarget {
                if let Some(raw) = &h.raw {
                    let raw = with_opener_attr(raw, "r:id", None);
                    h.raw = Some(with_opener_attr(&raw, "w:anchor", anchor.as_deref()));
                    h.content_changed = true;
                }
                h.target = target;
                h.anchor = anchor;
                h.rel_id = None;
            }
            if let Some(text) = new_text {
                let run = Run {
                    text,
                    props: first_props(h),
                };
                if h.content.is_empty() {
                    h.runs = vec![run];
                } else {
                    h.content = retexted_content(h, run);
                }
                h.content_changed |= h.raw.is_some();
            }
            let end = start + inline_len(&p.content[i]);
            ed.caret = Caret::at(path.clone(), end);
            ed.anchor = None;
        });
        true
    }

    /// Remove the link at the caret: its runs (or a complex link's whole
    /// content) become the paragraph's own at the same offsets, without the
    /// link look ([`drop_link_look`]). One undo step "Remove Hyperlink"; the
    /// caret and the selection stay. False, with nothing done, off a link.
    /// Not recorded while tracking changes.
    pub fn remove_hyperlink(&mut self) -> bool {
        let Some((path, i)) = self.caret_link_index() else {
            return false;
        };
        self.one_step("Remove Hyperlink", |ed| {
            ed.checkpoint(EditKind::Structural);
            let Some(p) = para_mut(&mut ed.doc.body, &path) else {
                return;
            };
            if !matches!(p.content.get(i), Some(Inline::Hyperlink(_))) {
                return;
            }
            let Inline::Hyperlink(h) = p.content.remove(i) else {
                return;
            };
            let mut items: Vec<Inline> = h
                .runs
                .into_iter()
                .map(Inline::Run)
                .chain(h.content)
                .collect();
            for item in &mut items {
                match item {
                    Inline::Run(Run { props, .. })
                    | Inline::Tab(props)
                    | Inline::Break(_, props) => drop_link_look(props),
                    _ => {}
                }
            }
            p.content.splice(i..i, items);
        });
        true
    }
}

/// The link at the caret ([`Editor::caret_link_found`]).
struct FoundLink {
    /// Its paragraph's path.
    path: Vec<usize>,
    /// Its index in the paragraph's content.
    index: usize,
    shown: CaretLink,
}

/// Whether caret offset `o` of `content` is inside a top-level link, or at
/// either of its edges, whose content holds a content control.
fn at_link_with_a_control(content: &[Inline], o: usize) -> bool {
    let mut acc = 0;
    content.iter().any(|inline| {
        let len = inline_len(inline);
        let at = acc <= o && o <= acc + len;
        acc += len;
        at && matches!(inline, Inline::Hyperlink(h) if h.content.iter().any(
            |c| matches!(c, Inline::Raw(raw) if crate::hf::is_sdt_open(raw))
        ))
    })
}

/// The formatting a link's new text takes: its first run's (or tab's, or
/// break's), else the link look alone.
fn first_props(h: &Hyperlink) -> RunProps {
    let first = h.runs.first().map(|r| &r.props).or_else(|| {
        h.content.iter().find_map(|inline| match inline {
            Inline::Run(Run { props, .. }) | Inline::Tab(props) | Inline::Break(_, props) => {
                Some(props)
            }
            _ => None,
        })
    });
    match first {
        Some(props) => props.continued(),
        None => {
            let mut props = RunProps::default();
            give_link_look(&mut props);
            props
        }
    }
}

#[cfg(test)]
mod tests;
