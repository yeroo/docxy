//! comshimcore — shared scaffolding for the Office COM shims (xlcomshim,
//! wordcomshim). The generic runtime lives here — the class factory,
//! LocalServer32 / InprocServer32 plumbing, VARIANT helpers, graceful
//! degradation, logging, and the type-library author/register tool
//! ([`typelib`]). Each shim provides only its app-specific object graph and a
//! `fn() -> IDispatch` that mints its root `Application`.
//!
//! Windows-only; an empty crate on other targets so the workspace still builds.

#![cfg(windows)]
#![allow(non_snake_case)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

use windows::Win32::Foundation::{
    BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, DISP_E_BADINDEX,
    DISP_E_MEMBERNOTFOUND, E_POINTER, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoInitializeEx, CoRegisterClassObject,
    CoResumeClassObjects, CoRevokeClassObject, CoUninitialize, DISPATCH_FLAGS, DISPATCH_PROPERTYPUT,
    DISPATCH_PROPERTYPUTREF, DISPPARAMS, EXCEPINFO, IClassFactory, IClassFactory_Impl, IDispatch,
    IDispatch_Impl, ITypeInfo, REGCLS_MULTIPLEUSE, REGCLS_SUSPENDED,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, TranslateMessage,
};
use windows::core::{BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR, Result, VARIANT, implement};

pub mod typelib;

// ---- VARIANT type tags -----------------------------------------------------
pub const VT_EMPTY: u16 = 0;
pub const VT_BSTR: u16 = 8;
pub const VT_ERROR: u16 = 10;
pub const VT_BOOL: u16 = 11;

const DISPID_UNKNOWN: i32 = -1;

// -----------------------------------------------------------------------
// Logging (the field diagnostic) — one file per shim, name set by `init`.
// -----------------------------------------------------------------------

static LOG_NAME: OnceLock<&'static str> = OnceLock::new();

/// Set the shim name (the `%TEMP%\<name>.log` file) and install the panic hook.
/// Call once at startup.
pub fn init(log_name: &'static str) {
    let _ = LOG_NAME.set(log_name);
    install_panic_hook();
}

pub fn log(msg: &str) {
    use std::io::Write;
    let name = LOG_NAME.get().copied().unwrap_or("comshim");
    let Ok(dir) = std::env::var("TEMP").or_else(|_| std::env::var("TMP")) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{dir}\\{name}.log"))
    {
        let _ = writeln!(f, "[{}] {msg}", std::process::id());
    }
}

/// A panic in a vtable method would otherwise unwind across the COM FFI boundary
/// (UB / RPC_E_SERVERFAULT with no diagnostic). Log it instead.
pub fn install_panic_hook() {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|info| log(&format!("PANIC: {info}"))));
    });
}

// -----------------------------------------------------------------------
// VARIANT helpers
// -----------------------------------------------------------------------

/// The VARTYPE tag (masking BYREF/ARRAY flags). A VARIANT begins with its 16-bit
/// `vt` field, so this read is layout-stable.
pub unsafe fn vt_of(v: *const VARIANT) -> u16 {
    unsafe { *(v as *const u16) & 0x0fff }
}

/// Positional argument `i` (0 = first), accounting for `rgvarg` being stored in
/// reverse order. `None` if omitted.
pub unsafe fn arg<'a>(p: *const DISPPARAMS, i: u32) -> Option<&'a VARIANT> {
    unsafe {
        if p.is_null() {
            return None;
        }
        let dp = &*p;
        if i >= dp.cArgs {
            return None;
        }
        Some(&*dp.rgvarg.add((dp.cArgs - 1 - i) as usize))
    }
}

pub unsafe fn arg_string(p: *const DISPPARAMS, i: u32) -> Option<String> {
    unsafe { arg(p, i).and_then(variant_to_string) }
}

