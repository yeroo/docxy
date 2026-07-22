//! mktypelib — author our OWN Excel-compatible type library (.tlb) so the
//! out-of-process LocalServer can marshal the shim's dual interfaces on a machine
//! with **no Office installed** (the VDI). The oleaut universal marshaller
//! {00020424} reads a registered typelib to build proxies for our IIDs; without
//! Office there is none, so we ship this one.
//!
//! Rather than hand-write ~1250 method descriptors (and risk subtle ABI errors),
//! we COPY Excel's exact FUNCDESCs from its registered typelib into a fresh
//! typelib we create, flattening only the user-defined type references that would
//! otherwise need cross-typelib remapping:
//!   * enum  -> VT_I4        (enums are I4 on the wire)
//!   * interface/dispatch/coclass pointer -> VT_UNKNOWN (a marshalable iface ptr)
//! Both flattenings are ABI-equivalent for marshalling. Everything else — param
//! counts, in/out/opt flags, the hidden [lcid] and [retval] flags, invkind, the
//! vtable order — is copied verbatim from Excel, so the result is faithful by
//! construction. The companion oracle test (tests/typelib_faithful.rs) proves it.
//!
//! Usage:  mktypelib [out.tlb]     (default: target/<profile>/docxy-excel.tlb)

#[cfg(not(windows))]
fn main() {
    eprintln!("mktypelib only runs on Windows (needs the COM typelib APIs).");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    win::run()
}

#[cfg(windows)]
mod win {
    #![allow(non_snake_case)]
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Ole::*;
    use windows::Win32::System::Variant::{VT_I4, VT_PTR, VT_UNKNOWN, VT_USERDEFINED};
    use windows::core::*;

    // Excel type library: LIBID {00020813-…}, versions 1.x.
    const EXCEL_LIBID: GUID = GUID::from_u128(0x00020813_0000_0000_c000_000000000046);
    // stdole (for the IDispatch base type): LIBID {00020430-…}, v2.0.
    const STDOLE_LIBID: GUID = GUID::from_u128(0x00020430_0000_0000_c000_000000000046);
    // Our OWN library id — NOT Excel's — so registering it never collides with a
    // real Office typelib registration.
    const DOCXY_LIBID: GUID = GUID::from_u128(0x7b3f9e21_4c1a_4e8b_a2d6_9f5c1e0b7a31);

    // The dual interfaces the shim implements, by Excel IID.
    const WANTED: &[(&str, u128)] = &[
        ("_Application", 0x000208d5_0000_0000_c000_000000000046),
        ("Workbooks", 0x000208db_0000_0000_c000_000000000046),
        ("_Workbook", 0x000208da_0000_0000_c000_000000000046),
        ("Sheets", 0x000208d7_0000_0000_c000_000000000046),
        ("_Worksheet", 0x000208d8_0000_0000_c000_000000000046),
        ("Range", 0x00020846_0000_0000_c000_000000000046),
        ("Font", 0x0002084d_0000_0000_c000_000000000046),
        ("Interior", 0x00020870_0000_0000_c000_000000000046),
    ];

    fn load_excel() -> Result<ITypeLib> {
        // Newest first; a machine may have any 1.x minor.
        for minor in (3u16..=9).rev() {
            if let Ok(tl) = unsafe { LoadRegTypeLib(&EXCEL_LIBID, 1, minor, 0) } {
                return Ok(tl);
            }
        }
        Err(Error::from_win32())
    }

