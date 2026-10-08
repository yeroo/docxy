//! Word's names for new documents (#631): a new blank document is
//! `Document1`, `Document2`, … (numbers never reused in a session), and the
//! first save of one proposes its first words as the file name.
//!
//! Project's blank plans use the same scheme with its own prefix (#588):
//! `Project1`, `Project2`, …, and the window title is `<stem>  -  docxy`.
use docxcore::model::Document;

const PREFIX: &str = "Document";
const PROJECT_PREFIX: &str = "Project";

/// The longest name proposed from a document's first words, in characters.
const MAX_FIRST_WORDS: usize = 50;

/// `n` when `title` is Word's `Document<n>` name of a new document.
pub(crate) fn document_number(title: &str) -> Option<u32> {
    let digits = title.strip_prefix(PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&n| n > 0)
}

/// The number the next new document takes after a session whose never-saved
/// document tabs are titled `titles`: one past the highest `Document<n>`, so
/// a restored `Document3` is never named twice.
pub(crate) fn next_after<'a>(titles: impl IntoIterator<Item = &'a str>) -> u32 {
    titles
        .into_iter()
        .filter_map(document_number)
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

/// The next new document's title, advancing the session's counter.
pub(crate) fn next_document_title(next: &mut u32) -> String {
    let title = format!("{PREFIX}{next}");
    *next = next.saturating_add(1);
    title
}

/// `n` when `title` is Project's `Project<n>` name of a new blank plan
/// (#588): the prefix, then digits, nothing else — `Project1.yppx` and
/// `project1` are file names, not minted names.
pub(crate) fn project_number(title: &str) -> Option<u32> {
    let digits = title.strip_prefix(PROJECT_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&n| n > 0)
}

/// The number the next new plan takes after a session whose never-saved
/// Project tabs are titled `titles`: one past the highest `Project<n>`, so
/// a restored `Project3` is never named twice.
pub(crate) fn next_project_after<'a>(titles: impl IntoIterator<Item = &'a str>) -> u32 {
    titles
        .into_iter()
        .filter_map(project_number)
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

/// The next new plan's title, advancing the run's counter.
pub(crate) fn next_project_title(next: &mut u32) -> String {
    let title = format!("{PROJECT_PREFIX}{next}");
    *next = next.saturating_add(1);
    title
}

/// The window title drawn for the active tab's title `tab_title`: Project's
/// `<stem>  -  <app>` (stem = the title without its extension, two spaces
/// either side of the hyphen). No tab draws the bare brand.
pub(crate) fn window_title(tab_title: Option<&str>) -> String {
    match tab_title {
        Some(t) => {
            let stem = std::path::Path::new(t)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| t.to_string());
            format!("{stem}  -  docxy")
        }
        None => "docxy".to_string(),
    }
}

