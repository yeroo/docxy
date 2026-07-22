//! Oracle differential test: prove our authored typelib (docxy-excel.tlb, emitted
//! by the `mktypelib` bin) is a faithful, ABI-equivalent copy of Excel's real
//! typelib for every early-bound (vtable) interface the shim serves.
//!
//! Excel's registered typelib is the ORACLE. For each interface, method, and
//! parameter we assert the marshalling-relevant facts match:
//!   * same interface exists, same vtable length,
//!   * each method at the same vtable slot (oVft),
//!   * same parameter count,
//!   * each parameter's flags identical — in/out/opt and, critically, the hidden
//!     **lcid** and **retval** flags that drive LCID injection and the
//!     return-value convention,
//!   * each parameter (and the return) ABI-equivalent in type, honoring the two
//!     deliberate flattenings (enum -> I4, interface pointer -> IUnknown).
//! That is thousands of assertions — one per parameter across ~940 methods —
//! covering every corner of the surface.
//!
//! Windows + real-Excel only; skips cleanly where Excel's typelib isn't
//! registered (CI, the VDI).
#![cfg(windows)]

use std::process::Command;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Variant::*;
use windows::core::*;

const EXCEL_LIBID: GUID = GUID::from_u128(0x00020813_0000_0000_c000_000000000046);

// The early-bound (vtable) interfaces the shim serves and mktypelib authors.
const WANTED: &[(&str, u128)] = &[
    ("_Application", 0x000208d5_0000_0000_c000_000000000046),
    ("Workbooks", 0x000208db_0000_0000_c000_000000000046),
    ("_Workbook", 0x000208da_0000_0000_c000_000000000046),
    ("Sheets", 0x000208d7_0000_0000_c000_000000000046),
    ("_Worksheet", 0x000208d8_0000_0000_c000_000000000046),
];

unsafe fn load_excel() -> Option<ITypeLib> {
    for minor in (3u16..=9).rev() {
        if let Ok(tl) = unsafe { LoadRegTypeLib(&EXCEL_LIBID, 1, minor, 0) } {
            return Some(tl);
        }
    }
    None
}

/// Find the vtable (TKIND_INTERFACE) typeinfo for an IID within a library — the
/// same resolution mktypelib uses (scan, then the dispinterface's `-1` partner).
unsafe fn vtable_iface(lib: &ITypeLib, iid: &GUID) -> Option<ITypeInfo> {
    unsafe {
        let n = lib.GetTypeInfoCount();
        for i in 0..n {
            let Ok(ti) = lib.GetTypeInfo(i) else { continue };
            let Ok(attr) = ti.GetTypeAttr() else { continue };
            let hit = (*attr).guid == *iid && (*attr).typekind == TKIND_INTERFACE;
            ti.ReleaseTypeAttr(attr);
            if hit {
                return Some(ti);
            }
        }
        let disp = lib.GetTypeInfoOfGuid(iid).ok()?;
        let href = disp.GetRefTypeOfImplType(u32::MAX).ok()?;
        disp.GetRefTypeInfo(href).ok()
    }
}

/// ABI-equivalence class of a type: walk any pointer chain to the leaf and reduce
/// to a marshalling-equivalence bucket. Enums and interfaces reduce to the same
/// buckets our typelib flattens them to (I4 / iface), so a faithful copy matches.
unsafe fn abi_class(ti: &ITypeInfo, td: *const TYPEDESC) -> String {
    unsafe {
        let mut node = td;
        while (*node).vt == VT_PTR {
            let next = (*node).Anonymous.lptdesc;
            if next.is_null() {
                break;
            }
            node = next;
        }
        let vt = (*node).vt;
        match vt {
            VT_VARIANT => "variant".into(),
            VT_BSTR => "bstr".into(),
            VT_BOOL => "bool".into(),
            VT_I1 => "i1".into(),
            VT_UI1 => "ui1".into(),
            VT_I2 => "i2".into(),
            VT_UI2 => "ui2".into(),
            VT_I4 | VT_INT => "i4".into(),
            VT_UI4 | VT_UINT => "ui4".into(),
            VT_I8 => "i8".into(),
            VT_UI8 => "ui8".into(),
            VT_R4 => "r4".into(),
            VT_R8 => "r8".into(),
            VT_CY => "cy".into(),
            VT_DATE => "date".into(),
            VT_ERROR => "error".into(),
            VT_HRESULT => "hresult".into(),
            VT_VOID => "void".into(),
            VT_DISPATCH | VT_UNKNOWN => "iface".into(),
            VT_USERDEFINED => {
                // enum -> i4, everything else (interface/dispatch/coclass) -> iface
                let kind = (*node)
                    .Anonymous
                    .hreftype
                    .pipe(|href| ref_kind(ti, href));
                match kind {
                    Some(TKIND_ENUM) => "i4".into(),
                    _ => "iface".into(),
                }
            }
            other => format!("vt{}", other.0),
        }
    }
}

