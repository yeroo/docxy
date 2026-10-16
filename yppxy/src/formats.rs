//! The file formats yppxy opens and saves (`projcore::formats`): the
//! built-in `.yppx` and `.xml` (MSPDI), `mppread`'s `.mpp` import, then the
//! format plugins (`yppx_*` shared libraries) found at start in
//! `<config>/yppxy/plugins` and `$YPPX_PLUGIN_DIR`, in that order, so a
//! plugin in the second takes over an extension one in the first claims too.
//! With no plugin the lists, the status lines and the save refusal are what
//! they were before plugins existed.

use crate::backstage::SaveType;
use projcore::formats::plugin::load_plugins_with_errors;
use projcore::formats::{NATIVE_EXTENSIONS, Registry, extension};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The environment variable naming one more plugin directory.
pub(crate) const PLUGIN_DIR_ENV: &str = "YPPX_PLUGIN_DIR";

/// The process's formats and the lists the File backstage shows, built
/// once. The lists are `'static` because the backstage takes them so.
pub(crate) struct Formats {
    pub registry: &'static Registry,
    /// The extensions the Open browser lists.
    pub open_exts: &'static [&'static str],
    /// Save As's *Save as type* list: empty unless a plugin writes, so the
    /// backstage stays as it was without one.
    pub save_types: &'static [SaveType],
    /// Why each plugin that did not load was skipped.
    pub errors: Vec<String>,
}

/// The file a plan was opened from, kept while a format plugin writes its
/// type: saving under that extension passes the bytes back, so the plugin
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
pub(crate) fn host_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(Box::new(mppread::project::MppImport));
    registry
}

/// The formats of this process: the host's, then the plugins'. Unit tests
/// load no plugins, whatever is installed.
pub(crate) fn global() -> &'static Formats {
    static GLOBAL: OnceLock<&'static Formats> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        let mut registry = host_registry();
        let dirs = if cfg!(test) {
            Vec::new()
        } else {
            plugin_dirs(
                ctlcore::config_app_dir("yppxy"),
                std::env::var_os(PLUGIN_DIR_ENV),
            )
        };
        let (plugins, errors) = load_plugins_with_errors(&dirs);
        for plugin in plugins {
            registry.register(plugin);
        }
        Formats::new(registry, errors)
    })
}

/// Where plugins load from: `<config>/plugins`, then `$YPPX_PLUGIN_DIR` when
/// it is set and not empty.
pub(crate) fn plugin_dirs(config: Option<PathBuf>, env: Option<OsString>) -> Vec<PathBuf> {
    config
        .map(|dir| dir.join("plugins"))
        .into_iter()
        .chain(env.filter(|dir| !dir.is_empty()).map(PathBuf::from))
        .collect()
}

impl Formats {
    /// Freeze `registry` for the process (it and its lists live as long).
    pub fn new(registry: Registry, errors: Vec<String>) -> &'static Formats {
        let registry: &'static Registry = Box::leak(Box::new(registry));
        let open_exts = Box::leak(registry.open_extensions().into_boxed_slice());
        let save_types: Vec<SaveType> = if registry.has_extra_writers() {
            registry
                .save_types()
                .into_iter()
                .map(|(ext, label)| SaveType { label, ext })
                .collect()
        } else {
            Vec::new()
        };
        Box::leak(Box::new(Formats {
            registry,
            open_exts,
            save_types: Box::leak(save_types.into_boxed_slice()),
            errors,
        }))
    }

    /// Whether a plugin, not a built-in, writes `ext`: then a file of that
    /// type opened is kept as the [`Source`] of its saves.
    fn plugin_writes(&self, ext: &str) -> bool {
        !is_native(ext) && self.registry.writer(ext).is_some()
    }

    /// The [`Source`] to keep for the file at `path` with `bytes`, if any.
    pub fn source(&self, path: &str, bytes: Vec<u8>) -> Option<Source> {
        let ext = extension(Path::new(path)).to_ascii_lowercase();
        self.plugin_writes(&ext).then_some(Source { ext, bytes })
    }

    /// The status line after opening `path` with `tasks` tasks: a type only
    /// read is an import ("— N .mpp tasks imported"); one a plugin also
    /// writes saves back in place, so it is just opened ("— N tasks").
    pub fn opened_status(&self, path: &str, tasks: usize) -> String {
        let ext = extension(Path::new(path)).to_ascii_lowercase();
        if is_native(&ext) || self.registry.reader(&ext).is_none() {
            format!("Opened {path}")
        } else if self.registry.writer(&ext).is_some() {
            format!("Opened {path} — {tasks} tasks")
        } else if tasks == 0 && ext == "mpp" {
            format!("Opened {path} — .mpp metadata only (no task table found)")
        } else {
            format!("Opened {path} — {tasks} .{ext} tasks imported")
        }
    }

    /// The Save As type the plan bound to `path` is saved as, if the list
    /// has one.
    pub fn save_type_of(&self, path: &str) -> Option<usize> {
        let ext = extension(Path::new(path));
        self.save_types
            .iter()
            .position(|t| t.ext.eq_ignore_ascii_case(ext))
    }
}

