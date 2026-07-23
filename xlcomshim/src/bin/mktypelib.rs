//! mktypelib — author/register our own Excel-compatible type library so the
//! out-of-process LocalServer can marshal the shim's dual interfaces on a machine
//! with no Office. The logic is generic (`comshimcore::typelib`); this is just
//! Excel's spec: copy Excel's typelib {00020813} v1.x into our own {7B3F9E21-…}.
//!
//! Usage: mktypelib [out.tlb] | register [out.tlb] | unregister [out.tlb]

#[cfg(not(windows))]
fn main() {
    eprintln!("mktypelib only runs on Windows (needs the COM typelib APIs).");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use windows::core::GUID;
    comshimcore::typelib::run(&comshimcore::typelib::Spec {
        src_libid: GUID::from_u128(0x00020813_0000_0000_c000_000000000046),
        src_major: 1,
        docxy_libid: GUID::from_u128(0x7b3f9e21_4c1a_4e8b_a2d6_9f5c1e0b7a31),
        name: "DocxyExcel",
        version: (1, 0),
        wanted: &[
            ("_Application", 0x000208d5_0000_0000_c000_000000000046),
            ("Workbooks", 0x000208db_0000_0000_c000_000000000046),
            ("_Workbook", 0x000208da_0000_0000_c000_000000000046),
            ("Sheets", 0x000208d7_0000_0000_c000_000000000046),
            ("_Worksheet", 0x000208d8_0000_0000_c000_000000000046),
            ("Range", 0x00020846_0000_0000_c000_000000000046),
            ("Font", 0x0002084d_0000_0000_c000_000000000046),
            ("Interior", 0x00020870_0000_0000_c000_000000000046),
        ],
        disp_wanted: xlcomshim::DISP_IFACES,
        default_file: "docxy-excel.tlb",
    })
}