unsafe fn ref_kind(ti: &ITypeInfo, href: u32) -> Option<TYPEKIND> {
    unsafe {
        let rti = ti.GetRefTypeInfo(href).ok()?;
        let attr = rti.GetTypeAttr().ok()?;
        let k = (*attr).typekind;
        rti.ReleaseTypeAttr(attr);
        Some(k)
    }
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

#[derive(PartialEq)]
struct Sig {
    ovft: i16,
    params: Vec<(u16, String)>, // (wParamFlags, abi_class)
    ret: String,
}

/// The interface's own (non-inherited) vtable members, in slot order.
unsafe fn sigs(ti: &ITypeInfo) -> Vec<Sig> {
    unsafe {
        let attr = ti.GetTypeAttr().expect("typeattr");
        let cfuncs = (*attr).cFuncs as u32;
        ti.ReleaseTypeAttr(attr);
        let ptr = 7 * std::mem::size_of::<usize>() as i16;
        let mut out = Vec::new();
        for f in 0..cfuncs {
            let Ok(fd) = ti.GetFuncDesc(f) else { continue };
            if (*fd).oVft < ptr {
                ti.ReleaseFuncDesc(fd);
                continue;
            }
            let cparams = (*fd).cParams as usize;
            let mut params = Vec::with_capacity(cparams);
            for p in 0..cparams {
                let ed = (*fd).lprgelemdescParam.add(p);
                let flags = (*ed).Anonymous.paramdesc.wParamFlags.0;
                params.push((flags, abi_class(ti, &(*ed).tdesc)));
            }
            out.push(Sig {
                ovft: (*fd).oVft,
                params,
                ret: abi_class(ti, &(*fd).elemdescFunc.tdesc),
            });
            ti.ReleaseFuncDesc(fd);
        }
        out
    }
}

#[test]
fn typelib_matches_excel_oracle() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let Some(excel) = load_excel() else {
            eprintln!("SKIP: Excel typelib not registered (no oracle on this machine).");
            return;
        };

        // Generate our typelib fresh via the mktypelib bin.
        let out = std::env::temp_dir().join("docxy-excel-oracletest.tlb");
        let _ = std::fs::remove_file(&out);
        let status = Command::new(env!("CARGO_BIN_EXE_mktypelib"))
            .arg(&out)
            .status()
            .expect("run mktypelib");
        assert!(status.success(), "mktypelib failed");
        let out_w: Vec<u16> = out
            .to_string_lossy()
            .encode_utf16()
            .chain([0])
            .collect();
        let ours: ITypeLib =
            LoadTypeLibEx(PCWSTR(out_w.as_ptr()), REGKIND_NONE).expect("load our tlb");

        let mut fails: Vec<String> = Vec::new();
        let (mut n_methods, mut n_params) = (0usize, 0usize);

        for (name, iid_u128) in WANTED {
            let iid = GUID::from_u128(*iid_u128);
            let e_ti = vtable_iface(&excel, &iid).expect("excel vtable iface");
            let o_ti = ours
                .GetTypeInfoOfGuid(&iid)
                .unwrap_or_else(|_| panic!("{name}: missing from our typelib"));

            let es = sigs(&e_ti);
            let os = sigs(&o_ti);
            if es.len() != os.len() {
                fails.push(format!(
                    "{name}: vtable length {} (ours) != {} (Excel)",
                    os.len(),
                    es.len()
                ));
                continue;
            }
            for (i, (e, o)) in es.iter().zip(os.iter()).enumerate() {
                n_methods += 1;
                if e.ovft != o.ovft {
                    fails.push(format!(
                        "{name} method#{i}: oVft {} (ours) != {} (Excel)",
                        o.ovft, e.ovft
                    ));
                }
                if e.params.len() != o.params.len() {
                    fails.push(format!(
                        "{name} method#{i} (slot {}): param count {} (ours) != {} (Excel)",
                        e.ovft,
                        o.params.len(),
                        e.params.len()
                    ));
                    continue;
                }
                for (j, (ep, op)) in e.params.iter().zip(o.params.iter()).enumerate() {
                    n_params += 1;
                    if ep.0 != op.0 {
                        fails.push(format!(
                            "{name} method#{i} param#{j}: flags {:#x} (ours) != {:#x} (Excel)",
                            op.0, ep.0
                        ));
                    }
                    if ep.1 != op.1 {
                        fails.push(format!(
                            "{name} method#{i} param#{j}: type '{}' (ours) != '{}' (Excel)",
                            op.1, ep.1
                        ));
                    }
                }
                if e.ret != o.ret {
                    fails.push(format!(
                        "{name} method#{i}: return '{}' (ours) != '{}' (Excel)",
                        o.ret, e.ret
                    ));
                }
            }
        }

        eprintln!(
            "oracle check: {} interfaces, {n_methods} methods, {n_params} params compared",
            WANTED.len()
        );
        assert!(
            fails.is_empty(),
            "{} ABI mismatch(es) vs the Excel oracle:\n{}",
            fails.len(),
            fails.join("\n")
        );
    }
}
