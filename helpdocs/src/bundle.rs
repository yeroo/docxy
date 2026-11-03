//! A manual bundle: `REVISION`, `contents.txt`, one `<id>.md` per page and
//! `images/`. A manual always loads; what is wrong with it is listed in
//! [`Manual::problems`], so one bad page never hides the rest.

use crate::md::{self, Block, Link};
use crate::{slug, valid_page_id};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// The file stem, `[a-z0-9-]+`.
    pub id: String,
    /// The front matter's `title`, else the id.
    pub title: String,
    /// The area page this page belongs to (a slug), from the front matter.
    pub area: Option<String>,
    /// The context ids this page answers (`bold`, `dialog:font`).
    pub keys: Vec<String>,
    pub blocks: Vec<Block>,
}

impl Page {
    /// The slugs of the page's headings, in order.
    pub fn heading_slugs(&self) -> impl Iterator<Item = &str> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Heading { slug, .. } => Some(slug.as_str()),
            _ => None,
        })
    }
}

/// One line of the contents tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// 0 at the top; each two spaces of indent is one level.
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// `contents.txt` is absent.
    NoContents,
    /// Listed in the contents, but there is no `<id>.md`.
    MissingPage(String),
    /// A `<id>.md` the contents do not list (it still loads, after the rest).
    UnlistedPage(String),
    /// The contents list the id twice (the second is ignored).
    DuplicateEntry(String),
    /// A page file or contents line whose id is not `[a-z0-9-]+` (skipped).
    BadPageId(String),
    /// Two pages claim one key; the first in contents order keeps it.
    DuplicateKey {
        key: String,
        kept: String,
        dropped: String,
    },
    /// A link to a page or heading that does not exist, or not a link the
    /// subset knows.
    BrokenLink { page: String, target: String },
    /// An image the bundle does not hold.
    MissingImage { page: String, src: String },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manual {
    /// The spec-repo commit the bundle was published from.
    pub revision: Option<String>,
    /// The contents tree in order, listed pages that exist only.
    pub contents: Vec<Entry>,
    /// Every loaded page: contents order, then unlisted pages by id.
    pub pages: Vec<Page>,
    /// Areas (slugs) whose commands must each have an exact page: the
    /// `# strict: a, b` line before the contents' first entry.
    pub strict: Vec<String>,
    /// The bundle's image paths (`images/x.png`).
    pub images: Vec<String>,
    pub problems: Vec<Problem>,
}

impl Manual {
    /// Load the bundle in `dir` (one editor's directory). Unreadable files
    /// count as absent.
    pub fn load(dir: &Path) -> std::io::Result<Manual> {
        let mut files = Vec::new();
        collect(dir, dir, &mut files)?;
        Ok(Manual::from_files(files))
    }

    /// Build a manual from `(path, bytes)` pairs, paths relative to the
    /// editor's directory (`home.md`, `images/x.png`); `\` is read as `/`.
    /// What `load` reads, and what a host embeds with `include_bytes!`.
    pub fn from_files<P: AsRef<str>, B: AsRef<[u8]>>(
        files: impl IntoIterator<Item = (P, B)>,
    ) -> Manual {
        let mut m = Manual::default();
        let mut contents_txt = None;
        let mut sources: Vec<(String, String)> = Vec::new();
        for (path, bytes) in files {
            let path = path.as_ref().replace('\\', "/");
            let path = path.trim_start_matches("./");
            let text = || String::from_utf8_lossy(bytes.as_ref()).into_owned();
            if path == "REVISION" {
                m.revision = text().lines().next().map(|l| l.trim().to_string());
            } else if path == "contents.txt" {
                contents_txt = Some(text());
            } else if path.starts_with("images/") {
                m.images.push(path.to_string());
            } else if let Some(stem) = path.strip_suffix(".md") {
                if path.contains('/') {
                    continue; // only the editor directory's own pages
                }
                if valid_page_id(stem) {
                    sources.push((stem.to_string(), text()));
                } else {
                    m.problems.push(Problem::BadPageId(stem.to_string()));
                }
            }
        }
        m.images.sort();
        sources.sort();
        let Some(contents_txt) = contents_txt else {
            m.problems.push(Problem::NoContents);
            m.pages = sources.iter().map(|(id, s)| page(id, s)).collect();
            m.check();
            return m;
        };
        let (entries, strict) = parse_contents(&contents_txt, &mut m.problems);
        m.strict = strict;
        for e in entries {
            match sources.iter().position(|(id, _)| *id == e.id) {
                Some(at) => {
                    let (id, src) = sources.remove(at);
                    m.pages.push(page(&id, &src));
                    m.contents.push(e);
                }
                None => m.problems.push(Problem::MissingPage(e.id)),
            }
        }
        for (id, src) in sources {
            m.problems.push(Problem::UnlistedPage(id.clone()));
            m.pages.push(page(&id, &src));
        }
        m.check();
        m
    }

