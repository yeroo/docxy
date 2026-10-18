//! The file formats yppxy opens and saves (`projcore::formats`): the
//! built-in `.yppx` and `.xml` (MSPDI), `mppread`'s `.mpp` import, then the
//! format plugins (`yppx_*` shared libraries) found at start in
//! `<config>/yppxy/plugins` and `$YPPX_PLUGIN_DIR`, in that order, so a
//! plugin in the second takes over an extension one in the first claims too.
//! With no plugin the lists, the status lines and the save refusal are what
//! they were before plugins existed. Save As proposes the type a plan was
//! opened from when something writes it, else the type a plugin declared the
//! default, else `.yppx` (`Registry::default_save_extension`).

use crate::backstage::SaveType;
use ctlcore::json::Json;
use projcore::formats::plugin::{Host, Loaded, PluginInfo, UiPlugin, load_plugins_for};
use projcore::formats::{NATIVE_EXTENSIONS, Registry, extension};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// How a plugin's version target names this host.
pub(crate) const HOST_NAME: &str = "yppxy";

/// The environment variable naming one more plugin directory.
pub(crate) const PLUGIN_DIR_ENV: &str = "YPPX_PLUGIN_DIR";

/// The process's formats and the lists the File backstage shows, built
/// once. The lists are `'static` because the backstage takes them so.
pub(crate) struct Formats {
    pub registry: &'static Registry,
    /// The extensions the Open browser lists.
    pub open_exts: &'static [&'static str],
    /// Save As's *Save as type* list in registry order: empty unless a
    /// plugin writes, so the backstage stays as it was without one. The
    /// backstage shows [`Formats::save_types_for`] the plan.
    pub save_types: &'static [SaveType],
    /// The list once per writable type, that type first.
    by_default: &'static [&'static [SaveType]],
    /// Why each plugin that did not load was skipped.
    pub errors: Vec<String>,
    /// The libraries loaded, for `app-info`.
    pub plugins: Vec<PluginInfo>,
    /// The skipped libraries, as path and reason.
    pub skipped: Vec<(PathBuf, String)>,
    /// The UI plugins, in load order: `--command` runs their commands.
    pub ui: Vec<Arc<UiPlugin>>,
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
        let registry = host_registry();
        let dirs = if cfg!(test) {
            Vec::new()
        } else {
            plugin_dirs(
                ctlcore::config_app_dir("yppxy"),
                std::env::var_os(PLUGIN_DIR_ENV),
            )
        };
        let loaded = load_plugins_for(
            &dirs,
            Some(Host {
                name: HOST_NAME,
                version: env!("CARGO_PKG_VERSION"),
            }),
        );
        Formats::from_loaded(registry, loaded)
    })
}

