//! Another app's list items pasted (#1967): a [`Clip`]'s `lists` give the
//! pasted paragraphs their list membership as Word's paste does, a
//! paragraph's formatting riding on its mark.

use super::*;

const BULLET: i32 = 1;
const DECIMAL: i32 = 2;

fn run(text: &str) -> Inline {
    Inline::Run(Run {
        text: text.to_string(),
        props: RunProps::default(),
    })
}

fn para(text: &str, num_id: Option<i32>) -> Block {
    Block::Paragraph(Paragraph {
        props: ParProps {
            num_id,
            ..ParProps::default()
        },
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![run(text)]
        },
    })
}

fn editor(paras: &[(&str, Option<i32>)]) -> Editor {
    Editor::new(Document {
        body: paras.iter().map(|(t, n)| para(t, *n)).collect(),
    })
}

fn item(num_id: i32, ilvl: i32) -> Option<ClipList> {
    Some(ClipList { num_id, ilvl })
}

/// A clip of `texts` (an empty text is an empty paragraph: a mark) with
/// `lists`.
fn clip(texts: &[&str], lists: Vec<Option<ClipList>>) -> Clip {
    let mut clip = Clip::new(
        texts
            .iter()
            .map(|t| {
                if t.is_empty() {
                    Vec::new()
                } else {
                    vec![run(t)]
                }
            })
            .collect(),
    );
    clip.set_lists(lists);
    clip
}

/// Each top-level paragraph's text and list membership.
fn paras(ed: &Editor) -> Vec<(String, Option<i32>, i32)> {
    ed.doc
        .body
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some((p.plain_text(), p.props.num_id, p.props.ilvl)),
            _ => None,
        })
        .collect()
}

fn three_items() -> Clip {
    clip(
        &["a", "b", "c"],
        vec![item(BULLET, 0), item(BULLET, 1), item(DECIMAL, 0)],
    )
}

#[test]
fn a_pasted_list_keeps_its_items_in_an_empty_paragraph() {
    // A browser's copy: no newline after the last item, so no mark.
    let mut ed = editor(&[("", None)]);
    ed.paste(&three_items());
    assert_eq!(
        paras(&ed),
        vec![
            ("a".into(), Some(BULLET), 0),
            ("b".into(), Some(BULLET), 1),
            ("c".into(), Some(DECIMAL), 0),
        ]
    );
    assert!(ed.undo());
    assert_eq!(paras(&ed), vec![(String::new(), None, 0)]);
}

#[test]
fn the_last_item_joins_the_text_after_the_caret_without_its_list() {
    let mut ed = editor(&[("xy", None)]);
    ed.caret = Caret::top(0, 1);
    ed.paste(&three_items());
    assert_eq!(
        paras(&ed),
        vec![
            ("xa".into(), Some(BULLET), 0),
            ("b".into(), Some(BULLET), 1),
            ("cy".into(), None, 0),
        ]
    );
}

#[test]
fn one_item_in_a_sentence_is_text() {
    let one = clip(&["a"], vec![item(BULLET, 0)]);
    for at in [0, 2, 4] {
        let mut ed = editor(&[("abcd", None)]);
        ed.caret = Caret::top(0, at);
        ed.paste(&one);
        assert_eq!(paras(&ed)[0].1, None, "pasted at {at}");
    }
    let mut ed = editor(&[("", None)]);
    ed.paste(&one);
    assert_eq!(paras(&ed), vec![("a".into(), Some(BULLET), 0)]);
}

#[test]
fn an_item_beside_an_object_is_not_alone() {
    // A paragraph holding only a tab has an editor length, though no text.
    let mut ed = Editor::new(Document {
        body: vec![Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![Inline::Tab(RunProps::default())],
        })],
    });
    ed.caret = Caret::top(0, 1);
    ed.paste(&clip(&["a"], vec![item(BULLET, 0)]));
    assert_eq!(paras(&ed)[0].1, None);
}

#[test]
fn a_non_list_piece_keeps_todays_props() {
    // Into a numbered paragraph: the pieces with no list keep the target's,
    // as every paste did before #1967.
    let mixed = clip(
        &["intro", "a", "outro", ""],
        vec![None, item(BULLET, 0), None, None],
    );
    let mut ed = editor(&[("z", Some(DECIMAL))]);
    ed.paste(&mixed);
    assert_eq!(
        paras(&ed),
        vec![
            ("intro".into(), Some(DECIMAL), 0),
            ("a".into(), Some(BULLET), 0),
            ("outro".into(), Some(DECIMAL), 0),
            ("z".into(), Some(DECIMAL), 0),
        ]
    );
}

#[test]
fn a_through_mark_list_paste_leaves_the_target_paragraph_as_it_was() {
    let through = clip(&["a", "b", ""], vec![item(BULLET, 0), item(DECIMAL, 2)]);
    let mut ed = editor(&[("z", None)]);
    ed.paste(&through);
    assert_eq!(
        paras(&ed),
        vec![
            ("a".into(), Some(BULLET), 0),
            ("b".into(), Some(DECIMAL), 2),
            ("z".into(), None, 0),
        ]
    );
    // And the last piece's own mark is the target's: an empty one there
    // leaves an empty plain paragraph.
    let mut ed = editor(&[("", None)]);
    ed.paste(&through);
    assert_eq!(paras(&ed)[2], (String::new(), None, 0));
}

#[test]
fn own_copy_and_text_have_no_lists() {
    let mut ed = editor(&[("one", Some(BULLET)), ("two", Some(DECIMAL))]);
    ed.select_all();
    let copy = ed.copy().expect("a copy");
    assert!(!copy.has_lists());
    assert_eq!(copy, Clip::new(copy.paras.clone()));
    let text = Clip::from_text("a\nb");
    assert!(!text.has_lists());
    // Our copy pasted into a plain paragraph pastes as it always did: the
    // pieces take the target's props.
    let mut ed = editor(&[("", None)]);
    ed.paste(&copy);
    assert_eq!(
        paras(&ed),
        vec![("one".into(), None, 0), ("two".into(), None, 0)]
    );
}

#[test]
fn lists_stay_canonical() {
    let mut c = Clip::new(vec![vec![run("a")], vec![run("b")]]);
    c.set_lists(vec![None, None]);
    assert!(!c.has_lists());
    assert_eq!(c, Clip::new(c.paras.clone()));
    c.set_lists(vec![item(BULLET, 0), None]);
    assert!(c.has_lists());
    assert_eq!(
        (c.list(0), c.list(1), c.list(7)),
        (item(BULLET, 0), None, None)
    );
}
