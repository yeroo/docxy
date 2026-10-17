//! Format plugins: `yppx_*` shared libraries that read and/or write more
//! file types, loaded at runtime ([`crate::plugin_abi`] is their ABI).
//!
//! A host calls [`load_plugins`] (or [`load_plugins_with_errors`]) once at
//! start with its plugin directories and registers what it returns in its
//! [`super::Registry`]. Each type a library declares becomes one
//! [`ProjectFormat`]: its `read` calls `yppx_plugin_read` and parses the MSPDI
//! with [`read_mspdi`], its `write` serialises with [`write_mspdi`] and calls
//! `yppx_plugin_write` with the source bytes. A library is loaded once and
//! kept for the life of the process, and calls into it are serialised.
//!
//! A host that names itself ([`Host`], through [`load_plugins_for`]) also
//! reads each library's version target (`yppx_plugin_target`,
//! [`Target`]) before any other symbol and skips one built for another
//! version, and loads the user-interface symbols of a library that has them
//! as a [`UiPlugin`]: its manifest, and calls for state, commands and the
//! chart overlay ([`crate::uiplugin`]). A library without the target symbol
//! is a format plugin only (its UI symbols are not looked up).
//!
//! Loading a library runs its initialisation code with the host's rights:
//! put only plugins you trust in a plugin directory.

use super::ProjectFormat;
use crate::model::Project;
use crate::mspdi::{read_mspdi, write_mspdi};
use crate::plugin_abi::{self as abi, YppxBuf};
use crate::uiplugin::{
    Command, CommandResult, Manifest, OverlayCtx, Shape, State, StateCtx, Target,
};
use std::collections::HashSet;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The host loading plugins: its name as the version target names it
/// (`suite`, `yppxy`) and its `CARGO_PKG_VERSION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// What [`load_plugins_for`] found.
#[derive(Default)]
pub struct Loaded {
    /// Every format the loaded libraries declare, in load order.
    pub formats: Vec<Box<dyn ProjectFormat>>,
    /// The libraries with user-interface symbols, in load order.
    pub ui: Vec<Arc<UiPlugin>>,
    /// One entry per library loaded.
    pub plugins: Vec<PluginInfo>,
    /// Why each library that was not loaded was skipped (`"<path>: <why>"`).
    pub errors: Vec<String>,
    /// The same, as the library's path and the reason.
    pub skipped: Vec<(PathBuf, String)>,
}

/// A loaded library, for `app-info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    /// Its file name.
    pub file: String,
    pub path: PathBuf,
    /// The extensions it declares.
    pub formats: Vec<String>,
    /// Its version target, when it has one.
    pub target: Option<Target>,
    /// Whether it is a UI plugin.
    pub ui: bool,
}

/// Every format the plugins in `dirs` declare, in directory order (and file
/// name order within one); a library that cannot be loaded is skipped. See
/// [`load_plugins_with_errors`] for why one was.
pub fn load_plugins(dirs: &[PathBuf]) -> Vec<Box<dyn ProjectFormat>> {
    load_plugins_with_errors(dirs).0
}

/// [`load_plugins`], with a message for each library that was skipped
/// (`"<path>: <why>"`). A directory that does not exist is no error; the
/// same library reached through two directories loads once.
pub fn load_plugins_with_errors(dirs: &[PathBuf]) -> (Vec<Box<dyn ProjectFormat>>, Vec<String>) {
    let loaded = load_plugins_for(dirs, None);
    (loaded.formats, loaded.errors)
}

/// [`load_plugins_with_errors`] for a named host: each library's version
/// target is checked for `host` first (a library targeting another version
/// is skipped with the reason among the errors), and a library with the UI
/// symbols is a [`UiPlugin`] too. With no host the targets are not checked
/// and no UI loads.
pub fn load_plugins_for(dirs: &[PathBuf], host: Option<Host>) -> Loaded {
    let mut loaded = Loaded::default();
    let mut seen = HashSet::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && is_plugin_file(path))
            .collect();
        paths.sort();
        for path in paths {
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(key) {
                continue;
            }
            match load_plugin_for(&path, host) {
                Ok(library) => {
                    loaded.formats.extend(library.formats);
                    loaded.ui.extend(library.ui);
                    loaded.plugins.push(library.info);
                }
                Err(e) => {
                    loaded.errors.push(format!("{}: {e}", path.display()));
                    loaded.skipped.push((path, e));
                }
            }
        }
    }
    loaded
}