    pub fn page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// Links, images and keys across pages.
    fn check(&mut self) {
        let mut problems = Vec::new();
        let mut keys: Vec<(&str, &str)> = Vec::new();
        for p in &self.pages {
            for key in &p.keys {
                match keys.iter().find(|(k, _)| k == key) {
                    Some((_, kept)) => problems.push(Problem::DuplicateKey {
                        key: key.clone(),
                        kept: kept.to_string(),
                        dropped: p.id.clone(),
                    }),
                    None => keys.push((key, &p.id)),
                }
            }
            for link in md::links(&p.blocks) {
                let ok = match link {
                    Link::Page { page, anchor } => self.page(page).is_some_and(|target| {
                        anchor
                            .as_ref()
                            .is_none_or(|a| target.heading_slugs().any(|s| s == a))
                    }),
                    Link::Anchor(a) => p.heading_slugs().any(|s| s == a),
                    Link::External(_) => true,
                    Link::Other(_) => false,
                };
                if !ok {
                    problems.push(Problem::BrokenLink {
                        page: p.id.clone(),
                        target: link_text(link),
                    });
                }
            }
            for b in &p.blocks {
                if let Block::Image { src, .. } = b {
                    let path = src.replace('\\', "/");
                    let path = path.trim_start_matches("./");
                    if !self.images.iter().any(|i| i == path) {
                        problems.push(Problem::MissingImage {
                            page: p.id.clone(),
                            src: src.clone(),
                        });
                    }
                }
            }
        }
        self.problems.extend(problems);
    }
}

fn link_text(link: &Link) -> String {
    match link {
        Link::Page {
            page,
            anchor: Some(a),
        } => format!("{page}#{a}"),
        Link::Page { page, anchor: None } => page.clone(),
        Link::Anchor(a) => format!("#{a}"),
        Link::External(t) | Link::Other(t) => t.clone(),
    }
}

/// The contents' entries and its strict areas. Duplicate and bad ids are
/// reported and skipped.
fn parse_contents(text: &str, problems: &mut Vec<Problem>) -> (Vec<Entry>, Vec<String>) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut strict = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        let id = line.trim_start();
        if id.is_empty() {
            continue;
        }
        if id.starts_with('#') {
            // The directive only before the first entry, spelled exactly so.
            if let Some(list) = line.strip_prefix("# strict:") {
                if entries.is_empty() {
                    strict.extend(list.split(',').map(slug).filter(|s| !s.is_empty()));
                }
            }
            continue;
        }
        if !valid_page_id(id) {
            problems.push(Problem::BadPageId(id.to_string()));
        } else if entries.iter().any(|e| e.id == id) {
            problems.push(Problem::DuplicateEntry(id.to_string()));
        } else {
            let indent = line.len() - id.len();
            entries.push(Entry {
                id: id.to_string(),
                depth: indent / 2,
            });
        }
    }
    (entries, strict)
}

