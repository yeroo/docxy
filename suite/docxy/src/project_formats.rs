//! The file types a Project tab opens and saves (`projcore::formats`): the
//! built-in `.yppx` and `.xml` (MSPDI), `mppread`'s read-only `.mpp` import,
//! then the format plugins (`yppx_*` shared libraries) found once, at start,
//! in `<config>/docxy/plugins` (`<config>` is the suite's config root,
//! `DOCXY_CONFIG_DIR` when set) and then `$YPPX_PLUGIN_DIR`, in that order
//! as yppxy loads its own, so a plugin in the second takes over an extension
//! one in the first claims too. With no plugin the Open and Save As lists,
//! the status lines and the save refusals are the built-ins' alone.
//!
//! A type nothing writes is an import: its tab says so and Save goes to Save
//! As. A type a plugin writes saves back in place, and the file the plan was
//! opened from goes to that plugin as the source of each save of its type
//! ([`Source`]), as in yppxy.

use projcore::Project;
use projcore::formats::plugin::load_plugins_with_errors;
use projcore::formats::{NATIVE_EXTENSIONS, ProjectFormat, Registry, extension};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The environment variable naming one more plugin directory (yppxy's too).
pub(crate) const PLUGIN_DIR_ENV: &str = "YPPX_PLUGIN_DIR";

/// The process's project formats, built once.
pub(crate) struct Formats {
    pub registry: Registry,
    /// Why each plugin that did not load was skipped (`<file>: <why>`).
    pub errors: Vec<String>,
    /// Whether mppread's import still reads `.mpp` (no plugin took it), so
    /// an MPP12 file's status can say what that import keeps.
    builtin_mpp: bool,
}

/// The file a plan was opened from, kept while a format plugin writes its
/// type: a save under that extension passes the bytes back, so the plugin
/// can carry over what the model does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    ext: String,
    bytes: Vec<u8>,
}

impl Source {
    /// The bytes to pass a save to `target`: only one of the same type.
    pub fn for_target(&self, target: &Path) -> Option<&[u8]> {
        extension(target)
            .eq_ignore_ascii_case(&self.ext)
            .then_some(self.bytes.as_slice())
    }
}

/// The built-ins and the `.mpp` import, before any plugin.
fn host_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(Box::new(mppread::project::MppImport));
    registry
}

/// The formats of this process: the host's, then the plugins'. `main` asks
/// first, before any tab loads, and reports [`Formats::errors`]. Unit tests
/// load no plugins, whatever is installed (see `tests::Override`).
pub(crate) fn global() -> &'static Formats {
    #[cfg(test)]
    if let Some(formats) = tests::OVERRIDE.with(std::cell::Cell::get) {
        return formats;
    }
    static GLOBAL: OnceLock<Formats> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        let dirs = if cfg!(test) {
            Vec::new()
        } else {
            plugin_dirs(&crate::config_root(), std::env::var_os(PLUGIN_DIR_ENV))
        };
        let (plugins, errors) = load_plugins_with_errors(&dirs);
        Formats::new(plugins, errors)
    })
}

/// Where plugins load from: `<root>/docxy/plugins`, beside the session and
/// the other files the suite keeps, then `$YPPX_PLUGIN_DIR` when it is set
/// and not empty.
pub(crate) fn plugin_dirs(root: &Path, env: Option<OsString>) -> Vec<PathBuf> {
    std::iter::once(root.join("docxy").join("plugins"))
        .chain(env.filter(|dir| !dir.is_empty()).map(PathBuf::from))
        .collect()
}

/// Whether `ext` is one of the built-in types.
fn is_native(ext: &str) -> bool {
    NATIVE_EXTENSIONS
        .iter()
        .any(|native| native.eq_ignore_ascii_case(ext))
}

