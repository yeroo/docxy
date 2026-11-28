//! The C ABI of an integration plugin. `docs/integration-plugins.md` is the
//! contract; `include/docxy_integration.h` has the same declarations for C.

use std::ffi::c_char;

/// The interface version this build speaks.
pub const ABI_VERSION: u32 = 1;
/// The oldest version a host of this build loads.
pub const MIN_ABI_VERSION: u32 = 1;

/// A plugin library's file name starts with this (any case) and ends with
/// the platform's library extension.
pub const FILE_PREFIX: &str = "dxi_";

pub const SYMBOL_ABI: &str = "dxi_abi";
pub const SYMBOL_INIT: &str = "dxi_init";
pub const SYMBOL_CALL: &str = "dxi_call";
pub const SYMBOL_FREE: &str = "dxi_free";
pub const SYMBOL_LAST_ERROR: &str = "dxi_last_error";

pub const METHOD_BRANDING: &str = "branding";
pub const METHOD_SIGN_IN: &str = "sign_in";
pub const METHOD_TOKEN: &str = "token";
pub const METHOD_CONTEXT: &str = "context";

pub const OK: i32 = 0;
pub const UNKNOWN_METHOD: i32 = 1;
pub const MALFORMED_REQUEST: i32 = 2;
pub const NOT_CONFIGURED: i32 = 3;
pub const INTERNAL_ERROR: i32 = 4;

/// The name of a return code, for a message.
pub fn code_name(code: i32) -> &'static str {
    match code {
        OK => "ok",
        UNKNOWN_METHOD => "unknown method",
        MALFORMED_REQUEST => "malformed request",
        NOT_CONFIGURED => "not configured",
        INTERNAL_ERROR => "internal error",
        _ => "unknown error",
    }
}

/// A byte buffer the plugin owns.
#[repr(C)]
#[derive(Debug)]
pub struct DxiBuf {
    pub ptr: *mut u8,
    pub len: usize,
}

impl DxiBuf {
    pub const fn empty() -> DxiBuf {
        DxiBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

/// `uint32_t dxi_abi(void)`
pub type AbiFn = unsafe extern "C" fn() -> u32;
/// `int32_t dxi_init(const uint8_t* host_json, size_t len, DxiBuf* out)`
pub type InitFn = unsafe extern "C" fn(*const u8, usize, *mut DxiBuf) -> i32;
/// `int32_t dxi_call(const char* method, const uint8_t* req_json, size_t len, DxiBuf* out)`
pub type CallFn = unsafe extern "C" fn(*const c_char, *const u8, usize, *mut DxiBuf) -> i32;
/// `void dxi_free(DxiBuf* buf)`
pub type FreeFn = unsafe extern "C" fn(*mut DxiBuf);
/// `int32_t dxi_last_error(DxiBuf* utf8_out)`
pub type LastErrorFn = unsafe extern "C" fn(*mut DxiBuf) -> i32;
