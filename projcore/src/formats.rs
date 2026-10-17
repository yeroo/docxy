//! The file formats a host opens and saves project schedules in.
//!
//! A [`ProjectFormat`] reads and/or writes one or more file extensions. A
//! [`Registry`] starts with the two built-in formats, the native `.yppx`
//! package ([`Yppx`]) and MSPDI XML `.xml` ([`Mspdi`]); a host registers
//! more: `mppread`'s read-only `.mpp` import, and the format plugins
//! `plugin::load_plugins` finds (behind the `plugins` feature, so the
//! default build stays dependency-free). The registry is what the Open
//! filter, the Save As list and the type it proposes
//! ([`Registry::save_types_default_first`]) and the save-path check
//! ([`Registry::save_target`]) consult.

use crate::model::Project;
use crate::mspdi::{read_mspdi, write_mspdi};
use crate::yppx::{read_yppx, write_yppx};
use std::path::{Path, PathBuf};

#[cfg(feature = "plugins")]
pub mod plugin;

/// The extensions of the built-in formats, which need no plugin: the native
/// package and MSPDI XML.
pub const NATIVE_EXTENSIONS: [&str; 2] = ["yppx", "xml"];

/// The extension a save path without one is given, and the type Save As
/// proposes for a new plan when no format plugin declares itself the
/// default ([`Registry::default_save_extension`]).
pub const DEFAULT_SAVE_EXTENSION: &str = "yppx";

/// A file format project schedules are read from and/or written to.
///
/// Extensions are lower-case and without the dot. A format that does not
/// read (or write) returns an error from [`ProjectFormat::read`] (or
/// [`ProjectFormat::write`]); the [`Registry`] never routes a call to it.
pub trait ProjectFormat: Send + Sync {
    /// The extensions this format is for, lower-case, without the dot.
    fn extensions(&self) -> &[&str];
    /// The type's name in Open and Save As lists.
    fn label(&self) -> &str;
    /// Whether [`ProjectFormat::read`] reads files of this type.
    fn reads(&self) -> bool;
    /// Whether [`ProjectFormat::write`] writes files of this type.
    fn writes(&self) -> bool;
    /// The project held in the bytes of a file of this type.
    fn read(&self, bytes: &[u8]) -> Result<Project, String>;
    /// The bytes of a file of this type holding `project`. `source` is the
    /// file of this type the plan was opened from, when there is one, so a
    /// writer can carry over what the model does not hold; the built-in
    /// formats ignore it.
    fn write(&self, project: &Project, source: Option<&[u8]>) -> Result<Vec<u8>, String>;
    /// Whether this type asks to be the default save type, what Save As
    /// proposes for a new plan ([`Registry::default_save_extension`]); it
    /// counts only while this format writes the type. A format plugin
    /// declares it with `"default": true`; the built-ins never do.
    fn is_default(&self) -> bool {
        false
    }
}

/// The native `.yppx` package ([`crate::yppx`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Yppx;

impl ProjectFormat for Yppx {
    fn extensions(&self) -> &[&str] {
        &["yppx"]
    }
    fn label(&self) -> &str {
        "Project schedule"
    }
    fn reads(&self) -> bool {
        true
    }
    fn writes(&self) -> bool {
        true
    }
    fn read(&self, bytes: &[u8]) -> Result<Project, String> {
        read_yppx(bytes)
    }
    fn write(&self, project: &Project, _source: Option<&[u8]>) -> Result<Vec<u8>, String> {
        write_yppx(project)
    }
}

/// MS Project's MSPDI XML ([`crate::mspdi`]), UTF-8.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mspdi;

impl ProjectFormat for Mspdi {
    fn extensions(&self) -> &[&str] {
        &["xml"]
    }
    fn label(&self) -> &str {
        "MSPDI"
    }
    fn reads(&self) -> bool {
        true
    }
    fn writes(&self) -> bool {
        true
    }
    fn read(&self, bytes: &[u8]) -> Result<Project, String> {
        let xml = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
        read_mspdi(xml)
    }
    fn write(&self, project: &Project, _source: Option<&[u8]>) -> Result<Vec<u8>, String> {
        Ok(write_mspdi(project).into_bytes())
    }
}

