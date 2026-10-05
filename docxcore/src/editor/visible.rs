//! The UI's Find over the text the renderer draws (#211).
//!
//! [`Editor::find_all`] searches only what the caret can reach (runs, tabs,
//! breaks, a link's plain text), so its offsets can be selected and edited,
//! and agents and Replace All use it. A user also sees text the editor gives
//! no width: tracked changes, field results, footnote reference marks,
//! equations, SmartArt. This search finds that too, but still reports editor
//! offsets, never a second offset space (the #197 rule): every displayed char
//! carries the editor range it lives at, and a match is marked editable only
//! when all of its chars are ones the editor can edit.

use super::{EditKind, Editor, Match, all_paragraph_paths, char_eq, para_mut, resolve_para};
use crate::model::{Block, Inline, RevisionTarget};

/// A match of the UI's visible search, in editor offsets.
///
/// `editable` is true when every matched char is a run, tab or break char;
/// then `[start, end)` is exactly the matched text and can be replaced. A
/// read-only match covers a field's result (`[start, end)` is the field's one
/// unit), or text the editor gives no width (a tracked change, a footnote
/// mark, an equation, SmartArt), which has the collapsed range where that
/// construct sits, or a mix of these with editable text (`colo[ins u]r` for
/// `colour`, `Body` + a field's `1` for `Body1`), whose range spans all of
/// it. `revision` is set when any matched char is drawn from a tracked
/// change; [`Editor::select_found`] then goes to that change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundMatch {
    pub path: Vec<usize>,
    pub start: usize,
    pub end: usize,
    pub editable: bool,
    /// The tracked change the matched text is drawn from, if any.
    pub revision: Option<RevisionTarget>,
}

impl FoundMatch {
    /// The editor range this match covers.
    pub fn to_match(&self) -> Match {
        Match {
            path: self.path.clone(),
            start: self.start,
            end: self.end,
        }
    }
}

/// The index of the next match when stepping through `len` matches from
/// `current` (`None`: not on a match yet, so the first, or last in reverse),
/// wrapping. Hosts step by index rather than from the caret, because several
/// read-only matches can share one editor offset (two hits in one deletion).
pub fn step_found(len: usize, current: Option<usize>, reverse: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (current, reverse) {
        (None, false) => 0,
        (None, true) => len - 1,
        (Some(i), false) => (i + 1) % len,
        (Some(i), true) => (i + len - 1) % len,
    })
}

/// Where [`Editor::select_found`] leaves the editor for a match.
enum FoundSpot {
    /// The tracked change it is drawn from is selected for review: the caret
    /// at the change's review start, nothing selected.
    Review(RevisionTarget, super::Caret),
    /// A collapsed caret at the match, nothing selected.
    Caret(super::Caret),
    /// The match's range is selected.
    Range,
}

/// One displayed char and the editor range it lives at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Shown {
    pub(super) ch: char,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) editable: bool,
    pub(super) revision: Option<RevisionTarget>,
}

/// A paragraph's displayed text, as `render.rs` draws it, in segments a match
/// may not cross: the inline text between block boxes (SmartArt, a chart, a
/// text box, a multi-line equation, a large image), each line of a box, and
/// each SmartArt node. Text boxes contribute nothing here: their paragraphs
/// are searched by their own paths.
pub(super) fn shown_segments(content: &[Inline]) -> Vec<Vec<Shown>> {
    let mut walk = Walk {
        segments: vec![Vec::new()],
        offset: 0,
    };
    walk.inlines(content, false);
    walk.segments.retain(|s| !s.is_empty());
    walk.segments
}

struct Walk {
    segments: Vec<Vec<Shown>>,
    /// The editor offset reached so far (the sum of [`super::inline_len`]).
    offset: usize,
}

impl Walk {
    fn push(&mut self, shown: Shown) {
        if let Some(segment) = self.segments.last_mut() {
            segment.push(shown);
        }
    }