/// One library [`load_plugin_for`] loaded.
pub struct Library {
    pub formats: Vec<Box<dyn ProjectFormat>>,
    pub ui: Option<Arc<UiPlugin>>,
    pub info: PluginInfo,
}

/// Whether `path` names a plugin library: `yppx_` and the platform's
/// shared-library extension (`.dll`, `.so`, `.dylib`), in any case.
pub fn is_plugin_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name.len() > abi::FILE_PREFIX.len() + std::env::consts::DLL_SUFFIX.len()
        && name.starts_with(abi::FILE_PREFIX)
        && name.ends_with(std::env::consts::DLL_SUFFIX)
}

/// Load the plugin library at `path` (whatever its name) and return a format
/// for each type it declares. No version check, no UI: see
/// [`load_plugin_for`].
pub fn load_plugin(path: &Path) -> Result<Vec<Box<dyn ProjectFormat>>, String> {
    load_plugin_for(path, None).map(|library| library.formats)
}

/// Load the plugin library at `path`: its version target first (checked
/// for `host`, when one is named), then its formats, then its UI symbols
/// (looked up only when it has a target and a host is named).
pub fn load_plugin_for(path: &Path, host: Option<Host>) -> Result<Library, String> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // SAFETY: loading runs the library's initialisers; plugin directories
    // hold libraries the person installed for this purpose (see the module
    // docs).
    let library = unsafe { libloading::Library::new(&path) }.map_err(|e| e.to_string())?;
    // Unloading a library whose code has run is unsound on some platforms
    // (its thread-local destructors, say, may still be registered), so a
    // library stays loaded for good, even one refused below.
    let library: &'static libloading::Library = Box::leak(Box::new(library));
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    // The version target, before any other symbol: a library built for
    // another version of this host is not called at all. `free` and
    // `last_error` are needed to read it, so they are looked up first; a
    // library without them is no plugin either way.
    // SAFETY: each symbol is looked up with the type the ABI declares for it;
    // the pointers stay valid because the library is never unloaded.
    let free: abi::FreeFn = unsafe { symbol(library, abi::SYMBOL_FREE)? };
    // SAFETY: as above.
    let last_error: abi::LastErrorFn = unsafe { symbol(library, abi::SYMBOL_LAST_ERROR)? };
    // SAFETY: as above.
    let target_fn: Option<abi::TargetFn> = unsafe { optional_symbol(library, abi::SYMBOL_TARGET) };
    let target = match target_fn {
        Some(f) => {
            let text = call_buf(&name, free, last_error, |out| {
                // SAFETY: an ABI call with a buffer for the plugin to fill.
                unsafe { f(out) }
            })?;
            let target = Target::parse(&text)?;
            if let Some(host) = host {
                target.check(host.name, host.version)?;
            }
            Some(target)
        }
        None => None,
    };
    // SAFETY: as above.
    let version = unsafe { (symbol::<abi::AbiFn>(library, abi::SYMBOL_ABI)?)() };
    if version != abi::ABI_VERSION {
        return Err(format!(
            "plugin ABI version {version}, this build speaks {}",
            abi::ABI_VERSION
        ));
    }
    // SAFETY: as above.
    let api = unsafe {
        Api {
            formats: symbol(library, abi::SYMBOL_FORMATS)?,
            read: symbol(library, abi::SYMBOL_READ)?,
            write: symbol(library, abi::SYMBOL_WRITE)?,
            free,
            last_error,
        }
    };
    let plugin = Arc::new(Plugin {
        name: name.clone(),
        api,
        lock: Mutex::new(()),
    });
    let decls = plugin.formats()?;
    // The UI symbols, only behind a version target (an ABI-1 library is
    // never asked) and for a named host: all five or none.
    let ui = if target.is_some() && host.is_some() {
        UiPlugin::load(library, &plugin)?.map(Arc::new)
    } else {
        None
    };
    let info = PluginInfo {
        file: name,
        path,
        formats: decls.iter().map(|d| d.ext.clone()).collect(),
        target,
        ui: ui.is_some(),
    };
    let formats = decls
        .into_iter()
        .map(|decl| {
            // `parse_formats` lets only letters, digits, `_` and `-` through.
            let ext_c = CString::new(decl.ext.as_str()).expect("extensions hold no NUL");
            Box::new(PluginFormat {
                plugin: Arc::clone(&plugin),
                // One small leak per declared type, for `extensions()`'s
                // borrowed slice; plugins load once per process.
                ext: Box::leak(decl.ext.into_boxed_str()),
                ext_c,
                label: decl.label,
                read: decl.read,
                write: decl.write,
                default: decl.default,
            }) as Box<dyn ProjectFormat>
        })
        .collect();
    Ok(Library { formats, ui, info })
}