pub unsafe fn arg_i32(p: *const DISPPARAMS, i: u32) -> Option<i32> {
    unsafe {
        arg(p, i).and_then(|v| {
            let vt = vt_of(v);
            if vt == VT_EMPTY || vt == VT_ERROR {
                None
            } else {
                i32::try_from(v).ok()
            }
        })
    }
}

/// Argument `i` as a bool, `default` when omitted/uncoercible.
pub unsafe fn arg_bool(p: *const DISPPARAMS, i: u32, default: bool) -> bool {
    unsafe { arg(p, i).and_then(|v| bool::try_from(v).ok()).unwrap_or(default) }
}

pub fn variant_to_string(v: &VARIANT) -> Option<String> {
    let vt = unsafe { vt_of(v) };
    if vt == VT_EMPTY || vt == VT_ERROR {
        return None;
    }
    if let Ok(b) = BSTR::try_from(v) {
        return Some(b.to_string());
    }
    let s = v.to_string();
    (!s.is_empty()).then_some(s)
}

/// Write a result VARIANT (guarding a null out-pointer, as for void methods).
pub unsafe fn put(pvarresult: *mut VARIANT, value: VARIANT) {
    if !pvarresult.is_null() {
        unsafe { std::ptr::write(pvarresult, value) };
    }
}

pub fn is_put(wflags: DISPATCH_FLAGS) -> bool {
    (wflags.0 & (DISPATCH_PROPERTYPUT.0 | DISPATCH_PROPERTYPUTREF.0)) != 0
}

// -----------------------------------------------------------------------
// Graceful degradation — never fault on a member the shim doesn't model.
// -----------------------------------------------------------------------

thread_local! {
    static SYNTH: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}
const SYNTH_BASE: i32 = 0x4000_0000;

/// A stable synthetic dispid for an unmodeled member name (so the get and the
/// follow-up put land on the same id).
pub fn synth_id(name: &str) -> i32 {
    SYNTH.with(|s| {
        let mut v = s.borrow_mut();
        match v.iter().position(|n| n == name) {
            Some(i) => SYNTH_BASE + i as i32,
            None => {
                v.push(name.to_string());
                SYNTH_BASE + (v.len() as i32 - 1)
            }
        }
    })
}
pub fn synth_name(id: i32) -> Option<String> {
    (id >= SYNTH_BASE).then(|| SYNTH.with(|s| s.borrow().get((id - SYNTH_BASE) as usize).cloned()))?
}

/// The default arm for any dispid an object doesn't handle: log the member and
/// degrade benignly — swallow a put, hand back a do-nothing object for a get.
pub unsafe fn unhandled(
    id: i32,
    wflags: DISPATCH_FLAGS,
    params: *const DISPPARAMS,
    result: *mut VARIANT,
) -> Result<()> {
    // DISPID_VALUE (0) with NO arguments reaching this fall-through means the
    // object has no default property (Workbook, Worksheet, Application...). A get
    // of it must fail with DISP_E_MEMBERNOTFOUND, NOT a benign NullObject:
    // well-behaved late-bound clients (pywin32's dynamic layer, some C++/Delphi
    // hosts) probe an object's default value on receipt, and a NullObject answer
    // makes them substitute that null for the real object — silently no-opping
    // the whole call chain. Objects that DO have a default handle id=0 first.
    // NB: pywin32 sends the probe as METHOD|PROPERTYGET (same flags as an indexed
    // `coll(1)` call), so the arg count is the only reliable discriminator — a
    // real default-indexed get like `unknownColl(1)` has cArgs>=1 and must keep
    // degrading gracefully (return the do-nothing object so the chain flows).
    let cargs = if params.is_null() { 0 } else { unsafe { (*params).cArgs } };
    if id == 0 && cargs == 0 && !is_put(wflags) {
        log("  -> default-value probe on an object with no default -> DISP_E_MEMBERNOTFOUND");
        return Err(DISP_E_MEMBERNOTFOUND.into());
    }
    let member = synth_name(id).unwrap_or_else(|| format!("dispid {id}"));
    log(&format!(
        "  -> unmodeled '{member}' (put={}) -> benign",
        is_put(wflags)
    ));
    if !is_put(wflags) {
        unsafe { put(result, VARIANT::from(null_dispatch())) };
    }
    Ok(())
}

