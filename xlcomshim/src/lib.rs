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

/// The dispinterfaces the shim serves + the mktypelib bin authors — (name, Office
/// source IID, our docxy IID). Shared so the .tlb and the shim never drift.
#[cfg(windows)]
pub use win::DISP_IFACES;

#[cfg(windows)]
mod win {
    // The vtable-stub arities in the `include!`d gen_*.rs are Excel's, not
    // ours: `#[interface]` gives each expanded slot the real interface's
    // parameter list, and several of Excel's run well past clippy's
    // 7-argument advice. There is nothing here to refactor.
    #![allow(clippy::too_many_arguments)]
    // COM interface methods are PascalCase by contract (they map to Excel's
    // typelib member names), so the generated interface traits opt out of the
    // snake_case lint.
    #![allow(non_snake_case)]

    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::process::ExitCode;
    // Shared COM scaffolding: VARIANT helpers, logging, graceful degradation,
    // resolve_names, the server + DLL plumbing, the Application counter, VT tags,
    // and the no_typeinfo! macro.
    use comshimcore::*;

    use gridcore::engine::Engine;
    use gridcore::sheet::{
        Align, Cell, CellValue, Styles, Xf, cell_name, parse_cell_name, parse_range_name,
    };
    use gridcore::xlsx::{
        SheetPackage, SpreadsheetKind, load_xlsx, new_xlsx, save_xlsx, save_xlsx_as,
    };

