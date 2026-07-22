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
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicI32, Ordering};

    use gridcore::engine::Engine;
    use gridcore::sheet::{Cell, CellValue, cell_name, parse_cell_name, parse_range_name};
    use gridcore::xlsx::{SheetPackage, new_xlsx, save_xlsx};

    use windows::Win32::Foundation::{
        BOOL, CLASS_E_NOAGGREGATION, DISP_E_BADINDEX, DISP_E_MEMBERNOTFOUND, DISP_E_UNKNOWNNAME,
        E_FAIL,
    };
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoInitializeEx, CoRegisterClassObject,
        CoResumeClassObjects, CoRevokeClassObject, CoUninitialize, DISPATCH_FLAGS,
        DISPATCH_PROPERTYPUT, DISPATCH_PROPERTYPUTREF, DISPPARAMS, EXCEPINFO, IClassFactory,
        IClassFactory_Impl, IDispatch, IDispatch_Impl, ITypeInfo, REGCLS_MULTIPLEUSE,
        REGCLS_SUSPENDED,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage,
    };
    use windows::core::{BSTR, GUID, IUnknown, Interface, PCWSTR, Result, VARIANT, implement};

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

    pub fn run() -> ExitCode {
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
                let app: IDispatch = Application::new().into();
                app.query(riid, ppvobject).ok()
            }
        }

        fn LockServer(&self, _flock: BOOL) -> Result<()> {
            Ok(())
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

    unsafe fn put_obj<T: Into<IDispatch>>(pvarresult: *mut VARIANT, obj: T) {
        unsafe { put(pvarresult, VARIANT::from(obj.into())) };
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
                Some(d) => {
                    ids[0] = d;
                    Ok(())
                }
                None => {
                    log(&format!("{who}: unknown member '{name}'"));
                    Err(DISP_E_UNKNOWNNAME.into())
                }
            }
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
    // Application
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
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
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbooks
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
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
            _wflags: DISPATCH_FLAGS,
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
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbook
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
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
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Worksheets / Sheets (collection)
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
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
            _wflags: DISPATCH_FLAGS,
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
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Worksheet
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct Worksheet {
        book: usize,
        sheet: usize,
    }

    fn worksheet_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "name" => 110,
            "cells" => 238,
            "range" => 197,
            "activate" => 304,
            "select" => 235,
            _ => return None,
        })
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
                    304 | 235 => {} // Activate / Select — no-op
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Range
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
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
                    // NumberFormat — accepted, not yet applied (formatting pass).
                    193 => {
                        if is_put(wflags) {
                            log(&format!(
                                "NumberFormat put (ignored in P1): {:?}",
                                arg_string(params, 0)
                            ));
                        } else {
                            put(result, VARIANT::from("General"));
                        }
                    }
                    146 => put_obj(result, Font),
                    129 => put_obj(result, Interior),
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
                    _ => return Err(DISP_E_MEMBERNOTFOUND.into()),
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
    // Font / Interior — accept-and-ignore in P1 (so `.Font.Bold = True` etc.
    // don't fault; real formatting lands with the broader coverage pass).
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct Font;

    #[implement(IDispatch)]
    struct Interior;

    fn ignore_id(name: &str) -> Option<i32> {
        // Any member resolves to a throwaway id; Invoke accepts puts and returns
        // benign gets.
        Some(match name.to_ascii_lowercase().as_str() {
            "bold" => 96,
            "italic" => 101,
            "size" => 104,
            "name" => 110,
            "color" => 99,
            "colorindex" => 97,
            "underline" => 106,
            "pattern" => 95,
            _ => 1,
        })
    }

    macro_rules! ignore_dispatch {
        ($t:ty, $who:literal) => {
            impl IDispatch_Impl for $t {
                no_typeinfo!();
                fn GetIDsOfNames(
                    &self,
                    _riid: *const GUID,
                    rgsznames: *const PCWSTR,
                    cnames: u32,
                    _lcid: u32,
                    rgdispid: *mut i32,
                ) -> Result<()> {
                    unsafe { resolve_names($who, rgsznames, cnames, rgdispid, ignore_id) }
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
                    unsafe {
                        if !is_put(wflags) {
                            put(result, VARIANT::from(false));
                        }
                        let _ = id;
                        Ok(())
                    }
                }
            }
        };
    }

    ignore_dispatch!(Font_Impl, "Font");
    ignore_dispatch!(Interior_Impl, "Interior");
}