/// A do-nothing IDispatch: resolves any name, swallows any put, returns itself
/// for any get — so a client can walk unmodeled property chains without faulting.
#[implement(IDispatch)]
struct NullObject;

pub fn null_dispatch() -> IDispatch {
    NullObject.into()
}

impl IDispatch_Impl for NullObject_Impl {
    fn GetTypeInfoCount(&self) -> Result<u32> {
        Ok(0)
    }
    fn GetTypeInfo(&self, _i: u32, _l: u32) -> Result<ITypeInfo> {
        Err(DISP_E_BADINDEX.into())
    }
    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        rgsznames: *const PCWSTR,
        cnames: u32,
        _lcid: u32,
        rgdispid: *mut i32,
    ) -> Result<()> {
        unsafe {
            if cnames > 0 {
                let names = std::slice::from_raw_parts(rgsznames, cnames as usize);
                let ids = std::slice::from_raw_parts_mut(rgdispid, cnames as usize);
                for id in ids.iter_mut() {
                    *id = DISPID_UNKNOWN;
                }
                ids[0] = synth_id(&names[0].to_string().unwrap_or_default());
            }
        }
        Ok(())
    }
    fn Invoke(
        &self,
        _id: i32,
        _riid: *const GUID,
        _lcid: u32,
        wflags: DISPATCH_FLAGS,
        _params: *const DISPPARAMS,
        result: *mut VARIANT,
        _ei: *mut EXCEPINFO,
        _ae: *mut u32,
    ) -> Result<()> {
        // A NullObject must NEVER fault — it is the graceful-degradation sink, so a
        // host can walk/call/coerce an unmodeled chain (Columns.EntireColumn.AutoFit(),
        // some.Unknown.Thing) without an error aborting its export. Every get/call
        // returns another NullObject so the chain keeps flowing; a put is swallowed.
        // Unlike a REAL modeled object (which routes through `unhandled`, where an
        // absent default property correctly yields DISP_E_MEMBERNOTFOUND), the null
        // sink returns itself even for DISPID_VALUE so coercions don't fault.
        unsafe {
            if !is_put(wflags) {
                put(result, VARIANT::from(null_dispatch()));
            }
        }
        Ok(())
    }
}

/// Shared `GetIDsOfNames`: resolve the first name via `resolver`; an unmodeled
/// name gets a synthetic id (so the follow-up Invoke reaches [`unhandled`]).
pub unsafe fn resolve_names(
    who: &str,
    rgsznames: *const PCWSTR,
    cnames: u32,
    rgdispid: *mut i32,
    resolver: impl Fn(&str) -> Option<i32>,
) -> Result<()> {
    unsafe {
        if cnames == 0 {
            return Ok(());
        }
        let names = std::slice::from_raw_parts(rgsznames, cnames as usize);
        let ids = std::slice::from_raw_parts_mut(rgdispid, cnames as usize);
        for id in ids.iter_mut() {
            *id = DISPID_UNKNOWN;
        }
        let name = names[0].to_string().unwrap_or_default();
        match resolver(&name) {
            Some(d) => ids[0] = d,
            None => {
                log(&format!("{who}: unmodeled member '{name}' -> graceful"));
                ids[0] = synth_id(&name);
            }
        }
        Ok(())
    }
}

/// The two boilerplate IDispatch methods every object shares (no type info).
#[macro_export]
macro_rules! no_typeinfo {
    () => {
        fn GetTypeInfoCount(&self) -> ::windows::core::Result<u32> {
            Ok(0)
        }
        fn GetTypeInfo(
            &self,
            _i: u32,
            _l: u32,
        ) -> ::windows::core::Result<::windows::Win32::System::Com::ITypeInfo> {
            Err(::windows::Win32::Foundation::DISP_E_BADINDEX.into())
        }
    };
}