/// One extension the registry knows: the formats that read and write it.
struct Slot {
    ext: String,
    reader: Option<usize>,
    writer: Option<usize>,
}

/// The formats a host opens and saves, by extension.
///
/// Extensions keep the order they were first registered in: the built-ins
/// first ([`Registry::new`]), then whatever the host adds. A later
/// registration for an extension replaces the earlier one in place: a
/// format that reads it becomes its reader and one that writes it its
/// writer, so a plugin can take over `.mpp` from the built-in import, and a
/// plugin that only writes `.mpp` keeps that import as the reader.
pub struct Registry {
    formats: Vec<Box<dyn ProjectFormat>>,
    slots: Vec<Slot>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry::new()
    }
}

impl Registry {
    /// The built-in formats: `.yppx` ([`Yppx`]) and `.xml` ([`Mspdi`]).
    pub fn new() -> Registry {
        let mut registry = Registry {
            formats: Vec::new(),
            slots: Vec::new(),
        };
        registry.register(Box::new(Yppx));
        registry.register(Box::new(Mspdi));
        registry
    }

    /// Add `format` for each of its extensions, replacing the earlier reader
    /// (when it reads) and writer (when it writes) of each. A format that
    /// neither reads nor writes changes nothing.
    pub fn register(&mut self, format: Box<dyn ProjectFormat>) {
        let (reads, writes) = (format.reads(), format.writes());
        if !reads && !writes {
            return;
        }
        let index = self.formats.len();
        for ext in format.extensions() {
            let ext = normalize(ext);
            if ext.is_empty() {
                continue;
            }
            let slot = match self.slots.iter().position(|s| s.ext == ext) {
                Some(i) => &mut self.slots[i],
                None => {
                    self.slots.push(Slot {
                        ext,
                        reader: None,
                        writer: None,
                    });
                    self.slots.last_mut().expect("just pushed")
                }
            };
            if reads {
                slot.reader = Some(index);
            }
            if writes {
                slot.writer = Some(index);
            }
        }
        self.formats.push(format);
    }

    fn slot(&self, ext: &str) -> Option<&Slot> {
        let ext = normalize(ext);
        self.slots.iter().find(|s| s.ext == ext)
    }

    /// The format that reads `ext` (any case, with or without the dot).
    pub fn reader(&self, ext: &str) -> Option<&dyn ProjectFormat> {
        let index = self.slot(ext)?.reader?;
        Some(self.formats[index].as_ref())
    }

    /// The format that writes `ext` (any case, with or without the dot).
    pub fn writer(&self, ext: &str) -> Option<&dyn ProjectFormat> {
        let index = self.slot(ext)?.writer?;
        Some(self.formats[index].as_ref())
    }

    /// The format registered for `ext`: its reader, else its writer.
    pub fn for_extension(&self, ext: &str) -> Option<&dyn ProjectFormat> {
        self.reader(ext).or_else(|| self.writer(ext))
    }