    use windows::Win32::Foundation::{
        DISP_E_BADINDEX, DISP_E_TYPEMISMATCH, E_FAIL, E_NOTIMPL, E_POINTER, S_OK,
    };
    use windows::Win32::System::Com::{
        DISPATCH_FLAGS, DISPPARAMS, EXCEPINFO, IDispatch, IDispatch_Impl, IDispatch_Vtbl,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::WindowsAndMessaging::PostQuitMessage;
    use windows::core::{BSTR, GUID, HRESULT, Interface, PCWSTR, Result, implement, interface};

    /// OUR authored type library's LIBID (see the mktypelib bin's `docxy_libid`).
    /// We source per-object typeinfo from HERE — our own registered docxy-excel.tlb
    /// — so it works on a machine with NO Excel (the VDI), not just this dev box.
    const DOCXY_LIBID: GUID = GUID::from_u128(0x7b3f9e21_4c1a_4e8b_a2d6_9f5c1e0b7a31);

    /// The dispinterfaces we author and serve, as (name, Office source IID, our
    /// docxy IID). The mktypelib bin copies each Office dispinterface (its real
    /// memids and invkinds) into our .tlb under OUR IID; the shim's `GetTypeInfo`
    /// returns that IID's typeinfo, so a typeinfo-driven late-bound client
    /// (pywin32, VB6) introspects each object correctly. One source of truth for
    /// both.
    // Names are prefixed `Docxy` so they never collide with the identically-named
    // dual `wanted` interfaces in the same typelib (a typelib requires unique type
    // names); the name is cosmetic for the dispatch path (clients read members).
    pub const DISP_IFACES: &[(&str, u128, u128)] = &[
        (
            "DocxyApplication",
            0x000208d5_0000_0000_c000_000000000046,
            0xd0c9a001_0208_d500_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyWorkbooks",
            0x000208db_0000_0000_c000_000000000046,
            0xd0c9a002_0208_db00_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyWorkbook",
            0x000208da_0000_0000_c000_000000000046,
            0xd0c9a003_0208_da00_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyWorksheets",
            0x000208b1_0000_0000_c000_000000000046,
            0xd0c9a004_0208_b100_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyWorksheet",
            0x000208d8_0000_0000_c000_000000000046,
            0xd0c9a005_0208_d800_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyRange",
            0x00020846_0000_0000_c000_000000000046,
            0xd0c9a006_0002_0846_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyFont",
            0x0002084d_0000_0000_c000_000000000046,
            0xd0c9a007_0002_084d_a2d6_9f5c1e0b7a31,
        ),
        (
            "DocxyInterior",
            0x00020870_0000_0000_c000_000000000046,
            0xd0c9a008_0002_0870_a2d6_9f5c1e0b7a31,
        ),
    ];

    /// Return the ITypeInfo for one of OUR dispinterface IIDs from our registered
    /// docxy typelib. Errs (→ client falls back to typeinfo-less dynamic) when the
    /// .tlb isn't registered.
    fn docxy_typeinfo(iid_u128: u128) -> Result<windows::Win32::System::Com::ITypeInfo> {
        unsafe {
            windows::Win32::System::Ole::LoadRegTypeLib(&DOCXY_LIBID, 1, 0, 0)?
                .GetTypeInfoOfGuid(&GUID::from_u128(iid_u128))
        }
    }

    /// The two IDispatch typeinfo methods for an object, sourcing its dispinterface
    /// typeinfo by OUR docxy IID.
    macro_rules! xl_typeinfo {
        ($iid:expr) => {
            fn GetTypeInfoCount(&self) -> Result<u32> {
                Ok(1)
            }
            fn GetTypeInfo(
                &self,
                i: u32,
                _l: u32,
            ) -> Result<windows::Win32::System::Com::ITypeInfo> {
                if i != 0 {
                    return Err(DISP_E_BADINDEX.into());
                }
                docxy_typeinfo($iid)
            }
        };
    }

    /// Our own coclass CLSID — a brand-new GUID, NEVER Microsoft's Excel CLSID
    /// {00024500-…}. `Excel.Application` (the ProgID) points here in HKCU.
    const SHIM_CLSID: GUID = GUID::from_u128(0x7b3f9e20_4c1a_4e8b_a2d6_9f5c1e0b7a31);

    /// Microsoft Excel's real coclass CLSID. We register a class object for it
    /// too, so an **early-bound** client (`new Excel.Application()` activates by
    /// this fixed CLSID, not the ProgID) reaches this server when the registry
    /// switch shadows it into HKCU. We never write this key into HKLM.
    const EXCEL_CLSID: GUID = GUID::from_u128(0x00024500_0000_0000_c000_000000000046);

    // -----------------------------------------------------------------------
    // Server / DLL plumbing — the generic runtime lives in comshimcore; the shim
    // supplies only its CLSIDs and the root Application constructor.
    // -----------------------------------------------------------------------

    fn make_app() -> IDispatch {
        let a: IApplication = Application::new().into();
        a.cast().expect("Application derives IDispatch")
    }

    pub fn run() -> ExitCode {
        init("xlcomshim");
        if !should_serve() {
            eprintln!(
                "xlcomshim — Excel-compatible COM automation server (LocalServer32).\n\
                 Register with tools/comshim/register-shim.ps1; COM launches it with -Embedding."
            );
            return ExitCode::SUCCESS;
        }
        match run_local_server(SHIM_CLSID, EXCEL_CLSID, make_app) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                log(&format!("server error: {e:?}"));
                ExitCode::FAILURE
            }
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllGetClassObject(
        rclsid: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        unsafe { dll_get_class_object(SHIM_CLSID, EXCEL_CLSID, make_app, rclsid, riid, ppv) }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        dll_can_unload_now()
    }

    // Excel's sheet extents, used when `Worksheet.Cells` (no index) yields a
    // Range covering the whole sheet.
    const MAX_ROW: u32 = 1_048_575;
    const MAX_COL: u32 = 16_383;

    // -----------------------------------------------------------------------
    // Shared workbook state (thread-local: the server is single-apartment STA,
    // so every Invoke is serialized on one thread — no locking needed, and the
    // COM objects hold plain Copy handles into this registry).
    // -----------------------------------------------------------------------

    struct Book {
        pkg: SheetPackage,
        engine: Engine,
        path: Option<String>,
        /// The file type chosen at the last SaveAs, which a later Save keeps
        /// (Excel's does); `None` goes by the path's extension.
        kind: Option<SpreadsheetKind>,
        saved: bool,
        dirty: bool,
    }

    /// The recalc engine for a workbook, on the local clock `TODAY()` and
    /// `NOW()` read, as Excel's.
    fn book_engine(pkg: &SheetPackage) -> Engine {
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = gridcore::clock::local_now_serial();
        engine
    }

    impl Book {
        fn new() -> Book {
            let pkg = new_xlsx();
            let engine = book_engine(&pkg);
            Book {
                pkg,
                engine,
                path: None,
                kind: None,
                saved: false,
                dirty: true,
            }
        }

        /// Load an existing `.xlsx` from disk (backs `Workbooks.Open`).
        fn open(path: &str) -> Option<Book> {
            let bytes = std::fs::read(path).ok()?;
            let pkg = load_xlsx(&bytes).ok()?;
            let engine = book_engine(&pkg);
            Some(Book {
                pkg,
                engine,
                path: Some(path.to_string()),
                kind: None,
                saved: true,
                dirty: false,
            })
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

        /// Assign `put` to every cell of the rect `(r1, c1, r2, c2)`, as
        /// Excel's Range.Value / Formula puts do: a string is entered like
        /// typed text into each cell under its own format (`"42"` a number,
        /// `"1/15/2024"` a date, `"'007"` quote-prefixed text), a formula's
        /// relative references moving per cell as a fill would; a number or
        /// boolean keeps each cell's format; [`Put::Clear`] resets the cell,
        /// format and all.
        fn assign(&mut self, sheet: usize, (r1, c1, r2, c2): (u32, u32, u32, u32), put: &Put) {
            match put {
                Put::Clear => {
                    for r in r1..=r2 {
                        for c in c1..=c2 {
                            self.set(sheet, r, c, Cell::default());
                        }
                    }
                }
                Put::Value(value) => {
                    for r in r1..=r2 {
                        for c in c1..=c2 {
                            let cell = self.value_cell(sheet, r, c, value.clone());
                            self.set(sheet, r, c, cell);
                        }
                    }
                }
                Put::Text(text) => {
                    let today = self.engine.clock;
                    let wb = &mut self.pkg.workbook;
                    let rect = (r1, c1, r2, c2);
                    match gridcore::entry::entry_range(wb, sheet, rect, (r1, c1), text, today) {
                        Ok(cells) => {
                            for (r, c, cell) in cells {
                                self.set(sheet, r, c, cell);
                            }
                        }
                        Err(e) => log(&format!("put refused: {e}")),
                    }
                }
            }
        }

        /// `value` as the constant of (sheet, r, c), on that cell's own style
        /// with any quote prefix cleared (a number is not quoted text).
        fn value_cell(&mut self, sheet: usize, r: u32, c: u32, value: CellValue) -> Cell {
            let base = self
                .pkg
                .workbook
                .sheets
                .get(sheet)
                .and_then(|s| s.cell(r, c))
                .map_or(0, |c| c.style);
            let e = gridcore::entry::Entry {
                cell: Cell {
                    value,
                    ..Cell::default()
                },
                format: None,
                quote_prefix: false,
                wrap: false,
            };
            let style = gridcore::entry::entry_style(&mut self.pkg.workbook.styles, base, &e);
            Cell { style, ..e.cell }
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

        /// Write the workbook as `kind` (`None`: keep the loaded type). A
        /// macro-free kind drops the VBA project without asking, as Excel does
        /// under automation with `DisplayAlerts = False`.
        fn save_as(&mut self, path: &str, kind: Option<SpreadsheetKind>) -> std::io::Result<()> {
            // No OOXML FileFormat and no spreadsheet extension: Excel keeps
            // the format last used.
            let kind = kind.or(self.kind);
            self.recalc_if_dirty();
            let bytes = match kind {
                Some(kind) => save_xlsx_as(&self.pkg, kind),
                None => save_xlsx(&self.pkg),
            };
            std::fs::write(path, bytes)?;
            self.path = Some(path.to_string());
            self.kind = kind;
            // Written without macros: the open workbook drops them too, so a
            // later SaveAs to a macro type cannot bring them back.
            if kind.is_some_and(|k| !k.allows_macros()) {
                self.pkg.remove_vba_project();
            }
            self.saved = true;
            Ok(())
        }

        /// `Workbook.Save`: the current path, in the type the last SaveAs
        /// chose, else its extension's. `None` when there is no path yet.
        fn save(&mut self) -> Option<std::io::Result<()>> {
            let path = self.path.clone()?;
            let kind = self.kind.or_else(|| SpreadsheetKind::from_path(&path));
            Some(self.save_as(&path, kind))
        }
    }

    /// The file type `SaveAs(path, FileFormat)` writes: Excel's
    /// `XlFileFormat` decides, then the path's extension, else the loaded type.
    fn kind_for(fmt: Option<i32>, path: &str) -> Option<SpreadsheetKind> {
        match fmt {
            Some(51) => Some(SpreadsheetKind::Workbook), // xlOpenXMLWorkbook
            Some(52) => Some(SpreadsheetKind::MacroWorkbook), // xlOpenXMLWorkbookMacroEnabled
            Some(53) => Some(SpreadsheetKind::MacroTemplate), // xlOpenXMLTemplateMacroEnabled
            Some(54) => Some(SpreadsheetKind::Template), // xlOpenXMLTemplate
            _ => SpreadsheetKind::from_path(path),
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

    /// The string reported by `Application.Name`. Honest by default; an operator
    /// who needs a client that literally checks for `"Microsoft Excel"` can set
    /// `XLCOMSHIM_APP_NAME` in their own environment. Kept out of the default so
    /// the shipped shim never presents itself as Microsoft's product.
    fn app_name() -> String {
        std::env::var("XLCOMSHIM_APP_NAME").unwrap_or_else(|_| "Docxy".to_string())
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

    /// How many of `params`' arguments are positional. An indexed property put
    /// (`ws.Cells(1, 1) = "x"`) carries the assigned value as a NAMED argument
    /// (`DISPID_PROPERTYPUT`), so `cArgs` alone would read it as one index too
    /// many — `ws.Range("A1") = "hi"` looks like `Range("A1", "hi")`.
    ///
    /// # Safety
    /// `params` must be null or a valid `DISPPARAMS`.
    unsafe fn n_pos_args(params: *const DISPPARAMS) -> u32 {
        unsafe {
            params
                .as_ref()
                .map_or(0, |dp| dp.cArgs.saturating_sub(dp.cNamedArgs))
        }
    }

    /// What a Value / Formula / Item put or a clear assigns to each cell of
    /// its range.
    #[derive(Clone, Debug, PartialEq)]
    enum Put {
        /// Range.Clear (111): value and format go.
        Clear,
        /// A number, a boolean, or `Empty` (the contents cleared, as a put of
        /// nothing and Range.ClearContents (113) do): the constant lands on
        /// each cell's own format.
        Value(CellValue),
        /// A string, entered as if typed into each cell ([`Book::assign`]).
        Text(String),
    }

    /// What an indexed property put is assigning. COM puts the named
    /// `DISPID_PROPERTYPUT` argument FIRST in `rgvarg`; an empty/omitted value
    /// clears the cell's contents, which is what Excel does.
    ///
    /// # Safety
    /// `params` must be null or a valid `DISPPARAMS`.
    unsafe fn put_arg(params: *const DISPPARAMS) -> Option<Put> {
        unsafe {
            put_of(
                params
                    .as_ref()
                    .filter(|dp| dp.cArgs > 0 && dp.cNamedArgs > 0)
                    .map(|dp| &*dp.rgvarg),
            )
        }
    }

    /// Interpret a VARIANT the way Excel interprets a value assigned to a
    /// cell: a string as typed entry (`=…` a formula, `42` a number, `'…`
    /// text), bools as booleans, numbers as numbers; empty/omitted (or null)
    /// clears the contents. `None` for an array (`Range.Value = arr`), which
    /// the shim cannot spread over cells: the put is refused
    /// ([`DISP_E_TYPEMISMATCH`]) rather than read as nothing, which would
    /// clear the range. App-specific (over gridcore).
    fn put_of(v: Option<&VARIANT>) -> Option<Put> {
        let Some(v) = v else {
            return Some(Put::Value(CellValue::Empty));
        };
        // `vt_of` masks the flags; VT_ARRAY is 0x2000 of the raw tag.
        if unsafe { *(v as *const VARIANT as *const u16) } & 0x2000 != 0 {
            return None;
        }
        Some(match unsafe { vt_of(v) } {
            VT_EMPTY | VT_ERROR => Put::Value(CellValue::Empty),
            VT_BSTR => Put::Text(BSTR::try_from(v).map(|b| b.to_string()).unwrap_or_default()),
            VT_BOOL => Put::Value(CellValue::Bool(bool::try_from(v).unwrap_or(false))),
            _ => Put::Value(f64::try_from(v).map_or(CellValue::Empty, CellValue::Number)),
        })
    }

    fn cellvalue_to_variant(v: &CellValue) -> VARIANT {
        match v {
            CellValue::Empty => VARIANT::default(),
            CellValue::Number(n) => VARIANT::from(*n),
            CellValue::Text(s) => VARIANT::from(BSTR::from(s.as_str())),
            CellValue::Bool(b) => VARIANT::from(*b),
            CellValue::Error(e) => VARIANT::from(BSTR::from(e.as_str())),
        }
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
        // Same source as the IDispatch path: hardcoding it here left
        // `XLCOMSHIM_APP_NAME` dead for exactly the early-bound clients that
        // gate on the name.
        unsafe { out_bstr(ret, &app_name()) }
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
    unsafe fn vt_wbs_item(
        _t: &Workbooks_Impl,
        index: *const VARIANT,
        ret: *mut *mut c_void,
    ) -> HRESULT {
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
        unsafe {
            out_bool(
                ret,
                reg(|r| r.books.get(t.book).map(|b| b.saved).unwrap_or(true)),
            )
        }
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
    unsafe fn vt_wb_saveas(
        t: &Workbook_Impl,
        filename: *const VARIANT,
        fmt: *const VARIANT,
    ) -> HRESULT {
        let Some(path) = (unsafe { filename.as_ref() }).and_then(variant_to_string) else {
            log("SaveAs(early): missing Filename");
            return E_FAIL;
        };
        let fmt = vi32(fmt);
        log(&format!("SaveAs(early) '{path}' fmt={fmt:?}"));
        let book = t.book;
        let kind = kind_for(fmt, &path);
        let res = reg(|r| r.books.get_mut(book).map(|b| b.save_as(&path, kind)));
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
        unsafe {
            out_i4(
                ret,
                reg(|r| r.books.get(t.book).map(|b| b.sheet_count()).unwrap_or(0)) as i32,
            )
        }
    }
    unsafe fn vt_sheets_item(
        t: &Worksheets_Impl,
        index: *const VARIANT,
        ret: *mut *mut c_void,
    ) -> HRESULT {
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
        let name = reg(|r| {
            r.books
                .get(t.book)
                .map(|b| b.sheet_name(t.sheet))
                .unwrap_or_default()
        });
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
    unsafe fn vt_ws_range(
        t: &Worksheet_Impl,
        cell1: *const VARIANT,
        _cell2: *const VARIANT,
        ret: *mut *mut c_void,
    ) -> HRESULT {
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
    unsafe fn vt_rng_child(
        t: &Range_Impl,
        row: *const VARIANT,
        col: *const VARIANT,
        ret: *mut VARIANT,
    ) -> HRESULT {
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
        let Some(put) = put_of(unsafe { val.as_ref() }) else {
            return DISP_E_TYPEMISMATCH;
        };
        t.write_fill(put);
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
        // A formula put is a value put: `=…` enters as a formula, any other
        // string as the typed value it spells.
        let Some(put) = put_of(unsafe { val.as_ref() }) else {
            return DISP_E_TYPEMISMATCH;
        };
        t.write_fill(put);
        S_OK
    }
    unsafe fn vt_rng_item_put(
        t: &Range_Impl,
        row: *const VARIANT,
        col: *const VARIANT,
        val: *const VARIANT,
    ) -> HRESULT {
        let rr = vi32(row).unwrap_or(1).max(1) as u32 - 1;
        let cc = vi32(col).unwrap_or(1).max(1) as u32 - 1;
        let (r, c) = (t.r1 + rr, t.c1 + cc);
        let Some(put) = put_of(unsafe { val.as_ref() }) else {
            return DISP_E_TYPEMISMATCH;
        };
        let (book, sheet) = (t.book, t.sheet);
        reg(|reg| {
            if let Some(b) = reg.books.get_mut(book) {
                b.assign(sheet, (r, c, r, c), &put);
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
            if reg(|r| {
                r.books
                    .get(book)
                    .map(|b| i < b.sheet_count())
                    .unwrap_or(false)
            }) {
                SheetSelV::Sheet(i)
            } else {
                SheetSelV::Invalid
            }
        }
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

    #[implement(IApplication, Agile = false)]
    struct Application;

    impl Application {
        fn new() -> Application {
            app_created();
            Application
        }
    }
    impl Drop for Application {
        fn drop(&mut self) {
            if app_dropped_is_last() {
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
        xl_typeinfo!(0xd0c9a001_0208_d500_a2d6_9f5c1e0b7a31);
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
                    // Application.Name — honest by default. Some interop clients
                    // gate behaviour on the literal string "Microsoft Excel"; an
                    // operator who needs that compatibility can opt in for their
                    // own session via the XLCOMSHIM_APP_NAME env var. We do NOT
                    // ship a default that claims to be Microsoft's product.
                    110 => put(result, VARIANT::from(app_name().as_str())),
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
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbooks
    // -----------------------------------------------------------------------

    #[implement(IWorkbooks, Agile = false)]
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
        xl_typeinfo!(0xd0c9a002_0208_db00_a2d6_9f5c1e0b7a31);
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
                    1923 => {
                        // Open(Filename, …) — load an existing .xlsx from disk.
                        let Some(path) = arg_string(params, 0) else {
                            log("Open: missing Filename");
                            return Err(DISP_E_BADINDEX.into());
                        };
                        match Book::open(&path) {
                            Some(b) => {
                                let book = reg(|r| {
                                    r.books.push(b);
                                    r.active = r.books.len() - 1;
                                    r.active
                                });
                                log(&format!("Open '{path}' -> book {book}"));
                                put_obj(result, Workbook { book });
                            }
                            None => {
                                log(&format!("Open '{path}' failed (missing or unparseable)"));
                                return Err(E_FAIL.into());
                            }
                        }
                    }
                    277 => {} // Close all — no-op
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Workbook
    // -----------------------------------------------------------------------

    #[implement(IWorkbook, Agile = false)]
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
        xl_typeinfo!(0xd0c9a003_0208_da00_a2d6_9f5c1e0b7a31);
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
                        // The OOXML formats (51-54) pick the file type; any other
                        // format falls back to the path's extension, else the
                        // loaded type (gridcore writes OOXML) rather than fault.
                        let kind = kind_for(fmt, &path);
                        let ok = reg(|r| {
                            r.books
                                .get_mut(book)
                                .map(|b| b.save_as(&path, kind))
                                .transpose()
                        });
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
                        // No path yet — no-op.
                        let res = reg(|r| r.books.get_mut(book).and_then(Book::save));
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
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Worksheets / Sheets (collection)
    // -----------------------------------------------------------------------

    #[implement(ISheets, Agile = false)]
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
        xl_typeinfo!(0xd0c9a004_0208_b100_a2d6_9f5c1e0b7a31);
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
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Worksheet
    // -----------------------------------------------------------------------

    #[implement(IWorksheet, Agile = false)]
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
        xl_typeinfo!(0xd0c9a005_0208_d800_a2d6_9f5c1e0b7a31);
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
                        // Cells or Cells(row, col). `ws.Cells(1, 1) = "x"` is a
                        // PROPERTYPUT with a NULL `result`, so handing back a
                        // Range object silently drops the write: assign into the
                        // cells instead. (`write_fill` refuses the unbounded
                        // whole-sheet form, so a bare `ws.Cells = "x"` is a
                        // logged no-op rather than a million writes.)
                        let np = n_pos_args(params);
                        let (r1, c1, r2, c2) = match (np >= 2)
                            .then(|| (arg_i32(params, 0), arg_i32(params, 1)))
                            .and_then(|(a, b)| a.zip(b))
                        {
                            Some((rr, cc)) => {
                                let r = (rr.max(1) - 1) as u32;
                                let c = (cc.max(1) - 1) as u32;
                                (r, c, r, c)
                            }
                            None => (0, 0, MAX_ROW, MAX_COL),
                        };
                        let rng = Range {
                            book,
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        };
                        if is_put(wflags) {
                            rng.write_fill(put_arg(params).ok_or(DISP_E_TYPEMISMATCH)?);
                        } else {
                            put_obj(result, rng);
                        }
                    }
                    197 => {
                        // Range("A1"[, "B2"]) or Range(cell1, cell2).
                        let np = n_pos_args(params);
                        let a = arg_string(params, 0).unwrap_or_default();
                        let cell2 = (np >= 2).then(|| arg_string(params, 1)).flatten();
                        let rect = if let Some(b) = cell2 {
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
                            // `ws.Range("A1") = "x"` puts through here too, with
                            // a NULL `result` — write rather than return an
                            // object nobody receives.
                            Some((r1, c1, r2, c2)) => {
                                let rng = Range {
                                    book,
                                    sheet,
                                    r1,
                                    c1,
                                    r2,
                                    c2,
                                };
                                if is_put(wflags) {
                                    rng.write_fill(put_arg(params).ok_or(DISP_E_TYPEMISMATCH)?);
                                } else {
                                    put_obj(result, rng);
                                }
                            }
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
                    _ => return unhandled(id, wflags, params, result),
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
    #[implement(IRange, Agile = false)]
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
            "clear" => 111,
            "clearcontents" => 113,
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
        xl_typeinfo!(0xd0c9a006_0002_0846_a2d6_9f5c1e0b7a31);
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
                            self.write_fill(put_of(Some(v)).ok_or(DISP_E_TYPEMISMATCH)?);
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
                            if let Some(v) = arg(params, 0) {
                                self.write_fill(put_of(Some(v)).ok_or(DISP_E_TYPEMISMATCH)?);
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
                    // Item / _Default(row, col) → sub-cell Range. A put
                    // (`rng(1, 1) = "x"`) gets a NULL `result`, so it has to
                    // write instead of returning the sub-range.
                    170 | 0 => {
                        let np = n_pos_args(params);
                        let idx = |i: u32| {
                            if i < np {
                                arg_i32(params, i).unwrap_or(1)
                            } else {
                                1
                            }
                        };
                        let rr = idx(0).max(1) as u32 - 1;
                        let cc = idx(1).max(1) as u32 - 1;
                        let r = r1 + rr;
                        let c = c1 + cc;
                        let sub = Range {
                            book,
                            sheet,
                            r1: r,
                            c1: c,
                            r2: r,
                            c2: c,
                        };
                        if is_put(wflags) {
                            sub.write_fill(put_arg(params).ok_or(DISP_E_TYPEMISMATCH)?);
                            return Ok(());
                        }
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
                                r.books.get(book).and_then(|b| {
                                    b.pkg.workbook.sheets.get(sheet).map(|s| s.col_width(c1))
                                })
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
                    // Count. `Worksheet.Cells`/`Rows`/`Columns` span the whole
                    // sheet (2^34 cells), so the product must be computed wide:
                    // in u32 it wraps to 0 in release and panics inside a vtable
                    // frame in debug, breaking `ws.Cells(ws.Rows.Count, 1)`.
                    118 => {
                        let n = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1);
                        put(result, VARIANT::from(n.min(i32::MAX as u64) as i32))
                    }
                    // Clear drops values and formats; ClearContents keeps
                    // the formats (Excel's DISPIDs, tools/comshim/excel-pia.txt).
                    111 => self.write_fill(Put::Clear),
                    113 => self.write_fill(Put::Value(CellValue::Empty)),
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
                            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
                            CellValue::Error(e) => e.clone(),
                        };
                        put(result, VARIANT::from(BSTR::from(s.as_str())));
                    }
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    impl Range {
        /// Assign `put` to every cell of the range (scalar fill), guarding
        /// against an unbounded whole-sheet range.
        fn write_fill(&self, put: Put) {
            let (book, sheet) = (self.book, self.sheet);
            let (r1, c1, r2, c2) = (self.r1, self.c1, self.r2, self.c2);
            let cells = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1);
            if cells > 1_000_000 {
                log(&format!("write_fill: refusing huge range ({cells} cells)"));
                return;
            }
            reg(|reg| {
                if let Some(b) = reg.books.get_mut(book) {
                    b.assign(sheet, (r1, c1, r2, c2), &put);
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
    fn apply_style(
        book: usize,
        sheet: usize,
        rect: (u32, u32, u32, u32),
        modify: impl Fn(&mut Xf),
    ) {
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
                let idx = wb
                    .sheets
                    .get(sheet)?
                    .cells
                    .get(&(r, c))
                    .map(|c| c.style)
                    .unwrap_or(0);
                wb.styles.xfs.get(idx as usize).cloned()
            })
        })
        .unwrap_or_default()
    }

    /// Excel's Color is a packed BGR long: R + G*256 + B*65536.
    fn excel_color(c: i32) -> (u8, u8, u8) {
        let c = c as u32;
        (
            (c & 0xFF) as u8,
            ((c >> 8) & 0xFF) as u8,
            ((c >> 16) & 0xFF) as u8,
        )
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

    #[implement(IFont, Agile = false)]
    struct Font {
        book: usize,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    }

    #[implement(IInterior, Agile = false)]
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
        xl_typeinfo!(0xd0c9a007_0002_084d_a2d6_9f5c1e0b7a31);
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
                            put(
                                result,
                                VARIANT::from(cell_xf(book, sheet, rect.0, rect.1).bold),
                            );
                        }
                    }
                    2 => {
                        if put_flag {
                            let on = arg_bool(params, 0, true);
                            apply_style(book, sheet, rect, move |xf| xf.italic = on);
                        } else {
                            put(
                                result,
                                VARIANT::from(cell_xf(book, sheet, rect.0, rect.1).italic),
                            );
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
                                VARIANT::from(
                                    cell_xf(book, sheet, rect.0, rect.1)
                                        .font_size
                                        .unwrap_or(11.0),
                                ),
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
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    impl IDispatch_Impl for Interior_Impl {
        xl_typeinfo!(0xd0c9a008_0002_0870_a2d6_9f5c1e0b7a31);
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
                    _ => return unhandled(id, wflags, params, result),
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

    #[implement(IDispatch, Agile = false)]
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
            params: *const DISPPARAMS,
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
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A Book whose cell (0, r, c) has the xf `xf`.
        fn formatted(r: u32, c: u32, xf: Xf) -> Book {
            let mut book = Book::new();
            let style = book.pkg.workbook.styles.intern(xf);
            book.set(
                0,
                r,
                c,
                Cell {
                    style,
                    ..Cell::default()
                },
            );
            book
        }

        fn code_xf(code: &str) -> Xf {
            let mut xf = Xf::default();
            xf.set_code(Some(code.to_string()));
            xf
        }

        fn cell(book: &Book, r: u32, c: u32) -> Cell {
            book.pkg.workbook.sheets[0]
                .cell(r, c)
                .cloned()
                .unwrap_or_default()
        }

        fn text(s: &str) -> Put {
            Put::Text(s.to_string())
        }

        #[test]
        fn a_string_put_converts_like_typed_entry() {
            let mut book = Book::new();
            book.assign(0, (0, 0, 0, 0), &text("42"));
            assert_eq!(cell(&book, 0, 0).value, CellValue::Number(42.0));
            book.assign(0, (1, 0, 1, 0), &text("1/15/2024"));
            let date = cell(&book, 1, 0);
            assert_eq!(date.value, CellValue::Number(45_306.0));
            let styles = &book.pkg.workbook.styles;
            assert_eq!(styles.xf(date.style).code.as_deref(), Some("m/d/yyyy"));
            book.assign(0, (2, 0, 2, 0), &text("$5"));
            let cash = cell(&book, 2, 0);
            assert_eq!(cash.value, CellValue::Number(5.0));
            let styles = &book.pkg.workbook.styles;
            assert!(styles.xf(cash.style).code.is_some_and(|c| c.contains('$')));
            book.assign(0, (3, 0, 3, 0), &text("'007"));
            let quoted = cell(&book, 3, 0);
            assert_eq!(quoted.value, CellValue::Text("007".into()));
            assert!(book.pkg.workbook.styles.xf(quoted.style).quote_prefix);
            book.assign(0, (4, 0, 4, 0), &text("=A1*2"));
            assert_eq!(cell(&book, 4, 0).formula.as_deref(), Some("A1*2"));
            assert_eq!(book.value(0, 4, 0), CellValue::Number(84.0));
        }

        #[test]
        fn a_string_put_into_a_text_cell_stays_text() {
            let mut book = formatted(0, 0, code_xf("@"));
            book.assign(0, (0, 0, 0, 0), &text("42"));
            let c = cell(&book, 0, 0);
            assert_eq!(c.value, CellValue::Text("42".into()));
            assert_eq!(
                book.pkg.workbook.styles.xf(c.style).code.as_deref(),
                Some("@")
            );
        }

        #[test]
        fn a_value_put_keeps_the_cells_format() {
            let xf = Xf {
                bold: true,
                ..code_xf("#,##0.00")
            };
            let mut book = formatted(0, 0, xf.clone());
            book.assign(0, (0, 0, 0, 0), &Put::Value(CellValue::Number(5.0)));
            let c = cell(&book, 0, 0);
            assert_eq!(c.value, CellValue::Number(5.0));
            assert_eq!(book.pkg.workbook.styles.xf(c.style), xf);
            // Emptying the contents keeps the format too; Clear drops it.
            book.assign(0, (0, 0, 0, 0), &Put::Value(CellValue::Empty));
            let c = cell(&book, 0, 0);
            assert_eq!(c.value, CellValue::Empty);
            assert_eq!(book.pkg.workbook.styles.xf(c.style), xf);
            book.assign(0, (0, 0, 0, 0), &Put::Clear);
            assert_eq!(cell(&book, 0, 0).style, 0);
            // By name: ClearContents is 113 and empties the value (the
            // format stays, above); Clear is 111 and resets the cell.
            assert_eq!(range_id("ClearContents"), Some(113));
            assert_eq!(range_id("Clear"), Some(111));
            // A number over quote-prefixed text is no longer quoted.
            let mut book = Book::new();
            book.assign(0, (0, 0, 0, 0), &text("'007"));
            book.assign(0, (0, 0, 0, 0), &Put::Value(CellValue::Number(7.0)));
            let c = cell(&book, 0, 0);
            assert!(!book.pkg.workbook.styles.xf(c.style).quote_prefix);
        }

        #[test]
        fn an_array_put_is_refused_not_read_as_nothing() {
            // VT_ARRAY | VT_VARIANT with no array behind it: only the tag is
            // read, and the VARIANT is never dropped (there is nothing to free).
            let mut v = std::mem::ManuallyDrop::new(VARIANT::default());
            unsafe { *(&mut *v as *mut VARIANT as *mut u16) = 0x2000 | 12 };
            assert_eq!(put_of(Some(&v)), None);
            assert_eq!(put_of(None), Some(Put::Value(CellValue::Empty)));
            let n = VARIANT::from(5.0);
            assert_eq!(put_of(Some(&n)), Some(Put::Value(CellValue::Number(5.0))));
            let s = VARIANT::from(BSTR::from("42"));
            assert_eq!(put_of(Some(&s)), Some(Put::Text("42".into())));
        }

        #[test]
        fn a_formula_put_on_a_range_moves_its_relative_references() {
            let mut book = Book::new();
            book.assign(0, (0, 1, 2, 1), &text("=A1"));
            let f = |r| cell(&book, r, 1).formula;
            assert_eq!(f(0).as_deref(), Some("A1"));
            assert_eq!(f(1).as_deref(), Some("A2"));
            assert_eq!(f(2).as_deref(), Some("A3"));
            // An absolute one stays put.
            book.assign(0, (0, 2, 1, 2), &text("=$A$1"));
            assert_eq!(cell(&book, 1, 2).formula.as_deref(), Some("$A$1"));
        }

        #[test]
        fn file_format_picks_the_kind_before_the_extension() {
            use SpreadsheetKind::*;
            assert_eq!(kind_for(Some(51), "x.xlsm"), Some(Workbook));
            assert_eq!(kind_for(Some(52), "x"), Some(MacroWorkbook));
            assert_eq!(kind_for(Some(53), "x.xlsx"), Some(MacroTemplate));
            assert_eq!(kind_for(Some(54), "x.xlsx"), Some(Template));
            assert_eq!(kind_for(None, "x.XLTX"), Some(Template));
            assert_eq!(kind_for(Some(6), "x.csv"), None);
            assert_eq!(kind_for(None, "x"), None);
        }

        /// A macro workbook with a VBA project, as Excel writes one.
        fn macro_pkg() -> SheetPackage {
            let mut pkg =
                load_xlsx(&save_xlsx_as(&new_xlsx(), SpreadsheetKind::MacroWorkbook)).unwrap();
            let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
                .replace(
                    "</Relationships>",
                    r#"<Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#,
                );
            pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
            pkg.set_part("xl/vbaProject.bin", b"VBA".to_vec());
            pkg
        }

        /// #601: `SaveAs "x.xlsx", 51` on a macro workbook writes a real
        /// `.xlsx`: no macro type, no VBA project.
        #[test]
        fn save_as_xlsx_drops_the_vba_project() {
            let mut book = Book::new();
            book.pkg = macro_pkg();
            let path =
                std::env::temp_dir().join(format!("xlcomshim-601-{}.xlsx", std::process::id()));
            let path = path.to_str().unwrap();
            book.save_as(path, kind_for(Some(51), path)).unwrap();
            let saved = load_xlsx(&std::fs::read(path).unwrap()).unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(!saved.has_vba_project());
            assert!(saved.part("xl/vbaProject.bin").is_none());
            let ct =
                String::from_utf8_lossy(saved.part("[Content_Types].xml").unwrap()).into_owned();
            assert!(ct.contains("spreadsheetml.sheet.main+xml"), "{ct}");
            assert!(!ct.contains("macroEnabled"), "{ct}");
        }

        /// #727: `SaveAs "x.xlsx", 51` writes no Excel 4.0 macro sheet, but
        /// the open workbook keeps it, so `Worksheets(i)` handles a client
        /// already holds still address the same sheets.
        #[test]
        fn save_as_xlsx_writes_no_macro_sheet_and_keeps_the_live_ones() {
            let mut pkg = new_xlsx();
            pkg.add_sheet("Macro1");
            let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
                .replace(
                    r#"Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml""#,
                    r#"Type="http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet" Target="worksheets/sheet2.xml""#,
                );
            pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
            let mut book = Book::new();
            book.pkg = load_xlsx(&save_xlsx_as(&pkg, SpreadsheetKind::MacroWorkbook)).unwrap();
            assert!(book.pkg.has_macro_sheets());
            let path =
                std::env::temp_dir().join(format!("xlcomshim-727-{}.xlsx", std::process::id()));
            let path = path.to_str().unwrap();
            book.save_as(path, kind_for(Some(51), path)).unwrap();
            let saved = load_xlsx(&std::fs::read(path).unwrap()).unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(!saved.has_macro_sheets());
            assert_eq!(saved.workbook.sheets.len(), 1);
            assert!(book.pkg.has_macro_sheets());
            assert_eq!(book.pkg.workbook.sheets.len(), 2, "the live sheet count");
            assert_eq!(book.sheet_name(1), "Macro1");
        }

        /// Save keeps the type the last SaveAs chose: `SaveAs "out", 51` then
        /// `Save` stays a macro-free workbook, and `SaveAs "x.xlsx", 52` then
        /// `Save` stays macro-enabled.
        #[test]
        fn save_keeps_the_type_chosen_at_save_as() {
            let dir =
                std::env::temp_dir().join(format!("xlcomshim-601-save-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let saved = |path: &std::path::Path| load_xlsx(&std::fs::read(path).unwrap()).unwrap();

            let mut book = Book::new();
            book.pkg = macro_pkg();
            let out = dir.join("out");
            let p = out.to_str().unwrap();
            book.save_as(p, kind_for(Some(51), p)).unwrap();
            book.save().unwrap().unwrap();
            let pkg = saved(&out);
            assert!(!pkg.has_vba_project());
            assert!(pkg.part("xl/vbaProject.bin").is_none());
            assert!(
                !book.pkg.has_vba_project(),
                "the open workbook dropped them too"
            );
            let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned();
            assert!(
                ct.contains("spreadsheetml.sheet.main+xml"),
                "Save kept fmt 51: {ct}"
            );
            assert!(!ct.contains("macroEnabled"), "{ct}");

            let mut book = Book::new();
            book.pkg = macro_pkg();
            let odd = dir.join("odd.xlsx");
            let p = odd.to_str().unwrap();
            book.save_as(p, kind_for(Some(52), p)).unwrap();
            book.save().unwrap().unwrap();
            assert!(saved(&odd).has_vba_project());

            // `SaveAs "r2"` with no FileFormat keeps the format used last.
            let mut book = Book::new();
            book.pkg = macro_pkg();
            let (r, r2) = (dir.join("r"), dir.join("r2"));
            let p = r.to_str().unwrap();
            book.save_as(p, kind_for(Some(51), p)).unwrap();
            let p = r2.to_str().unwrap();
            book.save_as(p, kind_for(None, p)).unwrap();
            let ct = String::from_utf8_lossy(saved(&r2).part("[Content_Types].xml").unwrap())
                .into_owned();
            assert!(ct.contains("spreadsheetml.sheet.main+xml"), "{ct}");
            assert!(!ct.contains("macroEnabled"), "{ct}");

            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
