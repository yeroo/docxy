//! A character-addressed view of the editor's paragraphs. Each paragraph mark
//! contributes one `\n`; table row and cell boundaries add no extra character.
//! Text boxes are separate stories, keyed by the path of their host inline.
//! Every character in a story has exactly one caret immediately before it.

use super::{Caret, inline_len};
use crate::model::{Block, BreakKind, Document, Inline};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryOffset {
    pub story: String,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct FlatStory {
    pub id: String,
    pub text: String,
    /// `carets[i]` is the caret immediately before character `i`.
    carets: Vec<Caret>,
}

impl FlatStory {
    pub fn len(&self) -> usize {
        self.carets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.carets.is_empty()
    }
    pub fn caret(&self, offset: usize) -> Option<Caret> {
        self.carets.get(offset).cloned()
    }
    pub fn offset(&self, caret: &Caret) -> Option<usize> {
        self.carets.iter().position(|c| c == caret)
    }
    pub fn paragraph_index(&self, caret: &Caret) -> Option<usize> {
        let mut last: Option<&[usize]> = None;
        let mut index = 0;
        for here in &self.carets {
            if last != Some(&here.path) {
                if here.path == caret.path {
                    return Some(index);
                }
                last = Some(&here.path);
                index += 1;
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct FlatDocument {
    pub stories: Vec<FlatStory>,
}

impl FlatDocument {
    pub fn new(doc: &Document) -> Self {
        let mut flat = Self {
            stories: vec![FlatStory {
                id: "main".into(),
                text: String::new(),
                carets: Vec::new(),
            }],
        };
        let mut path = Vec::new();
        flat.collect(&doc.body, &mut path, 0);
        flat
    }

    pub fn main(&self) -> &FlatStory {
        &self.stories[0]
    }
    pub fn story(&self, id: &str) -> Option<&FlatStory> {
        self.stories.iter().find(|s| s.id == id)
    }
    pub fn locate(&self, caret: &Caret) -> Option<StoryOffset> {
        self.stories.iter().find_map(|s| {
            s.offset(caret).map(|offset| StoryOffset {
                story: s.id.clone(),
                offset,
            })
        })
    }
    pub fn caret(&self, story: &str, offset: usize) -> Option<Caret> {
        self.story(story)?.caret(offset)
    }

    fn collect(&mut self, blocks: &[Block], path: &mut Vec<usize>, story: usize) {
        for (bi, block) in blocks.iter().enumerate() {
            path.push(bi);
            match block {
                Block::Paragraph(p) => {
                    let mut offset = 0;
                    for (ii, inline) in p.content.iter().enumerate() {
                        if let Inline::TextBox { blocks, .. } = inline {
                            path.push(ii);
                            let id = format!(
                                "textbox:{}",
                                path.iter()
                                    .map(usize::to_string)
                                    .collect::<Vec<_>>()
                                    .join("/")
                            );
                            let next = self.stories.len();
                            self.stories.push(FlatStory {
                                id,
                                text: String::new(),
                                carets: Vec::new(),
                            });
                            self.collect(blocks, path, next);
                            path.pop();
                        }
                        let chars = inline_chars(inline);
                        debug_assert_eq!(chars.chars().count(), inline_len(inline));
                        for ch in chars.chars() {
                            self.stories[story].text.push(ch);
                            self.stories[story]
                                .carets
                                .push(Caret::at(path.clone(), offset));
                            offset += 1;
                        }
                    }
                    self.stories[story].text.push('\n');
                    self.stories[story]
                        .carets
                        .push(Caret::at(path.clone(), offset));
                }
                Block::Table(t) => {
                    for (ri, row) in t.rows.iter().enumerate() {
                        for (ci, cell) in row.cells.iter().enumerate() {
                            path.extend([ri, ci]);
                            self.collect(&cell.blocks, path, story);
                            path.truncate(path.len() - 2);
                        }
                    }
                }
                Block::SectionProperties(_) | Block::Raw(_) => {}
            }
            path.pop();
        }
    }
}

fn inline_chars(inline: &Inline) -> String {
    match inline {
        Inline::Run(r) => r.text.clone(),
        Inline::Hyperlink(h) => h.runs.iter().map(|r| r.text.as_str()).collect(),
        Inline::Tab(_) => "\t".into(),
        Inline::Break(BreakKind::Line) => "\u{000b}".into(),
        Inline::Break(BreakKind::Page) => "\u{000c}".into(),
        Inline::Break(BreakKind::Column) => "\u{000e}".into(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, ParProps, Paragraph, Row, Run, RunProps, Table};

    fn run(s: &str) -> Inline {
        Inline::Run(Run {
            text: s.into(),
            props: RunProps::default(),
        })
    }
    fn para(items: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: items,
        })
    }
    fn cell(blocks: Vec<Block>) -> Cell {
        Cell {
            blocks,
            ..Default::default()
        }
    }

    #[test]
    fn flat_round_trip_every_caret_in_main_tables_and_textboxes() {
        let nested = Block::Table(Table {
            rows: vec![Row {
                cells: vec![cell(vec![para(vec![run("nested")])])],
                ..Default::default()
            }],
            ..Default::default()
        });
        let doc = Document {
            body: vec![
                para(vec![
                    run("a😀"),
                    Inline::Tab(RunProps::default()),
                    Inline::Break(BreakKind::Line),
                    Inline::Break(BreakKind::Page),
                    Inline::Break(BreakKind::Column),
                    Inline::Field {
                        raw: String::new(),
                        text: "invisible".into(),
                    },
                ]),
                Block::Table(Table {
                    rows: vec![Row {
                        cells: vec![
                            cell(vec![para(vec![run("first")]), nested]),
                            cell(vec![para(vec![])]),
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                para(vec![
                    run("host"),
                    Inline::TextBox {
                        raw: String::new(),
                        blocks: vec![para(vec![run("box")])],
                    },
                ]),
            ],
        };
        let flat = FlatDocument::new(&doc);
        assert_eq!(
            flat.main().text,
            "a😀\t\u{000b}\u{000c}\u{000e}\nfirst\nnested\n\nhost\n"
        );
        assert_eq!(flat.stories[1].id, "textbox:2/1");
        assert_eq!(flat.stories[1].text, "box\n");
        for story in &flat.stories {
            assert_eq!(story.text.chars().count(), story.len());
            for offset in 0..story.len() {
                let caret = story.caret(offset).unwrap();
                assert_eq!(
                    flat.locate(&caret),
                    Some(StoryOffset {
                        story: story.id.clone(),
                        offset
                    })
                );
                assert_eq!(flat.caret(&story.id, offset), Some(caret));
            }
            assert!(
                story.caret(story.len()).is_none(),
                "final mark has no following caret"
            );
        }
    }
}