    /// Every extension some format reads, built-ins first.
    pub fn open_extensions(&self) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|s| s.reader.is_some())
            .map(|s| s.ext.as_str())
            .collect()
    }

    /// Every extension some format writes, built-ins first.
    pub fn save_extensions(&self) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|s| s.writer.is_some())
            .map(|s| s.ext.as_str())
            .collect()
    }

    /// The Save As list: each writable extension with its writer's label, in
    /// [`Registry::save_extensions`] order.
    pub fn save_types(&self) -> Vec<(&str, &str)> {
        self.slots
            .iter()
            .filter_map(|s| Some((s.ext.as_str(), self.formats[s.writer?].label())))
            .collect()
    }

    /// The type Save As proposes for a plan opened from a file of extension
    /// `opened` (any case, with or without the dot; `None` for a new plan),
    /// lower-case and without the dot: `opened` itself when some format
    /// writes it, so a plan is saved as what it was opened from; else the
    /// first extension the last registered format that
    /// [`ProjectFormat::is_default`] still writes (a format plugin that
    /// declared itself the default); else [`DEFAULT_SAVE_EXTENSION`].
    pub fn default_save_extension(&self, opened: Option<&str>) -> &str {
        if let Some(slot) = opened
            .and_then(|ext| self.slot(ext))
            .filter(|s| s.writer.is_some())
        {
            return &slot.ext;
        }
        (0..self.formats.len())
            .rev()
            .filter(|&i| self.formats[i].is_default())
            .find_map(|i| self.slots.iter().find(|s| s.writer == Some(i)))
            .map_or(DEFAULT_SAVE_EXTENSION, |s| s.ext.as_str())
    }

    /// [`Registry::save_types`] in the order a Save As dialog lists them for
    /// a plan opened from a file of extension `opened` (`None` for a new
    /// plan): [`Registry::default_save_extension`] first, as the type the
    /// dialog opens on, then the others in registry order.
    pub fn save_types_default_first(&self, opened: Option<&str>) -> Vec<(&str, &str)> {
        let default = self.default_save_extension(opened);
        let mut types = self.save_types();
        if let Some(i) = types.iter().position(|(ext, _)| *ext == default) {
            types[..=i].rotate_right(1);
        }
        types
    }

    /// Whether something beyond the built-ins writes: the hosts' Save As
    /// lists stay as they are without it.
    pub fn has_extra_writers(&self) -> bool {
        self.save_extensions()
            .iter()
            .any(|ext| !NATIVE_EXTENSIONS.contains(ext))
    }

    /// Resolve a project save destination: a name without an extension
    /// becomes `.yppx`; one whose extension some format writes is kept; any
    /// other is refused with a message naming the writable extensions
    /// ("Project schedules can only be saved as .yppx or .xml (MSPDI)" with
    /// the built-ins alone).
    pub fn save_target(&self, path: &Path) -> Result<PathBuf, String> {
        match path.extension() {
            None => Ok(path.with_extension(DEFAULT_SAVE_EXTENSION)),
            Some(ext) if ext.to_str().is_some_and(|e| self.writer(e).is_some()) => {
                Ok(path.to_path_buf())
            }
            Some(_) => Err(self.save_refusal()),
        }
    }

    fn save_refusal(&self) -> String {
        let names: Vec<String> = self
            .save_extensions()
            .iter()
            .map(|ext| match *ext {
                "xml" => ".xml (MSPDI)".to_string(),
                ext => format!(".{ext}"),
            })
            .collect();
        let list = match names.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => "no format".to_string(),
        };
        format!("Project schedules can only be saved as {list}")
    }

    /// Read the file at `path` from its `bytes` with the reader of its
    /// extension. A file whose extension no format claims is read as MSPDI
    /// XML, as the hosts always have; one only a writer claims is refused.
    pub fn read(&self, path: &Path, bytes: &[u8]) -> Result<Project, String> {
        let ext = extension(path);
        match self.reader(ext) {
            Some(format) => format.read(bytes),
            None => match self.writer(ext) {
                Some(format) => Err(format!(
                    "{} files (.{}) can be saved but not opened",
                    format.label(),
                    normalize(ext)
                )),
                None => Mspdi.read(bytes),
            },
        }
    }

    /// The bytes of `project` as a file at `path`, which
    /// [`Registry::save_target`] has resolved, written by its extension's
    /// writer with `source` (see [`ProjectFormat::write`]).
    pub fn write(
        &self,
        project: &Project,
        path: &Path,
        source: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        self.writer(extension(path))
            .ok_or_else(|| self.save_refusal())?
            .write(project, source)
    }
}

