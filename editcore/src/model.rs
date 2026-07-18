//! The rich-text data model: blocks, runs, and the document they compose.

/// A single formatted span of text within a block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub link: Option<String>,
}

impl Run {
    /// A plain, unformatted run containing `text`.
    pub fn plain(text: &str) -> Run {
        Run {
            text: text.to_string(),
            ..Default::default()
        }
    }
}

/// A block-level element: a paragraph or a list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Run>),
    ListItem {
        ordered: bool,
        level: u8,
        runs: Vec<Run>,
    },
}

impl Block {
    fn runs(&self) -> &[Run] {
        match self {
            Block::Paragraph(runs) => runs,
            Block::ListItem { runs, .. } => runs,
        }
    }
}

/// A rich-text document: an ordered sequence of blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichText {
    pub blocks: Vec<Block>,
}

impl RichText {
    /// A new document containing a single empty paragraph.
    pub fn new() -> RichText {
        RichText {
            blocks: vec![Block::Paragraph(vec![])],
        }
    }

    /// Flattens the document to plain text: runs within a block are
    /// concatenated, and blocks are joined with `\n`.
    pub fn plain(&self) -> String {
        self.blocks
            .iter()
            .map(|block| {
                block
                    .runs()
                    .iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// True when the document is a single empty paragraph, as produced by
    /// [`RichText::new`].
    pub fn is_empty(&self) -> bool {
        matches!(self.blocks.as_slice(), [Block::Paragraph(runs)] if runs.is_empty())
    }
}

impl Default for RichText {
    fn default() -> Self {
        RichText::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_richtext_has_one_empty_paragraph() {
        let rt = RichText::new();
        assert_eq!(rt.blocks.len(), 1);
        assert!(rt.is_empty());
        assert_eq!(rt.plain(), "");
    }
    #[test]
    fn plain_flattens_runs_and_paragraphs() {
        let rt = RichText {
            blocks: vec![
                Block::Paragraph(vec![
                    Run::plain("Hello "),
                    Run {
                        text: "world".into(),
                        bold: true,
                        ..Run::plain("")
                    },
                ]),
                Block::Paragraph(vec![Run::plain("Next")]),
            ],
        };
        assert_eq!(rt.plain(), "Hello world\nNext");
    }
}
