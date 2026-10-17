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
//! Loading a library runs its initialisation code with the host's rights:
//! put only plugins you trust in a plugin directory.

use super::ProjectFormat;
use crate::model::Project;
use crate::mspdi::{read_mspdi, write_mspdi};
use crate::plugin_abi::{self as abi, YppxBuf};
use std::collections::HashSet;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    let mut formats = Vec::new();
    let mut errors = Vec::new();
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
            match load_plugin(&path) {
                Ok(found) => formats.extend(found),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
    }
    (formats, errors)
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
/// for each type it declares.
pub fn load_plugin(path: &Path) -> Result<Vec<Box<dyn ProjectFormat>>, String> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // SAFETY: loading runs the library's initialisers; plugin directories
    // hold libraries the person installed for this purpose (see the module
    // docs).
    let library = unsafe { libloading::Library::new(&path) }.map_err(|e| e.to_string())?;
    // Unloading a library whose code has run is unsound on some platforms
    // (its thread-local destructors, say, may still be registered), so a
    // library stays loaded for good, even one refused below.
    let library: &'static libloading::Library = Box::leak(Box::new(library));
    // SAFETY: each symbol is looked up with the type the ABI declares for it.
    let version = unsafe { (symbol::<abi::AbiFn>(library, abi::SYMBOL_ABI)?)() };
    if version != abi::ABI_VERSION {
        return Err(format!(
            "plugin ABI version {version}, this build speaks {}",
            abi::ABI_VERSION
        ));
    }
    // SAFETY: as above; the pointers stay valid because the library is
    // never unloaded.
    let api = unsafe {
        Api {
            formats: symbol(library, abi::SYMBOL_FORMATS)?,
            read: symbol(library, abi::SYMBOL_READ)?,
            write: symbol(library, abi::SYMBOL_WRITE)?,
            free: symbol(library, abi::SYMBOL_FREE)?,
            last_error: symbol(library, abi::SYMBOL_LAST_ERROR)?,
        }
    };
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let plugin = Arc::new(Plugin {
        name,
        api,
        lock: Mutex::new(()),
    });
    let decls = plugin.formats()?;
    Ok(decls
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
        .collect())
}

/// # Safety
/// `T` must be the type the ABI declares for `name`.
unsafe fn symbol<T: Copy>(library: &libloading::Library, name: &str) -> Result<T, String> {
    // SAFETY: the caller names the symbol's type.
    unsafe { library.get::<T>(name.as_bytes()) }
        .map(|symbol| *symbol)
        .map_err(|_| format!("not a format plugin: it has no {name}"))
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
    fn take(&self, mut buf: YppxBuf) -> Vec<u8> {
        if buf.ptr.is_null() {
            return Vec::new();
        }
        // SAFETY: the ABI makes `ptr` point at `len` bytes the plugin owns
        // until `yppx_plugin_free`.
        let bytes = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec();
        // SAFETY: a buffer the plugin filled, freed once.
        unsafe { (self.api.free)(&mut buf) };
        bytes
    }

    /// The message for a non-zero `code` just returned, the lock still held.
    fn error(&self, code: i32) -> String {
        let mut buf = YppxBuf::empty();
        // SAFETY: an ABI call with a buffer for the plugin to fill.
        let status = unsafe { (self.api.last_error)(&mut buf) };
        let text = self.take(buf);
        let message = String::from_utf8_lossy(&text);
        let message = message.trim();
        if status == abi::OK && !message.is_empty() {
            format!("{}: {message}", self.name)
        } else {
            format!("{}: {} (code {code})", self.name, abi::code_name(code))
        }
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