    /// Text the editor can edit: one offset per char.
    fn editable(&mut self, text: &str) {
        for ch in text.chars() {
            self.push(Shown {
                ch,
                start: self.offset,
                end: self.offset + 1,
                editable: true,
                revision: None,
            });
            self.offset += 1;
        }
    }

    /// Read-only text, every char at the editor range `[start, end)`.
    fn read_only(
        &mut self,
        text: &str,
        start: usize,
        end: usize,
        revision: Option<RevisionTarget>,
    ) {
        for ch in text.chars() {
            self.push(Shown {
                ch,
                start,
                end,
                editable: false,
                revision,
            });
        }
    }

    fn break_segment(&mut self) {
        if self.segments.last().is_some_and(|s| !s.is_empty()) {
            self.segments.push(Vec::new());
        }
    }

    /// A block box drawn on its own lines: each line its own segment, at the
    /// collapsed offset where the box sits.
    fn block<'a>(&mut self, lines: impl IntoIterator<Item = &'a str>) {
        self.break_segment();
        for line in lines {
            self.read_only(line, self.offset, self.offset, None);
            self.break_segment();
        }
    }

    /// The same walk as `render.rs`'s `flat_inlines` and its inline arms:
    /// `nested` is inside a hyperlink, where block content is not drawn.
    fn inlines(&mut self, content: &[Inline], nested: bool) {
        for inline in content {
            match inline {
                Inline::Run(r) => self.editable(&r.text),
                Inline::Hyperlink(h) => {
                    for r in &h.runs {
                        self.editable(&r.text);
                    }
                    self.inlines(&h.content, true);
                }
                Inline::Tab(_) => self.editable("\t"),
                Inline::Break(..) => self.editable("\n"),
                // A field's result is one editor unit (#642).
                Inline::Field { text, .. } => {
                    if !text.is_empty() {
                        self.read_only(text, self.offset, self.offset + 1, None);
                        self.offset += 1;
                    }
                }
                // render.rs draws only a tracked change's direct runs and
                // fields' results; nothing else inside it is shown.
                Inline::Revision {
                    metadata, content, ..
                } => {
                    for inner in content {
                        let text = match inner {
                            Inline::Run(r) => &r.text,
                            Inline::Field { text, .. } => text,
                            _ => continue,
                        };
                        self.read_only(text, self.offset, self.offset, Some(metadata.target));
                    }
                }
                Inline::FootnoteRef { id, .. } => {
                    let mark = crate::render::superscript(*id);
                    self.read_only(&mark, self.offset, self.offset, None);
                }
                Inline::Equation { text, .. } if !text.contains('\n') => {
                    self.read_only(text, self.offset, self.offset, None);
                }
                Inline::Equation { text, .. } => {
                    if !nested {
                        self.block(text.split('\n'));
                    }
                }
                // The node text, not the box's "SmartArt" caption.
                Inline::SmartArt { text, .. } => {
                    if !nested {
                        self.block(text.iter().map(String::as_str));
                    }
                }
                Inline::Chart { .. } | Inline::TextBox { .. } => {
                    if !nested {
                        self.block([]);
                    }
                }
                Inline::Raw(raw) => {
                    if !nested && crate::render::is_block_image(raw) {
                        self.block([]);
                    }
                }
                Inline::UnsupportedRevision { .. } => {}
            }
        }
    }
}

