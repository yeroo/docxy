//! Where the Help tab's manuals come from (#1855): one [`helpdocs::Manual`]
//! per editor, with its image bytes.
//!
//! A person's suite reads `<config root>/docxy/manual/<editor>/` (an installed
//! manual), else the manual built into the binary from `docs/manual/<editor>/`.
//! A harness instance reads only its sandbox's config root, where a script's
//! `manual <editor> <dir>` line put one, so no case depends on what
//! `docs/manual` holds. A bundle without pages counts as none: Help and F1
//! then say the documentation is coming, as before.
//!
//! Each editor's bundle is read once per process. Unit tests never read
//! either place: [`tests::Override`] says what [`load`] answers on a thread,
//! nothing by default.

use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// An editor a manual is published for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Editor {
    Word,
    Excel,
    Project,
}

impl Editor {
    pub(crate) const ALL: [Editor; 3] = [Editor::Word, Editor::Excel, Editor::Project];

    /// The bundle's directory name (`docs/manual/<name>/`).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Editor::Word => "word",
            Editor::Excel => "excel",
            Editor::Project => "project",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Editor> {
        Editor::ALL.into_iter().find(|e| e.name() == name)
    }
}

/// A loaded manual and the bytes of every file under its `images/`, by bundle
/// path (`images/ribbon.png`), which `helpdocs::Manual::images` lists only.
#[derive(Debug)]
pub(crate) struct Bundle {
    pub editor: Editor,
    pub manual: helpdocs::Manual,
    pub files: HashMap<String, Arc<[u8]>>,
}

impl Bundle {
    /// A bundle from `(path, bytes)` pairs relative to the editor's
    /// directory; `None` when it has no page.
    pub(crate) fn from_files(editor: Editor, files: Vec<(String, Vec<u8>)>) -> Option<Bundle> {
        let images = files
            .iter()
            .map(|(p, b)| (p.replace('\\', "/"), b))
            .filter(|(p, _)| p.starts_with("images/"))
            .map(|(p, b)| (p, Arc::<[u8]>::from(b.as_slice())))
            .collect();
        let manual = helpdocs::Manual::from_files(files);
        (!manual.pages.is_empty()).then_some(Bundle {
            editor,
            manual,
            files: images,
        })
    }

    /// The bundle in `dir` (an editor's directory), `None` when it is missing
    /// or has no page. Read as `helpdocs` reads one (`bundle::files`): a
    /// link is never followed, so it cannot take the walk out of the bundle
    /// or round in a loop.
    pub(crate) fn read_dir(editor: Editor, dir: &Path) -> Option<Bundle> {
        Bundle::from_files(editor, helpdocs::bundle::files(dir).ok()?)
    }

    /// The image `src` names (a bundle path such as `images/x.png`).
    pub(crate) fn image(&self, src: &str) -> Option<&Arc<[u8]>> {
        self.files.get(src.trim_start_matches("./"))
    }
}

/// The manuals published into this repository, built into the binary.
#[derive(RustEmbed)]
#[folder = "../../docs/manual"]
struct Embedded;

/// The embedded files of `editor`'s manual, paths relative to its directory.
fn embedded_files(editor: Editor) -> Vec<(String, Vec<u8>)> {
    let prefix = format!("{}/", editor.name());
    Embedded::iter()
        .filter_map(|path| {
            let rel = path.strip_prefix(&prefix)?.to_string();
            let file = Embedded::get(&path)?;
            Some((rel, file.data.into_owned()))
        })
        .collect()
}

/// Where the suite whose config root is `root` reads an installed manual.
pub(crate) fn installed_dir(root: &Path, editor: Editor) -> std::path::PathBuf {
    root.join("docxy").join("manual").join(editor.name())
}

/// The decision behind [`load`]: the installed manual under `root`, else
/// (never for a harness instance) the embedded one `embedded` gives.
pub(crate) fn find(
    editor: Editor,
    root: &Path,
    harness: bool,
    embedded: impl FnOnce() -> Vec<(String, Vec<u8>)>,
) -> Option<Bundle> {
    if let Some(b) = Bundle::read_dir(editor, &installed_dir(root, editor)) {
        return Some(b);
    }
    if harness {
        return None;
    }
    Bundle::from_files(editor, embedded())
}

