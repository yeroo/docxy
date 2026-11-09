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
    /// A `.md` file in a subdirectory (not loaded: pages live at the top).
    NestedPage(String),
    /// A page whose front matter has no closing `---` line (its whole file
    /// is read as the body).
    UnclosedFrontMatter(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manual {
    /// The spec-repo commit the bundle was published from: `REVISION`'s
    /// first line, trimmed; `None` when the file is missing or that line is
    /// blank.
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
        Ok(Manual::from_files(files(dir)?))
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
            let path = bundle_path(path.as_ref());
            let path = path.as_str();
            let text = || {
                let t = String::from_utf8_lossy(bytes.as_ref()).into_owned();
                t.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(t)
            };
            if path == "REVISION" {
                m.revision = text()
                    .lines()
                    .next()
                    .map(|l| l.trim().to_string())
                    .filter(|r| !r.is_empty());
            } else if path == "contents.txt" {
                contents_txt = Some(text());
            } else if path.starts_with("images/") {
                m.images.push(path.to_string());
            } else if let Some(stem) = path.strip_suffix(".md") {
                if path.contains('/') {
                    // Only the editor directory's own pages load.
                    m.problems.push(Problem::NestedPage(path.to_string()));
                    continue;
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
            for (id, src) in &sources {
                let p = page(id, src, &mut m.problems);
                m.pages.push(p);
            }
            m.check();
            return m;
        };
        let (entries, strict) = parse_contents(&contents_txt, &mut m.problems);
        m.strict = strict;
        for e in entries {
            match sources.iter().position(|(id, _)| *id == e.id) {
                Some(at) => {
                    let (id, src) = sources.remove(at);
                    let p = page(&id, &src, &mut m.problems);
                    m.pages.push(p);
                    m.contents.push(e);
                }
                None => m.problems.push(Problem::MissingPage(e.id)),
            }
        }
        for (id, src) in sources {
            m.problems.push(Problem::UnlistedPage(id.clone()));
            let p = page(&id, &src, &mut m.problems);
            m.pages.push(p);
        }
        m.check();
        m
    }

    /// The revision as the Help footer shows it (#1857): a commit (all
    /// ASCII hex, longer than 7) cut to its first 7 characters, as
    /// `buildinfo`'s `short_commit` cuts the binary's; anything else whole.
    pub fn short_revision(&self) -> Option<&str> {
        let r = self.revision.as_deref()?;
        let commit = r.len() > 7 && r.bytes().all(|b| b.is_ascii_hexdigit());
        Some(if commit { &r[..7] } else { r })
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
            for src in md::images(&p.blocks) {
                if !self.images.contains(&bundle_path(src)) {
                    problems.push(Problem::MissingImage {
                        page: p.id.clone(),
                        src: src.to_string(),
                    });
                }
            }
        }
        self.problems.extend(problems);
    }
}

/// A path inside the bundle as it is compared: `/` separators, no `./`.
fn bundle_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.trim_start_matches("./").to_string()
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
/// An unclosed front matter is reported, and the whole file is the body.
fn page(id: &str, src: &str, problems: &mut Vec<Problem>) -> Page {
    let src = src.replace("\r\n", "\n");
    let src = src.as_str();
    let mut title = None;
    let mut area = None;
    let mut keys = Vec::new();
    let mut body = src;
    // Front matter opens on a first line that is `---` and closes at the
    // next such line, both compared after trailing spaces are dropped.
    let is_fence = |line: &str| line.trim_end() == "---";
    let opened = src
        .split_inclusive('\n')
        .next()
        .filter(|l| is_fence(l))
        .map(|l| &src[l.len()..]);
    let front = opened.and_then(|rest| {
        let mut at = 0;
        for line in rest.split_inclusive('\n') {
            if is_fence(line) {
                return Some((&rest[..at], &rest[at + line.len()..]));
            }
            at += line.len();
        }
        None
    });
    if opened.is_some() && front.is_none() {
        problems.push(Problem::UnclosedFrontMatter(id.to_string()));
    }
    if let Some((matter, after)) = front {
        for line in matter.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            // The title is kept as written (`#N/A`); in `area` and `keys` a
            // ` #` starts a comment.
            let comment_free = || v.split(" #").next().unwrap_or("").trim();
            match k.trim() {
                "title" if !v.trim().is_empty() => title = Some(v.trim().to_string()),
                "area" if !comment_free().is_empty() => area = Some(slug(comment_free())),
                "keys" => keys.extend(
                    comment_free()
                        .split(',')
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty()),
                ),
                _ => {}
            }
        }
        body = after;
    }
    Page {
        id: id.to_string(),
        title: title.unwrap_or_else(|| id.to_string()),
        area,
        keys,
        blocks: md::parse(body),
    }
}

