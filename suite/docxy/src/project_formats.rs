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
//! ([`Source`]), as in yppxy. Save As proposes, first in its list, the type
//! the plan was opened from when something writes it, else the type a plugin
//! declared the default, else `.yppx` ([`Formats::default_save_extension`]).

use ctlcore::json::Json;
use projcore::Project;
use projcore::formats::plugin::{Host, Loaded, PluginInfo, UiPlugin, load_plugins_for};
use projcore::formats::{NATIVE_EXTENSIONS, ProjectFormat, Registry, extension};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The environment variable naming one more plugin directory (yppxy's too).
pub(crate) const PLUGIN_DIR_ENV: &str = "YPPX_PLUGIN_DIR";

/// How a plugin's version target names this host (#1532).
pub(crate) const HOST_NAME: &str = "suite";

/// The process's project formats, built once.
pub(crate) struct Formats {
    pub registry: Registry,
    /// Why each plugin that did not load was skipped (`<file>: <why>`).
    pub errors: Vec<String>,
    /// The libraries loaded, for `app-info`.
    pub plugins: Vec<PluginInfo>,
    /// The skipped libraries, as path and reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// The UI plugins (#1532), in load order.
    pub ui: Vec<Arc<UiPlugin>>,
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
        let loaded = load_plugins_for(
            &dirs,
            Some(Host {
                name: HOST_NAME,
                version: env!("CARGO_PKG_VERSION"),
            }),
        );
        Formats::from_loaded(loaded)
    })
}