    pub fn run() -> Result<()> {
        let out = std::env::args()
            .nth(1)
            .unwrap_or_else(|| default_out_path());
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;

            let src = load_excel()?;

            if out == "list" {
                let n = src.GetTypeInfoCount();
                for i in 0..n {
                    let Ok(ti) = src.GetTypeInfo(i) else { continue };
                    let Ok(attr) = ti.GetTypeAttr() else { continue };
                    let (k, g, cf) = ((*attr).typekind.0, (*attr).guid, (*attr).cFuncs);
                    ti.ReleaseTypeAttr(attr);
                    let mut name = BSTR::default();
                    let _ = ti.GetDocumentation(-1, Some(&mut name), None, std::ptr::null_mut(), None);
                    let ns = name.to_string();
                    if ns.contains("Range") || ns.contains("Worksheet") || ns == "_Application" {
                        println!("#{i} kind={k} cFuncs={cf} guid={g:?} {ns}");
                    }
                }
                return Ok(());
            }
            let stdole = LoadRegTypeLib(&STDOLE_LIBID, 2, 0, 0)?;
            let idisp_ti: ITypeInfo = stdole.GetTypeInfoOfGuid(&IDispatch::IID)?;

            let out_w: Vec<u16> = out.encode_utf16().chain([0]).collect();
            let dst: ICreateTypeLib2 = CreateTypeLib2(SYS_WIN64, PCWSTR(out_w.as_ptr()))?;
            dst.SetGuid(&DOCXY_LIBID)?;
            dst.SetVersion(1, 9)?;
            dst.SetName(&HSTRING::from("DocxyExcel"))?;
            dst.SetLcid(0)?;

            let mut total_funcs = 0usize;
            let mut done = 0usize;
            for (name, iid_u128) in WANTED {
                let iid = GUID::from_u128(*iid_u128);
                // We need the vtable (TKIND_INTERFACE) form, whose funcdescs are
                // *physical* (HRESULT return + [out,retval] param + [lcid]).
                // GetTypeInfoOfGuid is inconsistent (returns the interface for some
                // duals, the dispinterface for others, e.g. Range), so scan.
                let sti = match vtable_iface(&src, &iid) {
                    Ok(t) => t,
                    Err(e) => {
                        // No standalone vtable interface for this IID. Such members
                        // (Range/Font/Interior) reach the shim via IDispatch, which
                        // marshals through the standard oleaut dispatch proxy with
                        // NO typelib, so omitting them here is safe.
                        eprintln!("{name}: no vtable interface ({e:?}); skipping (IDispatch-marshaled)");
                        continue;
                    }
                };
                match copy_interface(&dst, &sti, &idisp_ti, name, &iid) {
                    Ok(n) => {
                        total_funcs += n;
                        done += 1;
                        println!("{name}: copied {n} funcs");
                    }
                    Err(e) => eprintln!("{name}: copy failed ({e:?}); skipping"),
                }
            }

            dst.SaveAllChanges()?;
            println!("wrote {out} ({done} vtable interfaces, {total_funcs} funcs)");
        }
        Ok(())
    }

    /// The vtable (TKIND_INTERFACE) typeinfo for an IID. A dual is stored as two
    /// typeinfos sharing the IID — a TKIND_DISPATCH and a TKIND_INTERFACE; we want
    /// the latter (physical funcdescs). Scan the whole library for it; fall back
    /// to the dispinterface's `-1` partner if a direct scan misses.
    unsafe fn vtable_iface(lib: &ITypeLib, iid: &GUID) -> Result<ITypeInfo> {
        unsafe {
            let n = lib.GetTypeInfoCount();
            for i in 0..n {
                let Ok(ti) = lib.GetTypeInfo(i) else { continue };
                let Ok(attr) = ti.GetTypeAttr() else { continue };
                let hit = (*attr).guid == *iid && (*attr).typekind == TKIND_INTERFACE;
                ti.ReleaseTypeAttr(attr);
                if hit {
                    return Ok(ti);
                }
            }
            let disp = lib.GetTypeInfoOfGuid(iid)?;
            let href = disp.GetRefTypeOfImplType(u32::MAX)?;
            disp.GetRefTypeInfo(href)
        }
    }

    unsafe fn copy_interface(
        dst: &ICreateTypeLib2,
        sti: &ITypeInfo,
        idisp_ti: &ITypeInfo,
        name: &str,
        iid: &GUID,
    ) -> Result<usize> {
        unsafe {
            let attr = sti.GetTypeAttr()?;
            let cfuncs = (*attr).cFuncs as u32;
            sti.ReleaseTypeAttr(attr);
            let cti: ICreateTypeInfo = dst.CreateTypeInfo(&HSTRING::from(name), TKIND_INTERFACE)?;
            cti.SetGuid(iid)?;
            // OLE-automation + dispatchable + IDispatch-derived: a vtable interface
            // the oleaut universal marshaller can build a proxy for. (FDUAL makes
            // LayOut strictly re-validate every get/put pair against Excel's exact
            // types, which our type-flattening perturbs; the interface still
            // inherits IDispatch via AddImplType below.)
            let flags = TYPEFLAG_FOLEAUTOMATION.0 | TYPEFLAG_FDISPATCHABLE.0;
            cti.SetTypeFlags(flags as u32)?;
            // Inherit IDispatch (impltype 0). The interface partner's own funcs
            // already carry vtable offsets past the 7 inherited slots.
            let mut href = 0u32;
            cti.AddRefTypeInfo(idisp_ti, &mut href)?;
            cti.AddImplType(0, href)?;

            let mut added = 0u32;
            for f in 0..cfuncs {
                let fd = sti.GetFuncDesc(f)?;
                // Skip any inherited IUnknown/IDispatch slots (oVft < 7*ptr) — the
                // AddImplType above already supplies them.
                if ((*fd).oVft as usize) < 7 * std::mem::size_of::<usize>() {
                    sti.ReleaseFuncDesc(fd);
                    continue;
                }
                flatten_elem(sti, &mut (*fd).elemdescFunc.tdesc);
                let cparams = (*fd).cParams as usize;
                for p in 0..cparams {
                    let ed = (*fd).lprgelemdescParam.add(p);
                    flatten_elem(sti, &mut (*ed).tdesc);
                }
                // The oleaut marshaller keys a vtable call off the slot (oVft) and
                // the param list, NOT the invkind or memid. LayOut, however,
                // strictly cross-validates property get/put groups (and our
                // type-flattening perturbs Excel's exact types). Present every
                // member as a plain method with a unique memid so there are no
                // property groups to validate — the physical signature (params,
                // slot) is untouched, so marshalling is unaffected.
                (*fd).invkind = INVOKE_FUNC;
                (*fd).memid = 0x2000 + added as i32;
                if let Err(e) = cti.AddFuncDesc(added, fd) {
                    eprintln!("  {name} func#{f}: AddFuncDesc failed: {e:?}");
                    sti.ReleaseFuncDesc(fd);
                    return Err(e);
                }
                sti.ReleaseFuncDesc(fd);
                added += 1;
            }

            cti.LayOut()?;
            Ok(added as usize)
        }
    }

    /// Rewrite a TYPEDESC so it references no other typelib's types, staying
    /// ABI-equivalent. Two user-defined cases:
    ///   * enum  -> replace the USERDEFINED leaf with VT_I4 (keeps any pointer
    ///     depth, so `[out] Enum*` stays `I4*`).
    ///   * interface/dispatch/coclass -> VT_UNKNOWN encodes a single interface
    ///     pointer, so collapse the *innermost* `VT_PTR -> USERDEFINED` to
    ///     VT_UNKNOWN. `[in] IFoo*` (PTR->UD) becomes VT_UNKNOWN; `[out] IFoo**`
    ///     (PTR->PTR->UD) becomes `PTR->VT_UNKNOWN` — pointer depth preserved.
    unsafe fn flatten_elem(sti: &ITypeInfo, td: *mut TYPEDESC) {
        unsafe {
            let mut node = td;
            loop {
                if (*node).vt == VT_PTR {
                    let next = (*node).Anonymous.lptdesc;
                    if next.is_null() {
                        return;
                    }
                    if (*next).vt == VT_USERDEFINED {
                        let href = (*next).Anonymous.hreftype;
                        match ref_kind(sti, href) {
                            Some(TKIND_ENUM) => {
                                (*next).vt = VT_I4;
                                (*next).Anonymous.hreftype = 0;
                            }
                            _ => {
                                // collapse this PTR->interface node to one iface ptr
                                (*node).vt = VT_UNKNOWN;
                                (*node).Anonymous.hreftype = 0;
                            }
                        }
                        return;
                    }
                    node = next;
                } else if (*node).vt == VT_USERDEFINED {
                    let href = (*node).Anonymous.hreftype;
                    match ref_kind(sti, href) {
                        Some(TKIND_ENUM) => (*node).vt = VT_I4,
                        _ => (*node).vt = VT_UNKNOWN,
                    }
                    (*node).Anonymous.hreftype = 0;
                    return;
                } else {
                    return;
                }
            }
        }
    }

    unsafe fn ref_kind(sti: &ITypeInfo, href: u32) -> Option<TYPEKIND> {
        unsafe {
            let rti = sti.GetRefTypeInfo(href).ok()?;
            let attr = rti.GetTypeAttr().ok()?;
            let k = (*attr).typekind;
            rti.ReleaseTypeAttr(attr);
            Some(k)
        }
    }

    fn default_out_path() -> String {
        // …/target/<profile>/docxy-excel.tlb, next to the exe.
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("docxy-excel.tlb")))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "docxy-excel.tlb".into())
    }
}
