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

/// The dispinterfaces the shim serves + the mkwordtypelib bin authors — (name,
/// Word source IID, our docxy IID). Shared so the .tlb and the shim never drift.
#[cfg(windows)]
pub use win::DISP_IFACES;

#[cfg(windows)]
mod win {
    #![allow(non_snake_case)]

    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::process::ExitCode;

    // Shared COM scaffolding: VARIANT helpers (vt_of/arg/put/is_put/…), logging,
    // graceful degradation (unhandled/null_dispatch/synth_*), resolve_names, the
    // server + DLL plumbing, the Application counter, and the no_typeinfo! /
    // dispatch_names! macros.
    use comshimcore::*;

    use docxcore::model::{
        Align, Block, BreakKind, Cell, Document, Inline, Paragraph, ParProps, Row, Run, RunProps,
        Table, VMerge,
    };
    use docxcore::package::{Package, load_package, new_package, save_package};

    /// A plain single-line bordered `w:tblPr` for tables the shim creates (Word's
    /// `Tables.Add` makes a bordered table by default).
    const TBLPR: &str = "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>\
<w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
</w:tblBorders></w:tblPr>";

    use windows::Win32::Foundation::{DISP_E_BADINDEX, E_FAIL, E_NOTIMPL, E_POINTER, S_OK};
    use windows::Win32::System::Com::{
        DISPATCH_FLAGS, DISPPARAMS, EXCEPINFO, IDispatch, IDispatch_Impl, IDispatch_Vtbl,
    };
    use windows::Win32::UI::WindowsAndMessaging::PostQuitMessage;
    use windows::core::{
        BSTR, GUID, HRESULT, Interface, PCWSTR, Result, VARIANT, implement, interface,
    };

    /// OUR authored type library's LIBID (mkwordtypelib's `docxy_libid`). We
    /// source per-object typeinfo from our own registered docxy-word.tlb so it
    /// works on a machine with NO Word (the VDI).
    const DOCXY_LIBID: GUID = GUID::from_u128(0x9c2f4a11_7d33_4b6e_b1a4_2e7c8d5f0a92);

    /// The dispinterfaces we author + serve, as (name, Word source IID, our docxy
    /// IID). The mkwordtypelib bin copies each Word dispinterface (real memids +
    /// invkinds) into our .tlb under OUR IID; the shim's `GetTypeInfo` returns that
    /// IID's typeinfo so a typeinfo-driven late-bound client (pywin32) introspects
    /// each object correctly. Single source of truth for both. Names are prefixed
    /// `Docxy` to avoid colliding with the dual interfaces in the same typelib.
    pub const DISP_IFACES: &[(&str, u128, u128)] = &[
        ("DocxyWordApplication", 0x00020970_0000_0000_c000_000000000046, 0xd0c9b001_0002_0970_b1a4_2e7c8d5f0a92),
        ("DocxyDocuments", 0x0002096c_0000_0000_c000_000000000046, 0xd0c9b002_0002_096c_b1a4_2e7c8d5f0a92),
        ("DocxyDocument", 0x0002096b_0000_0000_c000_000000000046, 0xd0c9b003_0002_096b_b1a4_2e7c8d5f0a92),
        ("DocxySelection", 0x00020975_0000_0000_c000_000000000046, 0xd0c9b004_0002_0975_b1a4_2e7c8d5f0a92),
        ("DocxyWordRange", 0x0002095e_0000_0000_c000_000000000046, 0xd0c9b005_0002_095e_b1a4_2e7c8d5f0a92),
        ("DocxyWordFont", 0x00020952_0000_0000_c000_000000000046, 0xd0c9b006_0002_0952_b1a4_2e7c8d5f0a92),
        ("DocxyParagraphFormat", 0x00020953_0000_0000_c000_000000000046, 0xd0c9b007_0002_0953_b1a4_2e7c8d5f0a92),
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
    macro_rules! wd_typeinfo {
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

    /// Our own coclass CLSID — a brand-new GUID, never Microsoft's Word CLSID.
    const SHIM_CLSID: GUID = GUID::from_u128(0x9c2f4a10_7d33_4b6e_b1a4_2e7c8d5f0a92);
    /// Microsoft Word's real coclass CLSID {000209FF-…}. We register a class
    /// object for it too so an early-bound `new Word.Application()` reaches us
    /// when the HKCU shadow points here. Never written to HKLM.
    const WORD_CLSID: GUID = GUID::from_u128(0x000209ff_0000_0000_c000_000000000046);

    // -----------------------------------------------------------------------
    // Server / DLL plumbing — the generic runtime lives in comshimcore; the shim
    // supplies only its CLSIDs and the root Application constructor.
    // -----------------------------------------------------------------------

    /// Mint the root Application as an IDispatch; the factory QIs whatever
    /// interface the client asked for.
    fn make_app() -> IDispatch {
        let a: IWordApp = Application::new().into();
        a.cast().expect("Application derives IDispatch")
    }

    pub fn run() -> ExitCode {
        init("wordcomshim");
        if !should_serve() {
            eprintln!(
                "wordcomshim — Word-compatible COM automation server (LocalServer32).\n\
                 Register with tools/wordshim/register-word.ps1; COM launches it with -Embedding."
            );
            return ExitCode::SUCCESS;
        }
        match run_local_server(SHIM_CLSID, WORD_CLSID, make_app) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                log(&format!("server error: {e:?}"));
                ExitCode::FAILURE
            }
        }
    }