/// # Safety
/// `T` must be the type the ABI declares for `name`.
unsafe fn symbol<T: Copy>(library: &libloading::Library, name: &str) -> Result<T, String> {
    // SAFETY: the caller names the symbol's type.
    unsafe { optional_symbol::<T>(library, name) }
        .ok_or_else(|| format!("not a format plugin: it has no {name}"))
}

/// # Safety
/// `T` must be the type the ABI declares for `name`.
unsafe fn optional_symbol<T: Copy>(library: &libloading::Library, name: &str) -> Option<T> {
    // SAFETY: the caller names the symbol's type.
    unsafe { library.get::<T>(name.as_bytes()) }
        .ok()
        .map(|symbol| *symbol)
}

/// Run `call` with an empty buffer, take what the library put in it as UTF-8
/// text, and turn a non-zero code into its message. For the calls made
/// before a [`Plugin`] exists or outside its format calls.
fn call_buf(
    name: &str,
    free: abi::FreeFn,
    last_error: abi::LastErrorFn,
    call: impl FnOnce(*mut YppxBuf) -> i32,
) -> Result<String, String> {
    let mut buf = YppxBuf::empty();
    let code = call(&mut buf);
    let bytes = take_buf(free, buf);
    if code != abi::OK {
        return Err(error_message(name, free, last_error, code));
    }
    String::from_utf8(bytes).map_err(|_| format!("{name}: the reply is not UTF-8"))
}

/// Copy a buffer the library filled and give it back.
fn take_buf(free: abi::FreeFn, mut buf: YppxBuf) -> Vec<u8> {
    if buf.ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: the ABI makes `ptr` point at `len` bytes the plugin owns
    // until `yppx_plugin_free`.
    let bytes = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec();
    // SAFETY: a buffer the plugin filled, freed once.
    unsafe { free(&mut buf) };
    bytes
}

/// The message for a non-zero `code` just returned.
fn error_message(name: &str, free: abi::FreeFn, last_error: abi::LastErrorFn, code: i32) -> String {
    let mut buf = YppxBuf::empty();
    // SAFETY: an ABI call with a buffer for the plugin to fill.
    let status = unsafe { last_error(&mut buf) };
    let text = take_buf(free, buf);
    let message = String::from_utf8_lossy(&text);
    let message = message.trim();
    if status == abi::OK && !message.is_empty() {
        format!("{name}: {message}")
    } else {
        format!("{name}: {} (code {code})", abi::code_name(code))
    }
}

/// The library's entry points.
struct Api {
    formats: abi::FormatsFn,
    read: abi::ReadFn,
    write: abi::WriteFn,
    free: abi::FreeFn,
    last_error: abi::LastErrorFn,
}

/// One loaded library, shared by the formats it declares.
struct Plugin {
    /// Its file name, for messages.
    name: String,
    api: Api,
    /// Held across every call (and the `last_error` after it), so the library
    /// is called from one thread at a time.
    lock: Mutex<()>,
}

impl Plugin {
    /// Copy a buffer the library filled and give it back.
    fn take(&self, buf: YppxBuf) -> Vec<u8> {
        take_buf(self.api.free, buf)
    }

    /// The message for a non-zero `code` just returned, the lock still held.
    fn error(&self, code: i32) -> String {
        error_message(&self.name, self.api.free, self.api.last_error, code)
    }

    /// A JSON call: `input` in, the reply's text out, with a non-zero code
    /// as `Err((code, message))`.
    fn call_json(&self, f: abi::UiCallFn, input: &str) -> Result<String, (i32, String)> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = YppxBuf::empty();
        // SAFETY: an ABI call; the input is valid for its length for the
        // duration of the call, and `buf` is for the plugin to fill.
        let code = unsafe { f(input.as_ptr(), input.len(), &mut buf) };
        let bytes = self.take(buf);
        if code != abi::OK {
            return Err((code, self.error(code)));
        }
        String::from_utf8(bytes).map_err(|_| {
            (
                abi::INTERNAL_ERROR,
                format!("{}: the reply is not UTF-8", self.name),
            )
        })
    }

    /// The types the library declares.
    fn formats(&self) -> Result<Vec<abi::FormatDecl>, String> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = YppxBuf::empty();
        // SAFETY: an ABI call with a buffer for the plugin to fill.
        let code = unsafe { (self.api.formats)(&mut buf) };
        let json = self.take(buf);
        if code != abi::OK {
            return Err(self.error(code));
        }
        let json = String::from_utf8(json).map_err(|_| "the formats JSON is not UTF-8")?;
        abi::parse_formats(&json)
    }
}

