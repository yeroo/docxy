//! Full-text search over a manual's pages, with a stable ranking.
//!
//! A query is lowercase word tokens (letters and digits); a page matches when
//! it holds every term. Per term a page scores 10 when the title holds it, 5
//! when a heading does, and 1 per body occurrence up to 5. Higher totals come
//! first; equal totals keep the pages' order (contents order).

use crate::Manual;
use crate::md::{self, Block};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub page: String,
    pub title: String,
    pub score: u32,
    /// The first body line holding a query term, cut to 120 characters;
    /// the title when no body line does.
    pub snippet: String,
}

/// The longest snippet, in characters.
pub const SNIPPET_CHARS: usize = 120;

pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The body of a page as lines: a paragraph, a list item, a table row, a code
/// line or an image's alt text each.
fn body_lines(blocks: &[Block]) -> Vec<String> {
    let mut lines = Vec::new();
    for b in blocks {
        match b {
            Block::Heading { .. } => {}
            Block::Code { text, .. } => lines.extend(text.lines().map(str::to_string)),
            Block::Image { alt, .. } => lines.push(alt.clone()),
            Block::Table { header, rows, .. } => {
                for row in std::iter::once(header).chain(rows) {
                    let cells: Vec<String> = row.iter().map(|c| md::plain(c)).collect();
                    lines.push(cells.join(" | "));
                }
            }
            Block::Paragraph(text) => lines.push(md::plain(text)),
            Block::List { items, .. } => {
                for item in items {
                    lines.push(md::plain(&item.text));
                    lines.extend(body_lines(&item.children));
                }
            }
        }
    }
    lines
}

/// Up to `SNIPPET_CHARS` of `line`, the window moved right as far as needed
/// to hold the first query term it finds.
fn snippet(line: &str, terms: &[String]) -> String {
    let chars: Vec<char> = line.chars().collect();
    // The first alphanumeric run that `tokens` would read as a term, by
    // `tokens`' own rule (whole-run lowercasing), so the two never disagree.
    let mut end = 0;
    let mut at = 0;
    while at < chars.len() {
        if !chars[at].is_alphanumeric() {
            at += 1;
            continue;
        }
        let run_end = (at..chars.len())
            .find(|&i| !chars[i].is_alphanumeric())
            .unwrap_or(chars.len());
        let run: String = chars[at..run_end].iter().collect::<String>().to_lowercase();
        if terms.contains(&run) {
            end = run_end;
            break;
        }
        at = run_end;
    }
    let start = end.saturating_sub(SNIPPET_CHARS);
    chars[start..].iter().take(SNIPPET_CHARS).collect()
}

