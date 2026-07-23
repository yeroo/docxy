//! xlcomshim — a COM **LocalServer32** that impersonates `Excel.Application`, so
//! applications that automate Office over COM can create spreadsheets without
//! Microsoft Excel installed. Document output is produced by the dependency-free
//! [`gridcore`] engine (the same one behind `xlsxy`).
//!
//! **P1 scope:** the create → write → save → quit path, late-bound over
//! `IDispatch`, backed by gridcore:
//! `Application → Workbooks.Add → Workbook → Worksheets → Worksheet → Range`,
//! cell writes via `Range.Value`/`.Formula` (and the `Cells(r,c)` / `Range("A1")`
//! addressing paths), and `Workbook.SaveAs(path, XlFileFormat)` writing a real
//! `.xlsx`. Member ids are Excel's **real DISPIDs** (verified against the live
//! Excel type library). Every activation and dispatch is logged to
//! `%TEMP%\xlcomshim.log` — the field diagnostic for Petrel on the VDI.
//!
//! Registration is out-of-band via `tools/comshim/register-shim.ps1` (per-user
//! `HKCU\Software\Classes`, a brand-new shim CLSID, guarded so it never clobbers
//! an installed Excel).


#[cfg(windows)]
pub use win::run;

#[cfg(windows)]
mod win {
    // COM interface methods are PascalCase by contract (they map to Excel's
    // typelib member names), so the generated interface traits opt out of the
    // snake_case lint.
    #![allow(non_snake_case)]

    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicI32, Ordering};

    use gridcore::engine::Engine;
    use gridcore::sheet::{
        Align, Cell, CellValue, Styles, Xf, cell_name, parse_cell_name, parse_range_name,
    };
    use gridcore::xlsx::{SheetPackage, new_xlsx, save_xlsx};

    use windows::Win32::Foundation::E_NOTIMPL;
    use windows::Win32::Foundation::{
        BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, DISP_E_BADINDEX,
        E_FAIL, E_POINTER, S_FALSE, S_OK,
    };
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoInitializeEx, CoRegisterClassObject,
        CoResumeClassObjects, CoRevokeClassObject, CoUninitialize, DISPATCH_FLAGS,
        DISPATCH_PROPERTYPUT, DISPATCH_PROPERTYPUTREF, DISPPARAMS, EXCEPINFO, IClassFactory,
        IClassFactory_Impl, IDispatch, IDispatch_Impl, IDispatch_Vtbl, ITypeInfo,
        REGCLS_MULTIPLEUSE, REGCLS_SUSPENDED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage,
    };
    use windows::core::{
        BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR, Result, VARIANT, implement, interface,
    };

    /// Our own coclass CLSID — a brand-new GUID, NEVER Microsoft's Excel CLSID
    /// {00024500-…}. `Excel.Application` (the ProgID) points here in HKCU.
    const SHIM_CLSID: GUID = GUID::from_u128(0x7b3f9e20_4c1a_4e8b_a2d6_9f5c1e0b7a31);

    /// Microsoft Excel's real coclass CLSID. We register a class object for it
    /// too, so an **early-bound** client (`new Excel.Application()` activates by
    /// this fixed CLSID, not the ProgID) reaches this server when the registry
    /// switch shadows it into HKCU. We never write this key into HKLM.
    const EXCEL_CLSID: GUID = GUID::from_u128(0x00024500_0000_0000_c000_000000000046);

    const DISPID_UNKNOWN: i32 = -1;

    // Excel's sheet extents, used when `Worksheet.Cells` (no index) yields a
    // Range covering the whole sheet.
    const MAX_ROW: u32 = 1_048_575;
    const MAX_COL: u32 = 16_383;

    // ---- VARIANT type tags (a VARIANT begins with its 16-bit VARTYPE) --------
    const VT_EMPTY: u16 = 0;
    const VT_BSTR: u16 = 8;
    const VT_ERROR: u16 = 10;
    const VT_BOOL: u16 = 11;

    /// Live `Application` objects; the server exits its loop when the count hits 0.
    static APPS: AtomicI32 = AtomicI32::new(0);

    // -----------------------------------------------------------------------
    // Shared workbook state (thread-local: the server is single-apartment STA,
    // so every Invoke is serialized on one thread — no locking needed, and the
    // COM objects hold plain Copy handles into this registry).
    // -----------------------------------------------------------------------

    struct Book {
        pkg: SheetPackage,
        engine: Engine,
        path: Option<String>,
        saved: bool,
        dirty: bool,
    }

    impl Book {
        fn new() -> Book {
            let pkg = new_xlsx();
            let engine = Engine::new(&pkg.workbook);
            Book {
                pkg,
                engine,
                path: None,
                saved: false,
                dirty: true,
            }
        }

        fn recalc_if_dirty(&mut self) {
            if self.dirty {
                self.engine.recalc_all(&mut self.pkg.workbook);
                self.dirty = false;
            }
        }

        fn set(&mut self, sheet: usize, r: u32, c: u32, cell: Cell) {
            self.engine
                .set_cell(&mut self.pkg.workbook, (sheet, r, c), cell);
            self.dirty = true;
            self.saved = false;
        }

        fn value(&mut self, sheet: usize, r: u32, c: u32) -> CellValue {
            self.recalc_if_dirty();
            self.pkg
                .workbook
                .sheets
                .get(sheet)
                .and_then(|s| s.cell(r, c))
                .map(|c| c.value.clone())
                .unwrap_or(CellValue::Empty)
        }

        fn formula_src(&self, sheet: usize, r: u32, c: u32) -> Option<String> {
            self.pkg
                .workbook
                .sheets
                .get(sheet)
                .and_then(|s| s.cell(r, c))
                .and_then(|c| c.formula.clone())
        }

        fn sheet_count(&self) -> usize {
            self.pkg.workbook.sheets.len()
        }

        fn sheet_name(&self, sheet: usize) -> String {
            self.pkg
                .workbook
                .sheets
                .get(sheet)
                .map(|s| s.name.clone())
                .unwrap_or_default()
        }

        fn save_as(&mut self, path: &str) -> std::io::Result<()> {
            self.recalc_if_dirty();
            let bytes = save_xlsx(&self.pkg);
            std::fs::write(path, bytes)?;
            self.path = Some(path.to_string());
            self.saved = true;
            Ok(())
        }
    }

    struct Registry {
        books: Vec<Book>,
        active: usize,
        visible: bool,
        display_alerts: bool,
    }

    thread_local! {
        static REG: RefCell<Registry> = const { RefCell::new(Registry {
            books: Vec::new(),
            active: 0,
            visible: false,
            display_alerts: true,
        }) };
    }

    fn reg<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
        REG.with(|r| f(&mut r.borrow_mut()))
    }

    // -----------------------------------------------------------------------
    // Logging (the field diagnostic)
    // -----------------------------------------------------------------------

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
            let _ = writeln!(f, "[{}] {msg}", std::process::id());
        }
    }

    // -----------------------------------------------------------------------
    // Server lifecycle
    // -----------------------------------------------------------------------

    /// Install a panic hook (once) that logs instead of letting a panic unwind
    /// across the COM FFI boundary (UB / RPC_E_SERVERFAULT with no diagnostic).
    fn install_panic_hook() {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| {
                log(&format!("PANIC: {info}"));
            }));
        });
    }

    pub fn run() -> ExitCode {
        install_panic_hook();
        let joined = std::env::args()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
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
            // Register the same factory for both our shim CLSID (ProgID path) and
            // Excel's real CLSID (early-bound activation path).
            let mut cookies = Vec::new();
            for clsid in [SHIM_CLSID, EXCEL_CLSID] {
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

    // -----------------------------------------------------------------------
    // Class factory
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
                let app: IApplication = Application::new().into();
                app.query(riid, ppvobject).ok()
            }
        }

        fn LockServer(&self, _flock: BOOL) -> Result<()> {
            Ok(())
        }
    }

    // -----------------------------------------------------------------------
    // In-process server exports (InprocServer32). When the shim is registered
    // as an in-proc DLL, COM loads it into the client's own process and calls
    // our vtable DIRECTLY — no proxy, no marshalling, NO type library required.
    // This is the path that works on a machine with no Office at all (the VDI):
    // the out-of-proc LocalServer needs a registered typelib to marshal our
    // dual interfaces, but in-proc needs nothing. Same COM objects either way.
    // -----------------------------------------------------------------------

    /// COM entry point: hand back a class object (our `IClassFactory`) for a
    /// CLSID this DLL implements. `regsvr32`/CoGetClassObject/CoCreateInstance
    /// (CLSCTX_INPROC_SERVER) all route here.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllGetClassObject(
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
            if clsid != SHIM_CLSID && clsid != EXCEL_CLSID {
                log(&format!("DllGetClassObject: unknown clsid {clsid:?}"));
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            // A panic hook, installed lazily, keeps any vtable-method panic from
            // unwinding across the FFI boundary into the host process.
            install_panic_hook();
            log(&format!("DllGetClassObject clsid={clsid:?}"));
            let factory: IClassFactory = ExcelClassFactory.into();
            factory.query(riid, ppv)
        }
    }

    /// COM asks whether the DLL can be unloaded. We keep it resident while any
    /// `Application` is alive (STA client, single-threaded export — simplest and
    /// safe).
    #[unsafe(no_mangle)]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        if APPS.load(Ordering::SeqCst) > 0 {
            S_FALSE
        } else {
            S_OK
        }
    }

    // -----------------------------------------------------------------------
    // VARIANT helpers
    // -----------------------------------------------------------------------

    /// The VARTYPE tag (masking BYREF/ARRAY flags). A VARIANT starts with its
    /// 16-bit `vt` field, so this read is layout-stable.
    unsafe fn vt_of(v: *const VARIANT) -> u16 {
        unsafe { *(v as *const u16) & 0x0fff }
    }

    /// Positional argument `i` (0 = first), accounting for `rgvarg` being stored
    /// in reverse order. `None` if omitted (fewer args passed).
    unsafe fn arg<'a>(p: *const DISPPARAMS, i: u32) -> Option<&'a VARIANT> {
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

    unsafe fn arg_string(p: *const DISPPARAMS, i: u32) -> Option<String> {
        unsafe { arg(p, i).and_then(variant_to_string) }
    }

    unsafe fn arg_i32(p: *const DISPPARAMS, i: u32) -> Option<i32> {
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
    unsafe fn arg_bool(p: *const DISPPARAMS, i: u32, default: bool) -> bool {
        unsafe { arg(p, i).and_then(|v| bool::try_from(v).ok()).unwrap_or(default) }
    }

    fn variant_to_string(v: &VARIANT) -> Option<String> {
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

    /// Interpret a VARIANT the way Excel interprets a value assigned to a cell:
    /// a string starting with `=` is a formula, other strings are text, bools are
    /// booleans, everything numeric is a number. `None` = empty/omitted (clear).
    fn variant_to_cell(v: &VARIANT) -> Option<Cell> {
        let vt = unsafe { vt_of(v) };
        match vt {
            VT_EMPTY | VT_ERROR => None,
            VT_BSTR => {
                let s = BSTR::try_from(v).map(|b| b.to_string()).unwrap_or_default();
                Some(match s.strip_prefix('=') {
                    Some(f) if !f.is_empty() => Cell::formula(f),
                    _ => Cell::text(&s),
                })
            }
            VT_BOOL => {
                let b = bool::try_from(v).unwrap_or(false);
                Some(Cell {
                    value: CellValue::Bool(b),
                    ..Cell::default()
                })
            }
            _ => f64::try_from(v).ok().map(Cell::number),
        }
    }

    fn cellvalue_to_variant(v: &CellValue) -> VARIANT {
        match v {
            CellValue::Empty => VARIANT::new(),
            CellValue::Number(n) => VARIANT::from(*n),
            CellValue::Text(s) => VARIANT::from(BSTR::from(s.as_str())),
            CellValue::Bool(b) => VARIANT::from(*b),
            CellValue::Error(e) => VARIANT::from(BSTR::from(e.as_str())),
        }
    }

    unsafe fn put(pvarresult: *mut VARIANT, value: VARIANT) {
        if !pvarresult.is_null() {
            unsafe { std::ptr::write(pvarresult, value) };
        }
    }

    /// Each child object now implements its own dual interface (not bare
    /// `IDispatch`), so to hand it back as a VT_DISPATCH VARIANT we convert to
    /// that interface then QI down to `IDispatch`.
    trait IntoDispatch {
        fn into_dispatch(self) -> IDispatch;
    }
    macro_rules! into_dispatch {
        ($struct:ty, $iface:ty) => {
            impl IntoDispatch for $struct {
                fn into_dispatch(self) -> IDispatch {
                    let i: $iface = self.into();
                    i.cast().expect("interface derives IDispatch")
                }
            }
        };
    }
    into_dispatch!(Workbooks, IWorkbooks);
    into_dispatch!(Workbook, IWorkbook);
    into_dispatch!(Worksheets, ISheets);
    into_dispatch!(Worksheet, IWorksheet);
    into_dispatch!(Range, IRange);
    into_dispatch!(Font, IFont);
    into_dispatch!(Interior, IInterior);

    unsafe fn put_obj<T: IntoDispatch>(pvarresult: *mut VARIANT, obj: T) {
        unsafe { put(pvarresult, VARIANT::from(obj.into_dispatch())) };
    }

    // -----------------------------------------------------------------------
    // Early-bound vtable method handlers (the real create-path members, called
    // by the generated interface stubs). [in] VARIANT is a 24-byte by-value
    // struct => passed by hidden pointer on x64, so we take `*const VARIANT`
    // (which also means we borrow, never drop, the caller's VARIANT).
    // -----------------------------------------------------------------------

    unsafe fn out_iface<I: Interface>(ret: *mut *mut c_void, iface: I) -> HRESULT {
        if ret.is_null() {
            return E_POINTER;
        }
        unsafe { *ret = iface.into_raw() };
        S_OK
    }
    unsafe fn out_bstr(ret: *mut BSTR, s: &str) -> HRESULT {
        if ret.is_null() {
            return E_POINTER;
        }
        unsafe { std::ptr::write(ret, BSTR::from(s)) };
        S_OK
    }
    unsafe fn out_bool(ret: *mut i16, v: bool) -> HRESULT {
        if ret.is_null() {
            return E_POINTER;
        }
        unsafe { *ret = if v { -1 } else { 0 } };
        S_OK
    }
    unsafe fn out_i4(ret: *mut i32, v: i32) -> HRESULT {
        if ret.is_null() {
            return E_POINTER;
        }
        unsafe { *ret = v };
        S_OK
    }
    unsafe fn out_var(ret: *mut VARIANT, v: VARIANT) -> HRESULT {
        if ret.is_null() {
            return E_POINTER;
        }
        unsafe { std::ptr::write(ret, v) };
        S_OK
    }
    fn vi32(v: *const VARIANT) -> Option<i32> {
        if v.is_null() {
            return None;
        }
        let vt = unsafe { vt_of(v) };
        if vt == VT_EMPTY || vt == VT_ERROR {
            None
        } else {
            i32::try_from(unsafe { &*v }).ok()
        }
    }

    // ---- Application ----
    unsafe fn vt_app_workbooks(_t: &Application_Impl, ret: *mut *mut c_void) -> HRESULT {
        let w: IWorkbooks = Workbooks.into();
        unsafe { out_iface(ret, w) }
    }
    unsafe fn vt_app_quit(_t: &Application_Impl) -> HRESULT {
        log("Application::Quit (early)");
        unsafe { PostQuitMessage(0) };
        S_OK
    }
    unsafe fn vt_app_da_get(_t: &Application_Impl, ret: *mut i16) -> HRESULT {
        unsafe { out_bool(ret, reg(|r| r.display_alerts)) }
    }
    unsafe fn vt_app_da_put(_t: &Application_Impl, v: i16) -> HRESULT {
        reg(|r| r.display_alerts = v != 0);
        S_OK
    }
    unsafe fn vt_app_vis_get(_t: &Application_Impl, ret: *mut i16) -> HRESULT {
        unsafe { out_bool(ret, reg(|r| r.visible)) }
    }
    unsafe fn vt_app_vis_put(_t: &Application_Impl, v: i16) -> HRESULT {
        reg(|r| r.visible = v != 0);
        S_OK
    }
    unsafe fn vt_app_name(_t: &Application_Impl, ret: *mut BSTR) -> HRESULT {
        unsafe { out_bstr(ret, "Docxy") }
    }
    unsafe fn vt_app_version(_t: &Application_Impl, ret: *mut BSTR) -> HRESULT {
        unsafe { out_bstr(ret, "16.0") }
    }

    // ---- Workbooks ----
    unsafe fn vt_wbs_add(_t: &Workbooks_Impl, ret: *mut *mut c_void) -> HRESULT {
        let book = reg(|r| {
            r.books.push(Book::new());
            r.active = r.books.len() - 1;
            r.active
        });
        let w: IWorkbook = Workbook { book }.into();
        unsafe { out_iface(ret, w) }
    }
    unsafe fn vt_wbs_count(_t: &Workbooks_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, reg(|r| r.books.len() as i32)) }
    }
    unsafe fn vt_wbs_item(_t: &Workbooks_Impl, index: *const VARIANT, ret: *mut *mut c_void) -> HRESULT {
        let idx = (vi32(index).unwrap_or(1).max(1) as usize) - 1;
        if !reg(|r| idx < r.books.len()) {
            return DISP_E_BADINDEX;
        }
        let w: IWorkbook = Workbook { book: idx }.into();
        unsafe { out_iface(ret, w) }
    }

    // ---- Workbook ----
    unsafe fn vt_wb_sheets(t: &Workbook_Impl, ret: *mut *mut c_void) -> HRESULT {
        let s: ISheets = Worksheets { book: t.book }.into();
        unsafe { out_iface(ret, s) }
    }
    unsafe fn vt_wb_name(t: &Workbook_Impl, ret: *mut BSTR) -> HRESULT {
        let name = reg(|r| r.books.get(t.book).and_then(|b| b.path.clone()))
            .as_deref()
            .and_then(|p| p.rsplit(['\\', '/']).next().map(str::to_string))
            .unwrap_or_else(|| "Book1".into());
        unsafe { out_bstr(ret, &name) }
    }
    unsafe fn vt_wb_saved_get(t: &Workbook_Impl, ret: *mut i16) -> HRESULT {
        unsafe { out_bool(ret, reg(|r| r.books.get(t.book).map(|b| b.saved).unwrap_or(true))) }
    }
    unsafe fn vt_wb_saved_put(t: &Workbook_Impl, v: i16) -> HRESULT {
        reg(|r| {
            if let Some(b) = r.books.get_mut(t.book) {
                b.saved = v != 0;
            }
        });
        S_OK
    }
    unsafe fn vt_wb_close(_t: &Workbook_Impl) -> HRESULT {
        S_OK
    }
    unsafe fn vt_wb_saveas(t: &Workbook_Impl, filename: *const VARIANT, _fmt: *const VARIANT) -> HRESULT {
        let Some(path) = (unsafe { filename.as_ref() }).and_then(variant_to_string) else {
            log("SaveAs(early): missing Filename");
            return E_FAIL;
        };
        log(&format!("SaveAs(early) '{path}'"));
        let book = t.book;
        let res = reg(|r| r.books.get_mut(book).map(|b| b.save_as(&path)));
        match res {
            Some(Ok(())) => S_OK,
            Some(Err(e)) => {
                log(&format!("SaveAs(early) failed: {e}"));
                E_FAIL
            }
            None => DISP_E_BADINDEX,
        }
    }

    // ---- Sheets (collection) ----
    unsafe fn vt_sheets_count(t: &Worksheets_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, reg(|r| r.books.get(t.book).map(|b| b.sheet_count()).unwrap_or(0)) as i32) }
    }
    unsafe fn vt_sheets_item(t: &Worksheets_Impl, index: *const VARIANT, ret: *mut *mut c_void) -> HRESULT {
        let book = t.book;
        let sheet = match unsafe { sheet_sel_idx(book, index) } {
            SheetSelV::Sheet(s) => s,
            SheetSelV::Invalid => return DISP_E_BADINDEX,
        };
        let ws: IWorksheet = Worksheet { book, sheet }.into();
        unsafe { out_iface(ret, ws) }
    }

    // ---- Worksheet ----
    unsafe fn vt_ws_name_get(t: &Worksheet_Impl, ret: *mut BSTR) -> HRESULT {
        let name = reg(|r| r.books.get(t.book).map(|b| b.sheet_name(t.sheet)).unwrap_or_default());
        unsafe { out_bstr(ret, &name) }
    }
    unsafe fn vt_ws_name_put(t: &Worksheet_Impl, v: *const u16) -> HRESULT {
        let name = unsafe { PCWSTR(v).to_string() }.unwrap_or_default();
        let (book, sheet) = (t.book, t.sheet);
        reg(|r| {
            if let Some(s) = r
                .books
                .get_mut(book)
                .and_then(|b| b.pkg.workbook.sheets.get_mut(sheet))
            {
                s.name = name;
            }
        });
        S_OK
    }
    unsafe fn vt_ws_cells(t: &Worksheet_Impl, ret: *mut *mut c_void) -> HRESULT {
        let rng: IRange = Range {
            book: t.book,
            sheet: t.sheet,
            r1: 0,
            c1: 0,
            r2: MAX_ROW,
            c2: MAX_COL,
        }
        .into();
        unsafe { out_iface(ret, rng) }
    }
    unsafe fn vt_ws_range(t: &Worksheet_Impl, cell1: *const VARIANT, _cell2: *const VARIANT, ret: *mut *mut c_void) -> HRESULT {
        let Some(a) = (unsafe { cell1.as_ref() }).and_then(variant_to_string) else {
            return E_FAIL;
        };
        let rect = parse_range_name(a.trim())
            .map(|(r1, c1, r2, c2)| (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
            .or_else(|| parse_cell_name(a.trim()).map(|(r, c)| (r, c, r, c)));
        let Some((r1, c1, r2, c2)) = rect else {
            return E_FAIL;
        };
        let rng: IRange = Range {
            book: t.book,
            sheet: t.sheet,
            r1,
            c1,
            r2,
            c2,
        }
        .into();
        unsafe { out_iface(ret, rng) }
    }

    // ---- Range ----
    unsafe fn vt_rng_child(t: &Range_Impl, row: *const VARIANT, col: *const VARIANT, ret: *mut VARIANT) -> HRESULT {
        let rr = vi32(row).unwrap_or(1).max(1) as u32 - 1;
        let cc = vi32(col).unwrap_or(1).max(1) as u32 - 1;
        let (r, c) = (t.r1 + rr, t.c1 + cc);
        let sub = Range {
            book: t.book,
            sheet: t.sheet,
            r1: r,
            c1: c,
            r2: r,
            c2: c,
        };
        unsafe { out_var(ret, VARIANT::from(sub.into_dispatch())) }
    }
    unsafe fn vt_rng_value_get(t: &Range_Impl, ret: *mut VARIANT) -> HRESULT {
        let (book, sheet, r1, c1) = (t.book, t.sheet, t.r1, t.c1);
        let val = reg(|r| {
            r.books
                .get_mut(book)
                .map(|b| b.value(sheet, r1, c1))
                .unwrap_or(CellValue::Empty)
        });
        unsafe { out_var(ret, cellvalue_to_variant(&val)) }
    }
    unsafe fn vt_rng_value_put(t: &Range_Impl, val: *const VARIANT) -> HRESULT {
        let cell = (unsafe { val.as_ref() })
            .and_then(variant_to_cell)
            .unwrap_or_default();
        t.write_fill(cell);
        S_OK
    }
    unsafe fn vt_rng_formula_get(t: &Range_Impl, ret: *mut VARIANT) -> HRESULT {
        let (book, sheet, r1, c1) = (t.book, t.sheet, t.r1, t.c1);
        let f = reg(|r| r.books.get(book).and_then(|b| b.formula_src(sheet, r1, c1)));
        let v = match f {
            Some(src) => VARIANT::from(BSTR::from(format!("={src}").as_str())),
            None => {
                let val = reg(|r| {
                    r.books
                        .get_mut(book)
                        .map(|b| b.value(sheet, r1, c1))
                        .unwrap_or(CellValue::Empty)
                });
                cellvalue_to_variant(&val)
            }
        };
        unsafe { out_var(ret, v) }
    }
    unsafe fn vt_rng_formula_put(t: &Range_Impl, val: *const VARIANT) -> HRESULT {
        let cell = match (unsafe { val.as_ref() }).and_then(variant_to_string) {
            Some(s) => match s.strip_prefix('=') {
                Some(f) if !f.is_empty() => Cell::formula(f),
                _ => (unsafe { val.as_ref() })
                    .and_then(variant_to_cell)
                    .unwrap_or_default(),
            },
            None => Cell::default(),
        };
        t.write_fill(cell);
        S_OK
    }
    unsafe fn vt_rng_item_put(t: &Range_Impl, row: *const VARIANT, col: *const VARIANT, val: *const VARIANT) -> HRESULT {
        let rr = vi32(row).unwrap_or(1).max(1) as u32 - 1;
        let cc = vi32(col).unwrap_or(1).max(1) as u32 - 1;
        let (r, c) = (t.r1 + rr, t.c1 + cc);
        let cell = (unsafe { val.as_ref() })
            .and_then(variant_to_cell)
            .unwrap_or_default();
        let (book, sheet) = (t.book, t.sheet);
        reg(|reg| {
            if let Some(b) = reg.books.get_mut(book) {
                b.set(sheet, r, c, cell);
            }
        });
        S_OK
    }

    /// Sheet selector variant used by the early-bound Sheets.Item handler
    /// (a name or a 1-based index). No arg is invalid here (Item always indexes).
    enum SheetSelV {
        Sheet(usize),
        Invalid,
    }
    unsafe fn sheet_sel_idx(book: usize, index: *const VARIANT) -> SheetSelV {
        if index.is_null() {
            return SheetSelV::Invalid;
        }
        let v = unsafe { &*index };
        if unsafe { vt_of(index) } == VT_BSTR {
            let name = variant_to_string(v).unwrap_or_default();
            match reg(|r| {
                r.books
                    .get(book)
                    .and_then(|b| b.pkg.workbook.sheet_index(&name))
            }) {
                Some(i) => SheetSelV::Sheet(i),
                None => SheetSelV::Invalid,
            }
        } else {
            let i = (vi32(index).unwrap_or(1).max(1) as usize) - 1;
            if reg(|r| r.books.get(book).map(|b| i < b.sheet_count()).unwrap_or(false)) {
                SheetSelV::Sheet(i)
            } else {
                SheetSelV::Invalid
            }
        }
    }

    fn is_put(wflags: DISPATCH_FLAGS) -> bool {
        (wflags.0 & (DISPATCH_PROPERTYPUT.0 | DISPATCH_PROPERTYPUTREF.0)) != 0
    }

    /// Excel's `Worksheets`/`Sheets` are *parameterized* properties: called with
    /// no argument they return the collection; called with an index or a name
    /// (as VBScript does for `wb.Worksheets(1)`) they return that sheet.
    enum SheetSel {
        Collection,
        Sheet(usize),
        Invalid,
    }

    unsafe fn sheet_sel(book: usize, params: *const DISPPARAMS) -> SheetSel {
        unsafe {
            let Some(v) = arg(params, 0) else {
                return SheetSel::Collection;
            };
            if vt_of(v) == VT_BSTR {
                let name = variant_to_string(v).unwrap_or_default();
                match reg(|r| {
                    r.books
                        .get(book)
                        .and_then(|b| b.pkg.workbook.sheet_index(&name))
                }) {
                    Some(i) => SheetSel::Sheet(i),
                    None => SheetSel::Invalid,
                }
            } else {
                let i = (arg_i32(params, 0).unwrap_or(1).max(1) as usize) - 1;
                if reg(|r| {
                    r.books
                        .get(book)
                        .map(|b| i < b.sheet_count())
                        .unwrap_or(false)
                }) {
                    SheetSel::Sheet(i)
                } else {
                    SheetSel::Invalid
                }
            }
        }
    }

    /// Shared `GetIDsOfNames`: resolve the first name via `resolver`, mark the
    /// rest (argument names) unknown.
    unsafe fn resolve_names(
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
                    // Don't fail — hand back a synthetic id so the follow-up
                    // Invoke reaches the graceful `unhandled` path (logged there).
                    log(&format!("{who}: unmodeled member '{name}' -> graceful"));
                    ids[0] = synth_id(&name);
                }
            }
            Ok(())
        }
    }

    macro_rules! no_typeinfo {
        () => {
            fn GetTypeInfoCount(&self) -> Result<u32> {
                Ok(0)
            }
            fn GetTypeInfo(&self, _i: u32, _l: u32) -> Result<ITypeInfo> {
                Err(DISP_E_BADINDEX.into())
            }
        };
    }

    // -----------------------------------------------------------------------
    // Graceful degradation — never fault on a member we don't model.
    //
    // Field strategy: cover broad, LOG every call, deploy on the VDI, then read
    // the log to see what the host (Petrel) actually called. For that to survive
    // the one-shot, an unmodeled member must NOT return an error (that throws in
    // the client and aborts the export) — it must degrade benignly: a property
    // put is swallowed, a get / method call yields a do-nothing object so a
    // chain like `range.Font.Bold = True` keeps flowing. Every such call is
    // logged with its member name for later triage.
    // -----------------------------------------------------------------------

    thread_local! {
        // dispid_of(unmodeled name) - stable per name so the get and the
        // subsequent put land on the same synthetic id; index into this table.
        static SYNTH: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }
    const SYNTH_BASE: i32 = 0x4000_0000;

    fn synth_id(name: &str) -> i32 {
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
    fn synth_name(id: i32) -> Option<String> {
        (id >= SYNTH_BASE).then(|| SYNTH.with(|s| s.borrow().get((id - SYNTH_BASE) as usize).cloned()))?
    }

    /// The default arm of every object's `Invoke` for a dispid it doesn't handle:
    /// log the member (name when we assigned a synthetic id, else the raw dispid)
    /// and degrade benignly — swallow puts, hand back a do-nothing object for
    /// gets/calls.
    unsafe fn unhandled(id: i32, wflags: DISPATCH_FLAGS, result: *mut VARIANT) -> Result<()> {
        let member = synth_name(id).unwrap_or_else(|| format!("dispid {id}"));
        // The caller's Invoke already logged the object + id at entry; this adds
        // the resolved member name and the benign outcome on the next line.
        log(&format!(
            "  -> unmodeled '{member}' (put={}) -> benign",
            is_put(wflags)
        ));
        if !is_put(wflags) {
            unsafe { put(result, VARIANT::from(null_dispatch())) };
        }
        Ok(())
    }

    /// A do-nothing `IDispatch`: resolves any name, swallows any put, returns
    /// itself for any get/call — so a client can walk unmodeled property chains
    /// without faulting.
    #[implement(IDispatch)]
    struct NullObject;

    fn null_dispatch() -> IDispatch {
        NullObject.into()
    }

    impl IDispatch_Impl for NullObject_Impl {
        no_typeinfo!();
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
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            _params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            log("NullObject::Invoke");
            unsafe { unhandled(id, wflags, result) }
        }
    }

    // -----------------------------------------------------------------------
    // Application
    // -----------------------------------------------------------------------

    // Excel's `_Application` (and, as coverage grows, the other) dual interfaces:
    // real IIDs, deriving IDispatch, with every vtable slot in Excel's exact
    // oVft order as an E_NOTIMPL stub. This makes an early-bound .NET client's
    // cast to `Excel.Application` succeed AND keeps any vtable call landing on a
    // real slot. Real create-path methods are layered on below; the stubs are
    // generated by tools/comshim/gen-vtables.ps1.
    include!("gen_excel.rs");

    #[implement(IApplication)]
    struct Application;

    impl Application {
        fn new() -> Application {
            APPS.fetch_add(1, Ordering::SeqCst);
            Application
        }
    }
    impl Drop for Application {
        fn drop(&mut self) {
            if APPS.fetch_sub(1, Ordering::SeqCst) == 1 {
                unsafe { PostQuitMessage(0) };
            }
        }
    }

    fn app_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "name" | "_default" => 110,
            "version" => 392,
            "visible" => 558,
            "displayalerts" => 343,
            "screenupdating" => 382,
            "calculation" => 316,
            "interactive" => 361,
            "usercontrol" => 1210,
            "workbooks" => 572,
            "worksheets" => 494,
            "sheets" => 485,
            "activeworkbook" => 308,
            "quit" => 302,
            "calculate" => 313,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Application_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Application", rgsznames, cnames, rgdispid, app_id) }
        }

        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            unsafe {
                log(&format!(
                    "Application::Invoke id={id} put={}",
                    is_put(wflags)
                ));
                match id {
                    110 => put(result, VARIANT::from("Docxy")),
                    392 => put(result, VARIANT::from("16.0")),
                    558 => {
                        if is_put(wflags) {
                            reg(|r| {
                                r.visible = arg(params, 0)
                                    .and_then(|v| bool::try_from(v).ok())
                                    .unwrap_or(false)
                            });
                        } else {
                            put(result, VARIANT::from(reg(|r| r.visible)));
                        }
                    }
                    343 => {
                        if is_put(wflags) {
                            reg(|r| {
                                r.display_alerts = arg(params, 0)
                                    .and_then(|v| bool::try_from(v).ok())
                                    .unwrap_or(true)
                            });
                        } else {
                            put(result, VARIANT::from(reg(|r| r.display_alerts)));
                        }
                    }
                    // ScreenUpdating / Interactive / UserControl — accept + report true.
                    382 | 361 | 1210 => {
                        if !is_put(wflags) {
                            put(result, VARIANT::from(true));
                        }
                    }
                    316 => {
                        if !is_put(wflags) {
                            put(result, VARIANT::from(-4105i32)); // xlCalculationAutomatic
                        }
                    }
                    572 => match arg(params, 0) {
                        None => put_obj(result, Workbooks),
                        Some(_) => {
                            let idx = (arg_i32(params, 0).unwrap_or(1).max(1) as usize) - 1;
                            if !reg(|r| idx < r.books.len()) {
                                return Err(DISP_E_BADINDEX.into());
                            }
                            put_obj(result, Workbook { book: idx });
                        }
                    },
                    494 | 485 => {
                        let book = reg(|r| r.active);
                        match sheet_sel(book, params) {
                            SheetSel::Collection => put_obj(result, Worksheets { book }),
                            SheetSel::Sheet(sheet) => put_obj(result, Worksheet { book, sheet }),
                            SheetSel::Invalid => return Err(DISP_E_BADINDEX.into()),
                        }
                    }
                    308 => {
                        let book = reg(|r| r.active);
                        put_obj(result, Workbook { book });
                    }
                    313 => {} // Calculate — no-op (we recalc lazily)
                    302 => {
                        log("Application::Quit");
                        PostQuitMessage(0);
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbooks
    // -----------------------------------------------------------------------

    #[implement(IWorkbooks)]
    struct Workbooks;

    fn workbooks_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "add" => 181,
            "item" => 170,
            "_default" => 0,
            "count" => 118,
            "open" => 1923,
            "close" => 277,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Workbooks_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Workbooks", rgsznames, cnames, rgdispid, workbooks_id) }
        }

        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            unsafe {
                log(&format!("Workbooks::Invoke id={id}"));
                match id {
                    181 => {
                        let book = reg(|r| {
                            r.books.push(Book::new());
                            r.active = r.books.len() - 1;
                            r.active
                        });
                        put_obj(result, Workbook { book });
                    }
                    170 | 0 => {
                        let idx = arg_i32(params, 0).unwrap_or(1).max(1) as usize - 1;
                        let ok = reg(|r| idx < r.books.len());
                        if !ok {
                            return Err(DISP_E_BADINDEX.into());
                        }
                        put_obj(result, Workbook { book: idx });
                    }
                    118 => put(result, VARIANT::from(reg(|r| r.books.len() as i32))),
                    277 => {} // Close all — no-op
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbook
    // -----------------------------------------------------------------------

    #[implement(IWorkbook)]
    struct Workbook {
        book: usize,
    }

    fn workbook_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "worksheets" => 494,
            "sheets" => 485,
            "activesheet" => 307,
            "saveas" => 3174,
            "save" => 283,
            "close" => 277,
            "saved" => 298,
            "name" => 110,
            "fullname" => 289,
            "path" => 291,
            "activate" => 304,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Workbook_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Workbook", rgsznames, cnames, rgdispid, workbook_id) }
        }

        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let book = self.book;
            unsafe {
                log(&format!(
                    "Workbook[{book}]::Invoke id={id} put={}",
                    is_put(wflags)
                ));
                match id {
                    494 | 485 => match sheet_sel(book, params) {
                        SheetSel::Collection => put_obj(result, Worksheets { book }),
                        SheetSel::Sheet(sheet) => put_obj(result, Worksheet { book, sheet }),
                        SheetSel::Invalid => return Err(DISP_E_BADINDEX.into()),
                    },
                    307 => put_obj(result, Worksheet { book, sheet: 0 }),
                    3174 => {
                        // SaveAs(Filename, [FileFormat], …)
                        let Some(path) = arg_string(params, 0) else {
                            log("SaveAs: missing Filename");
                            return Err(E_FAIL.into());
                        };
                        let fmt = arg_i32(params, 1);
                        log(&format!("SaveAs '{path}' fmt={fmt:?}"));
                        // 51 = xlOpenXMLWorkbook (.xlsx); other formats fall back
                        // to .xlsx today (gridcore writes OOXML) rather than fault.
                        let ok =
                            reg(|r| r.books.get_mut(book).map(|b| b.save_as(&path)).transpose());
                        match ok {
                            Ok(Some(())) => {}
                            Ok(None) => return Err(DISP_E_BADINDEX.into()),
                            Err(e) => {
                                log(&format!("SaveAs failed: {e}"));
                                return Err(E_FAIL.into());
                            }
                        }
                    }
                    283 => {
                        // Save to the existing path.
                        let res = reg(|r| {
                            let path = r.books.get(book).and_then(|b| b.path.clone());
                            match path {
                                Some(p) => r.books.get_mut(book).map(|b| b.save_as(&p)),
                                None => Some(Ok(())), // no path yet — no-op
                            }
                        });
                        if let Some(Err(e)) = res {
                            log(&format!("Save failed: {e}"));
                            return Err(E_FAIL.into());
                        }
                    }
                    298 => {
                        if is_put(wflags) {
                            let v = arg(params, 0)
                                .and_then(|v| bool::try_from(v).ok())
                                .unwrap_or(true);
                            reg(|r| {
                                if let Some(b) = r.books.get_mut(book) {
                                    b.saved = v;
                                }
                            });
                        } else {
                            put(
                                result,
                                VARIANT::from(reg(|r| {
                                    r.books.get(book).map(|b| b.saved).unwrap_or(true)
                                })),
                            );
                        }
                    }
                    277 => {} // Close — keep the handle valid; no teardown needed
                    304 => {} // Activate — no-op
                    110 | 289 | 291 => {
                        let s = reg(|r| r.books.get(book).and_then(|b| b.path.clone()));
                        let out = match id {
                            110 => s
                                .as_deref()
                                .and_then(|p| p.rsplit(['\\', '/']).next())
                                .unwrap_or("Book1")
                                .to_string(),
                            291 => s
                                .as_deref()
                                .and_then(|p| {
                                    p.rsplit_once(['\\', '/']).map(|(d, _)| d.to_string())
                                })
                                .unwrap_or_default(),
                            _ => s.unwrap_or_default(),
                        };
                        put(result, VARIANT::from(BSTR::from(out.as_str())));
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Worksheets / Sheets (collection)
    // -----------------------------------------------------------------------

    #[implement(ISheets)]
    struct Worksheets {
        book: usize,
    }

    fn sheets_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "item" => 170,
            "_default" => 0,
            "count" => 118,
            "add" => 181,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Worksheets_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Worksheets", rgsznames, cnames, rgdispid, sheets_id) }
        }

        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let book = self.book;
            unsafe {
                log(&format!("Worksheets[{book}]::Invoke id={id}"));
                match id {
                    170 | 0 => match sheet_sel(book, params) {
                        SheetSel::Sheet(sheet) => put_obj(result, Worksheet { book, sheet }),
                        _ => return Err(DISP_E_BADINDEX.into()),
                    },
                    118 => {
                        let n = reg(|r| r.books.get(book).map(|b| b.sheet_count()).unwrap_or(0));
                        put(result, VARIANT::from(n as i32));
                    }
                    181 => {
                        // Add — return the first sheet (P1 keeps the default sheet
                        // set; multi-sheet add lands with the broader coverage pass).
                        put_obj(result, Worksheet { book, sheet: 0 });
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Worksheet
    // -----------------------------------------------------------------------

    #[implement(IWorksheet)]
    struct Worksheet {
        book: usize,
        sheet: usize,
    }

    fn worksheet_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "name" => 110,
            "cells" => 238,
            "range" => 197,
            "columns" => 241,
            "rows" => 258,
            "usedrange" => 954,
            "activate" => 304,
            "select" => 235,
            _ => return None,
        })
    }

    /// A column letter ("A", "AB") -> 0-based index, via the cell-name parser.
    fn col_index(letters: &str) -> Option<u32> {
        parse_cell_name(&format!("{}1", letters.trim())).map(|(_, c)| c)
    }

    /// The `Columns`/`Rows` argument -> an inclusive 0-based span. Accepts a
    /// letter/number range ("A:B", "1:3"), a single letter/number, or nothing
    /// (the whole extent).
    unsafe fn columns_arg(params: *const DISPPARAMS) -> (u32, u32) {
        unsafe {
            match arg(params, 0) {
                None => (0, MAX_COL),
                Some(v) if vt_of(v) == VT_BSTR => {
                    let s = variant_to_string(v).unwrap_or_default();
                    if let Some((a, b)) = s.split_once(':') {
                        if let (Some(ca), Some(cb)) = (col_index(a), col_index(b)) {
                            return (ca.min(cb), ca.max(cb));
                        }
                    }
                    col_index(&s).map(|c| (c, c)).unwrap_or((0, MAX_COL))
                }
                Some(_) => {
                    let n = (arg_i32(params, 0).unwrap_or(1).max(1) as u32) - 1;
                    (n, n)
                }
            }
        }
    }
    unsafe fn rows_arg(params: *const DISPPARAMS) -> (u32, u32) {
        unsafe {
            let parse1 = |s: &str| s.trim().parse::<u32>().ok().map(|n| n.saturating_sub(1));
            match arg(params, 0) {
                None => (0, MAX_ROW),
                Some(v) if vt_of(v) == VT_BSTR => {
                    let s = variant_to_string(v).unwrap_or_default();
                    if let Some((a, b)) = s.split_once(':') {
                        if let (Some(ra), Some(rb)) = (parse1(a), parse1(b)) {
                            return (ra.min(rb), ra.max(rb));
                        }
                    }
                    parse1(&s).map(|r| (r, r)).unwrap_or((0, MAX_ROW))
                }
                Some(_) => {
                    let n = (arg_i32(params, 0).unwrap_or(1).max(1) as u32) - 1;
                    (n, n)
                }
            }
        }
    }

    impl IDispatch_Impl for Worksheet_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Worksheet", rgsznames, cnames, rgdispid, worksheet_id) }
        }

        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let (book, sheet) = (self.book, self.sheet);
            unsafe {
                log(&format!(
                    "Worksheet[{book}/{sheet}]::Invoke id={id} put={}",
                    is_put(wflags)
                ));
                match id {
                    110 => {
                        if is_put(wflags) {
                            if let Some(name) = arg_string(params, 0) {
                                reg(|r| {
                                    if let Some(s) = r
                                        .books
                                        .get_mut(book)
                                        .and_then(|b| b.pkg.workbook.sheets.get_mut(sheet))
                                    {
                                        s.name = name;
                                    }
                                });
                            }
                        } else {
                            let name = reg(|r| {
                                r.books
                                    .get(book)
                                    .map(|b| b.sheet_name(sheet))
                                    .unwrap_or_default()
                            });
                            put(result, VARIANT::from(BSTR::from(name.as_str())));
                        }
                    }
                    238 => {
                        // Cells or Cells(row, col).
                        if let (Some(rr), Some(cc)) = (arg_i32(params, 0), arg_i32(params, 1)) {
                            let r = (rr.max(1) - 1) as u32;
                            let c = (cc.max(1) - 1) as u32;
                            put_obj(
                                result,
                                Range {
                                    book,
                                    sheet,
                                    r1: r,
                                    c1: c,
                                    r2: r,
                                    c2: c,
                                },
                            );
                        } else {
                            put_obj(
                                result,
                                Range {
                                    book,
                                    sheet,
                                    r1: 0,
                                    c1: 0,
                                    r2: MAX_ROW,
                                    c2: MAX_COL,
                                },
                            );
                        }
                    }
                    197 => {
                        // Range("A1"[, "B2"]) or Range(cell1, cell2).
                        let a = arg_string(params, 0).unwrap_or_default();
                        let rect = if let Some(b) = arg_string(params, 1) {
                            match (parse_cell_name(a.trim()), parse_cell_name(b.trim())) {
                                (Some((r1, c1)), Some((r2, c2))) => {
                                    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
                                }
                                _ => None,
                            }
                        } else if let Some((r1, c1, r2, c2)) = parse_range_name(a.trim()) {
                            Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
                        } else {
                            parse_cell_name(a.trim()).map(|(r, c)| (r, c, r, c))
                        };
                        match rect {
                            Some((r1, c1, r2, c2)) => put_obj(
                                result,
                                Range {
                                    book,
                                    sheet,
                                    r1,
                                    c1,
                                    r2,
                                    c2,
                                },
                            ),
                            None => {
                                log(&format!("Range: cannot parse '{a}'"));
                                return Err(E_FAIL.into());
                            }
                        }
                    }
                    // Columns / Columns("A:B") / Columns(n) — span whole columns.
                    241 => {
                        let (c1, c2) = columns_arg(params);
                        put_obj(
                            result,
                            Range {
                                book,
                                sheet,
                                r1: 0,
                                c1,
                                r2: MAX_ROW,
                                c2,
                            },
                        );
                    }
                    // Rows / Rows("1:3") / Rows(n) — span whole rows.
                    258 => {
                        let (r1, r2) = rows_arg(params);
                        put_obj(
                            result,
                            Range {
                                book,
                                sheet,
                                r1,
                                c1: 0,
                                r2,
                                c2: MAX_COL,
                            },
                        );
                    }
                    // UsedRange — the populated bounding box (blank sheet -> A1).
                    954 => {
                        let (r1, c1, r2, c2) = reg(|r| {
                            r.books
                                .get(book)
                                .and_then(|b| b.pkg.workbook.sheets.get(sheet))
                                .map(used_bounds)
                                .unwrap_or((0, 0, 0, 0))
                        });
                        put_obj(
                            result,
                            Range {
                                book,
                                sheet,
                                r1,
                                c1,
                                r2,
                                c2,
                            },
                        );
                    }
                    304 | 235 => {} // Activate / Select — no-op
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    /// The bounding box of a sheet's populated cells (0,0,0,0 when empty).
    fn used_bounds(s: &gridcore::sheet::Sheet) -> (u32, u32, u32, u32) {
        let mut it = s.cells.keys();
        let Some(&(r0, c0)) = it.next() else {
            return (0, 0, 0, 0);
        };
        let (mut r1, mut c1, mut r2, mut c2) = (r0, c0, r0, c0);
        for &(r, c) in s.cells.keys() {
            r1 = r1.min(r);
            c1 = c1.min(c);
            r2 = r2.max(r);
            c2 = c2.max(c);
        }
        (r1, c1, r2, c2)
    }

    // -----------------------------------------------------------------------
    // Range
    // -----------------------------------------------------------------------

    // Range is a DISPINTERFACE ({..846-0000}), not a vtable dual: even REAL Excel
    // serves Range only through IDispatch — `(Excel.IRange)range` (the vtable IID
    // {..846-0001}) throws InvalidCastException against Excel itself. So our
    // IDispatch::Invoke path below is not a fallback, it is THE early-bound path,
    // matching Excel exactly. (Our IRange trait is generated only so an early-bound
    // client's `(Excel.Range)` cast resolves; the actual member calls dispatch.)
    #[implement(IRange)]
    struct Range {
        book: usize,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    }

    fn range_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "value" => 6,
            "value2" => 1388,
            "_default" => 0,
            "item" => 170,
            "formula" => 261,
            "formular1c1" => 264,
            "numberformat" | "numberformatlocal" => 193,
            "horizontalalignment" => 136,
            "columnwidth" => 242,
            "borders" => 435,
            "borderaround" => 2771,
            "cells" => 238,
            "font" => 146,
            "interior" => 129,
            "address" => 236,
            "row" => 257,
            "column" => 240,
            "count" => 118,
            "clearcontents" => 111,
            "select" => 235,
            "mergecells" | "merge" => 564,
            // Navigation — these commonly POSITION a subsequent write, so they
            // must return a real sub-Range (a NullObject would silently swallow
            // the data written through it).
            "offset" => 254,
            "resize" => 256,
            "rows" => 258,
            "columns" => 241,
            "entirerow" => 247,
            "entirecolumn" => 246,
            "text" => 138,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Range_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Range", rgsznames, cnames, rgdispid, range_id) }
        }

        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let this = (self.book, self.sheet, self.r1, self.c1, self.r2, self.c2);
            let (book, sheet, r1, c1, r2, c2) = this;
            unsafe {
                log(&format!(
                    "Range[{book}/{sheet} {r1},{c1}:{r2},{c2}]::Invoke id={id} put={}",
                    is_put(wflags)
                ));
                match id {
                    // Value / Value2
                    6 | 1388 => {
                        if is_put(wflags) {
                            let Some(v) = arg(params, 0) else {
                                return Ok(());
                            };
                            let Some(cell) = variant_to_cell(v) else {
                                // clearing
                                self.write_fill(Cell::default());
                                return Ok(());
                            };
                            self.write_fill(cell);
                        } else {
                            let val = reg(|r| {
                                r.books
                                    .get_mut(book)
                                    .map(|b| b.value(sheet, r1, c1))
                                    .unwrap_or(CellValue::Empty)
                            });
                            put(result, cellvalue_to_variant(&val));
                        }
                    }
                    // Formula
                    261 | 264 => {
                        if is_put(wflags) {
                            if let Some(s) = arg_string(params, 0) {
                                let cell = match s.strip_prefix('=') {
                                    Some(f) if !f.is_empty() => Cell::formula(f),
                                    _ => {
                                        variant_to_cell(arg(params, 0).unwrap()).unwrap_or_default()
                                    }
                                };
                                self.write_fill(cell);
                            }
                        } else {
                            let f = reg(|r| {
                                r.books.get(book).and_then(|b| b.formula_src(sheet, r1, c1))
                            });
                            match f {
                                Some(src) => put(
                                    result,
                                    VARIANT::from(BSTR::from(format!("={src}").as_str())),
                                ),
                                None => {
                                    let val = reg(|r| {
                                        r.books
                                            .get_mut(book)
                                            .map(|b| b.value(sheet, r1, c1))
                                            .unwrap_or(CellValue::Empty)
                                    });
                                    put(result, cellvalue_to_variant(&val));
                                }
                            }
                        }
                    }
                    // Item / _Default(row, col) → sub-cell Range
                    170 | 0 => {
                        let rr = arg_i32(params, 0).unwrap_or(1).max(1) as u32 - 1;
                        let cc = arg_i32(params, 1).unwrap_or(1).max(1) as u32 - 1;
                        let r = r1 + rr;
                        let c = c1 + cc;
                        put_obj(
                            result,
                            Range {
                                book,
                                sheet,
                                r1: r,
                                c1: c,
                                r2: r,
                                c2: c,
                            },
                        );
                    }
                    238 => put_obj(
                        result,
                        Range {
                            book,
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        },
                    ),
                    // NumberFormat / NumberFormatLocal — store the format code on
                    // the cells' xf so SaveAs writes a real numFmt.
                    193 => {
                        if is_put(wflags) {
                            if let Some(fmt) = arg_string(params, 0) {
                                apply_style(book, sheet, (r1, c1, r2, c2), move |xf| {
                                    xf.code = Some(fmt.clone())
                                });
                            }
                        } else {
                            let code = cell_xf(book, sheet, r1, c1)
                                .code
                                .unwrap_or_else(|| "General".into());
                            put(result, VARIANT::from(BSTR::from(code.as_str())));
                        }
                    }
                    // ColumnWidth (character units) — set on each column's <col>.
                    242 => {
                        if is_put(wflags) {
                            if let Some(w) = arg(params, 0).and_then(|v| f64::try_from(v).ok()) {
                                reg(|reg| {
                                    if let Some(b) = reg.books.get_mut(book) {
                                        if let Some(s) = b.pkg.workbook.sheets.get_mut(sheet) {
                                            for col in c1..=c2 {
                                                s.set_col_width(col, w);
                                            }
                                        }
                                        b.saved = false;
                                    }
                                });
                            }
                        } else {
                            let w = reg(|r| {
                                r.books
                                    .get(book)
                                    .and_then(|b| b.pkg.workbook.sheets.get(sheet).map(|s| s.col_width(c1)))
                            })
                            .unwrap_or(8.43);
                            put(result, VARIANT::from(w));
                        }
                    }
                    // HorizontalAlignment (xlLeft/-4131, xlCenter/-4108, xlRight/-4152).
                    136 => {
                        if is_put(wflags) {
                            let a = match arg_i32(params, 0) {
                                Some(-4131) => Align::Left,
                                Some(-4108) => Align::Center,
                                Some(-4152) => Align::Right,
                                _ => Align::General,
                            };
                            apply_style(book, sheet, (r1, c1, r2, c2), move |xf| xf.align = a);
                        }
                    }
                    // Borders -> collection object; BorderAround -> draw box now.
                    435 => {
                        let b: IDispatch = Borders {
                            book,
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        }
                        .into();
                        put(result, VARIANT::from(b));
                    }
                    2771 => apply_style(book, sheet, (r1, c1, r2, c2), |xf| xf.border = true),
                    146 => put_obj(
                        result,
                        Font {
                            book,
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        },
                    ),
                    129 => put_obj(
                        result,
                        Interior {
                            book,
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        },
                    ),
                    236 => put(
                        result,
                        VARIANT::from(BSTR::from(cell_name(r1, c1).as_str())),
                    ),
                    257 => put(result, VARIANT::from((r1 + 1) as i32)),
                    240 => put(result, VARIANT::from((c1 + 1) as i32)),
                    118 => put(
                        result,
                        VARIANT::from(((r2 - r1 + 1) * (c2 - c1 + 1)) as i32),
                    ),
                    111 => self.write_fill(Cell::default()),
                    235 | 564 => {} // Select / Merge — no-op in P1
                    // Offset(RowOffset, ColumnOffset) — shift the whole range.
                    254 => {
                        let dr = arg_i32(params, 0).unwrap_or(0) as i64;
                        let dc = arg_i32(params, 1).unwrap_or(0) as i64;
                        let sh = |v: u32, d: i64| (v as i64 + d).max(0) as u32;
                        put_obj(
                            result,
                            Range {
                                book,
                                sheet,
                                r1: sh(r1, dr),
                                c1: sh(c1, dc),
                                r2: sh(r2, dr),
                                c2: sh(c2, dc),
                            },
                        );
                    }
                    // Resize(RowSize, ColumnSize) — anchor at the top-left corner.
                    256 => {
                        let rs = arg_i32(params, 0).filter(|&x| x > 0).map(|x| x as u32);
                        let cs = arg_i32(params, 1).filter(|&x| x > 0).map(|x| x as u32);
                        let rs = rs.unwrap_or(r2 - r1 + 1);
                        let cs = cs.unwrap_or(c2 - c1 + 1);
                        put_obj(
                            result,
                            Range {
                                book,
                                sheet,
                                r1,
                                c1,
                                r2: r1 + rs - 1,
                                c2: c1 + cs - 1,
                            },
                        );
                    }
                    // Rows / Columns — return the range itself (Count/Item/writes
                    // still work); a faithful row/column iterator is not needed for
                    // the write path.
                    258 | 241 => put_obj(
                        result,
                        Range {
                            book,
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        },
                    ),
                    // EntireRow / EntireColumn — widen to the full row(s)/column(s).
                    247 => put_obj(
                        result,
                        Range {
                            book,
                            sheet,
                            r1,
                            c1: 0,
                            r2,
                            c2: MAX_COL,
                        },
                    ),
                    246 => put_obj(
                        result,
                        Range {
                            book,
                            sheet,
                            r1: 0,
                            c1,
                            r2: MAX_ROW,
                            c2,
                        },
                    ),
                    // Text — the displayed value of the top-left cell, as a string.
                    138 => {
                        let val = reg(|r| {
                            r.books
                                .get_mut(book)
                                .map(|b| b.value(sheet, r1, c1))
                                .unwrap_or(CellValue::Empty)
                        });
                        let s = match &val {
                            CellValue::Empty => String::new(),
                            CellValue::Number(n) => format!("{n}"),
                            CellValue::Text(t) => t.clone(),
                            CellValue::Bool(b) => {
                                if *b { "TRUE" } else { "FALSE" }.to_string()
                            }
                            CellValue::Error(e) => e.clone(),
                        };
                        put(result, VARIANT::from(BSTR::from(s.as_str())));
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    impl Range {
        /// Write `cell` into every cell of the range (scalar fill), guarding
        /// against an unbounded whole-sheet range.
        fn write_fill(&self, cell: Cell) {
            let (book, sheet) = (self.book, self.sheet);
            let (r1, c1, r2, c2) = (self.r1, self.c1, self.r2, self.c2);
            let cells = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1);
            if cells > 1_000_000 {
                log(&format!("write_fill: refusing huge range ({cells} cells)"));
                return;
            }
            reg(|reg| {
                if let Some(b) = reg.books.get_mut(book) {
                    for r in r1..=r2 {
                        for c in c1..=c2 {
                            b.set(sheet, r, c, cell.clone());
                        }
                    }
                }
            });
        }
    }

    // -----------------------------------------------------------------------
    // Cell formatting — applied to the workbook's style table so it survives
    // SaveAs. gridcore serialises authored xfs (bold/italic/font color/fill/
    // numFmt/alignment) into styles.xml, so real Excel shows the formatting.
    // -----------------------------------------------------------------------

    /// Intern `xf` in the style table (dedup), returning its `s=` index.
    fn xf_index(styles: &mut Styles, xf: Xf) -> u32 {
        match styles.xfs.iter().position(|x| *x == xf) {
            Some(i) => i as u32,
            None => {
                styles.xfs.push(xf);
                styles.xfs.len() as u32 - 1
            }
        }
    }

    /// Apply a formatting delta to every cell of a rect: read the cell's current
    /// Xf, mutate it, intern the result, and repoint the cell at it (creating a
    /// blank cell when needed — Excel formats empty cells too).
    fn apply_style(book: usize, sheet: usize, rect: (u32, u32, u32, u32), modify: impl Fn(&mut Xf)) {
        let (r1, c1, r2, c2) = rect;
        let cells = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1);
        if cells > 1_000_000 {
            log(&format!("apply_style: refusing huge range ({cells} cells)"));
            return;
        }
        reg(|reg| {
            let Some(b) = reg.books.get_mut(book) else {
                return;
            };
            let wb = &mut b.pkg.workbook;
            if sheet >= wb.sheets.len() {
                return;
            }
            for row in r1..=r2 {
                for col in c1..=c2 {
                    let cur = wb.sheets[sheet]
                        .cells
                        .get(&(row, col))
                        .map(|c| c.style)
                        .unwrap_or(0);
                    let mut xf = wb.styles.xfs.get(cur as usize).cloned().unwrap_or_default();
                    modify(&mut xf);
                    let idx = xf_index(&mut wb.styles, xf);
                    wb.sheets[sheet].cells.entry((row, col)).or_default().style = idx;
                }
            }
            b.saved = false;
        });
    }

    /// The resolved Xf of a single cell (its `s=` xf, or the default).
    fn cell_xf(book: usize, sheet: usize, r: u32, c: u32) -> Xf {
        reg(|reg| {
            reg.books.get(book).and_then(|b| {
                let wb = &b.pkg.workbook;
                let idx = wb.sheets.get(sheet)?.cells.get(&(r, c)).map(|c| c.style).unwrap_or(0);
                wb.styles.xfs.get(idx as usize).cloned()
            })
        })
        .unwrap_or_default()
    }

    /// Excel's Color is a packed BGR long: R + G*256 + B*65536.
    fn excel_color(c: i32) -> (u8, u8, u8) {
        let c = c as u32;
        ((c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8)
    }
    fn rgb_to_excel(rgb: (u8, u8, u8)) -> i32 {
        (rgb.0 as i32) | ((rgb.1 as i32) << 8) | ((rgb.2 as i32) << 16)
    }
    /// A slice of Excel's ColorIndex palette — enough for the common header colors.
    fn color_index(i: i32) -> Option<(u8, u8, u8)> {
        Some(match i {
            1 => (0, 0, 0),
            2 => (255, 255, 255),
            3 => (255, 0, 0),
            4 => (0, 255, 0),
            5 => (0, 0, 255),
            6 => (255, 255, 0),
            7 => (255, 0, 255),
            8 => (0, 255, 255),
            _ => return None,
        })
    }

    // -----------------------------------------------------------------------
    // Font / Interior — real formatting objects over gridcore styles. Each
    // carries the target range so `range.Font.Bold = True` / `.Interior.Color =`
    // land in styles.xml. Members we can't represent (Size/Name/Underline/
    // Pattern) fall through to the graceful `unhandled` path.
    // -----------------------------------------------------------------------

    #[implement(IFont)]
    struct Font {
        book: usize,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    }

    #[implement(IInterior)]
    struct Interior {
        book: usize,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    }

    fn font_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "bold" => 1,
            "italic" => 2,
            "color" => 3,
            "colorindex" => 4,
            "size" => 5,
            "name" => 6,
            _ => return None,
        })
    }
    fn interior_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "color" => 1,
            "colorindex" => 2,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Font_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Font", rgsznames, cnames, rgdispid, font_id) }
        }
        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let (book, sheet) = (self.book, self.sheet);
            let rect = (self.r1, self.c1, self.r2, self.c2);
            unsafe {
                log(&format!(
                    "Font[{book}/{sheet}]::Invoke id={id} put={}",
                    is_put(wflags)
                ));
                let put_flag = is_put(wflags);
                match id {
                    1 => {
                        if put_flag {
                            let on = arg_bool(params, 0, true);
                            apply_style(book, sheet, rect, move |xf| xf.bold = on);
                        } else {
                            put(result, VARIANT::from(cell_xf(book, sheet, rect.0, rect.1).bold));
                        }
                    }
                    2 => {
                        if put_flag {
                            let on = arg_bool(params, 0, true);
                            apply_style(book, sheet, rect, move |xf| xf.italic = on);
                        } else {
                            put(result, VARIANT::from(cell_xf(book, sheet, rect.0, rect.1).italic));
                        }
                    }
                    3 => {
                        if put_flag {
                            if let Some(c) = arg_i32(params, 0) {
                                let rgb = excel_color(c);
                                apply_style(book, sheet, rect, move |xf| xf.color = Some(rgb));
                            }
                        } else {
                            let c = cell_xf(book, sheet, rect.0, rect.1).color.map(rgb_to_excel);
                            put(result, VARIANT::from(c.unwrap_or(0)));
                        }
                    }
                    4 => {
                        if put_flag {
                            if let Some(rgb) = arg_i32(params, 0).and_then(color_index) {
                                apply_style(book, sheet, rect, move |xf| xf.color = Some(rgb));
                            }
                        }
                    }
                    5 => {
                        if put_flag {
                            if let Some(sz) = arg(params, 0).and_then(|v| f64::try_from(v).ok()) {
                                apply_style(book, sheet, rect, move |xf| xf.font_size = Some(sz));
                            }
                        } else {
                            put(
                                result,
                                VARIANT::from(cell_xf(book, sheet, rect.0, rect.1).font_size.unwrap_or(11.0)),
                            );
                        }
                    }
                    6 => {
                        if put_flag {
                            if let Some(nm) = arg_string(params, 0) {
                                apply_style(book, sheet, rect, move |xf| {
                                    xf.font_name = Some(nm.clone())
                                });
                            }
                        } else {
                            let n = cell_xf(book, sheet, rect.0, rect.1)
                                .font_name
                                .unwrap_or_else(|| "Calibri".into());
                            put(result, VARIANT::from(BSTR::from(n.as_str())));
                        }
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    impl IDispatch_Impl for Interior_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Interior", rgsznames, cnames, rgdispid, interior_id) }
        }
        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let (book, sheet) = (self.book, self.sheet);
            let rect = (self.r1, self.c1, self.r2, self.c2);
            unsafe {
                log(&format!(
                    "Interior[{book}/{sheet}]::Invoke id={id} put={}",
                    is_put(wflags)
                ));
                match id {
                    1 => {
                        if is_put(wflags) {
                            if let Some(c) = arg_i32(params, 0) {
                                let rgb = excel_color(c);
                                apply_style(book, sheet, rect, move |xf| xf.fill = Some(rgb));
                            }
                        } else {
                            let c = cell_xf(book, sheet, rect.0, rect.1).fill.map(rgb_to_excel);
                            put(result, VARIANT::from(c.unwrap_or(0)));
                        }
                    }
                    2 => {
                        if is_put(wflags) {
                            if let Some(rgb) = arg_i32(params, 0).and_then(color_index) {
                                apply_style(book, sheet, rect, move |xf| xf.fill = Some(rgb));
                            }
                        }
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Borders — `range.Borders`, `range.Borders(edge)`. We model a single thin
    // box border per cell, so setting a LineStyle/Weight on the collection or any
    // edge draws the box. (gridcore's style model doesn't carry per-edge styles.)
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct Borders {
        book: usize,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    }

    fn borders_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "item" | "_default" => 0,
            "linestyle" => 1,
            "weight" => 2,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Borders_Impl {
        no_typeinfo!();
        fn GetIDsOfNames(
            &self,
            _riid: *const GUID,
            rgsznames: *const PCWSTR,
            cnames: u32,
            _lcid: u32,
            rgdispid: *mut i32,
        ) -> Result<()> {
            unsafe { resolve_names("Borders", rgsznames, cnames, rgdispid, borders_id) }
        }
        fn Invoke(
            &self,
            id: i32,
            _riid: *const GUID,
            _lcid: u32,
            wflags: DISPATCH_FLAGS,
            _params: *const DISPPARAMS,
            result: *mut VARIANT,
            _ei: *mut EXCEPINFO,
            _ae: *mut u32,
        ) -> Result<()> {
            let (book, sheet) = (self.book, self.sheet);
            let rect = (self.r1, self.c1, self.r2, self.c2);
            unsafe {
                log(&format!("Borders[{book}/{sheet}]::Invoke id={id}"));
                match id {
                    // Borders(edge) -> another Borders over the same range; setting
                    // a LineStyle on it still draws our box.
                    0 => {
                        let b: IDispatch = Borders {
                            book,
                            sheet,
                            r1: rect.0,
                            c1: rect.1,
                            r2: rect.2,
                            c2: rect.3,
                        }
                        .into();
                        put(result, VARIANT::from(b));
                    }
                    // LineStyle / Weight put -> draw the box border.
                    1 | 2 => {
                        if is_put(wflags) {
                            apply_style(book, sheet, rect, |xf| xf.border = true);
                        }
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }
}