    /// In-process server exports (InprocServer32), forwarding to comshimcore.
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn DllGetClassObject(
        rclsid: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        unsafe { dll_get_class_object(SHIM_CLSID, WORD_CLSID, make_app, rclsid, riid, ppv) }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        dll_can_unload_now()
    }

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
        /// Current paragraph style id (`w:pStyle`) + heading level, from
        /// `Selection.Style`. Applied to new paragraphs.
        cur_style: Option<String>,
        cur_heading: Option<u8>,
        /// Current list numbering id (from `Range.ListFormat.Apply…`), applied to
        /// new paragraphs until `RemoveNumbers`.
        cur_list: Option<i32>,
    }

    impl DocState {
        fn new() -> DocState {
            DocState {
                pkg: new_package(Document::default()),
                path: None,
                saved: false,
                cur: RunProps::default(),
                cur_align: Align::Left,
                cur_style: None,
                cur_heading: None,
                cur_list: None,
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
                cur_style: None,
                cur_heading: None,
                cur_list: None,
            })
        }

        fn new_para(&self) -> Paragraph {
            Paragraph {
                props: ParProps {
                    align: self.cur_align,
                    style_id: self.cur_style.clone(),
                    heading_level: self.cur_heading,
                    num_id: self.cur_list,
                    ilvl: 0,
                    ..ParProps::default()
                },
                content: vec![],
            }
        }

        /// Set the current paragraph style from a name ("Heading 1", "Normal").
        /// For a heading we DEFINE the built-in style in styles.xml (via
        /// `ensure_styles`), so Word shows it semantically as "Heading N" with
        /// real heading formatting — not just direct formatting.
        fn set_style(&mut self, style: &str) {
            let low = style.trim().to_ascii_lowercase();
            let hn = low
                .strip_prefix("heading")
                .map(str::trim)
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|n| (1..=9).contains(n));
            if let Some(n) = hn {
                let id = format!("Heading{}", n.min(6));
                self.pkg.ensure_styles(&[id.as_str()]);
                self.cur_style = Some(id);
                self.cur_heading = Some(n);
                self.cur.bold = false;
                self.cur.size_half_pts = None;
            } else if low == "normal" || low.is_empty() {
                self.cur_style = None;
                self.cur_heading = None;
                self.cur.bold = false;
                self.cur.size_half_pts = None;
            } else {
                let id = style.replace(' ', "");
                self.pkg.ensure_styles(&[id.as_str()]);
                self.cur_style = Some(id);
                self.cur_heading = None;
            }
            // Word applies the style to the CURRENT paragraph; if it is still
            // empty (freshly started), retag it so `Style=x : TypeText` styles
            // this paragraph, not just the next one.
            let (style_id, heading) = (self.cur_style.clone(), self.cur_heading);
            if let Some(Block::Paragraph(p)) = self.pkg.document.body.last_mut() {
                if p.content.is_empty() {
                    p.props.style_id = style_id;
                    p.props.heading_level = heading;
                }
            }
        }

        /// Apply (Some) or clear (None) a bullet/numbered list to new paragraphs,
        /// also marking the current (last) paragraph — the "apply list then type
        /// items" idiom.
        fn apply_list(&mut self, bullet: Option<bool>) {
            match bullet {
                Some(b) => {
                    let id = self.pkg.ensure_list(b);
                    self.cur_list = Some(id);
                    if let Some(Block::Paragraph(p)) = self.pkg.document.body.last_mut() {
                        p.props.num_id = Some(id);
                        p.props.ilvl = 0;
                    }
                }
                None => self.cur_list = None,
            }
            self.saved = false;
        }

        /// `Selection.Style = wdStyleHeadingN` (-2..-10) / wdStyleNormal (-1).
        fn set_style_wd(&mut self, wd: i32) {
            match wd {
                -10..=-2 => self.set_style(&format!("Heading {}", -wd - 1)),
                -1 => self.set_style("Normal"),
                _ => {}
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

        /// Insert a page/column/line break inline at the end.
        fn insert_break(&mut self, kind: BreakKind) {
            if !matches!(self.pkg.document.body.last(), Some(Block::Paragraph(_))) {
                let p = self.new_para();
                self.pkg.document.body.push(Block::Paragraph(p));
            }
            if let Some(Block::Paragraph(p)) = self.pkg.document.body.last_mut() {
                p.content.push(Inline::Break(kind));
            }
            self.saved = false;
        }

        /// Replace the whole body with paragraphs split from `s`, in the current
        /// character format.
        fn set_text(&mut self, s: &str) {
            let (align, props) = (self.cur_align, self.cur.clone());
            let (style, heading) = (self.cur_style.clone(), self.cur_heading);
            self.pkg.document.body = split_paragraphs(s)
                .into_iter()
                .map(|seg| {
                    Block::Paragraph(Paragraph {
                        props: ParProps {
                            align,
                            style_id: style.clone(),
                            heading_level: heading,
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

        // ---- tables ----

        fn table_count(&self) -> usize {
            self.pkg
                .document
                .body
                .iter()
                .filter(|b| matches!(b, Block::Table(_)))
                .count()
        }

        /// Append a rows×cols bordered table; returns its 1-based table number.
        fn add_table(&mut self, rows: usize, cols: usize) -> usize {
            let cols = cols.max(1);
            let cell = || Cell {
                grid_span: 1,
                v_merge: VMerge::default(),
                blocks: vec![Block::Paragraph(Paragraph::default())],
                raw_tcpr: None,
            };
            let table = Table {
                grid: vec![0; cols],
                rows: (0..rows.max(1))
                    .map(|_| Row {
                        cells: (0..cols).map(|_| cell()).collect(),
                        raw_props: vec![],
                    })
                    .collect(),
                raw_tblpr: Some(TBLPR.to_string()),
            };
            self.pkg.document.body.push(Block::Table(table));
            self.saved = false;
            self.table_count()
        }

        /// The 1-based Nth table's block.
        fn table_mut(&mut self, tno: usize) -> Option<&mut Table> {
            self.pkg
                .document
                .body
                .iter_mut()
                .filter_map(|b| match b {
                    Block::Table(t) => Some(t),
                    _ => None,
                })
                .nth(tno.saturating_sub(1))
        }

        /// Set a cell's text (1-based row/col) in the current character format.
        fn set_cell_text(&mut self, tno: usize, row: usize, col: usize, text: &str) {
            let props = self.cur.clone();
            if let Some(t) = self.table_mut(tno) {
                if let Some(c) = t
                    .rows
                    .get_mut(row.saturating_sub(1))
                    .and_then(|r| r.cells.get_mut(col.saturating_sub(1)))
                {
                    c.blocks = vec![Block::Paragraph(Paragraph {
                        props: ParProps::default(),
                        content: vec![Inline::Run(Run {
                            text: text.to_string(),
                            props,
                        })],
                    })];
                }
            }
            self.saved = false;
        }

        fn cell_text(&self, tno: usize, row: usize, col: usize) -> String {
            self.pkg
                .document
                .body
                .iter()
                .filter_map(|b| match b {
                    Block::Table(t) => Some(t),
                    _ => None,
                })
                .nth(tno.saturating_sub(1))
                .and_then(|t| t.rows.get(row.saturating_sub(1)))
                .and_then(|r| r.cells.get(col.saturating_sub(1)))
                .map(|c| {
                    c.blocks
                        .iter()
                        .map(|b| b.plain_text())
                        .collect::<Vec<_>>()
                        .join("\r")
                })
                .unwrap_or_default()
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
    /// For objects whose primary interface already IS IDispatch (the collection /
    /// leaf helpers that a client only ever uses late-bound).
    macro_rules! into_disp_idispatch {
        ($($struct:ty),+ $(,)?) => {
            $(impl IntoDisp for $struct {
                fn into_disp(self) -> IDispatch {
                    self.into()
                }
            })+
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

    // Early-bound formatting: Font/ParagraphFormat objects (IDispatch, used
    // late-bound by the client after the vtable returns them) + Range's direct
    // Bold/Italic/Underline (I4 in Word: -1/0).
    unsafe fn vt_sel_font(t: &Selection_Impl, ret: *mut *mut c_void) -> HRESULT {
        let f: IFont = WordFont { doc: t.doc, all: false }.into();
        unsafe { out_iface(ret, f) }
    }
    unsafe fn vt_sel_paraformat(t: &Selection_Impl, ret: *mut *mut c_void) -> HRESULT {
        let f: IParaFmt = ParaFmt { doc: t.doc, all: false }.into();
        unsafe { out_iface(ret, f) }
    }
    unsafe fn vt_rng_font(t: &Range_Impl, ret: *mut *mut c_void) -> HRESULT {
        let f: IFont = WordFont { doc: t.doc, all: true }.into();
        unsafe { out_iface(ret, f) }
    }
    unsafe fn vt_rng_paraformat(t: &Range_Impl, ret: *mut *mut c_void) -> HRESULT {
        let f: IParaFmt = ParaFmt { doc: t.doc, all: true }.into();
        unsafe { out_iface(ret, f) }
    }
    unsafe fn vt_rng_bold_get(t: &Range_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, if reg(|r| r.docs.get(t.doc).map(|d| d.cur.bold).unwrap_or(false)) { -1 } else { 0 }) }
    }
    unsafe fn vt_rng_bold_put(t: &Range_Impl, v: i32) -> HRESULT {
        set_font(t.doc, true, |p| p.bold = v != 0);
        S_OK
    }
    unsafe fn vt_rng_italic_get(t: &Range_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, if reg(|r| r.docs.get(t.doc).map(|d| d.cur.italic).unwrap_or(false)) { -1 } else { 0 }) }
    }
    unsafe fn vt_rng_italic_put(t: &Range_Impl, v: i32) -> HRESULT {
        set_font(t.doc, true, |p| p.italic = v != 0);
        S_OK
    }
    unsafe fn vt_rng_underline_get(t: &Range_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, i32::from(reg(|r| r.docs.get(t.doc).map(|d| d.cur.underline).unwrap_or(false)))) }
    }
    unsafe fn vt_rng_underline_put(t: &Range_Impl, v: i32) -> HRESULT {
        set_font(t.doc, true, |p| p.underline = v != 0);
        S_OK
    }

    unsafe fn out_r4(ret: *mut f32, v: f32) -> HRESULT {
        if ret.is_null() {
            return E_POINTER;
        }
        unsafe { *ret = v };
        S_OK
    }
    fn align_to_wd(a: Align) -> i32 {
        match a {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
            Align::Justify => 3,
        }
    }
    fn wd_to_align(v: i32) -> Align {
        match v {
            1 => Align::Center,
            2 => Align::Right,
            3 => Align::Justify,
            _ => Align::Left,
        }
    }

    // _Font (dual): the create-path character-format members.
    unsafe fn vt_font_bold_get(t: &WordFont_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, if reg(|r| r.docs.get(t.doc).map(|d| d.cur.bold).unwrap_or(false)) { -1 } else { 0 }) }
    }
    unsafe fn vt_font_bold_put(t: &WordFont_Impl, v: i32) -> HRESULT {
        set_font(t.doc, t.all, |p| p.bold = v != 0);
        S_OK
    }
    unsafe fn vt_font_italic_get(t: &WordFont_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, if reg(|r| r.docs.get(t.doc).map(|d| d.cur.italic).unwrap_or(false)) { -1 } else { 0 }) }
    }
    unsafe fn vt_font_italic_put(t: &WordFont_Impl, v: i32) -> HRESULT {
        set_font(t.doc, t.all, |p| p.italic = v != 0);
        S_OK
    }
    unsafe fn vt_font_underline_get(t: &WordFont_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, i32::from(reg(|r| r.docs.get(t.doc).map(|d| d.cur.underline).unwrap_or(false)))) }
    }
    unsafe fn vt_font_underline_put(t: &WordFont_Impl, v: i32) -> HRESULT {
        set_font(t.doc, t.all, |p| p.underline = v != 0);
        S_OK
    }
    unsafe fn vt_font_size_get(t: &WordFont_Impl, ret: *mut f32) -> HRESULT {
        let pt = reg(|r| r.docs.get(t.doc).and_then(|d| d.cur.size_half_pts))
            .map(|h| h as f32 / 2.0)
            .unwrap_or(11.0);
        unsafe { out_r4(ret, pt) }
    }
    unsafe fn vt_font_size_put(t: &WordFont_Impl, v: f32) -> HRESULT {
        let hp = (v * 2.0).round() as u32;
        set_font(t.doc, t.all, |p| p.size_half_pts = Some(hp));
        S_OK
    }
    unsafe fn vt_font_name_get(t: &WordFont_Impl, ret: *mut BSTR) -> HRESULT {
        let n = reg(|r| r.docs.get(t.doc).and_then(|d| d.cur.font.clone()))
            .unwrap_or_else(|| "Calibri".into());
        unsafe { out_bstr(ret, &n) }
    }
    unsafe fn vt_font_name_put(t: &WordFont_Impl, v: *const u16) -> HRESULT {
        let n = unsafe { pcwstr(v) };
        set_font(t.doc, t.all, |p| p.font = Some(n.clone()));
        S_OK
    }
    unsafe fn vt_font_color_get(_t: &WordFont_Impl, ret: *mut i32) -> HRESULT {
        unsafe { out_i4(ret, 0) } // wdColorAutomatic-ish
    }
    unsafe fn vt_font_color_put(t: &WordFont_Impl, v: i32) -> HRESULT {
        if let Some(hex) = word_color_hex(v) {
            set_font(t.doc, t.all, |p| p.color = Some(hex.clone()));
        }
        S_OK
    }

    // _ParagraphFormat (dual): Alignment.
    unsafe fn vt_para_align_get(t: &ParaFmt_Impl, ret: *mut i32) -> HRESULT {
        let a = reg(|r| r.docs.get(t.doc).map(|d| d.cur_align).unwrap_or(Align::Left));
        unsafe { out_i4(ret, align_to_wd(a)) }
    }
    unsafe fn vt_para_align_put(t: &ParaFmt_Impl, v: i32) -> HRESULT {
        let (a, doc, all) = (wd_to_align(v), t.doc, t.all);
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
        S_OK
    }

    // -----------------------------------------------------------------------
    // Application
    // -----------------------------------------------------------------------

    #[implement(IWordApp)]
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

    // Word's REAL _Application dispids (so a typeinfo-driven client like pywin32,
    // which reads our authored dispinterface, calls the same ids our Invoke serves).
    fn app_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "name" => 0,
            "version" => 24,
            "visible" => 23,
            "documents" => 6,
            "activedocument" => 3,
            "selection" => 5,
            "quit" => 1105,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Application_Impl {
        wd_typeinfo!(0xd0c9b001_0002_0970_b1a4_2e7c8d5f0a92);
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
                    0 => put(result, VARIANT::from("Microsoft Word")),
                    24 => put(result, VARIANT::from("16.0")),
                    23 => {
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
                    6 => put_disp(result, Documents),
                    3 => {
                        let doc = reg(|r| r.active);
                        put_disp(result, DocumentObj { doc });
                    }
                    5 => {
                        let doc = reg(|r| r.active);
                        put_disp(result, Selection { doc });
                    }
                    1105 => {
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
    // Documents
    // -----------------------------------------------------------------------

    #[implement(IDocuments)]
    struct Documents;

    // Word's REAL Documents dispids.
    fn documents_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "add" => 14,
            "open" => 19,
            "item" | "_default" => 0,
            "count" => 2,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Documents_Impl {
        wd_typeinfo!(0xd0c9b002_0002_096c_b1a4_2e7c8d5f0a92);
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
                    14 => {
                        let doc = reg(|r| {
                            r.docs.push(DocState::new());
                            r.active = r.docs.len() - 1;
                            r.active
                        });
                        put_disp(result, DocumentObj { doc });
                    }
                    19 => {
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
                    0 => {
                        let idx = (arg_i32(params, 0).unwrap_or(1).max(1) as usize) - 1;
                        if !reg(|r| idx < r.docs.len()) {
                            return Err(DISP_E_BADINDEX.into());
                        }
                        put_disp(result, DocumentObj { doc: idx });
                    }
                    2 => put(result, VARIANT::from(reg(|r| r.docs.len() as i32))),
                    _ => return unhandled(id, wflags, params, result),
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

    // Word's REAL _Document dispids.
    fn document_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "content" => 41,
            "range" => 2000,
            "saveas" => 376,
            "saveas2" => 568,
            "saveas2000" => 102,
            "save" => 108,
            "close" => 1105,
            "name" => 0,
            "fullname" => 29,
            "path" => 3,
            "activate" => 113,
            "select" => 65535,
            "paragraphs" => 16,
            "tables" => 6,
            _ => return None,
        })
    }

    impl IDispatch_Impl for DocumentObj_Impl {
        wd_typeinfo!(0xd0c9b003_0002_096b_b1a4_2e7c8d5f0a92);
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
                    41 | 2000 => put_disp(result, Range { doc }), // Content / Range
                    // SaveAs / SaveAs2 / SaveAs2000 — filename in arg 0.
                    102 | 376 | 568 => {
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
                    108 => {
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
                    1105 => {} // Close — keep the slot so indices stay stable
                    0 => put(result, VARIANT::from(BSTR::from(reg(|r| {
                        r.docs.get(doc).map(|d| d.name()).unwrap_or_default()
                    }).as_str()))),
                    3 | 29 => put(result, VARIANT::from(BSTR::from(reg(|r| {
                        r.docs.get(doc).and_then(|d| d.path.clone()).unwrap_or_default()
                    }).as_str()))), // Path / FullName
                    113 | 65535 => {} // Activate / Select — no-op
                    6 => put_disp(result, Tables { doc }),
                    _ => return unhandled(id, wflags, params, result),
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

    // Word's REAL Selection dispids. Bold/Italic/Underline are NOT direct Selection
    // members in Word (they live on Font); we expose them as a convenience, so they
    // get high ids that can't collide with a real dispid the typeinfo might carry.
    fn selection_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "typetext" => 507,
            "typeparagraph" => 512,
            "text" => 0,
            "insertafter" => 104,
            "insertbefore" => 102,
            "range" => 400,
            "font" => 5,
            "bold" => 0x6001,
            "italic" => 0x6002,
            "underline" => 0x6003,
            "paragraphformat" => 1102,
            "style" => 8,
            "insertbreak" => 122,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Selection_Impl {
        wd_typeinfo!(0xd0c9b004_0002_0975_b1a4_2e7c8d5f0a92);
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
                    507 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.type_text(&s)
                                }
                            });
                        }
                    }
                    512 => reg(|r| {
                        if let Some(d) = r.docs.get_mut(doc) {
                            d.type_paragraph()
                        }
                    }),
                    0 => {
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
                    104 | 102 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.type_text(&s)
                                }
                            });
                        }
                    }
                    400 => put_disp(result, Range { doc }),
                    5 => put_disp(result, WordFont { doc, all: false }),
                    0x6001 => return bool_prop(doc, false, wflags, params, result, |p, on| p.bold = on, |p| p.bold),
                    0x6002 => return bool_prop(doc, false, wflags, params, result, |p, on| p.italic = on, |p| p.italic),
                    0x6003 => return bool_prop(doc, false, wflags, params, result, |p, on| p.underline = on, |p| p.underline),
                    1102 => put_disp(result, ParaFmt { doc, all: false }),
                    // InsertBreak([Type]) — wdPageBreak(7)/Column(8)/Line(6);
                    // section breaks (0..3) fall back to a page break.
                    122 => {
                        let kind = match arg_i32(params, 0) {
                            Some(8) => BreakKind::Column,
                            Some(6) => BreakKind::Line,
                            _ => BreakKind::Page,
                        };
                        reg(|r| {
                            if let Some(d) = r.docs.get_mut(doc) {
                                d.insert_break(kind)
                            }
                        });
                    }
                    // Style — a style name ("Heading 1") or a wdStyle int.
                    8 => {
                        if is_put(wflags) {
                            if let Some(v) = arg(params, 0) {
                                if vt_of(v) == VT_BSTR {
                                    let s = variant_to_string(v).unwrap_or_default();
                                    reg(|r| {
                                        if let Some(d) = r.docs.get_mut(doc) {
                                            d.set_style(&s)
                                        }
                                    });
                                } else if let Ok(n) = i32::try_from(v) {
                                    reg(|r| {
                                        if let Some(d) = r.docs.get_mut(doc) {
                                            d.set_style_wd(n)
                                        }
                                    });
                                }
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
    // Range (whole document, for the create path)
    // -----------------------------------------------------------------------

    #[implement(IWordRange)]
    struct Range {
        doc: usize,
    }

    // Word's REAL Range dispids.
    fn range_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "text" => 0,
            "insertafter" => 104,
            "insertbefore" => 102,
            "insertparagraphafter" => 161,
            "insertparagraph" => 160,
            "font" => 5,
            "bold" => 130,
            "italic" => 131,
            "underline" => 139,
            "paragraphformat" => 1102,
            "listformat" => 68,
            _ => return None,
        })
    }

    impl IDispatch_Impl for Range_Impl {
        wd_typeinfo!(0xd0c9b005_0002_095e_b1a4_2e7c8d5f0a92);
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
                    0 => {
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
                    104 | 102 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.type_text(&s)
                                }
                            });
                        }
                    }
                    160 | 161 => reg(|r| {
                        if let Some(d) = r.docs.get_mut(doc) {
                            d.type_paragraph()
                        }
                    }),
                    // Range.Font/Bold/etc. format the EXISTING text (all runs).
                    5 => put_disp(result, WordFont { doc, all: true }),
                    130 => return bool_prop(doc, true, wflags, params, result, |p, on| p.bold = on, |p| p.bold),
                    131 => return bool_prop(doc, true, wflags, params, result, |p, on| p.italic = on, |p| p.italic),
                    139 => return bool_prop(doc, true, wflags, params, result, |p, on| p.underline = on, |p| p.underline),
                    1102 => put_disp(result, ParaFmt { doc, all: true }),
                    68 => put_disp(result, ListFormat { doc }),
                    _ => return unhandled(id, wflags, params, result),
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

    // Word's Font / ParagraphFormat are DUAL interfaces (_Font {00020952},
    // _ParagraphFormat {00020953}), so an early-bound client casts to them — we
    // implement the duals (keeping IDispatch for late-bound), same as the other
    // objects. The create-path members forward to vt_font_*/vt_para_* below.
    #[implement(IFont)]
    struct WordFont {
        doc: usize,
        all: bool,
    }
    #[implement(IParaFmt)]
    struct ParaFmt {
        doc: usize,
        all: bool,
    }
    into_disp!(WordFont, IFont);
    into_disp!(ParaFmt, IParaFmt);

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

    // Word's REAL _Font dispids.
    fn font_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "bold" => 130,
            "italic" => 131,
            "underline" => 140,
            "size" => 141,
            "name" => 142,
            "color" => 159,
            _ => return None,
        })
    }

    impl IDispatch_Impl for WordFont_Impl {
        wd_typeinfo!(0xd0c9b006_0002_0952_b1a4_2e7c8d5f0a92);
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
                    130 => return bool_prop(doc, all, wflags, params, result, |p, on| p.bold = on, |p| p.bold),
                    131 => return bool_prop(doc, all, wflags, params, result, |p, on| p.italic = on, |p| p.italic),
                    140 => return bool_prop(doc, all, wflags, params, result, |p, on| p.underline = on, |p| p.underline),
                    141 => {
                        if is_put(wflags) {
                            if let Some(pt) = arg(params, 0).and_then(|v| f64::try_from(v).ok()) {
                                let hp = (pt * 2.0).round() as u32;
                                set_font(doc, all, |p| p.size_half_pts = Some(hp));
                            }
                        }
                    }
                    142 => {
                        if is_put(wflags) {
                            if let Some(nm) = arg_string(params, 0) {
                                set_font(doc, all, |p| p.font = Some(nm.clone()));
                            }
                        }
                    }
                    159 => {
                        if is_put(wflags) {
                            if let Some(hex) = arg_i32(params, 0).and_then(word_color_hex) {
                                set_font(doc, all, |p| p.color = Some(hex.clone()));
                            }
                        }
                    }
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    // Word's REAL _ParagraphFormat dispids.
    fn paraformat_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "alignment" => 101,
            _ => return None,
        })
    }

    impl IDispatch_Impl for ParaFmt_Impl {
        wd_typeinfo!(0xd0c9b007_0002_0953_b1a4_2e7c8d5f0a92);
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
                    101 => {
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
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Tables: Document.Tables.Add(range, rows, cols) -> Table.Cell(r,c).Range.Text
    // Late-bound (IDispatch); collections/leaves a report generator walks.
    // -----------------------------------------------------------------------

    #[implement(IDispatch)]
    struct Tables {
        doc: usize,
    }
    #[implement(IDispatch)]
    struct WordTable {
        doc: usize,
        tno: usize,
    }
    #[implement(IDispatch)]
    struct WordCell {
        doc: usize,
        tno: usize,
        row: usize,
        col: usize,
    }
    #[implement(IDispatch)]
    struct CellRange {
        doc: usize,
        tno: usize,
        row: usize,
        col: usize,
    }
    into_disp_idispatch!(Tables, WordTable, WordCell, CellRange, ListFormat);

    #[implement(IDispatch)]
    struct ListFormat {
        doc: usize,
    }
    fn listformat_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "applybulletdefault" => 1,
            "applynumberdefault" => 2,
            "removenumbers" => 3,
            _ => return None,
        })
    }
    impl IDispatch_Impl for ListFormat_Impl {
        no_typeinfo!();
        dispatch_names!("ListFormat", listformat_id);
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
                log(&format!("ListFormat[{doc}]::Invoke id={id}"));
                let bullet = match id {
                    1 => Some(true),
                    2 => Some(false),
                    3 => None,
                    _ => return unhandled(id, wflags, params, result),
                };
                reg(|r| {
                    if let Some(d) = r.docs.get_mut(doc) {
                        d.apply_list(bullet)
                    }
                });
                Ok(())
            }
        }
    }

    fn tables_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "add" => 1,
            "item" | "_default" => 2,
            "count" => 3,
            _ => return None,
        })
    }
    impl IDispatch_Impl for Tables_Impl {
        no_typeinfo!();
        dispatch_names!("Tables", tables_id);
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
                log(&format!("Tables[{doc}]::Invoke id={id}"));
                match id {
                    // Add(Range, NumRows, NumColumns, …) — args after the range.
                    1 => {
                        let rows = arg_i32(params, 1).unwrap_or(1).max(1) as usize;
                        let cols = arg_i32(params, 2).unwrap_or(1).max(1) as usize;
                        let tno = reg(|r| r.docs.get_mut(doc).map(|d| d.add_table(rows, cols)))
                            .unwrap_or(0);
                        put_disp(result, WordTable { doc, tno });
                    }
                    2 => {
                        let tno = arg_i32(params, 0).unwrap_or(1).max(1) as usize;
                        put_disp(result, WordTable { doc, tno });
                    }
                    3 => put(result, VARIANT::from(reg(|r| r.docs.get(doc).map(|d| d.table_count()).unwrap_or(0)) as i32)),
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    fn table_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "cell" => 1,
            "rows" => 2,
            "columns" => 3,
            _ => return None,
        })
    }
    impl IDispatch_Impl for WordTable_Impl {
        no_typeinfo!();
        dispatch_names!("Table", table_id);
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
            let (doc, tno) = (self.doc, self.tno);
            unsafe {
                log(&format!("Table[{doc}/{tno}]::Invoke id={id}"));
                match id {
                    // Cell(Row, Column)
                    1 => {
                        let row = arg_i32(params, 0).unwrap_or(1).max(1) as usize;
                        let col = arg_i32(params, 1).unwrap_or(1).max(1) as usize;
                        put_disp(result, WordCell { doc, tno, row, col });
                    }
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    fn cell_member_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "range" => 1,
            _ => return None,
        })
    }
    impl IDispatch_Impl for WordCell_Impl {
        no_typeinfo!();
        dispatch_names!("Cell", cell_member_id);
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
            let (doc, tno, row, col) = (self.doc, self.tno, self.row, self.col);
            unsafe {
                match id {
                    1 => put_disp(result, CellRange { doc, tno, row, col }),
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }

    fn cellrange_id(name: &str) -> Option<i32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "text" => 1,
            "insertafter" | "insertbefore" => 2,
            _ => return None,
        })
    }
    impl IDispatch_Impl for CellRange_Impl {
        no_typeinfo!();
        dispatch_names!("CellRange", cellrange_id);
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
            let (doc, tno, row, col) = (self.doc, self.tno, self.row, self.col);
            unsafe {
                match id {
                    1 => {
                        if is_put(wflags) {
                            let s = arg_string(params, 0).unwrap_or_default();
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    d.set_cell_text(tno, row, col, &s)
                                }
                            });
                        } else {
                            let t = reg(|r| {
                                r.docs.get(doc).map(|d| d.cell_text(tno, row, col)).unwrap_or_default()
                            });
                            put(result, VARIANT::from(BSTR::from(t.as_str())));
                        }
                    }
                    2 => {
                        if let Some(s) = arg_string(params, 0) {
                            reg(|r| {
                                if let Some(d) = r.docs.get_mut(doc) {
                                    let cur = d.cell_text(tno, row, col);
                                    d.set_cell_text(tno, row, col, &(cur + &s))
                                }
                            });
                        }
                    }
                    _ => return unhandled(id, wflags, params, result),
                }
                Ok(())
            }
        }
    }
}
