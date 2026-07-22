//! xlcomshim — a COM **LocalServer32** that impersonates `Excel.Application`, so
//! applications that automate Office over COM can create spreadsheets without
//! Microsoft Excel installed. Document output is (from P1 on) produced by the
//! dependency-free `gridcore` engine.
//!
//! **P0 scope (this file):** prove the COM plumbing end-to-end — the process is
//! launched by COM with `-Embedding`, registers a class factory for our own
//! CLSID, and answers a late-bound `IDispatch` client (`New-Object -ComObject
//! Excel.Application`) for a handful of `Application` members seeded with Excel's
//! *real* DISPIDs. Every activation and dispatch is logged; that log is our
//! Petrel diagnostic (it records exactly which members a real client invokes and
//! whether it binds late or early).
//!
//! Registration is done out-of-band by `tools/comshim/register-shim.ps1`
//! (per-user `HKCU\Software\Classes`, a brand-new shim CLSID, guarded so it can
//! never clobber an installed Excel). Self-registration moves into the binary in
//! P1.

#[cfg(not(windows))]
fn main() {
    eprintln!("xlcomshim is a Windows COM server and only runs on Windows.");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    win::run()
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicI32, Ordering};

    use windows::Win32::Foundation::{
        CLASS_E_NOAGGREGATION, DISP_E_BADINDEX, DISP_E_MEMBERNOTFOUND, DISP_E_UNKNOWNNAME,
    };
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoInitializeEx, CoRegisterClassObject,
        CoResumeClassObjects, CoRevokeClassObject, CoUninitialize, DISPATCH_FLAGS, DISPPARAMS,
        EXCEPINFO, IClassFactory, IClassFactory_Impl, IDispatch, IDispatch_Impl, ITypeInfo,
        REGCLS_MULTIPLEUSE, REGCLS_SUSPENDED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage,
    };
    use windows::core::{BSTR, GUID, IUnknown, Interface, PCWSTR, Result, VARIANT, implement};

    /// Our own coclass CLSID — a brand-new GUID, NEVER Microsoft's Excel CLSID
    /// {00024500-…}. `Excel.Application` (the ProgID) points here in HKCU.
    const SHIM_CLSID: GUID = GUID::from_u128(0x7b3f9e20_4c1a_4e8b_a2d6_9f5c1e0b7a31);

    // ---- Excel's real DISPIDs (verified against the live Excel typelib) -------
    const DISPID_VALUE: i32 = 0; // default member
    const DISPID_UNKNOWN: i32 = -1;
    const ID_NAME: i32 = 110;
    const ID_VERSION: i32 = 392;
    const ID_VISIBLE: i32 = 558;
    const ID_DISPLAYALERTS: i32 = 343;
    const ID_WORKBOOKS: i32 = 572;
    const ID_QUIT: i32 = 302;

    /// Live automation objects in this process; when it hits zero the server
    /// exits its message loop.
    static OBJECTS: AtomicI32 = AtomicI32::new(0);

    fn object_born() {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
    }
    fn object_died() {
        if OBJECTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last object gone — ask the message loop to quit.
            unsafe { PostQuitMessage(0) };
        }
    }

    /// Append a line to `%TEMP%\xlcomshim.log`. This is the Petrel diagnostic:
    /// every activation IID, name lookup, and dispatch id lands here.
    pub(crate) fn log(msg: &str) {
        use std::io::Write;
        let Ok(dir) = std::env::var("TEMP").or_else(|_| std::env::var("TMP")) else {
            return;
        };
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}\\xlcomshim.log"))
        {
            let pid = std::process::id();
            let _ = writeln!(f, "[{pid}] {msg}");
        }
    }

    pub fn run() -> ExitCode {
        let args: Vec<String> = std::env::args().collect();
        let joined = args.join(" ").to_lowercase();
        // COM launches a LocalServer32 with "-Embedding"; "/automation" is the
        // Office convention. "--serve" lets us run it by hand for testing.
        if joined.contains("-embedding")
            || joined.contains("/automation")
            || joined.contains("--serve")
        {
            match run_server() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    log(&format!("server error: {e:?}"));
                    ExitCode::FAILURE
                }
            }
        } else {
            eprintln!(
                "xlcomshim — Excel-compatible COM automation server (LocalServer32).\n\
                 Register with tools/comshim/register-shim.ps1; COM launches it with -Embedding."
            );
            ExitCode::SUCCESS
        }
    }

    fn run_server() -> Result<()> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            log("server starting; registering class object");

            let factory: IClassFactory = ExcelClassFactory.into();
            let cookie = CoRegisterClassObject(
                &SHIM_CLSID,
                &factory,
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE | REGCLS_SUSPENDED,
            )?;
            CoResumeClassObjects()?;
            log("class object registered; entering message loop");

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            log("message loop exited; revoking + uninitializing");
            CoRevokeClassObject(cookie)?;
            CoUninitialize();
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Class factory for Excel.Application
    // -----------------------------------------------------------------------

    #[implement(IClassFactory)]
    struct ExcelClassFactory;

    impl IClassFactory_Impl for ExcelClassFactory_Impl {
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
                let app: IDispatch = Application::new().into();
                app.query(riid, ppvobject).ok()
            }
        }

        fn LockServer(&self, flock: windows::Win32::Foundation::BOOL) -> Result<()> {
            if flock.as_bool() {
                object_born();
            } else {
                object_died();
            }
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // Application : IDispatch  (P0 subset)
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct Application;

    impl Application {
        fn new() -> Application {
            object_born();
            Application
        }
    }

    impl Drop for Application {
        fn drop(&mut self) {
            object_died();
        }
    }

    /// Resolve an `Application` member name to Excel's real DISPID.
    fn app_dispid(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "name" => ID_NAME,
            "version" => ID_VERSION,
            "visible" => ID_VISIBLE,
            "displayalerts" => ID_DISPLAYALERTS,
            "workbooks" => ID_WORKBOOKS,
            "quit" => ID_QUIT,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Application_Impl {
        fn GetTypeInfoCount(&self) -> Result<u32> {
            Ok(0) // no ITypeInfo in P0 (late-bound clients don't need it)
        }

        fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> Result<ITypeInfo> {
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
                if cnames == 0 {
                    return Ok(());
                }
                let names = std::slice::from_raw_parts(rgsznames, cnames as usize);
                let ids = std::slice::from_raw_parts_mut(rgdispid, cnames as usize);
                // Argument names (index >= 1) are not resolved.
                for id in ids.iter_mut() {
                    *id = DISPID_UNKNOWN;
                }
                let name = names[0].to_string().unwrap_or_default();
                match app_dispid(&name) {
                    Some(d) => {
                        log(&format!("Application::GetIDsOfNames '{name}' -> {d}"));
                        ids[0] = d;
                        Ok(())
                    }
                    None => {
                        log(&format!("Application::GetIDsOfNames '{name}' -> UNKNOWN"));
                        Err(DISP_E_UNKNOWNNAME.into())
                    }
                }
            }
        }

        fn Invoke(
            &self,
            dispidmember: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            pdispparams: *const DISPPARAMS,
            pvarresult: *mut VARIANT,
            _pexcepinfo: *mut EXCEPINFO,
            _puargerr: *mut u32,
        ) -> Result<()> {
            unsafe {
                let cargs = if pdispparams.is_null() {
                    0
                } else {
                    (*pdispparams).cArgs
                };
                log(&format!(
                    "Application::Invoke dispid={dispidmember} flags=0x{:x} cArgs={cargs} hasResult={}",
                    wflags.0,
                    !pvarresult.is_null()
                ));
                match dispidmember {
                    ID_NAME | DISPID_VALUE => put(pvarresult, VARIANT::from(BSTR::from("Docxy"))),
                    ID_VERSION => put(pvarresult, VARIANT::from(BSTR::from("16.0"))),
                    ID_VISIBLE => put(pvarresult, VARIANT::from(false)),
                    ID_DISPLAYALERTS => put(pvarresult, VARIANT::from(true)),
                    ID_WORKBOOKS => {
                        let wbs: IDispatch = Workbooks::new().into();
                        put(pvarresult, VARIANT::from(wbs));
                    }
                    ID_QUIT => {
                        log("Application::Quit -> shutting down");
                        PostQuitMessage(0);
                    }
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbooks : IDispatch  (P0 stub — proves child objects round-trip)
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct Workbooks;

    impl Workbooks {
        fn new() -> Workbooks {
            object_born();
            Workbooks
        }
    }

    impl Drop for Workbooks {
        fn drop(&mut self) {
            object_died();
        }
    }

    impl IDispatch_Impl for Workbooks_Impl {
        fn GetTypeInfoCount(&self) -> Result<u32> {
            Ok(0)
        }
        fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> Result<ITypeInfo> {
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
                if cnames == 0 {
                    return Ok(());
                }
                let names = std::slice::from_raw_parts(rgsznames, cnames as usize);
                let name = names[0].to_string().unwrap_or_default();
                log(&format!(
                    "Workbooks::GetIDsOfNames '{name}' -> UNKNOWN (P0 stub)"
                ));
                *rgdispid = DISPID_UNKNOWN;
                Err(DISP_E_UNKNOWNNAME.into())
            }
        }
        fn Invoke(
            &self,
            dispidmember: i32,
            _riid: *const GUID,
            _lcid: u32,
            _wflags: DISPATCH_FLAGS,
            _pdispparams: *const DISPPARAMS,
            _pvarresult: *mut VARIANT,
            _pexcepinfo: *mut EXCEPINFO,
            _puargerr: *mut u32,
        ) -> Result<()> {
            log(&format!(
                "Workbooks::Invoke dispid={dispidmember} (P0 stub)"
            ));
            Err(DISP_E_MEMBERNOTFOUND.into())
        }
    }

    /// Write a result VARIANT into an out-pointer (ignoring a null pointer, which
    /// a client passes when it discards the return value).
    unsafe fn put(pvarresult: *mut VARIANT, value: VARIANT) {
        if !pvarresult.is_null() {
            unsafe { std::ptr::write(pvarresult, value) };
        }
    }
}
