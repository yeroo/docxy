//! Author + register a shim's own type library, so the out-of-process
//! LocalServer can marshal its dual interfaces on a machine with no Office. The
//! oleaut universal marshaller reads a registered typelib to build vtable proxies
//! for our IIDs; without Office there is none, so each shim ships one.
//!
//! Generic over which Office typelib to copy from and which interfaces to include
//! ([`Spec`]). Rather than hand-write descriptors, we COPY the Office typelib's
//! exact physical FUNCDESCs into a fresh typelib we create, flattening only
//! user-defined type references (enum -> VT_I4, interface pointer -> VT_UNKNOWN;
//! both ABI-equivalent). Members are presented as plain methods with unique
//! memids so LayOut's get/put cross-validation (which the flattening perturbs)
//! doesn't reject them — the physical signature (slot, params, flags) is
//! untouched, so marshalling is faithful by construction.

use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Variant::{VT_I4, VT_PTR, VT_UNKNOWN, VT_USERDEFINED};
use windows::core::*;

/// stdole (for the IDispatch base type): LIBID {00020430-…}, v2.0.
const STDOLE_LIBID: GUID = GUID::from_u128(0x00020430_0000_0000_c000_000000000046);

/// What to author: which Office typelib to copy, our own identity, and the dual
/// interfaces (by Office IID) to include.
pub struct Spec {
    /// The Office typelib LIBID (Excel {00020813}, Word {00020905}).
    pub src_libid: GUID,
    /// Its major version (Excel 1, Word 8); minors 9..=0 are tried.
    pub src_major: u16,
    /// Our OWN library id — NOT Office's — so registering never collides.
    pub docxy_libid: GUID,
    /// Our typelib name and version.
    pub name: &'static str,
    pub version: (u16, u16),
    /// The dual interfaces to copy, as (name, IID-as-u128).
    pub wanted: &'static [(&'static str, u128)],
    /// Default output file name (next to the exe) when no path is given.
    pub default_file: &'static str,
}

/// CLI entry: `<no arg | out.tlb>` authors; `register <tlb>` / `unregister <tlb>`
/// register per-user. Each shim's `mk*typelib` bin is a one-line call to this.
pub fn run(spec: &Spec) -> Result<()> {
    let arg1 = std::env::args().nth(1);
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        match arg1.as_deref() {
            Some("register") => return register(spec, &arg2_path(spec), true),
            Some("unregister") => return register(spec, &arg2_path(spec), false),
            _ => {}
        }
        let out = arg1.unwrap_or_else(|| default_out_path(spec));
        author(spec, &out)
    }
}

/// Author the .tlb at `out`.
pub unsafe fn author(spec: &Spec, out: &str) -> Result<()> {
    unsafe {
        let src = load_office(spec)?;
        let stdole = LoadRegTypeLib(&STDOLE_LIBID, 2, 0, 0)?;
        let idisp_ti: ITypeInfo = stdole.GetTypeInfoOfGuid(&IDispatch::IID)?;

        let out_w: Vec<u16> = out.encode_utf16().chain([0]).collect();
        let dst: ICreateTypeLib2 = CreateTypeLib2(SYS_WIN64, PCWSTR(out_w.as_ptr()))?;
        dst.SetGuid(&spec.docxy_libid)?;
        dst.SetVersion(spec.version.0, spec.version.1)?;
        dst.SetName(&HSTRING::from(spec.name))?;
        dst.SetLcid(0)?;

        let (mut total, mut done) = (0usize, 0usize);
        for (name, iid_u128) in spec.wanted {
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
                    total += n;
                    done += 1;
                    println!("{name}: copied {n} funcs");
                }
                Err(e) => eprintln!("{name}: copy failed ({e:?}); skipping"),
            }
        }
        dst.SaveAllChanges()?;
        println!("wrote {out} ({done} vtable interfaces, {total} funcs)");
        Ok(())
    }
}

fn load_office(spec: &Spec) -> Result<ITypeLib> {
    for minor in (0u16..=9).rev() {
        if let Ok(tl) = unsafe { LoadRegTypeLib(&spec.src_libid, spec.src_major, minor, 0) } {
            return Ok(tl);
        }
    }
    Err(Error::from_win32())
}

/// The vtable (TKIND_INTERFACE) typeinfo for an IID — a dual is stored as a
/// TKIND_DISPATCH plus a TKIND_INTERFACE partner; we want the physical funcdescs
/// of the latter. Scan, then fall back to the dispinterface's `-1` partner.
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

fn arg2_path(spec: &Spec) -> String {
    std::env::args().nth(2).unwrap_or_else(|| default_out_path(spec))
}

/// Register (or unregister) our typelib per-user: TypeLib\{our LIBID} -> the
/// .tlb, and per interface Interface\{IID}\{ProxyStubClsid32 = {00020424}
/// (oleaut), TypeLib = {our LIBID}} — what the universal marshaller needs.
unsafe fn register(spec: &Spec, path: &str, on: bool) -> Result<()> {
    unsafe {
        let w: Vec<u16> = path.encode_utf16().chain([0]).collect();
        if on {
            let tl: ITypeLib = LoadTypeLibEx(PCWSTR(w.as_ptr()), REGKIND_NONE)?;
            RegisterTypeLibForUser(&tl, PCWSTR(w.as_ptr()), PCWSTR::null())?;
            println!("registered typelib (per-user) from {path}");
        } else {
            UnRegisterTypeLibForUser(
                &spec.docxy_libid,
                spec.version.0,
                spec.version.1,
                0,
                SYS_WIN64,
            )?;
            println!("unregistered typelib (per-user)");
        }
        Ok(())
    }
}

fn default_out_path(spec: &Spec) -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(spec.default_file)))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| spec.default_file.to_string())
}
