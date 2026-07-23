//! wordcomshim — a COM server that impersonates `Word.Application`, so software
//! that automates Office over COM (create a document, type text, save a `.docx`)
//! keeps working on machines with no Microsoft Word installed. Document output is
//! produced by the dependency-free [`docxcore`] engine — the same one behind
//! `docxy`.
//!
//! This is the late-bound (`IDispatch`) create path:
//! `Application → Documents.Add → Document → Selection/Range`, text via
//! `Selection.TypeText`/`TypeParagraph` and `Range.Text`/`InsertAfter`, then
//! `Document.SaveAs2(path)` writing a real `.docx`. Every activation and dispatch
//! is logged to `%TEMP%\wordcomshim.log` — the field diagnostic. Unmodeled
//! members degrade gracefully (never fault). Registration is per-user via
//! `tools/wordshim/register-word.ps1`.

#[cfg(windows)]
pub use win::run;

#[cfg(windows)]
mod win {
    #![allow(non_snake_case)]

    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicI32, Ordering};

    use docxcore::model::{Align, Block, Document, Inline, Paragraph, ParProps, Run, RunProps};
    use docxcore::package::{Package, load_package, new_package, save_package};

    use windows::Win32::Foundation::{
        BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, DISP_E_BADINDEX, E_FAIL, E_NOTIMPL,
        E_POINTER, S_FALSE, S_OK,
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
    use windows::core::{
        BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR, Result, VARIANT, implement, interface,
    };
    use windows::Win32::System::Com::IDispatch_Vtbl;

    /// Our own coclass CLSID — a brand-new GUID, never Microsoft's Word CLSID.
    const SHIM_CLSID: GUID = GUID::from_u128(0x9c2f4a10_7d33_4b6e_b1a4_2e7c8d5f0a92);
    /// Microsoft Word's real coclass CLSID {000209FF-…}. We register a class
    /// object for it too so an early-bound `new Word.Application()` reaches us
    /// when the HKCU shadow points here. Never written to HKLM.
    const WORD_CLSID: GUID = GUID::from_u128(0x000209ff_0000_0000_c000_000000000046);

    const DISPID_UNKNOWN: i32 = -1;

    // ---- VARIANT type tags -------------------------------------------------
    const VT_EMPTY: u16 = 0;
    const VT_ERROR: u16 = 10;

    static APPS: AtomicI32 = AtomicI32::new(0);

    // -----------------------------------------------------------------------
    // Document state (thread-local; STA single-thread, no locking).
    // -----------------------------------------------------------------------

    struct DocState {
        pkg: Package,
        path: Option<String>,
        saved: bool,
        /// Current character format — new text (`TypeText`) is emitted with this,
        /// so the Word idiom `sel.Font.Bold = True : sel.TypeText "x"` works.
        cur: RunProps,
        /// Current paragraph alignment for new paragraphs.
        cur_align: Align,
    }

    impl DocState {
        fn new() -> DocState {
            DocState {
                pkg: new_package(Document::default()),
                path: None,
                saved: false,
                cur: RunProps::default(),
                cur_align: Align::Left,
            }
        }

        fn open(path: &str) -> std::io::Result<DocState> {
            let bytes = std::fs::read(path)?;
            let pkg = load_package(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e:?}")))?;
            Ok(DocState {
                pkg,
                path: Some(path.to_string()),
                saved: true,
                cur: RunProps::default(),
                cur_align: Align::Left,
            })
        }

        fn new_para(&self) -> Paragraph {
            Paragraph {
                props: ParProps {
                    align: self.cur_align,
                    ..ParProps::default()
                },
                content: vec![],
            }
        }
        fn cur_run(&self, text: &str) -> Inline {
            Inline::Run(Run {
                text: text.to_string(),
                props: self.cur.clone(),
            })
        }

        /// Type text at the end, honoring embedded paragraph marks (\r / \n), in
        /// the current character format.
        fn type_text(&mut self, s: &str) {
            if !matches!(self.pkg.document.body.last(), Some(Block::Paragraph(_))) {
                let p = self.new_para();
                self.pkg.document.body.push(Block::Paragraph(p));
            }
            for (i, seg) in split_paragraphs(s).into_iter().enumerate() {
                if i > 0 {
                    let p = self.new_para();
                    self.pkg.document.body.push(Block::Paragraph(p));
                }
                if !seg.is_empty() {
                    let run = self.cur_run(&seg);
                    if let Some(Block::Paragraph(p)) = self.pkg.document.body.last_mut() {
                        p.content.push(run);
                    }
                }
            }
            self.saved = false;
        }

        fn type_paragraph(&mut self) {
            let p = self.new_para();
            self.pkg.document.body.push(Block::Paragraph(p));
            self.saved = false;
        }

        /// Replace the whole body with paragraphs split from `s`, in the current
        /// character format.
        fn set_text(&mut self, s: &str) {
            let (align, props) = (self.cur_align, self.cur.clone());
            self.pkg.document.body = split_paragraphs(s)
                .into_iter()
                .map(|seg| {
                    Block::Paragraph(Paragraph {
                        props: ParProps {
                            align,
                            ..ParProps::default()
                        },
                        content: if seg.is_empty() {
                            vec![]
                        } else {
                            vec![Inline::Run(Run {
                                text: seg,
                                props: props.clone(),
                            })]
                        },
                    })
                })
                .collect();
            self.saved = false;
        }

        /// The document's text, paragraphs joined by Word's paragraph mark (\r).
        fn text(&self) -> String {
            self.pkg
                .document
                .body
                .iter()
                .map(|b| b.plain_text())
                .collect::<Vec<_>>()
                .join("\r")
        }

        fn save_as(&mut self, path: &str) -> std::io::Result<()> {
            let bytes = save_package(&self.pkg);
            std::fs::write(path, bytes)?;
            self.path = Some(path.to_string());
            self.saved = true;
            Ok(())
        }

        fn name(&self) -> String {
            self.path
                .as_deref()
                .and_then(|p| p.rsplit(['\\', '/']).next())
                .unwrap_or("Document1")
                .to_string()
        }
    }

    /// Split on paragraph marks (\r\n, \r, \n), keeping empties so blank lines
    /// become empty paragraphs.
    fn split_paragraphs(s: &str) -> Vec<String> {
        s.replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .map(|x| x.to_string())
            .collect()
    }

    struct Registry {
        docs: Vec<DocState>,
        active: usize,
        visible: bool,
    }

    thread_local! {
        static REG: RefCell<Registry> = const { RefCell::new(Registry {
            docs: Vec::new(),
            active: 0,
            visible: false,
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
            .open(format!("{dir}\\wordcomshim.log"))
        {
            let _ = writeln!(f, "[{}] {msg}", std::process::id());
        }
    }

    // -----------------------------------------------------------------------
    // Server lifecycle
    // -----------------------------------------------------------------------

    fn install_panic_hook() {
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| log(&format!("PANIC: {info}"))));
        });
    }

    pub fn run() -> ExitCode {
        install_panic_hook();
        let joined = std::env::args().collect::<Vec<_>>().join(" ").to_lowercase();
        if joined.contains("-embedding") || joined.contains("/automation") || joined.contains("--serve")
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
                "wordcomshim — Word-compatible COM automation server (LocalServer32).\n\
                 Register with tools/wordshim/register-word.ps1; COM launches it with -Embedding."
            );
            ExitCode::SUCCESS
        }
    }

    fn run_server() -> Result<()> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            log("server starting; registering class object");
            let factory: IClassFactory = WordClassFactory.into();
            let mut cookies = Vec::new();
            for clsid in [SHIM_CLSID, WORD_CLSID] {
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

    #[implement(IClassFactory)]
    struct WordClassFactory;

    impl IClassFactory_Impl for WordClassFactory_Impl {
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
                let app: IWordApp = Application::new().into();
                app.query(riid, ppvobject).ok()
            }
        }
        fn LockServer(&self, _flock: BOOL) -> Result<()> {
            Ok(())
        }
    }

    /// COM entry point for the in-process server (InprocServer32) — no
    /// marshalling, no typelib, the RCW calls our vtable directly.
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
            if clsid != SHIM_CLSID && clsid != WORD_CLSID {
                return CLASS_E_CLASSNOTAVAILABLE;
            }
            install_panic_hook();
            log(&format!("DllGetClassObject clsid={clsid:?}"));
            let factory: IClassFactory = WordClassFactory.into();
            factory.query(riid, ppv)
        }
    }

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

    unsafe fn vt_of(v: *const VARIANT) -> u16 {
        unsafe { *(v as *const u16) & 0x0fff }
    }

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

    unsafe fn put(pvarresult: *mut VARIANT, value: VARIANT) {
        if !pvarresult.is_null() {
            unsafe { std::ptr::write(pvarresult, value) };
        }
    }

    unsafe fn put_disp<T: IntoDisp>(pvarresult: *mut VARIANT, obj: T) {
        unsafe { put(pvarresult, VARIANT::from(obj.into_disp())) };
    }

    /// Each object now implements its Word dual interface (not bare IDispatch), so
    /// to hand it back as a VT_DISPATCH VARIANT we convert to that interface then
    /// QI down to IDispatch.
    trait IntoDisp {
        fn into_disp(self) -> IDispatch;
    }
    macro_rules! into_disp {
        ($struct:ty, $iface:ty) => {
            impl IntoDisp for $struct {
                fn into_disp(self) -> IDispatch {
                    let i: $iface = self.into();
                    i.cast().expect("interface derives IDispatch")
                }
            }
        };
    }
    /// For an object whose primary interface already IS IDispatch (Font etc.).
    macro_rules! into_disp_idispatch {
        ($struct:ty) => {
            impl IntoDisp for $struct {
                fn into_disp(self) -> IDispatch {
                    self.into()
                }
            }
        };
    }

    fn is_put(wflags: DISPATCH_FLAGS) -> bool {
        (wflags.0 & (DISPATCH_PROPERTYPUT.0 | DISPATCH_PROPERTYPUTREF.0)) != 0
    }

    // -----------------------------------------------------------------------
    // Graceful degradation — never fault on an unmodeled member.
    // -----------------------------------------------------------------------

    thread_local! {
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

    unsafe fn unhandled(id: i32, wflags: DISPATCH_FLAGS, result: *mut VARIANT) -> Result<()> {
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

    #[implement(IDispatch)]
    struct NullObject;

    fn null_dispatch() -> IDispatch {
        NullObject.into()
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
                    log(&format!("{who}: unmodeled member '{name}' -> graceful"));
                    ids[0] = synth_id(&name);
                }
            }
            Ok(())
        }
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
            unsafe { unhandled(id, wflags, result) }
        }
    }

    /// Boilerplate `GetIDsOfNames` for an object using `resolver`.
    macro_rules! dispatch_names {
        ($who:literal, $resolver:path) => {
            fn GetIDsOfNames(
                &self,
                _riid: *const GUID,
                rgsznames: *const PCWSTR,
                cnames: u32,
                _lcid: u32,
                rgdispid: *mut i32,
            ) -> Result<()> {
                unsafe { resolve_names($who, rgsznames, cnames, rgdispid, $resolver) }
            }
        };
    }

    // -----------------------------------------------------------------------
    // Word's dual interfaces: real IIDs, deriving IDispatch, every vtable slot in
    // Word's exact oVft order as an E_NOTIMPL stub. This makes an early-bound
    // .NET client's cast to Word._Application/_Document/etc. succeed and keeps any
    // vtable call landing on a real slot. Create-path members are layered on by
    // the vt_* handlers below; generated by tools/wordshim/gen-word-vtables.ps1.
    // (Word uses NO [lcid] params, so the signatures are just the real args.)
    // -----------------------------------------------------------------------
    include!("gen_word.rs");

    into_disp!(Documents, IDocuments);
    into_disp!(DocumentObj, IWordDoc);
    into_disp!(Selection, ISelection);
    into_disp!(Range, IWordRange);

    // ---- early-bound vtable handlers (shared with the IDispatch path) -------

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
    /// Read a [in] BSTR (a raw pointer in a register — never owned, so no drop).
    unsafe fn pcwstr(p: *const u16) -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { PCWSTR(p).to_string() }.unwrap_or_default()
        }
    }
    fn variant_i32(v: *const VARIANT) -> i32 {
        if v.is_null() {
            1
        } else {
            i32::try_from(unsafe { &*v }).unwrap_or(1)
        }
    }

    // Application
    unsafe fn vt_app_name(_t: &Application_Impl, ret: *mut BSTR) -> HRESULT {
        unsafe { out_bstr(ret, "Microsoft Word") }
    }
    unsafe fn vt_app_version(_t: &Application_Impl, ret: *mut BSTR) -> HRESULT {
        unsafe { out_bstr(ret, "16.0") }
    }
    unsafe fn vt_app_visible_get(_t: &Application_Impl, ret: *mut i16) -> HRESULT {
        unsafe { out_bool(ret, reg(|r| r.visible)) }
    }
    unsafe fn vt_app_visible_put(_t: &Application_Impl, v: i16) -> HRESULT {
        reg(|r| r.visible = v != 0);
        S_OK
    }
    unsafe fn vt_app_documents(_t: &Application_Impl, ret: *mut *mut c_void) -> HRESULT {
        let d: IDocuments = Documents.into();
        unsafe { out_iface(ret, d) }
    }
    unsafe fn vt_app_activedoc(_t: &Application_Impl, ret: *mut *mut c_void) -> HRESULT {
        let doc = reg(|r| r.active);
        let d: IWordDoc = DocumentObj { doc }.into();
        unsafe { out_iface(ret, d) }
    }
    unsafe fn vt_app_selection(_t: &Application_Impl, ret: *mut *mut c_void) -> HRESULT {
        let doc = reg(|r| r.active);
        let s: ISelection = Selection { doc }.into();
        unsafe { out_iface(ret, s) }
    }
    unsafe fn vt_app_quit(_t: &Application_Impl) -> HRESULT {
        log("Application::Quit (early)");
        unsafe { PostQuitMessage(0) };
        S_OK
    }

    // Documents
    unsafe fn vt_docs_count(_t: &Documents_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, reg(|r| r.docs.len() as i32)) }
    }
    unsafe fn vt_docs_add(_t: &Documents_Impl, ret: *mut *mut c_void) -> HRESULT {
        let doc = reg(|r| {
            r.docs.push(DocState::new());
            r.active = r.docs.len() - 1;
            r.active
        });
        let d: IWordDoc = DocumentObj { doc }.into();
        unsafe { out_iface(ret, d) }
    }
    unsafe fn vt_docs_item(_t: &Documents_Impl, index: *const VARIANT, ret: *mut *mut c_void) -> HRESULT {
        let idx = (variant_i32(index).max(1) as usize) - 1;
        if !reg(|r| idx < r.docs.len()) {
            return DISP_E_BADINDEX;
        }
        let d: IWordDoc = DocumentObj { doc: idx }.into();
        unsafe { out_iface(ret, d) }
    }

    // Document
    unsafe fn vt_doc_name(t: &DocumentObj_Impl, ret: *mut BSTR) -> HRESULT {
        let n = reg(|r| r.docs.get(t.doc).map(|d| d.name()).unwrap_or_default());
        unsafe { out_bstr(ret, &n) }
    }
    unsafe fn vt_doc_content(t: &DocumentObj_Impl, ret: *mut *mut c_void) -> HRESULT {
        let r: IWordRange = Range { doc: t.doc }.into();
        unsafe { out_iface(ret, r) }
    }
    unsafe fn vt_doc_range(t: &DocumentObj_Impl, ret: *mut *mut c_void) -> HRESULT {
        let r: IWordRange = Range { doc: t.doc }.into();
        unsafe { out_iface(ret, r) }
    }
    unsafe fn vt_doc_close(_t: &DocumentObj_Impl) -> HRESULT {
        S_OK
    }
    unsafe fn vt_doc_saveas(t: &DocumentObj_Impl, filename: *const VARIANT) -> HRESULT {
        let Some(path) = (unsafe { filename.as_ref() }).and_then(variant_to_string) else {
            log("SaveAs(early): missing FileName");
            return E_FAIL;
        };
        log(&format!("SaveAs(early) '{path}'"));
        let doc = t.doc;
        match reg(|r| r.docs.get_mut(doc).map(|d| d.save_as(&path))) {
            Some(Ok(())) => S_OK,
            _ => E_FAIL,
        }
    }

    // Selection
    unsafe fn vt_sel_text_get(t: &Selection_Impl, ret: *mut BSTR) -> HRESULT {
        let txt = reg(|r| r.docs.get(t.doc).map(|d| d.text()).unwrap_or_default());
        unsafe { out_bstr(ret, &txt) }
    }
    unsafe fn vt_sel_text_put(t: &Selection_Impl, v: *const u16) -> HRESULT {
        let (doc, s) = (t.doc, unsafe { pcwstr(v) });
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                d.set_text(&s)
            }
        });
        S_OK
    }
    unsafe fn vt_sel_range(t: &Selection_Impl, ret: *mut *mut c_void) -> HRESULT {
        let r: IWordRange = Range { doc: t.doc }.into();
        unsafe { out_iface(ret, r) }
    }
    unsafe fn vt_sel_typetext(t: &Selection_Impl, text: *const u16) -> HRESULT {
        let (doc, s) = (t.doc, unsafe { pcwstr(text) });
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                d.type_text(&s)
            }
        });
        S_OK
    }
    unsafe fn vt_sel_insertafter(t: &Selection_Impl, text: *const u16) -> HRESULT {
        unsafe { vt_sel_typetext(t, text) }
    }
    unsafe fn vt_sel_typepara(t: &Selection_Impl) -> HRESULT {
        let doc = t.doc;
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                d.type_paragraph()
            }
        });
        S_OK
    }

    // Range
    unsafe fn vt_rng_text_get(t: &Range_Impl, ret: *mut BSTR) -> HRESULT {
        let txt = reg(|r| r.docs.get(t.doc).map(|d| d.text()).unwrap_or_default());
        unsafe { out_bstr(ret, &txt) }
    }
    unsafe fn vt_rng_text_put(t: &Range_Impl, v: *const u16) -> HRESULT {
        let (doc, s) = (t.doc, unsafe { pcwstr(v) });
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                d.set_text(&s)
            }
        });
        S_OK
    }
    unsafe fn vt_rng_insertafter(t: &Range_Impl, text: *const u16) -> HRESULT {
        let (doc, s) = (t.doc, unsafe { pcwstr(text) });
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                d.type_text(&s)
            }
        });
        S_OK
    }
    unsafe fn vt_rng_insertpara(t: &Range_Impl) -> HRESULT {
        let doc = t.doc;
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                d.type_paragraph()
            }
        });
        S_OK
    }

    // -----------------------------------------------------------------------
    // Application
    // -----------------------------------------------------------------------

    #[implement(IWordApp)]
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
            "name" => 1,
            "version" => 2,
            "visible" => 3,
            "documents" => 4,
            "activedocument" => 5,
            "selection" => 6,
            "quit" => 7,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Application_Impl {
        no_typeinfo!();
        dispatch_names!("Application", app_id);
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
                log(&format!("Application::Invoke id={id} put={}", is_put(wflags)));
                match id {
                    1 => put(result, VARIANT::from("Microsoft Word")),
                    2 => put(result, VARIANT::from("16.0")),
                    3 => {
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
                    4 => put_disp(result, Documents),
                    5 => {
                        let doc = reg(|r| r.active);
                        put_disp(result, DocumentObj { doc });
                    }
                    6 => {
                        let doc = reg(|r| r.active);
                        put_disp(result, Selection { doc });
                    }
                    7 => {
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
    // Documents
    // -----------------------------------------------------------------------

    #[implement(IDocuments)]
    struct Documents;

    fn documents_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "add" => 1,
            "open" => 2,
            "item" | "_default" => 3,
            "count" => 4,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Documents_Impl {
        no_typeinfo!();
        dispatch_names!("Documents", documents_id);
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
                log(&format!("Documents::Invoke id={id}"));
                match id {
                    1 => {
                        let doc = reg(|r| {
                            r.docs.push(DocState::new());
                            r.active = r.docs.len() - 1;
                            r.active
                        });
                        put_disp(result, DocumentObj { doc });
                    }
                    2 => {
                        let Some(path) = arg_string(params, 0) else {
                            return Err(DISP_E_BADINDEX.into());
                        };
                        match DocState::open(&path) {
                            Ok(d) => {
                                let doc = reg(|r| {
                                    r.docs.push(d);
                                    r.active = r.docs.len() - 1;
                                    r.active
                                });
                                put_disp(result, DocumentObj { doc });
                            }
                            Err(e) => {
                                log(&format!("Documents.Open failed: {e}"));
                                return Err(windows::Win32::Foundation::E_FAIL.into());
                            }
                        }
                    }
                    3 => {
                        let idx = (arg_i32(params, 0).unwrap_or(1).max(1) as usize) - 1;
                        if !reg(|r| idx < r.docs.len()) {
                            return Err(DISP_E_BADINDEX.into());
                        }
                        put_disp(result, DocumentObj { doc: idx });
                    }
                    4 => put(result, VARIANT::from(reg(|r| r.docs.len() as i32))),
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Document
    // -----------------------------------------------------------------------

    #[implement(IWordDoc)]
    struct DocumentObj {
        doc: usize,
    }

    fn document_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "content" | "range" => 1,
            "saveas" | "saveas2" | "saveas2000" => 2,
            "save" => 3,
            "close" => 4,
            "name" => 5,
            "fullname" => 6,
            "path" => 7,
            "activate" | "select" => 8,
            "paragraphs" => 9,
            _ => return None,
        })
    }

    impl IDispatch_Impl for DocumentObj_Impl {
        no_typeinfo!();
        dispatch_names!("Document", document_id);
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
            let doc = self.doc;
            unsafe {
                log(&format!("Document[{doc}]::Invoke id={id} put={}", is_put(wflags)));
                match id {
                    1 => put_disp(result, Range { doc }),
                    2 => {
                        let Some(path) = arg_string(params, 0) else {
                            return Err(windows::Win32::Foundation::E_FAIL.into());
                        };
                        log(&format!("Document.SaveAs '{path}'"));
                        let res = reg(|r| r.docs.get_mut(doc).map(|d| d.save_as(&path)));
                        match res {
                            Some(Ok(())) => {}
                            _ => return Err(windows::Win32::Foundation::E_FAIL.into()),
                        }
                    }
                    3 => {
                        let res = reg(|r| {
                            r.docs.get_mut(doc).map(|d| {
                                d.path
                                    .clone()
                                    .map(|p| d.save_as(&p))
                                    .unwrap_or(Ok(()))
                            })
                        });
                        if !matches!(res, Some(Ok(()))) {
                            log("Document.Save: no path (needs SaveAs)");
                        }
                    }
                    4 => {} // Close — keep the slot so indices stay stable
                    5 => put(result, VARIANT::from(BSTR::from(reg(|r| {
                        r.docs.get(doc).map(|d| d.name()).unwrap_or_default()
                    }).as_str()))),
                    6 | 7 => put(result, VARIANT::from(BSTR::from(reg(|r| {
                        r.docs.get(doc).and_then(|d| d.path.clone()).unwrap_or_default()
                    }).as_str()))),
                    8 => {} // Activate / Select — no-op
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Selection (bound to a document; a cursor at the end of the body)
    // -----------------------------------------------------------------------

    #[implement(ISelection)]
    struct Selection {
        doc: usize,
    }

    fn selection_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "typetext" => 1,
            "typeparagraph" => 2,
            "text" => 3,
            "insertafter" => 4,
            "insertbefore" => 5,
            "range" => 6,
            "font" => 10,
            "bold" => 11,
            "italic" => 12,
            "underline" => 13,
            "paragraphformat" => 14,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Selection_Impl {
        no_typeinfo!();
        dispatch_names!("Selection", selection_id);
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
            let doc = self.doc;
            unsafe {
                log(&format!("Selection[{doc}]::Invoke id={id} put={}", is_put(wflags)));
                match id {
                    1 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.type_text(&s)
                                }
                            });
                        }
                    }
                    2 => reg(|r| {
                        if let Some(d) = r.docs.get_mut(doc) {
                            d.type_paragraph()
                        }
                    }),
                    3 => {
                        if is_put(wflags) {
                            let s = arg_string(params, 0).unwrap_or_default();
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.set_text(&s)
                                }
                            });
                        } else {
                            let t = reg(|r| r.docs.get(doc).map(|d| d.text()).unwrap_or_default());
                            put(result, VARIANT::from(BSTR::from(t.as_str())));
                        }
                    }
                    4 | 5 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.type_text(&s)
                                }
                            });
                        }
                    }
                    6 => put_disp(result, Range { doc }),
                    10 => put_disp(result, WordFont { doc, all: false }),
                    11 => return bool_prop(doc, false, wflags, params, result, |p, on| p.bold = on, |p| p.bold),
                    12 => return bool_prop(doc, false, wflags, params, result, |p, on| p.italic = on, |p| p.italic),
                    13 => return bool_prop(doc, false, wflags, params, result, |p, on| p.underline = on, |p| p.underline),
                    14 => put_disp(result, ParaFmt { doc, all: false }),
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Range (whole document, for the create path)
    // -----------------------------------------------------------------------

    #[implement(IWordRange)]
    struct Range {
        doc: usize,
    }

    fn range_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "text" => 1,
            "insertafter" => 2,
            "insertbefore" => 3,
            "insertparagraphafter" | "insertparagraph" => 4,
            "font" => 10,
            "bold" => 11,
            "italic" => 12,
            "underline" => 13,
            "paragraphformat" => 14,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Range_Impl {
        no_typeinfo!();
        dispatch_names!("Range", range_id);
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
            let doc = self.doc;
            unsafe {
                log(&format!("Range[{doc}]::Invoke id={id} put={}", is_put(wflags)));
                match id {
                    1 => {
                        if is_put(wflags) {
                            let s = arg_string(params, 0).unwrap_or_default();
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.set_text(&s)
                                }
                            });
                        } else {
                            let t = reg(|r| r.docs.get(doc).map(|d| d.text()).unwrap_or_default());
                            put(result, VARIANT::from(BSTR::from(t.as_str())));
                        }
                    }
                    2 | 3 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.type_text(&s)
                                }
                            });
                        }
                    }
                    4 => reg(|r| {
                        if let Some(d) = r.docs.get_mut(doc) {
                            d.type_paragraph()
                        }
                    }),
                    // Range.Font/Bold/etc. format the EXISTING text (all runs).
                    10 => put_disp(result, WordFont { doc, all: true }),
                    11 => return bool_prop(doc, true, wflags, params, result, |p, on| p.bold = on, |p| p.bold),
                    12 => return bool_prop(doc, true, wflags, params, result, |p, on| p.italic = on, |p| p.italic),
                    13 => return bool_prop(doc, true, wflags, params, result, |p, on| p.underline = on, |p| p.underline),
                    14 => put_disp(result, ParaFmt { doc, all: true }),
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Font / ParagraphFormat — real formatting over docxcore RunProps/ParProps.
    // `all=false` (from Selection) sets the current format that new text inherits;
    // `all=true` (from Range) applies to the existing text.
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct WordFont {
        doc: usize,
        all: bool,
    }
    #[implement(IDispatch)]
    struct ParaFmt {
        doc: usize,
        all: bool,
    }
    into_disp_idispatch!(WordFont);
    into_disp_idispatch!(ParaFmt);

    /// Apply a RunProps change to the current format (`all=false`) or to every run
    /// in the document (`all=true`).
    fn set_font(doc: usize, all: bool, f: impl Fn(&mut RunProps)) {
        reg(|r| {
            if let Some(d) = r.docs.get_mut(doc) {
                if all {
                    for b in &mut d.pkg.document.body {
                        if let Block::Paragraph(p) = b {
                            for inl in &mut p.content {
                                if let Inline::Run(run) = inl {
                                    f(&mut run.props);
                                }
                            }
                        }
                    }
                } else {
                    f(&mut d.cur);
                }
                d.saved = false;
            }
        });
    }

    unsafe fn bool_prop(
        doc: usize,
        all: bool,
        wflags: DISPATCH_FLAGS,
        params: *const DISPPARAMS,
        result: *mut VARIANT,
        set: impl Fn(&mut RunProps, bool),
        get: impl Fn(&RunProps) -> bool,
    ) -> Result<()> {
        if is_put(wflags) {
            let on = unsafe { arg_bool(params, 0, true) };
            set_font(doc, all, |p| set(p, on));
        } else {
            let v = reg(|r| r.docs.get(doc).map(|d| get(&d.cur)).unwrap_or(false));
            unsafe { put(result, VARIANT::from(v)) };
        }
        Ok(())
    }

    unsafe fn arg_bool(p: *const DISPPARAMS, i: u32, default: bool) -> bool {
        unsafe { arg(p, i).and_then(|v| bool::try_from(v).ok()).unwrap_or(default) }
    }

    /// Word's WdColor is a packed BGR long (RGB(r,g,b) = r + g*256 + b*65536);
    /// docxcore wants an uppercase RRGGBB hex. Negative = automatic -> None.
    fn word_color_hex(c: i32) -> Option<String> {
        if c < 0 {
            return None;
        }
        let c = c as u32;
        Some(format!(
            "{:02X}{:02X}{:02X}",
            c & 0xFF,
            (c >> 8) & 0xFF,
            (c >> 16) & 0xFF
        ))
    }

    fn font_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "bold" => 1,
            "italic" => 2,
            "underline" => 3,
            "size" => 4,
            "name" => 5,
            "color" => 6,
            _ => return None,
        })
    }

    impl IDispatch_Impl for WordFont_Impl {
        no_typeinfo!();
        dispatch_names!("Font", font_id);
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
            let (doc, all) = (self.doc, self.all);
            unsafe {
                log(&format!("Font[{doc}]::Invoke id={id} put={}", is_put(wflags)));
                match id {
                    1 => return bool_prop(doc, all, wflags, params, result, |p, on| p.bold = on, |p| p.bold),
                    2 => return bool_prop(doc, all, wflags, params, result, |p, on| p.italic = on, |p| p.italic),
                    3 => return bool_prop(doc, all, wflags, params, result, |p, on| p.underline = on, |p| p.underline),
                    4 => {
                        if is_put(wflags) {
                            if let Some(pt) = arg(params, 0).and_then(|v| f64::try_from(v).ok()) {
                                let hp = (pt * 2.0).round() as u32;
                                set_font(doc, all, |p| p.size_half_pts = Some(hp));
                            }
                        }
                    }
                    5 => {
                        if is_put(wflags) {
                            if let Some(nm) = arg_string(params, 0) {
                                set_font(doc, all, |p| p.font = Some(nm.clone()));
                            }
                        }
                    }
                    6 => {
                        if is_put(wflags) {
                            if let Some(hex) = arg_i32(params, 0).and_then(word_color_hex) {
                                set_font(doc, all, |p| p.color = Some(hex.clone()));
                            }
                        }
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }

    fn paraformat_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "alignment" => 1,
            _ => return None,
        })
    }

    impl IDispatch_Impl for ParaFmt_Impl {
        no_typeinfo!();
        dispatch_names!("ParagraphFormat", paraformat_id);
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
            let (doc, all) = (self.doc, self.all);
            unsafe {
                log(&format!("ParagraphFormat[{doc}]::Invoke id={id}"));
                match id {
                    // Alignment: WdParagraphAlignment 0=Left 1=Center 2=Right 3=Justify.
                    1 => {
                        if is_put(wflags) {
                            let a = match arg_i32(params, 0) {
                                Some(1) => Align::Center,
                                Some(2) => Align::Right,
                                Some(3) => Align::Justify,
                                _ => Align::Left,
                            };
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.cur_align = a;
                                    if all {
                                        for b in &mut d.pkg.document.body {
                                            if let Block::Paragraph(p) = b {
                                                p.props.align = a;
                                            }
                                        }
                                    }
                                    d.saved = false;
                                }
                            });
                        }
                    }
                    _ => return unhandled(id, wflags, result),
                }
                Ok(())
            }
        }
    }
}