/// `exts` as prose: `.yppx or .xml`, `.yppx, .xml or .fakeproj`.
pub(crate) fn ext_list(exts: &[&str]) -> String {
    let names: Vec<String> = exts.iter().map(|ext| format!(".{ext}")).collect();
    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

impl Formats {
    /// The host's formats, then `plugins` in load order.
    pub fn new(plugins: Vec<Box<dyn ProjectFormat>>, errors: Vec<String>) -> Formats {
        let mut registry = host_registry();
        let mut builtin_mpp = true;
        for plugin in plugins {
            if plugin.reads()
                && plugin
                    .extensions()
                    .iter()
                    .any(|ext| ext.trim_start_matches('.').eq_ignore_ascii_case("mpp"))
            {
                builtin_mpp = false;
            }
            registry.register(plugin);
        }
        Formats {
            registry,
            errors,
            builtin_mpp,
        }
    }

    /// Whether `path` opens as a Project tab: some format reads its type.
    pub fn is_project_path(&self, path: &Path) -> bool {
        self.registry.reader(extension(path)).is_some()
    }

    /// Whether Save never writes `path` back: nothing writes its type, so
    /// its tab is an import and saves through Save As.
    pub fn is_import(&self, path: &Path) -> bool {
        self.registry.writer(extension(path)).is_none()
    }

    /// The note an opened import's status ends with (`imported from .mpp`,
    /// or what mppread keeps of an MPP12 file), or `None` for a type that
    /// saves back in place.
    pub fn import_note(&self, path: &Path) -> Option<String> {
        if !self.is_import(path) {
            return None;
        }
        let ext = extension(path).to_ascii_lowercase();
        let task_only = ext == "mpp"
            && self.builtin_mpp
            && std::fs::read(path).is_ok_and(|bytes| mppread::mpp::is_mpp12(&bytes));
        Some(if task_only {
            "MPP12 task-only import".to_string()
        } else {
            format!("imported from .{ext}")
        })
    }

    /// The plan in the file at `path`, read by its type's reader, with the
    /// file's bytes. A type no format reads is refused, naming the ones
    /// Open takes.
    pub fn read(&self, path: &Path) -> Result<(Project, Vec<u8>), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read error: {e}"))?;
        let ext = extension(path);
        if self.registry.reader(ext).is_none() && self.registry.writer(ext).is_none() {
            return Err(format!(
                "expected {}",
                ext_list(&self.registry.open_extensions())
            ));
        }
        let project = self.registry.read(path, &bytes)?;
        Ok((project, bytes))
    }

    /// Whether a plugin, not a built-in, writes `ext`: then a file of that
    /// type opened is kept as the [`Source`] of its saves.
    fn plugin_writes(&self, ext: &str) -> bool {
        !is_native(ext) && self.registry.writer(ext).is_some()
    }

    /// The [`Source`] to keep for the file at `path` with `bytes`, if any.
    pub fn source(&self, path: &Path, bytes: Vec<u8>) -> Option<Source> {
        let ext = extension(path).to_ascii_lowercase();
        self.plugin_writes(&ext).then_some(Source { ext, bytes })
    }

    /// [`Formats::source`] for the file at `path`, read only when a plugin
    /// writes its type (a plan restored from its sidecar, say).
    pub fn source_at(&self, path: &Path) -> Option<Source> {
        if !self.plugin_writes(extension(path)) {
            return None;
        }
        self.source(path, std::fs::read(path).ok()?)
    }

    /// The Save As dialog's types, in registry order: (label, extension).
    pub fn save_types(&self) -> Vec<(&str, &str)> {
        self.registry
            .save_types()
            .into_iter()
            .map(|(ext, label)| (label, ext))
            .collect()
    }

    /// The writable extensions as prose (`.yppx or .xml` with the built-ins).
    pub fn save_list(&self) -> String {
        ext_list(&self.registry.save_extensions())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use projcore::mspdi::{read_mspdi, write_mspdi};
    use std::cell::Cell;

    thread_local! {
        /// The formats [`global`] answers with on this test's thread.
        pub(super) static OVERRIDE: Cell<Option<&'static Formats>> = const { Cell::new(None) };
    }

    /// Makes [`global`] answer with other formats on this thread until it
    /// is dropped, so a test can drive the suite's own functions with a
    /// plugin-like format.
    pub(crate) struct Override;

    impl Override {
        pub fn new(plugins: Vec<Box<dyn ProjectFormat>>) -> Override {
            let formats: &'static Formats = Box::leak(Box::new(Formats::new(plugins, Vec::new())));
            OVERRIDE.with(|o| o.set(Some(formats)));
            Override
        }
    }

    impl Drop for Override {
        fn drop(&mut self) {
            OVERRIDE.with(|o| o.set(None));
        }
    }

    /// An in-process stand-in for a plugin: MSPDI under `ext`, its saves
    /// noting the source bytes they were given.
    pub(crate) struct Passthrough {
        pub ext: &'static [&'static str],
        pub label: &'static str,
        pub reads: bool,
    }

    impl ProjectFormat for Passthrough {
        fn extensions(&self) -> &[&str] {
            self.ext
        }
        fn label(&self) -> &str {
            self.label
        }
        fn reads(&self) -> bool {
            self.reads
        }
        fn writes(&self) -> bool {
            true
        }
        fn read(&self, bytes: &[u8]) -> Result<Project, String> {
            read_mspdi(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
        }
        fn write(&self, project: &Project, source: Option<&[u8]>) -> Result<Vec<u8>, String> {
            let mut out = write_mspdi(project);
            if let Some(source) = source {
                out.push_str(&format!("<!-- source {} bytes -->", source.len()));
            }
            Ok(out.into_bytes())
        }
    }

    /// A plugin-like format that reads and writes `.mpp`.
    pub(crate) fn mpp_writer() -> Box<dyn ProjectFormat> {
        Box::new(Passthrough {
            ext: &["mpp"],
            label: "Project",
            reads: true,
        })
    }

    #[test]
    fn without_plugins_the_lists_are_the_built_ins() {
        let formats = global();
        assert!(formats.errors.is_empty());
        assert_eq!(formats.registry.open_extensions(), ["yppx", "xml", "mpp"]);
        assert_eq!(
            formats.save_types(),
            [("Project schedule", "yppx"), ("MSPDI", "xml")]
        );
        assert_eq!(formats.save_list(), ".yppx or .xml");
        for name in ["a.yppx", "a.XML", "a.Mpp"] {
            assert!(formats.is_project_path(Path::new(name)), "{name}");
        }
        for name in ["a.docx", "a", "a.fakeproj"] {
            assert!(!formats.is_project_path(Path::new(name)), "{name}");
        }
        assert!(formats.is_import(Path::new("a.MPP")));
        assert!(!formats.is_import(Path::new("a.xml")));
        assert_eq!(
            formats.import_note(Path::new("missing.MPP")).as_deref(),
            Some("imported from .mpp")
        );
        assert_eq!(formats.import_note(Path::new("a.yppx")), None);
        assert_eq!(formats.source(Path::new("a.mpp"), b"x".to_vec()), None);
        assert_eq!(
            formats.read(Path::new("Cargo.toml")).unwrap_err(),
            "expected .yppx, .xml or .mpp"
        );
    }

    #[test]
    fn a_plugin_writer_lists_its_type_and_keeps_the_source() {
        let _plugins = Override::new(vec![mpp_writer()]);
        let formats = global();
        assert_eq!(formats.registry.open_extensions(), ["yppx", "xml", "mpp"]);
        assert_eq!(
            formats.save_types(),
            [
                ("Project schedule", "yppx"),
                ("MSPDI", "xml"),
                ("Project", "mpp")
            ]
        );
        assert_eq!(formats.save_list(), ".yppx, .xml or .mpp");
        assert!(!formats.is_import(Path::new("a.mpp")));
        assert_eq!(formats.import_note(Path::new("a.mpp")), None);
        let source = formats.source(Path::new("a.MPP"), b"abc".to_vec()).unwrap();
        assert_eq!(source.for_target(Path::new("b.mpp")), Some(&b"abc"[..]));
        assert_eq!(source.for_target(Path::new("b.xml")), None);
        assert_eq!(formats.source(Path::new("a.xml"), b"abc".to_vec()), None);
        assert!(!formats.builtin_mpp);
    }

    #[test]
    fn a_plugin_that_only_writes_mpp_keeps_the_import_as_its_reader() {
        let _plugins = Override::new(vec![Box::new(Passthrough {
            ext: &["mpp"],
            label: "Project",
            reads: false,
        })]);
        let formats = global();
        assert!(formats.builtin_mpp);
        assert!(!formats.is_import(Path::new("a.mpp")));
        assert_eq!(
            formats.registry.reader("mpp").unwrap().label(),
            "Microsoft Project"
        );
    }

    #[test]
    fn plugin_dirs_are_the_config_root_then_the_variable() {
        let root = PathBuf::from("cfg");
        let own = root.join("docxy").join("plugins");
        assert_eq!(
            plugin_dirs(&root, Some("extra".into())),
            [own.clone(), PathBuf::from("extra")]
        );
        for env in [Some("".into()), None] {
            assert_eq!(plugin_dirs(&root, env), std::slice::from_ref(&own));
        }
    }

    #[test]
    fn extension_lists_read_as_prose() {
        assert_eq!(ext_list(&[]), "");
        assert_eq!(ext_list(&["yppx"]), ".yppx");
        assert_eq!(ext_list(&["yppx", "xml"]), ".yppx or .xml");
        assert_eq!(
            ext_list(&["yppx", "xml", "fakeproj"]),
            ".yppx, .xml or .fakeproj"
        );
    }
}