/// The library's user-interface entry points.
struct UiApi {
    state: abi::UiCallFn,
    command: abi::UiCallFn,
    overlay: abi::UiCallFn,
}

/// A library's user interface: its manifest, read once, and the calls the
/// host makes for control state, commands and the chart overlay.
pub struct UiPlugin {
    plugin: Arc<Plugin>,
    api: UiApi,
    manifest: Manifest,
}

impl UiPlugin {
    /// The UI of `library`, when it has the symbols: all five, with
    /// `yppx_ui_abi` at [`abi::UI_ABI_VERSION`]; none is no UI; some is a
    /// refusal.
    fn load(
        library: &'static libloading::Library,
        plugin: &Arc<Plugin>,
    ) -> Result<Option<UiPlugin>, String> {
        let present: Vec<&str> = abi::UI_SYMBOLS
            .iter()
            .copied()
            // SAFETY: only the symbol's presence is read here.
            .filter(|name| unsafe { optional_symbol::<abi::AbiFn>(library, name) }.is_some())
            .collect();
        if present.is_empty() {
            return Ok(None);
        }
        if present.len() != abi::UI_SYMBOLS.len() {
            let missing: Vec<&str> = abi::UI_SYMBOLS
                .iter()
                .copied()
                .filter(|name| !present.contains(name))
                .collect();
            return Err(format!(
                "a UI plugin exports every yppx_ui_* symbol; this one lacks {}",
                missing.join(", ")
            ));
        }
        // SAFETY: each symbol is looked up with the type the ABI declares.
        let version = unsafe { (symbol::<abi::AbiFn>(library, abi::SYMBOL_UI_ABI)?)() };
        if version != abi::UI_ABI_VERSION {
            return Err(format!(
                "plugin UI ABI version {version}, this build speaks {}",
                abi::UI_ABI_VERSION
            ));
        }
        // SAFETY: as above.
        let (manifest_fn, api) = unsafe {
            (
                symbol::<abi::FormatsFn>(library, abi::SYMBOL_UI_MANIFEST)?,
                UiApi {
                    state: symbol(library, abi::SYMBOL_UI_STATE)?,
                    command: symbol(library, abi::SYMBOL_UI_COMMAND)?,
                    overlay: symbol(library, abi::SYMBOL_UI_OVERLAY)?,
                },
            )
        };
        let manifest = {
            let _guard = plugin.lock.lock().unwrap_or_else(|e| e.into_inner());
            let text = call_buf(
                &plugin.name,
                plugin.api.free,
                plugin.api.last_error,
                // SAFETY: an ABI call with a buffer for the plugin to fill.
                |out| unsafe { manifest_fn(out) },
            )?;
            Manifest::parse(&text).map_err(|e| format!("{}: {e}", plugin.name))?
        };
        Ok(Some(UiPlugin {
            plugin: Arc::clone(plugin),
            api,
            manifest,
        }))
    }