/// `app-info`'s `plugins` (#1532): the libraries loaded (`file`, `formats`,
/// `ui`, `built_against`), the UI plugins among them with the command ids
/// their manifests name (`ui: [{file, commands}]`), and the ones skipped
/// (`file`, `reason`).
pub(crate) fn plugins_json(formats: &Formats) -> Json {
    let file = |p: &Path| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    Json::obj(vec![
        (
            "loaded",
            Json::Arr(
                formats
                    .plugins
                    .iter()
                    .map(|p| {
                        Json::obj(vec![
                            ("file", Json::Str(p.file.clone())),
                            (
                                "formats",
                                Json::Arr(p.formats.iter().cloned().map(Json::Str).collect()),
                            ),
                            ("ui", Json::Bool(p.ui)),
                            (
                                "built_against",
                                p.target
                                    .as_ref()
                                    .map_or(Json::Null, |t| Json::Str(t.built_against.clone())),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "ui",
            Json::Arr(
                formats
                    .ui
                    .iter()
                    .map(|p| {
                        Json::obj(vec![
                            ("file", Json::Str(p.name().to_string())),
                            (
                                "commands",
                                Json::Arr(
                                    p.manifest()
                                        .command_ids()
                                        .into_iter()
                                        .map(|id| Json::Str(id.to_string()))
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "skipped",
            Json::Arr(
                formats
                    .skipped
                    .iter()
                    .map(|(path, reason)| {
                        Json::obj(vec![
                            ("file", Json::Str(file(path))),
                            ("reason", Json::Str(reason.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
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
    /// The host's formats, then what the loader found.
    pub fn from_loaded(loaded: Loaded) -> Formats {
        Formats {
            plugins: loaded.plugins,
            skipped: loaded.skipped,
            ui: loaded.ui,
            ..Formats::new(loaded.formats, loaded.errors)
        }
    }

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
            plugins: Vec::new(),
            skipped: Vec::new(),
            ui: Vec::new(),
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

    /// The type Save As proposes for a plan bound to `path` (`None` for a
    /// new plan): its own when something writes it, else the type a format
    /// plugin declared the default, else `.yppx`
    /// (`Registry::default_save_extension`).
    pub fn default_save_extension(&self, path: Option<&Path>) -> &str {
        self.registry.default_save_extension(path.map(extension))
    }

    /// The Save As dialog's types for a plan bound to `path` (`None` for a
    /// new plan), (label, extension): the one it proposes first, as the
    /// type the dialog opens on, then the others in registry order.
    pub fn save_types(&self, path: Option<&Path>) -> Vec<(&str, &str)> {
        self.registry
            .save_types_default_first(path.map(extension))
            .into_iter()
            .map(|(ext, label)| (label, ext))
            .collect()
    }

    /// The file name Save As suggests for a plan bound to `path`, or for a
    /// new one titled `title`: the stem, with the extension of the type it
    /// proposes ([`Formats::default_save_extension`]).
    pub fn suggested_name(&self, path: Option<&Path>, title: &str) -> String {
        let stem = path
            .unwrap_or_else(|| Path::new(title))
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        format!("{stem}.{}", self.default_save_extension(path))
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
        /// Declared the default save type, as `"default": true` does.
        pub default: bool,
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
        fn is_default(&self) -> bool {
            self.default
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
            default: false,
        })
    }

    /// A plugin-like format for `.fakeproj` that declares itself the default
    /// save type, as the fixture plugin does.
    pub(crate) fn default_writer() -> Box<dyn ProjectFormat> {
        Box::new(Passthrough {
            ext: &["fakeproj"],
            label: "Fake Project",
            reads: true,
            default: true,
        })
    }

    #[test]
    fn without_plugins_the_lists_are_the_built_ins() {
        let formats = global();
        assert!(formats.errors.is_empty());
        assert_eq!(formats.registry.open_extensions(), ["yppx", "xml", "mpp"]);
        assert_eq!(
            formats.save_types(None),
            [("Project schedule", "yppx"), ("MSPDI", "xml")]
        );
        // An import nothing writes proposes .yppx; an MSPDI plan its own type.
        assert_eq!(
            formats.save_types(Some(Path::new("a.mpp"))),
            formats.save_types(None)
        );
        assert_eq!(
            formats.save_types(Some(Path::new("a.xml"))),
            [("MSPDI", "xml"), ("Project schedule", "yppx")]
        );
        assert_eq!(formats.default_save_extension(None), "yppx");
        for (path, title, name) in [
            (None, "Project2", "Project2.yppx"),
            (None, "Untitled.yppx", "Untitled.yppx"),
            (Some("dir/plan.mpp"), "plan.mpp", "plan.yppx"),
            (Some("dir/plan.XML"), "plan.XML", "plan.xml"),
            (Some("dir/plan.yppx"), "plan.yppx", "plan.yppx"),
        ] {
            assert_eq!(
                formats.suggested_name(path.map(Path::new), title),
                name,
                "{path:?}"
            );
        }
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
            formats.save_types(None),
            [
                ("Project schedule", "yppx"),
                ("MSPDI", "xml"),
                ("Project", "mpp")
            ]
        );
        // An opened .mpp proposes its own type now that something writes it.
        assert_eq!(
            formats.save_types(Some(Path::new("a.MPP"))),
            [
                ("Project", "mpp"),
                ("Project schedule", "yppx"),
                ("MSPDI", "xml")
            ]
        );
        assert_eq!(
            formats.suggested_name(Some(Path::new("dir/a.mpp")), "a.mpp"),
            "a.mpp"
        );
        assert_eq!(formats.suggested_name(None, "Project1"), "Project1.yppx");
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
            default: false,
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
    fn a_default_plugin_type_is_proposed_first_for_new_plans_and_imports() {
        let _plugins = Override::new(vec![default_writer()]);
        let formats = global();
        let first = |path: Option<&str>| formats.save_types(path.map(Path::new))[0];
        // A new plan and an .mpp import (nothing writes .mpp) propose it.
        assert_eq!(formats.default_save_extension(None), "fakeproj");
        assert_eq!(first(None), ("Fake Project", "fakeproj"));
        assert_eq!(first(Some("a.mpp")), ("Fake Project", "fakeproj"));
        assert_eq!(first(Some("a.fakeproj")), ("Fake Project", "fakeproj"));
        // A writable type the plan came from stays its own.
        assert_eq!(first(Some("a.xml")), ("MSPDI", "xml"));
        assert_eq!(first(Some("a.yppx")), ("Project schedule", "yppx"));
        assert_eq!(
            formats.save_types(None),
            [
                ("Fake Project", "fakeproj"),
                ("Project schedule", "yppx"),
                ("MSPDI", "xml")
            ]
        );
        // The prose list keeps registry order.
        assert_eq!(formats.save_list(), ".yppx, .xml or .fakeproj");
        for (path, title, name) in [
            (None, "Project1", "Project1.fakeproj"),
            (Some("dir/plan.mpp"), "plan.mpp", "plan.fakeproj"),
            (Some("dir/plan.fakeproj"), "plan.fakeproj", "plan.fakeproj"),
            (Some("dir/plan.xml"), "plan.xml", "plan.xml"),
        ] {
            assert_eq!(
                formats.suggested_name(path.map(Path::new), title),
                name,
                "{path:?}"
            );
        }
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
