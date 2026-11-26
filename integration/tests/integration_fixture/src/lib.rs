//! An integration plugin for the tests. The first word of `fixture-mode`
//! beside the library chooses what it does; no file is `normal`:
//!
//! - `normal`: interface version 1, every method answered;
//! - `abi0`, `abi-above`: interface version 0, or one above this build's;
//! - `init-refuses`: `dxi_init` refuses the host;
//! - `slow <ms>`: each `dxi_call` sleeps that long first;
//! - `slow-init <ms>`: `dxi_init` sleeps that long first;
//! - `interaction`: `token` answers `interaction_required`;
//! - `panic`: each `dxi_call` panics, caught as INTERNAL_ERROR;
//! - `only-branding`: `dxi_init` offers `branding` alone.
//!
//! Each `dxi_call` appends `<method> <OS thread id>` to `fixture-calls`
//! beside the library, and each `dxi_init` a line to `fixture-inits`.

use std::ffi::{CStr, c_char, c_void};
use std::path::PathBuf;
use std::sync::Mutex;

#[repr(C)]
pub struct DxiBuf {
    ptr: *mut u8,
    len: usize,
}

const OK: i32 = 0;
const UNKNOWN_METHOD: i32 = 1;
const NOT_CONFIGURED: i32 = 3;
const INTERNAL_ERROR: i32 = 4;

static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

fn fail(code: i32, message: &str) -> i32 {
    *LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()) = message.to_string();
    code
}

/// # Safety
/// `out` is a buffer the host passed.
unsafe fn give(out: *mut DxiBuf, text: &str) {
    let bytes: Box<[u8]> = text.as_bytes().into();
    let len = bytes.len();
    // SAFETY: the host passes a writable buffer.
    unsafe {
        (*out).ptr = Box::into_raw(bytes).cast::<u8>();
        (*out).len = len;
    }
}

/// This library's own path.
fn own_path() -> Option<PathBuf> {
    static ANCHOR: u8 = 0;
    own_path_of(std::ptr::addr_of!(ANCHOR).cast())
}

#[cfg(unix)]
fn own_path_of(address: *const c_void) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }
    unsafe extern "C" {
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> i32;
    }
    let mut info = DlInfo {
        fname: std::ptr::null(),
        fbase: std::ptr::null_mut(),
        sname: std::ptr::null(),
        saddr: std::ptr::null_mut(),
    };
    // SAFETY: `info` is writable; `address` is in this library.
    if unsafe { dladdr(address, &mut info) } == 0 || info.fname.is_null() {
        return None;
    }
    // SAFETY: dladdr gives a NUL-terminated path.
    let name = unsafe { CStr::from_ptr(info.fname) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())))
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
    fn GetCurrentThreadId() -> u32;
}

#[cfg(windows)]
fn own_path_of(address: *const c_void) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;
    let mut module = std::ptr::null_mut();
    // SAFETY: `address` is in this library; `module` is writable.
    let found = unsafe {
        GetModuleHandleExW(
            FROM_ADDRESS | UNCHANGED_REFCOUNT,
            address.cast(),
            &mut module,
        )
    };
    if found == 0 {
        return None;
    }
    let mut buf = vec![0u16; 32_768];
    // SAFETY: `buf` holds `buf.len()` characters.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(
        &buf[..len as usize],
    )))
}

#[cfg(not(any(unix, windows)))]
fn own_path_of(_address: *const c_void) -> Option<PathBuf> {
    None
}

/// The calling thread's id as the OS numbers it.
fn os_thread_id() -> u64 {
    #[cfg(windows)]
    {
        // SAFETY: no arguments.
        u64::from(unsafe { GetCurrentThreadId() })
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn pthread_self() -> usize;
        }
        // SAFETY: no arguments.
        unsafe { pthread_self() as u64 }
    }
    #[cfg(not(any(unix, windows)))]
    {
        0
    }
}

fn beside(name: &str) -> Option<PathBuf> {
    Some(own_path()?.parent()?.join(name))
}

/// The mode's words.
fn mode() -> Vec<String> {
    let text = beside("fixture-mode")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    if words.is_empty() {
        vec!["normal".to_string()]
    } else {
        words
    }
}