fn is_native(ext: &str) -> bool {
    NATIVE_EXTENSIONS
        .iter()
        .any(|native| native.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use projcore::Project;
    use projcore::formats::ProjectFormat;
    use projcore::mspdi::{read_mspdi, write_mspdi};

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
            let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
            read_mspdi(text)
        }
        fn write(&self, project: &Project, source: Option<&[u8]>) -> Result<Vec<u8>, String> {
            let mut out = write_mspdi(project);
            if let Some(source) = source {
                out.push_str(&format!("<!-- source {} bytes -->", source.len()));
            }
            Ok(out.into_bytes())
        }
    }

    /// The host's formats plus a plugin-like writer (and reader) for `.mpp`.
    pub(crate) fn with_mpp_writer() -> &'static Formats {
        let mut registry = host_registry();
        registry.register(Box::new(Passthrough {
            ext: &["mpp"],
            label: "Project",
            reads: true,
        }));
        Formats::new(registry, Vec::new())
    }

    #[test]
    fn without_plugins_the_lists_are_the_built_ins() {
        let formats = global();
        assert_eq!(formats.open_exts, ["yppx", "xml", "mpp"]);
        assert!(formats.save_types.is_empty());
        assert!(formats.errors.is_empty());
        assert_eq!(formats.save_type_of("plan.yppx"), None);
        assert_eq!(formats.source("plan.mpp", b"x".to_vec()), None);
        assert_eq!(formats.opened_status("a.xml", 3), "Opened a.xml");
        assert_eq!(formats.opened_status("a.YPPX", 3), "Opened a.YPPX");
        assert_eq!(formats.opened_status("a.txt", 3), "Opened a.txt");
        assert_eq!(
            formats.opened_status("a.mpp", 3),
            "Opened a.mpp — 3 .mpp tasks imported"
        );
        assert_eq!(
            formats.opened_status("a.mpp", 0),
            "Opened a.mpp — .mpp metadata only (no task table found)"
        );
    }

    #[test]
    fn a_plugin_writer_lists_its_type_and_keeps_the_source() {
        let formats = with_mpp_writer();
        assert_eq!(formats.open_exts, ["yppx", "xml", "mpp"]);
        let types: Vec<_> = formats
            .save_types
            .iter()
            .map(|t| (t.ext, t.label))
            .collect();
        assert_eq!(
            types,
            [
                ("yppx", "Project schedule"),
                ("xml", "MSPDI"),
                ("mpp", "Project")
            ]
        );
        assert_eq!(formats.save_type_of("dir/plan.MPP"), Some(2));
        assert_eq!(formats.opened_status("a.mpp", 3), "Opened a.mpp — 3 tasks");
        assert_eq!(formats.opened_status("a.mpp", 0), "Opened a.mpp — 0 tasks");
        let source = formats.source("a.MPP", b"abc".to_vec()).unwrap();
        assert_eq!(source.for_target(Path::new("b.mpp")), Some(&b"abc"[..]));
        assert_eq!(source.for_target(Path::new("b.xml")), None);
        assert_eq!(formats.source("a.xml", b"abc".to_vec()), None);
    }

    #[test]
    fn plugin_dirs_are_the_config_dir_then_the_variable() {
        let config = PathBuf::from("cfg").join("yppxy");
        assert_eq!(
            plugin_dirs(Some(config.clone()), Some("extra".into())),
            [config.join("plugins"), PathBuf::from("extra")]
        );
        assert_eq!(
            plugin_dirs(Some(config.clone()), Some("".into())),
            [config.join("plugins")]
        );
        assert_eq!(
            plugin_dirs(None, Some("extra".into())),
            [PathBuf::from("extra")]
        );
        assert!(plugin_dirs(None, None).is_empty());
    }
}
