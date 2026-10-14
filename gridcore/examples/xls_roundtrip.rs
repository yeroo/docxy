//! Open a workbook (any format gridcore reads) and write it as an `.xls`
//! with the BIFF8 writer, checking the file's structure first. Used by
//! `scripts/check-xls-writer.ps1` (#874), which opens the result in Excel.
//!
//!     cargo run -p gridcore --example xls_roundtrip -- in.xls out.xls

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: xls_roundtrip <in> <out.xls>");
        return std::process::ExitCode::from(2);
    };
    let data = match std::fs::read(input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let opened = match gridcore::legacy::open_workbook_ex(&data) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{input}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let out = gridcore::legacy::write_xls(&opened.pkg);
    if let Err(e) = gridcore::legacy::check_xls(&out.bytes) {
        eprintln!("{input}: structure: {e}");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(output, &out.bytes) {
        eprintln!("{output}: {e}");
        return std::process::ExitCode::FAILURE;
    }
    for d in &opened.dropped {
        println!("dropped: {d}");
    }
    for l in &out.lost {
        println!("lost: {l}");
    }
    std::process::ExitCode::SUCCESS
}