/// The search behind [`Editor::find_visible`]: every paragraph (text box
/// paragraphs after their host, as [`super::find_all_in_body`] orders them),
/// matches in display order, non-overlapping within a segment.
///
/// Editable text wins over read-only text it overlaps: a read-only hit is
/// dropped when an editable hit starts inside it (`[ins a]` + `aa`, query
/// `aa`, finds the editable `aa`, not `[a]a`), so a match the user could
/// replace is never hidden. Hits still never overlap, so no drawn char is
/// counted twice.
pub(crate) fn find_visible_in_body(
    body: &[Block],
    query: &str,
    case_sensitive: bool,
) -> Vec<FoundMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let q: Vec<char> = query.chars().collect();
    let mut out = Vec::new();
    for path in all_paragraph_paths(body) {
        let Some(p) = resolve_para(body, &path) else {
            continue;
        };
        for segment in shown_segments(&p.content) {
            let hit_at = |i: usize| -> Option<FoundMatch> {
                let hit = segment.get(i..i + q.len())?;
                hit.iter()
                    .zip(&q)
                    .all(|(s, c)| char_eq(s.ch, *c, case_sensitive))
                    .then(|| FoundMatch {
                        path: path.clone(),
                        start: hit.iter().map(|s| s.start).min().unwrap_or(0),
                        end: hit.iter().map(|s| s.end).max().unwrap_or(0),
                        editable: hit.iter().all(|s| s.editable),
                        revision: hit.iter().find_map(|s| s.revision),
                    })
            };
            let mut i = 0;
            while i + q.len() <= segment.len() {
                let Some(m) = hit_at(i) else {
                    i += 1;
                    continue;
                };
                if !m.editable {
                    let editable_inside =
                        (i + 1..i + q.len()).find(|&j| hit_at(j).is_some_and(|h| h.editable));
                    if let Some(j) = editable_inside {
                        i = j;
                        continue;
                    }
                }
                out.push(m);
                i += q.len();
            }
        }
    }
    out
}

impl Editor {
    /// The UI's Find: every match of `query` in the text the document shows,
    /// in document order, including read-only matches (see [`FoundMatch`]).
    /// Agents and [`Editor::replace_all`] keep using [`Editor::find_all`],
    /// the editable text only.
    pub fn find_visible(&self, query: &str, case_sensitive: bool) -> Vec<FoundMatch> {
        find_visible_in_body(&self.doc.body, query, case_sensitive)
    }

    /// Go to a match. A match drawn (partly) from a tracked change selects
    /// that change for review; any other ranged match (editable text, a
    /// field's result) is selected; any other collapsed one puts the caret
    /// where its construct sits.
    pub fn select_found(&mut self, m: &FoundMatch) {
        match self.found_spot(m) {
            FoundSpot::Review(target, _) => {
                self.select_revision(target);
            }
            FoundSpot::Caret(caret) => {
                self.caret = caret;
                self.anchor = None;
                self.last = EditKind::None;
            }
            FoundSpot::Range => self.select_match(&m.to_match()),
        }
    }

    /// Where [`Editor::select_found`] puts the editor for `m`. Both it and
    /// [`Editor::is_at_found`] read this, so they cannot disagree.
    fn found_spot(&self, m: &FoundMatch) -> FoundSpot {
        if !m.editable {
            let review = m.revision.and_then(|target| {
                self.revision_locations()
                    .into_iter()
                    .find(|location| location.address.target == target)
                    .map(|location| FoundSpot::Review(target, location.start))
            });
            if let Some(review) = review {
                return review;
            }
        }
        if m.start == m.end {
            FoundSpot::Caret(super::Caret::at(m.path.clone(), m.start))
        } else {
            FoundSpot::Range
        }
    }

    /// The match to go to when not on one yet: the first one after the caret
    /// (in reverse, the last one before it), wrapping, the way
    /// [`Editor::find_next`] steps. From then on, step by index
    /// ([`step_found`]).
    pub fn found_from_caret(&self, matches: &[FoundMatch], reverse: bool) -> Option<usize> {
        let starts: Vec<(&[usize], usize)> = matches
            .iter()
            .map(|m| (m.path.as_slice(), m.start))
            .collect();
        self.index_from_caret(&starts, reverse)
    }