/// `GetIDsOfNames` for an object using a name->id `resolver`.
#[macro_export]
macro_rules! dispatch_names {
    ($who:literal, $resolver:path) => {
        fn GetIDsOfNames(
            &self,
            _riid: *const ::windows::core::GUID,
            rgsznames: *const ::windows::core::PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> ::windows::core::Result<()> {
            unsafe { $crate::resolve_names($who, rgsznames, cnames, rgdispid, $resolver) }
        }
    };
}

// -----------------------------------------------------------------------
// Server lifetime (STA, single-thread): a live-Application count so the server
// exits its message loop / the DLL unloads once the last one drops.
// -----------------------------------------------------------------------

static APPS: AtomicI32 = AtomicI32::new(0);

/// Call from the root Application's constructor.
pub fn app_created() {
    APPS.fetch_add(1, Ordering::SeqCst);
}
/// Call from the root Application's `Drop`; returns true when it was the last one
/// (the caller then posts a quit message to end the out-of-process message loop).
pub fn app_dropped_is_last() -> bool {
    APPS.fetch_sub(1, Ordering::SeqCst) == 1
}

// -----------------------------------------------------------------------
// Class factory + server / DLL plumbing (generic over the app's root object).
// -----------------------------------------------------------------------

#[implement(IClassFactory)]
struct ShimFactory {
    create: fn() -> IDispatch,
}

impl IClassFactory_Impl for ShimFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        unsafe {
            if punkouter.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            log(&format!("ClassFactory::CreateInstance riid={:?}", *riid));
            let app = (self.create)();
            app.query(riid, ppvobject).ok()
        }
    }
    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// Whether COM launched us as a server (`-Embedding` / `/automation` / `--serve`).
pub fn should_serve() -> bool {
    let joined = std::env::args().collect::<Vec<_>>().join(" ").to_lowercase();
    joined.contains("-embedding") || joined.contains("/automation") || joined.contains("--serve")
}

/// Run the out-of-process (LocalServer32) message loop: register a class object
/// for both CLSIDs, pump messages until the last Application drops, then revoke.
pub fn run_local_server(shim_clsid: GUID, app_clsid: GUID, create: fn() -> IDispatch) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        log("server starting; registering class object");
        let factory: IClassFactory = ShimFactory { create }.into();
        let mut cookies = Vec::new();
        for clsid in [shim_clsid, app_clsid] {
            cookies.push(CoRegisterClassObject(
                &clsid,
                &factory,
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE | REGCLS_SUSPENDED,
            )?);
        }
        CoResumeClassObjects()?;
        log("class objects registered; entering message loop");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log("message loop exited; revoking + uninitializing");
        for c in cookies {
            let _ = CoRevokeClassObject(c);
        }
        CoUninitialize();
    }
    Ok(())
}

/// The in-process (InprocServer32) `DllGetClassObject`: hand back the class object
/// for either CLSID. Each shim's `#[no_mangle]` export forwards here.
pub unsafe fn dll_get_class_object(
    shim_clsid: GUID,
    app_clsid: GUID,
    create: fn() -> IDispatch,
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    unsafe {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        *ppv = std::ptr::null_mut();
        let clsid = *rclsid;
        if clsid != shim_clsid && clsid != app_clsid {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        install_panic_hook();
        log(&format!("DllGetClassObject clsid={clsid:?}"));
        let factory: IClassFactory = ShimFactory { create }.into();
        factory.query(riid, ppv)
    }
}

/// The in-process `DllCanUnloadNow`: stay resident while any Application is alive.
pub fn dll_can_unload_now() -> HRESULT {
    if APPS.load(Ordering::SeqCst) > 0 {
        S_FALSE
    } else {
        S_OK
    }
}
