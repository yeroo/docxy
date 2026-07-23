//! mkwordtypelib — author our OWN Word-compatible type library (.tlb) so the
//! out-of-process LocalServer can marshal the shim's dual interfaces on a machine
//! with **no Word installed**. The Word counterpart of xlcomshim's mktypelib: it
//! COPIES Word's exact FUNCDESCs from its registered typelib into a fresh typelib
//! we create, flattening only user-defined type references (enum -> VT_I4,
//! interface pointer -> VT_UNKNOWN — both ABI-equivalent for marshalling). Word
//! uses no [lcid] params, but everything (param counts, in/out/opt flags, vtable
//! order) is copied verbatim, so the result is faithful by construction. The
//! oracle test (tests/typelib_faithful.rs) proves it.
//!
//! Usage: mkwordtypelib [out.tlb] | register [out.tlb] | unregister [out.tlb]

#[cfg(not(windows))]
fn main() {
    eprintln!("mkwordtypelib only runs on Windows (needs the COM typelib APIs).");
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

    // Microsoft Word Object Library, LIBID {00020905-…}, v8.x.
    const WORD_LIBID: GUID = GUID::from_u128(0x00020905_0000_0000_c000_000000000046);
    // stdole (for the IDispatch base type): LIBID {00020430-…}, v2.0.
    const STDOLE_LIBID: GUID = GUID::from_u128(0x00020430_0000_0000_c000_000000000046);
    // Our OWN library id — NOT Word's — so registering never collides with Office.
    const DOCXY_LIBID: GUID = GUID::from_u128(0x9c2f4a11_7d33_4b6e_b1a4_2e7c8d5f0a92);

    // The dual interfaces the shim implements, by Word IID.
    const WANTED: &[(&str, u128)] = &[
        ("_Application", 0x00020970_0000_0000_c000_000000000046),
        ("Documents", 0x0002096c_0000_0000_c000_000000000046),
        ("_Document", 0x0002096b_0000_0000_c000_000000000046),
        ("Selection", 0x00020975_0000_0000_c000_000000000046),
        ("Range", 0x0002095e_0000_0000_c000_000000000046),
    ];

    fn load_word() -> Result<ITypeLib> {
        for minor in (0u16..=9).rev() {
            if let Ok(tl) = unsafe { LoadRegTypeLib(&WORD_LIBID, 8, minor, 0) } {
                return Ok(tl);
            }
        }
        Err(Error::from_win32())
    }

    pub fn run() -> Result<()> {
        let arg1 = std::env::args().nth(1);
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            match arg1.as_deref() {
                Some("register") => return register(&arg2_path(), true),
                Some("unregister") => return register(&arg2_path(), false),
                _ => {}
            }
            let out = arg1.unwrap_or_else(default_out_path);

            let src = load_word()?;
            let stdole = LoadRegTypeLib(&STDOLE_LIBID, 2, 0, 0)?;
            let idisp_ti: ITypeInfo = stdole.GetTypeInfoOfGuid(&IDispatch::IID)?;

            let out_w: Vec<u16> = out.encode_utf16().chain([0]).collect();
            let dst: ICreateTypeLib2 = CreateTypeLib2(SYS_WIN64, PCWSTR(out_w.as_ptr()))?;
            dst.SetGuid(&DOCXY_LIBID)?;
            dst.SetVersion(1, 0)?;
            dst.SetName(&HSTRING::from("DocxyWord"))?;
            dst.SetLcid(0)?;

            let mut total_funcs = 0usize;
            let mut done = 0usize;
            for (name, iid_u128) in WANTED {
                let iid = GUID::from_u128(*iid_u128);
                let sti = match vtable_iface(&src, &iid) {
                    Ok(t) => t,
                    Err(e) => {
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
            let flags = TYPEFLAG_FOLEAUTOMATION.0 | TYPEFLAG_FDISPATCHABLE.0;
            cti.SetTypeFlags(flags as u32)?;
            let mut href = 0u32;
            cti.AddRefTypeInfo(idisp_ti, &mut href)?;
            cti.AddImplType(0, href)?;

            let mut added = 0u32;
            for f in 0..cfuncs {
                let fd = sti.GetFuncDesc(f)?;
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
                // Present every member as a plain method with a unique memid: the
                // oleaut marshaller keys off the slot + param list (not invkind/
                // memid), and this avoids LayOut's strict get/put cross-validation.
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

    /// Flatten a TYPEDESC's user-defined references to ABI-equivalent intrinsics
    /// (enum -> VT_I4 keeping pointer depth; interface -> collapse innermost
    /// PTR->USERDEFINED to VT_UNKNOWN), so it references no other typelib.
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

    fn arg2_path() -> String {
        std::env::args().nth(2).unwrap_or_else(default_out_path)
    }

    /// Register (or unregister) our typelib per-user: TypeLib\{our LIBID} -> the
    /// .tlb, and per interface Interface\{IID}\{ProxyStubClsid32 = {00020424}
    /// (oleaut), TypeLib = {our LIBID}} — what the universal marshaller needs.
    unsafe fn register(path: &str, on: bool) -> Result<()> {
        unsafe {
            let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
            if on {
                let tl: ITypeLib = LoadTypeLibEx(PCWSTR(w.as_ptr()), REGKIND_NONE)?;
                RegisterTypeLibForUser(&tl, PCWSTR(w.as_ptr()), PCWSTR::null())?;
                println!("registered Word typelib (per-user) from {path}");
            } else {
                UnRegisterTypeLibForUser(&DOCXY_LIBID, 1, 0, 0, SYS_WIN64)?;
                println!("unregistered Word typelib (per-user)");
            }
            Ok(())
        }
    }

    fn default_out_path() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("docxy-word.tlb")))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "docxy-word.tlb".into())
    }
}