    /// True while the editor is still on `m` as [`Editor::select_found`] left
    /// it: for a match it selected, the selection is exactly `m`; for one it
    /// went to (a tracked change's review start, or a collapsed match's
    /// spot), the caret is there with nothing selected. Hosts step from their
    /// current match only while this holds, and from the caret once the user
    /// has moved it (a click with the bar open, another tab).
    pub fn is_at_found(&self, m: &FoundMatch) -> bool {
        match self.found_spot(m) {
            FoundSpot::Review(_, caret) | FoundSpot::Caret(caret) => {
                !self.has_selection() && self.caret == caret
            }
            FoundSpot::Range => self.selection_is(m),
        }
    }

    /// True when the selection is exactly `m`'s range, so Replace may edit it.
    pub fn selection_is(&self, m: &FoundMatch) -> bool {
        self.selection_range().is_some_and(|(lo, hi)| {
            lo.path == m.path && hi.path == m.path && lo.offset == m.start && hi.offset == m.end
        })
    }

    /// The UI's Replace All: replaces the editable matches of
    /// [`Editor::find_visible`] (what Find showed, so text hidden between
    /// two drawn pieces is never matched) as one undo step. Returns the number
    /// replaced and the number of read-only matches left alone.
    pub fn replace_all_visible(
        &mut self,
        query: &str,
        with: &str,
        case_sensitive: bool,
    ) -> (usize, usize) {
        let (editable, read_only): (Vec<FoundMatch>, Vec<FoundMatch>) = self
            .find_visible(query, case_sensitive)
            .into_iter()
            .partition(|m| m.editable);
        let matches = editable.iter().map(FoundMatch::to_match).collect();
        (self.replace_matches(matches, with), read_only.len())
    }

    /// Replace each of `matches` (editor ranges in document order, at most one
    /// paragraph each, not overlapping) with `with`, as one undo step. Returns
    /// the number replaced.
    pub fn replace_matches(&mut self, matches: Vec<Match>, with: &str) -> usize {
        if matches.is_empty() {
            return 0;
        }
        self.checkpoint(EditKind::Structural);
        // Group consecutive matches by paragraph (they come in document order).
        let mut groups: Vec<(Vec<usize>, Vec<Match>)> = Vec::new();
        for m in matches {
            if let Some(last) = groups.last_mut() {
                if last.0 == m.path {
                    last.1.push(m);
                    continue;
                }
            }
            groups.push((m.path.clone(), vec![m]));
        }
        let mut count = 0;
        // Paragraphs back to front too. A text box's paragraphs are addressed
        // through the host's inline index (`[i, k, j]`, listed after `[i]`),
        // and an edit to the host can remove an emptied run before the text
        // box and shift `k`: edit the text box paragraphs first, while their
        // paths still resolve to the same text box.
        for (path, mut ms) in groups.into_iter().rev() {
            ms.sort_by_key(|m| std::cmp::Reverse(m.start)); // back-to-front keeps offsets valid
            if para_mut(&mut self.doc.body, &path).is_some() {
                for m in ms {
                    self.replace_text_range(&path, m.start, m.end, with);
                    count += 1;
                }
            }
        }
        self.clear_selection();
        self.clamp();
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Caret, FIELD_CHAR, editor_text};
    use crate::model::{Document, Paragraph, RunProps};

    fn xml_doc(p_inner: &str) -> Document {
        crate::load::parse_document_xml(
            &format!("<w:document><w:body><w:p>{p_inner}</w:p></w:body></w:document>"),
            &crate::load::Relationships::default(),
        )
    }