/// Word's file name for the first save of `doc`: its first line of text, up
/// to the end of the first sentence or clause, without characters a file
/// name cannot hold, and at most [`MAX_FIRST_WORDS`] characters (cut at a
/// word boundary). `None` when the document has no usable text.
pub(crate) fn first_words_name(doc: &Document) -> Option<String> {
    let text = doc.body.iter().map(|b| b.plain_text()).find_map(|t| {
        t.split(['\n', '\t', '\r', '\u{b}'])
            .find(|line| !line.trim().is_empty())
            .map(str::to_string)
    })?;
    let clause = text
        .split(['.', '!', '?', ';', ':', '\u{2026}'])
        .find(|s| !s.trim().is_empty())
        .unwrap_or("");
    let cleaned: String = clause
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\\' | '/' | '*' | '"' | '<' | '>' | '|'))
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let mut name = String::new();
    for word in words {
        let extra = usize::from(!name.is_empty()) + word.chars().count();
        if name.chars().count() + extra > MAX_FIRST_WORDS {
            if name.is_empty() {
                // One very long word: cut it.
                name = word.chars().take(MAX_FIRST_WORDS).collect();
            }
            break;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    // Windows refuses trailing dots and spaces, and the device names.
    let name = name.trim_end_matches(['.', ' ']).to_string();
    let reserved = matches!(
        name.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
    ) || {
        let upper = name.to_ascii_uppercase();
        (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit()
    };
    (!name.is_empty() && !reserved).then_some(name)
}

/// The stem Save proposes for a never-saved document titled `title`: its
/// first words, else the title itself (`Document1`).
pub(crate) fn untitled_stem(doc: &Document, title: &str) -> String {
    first_words_name(doc).unwrap_or_else(|| {
        std::path::Path::new(title)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| title.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(md: &str) -> Document {
        docxcore::markdown::from_markdown(md)
    }

    #[test]
    fn document_numbers() {
        assert_eq!(document_number("Document1"), Some(1));
        assert_eq!(document_number("Document27"), Some(27));
        for not in [
            "Document",
            "Document0",
            "Document1.docx",
            "document1",
            "Doc1",
            "Document-1",
        ] {
            assert_eq!(document_number(not), None, "{not}");
        }
    }

    #[test]
    fn numbers_continue_and_are_never_reused() {
        assert_eq!(next_after([]), 1);
        assert_eq!(next_after(["Document1", "Document3", "notes.docx"]), 4);
        let mut next = next_after(["Document2"]);
        assert_eq!(next_document_title(&mut next), "Document3");
        assert_eq!(next_document_title(&mut next), "Document4");
        // Closing Document4 does not give its number back: the counter only
        // goes up.
        assert_eq!(next_document_title(&mut next), "Document5");
    }

    #[test]
    fn project_numbers() {
        assert_eq!(project_number("Project1"), Some(1));
        assert_eq!(project_number("Project27"), Some(27));
        for not in [
            "Project",
            "Project0",
            "Project1.yppx",
            "project1",
            "Proj1",
            "Project-1",
        ] {
            assert_eq!(project_number(not), None, "{not}");
        }
    }

    #[test]
    fn project_numbers_continue_and_are_never_reused() {
        assert_eq!(next_project_after([]), 1);
        assert_eq!(
            next_project_after(["Project1", "Project3", "notes.yppx"]),
            4
        );
        let mut next = next_project_after(["Project2"]);
        assert_eq!(next_project_title(&mut next), "Project3");
        assert_eq!(next_project_title(&mut next), "Project4");
        // Closing Project4 does not give its number back: the counter only
        // goes up.
        assert_eq!(next_project_title(&mut next), "Project5");
    }

    #[test]
    fn window_title_strips_the_extension() {
        assert_eq!(
            window_title(Some("Project1")).as_str(),
            "Project1  -  docxy"
        );
        assert_eq!(
            window_title(Some("sample.docx")).as_str(),
            "sample  -  docxy"
        );
        assert_eq!(
            window_title(Some("Untitled.xlsx")).as_str(),
            "Untitled  -  docxy"
        );
        assert_eq!(window_title(None).as_str(), "docxy");
    }

    #[test]
    fn first_words_make_the_name() {
        assert_eq!(
            first_words_name(&doc("Quarterly report for the north region\n")).as_deref(),
            Some("Quarterly report for the north region")
        );
        // Up to the first sentence's end.
        assert_eq!(
            first_words_name(&doc("Minutes. Present: everyone\n")).as_deref(),
            Some("Minutes")
        );
        assert_eq!(
            first_words_name(&doc("Agenda: budget\n")).as_deref(),
            Some("Agenda")
        );
        // The first paragraph with text, past empty ones and headings alike.
        assert_eq!(
            first_words_name(&doc("\n\n# Plan for 2027\n\nBody\n")).as_deref(),
            Some("Plan for 2027")
        );
    }

    #[test]
    fn characters_a_file_name_cannot_hold_are_dropped() {
        assert_eq!(
            first_words_name(&doc("Q3 <draft> \"final\" A/B | C*\n")).as_deref(),
            Some("Q3 draft final AB C")
        );
        assert_eq!(first_words_name(&doc("...\n")), None);
        assert_eq!(first_words_name(&doc("CON\n")), None);
        assert_eq!(first_words_name(&doc("com1\n")), None);
    }

    #[test]
    fn a_long_line_is_cut_at_a_word_boundary() {
        let line = "Annual summary of regional sales figures and forecasts for the coming year";
        let name = first_words_name(&doc(&format!("{line}\n"))).unwrap();
        assert_eq!(name, "Annual summary of regional sales figures and");
        assert!(name.chars().count() <= MAX_FIRST_WORDS);
        let word = "x".repeat(80);
        assert_eq!(
            first_words_name(&doc(&format!("{word}\n"))).unwrap().len(),
            MAX_FIRST_WORDS
        );
    }

    #[test]
    fn an_empty_document_keeps_its_title() {
        let empty = Document {
            body: vec![docxcore::model::Block::Paragraph(Default::default())],
        };
        assert_eq!(first_words_name(&empty), None);
        assert_eq!(untitled_stem(&empty, "Document1"), "Document1");
        assert_eq!(
            untitled_stem(&doc("Quarterly report\n"), "Document1"),
            "Quarterly report"
        );
    }
}