    /// The library's file name.
    pub fn name(&self) -> &str {
        &self.plugin.name
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// `yppx_ui_state`: the controls' state and a dialog's values for `ctx`.
    pub fn state(&self, ctx: &StateCtx) -> Result<State, String> {
        let text = self
            .plugin
            .call_json(self.api.state, &ctx.json())
            .map_err(|(_, e)| e)?;
        State::parse(&text).map_err(|e| format!("{}: {e}", self.plugin.name))
    }

    /// `yppx_ui_command`: run `cmd`. `None` when the command is not this
    /// plugin's ([`abi::UNSUPPORTED_EXTENSION`]).
    pub fn command(&self, cmd: &Command) -> Result<Option<CommandResult>, String> {
        match self.plugin.call_json(self.api.command, &cmd.json()) {
            Ok(text) => CommandResult::parse(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", self.plugin.name)),
            Err((abi::UNSUPPORTED_EXTENSION, _)) => Ok(None),
            Err((_, e)) => Err(e),
        }
    }

    /// `yppx_ui_overlay`: the shapes to draw over the chart for `ctx`.
    pub fn overlay(&self, ctx: &OverlayCtx) -> Result<Vec<Shape>, String> {
        let text = self
            .plugin
            .call_json(self.api.overlay, &ctx.json())
            .map_err(|(_, e)| e)?;
        crate::uiplugin::parse_shapes(&text).map_err(|e| format!("{}: {e}", self.plugin.name))
    }
}

/// One type a plugin declares, as a [`ProjectFormat`].
struct PluginFormat {
    plugin: Arc<Plugin>,
    ext: &'static str,
    ext_c: CString,
    label: String,
    read: bool,
    write: bool,
    /// Declared `"default": true`: the type Save As proposes for a new plan.
    default: bool,
}

impl ProjectFormat for PluginFormat {
    fn extensions(&self) -> &[&str] {
        std::slice::from_ref(&self.ext)
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn reads(&self) -> bool {
        self.read
    }

    fn writes(&self) -> bool {
        self.write
    }

    fn is_default(&self) -> bool {
        self.default
    }

    fn read(&self, bytes: &[u8]) -> Result<Project, String> {
        if !self.read {
            return Err(format!("{}: does not read .{}", self.plugin.name, self.ext));
        }
        let plugin = &self.plugin;
        let xml = {
            let _guard = plugin.lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut out = YppxBuf::empty();
            // SAFETY: an ABI call; every pointer is valid for its length for
            // the duration of the call.
            let code = unsafe {
                (plugin.api.read)(self.ext_c.as_ptr(), bytes.as_ptr(), bytes.len(), &mut out)
            };
            let xml = plugin.take(out);
            if code != abi::OK {
                return Err(plugin.error(code));
            }
            xml
        };
        let xml = String::from_utf8(xml)
            .map_err(|_| format!("{}: the MSPDI it returned is not UTF-8", plugin.name))?;
        read_mspdi(&xml)
    }

    fn write(&self, project: &Project, source: Option<&[u8]>) -> Result<Vec<u8>, String> {
        if !self.write {
            return Err(format!(
                "{}: does not write .{}",
                self.plugin.name, self.ext
            ));
        }
        let plugin = &self.plugin;
        let xml = write_mspdi(project);
        let (source_ptr, source_len) =
            source.map_or((std::ptr::null(), 0), |s| (s.as_ptr(), s.len()));
        let _guard = plugin.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = YppxBuf::empty();
        // SAFETY: an ABI call; every pointer is valid for its length for the
        // duration of the call, and `source_ptr` is NULL without a source.
        let code = unsafe {
            (plugin.api.write)(
                self.ext_c.as_ptr(),
                xml.as_ptr(),
                xml.len(),
                source_ptr,
                source_len,
                &mut out,
            )
        };
        let bytes = plugin.take(out);
        if code != abi::OK {
            return Err(plugin.error(code));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_files_are_yppx_libraries() {
        let suffix = std::env::consts::DLL_SUFFIX;
        for name in ["yppx_example", "YPPX_Example", "yppx_a"] {
            let file = format!("{name}{suffix}");
            assert!(is_plugin_file(Path::new(&file)), "{file}");
            assert!(is_plugin_file(&Path::new("dir").join(&file)), "{file}");
        }
        for name in [
            format!("libyppx_example{suffix}"),
            format!("yppx_{suffix}"),
            "yppx_example.txt".to_string(),
            "yppx_example".to_string(),
            format!("other{suffix}"),
        ] {
            assert!(!is_plugin_file(Path::new(&name)), "{name}");
        }
    }

    #[test]
    fn missing_dirs_and_non_libraries_load_nothing() {
        let dir = std::env::temp_dir().join(format!("projcore-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(format!("yppx_fake{}", std::env::consts::DLL_SUFFIX));
        std::fs::write(&fake, b"not a library").unwrap();
        std::fs::write(dir.join("readme.txt"), b"ignored").unwrap();
        let (formats, errors) = load_plugins_with_errors(&[dir.join("missing"), dir.clone()]);
        assert!(formats.is_empty());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with(&fake.display().to_string()),
            "{errors:?}"
        );
        assert!(load_plugins(&[dir.clone(), dir.clone()]).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