/// `editor`'s manual, read once per process; `None` when none is installed
/// or built in (a harness instance: none installed in its sandbox).
pub(crate) fn load(editor: Editor, harness: bool) -> Option<Arc<Bundle>> {
    #[cfg(test)]
    {
        let _ = harness;
        tests::OVERRIDE.with(|o| o.borrow().get(&editor).cloned())
    }
    #[cfg(not(test))]
    {
        use std::sync::{Mutex, OnceLock};
        type Loaded = Mutex<HashMap<Editor, Option<Arc<Bundle>>>>;
        static LOADED: OnceLock<Loaded> = OnceLock::new();
        let mut loaded = LOADED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        loaded
            .entry(editor)
            .or_insert_with(|| {
                find(editor, &crate::config_root(), harness, || {
                    embedded_files(editor)
                })
                .map(Arc::new)
            })
            .clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        /// What [`load`] answers on this test's thread.
        pub(super) static OVERRIDE: RefCell<HashMap<Editor, Arc<Bundle>>> =
            RefCell::new(HashMap::new());
    }

    /// Makes [`load`] answer `editor` with a bundle on this thread until it
    /// is dropped; without one every editor has no manual.
    pub(crate) struct Override;

    impl Override {
        pub fn new(bundle: Bundle) -> Override {
            OVERRIDE.with(|o| o.borrow_mut().insert(bundle.editor, Arc::new(bundle)));
            Override
        }

        /// The helpdocs word fixture as `editor`'s manual.
        pub fn fixture(editor: Editor) -> Override {
            Override::new(fixture_bundle(editor))
        }
    }

    impl Drop for Override {
        fn drop(&mut self) {
            OVERRIDE.with(|o| o.borrow_mut().clear());
        }
    }

    /// `helpdocs/tests/fixture/<editor>`.
    pub(crate) fn fixture_dir(editor: Editor) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../helpdocs/tests/fixture")
            .join(editor.name())
    }

    pub(crate) fn fixture_bundle(editor: Editor) -> Bundle {
        Bundle::read_dir(editor, &fixture_dir(editor)).expect("the helpdocs fixture loads")
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "docxy-help-manual-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn embedded_one() -> Vec<(String, Vec<u8>)> {
        vec![
            ("contents.txt".into(), b"built-in\n".to_vec()),
            ("built-in.md".into(), b"# Built in\n".to_vec()),
        ]
    }

    fn install(root: &Path, editor: Editor, page: &str) {
        let dir = installed_dir(root, editor);
        std::fs::create_dir_all(dir.join("images")).unwrap();
        std::fs::write(dir.join("contents.txt"), format!("{page}\n")).unwrap();
        std::fs::write(dir.join(format!("{page}.md")), format!("# {page}\n")).unwrap();
        std::fs::write(dir.join("images").join("a.png"), b"png").unwrap();
    }

    #[test]
    fn an_installed_manual_wins_over_the_built_in_one() {
        let root = temp_root("installed");
        install(&root, Editor::Word, "installed");
        for harness in [false, true] {
            let b = find(Editor::Word, &root, harness, embedded_one).unwrap();
            assert_eq!(b.manual.pages[0].id, "installed");
            assert_eq!(b.image("images/a.png").map(|b| &b[..]), Some(&b"png"[..]));
        }
        // Another editor's directory is not this one's.
        let b = find(Editor::Excel, &root, false, embedded_one).unwrap();
        assert_eq!(b.manual.pages[0].id, "built-in");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_harness_instance_never_reads_the_built_in_manual() {
        let root = temp_root("harness");
        assert!(find(Editor::Word, &root, true, embedded_one).is_none());
        let b = find(Editor::Word, &root, false, embedded_one).unwrap();
        assert_eq!(b.manual.pages[0].id, "built-in");
    }

    #[test]
    fn a_bundle_without_pages_is_no_manual() {
        let root = temp_root("empty");
        let dir = installed_dir(&root, Editor::Word);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("REVISION"), b"abc\n").unwrap();
        assert!(find(Editor::Word, &root, false, Vec::new).is_none());
        // A README is no page (page ids are lowercase).
        let readme = || vec![("README.md".to_string(), b"# Manual\n".to_vec())];
        assert!(find(Editor::Word, &root, false, readme).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `docs/manual`'s own README is no editor's file: an editor's files
    /// are those under its directory, relative to it.
    #[test]
    fn the_built_in_manuals_are_the_editor_directories_only() {
        for editor in Editor::ALL {
            let files = embedded_files(editor);
            assert!(
                files.iter().all(
                    |(p, _)| p != "README.md" && !p.starts_with(&format!("{}/", editor.name()))
                ),
                "{editor:?}: {:?}",
                files.iter().map(|f| &f.0).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn the_fixture_keeps_its_image_bytes_by_path() {
        let b = fixture_bundle(Editor::Word);
        assert_eq!(b.manual.images, ["images/ribbon.png"]);
        let png = b.image("images/ribbon.png").unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert!(b.image("images/none.png").is_none());
    }

    /// An installed manual is read as helpdocs reads one: a link, to a
    /// directory outside the bundle or back up to its own root, is not
    /// followed.
    #[cfg(unix)]
    #[test]
    fn an_installed_manual_never_follows_a_link() {
        let root = temp_root("links");
        install(&root, Editor::Word, "installed");
        let dir = installed_dir(&root, Editor::Word);
        let outside = root.join("outside");
        std::fs::create_dir_all(outside.join("images")).unwrap();
        std::fs::write(outside.join("images").join("secret.png"), b"png").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("escape")).unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("images").join("loop")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("images").join("secret.png"),
            dir.join("images").join("link.png"),
        )
        .unwrap();
        let b = find(Editor::Word, &root, true, Vec::new).unwrap();
        let mut images: Vec<&String> = b.files.keys().collect();
        images.sort();
        assert_eq!(images, ["images/a.png"]);
        assert_eq!(b.manual.images, ["images/a.png"]);
        assert!(b.manual.problems.is_empty(), "{:?}", b.manual.problems);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_answers_what_the_override_holds() {
        assert!(load(Editor::Word, false).is_none());
        let _o = Override::fixture(Editor::Word);
        assert!(load(Editor::Word, true).is_some());
        assert!(load(Editor::Excel, true).is_none());
        for e in Editor::ALL {
            assert_eq!(Editor::from_name(e.name()), Some(e));
        }
        assert_eq!(Editor::from_name("mail"), None);
    }
}