/// `path`'s extension as text, empty when it has none or it is not UTF-8.
pub fn extension(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

/// An extension as the registry keys it: lower-case, without the dot.
fn normalize(ext: &str) -> String {
    ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A format that reads and/or writes `ext`, its bytes the project's name.
    struct Named {
        exts: &'static [&'static str],
        label: &'static str,
        reads: bool,
        writes: bool,
        default: bool,
    }

    impl ProjectFormat for Named {
        fn extensions(&self) -> &[&str] {
            self.exts
        }
        fn label(&self) -> &str {
            self.label
        }
        fn reads(&self) -> bool {
            self.reads
        }
        fn writes(&self) -> bool {
            self.writes
        }
        fn read(&self, bytes: &[u8]) -> Result<Project, String> {
            Ok(Project {
                name: format!("{}:{}", self.label, String::from_utf8_lossy(bytes)),
                ..Project::default()
            })
        }
        fn write(&self, project: &Project, source: Option<&[u8]>) -> Result<Vec<u8>, String> {
            let source = source.map_or("none".into(), String::from_utf8_lossy);
            Ok(format!("{}:{}:{source}", self.label, project.name).into_bytes())
        }
        fn is_default(&self) -> bool {
            self.default
        }
    }

    fn named(
        exts: &'static [&'static str],
        label: &'static str,
        reads: bool,
        writes: bool,
    ) -> Box<Named> {
        Box::new(Named {
            exts,
            label,
            reads,
            writes,
            default: false,
        })
    }

    /// [`named`], declaring itself the default save type.
    fn defaulting(
        exts: &'static [&'static str],
        label: &'static str,
        reads: bool,
        writes: bool,
    ) -> Box<Named> {
        Box::new(Named {
            default: true,
            ..*named(exts, label, reads, writes)
        })
    }

    const REFUSAL: &str = "Project schedules can only be saved as .yppx or .xml (MSPDI)";

    #[test]
    fn built_ins_read_and_write_yppx_and_xml() {
        let registry = Registry::new();
        assert_eq!(registry.open_extensions(), ["yppx", "xml"]);
        assert_eq!(registry.save_extensions(), ["yppx", "xml"]);
        assert_eq!(
            registry.save_types(),
            [("yppx", "Project schedule"), ("xml", "MSPDI")]
        );
        assert!(!registry.has_extra_writers());
        let project = Project {
            name: "Plan".into(),
            ..Project::default()
        };
        for name in ["a.yppx", "a.XML", "a.Yppx"] {
            let path = Path::new(name);
            let bytes = registry.write(&project, path, None).unwrap();
            assert_eq!(registry.read(path, &bytes).unwrap(), project, "{name}");
        }
        assert!(
            registry
                .write(&project, Path::new("a.yppx"), None)
                .unwrap()
                .starts_with(b"PK")
        );
        assert_eq!(registry.for_extension(".XML").unwrap().label(), "MSPDI");
        assert!(registry.for_extension("mpp").is_none());
    }

    #[test]
    fn unclaimed_extensions_read_as_mspdi() {
        let registry = Registry::new();
        let xml = write_mspdi(&Project::default());
        for name in ["plan.txt", "plan", "plan.mpp"] {
            assert!(
                registry.read(Path::new(name), xml.as_bytes()).is_ok(),
                "{name}"
            );
        }
        assert_eq!(
            registry.read(Path::new("plan.txt"), &[0xff]).unwrap_err(),
            "not UTF-8"
        );
    }

    #[test]
    fn save_target_keeps_the_built_in_refusal() {
        let registry = Registry::new();
        assert_eq!(
            registry.save_target(Path::new("dir/plan")).unwrap(),
            PathBuf::from("dir/plan.yppx")
        );
        for name in ["plan.yppx", "plan.XML"] {
            assert_eq!(
                registry.save_target(Path::new(name)).unwrap(),
                PathBuf::from(name)
            );
        }
        for name in ["plan.mpp", "plan.txt", "plan."] {
            assert_eq!(registry.save_target(Path::new(name)).unwrap_err(), REFUSAL);
        }
        assert_eq!(
            registry
                .write(&Project::default(), Path::new("plan.mpp"), None)
                .unwrap_err(),
            REFUSAL
        );
    }

    #[test]
    fn a_later_registration_replaces_reader_and_writer_in_place() {
        let mut registry = Registry::new();
        registry.register(named(&["mpp"], "Import", true, false));
        registry.register(named(&["fakeproj", ".FP2"], "Fake", true, true));
        assert_eq!(
            registry.open_extensions(),
            ["yppx", "xml", "mpp", "fakeproj", "fp2"]
        );
        assert_eq!(
            registry.save_extensions(),
            ["yppx", "xml", "fakeproj", "fp2"]
        );
        assert!(registry.has_extra_writers());
        assert_eq!(
            registry.save_target(Path::new("plan.mpp")).unwrap_err(),
            "Project schedules can only be saved as .yppx, .xml (MSPDI), .fakeproj or .fp2"
        );
        let mpp = Path::new("x.MPP");
        assert_eq!(registry.read(mpp, b"1").unwrap().name, "Import:1");

        // A writer only for .mpp keeps the import as its reader.
        registry.register(named(&["mpp"], "Writer", false, true));
        assert_eq!(registry.read(mpp, b"1").unwrap().name, "Import:1");
        assert_eq!(registry.save_target(mpp).unwrap(), mpp);
        let project = Project {
            name: "P".into(),
            ..Project::default()
        };
        assert_eq!(
            registry.write(&project, mpp, Some(b"src")).unwrap(),
            b"Writer:P:src"
        );

        // A plugin that reads and writes .mpp takes both, keeping the slot.
        registry.register(named(&["mpp"], "Plugin", true, true));
        assert_eq!(registry.read(mpp, b"2").unwrap().name, "Plugin:2");
        assert_eq!(
            registry.write(&project, mpp, None).unwrap(),
            b"Plugin:P:none"
        );
        assert_eq!(registry.open_extensions()[2], "mpp");
        assert_eq!(
            registry.save_extensions(),
            ["yppx", "xml", "mpp", "fakeproj", "fp2"]
        );
        assert_eq!(
            registry.save_types()[2..],
            [("mpp", "Plugin"), ("fakeproj", "Fake"), ("fp2", "Fake")]
        );
        assert_eq!(registry.for_extension("mpp").unwrap().label(), "Plugin");
    }

    #[test]
    fn built_ins_propose_yppx_or_the_type_the_plan_was_opened_from() {
        let mut registry = Registry::new();
        assert!(!Yppx.is_default() && !Mspdi.is_default());
        assert_eq!(registry.default_save_extension(None), "yppx");
        for opened in ["xml", ".XML", "Xml"] {
            assert_eq!(registry.default_save_extension(Some(opened)), "xml");
        }
        assert_eq!(registry.default_save_extension(Some("yppx")), "yppx");
        // Nothing writes these: the plan is saved as .yppx.
        for opened in ["mpp", "", ".", "txt"] {
            assert_eq!(registry.default_save_extension(Some(opened)), "yppx");
        }
        let built_in = [("yppx", "Project schedule"), ("xml", "MSPDI")];
        assert_eq!(registry.save_types_default_first(None), built_in);
        assert_eq!(registry.save_types_default_first(Some("yppx")), built_in);
        assert_eq!(
            registry.save_types_default_first(Some("xml")),
            [("xml", "MSPDI"), ("yppx", "Project schedule")]
        );
        // A read-only import opened is saved as .yppx too.
        registry.register(named(&["mpp"], "Import", true, false));
        assert_eq!(registry.default_save_extension(Some("MPP")), "yppx");
        assert_eq!(registry.save_types_default_first(Some("mpp")), built_in);
        assert_eq!(registry.save_types(), built_in);
    }

    #[test]
    fn a_writer_declaring_itself_the_default_is_proposed_for_new_plans() {
        let mut registry = Registry::new();
        registry.register(named(&["mpp"], "Import", true, false));
        // A writer that is not the default changes only the list.
        registry.register(named(&["mpx"], "Exchange", true, true));
        assert_eq!(registry.default_save_extension(None), "yppx");
        assert_eq!(registry.default_save_extension(Some("mpx")), "mpx");

        registry.register(defaulting(&["fakeproj", "fp2"], "Fake", true, true));
        // A new plan and a plan from a type nothing writes: the default.
        assert_eq!(registry.default_save_extension(None), "fakeproj");
        assert_eq!(registry.default_save_extension(Some("mpp")), "fakeproj");
        assert_eq!(registry.default_save_extension(Some("txt")), "fakeproj");
        // A plan from a writable type keeps it, built-in or not.
        assert_eq!(registry.default_save_extension(Some("xml")), "xml");
        assert_eq!(registry.default_save_extension(Some("FP2")), "fp2");
        assert_eq!(registry.default_save_extension(Some("mpx")), "mpx");
        // The list keeps registry order; the dialog's puts the default first.
        assert_eq!(
            registry.save_extensions(),
            ["yppx", "xml", "mpx", "fakeproj", "fp2"]
        );
        let exts = |opened| {
            registry
                .save_types_default_first(opened)
                .into_iter()
                .map(|(ext, _)| ext)
                .collect::<Vec<_>>()
        };
        assert_eq!(exts(None), ["fakeproj", "yppx", "xml", "mpx", "fp2"]);
        assert_eq!(exts(Some("mpp")), ["fakeproj", "yppx", "xml", "mpx", "fp2"]);
        assert_eq!(exts(Some("xml")), ["xml", "yppx", "mpx", "fakeproj", "fp2"]);
        assert_eq!(exts(Some("fp2")), ["fp2", "yppx", "xml", "mpx", "fakeproj"]);
        assert_eq!(
            registry.save_types_default_first(None)[0],
            ("fakeproj", "Fake")
        );
        // Save on an extensionless name is still .yppx.
        assert_eq!(
            registry.save_target(Path::new("plan")).unwrap(),
            PathBuf::from("plan.yppx")
        );
    }

    #[test]
    fn the_last_default_that_still_writes_wins() {
        let mut registry = Registry::new();
        registry.register(defaulting(&["aa"], "A", true, true));
        registry.register(defaulting(&["bb"], "B", false, true));
        assert_eq!(registry.default_save_extension(None), "bb");
        // A default that does not write is no save type.
        registry.register(defaulting(&["ro"], "Read only", true, false));
        assert_eq!(registry.default_save_extension(None), "bb");
        // A later writer of .bb that is not the default takes the slot, so
        // the default goes back to the last one still writing.
        registry.register(named(&["bb"], "Other B", false, true));
        assert_eq!(registry.default_save_extension(None), "aa");
        assert_eq!(registry.default_save_extension(Some("bb")), "bb");
        // A default that replaces the .yppx writer keeps its place first.
        registry.register(defaulting(&["yppx"], "Own yppx", true, true));
        assert_eq!(registry.default_save_extension(None), "yppx");
        assert_eq!(
            registry.save_types_default_first(None)[..2],
            [("yppx", "Own yppx"), ("xml", "MSPDI")]
        );
    }

    #[test]
    fn write_only_and_inert_formats() {
        let mut registry = Registry::new();
        registry.register(named(&["out"], "Out", false, true));
        registry.register(named(&["none"], "None", false, false));
        assert!(registry.for_extension("none").is_none());
        assert!(!registry.open_extensions().contains(&"out"));
        assert_eq!(
            registry.read(Path::new("a.out"), b"").unwrap_err(),
            "Out files (.out) can be saved but not opened"
        );
        assert_eq!(registry.for_extension("OUT").unwrap().label(), "Out");
    }
}