/// A page from its source: front matter, then the body.
fn page(id: &str, src: &str) -> Page {
    let src = src.replace("\r\n", "\n");
    let src = src.strip_prefix('\u{feff}').unwrap_or(&src);
    let mut title = None;
    let mut area = None;
    let mut keys = Vec::new();
    let mut body = src;
    if let Some(rest) = src.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                let Some((k, v)) = line.split_once(':') else {
                    continue;
                };
                // A value's ` #` comment is dropped.
                let v = v.split(" #").next().unwrap_or("").trim();
                match k.trim() {
                    "title" if !v.is_empty() => title = Some(v.to_string()),
                    "area" if !v.is_empty() => area = Some(slug(v)),
                    "keys" => keys.extend(
                        v.split(',')
                            .map(|k| k.trim().to_string())
                            .filter(|k| !k.is_empty()),
                    ),
                    _ => {}
                }
            }
            let after = &rest[end + 4..];
            body = after.split_once('\n').map_or("", |(_, b)| b);
        }
    }
    Page {
        id: id.to_string(),
        title: title.unwrap_or_else(|| id.to_string()),
        area,
        keys,
        blocks: md::parse(body),
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((rel.to_string_lossy().into_owned(), bytes));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(crate) fn fixture(editor: &str) -> Manual {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixture")
            .join(editor);
        Manual::load(&dir).expect("fixture manual")
    }

    fn files(extra: &[(&str, &str)]) -> Manual {
        let mut v = vec![
            ("contents.txt", "home\n  child\n"),
            (
                "home.md",
                "---\ntitle: Home\n---\n# Home\nSee [child](child).\n",
            ),
            ("child.md", "# Child\n"),
        ];
        v.extend_from_slice(extra);
        Manual::from_files(v)
    }

    #[test]
    fn the_fixtures_load_cleanly() {
        for editor in ["word", "excel", "project"] {
            let m = fixture(editor);
            assert_eq!(m.problems, vec![], "{editor}");
            assert!(m.revision.is_some(), "{editor}");
            assert!(m.pages.len() >= 2, "{editor}");
        }
        let word = fixture("word");
        assert_eq!(word.strict, vec!["help"]);
        assert_eq!(word.images, vec!["images/ribbon.png"]);
        let home = word.page("home").unwrap();
        assert_eq!(home.title, "Home tab");
        assert_eq!(home.area.as_deref(), Some("home"));
    }

    #[test]
    fn the_contents_tree_nests() {
        let word = fixture("word");
        let tree: Vec<(&str, usize)> = word
            .contents
            .iter()
            .map(|e| (e.id.as_str(), e.depth))
            .collect();
        assert_eq!(
            tree,
            [
                ("welcome", 0),
                ("home", 0),
                ("home-font", 1),
                ("insert", 0),
                ("tables", 1),
                ("images", 1),
                ("keyboard", 0),
                ("help", 0),
            ]
        );
        let ids: Vec<&str> = word.pages.iter().map(|p| p.id.as_str()).collect();
        let listed: Vec<&str> = tree.iter().map(|t| t.0).collect();
        assert_eq!(ids, listed);
    }

    #[test]
    fn a_broken_link_is_reported_and_the_rest_loads() {
        let m = Manual::from_files([
            ("contents.txt", "home\nchild\n"),
            ("home.md", "# Home\n"),
            (
                "child.md",
                "# Child\n[x](nowhere) [y](home#nope) [z](#child) [w](home#home) [v](notes.txt)",
            ),
        ]);
        let broken: Vec<&Problem> = m
            .problems
            .iter()
            .filter(|p| matches!(p, Problem::BrokenLink { .. }))
            .collect();
        assert!(broken.contains(&&Problem::BrokenLink {
            page: "child".into(),
            target: "nowhere".into()
        }));
        assert!(broken.contains(&&Problem::BrokenLink {
            page: "child".into(),
            target: "home#nope".into()
        }));
        assert!(broken.contains(&&Problem::BrokenLink {
            page: "child".into(),
            target: "notes.txt".into()
        }));
        assert!(!broken.iter().any(|p| matches!(p,
            Problem::BrokenLink { target, .. } if target == "#child" || target == "home#home")));
        assert!(m.page("home").is_some());
    }

    #[test]
    fn an_unlisted_page_is_reported_and_still_loads() {
        let m = files(&[("extra.md", "# Extra\n")]);
        assert_eq!(m.problems, vec![Problem::UnlistedPage("extra".into())]);
        assert_eq!(m.pages.last().unwrap().id, "extra");
        assert_eq!(m.contents.len(), 2);
    }

    #[test]
    fn a_missing_image_is_reported() {
        let m = files(&[
            ("a.md", "![x](images/there.png)\n![y](images/gone.png)\n"),
            ("images/there.png", "png"),
        ]);
        assert!(m.problems.contains(&Problem::MissingImage {
            page: "a".into(),
            src: "images/gone.png".into()
        }));
        assert!(
            !m.problems.iter().any(
                |p| matches!(p, Problem::MissingImage { src, .. } if src == "images/there.png")
            )
        );
    }

    #[test]
    fn contents_problems_are_reported() {
        let m = Manual::from_files([
            ("contents.txt", "a\nghost\na\nBad_Id\n"),
            ("a.md", "# A"),
            ("Upper.md", "# U"),
        ]);
        assert_eq!(
            m.problems,
            vec![
                Problem::BadPageId("Upper".into()),
                Problem::DuplicateEntry("a".into()),
                Problem::BadPageId("Bad_Id".into()),
                Problem::MissingPage("ghost".into()),
            ]
        );
        assert_eq!(m.pages.len(), 1);
        assert_eq!(
            Manual::from_files([("a.md", "# A")]).problems,
            vec![Problem::NoContents]
        );
    }

    #[test]
    fn a_key_claimed_twice_stays_with_the_first_page() {
        let m = Manual::from_files([
            ("contents.txt", "b\na\n"),
            ("a.md", "---\nkeys: bold\n---\n"),
            ("b.md", "---\nkeys: bold, italic\n---\n"),
        ]);
        assert_eq!(
            m.problems,
            vec![Problem::DuplicateKey {
                key: "bold".into(),
                kept: "b".into(),
                dropped: "a".into()
            }]
        );
    }

    #[test]
    fn the_strict_directive_counts_only_before_the_first_entry() {
        let m = Manual::from_files([
            (
                "contents.txt",
                "# a comment\n# strict: Home, Page Layout\nx\n# strict: insert\n",
            ),
            ("x.md", ""),
        ]);
        assert_eq!(m.strict, vec!["home", "page-layout"]);
        let m = Manual::from_files([("contents.txt", "# Strict: home\nx\n"), ("x.md", "")]);
        assert_eq!(m.strict, Vec::<String>::new());
    }

    #[test]
    fn backslash_paths_and_crlf_front_matter() {
        let m = Manual::from_files([
            ("contents.txt", "a\r\n"),
            (
                "a.md",
                "---\r\ntitle: A page   # a comment\r\narea: Page Layout\r\n---\r\n![p](images/p.png)\r\n",
            ),
            ("images\\p.png", "png"),
        ]);
        assert_eq!(m.problems, vec![]);
        let a = m.page("a").unwrap();
        assert_eq!(a.title, "A page");
        assert_eq!(a.area.as_deref(), Some("page-layout"));
        assert_eq!(m.images, vec!["images/p.png"]);
    }
}
