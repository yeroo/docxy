//! mkwordtypelib — author/register our own Word-compatible type library so the
//! out-of-process LocalServer can marshal the shim's dual interfaces on a machine
//! with no Word. The logic is generic (`comshimcore::typelib`); this is just
//! Word's spec: copy Word's typelib {00020905} v8.x into our own {9C2F4A11-…}.
//!
//! Usage: mkwordtypelib [out.tlb] | register [out.tlb] | unregister [out.tlb]

#[cfg(not(windows))]
fn main() {
    eprintln!("mkwordtypelib only runs on Windows (needs the COM typelib APIs).");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use windows::core::GUID;
    comshimcore::typelib::run(&comshimcore::typelib::Spec {
        src_libid: GUID::from_u128(0x00020905_0000_0000_c000_000000000046),
        src_major: 8,
        docxy_libid: GUID::from_u128(0x9c2f4a11_7d33_4b6e_b1a4_2e7c8d5f0a92),
        name: "DocxyWord",
        version: (1, 0),
        wanted: &[
            ("_Application", 0x00020970_0000_0000_c000_000000000046),
            ("Documents", 0x0002096c_0000_0000_c000_000000000046),
            ("_Document", 0x0002096b_0000_0000_c000_000000000046),
            ("Selection", 0x00020975_0000_0000_c000_000000000046),
            ("Range", 0x0002095e_0000_0000_c000_000000000046),
            ("_Font", 0x00020952_0000_0000_c000_000000000046),
            ("_ParagraphFormat", 0x00020953_0000_0000_c000_000000000046),
        ],
        default_file: "docxy-word.tlb",
    })
}