/// The files of the bundle in `dir` as `(path, bytes)`, paths relative to
/// it with `/` (`images/x.png`): what [`Manual::load`] reads, for a host
/// that also needs the image bytes (#1855). Only the root's listing can
/// fail; links are not followed (see [`collect`]).
pub fn files(dir: &Path) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    collect(dir, dir, &mut out)?;
    for (path, _) in &mut out {
        *path = bundle_path(path);
    }
    Ok(out)
}

/// The files under `dir`. Only the root's listing can fail the load: a
/// subdirectory that cannot be listed, an entry that cannot be read and a
/// symbolic link (to a directory or not; a junction too) count as absent, so
/// a link never takes the walk outside the bundle or round in a loop.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            let _ = collect(root, &path, out);
        } else if kind.is_file() {
            if let (Ok(bytes), Ok(rel)) = (std::fs::read(&path), path.strip_prefix(root)) {
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
                "---\r\ntitle: A page\r\narea: Page Layout   # a comment\r\n---\r\n![p](images/p.png)\r\n",
            ),
            ("images\\p.png", "png"),
        ]);
        assert_eq!(m.problems, vec![]);
        let a = m.page("a").unwrap();
        assert_eq!(a.title, "A page");
        assert_eq!(a.area.as_deref(), Some("page-layout"));
        assert_eq!(m.images, vec!["images/p.png"]);
    }

    #[test]
    fn front_matter_closes_at_an_exact_dashes_line() {
        let m = Manual::from_files([
            ("contents.txt", "a\nb\nc\n"),
            ("a.md", "---\n---\n# A\n"),
            ("b.md", "---\ntitle: B\n----\nkeys: x\n---\nbody\n"),
            ("c.md", "---\ntitle: C\n---x\n---\n"),
        ]);
        let a = m.page("a").unwrap();
        assert_eq!(a.title, "a");
        assert!(matches!(a.blocks[0], Block::Heading { .. }));
        let b = m.page("b").unwrap();
        assert_eq!(b.title, "B");
        assert_eq!(b.keys, vec!["x"]);
        assert_eq!(b.blocks.len(), 1);
        assert_eq!(m.page("c").unwrap().blocks, vec![]);
    }

    #[test]
    fn a_title_keeps_its_hash_and_keys_drop_a_comment() {
        let m = files(&[(
            "a.md",
            "---\ntitle: How to correct a #N/A error\nkeys: na, error   # see Excel\n---\n",
        )]);
        let a = m.page("a").unwrap();
        assert_eq!(a.title, "How to correct a #N/A error");
        assert_eq!(a.keys, vec!["na", "error"]);
    }

    #[test]
    fn front_matter_fences_may_have_trailing_spaces() {
        let m = files(&[
            ("a.md", "---  \ntitle: A\n--- \nbody\n"),
            ("b.md", "--- \ntitle: B\n"),
        ]);
        let a = m.page("a").unwrap();
        assert_eq!(a.title, "A");
        assert_eq!(
            a.blocks,
            vec![Block::Paragraph(vec![md::Inline::Text("body".into())])]
        );
        assert!(
            m.problems
                .contains(&Problem::UnclosedFrontMatter("b".into()))
        );
        assert!(
            !m.problems
                .contains(&Problem::UnclosedFrontMatter("a".into()))
        );
    }

    #[test]
    fn an_unclosed_front_matter_is_reported() {
        let m = files(&[("a.md", "---\ntitle: A\n# A\n")]);
        assert!(
            m.problems
                .contains(&Problem::UnclosedFrontMatter("a".into()))
        );
        assert_eq!(m.page("a").unwrap().title, "a");
    }

    #[test]
    fn a_missing_image_in_a_list_item_is_reported() {
        let m = files(&[("a.md", "1. Step\n\n   ![s](images/gone.png)\n")]);
        assert!(m.problems.contains(&Problem::MissingImage {
            page: "a".into(),
            src: "images/gone.png".into()
        }));
    }

    /// Link `link` to the directory `target`: a symbolic link on Unix, a
    /// junction on Windows (no privilege needed). False if it cannot be made.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    #[test]
    fn a_linked_directory_is_not_followed_and_only_the_root_can_fail() {
        let base = std::env::temp_dir().join(format!("helpdocs-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let bundle = base.join("word");
        let outside = base.join("outside");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(bundle.join("contents.txt"), "a\n").unwrap();
        std::fs::write(bundle.join("a.md"), "# A\n").unwrap();
        std::fs::write(outside.join("x.md"), "# X\n").unwrap();
        let linked = link_dir(&outside, &bundle.join("linked"));
        let m = Manual::load(&bundle).expect("the root lists");
        // What a host reads for the image bytes skips the link too.
        let mut listed: Vec<String> = super::files(&bundle)
            .expect("the root lists")
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        listed.sort();
        assert_eq!(listed, ["a.md", "contents.txt"], "linked: {linked}");
        // Remove the link before its target, so nothing is deleted through it.
        let _ = std::fs::remove_dir(bundle.join("linked"));
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(m.problems, vec![], "linked: {linked}");
        assert_eq!(m.pages.len(), 1);
        // A root that cannot be listed fails the load.
        assert!(Manual::load(&base.join("missing")).is_err());
    }

    /// `files` gives bundle paths with `/` on every platform.
    #[test]
    fn files_are_listed_by_their_bundle_path() {
        let base = std::env::temp_dir().join(format!("helpdocs-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("images")).unwrap();
        std::fs::write(base.join("contents.txt"), "a\n").unwrap();
        std::fs::write(base.join("images").join("x.png"), b"png").unwrap();
        let mut got = super::files(&base).unwrap();
        got.sort();
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            got,
            [
                ("contents.txt".to_string(), b"a\n".to_vec()),
                ("images/x.png".to_string(), b"png".to_vec()),
            ]
        );
    }

    #[test]
    fn a_page_in_a_subdirectory_is_reported() {
        let m = files(&[("sub/x.md", "# X")]);
        assert_eq!(m.problems, vec![Problem::NestedPage("sub/x.md".into())]);
        assert!(m.page("x").is_none());
    }

    #[test]
    fn a_byte_order_mark_is_dropped() {
        let m = Manual::from_files([
            ("REVISION", "\u{feff}abc123\n"),
            ("contents.txt", "\u{feff}# strict: home\nx\n"),
            ("x.md", ""),
        ]);
        assert_eq!(m.revision.as_deref(), Some("abc123"));
        assert_eq!(m.strict, vec!["home"]);
        assert_eq!(m.problems, vec![]);
    }

    #[test]
    fn a_blank_revision_is_none() {
        for rev in ["", "\n", "  \nabc\n", "\u{feff}\n"] {
            let m = Manual::from_files([("REVISION", rev), ("x.md", "")]);
            assert_eq!(m.revision, None, "{rev:?}");
            assert_eq!(m.short_revision(), None, "{rev:?}");
        }
        let m = Manual::from_files([("x.md", "")]);
        assert_eq!(m.revision, None);
        assert_eq!(m.short_revision(), None);
    }

    #[test]
    fn a_commit_revision_is_cut_to_seven() {
        let short = |rev: &str| {
            let m = Manual::from_files([("REVISION", rev), ("x.md", "")]);
            m.short_revision().map(str::to_string)
        };
        let sha = "0123abcdef0123abcdef0123abcdef0123abcdef";
        assert_eq!(short(sha).as_deref(), Some("0123abc"));
        assert_eq!(short("0123ABC").as_deref(), Some("0123ABC"));
        assert_eq!(short("0123abcd").as_deref(), Some("0123abc"));
        // Not all hex: kept whole (the fixtures' revision, a tag).
        let fixture = "0000000000000000000000000000000000fixture";
        assert_eq!(short(fixture).as_deref(), Some(fixture));
        assert_eq!(short("v2026.11").as_deref(), Some("v2026.11"));
    }
}