pub fn search(manual: &Manual, query: &str) -> Vec<Hit> {
    // Each term once, in query order.
    let mut terms: Vec<String> = Vec::new();
    for t in tokens(query) {
        if !terms.contains(&t) {
            terms.push(t);
        }
    }
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for page in &manual.pages {
        let title = tokens(&page.title);
        let mut headings = Vec::new();
        for b in &page.blocks {
            if let Block::Heading { text, .. } = b {
                headings.extend(tokens(&md::plain(text)));
            }
        }
        let lines = body_lines(&page.blocks);
        let body: Vec<Vec<String>> = lines.iter().map(|l| tokens(l)).collect();
        let mut score = 0;
        let mut all = true;
        for term in &terms {
            let in_title = title.contains(term);
            let in_heading = headings.contains(term);
            let n = body.iter().flatten().filter(|t| *t == term).count() as u32;
            if !in_title && !in_heading && n == 0 {
                all = false;
                break;
            }
            score += 10 * in_title as u32 + 5 * in_heading as u32 + n.min(5);
        }
        if !all {
            continue;
        }
        let snippet = lines
            .iter()
            .zip(&body)
            .find(|(_, toks)| toks.iter().any(|t| terms.contains(t)))
            .map(|(line, _)| snippet(line.trim(), &terms))
            .unwrap_or_else(|| page.title.clone());
        hits.push(Hit {
            page: page.id.clone(),
            title: page.title.clone(),
            score,
            snippet,
        });
    }
    // Stable: equal scores keep the pages' order.
    hits.sort_by_key(|h| std::cmp::Reverse(h.score));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::fixture;

    fn manual(pages: &[(&str, &str)]) -> Manual {
        let contents: String = pages.iter().map(|(id, _)| format!("{id}\n")).collect();
        let mut files = vec![("contents.txt".to_string(), contents)];
        files.extend(
            pages
                .iter()
                .map(|(id, s)| (format!("{id}.md"), s.to_string())),
        );
        Manual::from_files(files)
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.page.as_str()).collect()
    }

    #[test]
    fn a_title_hit_outranks_body_hits() {
        let m = manual(&[
            ("body", "Bold bold bold bold bold bold bold text."),
            ("titled", "---\ntitle: Bold\n---\nNothing here."),
            ("heading", "## Bold\n"),
        ]);
        let hits = search(&m, "bold");
        assert_eq!(ids(&hits), ["titled", "body", "heading"]);
        assert_eq!(hits[0].score, 10);
        // Body occurrences are capped at five.
        assert_eq!(hits[1].score, 5);
        assert_eq!(hits[2].score, 5);
    }

    #[test]
    fn every_term_must_match() {
        let m = manual(&[("a", "font size"), ("b", "font colour"), ("c", "size")]);
        assert_eq!(ids(&search(&m, "font size")), ["a"]);
        assert_eq!(ids(&search(&m, "font")), ["a", "b"]);
        assert!(search(&m, "  ").is_empty());
    }

    #[test]
    fn ties_follow_contents_order() {
        let m = manual(&[("z", "table"), ("a", "table"), ("m", "table")]);
        assert_eq!(ids(&search(&m, "table")), ["z", "a", "m"]);
    }

    #[test]
    fn the_query_is_case_insensitive() {
        let m = manual(&[("a", "The **Ribbon** is here.")]);
        assert_eq!(ids(&search(&m, "RIBBON")), ["a"]);
        assert_eq!(ids(&search(&m, "ribbon")), ["a"]);
    }

    #[test]
    fn the_snippet_is_the_first_body_line_with_a_term() {
        let long = format!("{}table", "x ".repeat(100));
        let m = manual(&[
            ("a", "---\ntitle: Tables\n---\nIntro.\n\n- a table item\n"),
            ("b", "---\ntitle: Table too\n---\nnone"),
            ("c", &long),
        ]);
        let hits = search(&m, "table");
        let snippet = |id: &str| hits.iter().find(|h| h.page == id).unwrap().snippet.clone();
        assert_eq!(snippet("a"), "a table item");
        assert_eq!(snippet("b"), "Table too");
        assert_eq!(snippet("c").chars().count(), SNIPPET_CHARS);
        assert!(snippet("c").ends_with("table"), "{}", snippet("c"));
    }

    #[test]
    fn the_snippet_finds_a_final_sigma_word_by_the_token_rule() {
        let line = format!("{}ΟΔΟΣ", "x ".repeat(100));
        let m = manual(&[("a", &line)]);
        let hits = search(&m, "ΟΔΟΣ");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.ends_with("ΟΔΟΣ"), "{}", hits[0].snippet);
    }

    #[test]
    fn the_snippet_matches_whole_tokens() {
        let line = format!("tables {} table", "x ".repeat(100));
        let m = manual(&[("a", &line)]);
        let hits = search(&m, "table");
        assert!(hits[0].snippet.ends_with(" table"), "{}", hits[0].snippet);
    }

    #[test]
    fn a_repeated_term_counts_once() {
        let m = manual(&[("p", "---\ntitle: A\n---\nb b")]);
        assert_eq!(search(&m, "a b a")[0].score, search(&m, "a b")[0].score);
    }

    #[test]
    fn the_fixture_finds_a_known_term() {
        let word = fixture("word");
        let hits = search(&word, "Strikethrough");
        assert_eq!(ids(&hits), ["home-font"]);
    }
}