    fn text(ed: &Editor) -> String {
        match &ed.doc.body[0] {
            Block::Paragraph(p) => editor_text(&p.content),
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    const INS: &str = "<w:r><w:t xml:space=\"preserve\">Body </w:t></w:r>\
        <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>added</w:t></w:r></w:ins>\
        <w:del w:id=\"2\" w:author=\"A\"><w:r><w:delText>gone</w:delText></w:r></w:del>\
        <w:r><w:t xml:space=\"preserve\"> end</w:t></w:r>";

    #[test]
    fn text_in_tracked_changes_is_found_read_only_211() {
        let ed = Editor::new(xml_doc(INS));
        assert_eq!(text(&ed), "Body  end");
        assert!(
            ed.find_all("added", false).is_empty(),
            "the editor search is unchanged"
        );
        let added = ed.find_visible("added", false);
        assert_eq!(added.len(), 1);
        assert!(!added[0].editable);
        assert_eq!(
            (added[0].start, added[0].end),
            (5, 5),
            "collapsed at the revision"
        );
        assert!(added[0].revision.is_some_and(RevisionTarget::is_assigned));
        let gone = ed.find_visible("gone", false);
        assert_eq!(gone.len(), 1);
        assert!(!gone[0].editable);
        assert_ne!(gone[0].revision, added[0].revision);
        let body = ed.find_visible("Body", false);
        assert_eq!(body.len(), 1);
        assert!(body[0].editable);
        assert_eq!((body[0].start, body[0].end), (0, 4));
    }

    #[test]
    fn going_to_a_match_in_a_tracked_change_selects_it_for_review_211() {
        let mut ed = Editor::new(xml_doc(INS));
        let m = ed.find_visible("gone", false).remove(0);
        ed.select_found(&m);
        assert_eq!(ed.review_target, m.revision);
        assert_eq!(ed.caret, Caret::at(vec![0], 5));
        assert!(!ed.has_selection());
        assert_eq!(m.start, m.end, "a tracked change's text is collapsed");
        assert!(ed.is_at_found(&m), "the editor is on the match");
        ed.caret = Caret::at(vec![0], 0);
        assert!(!ed.is_at_found(&m), "until the caret moves");
    }

    /// A match mixing editable text and a tracked change's text (`colo[ins
    /// u]r`) is read-only and ranged; going to it selects the change for
    /// review, and the editor then counts as on it (r2 M1).
    #[test]
    fn a_mixed_match_selects_its_change_and_is_at_it_211() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t>colo</w:t></w:r>\
             <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>u</w:t></w:r></w:ins>\
             <w:r><w:t>r</w:t></w:r>",
        ));
        let m = ed.find_visible("colour", false).remove(0);
        assert!(!m.editable && m.revision.is_some());
        assert_eq!(
            (m.start, m.end),
            (0, 5),
            "ranged: it covers editable chars too"
        );
        ed.select_found(&m);
        assert_eq!(ed.review_target, m.revision);
        assert!(!ed.has_selection());
        assert!(ed.is_at_found(&m), "on the match right after going to it");
        ed.anchor = Some(Caret::at(vec![0], 0));
        ed.caret = Caret::at(vec![0], 5);
        assert!(
            !ed.is_at_found(&m),
            "selecting its range is not where Find left it"
        );
    }

    const FIELD: &str = "<w:r><w:t>Body</w:t></w:r>\
        <w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple>";

    #[test]
    fn a_fields_result_is_found_as_its_unit_211() {
        let mut ed = Editor::new(xml_doc(FIELD));
        assert!(ed.find_all("1", false).is_empty());
        let one = ed.find_visible("1", false);
        assert_eq!(one.len(), 1);
        assert!(!one[0].editable);
        assert_eq!((one[0].start, one[0].end), (4, 5), "the field's one unit");
        ed.select_found(&one[0]);
        assert!(ed.selection_is(&one[0]), "the field is selected as a unit");
        assert!(ed.is_at_found(&one[0]));
        ed.clear_selection();
        assert!(!ed.is_at_found(&one[0]));
        let joined = ed.find_visible("Body1", false);
        assert_eq!(joined.len(), 1);
        assert!(
            !joined[0].editable,
            "run text plus a field result is read-only"
        );
        assert_eq!((joined[0].start, joined[0].end), (0, 5));
        let body = ed.find_visible("Body", false);
        assert!(body[0].editable);
    }

    #[test]
    fn footnote_marks_and_equations_are_found_211() {
        let ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![
                    Inline::Run(crate::model::Run {
                        text: "x".into(),
                        props: RunProps::default(),
                    }),
                    Inline::FootnoteRef {
                        id: 12,
                        endnote: false,
                        raw: String::new(),
                    },
                    Inline::Equation {
                        raw: String::new(),
                        text: "a²+b²".into(),
                        latex: None,
                    },
                    Inline::SmartArt {
                        raw: String::new(),
                        text: vec!["First node".into(), "Second".into()],
                    },
                ],
                ..Paragraph::default()
            })],
        });
        let mark = ed.find_visible("¹²", false);
        assert_eq!(mark.len(), 1);
        assert_eq!(
            (mark[0].start, mark[0].end, mark[0].editable),
            (1, 1, false)
        );
        assert_eq!(ed.find_visible("a²+b²", false).len(), 1);
        assert_eq!(ed.find_visible("node", false).len(), 1);
        assert!(
            ed.find_visible("nodeSecond", false).is_empty(),
            "a match never joins two SmartArt nodes"
        );
        assert!(
            ed.find_visible("b²First", false).is_empty(),
            "nor inline text and a box"
        );
        assert!(
            ed.find_visible("SmartArt", false).is_empty(),
            "not the caption"
        );
    }

    /// render.rs draws only a revision's direct runs and fields: a tab, or a
    /// nested link's text, inside a tracked change is not shown, so not found.
    #[test]
    fn only_what_a_revision_draws_is_found_211() {
        let ed = Editor::new(xml_doc(
            "<w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>shown</w:t></w:r>\
             <w:hyperlink w:anchor=\"x\"><w:r><w:t>hidden</w:t></w:r></w:hyperlink></w:ins>",
        ));
        assert_eq!(ed.find_visible("shown", false).len(), 1);
        assert!(ed.find_visible("hidden", false).is_empty());
    }

    #[test]
    fn a_text_box_paragraph_is_found_once_211() {
        let ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                content: vec![Inline::TextBox {
                    raw: String::new(),
                    blocks: vec![Block::Paragraph(Paragraph {
                        content: vec![Inline::Run(crate::model::Run {
                            text: "boxed".into(),
                            props: RunProps::default(),
                        })],
                        ..Paragraph::default()
                    })],
                }],
                ..Paragraph::default()
            })],
        });
        let ms = ed.find_visible("boxed", false);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].path, vec![0, 0, 0]);
        assert!(ms[0].editable);
    }

    /// With nothing zero-width in the paragraphs, the visible search and the
    /// editor search agree exactly.
    #[test]
    fn without_read_only_text_both_searches_agree_211() {
        let ed = Editor::new(xml_doc(
            "<w:r><w:t>banana</w:t><w:tab/><w:t>an</w:t><w:br/></w:r>\
             <w:hyperlink w:anchor=\"x\"><w:r><w:t>Ana</w:t></w:r></w:hyperlink>",
        ));
        for (query, case_sensitive) in [("an", false), ("an", true), ("a\tan", false)] {
            let visible = ed.find_visible(query, case_sensitive);
            assert!(visible.iter().all(|m| m.editable && m.revision.is_none()));
            let as_matches: Vec<Match> = visible.iter().map(FoundMatch::to_match).collect();
            assert_eq!(as_matches, ed.find_all(query, case_sensitive), "{query:?}");
            assert!(!as_matches.is_empty());
        }
    }

    /// A read-only hit never hides an editable one that overlaps it: `[ins a]`
    /// + `aa` finds the editable `aa` (m1 of review r1).
    #[test]
    fn editable_text_wins_over_an_overlapping_read_only_hit_211() {
        let mut ed = Editor::new(xml_doc(
            "<w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>a</w:t></w:r></w:ins>\
             <w:r><w:t>aa</w:t></w:r>",
        ));
        let ms = ed.find_visible("aa", false);
        assert_eq!(ms.len(), 1, "{ms:?}");
        assert!(ms[0].editable);
        assert_eq!((ms[0].start, ms[0].end), (0, 2));
        assert_eq!(ed.replace_all_visible("aa", "b", false), (1, 0));
        assert_eq!(text(&ed), "b");
        // Read-only hits alone still don't overlap: `aaa` drawn in a deletion,
        // query `aa`, is one hit.
        let ed = Editor::new(xml_doc(
            "<w:del w:id=\"1\" w:author=\"A\"><w:r><w:delText>aaa</w:delText></w:r></w:del>",
        ));
        assert_eq!(ed.find_visible("aa", false).len(), 1);
    }

    /// Two hits in one deletion share an editor offset; stepping by index
    /// still visits both.
    #[test]
    fn matches_sharing_an_offset_are_each_visited_211() {
        let ed = Editor::new(xml_doc(
            "<w:del w:id=\"1\" w:author=\"A\"><w:r><w:delText>ab ab</w:delText></w:r></w:del>",
        ));
        let ms = ed.find_visible("ab", false);
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].start, ms[1].start);
        let first = step_found(ms.len(), None, false);
        let second = step_found(ms.len(), first, false);
        assert_eq!((first, second), (Some(0), Some(1)));
        assert_eq!(step_found(ms.len(), second, false), Some(0), "wraps");
        assert_eq!(step_found(ms.len(), None, true), Some(1));
        assert_eq!(step_found(0, None, false), None);
    }

    #[test]
    fn the_first_match_is_the_one_after_the_caret_211() {
        let mut ed = Editor::new(xml_doc(INS)); // "Body [added][gone] end"
        let ms = ed.find_visible("n", false); // gone (at 5), end (at 7)
        assert_eq!(ms.len(), 2);
        ed.caret = Caret::at(vec![0], 0);
        assert_eq!(ed.found_from_caret(&ms, false), Some(0));
        assert_eq!(ed.found_from_caret(&ms, true), Some(1), "wraps back");
        ed.caret = Caret::at(vec![0], 6);
        assert_eq!(ed.found_from_caret(&ms, false), Some(1));
        assert_eq!(ed.found_from_caret(&ms, true), Some(0));
        ed.caret = Caret::at(vec![0], 9);
        assert_eq!(ed.found_from_caret(&ms, false), Some(0), "wraps forward");
        assert_eq!(ed.found_from_caret(&[], false), None);
    }

    #[test]
    fn replace_all_visible_skips_read_only_matches_211() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t xml:space=\"preserve\">x </w:t></w:r>\
             <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>x</w:t></w:r></w:ins>\
             <w:fldSimple w:instr=\" REF a \"><w:r><w:t>x</w:t></w:r></w:fldSimple>\
             <w:r><w:t xml:space=\"preserve\"> x</w:t></w:r>",
        ));
        let before = ed.doc.body.clone();
        assert_eq!(ed.replace_all_visible("x", "Y", false), (2, 2));
        assert_eq!(text(&ed), format!("Y {FIELD_CHAR} Y"));
        assert_eq!(
            ed.find_visible("x", false).len(),
            2,
            "the w:ins text and the field stay"
        );
        ed.undo();
        assert_eq!(ed.doc.body, before, "one undo step");
    }

    /// `bc` in `ab[del X]cd` is adjacent in the editor's text but not on
    /// screen: Find does not show it, so the UI's Replace All leaves it.
    #[test]
    fn replace_all_visible_never_edits_what_find_did_not_show_211() {
        let mut ed = Editor::new(xml_doc(
            "<w:r><w:t>ab</w:t></w:r>\
             <w:del w:id=\"1\" w:author=\"A\"><w:r><w:delText>X</w:delText></w:r></w:del>\
             <w:r><w:t>cd</w:t></w:r>",
        ));
        assert_eq!(
            ed.find_all("bc", false).len(),
            1,
            "the editor search sees it"
        );
        assert!(ed.find_visible("bc", false).is_empty());
        let before = ed.doc.body.clone();
        assert_eq!(ed.replace_all_visible("bc", "Z", false), (0, 0));
        assert_eq!(ed.doc.body, before);
    }
}