/// `app-info`'s `plugins`: the libraries loaded (`file`, `formats`, `ui`,
/// `built_against`), the UI plugins among them with their command ids
/// (`ui: [{file, commands}]`), and the ones skipped (`file`, `reason`).
pub(crate) fn plugins_json(
    plugins: &[PluginInfo],
    ui: &[Arc<UiPlugin>],
    skipped: &[(PathBuf, String)],
) -> Json {
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
                plugins
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
                ui.iter()
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
                skipped
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
    /// Freeze `registry` for the process (it and its lists live as long),
    /// with no plugin library behind it (the tests' stand-ins).
    #[cfg(test)]
    pub fn new(registry: Registry, errors: Vec<String>) -> &'static Formats {
        Self::build(registry, errors, Vec::new(), Vec::new(), Vec::new())
    }

    /// The host's `registry` with what the loader found registered.
    pub fn from_loaded(mut registry: Registry, loaded: Loaded) -> &'static Formats {
        for plugin in loaded.formats {
            registry.register(plugin);
        }
        Self::build(
            registry,
            loaded.errors,
            loaded.plugins,
            loaded.skipped,
            loaded.ui,
        )
    }

    fn build(
        registry: Registry,
        errors: Vec<String>,
        plugins: Vec<PluginInfo>,
        skipped: Vec<(PathBuf, String)>,
        ui: Vec<Arc<UiPlugin>>,
    ) -> &'static Formats {
        let registry: &'static Registry = Box::leak(Box::new(registry));
        let open_exts = Box::leak(registry.open_extensions().into_boxed_slice());
        let list = |types: Vec<(&'static str, &'static str)>| -> &'static [SaveType] {
            let types: Vec<SaveType> = types
                .into_iter()
                .map(|(ext, label)| SaveType { label, ext })
                .collect();
            Box::leak(types.into_boxed_slice())
        };
        let (save_types, by_default) = if registry.has_extra_writers() {
            let by_default: Vec<&'static [SaveType]> = registry
                .save_extensions()
                .into_iter()
                .map(|ext| list(registry.save_types_default_first(Some(ext))))
                .collect();
            (list(registry.save_types()), by_default)
        } else {
            (list(Vec::new()), Vec::new())
        };
        Box::leak(Box::new(Formats {
            registry,
            open_exts,
            save_types,
            by_default: Box::leak(by_default.into_boxed_slice()),
            errors,
            plugins,
            skipped,
            ui,
        }))
    }

    /// Run the plugin command `id` on `ed`'s plan with the editor's
    /// selection, as a UI plugin would from the ribbon: the first UI plugin
    /// that owns the id applies its ops as one undo step and the status is
    /// returned (a dialog the command asks for is not opened here; the plan
    /// is unchanged then, and the status says so). `Err` when no plugin owns
    /// the id or an op was refused.
    pub fn run_command(
        &self,
        ed: &mut projcore::editor::Editor,
        id: &str,
        values: Option<projcore::json::Json>,
    ) -> Result<String, String> {
        let command = projcore::uiplugin::Command {
            id: id.to_string(),
            plan: projcore::mspdi::write_mspdi(ed.project()),
            selection: projcore::uiplugin::Selection {
                uids: ed.selected_uid().into_iter().collect(),
                cols: vec!["Name".to_string()],
            },
            view: projcore::json::Json::obj(vec![(
                "switches",
                ed.project().formatting.switches_json(),
            )]),
            values,
            button: None,
            edit: None,
        };
        for plugin in &self.ui {
            let Some(result) = plugin.command(&command)? else {
                continue;
            };
            // The ops and the view's formatting (a plugin's switch, a
            // palette) as one step, as the suite applies them.
            ed.batch(|ed| {
                projctl::apply_ops_in(ed, &result.ops)?;
                if let Some(view) = &result.view {
                    let mut failed = None;
                    ed.edit_formatting(|f| {
                        if let Err(e) = f.apply_view(view) {
                            failed = Some(e);
                        }
                    });
                    if let Some(e) = failed {
                        return Err(e);
                    }
                }
                Ok(())
            })?;
            let mut status = result.status.unwrap_or_default();
            if let Some(dialog) = result.dialog {
                if !status.is_empty() {
                    status.push_str("; ");
                }
                status.push_str(&format!("opens the dialog '{dialog}' (not here)"));
            }
            return Ok(status);
        }
        Err(format!(
            "no plugin runs the command '{id}' ({} UI plugin{} loaded)",
            self.ui.len(),
            if self.ui.len() == 1 { "" } else { "s" }
        ))
    }

    /// The type Save As proposes for the plan bound to `path` (`None` for a
    /// new plan): see `Registry::default_save_extension`.
    pub fn default_save_extension(&self, path: Option<&str>) -> &'static str {
        self.registry
            .default_save_extension(path.map(|p| extension(Path::new(p))))
    }

    /// Save As's *Save as type* list for the plan bound to `path` (`None`
    /// for a new plan): [`Formats::save_types`] with the type Save As
    /// proposes first. Empty unless a plugin writes.
    pub fn save_types_for(&self, path: Option<&str>) -> &'static [SaveType] {
        let default = self.default_save_extension(path);
        self.by_default
            .iter()
            .copied()
            .find(|types| types.first().is_some_and(|t| t.ext == default))
            .unwrap_or(self.save_types)
    }

    /// The name Save As proposes for the plan bound to `path` (`None` for a
    /// new plan, named `new_name`): the file's own name when its type is
    /// the one proposed, else its stem (or `new_name`) with that type's
    /// extension.
    pub fn default_save_name(&self, path: Option<&str>, new_name: &str) -> String {
        let ext = self.default_save_extension(path);
        let Some(path) = path.map(Path::new) else {
            return format!("{new_name}.{ext}");
        };
        match path.file_name() {
            Some(name) if extension(path).eq_ignore_ascii_case(ext) => {
                name.to_string_lossy().into_owned()
            }
            _ => {
                let stem = path.file_stem().map(|s| s.to_string_lossy());
                format!("{}.{ext}", stem.as_deref().unwrap_or(new_name))
            }
        }
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
    /// has one: its index in [`Formats::save_types_for`] the plan.
    pub fn save_type_of(&self, path: &str) -> Option<usize> {
        let ext = extension(Path::new(path));
        self.save_types_for(Some(path))
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
        with_plugin(Passthrough {
            ext: &["mpp"],
            label: "Project",
            reads: true,
            default: false,
        })
    }

    /// The host's formats plus `plugin`.
    pub(crate) fn with_plugin(plugin: Passthrough) -> &'static Formats {
        let mut registry = host_registry();
        registry.register(Box::new(plugin));
        Formats::new(registry, Vec::new())
    }

    /// The extensions of a Save as type list, in its order.
    fn exts(types: &[SaveType]) -> Vec<&str> {
        types.iter().map(|t| t.ext).collect()
    }

    #[test]
    fn without_plugins_the_lists_are_the_built_ins() {
        let formats = global();
        assert_eq!(formats.open_exts, ["yppx", "xml", "mpp"]);
        assert!(formats.save_types.is_empty());
        assert!(formats.save_types_for(None).is_empty());
        assert!(formats.save_types_for(Some("plan.xml")).is_empty());
        assert!(formats.errors.is_empty());
        assert_eq!(formats.save_type_of("plan.yppx"), None);
        assert_eq!(formats.default_save_extension(None), "yppx");
        assert_eq!(formats.default_save_extension(Some("a.XML")), "xml");
        assert_eq!(formats.default_save_extension(Some("a.mpp")), "yppx");
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
        // An opened .mpp proposes its own type, first.
        assert_eq!(formats.save_type_of("dir/plan.MPP"), Some(0));
        assert_eq!(
            exts(formats.save_types_for(Some("dir/plan.MPP"))),
            ["mpp", "yppx", "xml"]
        );
        // A writer that is not the default leaves new plans on .yppx.
        assert_eq!(exts(formats.save_types_for(None)), ["yppx", "xml", "mpp"]);
        assert_eq!(formats.save_type_of("plan.xml"), Some(0));
        assert_eq!(
            exts(formats.save_types_for(Some("a.xml"))),
            ["xml", "yppx", "mpp"]
        );
        assert_eq!(formats.opened_status("a.mpp", 3), "Opened a.mpp — 3 tasks");
        assert_eq!(formats.opened_status("a.mpp", 0), "Opened a.mpp — 0 tasks");
        let source = formats.source("a.MPP", b"abc".to_vec()).unwrap();
        assert_eq!(source.for_target(Path::new("b.mpp")), Some(&b"abc"[..]));
        assert_eq!(source.for_target(Path::new("b.xml")), None);
        assert_eq!(formats.source("a.xml", b"abc".to_vec()), None);
    }

    #[test]
    fn a_default_plugin_type_is_proposed_for_new_plans_and_imports() {
        let formats = with_plugin(Passthrough {
            ext: &["fakeproj"],
            label: "Fake Project",
            reads: true,
            default: true,
        });
        assert_eq!(formats.default_save_extension(None), "fakeproj");
        assert_eq!(
            exts(formats.save_types_for(None)),
            ["fakeproj", "yppx", "xml"]
        );
        // The list in registry order is unchanged.
        assert_eq!(exts(formats.save_types), ["yppx", "xml", "fakeproj"]);
        // A plan from a type nothing writes (the .mpp import) takes it too;
        // one from a writable type keeps its own.
        assert_eq!(formats.default_save_extension(Some("a.mpp")), "fakeproj");
        assert_eq!(formats.save_type_of("a.mpp"), None);
        assert_eq!(
            exts(formats.save_types_for(Some("a.xml"))),
            ["xml", "yppx", "fakeproj"]
        );
        assert_eq!(formats.save_type_of("dir/a.yppx"), Some(0));
        let names = [
            (None, "Project1.fakeproj"),
            (Some("dir/plan.mpp"), "plan.fakeproj"),
            (Some("plan.XML"), "plan.XML"),
            (Some("dir/plan.fakeproj"), "plan.fakeproj"),
            (Some("plan"), "plan.fakeproj"),
        ];
        for (path, name) in names {
            assert_eq!(
                formats.default_save_name(path, "Project1"),
                name,
                "{path:?}"
            );
        }
    }

    #[test]
    fn without_a_default_plugin_new_plans_and_imports_propose_yppx() {
        let formats = global();
        let names = [
            (None, "Project1.yppx"),
            (Some("dir/plan.mpp"), "plan.yppx"),
            (Some("plan.xml"), "plan.xml"),
            (Some("old.yppx"), "old.yppx"),
        ];
        for (path, name) in names {
            assert_eq!(
                formats.default_save_name(path, "Project1"),
                name,
                "{path:?}"
            );
        }
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