fn append(log: &std::path::Path, line: &str) {
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Sleep for the mode's second word, in milliseconds.
fn sleep_ms(mode: &[String]) {
    let ms = mode.get(1).and_then(|m| m.parse().ok()).unwrap_or(2_000);
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn is(mode: &[String], word: &str) -> bool {
    mode.first().is_some_and(|m| m == word)
}

#[unsafe(no_mangle)]
pub extern "C" fn dxi_abi() -> u32 {
    let mode = mode();
    if is(&mode, "abi0") {
        0
    } else if is(&mode, "abi-above") {
        2
    } else {
        1
    }
}

/// # Safety
/// The ABI's: `host` is `len` bytes, `out` a buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dxi_init(_host: *const u8, _len: usize, out: *mut DxiBuf) -> i32 {
    let mode = mode();
    if let Some(log) = beside("fixture-inits") {
        append(&log, "init");
    }
    if is(&mode, "slow-init") {
        sleep_ms(&mode);
    }
    if is(&mode, "init-refuses") {
        return fail(NOT_CONFIGURED, "the fixture refuses this host");
    }
    let methods = if is(&mode, "only-branding") {
        r#"["branding"]"#
    } else {
        r#"["branding","sign_in","token","context"]"#
    };
    let answer = format!(r#"{{"plugin":"fixture","version":"0.0.1","methods":{methods}}}"#);
    // SAFETY: the host's buffer.
    unsafe { give(out, &answer) };
    OK
}

/// # Safety
/// The ABI's: `method` is NUL-terminated, `request` is `len` bytes, `out` a
/// buffer.
#[cfg_attr(not(feature = "missing-call"), unsafe(no_mangle))]
pub unsafe extern "C" fn dxi_call(
    method: *const c_char,
    request: *const u8,
    len: usize,
    out: *mut DxiBuf,
) -> i32 {
    // SAFETY: the ABI's.
    let method = unsafe { CStr::from_ptr(method) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: the ABI's.
    let request = unsafe { std::slice::from_raw_parts(request, len) };
    let request = String::from_utf8_lossy(request).into_owned();
    if let Some(log) = beside("fixture-calls") {
        append(&log, &format!("{method} {}", os_thread_id()));
    }
    let mode = mode();
    let answered = std::panic::catch_unwind(|| answer(&mode, &method, &request));
    match answered {
        Ok(Ok(text)) => {
            // SAFETY: the host's buffer.
            unsafe { give(out, &text) };
            OK
        }
        Ok(Err((code, message))) => fail(code, &message),
        Err(_) => fail(INTERNAL_ERROR, "the fixture panicked"),
    }
}

fn answer(mode: &[String], method: &str, request: &str) -> Result<String, (i32, String)> {
    if is(mode, "slow") {
        sleep_ms(mode);
    }
    if is(mode, "panic") {
        panic!("the fixture panics on purpose");
    }
    let app = ["suite", "mail", "chat"]
        .into_iter()
        .find(|app| request.contains(&format!(r#""app":"{app}""#)))
        .unwrap_or("none");
    Ok(match method {
        "branding" => format!(
            r#"{{"product":"Fixture {app}","short":"F","about_line":"From the fixture"}}"#
        ),
        "sign_in" => format!(
            r#"{{"client_id":"fixture-client","tenant":"fixture-tenant","login_hint":"person@example.com","cloud":"global","for":"{app}"}}"#
        ),
        "token" if is(mode, "interaction") => {
            r#"{"status":"interaction_required","why":"the fixture asks for a sign-in"}"#.into()
        }
        "token" => {
            r#"{"status":"ok","access_token":"fixture-token","expires_at_unix":4102444800,"account":"person@example.com"}"#.into()
        }
        "context" => {
            r#"{"organization":{"id":"org-1","name":"Fixture Org"},"desktop":{"id":"desk-1","name":"Desk 1"}}"#.into()
        }
        other => return Err((UNKNOWN_METHOD, format!("no method {other}"))),
    })
}

/// # Safety
/// The ABI's: a buffer this library filled, or an empty one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dxi_free(buf: *mut DxiBuf) {
    // SAFETY: the ABI's.
    let buf = unsafe { &mut *buf };
    if !buf.ptr.is_null() {
        // SAFETY: made by `give` from a boxed slice of this length.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) });
        buf.ptr = std::ptr::null_mut();
        buf.len = 0;
    }
}

/// # Safety
/// The ABI's: `out` is a buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dxi_last_error(out: *mut DxiBuf) -> i32 {
    let message = LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if message.is_empty() {
        return 1;
    }
    // SAFETY: the host's buffer.
    unsafe { give(out, &message) };
    OK
}
