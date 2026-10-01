//! An XLL that registers `XLLTWICE(x)` = `2*x` when Excel loads it.
//!
//! Excel calls `xlAutoOpen`; that registers the function through the C API
//! callback Excel exports from its own executable (`MdCallBack12`, which is
//! how the SDK's xlcall.cpp reaches `Excel12`). Only what one registration
//! needs is declared here (xlcall.h names).

use std::ffi::c_void;

/// `XLOPER12` on 64-bit Windows: a 24-byte value union, then `xltype`.
#[repr(C)]
struct Xloper12 {
    val: [u64; 3],
    xltype: u32,
}

const XLTYPE_NUM: u32 = 0x0001;
const XLTYPE_STR: u32 = 0x0002;
const XL_SPECIAL: i32 = 0x4000;
const XL_FREE: i32 = XL_SPECIAL;
const XL_GET_NAME: i32 = 9 | XL_SPECIAL;
const XLF_REGISTER: i32 = 149;
const XL_RET_SUCCESS: i32 = 0;

type Callback = unsafe extern "system" fn(i32, i32, *const *mut Xloper12, *mut Xloper12) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

impl Xloper12 {
    fn empty() -> Xloper12 {
        Xloper12 {
            val: [0; 3],
            xltype: 0,
        }
    }

    fn num(v: f64) -> Xloper12 {
        Xloper12 {
            val: [v.to_bits(), 0, 0],
            xltype: XLTYPE_NUM,
        }
    }

    /// A string XLOPER12 over `buf`, a length-prefixed wide string.
    fn str(buf: &mut [u16]) -> Xloper12 {
        Xloper12 {
            val: [buf.as_mut_ptr() as u64, 0, 0],
            xltype: XLTYPE_STR,
        }
    }
}

/// `s` as a length-prefixed wide string.
fn pascal(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.insert(0, v.len() as u16);
    v
}

/// Excel's `Excel12v`, or `None` outside Excel.
fn excel12() -> Option<Callback> {
    unsafe {
        let p = GetProcAddress(
            GetModuleHandleW(std::ptr::null()),
            c"MdCallBack12".as_ptr().cast(),
        );
        (!p.is_null()).then(|| std::mem::transmute::<*mut c_void, Callback>(p))
    }
}

/// Registers `XLLTWICE`. Returns 1 (loaded) either way, as the SDK's
/// samples do.
#[unsafe(no_mangle)]
pub extern "system" fn xlAutoOpen() -> i32 {
    let Some(excel) = excel12() else { return 1 };
    unsafe {
        let mut dll = Xloper12::empty();
        if excel(XL_GET_NAME, 0, std::ptr::null(), &mut dll) != XL_RET_SUCCESS {
            return 1;
        }
        let mut texts = [
            pascal("XLLTWICE"),
            pascal("BB"),
            pascal("XLLTWICE"),
            pascal("x"),
        ];
        let [proc_, ty, name, args] = &mut texts;
        let mut opers = [
            Xloper12::str(proc_),
            Xloper12::str(ty),
            Xloper12::str(name),
            Xloper12::str(args),
            Xloper12::num(1.0),
        ];
        let mut argv: Vec<*mut Xloper12> = vec![&mut dll];
        argv.extend(opers.iter_mut().map(|o| o as *mut Xloper12));
        let mut res = Xloper12::empty();
        excel(XLF_REGISTER, argv.len() as i32, argv.as_ptr(), &mut res);
        let mut free = [&mut dll as *mut Xloper12];
        excel(XL_FREE, 1, free.as_mut_ptr(), std::ptr::null_mut());
    }
    1
}

/// The worksheet function: `XLLTWICE(x)` = `2*x`.
#[unsafe(no_mangle)]
pub extern "system" fn XLLTWICE(x: f64) -> f64 {
    2.0 * x
}
