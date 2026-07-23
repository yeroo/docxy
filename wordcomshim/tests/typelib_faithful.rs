//! Oracle differential test: prove our authored Word typelib (docxy-word.tlb,
//! emitted by mkwordtypelib) is a faithful, ABI-equivalent copy of Word's real
//! typelib for every early-bound interface the shim serves.
//!
//! Word's registered typelib is the ORACLE. For each interface/method/param we
//! assert the marshalling-relevant facts match: same interface, same vtable
//! length, each method at the same vtable slot (oVft), same param count, identical
//! param flags (in/out/opt/retval), and ABI-equivalent types (honoring the two
//! flattenings enum->I4 and interface->IUnknown). Thousands of assertions.
//!
//! Windows + real-Word only; skips cleanly where Word's typelib isn't registered.
#![cfg(windows)]

use std::process::Command;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Variant::*;
use windows::core::*;

const WORD_LIBID: GUID = GUID::from_u128(0x00020905_0000_0000_c000_000000000046);

const WANTED: &[(&str, u128)] = &[
    ("_Application", 0x00020970_0000_0000_c000_000000000046),
    ("Documents", 0x0002096c_0000_0000_c000_000000000046),
    ("_Document", 0x0002096b_0000_0000_c000_000000000046),
    ("Selection", 0x00020975_0000_0000_c000_000000000046),
    ("Range", 0x0002095e_0000_0000_c000_000000000046),
];

unsafe fn load_word() -> Option<ITypeLib> {
    for minor in (0u16..=9).rev() {
        if let Ok(tl) = unsafe { LoadRegTypeLib(&WORD_LIBID, 8, minor, 0) } {
            return Some(tl);
        }
    }
    None
}

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
        match (*node).vt {
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
            VT_USERDEFINED => match ref_kind(ti, (*node).Anonymous.hreftype) {
                Some(TKIND_ENUM) => "i4".into(),
                _ => "iface".into(),
            },
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

#[derive(PartialEq)]
struct Sig {
    ovft: i16,
    params: Vec<(u16, String)>,
    ret: String,
}

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
fn typelib_matches_word_oracle() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let Some(word) = load_word() else {
            eprintln!("SKIP: Word typelib not registered (no oracle on this machine).");
            return;
        };

        let out = std::env::temp_dir().join("docxy-word-oracletest.tlb");
        let _ = std::fs::remove_file(&out);
        let status = Command::new(env!("CARGO_BIN_EXE_mkwordtypelib"))
            .arg(&out)
            .status()
            .expect("run mkwordtypelib");
        assert!(status.success(), "mkwordtypelib failed");
        let out_w: Vec<u16> = out.to_string_lossy().encode_utf16().chain([0]).collect();
        let ours: ITypeLib =
            LoadTypeLibEx(PCWSTR(out_w.as_ptr()), REGKIND_NONE).expect("load our tlb");

        let mut fails: Vec<String> = Vec::new();
        let (mut n_methods, mut n_params) = (0usize, 0usize);

        for (name, iid_u128) in WANTED {
            let iid = GUID::from_u128(*iid_u128);
            let e_ti = vtable_iface(&word, &iid).expect("word vtable iface");
            let o_ti = ours
                .GetTypeInfoOfGuid(&iid)
                .unwrap_or_else(|_| panic!("{name}: missing from our typelib"));

            let es = sigs(&e_ti);
            let os = sigs(&o_ti);
            if es.len() != os.len() {
                fails.push(format!(
                    "{name}: vtable length {} (ours) != {} (Word)",
                    os.len(),
                    es.len()
                ));
                continue;
            }
            for (i, (e, o)) in es.iter().zip(os.iter()).enumerate() {
                n_methods += 1;
                if e.ovft != o.ovft {
                    fails.push(format!("{name} method#{i}: oVft {} != {}", o.ovft, e.ovft));
                }
                if e.params.len() != o.params.len() {
                    fails.push(format!(
                        "{name} method#{i} (slot {}): param count {} != {}",
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
                            "{name} method#{i} param#{j}: flags {:#x} != {:#x}",
                            op.0, ep.0
                        ));
                    }
                    if ep.1 != op.1 {
                        fails.push(format!(
                            "{name} method#{i} param#{j}: type '{}' != '{}'",
                            op.1, ep.1
                        ));
                    }
                }
                if e.ret != o.ret {
                    fails.push(format!("{name} method#{i}: return '{}' != '{}'", o.ret, e.ret));
                }
            }
        }

        eprintln!(
            "oracle check: {} interfaces, {n_methods} methods, {n_params} params compared",
            WANTED.len()
        );
        assert!(
            fails.is_empty(),
            "{} ABI mismatch(es) vs the Word oracle:\n{}",
            fails.len(),
            fails.join("\n")
        );
    }
}
