//! `xlsxy` — terminal viewer/**editor** for `.xlsx` workbooks.
//!
//! Usage:
//!   xlsxy                               open a new workbook, or XLSTART's first
//!   xlsxy <file.xlsx>                   open in the editor
//!   xlsxy <in.xlsx> --recalc <out>      headless: recalculate and save
//!   xlsxy <in.xlsx> --csv <out.csv>     headless: export the active sheet as CSV UTF-8
//!   xlsxy <in.xlsx> --pdf <out.pdf>     headless: print the active sheet to PDF
//!
//! The engine lives in the pure `gridcore` crate; this binary is the TUI
//! shell: a cell grid with Excel muscle memory (formula bar, A1 navigation,
//! range selection, ref-translating copy/paste) and a dependency-graph
//! recalculation on every edit.

use opccore::fsio::{export_atomic, write_atomic};
use std::path::{Path, PathBuf};

use std::io;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

mod backstage;
mod control;
mod mcp;
mod outlinedlg;
mod ribbon;
mod skill;
mod textdlg;

// Bring the trait's methods (`extensions`, `default_save_name`, …) into scope
// for the `impl backstage::BackstageHost for App` call sites below.
use backstage::BackstageHost as _;

use gridcore::comments::Comment;
use gridcore::docprops::{CustomProperty, CustomValue, DocProperties};
use gridcore::edit::{FillDir, fill_changes, replace_all_in_sheet};
use gridcore::engine::{Engine, PART_OF_ARRAY};
use gridcore::entry::{EntryCtx, entry_cell, entry_cell_ctx, entry_ctx, seed_text};
use gridcore::formula::{qualify_sheet_in_formula, translate_formula};
use gridcore::frame::Agg;
use gridcore::legacy::{SourceFormat, open_workbook as open_any};
use gridcore::model::{
    DataModel, MODEL_PART, ModelSpec, Relationship, model_part_xml, model_pivot, parse_model_part,
};
use gridcore::options::{EditOptions, EnterMove};
use gridcore::outline::{self, Axis, OutlineError};
use gridcore::sheet::{
    Align, Cell, CellValue, MAX_COLS, MAX_ROWS, NumFmt, Sheet, Xf, cell_name, col_name,
    date_unrepresentable, format_with, sheet_to_csv,
};
use gridcore::textio::{AutoConvert, TextParse};
use gridcore::xlsx::{SheetPackage, SpreadsheetKind, new_xlsx, save_xlsx_for_path};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RLine, Span as RSpan};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

/// `--csv`: the active sheet as CSV UTF-8 to `out`. Never over the import
/// source or the workbook's binding, nor over an import's `<stem>.xlsx`:
/// the binding moves past that name when it exists ([`import_binding`]),
/// and it is then the user's own workbook.
fn export_csv_headless(
    pkg: &SheetPackage,
    source: &str,
    import_source: Option<&str>,
    out: &str,
) -> io::Result<usize> {
    let wb = &pkg.workbook;
    let bytes = csv_utf8_bytes(&wb.sheets[wb.active_tab.min(wb.sheets.len() - 1)], wb);
    let sibling = import_source.map(|s| Path::new(s).with_extension("xlsx"));
    let source = match &sibling {
        Some(p) if opccore::fsio::same_file(p, Path::new(out)) => p.to_str().unwrap_or(source),
        _ => source,
    };
    export_bytes(source, import_source, out, &bytes)?;
    Ok(bytes.len())
}

/// A sheet as Excel's *CSV UTF-8 (Comma delimited)* file: a byte-order mark,
/// then CR LF records.
fn csv_utf8_bytes(sheet: &gridcore::sheet::Sheet, wb: &gridcore::sheet::Workbook) -> Vec<u8> {
    let csv = sheet_to_csv(sheet, &wb.styles, wb.date1904);
    gridcore::textio::encode(&csv, gridcore::textio::Encoding::Utf8Bom)
}

/// A print job as PDF bytes: `&D`/`&T` from the local clock, `&Z`/`&F`
/// from the workbook's `path`.
pub(crate) fn print_pdf(
    wb: &gridcore::sheet::Workbook,
    job: &gridcore::print::paginate::Job,
    path: &str,
) -> Result<(Vec<u8>, u32), gridcore::print::pdf::PrintError> {
    let pages = gridcore::print::paginate::paginate(wb, job);
    let serial = now_serial().unwrap_or(0.0);
    let fmt = |code: &str| {
        gridcore::numfmt::parse_format(code)
            .and_then(|f| f.format_number(serial, false))
            .unwrap_or_default()
    };
    let abs = std::path::absolute(Path::new(path)).unwrap_or_else(|_| Path::new(path).into());
    let opts = gridcore::print::pdf::PdfOptions {
        date: fmt("m/d/yyyy"),
        time: fmt("h:mm AM/PM"),
        path: abs
            .parent()
            .map(|d| format!("{}{}", d.display(), std::path::MAIN_SEPARATOR))
            .unwrap_or_default(),
        file: abs
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let bytes = gridcore::print::pdf::to_pdf(wb, &pages, &opts)?;
    Ok((bytes, pages.pages.len() as u32))
}

/// `--pdf`: the active sheet printed to `out`, never over the workbook or
/// its import source. A sheet with nothing to print writes no file.
fn export_pdf_headless(
    pkg: &SheetPackage,
    source: &str,
    import_source: Option<&str>,
    out: &str,
) -> Result<usize, String> {
    let wb = &pkg.workbook;
    let active = wb.active_tab.min(wb.sheets.len() - 1);
    let job =
        gridcore::print::paginate::Job::new(gridcore::print::paginate::What::ActiveSheets(vec![
            active,
        ]));
    let (bytes, _) = print_pdf(wb, &job, source).map_err(|e| e.to_string())?;
    export_bytes(source, import_source, out, &bytes).map_err(|e| e.to_string())?;
    Ok(bytes.len())
}

/// Write an export (CSV, PDF, …) to `out` atomically, never over the
/// workbook `source` or its import source: an imported text file stays
/// protected independently of the rebound .xlsx save path. Either path can
/// identify the destination through a filesystem alias.
fn export_bytes(
    source: &str,
    import_source: Option<&str>,
    out: &str,
    bytes: &[u8],
) -> io::Result<()> {
    export_atomic(
        Some(Path::new(export_source(source, import_source, out))),
        Path::new(out),
        bytes,
    )
}

/// The file an export to `out` must not replace: the imported text file
/// when `out` is it (under any alias), else the workbook's own file.
fn export_source<'a>(source: &'a str, import_source: Option<&'a str>, out: &str) -> &'a str {
    import_source
        .filter(|import| opccore::fsio::same_file(Path::new(import), Path::new(out)))
        .unwrap_or(source)
}

/// Where *Always create backup* keeps the previous version: beside the file,
/// as `Backup of <stem>.xlk` (dots in the stem are kept).
fn backup_path(dest: &Path) -> std::path::PathBuf {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    dest.with_file_name(format!("Backup of {stem}.xlk"))
}

/// Honour the package's *Always create backup* flag: keep the bytes the save
/// is about to replace. A missing destination means no previous version;
/// any other failure aborts the save before the file is touched. Read
/// failures are reported against the workbook, write failures against the
/// backup, so the status line names the real culprit.
fn keep_backup(dest: &Path) -> io::Result<()> {
    let bytes = match std::fs::read(dest) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(io::Error::new(
                e.kind(),
                format!("cannot read {}: {e}", dest.display()),
            ));
        }
    };
    let backup = backup_path(dest);
    let write_ctx = |e: io::Error| {
        io::Error::new(
            e.kind(),
            format!("cannot write backup {}: {e}", backup.display()),
        )
    };
    // Mirror the file's permissions so a private workbook stays private —
    // but always keep the backup owner-writable, or the next save could not
    // replace it (a 0444 book would give a 0444 backup that write_atomic then
    // refuses to open). Windows skips the mirroring: its only permission bit
    // is readonly, which would brick the backup the same way.
    #[cfg(unix)]
    let mut perms = match std::fs::metadata(dest) {
        Ok(meta) => meta.permissions(),
        Err(e) => {
            return Err(io::Error::new(
                e.kind(),
                format!("cannot read {}: {e}", dest.display()),
            ));
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        perms.set_mode(perms.mode() | 0o200);
        // Create the first backup already chmodded: a brand-new destination
        // would be umask-default until write_atomic's rename lands, briefly
        // world-readable. The empty file sends write_atomic down its
        // replacement path, which preserves this mode.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(perms.mode())
            .open(&backup)
        {
            Ok(_) => {}
            // An existing backup gets its mode mirrored below; write_atomic
            // replaces it on the permission-preserving path.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(write_ctx(e)),
        }
    }
    write_atomic(&backup, &bytes).map_err(write_ctx)?;
    #[cfg(unix)]
    std::fs::set_permissions(&backup, perms).map_err(write_ctx)?;
    Ok(())
}

fn is_delimited(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".csv") || lower.ends_with(".tsv")
}

/// Excel's *Save as type* list, in Excel's order.
const SAVE_TYPES: [backstage::SaveType; 29] = [
    backstage::SaveType {
        label: "Excel Workbook",
        ext: "xlsx",
    },
    backstage::SaveType {
        label: "Excel Macro-Enabled Workbook",
        ext: "xlsm",
    },
    backstage::SaveType {
        label: "Excel Binary Workbook",
        ext: "xlsb",
    },
    backstage::SaveType {
        label: "Excel 97-2003 Workbook",
        ext: "xls",
    },
    backstage::SaveType {
        label: "CSV UTF-8 (Comma delimited)",
        ext: "csv",
    },
    backstage::SaveType {
        label: "XML Data",
        ext: "xml",
    },
    backstage::SaveType {
        label: "Single File Web Page",
        ext: "mht",
    },
    backstage::SaveType {
        label: "Web Page",
        ext: "htm",
    },
    backstage::SaveType {
        label: "Excel Template",
        ext: "xltx",
    },
    backstage::SaveType {
        label: "Excel Macro-Enabled Template",
        ext: "xltm",
    },
    backstage::SaveType {
        label: "Excel 97-2003 Template",
        ext: "xlt",
    },
    backstage::SaveType {
        label: "Text (Tab delimited)",
        ext: "txt",
    },
    backstage::SaveType {
        label: "Unicode Text",
        ext: "txt",
    },
    backstage::SaveType {
        label: "XML Spreadsheet 2003",
        ext: "xml",
    },
    backstage::SaveType {
        label: "Microsoft Excel 5.0/95 Workbook",
        ext: "xls",
    },
    backstage::SaveType {
        label: "CSV (Comma delimited)",
        ext: "csv",
    },
    backstage::SaveType {
        label: "Formatted Text (Space delimited)",
        ext: "prn",
    },
    backstage::SaveType {
        label: "Text (Macintosh)",
        ext: "txt",
    },
    backstage::SaveType {
        label: "Text (MS-DOS)",
        ext: "txt",
    },
    backstage::SaveType {
        label: "CSV (Macintosh)",
        ext: "csv",
    },
    backstage::SaveType {
        label: "CSV (MS-DOS)",
        ext: "csv",
    },
    backstage::SaveType {
        label: "DIF (Data Interchange Format)",
        ext: "dif",
    },
    backstage::SaveType {
        label: "SYLK (Symbolic Link)",
        ext: "slk",
    },
    backstage::SaveType {
        label: "Excel Add-in",
        ext: "xlam",
    },
    backstage::SaveType {
        label: "Excel 97-2003 Add-in",
        ext: "xla",
    },
    backstage::SaveType {
        label: "PDF",
        ext: "pdf",
    },
    backstage::SaveType {
        label: "XPS Document",
        ext: "xps",
    },
    backstage::SaveType {
        label: "Strict Open XML Spreadsheet",
        ext: "xlsx",
    },
    backstage::SaveType {
        label: "OpenDocument Spreadsheet",
        ext: "ods",
    },
];

/// How a Save As type is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveKind {
    /// The workbook package (`.xlsx`, `.xlsm`, `.xltx`, `.xltm`).
    Package,
    /// The active sheet as delimited text.
    Text {
        delim: char,
        encoding: gridcore::textio::Encoding,
    },
    /// Formatted Text (Space delimited).
    Prn,
    /// Web Page: `.htm` plus a `_files` folder.
    WebPage,
    /// XML Data: needs the workbook's XML maps.
    XmlData,
    /// Listed as Excel lists it, but not written.
    Unsupported,
}

fn save_kind(t: &backstage::SaveType) -> SaveKind {
    use gridcore::textio::Encoding;
    match t.label {
        "Excel Workbook"
        | "Excel Macro-Enabled Workbook"
        | "Excel Template"
        | "Excel Macro-Enabled Template" => SaveKind::Package,
        "CSV UTF-8 (Comma delimited)" => SaveKind::Text {
            delim: ',',
            encoding: Encoding::Utf8Bom,
        },
        "CSV (Comma delimited)" => SaveKind::Text {
            delim: ',',
            encoding: Encoding::Windows1252,
        },
        "Text (Tab delimited)" => SaveKind::Text {
            delim: '\t',
            encoding: Encoding::Windows1252,
        },
        "Unicode Text" => SaveKind::Text {
            delim: '\t',
            encoding: Encoding::Utf16LeBom,
        },
        "Formatted Text (Space delimited)" => SaveKind::Prn,
        "Web Page" => SaveKind::WebPage,
        "XML Data" => SaveKind::XmlData,
        _ => SaveKind::Unsupported,
    }
}

/// The Save As type a typed file name's extension picks (the first type
/// with it, as the list is ordered): `.csv` is CSV UTF-8, `.txt` Text (Tab
/// delimited). `None` for an extension no type has.
fn type_for_path(path: &str) -> Option<usize> {
    let ext = type_ext(path)?;
    SAVE_TYPES.iter().position(|t| t.ext == ext)
}

/// `path`'s save type when a save can't write a workbook under it: an
/// `.xls`, `.xlsb`, `.ods` (no writer), an `.xml` (needs XML maps), and so
/// on. `None` for a type it writes, and for no or an unknown extension.
fn unwritable_type(path: &str) -> Option<&'static backstage::SaveType> {
    let t = &SAVE_TYPES[type_for_path(path)?];
    matches!(save_kind(t), SaveKind::Unsupported | SaveKind::XmlData).then_some(t)
}

/// The path a new workbook opened on the missing `path` is bound to:
/// `path` itself, unless a save can't write its type (`xlsxy new.xls`),
/// when it is [`import_binding`]'s `.xlsx`, as Excel starts a new workbook
/// as one. A save then never writes `.xlsx` bytes under an `.xls` name.
fn missing_binding(path: &str) -> String {
    match unwritable_type(path) {
        Some(_) => import_binding(path),
        None => path.to_string(),
    }
}

/// `path`'s extension as the type list spells it: lower case, with `.html`
/// and `.mhtml` the Web Page types' `.htm` and `.mht`.
fn type_ext(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "html" => "htm".to_string(),
        "mhtml" => "mht".to_string(),
        _ => ext,
    })
}

/// Does `path` carry type `t`'s extension (or its alias)?
fn has_type_ext(path: &str, t: usize) -> bool {
    type_ext(path).as_deref() == SAVE_TYPES.get(t).map(|ty| ty.ext)
}

/// A `.txt` or `.prn`: opened through the Text Import Wizard.
fn is_text_import(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".txt") || lower.ends_with(".prn")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--mcp` runs the headless MCP stdio bridge (a client of a running xlsxy),
    // not the editor, so handle it before the file-oriented argument parsing.
    if args.iter().any(|a| a == "--mcp") {
        return match mcp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mcp: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // `xlsxy install skill` writes the agent SKILL.md and exits.
    if args.first().map(String::as_str) == Some("install")
        && args.get(1).map(String::as_str) == Some("skill")
    {
        return match skill::install() {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("install skill: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            print_usage();
            return ExitCode::from(2);
        }
    };
    if parsed.help {
        print_usage();
        return ExitCode::SUCCESS;
    }

    if parsed.inputs.len() > 1 && !parsed.verify {
        eprintln!("error: more than one input file (only --verify takes several)");
        return ExitCode::from(2);
    }
    if let Some(err) = read_only_input_error(&parsed) {
        eprintln!("error: {err}");
        return ExitCode::from(2);
    }

    // --verify sweeps any number of workbooks and prints an aggregate.
    if parsed.verify {
        if parsed.inputs.is_empty() {
            eprintln!("error: --verify requires at least one input file");
            return ExitCode::from(2);
        }
        let mut agg = VerifyStats::default();
        for input in &parsed.inputs {
            let data = match std::fs::read(input) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: cannot read {input}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let pkg = match open_any(&data) {
                Ok((p, _)) => p,
                Err(e) => {
                    eprintln!("error: {input}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let (report, stats) = verify_report(&pkg, input);
            print!("{report}");
            agg.add(&stats);
        }
        if parsed.inputs.len() > 1 {
            print!("{}", agg.summary());
        }
        return if agg.mismatched == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // Friendly cross-suggestion for files that belong to the sibling app.
    if let Some(input) = parsed.inputs.first() {
        let lower = input.to_ascii_lowercase();
        if lower.ends_with(".docx") || lower.ends_with(".md") || lower.ends_with(".markdown") {
            eprintln!("{input} is a document, not a spreadsheet — try: docxy {input}");
            return ExitCode::from(2);
        }
    }

    let (pkg, path, import_source, format) = match parsed.inputs.first() {
        // A .txt/.prn in the editor opens the Text Import Wizard over a new
        // workbook (`run_tui` gets it as `wizard`).
        Some(input)
            if is_text_import(input) && parsed.recalc_out.is_none() && parsed.csv_out.is_none() =>
        {
            (new_xlsx(), "untitled.xlsx".to_string(), None, None)
        }
        // CSV/TSV, and a .txt/.prn in a headless run (the wizard's
        // defaults), import as a one-sheet workbook. Ctrl-S then writes
        // .xlsx: the path is rebound to a free name so a spreadsheet never
        // lands in a text file or on an existing workbook.
        Some(input) if is_delimited(input) || is_text_import(input) => {
            match load_workbook(input, &TextOpen::from_prefs()) {
                Ok(loaded) => loaded,
                Err(e) => {
                    eprintln!("error: cannot read {input}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some(input) => match std::fs::read(input) {
            Ok(data) => match open_any(&data) {
                // An .xls, .xlsb or .ods is an import, as a CSV is: bound to
                // the .xlsx beside it, so a save never writes over it.
                Ok((pkg, format)) if format.is_import() => (
                    pkg,
                    import_binding(input),
                    Some(input.clone()),
                    Some(format),
                ),
                // A template opens in the editor as a new workbook from it. A
                // headless run keeps the template as its source, so the
                // export guard still refuses to write over it.
                Ok((pkg, _)) => (
                    pkg,
                    template_binding(input)
                        .filter(|_| parsed.recalc_out.is_none() && parsed.csv_out.is_none())
                        .unwrap_or_else(|| input.clone()),
                    None,
                    None,
                ),
                Err(e) => {
                    eprintln!("error: {input}: {e}");
                    return ExitCode::FAILURE;
                }
            },
            // A nonexistent path opens a new workbook bound to it, or to a
            // free .xlsx when a save can't write its type (`new.xls`).
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                (new_xlsx(), missing_binding(input), None, None)
            }
            Err(e) => {
                eprintln!("error: cannot read {input}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            if parsed.recalc_out.is_some() || parsed.csv_out.is_some() || parsed.pdf_out.is_some() {
                eprintln!("error: headless modes (--recalc/--csv/--pdf) require an input file");
                return ExitCode::from(2);
            }
            (new_xlsx(), "untitled.xlsx".to_string(), None, None)
        }
    };

    if let Some(out) = parsed.recalc_out {
        // Read-only (#882): the input is never written, so recalculating in
        // place is refused rather than done.
        if let Some(input) = parsed.inputs.first().filter(|_| parsed.read_only)
            && opccore::fsio::same_file(Path::new(input), Path::new(&out))
        {
            eprintln!("error: {}", read_only_refusal(input));
            return ExitCode::from(2);
        }
        // Only a workbook package is written: an .xls, .xlsb, .ods, .xml (or
        // any type Save As refuses) would get .xlsx bytes under its name.
        if let Some(t) = unwritable_type(&out) {
            eprintln!(
                "error: cannot save as .{} ({}): write .xlsx instead",
                t.ext, t.label
            );
            return ExitCode::from(2);
        }
        let mut pkg = pkg;
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        engine.recalc_all(&mut pkg.workbook);
        // Refresh pivots from the recalculated data, then recalculate
        // anything that reads pivot output cells.
        let pivots = gridcore::pivot::refresh_pivots(&mut pkg.workbook);
        if !pivots.changed.is_empty() {
            engine.recalc_from(&mut pkg.workbook, &pivots.changed);
        }
        if pivots.refreshed + pivots.skipped > 0 {
            println!(
                "pivots: {} refreshed, {} kept on cached values",
                pivots.refreshed, pivots.skipped
            );
        }
        // The saved file's type follows `out`: a template or macro workbook
        // written as .xlsx must say it is a workbook, or Excel refuses it.
        if let Some(kind) = SpreadsheetKind::from_path(&out) {
            if !kind.allows_macros() {
                for feature in pkg.macro_features() {
                    eprintln!(
                        "note: {feature} not saved in macro-free .{} workbook",
                        kind.extension()
                    );
                }
            }
        }
        let bytes = save_xlsx_for_path(&pkg, &out);
        // Recalculation is an in-place save for .xlsx, but a conversion for
        // CSV/TSV input. Never replace the imported text file with an XLSX.
        if let Err(e) = export_atomic(
            import_source.as_deref().map(Path::new),
            Path::new(&out),
            &bytes,
        ) {
            eprintln!("error: cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out} ({} bytes)", bytes.len());
        return ExitCode::SUCCESS;
    }

    if let Some(out) = parsed.pdf_out {
        match export_pdf_headless(&pkg, &path, import_source.as_deref(), &out) {
            Ok(len) => println!("wrote {out} ({len} bytes)"),
            Err(e) => {
                eprintln!("error: {out}: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    if let Some(out) = parsed.csv_out {
        match export_csv_headless(&pkg, &path, import_source.as_deref(), &out) {
            Ok(len) => println!("wrote {out} ({len} bytes)"),
            Err(e) => {
                eprintln!("error: cannot write {out}: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    let welcome = parsed.inputs.is_empty();
    let wizard = parsed.inputs.first().filter(|i| is_text_import(i)).cloned();
    // The template a new workbook was started from, when the input was one.
    let template = parsed
        .inputs
        .first()
        .filter(|input| is_template(input) && **input != path)
        .cloned();
    let read_only = parsed.inputs.first().filter(|_| parsed.read_only).cloned();
    match run_tui(
        pkg,
        &path,
        import_source.map(|s| (s, format)),
        template,
        welcome,
        wizard,
        TuiFlags {
            vim: parsed.vim,
            read_only,
        },
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// How a text file opens: File › Options › Data's Automatic Data Conversion,
/// and the clock that gives a yearless date such as `1/2` its year.
#[derive(Clone, Copy, Debug)]
struct TextOpen {
    auto: AutoConvert,
    today: Option<f64>,
}

impl TextOpen {
    /// The persisted options (headless runs read them too) and now.
    fn from_prefs() -> TextOpen {
        let text = view_prefs_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        TextOpen {
            auto: auto_convert_from_prefs(&text),
            today: now_serial(),
        }
    }
}

/// The Automatic Data Conversion switches' preference keys, in the order
/// File › Options lists them.
pub(crate) const CONVERT_KEYS: [&str; 4] = [
    "convert_leading_zeros",
    "convert_long_numbers",
    "convert_e_notation",
    "convert_dates",
];

/// The Automatic Data Conversion switches from the preferences file's text;
/// a missing key keeps Excel's default (on).
fn auto_convert_from_prefs(text: &str) -> AutoConvert {
    let mut auto = AutoConvert::default();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let on = v.trim() == "1";
            let slot = match CONVERT_KEYS.iter().position(|key| *key == k.trim()) {
                Some(0) => &mut auto.remove_leading_zeros,
                Some(1) => &mut auto.keep_15_digits,
                Some(2) => &mut auto.e_notation,
                Some(3) => &mut auto.dates,
                _ => continue,
            };
            *slot = on;
        }
    }
    auto
}

/// Load a workbook from disk, returning the package, its save path, the
/// imported file's path when it was an import, and the format of an
/// imported `.xls`/`.xlsb`/`.ods` (`None` for everything else). An `.xlsx`
/// (or other package) loads as it is; a `.csv`/`.tsv` imports as one sheet
/// as Excel opens it (`sep=`, typed-entry conversion); a `.txt`/`.prn`
/// imports with the Text Import Wizard's defaults (the editor shows the
/// wizard instead, see `App::open_workbook`). An `.xls`, `.xlsb` or `.ods`
/// (read by its bytes, whatever its name) imports through
/// `gridcore::legacy`. Every import, text or workbook, is bound to
/// [`import_binding`]: `<name>.xlsx`, or the next free numbered name. A
/// template (`.xltx`, `.xltm`) opens as a new workbook from it, as Excel
/// starts one: bound to [`template_binding`], so a save never writes the
/// template itself.
fn load_workbook(
    path: &str,
    open: &TextOpen,
) -> Result<(SheetPackage, String, Option<String>, Option<SourceFormat>), String> {
    if is_text_import(path) {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let text = gridcore::textio::decode(&bytes, gridcore::textio::Origin::Auto);
        return Ok((
            text_to_pkg(&text, &file_stem(path), &TextParse::default(), open),
            import_binding(path),
            Some(path.to_string()),
            None,
        ));
    }
    if is_delimited(path) {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let text = gridcore::textio::decode(&bytes, gridcore::textio::Origin::Auto);
        let tab = path.to_ascii_lowercase().ends_with(".tsv");
        let stem = file_stem(path);
        Ok((
            csv_to_pkg(&text, &stem, tab, open),
            import_binding(path),
            Some(path.to_string()),
            None,
        ))
    } else {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let (pkg, format) = open_any(&data).map_err(|e| e.to_string())?;
        if format.is_import() {
            return Ok((
                pkg,
                import_binding(path),
                Some(path.to_string()),
                Some(format),
            ));
        }
        let save = template_binding(path).unwrap_or_else(|| path.to_string());
        Ok((pkg, save, None, None))
    }
}

/// The `.xlsx` an imported workbook is bound to: `book.xls` gives
/// `book.xlsx` beside it, or, when that exists (or is the imported file
/// itself, an `.xls` named `.xlsx`), `book1.xlsx`, `book2.xlsx` and so on,
/// as [`template_binding`] numbers them. A save never lands on a file the
/// user already has.
fn import_binding(path: &str) -> String {
    let source = Path::new(path);
    let free = |p: &Path| p != source && !p.exists();
    let plain = source.with_extension("xlsx");
    if free(&plain) {
        return plain.to_string_lossy().into_owned();
    }
    let base = source.with_extension("");
    let base = base.to_string_lossy();
    (1u32..)
        .map(|n| format!("{base}{n}.xlsx"))
        .find(|p| free(Path::new(p)))
        .unwrap_or_else(|| plain.to_string_lossy().into_owned())
}

/// Whether `path` names a template (`.xltx`, `.xltm`).
fn is_template(path: &str) -> bool {
    matches!(
        SpreadsheetKind::from_path(path),
        Some(SpreadsheetKind::Template | SpreadsheetKind::MacroTemplate)
    )
}

/// The path a new workbook started from template `path` is bound to, as
/// Excel names it: `Budget.xltx` gives `Budget1.xlsx` beside it, or
/// `Budget2.xlsx` when that exists, and so on; an `.xltm` gives an `.xlsm`.
/// `None` when `path` is not a template.
fn template_binding(path: &str) -> Option<String> {
    let ext = match SpreadsheetKind::from_path(path)? {
        SpreadsheetKind::Template => "xlsx",
        SpreadsheetKind::MacroTemplate => "xlsm",
        _ => return None,
    };
    // The extension is `.xltx`/`.xltm`, five ASCII bytes.
    let base = &path[..path.len() - 5];
    (1u32..)
        .map(|n| format!("{base}{n}.{ext}"))
        .find(|p| !Path::new(p).exists())
}

/// Swap `items[sel]` with its neighbor. Returns false at the edges.
fn swap_entry<T>(items: &mut [T], sel: usize, up: bool) -> bool {
    if up {
        if sel == 0 || sel >= items.len() {
            return false;
        }
        items.swap(sel - 1, sel);
    } else {
        if sel + 1 >= items.len() {
            return false;
        }
        items.swap(sel, sel + 1);
    }
    true
}

/// `Sales[ProductID]` → ("Sales", "ProductID").
fn parse_table_col(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let open = s.find('[')?;
    let close = s.rfind(']')?;
    if close != s.len() - 1 || open == 0 || close <= open + 1 {
        return None;
    }
    Some((
        s[..open].trim().to_string(),
        s[open + 1..close].trim().to_string(),
    ))
}

/// Import CSV text as a fresh one-sheet workbook, as Excel opens a `.csv`: a
/// `sep=` first line names the delimiter (else a `.tsv` is tab-delimited and
/// a `.csv` sniffed), and every field converts as if typed into its cell.
fn csv_to_pkg(text: &str, sheet_name: &str, tab: bool, open: &TextOpen) -> SheetPackage {
    let (opts, body) = csv_parse(text, tab);
    text_to_pkg(body, sheet_name, &opts, open)
}

/// The file name without its extension (a new sheet's name).
fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "import".to_string())
}

/// Delimited or fixed-width text as a fresh one-sheet workbook, read under
/// `opts` (the Text Import Wizard's choices).
fn text_to_pkg(text: &str, sheet_name: &str, opts: &TextParse, open: &TextOpen) -> SheetPackage {
    let mut pkg = new_xlsx();
    let wb = &mut pkg.workbook;
    if !sheet_name.is_empty() {
        wb.sheets[0].name = sheet_name.chars().take(31).collect();
    }
    import_text(&mut wb.sheets[0], &mut wb.styles, text, opts, open, false);
    // A text file carries no cached values: work out its formulas now.
    let mut engine = Engine::new(&pkg.workbook);
    engine.clock = open.today;
    engine.recalc_all(&mut pkg.workbook);
    pkg
}

/// How a CSV reads: a `sep=` first line names the delimiter (and is
/// dropped), else a `.tsv` is tab-delimited and a `.csv` sniffed. Returns
/// the options and the text after any directive.
fn csv_parse(text: &str, tab: bool) -> (TextParse, &str) {
    let (directive, body) = gridcore::textio::csv_directive(text);
    let delim = directive.unwrap_or_else(|| {
        if tab {
            '\t'
        } else {
            gridcore::frame::sniff_delimiter(body)
        }
    });
    (TextParse::csv(delim), body)
}

/// Text converted into `sheet` (of a workbook in the 1904 date system when
/// `date1904`) from A1 under `opts`. Returns (rows, cols).
fn import_text(
    sheet: &mut gridcore::sheet::Sheet,
    styles: &mut gridcore::sheet::Styles,
    text: &str,
    opts: &TextParse,
    open: &TextOpen,
    date1904: bool,
) -> (u32, u32) {
    let records = gridcore::textio::split_text(text, opts);
    let ctx = gridcore::entry::EntryCtx {
        date1904,
        today: open.today,
        fixed_decimal: None,
    };
    gridcore::textio::import_records(sheet, styles, 0, 0, &records, opts, &open.auto, &ctx)
}

struct Parsed {
    inputs: Vec<String>,
    recalc_out: Option<String>,
    csv_out: Option<String>,
    pdf_out: Option<String>,
    verify: bool,
    help: bool,
    vim: bool,
    /// `--read-only` / `-r` (#882): the input is never written.
    read_only: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed {
        inputs: Vec::new(),
        recalc_out: None,
        csv_out: None,
        pdf_out: None,
        verify: false,
        help: false,
        vim: false,
        read_only: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => p.help = true,
            "--vim" => p.vim = true,
            "--read-only" | "-r" => p.read_only = true,
            "--verify" => p.verify = true,
            "--recalc" => {
                i += 1;
                p.recalc_out = Some(args.get(i).ok_or("--recalc needs an output path")?.clone());
            }
            "--csv" => {
                i += 1;
                p.csv_out = Some(args.get(i).ok_or("--csv needs an output path")?.clone());
            }
            "--pdf" => {
                i += 1;
                p.pdf_out = Some(args.get(i).ok_or("--pdf needs an output path")?.clone());
            }
            // A lone "-" is a filename-ish token, not an option; reject it
            // explicitly (stdin isn't supported) rather than as "unknown -".
            "-" => return Err("stdin (\"-\") is not supported; pass a file path".to_string()),
            a if a.starts_with('-') => return Err(format!("unknown option {a}")),
            a => p.inputs.push(a.to_string()),
        }
        i += 1;
    }
    // The headless modes are mutually exclusive — silently dropping one would
    // surprise; reject the combination instead.
    let modes = usize::from(p.recalc_out.is_some())
        + usize::from(p.csv_out.is_some())
        + usize::from(p.pdf_out.is_some())
        + usize::from(p.verify);
    if modes > 1 {
        return Err("choose only one of --recalc, --csv, --pdf, --verify".to_string());
    }
    Ok(p)
}

fn print_usage() {
    eprintln!(
        "Xlsxy — terminal .xlsx spreadsheet editor with a real calc engine\n\n\
         USAGE:\n  \
           xlsxy                            new workbook (or the first workbook in XLSTART)\n  \
           xlsxy <file.xlsx>                open a workbook\n  \
           xlsxy <file.csv|.tsv>            import CSV/TSV as a new workbook\n  \
           xlsxy <file.txt|.prn>            import text through the Text Import Wizard\n  \
           xlsxy <in> --recalc <out.xlsx>   recalculate all formulas, save, exit\n  \
           xlsxy <in> --csv <out.csv>       export the active sheet as CSV UTF-8, exit\n  \
           xlsxy <in> --pdf <out.pdf>       print the active sheet to PDF, exit\n  \
           xlsxy <in> --verify              conformance scoreboard: recalculate\n  \
                                            and diff against Excel's cached values\n  \
           xlsxy <file> --vim               modal (vim) navigation: hjkl, v, dd, :w :q\n  \
           xlsxy <file> --read-only (-r)    open read-only: Save asks for a new name\n  \
           xlsxy --mcp                      run the MCP bridge to drive a live xlsxy\n  \
           xlsxy install skill              install the agent SKILL.md (self-onboarding)\n\n\
         EDITOR KEYS:\n  \
           type to replace · F2 edit in place · = starts a formula\n  \
           Enter/Tab commit (move down/right; File › Options sets Enter's way)\n  \
           Esc cancel · Del clear · Ctrl-Shift-U expand the formula bar\n  \
           arrows / PgUp / PgDn move   (Ctrl-arrows jump to data edge)\n  \
           Shift + move select a range   (stats appear in the status bar)\n  \
           Ctrl-C copy   Ctrl-X cut   Ctrl-V paste (relative refs translate)\n  \
           Ctrl-Z undo   Ctrl-Y redo   Ctrl-S save   Ctrl-N new   Ctrl-Q quit\n  \
           Ctrl-PgUp/PgDn or click tabs to switch sheets\n  \
           Ctrl-D/Ctrl-R fill down/right   Ctrl-F find (F3 next)\n  \
           F5 insert rows  Shift-F5 delete rows  F6/Shift-F6 same for columns\n  \
           Ctrl-T add sheet  Shift-F2 rename sheet  Shift-Del delete sheet\n  \
           F12 Save As   F7 / F8 shrink / widen the current column\n  \
           mouse: click to move · drag to select · double-click to edit · wheel to scroll"
    );
}

/// Current local time as an Excel serial: Excel's `TODAY()`/`NOW()`, and
/// the year a typed `3/4` takes, follow the local clock.
fn now_serial() -> Option<f64> {
    gridcore::clock::local_now_serial()
}

fn entropy_seed() -> Option<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as u64 | 1)
}

/// Current UTC time as an ISO-8601 string for threaded-comment timestamps.
/// Falls back to the Excel epoch if the clock is unavailable.
fn iso_now() -> String {
    let serial = gridcore::clock::utc_now_serial().unwrap_or(1.0);
    match gridcore::sheet::serial_to_parts(serial, false) {
        Some(p) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            p.year, p.month, p.day, p.hour, p.minute, p.second
        ),
        None => "1899-12-31T00:00:00Z".to_string(),
    }
}

/// Render the first sheet of a workbook (or a CSV) as preview text lines,
/// bounded so a huge file can't stall the browser.
fn preview_lines(path: &str, width: usize) -> Vec<String> {
    let (pkg, ..) = match load_workbook(path, &TextOpen::from_prefs()) {
        Ok(x) => x,
        Err(e) => return vec![format!("(cannot preview: {e})")],
    };
    let sheet = &pkg.workbook.sheets[0];
    let styles = &pkg.workbook.styles;
    let d1904 = pkg.workbook.date1904;
    let (rows, cols) = sheet.used_size();
    let rows = rows.min(40);
    let cols = cols.clamp(1, 12);
    let colw = ((width.saturating_sub(1)) / cols as usize).clamp(4, 16);
    let mut out = Vec::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..cols {
            let text = sheet
                .cell(r, c)
                .map(|cl| format_with(&styles.xf(cl.style), &cl.value, d1904))
                .unwrap_or_default();
            line.push_str(&fit(&text, colw, false));
        }
        out.push(line.trim_end().to_string());
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        out.push("(empty)".to_string());
    }
    out
}

/// The terminal window title: `* AppName - filename` (the `* ` only when the
/// file has unsaved changes), and Excel's ` [Read-Only]` while the file is
/// one opened read-only (#882).
fn window_title(app: &str, path: &str, modified: bool, read_only: bool) -> String {
    let name = file_name_of(path);
    format!(
        "{}{app} - {name}{}",
        if modified { "* " } else { "" },
        if read_only { " [Read-Only]" } else { "" }
    )
}

/// Why `--read-only` cannot open its input (#882): there is none, or it is
/// not a file. Only an existing file can be kept from being written; a
/// missing one would be created by the first Save.
fn read_only_input_error(parsed: &Parsed) -> Option<String> {
    if !parsed.read_only {
        return None;
    }
    let Some(input) = parsed.inputs.first() else {
        return Some("--read-only needs a file to open".to_string());
    };
    (!Path::new(input).is_file()).then(|| format!("cannot open {input} read-only: no such file"))
}

/// Excel's refusal to write a file opened read-only (#882).
fn read_only_refusal(path: &str) -> String {
    format!(
        "\"{}\" is read-only. Save a copy under a new name.",
        file_name_of(path)
    )
}

/// `path`'s file name, or `path` itself when it has none.
fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// xlsxy's configuration folder (XDG / APPDATA).
fn config_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(dir.join("xlsxy"))
}

/// Path of the persisted view-preferences file.
fn view_prefs_path() -> Option<PathBuf> {
    Some(config_dir()?.join("view.conf"))
}

/// xlsxy's XLSTART folder: its `book.xltx` is the template a new workbook
/// starts from, and its workbooks open at launch. xlsxy's own, not Excel's
/// `%APPDATA%\Microsoft\Excel\XLSTART`, which usually holds `PERSONAL.XLSB`
/// (a hidden macro workbook) that would otherwise become the window's only
/// workbook on every launch.
fn xlstart_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("XLSTART"))
}

/// The files a startup folder opens: workbooks xlsxy reads without a
/// dialog. Templates are not opened, and a `.txt`/`.prn` would open the Text
/// Import Wizard at launch.
const STARTUP_EXTENSIONS: &[&str] = &["xlsx", "xlsm", "xlsb", "xls", "ods", "csv", "tsv"];

/// The workbooks to open at launch, as Excel opens them: those in the
/// XLSTART folder, then those in the alternate startup folder (File ›
/// Options › Advanced › *At startup, open all files in*), each folder's
/// sorted by name. Not templates, not lock files (`~$book.xlsx`) or hidden
/// files, not folders; a file in both folders (the alternate folder may be
/// XLSTART itself) is listed once. A folder that is missing is skipped.
fn startup_workbooks(dirs: &[&Path]) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_startup_workbook(p))
            .collect();
        files.sort_by_key(|p| file_name_of(&p.to_string_lossy()).to_lowercase());
        for file in files {
            let key = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
            if !seen.contains(&key) {
                seen.push(key);
                out.push(file);
            }
        }
    }
    out
}

/// Whether a startup folder opens `path` (see [`startup_workbooks`]).
fn is_startup_workbook(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    !name.starts_with("~$")
        && !name.starts_with('.')
        && path.is_file()
        && path
            .extension()
            .is_some_and(|e| STARTUP_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The template a new workbook starts from: `book.xltx` in `dir` (the
/// XLSTART folder), its name matched in any case.
fn default_template(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("book.xltx"))
                && p.is_file()
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Parse an A1 cell reference (`A1`, `$A$1`) into 0-based (row, col).
fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().trim_start_matches('$');
    let (col, used) = gridcore::sheet::parse_col(s)?;
    let row: u32 = s[used..].trim_start_matches('$').parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col))
}

/// The author name stamped on new comments — the OS user, else "xlsxy".
fn comment_author() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "xlsxy".to_string())
}

/// The conformance oracle: every formula cell in a real .xlsx carries the
/// value Excel last computed. Recalculate everything with our engine and
/// diff — the resulting scoreboard measures calculation fidelity on real
/// workbooks and catches semantic regressions.
/// Aggregate scoreboard counters across a multi-file `--verify` sweep.
#[derive(Default)]
struct VerifyStats {
    files: usize,
    total: usize,
    compared: usize,
    matched: usize,
    mismatched: usize,
    unsupported: usize,
    volatile: usize,
}

impl VerifyStats {
    fn add(&mut self, other: &VerifyStats) {
        self.files += 1;
        self.total += other.total;
        self.compared += other.compared;
        self.matched += other.matched;
        self.mismatched += other.mismatched;
        self.unsupported += other.unsupported;
        self.volatile += other.volatile;
    }

    fn summary(&self) -> String {
        let pct = if self.compared > 0 {
            self.matched as f64 / self.compared as f64 * 100.0
        } else {
            100.0
        };
        format!(
            "TOTAL: {} files, {} formula cells\n  \
             matched      {}/{} ({pct:.1}%)\n  \
             mismatched   {}\n  \
             unsupported  {}\n  \
             volatile     {}\n",
            self.files,
            self.total,
            self.matched,
            self.compared,
            self.mismatched,
            self.unsupported,
            self.volatile
        )
    }
}

fn verify_report(pkg: &SheetPackage, path: &str) -> (String, VerifyStats) {
    use gridcore::formula::{is_volatile, parse};
    use gridcore::sheet::Workbook;

    let original: &Workbook = &pkg.workbook;
    let mut wb = pkg.workbook.clone();
    let mut engine = Engine::new(&wb);
    // Deliberately give the engine *no* clock or RNG: volatile cells
    // (NOW/TODAY/RAND) then keep their cached values instead of being
    // recomputed to a fresh moment, so their non-volatile dependents
    // (e.g. `=A1*2` where A1 is `=NOW()`) recompute from the cached inputs
    // and still agree with Excel's cache — no false mismatches.
    engine.recalc_all(&mut wb);

    let mut total = 0usize;
    let mut matched = 0usize;
    let mut unsupported = 0usize;
    let mut volatile = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    for (s, sheet) in original.sheets.iter().enumerate() {
        for (&(r, c), cell) in &sheet.cells {
            let Some(src) = &cell.formula else { continue };
            total += 1;
            // Report volatiles before unsupported: with no clock they also
            // read as unsupported, but "volatile" is the meaningful label.
            if parse(src).map(|ast| is_volatile(&ast)).unwrap_or(false) {
                volatile += 1;
                continue;
            }
            if engine.is_unsupported((s, r, c)) {
                unsupported += 1;
                continue;
            }
            let expected = &cell.value;
            let got = wb.sheets[s]
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or(CellValue::Empty);
            if values_agree(expected, &got) {
                matched += 1;
            } else if mismatches.len() < 20 {
                mismatches.push(format!(
                    "  {}!{}: ={src}\n    excel: {expected:?}\n    ours:  {got:?}",
                    sheet.name,
                    cell_name(r, c)
                ));
            }
        }
    }

    let compared = total - unsupported - volatile;
    let mismatched = compared - matched;
    let stats = VerifyStats {
        files: 0,
        total,
        compared,
        matched,
        mismatched,
        unsupported,
        volatile,
    };
    let pct = if compared > 0 {
        matched as f64 / compared as f64 * 100.0
    } else {
        100.0
    };
    let mut out = format!(
        "{path}: {total} formula cells\n  \
         matched      {matched}/{compared} ({pct:.1}%)\n  \
         mismatched   {mismatched}\n  \
         unsupported  {unsupported} (kept Excel's cached values)\n  \
         volatile     {volatile} (excluded: time/random dependent)\n"
    );
    if !mismatches.is_empty() {
        out.push_str("mismatches (first 20):\n");
        for m in &mismatches {
            out.push_str(m);
            out.push('\n');
        }
    }
    (out, stats)
}

/// Cached-vs-recomputed comparison: numbers within 1e-9 relative tolerance
/// (Excel stores ~15 significant digits), everything else exact.
fn values_agree(a: &CellValue, b: &CellValue) -> bool {
    match (a, b) {
        (CellValue::Number(x), CellValue::Number(y)) => {
            let scale = x.abs().max(y.abs()).max(1.0);
            (x - y).abs() <= 1e-9 * scale
        }
        // A formula whose cache was never written compares as 0 (Excel
        // writes 0 for untouched formula results).
        (CellValue::Empty, CellValue::Number(n)) | (CellValue::Number(n), CellValue::Empty) => {
            *n == 0.0
        }
        _ => a == b,
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// In-cell editing state. `replace` distinguishes Excel's two modes: typing
/// over a cell (arrows commit + move) vs F2 (arrows move inside the text).
struct EditState {
    text: String,
    cursor: usize, // char index
    replace: bool,
    /// The cell's text the editor opened on (F2, Enter-to-edit), `None` when
    /// typing replaced it. Committing it unchanged leaves the cell alone:
    /// re-reading it would round a 17-digit number to Excel's 15.
    seed: Option<String>,
    /// AutoComplete's proposal (#672): the char index its selected suffix
    /// starts at (the caret stays there) and the value it completes to. Any
    /// commit takes the value; Backspace or Delete drops the suffix; a caret
    /// move keeps the text as typed-plus-suffix and drops the marker.
    proposal: Option<(usize, String)>,
}

/// How soon a second press on the same cell makes a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// New cell contents by (row, col), applied in order.
type CellChanges = Vec<(u32, u32, Cell)>;

/// One sheet's share of an undoable cell action: cell states before/after,
/// per address.
struct UndoGroup {
    sheet: usize,
    changes: Vec<(u32, u32, Option<Cell>, Option<Cell>)>,
    /// A restyle ([`App::apply_styles_on`]): undo/redo put back only each
    /// cell's style, so a spill the cells belong to stays whole.
    styles_only: bool,
}

/// The `(row, col, style)` a restyle group puts back: the style of each
/// change's `before` or `after` cell (picked by `side`), default if absent.
fn group_styles(
    group: &UndoGroup,
    side: impl Fn(&(u32, u32, Option<Cell>, Option<Cell>)) -> &Option<Cell>,
) -> Vec<(u32, u32, u32)> {
    group
        .changes
        .iter()
        .map(|ch| (ch.0, ch.1, side(ch).as_ref().map_or(0, |cl| cl.style)))
        .collect()
}

/// The `(row, col, cell)` an undo or redo puts back: each change's `before`
/// or `after` cell (picked by `side`), blank if absent.
fn group_cells<'a>(
    changes: impl Iterator<Item = &'a (u32, u32, Option<Cell>, Option<Cell>)>,
    side: impl Fn(&(u32, u32, Option<Cell>, Option<Cell>)) -> &Option<Cell>,
) -> Vec<(u32, u32, Cell)> {
    changes
        .map(|ch| (ch.0, ch.1, side(ch).clone().unwrap_or_default()))
        .collect()
}

/// The workbook state around a structural edit whose inverse is not
/// expressible as per-cell changes (row/column insert-delete, sheet rename,
/// the table commands — Table Name, Resize Table, Convert to Range): sheets,
/// defined names, the tables (live, and converted ones whose parts await the
/// save), the PivotTables' sources, and a table rename to replay on the data
/// model.
#[derive(Clone)]
struct WbSnapshot {
    sheets: Vec<gridcore::sheet::Sheet>,
    names: Vec<gridcore::sheet::DefinedName>,
    /// Tables move with row edits and change with the table commands; the
    /// converted ones keep their parts until a save.
    tables: Vec<gridcore::sheet::Table>,
    removed_tables: Vec<gridcore::sheet::RemovedTable>,
    /// Each PivotTable's source: a table rename moves it.
    pivot_sources: Vec<gridcore::pivot::PivotSource>,
    /// A table rename (from, to) to replay on the data model when this
    /// snapshot is restored. Model edits are not on the undo stack, so undo
    /// renames the tables the *current* model names rather than putting back
    /// a copy, which would take later model edits with it.
    model_rename: Option<(String, String)>,
}

enum UndoAction {
    /// One undo step; more than one group when it touched several sheets
    /// (a cut pasted on another sheet). The view follows the last group.
    Cells(Vec<UndoGroup>),
    Structural {
        before: WbSnapshot,
        after: WbSnapshot,
    },
}

/// What the minibuffer prompt is collecting.
#[derive(PartialEq, Clone, Copy)]
enum PromptKind {
    Find,
    SaveAs,
    RenameSheet,
    AddSheet,
    /// Table Name: a new name for the table under the cursor.
    RenameTable,
    /// Resize Table: a new range (`A1:D20`) for the table under the cursor.
    ResizeTable,
    /// `Sales[ProductID] = Products[ID]` — add a model relationship.
    Relate,
    /// `Total = SUM(Sales[Amount])` — add a model measure.
    Measure,
    /// `Sales; Groups[Category]; Total[; Products[Name]]` — build a report.
    ModelPivot,
    /// The body text of a new threaded comment/reply on the current cell.
    NewComment,
    /// The body text of a new legacy note on the current cell.
    NewNote,
    /// Find & Replace: the search text, then the replacement.
    ReplaceFind,
    ReplaceWith,
    /// Go To: a cell reference or defined name to jump to.
    GoTo,
    /// Conditional formatting: a comparison like ">500" applied to the selection.
    CondFormat,
    /// Data validation: comma-separated allowed values → a dropdown list.
    DataValidation,
    /// AutoFilter: a criteria on the current column ("=Laptop", ">500", "clear").
    Filter,
    /// Multi-level sort: a spec like "B asc, C desc" over the current region.
    SortKeys,
    /// Row height in points for the selected rows ("auto" clears it).
    RowHeight,
    /// File › Info: a new value for editable property `n` ([`INFO_FIELDS`]),
    /// or, past them, `Name = value` for a custom property.
    DocProperty(u8),
}

/// File › Info's editable document properties, in display order:
/// (row label, prompt label).
const INFO_FIELDS: [(&str, &str); 8] = [
    ("Title", "Title: "),
    ("Tags", "Tags: "),
    ("Categories", "Categories: "),
    ("Subject", "Subject: "),
    ("Comments", "Comments: "),
    ("Company", "Company: "),
    ("Manager", "Manager: "),
    ("Hyperlink base", "Hyperlink base: "),
];

/// The prompt File › Info's `Custom property…` row opens.
const CUSTOM_PROPERTY_PROMPT: &str = "Custom property  Name = value (empty value removes): ";

/// The property behind [`INFO_FIELDS`] row `i`.
fn info_field(p: &mut DocProperties, i: usize) -> Option<&mut Option<String>> {
    Some(match i {
        0 => &mut p.title,
        1 => &mut p.keywords,
        2 => &mut p.category,
        3 => &mut p.subject,
        4 => &mut p.description,
        5 => &mut p.company,
        6 => &mut p.manager,
        7 => &mut p.hyperlink_base,
        _ => return None,
    })
}

struct Prompt {
    kind: PromptKind,
    label: &'static str,
    text: String,
    cursor: usize,
}

/// Vim editing modes for the grid (`--vim`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VimMode {
    Normal,
    Visual,
    VisualLine,
}

/// Vim-mode state: current mode, a pending multi-key prefix (`g`, `d`, `y`),
/// and the `:` command line while it's being typed.
struct VimState {
    mode: VimMode,
    pending: char,
    cmdline: Option<String>,
}

/// Which formatting popup is open.
#[derive(Clone, Copy, PartialEq)]
enum PickKind {
    NumberFormat,
    FontColor,
    FillColor,
}

/// A formatting popup: a scrollable list applied to the selection.
struct FormatPicker {
    kind: PickKind,
    sel: usize,
}

/// The consolidated "Format Cells" dialog (Ctrl+1): sectioned tabs (Number /
/// Font / Fill / Align / Border) with a highlighted option per section.
struct FormatDialog {
    section: usize,
    sel: usize,
}

/// Status for an edit the workbook's file can't take (a malformed worksheet
/// part where the edit would go): nothing changed, so nothing to save or undo.
const WRITE_REFUSED: &str =
    "Can't add that here: this sheet's XML is damaged where it would go (nothing changed)";

/// Excel's warning when an edit creates a circular reference.
const CIRCULAR_WARNING: &str = "There are one or more circular references where a formula refers to its own cell either directly or indirectly. This might cause them to calculate incorrectly.";

/// The Format Cells section tabs.
const FMT_SECTIONS: &[&str] = &["Number", "Font", "Fill", "Align", "Border"];

/// Parse a conditional-format comparison into an Excel cellIs operator plus one
/// or two operands: ">500", "<=100", "=42" (default greaterThan), or a between
/// range "100..500".
fn parse_cf_input(s: &str) -> Option<(&'static str, String, Option<String>)> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once("..") {
        let (a, b) = (a.trim(), b.trim());
        if !a.is_empty() && !b.is_empty() {
            return Some(("between", a.to_string(), Some(b.to_string())));
        }
    }
    let (op, rest) = if let Some(r) = s.strip_prefix(">=") {
        ("greaterThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<=") {
        ("lessThanOrEqual", r)
    } else if let Some(r) = s.strip_prefix("<>") {
        ("notEqual", r)
    } else if let Some(r) = s.strip_prefix('>') {
        ("greaterThan", r)
    } else if let Some(r) = s.strip_prefix('<') {
        ("lessThan", r)
    } else if let Some(r) = s.strip_prefix('=') {
        ("equal", r)
    } else {
        ("greaterThan", s)
    };
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some((op, rest.to_string(), None))
    }
}

/// Number-format options offered by the picker: (label, format code).
const NUMFMT_OPTIONS: &[(&str, Option<&str>)] = &[
    ("General", None),
    ("Number  0", Some("0")),
    ("Number  0.00", Some("0.00")),
    ("Thousands  #,##0", Some("#,##0")),
    ("Thousands  #,##0.00", Some("#,##0.00")),
    ("Percent  0%", Some("0%")),
    ("Percent  0.00%", Some("0.00%")),
    ("Currency  $#,##0.00", Some("$#,##0.00")),
    ("Scientific  0.00E+00", Some("0.00E+00")),
    ("Date  m/d/yyyy", Some("m/d/yyyy")),
    ("Time  h:mm:ss", Some("h:mm:ss")),
    ("Text  @", Some("@")),
];

/// An (r, g, b) color.
type Rgb = (u8, u8, u8);

/// Color palette offered by the font/fill pickers: (label, rgb).
const COLOR_OPTIONS: &[(&str, Option<Rgb>)] = &[
    ("Automatic", None),
    ("Black", Some((0, 0, 0))),
    ("White", Some((255, 255, 255))),
    ("Red", Some((192, 0, 0))),
    ("Orange", Some((237, 125, 49))),
    ("Yellow", Some((255, 255, 0))),
    ("Green", Some((0, 128, 0))),
    ("Blue", Some((0, 112, 192))),
    ("Purple", Some((112, 48, 160))),
    ("Gray", Some((128, 128, 128))),
];

/// The pivot field editor's state: which pivot, which pane (0 = available
/// fields, 1 = rows, 2 = columns, 3 = values), and the selected entry.
struct PivotEdit {
    pivot: usize,
    pane: usize,
    sel: usize,
}

/// [`ClipData::sheet`] of a copy whose sheet was deleted.
const SHEET_GONE: usize = usize::MAX;

/// An internal clipboard: a rect of cells plus its source sheet and corner so
/// pasted formulas can shift their relative references (Excel semantics) and
/// a cut clears the sheet it came from.
#[derive(Clone)]
struct ClipData {
    cells: Vec<Vec<Option<Cell>>>,
    sheet: usize,
    from: (u32, u32),
    cut: bool,
}

/// The list-validation dropdown: the allowed values and the highlighted row.
struct DvPicker {
    values: Vec<String>,
    sel: usize,
}

/// What a confirmed (Yes) modal should do.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ConfirmAction {
    Exit,
    DeleteSheet,
    /// Save As to a macro-free type (`.xlsx`/`.xltx`) drops what it cannot
    /// carry: the VB project, Excel 4.0 macro and dialog sheets, and Excel
    /// 4.0 names ([`SheetPackage::macro_features`]).
    SaveWithoutMacros(String),
    /// Text to Columns over cells that hold data.
    TextToColumns(gridcore::edit::TtcSource, TextParse),
    /// Open or New over a modified workbook discards its changes (#882).
    Discard(Next),
}

/// What Yes to [`ConfirmAction::Discard`] goes on to do.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Next {
    Open(String),
    New,
}

// The Yes/No modal itself lives in `backstage::Confirm<ConfirmAction>` (shared
// across all apps); xlsxy only supplies the action carried on Yes.

struct App {
    pkg: SheetPackage,
    engine: Engine,
    path: String,
    import_source: Option<String>,
    /// The template this workbook was started from, until this session first
    /// writes it (to `Budget1.xlsx` from `Budget.xltx`, or a Save As name).
    template: Option<String>,
    /// An import (a `.csv`/`.tsv`/`.txt`/`.prn` or an `.xls`/`.xlsb`/`.ods`)
    /// this session has not written yet: its binding was free when it
    /// opened, and the first save rechecks it.
    import_unsaved: bool,
    sheet: usize,
    cur: (u32, u32),
    anchor: Option<(u32, u32)>,
    top: u32,
    left: u32,
    edit: Option<EditState>,
    modified: bool,
    status: Option<String>,
    undo: Vec<UndoAction>,
    redo: Vec<UndoAction>,
    clip: Option<ClipData>,
    os_clip: Option<arboard::Clipboard>,
    clip_text: Option<String>,
    /// A modal Yes/No confirmation (e.g. Exit, delete sheet). The shared widget
    /// records its own selection/action.
    confirm: Option<backstage::Confirm<ConfirmAction>>,
    /// An external hyperlink awaiting the user's confirmation to open.
    pending_link: Option<String>,
    prompt: Option<Prompt>,
    pivot_edit: Option<PivotEdit>,
    /// Ctrl-M overlay: pane 0 = relationships, 1 = measures.
    model_view: Option<(usize, usize)>,
    model_rels: Vec<Relationship>,
    model_measures: Vec<gridcore::model::Measure>,
    last_find: Option<String>,
    // Review comments + the side panel that shows them.
    ribbon: ribbon::Ribbon,
    ribbon_focus: ribbon::Focus,
    comments: Vec<Comment>,
    show_comments: bool,
    /// File › Options › Data › Automatic Data Conversion (persisted).
    auto_convert: AutoConvert,
    /// File › Options › Editing (persisted, #672).
    edit_opts: EditOptions,
    /// Ctrl+Shift+U: the formula bar is four rows of wrapped text. View
    /// state for this session, not persisted.
    fx_expanded: bool,
    /// The last press on a grid cell, for double-click detection.
    last_click: Option<(Instant, u32, u32)>,
    comment_sel: usize,
    // The File backstage (folder browser / preview / info / save-as).
    backstage: Option<backstage::Backstage>,
    // The welcome screen shown when launched with no file.
    start_screen: bool,
    start: backstage::Start,
    // The formatting popup (number format / font & fill color).
    format_picker: Option<FormatPicker>,
    format_dialog: Option<FormatDialog>,
    /// The Text Import Wizard or Convert Text to Columns Wizard.
    text_dialog: Option<textdlg::TextDialog>,
    /// Data ▸ Outline's open dialog: Subtotal, Settings, or Rows/Columns.
    outline_dialog: Option<outlinedlg::Dialog>,
    /// The text Save As type (index into [`SAVE_TYPES`]) the workbook was
    /// last saved as; Ctrl+S keeps writing it while the path has its extension.
    text_type: Option<usize>,
    // View preferences (persisted to a config file).
    formula_view: bool,
    light_theme: bool,
    auto_hide_ribbon: bool,
    /// Reveal rows/columns hidden by a filter or manual hide (off = Excel view).
    show_hidden: bool,
    /// Paint floating pictures/charts over the grid (on by default).
    show_drawings: bool,
    /// Terminal graphics capability (kitty/iTerm2/Sixel/half-block); None until the
    /// TUI starts (and in headless tests) — pictures then fall back to a box.
    picker: Option<Picker>,
    /// Encoded picture protocols, keyed by `"{media part}#{cols}x{rows}"`. `None`
    /// is cached for undecodable media (WMF/EMF/missing) so the box stays.
    img_cache: std::collections::HashMap<String, Option<Protocol>>,
    /// The pending Find text while the Replace-with prompt is open.
    replace_find: Option<String>,
    /// Vim-mode state (Some when launched with `--vim`).
    vim: Option<VimState>,
    /// The sheet-picker popup: the highlighted sheet index while open.
    sheet_picker: Option<usize>,
    /// The list-validation dropdown, open on a `list`-validated cell.
    dv_picker: Option<DvPicker>,
    // Geometry captured during draw, for mouse hit-testing.
    grid_area: Rect,
    gutter_w: u16,
    /// Of `gutter_w`, the row outline's share on its left: one column per
    /// level plus one (0 without a row outline).
    outline_w: u16,
    /// The screen line of the column outline (level buttons, group bars and
    /// +/-), above the column header; `None` without a column outline.
    col_outline_y: Option<u16>,
    /// The column header's screen line (row level buttons on its left).
    col_hdr_y: u16,
    vis_cols: Vec<(u32, u16, u16)>, // (col, x, width)
    vis_rows: Vec<u32>,             // sheet row per screen line (freeze-aware)
    vis_subline: Vec<u8>,           // which wrapped sub-line of that row (parallel to vis_rows)
    tab_spans: Vec<(usize, u16, u16)>,
    ribbon_rows: u16,
    /// A cell or structural edit made a new circle: its warning is added to
    /// whatever status the edit's caller sets, once the action is done
    /// ([`App::flush_circle_warning`]).
    circle_warning_pending: bool,
    /// The file opened read-only (`--read-only`, #882): nothing writes it,
    /// so Save on it asks for another name. Compared by file identity, so no
    /// other spelling or link reaches it. A person's Open that loads (any
    /// file, this one included), a finished interactive import, or New
    /// ends it. The startup import of the `-r` file itself keeps it
    /// ([`Self::startup_import`]), as do an Open that fails or is cancelled,
    /// a reload, and the control surface's scripted `wb.open`/`wb.reload`.
    read_only: Option<std::path::PathBuf>,
    /// The XLSTART folder ([`xlstart_dir`]): its `book.xltx` is what New
    /// starts from. `None` (tests, and no config folder) starts New blank.
    xlstart: Option<PathBuf>,
    /// File › Options › Advanced › *At startup, open all files in*: the
    /// alternate startup folder (`alt_startup_path` in the preferences).
    alt_startup: Option<String>,
    /// Why no startup workbook opened, held while the welcome screen (which
    /// draws no status line) is up and shown when it closes.
    startup_note: Option<String>,
    /// The Text Import Wizard `xlsxy -r notes.txt` opened at startup: its
    /// finish keeps read-only, unlike an interactive import. Cleared when
    /// that wizard finishes or is cancelled.
    startup_import: bool,
}

impl App {
    fn new(pkg: SheetPackage, path: &str) -> App {
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        let (model_rels, model_measures) = pkg
            .part(MODEL_PART)
            .map(|b| parse_model_part(&String::from_utf8_lossy(b)))
            .unwrap_or_default();
        let comments = pkg.comments();
        // Excel warns about a workbook's circular references when it opens
        // (not when iterative calculation is on: the circles are intended).
        let status = (pkg.workbook.iterate.is_none() && !engine.circular_refs().is_empty())
            .then(|| CIRCULAR_WARNING.to_string());
        // A workbook opens on the sheet it was saved on.
        let pkg_active_tab = pkg
            .workbook
            .active_tab
            .min(pkg.workbook.sheets.len().saturating_sub(1));
        App {
            pkg,
            engine,
            path: path.to_string(),
            import_source: None,
            template: None,
            import_unsaved: false,
            sheet: pkg_active_tab,
            cur: (0, 0),
            anchor: None,
            top: 0,
            left: 0,
            edit: None,
            modified: false,
            status,
            circle_warning_pending: false,
            undo: Vec::new(),
            redo: Vec::new(),
            clip: None,
            os_clip: arboard::Clipboard::new().ok(),
            clip_text: None,
            confirm: None,
            pending_link: None,
            prompt: None,
            pivot_edit: None,
            model_view: None,
            model_rels,
            model_measures,
            last_find: None,
            ribbon: ribbon::Ribbon::new(),
            ribbon_focus: ribbon::Focus::None,
            comments,
            show_comments: false,
            auto_convert: AutoConvert::default(),
            edit_opts: EditOptions::default(),
            fx_expanded: false,
            last_click: None,
            comment_sel: 0,
            backstage: None,
            start_screen: false,
            start: backstage::Start::new(
                "xlsxy",
                vec![
                    backstage::StartItem {
                        label: "New workbook".to_string(),
                        desc: Some("Start a fresh blank .xlsx".to_string()),
                    },
                    backstage::StartItem {
                        label: "Open…".to_string(),
                        desc: Some("Browse for a workbook or CSV".to_string()),
                    },
                    backstage::StartItem {
                        label: "Quit".to_string(),
                        desc: Some("Exit xlsxy".to_string()),
                    },
                ],
                Color::Green,
            ),
            format_picker: None,
            format_dialog: None,
            text_dialog: None,
            outline_dialog: None,
            text_type: None,
            formula_view: false,
            light_theme: false,
            auto_hide_ribbon: false,
            show_hidden: false,
            show_drawings: true,
            picker: None,
            img_cache: std::collections::HashMap::new(),
            replace_find: None,
            vim: None,
            read_only: None,
            xlstart: None,
            alt_startup: None,
            startup_note: None,
            startup_import: false,
            sheet_picker: None,
            dv_picker: None,
            grid_area: Rect::default(),
            gutter_w: 4,
            outline_w: 0,
            col_outline_y: None,
            col_hdr_y: 0,
            vis_cols: Vec::new(),
            vis_rows: Vec::new(),
            vis_subline: Vec::new(),
            tab_spans: Vec::new(),
            ribbon_rows: 1,
        }
    }

    fn sheet(&self) -> &Sheet {
        &self.pkg.workbook.sheets[self.sheet]
    }

    /// Selection rectangle (anchor..cursor), or the cursor cell alone.
    fn selection(&self) -> (u32, u32, u32, u32) {
        let (r, c) = self.cur;
        match self.anchor {
            Some((ar, ac)) => (ar.min(r), ac.min(c), ar.max(r), ac.max(c)),
            None => (r, c, r, c),
        }
    }

    /// The selection intersected with the sheet's used range, so operations
    /// that iterate every coordinate (copy, clear) never walk the full
    /// 1,048,576 × 16,384 grid when the user selects whole rows/columns or
    /// the entire sheet. Falls back to the cursor cell when the selection
    /// covers only empty area (nothing to iterate).
    fn iter_selection(&self) -> (u32, u32, u32, u32) {
        let (r1, c1, r2, c2) = self.selection();
        let (used_r, used_c) = self.sheet().used_size();
        if used_r == 0 || used_c == 0 || r1 >= used_r || c1 >= used_c {
            return (r1, c1, r1, c1); // just the anchor corner
        }
        (r1, c1, r2.min(used_r - 1), c2.min(used_c - 1))
    }

    // --- editing -----------------------------------------------------------

    /// Whether the active sheet is protected. When true, cell edits and
    /// content-clearing are blocked (mirroring Excel), with a status hint.
    fn protected(&self) -> bool {
        self.sheet().is_protected()
    }

    /// Toggle protection on the active sheet. Goes through `structural` so it's
    /// undoable; protecting uses Excel's default flag set. It moves no cells,
    /// so a pending cut stays a cut: a paste refused on a protected sheet
    /// can still move it once the sheet is unprotected.
    fn toggle_protection(&mut self) {
        let now = !self.protected();
        let s = self.sheet;
        let cut = self.clip.as_ref().is_some_and(|c| c.cut);
        self.structural(|wb| wb.sheets[s].set_protected(now));
        if let Some(clip) = &mut self.clip {
            clip.cut = cut;
        }
        self.status = Some(if now {
            "Sheet protected — cells are read-only until unprotected".into()
        } else {
            "Sheet protection removed".into()
        });
    }

    fn start_edit(&mut self, initial: Option<char>) {
        if self.protected() {
            self.status =
                Some("Sheet is protected — unprotect it to edit (Review ▸ Protect)".into());
            return;
        }
        let text = match initial {
            Some(ch) => ch.to_string(),
            None => self.current_input_text(),
        };
        let cursor = text.chars().count();
        self.edit = Some(EditState {
            seed: initial.is_none().then(|| text.clone()),
            text,
            cursor,
            replace: initial.is_some(),
            proposal: None,
        });
        self.anchor = None;
    }

    /// The context a typed commit reads its entry under: the workbook's, with
    /// the user's fixed decimal point (only typing shifts a number, #672).
    fn typed_ctx(&self) -> EntryCtx {
        EntryCtx {
            fixed_decimal: self.edit_opts.fixed_places(),
            ..entry_ctx(&self.pkg.workbook, now_serial())
        }
    }

    /// AutoComplete (#672): with the caret at the end of the editor, append
    /// the rest of the one column value the typed text starts, selected
    /// ([`EditState::proposal`]).
    fn propose(&mut self) {
        if !self.edit_opts.autocomplete {
            return;
        }
        let (r, c) = self.cur;
        let Some(e) = &self.edit else { return };
        let n = e.text.chars().count();
        if e.proposal.is_some() || e.cursor != n {
            return;
        }
        let Some(value) = gridcore::entry::autocomplete(self.sheet(), r, c, &e.text) else {
            return;
        };
        let suffix: String = value.chars().skip(n).collect();
        if let Some(e) = self.edit.as_mut().filter(|_| !suffix.is_empty()) {
            e.text.push_str(&suffix);
            e.proposal = Some((n, value));
        }
    }

    /// Drop the AutoComplete proposal's suffix from the editor (Backspace,
    /// Delete, or a typed character replacing it). True when there was one.
    fn drop_proposal(&mut self) -> bool {
        let Some(e) = self.edit.as_mut() else {
            return false;
        };
        let Some((from, _)) = e.proposal.take() else {
            return false;
        };
        e.text = e.text.chars().take(from).collect();
        e.cursor = from;
        true
    }

    /// Enter (Shift+Enter backwards) after a commit or on the grid: File ›
    /// Options › Editing's direction, or nowhere with "move selection" off.
    fn enter_move(&mut self, back: bool) {
        let (dr, dc) = self.edit_opts.enter_delta(back);
        if (dr, dc) != (0, 0) {
            self.move_cur(i64::from(dr), i64::from(dc), false);
        }
    }

    /// A press on grid cell (row, col) at `at`. A second press on the same
    /// cell within [`DOUBLE_CLICK`] is a double-click (crossterm reports
    /// none): it edits the cell or, with editing directly in cells off,
    /// jumps from a formula to its first precedent. A press that follows a
    /// hyperlink starts no double-click.
    fn click_cell(&mut self, row: u32, col: u32, at: Instant) {
        let double = self.last_click.take().is_some_and(|(t, r, c)| {
            (r, c) == (row, col)
                && at
                    .checked_duration_since(t)
                    .is_some_and(|d| d <= DOUBLE_CLICK)
        });
        self.anchor = None;
        self.cur = (row, col);
        if double {
            self.cell_double_click(row, col);
            return;
        }
        if self.sheet().hyperlinks.contains_key(&(row, col)) {
            self.follow_hyperlink(row, col);
        } else {
            self.last_click = Some((at, row, col));
        }
    }

    /// A double-click on (row, col): edit it (as F2), or — editing directly in
    /// cells off — select a formula's first direct precedent.
    fn cell_double_click(&mut self, row: u32, col: u32) {
        let formula = self
            .sheet()
            .cell(row, col)
            .is_some_and(|c| c.formula.is_some());
        if formula && !self.edit_opts.edit_in_cell {
            self.goto_precedent(row, col);
        } else {
            self.start_edit(None);
        }
    }

    /// Select the first area the formula at (row, col) refers to
    /// ([`gridcore::formula::direct_precedents`]), switching sheet when it is
    /// on another one. Excel selects every direct precedent; xlsxy's
    /// selection is one rect, so it takes the first.
    fn goto_precedent(&mut self, row: u32, col: u32) {
        let areas = gridcore::formula::direct_precedents(&self.pkg.workbook, self.sheet, row, col);
        let Some(&(si, (r1, c1, r2, c2))) = areas.first() else {
            self.status = Some("No precedent cells to go to".into());
            return;
        };
        if self.pkg.workbook.sheets[si].hidden {
            self.status = Some("The precedent cells are on a hidden sheet".into());
            return;
        }
        if si != self.sheet {
            self.goto_sheet(si);
        }
        self.cur = (r1, c1);
        self.anchor = ((r1, c1) != (r2, c2)).then_some((r2, c2));
        self.ensure_visible();
    }

    /// The words Excel's status bar shows for the options in force.
    fn status_words(&self) -> Vec<&'static str> {
        let mut words = Vec::new();
        if self.edit_opts.fixed_decimal {
            words.push("Fixed Decimal");
        }
        words
    }

    /// The formula bar's height: one row, or four with Ctrl+Shift+U.
    fn fx_bar_height(&self) -> u16 {
        if self.fx_expanded { 4 } else { 1 }
    }

    /// What editing an existing cell starts from: the formula with `=`, or
    /// the value as [`gridcore::entry::seed_text`] writes it (every digit of
    /// a number, a percent cell's `150%`, a quote prefix's `'`), as gridwasm
    /// and the suite seed theirs.
    fn current_input_text(&self) -> String {
        let (r, c) = self.cur;
        let styles = &self.pkg.workbook.styles;
        self.sheet()
            .cell(r, c)
            .map(|cl| seed_text(cl, &styles.xf(cl.style)))
            .unwrap_or_default()
    }

    /// Commit the editor text into the current cell as a typed entry
    /// (gridcore::entry). Returns false (and stays in edit mode) when a
    /// formula doesn't parse, the entry is over the 32,767-character cell
    /// limit, or it would change part of an array ([`PART_OF_ARRAY`]).
    /// A live AutoComplete proposal commits as the value it completes to,
    /// in that value's case; a fixed decimal point shifts a typed number.
    fn commit_edit(&mut self) -> bool {
        let Some(mut edit) = self.edit.take() else {
            return true;
        };
        if let Some((_, value)) = edit.proposal.take() {
            edit.text = value;
        }
        let (text, seed) = (edit.text, edit.seed);
        // A seeded editor left unchanged must not re-read the cell: `007` in
        // a quote-prefixed cell is fine either way, but a stored
        // 0.30000000000000004 would come back as 0.3.
        if seed.as_deref() == Some(text.as_str()) {
            return true;
        }
        let (r, c) = self.cur;
        let formula = gridcore::entry::typed_formula(&self.pkg.workbook, self.sheet, r, c, &text);
        if let Some(Err(e)) = formula.map(Engine::validate) {
            self.status = Some(format!("formula error: {e}"));
            self.edit = Some(EditState {
                cursor: text.chars().count(),
                text,
                replace: false,
                seed,
                proposal: None,
            });
            return false;
        }
        let ctx = self.typed_ctx();
        let cell = match entry_cell_ctx(&mut self.pkg.workbook, self.sheet, r, c, &text, &ctx) {
            Ok(cell) => cell,
            Err(e) => {
                // Refused (too long): keep the editor open with the text.
                self.status = Some(e.to_string());
                self.edit = Some(EditState {
                    cursor: text.chars().count(),
                    text,
                    replace: false,
                    seed,
                    proposal: None,
                });
                return false;
            }
        };
        if !self.apply(vec![(r, c, cell)]) {
            // Refused (part of an array): keep the editor open, as Excel does.
            self.edit = Some(EditState {
                cursor: text.chars().count(),
                text,
                replace: false,
                seed,
                proposal: None,
            });
            return false;
        }
        true
    }

    fn cancel_edit(&mut self) {
        self.edit = None;
    }

    /// Undo/redo snapshots of `keys` on sheet `sheet_idx` as it is now
    /// ([`gridcore::sheet::snapshot_cells`]): spill output of a live anchor
    /// is a blank the anchor re-spills over.
    fn snapshot(&self, sheet_idx: usize, keys: &[(u32, u32)]) -> Vec<Option<Cell>> {
        let wb = &self.pkg.workbook;
        let frozen = |r, c| self.engine.is_frozen(wb, (sheet_idx, r, c));
        gridcore::sheet::snapshot_cells(&wb.sheets[sheet_idx], keys, frozen)
    }

    /// The cells an undo group over `keys` on sheet `sheet_idx` records:
    /// `keys`, plus each frozen anchor that is a key or whose block a key
    /// lies in, with that block's held values
    /// ([`gridcore::sheet::frozen_spill_keys`]).
    fn undo_keys(&self, sheet_idx: usize, keys: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let wb = &self.pkg.workbook;
        let frozen = |r, c| self.engine.is_frozen(wb, (sheet_idx, r, c));
        gridcore::sheet::frozen_spill_keys(&wb.sheets[sheet_idx], keys, frozen)
    }

    /// Apply cell changes to the current sheet as one undo group. False
    /// when refused ([`App::apply_groups`]).
    fn apply(&mut self, changes: Vec<(u32, u32, Cell)>) -> bool {
        self.apply_on(self.sheet, changes)
    }

    /// Apply cell changes to sheet `sheet_idx` as one undo group, through the
    /// engine. This is the shared edit path for keyboard edits (via [`Self::apply`])
    /// and agent control edits (which may target a non-active sheet).
    fn apply_on(&mut self, sheet_idx: usize, changes: Vec<(u32, u32, Cell)>) -> bool {
        self.apply_groups(vec![(sheet_idx, changes)])
    }

    /// Apply per-sheet cell changes, in order ([`Engine::set_cells`]: blanks
    /// landing in a frozen array block last), as one undo step: a cut
    /// pasted on another sheet clears its source and writes its destination
    /// together, so one undo puts both back.
    ///
    /// Refused whole, with nothing applied, no undo step and the status
    /// saying why, when a group would change part of an array
    /// ([`Engine::refuses`]). False then.
    fn apply_groups(&mut self, groups: Vec<(usize, CellChanges)>) -> bool {
        let wb = &self.pkg.workbook;
        if groups
            .iter()
            .any(|(s, changes)| self.engine.refuses(wb, *s, changes))
        {
            self.status = Some(PART_OF_ARRAY.to_string());
            return false;
        }
        let keys = groups
            .iter()
            .map(|(s, changes)| (*s, changes.iter().map(|&(r, c, _)| (r, c)).collect()))
            .collect();
        // Decided once above, for every group: written without deciding
        // again against what the earlier groups recalculated.
        self.record_groups(keys, None, |app| {
            for (sheet_idx, changes) in groups {
                app.engine
                    .set_cells_prechecked(&mut app.pkg.workbook, sheet_idx, changes);
            }
        });
        true
    }

    /// Run `write`, which edits the cells `keys` names (per sheet), as one
    /// undo step. Every cell's before is taken ahead of the first write and
    /// its after once the last is done, so each group is one snapshot of its
    /// cells, as [`Engine::restore_cells`] needs: taken cell by cell, a spill
    /// anchor's after could still claim a cell a later write blocked it with.
    ///
    /// `cut_from` is a cut's source (sheet, rect) when `write` pastes one: a
    /// table lying wholly inside it was cut whole, so clearing its headers
    /// renames none of its columns.
    fn record_groups(
        &mut self,
        keys: Vec<(usize, Vec<(u32, u32)>)>,
        cut_from: Option<(usize, (u32, u32, u32, u32))>,
        write: impl FnOnce(&mut Self),
    ) {
        let keys: Vec<_> = keys.into_iter().filter(|(_, k)| !k.is_empty()).collect();
        if keys.is_empty() {
            return;
        }
        // An edit can take what only its undo puts back from outside the
        // keys: overwriting a frozen anchor clears its cached block, and
        // typing into the block drops the anchor's extent. The group
        // records the anchor and its block too (`frozen_spill_keys`).
        let keys: Vec<_> = keys
            .into_iter()
            .map(|(s, cells)| (s, self.undo_keys(s, &cells)))
            .collect();
        let circles_before = self.engine.circular_refs();
        self.engine.clock = now_serial();
        // Spill output of a live anchor is snapshotted as the blank its
        // anchor re-spills over ([`App::snapshot`]), whether or not the anchor
        // is in the group.
        let snapshot = |app: &Self| -> Vec<Vec<Option<Cell>>> {
            keys.iter()
                .map(|(s, cells)| app.snapshot(*s, cells))
                .collect()
        };
        // A header cell of a table names its column: an edit there renames
        // the column in every formula (`sync_table_headers`), which only a
        // whole-workbook step can undo. Snapshotted only when one is hit.
        let headers = keys
            .iter()
            .any(|(s, cells)| self.hits_table_header(*s, cells));
        let wb_before = headers.then(|| self.wb_snapshot());
        let befores = snapshot(self);
        write(self);
        let mut written = None;
        let mut renamed = false;
        if headers {
            written = Some(snapshot(self));
            let inside = |a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)| {
                b.0 <= a.0 && a.2 <= b.2 && b.1 <= a.1 && a.3 <= b.3
            };
            let cut_whole: Vec<(usize, (u32, u32, u32, u32))> = (self.pkg.workbook.tables.iter())
                .filter(|t| cut_from.is_some_and(|(s, rect)| t.sheet == s && inside(t.range, rect)))
                .map(|t| (t.sheet, t.range))
                .collect();
            for (s, cells) in &keys {
                let cells: Vec<(u32, u32)> = (cells.iter().copied())
                    .filter(|&(r, c)| {
                        !cut_whole
                            .iter()
                            .any(|&(ts, rect)| ts == *s && inside((r, c, r, c), rect))
                    })
                    .collect();
                renamed |= gridcore::edit::sync_table_headers(&mut self.pkg.workbook, *s, &cells);
            }
        }
        if let (true, Some(before)) = (renamed, wb_before) {
            let after = self.wb_snapshot();
            self.rebuild_engine();
            // As every structural step does (`try_structural`): a pending
            // cut's cells were captured before the formulas were rewritten.
            // When this step is the cut-paste itself, the cut is spent.
            self.cancel_cut();
            self.undo.push(UndoAction::Structural { before, after });
            self.redo.clear();
            self.modified = true;
            self.warn_new_circles(&circles_before);
            return;
        }
        let afters = snapshot(self);
        // A header written back as its column's name (a formula, a
        // duplicate) changed under the engine.
        if written.is_some_and(|w| w != afters) {
            self.rebuild_engine();
        }
        let undo = keys
            .iter()
            .zip(befores.into_iter().zip(afters))
            .map(|((s, cells), (before, after))| UndoGroup {
                sheet: *s,
                changes: cells
                    .iter()
                    .zip(before.into_iter().zip(after))
                    .map(|(&(r, c), (b, a))| (r, c, b, a))
                    .collect(),
                styles_only: false,
            })
            .collect();
        self.undo.push(UndoAction::Cells(undo));
        self.redo.clear();
        self.modified = true;
        self.warn_new_circles(&circles_before);
    }

    /// Whether any of `cells` on sheet `sheet` is a header cell of a table.
    fn hits_table_header(&self, sheet: usize, cells: &[(u32, u32)]) -> bool {
        self.pkg.workbook.tables.iter().any(|t| {
            let (r1, c1, _, c2) = t.range;
            t.sheet == sheet
                && t.header_rows > 0
                && cells
                    .iter()
                    .any(|&(r, c)| r == r1 && (c1..=c2).contains(&c))
        })
    }

    /// Restyle cells on sheet `sheet_idx` — `(row, col, style index)` — as one
    /// undo group. Only styles change ([`Engine::set_styles`]): a value,
    /// formula or spill is never re-entered, so formatting a spilled block
    /// keeps the spill.
    fn apply_styles_on(&mut self, sheet_idx: usize, styles: Vec<(u32, u32, u32)>) {
        if styles.is_empty() {
            return;
        }
        let circles_before = self.engine.circular_refs();
        self.engine.clock = now_serial();
        let before: Vec<Option<Cell>> = {
            let sheet = &self.pkg.workbook.sheets[sheet_idx];
            styles
                .iter()
                .map(|&(r, c, _)| sheet.cell(r, c).cloned())
                .collect()
        };
        self.engine
            .set_styles(&mut self.pkg.workbook, sheet_idx, &styles);
        let sheet = &self.pkg.workbook.sheets[sheet_idx];
        let changes = styles
            .iter()
            .zip(before)
            .map(|(&(r, c, _), before)| (r, c, before, sheet.cell(r, c).cloned()))
            .collect();
        self.undo.push(UndoAction::Cells(vec![UndoGroup {
            sheet: sheet_idx,
            changes,
            styles_only: true,
        }]));
        self.redo.clear();
        self.modified = true;
        self.warn_new_circles(&circles_before);
    }

    /// Excel's circular-reference warning, once, when a cell edit put a
    /// cell on a circle that was not on one before. Like a structural edit's,
    /// it is added to the caller's own status ("Pasted", "Filled", …) by
    /// [`App::flush_circle_warning`].
    fn warn_new_circles(&mut self, before: &[(usize, u32, u32)]) {
        let now = self.engine.circular_refs();
        if self.circles_shown() && now.iter().any(|k| !before.contains(k)) {
            self.circle_warning_pending = true;
        }
    }

    /// Whether circles are surfaced (warning, footer): not when the workbook
    /// enables iterative calculation, where Excel treats them as intended.
    /// `wb.path.circular` lists them either way.
    fn circles_shown(&self) -> bool {
        self.pkg.workbook.iterate.is_none() && !self.engine.circular_refs().is_empty()
    }

    /// Add a cell or structural edit's pending circle warning to the status
    /// its caller set ("Pasted. There are one or more circular …").
    fn flush_circle_warning(&mut self) {
        if !std::mem::take(&mut self.circle_warning_pending) {
            return;
        }
        self.status = Some(match self.status.take() {
            Some(s) if !s.is_empty() && s != CIRCULAR_WARNING => format!("{s}. {CIRCULAR_WARNING}"),
            _ => CIRCULAR_WARNING.into(),
        });
    }

    /// The cells on circular references as A1 refs: bare on the active
    /// sheet, `Sheet!A1` elsewhere. Active-sheet cells come first.
    pub(crate) fn circular_refs(&self) -> Vec<String> {
        let refs = self.engine.circular_refs();
        let name = |&(s, r, c): &(usize, u32, u32)| {
            if s == self.sheet {
                cell_name(r, c)
            } else {
                let sheet = self
                    .pkg
                    .workbook
                    .sheets
                    .get(s)
                    .map(|sh| sh.name.as_str())
                    .unwrap_or_default();
                format!(
                    "{}!{}",
                    gridcore::sheet::quote_sheet_name(sheet),
                    cell_name(r, c)
                )
            }
        };
        let (here, elsewhere): (Vec<_>, Vec<_>) = refs.iter().partition(|k| k.0 == self.sheet);
        here.iter().chain(elsewhere.iter()).map(name).collect()
    }

    /// Snapshot-run-snapshot for an edit to page layout (page setup, print
    /// areas and titles, page breaks): `op` returns whether it changed
    /// anything, and only a change lands on the undo stack and marks the
    /// workbook modified. An error leaves the workbook as it was. No cell
    /// moves, so nothing is recalculated.
    fn layout_edit(
        &mut self,
        op: impl FnOnce(&mut gridcore::sheet::Workbook) -> Result<bool, String>,
    ) -> Result<bool, String> {
        let before = self.wb_snapshot();
        match op(&mut self.pkg.workbook) {
            Err(e) => {
                self.put_back(&before);
                Err(e)
            }
            Ok(false) => Ok(false),
            Ok(true) => {
                let after = self.wb_snapshot();
                self.undo.push(UndoAction::Structural { before, after });
                self.redo.clear();
                self.modified = true;
                Ok(true)
            }
        }
    }

    /// Snapshot-run-snapshot for structural edits (row/col ops, renames):
    /// the inverse isn't per-cell, so undo restores the whole grid state.
    fn structural(&mut self, op: impl FnOnce(&mut gridcore::sheet::Workbook)) {
        let infallible = self.try_structural(None, |wb| {
            op(wb);
            Ok(())
        });
        debug_assert!(infallible.is_ok());
    }

    /// [`Self::structural`] for an edit that can be refused: an `Err` leaves
    /// the workbook as it was, with nothing on the undo stack. A table rename
    /// passes `model_rename` (old, new): the data model follows it, and the
    /// undo step replays it backwards (and redo forwards) on whatever the
    /// model holds then.
    fn try_structural(
        &mut self,
        model_rename: Option<(&str, &str)>,
        op: impl FnOnce(&mut gridcore::sheet::Workbook) -> Result<(), String>,
    ) -> Result<(), String> {
        self.structural_step(model_rename, false, op)
    }

    /// [`Self::structural`] for an edit that writes cell content in place
    /// (Replace All, Text to Columns, AutoSum): a table header it rewrote
    /// renames that column, as typing there does
    /// ([`Self::sync_written_headers`]). An edit that moves cells (rows,
    /// columns, a sort) must not use it: its moved headers aren't written.
    fn structural_writing_cells(&mut self, op: impl FnOnce(&mut gridcore::sheet::Workbook)) {
        let infallible = self.structural_step(None, true, |wb| {
            op(wb);
            Ok(())
        });
        debug_assert!(infallible.is_ok());
    }

    /// The one structural step behind [`Self::try_structural`] and
    /// [`Self::structural_writing_cells`]; `sync_headers` says whether the
    /// edit wrote cells in place, whose header cells then rename columns.
    fn structural_step(
        &mut self,
        model_rename: Option<(&str, &str)>,
        sync_headers: bool,
        op: impl FnOnce(&mut gridcore::sheet::Workbook) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut before = self.wb_snapshot();
        // A structural edit moves cells, so compare how many cells sit on
        // circles rather than where: a circle that merely moved is not new.
        let circles_before = self.engine.circular_refs().len();
        if let Err(e) = op(&mut self.pkg.workbook) {
            self.put_back(&before);
            return Err(e);
        }
        if sync_headers {
            self.sync_written_headers(&before);
        }
        let mut after = self.wb_snapshot();
        if let Some((old, new)) = model_rename {
            self.rename_table_in_model(old, new);
            before.model_rename = Some((new.to_string(), old.to_string()));
            after.model_rename = Some((old.to_string(), new.to_string()));
        }
        self.rebuild_engine();
        if self.circles_shown() && self.engine.circular_refs().len() > circles_before {
            self.circle_warning_pending = true;
        }
        self.undo.push(UndoAction::Structural { before, after });
        self.redo.clear();
        self.modified = true;
        self.clamp_cursor();
        self.cancel_cut();
        Ok(())
    }

    /// A structural edit that wrote cells in place (Replace All, Text to
    /// Columns, AutoSum; see [`Self::structural_writing_cells`]) renames the
    /// columns whose header cells it rewrote, as typing there does
    /// ([`gridcore::edit::sync_table_headers`]). Only a header that differs
    /// from `before` while its table kept its name and range counts, so a
    /// loaded header that merely reads otherwise than its column's name (a
    /// number, say) is left as the file has it.
    fn sync_written_headers(&mut self, before: &WbSnapshot) {
        let wb = &self.pkg.workbook;
        let mut written: Vec<(usize, Vec<(u32, u32)>)> = Vec::new();
        for t in &wb.tables {
            let kept = before.tables.iter().any(|b| {
                b.sheet == t.sheet && b.range == t.range && b.name.eq_ignore_ascii_case(&t.name)
            });
            if t.header_rows == 0 || !kept {
                continue;
            }
            let (r1, c1, _, c2) = t.range;
            let was = |c| before.sheets.get(t.sheet).and_then(|s| s.cell(r1, c));
            let cells: Vec<(u32, u32)> = (c1..=c2)
                .filter(|&c| was(c) != wb.sheets[t.sheet].cell(r1, c))
                .map(|c| (r1, c))
                .collect();
            if !cells.is_empty() {
                written.push((t.sheet, cells));
            }
        }
        for (s, cells) in written {
            gridcore::edit::sync_table_headers(&mut self.pkg.workbook, s, &cells);
        }
    }

    /// The data model follows table `old` being renamed `new`: its
    /// relationships' ends and its measures' formulas.
    fn rename_table_in_model(&mut self, old: &str, new: &str) {
        for r in &mut self.model_rels {
            for table in [&mut r.from.0, &mut r.to.0] {
                if table.eq_ignore_ascii_case(old) {
                    *table = new.to_string();
                }
            }
        }
        let map = [(old.to_string(), new.to_string())];
        for m in &mut self.model_measures {
            if let Ok(ast) = gridcore::formula::parse(&m.formula) {
                let out = gridcore::formula::rename_tables_in_expr(&ast, &map);
                if out != ast {
                    m.formula = gridcore::formula::to_string(&out);
                }
            }
        }
    }

    /// The workbook state a structural undo step restores.
    fn wb_snapshot(&self) -> WbSnapshot {
        let wb = &self.pkg.workbook;
        WbSnapshot {
            sheets: wb.sheets.clone(),
            names: wb.defined_names.clone(),
            tables: wb.tables.clone(),
            removed_tables: wb.removed_tables.clone(),
            pivot_sources: wb.pivots.iter().map(|p| p.source.clone()).collect(),
            model_rename: None,
        }
    }

    /// Put `snap`'s workbook state back, without recalculating or touching
    /// the data model (see [`Self::restore`]).
    fn put_back(&mut self, snap: &WbSnapshot) {
        let wb = &mut self.pkg.workbook;
        wb.sheets = snap.sheets.clone();
        wb.defined_names = snap.names.clone();
        wb.tables = snap.tables.clone();
        wb.removed_tables = snap.removed_tables.clone();
        // Pivots aren't added or removed by a structural edit; if their count
        // changed since, the sources no longer line up and stay as they are.
        if wb.pivots.len() == snap.pivot_sources.len() {
            for (p, source) in wb.pivots.iter_mut().zip(&snap.pivot_sources) {
                p.source = source.clone();
            }
        }
    }

    fn restore(&mut self, snap: &WbSnapshot) {
        self.cancel_cut();
        self.put_back(snap);
        if let Some((old, new)) = &snap.model_rename {
            self.rename_table_in_model(old, new);
        }
        self.rebuild_engine();
        self.clamp_cursor();
        self.modified = true;
    }

    /// Formulas changed wholesale — reparse the graph and refresh values.
    fn rebuild_engine(&mut self) {
        let mut engine = Engine::new(&self.pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        engine.recalc_all(&mut self.pkg.workbook);
        self.engine = engine;
    }

    // --- data model ---------------------------------------------------------

    /// The live model: workbook Tables + the session's definitions.
    fn current_model(&self) -> DataModel {
        let mut m = DataModel::from_workbook(&self.pkg.workbook);
        m.relationships = self.model_rels.clone();
        m.measures = self.model_measures.clone();
        m
    }

    /// Ctrl-M — the model view (tables, relationships, measures).
    fn open_model_view(&mut self) {
        self.model_view = Some((0, 0));
    }

    fn model_view_key(&mut self, code: KeyCode) {
        let Some((mut pane, mut sel)) = self.model_view.take() else {
            return;
        };
        let pane_len = |app: &App, pane: usize| {
            if pane == 0 {
                app.model_rels.len()
            } else {
                app.model_measures.len()
            }
        };
        match code {
            KeyCode::Esc | KeyCode::Enter => return,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                pane = 1 - pane;
                sel = 0;
            }
            KeyCode::Up => sel = sel.saturating_sub(1),
            KeyCode::Down => sel = (sel + 1).min(pane_len(self, pane).saturating_sub(1)),
            KeyCode::Char('r') => {
                self.open_prompt(PromptKind::Relate);
            }
            KeyCode::Char('m') => {
                self.open_prompt(PromptKind::Measure);
            }
            KeyCode::Char('p') => {
                self.open_prompt(PromptKind::ModelPivot);
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if pane == 0 && sel < self.model_rels.len() {
                    self.model_rels.remove(sel);
                    self.modified = true;
                } else if pane == 1 && sel < self.model_measures.len() {
                    self.model_measures.remove(sel);
                    self.modified = true;
                }
                sel = sel.saturating_sub(1);
            }
            _ => {}
        }
        self.model_view = Some((pane, sel));
    }

    /// Materialize a model pivot into a fresh sheet and jump to it.
    fn build_model_report(&mut self, base: &str, spec: &ModelSpec) {
        let model = self.current_model();
        let out = match model_pivot(&model, base, spec) {
            Ok(o) => o,
            Err(e) => {
                self.status = Some(format!("model pivot: {e}"));
                return;
            }
        };
        let mut name = "Model Pivot".to_string();
        let mut n = 1;
        while self.pkg.workbook.sheet_index(&name).is_some() {
            n += 1;
            name = format!("Model Pivot {n}");
        }
        let idx = self.pkg.add_sheet(&name);
        let sheet = &mut self.pkg.workbook.sheets[idx];
        for (r, row) in out.grid.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let value = match v {
                    gridcore::formula::Value::Empty => continue,
                    gridcore::formula::Value::Num(x) => CellValue::Number(*x),
                    gridcore::formula::Value::Str(t) => CellValue::Text(t.clone()),
                    gridcore::formula::Value::Bool(b) => CellValue::Bool(*b),
                    gridcore::formula::Value::Err(e) => CellValue::Error(e.code().to_string()),
                };
                sheet.set_cell(
                    r as u32,
                    c as u32,
                    Cell {
                        value,
                        ..Cell::default()
                    },
                );
            }
        }
        self.sheet = idx;
        self.cur = (0, 0);
        self.top = 0;
        self.left = 0;
        self.anchor = None;
        self.undo.clear();
        self.redo.clear();
        self.rebuild_engine();
        self.modified = true;
        self.status = Some(format!("Built {name}"));
    }

    // --- pivot editor -------------------------------------------------------

    /// Ctrl-P — open the field editor for the pivot under the cursor, or
    /// create one from the selection / enclosing Table when there is none.
    fn open_pivot_editor(&mut self) {
        let wb = &self.pkg.workbook;
        let (r, c) = self.cur;
        let here = wb.pivots.iter().position(|p| {
            p.sheet == self.sheet
                && r >= p.location.0
                && r <= p.location.2
                && c >= p.location.1
                && c <= p.location.3
        });
        if here.is_none() {
            // Not on a pivot: a data selection (or enclosing Table) creates
            // one on a fresh sheet.
            let (r1, c1, r2, c2) = self.selection();
            if r2 > r1 && c2 >= c1 {
                self.create_pivot_from(gridcore::pivot::PivotSource::Range {
                    sheet: wb.sheets[self.sheet].name.clone(),
                    rect: (r1, c1, r2, c2),
                });
                return;
            }
            if let Some(t) = wb.table_at(self.sheet, r, c) {
                let name = t.name.clone();
                self.create_pivot_from(gridcore::pivot::PivotSource::Table(name));
                return;
            }
        }
        if wb.pivots.is_empty() {
            self.status = Some(
                "No pivots — select a data range (headers + rows) or stand in a Table, then Ctrl-P"
                    .to_string(),
            );
            return;
        }
        let idx = here
            .or_else(|| wb.pivots.iter().position(|p| p.sheet == self.sheet))
            .unwrap_or(0);
        if wb.pivots[idx].unsupported {
            self.status = Some(format!(
                "Pivot '{}' uses features beyond the editor (filters, calculated fields…) — left on cached values",
                wb.pivots[idx].name
            ));
            return;
        }
        self.pivot_edit = Some(PivotEdit {
            pivot: idx,
            pane: 0,
            sel: 0,
        });
    }

    /// Create a pivot from a source, land it on a new sheet, and open the
    /// field editor with a default Sum over the last numeric column.
    fn create_pivot_from(&mut self, source: gridcore::pivot::PivotSource) {
        let frame = match &source {
            gridcore::pivot::PivotSource::Range { sheet, rect } => {
                match self.pkg.workbook.sheet_index(sheet) {
                    Some(si) => gridcore::frame::Frame::from_range(&self.pkg.workbook, si, *rect),
                    None => return,
                }
            }
            gridcore::pivot::PivotSource::Table(name) => {
                match gridcore::frame::Frame::from_table(&self.pkg.workbook, name) {
                    Some(f) => f,
                    None => return,
                }
            }
        };
        if frame.names.is_empty() || frame.rows() == 0 {
            self.status = Some("The selection needs a header row and data rows".to_string());
            return;
        }
        // Default measure: the last column holding numbers (else the last).
        let field = (0..frame.cols.len())
            .rev()
            .find(|&i| {
                frame.cols[i]
                    .iter()
                    .any(|v| matches!(v, gridcore::formula::Value::Num(_)))
            })
            .unwrap_or(frame.cols.len() - 1);
        let measure = gridcore::pivot::DataField {
            name: format!("Sum of {}", frame.names[field]),
            field,
            agg: Agg::Sum,
        };
        let mut sheet_name = "Pivot".to_string();
        let mut n = 1;
        while self.pkg.workbook.sheet_index(&sheet_name).is_some() {
            n += 1;
            sheet_name = format!("Pivot {n}");
        }
        let dest = self.pkg.add_sheet(&sheet_name);
        let Some(idx) = self
            .pkg
            .add_pivot(source, frame.names.clone(), measure, dest, (2, 0))
        else {
            self.status = Some("Could not create the pivot".to_string());
            return;
        };
        let outcome = gridcore::pivot::refresh_pivots(&mut self.pkg.workbook);
        let _ = outcome;
        self.sheet = dest;
        self.cur = (2, 0);
        self.top = 0;
        self.left = 0;
        self.anchor = None;
        self.undo.clear();
        self.redo.clear();
        self.rebuild_engine();
        self.modified = true;
        self.status = Some(format!(
            "Created {} on {sheet_name} — add fields",
            self.pkg.workbook.pivots[idx].name
        ));
        self.pivot_edit = Some(PivotEdit {
            pivot: idx,
            pane: 0,
            sel: 0,
        });
    }

    /// Items in one editor pane, as display strings.
    fn pivot_pane_items(&self, pe: &PivotEdit, pane: usize) -> Vec<String> {
        let p = &self.pkg.workbook.pivots[pe.pivot];
        match pane {
            0 => p.fields.clone(),
            1 => p
                .row_fields
                .iter()
                .map(|&i| p.fields.get(i).cloned().unwrap_or_default())
                .collect(),
            2 => p
                .col_fields
                .iter()
                .map(|&i| p.fields.get(i).cloned().unwrap_or_default())
                .collect(),
            _ => p.data_fields.iter().map(|d| d.name.clone()).collect(),
        }
    }

    /// A layout change happened: recompute the pivot and its dependents.
    fn apply_pivot_edit(&mut self, pe_pivot: usize) {
        self.pkg.workbook.pivots[pe_pivot].edited = true;
        let outcome = gridcore::pivot::refresh_pivots(&mut self.pkg.workbook);
        if !outcome.changed.is_empty() {
            self.engine
                .recalc_from(&mut self.pkg.workbook, &outcome.changed);
        }
        self.modified = true;
    }

    /// Key handling inside the pivot editor. Returns None when the editor
    /// closed.
    fn pivot_editor_key(&mut self, code: KeyCode, shift: bool) {
        let Some(mut pe) = self.pivot_edit.take() else {
            return;
        };
        let field_name = |p: &gridcore::pivot::Pivot, i: usize| -> String {
            p.fields.get(i).cloned().unwrap_or_default()
        };
        let mut changed = false;
        match code {
            KeyCode::Esc | KeyCode::Enter => {
                self.status = Some("Pivot editor closed".to_string());
                return; // editor stays taken (closed)
            }
            KeyCode::Tab | KeyCode::Right => {
                pe.pane = (pe.pane + 1) % 4;
                pe.sel = 0;
            }
            KeyCode::BackTab | KeyCode::Left => {
                pe.pane = (pe.pane + 3) % 4;
                pe.sel = 0;
            }
            // Shift-Up/Down reorders within an area — field order is
            // nesting order (outer to inner), so it changes the layout.
            KeyCode::Up | KeyCode::Down if shift && pe.pane > 0 => {
                let p = &mut self.pkg.workbook.pivots[pe.pivot];
                let up = code == KeyCode::Up;
                let moved = match pe.pane {
                    1 => swap_entry(&mut p.row_fields, pe.sel, up),
                    2 => swap_entry(&mut p.col_fields, pe.sel, up),
                    _ => swap_entry(&mut p.data_fields, pe.sel, up),
                };
                if moved {
                    pe.sel = if up { pe.sel - 1 } else { pe.sel + 1 };
                    changed = true;
                }
            }
            KeyCode::Up => pe.sel = pe.sel.saturating_sub(1),
            KeyCode::Down => {
                let len = self.pivot_pane_items(&pe, pe.pane).len();
                pe.sel = (pe.sel + 1).min(len.saturating_sub(1));
            }
            // Add the selected available field to an area.
            KeyCode::Char('r') | KeyCode::Char('c') if pe.pane == 0 => {
                let p = &mut self.pkg.workbook.pivots[pe.pivot];
                let i = pe.sel.min(p.fields.len().saturating_sub(1));
                if !p.row_fields.contains(&i) && !p.col_fields.contains(&i) {
                    if code == KeyCode::Char('r') {
                        p.row_fields.push(i);
                    } else {
                        p.col_fields.push(i);
                    }
                    changed = true;
                } else {
                    self.status = Some("Field is already on an axis".to_string());
                }
            }
            KeyCode::Char('v') if pe.pane == 0 => {
                let p = &mut self.pkg.workbook.pivots[pe.pivot];
                let i = pe.sel.min(p.fields.len().saturating_sub(1));
                let name = format!("Sum of {}", field_name(p, i));
                p.data_fields.push(gridcore::pivot::DataField {
                    name,
                    field: i,
                    agg: Agg::Sum,
                });
                changed = true;
            }
            // Remove the selected entry from its area.
            KeyCode::Char('d') | KeyCode::Delete if pe.pane > 0 => {
                let p = &mut self.pkg.workbook.pivots[pe.pivot];
                let removed = match pe.pane {
                    1 if pe.sel < p.row_fields.len() => {
                        p.row_fields.remove(pe.sel);
                        true
                    }
                    2 if pe.sel < p.col_fields.len() => {
                        p.col_fields.remove(pe.sel);
                        true
                    }
                    3 if pe.sel < p.data_fields.len() => {
                        p.data_fields.remove(pe.sel);
                        true
                    }
                    _ => false,
                };
                if removed {
                    let no_values = p.data_fields.is_empty();
                    if no_values {
                        // Refresh with zero measures would blank the pivot;
                        // keep the model consistent but skip the refresh.
                        p.edited = true;
                        self.status = Some("A pivot needs at least one value field".to_string());
                        self.modified = true;
                    } else {
                        changed = true;
                    }
                    pe.sel = pe.sel.saturating_sub(usize::from(
                        pe.sel >= self.pivot_pane_items(&pe, pe.pane).len(),
                    ));
                }
            }
            // Cycle the aggregation of the selected value field.
            KeyCode::Char('a') if pe.pane == 3 => {
                let p = &mut self.pkg.workbook.pivots[pe.pivot];
                let fields = p.fields.clone();
                if let Some(df) = p.data_fields.get_mut(pe.sel) {
                    df.agg = match df.agg {
                        Agg::Sum => Agg::Count,
                        Agg::Count => Agg::Average,
                        Agg::Average => Agg::Max,
                        Agg::Max => Agg::Min,
                        Agg::Min => Agg::Product,
                        Agg::Product => Agg::CountNums,
                        Agg::CountNums => Agg::StdDev,
                        Agg::StdDev => Agg::StdDevP,
                        Agg::StdDevP => Agg::Var,
                        Agg::Var => Agg::VarP,
                        Agg::VarP => Agg::Sum,
                    };
                    let fname = fields.get(df.field).cloned().unwrap_or_default();
                    df.name = format!("{} of {}", df.agg.label(), fname);
                    changed = true;
                }
            }
            _ => {}
        }
        if changed {
            self.apply_pivot_edit(pe.pivot);
        }
        self.pivot_edit = Some(pe);
    }

    /// F9 — full recalculation plus pivot refresh (like Excel's refresh-all).
    fn recalc_and_refresh(&mut self) {
        self.engine.recalc_all(&mut self.pkg.workbook);
        let outcome = gridcore::pivot::refresh_pivots(&mut self.pkg.workbook);
        if !outcome.changed.is_empty() {
            self.engine
                .recalc_from(&mut self.pkg.workbook, &outcome.changed);
            self.modified = true;
        }
        self.status = Some(match (outcome.refreshed, outcome.skipped) {
            (0, 0) => "Recalculated".to_string(),
            (r, 0) => format!("Recalculated; {r} pivot(s) refreshed"),
            (r, s) => format!("Recalculated; {r} pivot(s) refreshed, {s} kept cached values"),
        });
    }

    fn clamp_cursor(&mut self) {
        self.sheet = self.sheet.min(self.pkg.workbook.sheets.len() - 1);
        self.anchor = None;
        self.ensure_visible();
    }

    fn undo(&mut self) {
        match self.undo.pop() {
            Some(UndoAction::Cells(groups)) => {
                for group in groups.iter().rev() {
                    if group.styles_only {
                        let styles = group_styles(group, |ch| &ch.2);
                        self.engine
                            .set_styles(&mut self.pkg.workbook, group.sheet, &styles);
                        continue;
                    }
                    let cells = group_cells(group.changes.iter().rev(), |ch| &ch.2);
                    self.engine
                        .restore_cells(&mut self.pkg.workbook, group.sheet, &cells);
                }
                self.show_undo_group(groups.last());
                self.redo.push(UndoAction::Cells(groups));
                self.modified = true;
                self.status = Some("Undid".to_string());
            }
            Some(UndoAction::Structural { before, after }) => {
                self.restore(&before);
                self.redo.push(UndoAction::Structural { before, after });
                self.status = Some("Undid".to_string());
            }
            None => self.status = Some("Nothing to undo".to_string()),
        }
    }

    /// Move the view to an undone/redone group: its sheet and first cell.
    fn show_undo_group(&mut self, group: Option<&UndoGroup>) {
        let Some(group) = group else { return };
        self.sheet = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
        if let Some(&(r, c, _, _)) = group.changes.first() {
            self.cur = (r, c);
            self.ensure_visible();
        }
    }

    fn redo(&mut self) {
        match self.redo.pop() {
            Some(UndoAction::Cells(groups)) => {
                for group in &groups {
                    if group.styles_only {
                        let styles = group_styles(group, |ch| &ch.3);
                        self.engine
                            .set_styles(&mut self.pkg.workbook, group.sheet, &styles);
                        continue;
                    }
                    let cells = group_cells(group.changes.iter(), |ch| &ch.3);
                    self.engine
                        .restore_cells(&mut self.pkg.workbook, group.sheet, &cells);
                }
                self.show_undo_group(groups.last());
                self.undo.push(UndoAction::Cells(groups));
                self.modified = true;
                self.status = Some("Redid".to_string());
            }
            Some(UndoAction::Structural { before, after }) => {
                self.restore(&after);
                self.undo.push(UndoAction::Structural { before, after });
                self.status = Some("Redid".to_string());
            }
            None => self.status = Some("Nothing to redo".to_string()),
        }
    }

    // --- clipboard -----------------------------------------------------------

    fn copy(&mut self, cut: bool) {
        let (r1, c1, r2, c2) = self.iter_selection();
        let sheet = self.sheet();
        let mut rows = Vec::new();
        let mut tsv = String::new();
        for r in r1..=r2 {
            let mut row = Vec::new();
            for c in c1..=c2 {
                if c > c1 {
                    tsv.push('\t');
                }
                let cell = sheet.cell(r, c).cloned();
                if let Some(cl) = &cell {
                    // With the `'` a paste needs to read the text back.
                    let xf = self.pkg.workbook.styles.xf(cl.style);
                    let shown = format_with(&xf, &cl.value, self.pkg.workbook.date1904);
                    tsv.push_str(&gridcore::entry::copy_field(cl, &xf, shown));
                }
                row.push(cell);
            }
            tsv.push('\n');
            rows.push(row);
        }
        self.clip = Some(ClipData {
            cells: rows,
            sheet: self.sheet,
            from: (r1, c1),
            cut,
        });
        if let Some(cb) = &mut self.os_clip {
            let _ = cb.set_text(tsv.clone());
        }
        self.clip_text = Some(tsv);
        self.status = Some(if cut { "Cut" } else { "Copied" }.to_string());
    }

    fn paste(&mut self) {
        let os_text = self.os_clip.as_mut().and_then(|cb| cb.get_text().ok());
        self.paste_from(os_text);
    }

    /// Paste with `os_text` as the OS clipboard's text (`None` when there is
    /// no OS clipboard). A protected sheet refuses the paste and keeps the
    /// clip as it was, so a cut can still be pasted elsewhere.
    fn paste_from(&mut self, os_text: Option<String>) {
        if self.protected() {
            self.status =
                Some("Sheet is protected — unprotect it to edit (Review ▸ Protect)".into());
            return;
        }
        // Our own clip (still on the OS clipboard) pastes with formulas and
        // ref translation; external text pastes as TSV values.
        let own = match (&os_text, &self.clip_text) {
            (Some(t), Some(ours)) => t == ours,
            (None, _) => true, // no OS clipboard → use internal
            _ => false,
        };
        let (r0, c0) = self.cur;
        if own {
            if let Some(clip) = self.clip.clone() {
                // A cut clears its source (once), on the sheet it came from.
                // A source sheet that is gone makes it a copy, and so does a
                // protected one: the clear would edit a locked sheet off-screen.
                let same_sheet = clip.sheet == self.sheet;
                let source_locked = clip.cut
                    && !same_sheet
                    && self
                        .pkg
                        .workbook
                        .sheets
                        .get(clip.sheet)
                        .is_some_and(|s| s.is_protected());
                let cut = clip.cut && clip.sheet < self.pkg.workbook.sheets.len() && !source_locked;
                // A cut pasted on another sheet moves its formulas off the
                // source sheet; their unqualified refs are qualified with it
                // below, so a moved `=B1` keeps reading Sheet2!B1.
                let src_name = if cut && !same_sheet {
                    Some(self.pkg.workbook.sheets[clip.sheet].name.clone())
                } else {
                    None
                };
                let mut clears = Vec::new();
                if cut {
                    let (fr, fc) = clip.from;
                    for (dr, row) in clip.cells.iter().enumerate() {
                        for (dc, cell) in row.iter().enumerate() {
                            // A cell pushed off the grid's edge isn't
                            // written, so its source stays.
                            let (dr, dc) = (dr as u32, dc as u32);
                            if cell.is_some() && r0 + dr < MAX_ROWS && c0 + dc < MAX_COLS {
                                clears.push((fr + dr, fc + dc, Cell::default()));
                            }
                        }
                    }
                }
                let (dr_all, dc_all) = (
                    r0 as i64 - clip.from.0 as i64,
                    c0 as i64 - clip.from.1 as i64,
                );
                // The block written from (r0, c0): rows and cells pushed off
                // the grid's edge are dropped, so rows may differ in length.
                let mut block = Vec::new();
                let mut writes = Vec::new();
                for (dr, row) in clip.cells.iter().enumerate() {
                    let r = r0 + dr as u32;
                    if r >= MAX_ROWS {
                        break;
                    }
                    let mut out = Vec::new();
                    for (dc, cell) in row.iter().enumerate() {
                        let c = c0 + dc as u32;
                        if c >= MAX_COLS {
                            break;
                        }
                        let mut new_cell = cell.clone().unwrap_or_default();
                        // Copies translate relative refs; cuts keep them, and
                        // so does a copy pasted where it came from (translating
                        // reprints the text, even by zero).
                        if !cut && (dr_all, dc_all) != (0, 0) {
                            if let Some(f) = &new_cell.formula {
                                if let Some(t) = translate_formula(f, dr_all, dc_all) {
                                    new_cell.formula = Some(t);
                                }
                            }
                        }
                        // A cut moved to another sheet keeps reading the
                        // cells it read: qualify with the source sheet (an
                        // unparseable formula keeps its text).
                        if let (Some(f), Some(sheet)) = (&new_cell.formula, &src_name) {
                            if let Some(t) = qualify_sheet_in_formula(f, sheet) {
                                new_cell.formula = Some(t);
                            }
                        }
                        // Overwrite position wins over source-clear on overlap.
                        if same_sheet {
                            clears.retain(|&(cr, cc, _)| (cr, cc) != (r, c));
                        }
                        writes.push((r, c));
                        out.push(new_cell);
                    }
                    block.push(out);
                }
                // The cut's clears go first, then the block through
                // `Engine::paste_block`: a pasted spilling array still spills,
                // and an array block pasted back in place, copy or cut, keeps
                // its block. A clear landing in a frozen array block goes
                // last (`Engine::split_frozen_blanks`): a no-op while the
                // block is whole, it clears its cell once the paste has put
                // content into the block. One undo step.
                let (src, here) = (clip.sheet, self.sheet);
                // Refused whole, before the cut's source is cleared, when the
                // clears or the paste would change part of an array: the clip
                // stays as it was, so it can still be pasted elsewhere.
                let wb = &self.pkg.workbook;
                let refused = if same_sheet {
                    self.engine
                        .refuses_paste(wb, here, (r0, c0), &block, &clears)
                } else {
                    self.engine.refuses(wb, src, &clears)
                        || self.engine.refuses_paste(wb, here, (r0, c0), &block, &[])
                };
                if refused {
                    self.status = Some(PART_OF_ARRAY.to_string());
                    return;
                }
                self.cancel_cut();
                let clear_keys: Vec<_> = clears.iter().map(|&(r, c, _)| (r, c)).collect();
                let keys = if same_sheet {
                    vec![(here, clear_keys.into_iter().chain(writes).collect())]
                } else {
                    vec![(src, clear_keys), (here, writes)]
                };
                // The cut's source block: a table lying wholly inside it keeps
                // its column names (it does not move with the cells).
                let cut_from = cut.then(|| {
                    let (fr, fc) = clip.from;
                    let h = clip.cells.len().max(1) as u32;
                    let w = clip.cells.iter().map(Vec::len).max().unwrap_or(1).max(1) as u32;
                    (src, (fr, fc, fr + h - 1, fc + w - 1))
                });
                self.record_groups(keys, cut_from, |app| {
                    let (clears, late) = if same_sheet {
                        app.engine
                            .split_frozen_blanks(&app.pkg.workbook, src, clears)
                    } else {
                        (clears, Vec::new())
                    };
                    // Checked whole above: each part is written without
                    // deciding again against what the clears recalculated.
                    let wb = &mut app.pkg.workbook;
                    app.engine.set_cells_prechecked(wb, src, clears);
                    app.engine
                        .paste_block_prechecked(wb, here, (r0, c0), &block);
                    app.engine.set_cells_prechecked(wb, src, late);
                });
                self.status = Some(if source_locked {
                    "Pasted (source sheet is protected; cut kept as copy)".to_string()
                } else {
                    "Pasted".to_string()
                });
                return;
            }
        }
        if let Some(text) = os_text {
            // External TSV/plain text. Cap the paste so a hostile/huge
            // clipboard can't lock the UI in per-cell recalcs.
            const MAX_PASTE_CELLS: usize = 100_000;
            let mut changes = Vec::new();
            let mut truncated = false;
            let ctx = entry_ctx(&self.pkg.workbook, now_serial());
            'outer: for (dr, line) in text.trim_end_matches('\n').split('\n').enumerate() {
                for (dc, field) in line.trim_end_matches('\r').split('\t').enumerate() {
                    if changes.len() >= MAX_PASTE_CELLS {
                        truncated = true;
                        break 'outer;
                    }
                    let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                    if r >= MAX_ROWS || c >= MAX_COLS {
                        continue;
                    }
                    let style = self.sheet().cell(r, c).map(|x| x.style).unwrap_or(0);
                    // Read as typed into the target (a leading `'` is
                    // quote-prefixed text, a date brings its format).
                    let styles = &mut self.pkg.workbook.styles;
                    let mut cell = gridcore::entry::paste_cell(styles, style, field, &ctx);
                    // A pasted `=…` that doesn't parse would freeze as an
                    // unsupported cell; demote it to literal text instead
                    // (entry-time editing rejects such input outright).
                    if let Some(f) = &cell.formula {
                        if Engine::validate(f).is_err() {
                            cell = Cell {
                                value: CellValue::Text(field.to_string()),
                                style: cell.style,
                                ..Cell::default()
                            };
                        }
                    }
                    changes.push((r, c, cell));
                }
            }
            if !self.apply(changes) {
                return;
            }
            self.status = Some(if truncated {
                format!("Pasted (clipped to {MAX_PASTE_CELLS} cells)")
            } else {
                "Pasted".to_string()
            });
        }
    }

    // --- movement ------------------------------------------------------------

    /// A picture protocol encoded to fill a `cols`×`rows` cell box, cached by
    /// media part + size. Returns `None` (and caches it) when there's no graphics
    /// terminal or the media can't be decoded, so the caller draws a box instead.
    fn image_proto(&mut self, part: &str, cols: u16, rows: u16) -> Option<&Protocol> {
        let key = format!("{part}#{cols}x{rows}");
        if !self.img_cache.contains_key(&key) {
            let proto = self.encode_image(part, cols, rows);
            self.img_cache.insert(key.clone(), proto);
        }
        self.img_cache.get(&key).and_then(|o| o.as_ref())
    }

    fn encode_image(&self, part: &str, cols: u16, rows: u16) -> Option<Protocol> {
        let picker = self.picker.as_ref()?;
        let bytes = self.pkg.part(part)?;
        let img = image::load_from_memory(bytes).ok()?;
        let size = Rect::new(0, 0, cols, rows);
        picker.new_protocol(img, size, Resize::Fit(None)).ok()
    }

    fn toggle_show_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.status = Some(
            if self.show_hidden {
                "Showing hidden rows/columns"
            } else {
                "Hiding filtered / hidden rows/columns"
            }
            .to_string(),
        );
    }

    /// Nudge a row index onto the nearest non-hidden row, preferring direction
    /// `dir`. With `show_hidden` on, or no hidden rows in the way, returns `from`.
    fn visible_row(&self, from: u32, dir: i64) -> u32 {
        if self.show_hidden || !self.sheet().row_hidden(from) {
            return from;
        }
        let step = if dir < 0 { -1 } else { 1 };
        for &s in &[step, -step] {
            let mut r = from as i64;
            while r >= 0 && r < MAX_ROWS as i64 {
                if !self.sheet().row_hidden(r as u32) {
                    return r as u32;
                }
                r += s;
            }
        }
        from
    }

    /// Column counterpart to [`Self::visible_row`].
    fn visible_col(&self, from: u32, dir: i64) -> u32 {
        if self.show_hidden || !self.sheet().col_hidden(from) {
            return from;
        }
        let step = if dir < 0 { -1 } else { 1 };
        for &s in &[step, -step] {
            let mut c = from as i64;
            while c >= 0 && c < MAX_COLS as i64 {
                if !self.sheet().col_hidden(c as u32) {
                    return c as u32;
                }
                c += s;
            }
        }
        from
    }

    fn move_cur(&mut self, dr: i64, dc: i64, select: bool) {
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cur);
            }
        } else {
            self.anchor = None;
        }
        let (r, c) = self.cur;
        let nr = (r as i64 + dr).clamp(0, MAX_ROWS as i64 - 1) as u32;
        let nc = (c as i64 + dc).clamp(0, MAX_COLS as i64 - 1) as u32;
        // Skip over rows/columns hidden by a filter or manual hide.
        self.cur = (self.visible_row(nr, dr), self.visible_col(nc, dc));
        self.ensure_visible();
    }

    /// Ctrl+arrow: jump to the edge of the data region, like Excel.
    fn jump(&mut self, dr: i64, dc: i64, select: bool) {
        if select && self.anchor.is_none() {
            self.anchor = Some(self.cur);
        }
        if !select {
            self.anchor = None;
        }
        let sheet = self.sheet();
        let (mut r, mut c) = self.cur;
        let occupied = |r: u32, c: u32| {
            sheet
                .cell(r, c)
                .map(|cl| !cl.value.is_empty() || cl.formula.is_some())
                .unwrap_or(false)
        };
        let step = |r: u32, c: u32| -> Option<(u32, u32)> {
            let nr = r as i64 + dr;
            let nc = c as i64 + dc;
            if nr < 0 || nc < 0 || nr >= MAX_ROWS as i64 || nc >= MAX_COLS as i64 {
                None
            } else {
                Some((nr as u32, nc as u32))
            }
        };
        let start_occ = occupied(r, c);
        let next_occ = step(r, c).map(|(nr, nc)| occupied(nr, nc)).unwrap_or(false);
        if start_occ && next_occ {
            // Inside a block: go to its edge.
            while let Some((nr, nc)) = step(r, c) {
                if !occupied(nr, nc) {
                    break;
                }
                (r, c) = (nr, nc);
            }
        } else {
            // Skip the gap, then land on the next occupied cell (or the edge).
            let mut moved = false;
            while let Some((nr, nc)) = step(r, c) {
                (r, c) = (nr, nc);
                moved = true;
                if occupied(r, c) {
                    break;
                }
            }
            let _ = moved;
        }
        self.cur = (self.visible_row(r, dr), self.visible_col(c, dc));
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let (r, c) = self.cur;
        let rows_vis = self.grid_area.height.max(1) as u32;
        if r < self.top {
            self.top = r;
        }
        if r >= self.top + rows_vis {
            self.top = r - rows_vis + 1;
        }
        if c < self.left {
            self.left = c;
        }
        // Horizontal: widen the window until the cursor column fits.
        let avail = self.grid_area.width.saturating_sub(self.gutter_w).max(1);
        loop {
            let mut x = 0u32;
            let mut fits = false;
            let mut col = self.left;
            while x < avail as u32 && col < MAX_COLS {
                if col == c {
                    // The whole column must fit (or be the first shown).
                    let w = self.col_disp_width(col) as u32;
                    fits = x + w <= avail as u32 || col == self.left;
                    break;
                }
                x += self.col_disp_width(col) as u32;
                col += 1;
            }
            if c < self.left {
                fits = false;
            }
            if fits {
                break;
            }
            if self.left >= c {
                self.left = c;
                break;
            }
            self.left += 1;
        }
    }

    fn col_disp_width(&self, col: u32) -> u16 {
        let w = self.sheet().col_width(col);
        (w.round() as u16 + 1).clamp(4, 60)
    }

    // --- actions ---------------------------------------------------------------

    /// Serialize the package, persisting model definitions in the custom
    /// part (removed again when the model is empty).
    fn package_bytes(&mut self) -> Vec<u8> {
        // The workbook reopens on the sheet it is saved on.
        self.pkg.workbook.active_tab = self.sheet;
        self.pkg.remove_part(MODEL_PART);
        if !self.model_rels.is_empty() || !self.model_measures.is_empty() {
            let xml = model_part_xml(&self.model_rels, &self.model_measures);
            self.pkg.set_part(MODEL_PART, xml.into_bytes());
        }
        // As Excel does: the file says when it was saved and by whom. Its
        // author and creation time are kept, or, on a workbook's first save
        // (no core properties yet), set to this user and this time.
        self.pkg.stamp_save(&iso_now(), &comment_author());
        // The file's type follows the path it is written to.
        save_xlsx_for_path(&self.pkg, &self.path)
    }

    /// Ctrl+S, `:w` and the backstage's Save. On a file opened read-only it
    /// opens Save As instead (#882), as Excel does.
    fn save(&mut self) {
        if let Err(msg) = self.refuse_read_only(&self.path) {
            self.open_prompt(PromptKind::SaveAs);
            self.status = Some(msg);
            return;
        }
        if self.ask_where_to_save_template_workbook() {
            return;
        }
        let _ = self.save_current();
    }

    /// A workbook started from a template has no file of its own until this
    /// session writes one, so an interactive save opens Save As instead, as
    /// Excel does, with the name the workbook was bound to (`Budget1.xlsx`),
    /// or the next free one when that has been taken since it opened.
    /// Returns whether it did. The control surface's `wb.save` is scripted
    /// and writes the bound name directly, as Excel's `Workbook.Save` does.
    fn ask_where_to_save_template_workbook(&mut self) -> bool {
        let Some(template) = self.template.clone() else {
            return false;
        };
        if Path::new(&self.path).exists() {
            if let Some(free) = template_binding(&template) {
                self.path = free;
            }
        }
        self.open_prompt(PromptKind::SaveAs);
        self.status = Some(format!(
            "{} is a new workbook from {}: choose where to save it",
            file_name_of(&self.path),
            file_name_of(&template)
        ));
        true
    }

    /// The `.txt`/`.prn` named on the command line: its Text Import Wizard,
    /// whose finish keeps a `--read-only` file read-only (#882).
    fn open_startup_wizard(&mut self, text_file: &str) {
        self.open_workbook(text_file);
        self.startup_import = self.text_dialog.is_some();
    }

    /// Open `source` read-only (#882). An import is bound to a new name, so
    /// its Save writes that file as usual and shows no caption, but every
    /// write onto `source` itself is still refused.
    fn set_read_only(&mut self, source: &str) {
        self.read_only = Some(std::path::PathBuf::from(source));
    }

    /// Whether the workbook is bound to the file it was opened read-only from.
    fn bound_read_only(&self) -> bool {
        self.refuse_read_only(&self.path).is_err()
    }

    /// Excel's refusal when `target` is the file opened read-only.
    fn refuse_read_only(&self, target: impl AsRef<Path>) -> Result<(), String> {
        let target = target.as_ref();
        match &self.read_only {
            Some(src) if opccore::fsio::same_file(src, target) => {
                Err(read_only_refusal(&target.to_string_lossy()))
            }
            _ => Ok(()),
        }
    }

    // ---- writes ----------------------------------------------------------
    //
    // Every file an App method writes goes through one of these three, and
    // each refuses the file opened read-only (#882) itself, so a new route
    // cannot forget to ask. The one other write is the view-preferences file
    // (`save_view_prefs`), the app's own config. Headless runs (`--recalc`,
    // `--csv`) have no App and guard the input at startup.

    fn guard_write(&self, target: &Path) -> io::Result<()> {
        self.refuse_read_only(target)
            .map_err(|msg| io::Error::new(io::ErrorKind::PermissionDenied, msg))
    }

    /// `bytes` to `target` atomically, never replacing `source` (the file
    /// an export was made from, see `export_atomic`) or the read-only file.
    fn write_export(&self, source: Option<&Path>, target: &Path, bytes: &[u8]) -> io::Result<()> {
        self.guard_write(target)?;
        export_atomic(source, target, bytes)
    }

    /// A supporting file (a Web Page's `<stem>_files/…`), its folder made
    /// first; never the read-only file.
    fn write_supporting(&self, target: &Path, bytes: &[u8]) -> io::Result<()> {
        self.guard_write(target)?;
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(target, bytes)
    }

    /// *Always create backup*'s copy of `dest` ([`keep_backup`]); never
    /// over the read-only file.
    fn write_backup(&self, dest: &Path) -> io::Result<()> {
        self.guard_write(&backup_path(dest))?;
        keep_backup(dest)
    }

    /// Write the workbook to `self.path`. A failure's message is both the
    /// status line and the `Err`, so a caller that reports it (the control
    /// surface's `wb.save`) says exactly what the status bar says.
    fn save_current(&mut self) -> Result<(), String> {
        // Every save of the workbook comes through here: Ctrl+S, `:w`/`:wq`,
        // Save As (which binds the path first), and the control surface's
        // `wb.save`. Other file writes go through write_export,
        // write_supporting and write_backup.
        if let Err(msg) = self.refuse_read_only(&self.path) {
            self.status = Some(msg.clone());
            return Err(msg);
        }
        if let Some(t) = self.bound_text_type() {
            return self.save_text(t);
        }
        // A workbook started from a template that this session has not
        // written yet was bound to a name free when it opened. If that name
        // is taken now (another session from the same template), the save
        // moves on to the next free one rather than replace that file.
        // The same for an imported workbook not yet written: if its
        // `<stem>.xlsx` appeared since it opened, it moves on to the next
        // free name rather than replace that file.
        let taken = match (&self.template, &self.import_source) {
            (Some(t), _) if Path::new(&self.path).exists() => {
                template_binding(t).map(|free| std::mem::replace(&mut self.path, free))
            }
            (None, Some(source)) if self.import_unsaved && Path::new(&self.path).exists() => {
                Some(std::mem::replace(&mut self.path, import_binding(source)))
            }
            _ => None,
        };
        let bytes = self.package_bytes();
        if self.pkg.always_create_backup() {
            if let Err(e) = self.write_backup(Path::new(&self.path)) {
                let msg = format!("save failed: {e}");
                self.status = Some(msg.clone());
                return Err(msg);
            }
        }
        match self.write_export(
            self.import_source.as_deref().map(Path::new),
            Path::new(&self.path),
            &bytes,
        ) {
            Ok(()) => {
                self.modified = false;
                self.text_type = None;
                self.template = None;
                self.import_unsaved = false;
                self.status = Some(match taken {
                    Some(taken) => format!(
                        "Saved {} ({} bytes): {taken} already exists",
                        self.path,
                        bytes.len()
                    ),
                    None => format!("Saved {} ({} bytes)", self.path, bytes.len()),
                });
                Ok(())
            }
            Err(e) => {
                if let Some(taken) = taken {
                    self.path = taken;
                }
                let msg = format!("save failed: {e}");
                self.status = Some(msg.clone());
                Err(msg)
            }
        }
    }

    fn save_as(&mut self, path: String) -> bool {
        let previous = std::mem::replace(&mut self.path, path);
        // A name chosen in Save As is written as chosen, even over a file.
        let template = self.template.take();
        let import_unsaved = std::mem::take(&mut self.import_unsaved);
        if self.save_current().is_ok() {
            self.import_source = None;
            true
        } else {
            // A failed Save As must retain protection for the imported file.
            self.path = previous;
            self.template = template;
            self.import_unsaved = import_unsaved;
            false
        }
    }

    /// The text type Ctrl+S writes: the one last saved as, while the path
    /// still has its extension.
    fn bound_text_type(&self) -> Option<usize> {
        let t = self.text_type?;
        let text = matches!(
            save_kind(SAVE_TYPES.get(t)?),
            SaveKind::Text { .. } | SaveKind::Prn | SaveKind::WebPage
        );
        (text && has_type_ext(&self.path, t)).then_some(t)
    }

    /// Write the active sheet to `self.path` as text type `t`. Only that sheet
    /// is kept, so the workbook stays modified: closing still asks to save,
    /// as Excel does after saving as CSV.
    fn save_text(&mut self, t: usize) -> Result<(), String> {
        let ty = SAVE_TYPES[t];
        let wb = &self.pkg.workbook;
        let sheet = &wb.sheets[self.sheet.min(wb.sheets.len() - 1)];
        let mut extra: Vec<(std::path::PathBuf, String)> = Vec::new();
        let bytes = match save_kind(&ty) {
            SaveKind::Text { delim, encoding } => gridcore::textio::encode(
                &gridcore::textio::sheet_text(sheet, &wb.styles, wb.date1904, delim),
                encoding,
            ),
            SaveKind::Prn => gridcore::textio::encode(
                &gridcore::textio::sheet_prn(sheet, &wb.styles, wb.date1904),
                gridcore::textio::Encoding::Windows1252,
            ),
            SaveKind::WebPage => {
                let stem = file_stem(&self.path);
                let file_name = Path::new(&self.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("{stem}.htm"));
                let page = gridcore::textio::web_page(sheet, &wb.styles, wb.date1904, &file_name);
                let folder = Path::new(&self.path).with_file_name(format!("{stem}_files"));
                extra = page
                    .files
                    .into_iter()
                    .map(|(name, body)| (folder.join(name), body))
                    .collect();
                page.htm.into_bytes()
            }
            _ => return Err(format!("{} is not a text type", ty.label)),
        };
        let sheet_name = sheet.name.clone();
        let many = wb.sheets.len() > 1;
        let written = self
            .write_export(
                self.import_source.as_deref().map(Path::new),
                Path::new(&self.path),
                &bytes,
            )
            .and_then(|()| {
                for (p, body) in &extra {
                    self.write_supporting(p, body.as_bytes())?;
                }
                Ok(())
            });
        match written {
            Ok(()) => {
                self.modified = true;
                self.status = Some(if many {
                    format!(
                        "Saved only the active sheet \"{sheet_name}\" to {} as {}: the selected \
                         file type does not support workbooks that contain multiple sheets \
                         (possible data loss).",
                        self.path, ty.label
                    )
                } else {
                    format!(
                        "Saved {} as {}. Some features in your workbook might be lost in this \
                         file type (possible data loss).",
                        self.path, ty.label
                    )
                });
                Ok(())
            }
            Err(e) => {
                let msg = format!("save failed: {e}");
                self.status = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Save As type `t` (an index into [`SAVE_TYPES`]) to `path`: the
    /// workbook package, the active sheet as text, or Excel's refusal for a
    /// type nothing here writes.
    fn save_as_type(&mut self, path: String, t: usize) {
        let ty = SAVE_TYPES[t];
        match save_kind(&ty) {
            SaveKind::Package => self.request_save_as_package(path),
            SaveKind::Text { .. } | SaveKind::Prn | SaveKind::WebPage => {
                let previous = self.text_type.replace(t);
                if !self.save_as(path) {
                    self.text_type = previous;
                }
            }
            SaveKind::XmlData if self.pkg.part("xl/xmlMaps.xml").is_none() => {
                self.status = Some(
                    "Cannot save XML data because the workbook does not contain any XML \
                     mappings."
                        .to_string(),
                );
            }
            SaveKind::XmlData | SaveKind::Unsupported => {
                self.status = Some(format!(
                    "xlsxy cannot save as {} (*.{}) yet; nothing was written. Choose another \
                     type.",
                    ty.label, ty.ext
                ));
            }
        }
    }

    /// Save As to a typed path: the type the workbook is bound to while the
    /// name keeps that type's extension, else the type its extension names
    /// (`out.csv` is CSV UTF-8, never a workbook under a `.csv` name); an
    /// extension no type has keeps the workbook package.
    fn request_save_as(&mut self, path: String) {
        // The type the workbook is bound to wins while the name keeps its
        // extension: Unicode Text or CSV (Comma delimited) saved again stays
        // that type, not the first type with the extension.
        let bound = self.bound_text_type().filter(|&t| has_type_ext(&path, t));
        match bound.or_else(|| type_for_path(&path)) {
            Some(t) => self.save_as_type(path, t),
            None => self.request_save_as_package(path),
        }
    }

    /// Save the package As, first asking (as Excel does) before a macro-free
    /// type drops the workbook's VBA project or Excel 4.0 macros.
    fn request_save_as_package(&mut self, path: String) {
        // Refused before asking about the macros it would drop.
        if let Err(msg) = self.refuse_read_only(&path) {
            self.status = Some(msg);
            return;
        }
        let drops_macros = SpreadsheetKind::from_path(&path).is_some_and(|k| !k.allows_macros());
        let features = self.pkg.macro_features();
        if drops_macros && !features.is_empty() {
            self.confirm = Some(
                backstage::Confirm::new(
                    format!(
                        "The following features cannot be saved in macro-free workbooks: \
                         {}. Save without them?",
                        features.join(", ")
                    ),
                    ConfirmAction::SaveWithoutMacros(path),
                    Color::Green,
                )
                .default_no(),
            );
        } else {
            self.save_as(path);
        }
    }

    /// Yes to [`ConfirmAction::SaveWithoutMacros`]: once the file is written
    /// without them, the open workbook drops its VB project too, so a later
    /// Save As neither asks again nor writes it back into an `.xlsm`. A failed
    /// write keeps it. Excel 4.0 macro sheets stay in the open workbook, as
    /// Excel keeps them: dropping them would shift the sheet indices the
    /// engine, selection, comments and undo history hold. Only the file
    /// written lacks them, and a later Save As to a macro-free type asks again.
    fn save_as_without_macros(&mut self, path: String) {
        if self.save_as(path) {
            self.pkg.remove_vba_project();
        }
    }

    // --- review comments -----------------------------------------------------

    /// Re-read comments from the package (after an author/delete edit).
    fn refresh_comments(&mut self) {
        self.comments = self.pkg.comments();
        if self.comment_sel >= self.comments.len() {
            self.comment_sel = self.comments.len().saturating_sub(1);
        }
    }

    /// The comment on `(row, col)` of the current sheet, if any.
    fn comment_at(&self, row: u32, col: u32) -> Option<&Comment> {
        self.comments
            .iter()
            .find(|c| c.sheet == self.sheet && c.row == row && c.col == col)
    }

    fn has_comment(&self, row: u32, col: u32) -> bool {
        self.comment_at(row, col).is_some()
    }

    /// Start a threaded comment (or a reply, if the cell already has a thread)
    /// on the current cell.
    fn start_comment(&mut self) {
        self.open_prompt(PromptKind::NewComment);
        self.show_comments = true;
    }

    /// Start a legacy note on the current cell (pre-filled when one exists).
    fn start_note(&mut self) {
        let (r, c) = self.cur;
        let existing = self
            .comment_at(r, c)
            .filter(|cm| !cm.threaded)
            .map(|cm| cm.text.clone())
            .unwrap_or_default();
        self.open_prompt(PromptKind::NewNote);
        if let Some(p) = &mut self.prompt {
            p.text = existing;
            p.cursor = p.text.chars().count();
        }
        self.show_comments = true;
    }

    /// Commit a threaded comment/reply onto the current cell.
    fn commit_comment(&mut self, text: &str) {
        let (r, c) = self.cur;
        let text = text.trim();
        if text.is_empty() {
            self.status = Some("Comment cancelled (empty)".to_string());
            return;
        }
        let author = comment_author();
        let reply = self.comment_at(r, c).is_some_and(|cm| cm.threaded);
        if !self
            .pkg
            .add_threaded_comment(self.sheet, r, c, &author, text, &iso_now())
        {
            self.status = Some(WRITE_REFUSED.into());
            return;
        }
        self.modified = true;
        self.refresh_comments();
        self.status = Some(format!(
            "{} on {}",
            if reply {
                "Reply added"
            } else {
                "Comment added"
            },
            cell_name(r, c)
        ));
    }

    /// Commit a legacy note onto the current cell.
    fn commit_note(&mut self, text: &str) {
        let (r, c) = self.cur;
        let text = text.trim();
        if text.is_empty() {
            self.status = Some("Note cancelled (empty)".to_string());
            return;
        }
        let author = comment_author();
        if !self.pkg.set_comment(self.sheet, r, c, &author, text) {
            self.status = Some(WRITE_REFUSED.into());
            return;
        }
        self.modified = true;
        self.refresh_comments();
        self.status = Some(format!("Note added on {}", cell_name(r, c)));
    }

    fn delete_comment(&mut self) {
        let (r, c) = self.cur;
        if !self.has_comment(r, c) {
            self.status = Some("No comment on this cell".to_string());
            return;
        }
        self.pkg.remove_comment(self.sheet, r, c);
        self.modified = true;
        self.refresh_comments();
        self.status = Some(format!("Comment deleted from {}", cell_name(r, c)));
    }

    /// Jump the cursor to the previous/next comment (across sheets).
    fn nav_comment(&mut self, delta: i32) {
        if self.comments.is_empty() {
            self.status = Some("No comments in this workbook".to_string());
            return;
        }
        self.show_comments = true;
        let n = self.comments.len() as i32;
        self.comment_sel = (((self.comment_sel as i32 + delta) % n + n) % n) as usize;
        let c = &self.comments[self.comment_sel];
        let (sheet, row, col) = (c.sheet, c.row, c.col);
        if sheet < self.pkg.workbook.sheets.len() {
            self.sheet = sheet;
        }
        self.cur = (row, col);
        self.anchor = None;
        self.ensure_visible();
        self.status = Some(format!(
            "Comment {}/{} on {}",
            self.comment_sel + 1,
            self.comments.len(),
            cell_name(row, col)
        ));
    }

    fn toggle_comments(&mut self) {
        self.show_comments = !self.show_comments;
        self.status = Some(if self.comments.is_empty() {
            "No comments in this workbook".to_string()
        } else if self.show_comments {
            format!("Showing {} comment(s)", self.comments.len())
        } else {
            "Comments panel hidden".to_string()
        });
    }

    /// Which ribbon toggle buttons are currently "on".
    fn ribbon_toggles(&self) -> Vec<ribbon::Act> {
        let mut v = Vec::new();
        if self.show_comments {
            v.push(ribbon::Act::ToggleComments);
        }
        if self.formula_view {
            v.push(ribbon::Act::FormulaView);
        }
        if self.freeze() != (0, 0) {
            v.push(ribbon::Act::FreezePanes);
        }
        if self.light_theme {
            v.push(ribbon::Act::ThemeToggle);
        }
        if self.auto_hide_ribbon {
            v.push(ribbon::Act::AutoHideRibbon);
        }
        if self.show_hidden {
            v.push(ribbon::Act::ShowHidden);
        }
        if self.show_drawings {
            v.push(ribbon::Act::ShowObjects);
        }
        v
    }

    /// Keyboard navigation while the ribbon is engaged.
    fn ribbon_key(&mut self, code: KeyCode) {
        use ribbon::{Dir, Focus};
        match code {
            KeyCode::Esc => self.ribbon_focus = Focus::None,
            KeyCode::Left | KeyCode::BackTab => {
                self.ribbon_focus = self.ribbon.nav(self.ribbon_focus, Dir::Left);
                if let Focus::Tab(t) = self.ribbon_focus {
                    self.ribbon.set_active(t);
                }
            }
            KeyCode::Right | KeyCode::Tab => {
                self.ribbon_focus = self.ribbon.nav(self.ribbon_focus, Dir::Right);
                if let Focus::Tab(t) = self.ribbon_focus {
                    self.ribbon.set_active(t);
                }
            }
            KeyCode::Up => self.ribbon_focus = self.ribbon.nav(self.ribbon_focus, Dir::Up),
            KeyCode::Down => self.ribbon_focus = self.ribbon.nav(self.ribbon_focus, Dir::Down),
            KeyCode::Enter => match self.ribbon_focus {
                Focus::Tab(t) if self.ribbon.tab_is_file(t) => {
                    self.ribbon_focus = Focus::None;
                    self.open_backstage();
                }
                Focus::Tab(t) => {
                    self.ribbon.set_active(t);
                    self.ribbon_focus = self.ribbon.enter_body();
                }
                Focus::Button(_) => {
                    if let Some((act, _)) = self.ribbon.focus_act(self.ribbon_focus) {
                        self.ribbon_focus = Focus::None; // apply, then collapse
                        self.ribbon_act(act);
                    }
                }
                Focus::None => {}
            },
            _ => {}
        }
    }

    // --- cell formatting -----------------------------------------------------

    /// Apply `f` to the `Xf` of every cell in the selection (interning the
    /// result so styles aren't duplicated), as one undoable edit.
    fn apply_format(&mut self, f: impl Fn(&mut Xf)) {
        let (r1, c1, r2, c2) = self.iter_selection();
        let snapshot: Vec<(u32, u32, u32)> = {
            let sheet = self.sheet();
            let mut v = Vec::new();
            for r in r1..=r2 {
                for c in c1..=c2 {
                    v.push((r, c, sheet.cell(r, c).map_or(0, |cl| cl.style)));
                }
            }
            v
        };
        let mut styles = Vec::new();
        for (r, c, cur) in snapshot {
            let mut xf = self.pkg.workbook.styles.xf(cur);
            f(&mut xf);
            let idx = self.pkg.workbook.styles.intern(xf);
            styles.push((r, c, idx));
        }
        // Only styles change: a spilled block stays spilled (#784).
        self.apply_styles_on(self.sheet, styles);
    }

    fn toggle_bold(&mut self) {
        self.apply_format(|x| x.bold = !x.bold);
        self.status = Some("Bold".to_string());
    }

    fn toggle_italic(&mut self) {
        self.apply_format(|x| x.italic = !x.italic);
        self.status = Some("Italic".to_string());
    }

    fn set_align(&mut self, a: Align) {
        self.apply_format(move |x| x.align = a);
    }

    /// Toggle Wrap Text on the selection. Wrapped cells render across multiple
    /// lines (the row grows to fit); Excel auto-fits the row height on open.
    fn toggle_wrap(&mut self) {
        self.apply_format(|x| x.wrap = !x.wrap);
        self.status = Some("Wrap text".to_string());
    }

    /// Set (or clear) an explicit height in points for every row in the
    /// selection. "auto"/"0"/empty clears it so the row auto-fits.
    fn commit_row_height(&mut self, text: &str) {
        let t = text.trim();
        let pts = if t.is_empty() || t.eq_ignore_ascii_case("auto") {
            None
        } else {
            match t.parse::<f64>() {
                Ok(h) if h > 0.0 => Some(h),
                Ok(_) => None,
                Err(_) => {
                    self.status = Some("Row height: enter a number of points (or 'auto')".into());
                    return;
                }
            }
        };
        let (r1, _, r2, _) = self.selection();
        let s = self.sheet;
        self.structural(move |wb| {
            for r in r1..=r2 {
                wb.sheets[s].set_row_height(r, pts);
            }
        });
        self.status = Some(match pts {
            Some(h) => format!("Row height {h}"),
            None => "Row height auto".into(),
        });
    }

    fn open_picker(&mut self, kind: PickKind) {
        self.format_picker = Some(FormatPicker { kind, sel: 0 });
    }

    fn picker_len(kind: PickKind) -> usize {
        match kind {
            PickKind::NumberFormat => NUMFMT_OPTIONS.len(),
            _ => COLOR_OPTIONS.len(),
        }
    }

    /// Handle a key while the formatting popup is open.
    fn picker_key(&mut self, code: KeyCode) {
        let Some(p) = &mut self.format_picker else {
            return;
        };
        let len = Self::picker_len(p.kind);
        match code {
            KeyCode::Esc => self.format_picker = None,
            KeyCode::Up => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down => p.sel = (p.sel + 1).min(len - 1),
            KeyCode::Enter => self.apply_picker(),
            _ => {}
        }
    }

    fn apply_picker(&mut self) {
        let Some(p) = self.format_picker.take() else {
            return;
        };
        match p.kind {
            PickKind::NumberFormat => {
                let (label, code) = NUMFMT_OPTIONS[p.sel];
                let code = code.map(str::to_string);
                self.apply_format(move |x| x.set_code(code.clone()));
                self.status = Some(format!("Number format: {label}"));
            }
            PickKind::FontColor => {
                let (label, rgb) = COLOR_OPTIONS[p.sel];
                self.apply_format(move |x| x.color = rgb);
                self.status = Some(format!("Font color: {label}"));
            }
            PickKind::FillColor => {
                let (label, rgb) = COLOR_OPTIONS[p.sel];
                self.apply_format(move |x| x.fill = rgb);
                self.status = Some(format!("Fill color: {label}"));
            }
        }
    }

    // --- Format Cells dialog (Ctrl+1) ---------------------------------------

    fn open_format_dialog(&mut self) {
        self.format_dialog = Some(FormatDialog { section: 0, sel: 0 });
    }

    /// Number of options in a Format Cells section: Number=formats, Font=Bold/
    /// Italic + font colours, Fill=colours, Align=L/C/R, Border=box toggle.
    fn fmt_section_len(section: usize) -> usize {
        match section {
            0 => NUMFMT_OPTIONS.len(),
            1 => 2 + COLOR_OPTIONS.len(),
            2 => COLOR_OPTIONS.len(),
            3 => 3,
            4 => 1,
            _ => 0,
        }
    }

    fn format_dialog_key(&mut self, code: KeyCode) {
        let Some(d) = &mut self.format_dialog else {
            return;
        };
        let n = FMT_SECTIONS.len();
        match code {
            KeyCode::Esc => self.format_dialog = None,
            KeyCode::Left | KeyCode::BackTab => {
                d.section = (d.section + n - 1) % n;
                d.sel = 0;
            }
            KeyCode::Right | KeyCode::Tab => {
                d.section = (d.section + 1) % n;
                d.sel = 0;
            }
            KeyCode::Up => d.sel = d.sel.saturating_sub(1),
            KeyCode::Down => {
                let len = Self::fmt_section_len(d.section);
                d.sel = (d.sel + 1).min(len.saturating_sub(1));
            }
            KeyCode::Enter => self.apply_format_dialog(),
            _ => {}
        }
    }

    /// Apply the highlighted option; the dialog stays open so several attributes
    /// can be set in one visit (Esc closes it).
    fn apply_format_dialog(&mut self) {
        let Some((section, sel)) = self.format_dialog.as_ref().map(|d| (d.section, d.sel)) else {
            return;
        };
        match section {
            0 => {
                let (label, code) = NUMFMT_OPTIONS[sel];
                let code = code.map(str::to_string);
                self.apply_format(move |x| x.set_code(code.clone()));
                self.status = Some(format!("Number format: {label}"));
            }
            1 => match sel {
                0 => self.toggle_bold(),
                1 => self.toggle_italic(),
                _ => {
                    let (label, rgb) = COLOR_OPTIONS[sel - 2];
                    self.apply_format(move |x| x.color = rgb);
                    self.status = Some(format!("Font color: {label}"));
                }
            },
            2 => {
                let (label, rgb) = COLOR_OPTIONS[sel];
                self.apply_format(move |x| x.fill = rgb);
                self.status = Some(format!("Fill: {label}"));
            }
            3 => {
                let a = [Align::Left, Align::Center, Align::Right][sel];
                self.set_align(a);
            }
            4 => {
                self.apply_format(|x| x.border = !x.border);
                self.status = Some("Toggled box border".into());
            }
            _ => {}
        }
    }

    // --- view toggles --------------------------------------------------------

    fn base_style(&self) -> Style {
        if self.light_theme {
            Style::new().fg(Color::Black).bg(Color::White)
        } else {
            Style::new()
        }
    }

    fn toggle_formula_view(&mut self) {
        self.formula_view = !self.formula_view;
        self.status = Some(
            if self.formula_view {
                "Showing formulas"
            } else {
                "Showing values"
            }
            .to_string(),
        );
    }

    /// The current sheet's frozen panes as (rows, cols) — read from the file's
    /// `<pane state="frozen">` on open, and updated by [`Self::toggle_freeze`].
    fn freeze(&self) -> (u32, u32) {
        self.pkg
            .workbook
            .sheets
            .get(self.sheet)
            .map(|s| s.freeze)
            .unwrap_or((0, 0))
    }

    fn set_freeze(&mut self, f: (u32, u32)) {
        if let Some(s) = self.pkg.workbook.sheets.get_mut(self.sheet) {
            s.freeze = f;
        }
    }

    fn toggle_freeze(&mut self) {
        if self.freeze() == (0, 0) {
            let at = self.cur;
            self.set_freeze(at);
            self.status = Some(if at == (0, 0) {
                "Nothing to freeze at A1".to_string()
            } else {
                format!("Froze {} row(s), {} col(s)", at.0, at.1)
            });
        } else {
            self.set_freeze((0, 0));
            self.status = Some("Unfroze panes".to_string());
        }
    }

    /// Follow the hyperlink on cell (row, col), if any: an in-workbook
    /// `#Sheet!A1` jumps immediately; an external URL is queued for confirmation.
    fn follow_hyperlink(&mut self, row: u32, col: u32) {
        let Some(target) = self.sheet().hyperlinks.get(&(row, col)).cloned() else {
            return;
        };
        if let Some(loc) = target.strip_prefix('#') {
            let (sheet_name, cell) = match loc.rsplit_once('!') {
                Some((s, c)) => (Some(s.trim_matches('\'')), c),
                None => (None, loc),
            };
            if let Some(sname) = sheet_name {
                if let Some(idx) = self
                    .pkg
                    .workbook
                    .sheets
                    .iter()
                    .position(|s| s.name == sname)
                {
                    if idx != self.sheet {
                        self.goto_sheet(idx);
                    }
                }
            }
            if let Some((r, c)) = gridcore::sheet::parse_cell_name(&cell.replace('$', "")) {
                self.cur = (r, c);
                self.status = Some(format!("Jumped to {loc}"));
            }
        } else if safe_url(&target) {
            // External link: never opened directly — confirm first.
            self.status = Some(format!("Open link? {target}   (y = open)"));
            self.pending_link = Some(target);
        } else {
            self.status = Some(format!("Blocked non-web link: {target}"));
        }
    }

    /// The data-validation rule covering the cursor cell, if any.
    fn current_validation(&self) -> Option<&gridcore::sheet::DataValidation> {
        let (r, c) = self.cur;
        self.sheet().validations.iter().find(|v| v.covers(r, c))
    }

    /// The allowed values for a `list` validation: the inline CSV, or the cells
    /// of the range / named range its `formula1` points at.
    fn resolve_list_values(&self, dv: &gridcore::sheet::DataValidation) -> Vec<String> {
        if let Some(v) = dv.list_values() {
            return v;
        }
        let refstr = dv.formula1.trim();
        if refstr.is_empty() {
            return Vec::new();
        }
        // A named range resolves to its own reference formula first.
        let resolved = self
            .pkg
            .workbook
            .defined_names
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(refstr))
            .map(|d| d.formula.clone());
        let refstr = resolved.as_deref().unwrap_or(refstr);
        // Optional Sheet! prefix; the range itself may carry `$` anchors.
        let (sheet_name, rng) = match refstr.rsplit_once('!') {
            Some((s, r)) => (Some(s.trim_matches(['\'', ' ', '='])), r),
            None => (None, refstr),
        };
        let sidx = match sheet_name {
            Some(n) => self
                .pkg
                .workbook
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(n))
                .unwrap_or(self.sheet),
            None => self.sheet,
        };
        let clean = rng.replace('$', "");
        let Some((r1, c1, r2, c2)) = gridcore::sheet::parse_range_name(&clean)
            .or_else(|| gridcore::sheet::parse_cell_name(&clean).map(|(r, c)| (r, c, r, c)))
        else {
            return Vec::new();
        };
        let sheet = &self.pkg.workbook.sheets[sidx];
        let styles = &self.pkg.workbook.styles;
        let date1904 = self.pkg.workbook.date1904;
        let mut out = Vec::new();
        // Bound the scan: dropdowns are small, and a whole-column ref is huge.
        for r in r1..=r2.min(r1.saturating_add(1024)) {
            for c in c1..=c2 {
                if let Some(cell) = sheet.cell(r, c) {
                    let text = format_with(&styles.xf(cell.style), &cell.value, date1904);
                    if !text.is_empty() {
                        out.push(text);
                    }
                }
            }
        }
        out
    }

    /// Open the dropdown for the `list` validation on the cursor cell, if there
    /// is one. Preselects the current cell value when it is among the choices.
    fn open_dv_dropdown(&mut self) {
        let Some(dv) = self.current_validation() else {
            return;
        };
        if dv.kind != "list" {
            self.status = Some(format!("Data validation — {}", dv.describe()));
            return;
        }
        let dv = dv.clone();
        let values = self.resolve_list_values(&dv);
        if values.is_empty() {
            self.status = Some(format!("List: {} (no resolvable values)", dv.formula1));
            return;
        }
        let current = self.current_input_text();
        let sel = values.iter().position(|v| *v == current).unwrap_or(0);
        self.dv_picker = Some(DvPicker { values, sel });
    }

    fn dv_picker_key(&mut self, code: KeyCode) {
        let Some(p) = self.dv_picker.as_mut() else {
            return;
        };
        let n = p.values.len();
        match code {
            KeyCode::Esc => self.dv_picker = None,
            KeyCode::Up | KeyCode::Char('k') => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.sel = (p.sel + 1).min(n - 1),
            KeyCode::Home => p.sel = 0,
            KeyCode::End => p.sel = n - 1,
            KeyCode::Enter | KeyCode::Tab => {
                let value = p.values[p.sel].clone();
                self.dv_picker = None;
                let (r, c) = self.cur;
                match entry_cell(
                    &mut self.pkg.workbook,
                    self.sheet,
                    r,
                    c,
                    &value,
                    now_serial(),
                ) {
                    Ok(cell) => {
                        if self.apply(vec![(r, c, cell)]) {
                            self.status = Some(format!("Set {} = {value}", cell_name(r, c)));
                        }
                    }
                    Err(e) => self.status = Some(e.to_string()),
                }
            }
            _ => {}
        }
    }

    fn toggle_theme(&mut self) {
        self.light_theme = !self.light_theme;
        self.status = Some(
            if self.light_theme {
                "Light theme"
            } else {
                "Dark theme"
            }
            .to_string(),
        );
    }

    /// Load persisted view preferences (best effort).
    fn load_view_prefs(&mut self) {
        let Some(p) = view_prefs_path() else { return };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return;
        };
        self.apply_view_prefs(&text);
    }

    /// Apply the preferences file's text.
    fn apply_view_prefs(&mut self, text: &str) {
        self.auto_convert = auto_convert_from_prefs(text);
        self.edit_opts = EditOptions::from_text(text);
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let on = v.trim() == "1";
                match k.trim() {
                    "formula_view" => self.formula_view = on,
                    "light_theme" => self.light_theme = on,
                    "auto_hide_ribbon" => self.auto_hide_ribbon = on,
                    "show_comments" => self.show_comments = on,
                    "alt_startup_path" => {
                        self.alt_startup = Some(v.trim().to_string()).filter(|d| !d.is_empty())
                    }
                    _ => {}
                }
            }
        }
    }

    /// Persist view preferences (best effort; freeze is per-file, not saved).
    fn save_view_prefs(&self) {
        let Some(p) = view_prefs_path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, self.view_prefs_text());
    }

    /// The preferences file's text.
    fn view_prefs_text(&self) -> String {
        let auto = self.auto_convert;
        let mut text = format!(
            "formula_view={}\nlight_theme={}\nauto_hide_ribbon={}\nshow_comments={}\n",
            self.formula_view as u8,
            self.light_theme as u8,
            self.auto_hide_ribbon as u8,
            self.show_comments as u8,
        );
        let switches = [
            auto.remove_leading_zeros,
            auto.keep_15_digits,
            auto.e_notation,
            auto.dates,
        ];
        for (key, on) in CONVERT_KEYS.into_iter().zip(switches) {
            text.push_str(&format!("{key}={}\n", u8::from(on)));
        }
        text.push_str(&self.edit_opts.to_lines());
        if let Some(dir) = &self.alt_startup {
            text.push_str(&format!("alt_startup_path={dir}\n"));
        }
        text
    }

    /// Dispatch a ribbon command to the matching editor operation.
    fn ribbon_act(&mut self, act: ribbon::Act) {
        use ribbon::Act::*;
        match act {
            Cut => self.copy(true),
            Copy => self.copy(false),
            Paste => self.paste(),
            Undo => self.undo(),
            Redo => self.redo(),
            Find => self.open_prompt(PromptKind::Find),
            Replace => self.open_prompt(PromptKind::ReplaceFind),
            GoTo => self.open_prompt(PromptKind::GoTo),
            ClearContents => self.clear_selection(),
            FillDown => self.fill(FillDir::Down),
            FillRight => self.fill(FillDir::Right),
            FillUp => self.fill(FillDir::Up),
            FillLeft => self.fill(FillDir::Left),
            PasteSpecial => self.open_paste_special(),
            InsertRow => self.row_op(true),
            InsertCol => self.col_op(true),
            DeleteRow => self.row_op(false),
            DeleteCol => self.col_op(false),
            SortAsc => self.sort_region(true),
            SortDesc => self.sort_region(false),
            CustomSort => self.open_prompt(PromptKind::SortKeys),
            AutoSum => self.autosum(),
            InsertChart(kind) => self.insert_chart(kind),
            AddSheet => self.open_prompt(PromptKind::AddSheet),
            RenameSheet => self.open_prompt(PromptKind::RenameSheet),
            Save => self.save(),
            SaveAs => self.open_prompt(PromptKind::SaveAs),
            Bold => self.toggle_bold(),
            Italic => self.toggle_italic(),
            AlignLeft => self.set_align(Align::Left),
            AlignCenter => self.set_align(Align::Center),
            AlignRight => self.set_align(Align::Right),
            NumberFormat => self.open_picker(PickKind::NumberFormat),
            FontColor => self.open_picker(PickKind::FontColor),
            FillColor => self.open_picker(PickKind::FillColor),
            MergeCenter => self.merge_toggle(),
            WrapText => self.toggle_wrap(),
            RowHeight => self.open_prompt(PromptKind::RowHeight),
            CondFormat => self.open_prompt(PromptKind::CondFormat),
            DataValidation => self.open_prompt(PromptKind::DataValidation),
            Filter => self.open_prompt(PromptKind::Filter),
            RemoveDuplicates => self.remove_duplicates(),
            TextToColumns => self.open_text_to_columns(),
            FormatAsTable => self.format_as_table(),
            TableName => self.table_name_act(),
            ResizeTable => self.resize_table_act(),
            ConvertToRange => self.convert_table_act(),
            Consolidate => self.open_consolidate(),
            Subtotal => self.subtotal(),
            GroupOutline => self.group_outline(false),
            UngroupOutline => self.group_outline(true),
            ShowDetail => self.outline_detail(true),
            HideDetail => self.outline_detail(false),
            AutoOutline => self.auto_outline(),
            ClearOutline => self.clear_outline(),
            OutlineSettings => self.open_outline_settings(),
            NewComment => self.start_comment(),
            NewNote => self.start_note(),
            DeleteComment => self.delete_comment(),
            PrevComment => self.nav_comment(-1),
            NextComment => self.nav_comment(1),
            ToggleComments => self.toggle_comments(),
            ProtectSheet => self.toggle_protection(),
            FormulaView => self.toggle_formula_view(),
            FreezePanes => self.toggle_freeze(),
            ShowHidden => self.toggle_show_hidden(),
            ShowObjects => {
                self.show_drawings = !self.show_drawings;
                self.status = Some(
                    if self.show_drawings {
                        "Showing pictures & charts"
                    } else {
                        "Hiding pictures & charts"
                    }
                    .to_string(),
                );
            }
            ThemeToggle => self.toggle_theme(),
            AutoHideRibbon => {
                self.auto_hide_ribbon = !self.auto_hide_ribbon;
                self.status = Some(
                    if self.auto_hide_ribbon {
                        "Ribbon auto-hide on"
                    } else {
                        "Ribbon auto-hide off"
                    }
                    .to_string(),
                );
            }
            Todo(name) => self.status = Some(format!("{name}: not implemented yet")),
        }
    }

    // --- File backstage ------------------------------------------------------

    /// Set File › Info's property row `i` to `text` (empty removes it); past
    /// [`INFO_FIELDS`], `text` is `Name = value` for a custom property, its
    /// type read from the value ([`CustomValue::from_input`]). Not undoable,
    /// as in Excel; marks the workbook modified when something changed.
    /// Returns what happened, for the Info page to say.
    fn commit_doc_property(&mut self, i: usize, text: &str) -> Option<String> {
        let before = self.pkg.doc_properties();
        let mut p = before.clone();
        let label = match info_field(&mut p, i) {
            Some(slot) => {
                *slot = (!text.is_empty()).then(|| text.to_string());
                INFO_FIELDS[i].0
            }
            None => {
                if text.is_empty() {
                    return None;
                }
                let Some((name, value)) = text.split_once('=') else {
                    return Some("Custom property: type Name = value".to_string());
                };
                let (name, value) = (name.trim(), value.trim());
                if name.is_empty() {
                    return Some("Custom property: the name is missing".to_string());
                }
                let at = p
                    .custom
                    .iter()
                    .position(|c| c.name.to_lowercase() == name.to_lowercase());
                match (at, value.is_empty()) {
                    (Some(at), true) => {
                        p.custom.remove(at);
                    }
                    (Some(at), false) => p.custom[at].value = CustomValue::from_input(value),
                    (None, false) => p.custom.push(CustomProperty {
                        name: name.to_string(),
                        value: CustomValue::from_input(value),
                    }),
                    (None, true) => {
                        return Some(format!("No custom property named {name}"));
                    }
                }
                "Custom property"
            }
        };
        if p == before {
            return None;
        }
        match self.pkg.set_doc_properties(&p) {
            Ok(()) => {
                self.modified = true;
                Some(format!("{label} updated"))
            }
            Err(e) => Some(e),
        }
    }

    /// Back to File › Info on row `row`, after its prompt, saying `message`
    /// there (the status bar is hidden under the backstage).
    fn reopen_info(&mut self, row: usize, message: Option<String>) {
        self.open_backstage();
        if let Some(b) = &mut self.backstage {
            b.focus_info(row);
            b.info_message = message;
        }
    }

    /// Open the File backstage rooted at the current file's directory.
    fn open_backstage(&mut self) {
        let dir = std::path::Path::new(&self.path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        self.backstage = Some(
            backstage::Backstage::open(dir, self.extensions())
                .with_option_rows(self.option_rows())
                .with_save_types(
                    &SAVE_TYPES,
                    "Export the current sheet as CSV UTF-8 next to the workbook",
                )
                .with_extra_exports(&["Export the current sheet as PDF next to the workbook"]),
        );
        self.ribbon_focus = ribbon::Focus::None;
    }

    /// File › Options' rows: Data's Automatic Data Conversion switches and
    /// Advanced › Editing's options (#672), each keyed by its preference key.
    /// Fill handle and drag-and-drop is left out: xlsxy has neither.
    fn option_rows(&self) -> Vec<backstage::OptRow> {
        use backstage::OptRow;
        use gridcore::options as o;
        let data = "Data › Automatic Data Conversion (opening .csv and text files)";
        let editing = "Advanced › Editing options";
        let auto = self.auto_convert;
        let e = self.edit_opts;
        let dirs = EnterMove::ALL.map(EnterMove::label);
        let at = EnterMove::ALL
            .iter()
            .position(|m| *m == e.enter_move)
            .unwrap_or(0);
        vec![
            OptRow::check(
                CONVERT_KEYS[0],
                data,
                "Remove leading zeros and convert to number",
                auto.remove_leading_zeros,
            ),
            OptRow::check(
                CONVERT_KEYS[1],
                data,
                "Keep first 15 digits of long numbers and display in scientific notation if needed",
                auto.keep_15_digits,
            ),
            OptRow::check(
                CONVERT_KEYS[2],
                data,
                "Convert digits surrounding the letter \"E\" to a number in scientific notation",
                auto.e_notation,
            ),
            OptRow::check(
                CONVERT_KEYS[3],
                data,
                "Convert continuous letters and numbers to a date",
                auto.dates,
            ),
            OptRow::check(
                o::KEY_FIXED_DECIMAL,
                editing,
                "Automatically insert a decimal point",
                e.fixed_decimal,
            ),
            OptRow::int(
                o::KEY_PLACES,
                editing,
                "Places",
                i32::from(e.places),
                i32::from(o::PLACES_MIN),
                i32::from(o::PLACES_MAX),
            )
            .depends_on(o::KEY_FIXED_DECIMAL),
            OptRow::check(
                o::KEY_MOVE_AFTER_ENTER,
                editing,
                "After pressing Enter, move selection",
                e.move_after_enter,
            ),
            OptRow::choice(o::KEY_MOVE_DIRECTION, editing, "Direction", &dirs, at)
                .depends_on(o::KEY_MOVE_AFTER_ENTER),
            OptRow::check(
                o::KEY_EDIT_IN_CELL,
                editing,
                "Allow editing directly in cells",
                e.edit_in_cell,
            ),
            OptRow::check(
                o::KEY_AUTOCOMPLETE,
                editing,
                "Enable AutoComplete for cell values",
                e.autocomplete,
            ),
        ]
    }

    /// Take File › Options' rows into the app, by key (they are saved with
    /// the other preferences).
    fn sync_backstage_options(&mut self) {
        use gridcore::options as o;
        let Some(b) = &self.backstage else {
            return;
        };
        if b.options.is_empty() {
            return;
        }
        let check = |k: &str, was: bool| b.option_check(k).unwrap_or(was);
        let auto = self.auto_convert;
        self.auto_convert = AutoConvert {
            remove_leading_zeros: check(CONVERT_KEYS[0], auto.remove_leading_zeros),
            keep_15_digits: check(CONVERT_KEYS[1], auto.keep_15_digits),
            e_notation: check(CONVERT_KEYS[2], auto.e_notation),
            dates: check(CONVERT_KEYS[3], auto.dates),
        };
        let e = self.edit_opts;
        self.edit_opts = EditOptions {
            fixed_decimal: check(o::KEY_FIXED_DECIMAL, e.fixed_decimal),
            places: b
                .option_int(o::KEY_PLACES)
                .and_then(|p| i16::try_from(p).ok())
                .unwrap_or(e.places),
            move_after_enter: check(o::KEY_MOVE_AFTER_ENTER, e.move_after_enter),
            enter_move: b
                .option_choice(o::KEY_MOVE_DIRECTION)
                .and_then(|i| EnterMove::ALL.get(i).copied())
                .unwrap_or(e.enter_move),
            edit_in_cell: check(o::KEY_EDIT_IN_CELL, e.edit_in_cell),
            autocomplete: check(o::KEY_AUTOCOMPLETE, e.autocomplete),
            fill_handle: e.fill_handle,
        };
    }

    /// Leave the File backstage via a click on the ribbon tab strip. Clicking
    /// the File header closes the panel back to the grid; any other tab
    /// switches to it and opens its ribbon.
    fn backstage_tab_click(&mut self, tab: usize) {
        self.backstage = None;
        if self.ribbon.tab_is_file(tab) {
            self.ribbon_focus = ribbon::Focus::None;
        } else {
            self.ribbon.set_active(tab);
            self.ribbon_focus = ribbon::Focus::Tab(tab);
        }
    }

    /// Replace the whole editing session with a freshly loaded workbook. A
    /// `.txt`/`.prn` opens the Text Import Wizard first.
    fn open_workbook(&mut self, path: &str) {
        if is_text_import(path) {
            match std::fs::read(path) {
                Ok(bytes) => {
                    self.text_dialog = Some(textdlg::TextDialog::import(path.to_string(), bytes));
                    self.backstage = None;
                    self.start_screen = false;
                }
                Err(e) => self.status = Some(format!("Open failed: {e}")),
            }
            return;
        }
        match load_workbook(path, &self.text_open()) {
            Ok((pkg, p, import_source, format)) => {
                // A person's Open that loads ends read-only (#882), the
                // read-only file's own included.
                self.read_only = None;
                self.install_workbook(pkg, p, import_source);
                self.note_import(format);
                self.note_template(path);
            }
            Err(e) => self.status = Some(format!("Open failed: {e}")),
        }
    }

    /// After an import, the first save rechecks the binding. After
    /// importing a workbook read as `format` (an `.xls`, `.xlsb` or `.ods`),
    /// the status line names the format and the `.xlsx` a save writes.
    fn note_import(&mut self, format: Option<SourceFormat>) {
        self.import_unsaved = self.import_source.is_some();
        if let (Some(format), Some(source)) = (format, &self.import_source) {
            self.status = Some(format!(
                "Opened {source} ({}); saving writes {}",
                format.label(),
                self.path
            ));
        }
    }

    /// After opening `opened`: when it was a template, the workbook is a new
    /// one started from it, and the status line says so.
    fn note_template(&mut self, opened: &str) {
        if is_template(opened) && self.path != opened {
            self.template = Some(opened.to_string());
            self.status = Some(format!("New workbook {} from template {opened}", self.path));
        }
    }

    /// Make `pkg` (loaded from, and to be saved to, `p`) the open workbook.
    fn install_workbook(&mut self, pkg: SheetPackage, p: String, import_source: Option<String>) {
        let (rels, meas) = pkg
            .part(MODEL_PART)
            .map(|b| parse_model_part(&String::from_utf8_lossy(b)))
            .unwrap_or_default();
        let comments = pkg.comments();
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        self.engine = engine;
        self.pkg = pkg;
        self.forget_clip();
        self.path = p;
        self.import_source = import_source;
        self.template = None;
        self.import_unsaved = false;
        self.model_rels = rels;
        self.model_measures = meas;
        self.comments = comments;
        self.reset_view();
        self.modified = false;
        self.backstage = None;
        self.start_screen = false;
        self.status = Some(if !self.circles_shown() {
            format!("Opened {}", self.path)
        } else {
            format!("Opened {}. {CIRCULAR_WARNING}", self.path)
        });
    }

    /// Open `path` without a dialog, as the control surface does: a
    /// `.txt`/`.prn` is imported with the Text Import Wizard's defaults
    /// (`sheet.import-text` takes other options) instead of showing it.
    /// A load that fails is the error (the verb reports it).
    fn open_without_wizard(&mut self, path: &str) -> Result<(), String> {
        let (pkg, save, source, format) = load_workbook(path, &self.text_open())?;
        self.install_workbook(pkg, save, source);
        self.note_import(format);
        self.note_template(path);
        Ok(())
    }

    /// Re-read the file the workbook is bound to, dropping unsaved edits. A
    /// workbook saved as CSV, Text (Tab delimited) or Unicode Text stays bound
    /// to that file and type (it is re-imported from it), so a later save
    /// writes the text file again rather than a `<name>.xlsx` beside it.
    /// Formatted Text and Web Page cannot be read back, so reload refuses
    /// them and changes nothing.
    fn reload(&mut self) -> Result<(), String> {
        let path = self.path.clone();
        // A workbook started from a template has no file of its own until
        // this session writes one; a file of its name is someone else's.
        if self.template.is_some() {
            let msg = "nothing to revert: not saved yet".to_string();
            self.status = Some(msg.clone());
            return Err(msg);
        }
        let Some(t) = self.bound_text_type() else {
            return self.open_without_wizard(&path);
        };
        let SaveKind::Text { delim, encoding } = save_kind(&SAVE_TYPES[t]) else {
            return Err(format!(
                "{} cannot be read back; reload is not available for this file",
                SAVE_TYPES[t].label
            ));
        };
        // Read back with the delimiter it was written with, never a sniffed
        // one: a first row with as many `;` as `,` must not re-split.
        // And in the encoding it was written in: a 1252 `é` must not be
        // read as UTF-8.
        let origin = match encoding {
            gridcore::textio::Encoding::Windows1252 => gridcore::textio::Origin::Windows1252,
            gridcore::textio::Encoding::Utf8Bom => gridcore::textio::Origin::Utf8,
            gridcore::textio::Encoding::Utf16LeBom => gridcore::textio::Origin::Utf16Le,
        };
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let text = gridcore::textio::decode(&bytes, origin);
        let pkg = text_to_pkg(
            &text,
            &file_stem(&path),
            &TextParse::csv(delim),
            &self.text_open(),
        );
        // The text file is the save target itself, not a source to guard.
        self.install_workbook(pkg, path, None);
        self.text_type = Some(t);
        Ok(())
    }

    /// How a text file opens now: File › Options › Data and the clock.
    fn text_open(&self) -> TextOpen {
        TextOpen {
            auto: self.auto_convert,
            today: now_serial(),
        }
    }

    /// Drop the internal clip when the workbook is replaced: its cells carry
    /// the old workbook's style indices and `cm`/`vm` metadata indices, and a
    /// cut names source cells that no longer exist. A later paste goes through
    /// the OS clipboard's text (values only), as from any other program.
    fn forget_clip(&mut self) {
        self.clip = None;
        self.clip_text = None;
    }

    /// Start a new workbook (discarding the current one): from `book.xltx`
    /// in the XLSTART folder when there is one, as Excel's Ctrl+N does, else
    /// blank. Either way it is `untitled.xlsx`, never bound into XLSTART.
    fn new_workbook(&mut self) {
        let mut failed = None;
        if let Some(t) = self.xlstart.as_deref().and_then(default_template) {
            let name = file_name_of(&t.to_string_lossy());
            match std::fs::read(&t)
                .map_err(|e| e.to_string())
                .and_then(|data| open_any(&data).map_err(|e| e.to_string()))
            {
                Ok((pkg, _)) => {
                    self.install_workbook(pkg, "untitled.xlsx".to_string(), None);
                    self.read_only = None;
                    self.status = Some(format!("New workbook from {name}"));
                    return;
                }
                Err(e) => failed = Some(format!("New workbook ({name} not used: {e})")),
            }
        }
        let pkg = new_xlsx();
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        self.engine = engine;
        self.pkg = pkg;
        self.forget_clip();
        self.path = "untitled.xlsx".to_string();
        self.read_only = None;
        self.import_source = None;
        self.template = None;
        self.import_unsaved = false;
        self.model_rels = Vec::new();
        self.model_measures = Vec::new();
        self.comments = Vec::new();
        self.reset_view();
        self.modified = false;
        self.backstage = None;
        self.start_screen = false;
        self.status = Some(failed.unwrap_or_else(|| "New workbook".to_string()));
    }

    /// At launch with no file: open the first of `files` (see
    /// [`startup_workbooks`]) that loads, instead of the welcome screen, as
    /// Excel opens its startup workbooks instead of a blank one. xlsxy holds
    /// one workbook per window, so the status counts the ones not opened. A
    /// file that fails to load is named and the next one is tried; when none
    /// loads, the welcome screen stays.
    fn open_startup_workbooks(&mut self, files: &[PathBuf]) {
        let mut failed = Vec::new();
        for (i, file) in files.iter().enumerate() {
            let path = file.to_string_lossy();
            match self.open_without_wizard(&path) {
                Ok(()) => {
                    let mut status = self.status.take().unwrap_or_default();
                    for f in &failed {
                        status.push_str(&format!("; {f}"));
                    }
                    let rest = files.len() - i - 1;
                    if rest > 0 {
                        let s = if rest == 1 { "" } else { "s" };
                        status.push_str(&format!(
                            "; {rest} more startup workbook{s} not opened: \
                             xlsxy shows one workbook per window"
                        ));
                    }
                    self.status = Some(status);
                    return;
                }
                Err(e) => failed.push(format!("{} not opened: {e}", file_name_of(&path))),
            }
        }
        if !failed.is_empty() {
            self.startup_note = Some(format!("Startup workbook {}", failed.join("; ")));
        }
    }

    fn reset_view(&mut self) {
        // A workbook opens on the sheet it was saved on.
        let wb = &self.pkg.workbook;
        self.sheet = wb.active_tab.min(wb.sheets.len().saturating_sub(1));
        self.cur = (0, 0);
        self.top = 0;
        self.left = 0;
        self.anchor = None;
        self.edit = None;
        self.undo.clear();
        self.redo.clear();
        self.comment_sel = 0;
    }

    /// Print the current sheet to a `.pdf` next to the workbook. A sheet with
    /// nothing to print says so and writes nothing.
    fn export_pdf(&mut self) {
        let job = gridcore::print::paginate::Job::new(
            gridcore::print::paginate::What::ActiveSheets(vec![self.sheet]),
        );
        let out = match self.path.rsplit_once('.') {
            Some((base, _)) => format!("{base}.pdf"),
            None => format!("{}.pdf", self.path),
        };
        self.status = Some(match print_pdf(&self.pkg.workbook, &job, &self.path) {
            Err(e) => e.to_string(),
            Ok((pdf, pages)) => {
                // Through the App's guarded write, like every App write (#882).
                let source = export_source(&self.path, self.import_source.as_deref(), &out);
                match self.write_export(Some(Path::new(source)), Path::new(&out), &pdf) {
                    Ok(()) => format!(
                        "Exported {out} ({pages} page{})",
                        if pages == 1 { "" } else { "s" }
                    ),
                    Err(e) => format!("Export failed: {e}"),
                }
            }
        });
        self.backstage = None;
    }

    /// Export the current sheet to a `.csv` (CSV UTF-8) next to the workbook.
    fn export_csv(&mut self) {
        let csv = csv_utf8_bytes(self.sheet(), &self.pkg.workbook);
        let out = match self.path.rsplit_once('.') {
            Some((base, _)) => format!("{base}.csv"),
            None => format!("{}.csv", self.path),
        };
        let source = export_source(&self.path, self.import_source.as_deref(), &out);
        match self.write_export(Some(Path::new(source)), Path::new(&out), &csv) {
            Ok(()) => self.status = Some(format!("Exported {out} ({} bytes)", csv.len())),
            Err(e) => self.status = Some(format!("Export failed: {e}")),
        }
        self.backstage = None;
    }

    /// Act on a [`backstage::BackstageEvent`] returned by the backstage's own
    /// `key`/`mouse` handlers. Shared by `backstage_key` and `bs_mouse`.
    /// Returns true when the app should exit.
    fn apply_backstage_event(&mut self, ev: backstage::BackstageEvent) -> bool {
        use backstage::BackstageEvent;
        match ev {
            BackstageEvent::None => false,
            BackstageEvent::Close => {
                self.backstage = None;
                false
            }
            BackstageEvent::New => {
                self.request_discard(Next::New);
                false
            }
            BackstageEvent::Open(p) => {
                let p = p.to_string_lossy().into_owned();
                self.request_discard(Next::Open(p));
                false
            }
            BackstageEvent::Save => {
                self.save();
                self.backstage = None;
                false
            }
            BackstageEvent::SaveAs { dir, name } => {
                self.commit_save_as(dir, name);
                false
            }
            BackstageEvent::Export => {
                self.export_csv();
                false
            }
            BackstageEvent::ExportExtra(_) => {
                self.export_pdf();
                false
            }
            BackstageEvent::Exit => {
                self.request_exit();
                false
            }
            // The minibuffer edits the property; the backstage comes back
            // on Info when it is done (see `reopen_info`).
            BackstageEvent::EditInfo(i) => {
                self.backstage = None;
                self.open_prompt(PromptKind::DocProperty(i as u8));
                false
            }
        }
    }

    /// Open or New from the backstage (#882): a modified workbook asks before
    /// its changes are discarded; an unmodified one goes ahead. The
    /// workbook stops being read-only only once another one has loaded
    /// ([`Self::open_workbook`], the import's finish, [`Self::new_workbook`]),
    /// so an Open that fails or is cancelled leaves the source guarded. The
    /// control surface's `wb.open` is scripted, does not ask, and keeps it.
    fn request_discard(&mut self, next: Next) {
        self.backstage = None;
        if !self.modified {
            self.discard_for(next);
            return;
        }
        let name = file_name_of(&self.path);
        let prompt = match &next {
            Next::Open(p) => {
                let other = file_name_of(p);
                format!("Discard changes to \"{name}\" and open \"{other}\"?")
            }
            Next::New => format!("Discard changes to \"{name}\" and start a new workbook?"),
        };
        self.confirm = Some(
            backstage::Confirm::new(prompt, ConfirmAction::Discard(next), Color::Green)
                .default_no(),
        );
    }

    /// Yes to [`ConfirmAction::Discard`], or an Open/New with nothing to lose.
    fn discard_for(&mut self, next: Next) {
        match next {
            Next::Open(p) => self.open_workbook(&p),
            Next::New => self.new_workbook(),
        }
    }

    /// Open the Exit confirmation modal (used by Ctrl+Q and File ▸ Exit).
    fn request_exit(&mut self) {
        self.backstage = None;
        let prompt = if self.modified {
            "Exit xlsxy? Unsaved changes will be lost."
        } else {
            "Exit xlsxy?"
        };
        self.confirm = Some(backstage::Confirm::new(
            prompt,
            ConfirmAction::Exit,
            Color::Green,
        ));
    }

    /// Open the "delete this sheet?" confirmation modal (Shift-Del).
    fn request_delete_sheet(&mut self) {
        let name = self.pkg.workbook.sheets[self.sheet].name.clone();
        self.confirm = Some(
            backstage::Confirm::new(
                format!("Delete sheet '{name}'?"),
                ConfirmAction::DeleteSheet,
                Color::Green,
            )
            .default_no(),
        );
    }

    /// Act on the shared dialog's outcome. Returns true if the app should quit.
    fn apply_confirm(&mut self, outcome: backstage::ConfirmOutcome<ConfirmAction>) -> bool {
        match outcome {
            backstage::ConfirmOutcome::Pending => false,
            backstage::ConfirmOutcome::Cancelled => {
                if let Some(ConfirmAction::SaveWithoutMacros(_)) =
                    self.confirm.take().map(|c| c.action().clone())
                {
                    self.status = Some("Save As cancelled — the macros are kept".into());
                }
                false
            }
            backstage::ConfirmOutcome::Confirmed(action) => {
                self.confirm = None;
                match action {
                    ConfirmAction::Exit => true,
                    ConfirmAction::DeleteSheet => {
                        self.delete_current_sheet();
                        false
                    }
                    ConfirmAction::SaveWithoutMacros(path) => {
                        self.save_as_without_macros(path);
                        false
                    }
                    ConfirmAction::TextToColumns(src, opts) => {
                        self.apply_text_to_columns(&src, &opts);
                        false
                    }
                    ConfirmAction::Discard(next) => {
                        self.discard_for(next);
                        false
                    }
                }
            }
        }
    }

    /// Route a key to the Yes/No modal. Returns true if the app should quit.
    fn confirm_key(&mut self, key: KeyEvent) -> bool {
        let Some(c) = self.confirm.as_mut() else {
            return false;
        };
        let outcome = c.key(key);
        self.apply_confirm(outcome)
    }

    /// Route a click to the Yes/No modal. Returns true if the app should quit.
    fn confirm_mouse(&mut self, x: u16, y: u16) -> bool {
        let Some(c) = self.confirm.as_mut() else {
            return false;
        };
        let outcome = c.mouse(x, y);
        self.apply_confirm(outcome)
    }

    /// Route a key to the backstage. Returns true when the app should exit.
    fn backstage_key(&mut self, key: KeyEvent) -> bool {
        let mut bs = self.backstage.take();
        let ev = bs
            .as_mut()
            .map(|b| b.key(key, self))
            .unwrap_or(backstage::BackstageEvent::None);
        self.backstage = bs;
        self.sync_backstage_options();
        self.apply_backstage_event(ev)
    }

    /// Route a left-click inside the File backstage. Row 0 is the ribbon tab
    /// strip (drawn over the backstage) and is handled here directly; every
    /// other row is delegated to `backstage::Backstage::mouse`. Returns true
    /// when the app should exit.
    fn bs_mouse(&mut self, x: u16, y: u16) -> bool {
        if y == 0 {
            match self.ribbon.hit(x, 0, false) {
                ribbon::Hit::Tab(i) if !self.ribbon.tab_is_file(i) => self.backstage_tab_click(i),
                _ => self.backstage_tab_click(0),
            }
            return false;
        }
        let mut bs = self.backstage.take();
        let ev = bs
            .as_mut()
            .map(|b| b.mouse(x, y, self))
            .unwrap_or(backstage::BackstageEvent::None);
        self.backstage = bs;
        self.sync_backstage_options();
        self.apply_backstage_event(ev)
    }

    /// Write the workbook to `dir/name` (defaulting to `.xlsx` when the typed
    /// name carries no extension), then make it the current file and close the
    /// backstage.
    fn commit_save_as(&mut self, dir: std::path::PathBuf, name: String) {
        if name.is_empty() {
            self.status = Some("Save As — type a file name first.".to_string());
            return;
        }
        // A type picked in the list, else the type the workbook is bound to
        // while the name still has its extension, else the name decides.
        let chosen = self.backstage.as_ref().and_then(|b| {
            b.chosen_type()
                .or_else(|| b.preset_type().filter(|&t| has_type_ext(&name, t)))
        });
        let fname = match chosen {
            Some(t) if has_type_ext(&name, t) => name,
            Some(t) => format!("{name}.{}", SAVE_TYPES[t].ext),
            None if name.contains('.') => name,
            None => format!("{name}.xlsx"),
        };
        let path = dir.join(&fname).to_string_lossy().into_owned();
        self.backstage = None;
        match chosen {
            Some(t) => self.save_as_type(path, t),
            None => self.request_save_as(path),
        }
    }

    // --- welcome / start screen ----------------------------------------------

    /// Route a key on the welcome screen. Returns true to exit.
    fn start_screen_key(&mut self, key: KeyEvent) -> bool {
        // Ctrl+N is Blank workbook, as on Excel's start screen.
        if matches!(key.code, KeyCode::Char('n' | 'N'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return self.start_choose(0);
        }
        match self.start.key(key) {
            backstage::StartEvent::Choose(i) => self.start_choose(i),
            // The welcome screen has no open workbook to lose, so Quit exits
            // directly (matching docxy) — the in-app exit is what confirms.
            backstage::StartEvent::Quit => true,
            backstage::StartEvent::None => false,
        }
    }

    /// Act on a chosen welcome-screen item. Returns true to quit.
    fn start_choose(&mut self, idx: usize) -> bool {
        self.start_screen = false;
        match idx {
            0 => self.new_workbook(),
            1 => {
                // Open → jump into the backstage browser.
                self.open_backstage();
                if let Some(bs) = &mut self.backstage {
                    bs.pane = backstage::Pane::Browser;
                }
            }
            _ => return true, // Quit — nothing open to lose, exit directly
        }
        if let Some(note) = self.startup_note.take() {
            self.status = Some(match self.status.take() {
                Some(status) => format!("{note}; {status}"),
                None => note,
            });
        }
        false
    }

    fn clear_selection(&mut self) {
        if self.protected() {
            self.status =
                Some("Sheet is protected — unprotect it to edit (Review ▸ Protect)".into());
            return;
        }
        let (r1, c1, r2, c2) = self.iter_selection();
        let mut changes = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                if let Some(cell) = self.sheet().cell(r, c) {
                    changes.push((
                        r,
                        c,
                        Cell {
                            style: cell.style,
                            ..Cell::default()
                        },
                    ));
                }
            }
        }
        self.apply(changes);
    }

    fn switch_sheet(&mut self, delta: i64) {
        let n = self.pkg.workbook.sheets.len() as i64;
        let cur = self.sheet as i64;
        self.goto_sheet(((cur + delta).rem_euclid(n)) as usize);
    }

    /// Jump to sheet `idx`, resetting the viewport.
    fn goto_sheet(&mut self, idx: usize) {
        if idx >= self.pkg.workbook.sheets.len() {
            return;
        }
        self.sheet = idx;
        self.cur = (0, 0);
        self.top = 0;
        self.left = 0;
        self.anchor = None;
    }

    // --- sheet picker --------------------------------------------------------

    fn open_sheet_picker(&mut self) {
        if self.pkg.workbook.sheets.len() > 1 {
            self.sheet_picker = Some(self.sheet);
        } else {
            self.status = Some("Only one sheet — Ctrl-T adds another".to_string());
        }
    }

    fn sheet_picker_key(&mut self, code: KeyCode) {
        let Some(sel) = self.sheet_picker else { return };
        let n = self.pkg.workbook.sheets.len();
        match code {
            KeyCode::Esc => self.sheet_picker = None,
            KeyCode::Up => self.sheet_picker = Some(sel.saturating_sub(1)),
            KeyCode::Down => self.sheet_picker = Some((sel + 1).min(n - 1)),
            KeyCode::Home => self.sheet_picker = Some(0),
            KeyCode::End => self.sheet_picker = Some(n - 1),
            KeyCode::Enter => {
                self.sheet_picker = None;
                self.goto_sheet(sel);
            }
            _ => {}
        }
    }

    // --- editor sprint operations -------------------------------------------

    /// Insert `count` rows above the selection (or delete the selected rows).
    fn row_op(&mut self, insert: bool) {
        let (r1, _, r2, _) = self.selection();
        let count = r2 - r1 + 1;
        let sheet = self.sheet;
        self.structural(|wb| {
            if insert {
                gridcore::edit::insert_rows(wb, sheet, r1, count);
            } else {
                gridcore::edit::delete_rows(wb, sheet, r1, count);
            }
        });
        self.status = Some(format!(
            "{} {count} row{}",
            if insert { "Inserted" } else { "Deleted" },
            if count == 1 { "" } else { "s" }
        ));
    }

    fn col_op(&mut self, insert: bool) {
        let (_, c1, _, c2) = self.selection();
        let count = c2 - c1 + 1;
        let sheet = self.sheet;
        self.structural(|wb| {
            if insert {
                gridcore::edit::insert_cols(wb, sheet, c1, count);
            } else {
                gridcore::edit::delete_cols(wb, sheet, c1, count);
            }
        });
        self.status = Some(format!(
            "{} {count} column{}",
            if insert { "Inserted" } else { "Deleted" },
            if count == 1 { "" } else { "s" }
        ));
    }

    /// Sort the contiguous region around the cursor by the cursor's column
    /// (header-aware; rows move as whole units; blanks last). Value-table sort —
    /// like the suite; formula refs in moved rows are not re-based.
    /// The contiguous region around the cursor to sort, as `(start, bottom)`
    /// data-row bounds (header excluded). A header is inferred when the top row
    /// has a text label over numeric data in *any* column. `None` when there's
    /// nothing to sort.
    fn sort_bounds(&self) -> Option<(u32, u32)> {
        use gridcore::sheet::CellValue;
        let (rc, cc) = self.sheet().used_size();
        if rc == 0 || cc == 0 {
            return None;
        }
        let (max_r, max_c) = (rc - 1, cc - 1);
        let cur_r = self.cur.0;
        let sh = self.sheet();
        let used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
        if !used(cur_r) {
            return None;
        }
        let mut top = cur_r;
        while top > 0 && used(top - 1) {
            top -= 1;
        }
        let mut bottom = cur_r;
        while bottom < max_r && used(bottom + 1) {
            bottom += 1;
        }
        let header = (0..=max_c).any(|c| {
            matches!(
                sh.cell(top, c).map(|cl| &cl.value),
                Some(CellValue::Text(_))
            ) && (top + 1..=bottom).any(|r| {
                matches!(
                    sh.cell(r, c).map(|cl| &cl.value),
                    Some(CellValue::Number(_))
                )
            })
        });
        let start = if header { top + 1 } else { top };
        (bottom > start).then_some((start, bottom))
    }

    fn sort_region(&mut self, ascending: bool) {
        let sc = self.cur.1;
        let Some((start, bottom)) = self.sort_bounds() else {
            return;
        };
        let s = self.sheet;
        if gridcore::edit::sort_cuts_spill(&self.pkg.workbook, s, start, bottom) {
            self.status = Some(gridcore::edit::SORT_CUTS_SPILL.into());
            return;
        }
        self.structural(move |wb| {
            gridcore::edit::sort_rows(wb, s, start, bottom, &[(sc, ascending)]);
        });
        self.status = Some(format!(
            "Sorted {}",
            if ascending { "A->Z" } else { "Z->A" }
        ));
    }

    /// Multi-level sort from a typed spec like "B asc, C desc" (column letters,
    /// optional asc/desc, default ascending). The first key is primary.
    fn commit_sort(&mut self, text: &str) {
        let Some(keys) = gridcore::edit::parse_sort_spec(text) else {
            self.status = Some("Sort: enter columns, e.g. \"B asc, C desc\"".into());
            return;
        };
        let Some((start, bottom)) = self.sort_bounds() else {
            self.status = Some("Sort: put the cursor in the data".into());
            return;
        };
        let s = self.sheet;
        if gridcore::edit::sort_cuts_spill(&self.pkg.workbook, s, start, bottom) {
            self.status = Some(gridcore::edit::SORT_CUTS_SPILL.into());
            return;
        }
        let keys2 = keys.clone();
        self.structural(move |wb| {
            gridcore::edit::sort_rows(wb, s, start, bottom, &keys2);
        });
        self.status = Some(format!(
            "Sorted by {} key{}",
            keys.len(),
            if keys.len() == 1 { "" } else { "s" }
        ));
    }

    /// AutoSum: put =SUM(range) in the current cell, summing the run of numbers
    /// directly above (else to the left).
    fn autosum(&mut self) {
        use gridcore::sheet::{Cell, CellValue, cell_name};
        let s = self.sheet;
        let (r, c) = self.cur;
        let range = {
            let sh = self.sheet();
            let is_num = |rr: u32, cc: u32| {
                matches!(
                    sh.cell(rr, cc).map(|x| &x.value),
                    Some(CellValue::Number(_))
                )
            };
            if r > 0 && is_num(r - 1, c) {
                let mut top = r - 1;
                while top > 0 && is_num(top - 1, c) {
                    top -= 1;
                }
                Some(format!("{}:{}", cell_name(top, c), cell_name(r - 1, c)))
            } else if c > 0 && is_num(r, c - 1) {
                let mut left = c - 1;
                while left > 0 && is_num(r, left - 1) {
                    left -= 1;
                }
                Some(format!("{}:{}", cell_name(r, left), cell_name(r, c - 1)))
            } else {
                None
            }
        };
        let Some(range) = range else {
            self.status = Some("AutoSum: no adjacent numbers".into());
            return;
        };
        let style = self.sheet().cell(r, c).map(|x| x.style).unwrap_or(0);
        self.status = Some(format!("AutoSum: =SUM({range})"));
        self.structural_writing_cells(move |wb| {
            let cell = Cell {
                style,
                ..Cell::formula(&format!("SUM({range})"))
            };
            wb.sheets[s].set_cell(r, c, cell);
        });
    }

    /// Data › Text to Columns: the Convert Text to Columns Wizard over the
    /// selected column. More than one column is refused, as Excel does.
    fn open_text_to_columns(&mut self) {
        let src = match gridcore::edit::TtcSource::new(self.sheet, self.selection()) {
            Ok(src) => src,
            Err(msg) => {
                self.status = Some(msg.to_string());
                return;
            }
        };
        let wb = &self.pkg.workbook;
        let sheet = &wb.sheets[self.sheet];
        let last = sheet.used_size().0.saturating_sub(1).min(src.r2);
        let sample: Vec<String> = (src.r1..=last)
            .filter_map(|r| sheet.cell(r, src.col))
            .map(|c| format_with(&wb.styles.xf(c.style), &c.value, wb.date1904))
            .filter(|t| !t.is_empty())
            .take(8)
            .collect();
        let dest = cell_name(src.dest.0, src.dest.1);
        self.text_dialog = Some(textdlg::TextDialog::columns(src, sample, dest));
    }

    /// A key for the wizard.
    fn text_dialog_key(&mut self, code: KeyCode) {
        let Some(d) = self.text_dialog.as_mut() else {
            return;
        };
        match d.key(code) {
            textdlg::Outcome::Pending => {}
            textdlg::Outcome::Cancel => {
                self.text_dialog = None;
                self.startup_import = false;
                self.status = Some("Cancelled".to_string());
            }
            textdlg::Outcome::Finish => self.finish_text_dialog(),
        }
    }

    /// Finish: import the text file, or convert the column (asking first
    /// when that overwrites data).
    fn finish_text_dialog(&mut self) {
        let Some(d) = self.text_dialog.take() else {
            return;
        };
        let opts = d.parse();
        if opts.decimal == opts.thousands {
            self.status = Some("The decimal and thousands separators must differ".to_string());
            self.text_dialog = Some(d);
            return;
        }
        match &d.purpose {
            textdlg::Purpose::Import { path, .. } => {
                let pkg = text_to_pkg(d.text(), &file_stem(path), &opts, &self.text_open());
                // A finished interactive import ends read-only (#882); the
                // startup import of the `-r` file keeps it refused.
                if !std::mem::take(&mut self.startup_import) {
                    self.read_only = None;
                }
                self.install_workbook(pkg, import_binding(path), Some(path.clone()));
                self.import_unsaved = true;
            }
            textdlg::Purpose::Columns { src, .. } => {
                let Some(dest) = parse_a1(&d.dest) else {
                    self.status = Some("The destination must be a cell, such as B1".to_string());
                    self.text_dialog = Some(d);
                    return;
                };
                let src = gridcore::edit::TtcSource { dest, ..*src };
                self.request_text_to_columns(src, opts);
            }
        }
    }

    /// Convert, asking Excel's question first when a destination cell other
    /// than the source column holds data.
    fn request_text_to_columns(&mut self, src: gridcore::edit::TtcSource, opts: TextParse) {
        if gridcore::edit::ttc_would_overwrite(&self.pkg.workbook, &src, &opts) {
            self.confirm = Some(backstage::Confirm::new(
                gridcore::edit::TTC_REPLACE,
                ConfirmAction::TextToColumns(src, opts),
                Color::Green,
            ));
        } else {
            self.apply_text_to_columns(&src, &opts);
        }
    }

    /// Text to Columns, as one undoable edit. Returns the rows converted.
    fn apply_text_to_columns(
        &mut self,
        src: &gridcore::edit::TtcSource,
        opts: &TextParse,
    ) -> usize {
        let today = now_serial();
        let mut n = 0;
        self.structural_writing_cells(|wb| {
            n = gridcore::edit::text_to_columns(wb, src, opts, today);
        });
        self.status = Some(format!(
            "Text to Columns: converted {n} row{}",
            if n == 1 { "" } else { "s" }
        ));
        n
    }

    /// Remove duplicate rows in the contiguous region around the cursor
    /// (header-aware), keeping the first occurrence.
    fn remove_duplicates(&mut self) {
        use gridcore::sheet::CellValue;
        let s = self.sheet;
        let sc = self.cur.1;
        let cur_r = self.cur.0;
        let (rc, cc) = self.sheet().used_size();
        if rc == 0 || cc == 0 {
            return;
        }
        let (max_r, max_c) = (rc - 1, cc - 1);
        let (top, bottom, header) = {
            let sh = self.sheet();
            let used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
            if !used(cur_r) {
                return;
            }
            let mut top = cur_r;
            while top > 0 && used(top - 1) {
                top -= 1;
            }
            let mut bottom = cur_r;
            while bottom < max_r && used(bottom + 1) {
                bottom += 1;
            }
            let header = matches!(sh.cell(top, sc).map(|c| &c.value), Some(CellValue::Text(_)));
            (top, bottom, header)
        };
        let mut removed = 0;
        self.structural(|wb| {
            removed = gridcore::edit::dedupe_rows(wb, s, top, bottom, header);
        });
        self.status = Some(format!(
            "Removed {removed} duplicate row{}",
            if removed == 1 { "" } else { "s" }
        ));
    }

    // --- Data ▸ Outline -----------------------------------------------------

    /// A structural edit that may refuse. On `Err` the workbook is put back
    /// and the message shown; on `Ok` its message is shown, and an undo step
    /// is recorded only when a sheet actually changed, so a refusal or a
    /// no-op leaves the undo stack alone. A protected sheet refuses, as in
    /// Excel. `Err(())` when refused, else whether anything changed.
    fn try_outline_edit(
        &mut self,
        op: impl FnOnce(&mut gridcore::sheet::Workbook) -> Result<String, String>,
    ) -> Result<bool, ()> {
        self.try_outline_edit_on(self.sheet, op)
    }

    /// [`App::try_outline_edit`] for an edit of `sheet`, which need not be
    /// the active one: that sheet's protection is what refuses it.
    fn try_outline_edit_on(
        &mut self,
        sheet: usize,
        op: impl FnOnce(&mut gridcore::sheet::Workbook) -> Result<String, String>,
    ) -> Result<bool, ()> {
        if self
            .pkg
            .workbook
            .sheets
            .get(sheet)
            .is_some_and(|s| s.is_protected())
        {
            self.status =
                Some("Sheet is protected — unprotect it to edit (Review ▸ Protect)".into());
            return Err(());
        }
        let before = self.wb_snapshot();
        match op(&mut self.pkg.workbook) {
            Err(msg) => {
                self.put_back(&before);
                self.status = Some(msg);
                Err(())
            }
            Ok(msg) => {
                self.status = Some(msg);
                if !gridcore::edit::sheets_differ(&before.sheets, &self.pkg.workbook.sheets) {
                    return Ok(false);
                }
                self.rebuild_engine();
                let after = self.wb_snapshot();
                self.undo.push(UndoAction::Structural { before, after });
                self.redo.clear();
                self.modified = true;
                self.cancel_cut();
                self.ensure_visible();
                Ok(true)
            }
        }
    }

    /// The rows or columns the selection covers whole, for Group and
    /// Ungroup; `None` when it is neither, which asks Rows or Columns.
    fn outline_target(&self) -> Option<(Axis, u32, u32)> {
        let (r1, c1, r2, c2) = self.selection();
        if c1 == 0 && c2 == MAX_COLS - 1 {
            Some((Axis::Rows, r1, r2))
        } else if r1 == 0 && r2 == MAX_ROWS - 1 {
            Some((Axis::Cols, c1, c2))
        } else {
            None
        }
    }

    /// Group (Alt+Shift+Right) or Ungroup (Alt+Shift+Left) the selection.
    fn group_outline(&mut self, ungroup: bool) {
        match self.outline_target() {
            Some((axis, a, b)) => self.apply_group(axis, a, b, ungroup),
            None => {
                self.outline_dialog = Some(outlinedlg::Dialog::Axis(outlinedlg::AxisDialog::new(
                    ungroup,
                )));
            }
        }
    }

    /// Group or ungroup rows (columns) `a..=b`.
    fn apply_group(&mut self, axis: Axis, a: u32, b: u32, ungroup: bool) {
        let s = self.sheet;
        let n = b - a + 1;
        let what = match axis {
            Axis::Rows => "row",
            Axis::Cols => "column",
        };
        let _ = self.try_outline_edit(|wb| {
            let sh = &mut wb.sheets[s];
            if ungroup {
                outline::ungroup(sh, axis, a, b)
            } else {
                outline::group(sh, axis, a, b)
            }
            .map_err(|e| e.to_string())?;
            Ok(format!(
                "{} {n} {what}{}",
                if ungroup { "Ungrouped" } else { "Grouped" },
                if n == 1 { "" } else { "s" }
            ))
        });
    }

    /// Show Detail / Hide Detail at the cursor: its row's group, else its
    /// column's (only the column's when whole columns are selected).
    fn outline_detail(&mut self, show: bool) {
        let (r1, _, r2, _) = self.selection();
        let whole_cols = r1 == 0 && r2 == MAX_ROWS - 1;
        let (r, c) = self.cur;
        let s = self.sheet;
        let act: fn(&mut gridcore::sheet::Sheet, Axis, u32) -> Result<(), OutlineError> = if show {
            outline::show_detail
        } else {
            outline::hide_detail
        };
        let _ = self.try_outline_edit(|wb| {
            let sh = &mut wb.sheets[s];
            let tries: &[(Axis, u32)] = if whole_cols {
                &[(Axis::Cols, c)]
            } else {
                &[(Axis::Rows, r), (Axis::Cols, c)]
            };
            for &(axis, i) in tries {
                if act(sh, axis, i).is_ok() {
                    return Ok(if show {
                        "Detail shown"
                    } else {
                        "Detail hidden"
                    }
                    .into());
                }
            }
            Err(OutlineError::NoGroup.to_string())
        });
    }

    /// Auto Outline over the selection when it is more than one cell, else
    /// the whole sheet.
    fn auto_outline(&mut self) {
        let (r1, c1, r2, c2) = self.selection();
        let area = ((r1, c1) != (r2, c2)).then(|| self.iter_selection());
        let s = self.sheet;
        let _ = self.try_outline_edit(|wb| {
            outline::auto_outline(&mut wb.sheets[s], area).map_err(|e| e.to_string())?;
            Ok("Outline created from the summary formulas".into())
        });
    }

    fn clear_outline(&mut self) {
        let s = self.sheet;
        let _ = self.try_outline_edit(|wb| {
            outline::clear_outline(&mut wb.sheets[s]).map_err(|e| e.to_string())?;
            Ok("Outline cleared".into())
        });
    }

    /// A level button: show levels below `n`.
    fn outline_show_level(&mut self, axis: Axis, n: u8) {
        let s = self.sheet;
        let _ = self.try_outline_edit(|wb| {
            outline::show_level(&mut wb.sheets[s], axis, n);
            Ok(format!("Showing outline level {n}"))
        });
    }

    /// A +/- button: collapse or expand that group.
    fn outline_toggle(&mut self, axis: Axis, g: outline::Group) {
        let s = self.sheet;
        let _ = self.try_outline_edit(|wb| {
            outline::toggle_group(&mut wb.sheets[s], axis, &g);
            Ok(if g.collapsed { "Expanded" } else { "Collapsed" }.into())
        });
    }

    /// Settings…: the outline direction dialog.
    fn open_outline_settings(&mut self) {
        self.outline_dialog = Some(outlinedlg::Dialog::Settings(
            outlinedlg::SettingsDialog::new(self.sheet, self.sheet().outline),
        ));
    }

    /// Subtotal…: the dialog over the region around the cursor, grouping at
    /// the cursor's column, with its numeric columns checked.
    fn subtotal(&mut self) {
        let (r, c) = self.cur;
        let sh = self.sheet();
        let Some((area, header)) = gridcore::edit::subtotal_region(sh, r, c) else {
            self.status = Some("Subtotal: put the cursor in the data".into());
            return;
        };
        let (top, c1, bottom, c2) = area;
        let cols = gridcore::edit::subtotal_columns(sh, area, header);
        let data = (top + u32::from(header), c1, bottom, c2);
        let checked = gridcore::edit::numeric_columns(sh, data, c);
        let defaults = gridcore::edit::SubtotalOptions::new(c, checked, header);
        self.outline_dialog = Some(outlinedlg::Dialog::Subtotal(
            outlinedlg::SubtotalDialog::new(self.sheet, area, cols, &defaults),
        ));
    }

    /// Consolidate…: the dialog writing at the cursor, starting from the
    /// settings this sheet kept.
    fn open_consolidate(&mut self) {
        let kept = self.sheet().consolidate.as_ref();
        let names = self
            .pkg
            .workbook
            .sheets
            .iter()
            .map(|s| s.name.clone())
            .collect();
        self.outline_dialog = Some(outlinedlg::Dialog::Consolidate(
            outlinedlg::ConsolidateDialog::new(self.sheet, self.cur, kept, names),
        ));
    }

    /// Consolidate into `sheet` at `at` as one undo step. The detail rows of
    /// a linked consolidation carry the workbook's name: the file's stem.
    /// A refusal changes nothing and says why.
    pub(crate) fn apply_consolidate(
        &mut self,
        sheet: usize,
        at: (u32, u32),
        mut opts: gridcore::edit::ConsolidateOptions,
    ) -> Result<gridcore::edit::Area, String> {
        opts.book_name = file_stem(&self.path);
        let mut out = None;
        let done = self.try_outline_edit_on(sheet, |wb| {
            let area =
                gridcore::edit::consolidate(wb, sheet, at, &opts).map_err(|e| e.to_string())?;
            out = Some(area);
            let (r1, c1, r2, c2) = area;
            Ok(format!(
                "Consolidated into {}:{}",
                cell_name(r1, c1),
                cell_name(r2, c2)
            ))
        });
        match (done, out) {
            (Ok(_), Some(area)) => Ok(area),
            _ => Err(self.status.clone().unwrap_or_default()),
        }
    }

    /// Home › Paste Special (Ctrl+Alt+V): the dialog, over xlsxy's own copy
    /// (a paste special needs its formulas and formats; text from another
    /// program has neither).
    fn open_paste_special(&mut self) {
        if self.protected() {
            self.status =
                Some("Sheet is protected — unprotect it to edit (Review ▸ Protect)".into());
            return;
        }
        match &self.clip {
            None => {
                self.status = Some("Paste Special pastes a copy made here: copy first".into());
                return;
            }
            // Excel offers only Paste for a cut (#707 r1).
            Some(clip) if clip.cut => {
                self.status = Some("A cut pastes with Paste only (Ctrl+V)".into());
                return;
            }
            _ => {}
        }
        self.outline_dialog = Some(outlinedlg::Dialog::PasteSpecial(
            outlinedlg::PasteSpecialDialog::default(),
        ));
    }

    /// Paste Special `spec` of the copy at the cursor, through gridcore's
    /// [`gridcore::edit::paste_special_changes`], as one undo step.
    fn paste_special(&mut self, spec: gridcore::edit::PasteSpec) {
        let Some(clip) = self.clip.clone() else {
            return;
        };
        if clip.sheet >= self.pkg.workbook.sheets.len() {
            self.status = Some("The copy's sheet is gone: copy again".into());
            return;
        }
        let rows: Vec<u32> = (0..clip.cells.len() as u32)
            .map(|i| clip.from.0 + i)
            .collect();
        let w = clip.cells.iter().map(Vec::len).max().unwrap_or(0) as u32;
        let cols: Vec<u32> = (0..w).map(|j| clip.from.1 + j).collect();
        let mut block =
            gridcore::edit::ClipBlock::capture(&self.pkg.workbook, clip.sheet, rows, cols);
        // The cells as they were copied.
        block.cells = clip
            .cells
            .iter()
            .map(|row| {
                let mut row: Vec<Cell> =
                    row.iter().map(|c| c.clone().unwrap_or_default()).collect();
                row.resize(w as usize, Cell::default());
                row
            })
            .collect();
        let s = self.sheet;
        let changes = gridcore::edit::paste_special_changes(
            &mut self.pkg.workbook,
            s,
            self.cur,
            &block,
            &spec,
        );
        let n = changes.len();
        if self.apply(changes) {
            self.status = Some(format!("Pasted {} into {n} cell(s)", spec.what.label()));
        }
    }

    /// A key for the open outline dialog.
    fn outline_dialog_key(&mut self, code: KeyCode) {
        let Some(d) = self.outline_dialog.as_mut() else {
            return;
        };
        let outcome = d.key(code);
        match outcome {
            outlinedlg::Outcome::Pending => {}
            outlinedlg::Outcome::Cancel => self.outline_dialog = None,
            outlinedlg::Outcome::PasteSpecial(spec) => {
                self.outline_dialog = None;
                self.paste_special(spec);
            }
            outlinedlg::Outcome::Subtotal(opts) => {
                let Some(outlinedlg::Dialog::Subtotal(d)) = self.outline_dialog.clone() else {
                    return;
                };
                let done = self.try_outline_edit(|wb| {
                    let n = gridcore::edit::subtotal(wb, d.sheet, d.area, &opts)
                        .map_err(|e| e.to_string())?;
                    Ok(format!(
                        "Inserted {n} subtotal row{}",
                        if n == 1 { "" } else { "s" }
                    ))
                });
                // A refusal (no column chosen) leaves the dialog open.
                if done.is_ok() {
                    self.outline_dialog = None;
                }
            }
            outlinedlg::Outcome::Consolidate(opts) => {
                let Some(outlinedlg::Dialog::Consolidate(d)) = self.outline_dialog.clone() else {
                    return;
                };
                // A refusal leaves the dialog open, with the reason showing.
                if self.apply_consolidate(d.sheet, d.at, opts).is_ok() {
                    self.outline_dialog = None;
                }
            }
            outlinedlg::Outcome::RemoveAll => {
                let Some(outlinedlg::Dialog::Subtotal(d)) = self.outline_dialog.take() else {
                    return;
                };
                let _ = self.try_outline_edit(|wb| {
                    let n = gridcore::edit::remove_subtotals(wb, d.sheet, d.area);
                    Ok(format!(
                        "Removed {n} subtotal row{}",
                        if n == 1 { "" } else { "s" }
                    ))
                });
            }
            outlinedlg::Outcome::Settings(o) => {
                let Some(outlinedlg::Dialog::Settings(d)) = self.outline_dialog.take() else {
                    return;
                };
                let _ = self.try_outline_edit(|wb| {
                    wb.sheets[d.sheet].outline = o;
                    Ok("Outline settings changed".into())
                });
            }
            outlinedlg::Outcome::Axis(axis) => {
                let Some(outlinedlg::Dialog::Axis(d)) = self.outline_dialog.take() else {
                    return;
                };
                let (r1, c1, r2, c2) = self.selection();
                let (a, b) = match axis {
                    Axis::Rows => (r1, r2),
                    Axis::Cols => (c1, c2),
                };
                self.apply_group(axis, a, b, d.ungroup);
            }
        }
    }

    /// A click on an outline control: a level button (row levels left of the
    /// column header, column levels left of the column outline line), or a
    /// +/- button (in the row outline gutter, or on the column outline line).
    /// `true` when the click landed on the outline area.
    fn outline_click(&mut self, x: u16, y: u16) -> bool {
        let g = self.grid_area;
        let sh = self.sheet();
        let (row_max, col_max) = (sh.max_row_outline(), sh.max_col_outline());
        if x < g.x {
            return false;
        }
        let dx = x - g.x;
        if self.col_outline_y == Some(y) {
            if dx < self.gutter_w {
                let n = dx as u8 + 1;
                if n <= col_max + 1 {
                    self.outline_show_level(Axis::Cols, n);
                }
                return true;
            }
            let col = self
                .vis_cols
                .iter()
                .find(|&&(_, cx, w)| x >= cx && x < cx + w)
                .map(|&(c, _, _)| c);
            let hit = col.and_then(|c| {
                outline::groups(sh, Axis::Cols)
                    .into_iter()
                    .filter(|gr| gr.summary == Some(c))
                    .max_by_key(|gr| gr.level)
            });
            if let Some(gr) = hit {
                self.outline_toggle(Axis::Cols, gr);
            }
            return true;
        }
        if dx >= self.outline_w {
            return false;
        }
        if y == self.col_hdr_y {
            let n = dx as u8 + 1;
            if n <= row_max + 1 {
                self.outline_show_level(Axis::Rows, n);
            }
            return true;
        }
        if y < g.y || y >= g.y + g.height {
            return false;
        }
        let Some(&row) = self.vis_rows.get((y - g.y) as usize) else {
            return true;
        };
        let level = dx as u8 + 1;
        let hit = outline::groups(sh, Axis::Rows)
            .into_iter()
            .find(|gr| gr.level == level && gr.summary == Some(row));
        if let Some(gr) = hit {
            self.outline_toggle(Axis::Rows, gr);
        }
        true
    }

    /// Format as Table: wrap the contiguous region around the cursor (or the
    /// active multi-cell selection) in an Excel Table — banded, filterable, and
    /// styled by Excel on open. The first row is treated as headers when it is
    /// all text.
    fn format_as_table(&mut self) {
        use gridcore::sheet::CellValue;
        let s = self.sheet;
        // Prefer an explicit multi-cell selection; else grow the region around the cursor.
        let (r1, c1, r2, c2) = {
            let (sr1, sc1, sr2, sc2) = self.selection();
            if sr1 != sr2 || sc1 != sc2 {
                (sr1, sc1, sr2, sc2)
            } else {
                let (rc, cc) = self.sheet().used_size();
                if rc == 0 || cc == 0 {
                    self.status = Some("Format as Table: the sheet is empty".into());
                    return;
                }
                let (max_r, max_c) = (rc - 1, cc - 1);
                let cur_r = self.cur.0;
                let sh = self.sheet();
                let row_used =
                    |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
                if !row_used(cur_r) {
                    self.status = Some("Format as Table: put the cursor in the data".into());
                    return;
                }
                let mut top = cur_r;
                while top > 0 && row_used(top - 1) {
                    top -= 1;
                }
                let mut bottom = cur_r;
                while bottom < max_r && row_used(bottom + 1) {
                    bottom += 1;
                }
                // Widen to the used columns spanning that block.
                let col_used =
                    |c: u32| (top..=bottom).any(|r| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
                let mut left = self.cur.1;
                while left > 0 && col_used(left - 1) {
                    left -= 1;
                }
                let mut right = self.cur.1;
                while right < max_c && col_used(right + 1) {
                    right += 1;
                }
                (top, left, bottom, right)
            }
        };
        let has_header = (c1..=c2).all(|c| {
            matches!(
                self.sheet().cell(r1, c).map(|cl| &cl.value),
                Some(CellValue::Text(_))
            )
        });
        match self
            .pkg
            .add_table(s, (r1, c1, r2, c2), has_header, "TableStyleMedium2")
        {
            Ok(i) => {
                // add_table rewrites package parts; existing undo snapshots no longer line up.
                self.undo.clear();
                self.redo.clear();
                self.modified = true;
                let name = self.pkg.workbook.tables[i].name.clone();
                self.status = Some(format!(
                    "Created {name} ({} cols){}",
                    c2 - c1 + 1,
                    if has_header {
                        ""
                    } else {
                        ", generated headers"
                    }
                ));
            }
            Err(why) => self.status = Some(format!("Format as Table: {why}")),
        }
    }

    /// The table under the cursor, by name.
    fn table_here(&self) -> Option<String> {
        self.pkg
            .workbook
            .table_at(self.sheet, self.cur.0, self.cur.1)
            .map(|t| t.name.clone())
    }

    /// Run a table command on the table under the cursor, or say there is
    /// none.
    fn with_table_here(&mut self, f: impl FnOnce(&mut App, String)) {
        match self.table_here() {
            Some(name) => f(self, name),
            None => self.status = Some("Select a cell in a table".into()),
        }
    }

    /// Table Name: rename table `old`, every formula that uses it, and the
    /// data model's relationships and measures that name it. One undo step.
    fn rename_table(&mut self, old: &str, new: &str) -> Result<(), String> {
        let cur = self
            .pkg
            .workbook
            .table(old)
            .map(|t| t.name.clone())
            .ok_or_else(|| format!("There is no table named {old}"))?;
        self.try_structural(Some((&cur, new)), |wb| {
            gridcore::edit::rename_table(wb, &cur, new)
        })
    }

    /// Resize Table: move table `name` onto `range` ("A1:D20"). One undo step.
    fn resize_table(&mut self, name: &str, range: &str) -> Result<(), String> {
        let rect = gridcore::sheet::parse_range_name(&range.replace('$', ""))
            .ok_or_else(|| format!("\"{range}\" isn't a range"))?;
        self.try_structural(None, |wb| gridcore::edit::resize_table(wb, name, rect))
    }

    /// Convert to Range: table `name` becomes plain cells. Refused while the
    /// data model names it (a relationship or a measure), as gridcore refuses
    /// it while a PivotTable does. One undo step; the table part leaves the
    /// file at the next save.
    fn convert_table(&mut self, name: &str) -> Result<(), String> {
        let named = |t: &str| t.eq_ignore_ascii_case(name);
        let by_rel = self
            .model_rels
            .iter()
            .any(|r| named(&r.from.0) || named(&r.to.0));
        let by_measure = self.model_measures.iter().any(|m| {
            gridcore::formula::parse(&m.formula).is_ok_and(|ast| {
                let (mut refs, mut names) = (Vec::new(), Vec::new());
                gridcore::formula::collect_structured(&ast, &mut refs);
                gridcore::formula::collect_names(&ast, &mut names);
                refs.iter().any(|r| r.0.as_deref().is_some_and(named))
                    || names.iter().any(|n| named(n))
            })
        });
        if by_rel || by_measure {
            return Err(format!("The data model uses {name}"));
        }
        self.try_structural(None, |wb| gridcore::edit::convert_table_to_range(wb, name))
    }

    /// The ribbon's Table Name…: prompt for a new name.
    fn table_name_act(&mut self) {
        self.with_table_here(|app, _| app.open_prompt(PromptKind::RenameTable));
    }

    /// The ribbon's Resize Table…: prompt for the new range.
    fn resize_table_act(&mut self) {
        self.with_table_here(|app, _| app.open_prompt(PromptKind::ResizeTable));
    }

    /// The ribbon's Convert to Range.
    fn convert_table_act(&mut self) {
        self.with_table_here(|app, name| {
            app.status = Some(match app.convert_table(&name) {
                Ok(()) => format!("Converted {name} to a range"),
                Err(why) => why,
            });
        });
    }

    /// AutoFilter: hide the rows of the current region whose cursor-column value
    /// fails the typed criteria (header row kept). "clear" unhides them all.
    fn commit_filter(&mut self, text: &str) {
        use gridcore::sheet::CellValue;
        let s = self.sheet;
        let sc = self.cur.1;
        let cur_r = self.cur.0;
        let (rc, cc) = self.sheet().used_size();
        if rc == 0 || cc == 0 {
            return;
        }
        let (max_r, max_c) = (rc - 1, cc - 1);
        // Contiguous region around the cursor.
        let (top, bottom, header) = {
            let sh = self.sheet();
            let used = |r: u32| (0..=max_c).any(|c| sh.cell(r, c).is_some_and(|cl| !cl.is_blank()));
            if !used(cur_r) {
                return;
            }
            let mut top = cur_r;
            while top > 0 && used(top - 1) {
                top -= 1;
            }
            let mut bottom = cur_r;
            while bottom < max_r && used(bottom + 1) {
                bottom += 1;
            }
            let header = matches!(sh.cell(top, sc).map(|c| &c.value), Some(CellValue::Text(_)));
            (top, bottom, header)
        };
        if text.trim().eq_ignore_ascii_case("clear") {
            for r in top..=bottom {
                self.pkg.workbook.sheets[s].set_row_filtered(r, false);
            }
            // SUBTOTAL(1..11) counts rows by whether a filter hid them.
            self.engine.recalc_all(&mut self.pkg.workbook);
            self.clamp_cursor();
            self.modified = true;
            self.status = Some("Filter cleared".into());
            return;
        }
        let Some((op, operand)) = gridcore::filter::parse(text) else {
            self.status = Some("Filter: enter a value or comparison".into());
            return;
        };
        let start = if header { top + 1 } else { top };
        let keep: Vec<bool> = (start..=bottom)
            .map(|r| {
                let v = self.sheet().cell(r, sc).map(|c| c.value.clone());
                gridcore::filter::matches(v.as_ref(), op, &operand)
            })
            .collect();
        let mut hidden = 0;
        for (i, r) in (start..=bottom).enumerate() {
            let hide = !keep[i];
            if hide {
                hidden += 1;
            }
            self.pkg.workbook.sheets[s].set_row_filtered(r, hide);
        }
        self.engine.recalc_all(&mut self.pkg.workbook);
        self.clamp_cursor();
        self.modified = true;
        self.status = Some(format!("Filtered by column: {hidden} rows hidden"));
    }

    /// Create a list data-validation (dropdown) over the selection from a
    /// comma-separated list of allowed values.
    fn commit_data_validation(&mut self, text: &str) {
        let items: Vec<&str> = text
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if items.is_empty() {
            self.status = Some("Data validation: enter comma-separated values".into());
            return;
        }
        let f1 = format!("\"{}\"", items.join(","));
        let (r1, c1, r2, c2) = self.selection();
        let s = self.sheet;
        if !self
            .pkg
            .add_data_validation(s, (r1, c1, r2, c2), "list", "", &f1, None)
        {
            self.status = Some(WRITE_REFUSED.into());
            return;
        }
        self.undo.clear();
        self.redo.clear();
        self.modified = true;
        self.status = Some(format!("Dropdown list: {} values", items.len()));
    }

    /// Apply a "Highlight Cells" conditional-format rule to the selection from a
    /// typed comparison (">500", "<=100", "=42"; leading operator parsed, default
    /// greaterThan) using Excel's Light-Red-Fill / Dark-Red-Text preset.
    fn commit_cond_format(&mut self, text: &str) {
        let s = self.sheet;
        // "clear" removes all rules on the sheet.
        if text.trim().eq_ignore_ascii_case("clear") {
            if !self.pkg.clear_conditional_formats(s) {
                self.status = Some(WRITE_REFUSED.into());
                return;
            }
            self.undo.clear();
            self.redo.clear();
            self.rebuild_engine();
            self.modified = true;
            self.status = Some("Cleared conditional formatting".into());
            return;
        }
        let Some((op, val, val2)) = parse_cf_input(text) else {
            self.status = Some("Conditional format: enter a value, e.g. >500".into());
            return;
        };
        let (r1, c1, r2, c2) = self.selection();
        let dxf = gridcore::sheet::Dxf {
            fill: Some((0xFF, 0xC7, 0xCE)),
            color: Some((0x9C, 0x00, 0x06)),
            bold: None,
            italic: None,
        };
        if !self
            .pkg
            .add_conditional_format(s, (r1, c1, r2, c2), op, &val, val2.as_deref(), dxf)
        {
            self.status = Some(WRITE_REFUSED.into());
            return;
        }
        // add_conditional_format rewrites package parts; drop stale undo snapshots.
        self.undo.clear();
        self.redo.clear();
        self.rebuild_engine();
        self.modified = true;
        self.status = Some(format!("Conditional format: value {op} {val}"));
    }

    /// Merge & Center the selection (or unmerge if its top-left is already a merge
    /// origin). Centres the anchor cell; renders via the merge-aware grid draw.
    fn merge_toggle(&mut self) {
        use gridcore::sheet::Align;
        let (r1, c1, r2, c2) = self.selection(); // raw: merging blank cells is valid
        let s = self.sheet;
        self.structural(move |wb| {
            if let Some(i) = wb.sheets[s]
                .merges
                .iter()
                .position(|&(a, b, _, _)| a == r1 && b == c1)
            {
                wb.sheets[s].merges.remove(i);
            } else if r2 > r1 || c2 > c1 {
                wb.sheets[s].merges.push((r1, c1, r2, c2));
                let style = wb.sheets[s].cell(r1, c1).map(|x| x.style).unwrap_or(0);
                let mut xf = wb.styles.xf(style);
                xf.align = Align::Center;
                let idx = wb.styles.intern(xf);
                wb.sheets[s].cells.entry((r1, c1)).or_default().style = idx;
            }
        });
        self.status = Some("Toggled Merge & Center".into());
    }

    /// Insert a chart from the selection: the first non-numeric column is the
    /// category axis, each numeric column a series. Anchored just right of the
    /// selection and wired via SheetPackage::add_chart (renders + persists).
    fn insert_chart(&mut self, kind: &str) {
        let (r1, c1, r2, c2) = self.iter_selection();
        // Same reader the desktop suite uses: header row names the series, the
        // first non-numeric column supplies the labels.
        let data = {
            let sh = self.sheet();
            gridcore::sheet::chart_from_range(sh, &sh.name, (r1, c1, r2, c2), kind, false)
        };
        let Some(data) = data else {
            // `chart_from_range` refuses both a header-only selection and one
            // with nothing numeric under the header; say which.
            self.status = Some(if r2 <= r1 {
                "Insert chart: select the data rows too, not just the header".into()
            } else {
                "Insert chart: no numeric columns in the selection".to_string()
            });
            return;
        };
        let sheet = self.sheet;
        if !self
            .pkg
            .add_chart(sheet, (r1, c2 + 2), (r1 + 16, c2 + 10), &data)
        {
            self.status = Some(WRITE_REFUSED.into());
            return;
        }
        // add_chart rewrites package parts; existing undo snapshots no longer line up.
        self.undo.clear();
        self.redo.clear();
        self.modified = true;
        self.status = Some(format!("Inserted {kind} chart"));
    }

    /// Ctrl-D / Ctrl-R and Fill Up / Left: fill the selection from its first
    /// row/column (last, for Up and Left), translating relative refs — or,
    /// when the selection is one cell deep along the fill, pull each cell
    /// from its neighbour before it.
    fn fill(&mut self, dir: FillDir) {
        let changes = fill_changes(self.sheet(), self.selection(), dir);
        if changes.is_empty() {
            return;
        }
        let n = changes.len();
        if !self.apply(changes) {
            return;
        }
        self.status = Some(format!(
            "Filled {n} cell{} {}",
            if n == 1 { "" } else { "s" },
            dir.label().to_ascii_lowercase()
        ));
    }

    /// Jump to the next cell (row-major, wrapping) whose display text or
    /// formula contains `query`, case-insensitively.
    fn find_next(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        let q = query.to_lowercase();
        let sheet = self.sheet();
        let keys: Vec<(u32, u32)> = sheet.cells.keys().copied().collect();
        if keys.is_empty() {
            self.status = Some(format!("Not found: {query}"));
            return;
        }
        let start = keys.iter().position(|&k| k > self.cur).unwrap_or(0);
        let date1904 = self.pkg.workbook.date1904;
        for i in 0..keys.len() {
            let (r, c) = keys[(start + i) % keys.len()];
            let cell = sheet.cell(r, c).unwrap();
            let shown = format_with(
                &self.pkg.workbook.styles.xf(cell.style),
                &cell.value,
                date1904,
            );
            let hit = shown.to_lowercase().contains(&q)
                || cell
                    .formula
                    .as_deref()
                    .is_some_and(|f| f.to_lowercase().contains(&q));
            if hit {
                self.cur = (r, c);
                self.anchor = None;
                self.ensure_visible();
                self.status = Some(format!("Found at {}", cell_name(r, c)));
                return;
            }
        }
        self.status = Some(format!("Not found: {query}"));
    }

    /// Literally replace `find` with `with` in every cell's input text
    /// (formula or entered value), reparsing each — one undoable edit.
    fn replace_all(&mut self, find: &str, with: &str) {
        if find.is_empty() {
            self.status = Some("Nothing to find".to_string());
            return;
        }
        let wb = &mut self.pkg.workbook;
        let ctx = entry_ctx(wb, now_serial());
        let changes =
            replace_all_in_sheet(&wb.sheets[self.sheet], &mut wb.styles, &ctx, find, with);
        let n = changes.len();
        if n == 0 {
            self.status = Some(format!("Not found: {find}"));
            return;
        }
        if !self.apply(changes) {
            return;
        }
        self.status = Some(format!("Replaced in {n} cell(s)"));
    }

    /// Jump the cursor to a cell reference (`A1`, `Sheet2!B3`) or a defined name.
    fn goto(&mut self, target: &str) {
        let target = target.trim();
        if target.is_empty() {
            return;
        }
        // A defined name → jump to the top-left of its reference.
        let resolved = self
            .pkg
            .workbook
            .defined_names
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(target))
            .map(|d| d.formula.clone());
        let refstr = resolved.as_deref().unwrap_or(target);
        // Optional Sheet! prefix.
        let (sheet_name, cellref) = match refstr.rsplit_once('!') {
            Some((s, r)) => (Some(s.trim_matches(['\'', ' '])), r),
            None => (None, refstr),
        };
        if let Some(name) = sheet_name {
            if let Some(i) = self
                .pkg
                .workbook
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(name))
            {
                self.sheet = i;
            }
        }
        // First cell of the (possibly ranged) reference.
        let first = cellref.split(':').next().unwrap_or(cellref);
        match parse_a1(first) {
            Some((row, col)) => {
                self.cur = (row.min(MAX_ROWS - 1), col.min(MAX_COLS - 1));
                self.anchor = None;
                self.ensure_visible();
                self.status = Some(format!("Went to {}", cell_name(self.cur.0, self.cur.1)));
            }
            None => self.status = Some(format!("Can't go to: {target}")),
        }
    }

    // --- vim mode ------------------------------------------------------------

    fn vim_mode(&self) -> VimMode {
        self.vim.as_ref().map(|v| v.mode).unwrap_or(VimMode::Normal)
    }

    fn vim_exit_visual(&mut self) {
        if let Some(v) = &mut self.vim {
            v.mode = VimMode::Normal;
        }
        self.anchor = None;
    }

    fn vim_toggle_visual(&mut self, target: VimMode) {
        let cur = self.vim_mode();
        if cur == target {
            self.vim_exit_visual();
            return;
        }
        if let Some(v) = &mut self.vim {
            v.mode = target;
        }
        self.anchor = Some(self.cur);
        if target == VimMode::VisualLine {
            self.vim_extend_line();
        }
    }

    /// In VisualLine mode, span the selection across full rows.
    fn vim_extend_line(&mut self) {
        let (_, used_c) = self.sheet().used_size();
        let last = used_c.saturating_sub(1);
        self.anchor = Some((self.anchor.map(|a| a.0).unwrap_or(self.cur.0), 0));
        self.cur.1 = last;
    }

    /// Move to the next/prev non-empty cell in the current row (or by one).
    fn vim_next_used(&mut self, dir: i64) {
        let (r, c) = self.cur;
        let (_, used_c) = self.sheet().used_size();
        let mut nc = c as i64 + dir;
        while nc >= 0 && (nc as u32) < used_c {
            if self.sheet().cell(r, nc as u32).is_some() {
                self.cur = (r, nc as u32);
                self.ensure_visible();
                return;
            }
            nc += dir;
        }
        self.move_cur(0, dir, self.vim_mode() != VimMode::Normal);
    }

    /// Route a key while in vim mode (not editing). Returns true to exit.
    fn vim_key(&mut self, code: KeyCode, ctrl: bool, _shift: bool) -> bool {
        if self.vim.as_ref().and_then(|v| v.cmdline.as_ref()).is_some() {
            return self.vim_cmdline_key(code);
        }
        let pending = self.vim.as_ref().map(|v| v.pending).unwrap_or('\0');
        if let Some(v) = &mut self.vim {
            v.pending = '\0';
        }
        let visual = self.vim_mode() != VimMode::Normal;

        // Multi-key prefixes.
        match pending {
            'g' => {
                match code {
                    KeyCode::Char('g') => {
                        self.cur.0 = 0;
                        self.ensure_visible();
                    }
                    KeyCode::Char('t') => self.switch_sheet(1), // next sheet
                    KeyCode::Char('T') => self.switch_sheet(-1), // previous sheet
                    _ => {}
                }
                return false;
            }
            'd' => {
                if code == KeyCode::Char('d') {
                    self.row_op(false);
                }
                return false;
            }
            'y' => {
                if code == KeyCode::Char('y') {
                    let saved = self.anchor;
                    let (_, used_c) = self.sheet().used_size();
                    self.anchor = Some((self.cur.0, 0));
                    self.cur.1 = used_c.saturating_sub(1);
                    self.copy(false);
                    self.anchor = saved;
                }
                return false;
            }
            _ => {}
        }

        match code {
            KeyCode::Char(':') => {
                if let Some(v) = &mut self.vim {
                    v.cmdline = Some(String::new());
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.move_cur(0, -1, visual),
            KeyCode::Char('l') | KeyCode::Right => self.move_cur(0, 1, visual),
            KeyCode::Char('k') | KeyCode::Up => self.move_cur(-1, 0, visual),
            KeyCode::Char('j') | KeyCode::Down => self.move_cur(1, 0, visual),
            KeyCode::Char('0') => {
                self.cur.1 = 0;
                self.ensure_visible();
            }
            KeyCode::Char('$') => {
                let (_, used_c) = self.sheet().used_size();
                self.cur.1 = used_c.saturating_sub(1);
                self.ensure_visible();
            }
            KeyCode::Char('G') => {
                let (used_r, _) = self.sheet().used_size();
                self.cur.0 = used_r.saturating_sub(1);
                self.ensure_visible();
            }
            KeyCode::Char('g') => {
                if let Some(v) = &mut self.vim {
                    v.pending = 'g';
                }
            }
            KeyCode::Char('w') => self.vim_next_used(1),
            KeyCode::Char('b') => self.vim_next_used(-1),
            KeyCode::Char('i') | KeyCode::Char('a') | KeyCode::Enter => {
                self.vim_exit_visual();
                self.start_edit(None);
            }
            KeyCode::Char('c') => {
                self.vim_exit_visual();
                self.start_edit(Some(' '));
                if let Some(e) = &mut self.edit {
                    e.text.clear();
                    e.cursor = 0;
                }
            }
            KeyCode::Char('x') => self.clear_selection(),
            KeyCode::Char('v') => self.vim_toggle_visual(VimMode::Visual),
            KeyCode::Char('V') => self.vim_toggle_visual(VimMode::VisualLine),
            KeyCode::Char('y') => {
                if visual {
                    self.copy(false);
                    self.vim_exit_visual();
                } else if let Some(v) = &mut self.vim {
                    v.pending = 'y';
                }
            }
            KeyCode::Char('d') => {
                if visual {
                    self.clear_selection();
                    self.vim_exit_visual();
                } else if let Some(v) = &mut self.vim {
                    v.pending = 'd';
                }
            }
            KeyCode::Char('p') => self.paste(),
            KeyCode::F(4) => self.open_sheet_picker(),
            KeyCode::PageUp if ctrl => self.switch_sheet(-1),
            KeyCode::PageDown if ctrl => self.switch_sheet(1),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('r') if ctrl => self.redo(),
            KeyCode::Char('s') if ctrl => self.save(),
            KeyCode::Char('q') if ctrl => self.request_exit(),
            KeyCode::Esc => self.vim_exit_visual(),
            _ => {}
        }
        if self.vim_mode() == VimMode::VisualLine {
            self.vim_extend_line();
        }
        false
    }

    fn vim_cmdline_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Esc => {
                if let Some(v) = &mut self.vim {
                    v.cmdline = None;
                }
            }
            KeyCode::Enter => {
                let cmd = self
                    .vim
                    .as_mut()
                    .and_then(|v| v.cmdline.take())
                    .unwrap_or_default();
                return self.vim_run_command(&cmd);
            }
            KeyCode::Backspace => {
                if let Some(Some(c)) = self.vim.as_mut().map(|v| v.cmdline.as_mut()) {
                    c.pop();
                }
            }
            KeyCode::Char(ch) => {
                if let Some(Some(c)) = self.vim.as_mut().map(|v| v.cmdline.as_mut()) {
                    c.push(ch);
                }
            }
            _ => {}
        }
        false
    }

    fn vim_run_command(&mut self, cmd: &str) -> bool {
        match cmd.trim() {
            "w" => {
                self.save();
                false
            }
            // Quit only when the save landed and kept everything: a failed
            // save, or one to a text type that holds only the active sheet
            // (the workbook stays modified), keeps the editor open with the
            // reason on the status line.
            "wq" | "x" => {
                if self.ask_where_to_save_template_workbook() || self.save_current().is_err() {
                    return false;
                }
                if self.modified {
                    let saved = self.status.take().unwrap_or_default();
                    self.status = Some(format!(
                        "{saved} Not quitting: the workbook is not saved in full \
                         (Save As a workbook, or :q! to discard)."
                    ));
                    return false;
                }
                true
            }
            "q" => {
                if self.modified {
                    self.status = Some("Unsaved changes (use :q! to discard)".to_string());
                    false
                } else {
                    true
                }
            }
            "q!" => true,
            other => {
                self.status = Some(format!("Not a command: :{other}"));
                false
            }
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        let (label, text) = match kind {
            PromptKind::Find => ("Find: ", self.last_find.clone().unwrap_or_default()),
            PromptKind::SaveAs => ("Save as: ", self.path.clone()),
            PromptKind::RenameSheet => (
                "Rename sheet: ",
                self.pkg.workbook.sheets[self.sheet].name.clone(),
            ),
            PromptKind::AddSheet => (
                "New sheet name: ",
                format!("Sheet{}", self.pkg.workbook.sheets.len() + 1),
            ),
            PromptKind::RenameTable => ("Table name: ", self.table_here().unwrap_or_default()),
            PromptKind::ResizeTable => {
                let here = self
                    .pkg
                    .workbook
                    .table_at(self.sheet, self.cur.0, self.cur.1);
                let range = here.map(|t| {
                    let (r1, c1, r2, c2) = t.range;
                    format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2))
                });
                ("Resize table to: ", range.unwrap_or_default())
            }
            PromptKind::Relate => ("Relate  From[Col] = To[Col]: ", String::new()),
            PromptKind::Measure => ("Measure  Name = FORMULA: ", String::new()),
            PromptKind::ModelPivot => ("Report  Base; rows; values[; cols]: ", String::new()),
            PromptKind::NewComment => ("Comment (threaded): ", String::new()),
            PromptKind::NewNote => ("Note: ", String::new()),
            PromptKind::ReplaceFind => (
                "Replace — find: ",
                self.last_find.clone().unwrap_or_default(),
            ),
            PromptKind::ReplaceWith => ("Replace with: ", String::new()),
            PromptKind::GoTo => ("Go to: ", String::new()),
            PromptKind::CondFormat => (
                "Highlight (>500, =42, 100..500 between, 'clear'): ",
                String::new(),
            ),
            PromptKind::DataValidation => {
                ("Dropdown list (comma-separated values): ", String::new())
            }
            PromptKind::Filter => (
                "Filter this column (=Laptop, >500, <>0, 'clear'): ",
                String::new(),
            ),
            PromptKind::SortKeys => ("Sort by (e.g. B asc, C desc): ", String::new()),
            PromptKind::RowHeight => ("Row height in points (or 'auto'): ", String::new()),
            PromptKind::DocProperty(i) => {
                let mut p = self.pkg.doc_properties();
                match info_field(&mut p, i as usize) {
                    Some(slot) => (INFO_FIELDS[i as usize].1, slot.take().unwrap_or_default()),
                    None => (CUSTOM_PROPERTY_PROMPT, String::new()),
                }
            }
        };
        let cursor = text.chars().count();
        self.prompt = Some(Prompt {
            kind,
            label,
            text,
            cursor,
        });
    }

    fn commit_prompt(&mut self) {
        let Some(p) = self.prompt.take() else { return };
        let text = p.text.trim().to_string();
        match p.kind {
            PromptKind::Find => {
                if !text.is_empty() {
                    self.last_find = Some(text.clone());
                    self.find_next(&text);
                }
            }
            PromptKind::NewComment => self.commit_comment(&text),
            PromptKind::NewNote => self.commit_note(&text),
            PromptKind::ReplaceFind => {
                if !text.is_empty() {
                    self.replace_find = Some(text.clone());
                    self.last_find = Some(text);
                    self.open_prompt(PromptKind::ReplaceWith);
                }
            }
            PromptKind::ReplaceWith => {
                if let Some(find) = self.replace_find.take() {
                    self.replace_all(&find, &text);
                }
            }
            PromptKind::GoTo => self.goto(&text),
            PromptKind::CondFormat => self.commit_cond_format(&text),
            PromptKind::DataValidation => self.commit_data_validation(&text),
            PromptKind::Filter => self.commit_filter(&text),
            PromptKind::SortKeys => self.commit_sort(&text),
            PromptKind::RowHeight => self.commit_row_height(&text),
            PromptKind::DocProperty(i) => {
                let message = self.commit_doc_property(i as usize, &text);
                self.reopen_info(i as usize, message);
            }
            PromptKind::SaveAs => {
                if !text.is_empty() {
                    self.request_save_as(text);
                }
            }
            PromptKind::RenameSheet => {
                if !text.is_empty() && !text.contains(['[', ']', '*', '?', ':', '/', '\\']) {
                    let idx = self.sheet;
                    self.structural(|wb| gridcore::edit::rename_sheet(wb, idx, &text));
                    self.status = Some(format!("Renamed sheet to {text}"));
                } else {
                    self.status = Some("Invalid sheet name".to_string());
                }
            }
            PromptKind::RenameTable => {
                if let Some(old) = self.table_here() {
                    self.status = Some(match self.rename_table(&old, &text) {
                        Ok(()) => format!("Renamed table {old} to {text}"),
                        Err(why) => why,
                    });
                }
            }
            PromptKind::ResizeTable => {
                if let Some(name) = self.table_here() {
                    self.status = Some(match self.resize_table(&name, &text) {
                        Ok(()) => format!("Resized {name} to {}", text.to_uppercase()),
                        Err(why) => why,
                    });
                }
            }
            PromptKind::Relate => {
                let parts: Vec<&str> = if text.contains("->") {
                    text.splitn(2, "->").collect()
                } else {
                    text.splitn(2, '=').collect()
                };
                let parsed = match parts.as_slice() {
                    [a, b] => parse_table_col(a).zip(parse_table_col(b)),
                    _ => None,
                };
                match parsed {
                    Some(((ft, fc), (tt, tc))) => {
                        let mut model = self.current_model();
                        match model.relate(&ft, &fc, &tt, &tc) {
                            Ok(()) => {
                                self.model_rels
                                    .push(model.relationships.pop().expect("just added"));
                                self.modified = true;
                                self.status = Some(format!("Related {ft}[{fc}] → {tt}[{tc}]"));
                            }
                            Err(e) => self.status = Some(format!("relate: {e}")),
                        }
                    }
                    None => {
                        self.status =
                            Some("Expected  From[Col] = To[Col]  (tables must exist)".to_string());
                    }
                }
            }
            PromptKind::Measure => {
                let Some((name, formula)) = text.split_once('=') else {
                    self.status = Some("Expected  Name = FORMULA".to_string());
                    return;
                };
                let (name, formula) = (name.trim(), formula.trim());
                if name.is_empty() || name.contains(['[', ']', ' ']) {
                    self.status = Some("Measure names are single words".to_string());
                    return;
                }
                if let Err(e) = Engine::validate(formula) {
                    self.status = Some(format!("measure formula: {e}"));
                    return;
                }
                self.model_measures
                    .retain(|m| !m.name.eq_ignore_ascii_case(name));
                self.model_measures.push(gridcore::model::Measure {
                    name: name.to_string(),
                    formula: formula.to_string(),
                });
                self.modified = true;
                self.status = Some(format!("Measure {name} defined"));
            }
            PromptKind::ModelPivot => {
                let seg: Vec<&str> = text.split(';').map(str::trim).collect();
                if seg.len() < 3 || seg[0].is_empty() {
                    self.status = Some("Expected  Base; rows; values[; cols]".to_string());
                    return;
                }
                let list = |s: &str| -> Vec<String> {
                    s.split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect()
                };
                let spec = ModelSpec {
                    rows: list(seg[1]),
                    cols: seg.get(3).map(|s| list(s)).unwrap_or_default(),
                    measures: list(seg[2]).into_iter().map(|v| (v.clone(), v)).collect(),
                    grand_rows: true,
                    grand_cols: true,
                };
                if spec.measures.is_empty() {
                    self.status = Some("A report needs at least one value".to_string());
                    return;
                }
                let base = seg[0].to_string();
                self.build_model_report(&base, &spec);
            }
            PromptKind::AddSheet => {
                if text.is_empty() || self.pkg.workbook.sheet_index(&text).is_some() {
                    self.status = Some("Sheet name empty or already taken".to_string());
                } else {
                    let new_idx = self.pkg.add_sheet(&text);
                    self.sheet = new_idx;
                    self.cur = (0, 0);
                    self.top = 0;
                    self.left = 0;
                    self.anchor = None;
                    // Package parts changed: old snapshots no longer line up.
                    self.undo.clear();
                    self.redo.clear();
                    self.rebuild_engine();
                    self.modified = true;
                    self.status = Some(format!("Added sheet {text}"));
                }
            }
        }
    }

    /// Make a pending cut a copy: a later paste clears nothing. Removing a
    /// sheet does this, since it can take or renumber the cut's source sheet,
    /// and so does any structural edit and the undo/redo of one, since most
    /// of them move cells under the cut's recorded coordinates. A protection
    /// toggle moves none and keeps the cut (`toggle_protection`).
    fn cancel_cut(&mut self) {
        if let Some(clip) = &mut self.clip {
            clip.cut = false;
        }
    }

    fn delete_current_sheet(&mut self) {
        let name = self.pkg.workbook.sheets[self.sheet].name.clone();
        let gone = self.sheet;
        if self.pkg.remove_sheet(gone) {
            self.cancel_cut();
            // The copy names its sheet by index: one deleted leaves it no
            // sheet (it still pastes its cells as a copy, but Paste Special
            // has nothing to read), and one before it renumbers it (#707 r1).
            if let Some(clip) = self.clip.as_mut() {
                if clip.sheet == gone {
                    clip.sheet = SHEET_GONE;
                } else if clip.sheet > gone && clip.sheet != SHEET_GONE {
                    clip.sheet -= 1;
                }
            }
            self.sheet = self.sheet.min(self.pkg.workbook.sheets.len() - 1);
            self.cur = (0, 0);
            self.top = 0;
            self.left = 0;
            self.anchor = None;
            self.undo.clear();
            self.redo.clear();
            self.rebuild_engine();
            self.modified = true;
            self.status = Some(format!("Deleted sheet {name}"));
        } else {
            self.status = Some("Cannot delete the last sheet".to_string());
        }
    }

    /// Numeric stats over the selection for the status bar, Excel-style.
    fn selection_stats(&self) -> Option<String> {
        let (r1, c1, r2, c2) = self.selection();
        if r1 == r2 && c1 == c2 {
            return None;
        }
        let mut nums = Vec::new();
        let mut count_all = 0usize;
        for (&(r, c), cell) in self.sheet().cells.range((r1, 0)..=(r2, u32::MAX)) {
            if c < c1 || c > c2 || r < r1 || r > r2 {
                continue;
            }
            if cell.value.is_empty() {
                continue;
            }
            count_all += 1;
            if let CellValue::Number(n) = cell.value {
                nums.push(n);
            }
        }
        if count_all == 0 {
            return None;
        }
        let mut s = format!("Count: {count_all}");
        if !nums.is_empty() {
            let sum: f64 = nums.iter().sum();
            let avg = sum / nums.len() as f64;
            s = format!(
                "Average: {}   Count: {}   Sum: {}",
                gridcore::sheet::fmt_general(avg),
                count_all,
                gridcore::sheet::fmt_general(sum)
            );
        }
        Some(s)
    }
}

/// Format-specific content the shared File backstage needs from xlsxy: only
/// workbooks/CSVs are listed/opened, the Save As default is the current
/// file's name, the preview renders the highlighted workbook's first sheet,
/// the Info pane shows workbook stats, and the accent matches xlsxy's ribbon
/// (green).
impl backstage::BackstageHost for App {
    fn extensions(&self) -> &'static [&'static str] {
        &[
            "xlsx", "xlsm", "xltx", "xltm", "xls", "xlsb", "ods", "csv", "tsv", "txt", "prn",
        ]
    }

    fn default_save_type(&self) -> Option<usize> {
        self.bound_text_type().or_else(|| type_for_path(&self.path))
    }

    fn default_save_name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.xlsx".to_string())
    }

    /// Render a quick preview of the highlighted workbook's first sheet.
    fn preview_lines(&self, path: &std::path::Path, width: usize) -> Vec<String> {
        preview_lines(&path.to_string_lossy(), width)
    }

    fn info_lines(&self) -> Vec<RLine<'static>> {
        let sheets: Vec<&str> = self
            .pkg
            .workbook
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        let mut lines = vec![
            RLine::raw(format!("  File        {}", self.path)),
            RLine::raw(format!(
                "  Modified    {}",
                if self.modified { "yes" } else { "no" }
            )),
            RLine::raw(String::new()),
            RLine::raw(format!(
                "  Sheets      {} ({})",
                sheets.len(),
                sheets.join(", ")
            )),
            RLine::raw(format!("  Comments    {}", self.comments.len())),
        ];
        let p = self.pkg.doc_properties();
        let row = |label: &str, value: &Option<String>| {
            RLine::raw(format!("  {label:<18}{}", value.as_deref().unwrap_or("")))
        };
        lines.push(RLine::raw(String::new()));
        lines.push(row("Author", &p.creator));
        lines.push(row("Last Modified By", &p.last_modified_by));
        lines.push(row("Created", &p.created));
        lines.push(row("Last Modified", &p.modified));
        for (i, c) in p.custom.iter().enumerate() {
            let label = if i == 0 { "Custom" } else { "" };
            lines.push(RLine::raw(format!(
                "  {label:<18}{} = {}",
                c.name,
                c.value.display()
            )));
        }
        lines
    }

    fn info_fields(&self) -> Vec<(String, String)> {
        let mut p = self.pkg.doc_properties();
        INFO_FIELDS
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let value = info_field(&mut p, i).and_then(Option::take);
                (label.to_string(), value.unwrap_or_default())
            })
            .collect()
    }

    fn info_custom_row(&self) -> bool {
        true
    }

    fn accent(&self) -> Color {
        Color::Green
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

const HDR_STYLE: Style = Style::new().fg(Color::Black).bg(Color::Gray);
const HDR_CUR: Style = Style::new()
    .fg(Color::White)
    .bg(Color::DarkGray)
    .add_modifier(Modifier::BOLD);

fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    if area.height < 8 || area.width < 12 {
        return;
    }
    // A confirmation modal owns the whole screen — no content behind it (it
    // can be raised from the welcome screen too, so check it first).
    if let Some(c) = app.confirm.as_mut() {
        f.render_widget(Clear, area);
        c.draw(f, area);
        return;
    }
    // Full-screen surfaces paint over everything else.
    if app.start_screen {
        f.render_widget(Clear, area);
        app.start.draw(f, area);
        return;
    }
    if app.backstage.is_some() {
        // `backstagecore::draw` clears the full frame and renders the menu +
        // content below row 0 — draw it first, then paint the ribbon tab strip
        // (File highlighted) over row 0 last so it isn't wiped out.
        let mut bs = app.backstage.take();
        if let Some(b) = bs.as_mut() {
            backstage::draw(f, area, b, app);
        }
        app.backstage = bs;
        // Keep the ribbon tab headers visible: clicking another tab leaves the
        // backstage, and clicking File closes it back to the grid — so the
        // panel can be dismissed entirely with the mouse.
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut tabline = app.ribbon.render_tabs_as(0); // 0 = File
        tabline
            .spans
            .push(RSpan::styled("   (click a tab or Esc to leave)", dim));
        let row0 = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(tabline), row0);
        return;
    }
    // --- ribbon (tab strip, plus body + hint when engaged) --------------------
    let toggles = app.ribbon_toggles();
    app.ribbon.set_toggles(toggles);
    let engaged = app.ribbon_focus != ribbon::Focus::None;
    let ribbon_h: u16 = if engaged {
        ribbon::EXPANDED_H // tab strip + closed body box (6)
    } else if app.auto_hide_ribbon {
        0 // auto-hide reclaims the tab strip line for the grid
    } else {
        1
    };
    app.ribbon_rows = ribbon_h;
    let mut y = area.y;
    if ribbon_h >= 1 {
        f.render_widget(
            Paragraph::new(app.ribbon.render_tabs(app.ribbon_focus)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }
    if engaged {
        let body = app.ribbon.render_body(app.ribbon_focus);
        f.render_widget(Paragraph::new(body), Rect::new(area.x, y, area.width, 6));
        y += 6;
    }

    let fx_h = app.fx_bar_height();
    let formula_bar = Rect::new(area.x, y, area.width, fx_h);
    // A column outline takes a line above the column header.
    let (row_levels, col_levels) = {
        let sh = app.sheet();
        (sh.max_row_outline(), sh.max_col_outline())
    };
    let col_outline_h = u16::from(col_levels > 0);
    let col_outline = Rect::new(area.x, y + fx_h, area.width, col_outline_h);
    let col_hdr = Rect::new(area.x, y + fx_h + col_outline_h, area.width, 1);
    let grid_h = area
        .height
        .saturating_sub(ribbon_h + 3 + fx_h + col_outline_h);
    let mut grid = Rect::new(area.x, y + fx_h + 1 + col_outline_h, area.width, grid_h);
    app.col_outline_y = (col_outline_h > 0).then_some(col_outline.y);
    app.col_hdr_y = col_hdr.y;
    let tabs_line = Rect::new(area.x, area.y + area.height - 2, area.width, 1);
    let hint_line = Rect::new(area.x, area.y + area.height - 1, area.width, 1);

    // --- comments side panel reserves space on the right ----------------------
    let panel_w: u16 = if app.show_comments && !app.comments.is_empty() {
        34u16.min(grid.width / 2)
    } else {
        0
    };
    let panel = if panel_w > 0 {
        let p = Rect::new(grid.x + grid.width - panel_w, grid.y, panel_w, grid.height);
        grid.width -= panel_w;
        Some(p)
    } else {
        None
    };
    app.grid_area = grid;

    // Freeze: keep the scroll origin at or past the frozen region.
    let (fr, fc) = app.freeze();
    if app.left < fc {
        app.left = fc;
    }
    if app.top < fr {
        app.top = fr;
    }

    // Row gutter sized for the largest visible row number, after the row
    // outline's columns (one per level, plus one).
    let max_row = app.top + grid.height as u32;
    app.outline_w = if row_levels > 0 {
        u16::from(row_levels) + 1
    } else {
        0
    };
    app.gutter_w = app.outline_w + (max_row + 1).to_string().len().max(3) as u16 + 1;

    // Visible columns: frozen columns (0..fc) pinned, then scrollable from left.
    app.vis_cols.clear();
    {
        let mut x = app.gutter_w;
        let show_hidden = app.show_hidden;
        let push = |app: &mut App, col: u32, x: &mut u16| {
            if *x >= grid.width || col >= MAX_COLS {
                return false;
            }
            // A hidden column takes no space but keeps the scan going.
            if !show_hidden && app.sheet().col_hidden(col) {
                return true;
            }
            let w = app.col_disp_width(col).min(grid.width - *x);
            app.vis_cols.push((col, *x, w));
            *x += w;
            true
        };
        for col in 0..fc {
            push(app, col, &mut x);
        }
        let mut col = app.left.max(fc);
        while push(app, col, &mut x) {
            col += 1;
        }
    }

    // Visible rows: frozen rows (0..fr) pinned, then scrollable from top.
    // Rows hidden by a filter or manual hide are skipped unless `show_hidden`.
    // A tall row (wrap / explicit height) occupies several screen lines, so it
    // is emitted once per line with its sub-line index in `vis_subline`.
    let cap = grid.height as usize;
    let (mut vr, mut vs): (Vec<u32>, Vec<u8>) = (Vec::new(), Vec::new());
    {
        let sheet = &app.pkg.workbook.sheets[app.sheet];
        let styles = &app.pkg.workbook.styles;
        let d1904 = app.pkg.workbook.date1904;
        let show_hidden = app.show_hidden;
        let vis_cols = &app.vis_cols;
        // Rows to consider: frozen (0..fr), then scrollable from the top.
        let scroll = (app.top.max(fr)..MAX_ROWS).take(cap);
        for row in (0..fr).chain(scroll) {
            if vr.len() >= cap {
                break;
            }
            if !show_hidden && sheet.row_hidden(row) {
                continue;
            }
            let h = row_line_count(sheet, styles, row, vis_cols, d1904);
            for sub in 0..h {
                if vr.len() >= cap {
                    break;
                }
                vr.push(row);
                vs.push(sub as u8);
            }
        }
    }
    app.vis_rows = vr;
    app.vis_subline = vs;

    // --- formula bar --------------------------------------------------------
    let (r, c) = app.cur;
    let name = cell_name(r, c);
    let content = match &app.edit {
        Some(e) => e.text.clone(),
        None => app.current_input_text(),
    };
    let mut spans = vec![
        RSpan::styled(
            format!(" {name:<8}"),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        RSpan::raw("│ "),
    ];
    if let Some((from, e)) = app
        .edit
        .as_ref()
        .and_then(|e| Some((e.proposal.as_ref()?.0, e)))
    {
        // An AutoComplete proposal: the typed text, then its suffix
        // selected (the caret sits where the suffix starts).
        let chars: Vec<char> = e.text.chars().collect();
        spans.push(RSpan::raw(chars[..from].iter().collect::<String>()));
        spans.push(RSpan::styled(
            chars[from..].iter().collect::<String>(),
            Style::new().add_modifier(Modifier::REVERSED),
        ));
    } else if let Some(e) = &app.edit {
        // Draw text with a visible cursor block.
        let chars: Vec<char> = e.text.chars().collect();
        let before: String = chars[..e.cursor.min(chars.len())].iter().collect();
        let at: String = chars
            .get(e.cursor)
            .map(|ch| ch.to_string())
            .unwrap_or_else(|| " ".to_string());
        let after: String = if e.cursor < chars.len() {
            chars[(e.cursor + 1).min(chars.len())..].iter().collect()
        } else {
            String::new()
        };
        spans.push(RSpan::raw(before));
        spans.push(RSpan::styled(
            at,
            Style::new().add_modifier(Modifier::REVERSED),
        ));
        spans.push(RSpan::raw(after));
    } else {
        spans.push(RSpan::raw(content));
    }
    let bar = Paragraph::new(RLine::from(spans));
    let bar = if app.fx_expanded {
        bar.wrap(ratatui::widgets::Wrap { trim: false })
    } else {
        bar
    };
    f.render_widget(bar, formula_bar);

    // --- column outline -----------------------------------------------------
    if col_levels > 0 {
        let groups = outline::groups(app.sheet(), Axis::Cols);
        let mut spans = vec![RSpan::styled(
            level_buttons(col_levels, app.gutter_w as usize),
            HDR_STYLE,
        )];
        for &(col, _, w) in &app.vis_cols {
            let summary = groups
                .iter()
                .filter(|g| g.summary == Some(col))
                .max_by_key(|g| g.level);
            let text = match summary {
                Some(g) => center(if g.collapsed { "+" } else { "-" }, w as usize),
                None if app.sheet().col_outline(col) > 0 => "─".repeat(w as usize),
                None => " ".repeat(w as usize),
            };
            spans.push(RSpan::raw(text));
        }
        f.render_widget(Paragraph::new(RLine::from(spans)), col_outline);
    }

    // --- column headers ------------------------------------------------------
    let mut hdr_spans: Vec<RSpan> = vec![RSpan::styled(
        format!(
            "{}{}",
            level_buttons(row_levels, app.outline_w as usize),
            " ".repeat((app.gutter_w - app.outline_w) as usize)
        ),
        HDR_STYLE,
    )];
    for &(col, _, w) in &app.vis_cols {
        let name = col_name(col);
        let style = if col == c { HDR_CUR } else { HDR_STYLE };
        hdr_spans.push(RSpan::styled(center(&name, w as usize), style));
    }
    f.render_widget(Paragraph::new(RLine::from(hdr_spans)), col_hdr);

    // --- grid ---------------------------------------------------------------
    let (r1, c1, r2, c2) = app.selection();
    let cur_sheet = app.sheet;
    let commented: std::collections::HashSet<(u32, u32)> = app
        .comments
        .iter()
        .filter(|c| c.sheet == cur_sheet)
        .map(|c| (c.row, c.col))
        .collect();
    let base = app.base_style();
    let formula_view = app.formula_view;
    let vis_rows = app.vis_rows.clone();
    let vis_subline = app.vis_subline.clone();
    let sheet = app.sheet();
    let styles = &app.pkg.workbook.styles;
    let date1904 = app.pkg.workbook.date1904;
    let merges = sheet.merges.clone();
    let row_groups = if row_levels > 0 {
        outline::groups(sheet, Axis::Rows)
    } else {
        Vec::new()
    };
    let num_w = (app.gutter_w - app.outline_w) as usize;
    let mut lines: Vec<RLine> = Vec::with_capacity(grid.height as usize);
    for (li, &row) in vis_rows.iter().enumerate() {
        let sub = vis_subline.get(li).copied().unwrap_or(0) as usize;
        let mut spans: Vec<RSpan> = Vec::with_capacity(app.vis_cols.len() + 2);
        let gut_style = if row == r { HDR_CUR } else { HDR_STYLE };
        if row_levels > 0 {
            spans.push(RSpan::styled(
                row_outline_cells(sheet, &row_groups, row, row_levels, sub == 0),
                HDR_STYLE,
            ));
        }
        // The row number shows only on the row's first line.
        spans.push(RSpan::styled(
            if sub == 0 {
                format!("{:>w$} ", row + 1, w = num_w - 1)
            } else {
                " ".repeat(num_w)
            },
            gut_style,
        ));
        // Skip columns covered by a horizontal merge whose top-left is to the left.
        let mut skip_to: i64 = -1;
        for &(col, _, w) in &app.vis_cols {
            if (col as i64) <= skip_to {
                continue;
            }
            // Merged regions: the top-left cell spans its columns' combined visible
            // width; covered cells in the same row are skipped; cells under a
            // vertical merge render blank (content lives only in the top-left).
            let merge = merges
                .iter()
                .find(|&&(mr1, mc1, mr2, mc2)| row >= mr1 && row <= mr2 && col >= mc1 && col <= mc2)
                .copied();
            let (w, blank_covered) = match merge {
                Some((mr1, mc1, _mr2, mc2)) if row == mr1 && col == mc1 => {
                    skip_to = mc2 as i64;
                    let cw: u16 = app
                        .vis_cols
                        .iter()
                        .filter(|&&(cc, _, _)| cc >= col && cc <= mc2)
                        .map(|&(_, _, ww)| ww)
                        .sum();
                    (cw.max(1), false)
                }
                Some((mr1, _, _, _)) if row == mr1 => continue, // covered in the top row
                Some(_) => (w, true),                           // under a vertical merge
                None => (w, false),
            };
            let cell = sheet.cell(row, col);
            let xf = cell.map(|cl| styles.xf(cl.style)).unwrap_or_default();
            let text = if blank_covered {
                String::new()
            } else {
                match cell {
                    Some(cl) if formula_view && cl.formula.is_some() => {
                        format!("={}", cl.formula.as_ref().unwrap())
                    }
                    Some(cl) => grid_text(&xf, &cl.value, date1904, w),
                    None => String::new(),
                }
            };
            let numeric = matches!(cell.map(|cl| &cl.value), Some(CellValue::Number(_)))
                && xf.numfmt != NumFmt::Text;
            // In a multi-line row, pick this screen line's slice of the cell:
            // wrapped cells split across lines; other cells sit on line 0 (blank
            // below). Non-wrap content is never truncated by wrapping.
            let line_text = if xf.wrap && !formula_view {
                wrap_text(&text, w as usize)
                    .get(sub)
                    .cloned()
                    .unwrap_or_default()
            } else if sub == 0 {
                text.clone()
            } else {
                String::new()
            };
            // Formula view is always left-aligned (Excel shows the raw text).
            let display = if formula_view {
                fit(&line_text, w as usize, false)
            } else {
                match xf.align {
                    Align::Left => fit(&line_text, w as usize, false),
                    Align::Right => fit(&line_text, w as usize, true),
                    Align::Center => center(&line_text, w as usize),
                    Align::General => fit(&line_text, w as usize, numeric),
                }
            };
            let mut style = base;
            if xf.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if xf.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if let Some((cr, cg, cb)) = xf.color {
                style = style.fg(Color::Rgb(cr, cg, cb));
            }
            if let Some((fr, fg, fb)) = xf.fill {
                style = style.bg(Color::Rgb(fr, fg, fb));
            }
            // Conditional formatting overlays a differential format on the cell.
            if let Some(dxf) = gridcore::cf::cell_dxf(&app.pkg.workbook, app.sheet, row, col) {
                if dxf.bold == Some(true) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if dxf.italic == Some(true) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if let Some((cr, cg, cb)) = dxf.color {
                    style = style.fg(Color::Rgb(cr, cg, cb));
                }
                if let Some((fr, fg, fb)) = dxf.fill {
                    style = style.bg(Color::Rgb(fr, fg, fb));
                }
            }
            // A hyperlinked cell reads as a link (blue + underline).
            if sheet.hyperlinks.contains_key(&(row, col)) {
                style = style.fg(Color::Blue).add_modifier(Modifier::UNDERLINED);
            }
            let selected = row >= r1 && row <= r2 && col >= c1 && col <= c2;
            let is_cursor = (row, col) == (r, c);
            if is_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            } else if selected {
                style = style.bg(Color::DarkGray).fg(Color::White);
            }
            // A commented cell is underlined (Excel's red-triangle analogue).
            if commented.contains(&(row, col)) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            spans.push(RSpan::styled(display, style));
        }
        lines.push(RLine::from(spans));
    }
    f.render_widget(Paragraph::new(lines), grid);

    // --- floating drawings (pictures / charts) --------------------------------
    if app.show_drawings {
        draw_drawings(app, f, grid);
    }

    // --- comments side panel --------------------------------------------------
    if let Some(rect) = panel {
        draw_comments_panel(app, f, rect);
    }

    // --- pivot editor overlay -------------------------------------------------
    if let Some(pe) = &app.pivot_edit {
        draw_pivot_editor(app, pe, f, grid);
    }

    // --- model view overlay -----------------------------------------------------
    if let Some((pane, sel)) = app.model_view {
        draw_model_view(app, pane, sel, f, grid);
    }

    // --- formatting popup -------------------------------------------------------
    if let Some(p) = &app.format_picker {
        draw_format_picker(p, f, grid);
    }
    if let Some(d) = &app.format_dialog {
        draw_format_dialog(app, d, f, grid);
    }
    if let Some(d) = &app.text_dialog {
        d.draw(f, grid);
    }
    if let Some(d) = &app.outline_dialog {
        d.draw(f, grid);
    }

    // --- sheet picker -----------------------------------------------------------
    if let Some(sel) = app.sheet_picker {
        draw_sheet_picker(app, sel, f, grid);
    }

    // --- data-validation dropdown ----------------------------------------------
    if let Some(p) = &app.dv_picker {
        draw_dv_picker(app, p, f, grid);
    }

    // --- sheet tabs + stats ---------------------------------------------------
    app.tab_spans.clear();
    // A leading marker doubles as a click target for the sheet picker.
    let marker = format!(" ⊞ {}/{} ", app.sheet + 1, app.pkg.workbook.sheets.len());
    let marker_w = marker.chars().count() as u16;
    let mut tab_spans_ui: Vec<RSpan> = vec![RSpan::styled(
        marker.clone(),
        Style::new().fg(Color::Black).bg(Color::Cyan),
    )];
    app.tab_spans.push((usize::MAX, 0, marker_w)); // sentinel: opens the picker
    let mut x: u16 = marker_w;
    for (i, s) in app.pkg.workbook.sheets.iter().enumerate() {
        let active = i == app.sheet;
        // A protected sheet carries a lock glyph in its tab.
        let lock = if s.is_protected() { "🔒" } else { "" };
        // Same width whether active or not, so tabs stay aligned.
        let label = if active {
            format!("[ {lock}{} ]", s.name)
        } else {
            format!("  {lock}{}  ", s.name)
        };
        let w = label.chars().count() as u16;
        let style = if active {
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Gray)
        };
        app.tab_spans.push((i, x, x + w));
        tab_spans_ui.push(RSpan::styled(label, style));
        tab_spans_ui.push(RSpan::raw(" "));
        x += w + 1;
    }
    // Excel's persistent status-bar note while any circle exists (none with
    // iterative calculation on).
    if let Some(first) = app.circular_refs().first().filter(|_| app.circles_shown()) {
        tab_spans_ui.push(RSpan::styled(
            format!(" Circular References: {first} "),
            Style::new().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    for word in app.status_words() {
        tab_spans_ui.push(RSpan::styled(
            format!(" {word} "),
            Style::new().fg(Color::Black).bg(Color::Gray),
        ));
    }
    let mut tabs_line_ui = RLine::from(tab_spans_ui);
    if let Some(stats) = app.selection_stats() {
        let pad = (tabs_line.width as usize)
            .saturating_sub(tabs_line_ui.width() + stats.chars().count() + 1);
        tabs_line_ui.push_span(RSpan::raw(" ".repeat(pad)));
        tabs_line_ui.push_span(RSpan::styled(stats, Style::new().fg(Color::Cyan)));
    }
    f.render_widget(Paragraph::new(tabs_line_ui), tabs_line);

    // --- hints / status ---------------------------------------------------------
    if let Some(p) = &app.prompt {
        // Minibuffer with a visible cursor block.
        let chars: Vec<char> = p.text.chars().collect();
        let before: String = chars[..p.cursor.min(chars.len())].iter().collect();
        let at: String = chars
            .get(p.cursor)
            .map(|ch| ch.to_string())
            .unwrap_or_else(|| " ".to_string());
        let after: String = if p.cursor < chars.len() {
            chars[(p.cursor + 1).min(chars.len())..].iter().collect()
        } else {
            String::new()
        };
        f.render_widget(
            Paragraph::new(RLine::from(vec![
                RSpan::styled(p.label, Style::new().add_modifier(Modifier::BOLD)),
                RSpan::raw(before),
                RSpan::styled(at, Style::new().add_modifier(Modifier::REVERSED)),
                RSpan::raw(after),
            ])),
            hint_line,
        );
        return;
    }
    let hint = if let Some(v) = app.vim.as_ref().filter(|_| app.prompt.is_none()) {
        match &v.cmdline {
            Some(c) => format!(":{c}"),
            None => match v.mode {
                VimMode::Normal => {
                    "-- NORMAL --  hjkl move · i edit · v visual · :w :q".to_string()
                }
                VimMode::Visual => {
                    "-- VISUAL --  hjkl extend · y yank · d/x delete · Esc".to_string()
                }
                VimMode::VisualLine => {
                    "-- VISUAL LINE --  j/k rows · y yank · d delete · Esc".to_string()
                }
            },
        }
    } else if app.model_view.is_some() && app.prompt.is_none() {
        "Model: ←/→ pane · ↑/↓ select · r relate · m measure · p report · d delete · Esc close"
            .to_string()
    } else if app.pivot_edit.is_some() {
        "Pivot: ←/→ pane · ↑/↓ select · Shift-↑/↓ reorder · r/c/v add · d remove · a aggregation · Esc close"
            .to_string()
    } else if let Some(s) = &app.status {
        s.clone()
    } else if app.edit.is_some() {
        "Enter commit ↓ · Tab commit → · Esc cancel".to_string()
    } else if let Some(dv) = app.current_validation() {
        if dv.kind == "list" {
            format!("✔ {}   ·   Alt-↓ dropdown", dv.describe())
        } else {
            format!("✔ Data validation — {}", dv.describe())
        }
    } else {
        format!(
            "{}{}  F9 ribbon  ^S save  ^Q quit  ^Z undo  ^F find  ^D/^R fill  ^T sheet",
            app.path,
            if app.modified { " *" } else { "" }
        )
    };
    f.render_widget(
        Paragraph::new(RLine::from(RSpan::styled(
            fit(&hint, hint_line.width as usize, false),
            Style::new().fg(Color::Gray),
        ))),
        hint_line,
    );
}

/// Display width of a string in terminal columns (wide CJK/emoji glyphs count
/// as 2), so grid layout and mouse hit-testing stay aligned with what's drawn.
fn disp_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Truncate a string to at most `w` display columns, returning it and the
/// columns it actually occupies (a wide glyph may stop one short of `w`).
fn truncate_width(s: &str, w: usize) -> (String, usize) {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    (out, used)
}

/// Pad/clip to exactly `w` display columns. Wide glyphs are measured by their
/// terminal width so alignment (and mouse hit-testing over `vis_cols`) holds.
fn fit(s: &str, w: usize, right: bool) -> String {
    let width = disp_width(s);
    if width >= w {
        // Leave a trailing space as a clipped-content indicator.
        let (cut, used) = truncate_width(s, w.saturating_sub(1));
        format!("{cut}{}", " ".repeat(w - used))
    } else if right {
        format!("{}{} ", " ".repeat(w - width - 1), s)
    } else {
        format!("{}{}", s, " ".repeat(w - width))
    }
}

fn center(s: &str, w: usize) -> String {
    let width = disp_width(s);
    if width >= w {
        return truncate_width(s, w).0;
    }
    let lead = (w - width) / 2;
    format!("{}{}{}", " ".repeat(lead), s, " ".repeat(w - width - lead))
}

/// What a grid cell `w` columns wide (a merge's whole span) shows for
/// `value`: a date/time it cannot show fills it with `#`; a General number
/// is fitted as Excel's General shows it (at most 11 characters, fewer
/// decimals or scientific when narrower; `fit` keeps one column for the
/// trailing space); anything else is its formatted text.
fn grid_text(xf: &gridcore::sheet::Xf, value: &CellValue, date1904: bool, w: u16) -> String {
    if date_unrepresentable(xf, value, date1904) {
        return "#".repeat(w as usize);
    }
    match value {
        CellValue::Number(n) if gridcore::entry::is_general(xf) => {
            gridcore::sheet::fmt_general_cell(*n, (w as usize).saturating_sub(1))
        }
        _ => format_with(xf, value, date1904),
    }
}

/// How many screen lines a row occupies: derived from an explicit row height
/// (≈15pt per line) and from any wrap-enabled cell's wrapped line count, capped
/// so a pathological cell can't blow up the layout.
fn row_line_count(
    sheet: &Sheet,
    styles: &gridcore::sheet::Styles,
    row: u32,
    vis_cols: &[(u32, u16, u16)],
    date1904: bool,
) -> u16 {
    const CAP: u16 = 12;
    let mut lines = 1u16;
    if let Some(ht) = sheet.row_height(row) {
        lines = lines.max(((ht / 15.0).round() as u16).max(1));
    }
    for &(col, _, w) in vis_cols {
        if let Some(cl) = sheet.cell(row, col) {
            let xf = styles.xf(cl.style);
            if xf.wrap && !cl.is_blank() {
                let text = format_with(&xf, &cl.value, date1904);
                lines = lines.max(wrap_text(&text, w as usize).len() as u16);
            }
        }
    }
    lines.min(CAP)
}

/// The pivot field editor: four panes over a cleared overlay rect.
fn draw_pivot_editor(app: &App, pe: &PivotEdit, f: &mut Frame, grid: Rect) {
    let piv = &app.pkg.workbook.pivots[pe.pivot];
    // Never exceed the grid area (tiny terminals must not underflow).
    let w = grid.width.min(76);
    let h = grid.height.min(14);
    if w < 12 || h < 4 {
        return;
    }
    let x = grid.x + (grid.width - w) / 2;
    let y = grid.y + (grid.height - h) / 2;
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);

    let col_w = (w as usize - 2) / 4;
    let titles = ["Fields", "Rows", "Columns", "Values"];
    let mut lines: Vec<RLine> = Vec::new();
    lines.push(RLine::from(RSpan::styled(
        fit(&format!(" Pivot: {}", piv.name), w as usize, false),
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )));
    let mut hdr: Vec<RSpan> = vec![RSpan::raw(" ")];
    for (i, t) in titles.iter().enumerate() {
        let style = if i == pe.pane {
            Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        };
        hdr.push(RSpan::styled(fit(t, col_w, false), style));
    }
    lines.push(RLine::from(hdr));
    let panes: Vec<Vec<String>> = (0..4).map(|i| app.pivot_pane_items(pe, i)).collect();
    let rows = h as usize - 3;
    for row in 0..rows {
        let mut spans: Vec<RSpan> = vec![RSpan::raw(" ")];
        for (i, items) in panes.iter().enumerate() {
            let text = items.get(row).cloned().unwrap_or_default();
            let mut style = Style::new();
            if i == pe.pane && row == pe.sel && !text.is_empty() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            spans.push(RSpan::styled(fit(&text, col_w, false), style));
        }
        lines.push(RLine::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(Color::Black).fg(Color::White)),
        area,
    );
}

/// The sheet-picker popup: every sheet, the highlighted one selected.
fn draw_sheet_picker(app: &App, sel: usize, f: &mut Frame, grid: Rect) {
    let names: Vec<&str> = app
        .pkg
        .workbook
        .sheets
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    let widest = names.iter().map(|n| n.chars().count()).max().unwrap_or(6);
    // `.max().min()`, not `clamp`: on a terminal narrower than the minimum,
    // `clamp`'s `min <= max` assert fires before the "too small to draw" guard
    // below can decline.
    let w = ((widest + 6) as u16)
        .max(16)
        .min(grid.width.saturating_sub(2));
    let h = (names.len() as u16 + 3).min(grid.height);
    if w < 12 || h < 4 {
        return;
    }
    let x = grid.x + (grid.width - w) / 2;
    let y = grid.y + (grid.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);
    let mut lines: Vec<RLine> = vec![RLine::from(RSpan::styled(
        fit(" Go to sheet", w as usize, false),
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    ))];
    // Keep the selected row visible if the list is long.
    let vis = h.saturating_sub(3) as usize;
    let top = sel.saturating_sub(vis.saturating_sub(1));
    for (i, name) in names.iter().enumerate().skip(top).take(vis) {
        let marker = if i == app.sheet { "●" } else { " " };
        let label = format!(" {marker} {name}");
        let style = if i == sel {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new()
        };
        lines.push(RLine::from(RSpan::styled(
            fit(&label, w as usize, false),
            style,
        )));
    }
    lines.push(RLine::from(RSpan::styled(
        " ↑↓ · Enter go · Esc",
        Style::new().add_modifier(Modifier::DIM),
    )));
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(Color::Black).fg(Color::White)),
        area,
    );
}

/// The list-validation dropdown, anchored just below the cursor cell.
/// Paint the sheet's floating drawings (pictures + charts) over the grid, each
/// clipped to the on-screen slice of its anchor rectangle. Pictures show a
/// labelled box (real pixels are a later enhancement); charts render a compact
/// bar view of their cached data.
fn draw_drawings(app: &mut App, f: &mut Frame, grid: Rect) {
    let drawings = app.sheet().drawings.clone();
    if drawings.is_empty() {
        return;
    }
    let has_picker = app.picker.is_some();
    for d in &drawings {
        // Horizontal extent from the visible columns inside [from.col, to.col].
        let (mut x0, mut x1) = (u16::MAX, 0u16);
        for &(c, x, w) in &app.vis_cols {
            if c >= d.from.1 && c <= d.to.1 {
                x0 = x0.min(x);
                x1 = x1.max(x + w);
            }
        }
        if x0 == u16::MAX {
            continue; // no anchored column on screen
        }
        // Vertical extent from the visible rows inside [from.row, to.row].
        let (mut y0, mut y1) = (u16::MAX, 0u16);
        for (i, &r) in app.vis_rows.iter().enumerate() {
            if r >= d.from.0 && r <= d.to.0 {
                let y = grid.y + i as u16;
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        if y0 == u16::MAX {
            continue;
        }
        // Clip to the grid area.
        let x0 = x0.max(grid.x);
        let x1 = x1.min(grid.x + grid.width);
        let y1 = y1.min(grid.y + grid.height.saturating_sub(1));
        if x1 <= x0 || y1 < y0 {
            continue;
        }
        let rect = Rect::new(x0, y0, x1 - x0, y1 - y0 + 1);
        if rect.width < 3 || rect.height < 1 {
            continue;
        }
        f.render_widget(Clear, rect);
        let border = Style::new().fg(Color::DarkGray);
        match &d.kind {
            gridcore::sheet::DrawingKind::Image { part, name } => {
                // Real pixels when the terminal supports graphics and the media
                // decodes; otherwise a labelled box.
                let mut drawn = false;
                if has_picker {
                    if let Some(proto) = app.image_proto(part, rect.width, rect.height) {
                        f.render_widget(Image::new(proto), rect);
                        drawn = true;
                    }
                }
                if !drawn {
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_style(border)
                        .title(fit(
                            &format!(" \u{1f5bc} {name} "),
                            rect.width as usize,
                            false,
                        ));
                    let inner = block.inner(rect);
                    f.render_widget(block, rect);
                    if inner.height > 0 {
                        let mid = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1);
                        f.render_widget(
                            Paragraph::new(RLine::from(RSpan::styled(
                                center("(picture)", inner.width as usize),
                                Style::new().add_modifier(Modifier::DIM),
                            ))),
                            mid,
                        );
                    }
                }
            }
            gridcore::sheet::DrawingKind::Chart(cd) => {
                let title = if cd.title.is_empty() {
                    format!(" \u{1f4ca} {} chart ", cd.kind)
                } else {
                    format!(" \u{1f4ca} {} ", cd.title)
                };
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(border)
                    .title(fit(&title, rect.width as usize, false));
                let inner = block.inner(rect);
                f.render_widget(block, rect);
                if inner.width > 2 && inner.height > 0 {
                    f.render_widget(
                        Paragraph::new(chart_bar_lines(cd, inner.width, inner.height)),
                        inner,
                    );
                }
            }
        }
    }
}

/// A compact horizontal-bar view of a chart's first series, one category per row.
fn chart_bar_lines(cd: &gridcore::sheet::ChartData, w: u16, h: u16) -> Vec<RLine<'static>> {
    let mut lines: Vec<RLine> = Vec::new();
    let w = w as usize;
    // A header noting the shape of the data when several series are present.
    if cd.series.len() > 1 && h as usize > cd.series.first().map_or(0, |s| s.values.len()) {
        lines.push(RLine::from(RSpan::styled(
            fit(&format!("{} series", cd.series.len()), w, false),
            Style::new().add_modifier(Modifier::DIM),
        )));
    }
    let Some(series) = cd.series.first() else {
        return lines;
    };
    let maxv = series.values.iter().cloned().fold(0f64, f64::max).max(1e-9);
    let label_w = 8usize.min(w / 3);
    let bar_max = w.saturating_sub(label_w + 8).max(1);
    let rows = (h as usize).saturating_sub(lines.len());
    for (i, &v) in series.values.iter().take(rows).enumerate() {
        let label = cd.categories.get(i).map(String::as_str).unwrap_or("");
        let filled = ((v / maxv) * bar_max as f64).round().max(0.0) as usize;
        let bar = "\u{2588}".repeat(filled.min(bar_max));
        let s = format!(
            "{:>lw$} {bar} {}",
            ellipsize(label, label_w),
            num_short(v),
            lw = label_w
        );
        lines.push(RLine::from(RSpan::styled(
            fit(&s, w, false),
            Style::new().fg(Color::Cyan),
        )));
    }
    lines
}

/// Trim a label to `n` columns with an ellipsis.
fn ellipsize(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n <= 1 {
        "\u{2026}".to_string()
    } else {
        let keep: String = s.chars().take(n - 1).collect();
        format!("{keep}\u{2026}")
    }
}

/// Format a number compactly for a chart bar (no trailing zeros, k/M suffixes).
fn num_short(v: f64) -> String {
    let a = v.abs();
    if a >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if a >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

fn draw_dv_picker(app: &App, p: &DvPicker, f: &mut Frame, grid: Rect) {
    if p.values.is_empty() {
        return;
    }
    let widest = p
        .values
        .iter()
        .map(|v| v.chars().count())
        .max()
        .unwrap_or(6)
        .max(8);
    // See `draw_sheet_picker`: `clamp` asserts `min <= max`, and a narrow
    // terminal leaves the available width below the 10 asked for here.
    let w = ((widest + 4) as u16)
        .max(10)
        .min(grid.width.saturating_sub(2));
    let h = (p.values.len() as u16 + 2).min(grid.height.max(3));
    // Anchor under the cursor cell when it's on screen, else the grid's corner.
    let cell_x = app
        .vis_cols
        .iter()
        .find(|&&(col, ..)| col == app.cur.1)
        .map(|&(_, x, _)| x)
        .unwrap_or(grid.x);
    let cell_y = app
        .vis_rows
        .iter()
        .position(|&r| r == app.cur.0)
        .map(|i| grid.y + i as u16)
        .unwrap_or(grid.y);
    let x = cell_x.min(grid.x + grid.width.saturating_sub(w));
    let y = if cell_y + 1 + h <= grid.y + grid.height {
        cell_y + 1
    } else {
        cell_y.saturating_sub(h).max(grid.y)
    };
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);
    let mut lines: Vec<RLine> = vec![RLine::from(RSpan::styled(
        fit(" Choose value", w as usize, false),
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    ))];
    let vis = h.saturating_sub(2) as usize;
    let top = p.sel.saturating_sub(vis.saturating_sub(1));
    for (i, v) in p.values.iter().enumerate().skip(top).take(vis) {
        let style = if i == p.sel {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new()
        };
        lines.push(RLine::from(RSpan::styled(
            fit(&format!(" {v}"), w as usize, false),
            style,
        )));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(Color::Black).fg(Color::White)),
        area,
    );
}

/// The formatting popup (number format / font & fill color).
fn draw_format_picker(p: &FormatPicker, f: &mut Frame, grid: Rect) {
    let (title, rows): (&str, Vec<(String, Option<Rgb>)>) = match p.kind {
        PickKind::NumberFormat => (
            "Number format",
            NUMFMT_OPTIONS
                .iter()
                .map(|(l, _)| (l.to_string(), None))
                .collect(),
        ),
        PickKind::FontColor => (
            "Font color",
            COLOR_OPTIONS
                .iter()
                .map(|(l, c)| (l.to_string(), *c))
                .collect(),
        ),
        PickKind::FillColor => (
            "Fill color",
            COLOR_OPTIONS
                .iter()
                .map(|(l, c)| (l.to_string(), *c))
                .collect(),
        ),
    };
    let w = 30u16.min(grid.width.saturating_sub(2));
    let h = (rows.len() as u16 + 3).min(grid.height);
    if w < 12 || h < 4 {
        return;
    }
    let x = grid.x + (grid.width - w) / 2;
    let y = grid.y + (grid.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);
    let mut lines: Vec<RLine> = vec![RLine::from(RSpan::styled(
        fit(&format!(" {title}"), w as usize, false),
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    ))];
    for (i, (label, color)) in rows.iter().enumerate() {
        let sel = i == p.sel;
        let mut spans: Vec<RSpan> = Vec::new();
        if let Some((r, g, b)) = color {
            spans.push(RSpan::styled(
                "  ██ ",
                Style::new().fg(Color::Rgb(*r, *g, *b)),
            ));
        } else {
            spans.push(RSpan::raw("     "));
        }
        let style = if sel {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new()
        };
        spans.push(RSpan::styled(label.clone(), style));
        lines.push(RLine::from(spans));
    }
    lines.push(RLine::from(RSpan::styled(
        " ↑↓ · Enter apply · Esc",
        Style::new().add_modifier(Modifier::DIM),
    )));
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(Color::Black).fg(Color::White)),
        area,
    );
}

/// The consolidated Format Cells dialog: section tabs across the top, the active
/// section's options below (a ● marks the selection's current value), and a hint.
fn draw_format_dialog(app: &App, d: &FormatDialog, f: &mut Frame, grid: Rect) {
    // The selected cell's effective style, to mark which options are already set.
    let (cr, cc) = app.cur;
    let xf = app
        .sheet()
        .cell(cr, cc)
        .map(|cl| app.pkg.workbook.styles.xf(cl.style))
        .unwrap_or_default();
    let rows: Vec<(String, Option<Rgb>, bool)> = match d.section {
        0 => NUMFMT_OPTIONS
            .iter()
            .map(|(l, code)| (l.to_string(), None, code.map(str::to_string) == xf.code))
            .collect(),
        1 => {
            let mut v = vec![
                ("Bold".to_string(), None, xf.bold),
                ("Italic".to_string(), None, xf.italic),
            ];
            for (l, c) in COLOR_OPTIONS {
                v.push((format!("Text {l}"), *c, xf.color == *c));
            }
            v
        }
        2 => COLOR_OPTIONS
            .iter()
            .map(|(l, c)| (l.to_string(), *c, xf.fill == *c))
            .collect(),
        3 => vec![
            ("Left".to_string(), None, xf.align == Align::Left),
            ("Center".to_string(), None, xf.align == Align::Center),
            ("Right".to_string(), None, xf.align == Align::Right),
        ],
        4 => vec![("Box border".to_string(), None, xf.border)],
        _ => vec![],
    };

    let w = 42u16.min(grid.width.saturating_sub(2));
    // See `draw_sheet_picker`: `clamp` asserts `min <= max`, and a terminal
    // under 9 rows tall would trip it before the guard below returns.
    let h = ((rows.len() as u16) + 6).max(9).min(grid.height.min(22));
    if w < 22 || h < 8 {
        return;
    }
    let x = grid.x + (grid.width - w) / 2;
    let y = grid.y + (grid.height.saturating_sub(h)) / 2;
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);
    let iw = w.saturating_sub(2) as usize; // inside the border

    let mut tabs: Vec<RSpan> = Vec::new();
    for (i, name) in FMT_SECTIONS.iter().enumerate() {
        let st = if i == d.section {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::new().fg(Color::Gray)
        };
        tabs.push(RSpan::styled(format!(" {name} "), st));
    }
    let mut lines: Vec<RLine> = vec![RLine::from(tabs), RLine::from("")];

    let list_h = (h as usize).saturating_sub(5).max(1);
    let start = d
        .sel
        .saturating_sub(list_h - 1)
        .min(rows.len().saturating_sub(list_h));
    for (i, (label, color, active)) in rows.iter().enumerate().skip(start).take(list_h) {
        let mut spans: Vec<RSpan> = vec![RSpan::raw(if *active { "● " } else { "  " }.to_string())];
        if let Some((r, g, b)) = color {
            spans.push(RSpan::styled(
                "██ ",
                Style::new().fg(Color::Rgb(*r, *g, *b)),
            ));
        }
        let base = if i == d.sel {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        spans.push(RSpan::styled(fit(label, iw.saturating_sub(6), false), base));
        lines.push(RLine::from(spans));
    }
    while lines.len() < (h as usize).saturating_sub(3) {
        lines.push(RLine::from(""));
    }
    lines.push(RLine::from(RSpan::styled(
        fit("<-/-> section  up/down  Enter  Esc", iw, false),
        Style::new().fg(Color::DarkGray),
    )));

    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Format Cells (Ctrl+1) "),
        ),
        area,
    );
}

/// The data-model view: tables summary plus relationship/measure panes.
fn draw_model_view(app: &App, pane: usize, sel: usize, f: &mut Frame, grid: Rect) {
    let w = grid.width.min(76);
    let h = grid.height.min(14);
    if w < 20 || h < 5 {
        return;
    }
    let x = grid.x + (grid.width - w) / 2;
    let y = grid.y + (grid.height - h) / 2;
    let area = Rect::new(x, y, w, h);
    f.render_widget(Clear, area);

    let model = app.current_model();
    let tables: Vec<String> = model
        .tables
        .iter()
        .map(|(n, fr)| format!("{n}({})", fr.rows()))
        .collect();
    let mut lines: Vec<RLine> = Vec::new();
    lines.push(RLine::from(RSpan::styled(
        fit(" Data model", w as usize, false),
        Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )));
    lines.push(RLine::from(RSpan::styled(
        fit(
            &format!(
                " Tables: {}",
                if tables.is_empty() {
                    "none — create Excel Tables or import CSV".to_string()
                } else {
                    tables.join("  ")
                }
            ),
            w as usize,
            false,
        ),
        Style::new().fg(Color::Gray),
    )));
    let col_w = (w as usize - 2) / 2;
    let mut hdr: Vec<RSpan> = vec![RSpan::raw(" ")];
    for (i, t) in ["Relationships", "Measures"].iter().enumerate() {
        let style = if i == pane {
            Style::new().add_modifier(Modifier::BOLD).fg(Color::Cyan)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        };
        hdr.push(RSpan::styled(fit(t, col_w, false), style));
    }
    lines.push(RLine::from(hdr));
    let rels: Vec<String> = app
        .model_rels
        .iter()
        .map(|r| format!("{}[{}] → {}[{}]", r.from.0, r.from.1, r.to.0, r.to.1))
        .collect();
    let measures: Vec<String> = app
        .model_measures
        .iter()
        .map(|m| format!("{} = {}", m.name, m.formula))
        .collect();
    for row in 0..(h as usize - 4) {
        let mut spans: Vec<RSpan> = vec![RSpan::raw(" ")];
        for (i, items) in [&rels, &measures].iter().enumerate() {
            let text = items.get(row).cloned().unwrap_or_default();
            let mut style = Style::new();
            if i == pane && row == sel && !text.is_empty() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            spans.push(RSpan::styled(fit(&text, col_w, false), style));
        }
        lines.push(RLine::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::new().bg(Color::Black).fg(Color::White)),
        area,
    );
}

/// Word-wrap `text` to `width` columns (never zero); explicit newlines break.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if line.is_empty() {
                line = word.to_string();
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
            // A single word longer than the width is hard-split.
            while line.chars().count() > width {
                let cut: String = line.chars().take(width).collect();
                out.push(cut);
                line = line.chars().skip(width).collect();
            }
        }
        out.push(line);
    }
    out
}

/// The review-comments side panel: every comment in the workbook, the one on
/// the cursor cell highlighted.
fn draw_comments_panel(app: &App, f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let inner_w = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<RLine> = Vec::new();
    lines.push(RLine::from(RSpan::styled(
        fit(
            &format!(" Comments ({})", app.comments.len()),
            area.width as usize,
            false,
        ),
        Style::new()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    for c in &app.comments {
        let here = c.sheet == app.sheet && (c.row, c.col) == app.cur;
        let sheet_name = app
            .pkg
            .workbook
            .sheets
            .get(c.sheet)
            .map(|s| s.name.as_str())
            .unwrap_or("?");
        let head = format!("{sheet_name}!{} · {}", cell_name(c.row, c.col), c.author);
        let head_style = if here {
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Cyan)
        };
        lines.push(RLine::from(RSpan::styled(head, head_style)));
        for wl in wrap_text(&c.text, inner_w) {
            lines.push(RLine::from(RSpan::raw(format!("  {wl}"))));
        }
        if c.threaded {
            lines.push(RLine::from(RSpan::styled(
                "  (threaded)".to_string(),
                Style::new().add_modifier(Modifier::DIM),
            )));
        }
        lines.push(RLine::from(RSpan::raw(String::new())));
    }
    lines.truncate(area.height as usize);
    f.render_widget(Paragraph::new(lines), area);
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Run one control verb. A circle it made is announced on its own, or
/// after the status the verb set, never appended to an earlier action's
/// leftover status; a verb that says nothing leaves that status alone.
fn run_control(
    app: &mut App,
    verb: &str,
    args: &ctlcore::json::Json,
) -> Result<ctlcore::json::Json, String> {
    let before = app.status.take();
    let result = control::dispatch(app, verb, args);
    if app.status.is_none() && !app.circle_warning_pending {
        app.status = before;
    }
    app.flush_circle_warning();
    result
}

/// Returns true when the app should exit.
fn handle_event(app: &mut App, ev: Event) -> bool {
    let quit = match ev {
        Event::Key(key) => handle_key(app, key),
        Event::Mouse(m) => handle_mouse(app, m),
        Event::Resize(_, _) => false,
        _ => false,
    };
    app.flush_circle_warning();
    quit
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }
    app.status = None;
    app.last_click = None;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // A modal confirmation owns all keys while open — even over the welcome
    // screen or the backstage (Ctrl+Q / Esc-quit can fire from either).
    if app.confirm.is_some() {
        return app.confirm_key(key);
    }
    // Full-screen surfaces own the keyboard entirely.
    if app.start_screen {
        return app.start_screen_key(key);
    }
    if app.backstage.is_some() {
        return app.backstage_key(key);
    }

    if let Some(url) = app.pending_link.take() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                open_url(&url);
                app.status = Some(format!("Opening {url}"));
            }
            _ => app.status = Some("Link cancelled".to_string()),
        }
        return false;
    }

    // --- ribbon ---------------------------------------------------------------
    let overlay_open = app.pivot_edit.is_some()
        || app.model_view.is_some()
        || app.prompt.is_some()
        || app.edit.is_some()
        || app.format_picker.is_some()
        || app.format_dialog.is_some()
        || app.text_dialog.is_some()
        || app.outline_dialog.is_some()
        || app.sheet_picker.is_some()
        || app.dv_picker.is_some();
    // Plain F9 engages the ribbon (docxy parity); Shift/Ctrl+F9 stays recalc.
    if key.code == KeyCode::F(9) && !overlay_open && !shift && !ctrl {
        app.ribbon_focus = if app.ribbon_focus == ribbon::Focus::None {
            ribbon::Focus::Tab(app.ribbon.active_tab())
        } else {
            ribbon::Focus::None
        };
        return false;
    }
    if app.ribbon_focus != ribbon::Focus::None && !overlay_open {
        app.ribbon_key(key.code);
        return false;
    }

    // --- formatting popup -----------------------------------------------------
    if app.format_picker.is_some() {
        app.picker_key(key.code);
        return false;
    }
    if app.format_dialog.is_some() {
        app.format_dialog_key(key.code);
        return false;
    }
    if app.text_dialog.is_some() {
        app.text_dialog_key(key.code);
        return false;
    }
    if app.outline_dialog.is_some() {
        app.outline_dialog_key(key.code);
        return false;
    }

    // --- sheet picker ---------------------------------------------------------
    if app.sheet_picker.is_some() {
        app.sheet_picker_key(key.code);
        return false;
    }

    // --- data-validation dropdown ---------------------------------------------
    if app.dv_picker.is_some() {
        app.dv_picker_key(key.code);
        return false;
    }

    // --- pivot editor ---------------------------------------------------------
    if app.pivot_edit.is_some() {
        app.pivot_editor_key(key.code, shift);
        return false;
    }

    // --- minibuffer prompt ----------------------------------------------------
    if app.prompt.is_some() {
        match key.code {
            KeyCode::Esc => {
                if let Some(Prompt {
                    kind: PromptKind::DocProperty(i),
                    ..
                }) = app.prompt.take()
                {
                    app.reopen_info(i as usize, None);
                }
            }
            KeyCode::Enter => app.commit_prompt(),
            KeyCode::Left => {
                if let Some(p) = &mut app.prompt {
                    p.cursor = p.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(p) = &mut app.prompt {
                    p.cursor = (p.cursor + 1).min(p.text.chars().count());
                }
            }
            KeyCode::Home => {
                if let Some(p) = &mut app.prompt {
                    p.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(p) = &mut app.prompt {
                    p.cursor = p.text.chars().count();
                }
            }
            KeyCode::Backspace => {
                if let Some(p) = &mut app.prompt {
                    if p.cursor > 0 {
                        let idx = char_index(&p.text, p.cursor - 1);
                        p.text.remove(idx);
                        p.cursor -= 1;
                    }
                }
            }
            KeyCode::Delete => {
                if let Some(p) = &mut app.prompt {
                    if p.cursor < p.text.chars().count() {
                        let idx = char_index(&p.text, p.cursor);
                        p.text.remove(idx);
                    }
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                if let Some(p) = &mut app.prompt {
                    let idx = char_index(&p.text, p.cursor);
                    p.text.insert(idx, ch);
                    p.cursor += 1;
                }
            }
            _ => {}
        }
        return false;
    }

    // --- model view -------------------------------------------------------------
    if app.model_view.is_some() {
        app.model_view_key(key.code);
        return false;
    }

    // Ctrl+Shift+U expands the formula bar, editing or not. A legacy
    // terminal reports it as Ctrl+U (no SHIFT) and Ctrl+U is unbound, so
    // either toggles; a later Ctrl+U (underline) must keep Ctrl+Shift+U apart.
    if ctrl && matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U')) {
        app.fx_expanded = !app.fx_expanded;
        return false;
    }

    // --- edit mode -----------------------------------------------------------
    if app.edit.is_some() {
        let replace = app.edit.as_ref().is_some_and(|e| e.replace);
        // A caret move keeps an AutoComplete proposal's text and drops its
        // marker; the keys that commit take the proposal in `commit_edit`.
        // Home/End move the caret in every mode; Left/Right only outside
        // type-over, where they commit instead.
        let caret_move = match key.code {
            KeyCode::Home | KeyCode::End => true,
            KeyCode::Left | KeyCode::Right => !replace,
            _ => false,
        };
        if caret_move {
            if let Some(e) = &mut app.edit {
                e.proposal = None;
            }
        }
        match key.code {
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Enter => {
                if app.commit_edit() {
                    app.enter_move(shift);
                }
            }
            KeyCode::Tab => {
                if app.commit_edit() {
                    app.move_cur(0, if shift { -1 } else { 1 }, false);
                }
            }
            KeyCode::BackTab => {
                if app.commit_edit() {
                    app.move_cur(0, -1, false);
                }
            }
            // In type-over mode, arrows commit and move (Excel behavior).
            KeyCode::Up | KeyCode::Down if replace => {
                if app.commit_edit() {
                    app.move_cur(if key.code == KeyCode::Up { -1 } else { 1 }, 0, false);
                }
            }
            KeyCode::Left | KeyCode::Right if replace => {
                if app.commit_edit() {
                    app.move_cur(0, if key.code == KeyCode::Left { -1 } else { 1 }, false);
                }
            }
            KeyCode::Left => {
                if let Some(e) = &mut app.edit {
                    e.cursor = e.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(e) = &mut app.edit {
                    e.cursor = (e.cursor + 1).min(e.text.chars().count());
                }
            }
            KeyCode::Home => {
                if let Some(e) = &mut app.edit {
                    e.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(e) = &mut app.edit {
                    e.cursor = e.text.chars().count();
                }
            }
            KeyCode::Backspace => {
                if app.drop_proposal() {
                    return false;
                }
                if let Some(e) = &mut app.edit {
                    if e.cursor > 0 {
                        let idx = char_index(&e.text, e.cursor - 1);
                        e.text.remove(idx);
                        e.cursor -= 1;
                    }
                }
            }
            KeyCode::Delete => {
                if app.drop_proposal() {
                    return false;
                }
                if let Some(e) = &mut app.edit {
                    if e.cursor < e.text.chars().count() {
                        let idx = char_index(&e.text, e.cursor);
                        e.text.remove(idx);
                    }
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                // A typed character replaces a proposal's suffix, then the
                // match runs again on the longer text.
                app.drop_proposal();
                if let Some(e) = &mut app.edit {
                    let idx = char_index(&e.text, e.cursor);
                    e.text.insert(idx, ch);
                    e.cursor += 1;
                }
                app.propose();
            }
            _ => {}
        }
        return false;
    }

    // --- vim mode (normal / visual / command-line) ------------------------------
    if app.vim.is_some() {
        return app.vim_key(key.code, ctrl, shift);
    }

    // --- navigation / commands --------------------------------------------------
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') if ctrl => {
            app.request_exit();
            return false;
        }
        KeyCode::Char('s') | KeyCode::Char('S') if ctrl => app.save(),
        KeyCode::Char('n') | KeyCode::Char('N') if ctrl => app.request_discard(Next::New),
        KeyCode::Char('z') | KeyCode::Char('Z') if ctrl => app.undo(),
        KeyCode::Char('y') | KeyCode::Char('Y') if ctrl => app.redo(),
        KeyCode::Char('c') | KeyCode::Char('C') if ctrl => app.copy(false),
        KeyCode::Char('x') | KeyCode::Char('X') if ctrl => app.copy(true),
        // Ctrl+Alt+V, when the terminal reports it: Paste Special (#669).
        KeyCode::Char('v') | KeyCode::Char('V') if ctrl && alt => app.open_paste_special(),
        KeyCode::Char('v') | KeyCode::Char('V') if ctrl => app.paste(),
        KeyCode::Char('d') | KeyCode::Char('D') if ctrl => app.fill(FillDir::Down),
        KeyCode::Char('r') | KeyCode::Char('R') if ctrl => app.fill(FillDir::Right),
        KeyCode::Char('f') | KeyCode::Char('F') if alt => app.open_backstage(),
        KeyCode::Char('o') | KeyCode::Char('O') if ctrl => {
            app.open_backstage();
            if let Some(bs) = &mut app.backstage {
                bs.pane = backstage::Pane::Browser;
            }
        }
        KeyCode::Char('b') | KeyCode::Char('B') if ctrl => app.toggle_bold(),
        KeyCode::Char('1') if ctrl => app.open_format_dialog(),
        KeyCode::Char('i') | KeyCode::Char('I') if ctrl => app.toggle_italic(),
        KeyCode::Char('`') if ctrl => app.toggle_formula_view(),
        KeyCode::Char('f') | KeyCode::Char('F') if ctrl => app.open_prompt(PromptKind::Find),
        KeyCode::Char('h') | KeyCode::Char('H') if ctrl => app.open_prompt(PromptKind::ReplaceFind),
        KeyCode::Char('g') | KeyCode::Char('G') if ctrl => app.open_prompt(PromptKind::GoTo),
        KeyCode::Char('t') | KeyCode::Char('T') if ctrl => app.open_prompt(PromptKind::AddSheet),
        KeyCode::F(3) => {
            if let Some(q) = app.last_find.clone() {
                app.find_next(&q);
            } else {
                app.open_prompt(PromptKind::Find);
            }
        }
        KeyCode::F(4) => app.open_sheet_picker(),
        // Plain F9 opens the ribbon (handled earlier); Shift+F9 forces recalc.
        KeyCode::F(9) => app.recalc_and_refresh(),
        KeyCode::Char('p') | KeyCode::Char('P') if ctrl => app.open_pivot_editor(),
        KeyCode::Char('m') | KeyCode::Char('M') if ctrl => app.open_model_view(),
        KeyCode::F(12) => app.open_prompt(PromptKind::SaveAs),
        KeyCode::F(2) if shift => app.open_prompt(PromptKind::RenameSheet),
        KeyCode::F(5) if shift => app.row_op(false),
        KeyCode::F(5) => app.row_op(true),
        KeyCode::F(6) if shift => app.col_op(false),
        KeyCode::F(6) => app.col_op(true),
        KeyCode::Delete if shift => {
            app.request_delete_sheet();
            return false;
        }
        KeyCode::Char('a') | KeyCode::Char('A') if ctrl => {
            let (rows, cols) = app.sheet().used_size();
            if rows > 0 {
                app.anchor = Some((0, 0));
                app.cur = (rows - 1, cols.max(1) - 1);
            }
        }
        // Alt-↓ on a validated cell opens its dropdown (Excel parity).
        KeyCode::Down if alt => app.open_dv_dropdown(),
        // Excel's Group / Ungroup.
        KeyCode::Right if alt && shift => app.group_outline(false),
        KeyCode::Left if alt && shift => app.group_outline(true),
        KeyCode::Up if ctrl => app.jump(-1, 0, shift),
        KeyCode::Down if ctrl => app.jump(1, 0, shift),
        KeyCode::Left if ctrl => app.jump(0, -1, shift),
        KeyCode::Right if ctrl => app.jump(0, 1, shift),
        KeyCode::Up => app.move_cur(-1, 0, shift),
        KeyCode::Down => app.move_cur(1, 0, shift),
        KeyCode::Left => app.move_cur(0, -1, shift),
        KeyCode::Right => app.move_cur(0, 1, shift),
        KeyCode::PageUp if ctrl => app.switch_sheet(-1),
        KeyCode::PageDown if ctrl => app.switch_sheet(1),
        KeyCode::PageUp => {
            let page = app.grid_area.height.max(1) as i64;
            app.move_cur(-page, 0, shift);
        }
        KeyCode::PageDown => {
            let page = app.grid_area.height.max(1) as i64;
            app.move_cur(page, 0, shift);
        }
        KeyCode::Home if ctrl => {
            app.cur = (0, 0);
            app.anchor = None;
            app.ensure_visible();
        }
        KeyCode::End if ctrl => {
            let (rows, cols) = app.sheet().used_size();
            app.cur = (rows.max(1) - 1, cols.max(1) - 1);
            app.anchor = None;
            app.ensure_visible();
        }
        KeyCode::Home => {
            app.cur.1 = 0;
            app.anchor = None;
            app.ensure_visible();
        }
        KeyCode::End => {
            // Last used column in this row.
            let row = app.cur.0;
            let last = app
                .sheet()
                .cells
                .range((row, 0)..=(row, u32::MAX))
                .map(|(&(_, c), _)| c)
                .next_back()
                .unwrap_or(0);
            app.cur.1 = last;
            app.anchor = None;
            app.ensure_visible();
        }
        KeyCode::Enter => app.enter_move(shift),
        KeyCode::Tab => app.move_cur(0, 1, false),
        KeyCode::BackTab => app.move_cur(0, -1, false),
        KeyCode::Delete => app.clear_selection(),
        KeyCode::Backspace => {
            // Excel: Backspace clears the cell and starts empty editing.
            app.start_edit(None);
            if let Some(e) = &mut app.edit {
                e.text.clear();
                e.cursor = 0;
                e.replace = true;
            }
        }
        KeyCode::F(2) => app.start_edit(None),
        KeyCode::F(7) | KeyCode::F(8) => {
            let col = app.cur.1;
            let w = app.sheet().col_width(col);
            let nw = if key.code == KeyCode::F(7) {
                (w - 1.0).max(2.0)
            } else {
                (w + 1.0).min(60.0)
            };
            app.pkg.workbook.sheets[app.sheet].set_col_width(col, nw);
            app.modified = true;
            app.status = Some(format!("Column {} width: {nw:.0}", col_name(col)));
        }
        KeyCode::Esc => {
            app.anchor = None;
        }
        KeyCode::Char(ch) if !ctrl => {
            app.start_edit(Some(ch));
            app.propose();
        }
        _ => {}
    }
    false
}

fn char_index(s: &str, char_pos: usize) -> usize {
    s.char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn handle_mouse(app: &mut App, m: MouseEvent) -> bool {
    // A modal confirmation owns the mouse while open — even over the welcome
    // screen or the backstage.
    if app.confirm.is_some() {
        if m.kind == MouseEventKind::Down(MouseButton::Left) {
            return app.confirm_mouse(m.column, m.row);
        }
        return false;
    }
    // An outline dialog is modal: a click under it (a sheet tab, a ribbon
    // command) must not change what its OK acts on.
    if app.outline_dialog.is_some() {
        return false;
    }
    // The welcome screen owns the whole terminal; handle its clicks here so
    // nothing leaks to the hidden workbook behind it. Hovering highlights an
    // item, clicking activates it.
    if app.start_screen {
        let ev = app.start.mouse(m.column, m.row);
        if m.kind == MouseEventKind::Down(MouseButton::Left) {
            if let backstage::StartEvent::Choose(i) = ev {
                return app.start_choose(i);
            }
        }
        return false;
    }
    // The File backstage is a full-screen surface: it owns the mouse while open,
    // so clicks never leak through to the grid underneath.
    if app.backstage.is_some() {
        return match m.kind {
            MouseEventKind::Down(MouseButton::Left) => app.bs_mouse(m.column, m.row),
            MouseEventKind::ScrollDown => {
                if let Some(b) = app.backstage.as_mut() {
                    b.scroll_preview(3);
                }
                false
            }
            MouseEventKind::ScrollUp => {
                if let Some(b) = app.backstage.as_mut() {
                    b.scroll_preview(-3);
                }
                false
            }
            _ => false,
        };
    }
    match m.kind {
        MouseEventKind::ScrollUp => {
            app.top = app.top.saturating_sub(3);
        }
        MouseEventKind::ScrollDown => {
            app.top = (app.top + 3).min(MAX_ROWS - 1);
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
            let drag = matches!(m.kind, MouseEventKind::Drag(_));
            // The ribbon occupies the top rows.
            if !drag && m.row < app.ribbon_rows {
                let expanded = app.ribbon_focus != ribbon::Focus::None;
                match app.ribbon.hit(m.column, m.row, expanded) {
                    ribbon::Hit::Tab(t) if app.ribbon.tab_is_file(t) => {
                        app.open_backstage();
                    }
                    ribbon::Hit::Tab(t) => {
                        app.ribbon.set_active(t);
                        app.ribbon_focus = ribbon::Focus::Tab(t);
                    }
                    ribbon::Hit::Button(act) => {
                        app.ribbon_focus = ribbon::Focus::None;
                        app.ribbon_act(act);
                    }
                    ribbon::Hit::Outside => {}
                }
                return false;
            }
            // Sheet tabs live on the line right below the grid.
            let tabs_y = app.grid_area.y + app.grid_area.height;
            if !drag && m.row == tabs_y {
                for &(i, x1, x2) in &app.tab_spans {
                    if m.column >= x1 && m.column < x2 {
                        if i == usize::MAX {
                            app.open_sheet_picker(); // the ⊞ marker
                        } else if i != app.sheet {
                            app.goto_sheet(i);
                        }
                        return false;
                    }
                }
                return false;
            }
            // Outline level and +/- buttons.
            if !drag && app.outline_click(m.column, m.row) {
                return false;
            }
            // Grid?
            let g = app.grid_area;
            if m.row < g.y || m.row >= g.y + g.height || m.column < g.x + app.gutter_w {
                return false;
            }
            // Map the screen line to a sheet row via the freeze-aware table.
            let vy = (m.row - g.y) as usize;
            let row = app
                .vis_rows
                .get(vy)
                .copied()
                .unwrap_or_else(|| app.top + vy as u32)
                .min(MAX_ROWS - 1);
            let mut col = None;
            for &(cidx, x, w) in &app.vis_cols {
                if m.column >= x && m.column < x + w {
                    col = Some(cidx.min(MAX_COLS - 1));
                    break;
                }
            }
            let Some(col) = col else { return false };
            if app.edit.is_some() {
                // Clicking outside while editing commits first.
                if !app.commit_edit() {
                    return false;
                }
            }
            if drag {
                if app.anchor.is_none() {
                    app.anchor = Some(app.cur);
                }
                app.last_click = None;
                app.cur = (row, col);
            } else {
                app.click_cell(row, col, Instant::now());
            }
        }
        _ => {}
    }
    false
}

/// The outline level buttons `1 2 …` (one per level, plus one), in a field
/// `width` wide.
fn level_buttons(levels: u8, width: usize) -> String {
    let mut s: String = if levels == 0 {
        String::new()
    } else {
        (1..=levels + 1).map(|n| char::from(b'0' + n)).collect()
    };
    s.truncate(width);
    format!("{s:<width$}")
}

/// One row's outline gutter: a column per level holding a bar while the row
/// is in a group at that level, or the `+`/`-` of the group it summarizes
/// (only on the row's first screen line), then a blank.
fn row_outline_cells(
    sheet: &Sheet,
    groups: &[outline::Group],
    row: u32,
    levels: u8,
    first_line: bool,
) -> String {
    let lvl = sheet.row_outline(row);
    let mut s = String::new();
    for l in 1..=levels {
        let sum = groups
            .iter()
            .find(|g| g.level == l && g.summary == Some(row));
        s.push(match sum {
            Some(g) if first_line => {
                if g.collapsed {
                    '+'
                } else {
                    '-'
                }
            }
            _ if lvl >= l => '│',
            _ => ' ',
        });
    }
    s.push(' ');
    s
}

/// Only http/https links may be opened, and only via a direct process exec (no
/// shell), so a hyperlink can never run an arbitrary command.
fn safe_url(url: &str) -> bool {
    let u = url.trim();
    if u.len() > 2048 || u.chars().any(|c| c.is_control()) {
        return false;
    }
    let lower = u.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn open_url(url: &str) {
    if !safe_url(url) {
        return;
    }
    #[cfg(windows)]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

// ---------------------------------------------------------------------------
// Terminal shell
// ---------------------------------------------------------------------------

/// The editor's command-line switches.
struct TuiFlags {
    /// `--vim`: modal navigation.
    vim: bool,
    /// `--read-only`: the input file, opened read-only (#882).
    read_only: Option<String>,
}

fn run_tui(
    pkg: SheetPackage,
    path: &str,
    import: Option<(String, Option<SourceFormat>)>,
    template: Option<String>,
    welcome: bool,
    wizard: Option<String>,
    flags: TuiFlags,
) -> io::Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(pkg, path);
    app.xlstart = xlstart_dir();
    let format = import.as_ref().and_then(|(_, f)| *f);
    app.import_source = import.map(|(s, _)| s);
    app.note_import(format);
    if let Some(template) = template {
        app.note_template(&template);
    }
    app.load_view_prefs();
    if flags.vim {
        app.vim = Some(VimState {
            mode: VimMode::Normal,
            pending: '\0',
            cmdline: None,
        });
    }
    app.start_screen = welcome;
    // With no file to open, the startup folders' workbooks open instead of
    // the welcome screen.
    if welcome {
        let alt = app.alt_startup.as_deref().map(Path::new);
        let dirs: Vec<&Path> = app.xlstart.as_deref().into_iter().chain(alt).collect();
        let files = startup_workbooks(&dirs);
        app.open_startup_workbooks(&files);
    }
    if let Some(source) = flags.read_only {
        app.set_read_only(&source);
    }
    if let Some(text_file) = wizard {
        app.open_startup_wizard(&text_file);
    }
    // Detect the terminal's graphics capability (kitty/iTerm2/Sixel); fall back to
    // a half-block renderer so embedded pictures still show something.
    app.picker =
        Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16))));
    let mut last_title = String::new();

    // Bring up the agent control surface. Best-effort: if the config directory or
    // the loopback bind fails, the editor runs exactly as before, just without a
    // control channel. `ctl_server` is held for the whole session — its Drop
    // removes the discovery file.
    let ctl_instance = ctlcore::instance_name("xlsxy");
    let (ctl_server, ctl_rx) = match ctlcore::config_ctl_dir("xlsxy") {
        Some(dir) => match ctlcore::serve(&dir, &ctl_instance) {
            Ok((srv, rx)) => (Some(srv), Some(rx)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    // One message stream drives the loop: terminal input (read on its own thread
    // so the loop can block cheaply) and control requests. The main thread stays
    // the sole owner of the workbook, so applying a request needs no locking.
    enum Msg {
        Term(Event),
        Ctl(ctlcore::Request),
    }
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("xlsxy-input".into())
            .spawn(move || {
                while let Ok(ev) = event::read() {
                    if tx.send(Msg::Term(ev)).is_err() {
                        break;
                    }
                }
            });
    }
    if let Some(ctl_rx) = ctl_rx {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("xlsxy-ctl".into())
            .spawn(move || {
                for req in ctl_rx {
                    if tx.send(Msg::Ctl(req)).is_err() {
                        break;
                    }
                }
            });
    }
    drop(tx); // only the reader/forwarder threads keep the channel open now

    let result = loop {
        // Reflect the file + unsaved state in the terminal window title.
        let title = window_title("xlsxy", &app.path, app.modified, app.bound_read_only());
        if title != last_title {
            let _ = execute!(io::stdout(), SetTitle(&title));
            last_title = title;
        }
        if let Err(e) = terminal.draw(|f| draw(&mut app, f)) {
            break Err(e);
        }

        // Block until something arrives (no busy polling), then drain anything
        // already queued so a burst — fast scrolling, or a run of agent edits —
        // collapses into a single repaint.
        let mut next = match rx.recv() {
            Ok(m) => Some(m),
            Err(_) => break Ok(()), // every input source is gone
        };
        let mut quit = false;
        while let Some(msg) = next.take() {
            match msg {
                Msg::Term(ev) => {
                    if handle_event(&mut app, ev) {
                        quit = true;
                    }
                }
                Msg::Ctl(req) => match run_control(&mut app, &req.verb, &req.args) {
                    Ok(result) => req.reply_ok(result),
                    Err(e) => req.reply_err(e),
                },
            }
            if quit {
                break;
            }
            next = rx.try_recv().ok();
        }
        if quit {
            break Ok(());
        }
    };
    drop(ctl_server); // remove the discovery file

    app.save_view_prefs();
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcore::edit::parse_input;
    use gridcore::xlsx::{load_xlsx, save_xlsx};

    /// `Backup of <stem>.xlk` lives beside the file; the stem keeps any
    /// dots in the name.
    #[test]
    fn backup_path_names_the_xlk_beside_the_file() {
        assert_eq!(
            backup_path(Path::new("d/book.xlsx")),
            Path::new("d/Backup of book.xlk")
        );
        assert_eq!(
            backup_path(Path::new("d/my.book.xlsm")),
            Path::new("d/Backup of my.book.xlk")
        );
    }

    /// #604: a workbook opens on the sheet it was saved on, and saving
    /// records the sheet the user is on.
    #[test]
    fn a_workbook_opens_on_its_active_sheet_and_saves_the_current_one() {
        let dir = std::env::temp_dir().join(format!("xlsxy-active-tab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("active.xlsx");
        let mut pkg = new_xlsx();
        pkg.add_sheet("Two");
        pkg.workbook.active_tab = 1;
        std::fs::write(&path, save_xlsx(&pkg)).unwrap();
        let loaded = load_xlsx(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(App::new(loaded, path.to_str().unwrap()).sheet, 1);
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(path.to_str().unwrap());
        assert_eq!(app.sheet, 1);
        app.sheet = 0;
        app.save();
        let saved = load_xlsx(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.workbook.active_tab, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #615: the *Save as type* list is Excel's, in Excel's order.
    #[test]
    fn save_as_offers_excels_types_in_excels_order() {
        let labels: Vec<&str> = SAVE_TYPES.iter().map(|t| t.label).collect();
        assert_eq!(
            labels[..8],
            [
                "Excel Workbook",
                "Excel Macro-Enabled Workbook",
                "Excel Binary Workbook",
                "Excel 97-2003 Workbook",
                "CSV UTF-8 (Comma delimited)",
                "XML Data",
                "Single File Web Page",
                "Web Page",
            ]
        );
        let at = |l: &str| labels.iter().position(|x| *x == l).unwrap();
        assert!(at("Text (Tab delimited)") < at("Unicode Text"));
        assert!(at("Unicode Text") < at("CSV (Comma delimited)"));
        assert!(at("CSV (Comma delimited)") < at("Formatted Text (Space delimited)"));
        assert_eq!(labels.last(), Some(&"OpenDocument Spreadsheet"));
        assert_eq!(type_for_path("a/out.CSV"), Some(4));
        assert_eq!(
            type_for_path("x.txt").map(|t| SAVE_TYPES[t].label),
            Some("Text (Tab delimited)")
        );
        assert_eq!(
            type_for_path("x.html").map(|t| SAVE_TYPES[t].label),
            Some("Web Page")
        );
        assert_eq!(type_for_path("x.dat"), None);
    }

    fn two_sheet_app(dir: &Path) -> App {
        use gridcore::sheet::Cell;
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("first sheet"));
        let data = pkg.add_sheet("Data");
        let sh = &mut pkg.workbook.sheets[data];
        sh.set_cell(0, 0, Cell::text("Name"));
        sh.set_cell(0, 1, Cell::text("Note"));
        sh.set_cell(1, 0, Cell::text("Z\u{fc}rich"));
        sh.set_cell(1, 1, Cell::text("line1\nline2"));
        let mut app = App::new(pkg, dir.join("book.xlsx").to_str().unwrap());
        app.os_clip = None;
        app.sheet = data;
        app
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xlsxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// #615: Save As CSV UTF-8 from the list writes only the active sheet,
    /// keeps every sheet open, says so, and closing still asks to save.
    #[test]
    fn save_as_csv_utf8_writes_the_active_sheet_and_keeps_the_rest() {
        let dir = tmp("save-csv");
        let mut app = two_sheet_app(&dir);
        app.open_backstage();
        let b = app.backstage.as_mut().unwrap();
        b.begin_save_as("book.xlsx".into(), None);
        b.pick_type(4);
        assert_eq!(b.name_input, "book.csv");
        let name = b.name_input.clone();
        app.commit_save_as(dir.clone(), name);
        let out = dir.join("book.csv");
        let mut want = b"\xEF\xBB\xBF".to_vec();
        want.extend_from_slice("Name,Note\r\nZ\u{fc}rich,\"line1\nline2\"\r\n".as_bytes());
        assert_eq!(std::fs::read(&out).unwrap(), want);
        assert_eq!(Path::new(&app.path), out);
        assert_eq!(app.pkg.workbook.sheets.len(), 2);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("possible data loss")
        );
        assert!(app.status.as_deref().unwrap().contains("multiple sheets"));
        assert!(app.modified, "closing after a CSV save still asks");
        app.request_exit();
        assert!(app.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        app.confirm = None;
        // Ctrl+S keeps writing CSV to the CSV path.
        std::fs::remove_file(&out).unwrap();
        app.save();
        assert_eq!(std::fs::read(&out).unwrap(), want);
        // Saving As a workbook again writes the package and is clean.
        app.commit_save_as(dir.clone(), "again.xlsx".into());
        assert!(
            std::fs::read(dir.join("again.xlsx"))
                .unwrap()
                .starts_with(b"PK")
        );
        assert!(!app.modified);
        assert_eq!(app.text_type, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #615's repro: F12, `out.csv`, Enter no longer writes a ZIP.
    #[test]
    fn save_as_prompt_out_csv_is_a_csv_not_a_zip() {
        let dir = tmp("f12-csv");
        let mut app = two_sheet_app(&dir);
        app.request_save_as(dir.join("out.csv").to_string_lossy().into_owned());
        let bytes = std::fs::read(dir.join("out.csv")).unwrap();
        assert!(!bytes.starts_with(b"PK"));
        assert!(bytes.starts_with(b"\xEF\xBB\xBFName,Note\r\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #615: each text type is written as Excel writes it.
    #[test]
    fn save_as_text_types_write_excels_bytes() {
        let dir = tmp("text-types");
        let at = |l: &str| SAVE_TYPES.iter().position(|t| t.label == l).unwrap();
        let cases: [(&str, &str, &[u8]); 4] = [
            (
                "CSV (Comma delimited)",
                "c.csv",
                b"Name,Note\r\nZ\xFCrich,\"line1\nline2\"\r\n",
            ),
            (
                "Text (Tab delimited)",
                "t.txt",
                b"Name\tNote\r\nZ\xFCrich\t\"line1\nline2\"\r\n",
            ),
            (
                "Unicode Text",
                "u.txt",
                b"\xFF\xFEN\x00a\x00m\x00e\x00\t\x00",
            ),
            (
                "Formatted Text (Space delimited)",
                "f.prn",
                b"Name    Note\r\n",
            ),
        ];
        for (label, file, prefix) in cases {
            let mut app = two_sheet_app(&dir);
            app.save_as_type(dir.join(file).to_string_lossy().into_owned(), at(label));
            let bytes = std::fs::read(dir.join(file)).unwrap();
            assert!(bytes.starts_with(prefix), "{label}: {bytes:?}");
        }
        // Web Page: the .htm and its _files folder.
        let mut app = two_sheet_app(&dir);
        app.save_as_type(
            dir.join("page.htm").to_string_lossy().into_owned(),
            at("Web Page"),
        );
        let htm = std::fs::read_to_string(dir.join("page.htm")).unwrap();
        assert!(htm.contains("page_files/filelist.xml"), "{htm}");
        assert!(htm.contains("Z\u{fc}rich"));
        assert!(dir.join("page_files").join("filelist.xml").is_file());
        assert!(dir.join("page_files").join("stylesheet.css").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #615: `.html` is Web Page's other spelling: Save As and Ctrl+S write
    /// the page, never the workbook package.
    #[test]
    fn save_as_html_writes_a_web_page_and_ctrl_s_keeps_it() {
        let dir = tmp("html");
        let mut app = two_sheet_app(&dir);
        let page = dir.join("page.html");
        app.request_save_as(page.to_string_lossy().into_owned());
        let bytes = std::fs::read(&page).unwrap();
        assert!(!bytes.starts_with(b"PK"));
        let htm = String::from_utf8(bytes).unwrap();
        assert!(htm.contains("page_files/filelist.xml"), "{htm}");
        let list = std::fs::read_to_string(dir.join("page_files").join("filelist.xml")).unwrap();
        assert!(list.contains("HRef=\"../page.html\""), "{list}");
        std::fs::remove_file(&page).unwrap();
        app.save();
        assert!(!std::fs::read(&page).unwrap().starts_with(b"PK"));
        // Picked from the list, a typed .html name is kept as it is.
        app.open_backstage();
        let web = SAVE_TYPES
            .iter()
            .position(|t| t.label == "Web Page")
            .unwrap();
        let b = app.backstage.as_mut().unwrap();
        b.begin_save_as("other.xlsx".into(), None);
        b.pick_type(web);
        b.name_input = "other.html".into();
        app.commit_save_as(dir.clone(), "other.html".into());
        assert!(dir.join("other.html").is_file());
        assert!(!dir.join("other.html.htm").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #600: File › Info edits a property through the minibuffer and comes
    /// back to Info on the same row; Esc cancels the same way.
    #[test]
    fn file_info_edits_properties_through_the_prompt() {
        let mut app = App::new(new_xlsx(), "info.xlsx");
        app.os_clip = None;
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let type_text = |app: &mut App, s: &str| {
            for c in s.chars() {
                handle_key(app, key(KeyCode::Char(c)));
            }
        };
        let at_info = |app: &App, row: usize| {
            let b = app.backstage.as_ref().expect("back on the backstage");
            assert_eq!(
                (b.item, b.pane, b.info_sel),
                (backstage::Item::Info, backstage::Pane::Info, row)
            );
        };
        app.open_backstage();
        while app.backstage.as_ref().unwrap().item != backstage::Item::Info {
            app.backstage_key(key(KeyCode::Down));
        }
        let labels: Vec<String> = app.info_fields().into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            labels,
            [
                "Title",
                "Tags",
                "Categories",
                "Subject",
                "Comments",
                "Company",
                "Manager",
                "Hyperlink base"
            ]
        );
        assert!(app.info_custom_row());

        // Enter focuses the rows; Enter on Title opens its prompt.
        app.backstage_key(key(KeyCode::Enter));
        app.backstage_key(key(KeyCode::Enter));
        assert!(app.backstage.is_none());
        let p = app.prompt.as_ref().unwrap();
        assert!(p.kind == PromptKind::DocProperty(0));
        assert_eq!((p.label, p.text.as_str()), ("Title: ", ""));
        type_text(&mut app, "Budget");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.prompt.is_none());
        assert!(app.modified);
        assert_eq!(app.pkg.doc_properties().title.as_deref(), Some("Budget"));
        assert_eq!(app.info_fields()[0].1, "Budget");
        at_info(&app, 0);

        // The prompt opens on the current value; Esc keeps it.
        app.modified = false;
        app.backstage_key(key(KeyCode::Enter));
        let p = app.prompt.as_ref().unwrap();
        assert_eq!((p.text.as_str(), p.cursor), ("Budget", 6));
        handle_key(&mut app, key(KeyCode::Backspace));
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.prompt.is_none());
        assert!(!app.modified);
        assert_eq!(app.pkg.doc_properties().title.as_deref(), Some("Budget"));
        at_info(&app, 0);

        // Company, then the custom-property row after the eight fields.
        for _ in 0..5 {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        type_text(&mut app, "Acme & Co");
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            app.pkg.doc_properties().company.as_deref(),
            Some("Acme & Co")
        );
        at_info(&app, 5);
        for _ in 0..10 {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        let p = app.prompt.as_ref().unwrap();
        assert!(p.kind == PromptKind::DocProperty(8));
        assert_eq!(p.label, CUSTOM_PROPERTY_PROMPT);
        type_text(&mut app, "Count = 42");
        handle_key(&mut app, key(KeyCode::Enter));
        at_info(&app, 8);
        assert_eq!(
            app.pkg.doc_properties().custom,
            [CustomProperty {
                name: "Count".into(),
                value: CustomValue::Number(42.0),
            }]
        );
        let info: Vec<String> = app.info_lines().iter().map(|l| l.to_string()).collect();
        assert!(
            info.iter()
                .any(|l| l.contains("Custom") && l.contains("Count = 42")),
            "{info:?}"
        );
        // An empty value removes it (the name in any case).
        app.backstage_key(key(KeyCode::Enter));
        type_text(&mut app, "count =");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.pkg.doc_properties().custom.is_empty());
        // Without `=`, nothing changes and the status says why.
        app.backstage_key(key(KeyCode::Enter));
        type_text(&mut app, "oops");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.pkg.doc_properties().custom.is_empty());
        assert_eq!(
            app.backstage.as_ref().unwrap().info_message.as_deref(),
            Some("Custom property: type Name = value")
        );
        at_info(&app, 8);

        // A field set to empty is removed.
        while app.backstage.as_ref().unwrap().info_sel > 0 {
            app.backstage_key(key(KeyCode::Up));
        }
        app.backstage_key(key(KeyCode::Enter));
        for _ in 0..6 {
            handle_key(&mut app, key(KeyCode::Backspace));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.pkg.doc_properties().title, None);
    }

    /// #600 r1 M2: with eight custom properties on 80x24, the Info page
    /// scrolls so the selected `Custom property…` row is inside its box.
    #[test]
    fn file_info_keeps_the_selected_row_on_screen() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut app = App::new(new_xlsx(), "info.xlsx");
        app.os_clip = None;
        let mut p = app.pkg.doc_properties();
        p.custom = (1..=8)
            .map(|i| CustomProperty {
                name: format!("Prop{i}"),
                value: CustomValue::Number(f64::from(i)),
            })
            .collect();
        app.pkg.set_doc_properties(&p).unwrap();
        app.open_backstage();
        app.backstage
            .as_mut()
            .unwrap()
            .focus_info(INFO_FIELDS.len());
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let buf = term.backend().buffer();
        let row = |y: u16| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let at = (2..23)
            .find(|&y| row(y).contains("Custom property…"))
            .unwrap_or_else(|| panic!("selected row not inside the box"));
        assert_eq!(buf[(20, at)].bg, Color::Green);
        assert!(row(23).contains('└'), "{}", row(23));
    }

    /// #600 r2: the outcome of an Info edit is on screen, on the Info page
    /// (the status bar is hidden under the backstage) — including an edit
    /// an unreadable core.xml can't take.
    #[test]
    fn file_info_says_how_an_edit_went() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let screen = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
            term.draw(|f| draw(app, f)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let edit_title = |app: &mut App, text: &str| {
            app.open_backstage();
            app.backstage.as_mut().unwrap().focus_info(0);
            app.backstage_key(key(KeyCode::Enter));
            for c in text.chars() {
                handle_key(app, key(KeyCode::Char(c)));
            }
            handle_key(app, key(KeyCode::Enter));
        };

        let mut app = App::new(new_xlsx(), "ok.xlsx");
        app.os_clip = None;
        edit_title(&mut app, "T");
        assert!(screen(&mut app).contains("Title updated"));

        // A UTF-16 core.xml can't be patched: the edit is refused, visibly.
        let mut pkg = new_xlsx();
        pkg.stamp_save("2026-10-01T12:00:00Z", "me");
        let core = String::from_utf8(pkg.part("docProps/core.xml").unwrap().to_vec()).unwrap();
        let mut utf16 = vec![0xFF, 0xFE];
        for u in core.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        pkg.set_part("docProps/core.xml", utf16.clone());
        let mut app = App::new(pkg, "bad.xlsx");
        app.os_clip = None;
        edit_title(&mut app, "T");
        let text = screen(&mut app);
        assert!(
            text.contains("document properties can't be edited: docProps/core.xml is unreadable"),
            "{text}"
        );
        assert!(!app.modified);
        assert_eq!(app.pkg.part("docProps/core.xml").unwrap(), utf16.as_slice());
    }

    /// File › Info lists who wrote the workbook and when, from core.xml.
    #[test]
    fn file_info_shows_the_read_only_properties() {
        let mut pkg = new_xlsx();
        pkg.stamp_save("2026-10-01T12:00:00Z", "Ann");
        let app = App::new(
            load_xlsx(&gridcore::xlsx::save_xlsx(&pkg)).unwrap(),
            "x.xlsx",
        );
        let info: Vec<String> = app.info_lines().iter().map(|l| l.to_string()).collect();
        for want in [
            "  Author            Ann",
            "  Last Modified By  Ann",
            "  Created           2026-10-01T12:00:00Z",
            "  Last Modified     2026-10-01T12:00:00Z",
        ] {
            assert!(info.iter().any(|l| l == want), "{want:?} in {info:?}");
        }
    }

    /// Save As opens on the type the workbook is bound to: Unicode Text
    /// saved again (Save As, Enter) stays UTF-16, not Text (Tab) 1252.
    #[test]
    fn save_as_again_keeps_the_bound_unicode_text_type() {
        let dir = tmp("unicode-again");
        let mut app = two_sheet_app(&dir);
        let unicode = SAVE_TYPES
            .iter()
            .position(|t| t.label == "Unicode Text")
            .unwrap();
        let path = dir.join("u.txt");
        app.save_as_type(path.to_string_lossy().into_owned(), unicode);
        std::fs::remove_file(&path).unwrap();
        app.open_backstage();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        while app.backstage.as_ref().unwrap().item != backstage::Item::SaveAs {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        assert_eq!(app.backstage.as_ref().unwrap().type_sel, unicode);
        let b = app.backstage.as_mut().unwrap();
        b.dir = dir.clone();
        app.backstage_key(key(KeyCode::Enter));
        assert!(std::fs::read(&path).unwrap().starts_with(b"\xFF\xFE"));
        // A .html workbook's Save As shows Web Page, not Excel Workbook.
        let web = SAVE_TYPES
            .iter()
            .position(|t| t.label == "Web Page")
            .unwrap();
        app.save_as_type(dir.join("p.html").to_string_lossy().into_owned(), web);
        app.open_backstage();
        while app.backstage.as_ref().unwrap().item != backstage::Item::SaveAs {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        assert_eq!(app.backstage.as_ref().unwrap().type_sel, web);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Save As prompt (F12) keeps the bound type: Unicode Text saved
    /// again is still UTF-16, CSV (Comma delimited) is still Windows-1252.
    #[test]
    fn the_save_as_prompt_keeps_the_bound_text_type() {
        let dir = tmp("prompt-bound");
        let at = |l: &str| SAVE_TYPES.iter().position(|t| t.label == l).unwrap();
        for (label, file, head) in [
            ("Unicode Text", "u.txt", &b"\xFF\xFE"[..]),
            ("CSV (Comma delimited)", "c.csv", &b"Name,Note"[..]),
        ] {
            let mut app = two_sheet_app(&dir);
            let path = dir.join(file);
            app.save_as_type(path.to_string_lossy().into_owned(), at(label));
            std::fs::remove_file(&path).unwrap();
            app.open_prompt(PromptKind::SaveAs);
            app.prompt.as_mut().unwrap().text = path.to_string_lossy().into_owned();
            app.commit_prompt();
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.starts_with(head), "{label}: {bytes:?}");
            assert_eq!(app.text_type, Some(at(label)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `:wq` after saving as CSV (only the active sheet was written) stays
    /// open instead of quitting with the other sheets unsaved.
    #[test]
    fn vim_wq_after_a_csv_save_as_does_not_quit() {
        let dir = tmp("wq-csv");
        let mut app = two_sheet_app(&dir);
        app.request_save_as(dir.join("book.csv").to_string_lossy().into_owned());
        assert!(app.modified);
        for cmd in ["wq", "x"] {
            assert!(!app.vim_run_command(cmd), ":{cmd} quit");
            assert!(app.status.as_deref().unwrap().contains("Not quitting"));
        }
        // Saved as a workbook, :wq quits.
        app.request_save_as(dir.join("book.xlsx").to_string_lossy().into_owned());
        assert!(app.vim_run_command("wq"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #615: XML Data needs XML maps; other unwritten types are refused and
    /// nothing is written under their name.
    #[test]
    fn save_as_refuses_types_it_cannot_write() {
        let dir = tmp("refused");
        let at = |l: &str| SAVE_TYPES.iter().position(|t| t.label == l).unwrap();
        let mut app = two_sheet_app(&dir);
        app.save_as_type(
            dir.join("x.xml").to_string_lossy().into_owned(),
            at("XML Data"),
        );
        assert_eq!(
            app.status.as_deref(),
            Some("Cannot save XML data because the workbook does not contain any XML mappings.")
        );
        for (label, file) in [
            ("Excel Binary Workbook", "b.xlsb"),
            ("Excel 97-2003 Workbook", "o.xls"),
            ("OpenDocument Spreadsheet", "o.ods"),
            ("Single File Web Page", "s.mht"),
        ] {
            app.save_as_type(dir.join(file).to_string_lossy().into_owned(), at(label));
            assert!(
                app.status.as_deref().unwrap().contains("cannot save as"),
                "{label}"
            );
            assert!(!dir.join(file).exists(), "{label}");
        }
        assert!(app.path.ends_with("book.xlsx"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #615: File › Export › Change File Type opens Save As with that type.
    #[test]
    fn export_change_file_type_opens_save_as_with_the_type() {
        let mut app = App::new(new_xlsx(), "book.xlsx");
        app.open_backstage();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        while app.backstage.as_ref().unwrap().item != backstage::Item::Export {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::Export
        );
        let tab = SAVE_TYPES
            .iter()
            .position(|t| t.label == "Text (Tab delimited)")
            .unwrap();
        // Past the CSV and PDF exports to the type.
        for _ in 0..=tab + 1 {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        let b = app.backstage.as_ref().unwrap();
        assert_eq!(b.pane, backstage::Pane::SaveAs);
        assert_eq!(b.name_input, "book.txt");
        assert_eq!(b.chosen_type(), Some(tab));
    }

    /// 2024-09-30, the clock the CSV tests open with.
    const CSV_TODAY: f64 = 45_565.0;

    fn csv_open(auto: AutoConvert) -> TextOpen {
        TextOpen {
            auto,
            today: Some(CSV_TODAY),
        }
    }

    fn value(pkg: &SheetPackage, a1: &str) -> gridcore::sheet::CellValue {
        let (r, c) = parse_a1(a1).unwrap();
        pkg.workbook.sheets[0]
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn code(pkg: &SheetPackage, a1: &str) -> Option<String> {
        let (r, c) = parse_a1(a1).unwrap();
        let style = pkg.workbook.sheets[0].cell(r, c).unwrap().style;
        pkg.workbook.styles.xf(style).code
    }

    /// #605: a CSV's fields convert as typed entry does, with 15 digits.
    #[test]
    fn opening_a_csv_converts_every_field_as_typed() {
        use gridcore::sheet::CellValue::{Bool, Number, Text};
        let bytes = b"\xEF\xBB\xBFcode,when,pct,flag,sci,long,calc,text,quoted\r\n\
                      007,1/2,12%,TRUE,1E5,1234567890123456789,=1+1,apple,\"a,b\"\r\n";
        let text = gridcore::textio::decode(bytes, gridcore::textio::Origin::Auto);
        let pkg = csv_to_pkg(&text, "in", false, &csv_open(AutoConvert::default()));
        // The header row stays plain text.
        assert_eq!(value(&pkg, "A1"), Text("code".into()));
        assert_eq!(value(&pkg, "I1"), Text("quoted".into()));
        assert_eq!(value(&pkg, "A2"), Number(7.0));
        let jan2 = gridcore::sheet::parts_to_serial(2024, 1, 2, 0, false);
        assert_eq!(value(&pkg, "B2"), Number(jan2));
        let date = gridcore::sheet::classify_format_code(&code(&pkg, "B2").unwrap());
        assert_eq!(date, gridcore::sheet::NumFmt::Date);
        assert_eq!(value(&pkg, "C2"), Number(0.12));
        assert_eq!(code(&pkg, "C2").as_deref(), Some("0%"));
        assert_eq!(value(&pkg, "D2"), Bool(true));
        assert_eq!(value(&pkg, "E2"), Number(100_000.0));
        assert_eq!(value(&pkg, "F2"), Number(1.23456789012346e18));
        let g2 = pkg.workbook.sheets[0].cell(1, 6).unwrap();
        assert_eq!(g2.formula.as_deref(), Some("1+1"));
        assert_eq!(g2.value, Number(2.0));
        assert_eq!(value(&pkg, "H2"), Text("apple".into()));
        assert_eq!(value(&pkg, "I2"), Text("a,b".into()));
    }

    /// #606: a `sep=` first line names the delimiter and is not imported;
    /// a blank header stays blank.
    #[test]
    fn a_sep_line_sets_the_delimiter_and_blank_headers_stay_blank() {
        use gridcore::sheet::CellValue::{Empty, Number, Text};
        let open = csv_open(AutoConvert::default());
        let pkg = csv_to_pkg("sep=;\r\na;b\r\n1;2\r\n", "sep", false, &open);
        assert_eq!(value(&pkg, "A1"), Text("a".into()));
        assert_eq!(value(&pkg, "B1"), Text("b".into()));
        assert_eq!(value(&pkg, "A2"), Number(1.0));
        assert_eq!(value(&pkg, "B2"), Number(2.0));
        assert_eq!(pkg.workbook.sheets[0].used_size(), (2, 2));
        let pkg = csv_to_pkg("a,,c\n1,2,3\n", "blank", false, &open);
        assert_eq!(value(&pkg, "B1"), Empty);
        assert_eq!(value(&pkg, "B2"), Number(2.0));
    }

    /// #607: with Automatic Data Conversion off those fields open as text.
    #[test]
    fn automatic_data_conversion_off_keeps_fields_as_text() {
        use gridcore::sheet::CellValue::{Number, Text};
        let csv = "007,1/2,1E5,1234567890123456789\n";
        let all_off = AutoConvert {
            remove_leading_zeros: false,
            keep_15_digits: false,
            e_notation: false,
            dates: false,
        };
        let off = csv_to_pkg(csv, "off", false, &csv_open(all_off));
        for (a1, text) in [
            ("A1", "007"),
            ("B1", "1/2"),
            ("C1", "1E5"),
            ("D1", "1234567890123456789"),
        ] {
            assert_eq!(value(&off, a1), Text(text.into()), "{a1}");
        }
        let on = csv_to_pkg(csv, "on", false, &csv_open(AutoConvert::default()));
        assert_eq!(value(&on, "A1"), Number(7.0));
        assert_eq!(value(&on, "C1"), Number(100_000.0));
        assert_eq!(value(&on, "D1"), Number(1.23456789012346e18));
    }

    /// #607: File › Options › Data shows the four switches and changes them.
    #[test]
    fn file_options_data_changes_the_conversion_switches() {
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_backstage();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        // Walk the menu down to Options and enter it.
        while app.backstage.as_ref().unwrap().item != backstage::Item::Options {
            app.backstage_key(key(KeyCode::Down));
        }
        app.backstage_key(key(KeyCode::Enter));
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::Options
        );
        app.backstage_key(key(KeyCode::Char(' ')));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Char(' ')));
        assert_eq!(
            app.auto_convert,
            AutoConvert {
                remove_leading_zeros: false,
                dates: false,
                ..AutoConvert::default()
            }
        );
        // Reopened, the page shows the current switches.
        app.backstage = None;
        app.open_backstage();
        let opts = &app.backstage.as_ref().unwrap().options;
        assert_eq!(
            opts.iter()
                .take(4)
                .map(|o| o.value == backstage::OptValue::Check(true))
                .collect::<Vec<_>>(),
            [false, true, true, false]
        );
        assert!(opts[0].label.starts_with("Remove leading zeros"));
    }

    /// #607: the four switches persist in the preferences file.
    #[test]
    fn the_conversion_options_round_trip_through_the_preferences() {
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        assert_eq!(app.auto_convert, AutoConvert::default());
        app.auto_convert.remove_leading_zeros = false;
        app.auto_convert.dates = false;
        let text = app.view_prefs_text();
        assert!(text.contains("convert_leading_zeros=0"), "{text}");
        let mut again = App::new(new_xlsx(), "untitled.xlsx");
        again.apply_view_prefs(&text);
        assert_eq!(again.auto_convert, app.auto_convert);
        // An older file without the keys keeps Excel's defaults.
        again.apply_view_prefs("formula_view=0\n");
        assert_eq!(again.auto_convert, AutoConvert::default());
    }

    /// #603: an .xls, .xlsb or .ods opens as an import, as a CSV does: bound
    /// to the .xlsx beside it, with a status line naming the format, and a
    /// save writes that .xlsx and leaves the original alone. File › Open
    /// lists the three types.
    #[test]
    fn legacy_workbooks_open_as_imports_bound_to_xlsx() {
        let dir = tmp("legacy-import");
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/legacy");
        for (ext, label) in [
            ("xls", "Excel 97-2003 Workbook"),
            ("xlsb", "Excel Binary Workbook"),
            ("ods", "OpenDocument Spreadsheet"),
        ] {
            let source = dir.join(format!("book.{ext}"));
            std::fs::copy(corpus.join(format!("oracle-basic.{ext}")), &source).unwrap();
            let original = std::fs::read(&source).unwrap();
            let binding = source.with_extension("xlsx");
            let mut app = App::new(new_xlsx(), "untitled.xlsx");
            app.open_workbook(source.to_str().unwrap());
            assert_eq!(Path::new(&app.path), binding, "{ext}");
            assert_eq!(app.import_source.as_deref(), source.to_str(), "{ext}");
            let note = format!(
                "Opened {} ({label}); saving writes {}",
                source.display(),
                binding.display()
            );
            assert_eq!(app.status.as_deref(), Some(note.as_str()));
            assert!(!app.sheet().cells.is_empty(), "{ext}");
            app.save();
            let saved = load_xlsx(&std::fs::read(&binding).unwrap()).unwrap();
            assert_eq!(saved.workbook.sheets[0].name, app.sheet().name, "{ext}");
            assert_eq!(std::fs::read(&source).unwrap(), original, "{ext}");
            // The next format binds book.xlsx again only once it's gone.
            std::fs::remove_file(&binding).unwrap();
        }
        let app = App::new(new_xlsx(), "untitled.xlsx");
        for ext in ["xls", "xlsb", "ods"] {
            assert!(app.extensions().contains(&ext), "{ext}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #876: a CSV/TSV or text import binds as an `.xls` does: to
    /// `<stem>.xlsx` when it is free, else the next free numbered name, so
    /// Ctrl+S leaves an existing workbook alone.
    #[test]
    fn csv_import_never_binds_over_an_existing_xlsx() {
        let dir = tmp("csv-import-free");
        let opts = TextOpen::from_prefs();
        for ext in ["csv", "tsv", "txt", "prn"] {
            let source = dir.join(format!("{ext}book.{ext}"));
            std::fs::write(&source, "a,b\n1,2\n").unwrap();
            let (_, bound, from, _) = load_workbook(source.to_str().unwrap(), &opts).unwrap();
            assert_eq!(Path::new(&bound), source.with_extension("xlsx"), "{ext}");
            assert_eq!(from.as_deref(), source.to_str());
            std::fs::write(source.with_extension("xlsx"), b"taken").unwrap();
            let (_, bound, _, _) = load_workbook(source.to_str().unwrap(), &opts).unwrap();
            assert_eq!(
                bound,
                format!("{}1.xlsx", source.with_extension("").display())
            );
        }

        let source = dir.join("book.csv");
        std::fs::write(&source, "a,b\n1,2\n").unwrap();
        let existing = dir.join("book.xlsx");
        std::fs::write(&existing, b"the user's own book").unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        let bound = dir.join("book1.xlsx");
        assert_eq!(Path::new(&app.path), bound);
        app.save();
        assert_eq!(std::fs::read(&existing).unwrap(), b"the user's own book");
        assert!(load_xlsx(&std::fs::read(&bound).unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #876: a CSV import rechecks its binding at the first save, as an
    /// `.xls` import does: a `<stem>.xlsx` made after the open is kept.
    #[test]
    fn a_csv_import_rechecks_its_binding_at_the_first_save() {
        let dir = tmp("csv-import-late");
        let source = dir.join("late.csv");
        std::fs::write(&source, "a,b\n1,2\n").unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        let plain = dir.join("late.xlsx");
        assert_eq!(Path::new(&app.path), plain);
        std::fs::write(&plain, b"made by someone else").unwrap();
        app.save();
        assert_eq!(Path::new(&app.path), dir.join("late1.xlsx"));
        assert_eq!(std::fs::read(&plain).unwrap(), b"made by someone else");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #876: the Text Import Wizard's Finish binds the import to a free
    /// name too.
    #[test]
    fn text_wizard_import_binds_to_a_free_name() {
        let dir = tmp("text-wizard-free");
        let source = dir.join("notes.txt");
        std::fs::write(&source, "a\tb\n1\t2\n").unwrap();
        let existing = dir.join("notes.xlsx");
        std::fs::write(&existing, b"the user's own notes").unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        assert!(app.text_dialog.is_some());
        app.finish_text_dialog();
        let bound = dir.join("notes1.xlsx");
        assert_eq!(Path::new(&app.path), bound);
        assert_eq!(app.import_source.as_deref(), source.to_str());
        assert!(app.import_unsaved);
        app.save();
        assert_eq!(std::fs::read(&existing).unwrap(), b"the user's own notes");
        assert!(load_xlsx(&std::fs::read(&bound).unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #876: `xlsxy new.xls` on a missing path starts a workbook bound to
    /// a free `.xlsx`, as Excel would; a type a save writes, or no known
    /// type at all, stays bound as typed.
    #[test]
    fn missing_legacy_path_binds_a_new_workbook_to_xlsx() {
        let dir = tmp("missing-legacy");
        let at = |name: &str| dir.join(name).to_string_lossy().into_owned();
        for name in ["new.xls", "new.xlsb", "new.ods", "new.XLS", "new.xml"] {
            assert_eq!(missing_binding(&at(name)), at("new.xlsx"), "{name}");
        }
        for name in ["new.xlsx", "new.xlsm", "new", "new.zzz"] {
            assert_eq!(missing_binding(&at(name)), at(name), "{name}");
        }
        std::fs::write(dir.join("new.xlsx"), b"taken").unwrap();
        assert_eq!(missing_binding(&at("new.xls")), at("new1.xlsx"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #603: an import beside an existing `<stem>.xlsx` binds to the next
    /// free name, so Ctrl+S leaves that file alone; an `.xls` that is named
    /// `.xlsx` never binds to itself.
    #[test]
    fn legacy_imports_never_bind_to_an_existing_file() {
        let dir = tmp("legacy-import-free");
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/legacy");
        let source = dir.join("report.xls");
        std::fs::copy(corpus.join("oracle-basic.xls"), &source).unwrap();
        let existing = dir.join("report.xlsx");
        std::fs::write(&existing, b"the user's own report").unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        let bound = dir.join("report1.xlsx");
        assert_eq!(Path::new(&app.path), bound);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .ends_with(&format!("saving writes {}", bound.display()))
        );
        app.save();
        assert_eq!(std::fs::read(&existing).unwrap(), b"the user's own report");
        assert!(load_xlsx(&std::fs::read(&bound).unwrap()).is_ok());

        // An .xls saved under an .xlsx name.
        let disguised = dir.join("book.xlsx");
        std::fs::copy(corpus.join("oracle-basic.xls"), &disguised).unwrap();
        let original = std::fs::read(&disguised).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(disguised.to_str().unwrap());
        assert_eq!(Path::new(&app.path), dir.join("book1.xlsx"));
        app.save();
        assert_eq!(std::fs::read(&disguised).unwrap(), original);
        assert!(load_xlsx(&std::fs::read(dir.join("book1.xlsx")).unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #603: the binding is rechecked at the first save: a `<stem>.xlsx`
    /// that appeared after the open is not overwritten, and once this
    /// session has written its file, later saves go to it.
    #[test]
    fn an_import_rechecks_its_binding_at_the_first_save() {
        let dir = tmp("legacy-import-late");
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/legacy");
        let source = dir.join("report.xls");
        std::fs::copy(corpus.join("oracle-basic.xls"), &source).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        let plain = dir.join("report.xlsx");
        assert_eq!(Path::new(&app.path), plain);
        std::fs::write(&plain, b"made by someone else").unwrap();
        app.save();
        let bound = dir.join("report1.xlsx");
        assert_eq!(Path::new(&app.path), bound);
        assert_eq!(std::fs::read(&plain).unwrap(), b"made by someone else");
        let status = app.status.clone().unwrap();
        assert!(
            status.starts_with(&format!("Saved {}", bound.display())),
            "{status}"
        );
        assert!(
            status.ends_with(&format!("{} already exists", plain.display())),
            "{status}"
        );
        // A second save writes the same file.
        app.save();
        assert_eq!(Path::new(&app.path), bound);
        assert!(load_xlsx(&std::fs::read(&bound).unwrap()).is_ok());
        assert!(!dir.join("report2.xlsx").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #603: Save As on an import not yet written writes the chosen name as
    /// chosen, even over a file, and never rebinds to `<stem>N.xlsx`. A Save
    /// As that fails keeps the import unwritten (the next save rechecks).
    #[test]
    fn save_as_on_an_unsaved_import_writes_the_chosen_name() {
        let dir = tmp("legacy-import-save-as");
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/legacy");
        let source = dir.join("report.xls");
        std::fs::copy(corpus.join("oracle-basic.xls"), &source).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        assert!(app.import_unsaved);

        // A failed Save As (no such folder) changes nothing.
        let bound = app.path.clone();
        let nowhere = dir.join("missing").join("x.xlsx");
        assert!(!app.save_as(nowhere.to_string_lossy().into_owned()));
        assert!(app.import_unsaved);
        assert_eq!(app.path, bound);

        let chosen = dir.join("chosen.xlsx");
        std::fs::write(&chosen, b"an older file").unwrap();
        assert!(app.save_as(chosen.to_string_lossy().into_owned()));
        assert_eq!(Path::new(&app.path), chosen);
        let saved = load_xlsx(&std::fs::read(&chosen).unwrap()).unwrap();
        assert_eq!(saved.workbook.sheets[0].name, app.sheet().name);
        assert!(!dir.join("report.xlsx").exists());
        assert!(!dir.join("report1.xlsx").exists());
        assert!(!app.import_unsaved);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #603: the control surface's open (no dialog) imports them the same way.
    #[test]
    fn opening_without_a_dialog_imports_legacy_workbooks_too() {
        let dir = tmp("legacy-import-ctl");
        let source = dir.join("book.ods");
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/legacy");
        std::fs::copy(corpus.join("calc-refs.ods"), &source).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_without_wizard(source.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&app.path), source.with_extension("xlsx"));
        assert_eq!(app.import_source.as_deref(), source.to_str());
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("(OpenDocument Spreadsheet)")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backstage_pdf_export_prints_the_current_sheet_next_to_the_workbook() {
        let dir = std::env::temp_dir().join(format!("xlsxy-pdf-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        let pdf = dir.join("book.pdf");
        let _ = std::fs::remove_file(&pdf);
        let mut app = App::new(new_xlsx(), book.to_str().unwrap());
        app.export_pdf();
        assert_eq!(
            app.status.as_deref(),
            Some("We didn't find anything to print.")
        );
        assert!(!pdf.exists());
        app.pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::text("hello"));
        app.export_pdf();
        assert!(
            app.status.as_deref().unwrap().ends_with("(1 page)"),
            "{:?}",
            app.status
        );
        let bytes = std::fs::read(&pdf).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("(hello) Tj"));
        assert!(app.backstage.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backstage_pdf_export_of_a_read_only_workbook_writes_the_pdf_only() {
        // #882: a read-only workbook is never written; its PDF is a new file.
        let dir = std::env::temp_dir().join(format!("xlsxy-pdf-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        let pdf = dir.join("book.pdf");
        let _ = std::fs::remove_file(&pdf);
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::text("hello"));
        std::fs::write(&book, save_xlsx(&pkg)).unwrap();
        let before = std::fs::read(&book).unwrap();
        let mut app = App::new(pkg, book.to_str().unwrap());
        app.set_read_only(book.to_str().unwrap());
        app.export_pdf();
        assert!(
            app.status.as_deref().unwrap().starts_with("Exported"),
            "{:?}",
            app.status
        );
        assert!(String::from_utf8_lossy(&std::fs::read(&pdf).unwrap()).contains("(hello) Tj"));
        assert_eq!(std::fs::read(&book).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imported_csv_stays_protected_until_successful_save_as_or_new() {
        let dir = std::env::temp_dir().join(format!("xlsxy-import-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("import.csv");
        let bytes = b"first;second\n1;2\n";
        std::fs::write(&source, bytes).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(source.to_str().unwrap());
        assert_eq!(app.import_source.as_deref(), source.to_str());
        let binding = source.with_extension("xlsx");
        assert_eq!(Path::new(&app.path), binding);
        app.export_csv();
        assert_eq!(
            app.status.as_deref(),
            Some("Export failed: cannot overwrite the source document")
        );
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        app.open_workbook(dir.join("missing.xlsx").to_str().unwrap());
        assert_eq!(app.import_source.as_deref(), source.to_str());
        app.save();
        assert!(load_xlsx(&std::fs::read(&binding).unwrap()).is_ok());
        assert_eq!(app.import_source.as_deref(), source.to_str());
        app.export_csv();
        assert!(app.status.as_deref().unwrap().contains("cannot overwrite"));
        app.commit_save_as(dir.clone(), "./import.csv".into());
        assert_eq!(Path::new(&app.path), binding);
        assert_eq!(app.import_source.as_deref(), source.to_str());
        app.commit_save_as(dir.join("missing-parent"), "failed.xlsx".into());
        assert_eq!(app.import_source.as_deref(), source.to_str());
        assert_eq!(Path::new(&app.path), binding);
        app.commit_save_as(dir.clone(), "converted.xlsx".into());
        assert!(app.import_source.is_none());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        app.open_workbook(source.to_str().unwrap());
        app.new_workbook();
        assert!(app.import_source.is_none());
        app.open_workbook(source.to_str().unwrap());
        app.open_workbook(binding.to_str().unwrap());
        assert!(app.import_source.is_none());
        for file in [source, binding, dir.join("converted.xlsx")] {
            std::fs::remove_file(file).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }

    /// A macro workbook: `.xlsm`-typed, with a VBA project behind the `.bin`
    /// Default, as Excel writes one.
    fn xlsm_pkg() -> SheetPackage {
        let bytes = gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::MacroWorkbook);
        let mut pkg = load_xlsx(&bytes).unwrap();
        let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
            .replace(
                "</Relationships>",
                r#"<Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#,
            );
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
        let ct = String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).replace(
            "</Types>",
            r#"<Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/></Types>"#,
        );
        pkg.set_part("[Content_Types].xml", ct.into_bytes());
        pkg.set_part("xl/vbaProject.bin", b"VBA".to_vec());
        assert!(pkg.has_vba_project());
        pkg
    }

    fn saved_content_types(path: &Path) -> String {
        let pkg = load_xlsx(&std::fs::read(path).unwrap()).unwrap();
        String::from_utf8_lossy(pkg.part("[Content_Types].xml").unwrap()).into_owned()
    }

    fn macro_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("xlsxy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// #601: Save As `.xlsx` from a macro workbook asks first; Yes writes a
    /// real `.xlsx` and the open workbook drops its macros too, so a later
    /// Save As neither asks again nor writes them back.
    #[test]
    fn save_as_xlsx_from_a_macro_workbook_asks_and_yes_drops_the_macros() {
        let dir = macro_dir("macros-yes");
        let mut app = App::new(xlsm_pkg(), dir.join("in.xlsm").to_str().unwrap());
        app.modified = true;
        app.commit_save_as(dir.clone(), "out.xlsx".into());
        let c = app
            .confirm
            .as_ref()
            .expect("Save As asks before dropping macros");
        assert!(
            c.prompt()
                .contains("cannot be saved in macro-free workbooks: VB project")
        );
        let out = dir.join("out.xlsx");
        assert!(!out.exists(), "nothing is written before the answer");

        assert!(!app.confirm_key(KeyEvent::from(KeyCode::Char('y'))));
        assert!(app.confirm.is_none());
        let ct = saved_content_types(&out);
        assert!(
            ct.contains("spreadsheetml.sheet.main+xml") && !ct.contains("macroEnabled"),
            "{ct}"
        );
        assert!(!ct.contains("vbaProject"), "{ct}");
        assert!(
            !load_xlsx(&std::fs::read(&out).unwrap())
                .unwrap()
                .has_vba_project()
        );
        assert_eq!(Path::new(&app.path), out);
        assert!(!app.modified);
        assert!(!app.pkg.has_vba_project());

        app.commit_save_as(dir.clone(), "again.xlsx".into());
        assert!(app.confirm.is_none(), "a later Save As does not ask again");
        assert!(dir.join("again.xlsx").exists());
        app.commit_save_as(dir.clone(), "back.xlsm".into());
        assert!(app.confirm.is_none());
        let back = load_xlsx(&std::fs::read(dir.join("back.xlsm")).unwrap()).unwrap();
        assert!(!back.has_vba_project(), "the dropped macros stay dropped");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A workbook with an Excel 4.0 macro sheet (`Macro1`, after `Sheet1`).
    fn xlm_pkg() -> SheetPackage {
        let mut pkg = new_xlsx();
        pkg.add_sheet("Macro1");
        let rels = String::from_utf8_lossy(pkg.part("xl/_rels/workbook.xml.rels").unwrap())
            .replace(
                r#"Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml""#,
                r#"Type="http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet" Target="worksheets/sheet2.xml""#,
            );
        pkg.set_part("xl/_rels/workbook.xml.rels", rels.into_bytes());
        let bytes = gridcore::xlsx::save_xlsx_as(&pkg, SpreadsheetKind::MacroWorkbook);
        let pkg = load_xlsx(&bytes).unwrap();
        assert!(pkg.has_macro_sheets());
        pkg
    }

    /// #727: Save As `.xlsx` asks before dropping Excel 4.0 macro sheets. The
    /// file written has none; the open workbook keeps them (as Excel does),
    /// so its sheets do not shift, and a later Save As asks again.
    #[test]
    fn save_as_xlsx_with_macro_sheets_asks_and_writes_none() {
        let dir = macro_dir("xlm-yes");
        let mut app = App::new(xlm_pkg(), dir.join("in.xlsm").to_str().unwrap());
        app.sheet = 1;
        app.commit_save_as(dir.clone(), "out.xlsx".into());
        let c = app.confirm.as_ref().expect("Save As asks first");
        assert!(
            c.prompt().contains(
                "cannot be saved in macro-free workbooks: Excel 4.0 macro sheets. Save without them?"
            ),
            "{}",
            c.prompt()
        );
        assert!(!app.confirm_key(KeyEvent::from(KeyCode::Char('y'))));
        let out = load_xlsx(&std::fs::read(dir.join("out.xlsx")).unwrap()).unwrap();
        assert!(!out.has_macro_sheets());
        assert_eq!(out.workbook.sheets.len(), 1);
        assert_eq!(out.workbook.sheets[0].name, "Sheet1");
        // The open workbook is unchanged.
        assert!(app.pkg.has_macro_sheets());
        assert_eq!(app.pkg.workbook.sheets.len(), 2);
        assert_eq!(app.sheet, 1);

        app.commit_save_as(dir.clone(), "again.xlsx".into());
        assert!(app.confirm.is_some(), "a later Save As asks again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #727 r1: an Excel 4.0 name alone, with no macro sheet, is asked about
    /// too, and the `.xlsx` written has none.
    #[test]
    fn save_as_xlsx_asks_about_an_excel4_name_alone() {
        let mut pkg = new_xlsx();
        let wb = String::from_utf8_lossy(pkg.part("xl/workbook.xml").unwrap()).replace(
            "</sheets>",
            r#"</sheets><definedNames><definedName name="CellColor" xlm="1">GET.CELL(63,INDIRECT("rc",FALSE))</definedName></definedNames>"#,
        );
        pkg.set_part("xl/workbook.xml", wb.into_bytes());
        let pkg = load_xlsx(&gridcore::xlsx::save_xlsx_as(
            &pkg,
            SpreadsheetKind::MacroWorkbook,
        ))
        .unwrap();
        assert!(!pkg.has_macro_sheets() && pkg.has_macro_names());
        let dir = macro_dir("xlm-name");
        let mut app = App::new(pkg, dir.join("in.xlsm").to_str().unwrap());
        app.commit_save_as(dir.clone(), "out.xlsx".into());
        let prompt = app
            .confirm
            .as_ref()
            .expect("Save As asks")
            .prompt()
            .to_string();
        assert!(
            prompt.contains("workbooks: Excel 4.0 function stored in defined names. Save"),
            "{prompt}"
        );
        assert!(!app.confirm_key(KeyEvent::from(KeyCode::Char('y'))));
        let out = load_xlsx(&std::fs::read(dir.join("out.xlsx")).unwrap()).unwrap();
        assert!(!out.has_macro_names());
        assert!(app.pkg.has_macro_names(), "the open workbook keeps it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Both features in one question, in Excel's order.
    #[test]
    fn one_question_lists_every_macro_feature() {
        let mut pkg = xlsm_pkg();
        let with_sheets = xlm_pkg();
        pkg.set_part(
            "xl/_rels/workbook.xml.rels",
            with_sheets
                .part("xl/_rels/workbook.xml.rels")
                .map(|b| {
                    String::from_utf8_lossy(b).replace(
                        "</Relationships>",
                        r#"<Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#,
                    )
                })
                .unwrap()
                .into_bytes(),
        );
        let pkg = {
            let mut p = with_sheets;
            for name in [
                "xl/_rels/workbook.xml.rels",
                "xl/vbaProject.bin",
                "[Content_Types].xml",
            ] {
                p.set_part(name, pkg.part(name).unwrap().to_vec());
            }
            p
        };
        assert!(pkg.has_vba_project() && pkg.has_macro_sheets());
        let dir = macro_dir("xlm-both");
        let mut app = App::new(pkg, dir.join("in.xlsm").to_str().unwrap());
        app.commit_save_as(dir.clone(), "out.xlsx".into());
        let prompt = app.confirm.as_ref().unwrap().prompt().to_string();
        assert!(
            prompt.contains("workbooks: VB project, Excel 4.0 macro sheets. Save"),
            "{prompt}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #727: a template's new workbook is `<stem>N` beside it, with the
    /// template's macro-ness: `.xltx` gives `.xlsx`, `.xltm` gives `.xlsm`.
    #[test]
    fn a_template_binds_the_first_free_numbered_workbook() {
        let dir = macro_dir("tmpl-names");
        let t = dir.join("Budget.xltx");
        let t = t.to_str().unwrap();
        let first = dir.join("Budget1.xlsx");
        assert_eq!(template_binding(t).as_deref(), first.to_str());
        std::fs::write(&first, b"taken").unwrap();
        assert_eq!(
            template_binding(t).as_deref(),
            dir.join("Budget2.xlsx").to_str()
        );
        let m = dir.join("Macros.XLTM");
        assert_eq!(
            template_binding(m.to_str().unwrap()).as_deref(),
            dir.join("Macros1.xlsm").to_str()
        );
        assert_eq!(template_binding(dir.join("a.xlsx").to_str().unwrap()), None);
        assert_eq!(template_binding(dir.join("a.xlsm").to_str().unwrap()), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #727: opening a template starts a new workbook from it, as Excel does.
    /// Save writes `Budget1.xlsx` as a workbook and leaves the template
    /// byte for byte; revert before the first save has nothing to go back to.
    #[test]
    fn opening_a_template_starts_a_new_workbook() {
        let dir = macro_dir("tmpl-open");
        let template = dir.join("Budget.xltx");
        let bytes = gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template);
        std::fs::write(&template, &bytes).unwrap();
        let t = template.to_str().unwrap();
        let new_book = dir.join("Budget1.xlsx");

        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(t);
        assert_eq!(Path::new(&app.path), new_book);
        let status = app.status.clone().unwrap();
        assert!(
            status.contains("New workbook") && status.contains("from template"),
            "{status}"
        );
        assert_eq!(
            app.reload(),
            Err("nothing to revert: not saved yet".to_string())
        );
        assert_eq!(Path::new(&app.path), new_book);

        app.pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(7.0));
        app.modified = true;
        app.save_current().unwrap();
        assert_eq!(
            std::fs::read(&template).unwrap(),
            bytes,
            "the template is untouched"
        );
        let ct = saved_content_types(&new_book);
        assert!(
            ct.contains("spreadsheetml.sheet.main+xml") && !ct.contains("template"),
            "{ct}"
        );
        // Once saved, revert reads the new workbook back.
        assert!(app.reload().is_ok());
        assert_eq!(Path::new(&app.path), new_book);

        // The control surface's open does the same; the next number is free.
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_without_wizard(t).unwrap();
        assert_eq!(Path::new(&app.path), dir.join("Budget2.xlsx"));
        assert!(app.status.as_deref().unwrap().contains("from template"));

        // A macro template gives a macro workbook.
        let xltm = dir.join("Macros.xltm");
        std::fs::write(
            &xltm,
            gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::MacroTemplate),
        )
        .unwrap();
        app.open_workbook(xltm.to_str().unwrap());
        assert_eq!(Path::new(&app.path), dir.join("Macros1.xlsm"));
        app.save_current().unwrap();
        assert!(
            saved_content_types(&dir.join("Macros1.xlsm")).contains("sheet.macroEnabled.main+xml")
        );

        // A workbook opened afterwards is not "from a template".
        app.open_workbook(new_book.to_str().unwrap());
        assert_eq!(Path::new(&app.path), new_book);
        assert!(app.template.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A template's new workbook in `dir`: `Budget.xltx` written as a
    /// template and opened, A1 typed. Returns the template's bytes.
    fn template_workbook(dir: &Path) -> (App, Vec<u8>) {
        let template = dir.join("Budget.xltx");
        let bytes = gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template);
        std::fs::write(&template, &bytes).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.os_clip = None;
        app.open_workbook(template.to_str().unwrap());
        assert!(app.template.is_some());
        app.pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(7.0));
        app.modified = true;
        (app, bytes)
    }

    fn prompts_save_as(app: &App) -> bool {
        matches!(
            app.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::SaveAs)
        )
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// #614: Ctrl+S on a workbook started from a template opens Save As
    /// with the name it was bound to and writes nothing, as Excel does;
    /// confirming writes a workbook, never the template.
    #[test]
    fn saving_a_template_workbook_opens_save_as() {
        let dir = macro_dir("tmpl-save-as");
        let (mut app, bytes) = template_workbook(&dir);
        let new_book = dir.join("Budget1.xlsx");
        handle_key(&mut app, ctrl('s'));
        assert!(prompts_save_as(&app), "Ctrl+S opens Save As");
        assert_eq!(
            Path::new(&app.prompt.as_ref().unwrap().text),
            new_book.as_path()
        );
        assert_eq!(
            app.status.as_deref(),
            Some("Budget1.xlsx is a new workbook from Budget.xltx: choose where to save it")
        );
        assert!(!new_book.exists(), "nothing written yet");
        assert!(app.modified);

        app.commit_prompt();
        assert!(app.status.as_deref().unwrap().starts_with("Saved"));
        let ct = saved_content_types(&new_book);
        assert!(
            ct.contains("spreadsheetml.sheet.main+xml") && !ct.contains("template"),
            "{ct}"
        );
        assert_eq!(std::fs::read(dir.join("Budget.xltx")).unwrap(), bytes);
        assert!(app.template.is_none() && !app.modified);
        // Once it has a file, Ctrl+S saves to it.
        handle_key(&mut app, ctrl('s'));
        assert!(app.prompt.is_none());
        assert!(app.status.as_deref().unwrap().starts_with("Saved"));

        // `:w`, the ribbon's Save and File › Save ask the same way.
        type Route = fn(&mut App);
        let routes: [(&str, Route); 3] = [
            (":w", |app| {
                app.vim_run_command("w");
            }),
            ("ribbon", |app| app.ribbon_act(ribbon::Act::Save)),
            ("backstage", |app| {
                app.open_backstage();
                app.apply_backstage_event(backstage::BackstageEvent::Save);
            }),
        ];
        for (route, save) in routes {
            let (mut app, _) = template_workbook(&dir);
            let bound = PathBuf::from(&app.path);
            save(&mut app);
            assert!(prompts_save_as(&app), "{route} opens Save As");
            assert!(app.backstage.is_none(), "{route}: the prompt is visible");
            assert!(!bound.exists(), "{route} writes nothing");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: `:wq` and `:x` on a template's workbook open Save As and do
    /// not quit (vim's "no file name"), writing nothing.
    #[test]
    fn vim_wq_on_a_template_workbook_opens_save_as_and_stays() {
        let dir = macro_dir("tmpl-wq");
        for cmd in ["wq", "x"] {
            let (mut app, _) = template_workbook(&dir);
            let bound = PathBuf::from(&app.path);
            assert!(!app.vim_run_command(cmd), ":{cmd} does not quit");
            assert!(prompts_save_as(&app), ":{cmd} opens Save As");
            assert!(!bound.exists(), ":{cmd} writes nothing");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614 (keeping #727 r1): when the bound name has been taken since the
    /// template opened (another session), Save As offers the next free one,
    /// and confirming leaves the other file alone.
    #[test]
    fn save_as_prefill_moves_past_a_taken_template_binding() {
        let dir = macro_dir("tmpl-taken-prompt");
        let (mut app, _) = template_workbook(&dir);
        let taken = dir.join("Budget1.xlsx");
        std::fs::write(&taken, b"another session").unwrap();
        app.save();
        let free = dir.join("Budget2.xlsx");
        assert_eq!(
            Path::new(&app.prompt.as_ref().unwrap().text),
            free.as_path()
        );
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("Budget2.xlsx is")
        );
        app.commit_prompt();
        assert_eq!(std::fs::read(&taken).unwrap(), b"another session");
        assert!(load_xlsx(&std::fs::read(&free).unwrap()).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: the control surface's `wb.save` is scripted: it writes a
    /// template's workbook to its bound name without a dialog, as Excel's
    /// `Workbook.Save` does.
    #[test]
    fn wb_save_writes_a_template_workbook_without_asking() {
        use ctlcore::json::Json;
        let dir = macro_dir("tmpl-wb-save");
        let (mut app, bytes) = template_workbook(&dir);
        control::dispatch(&mut app, "wb.save", &Json::obj(vec![])).unwrap();
        assert!(app.prompt.is_none());
        assert!(load_xlsx(&std::fs::read(dir.join("Budget1.xlsx")).unwrap()).is_ok());
        assert_eq!(std::fs::read(dir.join("Budget.xltx")).unwrap(), bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: Ctrl+N is File › New: it asks before discarding changes.
    #[test]
    fn ctrl_n_starts_a_new_workbook() {
        let mut app = App::new(new_xlsx(), "kept.xlsx");
        app.os_clip = None;
        app.pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(1.0));
        handle_key(&mut app, ctrl('n'));
        assert_eq!(app.path, "untitled.xlsx");
        assert!(app.sheet().cell(0, 0).is_none());
        assert_eq!(app.status.as_deref(), Some("New workbook"));

        let mut app = App::new(new_xlsx(), "kept.xlsx");
        app.os_clip = None;
        app.pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(1.0));
        app.modified = true;
        handle_key(&mut app, ctrl('n'));
        assert!(matches!(
            app.confirm.as_ref().map(|c| c.action()),
            Some(ConfirmAction::Discard(Next::New))
        ));
        assert_eq!(app.path, "kept.xlsx");
        assert!(app.sheet().cell(0, 0).is_some());
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(app.path, "untitled.xlsx");
        assert!(app.sheet().cell(0, 0).is_none());
    }

    /// #614: Ctrl+N on the welcome screen is Blank workbook, as on Excel's
    /// start screen.
    #[test]
    fn ctrl_n_on_the_welcome_screen_is_blank_workbook() {
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.start_screen = true;
        assert!(!handle_key(&mut app, ctrl('n')));
        assert!(!app.start_screen);
        assert_eq!(app.status.as_deref(), Some("New workbook"));
    }

    /// #614: `book.xltx` in XLSTART is what a new workbook starts from,
    /// cells, styles and comments, as an unsaved `untitled.xlsx` (never
    /// bound into XLSTART); its name matches in any case.
    #[test]
    fn new_workbook_starts_from_book_xltx_in_xlstart() {
        let dir = macro_dir("xlstart-book");
        let xlstart = dir.join("XLSTART");
        std::fs::create_dir_all(&xlstart).unwrap();
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::text("hello"));
        assert!(pkg.set_comment(0, 0, 0, "me", "from the template"));
        let book = xlstart.join("Book.XLTX");
        let bytes = gridcore::xlsx::save_xlsx_as(&pkg, SpreadsheetKind::Template);
        std::fs::write(&book, &bytes).unwrap();

        let mut app = App::new(new_xlsx(), "kept.xlsx");
        app.os_clip = None;
        app.xlstart = Some(xlstart.clone());
        handle_key(&mut app, ctrl('n'));
        assert_eq!(
            app.sheet().cell(0, 0).map(|c| &c.value),
            Some(&CellValue::Text("hello".into()))
        );
        assert_eq!(app.comments.len(), 1, "the template's comment is kept");
        assert_eq!(app.path, "untitled.xlsx");
        assert!(!app.modified && app.template.is_none());
        assert_eq!(app.status.as_deref(), Some("New workbook from Book.XLTX"));

        // Saved, it is a workbook; the template is untouched.
        let out = dir.join("untitled.xlsx");
        assert!(app.save_as(out.to_string_lossy().into_owned()));
        let ct = saved_content_types(&out);
        assert!(
            ct.contains("spreadsheetml.sheet.main+xml") && !ct.contains("template"),
            "{ct}"
        );
        assert_eq!(std::fs::read(&book).unwrap(), bytes);

        // File › New and the welcome screen's Blank workbook go the same way.
        app.request_discard(Next::New);
        assert_eq!(app.status.as_deref(), Some("New workbook from Book.XLTX"));
        app.start_screen = true;
        app.start_choose(0);
        assert_eq!(app.status.as_deref(), Some("New workbook from Book.XLTX"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: a `book.xltx` that cannot be read gives a blank workbook and
    /// says why.
    #[test]
    fn a_broken_book_xltx_falls_back_to_blank() {
        let dir = macro_dir("xlstart-broken");
        std::fs::write(dir.join("book.xltx"), b"not a workbook").unwrap();
        let mut app = App::new(new_xlsx(), "kept.xlsx");
        app.pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(1.0));
        app.xlstart = Some(dir.clone());
        app.new_workbook();
        assert!(app.sheet().cell(0, 0).is_none());
        assert_eq!(app.path, "untitled.xlsx");
        let status = app.status.clone().unwrap();
        assert!(
            status.starts_with("New workbook (book.xltx not used: "),
            "{status}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: the startup folders open their workbooks, XLSTART's first,
    /// each sorted by name: not templates, lock or hidden files, text files
    /// that would open the wizard, or folders; a file listed once.
    #[test]
    fn startup_workbooks_skip_templates_and_sort() {
        let dir = macro_dir("startup-list");
        let xlstart = dir.join("XLSTART");
        let alt = dir.join("alt");
        std::fs::create_dir_all(xlstart.join("s.xlsx")).unwrap();
        std::fs::create_dir_all(&alt).unwrap();
        for name in [
            "b.xlsx",
            "A.XLSX",
            "d.tsv",
            "book.xltx",
            "x.xltm",
            "~$a.xlsx",
            ".hidden.xlsx",
            "notes.txt",
        ] {
            std::fs::write(xlstart.join(name), b"").unwrap();
        }
        std::fs::write(alt.join("c.xlsm"), b"").unwrap();
        let names = |dirs: &[&Path]| -> Vec<String> {
            startup_workbooks(dirs)
                .iter()
                .map(|p| file_name_of(&p.to_string_lossy()))
                .collect()
        };
        let want = ["A.XLSX", "b.xlsx", "d.tsv", "c.xlsm"];
        assert_eq!(names(&[&xlstart, &alt]), want);
        // The alternate folder may be XLSTART itself; a missing one is skipped.
        assert_eq!(names(&[&xlstart, &alt, &xlstart]), want);
        assert_eq!(names(&[&dir.join("missing"), &alt]), ["c.xlsm"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: at launch with no file, the first startup workbook that loads
    /// opens instead of the welcome screen, bound to its own path; one that
    /// fails is named and skipped, and the status counts those not opened.
    #[test]
    fn startup_opens_the_first_startup_workbook() {
        let dir = macro_dir("startup-open");
        let broken = dir.join("broken.xlsx");
        std::fs::write(&broken, b"not a workbook").unwrap();
        let mut files = vec![broken.clone()];
        for name in ["one.xlsx", "two.xlsx", "three.xlsx"] {
            let mut pkg = new_xlsx();
            pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::text(name));
            std::fs::write(dir.join(name), save_xlsx(&pkg)).unwrap();
            files.push(dir.join(name));
        }
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.start_screen = true;
        app.open_startup_workbooks(&files);
        assert!(!app.start_screen, "no welcome screen");
        assert_eq!(Path::new(&app.path), dir.join("one.xlsx"));
        assert_eq!(
            app.sheet().cell(0, 0).map(|c| &c.value),
            Some(&CellValue::Text("one.xlsx".into()))
        );
        let status = app.status.clone().unwrap();
        assert!(status.contains("broken.xlsx not opened"), "{status}");
        assert!(
            status.ends_with(
                "2 more startup workbooks not opened: xlsxy shows one workbook per window"
            ),
            "{status}"
        );

        // When none loads, the welcome screen stays; it draws no status
        // line, so the reason shows when it closes, on Blank workbook or
        // Ctrl+N, and only once.
        let routes: [fn(&mut App); 2] = [
            |app| {
                app.start_choose(0);
            },
            |app| {
                handle_key(app, ctrl('n'));
            },
        ];
        for close in routes {
            let mut app = App::new(new_xlsx(), "untitled.xlsx");
            app.start_screen = true;
            app.open_startup_workbooks(std::slice::from_ref(&broken));
            assert!(app.start_screen);
            assert_eq!(app.path, "untitled.xlsx");
            close(&mut app);
            assert!(!app.start_screen);
            let status = app.status.clone().unwrap();
            assert!(
                status.starts_with("Startup workbook broken.xlsx not opened: ")
                    && status.ends_with("; New workbook"),
                "{status}"
            );
            app.new_workbook();
            assert_eq!(app.status.as_deref(), Some("New workbook"));
        }
        // No startup workbooks: nothing changes.
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.start_screen = true;
        app.open_startup_workbooks(&[]);
        assert!(app.start_screen);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #614: the alternate startup folder is the `alt_startup_path`
    /// preference, kept when the preferences are written back.
    #[test]
    fn alt_startup_path_survives_prefs_round_trip() {
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.apply_view_prefs("formula_view=1\nalt_startup_path= D:\\a=b\\start \n");
        assert_eq!(app.alt_startup.as_deref(), Some("D:\\a=b\\start"));
        let text = app.view_prefs_text();
        assert!(text.contains("alt_startup_path=D:\\a=b\\start\n"), "{text}");
        let mut again = App::new(new_xlsx(), "untitled.xlsx");
        again.apply_view_prefs(&text);
        assert_eq!(again.alt_startup, app.alt_startup);
        // Empty means none, and none is not written.
        again.apply_view_prefs("alt_startup_path=\n");
        assert_eq!(again.alt_startup, None);
        assert!(!again.view_prefs_text().contains("alt_startup_path"));
    }

    /// #727 r1: the name a template's workbook was bound to at open can be
    /// taken before its first save (a second session from the same
    /// template). The save moves on to the next free name and leaves that
    /// file alone; revert never loads it. A Save As name is written as chosen.
    #[test]
    fn a_template_workbook_never_replaces_a_file_that_appeared_after_open() {
        let dir = macro_dir("tmpl-race");
        let template = dir.join("Budget.xltx");
        std::fs::write(
            &template,
            gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template),
        )
        .unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(template.to_str().unwrap());
        let first = dir.join("Budget1.xlsx");
        assert_eq!(Path::new(&app.path), first);

        std::fs::write(&first, b"another session's workbook").unwrap();
        assert_eq!(
            app.reload(),
            Err("nothing to revert: not saved yet".to_string()),
            "revert does not load the other file"
        );
        app.save_current().unwrap();
        let second = dir.join("Budget2.xlsx");
        assert_eq!(Path::new(&app.path), second);
        assert_eq!(
            std::fs::read(&first).unwrap(),
            b"another session's workbook"
        );
        assert!(load_xlsx(&std::fs::read(&second).unwrap()).is_ok());
        let status = app.status.clone().unwrap();
        assert!(status.contains("already exists"), "{status}");

        // Written once, it is an ordinary workbook: Ctrl-S stays put and
        // revert reads it back.
        app.save_current().unwrap();
        assert_eq!(Path::new(&app.path), second);
        assert!(app.reload().is_ok());
        assert_eq!(Path::new(&app.path), second);

        // Save As over an existing file is the user's choice.
        app.open_workbook(template.to_str().unwrap());
        let chosen = dir.join("chosen.xlsx");
        std::fs::write(&chosen, b"old").unwrap();
        assert!(app.save_as(chosen.to_str().unwrap().to_string()));
        assert_eq!(Path::new(&app.path), chosen);
        assert!(load_xlsx(&std::fs::read(&chosen).unwrap()).is_ok());
        assert!(app.template.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #727 r2: File > New after opening a template starts an ordinary
    /// workbook: Ctrl-S writes `untitled.xlsx` where it is bound, and does
    /// not move on to a `<template>N` name.
    #[test]
    fn a_new_workbook_after_a_template_is_not_template_born() {
        let dir = macro_dir("tmpl-new");
        let template = dir.join("Budget.xltx");
        std::fs::write(
            &template,
            gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template),
        )
        .unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(template.to_str().unwrap());
        assert!(app.template.is_some());
        app.new_workbook();
        assert!(app.template.is_none());
        // Bind it inside the scratch folder, where a file of that name exists.
        let untitled = dir.join("untitled.xlsx");
        std::fs::write(&untitled, b"old").unwrap();
        app.path = untitled.to_str().unwrap().to_string();
        app.save_current().unwrap();
        assert_eq!(Path::new(&app.path), untitled);
        assert!(load_xlsx(&std::fs::read(&untitled).unwrap()).is_ok());
        assert!(!dir.join("Budget1.xlsx").exists());
        assert!(app.reload().is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Yes, but the write fails: nothing changes, the macros included.
    #[test]
    fn a_failed_save_without_macros_keeps_them() {
        let dir = macro_dir("macros-fail");
        let source = dir.join("in.xlsm");
        let mut app = App::new(xlsm_pkg(), source.to_str().unwrap());
        app.modified = true;
        app.commit_save_as(dir.join("missing-parent"), "out.xlsx".into());
        assert!(app.confirm.is_some());
        assert!(!app.confirm_key(KeyEvent::from(KeyCode::Char('y'))));
        assert!(app.status.as_deref().unwrap().contains("save failed"));
        assert!(app.pkg.has_vba_project());
        assert_eq!(Path::new(&app.path), source);
        assert!(app.modified);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// No keeps everything: the path, the unsaved flag and the macros.
    #[test]
    fn save_as_xlsx_from_a_macro_workbook_no_keeps_everything() {
        let dir = macro_dir("macros-no");
        let source = dir.join("in.xlsm");
        let mut app = App::new(xlsm_pkg(), source.to_str().unwrap());
        app.modified = true;
        // The typed-path prompt asks too.
        app.open_prompt(PromptKind::SaveAs);
        app.prompt.as_mut().unwrap().text = dir.join("out.xlsx").to_string_lossy().into_owned();
        app.commit_prompt();
        assert!(app.confirm.is_some());
        // Dropping macros is destructive: Enter alone means No.
        assert!(!app.confirm_key(KeyEvent::from(KeyCode::Enter)));
        assert!(app.confirm.is_none());
        assert!(!dir.join("out.xlsx").exists());
        assert_eq!(Path::new(&app.path), source);
        assert!(app.modified);
        assert!(app.pkg.has_vba_project());
        assert!(app.status.as_deref().unwrap().contains("cancelled"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A macro-enabled target keeps the macros and needs no question; a
    /// template saved as `.xlsx` becomes a workbook.
    #[test]
    fn save_writes_the_type_of_the_target_path() {
        let dir = macro_dir("macros-kind");
        let mut app = App::new(xlsm_pkg(), dir.join("in.xlsm").to_str().unwrap());
        app.commit_save_as(dir.clone(), "copy.xltm".into());
        assert!(app.confirm.is_none());
        let ct = saved_content_types(&dir.join("copy.xltm"));
        assert!(ct.contains("template.macroEnabled.main+xml"), "{ct}");
        assert!(ct.contains("vbaProject"), "{ct}");
        assert!(app.pkg.has_vba_project());

        let template = gridcore::xlsx::save_xlsx_as(&new_xlsx(), SpreadsheetKind::Template);
        let mut app = App::new(
            load_xlsx(&template).unwrap(),
            dir.join("t.xltx").to_str().unwrap(),
        );
        app.commit_save_as(dir.clone(), "budget.xlsx".into());
        assert!(app.confirm.is_none());
        let ct = saved_content_types(&dir.join("budget.xlsx"));
        assert!(
            ct.contains("spreadsheetml.sheet.main+xml") && !ct.contains("template"),
            "{ct}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_file_browser_offers_every_workbook_type() {
        let app = App::new(new_xlsx(), "t.xlsx");
        let exts = backstage::BackstageHost::extensions(&app);
        for ext in ["xlsx", "xlsm", "xltx", "xltm"] {
            assert!(exts.contains(&ext), "{ext}");
        }
    }

    #[test]
    fn exports_refuse_source_aliases() {
        let dir = std::env::temp_dir().join(format!("xlsxy-export-alias-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("note.xlsx");
        let pkg = new_xlsx();
        let original = save_xlsx(&pkg);
        std::fs::write(&source, &original).unwrap();
        assert!(
            export_csv_headless(
                &pkg,
                source.to_str().unwrap(),
                None,
                dir.join("./note.xlsx").to_str().unwrap()
            )
            .is_err()
        );
        let csv = source.with_extension("csv");
        std::fs::hard_link(&source, &csv).unwrap();
        let mut app = App::new(pkg, source.to_str().unwrap());
        app.export_csv();
        assert!(app.status.as_deref().unwrap().contains("cannot overwrite"));
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&csv).unwrap(), original);
        std::fs::remove_file(csv).unwrap();
        app.save();
        assert!(load_xlsx(&std::fs::read(&source).unwrap()).is_ok());
        std::fs::remove_file(source).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn internal_clip_does_not_survive_a_workbook_switch() {
        // A clip holds the old workbook's cells, whose style indices and
        // cm/vm metadata indices mean nothing in another workbook.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        let mut cell = Cell::number(1.0);
        cell.meta = Some(Box::new(gridcore::sheet::CellMeta {
            cm: Some("1".into()),
            ..Default::default()
        }));
        app.pkg.workbook.sheets[0].set_cell(0, 0, cell);
        app.copy(false);
        app.cur = (0, 1);
        app.paste();
        assert!(app.sheet().cell(0, 1).is_some_and(|c| c.meta.is_some()));

        app.new_workbook();
        app.paste();
        assert!(app.sheet().cell(0, 0).is_none());
    }

    #[test]
    fn comment_authoring_flow() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        // Author a comment on the current cell (A1).
        app.commit_comment("Please double-check");
        assert_eq!(app.comments.len(), 1);
        assert!(app.has_comment(0, 0));
        assert_eq!(app.comment_at(0, 0).unwrap().text, "Please double-check");
        assert!(app.modified);

        // A second comment elsewhere, then navigate to it.
        app.cur = (4, 2);
        app.commit_comment("Second note");
        assert_eq!(app.comments.len(), 2);
        app.cur = (0, 0);
        app.nav_comment(1); // from A1 → next comment
        assert_eq!(app.cur, (4, 2));

        // Deleting removes it and the marker.
        app.delete_comment();
        assert!(!app.has_comment(4, 2));
        assert_eq!(app.comments.len(), 1);

        // Survives a save/load round-trip.
        let bytes = save_xlsx(&app.pkg);
        let reloaded = load_xlsx(&bytes).unwrap();
        let cs = reloaded.comments();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].text, "Please double-check");
    }

    #[test]
    fn threaded_comment_and_reply_flow() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        // First threaded comment on A1, then a reply on the same cell.
        app.commit_comment("Is this right?");
        app.commit_comment("Yes, confirmed");
        let a1: Vec<&Comment> = app
            .comments
            .iter()
            .filter(|c| c.row == 0 && c.col == 0)
            .collect();
        assert_eq!(a1.len(), 2);
        assert!(a1.iter().all(|c| c.threaded));
        assert_eq!(a1[1].text, "Yes, confirmed");

        // A legacy note lands on a different cell as a non-threaded comment.
        app.cur = (3, 3);
        app.commit_note("A plain note");
        let note = app.comment_at(3, 3).unwrap();
        assert!(!note.threaded);

        // Both survive a round-trip.
        let reloaded = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let cs = reloaded.comments();
        assert_eq!(cs.iter().filter(|c| c.threaded).count(), 2);
        assert_eq!(cs.iter().filter(|c| !c.threaded).count(), 1);
    }

    #[test]
    fn list_validation_dropdown_sets_cell() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.pkg.workbook.sheets[0]
            .validations
            .push(gridcore::sheet::DataValidation {
                ranges: vec![(0, 0, 4, 0)], // A1:A5
                kind: "list".into(),
                formula1: "\"Yes,No,Maybe\"".into(),
                ..Default::default()
            });
        // On a covered cell the dropdown opens with the parsed values.
        app.cur = (0, 0);
        assert!(app.current_validation().is_some());
        app.open_dv_dropdown();
        let p = app.dv_picker.as_ref().expect("dropdown opened");
        assert_eq!(p.values, vec!["Yes", "No", "Maybe"]);
        // Move to "No" and commit → the cell takes that value.
        app.dv_picker_key(KeyCode::Down);
        app.dv_picker_key(KeyCode::Enter);
        assert!(app.dv_picker.is_none());
        assert_eq!(app.current_input_text(), "No");

        // A cell outside every range has no validation.
        app.cur = (9, 0);
        assert!(app.current_validation().is_none());
        app.open_dv_dropdown();
        assert!(app.dv_picker.is_none());
    }

    #[test]
    fn cursor_skips_hidden_rows() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        // Hide rows 2 and 3 (0-based), as an applied filter would.
        let s = &mut app.pkg.workbook.sheets[0];
        s.row_attrs.insert(1, "hidden=\"1\"".into());
        s.row_attrs.insert(2, "hidden=\"1\"".into());
        app.cur = (0, 0);
        // Down from row 0 lands past the two hidden rows, on row 3.
        app.move_cur(1, 0, false);
        assert_eq!(app.cur.0, 3);
        // With hidden shown, the very next row is reachable again.
        app.cur = (0, 0);
        app.toggle_show_hidden();
        app.move_cur(1, 0, false);
        assert_eq!(app.cur.0, 1);
    }

    #[test]
    fn bold_then_insert_row_above_keeps_a_cse_array() {
        // #725: a legacy Ctrl+Shift+Enter array (no `cm`) over D1:D3. Bolding
        // it goes through Engine::set_cell; inserting a row rebuilds the
        // engine. It must come out the other side still an array.
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        for r in 0..3u32 {
            sheet.set_cell(r, 0, Cell::number(f64::from(r + 1)));
            sheet.set_cell(r, 3, Cell::number(f64::from(2 * (r + 1))));
        }
        sheet.set_cell(
            0,
            3,
            Cell {
                value: CellValue::Number(2.0),
                formula: Some("A1:A3*2".into()),
                f_attrs: Some(" t=\"array\" ref=\"D1:D3\"".into()),
                spill: Some((3, 1)), // as load_xlsx reads it from the ref
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 3);
        app.anchor = None;
        app.toggle_bold();
        app.cur = (0, 0);
        app.row_op(true);

        for r in 1..4u32 {
            assert_eq!(
                app.sheet().cell(r, 3).unwrap().value,
                CellValue::Number(f64::from(2 * r)),
                "row {r}"
            );
        }
        let style = app.sheet().cell(1, 3).unwrap().style;
        assert!(app.pkg.workbook.styles.xf(style).bold);
        let saved = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let ws = String::from_utf8_lossy(saved.part("xl/worksheets/sheet1.xml").unwrap());
        assert!(
            ws.contains(&format!(
                r#"<c r="D2" s="{style}"><f t="array" ref="D2:D4">A2:A4*2</f><v>2</v></c>"#
            )),
            "{ws}"
        );
    }

    /// An app with `formula` typed into D1 (spilling down column D).
    fn app_with_spill(formula: &str) -> App {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.apply(vec![(0, 3, Cell::formula(formula))]);
        assert_eq!(app.sheet().cell(0, 3).unwrap().spill, Some((3, 1)));
        app
    }

    /// D1 still spills over D1:D3, and each of D1:D3 is (not) bold.
    fn assert_spill_bold(app: &App, bold: bool, when: &str) {
        let anchor = app.sheet().cell(0, 3).unwrap();
        assert_eq!(anchor.spill, Some((3, 1)), "{when}: {:?}", anchor.value);
        for r in 0..3 {
            let cell = app.sheet().cell(r, 3).unwrap();
            assert!(!cell.value.is_empty(), "{when}: D{}", r + 1);
            let xf = app.pkg.workbook.styles.xf(cell.style);
            assert_eq!(xf.bold, bold, "{when}: D{}", r + 1);
        }
    }

    #[test]
    fn bold_on_a_spill_block_keeps_the_spill_through_undo_redo() {
        // #784: formatting a whole spilled block left D1 #SPILL!.
        let mut app = app_with_spill("SEQUENCE(3)");
        app.anchor = Some((0, 3));
        app.cur = (2, 3);
        app.toggle_bold();
        assert_spill_bold(&app, true, "bold");
        for r in 0..3 {
            let v = app.sheet().cell(r, 3).unwrap().value.clone();
            assert_eq!(v, CellValue::Number(f64::from(r + 1)));
        }
        app.undo();
        assert_spill_bold(&app, false, "undo");
        app.redo();
        assert_spill_bold(&app, true, "redo");
    }

    #[test]
    fn bold_on_one_spill_member_keeps_the_spill_through_undo_redo() {
        let mut app = app_with_spill("SEQUENCE(3)");
        app.anchor = None;
        app.cur = (1, 3);
        app.toggle_bold();
        let bold_at = |app: &App, r: u32| {
            let style = app.sheet().cell(r, 3).unwrap().style;
            app.pkg.workbook.styles.xf(style).bold
        };
        let check = |app: &App, d2_bold: bool, when: &str| {
            assert_eq!(
                app.sheet().cell(0, 3).unwrap().spill,
                Some((3, 1)),
                "{when}"
            );
            assert_eq!(
                app.sheet().cell(1, 3).unwrap().value,
                CellValue::Number(2.0),
                "{when}"
            );
            assert_eq!(bold_at(app, 1), d2_bold, "{when}");
            assert!(!bold_at(app, 0) && !bold_at(app, 2), "{when}");
        };
        check(&app, true, "bold");
        app.undo();
        check(&app, false, "undo");
        app.redo();
        check(&app, true, "redo");
    }

    #[test]
    fn bold_on_a_randarray_spill_keeps_it_through_undo_redo() {
        // Volatile: every recalc redraws the values, so a restyle's undo must
        // not hinge on the cells' values being unchanged.
        let mut app = app_with_spill("RANDARRAY(3)");
        app.anchor = Some((0, 3));
        app.cur = (2, 3);
        app.toggle_bold();
        assert_spill_bold(&app, true, "bold");
        app.undo();
        assert_spill_bold(&app, false, "undo");
        app.redo();
        assert_spill_bold(&app, true, "redo");
    }

    #[test]
    fn undo_bold_on_an_empty_cell_leaves_no_cell() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        app.anchor = None;
        app.toggle_bold();
        assert!(app.sheet().cell(0, 0).is_some_and(|cl| cl.style != 0));
        app.undo();
        assert!(app.sheet().cell(0, 0).is_none());
        app.redo();
        assert!(app.sheet().cell(0, 0).is_some_and(|cl| cl.style != 0));
    }

    /// A legacy CSE block over D1:D3 with a 1x1 result (repeated over the block).
    fn cse_sum_block() -> Cell {
        Cell {
            value: CellValue::Number(6.0),
            formula: Some("SUM(A1:A3)".into()),
            f_attrs: Some(" t=\"array\" ref=\"D1:D3\"".into()),
            ..Cell::default()
        }
    }

    fn f_attrs_at(app: &App, sheet: usize, r: u32, c: u32) -> Option<String> {
        app.pkg.workbook.sheets[sheet].cell(r, c)?.f_attrs.clone()
    }

    #[test]
    fn pasting_a_cse_block_onto_another_sheet_covers_only_its_cell() {
        // The same address on another sheet: nothing moves, but the cell
        // there holds no such formula, so the paste is typing (#724) and
        // brings none of the source's `<f>` attributes.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.open_prompt(PromptKind::AddSheet);
        app.prompt.as_mut().unwrap().text = "Data".to_string();
        app.commit_prompt();
        app.pkg.workbook.sheets[1].set_cell(0, 3, cse_sum_block());
        app.goto_sheet(1);
        app.cur = (0, 3);
        app.anchor = None;
        app.copy(false);
        app.goto_sheet(0);
        app.cur = (0, 3);
        app.paste();
        assert_eq!(f_attrs_at(&app, 0, 0, 3), None);
        assert_eq!(
            f_attrs_at(&app, 1, 0, 3).as_deref(),
            Some(" t=\"array\" ref=\"D1:D3\"")
        );
        let saved = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let ws = String::from_utf8_lossy(saved.part("xl/worksheets/sheet1.xml").unwrap());
        assert!(ws.contains(r#"SUM(A1:A3)</f>"#), "{ws}");
        assert!(!ws.contains("D1:D3"), "{ws}");
    }

    #[test]
    fn pasting_a_cse_block_back_after_it_moved_does_not_overlap_it() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 3, cse_sum_block());
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 3);
        app.anchor = None;
        app.copy(false);
        app.cur = (0, 0);
        app.row_op(true); // the block moves to D2:D4
        app.cur = (0, 3);
        app.paste();
        assert_eq!(
            f_attrs_at(&app, 0, 1, 3).as_deref(),
            Some(" t=\"array\" ref=\"D2:D4\"")
        );
        // Typed at D1 (#724): no block of its own to overlap D2:D4 with.
        assert_eq!(f_attrs_at(&app, 0, 0, 3), None);
    }

    /// A1:A3 = 1, 2, 3 and a legacy CSE block `{=A1:A3*2}` over D1:D3, as
    /// load_xlsx reads it, spilling 2, 4, 6.
    /// #669: Paste Special over xlsxy's own copy, through gridcore's rules.
    #[test]
    fn paste_special_values_transpose_and_an_operation() {
        let mut app = App::new(new_xlsx(), "ps.xlsx");
        app.os_clip = None;
        let s = &mut app.pkg.workbook.sheets[0];
        s.set_cell(0, 0, Cell::number(2.0));
        s.set_cell(1, 0, Cell::formula("A1*3"));
        app.engine = Engine::new(&app.pkg.workbook);
        app.engine.recalc_all(&mut app.pkg.workbook);
        app.cur = (1, 0);
        app.anchor = Some((0, 0));
        app.copy(false);
        app.anchor = None;
        app.cur = (0, 2);
        app.open_paste_special();
        assert!(matches!(
            app.outline_dialog,
            Some(outlinedlg::Dialog::PasteSpecial(_))
        ));
        // Values, transposed: C1 2, D1 6 (no formula).
        app.outline_dialog_key(KeyCode::Right);
        app.outline_dialog_key(KeyCode::Right);
        for _ in 0..3 {
            app.outline_dialog_key(KeyCode::Down);
        }
        app.outline_dialog_key(KeyCode::Char(' '));
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        let cell = |app: &App, r, c| app.sheet().cell(r, c).cloned().unwrap_or_default();
        assert_eq!(cell(&app, 0, 3).value, CellValue::Number(6.0));
        assert!(cell(&app, 0, 3).formula.is_none());
        // Multiply onto a constant.
        app.pkg.workbook.sheets[0].set_cell(5, 0, Cell::number(10.0));
        app.cur = (0, 0);
        app.anchor = None;
        app.copy(false);
        app.cur = (5, 0);
        app.paste_special(gridcore::edit::PasteSpec {
            op: gridcore::edit::PasteOp::Multiply,
            ..Default::default()
        });
        assert_eq!(cell(&app, 5, 0).value, CellValue::Number(20.0));
        assert!(app.undo.len() >= 2, "each paste is an undo step");
    }

    /// #707 r1 C1: a copy whose sheet is deleted goes with it, so Paste
    /// Special cannot read a sheet that is not there (it panicked); a copy
    /// on a later sheet follows the renumbering. M5: a cut takes Paste only.
    #[test]
    fn paste_special_after_the_copys_sheet_is_deleted() {
        let mut pkg = new_xlsx();
        pkg.add_sheet("Sheet2");
        pkg.add_sheet("Sheet3");
        let mut app = App::new(pkg, "del.xlsx");
        app.os_clip = None;
        app.sheet = 1;
        app.pkg.workbook.sheets[1].set_cell(0, 0, Cell::number(2.0));
        app.pkg.workbook.sheets[1].set_cell(1, 0, Cell::formula("A1*3"));
        app.cur = (1, 0);
        app.anchor = Some((0, 0));
        app.copy(false);
        app.delete_current_sheet();
        assert_eq!(app.clip.as_ref().map(|c| c.sheet), Some(SHEET_GONE));
        app.anchor = None;
        app.open_paste_special();
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        assert!(app.status.as_deref().unwrap_or("").contains("gone"));
        // Even a clip left pointing past the sheets is refused, not a panic.
        app.clip = Some(ClipData {
            cells: vec![vec![Some(Cell::number(1.0))]],
            sheet: 9,
            from: (0, 0),
            cut: false,
        });
        app.paste_special(gridcore::edit::PasteSpec::default());
        assert!(app.status.as_deref().unwrap_or("").contains("gone"));
        // A copy on Sheet3 survives deleting Sheet1, renumbered.
        let mut pkg = new_xlsx();
        pkg.add_sheet("Sheet2");
        let mut app = App::new(pkg, "renum.xlsx");
        app.os_clip = None;
        app.sheet = 1;
        app.pkg.workbook.sheets[1].set_cell(0, 0, Cell::number(5.0));
        app.copy(false);
        app.sheet = 0;
        app.delete_current_sheet();
        assert_eq!(app.clip.as_ref().map(|c| c.sheet), Some(0));
        // M5: a cut refuses Paste Special.
        app.copy(true);
        app.open_paste_special();
        assert!(app.outline_dialog.is_none());
        assert!(app.status.as_deref().unwrap_or("").contains("cut"));
    }

    /// #668: Fill Up and Fill Left.
    #[test]
    fn fill_up_and_left() {
        let mut app = App::new(new_xlsx(), "fill.xlsx");
        app.os_clip = None;
        app.pkg.workbook.sheets[0].set_cell(2, 0, Cell::number(7.0));
        app.pkg.workbook.sheets[0].set_cell(0, 3, Cell::formula("E1+1"));
        app.cur = (0, 0);
        app.anchor = Some((2, 0));
        app.fill(FillDir::Up);
        assert_eq!(
            app.sheet().cell(0, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(7.0))
        );
        app.cur = (0, 1);
        app.anchor = Some((0, 3));
        app.fill(FillDir::Left);
        assert_eq!(
            app.sheet()
                .cell(0, 1)
                .and_then(|c| c.formula.clone())
                .as_deref(),
            Some("C1+1")
        );
    }

    fn app_with_spilling_cse() -> App {
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        for r in 0..3u32 {
            sheet.set_cell(r, 0, Cell::number(f64::from(r + 1)));
            sheet.set_cell(r, 3, Cell::number(f64::from(2 * (r + 1))));
        }
        sheet.set_cell(
            0,
            3,
            Cell {
                value: CellValue::Number(2.0),
                formula: Some("A1:A3*2".into()),
                f_attrs: Some(" t=\"array\" ref=\"D1:D3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        assert_cse_spills(&app, "loaded");
        app
    }

    /// D1 is the CSE block over D1:D3, spilling 2, 4, 6.
    fn assert_cse_spills(app: &App, when: &str) {
        let d1 = app.sheet().cell(0, 3).unwrap();
        assert_eq!(d1.spill, Some((3, 1)), "{when}: {:?}", d1.value);
        assert_eq!(
            d1.f_attrs.as_deref(),
            Some(" t=\"array\" ref=\"D1:D3\""),
            "{when}"
        );
        for r in 0..3u32 {
            assert_eq!(
                app.sheet().cell(r, 3).unwrap().value,
                CellValue::Number(f64::from(2 * (r + 1))),
                "{when}: D{}",
                r + 1
            );
        }
    }

    /// r7 M2: a refused edit changes nothing, records no undo step, leaves
    /// the workbook unmodified, and says why in Excel's words.
    fn assert_refused(app: &App, before: &std::collections::BTreeMap<(u32, u32), Cell>) {
        assert_eq!(&app.sheet().cells, before);
        assert!(app.undo.is_empty());
        assert!(!app.modified);
        assert_eq!(app.status.as_deref(), Some(PART_OF_ARRAY));
        assert_cse_spills(app, "refused");
    }

    #[test]
    fn a_cut_pasted_into_part_of_a_cse_block_is_refused_whole() {
        // Cut A5:A6 and paste at D2: the cut's source is not cleared, and
        // the clip stays a cut that pastes elsewhere.
        let mut app = app_with_spilling_cse();
        app.pkg.workbook.sheets[0].set_cell(4, 0, Cell::number(7.0));
        app.pkg.workbook.sheets[0].set_cell(5, 0, Cell::number(8.0));
        let before = app.sheet().cells.clone();
        app.cur = (4, 0);
        app.anchor = Some((5, 0));
        app.copy(true);
        app.cur = (1, 3);
        app.anchor = None;
        app.paste();
        assert_refused(&app, &before);
        app.cur = (0, 5);
        app.paste();
        assert_eq!(app.status.as_deref(), Some("Pasted"));
        assert_eq!(
            app.sheet().cell(0, 5).unwrap().value,
            CellValue::Number(7.0)
        );
        assert!(app.sheet().cell(4, 0).is_none_or(|c| c.value.is_empty()));
    }

    #[test]
    fn a_cut_whose_clears_change_the_block_is_still_refused_whole() {
        // r8 M3: B2 = 4; cut A2:B2 and paste at C2, which puts the 4 into
        // D2. D2 holds 4 now, but clearing A2 would recalculate it: decided
        // once, before the clears, it is refused whole, the source intact
        // and the clip still a cut.
        let mut app = app_with_spilling_cse();
        app.pkg.workbook.sheets[0].set_cell(1, 1, Cell::number(4.0));
        let before = app.sheet().cells.clone();
        app.cur = (1, 0);
        app.anchor = Some((1, 1));
        app.copy(true);
        app.cur = (1, 2);
        app.anchor = None;
        app.paste();
        assert_refused(&app, &before);
        assert!(app.clip.as_ref().is_some_and(|c| c.cut));
    }

    #[test]
    fn typing_into_part_of_a_cse_block_is_refused_and_keeps_the_editor() {
        let mut app = app_with_spilling_cse();
        let before = app.sheet().cells.clone();
        app.cur = (1, 3);
        app.start_edit(Some('9'));
        assert!(!app.commit_edit());
        assert_eq!(app.edit.as_ref().map(|e| e.text.as_str()), Some("9"));
        assert_refused(&app, &before);
    }

    #[test]
    fn a_fill_or_clear_over_part_of_a_cse_block_is_refused_whole() {
        // Ctrl-D over C2:D4 would write C3:C4 and D3:D4; D3 is the block's.
        let mut app = app_with_spilling_cse();
        app.pkg.workbook.sheets[0].set_cell(1, 2, Cell::number(5.0));
        let before = app.sheet().cells.clone();
        app.cur = (3, 3);
        app.anchor = Some((1, 2));
        app.fill(FillDir::Down);
        assert_refused(&app, &before);
        app.cur = (2, 3);
        app.anchor = Some((1, 2));
        app.clear_selection();
        assert_refused(&app, &before);
        // All of it, anchor included, clears.
        app.cur = (0, 3);
        app.anchor = Some((2, 3));
        app.clear_selection();
        assert!((0..3).all(|r| app.sheet().cell(r, 3).is_none_or(|c| c.value.is_empty())));
        assert_eq!(app.undo.len(), 1);
    }

    #[test]
    fn a_spilling_cse_block_pasted_in_place_still_spills() {
        // #825 AC4: copy (or cut) D1:D3 and paste it back at D1. Written cell
        // by cell, the 4 pasted into D2 blocked D1 (#SPILL!). Undo puts back
        // exactly what was there, and redo the paste, both still spilling.
        for cut in [false, true] {
            let mut app = app_with_spilling_cse();
            let before = app.sheet().cells.clone();
            app.cur = (0, 3);
            app.anchor = Some((2, 3));
            app.copy(cut);
            app.cur = (0, 3);
            app.anchor = None;
            app.paste();
            assert_cse_spills(&app, &format!("paste, cut {cut}"));
            let after = app.sheet().cells.clone();
            app.undo();
            assert_cse_spills(&app, &format!("undo, cut {cut}"));
            assert_eq!(app.sheet().cells, before, "undo, cut {cut}");
            app.redo();
            assert_cse_spills(&app, &format!("redo, cut {cut}"));
            assert_eq!(app.sheet().cells, after, "redo, cut {cut}");
        }
    }

    #[test]
    fn a_group_that_blocks_its_own_spill_undoes_and_redoes_exactly() {
        // #825 AC9: one edit types a spilling D1 and then a 5 into D2, which
        // blocks it. Snapshotted cell by cell, D1's after still claimed D2,
        // so redo blanked the 5 and spilled over it.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.apply(
            (0..3)
                .map(|r| (r, 0, Cell::number(f64::from(r + 1))))
                .collect(),
        );
        let before = app.sheet().cells.clone();
        app.apply(vec![
            (0, 3, Cell::formula("A1:A3*2")),
            (1, 3, Cell::number(5.0)),
        ]);
        let blocked = |app: &App, when: &str| {
            let d1 = app.sheet().cell(0, 3).unwrap();
            assert_eq!(d1.value, CellValue::Error("#SPILL!".into()), "{when}");
            assert_eq!(d1.spill, None, "{when}");
            assert_eq!(
                app.sheet().cell(1, 3).unwrap().value,
                CellValue::Number(5.0),
                "{when}"
            );
        };
        blocked(&app, "edit");
        let after = app.sheet().cells.clone();
        app.undo();
        assert_eq!(app.sheet().cells, before, "undo");
        app.redo();
        blocked(&app, "redo");
        assert_eq!(app.sheet().cells, after, "redo");
    }

    #[test]
    fn pasting_a_cse_block_in_place_keeps_the_block() {
        // Copy or cut D1 and paste it straight back: it lands on its own
        // block, which stays D1:D3, and its loaded text is not reprinted
        // (spaces and `_xlfn.` prefixes survive).
        for cut in [false, true] {
            for src in ["SUM(A1:A3)", "SUM(A1:A3) * 2", "_xlfn.SINGLE(A1:A3)"] {
                let mut pkg = new_xlsx();
                let mut block = cse_sum_block();
                block.formula = Some(src.into());
                pkg.workbook.sheets[0].set_cell(0, 3, block);
                let mut app = App::new(pkg, "t.xlsx");
                app.os_clip = None;
                app.cur = (0, 3);
                app.anchor = None;
                app.copy(cut);
                app.paste();
                let d1 = app.sheet().cell(0, 3).unwrap();
                assert_eq!(
                    d1.f_attrs.as_deref(),
                    Some(" t=\"array\" ref=\"D1:D3\""),
                    "cut: {cut}, {src}"
                );
                assert_eq!(d1.formula.as_deref(), Some(src), "cut: {cut}");
            }
        }
    }

    #[test]
    fn cell_formatting_applies_and_round_trips() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        app.start_edit(Some('5'));
        assert!(app.commit_edit());
        app.cur = (0, 0);
        app.anchor = None;
        // Bold + right align + a percent format via the picker.
        app.toggle_bold();
        app.set_align(Align::Right);
        app.open_picker(PickKind::NumberFormat);
        // Select "Percent  0%".
        let pct = NUMFMT_OPTIONS
            .iter()
            .position(|(l, _)| *l == "Percent  0%")
            .unwrap();
        app.format_picker.as_mut().unwrap().sel = pct;
        app.apply_picker();

        let xf = {
            let cell = app.sheet().cell(0, 0).unwrap();
            app.pkg.workbook.styles.xf(cell.style)
        };
        assert!(xf.bold);
        assert_eq!(xf.align, Align::Right);
        assert_eq!(xf.code.as_deref(), Some("0%"));

        // Undo peels back the number format (last op).
        app.undo();
        let xf = {
            let cell = app.sheet().cell(0, 0).unwrap();
            app.pkg.workbook.styles.xf(cell.style)
        };
        assert_ne!(xf.code.as_deref(), Some("0%"));

        // Reapply, save, reload: formatting persists.
        app.open_picker(PickKind::NumberFormat);
        app.format_picker.as_mut().unwrap().sel = pct;
        app.apply_picker();
        let reloaded = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let s = &reloaded.workbook.sheets[0];
        let xf = reloaded.workbook.styles.xf(s.cell(0, 0).unwrap().style);
        assert!(xf.bold);
        assert_eq!(xf.align, Align::Right);
        assert_eq!(xf.code.as_deref(), Some("0%"));
    }

    #[test]
    fn format_dialog_applies_each_section() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        app.start_edit(Some('5'));
        assert!(app.commit_edit());
        app.cur = (0, 0);
        app.anchor = None;

        app.open_format_dialog();
        assert!(app.format_dialog.is_some());

        // Apply one option from each of four sections; the dialog stays open.
        let set = |app: &mut App, section: usize, sel: usize| {
            let d = app.format_dialog.as_mut().unwrap();
            d.section = section;
            d.sel = sel;
            app.apply_format_dialog();
        };
        let pct = NUMFMT_OPTIONS
            .iter()
            .position(|(l, _)| *l == "Percent  0%")
            .unwrap();
        set(&mut app, 0, pct); // Number: Percent 0%
        set(&mut app, 1, 0); // Font: Bold
        set(&mut app, 3, 1); // Align: Center
        set(&mut app, 4, 0); // Border: box

        let xf = {
            let c = app.sheet().cell(0, 0).unwrap();
            app.pkg.workbook.styles.xf(c.style)
        };
        assert_eq!(xf.code.as_deref(), Some("0%"));
        assert!(xf.bold);
        assert_eq!(xf.align, Align::Center);
        assert!(xf.border);

        // Section navigation wraps; Esc closes.
        app.format_dialog.as_mut().unwrap().section = 0;
        app.format_dialog_key(KeyCode::Right);
        assert_eq!(app.format_dialog.as_ref().unwrap().section, 1);
        app.format_dialog_key(KeyCode::Left);
        app.format_dialog_key(KeyCode::Left);
        assert_eq!(
            app.format_dialog.as_ref().unwrap().section,
            FMT_SECTIONS.len() - 1
        );
        app.format_dialog_key(KeyCode::Esc);
        assert!(app.format_dialog.is_none());

        // Formatting persists across save/reload.
        let re = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let xf = re
            .workbook
            .styles
            .xf(re.workbook.sheets[0].cell(0, 0).unwrap().style);
        assert!(xf.bold && xf.align == Align::Center && xf.code.as_deref() == Some("0%"));
    }

    #[test]
    fn sort_region_orders_by_column() {
        use gridcore::sheet::{Cell, CellValue};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Item"));
            sh.set_cell(0, 1, Cell::text("Qty"));
            for (r, (name, qty)) in [("B", 3.0), ("A", 1.0), ("C", 2.0)].iter().enumerate() {
                sh.set_cell(r as u32 + 1, 0, Cell::text(name));
                sh.set_cell(r as u32 + 1, 1, Cell::number(*qty));
            }
        }
        app.rebuild_engine();
        app.cur = (1, 1); // a data cell in the Qty column
        app.anchor = None;
        app.sort_region(true); // ascending by Qty

        let v = |r, c| app.sheet().cell(r, c).unwrap().value.clone();
        assert_eq!(v(0, 0), CellValue::Text("Item".into())); // header stays put
        // Qty ascending 1,2,3 => rows A, C, B.
        assert_eq!(v(1, 0), CellValue::Text("A".into()));
        assert_eq!(v(1, 1), CellValue::Number(1.0));
        assert_eq!(v(2, 0), CellValue::Text("C".into()));
        assert_eq!(v(3, 0), CellValue::Text("B".into()));

        app.sort_region(false); // descending => B, C, A
        let v = |r, c| app.sheet().cell(r, c).unwrap().value.clone();
        assert_eq!(v(1, 0), CellValue::Text("B".into()));
        assert_eq!(v(3, 0), CellValue::Text("A".into()));
    }

    #[test]
    fn a_sort_across_a_spill_is_refused() {
        // #840: rows that cut a spilled array don't sort; the status says
        // why and no undo step is pushed. A1:A3 = 3, 1, 2 beside C1
        // `=SEQUENCE(3)`.
        use gridcore::sheet::{Cell, CellValue};
        let mut app = app_with_sequence_in_c1();
        for (r, n) in [3.0, 1.0, 2.0].iter().enumerate() {
            app.pkg.workbook.sheets[0].set_cell(r as u32, 0, Cell::number(*n));
        }
        app.rebuild_engine();
        let before = app.sheet().cells.clone();
        let undo = app.undo.len();
        app.cur = (0, 0);
        app.anchor = None;
        app.sort_region(true);
        assert_eq!(app.status.as_deref(), Some(gridcore::edit::SORT_CUTS_SPILL));
        app.status = None;
        app.commit_sort("A desc");
        assert_eq!(app.status.as_deref(), Some(gridcore::edit::SORT_CUTS_SPILL));
        assert_eq!(app.sheet().cells, before);
        assert_eq!(app.undo.len(), undo);
        assert_eq!(
            app.sheet().cell(0, 0).unwrap().value,
            CellValue::Number(3.0)
        );
    }

    #[test]
    fn commit_sort_multi_level() {
        use gridcore::sheet::{Cell, CellValue};
        assert_eq!(
            gridcore::edit::parse_sort_spec("A, B desc"),
            Some(vec![(0, true), (1, false)])
        );
        assert_eq!(gridcore::edit::parse_sort_spec("bad3"), None);
        assert_eq!(gridcore::edit::parse_sort_spec(""), None);

        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Grp"));
            sh.set_cell(0, 1, Cell::text("Score"));
            for (i, (g, sc)) in [("B", 10.0), ("A", 5.0), ("B", 20.0), ("A", 8.0)]
                .iter()
                .enumerate()
            {
                sh.set_cell(i as u32 + 1, 0, Cell::text(g));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*sc));
            }
        }
        app.rebuild_engine();
        app.cur = (1, 0);
        app.anchor = None;
        app.commit_sort("A asc, B desc"); // Grp asc, then Score desc
        let v = |r, c| app.sheet().cell(r, c).unwrap().value.clone();
        assert_eq!(v(0, 0), CellValue::Text("Grp".into())); // header kept
        assert_eq!(
            (v(1, 0), v(1, 1)),
            (CellValue::Text("A".into()), CellValue::Number(8.0))
        );
        assert_eq!(
            (v(2, 0), v(2, 1)),
            (CellValue::Text("A".into()), CellValue::Number(5.0))
        );
        assert_eq!(
            (v(3, 0), v(3, 1)),
            (CellValue::Text("B".into()), CellValue::Number(20.0))
        );
        assert_eq!(
            (v(4, 0), v(4, 1)),
            (CellValue::Text("B".into()), CellValue::Number(10.0))
        );
    }

    #[test]
    fn autosum_sums_the_run_above() {
        use gridcore::sheet::{Cell, CellValue};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::number(2.0));
            sh.set_cell(1, 0, Cell::number(4.0));
            sh.set_cell(2, 0, Cell::number(6.0));
        }
        app.rebuild_engine();
        app.cur = (3, 0); // directly below the numbers
        app.anchor = None;
        app.autosum();

        let c = app.sheet().cell(3, 0).unwrap();
        assert_eq!(c.formula.as_deref(), Some("SUM(A1:A3)"));
        assert_eq!(c.value, CellValue::Number(12.0));
    }

    #[test]
    fn parse_cf_input_operators() {
        assert_eq!(
            parse_cf_input(">500"),
            Some(("greaterThan", "500".into(), None))
        );
        assert_eq!(
            parse_cf_input("<=100"),
            Some(("lessThanOrEqual", "100".into(), None))
        );
        assert_eq!(parse_cf_input("<>0"), Some(("notEqual", "0".into(), None)));
        assert_eq!(parse_cf_input("=42"), Some(("equal", "42".into(), None)));
        assert_eq!(
            parse_cf_input("42"),
            Some(("greaterThan", "42".into(), None))
        );
        assert_eq!(
            parse_cf_input("100..500"),
            Some(("between", "100".into(), Some("500".into())))
        );
        assert_eq!(parse_cf_input("   "), None);
    }

    #[test]
    fn cond_format_highlights_matching_cells() {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::number(100.0));
            sh.set_cell(1, 0, Cell::number(900.0));
        }
        app.rebuild_engine();
        app.cur = (0, 0);
        app.anchor = Some((1, 0)); // A1:A2
        app.commit_cond_format(">500");

        assert_eq!(app.pkg.workbook.sheets[0].cond_formats.len(), 1);
        assert!(gridcore::cf::cell_dxf(&app.pkg.workbook, 0, 0, 0).is_none());
        let d = gridcore::cf::cell_dxf(&app.pkg.workbook, 0, 1, 0).expect("900 > 500 matches");
        assert_eq!(d.fill, Some((0xFF, 0xC7, 0xCE)));

        // Round-trips.
        let re = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        assert!(gridcore::cf::cell_dxf(&re.workbook, 0, 1, 0).is_some());
        assert!(gridcore::cf::cell_dxf(&re.workbook, 0, 0, 0).is_none());
    }

    fn ttc_app(cells: &[(u32, u32, &str)]) -> App {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        for (r, c, t) in cells {
            app.pkg.workbook.sheets[0].set_cell(*r, *c, Cell::text(t));
        }
        app.rebuild_engine();
        app
    }

    /// #692: Data › Text to Columns opens the wizard; Finish converts.
    #[test]
    fn text_to_columns_runs_through_the_wizard() {
        use gridcore::sheet::CellValue;
        let mut app = ttc_app(&[(0, 0, "Pen,4,\"Blue, fine\",0012")]);
        app.cur = (0, 0);
        app.anchor = None;
        app.ribbon_act(ribbon::Act::TextToColumns);
        let d = app.text_dialog.as_mut().expect("wizard open");
        assert!(!d.is_import());
        d.goto_step(1);
        d.key(KeyCode::Char(' ')); // untick Tab
        d.key(KeyCode::Down);
        d.key(KeyCode::Down);
        d.key(KeyCode::Char(' ')); // Comma
        d.goto_step(2);
        d.key(KeyCode::Right);
        d.key(KeyCode::Right);
        d.key(KeyCode::Right); // column 4
        d.key(KeyCode::Char('t'));
        app.text_dialog_key(KeyCode::Enter);
        assert!(app.text_dialog.is_none());
        let sh = app.sheet();
        assert_eq!(sh.cell(0, 0).unwrap().value, CellValue::Text("Pen".into()));
        assert_eq!(sh.cell(0, 1).unwrap().value, CellValue::Number(4.0));
        assert_eq!(
            sh.cell(0, 2).unwrap().value,
            CellValue::Text("Blue, fine".into())
        );
        assert_eq!(sh.cell(0, 3).unwrap().value, CellValue::Text("0012".into()));
        // One undo restores the column.
        app.undo();
        assert_eq!(
            app.sheet().cell(0, 0).unwrap().value,
            CellValue::Text("Pen,4,\"Blue, fine\",0012".into())
        );
    }

    /// #692: data in the way asks first; No changes nothing, Yes converts.
    #[test]
    fn text_to_columns_asks_before_replacing_data() {
        use gridcore::sheet::CellValue;
        let cells = [(0, 0, "a\tb"), (0, 1, "keep")];
        for (answer, b1) in [(KeyCode::Char('n'), "keep"), (KeyCode::Char('y'), "b")] {
            let mut app = ttc_app(&cells);
            app.cur = (0, 0);
            app.ribbon_act(ribbon::Act::TextToColumns);
            app.text_dialog_key(KeyCode::Enter);
            let c = app.confirm.as_ref().expect("asks first");
            assert_eq!(c.prompt(), gridcore::edit::TTC_REPLACE);
            app.confirm_key(KeyEvent::new(answer, KeyModifiers::NONE));
            assert!(app.confirm.is_none());
            assert_eq!(
                app.sheet().cell(0, 1).unwrap().value,
                CellValue::Text(b1.into())
            );
        }
    }

    /// The wizard refuses Finish while the decimal and thousands separators
    /// are the same character, and stays open.
    #[test]
    fn the_wizard_refuses_equal_separators() {
        let mut app = ttc_app(&[(0, 0, "1.5")]);
        app.ribbon_act(ribbon::Act::TextToColumns);
        app.text_dialog.as_mut().unwrap().thousands = '.';
        app.text_dialog_key(KeyCode::Enter);
        assert!(app.text_dialog.is_some());
        assert_eq!(
            app.status.as_deref(),
            Some("The decimal and thousands separators must differ")
        );
        assert_eq!(
            app.sheet().cell(0, 0).unwrap().value,
            gridcore::sheet::CellValue::Text("1.5".into())
        );
    }

    /// #692: more than one column is refused with Excel's message.
    #[test]
    fn text_to_columns_refuses_two_columns() {
        let mut app = ttc_app(&[(0, 0, "a,b"), (0, 1, "c")]);
        app.cur = (0, 1);
        app.anchor = Some((0, 0));
        app.ribbon_act(ribbon::Act::TextToColumns);
        assert!(app.text_dialog.is_none());
        assert_eq!(app.status.as_deref(), Some(gridcore::edit::TTC_ONE_COLUMN));
    }

    /// #607: opening a .txt shows the wizard; Finish imports it with the
    /// chosen formats; Esc leaves the workbook as it was.
    #[test]
    fn opening_a_text_file_runs_the_import_wizard() {
        use gridcore::sheet::CellValue;
        let dir = std::env::temp_dir().join(format!("xlsxy-wizard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("parts.txt");
        std::fs::write(&path, "02134\t03/04/2024\t1.234,5-\tx\r\n").unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.open_workbook(path.to_str().unwrap());
        assert!(app.text_dialog.as_ref().is_some_and(|d| d.is_import()));
        app.text_dialog_key(KeyCode::Esc);
        assert!(app.text_dialog.is_none());
        assert_eq!(app.path, "untitled.xlsx");

        app.open_workbook(path.to_str().unwrap());
        let d = app.text_dialog.as_mut().unwrap();
        d.goto_step(2);
        d.columns = vec![
            gridcore::textio::ColFormat::Text,
            gridcore::textio::ColFormat::Date(gridcore::textio::DateOrder::Dmy),
            gridcore::textio::ColFormat::General,
            gridcore::textio::ColFormat::Skip,
        ];
        d.decimal = ',';
        d.thousands = '.';
        app.text_dialog_key(KeyCode::Enter);
        assert!(app.text_dialog.is_none());
        assert_eq!(Path::new(&app.path), path.with_extension("xlsx"));
        assert_eq!(app.import_source.as_deref(), path.to_str());
        let sh = app.sheet();
        assert_eq!(
            sh.cell(0, 0).unwrap().value,
            CellValue::Text("02134".into())
        );
        let apr3 = gridcore::sheet::parts_to_serial(2024, 4, 3, 0, false);
        assert_eq!(sh.cell(0, 1).unwrap().value, CellValue::Number(apr3));
        assert_eq!(sh.cell(0, 2).unwrap().value, CellValue::Number(-1234.5));
        assert!(sh.cell(0, 3).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wizard_draws_over_the_grid() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut app = ttc_app(&[(0, 0, "a,b")]);
        app.ribbon_act(ribbon::Act::TextToColumns);
        let mut term = Terminal::new(TestBackend::new(100, 34)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(
            text.contains("Convert Text to Columns Wizard"),
            "title missing"
        );
    }

    #[test]
    fn remove_duplicates_dedupes_region() {
        use gridcore::sheet::{Cell, CellValue};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Item"));
            sh.set_cell(1, 0, Cell::text("A"));
            sh.set_cell(2, 0, Cell::text("B"));
            sh.set_cell(3, 0, Cell::text("A")); // duplicate of row 2
        }
        app.rebuild_engine();
        app.cur = (1, 0);
        app.anchor = None;
        app.remove_duplicates();
        let sh = app.sheet();
        assert_eq!(sh.cell(1, 0).unwrap().value, CellValue::Text("A".into()));
        assert_eq!(sh.cell(2, 0).unwrap().value, CellValue::Text("B".into()));
        assert!(sh.cell(3, 0).map(|c| c.value.is_empty()).unwrap_or(true));
    }

    #[test]
    fn format_as_table_wraps_region() {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Item"));
            sh.set_cell(0, 1, Cell::text("Qty"));
            sh.set_cell(1, 0, Cell::text("Pen"));
            sh.set_cell(1, 1, Cell::number(3.0));
            sh.set_cell(2, 0, Cell::text("Pad"));
            sh.set_cell(2, 1, Cell::number(5.0));
        }
        app.rebuild_engine();
        app.cur = (1, 0); // inside the block, no explicit selection
        app.anchor = None;
        app.format_as_table();
        assert_eq!(app.pkg.workbook.tables.len(), 1);
        let t = &app.pkg.workbook.tables[0];
        assert_eq!(t.range, (0, 0, 2, 1)); // grew to A1:B3
        assert_eq!(t.header_rows, 1);
        assert_eq!(t.columns, vec!["Item", "Qty"]);
        // Survives a save/reload.
        let re = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&app.pkg)).unwrap();
        assert_eq!(re.workbook.tables.len(), 1);
    }

    #[test]
    fn protection_toggles_and_blocks_edits() {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("keep"));
        app.rebuild_engine();
        app.cur = (0, 0);
        app.anchor = None;
        // Protect: editing is now blocked.
        app.toggle_protection();
        assert!(app.protected());
        app.start_edit(Some('x'));
        assert!(app.edit.is_none(), "edit must be blocked while protected");
        app.clear_selection();
        assert_eq!(
            app.sheet().cell(0, 0).unwrap().value,
            gridcore::sheet::CellValue::Text("keep".into())
        );
        // Unprotect: editing works again.
        app.toggle_protection();
        assert!(!app.protected());
        app.start_edit(Some('y'));
        assert!(app.edit.is_some());
        // Protection survives save/reload.
        app.pkg.workbook.sheets[0].set_protected(true);
        let re = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&app.pkg)).unwrap();
        assert!(re.workbook.sheets[0].is_protected());
    }

    #[test]
    fn wrap_text_renders_across_multiple_lines() {
        use gridcore::sheet::{Cell, Xf};
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        // A wrapped cell with text longer than its column.
        let idx = app.pkg.workbook.styles.intern(Xf {
            wrap: true,
            ..Default::default()
        });
        app.pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                value: gridcore::sheet::CellValue::Text("alpha beta gamma delta".into()),
                style: idx,
                ..Cell::default()
            },
        );
        app.rebuild_engine();

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        // Row 0 now spans several screen lines (sub-line indices 0,1,2…).
        let n0 = app.vis_rows.iter().filter(|&&r| r == 0).count();
        assert!(
            n0 >= 2,
            "wrapped row should occupy multiple lines, got {n0}"
        );
        assert_eq!(app.vis_subline[0], 0);
        assert!(app.vis_subline.iter().any(|&s| s >= 1));

        // An explicit row height also makes a plain row taller.
        app.toggle_wrap(); // turn wrap back off on A1
        app.cur = (5, 0);
        app.anchor = None;
        app.commit_row_height("45");
        term.draw(|f| draw(&mut app, f)).unwrap();
        assert!(app.vis_rows.iter().filter(|&&r| r == 5).count() >= 2);
        assert_eq!(app.pkg.workbook.sheets[0].row_height(5), Some(45.0));
    }

    /// East and West (labels over A1:B3) and an empty Summary, the cursor
    /// on Summary!A1.
    fn consolidate_app() -> App {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].name = "East".into();
        pkg.add_sheet("West");
        pkg.add_sheet("Summary");
        for (s, rows) in [
            (0, [("A", 1.0), ("B", 2.0)]),
            (1, [("b", 10.0), ("C", 20.0)]),
        ] {
            let sh = &mut pkg.workbook.sheets[s];
            sh.set_cell(0, 1, Cell::text("Jan"));
            for (i, (k, v)) in rows.iter().enumerate() {
                sh.set_cell(i as u32 + 1, 0, Cell::text(k));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*v));
            }
        }
        let mut app = App::new(pkg, "Sales.xlsx");
        app.os_clip = None;
        app.sheet = 2;
        app.cur = (0, 0);
        app.anchor = None;
        app.rebuild_engine();
        app
    }

    fn consolidate_dialog(app: &mut App) -> &mut outlinedlg::ConsolidateDialog {
        match &mut app.outline_dialog {
            Some(outlinedlg::Dialog::Consolidate(d)) => d,
            other => panic!("the Consolidate dialog opens: {other:?}"),
        }
    }

    #[test]
    fn consolidate_ok_is_one_undo_step_and_the_dialog_remembers() {
        use gridcore::sheet::CellValue;
        let mut app = consolidate_app();
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::Consolidate);
        let d = consolidate_dialog(&mut app);
        d.refs = vec!["East!A1:B3".into(), "west!a1:b3".into()];
        (d.top_row, d.left_col, d.links) = (true, true, true);
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        assert_eq!(app.undo.len(), undo_len + 1);
        let v = |app: &App, r, c| app.sheet().cell(r, c).map(|cl| cl.value.clone());
        // Jan over the data column; A (East), B (East + West's b), C (West).
        assert_eq!(v(&app, 0, 2), Some(CellValue::Text("Jan".into())));
        assert_eq!(
            v(&app, 1, 1),
            Some(CellValue::Text("Sales".into())),
            "the book's name"
        );
        assert_eq!(v(&app, 2, 0), Some(CellValue::Text("A".into())));
        assert_eq!(v(&app, 2, 2), Some(CellValue::Number(1.0)), "recalculated");
        assert_eq!(v(&app, 5, 2), Some(CellValue::Number(12.0)));
        assert!(app.sheet().row_hidden(1) && app.sheet().row_collapsed(2));
        // Reopened, the dialog starts from what OK kept.
        app.ribbon_act(ribbon::Act::Consolidate);
        let d = consolidate_dialog(&mut app);
        assert_eq!(d.refs, ["East!$A$1:$B$3", "West!$A$1:$B$3"]);
        assert!(d.top_row && d.left_col && d.links);
        app.outline_dialog_key(KeyCode::Esc);
        app.undo();
        assert_eq!(app.sheet().cell(0, 2), None);
        assert_eq!(app.sheet().consolidate, None);
        assert_eq!(app.sheet().max_row_outline(), 0);
    }

    #[test]
    fn consolidate_refusal_keeps_the_dialog_open() {
        let mut app = consolidate_app();
        let undo_len = app.undo.len();
        let before = app.pkg.workbook.sheets.clone();
        app.ribbon_act(ribbon::Act::Consolidate);
        let d = consolidate_dialog(&mut app);
        d.refs = vec!["East!A1:B3".into(), "A1:B3".into()];
        d.links = true;
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_some());
        assert_eq!(
            app.status.as_deref(),
            Some(
                gridcore::edit::ConsolidateError::LinksOnDestSheet
                    .to_string()
                    .as_str()
            )
        );
        assert!(!gridcore::edit::sheets_differ(
            &before,
            &app.pkg.workbook.sheets
        ));
        assert_eq!(app.undo.len(), undo_len);
        // Nothing to consolidate: refused too.
        consolidate_dialog(&mut app).refs.clear();
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_some());
        assert_eq!(
            app.status.as_deref(),
            Some(
                gridcore::edit::ConsolidateError::NoRefs
                    .to_string()
                    .as_str()
            )
        );
        // A protected sheet refuses it like Subtotal.
        app.outline_dialog_key(KeyCode::Esc);
        app.pkg.workbook.sheets[2].set_protected(true);
        app.ribbon_act(ribbon::Act::Consolidate);
        consolidate_dialog(&mut app).refs = vec!["East!A1:B3".into()];
        app.outline_dialog_key(KeyCode::Enter);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|s| s.contains("protected"))
        );
        assert_eq!(app.sheet().cell(0, 0), None);
        assert_eq!(app.undo.len(), undo_len);
    }

    #[test]
    fn space_on_the_function_row_does_not_run_consolidate() {
        let mut app = consolidate_app();
        let before = app.pkg.workbook.sheets.clone();
        app.ribbon_act(ribbon::Act::Consolidate);
        consolidate_dialog(&mut app).refs = vec!["East!A1:B3".into()];
        app.outline_dialog_key(KeyCode::Char(' '));
        assert!(app.outline_dialog.is_some());
        assert!(!gridcore::edit::sheets_differ(
            &before,
            &app.pkg.workbook.sheets
        ));
        // Enter there is OK.
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        assert!(gridcore::edit::sheets_differ(
            &before,
            &app.pkg.workbook.sheets
        ));
    }

    /// Grp/Amt with two A rows and one B row (A1:B4), cursor in the data.
    fn subtotal_app() -> App {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Grp"));
            sh.set_cell(0, 1, Cell::text("Amt"));
            for (i, (g, a)) in [("A", 1.0), ("A", 2.0), ("B", 4.0)].iter().enumerate() {
                sh.set_cell(i as u32 + 1, 0, Cell::text(g));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*a));
            }
        }
        app.rebuild_engine();
        app.cur = (1, 0); // group by column A
        app.anchor = None;
        app
    }

    #[test]
    fn subtotal_and_outline_collapse() {
        use gridcore::sheet::CellValue;
        let mut app = subtotal_app();
        app.ribbon_act(ribbon::Act::Subtotal);
        let Some(outlinedlg::Dialog::Subtotal(d)) = &app.outline_dialog else {
            panic!("the Subtotal dialog opens");
        };
        assert_eq!((d.area, d.has_header), ((0, 0, 3, 1), true));
        assert_eq!(d.options().add_to, [1], "the numeric column is checked");
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        let v = |app: &App, r, c| app.sheet().cell(r, c).map(|cl| cl.value.clone());
        assert_eq!(v(&app, 3, 0), Some(CellValue::Text("A Total".into())));
        assert_eq!(v(&app, 6, 0), Some(CellValue::Text("Grand Total".into())));
        assert_eq!(v(&app, 6, 1), Some(CellValue::Number(7.0)));
        let levels: Vec<u8> = (0..7).map(|r| app.sheet().row_outline(r)).collect();
        assert_eq!(levels, [0, 2, 2, 1, 2, 1, 0]);

        // Level 2 hides the detail; level 3 shows it; each is one undo step.
        app.outline_show_level(Axis::Rows, 2);
        assert!(app.sheet().row_hidden(1) && !app.sheet().row_hidden(3));
        app.outline_show_level(Axis::Rows, 3);
        assert!(!app.sheet().row_hidden(1));
        app.undo();
        assert!(app.sheet().row_hidden(1));
        app.undo();
        assert!(!app.sheet().row_hidden(1));
        // Undo takes the subtotals out too.
        app.undo();
        assert_eq!(v(&app, 3, 0), Some(CellValue::Text("B".into())));
        assert_eq!(app.sheet().max_row_outline(), 0);
    }

    #[test]
    fn subtotal_dialog_remove_all_and_refusal() {
        let mut app = subtotal_app();
        app.ribbon_act(ribbon::Act::Subtotal);
        app.outline_dialog_key(KeyCode::Enter);
        assert_eq!(app.sheet().used_size().0, 7);
        // Reopen at a total row: Remove All takes them out again.
        app.cur = (3, 0);
        app.ribbon_act(ribbon::Act::Subtotal);
        if let Some(outlinedlg::Dialog::Subtotal(d)) = &mut app.outline_dialog {
            d.focus = 0;
        }
        app.outline_dialog_key(KeyCode::Up); // Cancel
        app.outline_dialog_key(KeyCode::Up); // Remove All
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        assert_eq!(app.sheet().used_size().0, 4);
        assert_eq!(app.sheet().max_row_outline(), 0);
        // No column checked: the dialog says so and stays open; nothing to undo.
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::Subtotal);
        if let Some(outlinedlg::Dialog::Subtotal(d)) = &mut app.outline_dialog {
            d.add_to.iter_mut().for_each(|on| *on = false);
        }
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_some());
        assert_eq!(
            app.status.as_deref(),
            Some(
                gridcore::edit::SubtotalError::NoColumns
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(app.undo.len(), undo_len);
        app.outline_dialog_key(KeyCode::Esc);
        assert!(app.outline_dialog.is_none());
    }

    #[test]
    fn an_outline_dialog_acts_on_its_own_sheet_and_holds_the_mouse() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = subtotal_app();
        app.pkg.workbook.sheets.push(Sheet {
            name: "Sheet2".into(),
            ..Sheet::default()
        });
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        app.ribbon_act(ribbon::Act::Subtotal);
        // A click on the Sheet2 tab under the dialog does nothing.
        let &(_, x, _) = app.tab_spans.iter().find(|&&(i, _, _)| i == 1).unwrap();
        let y = app.grid_area.y + app.grid_area.height;
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::empty(),
            },
        );
        assert_eq!(app.sheet, 0);
        assert!(app.outline_dialog.is_some());
        // Even with another sheet active, OK lands where the dialog opened.
        app.sheet = 1;
        app.outline_dialog_key(KeyCode::Enter);
        assert_eq!(app.pkg.workbook.sheets[0].max_row_outline(), 2);
        assert!(app.pkg.workbook.sheets[1].cells.is_empty());
        assert_eq!(app.pkg.workbook.sheets[1].max_row_outline(), 0);
    }

    #[test]
    fn remove_all_records_the_page_breaks_it_drops() {
        let mut app = subtotal_app();
        gridcore::print::area::insert_page_break(&mut app.pkg.workbook.sheets[0], 2, 0);
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::Subtotal);
        app.outline_dialog_key(KeyCode::Up); // Cancel
        app.outline_dialog_key(KeyCode::Up); // Remove All
        app.outline_dialog_key(KeyCode::Enter);
        assert!(
            gridcore::print::area::manual_breaks(app.sheet())
                .0
                .is_empty()
        );
        assert_eq!(app.undo.len(), undo_len + 1);
        app.undo();
        assert_eq!(gridcore::print::area::manual_breaks(app.sheet()).0, [2]);
        // Redo removes them again; then there is nothing left, so no step.
        app.redo();
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::Subtotal);
        app.outline_dialog_key(KeyCode::Up);
        app.outline_dialog_key(KeyCode::Up);
        app.outline_dialog_key(KeyCode::Enter);
        assert_eq!(app.undo.len(), undo_len);
    }

    #[test]
    fn alt_shift_arrows_group_whole_rows_and_columns() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        // Whole rows 2..=4 selected.
        app.anchor = Some((1, 0));
        app.cur = (3, MAX_COLS - 1);
        press_mod(
            &mut app,
            KeyCode::Right,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        let levels: Vec<u8> = (0..5).map(|r| app.sheet().row_outline(r)).collect();
        assert_eq!(levels, [0, 1, 1, 1, 0]);
        assert!(app.outline_dialog.is_none());
        press_mod(
            &mut app,
            KeyCode::Right,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(app.sheet().row_outline(2), 2);
        press_mod(
            &mut app,
            KeyCode::Left,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(app.sheet().row_outline(2), 1);
        // Whole columns C..D.
        app.anchor = Some((0, 2));
        app.cur = (MAX_ROWS - 1, 3);
        press_mod(
            &mut app,
            KeyCode::Right,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(
            (app.sheet().col_outline(2), app.sheet().col_outline(3)),
            (1, 1)
        );
        // Plain Shift+Right still extends the selection.
        app.anchor = None;
        app.cur = (5, 5);
        press_mod(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        assert_eq!((app.anchor, app.cur), (Some((5, 5)), (5, 6)));
        assert_eq!(app.sheet().col_outline(5), 0);
    }

    #[test]
    fn group_on_a_cell_range_asks_rows_or_columns() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.anchor = Some((1, 1));
        app.cur = (2, 3);
        app.ribbon_act(ribbon::Act::GroupOutline);
        assert!(matches!(
            app.outline_dialog,
            Some(outlinedlg::Dialog::Axis(_))
        ));
        app.outline_dialog_key(KeyCode::Down); // Columns
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        let cols: Vec<u8> = (0..5).map(|c| app.sheet().col_outline(c)).collect();
        assert_eq!(cols, [0, 1, 1, 1, 0]);
        assert_eq!(app.sheet().max_row_outline(), 0);
        // Ungroup, Rows: nothing grouped refuses, with no undo step.
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::UngroupOutline);
        app.outline_dialog_key(KeyCode::Enter);
        assert_eq!(app.undo.len(), undo_len);
        assert_eq!(
            app.status.as_deref(),
            Some(OutlineError::NotGrouped.to_string().as_str())
        );
        // Undo takes the column group away.
        app.undo();
        assert_eq!(app.sheet().max_col_outline(), 0);
    }

    #[test]
    fn detail_auto_and_clear_outline_from_the_ribbon() {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            for r in 0..3 {
                sh.set_cell(r, 0, Cell::number(r as f64 + 1.0));
            }
            sh.set_cell(3, 0, Cell::formula("SUM(A1:A3)"));
        }
        app.rebuild_engine();
        app.ribbon_act(ribbon::Act::AutoOutline);
        assert_eq!(app.sheet().row_outline(1), 1);
        // Hide Detail at the summary row, then Show Detail.
        app.cur = (3, 0);
        app.ribbon_act(ribbon::Act::HideDetail);
        assert!(app.sheet().row_hidden(0) && app.sheet().row_collapsed(3));
        app.ribbon_act(ribbon::Act::ShowDetail);
        assert!(!app.sheet().row_hidden(0));
        // Show Detail with nothing collapsed refuses without an undo step.
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::ShowDetail);
        assert_eq!(app.undo.len(), undo_len);
        app.ribbon_act(ribbon::Act::ClearOutline);
        assert_eq!(app.sheet().max_row_outline(), 0);
        app.undo();
        assert_eq!(app.sheet().row_outline(1), 1);
        // On a protected sheet every outline command refuses.
        app.pkg.workbook.sheets[0].set_protected(true);
        let undo_len = app.undo.len();
        app.ribbon_act(ribbon::Act::ClearOutline);
        assert_eq!(app.sheet().row_outline(1), 1);
        assert_eq!(app.undo.len(), undo_len);
        assert!(app.status.as_deref().unwrap().contains("protected"));
    }

    #[test]
    fn outline_settings_dialog_sets_the_direction() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.ribbon_act(ribbon::Act::OutlineSettings);
        app.outline_dialog_key(KeyCode::Char(' ')); // Summary rows below: off
        app.outline_dialog_key(KeyCode::Enter);
        assert!(app.outline_dialog.is_none());
        assert!(!app.sheet().outline.summary_below);
        assert!(app.sheet().outline.summary_right);
        app.undo();
        assert!(app.sheet().outline.summary_below);
    }

    #[test]
    fn outline_gutter_and_buttons_answer_the_mouse() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = subtotal_app();
        app.ribbon_act(ribbon::Act::Subtotal);
        app.outline_dialog_key(KeyCode::Enter);
        // A column group too, so both outlines show.
        outline::group(&mut app.pkg.workbook.sheets[0], Axis::Cols, 1, 1).unwrap();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        assert_eq!(app.outline_w, 3, "two row levels plus one");
        let g = app.grid_area;
        let hdr = app.col_hdr_y;
        let col_line = app.col_outline_y.expect("a column outline line");
        assert_eq!((col_line + 1, hdr + 1), (hdr, g.y));
        let screen = |term: &Terminal<TestBackend>, y: u16| -> String {
            let buf = term.backend().buffer();
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        // Level buttons 1 2 3 left of the header; the gutter shows a bar on
        // detail rows and "-" on the totals; the column line has its group.
        assert!(
            screen(&term, hdr).starts_with("123"),
            "{}",
            screen(&term, hdr)
        );
        assert!(screen(&term, col_line).starts_with("12"));
        let row_line =
            |app: &App, r: u32| g.y + app.vis_rows.iter().position(|&x| x == r).unwrap() as u16;
        assert!(
            screen(&term, row_line(&app, 1)).starts_with("││"),
            "{}",
            screen(&term, row_line(&app, 1))
        );
        assert!(
            screen(&term, row_line(&app, 3)).starts_with("│-"),
            "{}",
            screen(&term, row_line(&app, 3))
        );
        assert!(
            screen(&term, row_line(&app, 6)).starts_with("- "),
            "{}",
            screen(&term, row_line(&app, 6))
        );

        let click = |app: &mut App, x: u16, y: u16| {
            handle_mouse(
                app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::empty(),
                },
            );
        };
        // A cell click lands on that cell despite the gutter and the line.
        let (_, bx, _) = *app.vis_cols.iter().find(|&&(c, _, _)| c == 1).unwrap();
        let y = row_line(&app, 2);
        click(&mut app, bx + 1, y);
        assert_eq!(app.cur, (2, 1));
        // "-" on "A Total" (level 2) collapses its group.
        let y = row_line(&app, 3);
        click(&mut app, g.x + 1, y);
        assert!(app.sheet().row_hidden(1) && app.sheet().row_hidden(2));
        assert!(app.sheet().row_collapsed(3));
        term.draw(|f| draw(&mut app, f)).unwrap();
        assert!(screen(&term, row_line(&app, 3)).starts_with("│+"));
        let y = row_line(&app, 3);
        click(&mut app, g.x + 1, y);
        assert!(!app.sheet().row_hidden(1));
        // Level button 1 leaves only the grand total.
        click(&mut app, g.x, hdr);
        let shown: Vec<u32> = (0..7).filter(|&r| !app.sheet().row_hidden(r)).collect();
        assert_eq!(shown, [0, 6]);
        // The column line's "-" over C (the summary of B) hides column B.
        term.draw(|f| draw(&mut app, f)).unwrap();
        let (_, cx, _) = *app.vis_cols.iter().find(|&&(c, _, _)| c == 2).unwrap();
        click(&mut app, cx + 1, col_line);
        assert!(app.sheet().col_hidden(1) && app.sheet().col_collapsed(2));
        // Its level button 2 shows it again.
        click(&mut app, g.x + 1, col_line);
        assert!(!app.sheet().col_hidden(1));
    }
    #[test]
    fn commit_filter_hides_nonmatching_rows() {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Item"));
            sh.set_cell(0, 1, Cell::text("Qty"));
            for (i, (name, qty)) in [("A", 300.0), ("B", 50.0), ("C", 900.0)].iter().enumerate() {
                sh.set_cell(i as u32 + 1, 0, Cell::text(name));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*qty));
            }
        }
        app.rebuild_engine();
        app.cur = (1, 1); // Qty column, a data cell
        app.anchor = None;
        app.commit_filter(">100");
        // header visible; A(300),C(900) kept; B(50) hidden.
        let sh = app.sheet();
        assert!(!sh.row_hidden(0)); // header
        assert!(!sh.row_hidden(1)); // 300
        assert!(sh.row_hidden(2)); // 50 hidden
        assert!(!sh.row_hidden(3)); // 900
        // #678: the filter's rows are filter-hidden, so SUBTOTAL(9) leaves
        // them out while a row hidden by hand still counts.
        assert!(sh.row_filtered(2));
        app.pkg.workbook.sheets[0].set_row_hidden(3, true);
        app.pkg.workbook.sheets[0].set_cell(5, 1, Cell::formula("SUBTOTAL(9,B2:B4)"));
        app.pkg.workbook.sheets[0].set_cell(6, 1, Cell::formula("SUBTOTAL(109,B2:B4)"));
        app.rebuild_engine();
        let v = |app: &App, r: u32| app.sheet().cell(r, 1).unwrap().value.clone();
        use gridcore::sheet::CellValue;
        assert_eq!(v(&app, 5), CellValue::Number(1200.0));
        assert_eq!(v(&app, 6), CellValue::Number(300.0));
        app.pkg.workbook.sheets[0].set_row_hidden(3, false);

        // Clear unhides everything, and the rows are no longer filtered.
        app.commit_filter("clear");
        let sh = app.sheet();
        assert!(!sh.row_hidden(2));
        assert!(sh.filtered_rows.is_empty());
        assert_eq!(v(&app, 5), CellValue::Number(1250.0));

        // Text equals filter on the Item column.
        app.cur = (1, 0);
        app.commit_filter("=C");
        let sh = app.sheet();
        assert!(sh.row_hidden(1) && sh.row_hidden(2) && !sh.row_hidden(3));
    }

    #[test]
    fn commit_data_validation_creates_list() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        app.anchor = Some((4, 0)); // A1:A5
        app.commit_data_validation("Laptop, Monitor , Dock");
        let dvs = &app.pkg.workbook.sheets[0].validations;
        assert_eq!(dvs.len(), 1);
        assert_eq!(dvs[0].kind, "list");
        assert_eq!(dvs[0].formula1, "\"Laptop,Monitor,Dock\"");
        assert!(dvs[0].covers(2, 0));
        // Round-trips + the dropdown resolves the values.
        let re = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].validations.len(), 1);
    }

    #[test]
    fn cond_format_between_and_clear() {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::number(50.0));
            sh.set_cell(1, 0, Cell::number(300.0));
            sh.set_cell(2, 0, Cell::number(900.0));
        }
        app.rebuild_engine();
        app.cur = (0, 0);
        app.anchor = Some((2, 0)); // A1:A3
        app.commit_cond_format("100..500");
        // 300 is within [100,500]; 50 and 900 are not.
        assert!(gridcore::cf::cell_dxf(&app.pkg.workbook, 0, 0, 0).is_none());
        assert!(gridcore::cf::cell_dxf(&app.pkg.workbook, 0, 1, 0).is_some());
        assert!(gridcore::cf::cell_dxf(&app.pkg.workbook, 0, 2, 0).is_none());

        app.commit_cond_format("clear");
        assert!(app.pkg.workbook.sheets[0].cond_formats.is_empty());
        assert!(gridcore::cf::cell_dxf(&app.pkg.workbook, 0, 1, 0).is_none());
    }

    #[test]
    fn merge_toggle_merges_and_unmerges() {
        use gridcore::sheet::{Align, Cell};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("Title"));
        app.rebuild_engine();
        app.cur = (0, 0);
        app.anchor = Some((0, 2)); // A1:C1
        app.merge_toggle();
        assert_eq!(app.pkg.workbook.sheets[0].merges, vec![(0, 0, 0, 2)]);
        let xf = {
            let c = app.sheet().cell(0, 0).unwrap();
            app.pkg.workbook.styles.xf(c.style)
        };
        assert_eq!(xf.align, Align::Center); // anchor centred

        let re = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        assert_eq!(re.workbook.sheets[0].merges, vec![(0, 0, 0, 2)]);

        // Re-toggle on the anchor unmerges.
        app.cur = (0, 0);
        app.anchor = None;
        app.merge_toggle();
        assert!(app.pkg.workbook.sheets[0].merges.is_empty());
    }

    #[test]
    fn circular_reference_warns_once_and_shows_in_the_footer() {
        // #660: a new circle warns once (Excel's message); while any circle
        // exists the footer names one, on the active sheet first.
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.apply_on(0, vec![(0, 4, parse_input("=E1+1"))]);
        app.flush_circle_warning();
        assert_eq!(app.status.as_deref(), Some(CIRCULAR_WARNING));
        assert_eq!(
            app.sheet().cell(0, 4).unwrap().value,
            CellValue::Number(0.0)
        );
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Circular References: E1"), "{text}");

        // An edit that makes no new circle does not warn again.
        app.status = None;
        app.apply_on(0, vec![(3, 0, parse_input("7"))]);
        app.apply_on(0, vec![(0, 7, parse_input("=E1+5"))]);
        app.flush_circle_warning();
        assert_eq!(app.status, None);
        assert_eq!(app.circular_refs(), vec!["E1".to_string()]);

        // A circle on another sheet is named with its sheet, after the
        // active sheet's.
        app.pkg.workbook.sheets.push(Sheet {
            name: "My Data".into(),
            ..Sheet::default()
        });
        app.rebuild_engine();
        app.apply_on(1, vec![(0, 0, parse_input("=A1*2"))]);
        app.flush_circle_warning();
        assert_eq!(app.status.as_deref(), Some(CIRCULAR_WARNING));
        assert_eq!(
            app.circular_refs(),
            vec!["E1".to_string(), "'My Data'!A1".to_string()]
        );
        // Breaking E1's circle leaves the other sheet's in the footer.
        app.apply_on(0, vec![(0, 4, parse_input("1"))]);
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Circular References: 'My Data'!A1"), "{text}");
        app.apply_on(1, vec![(0, 0, parse_input("2"))]);
        assert!(app.circular_refs().is_empty());
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(!text.contains("Circular References"), "{text}");
    }

    #[test]
    fn opening_a_workbook_with_a_circle_reports_it() {
        // #660: an opened file's circle is known at once (footer,
        // wb.path, the open warning) without recalculating it, and moving it
        // by a structural edit does not warn again.
        use gridcore::xlsx::{load_xlsx, save_xlsx};
        use ratatui::{Terminal, backend::TestBackend};
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(
            0,
            4,
            Cell {
                value: CellValue::Number(7.0),
                ..Cell::formula("E1+1")
            },
        );
        let pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
        let mut app = App::new(pkg, "circle.xlsx");
        app.os_clip = None;
        assert_eq!(app.status.as_deref(), Some(CIRCULAR_WARNING));
        assert_eq!(app.circular_refs(), vec!["E1".to_string()]);
        // Cached, not recalculated.
        assert_eq!(
            app.sheet().cell(0, 4).unwrap().value,
            CellValue::Number(7.0)
        );
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Circular References: E1"), "{text}");

        // Insert a row above the circle: it moves to E2, no new warning.
        app.status = None;
        app.cur = (0, 0);
        app.anchor = None;
        app.row_op(true);
        app.flush_circle_warning();
        assert_eq!(app.status.as_deref(), Some("Inserted 1 row"));
        assert_eq!(app.circular_refs(), vec!["E2".to_string()]);
    }

    #[test]
    fn a_structural_edit_that_makes_a_circle_warns_after_its_own_status() {
        // #660: renaming a sheet can close a circle (Sheet1!A1 = Budget!A1,
        // and the sheet renamed to Budget reads Sheet1!A1). The warning joins
        // the rename's own status instead of being overwritten by it.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.apply_on(0, vec![(0, 0, parse_input("=Budget!A1"))]);
        app.open_prompt(PromptKind::AddSheet);
        app.prompt.as_mut().unwrap().text = "Data".to_string();
        app.commit_prompt();
        let data = app.sheet;
        app.apply_on(data, vec![(0, 0, parse_input("=Sheet1!A1"))]);
        assert!(app.circular_refs().is_empty());
        app.status = None;
        app.open_prompt(PromptKind::RenameSheet);
        app.prompt.as_mut().unwrap().text = "Budget".to_string();
        app.commit_prompt();
        app.flush_circle_warning();
        assert_eq!(
            app.status.as_deref(),
            Some(format!("Renamed sheet to Budget. {CIRCULAR_WARNING}").as_str())
        );
        assert_eq!(app.circular_refs().len(), 2);
    }

    #[test]
    fn a_paste_that_makes_a_circle_warns_after_pasted() {
        // #660: cutting A1 (=B1) and pasting it into B1 makes B1 read
        // itself; the warning follows the paste's own status.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.apply_on(0, vec![(0, 0, parse_input("=B1"))]);
        app.flush_circle_warning();
        app.cur = (0, 0);
        app.anchor = None;
        app.copy(true);
        app.cur = (0, 1);
        app.paste();
        app.flush_circle_warning();
        assert_eq!(app.circular_refs(), vec!["B1".to_string()]);
        assert_eq!(
            app.status.as_deref(),
            Some(format!("Pasted. {CIRCULAR_WARNING}").as_str())
        );
    }

    /// Sheet1 A1:B2 = 10..13 and Sheet2 A1:B2 = 1..4, with a cut of
    /// Sheet2!A1:B2 pending and Sheet1 active (#782).
    fn cross_sheet_cut_app() -> App {
        let mut pkg = new_xlsx();
        pkg.add_sheet("Sheet2");
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        let block = |base: f64| {
            vec![
                (0, 0, parse_input(&base.to_string())),
                (0, 1, parse_input(&(base + 1.0).to_string())),
                (1, 0, parse_input(&(base + 2.0).to_string())),
                (1, 1, parse_input(&(base + 3.0).to_string())),
            ]
        };
        app.apply_on(0, block(10.0));
        app.apply_on(1, block(1.0));
        app.goto_sheet(1);
        app.anchor = Some((0, 0));
        app.cur = (1, 1);
        app.copy(true);
        app.goto_sheet(0);
        app
    }

    fn block_values(app: &App, sheet: usize, r: u32, c: u32) -> Vec<Option<f64>> {
        let num = |r, c| match app.pkg.workbook.sheets[sheet].cell(r, c).map(|x| &x.value) {
            Some(CellValue::Number(n)) => Some(*n),
            _ => None,
        };
        vec![num(r, c), num(r, c + 1), num(r + 1, c), num(r + 1, c + 1)]
    }

    #[test]
    fn a_cut_pasted_on_another_sheet_clears_its_source_sheet() {
        // #782: the clears land on the sheet the cut came from, not on the
        // paste sheet at the same coordinates.
        let mut app = cross_sheet_cut_app();
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 1, 0, 0), vec![None; 4]);
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
    }

    /// Sheet2!C1 holding `=B1`, selected as the whole clip (re-cut from the
    /// cross_sheet_cut_app's pending block cut).
    fn cut_formula_cell_app() -> App {
        let mut app = cross_sheet_cut_app();
        app.goto_sheet(1);
        app.apply_on(1, vec![(0, 2, parse_input("=B1"))]);
        app.anchor = Some((0, 2));
        app.cur = (0, 2);
        app.copy(true);
        app
    }

    #[test]
    fn a_formula_cut_to_another_sheet_keeps_reading_its_source_sheet() {
        // #820: the moved formula is qualified with the sheet it came from,
        // so it still reads Sheet2!B1 (=2), not Sheet1!B1 (=11).
        let mut app = cut_formula_cell_app();
        app.goto_sheet(0);
        app.cur = (0, 5);
        app.paste();
        let cell = app.pkg.workbook.sheets[0].cell(0, 5).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("Sheet2!B1"));
        assert_eq!(cell.value, CellValue::Number(2.0));
        // The dependency is live, not just the text: editing Sheet2!B1
        // re-flows into the moved formula.
        app.apply_on(1, vec![(0, 1, parse_input("50"))]);
        let cell = app.pkg.workbook.sheets[0].cell(0, 5).unwrap();
        assert_eq!(cell.value, CellValue::Number(50.0));
    }

    #[test]
    fn undo_of_a_cross_sheet_formula_cut_restores_the_unqualified_formula() {
        let mut app = cut_formula_cell_app();
        app.goto_sheet(0);
        app.cur = (0, 5);
        app.paste();
        app.undo();
        let c1 = app.pkg.workbook.sheets[1].cell(0, 2).unwrap();
        assert_eq!(c1.formula.as_deref(), Some("B1"));
        assert_eq!(c1.value, CellValue::Number(2.0));
        assert!(app.pkg.workbook.sheets[0].cell(0, 5).is_none());
    }

    #[test]
    fn a_formula_copied_to_another_sheet_gets_no_qualifier() {
        let mut app = cut_formula_cell_app();
        // Re-copy as a copy (not a cut): translation, no qualifier.
        app.copy(false);
        app.goto_sheet(0);
        app.cur = (0, 5);
        app.paste();
        let cell = app.pkg.workbook.sheets[0].cell(0, 5).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("E1"));
        assert!(!cell.formula.as_deref().unwrap().contains("Sheet2!"));
    }

    #[test]
    fn a_formula_cut_on_its_own_sheet_keeps_its_text() {
        let mut app = cut_formula_cell_app();
        app.cur = (0, 5);
        app.paste();
        let cell = app.pkg.workbook.sheets[1].cell(0, 5).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("B1"));
        assert_eq!(cell.value, CellValue::Number(2.0));
    }

    #[test]
    fn undo_of_a_cross_sheet_cut_paste_restores_both_sheets_in_one_step() {
        let mut app = cross_sheet_cut_app();
        let depth = app.undo.len();
        app.cur = (0, 5);
        app.paste();
        assert_eq!(app.undo.len(), depth + 1);
        app.goto_sheet(1);
        app.undo();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 1, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 5), vec![None; 4]);
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
        assert_eq!(app.redo.len(), 1);
        // The view follows the paste, as after any other undo.
        assert_eq!((app.sheet, app.cur), (0, (0, 5)));
        app.goto_sheet(1);
        app.redo();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 1, 0, 0), vec![None; 4]);
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
        assert_eq!((app.sheet, app.cur), (0, (0, 5)));
    }

    #[test]
    fn a_second_paste_of_a_cross_sheet_cut_is_a_copy() {
        let mut app = cross_sheet_cut_app();
        app.cur = (0, 5);
        app.paste();
        // Refill the source; the second paste must leave it alone.
        app.apply_on(1, vec![(0, 0, parse_input("7"))]);
        app.cur = (5, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 5, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 1, 0, 0)[0], Some(7.0));
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
    }

    #[test]
    fn a_cut_from_a_deleted_sheet_pastes_as_a_copy() {
        // Deleting a sheet cancels the pending cut: its recorded source
        // sheet may be gone or renumbered.
        let mut app = cross_sheet_cut_app();
        app.goto_sheet(1);
        app.delete_current_sheet();
        assert_eq!(app.pkg.workbook.sheets.len(), 1);
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
    }

    #[test]
    fn a_cut_whose_sheet_is_deleted_spares_the_sheet_that_takes_its_index() {
        // Deleting Sheet1 under a pending cut of Sheet1!A1:B2 makes Sheet2
        // index 0; the paste must not clear Sheet2!A1:B2 as the cut source.
        let mut app = cross_sheet_cut_app();
        app.anchor = Some((0, 0));
        app.cur = (1, 1);
        app.copy(true);
        app.delete_current_sheet();
        assert_eq!(app.pkg.workbook.sheets[0].name, "Sheet2");
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 5), some([10.0, 11.0, 12.0, 13.0]));
    }

    #[test]
    fn a_cut_pasted_at_the_grid_edge_keeps_the_cells_it_cannot_write() {
        // Cut A1:B1 pasted at XFD1: A1 moves to XFD1, B1 would land past
        // the last column, so it is neither written nor cleared.
        let mut app = cross_sheet_cut_app();
        app.anchor = Some((0, 0));
        app.cur = (0, 1);
        app.copy(true);
        app.anchor = None;
        app.cur = (0, MAX_COLS - 1);
        app.paste();
        let num = |c| match app.pkg.workbook.sheets[0].cell(0, c).map(|x| &x.value) {
            Some(CellValue::Number(n)) => Some(*n),
            _ => None,
        };
        assert_eq!(num(MAX_COLS - 1), Some(10.0));
        assert_eq!(num(0), None);
        assert_eq!(num(1), Some(11.0));
    }

    #[test]
    fn a_cut_from_a_protected_sheet_pastes_elsewhere_as_a_copy() {
        // Clearing the source would edit a protected sheet off-screen.
        let mut app = cross_sheet_cut_app();
        app.pkg.workbook.sheets[1].set_protected(true);
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 1, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(
            app.status.as_deref(),
            Some("Pasted (source sheet is protected; cut kept as copy)")
        );
    }

    #[test]
    fn a_formula_cut_from_a_protected_sheet_is_pasted_unqualified() {
        // The protected source demotes the cut to a copy: the formula is
        // translated like any copy's, with no source-sheet qualifier.
        let mut app = cut_formula_cell_app();
        app.pkg.workbook.sheets[1].set_protected(true);
        app.goto_sheet(0);
        app.cur = (0, 5);
        app.paste();
        let cell = app.pkg.workbook.sheets[0].cell(0, 5).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("E1"));
        assert!(!cell.formula.as_deref().unwrap().contains("Sheet2!"));
        // The source is left alone.
        assert_eq!(
            app.pkg.workbook.sheets[1]
                .cell(0, 2)
                .unwrap()
                .formula
                .as_deref(),
            Some("B1")
        );
    }

    /// One sheet, no OS clipboard, `cells` as `(row, col, number)`.
    fn cut_app(cells: &[(u32, u32, f64)]) -> App {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        let changes = cells
            .iter()
            .map(|&(r, c, n)| (r, c, parse_input(&n.to_string())))
            .collect();
        app.apply_on(0, changes);
        app
    }

    /// Select `from..=to` on the active sheet and cut or copy it.
    fn clip_range(app: &mut App, from: (u32, u32), to: (u32, u32), cut: bool) {
        app.anchor = Some(from);
        app.cur = to;
        app.copy(cut);
        app.anchor = None;
    }

    /// #683: `Sales` over A1:B3 (Item, Qty), `=SUM(Sales[Qty])` in D1.
    fn header_app() -> App {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        let sh = &mut app.pkg.workbook.sheets[0];
        sh.set_cell(0, 0, Cell::text("Item"));
        sh.set_cell(0, 1, Cell::text("Qty"));
        sh.set_cell(1, 1, Cell::number(3.0));
        sh.set_cell(2, 1, Cell::number(4.0));
        app.pkg
            .add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
            .unwrap();
        gridcore::edit::rename_table(&mut app.pkg.workbook, "Table1", "Sales").unwrap();
        app.rebuild_engine();
        app.apply_on(0, vec![(0, 3, parse_input("=SUM(Sales[Qty])"))]);
        app
    }

    fn header_state(app: &App) -> (String, Vec<String>, String) {
        let sh = &app.pkg.workbook.sheets[0];
        let text = |r, c| match sh.cell(r, c).map(|x| &x.value) {
            Some(CellValue::Text(t)) => t.clone(),
            other => format!("{other:?}"),
        };
        let d1 = sh
            .cell(0, 3)
            .and_then(|c| c.formula.clone())
            .unwrap_or_default();
        (text(0, 1), app.pkg.workbook.tables[0].columns.clone(), d1)
    }

    #[test]
    fn header_rename_is_one_undo_step_683() {
        let mut app = header_app();
        let before = header_state(&app);
        app.apply_on(0, vec![(0, 1, parse_input("Units"))]);
        let after = header_state(&app);
        assert_eq!(after.0, "Units");
        assert_eq!(after.1, ["Item", "Units"]);
        assert_eq!(after.2, "SUM(Sales[Units])");
        assert!(matches!(
            app.undo.last(),
            Some(UndoAction::Structural { .. })
        ));
        app.undo();
        assert_eq!(header_state(&app), before);
        app.redo();
        assert_eq!(header_state(&app), after);
        let d1 = app.pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.value, CellValue::Number(7.0));
        // A header edit that keeps the name is an ordinary cell step.
        app.apply_on(0, vec![(0, 1, parse_input("Units"))]);
        assert!(matches!(app.undo.last(), Some(UndoAction::Cells(_))));
        // A cleared header takes `Column2`, written into the cell.
        app.apply_on(0, vec![(0, 1, Cell::default())]);
        assert_eq!(header_state(&app).0, "Column2");
        assert_eq!(header_state(&app).2, "SUM(Sales[Column2])");
    }

    #[test]
    fn text_to_columns_over_a_header_renames_the_columns_683() {
        // A1 `Item;Units` split on `;` writes B1, the Qty header.
        let mut app = header_app();
        app.apply_on(0, vec![(0, 0, parse_input("Item;Units"))]);
        let src = gridcore::edit::TtcSource::new(0, (0, 0, 0, 0)).unwrap();
        app.apply_text_to_columns(&src, &TextParse::csv(';'));
        assert_eq!(header_state(&app).1, ["Item", "Units"]);
        assert_eq!(header_state(&app).2, "SUM(Sales[Units])");
        let d1 = app.pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.value, CellValue::Number(7.0));
    }

    #[test]
    fn a_sort_over_a_tables_header_renames_nothing_683() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        let sh = &mut app.pkg.workbook.sheets[0];
        for (r, row) in [["Name", "City"], ["Zed", "Oslo"], ["Amy", "Rome"]]
            .into_iter()
            .enumerate()
        {
            for (c, t) in row.into_iter().enumerate() {
                sh.set_cell(r as u32, c as u32, Cell::text(t));
            }
        }
        app.pkg
            .add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
            .unwrap();
        app.rebuild_engine();
        // Away from the region the sort moves.
        app.apply_on(0, vec![(9, 5, parse_input("=COUNTA(Table1[City])"))]);
        let header = |app: &App| {
            let sh = &app.pkg.workbook.sheets[0];
            [0, 1].map(|c| sh.cell(0, c).map(|x| x.value.clone()))
        };
        let before = header(&app);
        app.cur = (1, 0);
        app.sort_region(true);
        assert_ne!(header(&app), before, "the sort moved the header row");
        assert_eq!(app.pkg.workbook.tables[0].columns, ["Name", "City"]);
        let f10 = app.pkg.workbook.sheets[0].cell(9, 5).unwrap();
        assert_eq!(f10.formula.as_deref(), Some("COUNTA(Table1[City])"));
    }

    #[test]
    fn cutting_a_whole_table_renames_nothing_683() {
        let mut app = header_app();
        clip_range(&mut app, (0, 0), (2, 1), true);
        app.cur = (9, 5);
        app.paste();
        assert_eq!(header_state(&app).1, ["Item", "Qty"]);
        assert_eq!(header_state(&app).2, "SUM(Sales[Qty])");
        let f10 = app.pkg.workbook.sheets[0]
            .cell(9, 5)
            .map(|c| c.value.clone());
        assert_eq!(f10, Some(CellValue::Text("Item".into())));
        // Cutting a header alone still clears it to `Column<n>`.
        let mut app = header_app();
        clip_range(&mut app, (0, 1), (0, 1), true);
        app.cur = (9, 5);
        app.paste();
        assert_eq!(header_state(&app).1, ["Item", "Column2"]);
    }

    #[test]
    fn cut_paste_onto_a_header_renames_the_column_683() {
        let mut app = header_app();
        app.apply_on(0, vec![(5, 0, parse_input("Units"))]);
        clip_range(&mut app, (5, 0), (5, 0), true);
        app.cur = (0, 1);
        app.paste();
        assert_eq!(header_state(&app).1, ["Item", "Units"]);
        assert_eq!(header_state(&app).2, "SUM(Sales[Units])");
        assert!(
            app.pkg.workbook.sheets[0]
                .cell(5, 0)
                .is_none_or(|c| c.value.is_empty())
        );
        // One undo puts back the header, the formula and the cut cell.
        app.undo();
        assert_eq!(header_state(&app).1, ["Item", "Qty"]);
        assert_eq!(header_state(&app).2, "SUM(Sales[Qty])");
        let a6 = app.pkg.workbook.sheets[0]
            .cell(5, 0)
            .map(|c| c.value.clone());
        assert_eq!(a6, Some(CellValue::Text("Units".into())));
    }

    #[test]
    fn a_header_rename_cancels_a_pending_cut_683() {
        // Cut D1, rename the Qty header, paste at F1: the rename rewrote
        // D1, so the cut is a copy now and D1 stays.
        let mut app = header_app();
        clip_range(&mut app, (0, 3), (0, 3), true);
        app.apply_on(0, vec![(0, 1, parse_input("Units"))]);
        app.cur = (0, 5);
        app.paste();
        let d1 = app.pkg.workbook.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("SUM(Sales[Units])"));
        assert_eq!(d1.value, CellValue::Number(7.0));
    }

    const PROTECTED_STATUS: &str = "Sheet is protected — unprotect it to edit (Review ▸ Protect)";

    #[test]
    fn a_row_insert_cancels_a_pending_cut() {
        // #821: cut A5:B6, insert a row above row 1, paste at F1. The cut's
        // recorded A5:B6 now holds what was A4:B5; neither block is cleared.
        let mut app = cut_app(&[
            (3, 0, 10.0),
            (3, 1, 11.0),
            (4, 0, 1.0),
            (4, 1, 2.0),
            (5, 0, 3.0),
            (5, 1, 4.0),
        ]);
        clip_range(&mut app, (4, 0), (5, 1), true);
        app.cur = (0, 0);
        app.row_op(true);
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 5, 0), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 4, 0)[..2], [Some(10.0), Some(11.0)]);
    }

    #[test]
    fn a_column_delete_cancels_a_pending_cut() {
        // Cut C1:D2, delete column A: the cut's cells now sit at B1:C2.
        let mut app = cut_app(&[
            (0, 0, 10.0),
            (0, 1, 11.0),
            (0, 2, 1.0),
            (0, 3, 2.0),
            (1, 2, 3.0),
            (1, 3, 4.0),
        ]);
        clip_range(&mut app, (0, 2), (1, 3), true);
        app.cur = (0, 0);
        app.col_op(false);
        app.cur = (4, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 4, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 1), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 0)[0], Some(11.0));
    }

    #[test]
    fn a_sort_cancels_a_pending_cut() {
        // Cut A1:B2, sort A1:B4 ascending: other rows now sit under A1:B2.
        let mut app = cut_app(&[
            (0, 0, 4.0),
            (0, 1, 40.0),
            (1, 0, 3.0),
            (1, 1, 30.0),
            (2, 0, 2.0),
            (2, 1, 20.0),
            (3, 0, 1.0),
            (3, 1, 10.0),
        ]);
        clip_range(&mut app, (0, 0), (1, 1), true);
        app.cur = (0, 0);
        app.sort_region(true);
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([4.0, 40.0, 3.0, 30.0]));
        assert_eq!(block_values(&app, 0, 0, 0), some([1.0, 10.0, 2.0, 20.0]));
        assert_eq!(block_values(&app, 0, 2, 0), some([3.0, 30.0, 4.0, 40.0]));
    }

    #[test]
    fn undoing_a_structural_edit_after_a_cut_does_not_revive_it() {
        // Cut, insert a row, undo: the layout is back, but the cut stays a copy.
        let mut app = cut_app(&[(0, 0, 1.0), (0, 1, 2.0), (1, 0, 3.0), (1, 1, 4.0)]);
        clip_range(&mut app, (0, 0), (1, 1), true);
        app.cur = (0, 0);
        app.row_op(true);
        app.undo();
        app.cur = (4, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 4, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn undoing_a_structural_edit_after_a_cut_cancels_it() {
        // Insert a row, cut the shifted block A2:B3, undo the insert: the
        // block is back at A1:B2 and A2:B3 holds other cells.
        let mut app = cut_app(&[(0, 0, 1.0), (0, 1, 2.0), (1, 0, 3.0), (1, 1, 4.0)]);
        app.cur = (0, 0);
        app.row_op(true);
        clip_range(&mut app, (1, 0), (2, 1), true);
        app.undo();
        app.cur = (4, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 4, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn redoing_a_structural_edit_after_a_cut_cancels_it() {
        // Insert a row, undo, cut A1:B2, redo the insert: the block moves
        // to A2:B3 and A1:B2 no longer holds the cut's cells.
        let mut app = cut_app(&[(0, 0, 1.0), (0, 1, 2.0), (1, 0, 3.0), (1, 1, 4.0)]);
        app.cur = (0, 0);
        app.row_op(true);
        app.undo();
        clip_range(&mut app, (0, 0), (1, 1), true);
        app.redo();
        app.cur = (4, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 4, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 1, 0), some([1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn toggling_protection_keeps_a_pending_cut() {
        // A protection toggle moves no cells, so the cut still moves.
        let mut app = cut_app(&[(0, 0, 1.0), (0, 1, 2.0), (1, 0, 3.0), (1, 1, 4.0)]);
        clip_range(&mut app, (0, 0), (1, 1), true);
        app.toggle_protection();
        app.toggle_protection();
        assert!(!app.protected());
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 0), vec![None; 4]);
    }

    #[test]
    fn unprotecting_after_a_refused_paste_moves_the_cut() {
        // Cut Sheet2!A1:B2, paste on protected Sheet1: refused. Unprotect
        // as the status says, paste again: the cut moves.
        let mut app = cross_sheet_cut_app();
        app.pkg.workbook.sheets[0].set_protected(true);
        app.cur = (0, 5);
        app.paste();
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
        app.toggle_protection();
        assert!(!app.protected());
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 1, 0, 0), vec![None; 4]);
        assert_eq!(app.status.as_deref(), Some("Pasted"));
    }

    #[test]
    fn paste_is_refused_on_a_protected_sheet() {
        let mut app = cut_app(&[(0, 0, 1.0), (0, 1, 2.0), (1, 0, 3.0), (1, 1, 4.0)]);
        clip_range(&mut app, (0, 0), (1, 1), false);
        app.pkg.workbook.sheets[0].set_protected(true);
        let undo_len = app.undo.len();
        app.cur = (0, 5);
        app.paste();
        assert_eq!(block_values(&app, 0, 0, 5), vec![None; 4]);
        assert_eq!(app.undo.len(), undo_len);
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
    }

    #[test]
    fn a_same_sheet_cut_is_not_pasted_on_a_protected_sheet() {
        let mut app = cut_app(&[(0, 0, 1.0), (0, 1, 2.0), (1, 0, 3.0), (1, 1, 4.0)]);
        clip_range(&mut app, (0, 0), (1, 1), true);
        app.pkg.workbook.sheets[0].set_protected(true); // setup, not the toggle
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 5), vec![None; 4]);
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
    }

    #[test]
    fn a_refused_paste_on_a_protected_sheet_keeps_the_cut() {
        // The cut is Sheet2!A1:B2; the active Sheet1 is protected, so the
        // paste is refused and the cut can still move to Sheet3.
        let mut app = cross_sheet_cut_app();
        app.pkg.add_sheet("Sheet3");
        app.pkg.workbook.sheets[0].set_protected(true);
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
        assert_eq!(block_values(&app, 0, 0, 5), vec![None; 4]);
        assert_eq!(block_values(&app, 1, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
        assert!(app.clip.as_ref().is_some_and(|c| c.cut));
        app.goto_sheet(2);
        app.cur = (0, 5);
        app.paste();
        assert_eq!(block_values(&app, 2, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 1, 0, 0), vec![None; 4]);
    }

    #[test]
    fn external_text_paste_is_refused_on_a_protected_sheet() {
        let mut app = cut_app(&[]);
        app.pkg.workbook.sheets[0].set_protected(true);
        let undo_len = app.undo.len();
        app.cur = (0, 5);
        app.paste_from(Some("5\t6\n7\t8\n".into()));
        assert_eq!(block_values(&app, 0, 0, 5), vec![None; 4]);
        assert_eq!(app.undo.len(), undo_len);
        assert_eq!(app.status.as_deref(), Some(PROTECTED_STATUS));
    }

    #[test]
    fn external_text_paste_writes_cells_through_paste_from() {
        // The seam the protected test above goes through: unprotected, the
        // external TSV lands.
        let mut app = cut_app(&[]);
        app.cur = (0, 5);
        app.paste_from(Some("5\t6\n7\t8\n".into()));
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([5.0, 6.0, 7.0, 8.0]));
    }

    #[test]
    fn a_cut_whose_source_sheet_is_out_of_range_pastes_as_a_copy() {
        let mut app = cross_sheet_cut_app();
        if let Some(clip) = &mut app.clip {
            clip.sheet = 99;
        }
        app.cur = (0, 5);
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 0, 5), some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
        assert_eq!(block_values(&app, 1, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn a_same_sheet_cut_paste_is_one_undo_step() {
        // The same-sheet move keeps today's single group: one undo
        // restores both the source and the destination.
        let mut app = cross_sheet_cut_app();
        app.anchor = Some((0, 0));
        app.cur = (1, 1);
        app.copy(true);
        app.anchor = None;
        app.cur = (1, 1); // overlaps the source at B2
        app.paste();
        let some = |v: [f64; 4]| v.map(Some).to_vec();
        assert_eq!(block_values(&app, 0, 1, 1), some([10.0, 11.0, 12.0, 13.0]));
        assert_eq!(block_values(&app, 0, 0, 0)[..3], [None, None, None]);
        app.undo();
        assert_eq!(block_values(&app, 0, 0, 0), some([10.0, 11.0, 12.0, 13.0]));
        assert_eq!(block_values(&app, 0, 1, 2)[0], None);
        assert_eq!(block_values(&app, 1, 0, 0), some([1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn a_control_verbs_circle_warning_ignores_stale_status() {
        // #660: a verb that makes a circle shows the warning alone, not
        // appended to an earlier action's status; a verb that says nothing
        // leaves that status as it was.
        use ctlcore::json::Json;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        let set = |app: &mut App, r: &str, t: &str| {
            run_control(
                app,
                "cell.set",
                &Json::obj(vec![
                    ("ref", Json::Str(r.into())),
                    ("text", Json::Str(t.into())),
                ]),
            )
            .unwrap();
        };
        app.status = Some("Saved report.xlsx".into());
        set(&mut app, "A1", "5");
        assert_eq!(app.status.as_deref(), Some("Saved report.xlsx"));
        set(&mut app, "E1", "=E1+1");
        assert_eq!(app.status.as_deref(), Some(CIRCULAR_WARNING));
    }

    #[test]
    fn iterative_workbooks_do_not_surface_their_circles() {
        // #660: with iterative calculation on, Excel shows neither the
        // warning nor the status-bar note; the engine still knows the circle.
        use ratatui::{Terminal, backend::TestBackend};
        let mut pkg = new_xlsx();
        pkg.workbook.iterate = Some((100, 0.001));
        pkg.workbook.sheets[0].set_cell(0, 3, Cell::formula("D1/2+5"));
        let mut app = App::new(pkg, "iter.xlsx");
        app.os_clip = None;
        assert_eq!(app.status, None);
        assert_eq!(app.circular_refs(), vec!["D1".to_string()]);
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(!text.contains("Circular References"), "{text}");
        // A new circle by a cell edit does not warn either.
        app.apply_on(0, vec![(0, 4, parse_input("=E1+1"))]);
        app.flush_circle_warning();
        assert_eq!(app.status, None);
    }

    #[test]
    fn verify_compares_database_functions() {
        // #677: D-functions always recalculate but are deterministic, so
        // --verify compares them instead of skipping them as volatile.
        let mut pkg = new_xlsx();
        {
            let sh = &mut pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Amount"));
            sh.set_cell(1, 0, Cell::number(5.0));
            sh.set_cell(2, 0, Cell::number(7.0));
            sh.set_cell(0, 2, Cell::text("Amount"));
            sh.set_cell(1, 2, Cell::text(">6"));
            sh.set_cell(
                0,
                4,
                Cell {
                    value: CellValue::Number(7.0),
                    ..Cell::formula("DSUM(A1:A3,\"Amount\",C1:C2)")
                },
            );
        }
        let (_, stats) = verify_report(&pkg, "d.xlsx");
        assert_eq!(stats.volatile, 0);
        assert_eq!((stats.total, stats.compared, stats.matched), (1, 1, 1));
    }

    #[test]
    fn a_general_number_is_fitted_to_its_column() {
        let general = gridcore::sheet::Xf::default();
        let big = CellValue::Number(123_456_789_012.0);
        // A wide column still stops at General's 11 characters; a narrower
        // one shortens further; the stored value keeps every digit.
        assert_eq!(grid_text(&general, &big, false, 20), "1.23457E+11");
        assert_eq!(grid_text(&general, &big, false, 9), "1.23E+11");
        assert_eq!(grid_text(&general, &big, false, 3), "##");
        assert_eq!(
            grid_text(&general, &CellValue::Number(42.0), false, 9),
            "42"
        );
        // A number format is not General: it keeps its own text.
        let mut fixed = gridcore::sheet::Xf::default();
        fixed.set_code(Some("0.00".into()));
        assert_eq!(grid_text(&fixed, &CellValue::Number(1.5), false, 9), "1.50");
        // Drawn: the default column shows the fitted text, not the digits.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        // Off the cursor (A1), so the formula bar does not show its digits.
        app.pkg.workbook.sheets[0].set_cell(2, 1, gridcore::sheet::Cell::number(123_456_789_012.0));
        app.rebuild_engine();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(!text.contains("123456789012"), "full digits drawn");
        assert!(text.contains("E+11"), "fitted text missing");
    }

    #[test]
    fn a_copy_pasted_between_text_and_general_cells_is_the_same_cells() {
        use gridcore::sheet::Xf;
        let mut pkg = new_xlsx();
        let styles = &mut pkg.workbook.styles;
        let quoted = styles.intern(Xf {
            quote_prefix: true,
            ..Xf::default()
        });
        let mut text_xf = Xf::default();
        text_xf.set_code(Some("@".into()));
        let text_fmt = styles.intern(text_xf);
        let q007 = Cell {
            style: quoted,
            ..Cell::text("007")
        };
        let tabc = Cell {
            style: text_fmt,
            ..Cell::text("'abc")
        };
        let sh = &mut pkg.workbook.sheets[0];
        sh.set_cell(0, 0, q007.clone());
        sh.set_cell(0, 1, tabc.clone());
        sh.set_cell(
            2,
            0,
            Cell {
                style: text_fmt,
                ..Cell::default()
            },
        );
        sh.set_cell(2, 1, Cell::text("x"));
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None; // the internal clip, as when the OS text is ours
        app.cur = (0, 0);
        app.anchor = Some((0, 1));
        app.copy(false);
        app.anchor = None;
        app.cur = (2, 0);
        app.paste();
        assert_eq!(app.sheet().cell(2, 0), Some(&q007));
        assert_eq!(app.sheet().cell(2, 1), Some(&tabc));
    }

    #[test]
    fn merged_cells_render_spanned() {
        use gridcore::sheet::Cell;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("MERGEDTITLE"));
            sh.set_cell(0, 1, Cell::text("COVERED")); // hidden by the merge
            sh.merges.push((0, 0, 0, 2));
        }
        app.rebuild_engine();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("MERGEDTITLE"), "top-left content missing");
        assert!(!text.contains("COVERED"), "covered cell should be blanked");
    }

    #[test]
    fn insert_chart_from_selection() {
        use gridcore::sheet::{Cell, DrawingKind};
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Item"));
            sh.set_cell(0, 1, Cell::text("Qty"));
            sh.set_cell(1, 0, Cell::text("A"));
            sh.set_cell(1, 1, Cell::number(3.0));
            sh.set_cell(2, 0, Cell::text("B"));
            sh.set_cell(2, 1, Cell::number(5.0));
        }
        app.rebuild_engine();
        app.cur = (0, 0);
        app.anchor = Some((2, 1)); // select A1:B3
        app.insert_chart("column");

        let drawings = &app.pkg.workbook.sheets[0].drawings;
        assert_eq!(drawings.len(), 1);
        match &drawings[0].kind {
            DrawingKind::Chart(cd) => {
                assert_eq!(cd.categories, vec!["A", "B"]);
                assert_eq!(cd.series.len(), 1);
                assert_eq!(cd.series[0].name, "Qty");
                assert_eq!(cd.series[0].values, vec![3.0, 5.0]);
            }
            _ => panic!("expected a chart drawing"),
        }
        // Round-trips: reload sees the chart with the column orientation.
        let re = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let dl = &re.workbook.sheets[0].drawings;
        assert_eq!(dl.len(), 1);
        match &dl[0].kind {
            DrawingKind::Chart(cd) => assert_eq!(cd.kind, "column"),
            _ => panic!("expected a chart drawing"),
        }
    }

    #[test]
    fn backstage_open_and_new_workbook() {
        let tmp = std::env::temp_dir().join("xlsxy_open_flow.xlsx");
        // Author a workbook with a value and save it.
        let mut src = App::new(new_xlsx(), tmp.to_str().unwrap());
        src.os_clip = None;
        src.start_edit(Some('7'));
        assert!(src.commit_edit());
        src.save();

        // A different session opens it.
        let mut app = App::new(new_xlsx(), "other.xlsx");
        app.os_clip = None;
        app.open_workbook(tmp.to_str().unwrap());
        assert_eq!(app.path, tmp.to_str().unwrap());
        assert_eq!(
            app.sheet().cell(0, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(7.0))
        );
        assert!(!app.modified);

        // New workbook wipes back to a blank untitled sheet.
        app.new_workbook();
        assert_eq!(app.path, "untitled.xlsx");
        assert!(app.sheet().cell(0, 0).is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn backstage_mouse_selects_and_opens() {
        // A temp folder holding one saved workbook.
        let dir = std::env::temp_dir().join("xlsxy_bs_mouse");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.xlsx");
        {
            let mut src = App::new(new_xlsx(), file.to_str().unwrap());
            src.os_clip = None;
            src.start_edit(Some('9'));
            assert!(src.commit_edit());
            src.save();
        }

        let mut app = App::new(new_xlsx(), "other.xlsx");
        app.os_clip = None;
        app.backstage = Some(backstage::Backstage::open(dir.clone(), app.extensions()));

        // The left menu column is x<14; item rows start at y=1. "Open" is
        // already the default item, so a click on its row switches the pane.
        let open_row = 1 + backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::Open)
            .unwrap() as u16;
        assert!(!app.bs_mouse(3, open_row));
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::Browser
        );

        // Entries: "a.xlsx" (no ".."-worthy parent quirks here). The file list
        // starts at screen y=2.
        let a_idx = app
            .backstage
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .position(|e| e.name == "a.xlsx")
            .unwrap();
        let a_row = 2 + a_idx as u16;

        // Single click selects but does not open.
        assert!(!app.bs_mouse(20, a_row));
        assert_eq!(app.backstage.as_ref().unwrap().sel, a_idx);
        assert_eq!(app.path, "other.xlsx");

        // A second click on the same row (double click) opens it.
        assert!(!app.bs_mouse(20, a_row));
        assert!(app.backstage.is_none());
        assert_eq!(app.path, file.to_str().unwrap());
        assert_eq!(
            app.sheet().cell(0, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(9.0))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backstage_mouse_exit_item_opens_confirm_then_quits() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.backstage = Some(backstage::Backstage::open(
            std::env::temp_dir(),
            app.extensions(),
        ));
        // "Exit" is the last item; clicking it opens the shared confirm modal
        // instead of quitting outright, even on an unmodified workbook.
        let exit_row = 1 + backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::Exit)
            .unwrap() as u16;
        assert!(!app.bs_mouse(3, exit_row));
        assert!(app.backstage.is_none());
        assert!(app.confirm.is_some());
        assert!(!app.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        // Confirming (press 'y') quits.
        assert!(app.confirm_key(KeyEvent::from(KeyCode::Char('y'))));
    }

    #[test]
    fn single_click_exit_opens_confirm_and_new_stays_guarded() {
        // Exit is the last menu item (after xlsxy's Options), drawn at screen
        // row 1 + idx. One click goes straight to the shared confirm modal —
        // no second click.
        let mut app = App::new(new_xlsx(), "doc.xlsx");
        app.os_clip = None;
        app.open_backstage();
        let exit_row = 1 + app
            .backstage
            .as_ref()
            .unwrap()
            .items()
            .iter()
            .position(|i| *i == backstage::Item::Exit)
            .unwrap() as u16;
        app.bs_mouse(3, exit_row);
        assert!(app.backstage.is_none(), "Exit closes the backstage");
        assert!(app.confirm.is_some(), "Exit raises the confirm dialog");

        // New is guarded by the shared backstagecore mouse handler: a first
        // click only selects it, a second (confirming) click fires it.
        let new_row = 1 + backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::New)
            .unwrap() as u16;
        let mut app = App::new(new_xlsx(), "doc.xlsx");
        app.os_clip = None;
        app.open_backstage();
        app.bs_mouse(3, new_row);
        assert!(app.backstage.is_some(), "first New click only selects");
        assert_eq!(app.backstage.as_ref().unwrap().item, backstage::Item::New);
        // Second click on the already-selected New actually starts a new workbook.
        app.bs_mouse(3, new_row);
        assert!(app.backstage.is_none());
        assert_eq!(app.path, "untitled.xlsx");
    }

    #[test]
    fn ctrl_q_opens_the_exit_confirmation() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.modified = true;
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        // Ctrl+Q does not quit outright — it opens the Yes/No modal.
        assert!(!handle_key(&mut app, ctrl_q));
        assert!(app.confirm.is_some());
        // the prompt warns about unsaved changes
        assert!(app.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        // confirming with 'y' quits
        assert!(handle_key(&mut app, KeyEvent::from(KeyCode::Char('y'))));
    }

    #[test]
    fn ctrl_q_confirmation_can_be_cancelled() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        // Even with no changes, Ctrl+Q asks first.
        assert!(!handle_key(&mut app, ctrl_q));
        assert!(app.confirm.is_some());
        assert!(!app.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        // No / Esc dismisses without quitting.
        assert!(!handle_key(&mut app, KeyEvent::from(KeyCode::Esc)));
        assert!(app.confirm.is_none());
    }

    #[test]
    fn shift_delete_opens_delete_sheet_confirmation() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.pkg.add_sheet("Sheet2");
        let n_sheets = app.pkg.workbook.sheets.len();
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::SHIFT)
        ));
        assert!(app.confirm.is_some());
        assert!(
            !app.confirm.as_ref().unwrap().yes_selected(),
            "No is the default for a destructive prompt"
        );
        // Cancelling leaves every sheet in place.
        assert!(!handle_key(&mut app, KeyEvent::from(KeyCode::Esc)));
        assert!(app.confirm.is_none());
        assert_eq!(app.pkg.workbook.sheets.len(), n_sheets);
        // Confirming with 'y' deletes the current sheet.
        assert!(!handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::SHIFT)
        ));
        assert!(!handle_key(&mut app, KeyEvent::from(KeyCode::Char('y'))));
        assert_eq!(app.pkg.workbook.sheets.len(), n_sheets - 1);
    }

    #[test]
    fn find_replace_and_goto() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        // A1 = "foo bar", A2 = "=foo", B1 = 3
        app.cur = (0, 0);
        app.start_edit(Some('f'));
        if let Some(e) = &mut app.edit {
            e.text = "foo bar".into();
            e.cursor = 7;
        }
        assert!(app.commit_edit());
        app.cur = (1, 0);
        app.start_edit(Some('='));
        if let Some(e) = &mut app.edit {
            e.text = "=\"foo\"".into();
            e.cursor = 6;
        }
        assert!(app.commit_edit());

        // Replace foo → baz across the sheet.
        app.replace_all("foo", "baz");
        assert_eq!(
            app.pkg.workbook.sheets[0].cell(0, 0).unwrap().value,
            CellValue::Text("baz bar".into())
        );
        assert_eq!(
            app.pkg.workbook.sheets[0]
                .cell(1, 0)
                .unwrap()
                .formula
                .as_deref(),
            Some("\"baz\"")
        );

        // Go To jumps the cursor.
        app.goto("C5");
        assert_eq!(app.cur, (4, 2));
        app.goto("A1");
        assert_eq!(app.cur, (0, 0));
    }

    #[test]
    fn vim_mode_navigation_and_commands() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.vim = Some(VimState {
            mode: VimMode::Normal,
            pending: '\0',
            cmdline: None,
        });
        // l l j → right, right, down.
        app.vim_key(KeyCode::Char('l'), false, false);
        app.vim_key(KeyCode::Char('l'), false, false);
        app.vim_key(KeyCode::Char('j'), false, false);
        assert_eq!(app.cur, (1, 2));
        // gg → first row.
        app.vim_key(KeyCode::Char('g'), false, false);
        app.vim_key(KeyCode::Char('g'), false, false);
        assert_eq!(app.cur.0, 0);
        // Visual select + yank returns to Normal.
        app.vim_key(KeyCode::Char('v'), false, false);
        assert_eq!(app.vim_mode(), VimMode::Visual);
        app.vim_key(KeyCode::Char('l'), false, false);
        app.vim_key(KeyCode::Char('y'), false, false);
        assert_eq!(app.vim_mode(), VimMode::Normal);
        // `i` enters insert (edit) mode.
        app.vim_key(KeyCode::Char('i'), false, false);
        assert!(app.edit.is_some());
        app.cancel_edit();
        // :q on an unmodified sheet exits.
        app.vim_key(KeyCode::Char(':'), false, false);
        app.vim_key(KeyCode::Char('q'), false, false);
        assert!(app.vim_key(KeyCode::Enter, false, false));
    }

    #[test]
    fn vim_wq_stays_open_when_the_save_fails() {
        let dir = std::env::temp_dir().join(format!("xlsxy-609-wq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The parent directory does not exist, so the save fails.
        let book = dir.join("missing").join("book.xlsx");
        let mut app = App::new(new_xlsx(), book.to_str().unwrap());
        app.os_clip = None;
        app.modified = true;
        for cmd in ["wq", "x"] {
            assert!(!app.vim_run_command(cmd), ":{cmd} quit after a failed save");
            assert!(app.modified);
            assert!(
                app.status.as_deref().unwrap().starts_with("save failed: "),
                "{:?}",
                app.status
            );
        }
        // Once the save can land, :wq quits.
        std::fs::create_dir_all(book.parent().unwrap()).unwrap();
        assert!(app.vim_run_command("wq"));
        assert!(!app.modified);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn window_title_format() {
        assert_eq!(
            window_title("xlsxy", "/tmp/report.xlsx", false, false),
            "xlsxy - report.xlsx"
        );
        assert_eq!(
            window_title("xlsxy", "/tmp/report.xlsx", true, false),
            "* xlsxy - report.xlsx"
        );
        assert_eq!(
            window_title("xlsxy", "book.xlsx", true, false),
            "* xlsxy - book.xlsx"
        );
        assert_eq!(
            window_title("xlsxy", "book.xlsx", true, true),
            "* xlsxy - book.xlsx [Read-Only]"
        );
    }

    #[test]
    fn sheet_picker_jumps() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.pkg.add_sheet("Second");
        app.pkg.add_sheet("Third");
        assert_eq!(app.sheet, 0);
        app.open_sheet_picker();
        assert_eq!(app.sheet_picker, Some(0));
        app.sheet_picker_key(KeyCode::Down);
        app.sheet_picker_key(KeyCode::Down);
        app.sheet_picker_key(KeyCode::Enter);
        assert_eq!(app.sheet, 2);
        assert!(app.sheet_picker.is_none());
        assert_eq!(app.cur, (0, 0)); // viewport reset
    }

    #[test]
    fn view_toggles_and_ribbon_state() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        assert!(!app.formula_view);
        app.toggle_formula_view();
        assert!(app.formula_view);
        app.cur = (3, 2);
        app.toggle_freeze();
        assert_eq!(app.freeze(), (3, 2));
        app.toggle_freeze();
        assert_eq!(app.freeze(), (0, 0));
        app.toggle_theme();
        assert!(app.light_theme);
        let t = app.ribbon_toggles();
        assert!(t.contains(&ribbon::Act::FormulaView));
        assert!(t.contains(&ribbon::Act::ThemeToggle));
        assert!(!t.contains(&ribbon::Act::FreezePanes));
    }

    #[test]
    fn start_screen_activation() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.start_screen = true;
        assert!(!app.start_choose(0)); // New workbook
        assert!(!app.start_screen);
        // Open drops into the File backstage.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.start_screen = true;
        assert!(!app.start_choose(1));
        assert!(app.backstage.is_some());
        // The welcome-screen Quit exits directly (nothing open to lose).
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.start_screen = true;
        assert!(app.start_choose(2), "welcome-screen Quit exits directly");
        assert!(app.confirm.is_none());
    }

    #[test]
    fn start_screen_navigation_wraps_and_digits_pick() {
        // backstagecore::Start wraps at the ends: Up on the first item lands on
        // the last (there are 3 items: indices 0..=2).
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.start_screen = true;
        app.os_clip = None;
        assert!(!handle_key(&mut app, KeyEvent::from(KeyCode::Up))); // wraps to last
        assert_eq!(app.start.sel(), 2);
        assert!(!handle_key(&mut app, KeyEvent::from(KeyCode::Down))); // wraps to first
        assert_eq!(app.start.sel(), 0);
        assert!(!handle_key(&mut app, KeyEvent::from(KeyCode::Down)));
        assert_eq!(app.start.sel(), 1);
        // A digit selects and activates: '3' → Quit exits directly.
        assert!(handle_key(&mut app, KeyEvent::from(KeyCode::Char('3'))));
    }

    #[test]
    fn start_screen_mouse_clicks_activate() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.start_screen = true;
        app.os_clip = None;
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        // Clicking the New workbook row (index 0) activates it.
        assert!(!handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 9,
                modifiers: KeyModifiers::empty(),
            }
        ));
        assert!(!app.start_screen);
    }

    #[test]
    fn ribbon_new_comment_opens_the_prompt() {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.ribbon_act(ribbon::Act::NewComment);
        assert!(matches!(
            app.prompt.as_ref().map(|p| p.kind),
            Some(PromptKind::NewComment)
        ));
        assert!(app.show_comments);
    }

    #[test]
    fn parse_input_kinds() {
        assert_eq!(parse_input("42").value, CellValue::Number(42.0));
        assert_eq!(parse_input("-2.5").value, CellValue::Number(-2.5));
        assert_eq!(parse_input("50%").value, CellValue::Number(0.5));
        assert_eq!(parse_input("true").value, CellValue::Bool(true));
        assert_eq!(parse_input("#N/A").value, CellValue::Error("#N/A".into()));
        assert_eq!(parse_input("hello").value, CellValue::Text("hello".into()));
        assert_eq!(
            parse_input("=SUM(A1:A3)").formula.as_deref(),
            Some("SUM(A1:A3)")
        );
        // A bare "=" is just text-less empty; "=1e3" is a formula.
        assert!(parse_input("=").formula.is_none());
        assert_eq!(parse_input("").value, CellValue::Empty);
    }

    #[test]
    fn app_edit_cycle_updates_dependents() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(2.0));
        pkg.workbook.sheets[0].set_cell(
            1,
            0,
            Cell {
                value: CellValue::Number(4.0),
                formula: Some("A1*2".into()),
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        // Type 10 into A1.
        app.start_edit(Some('1'));
        if let Some(e) = &mut app.edit {
            e.text.push('0');
            e.cursor += 1;
        }
        assert!(app.commit_edit());
        let v = app.pkg.workbook.sheets[0].cell(1, 0).unwrap().value.clone();
        assert_eq!(v, CellValue::Number(20.0));
        // Undo restores both the cell and (via recalc) the dependent.
        app.undo();
        let v = app.pkg.workbook.sheets[0].cell(1, 0).unwrap().value.clone();
        assert_eq!(v, CellValue::Number(4.0));
        // Redo brings the edit back.
        app.redo();
        let v = app.pkg.workbook.sheets[0].cell(1, 0).unwrap().value.clone();
        assert_eq!(v, CellValue::Number(20.0));
    }

    #[test]
    fn copy_paste_translates_relative_refs() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(1.0));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::number(2.0));
        pkg.workbook.sheets[0].set_cell(
            0,
            1,
            Cell {
                value: CellValue::Number(2.0),
                formula: Some("A1*2".into()),
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        // Copy B1, paste at B2: formula becomes A2*2 → 4.
        app.cur = (0, 1);
        app.copy(false);
        app.cur = (1, 1);
        app.paste();
        let b2 = app.pkg.workbook.sheets[0].cell(1, 1).unwrap().clone();
        assert_eq!(b2.formula.as_deref(), Some("A2*2"));
        assert_eq!(b2.value, CellValue::Number(4.0));
    }

    /// The values of column `c`, rows `r1..=r2`, on the first sheet.
    fn col_values(app: &App, c: u32, r1: u32, r2: u32) -> Vec<CellValue> {
        (r1..=r2)
            .map(|r| {
                app.pkg.workbook.sheets[0]
                    .cell(r, c)
                    .map(|cl| cl.value.clone())
                    .unwrap_or(CellValue::Empty)
            })
            .collect()
    }

    /// An app with C1 `=SEQUENCE(3)` spilling C1:C3 (typed, through the engine).
    fn app_with_sequence_in_c1() -> App {
        let mut app = App::new(new_xlsx(), "test.xlsx");
        app.os_clip = None;
        app.apply(vec![(0, 2, Cell::formula("SEQUENCE(3)"))]);
        let n = |v: f64| CellValue::Number(v);
        assert_eq!(col_values(&app, 2, 0, 2), vec![n(1.0), n(2.0), n(3.0)]);
        app
    }

    #[test]
    fn paste_whole_spill_block_respills() {
        // #777: a copied spill block (anchor + its spilled values) pastes as a
        // spilling anchor, also on redo; undo gives back what was there.
        let n = |v: f64| CellValue::Number(v);
        let t = |s: &str| CellValue::Text(s.into());
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        for old in [None, Some(["a", "b", "c"])] {
            let mut app = app_with_sequence_in_c1();
            if let Some(old) = old {
                let cells = (0..3).map(|r| (r as u32, 6, Cell::text(old[r]))).collect();
                app.apply(cells);
            }
            let was = col_values(&app, 6, 0, 2);
            app.anchor = Some((0, 2));
            app.cur = (2, 2);
            app.copy(false);
            app.anchor = None;
            app.cur = (0, 6);
            app.paste();
            let g1 = || app.pkg.workbook.sheets[0].cell(0, 6).cloned().unwrap();
            assert_eq!(col_values(&app, 6, 0, 2), seq, "pasted over {old:?}");
            assert_eq!(g1().spill, Some((3, 1)));
            for round in 0..2 {
                app.undo();
                assert_eq!(col_values(&app, 6, 0, 2), was, "undo {round} over {old:?}");
                app.redo();
                assert_eq!(col_values(&app, 6, 0, 2), seq, "redo {round} over {old:?}");
                let g1 = app.pkg.workbook.sheets[0].cell(0, 6).cloned().unwrap();
                assert_eq!(g1.spill, Some((3, 1)), "redo {round} over {old:?}");
                assert!(
                    app.pkg.workbook.sheets[0]
                        .cell(1, 6)
                        .unwrap()
                        .formula
                        .is_none()
                );
            }
            app.undo();
            assert_eq!(col_values(&app, 6, 0, 2), was);
            if old.is_some() {
                assert_eq!(was, vec![t("a"), t("b"), t("c")]);
            } else {
                assert_eq!(was, vec![CellValue::Empty; 3]);
            }
        }

        // Copying only a spill child pastes its value.
        let mut app = app_with_sequence_in_c1();
        app.cur = (1, 2);
        app.copy(false);
        app.cur = (1, 8);
        app.paste();
        assert_eq!(col_values(&app, 8, 1, 1), vec![n(2.0)]);
    }

    #[test]
    fn paste_single_anchor_over_occupied_spills_error() {
        // #777 / #725: a pasted anchor whose spill area is occupied is
        // #SPILL! and keeps the occupant, through undo and redo.
        let n = |v: f64| CellValue::Number(v);
        let spill = CellValue::Error("#SPILL!".into());
        let keep = CellValue::Text("keep".into());
        let mut app = app_with_sequence_in_c1();
        app.apply(vec![(1, 6, Cell::text("keep"))]);
        app.cur = (0, 2);
        app.copy(false);
        app.cur = (0, 6);
        app.paste();
        let after = vec![spill.clone(), keep.clone(), CellValue::Empty];
        assert_eq!(col_values(&app, 6, 0, 2), after);
        app.undo();
        assert_eq!(
            col_values(&app, 6, 0, 2),
            vec![CellValue::Empty, keep, CellValue::Empty]
        );
        app.redo();
        assert_eq!(col_values(&app, 6, 0, 2), after);

        // Over empty cells it spills; undo empties them; redo re-spills.
        app.cur = (0, 9);
        app.paste();
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        assert_eq!(col_values(&app, 9, 0, 2), seq);
        app.undo();
        assert_eq!(col_values(&app, 9, 0, 2), vec![CellValue::Empty; 3]);
        app.redo();
        assert_eq!(col_values(&app, 9, 0, 2), seq);
    }

    #[test]
    fn undo_redo_around_a_live_spill_keeps_it_spilling() {
        // #777 r1: spill output edited without its anchor is snapshotted as
        // the blank its anchor re-spills over, not as a plain value.
        let n = |v: f64| CellValue::Number(v);
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        let spill = CellValue::Error("#SPILL!".into());
        let x = CellValue::Text("x".into());
        let spilling = |app: &App| {
            let c1 = app.pkg.workbook.sheets[0].cell(0, 2).unwrap();
            c1.spill == Some((3, 1))
        };

        // Delete C2:C3 under C1's spill: a no-op, and so are undo and redo.
        let mut app = app_with_sequence_in_c1();
        app.apply(vec![(1, 2, Cell::default()), (2, 2, Cell::default())]);
        for step in ["delete", "undo", "redo", "undo"] {
            match step {
                "undo" => app.undo(),
                "redo" => app.redo(),
                _ => {}
            }
            assert_eq!(col_values(&app, 2, 0, 2), seq, "{step}");
            assert!(spilling(&app), "{step}");
        }

        // Type into a spill child: #SPILL!; undo re-spills; redo blocks again.
        let mut app = app_with_sequence_in_c1();
        app.apply(vec![(1, 2, Cell::text("x"))]);
        let blocked = vec![spill.clone(), x.clone(), CellValue::Empty];
        assert_eq!(col_values(&app, 2, 0, 2), blocked);
        app.undo();
        assert_eq!(col_values(&app, 2, 0, 2), seq);
        assert!(spilling(&app));
        app.redo();
        assert_eq!(col_values(&app, 2, 0, 2), blocked);
        app.undo();
        assert_eq!(col_values(&app, 2, 0, 2), seq);

        // Delete the blocker of a #SPILL! anchor: it spills; undo brings the
        // blocker back; redo spills again.
        app.apply(vec![(1, 2, Cell::text("x"))]);
        app.apply(vec![(1, 2, Cell::default())]);
        assert_eq!(col_values(&app, 2, 0, 2), seq);
        app.undo();
        assert_eq!(col_values(&app, 2, 0, 2), blocked);
        app.redo();
        assert_eq!(col_values(&app, 2, 0, 2), seq);
        assert!(spilling(&app));
    }

    #[test]
    fn a_frozen_array_block_keeps_its_cached_values() {
        // #777 r1: an array anchor the engine can't evaluate never re-spills,
        // so its cached block is pasted and undone as values. xlsxy opens on
        // cached values: nothing has been evaluated yet.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        sheet.set_cell(
            0,
            4,
            Cell {
                value: n(7.0),
                formula: Some("PIVOTBY(A1,4)".into()),
                f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 4, Cell::number(8.0));
        sheet.set_cell(2, 4, Cell::number(9.0));
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        app.anchor = Some((0, 4));
        app.cur = (2, 4);
        app.copy(false);
        app.anchor = None;
        app.cur = (0, 6);
        app.paste();
        assert_eq!(col_values(&app, 6, 0, 2), cached);
        // Cleared and undone, the block comes back whole.
        app.apply((0..3).map(|r| (r, 4, Cell::default())).collect());
        assert_eq!(col_values(&app, 4, 0, 2), vec![CellValue::Empty; 3]);
        app.undo();
        assert_eq!(col_values(&app, 4, 0, 2), cached);
    }

    #[test]
    fn undoing_an_overwrite_of_a_frozen_anchor_restores_its_block() {
        // #837: typing over a frozen anchor clears its cached block; undo
        // puts the block back, extent and saved `ref` too.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let typed = vec![n(5.0), CellValue::Empty, CellValue::Empty];
        let f_attrs = Some("t=\"array\" ref=\"E1:E3\"".to_string());
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        sheet.set_cell(
            0,
            4,
            Cell {
                value: n(7.0),
                formula: Some("PIVOTBY(A1,4)".into()),
                f_attrs: f_attrs.clone(),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 4, Cell::number(8.0));
        sheet.set_cell(2, 4, Cell::number(9.0));
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        let e1 = |app: &App| app.pkg.workbook.sheets[0].cell(0, 4).unwrap().clone();
        app.apply(vec![(0, 4, Cell::number(5.0))]);
        assert_eq!(col_values(&app, 4, 0, 2), typed);
        for round in 0..2 {
            app.undo();
            assert_eq!(col_values(&app, 4, 0, 2), cached, "undo {round}");
            assert_eq!(e1(&app).spill, Some((3, 1)), "undo {round}");
            assert_eq!(e1(&app).f_attrs, f_attrs, "undo {round}");
            if round == 0 {
                app.redo();
                assert_eq!(col_values(&app, 4, 0, 2), typed, "redo");
            }
        }
        let saved = gridcore::xlsx::save_xlsx(&app.pkg);
        let re = gridcore::xlsx::load_xlsx(&saved).unwrap();
        let ws = String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{ws}");
        let values: Vec<CellValue> = (0..3)
            .map(|r| re.workbook.sheets[0].cell(r, 4).unwrap().value.clone())
            .collect();
        assert_eq!(values, cached, "saved");
    }

    #[test]
    fn undoing_typing_into_a_frozen_block_restores_its_extent() {
        // #837 r2: a value typed into a frozen block drops its anchor's
        // extent; undo puts the extent back, so the block saves whole. One
        // cell, then every cell but the anchor.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let edits = [
            (
                vec![(1, 4, Cell::number(5.0))],
                vec![n(7.0), n(5.0), n(9.0)],
            ),
            (
                vec![(1, 4, Cell::number(5.0)), (2, 4, Cell::number(6.0))],
                vec![n(7.0), n(5.0), n(6.0)],
            ),
        ];
        for (edit, typed) in edits {
            let mut pkg = new_xlsx();
            let sheet = &mut pkg.workbook.sheets[0];
            sheet.set_cell(0, 0, Cell::number(1.0));
            sheet.set_cell(
                0,
                4,
                Cell {
                    value: n(7.0),
                    formula: Some("PIVOTBY(A1,4)".into()),
                    f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
                    spill: Some((3, 1)),
                    ..Cell::default()
                },
            );
            sheet.set_cell(1, 4, Cell::number(8.0));
            sheet.set_cell(2, 4, Cell::number(9.0));
            let mut app = App::new(pkg, "test.xlsx");
            app.os_clip = None;
            let at = format!("{} cells", edit.len());
            let extent = |app: &App| app.pkg.workbook.sheets[0].cell(0, 4).unwrap().spill;
            app.apply(edit);
            assert_eq!(col_values(&app, 4, 0, 2), typed, "{at}");
            for round in 0..2 {
                app.undo();
                assert_eq!(col_values(&app, 4, 0, 2), cached, "undo {round}, {at}");
                assert_eq!(extent(&app), Some((3, 1)), "undo {round}, {at}");
                if round == 0 {
                    app.redo();
                    assert_eq!(col_values(&app, 4, 0, 2), typed, "redo, {at}");
                }
            }
            let saved = gridcore::xlsx::save_xlsx(&app.pkg);
            let re = gridcore::xlsx::load_xlsx(&saved).unwrap();
            let ws =
                String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
            assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{at}: {ws}");
            let values: Vec<CellValue> = (0..3)
                .map(|r| re.workbook.sheets[0].cell(r, 4).unwrap().value.clone())
                .collect();
            assert_eq!(values, cached, "saved, {at}");
        }
    }

    #[test]
    fn undoing_an_overwrite_of_a_live_anchor_re_spills_it() {
        // #837 guard: a live anchor's block is not recorded; undo re-spills
        // it from its formula.
        let n = |v: f64| CellValue::Number(v);
        let seq = vec![n(1.0), n(2.0), n(3.0)];
        let x = vec![
            CellValue::Text("x".into()),
            CellValue::Empty,
            CellValue::Empty,
        ];
        let mut app = app_with_sequence_in_c1();
        app.apply(vec![(0, 2, Cell::text("x"))]);
        assert_eq!(col_values(&app, 2, 0, 2), x);
        app.undo();
        assert_eq!(col_values(&app, 2, 0, 2), seq);
        let c1 = app.pkg.workbook.sheets[0].cell(0, 2).unwrap();
        assert_eq!(c1.spill, Some((3, 1)));
        app.redo();
        assert_eq!(col_values(&app, 2, 0, 2), x);
    }

    #[test]
    fn deleting_a_cell_of_a_frozen_block_keeps_the_others() {
        // #777 r2/r3: a frozen anchor never re-spills. Deleting a cell of its
        // block is a no-op, as deleting a spilled cell is in Excel: values,
        // extent and saved `ref` stay, through undo and redo. Typing a value
        // there breaks the block but keeps the other cached cells. Legacy
        // CSE and dynamic-array blocks, on row 0 and off it.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let blocks = [
            (0u32, 4u32, false),
            (10, 5, false),
            (0, 6, true),
            (10, 7, true),
        ];
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        for (top, col, dynamic) in blocks {
            let range = format!("{}:{}", cell_name(top, col), cell_name(top + 2, col));
            sheet.set_cell(
                top,
                col,
                Cell {
                    value: n(7.0),
                    formula: Some("PIVOTBY(A1,4)".into()),
                    f_attrs: Some(format!("t=\"array\" ref=\"{range}\"")),
                    spill: Some((3, 1)),
                    meta: dynamic.then(|| {
                        Box::new(gridcore::sheet::CellMeta {
                            dynamic: true,
                            ..Default::default()
                        })
                    }),
                    ..Cell::default()
                },
            );
            sheet.set_cell(top + 1, col, Cell::number(8.0));
            sheet.set_cell(top + 2, col, Cell::number(9.0));
        }
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        for (top, col, dynamic) in blocks {
            let at = format!("{} dynamic={dynamic}", cell_name(top, col));
            let block = |app: &App| col_values(app, col, top, top + 2);
            let extent = |app: &App| app.pkg.workbook.sheets[0].cell(top, col).unwrap().spill;
            app.apply(vec![(top + 1, col, Cell::default())]);
            assert_eq!(block(&app), cached, "delete at {at}");
            assert_eq!(extent(&app), Some((3, 1)), "delete at {at}");
            app.undo();
            assert_eq!(block(&app), cached, "undo at {at}");
            assert_eq!(extent(&app), Some((3, 1)), "undo at {at}");
            app.redo();
            assert_eq!(block(&app), cached, "redo at {at}");
            assert_eq!(extent(&app), Some((3, 1)), "redo at {at}");
            let saved = gridcore::xlsx::save_xlsx(&app.pkg);
            let re = gridcore::xlsx::load_xlsx(&saved).unwrap();
            let ws =
                String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
            let range = format!("{}:{}", cell_name(top, col), cell_name(top + 2, col));
            assert!(
                ws.contains(&format!(r#"<f t="array" ref="{range}">"#)),
                "{at}: {ws}"
            );
            let values: Vec<CellValue> = (top..top + 3)
                .map(|r| re.workbook.sheets[0].cell(r, col).unwrap().value.clone())
                .collect();
            assert_eq!(values, cached, "saved at {at}");
        }
        // Typing a value into a frozen block keeps its other cached cells.
        app.apply(vec![(1, 4, Cell::number(5.0))]);
        assert_eq!(col_values(&app, 4, 0, 2), vec![n(7.0), n(5.0), n(9.0)]);
        app.undo();
        assert_eq!(col_values(&app, 4, 0, 2), cached);
        app.redo();
        assert_eq!(col_values(&app, 4, 0, 2), vec![n(7.0), n(5.0), n(9.0)]);
    }

    /// An app over a frozen array block: A1 = 1, E1 `PIVOTBY(A1,4)`
    /// (`ref="E1:E3"`, cached 7) with `e2` and `e3` as its other cached cells.
    fn app_with_frozen_block(e2: Cell, e3: Cell) -> App {
        let mut pkg = new_xlsx();
        let sheet = &mut pkg.workbook.sheets[0];
        sheet.set_cell(0, 0, Cell::number(1.0));
        sheet.set_cell(
            0,
            4,
            Cell {
                value: CellValue::Number(7.0),
                formula: Some("PIVOTBY(A1,4)".into()),
                f_attrs: Some("t=\"array\" ref=\"E1:E3\"".into()),
                spill: Some((3, 1)),
                ..Cell::default()
            },
        );
        sheet.set_cell(1, 4, e2);
        sheet.set_cell(2, 4, e3);
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        app
    }

    #[test]
    fn retyping_a_frozen_anchor_keeps_its_block() {
        // #840: the same formula typed over a frozen anchor (no F2: an
        // unchanged F2 commits nothing) keeps its cached block, extent and
        // saved `ref`, through undo and redo.
        let n = |v: f64| CellValue::Number(v);
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        let mut app = app_with_frozen_block(Cell::number(8.0), Cell::number(9.0));
        let extent = |app: &App| app.pkg.workbook.sheets[0].cell(0, 4).unwrap().spill;
        app.cur = (0, 4);
        app.start_edit(Some('='));
        if let Some(e) = &mut app.edit {
            e.text = "=PIVOTBY(A1,4)".into();
            e.cursor = e.text.chars().count();
        }
        assert!(app.commit_edit());
        assert_eq!(col_values(&app, 4, 0, 2), cached, "typed");
        assert_eq!(extent(&app), Some((3, 1)), "typed");
        app.undo();
        assert_eq!(col_values(&app, 4, 0, 2), cached, "undo");
        assert_eq!(extent(&app), Some((3, 1)), "undo");
        app.redo();
        assert_eq!(col_values(&app, 4, 0, 2), cached, "redo");
        assert_eq!(extent(&app), Some((3, 1)), "redo");
        let saved = gridcore::xlsx::save_xlsx(&app.pkg);
        let re = gridcore::xlsx::load_xlsx(&saved).unwrap();
        let ws = String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        assert!(ws.contains(r#"<f t="array" ref="E1:E3">"#), "{ws}");
        let values: Vec<CellValue> = (0..3)
            .map(|r| re.workbook.sheets[0].cell(r, 4).unwrap().value.clone())
            .collect();
        assert_eq!(values, cached, "saved");
    }

    #[test]
    fn a_fill_or_replace_mixing_blanks_into_a_frozen_block_clears_them() {
        // #840 (r4-m1): a group that puts a blank and a value into one frozen
        // block breaks the block, and the blank clears its cell, though it
        // comes first. Fill right D2:E3 from D2 = blank, D3 = 5.
        let n = |v: f64| CellValue::Number(v);
        let t = |s: &str| CellValue::Text(s.into());
        let mut app = app_with_frozen_block(Cell::number(8.0), Cell::number(9.0));
        app.pkg.workbook.sheets[0].set_cell(2, 3, Cell::number(5.0));
        app.anchor = Some((1, 3));
        app.cur = (2, 4);
        app.fill(FillDir::Right);
        let filled = vec![n(7.0), CellValue::Empty, n(5.0)];
        assert_eq!(col_values(&app, 4, 0, 2), filled, "fill");
        app.undo();
        assert_eq!(col_values(&app, 4, 0, 2), vec![n(7.0), n(8.0), n(9.0)]);
        // #840 r2: redo ends as the fill did.
        app.redo();
        assert_eq!(col_values(&app, 4, 0, 2), filled, "redo fill");
        let e1 = |app: &App| app.pkg.workbook.sheets[0].cell(0, 4).unwrap().spill;
        assert_eq!(e1(&app), None, "redo fill");

        // Replace "x" with "" over E2 "x", E3 "xy".
        let mut app = app_with_frozen_block(Cell::text("x"), Cell::text("xy"));
        app.replace_all("x", "");
        let replaced = vec![n(7.0), CellValue::Empty, t("y")];
        assert_eq!(col_values(&app, 4, 0, 2), replaced, "replace");
        app.undo();
        assert_eq!(col_values(&app, 4, 0, 2), vec![n(7.0), t("x"), t("xy")]);
        app.redo();
        assert_eq!(col_values(&app, 4, 0, 2), replaced, "redo replace");
        assert_eq!(e1(&app), None, "redo replace");
    }

    #[test]
    fn cutting_a_frozen_block_cell_into_the_block_clears_its_source() {
        // #840 r3 p1: cut E3 of a frozen block, paste at E2. The clear of E3
        // is a no-op while the block is whole, so it goes after the paste,
        // which breaks the block: E3 is cleared, through undo and redo.
        let n = |v: f64| CellValue::Number(v);
        let mut app = app_with_frozen_block(Cell::number(8.0), Cell::number(9.0));
        let moved = vec![n(7.0), n(9.0), CellValue::Empty];
        app.anchor = None;
        app.cur = (2, 4);
        app.copy(true);
        app.cur = (1, 4);
        app.paste();
        assert_eq!(col_values(&app, 4, 0, 2), moved, "paste");
        app.undo();
        assert_eq!(col_values(&app, 4, 0, 2), vec![n(7.0), n(8.0), n(9.0)]);
        let extent = |app: &App| app.pkg.workbook.sheets[0].cell(0, 4).unwrap().spill;
        assert_eq!(extent(&app), Some((3, 1)), "undo");
        app.redo();
        assert_eq!(col_values(&app, 4, 0, 2), moved, "redo");
    }

    #[test]
    fn rejects_bad_formula_at_entry() {
        let pkg = new_xlsx();
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        app.start_edit(Some('='));
        if let Some(e) = &mut app.edit {
            e.text.push_str("SUM((");
            e.cursor = e.text.chars().count();
        }
        assert!(!app.commit_edit());
        assert!(app.edit.is_some()); // still editing
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("formula error")
        );
    }

    #[test]
    fn row_insert_rewrites_and_undoes() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(1.0));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::number(2.0));
        pkg.workbook.sheets[0].set_cell(
            2,
            0,
            Cell {
                value: CellValue::Number(3.0),
                formula: Some("SUM(A1:A2)".into()),
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.cur = (1, 0); // insert one row above row 2
        app.row_op(true);
        let s = &app.pkg.workbook.sheets[0];
        assert_eq!(s.cell(2, 0).unwrap().value, CellValue::Number(2.0));
        assert_eq!(s.cell(3, 0).unwrap().formula.as_deref(), Some("SUM(A1:A3)"));
        assert_eq!(s.cell(3, 0).unwrap().value, CellValue::Number(3.0));
        // Structural undo restores the original grid.
        app.undo();
        let s = &app.pkg.workbook.sheets[0];
        assert_eq!(s.cell(1, 0).unwrap().value, CellValue::Number(2.0));
        assert_eq!(s.cell(2, 0).unwrap().formula.as_deref(), Some("SUM(A1:A2)"));
        // And redo replays it.
        app.redo();
        let s = &app.pkg.workbook.sheets[0];
        assert_eq!(s.cell(3, 0).unwrap().formula.as_deref(), Some("SUM(A1:A3)"));
    }

    #[test]
    fn fill_down_translates_relative_refs() {
        let mut pkg = new_xlsx();
        for r in 0..4 {
            pkg.workbook.sheets[0].set_cell(r, 0, Cell::number((r + 1) as f64));
        }
        pkg.workbook.sheets[0].set_cell(
            0,
            1,
            Cell {
                value: CellValue::Number(2.0),
                formula: Some("A1*2".into()),
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        // Select B1:B4 and fill down.
        app.cur = (3, 1);
        app.anchor = Some((0, 1));
        app.fill(FillDir::Down);
        let s = &app.pkg.workbook.sheets[0];
        assert_eq!(s.cell(2, 1).unwrap().formula.as_deref(), Some("A3*2"));
        assert_eq!(s.cell(3, 1).unwrap().value, CellValue::Number(8.0));
    }

    #[test]
    fn find_wraps_and_matches_formulas() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("hello world"));
        pkg.workbook.sheets[0].set_cell(
            4,
            2,
            Cell {
                value: CellValue::Number(0.0),
                formula: Some("SUM(Z1:Z9)".into()),
                ..Cell::default()
            },
        );
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.find_next("WORLD");
        assert_eq!(app.cur, (0, 0));
        app.find_next("sum(z");
        assert_eq!(app.cur, (4, 2));
        // Wraps back around.
        app.find_next("world");
        assert_eq!(app.cur, (0, 0));
    }

    #[test]
    fn sheet_add_rename_delete() {
        let pkg = new_xlsx();
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        // Add a sheet via the prompt path.
        app.open_prompt(PromptKind::AddSheet);
        if let Some(p) = &mut app.prompt {
            p.text = "Budget".to_string();
        }
        app.commit_prompt();
        assert_eq!(app.pkg.workbook.sheets.len(), 2);
        assert_eq!(app.sheet, 1);
        // Rename it (structural: formulas elsewhere would follow).
        app.open_prompt(PromptKind::RenameSheet);
        if let Some(p) = &mut app.prompt {
            p.text = "Plan".to_string();
        }
        app.commit_prompt();
        assert_eq!(app.pkg.workbook.sheets[1].name, "Plan");
        // Delete it.
        app.delete_current_sheet();
        assert_eq!(app.pkg.workbook.sheets.len(), 1);
        // The last one refuses to go.
        app.delete_current_sheet();
        assert_eq!(app.pkg.workbook.sheets.len(), 1);
    }

    #[test]
    fn rename_sheet_round_trips_through_save() {
        // The TUI rename is model-only (gridcore::edit::rename_sheet); it relies on
        // save_xlsx re-syncing workbook.xml (patch_sheet_names). Guard that path so
        // a regression there can't silently drop the rename on reload.
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.open_prompt(PromptKind::AddSheet);
        app.prompt.as_mut().unwrap().text = "Data".to_string();
        app.commit_prompt();
        app.open_prompt(PromptKind::RenameSheet);
        app.prompt.as_mut().unwrap().text = "Budget".to_string();
        app.commit_prompt();

        let reloaded = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let names: Vec<&str> = reloaded
            .workbook
            .sheets
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            names.contains(&"Budget"),
            "rename lost on reload: {names:?}"
        );
        assert!(!names.contains(&"Data"), "stale name survived: {names:?}");
    }

    #[test]
    fn f2_enter_keeps_every_digit_and_typing_over_still_converts() {
        let mut pkg = new_xlsx();
        let noisy = 0.1 + 0.2;
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(noisy));
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("5"));
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        // The seed is the stored number in full, as gridwasm and the suite
        // seed it, and committing it unchanged changes nothing.
        assert_eq!(app.current_input_text(), "0.30000000000000004");
        app.start_edit(None);
        assert!(app.commit_edit());
        let a1 = app.sheet().cell(0, 0).unwrap().value.clone();
        assert_eq!(a1, CellValue::Number(noisy));
        assert!(app.undo.is_empty(), "an unchanged edit adds no undo step");
        // Typing over a text 5 with 5 is a fresh entry: it becomes a number.
        app.cur = (1, 0);
        app.start_edit(Some('5'));
        assert!(app.commit_edit());
        let a2 = app.sheet().cell(1, 0).unwrap().value.clone();
        assert_eq!(a2, CellValue::Number(5.0));
        // F2 on the same text 5 and Enter keeps it text.
        app.pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("5"));
        app.start_edit(None);
        assert!(app.commit_edit());
        let a2 = app.sheet().cell(1, 0).unwrap().value.clone();
        assert_eq!(a2, CellValue::Text("5".into()));
    }

    #[test]
    fn a_percent_cell_seeds_its_percent() {
        let mut pkg = new_xlsx();
        let mut xf = gridcore::sheet::Xf::default();
        xf.set_code(Some("0%".into()));
        let style = pkg.workbook.styles.intern(xf);
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                style,
                ..Cell::number(1.5)
            },
        );
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        assert_eq!(app.current_input_text(), "150%");
        // Edited to 160%, it is 1.6, not 0.016.
        app.start_edit(None);
        if let Some(e) = app.edit.as_mut() {
            e.text = "160%".into();
        }
        assert!(app.commit_edit());
        let a1 = app.sheet().cell(0, 0).unwrap().value.clone();
        assert_eq!(a1, CellValue::Number(1.6));
    }

    #[test]
    fn formula_bar_text_reconstructs_input() {
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(
            0,
            0,
            Cell {
                value: CellValue::Number(6.0),
                formula: Some("2*3".into()),
                ..Cell::default()
            },
        );
        pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("plain"));
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        app.cur = (0, 0);
        assert_eq!(app.current_input_text(), "=2*3");
        app.cur = (1, 0);
        assert_eq!(app.current_input_text(), "plain");
    }

    #[test]
    fn pivot_editor_edits_fields_and_refreshes_live() {
        use gridcore::pivot::{DataField, Pivot, PivotSource};
        let mut pkg = new_xlsx();
        // Data: Region | Sales
        let sh = &mut pkg.workbook.sheets[0];
        sh.set_cell(0, 0, Cell::text("Region"));
        sh.set_cell(0, 1, Cell::text("Sales"));
        for (i, (r, v)) in [("East", 10.0), ("West", 20.0), ("East", 30.0)]
            .iter()
            .enumerate()
        {
            sh.set_cell(i as u32 + 1, 0, Cell::text(r));
            sh.set_cell(i as u32 + 1, 1, Cell::number(*v));
        }
        pkg.workbook.pivots.push(Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 3, 0, 3), // D1
            source: PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 3, 1),
            },
            fields: vec!["Region".into(), "Sales".into()],
            row_fields: vec![0],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Sales".into(),
                field: 1,
                agg: gridcore::frame::Agg::Sum,
            }],
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        });
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        app.open_pivot_editor();
        assert!(app.pivot_edit.is_some());

        // Cycle the value field's aggregation: Values pane, 'a' → Count.
        app.pivot_editor_key(KeyCode::Tab, false); // rows
        app.pivot_editor_key(KeyCode::Tab, false); // cols
        app.pivot_editor_key(KeyCode::Tab, false); // values
        app.pivot_editor_key(KeyCode::Char('a'), false);
        let piv = &app.pkg.workbook.pivots[0];
        assert_eq!(piv.data_fields[0].agg, gridcore::frame::Agg::Count);
        assert_eq!(piv.data_fields[0].name, "Count of Sales");
        assert!(piv.edited);
        // Live refresh wrote the new output (East 2, West 1, total 3).
        let val = |app: &App, r: u32, c: u32| {
            app.pkg.workbook.sheets[0]
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(val(&app, 0, 4), CellValue::Text("Count of Sales".into()));
        assert_eq!(val(&app, 1, 4), CellValue::Number(2.0));
        assert_eq!(val(&app, 2, 4), CellValue::Number(1.0));
        assert_eq!(val(&app, 3, 4), CellValue::Number(3.0));

        // Remove the row field: Rows pane, 'd' → single Total row.
        app.pivot_editor_key(KeyCode::BackTab, false);
        app.pivot_editor_key(KeyCode::BackTab, false); // rows
        app.pivot_editor_key(KeyCode::Char('d'), false);
        assert!(app.pkg.workbook.pivots[0].row_fields.is_empty());
        assert_eq!(val(&app, 1, 3), CellValue::Text("Total".into()));
        assert_eq!(val(&app, 1, 4), CellValue::Number(3.0));

        // Esc closes.
        app.pivot_editor_key(KeyCode::Esc, false);
        assert!(app.pivot_edit.is_none());
        assert!(app.modified);
    }

    #[test]
    fn pivot_editor_overlay_renders_on_small_terminals() {
        use gridcore::pivot::{DataField, Pivot, PivotSource};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("Region"));
        pkg.workbook.sheets[0].set_cell(0, 1, Cell::text("Sales"));
        pkg.workbook.pivots.push(Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 3, 0, 3),
            source: PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 1, 1),
            },
            fields: vec!["Region".into(), "Sales".into()],
            row_fields: vec![0],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Sales".into(),
                field: 1,
                agg: gridcore::frame::Agg::Sum,
            }],
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        });
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        app.open_pivot_editor();
        // A comfortable size and pathologically small ones must not panic.
        for (w, h) in [(100u16, 30u16), (20, 6), (13, 5)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(&mut app, f)).unwrap();
        }
        // The overlay shows the pane titles at a normal size.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Fields"));
        assert!(text.contains("Values"));
        assert!(text.contains("Pivot: P"));
    }

    #[test]
    fn csv_imports_as_workbook() {
        let open = csv_open(AutoConvert::default());
        let pkg = csv_to_pkg(
            "Region,Sales\nEast,10\n\"West, far\",20.5\n",
            "sales",
            false,
            &open,
        );
        let sh = &pkg.workbook.sheets[0];
        assert_eq!(sh.name, "sales");
        assert_eq!(
            sh.cell(0, 0).unwrap().value,
            CellValue::Text("Region".into())
        );
        assert_eq!(sh.cell(1, 1).unwrap().value, CellValue::Number(10.0));
        assert_eq!(
            sh.cell(2, 0).unwrap().value,
            CellValue::Text("West, far".into())
        );
        assert_eq!(sh.cell(2, 1).unwrap().value, CellValue::Number(20.5));
        // The imported workbook saves as a valid xlsx and round-trips.
        let bytes = save_xlsx(&pkg);
        let pkg2 = load_xlsx(&bytes).unwrap();
        assert_eq!(
            pkg2.workbook.sheets[0].cell(2, 1).unwrap().value,
            CellValue::Number(20.5)
        );
    }

    #[test]
    fn model_definitions_persist_and_build_reports() {
        // Workbook with a Sales table and a Products table on one sheet.
        let mut pkg = new_xlsx();
        {
            let sh = &mut pkg.workbook.sheets[0];
            for (c, h) in ["PID", "Amount"].iter().enumerate() {
                sh.set_cell(0, c as u32, Cell::text(h));
            }
            for (i, (pid, amt)) in [(1.0, 10.0), (2.0, 20.0), (1.0, 30.0)].iter().enumerate() {
                sh.set_cell(i as u32 + 1, 0, Cell::number(*pid));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*amt));
            }
            for (c, h) in ["ID", "Cat"].iter().enumerate() {
                sh.set_cell(0, c as u32 + 3, Cell::text(h));
            }
            for (i, (id, cat)) in [(1.0, "A"), (2.0, "B")].iter().enumerate() {
                sh.set_cell(i as u32 + 1, 3, Cell::number(*id));
                sh.set_cell(i as u32 + 1, 4, Cell::text(cat));
            }
        }
        let table = |name: &str, range, cols: &[&str]| gridcore::sheet::Table {
            name: name.into(),
            sheet: 0,
            range,
            header_rows: 1,
            totals_rows: 0,
            columns: cols.iter().map(|s| s.to_string()).collect(),
            part: String::new(),
            column_ids: Vec::new(),
        };
        pkg.workbook
            .tables
            .push(table("Sales", (0, 0, 3, 1), &["PID", "Amount"]));
        pkg.workbook
            .tables
            .push(table("Products", (0, 3, 2, 4), &["ID", "Cat"]));
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;

        // Add a relationship and a measure through the prompt handlers.
        app.prompt = Some(Prompt {
            kind: PromptKind::Relate,
            label: "",
            text: "Sales[PID] = Products[ID]".into(),
            cursor: 0,
        });
        app.commit_prompt();
        assert_eq!(app.model_rels.len(), 1, "{:?}", app.status);
        app.prompt = Some(Prompt {
            kind: PromptKind::Measure,
            label: "",
            text: "Total = SUM(Sales[Amount])".into(),
            cursor: 0,
        });
        app.commit_prompt();
        assert_eq!(app.model_measures.len(), 1);
        // A bad relationship is rejected with a message.
        app.prompt = Some(Prompt {
            kind: PromptKind::Relate,
            label: "",
            text: "Sales[Nope] = Products[ID]".into(),
            cursor: 0,
        });
        app.commit_prompt();
        assert_eq!(app.model_rels.len(), 1);

        // Build a report grouped by the related dimension column.
        app.prompt = Some(Prompt {
            kind: PromptKind::ModelPivot,
            label: "",
            text: "Sales; Products[Cat]; Total".into(),
            cursor: 0,
        });
        app.commit_prompt();
        let idx = app
            .pkg
            .workbook
            .sheet_index("Model Pivot")
            .expect("report sheet");
        let sh = &app.pkg.workbook.sheets[idx];
        assert_eq!(
            sh.cell(0, 1).unwrap().value,
            CellValue::Text("Total".into())
        );
        assert_eq!(sh.cell(1, 0).unwrap().value, CellValue::Text("A".into()));
        assert_eq!(sh.cell(1, 1).unwrap().value, CellValue::Number(40.0));
        assert_eq!(sh.cell(2, 1).unwrap().value, CellValue::Number(20.0));
        assert_eq!(
            sh.cell(3, 0).unwrap().value,
            CellValue::Text("Grand Total".into())
        );
        assert_eq!(sh.cell(3, 1).unwrap().value, CellValue::Number(60.0));

        // Definitions survive save → load via the custom part. (The
        // in-memory test Tables have no parts, so only the definitions —
        // not the tables — are expected back.)
        let bytes = app.package_bytes();
        let pkg2 = load_xlsx(&bytes).unwrap();
        let app2 = App::new(pkg2, "test.xlsx");
        assert_eq!(app2.model_rels, app.model_rels);
        assert_eq!(app2.model_measures.len(), 1);
        assert_eq!(app2.model_measures[0].formula, "SUM(Sales[Amount])");

        // The model view overlay renders.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        app.open_model_view();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Relationships"));
        assert!(text.contains("Measures"));
    }

    #[test]
    fn format_dialog_renders() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app.open_format_dialog();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Format Cells"), "title missing");
        assert!(
            text.contains("Number") && text.contains("Border"),
            "section tabs missing"
        );
        assert!(text.contains("General"), "number options missing");

        // Move to the Align section; its options render.
        app.format_dialog_key(KeyCode::Right); // Font
        app.format_dialog_key(KeyCode::Right); // Fill
        app.format_dialog_key(KeyCode::Right); // Align
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Center"), "align options missing");
    }

    #[test]
    fn chart_bar_lines_scales_bars() {
        use gridcore::sheet::{ChartData, ChartSeries};
        let cd = ChartData {
            title: "T".into(),
            kind: "bar".into(),
            categories: vec!["North".into(), "South".into()],
            series: vec![ChartSeries {
                name: "s".into(),
                values: vec![5.0, 10.0],
                ..Default::default()
            }],
            ..Default::default()
        };
        let lines = chart_bar_lines(&cd, 40, 6);
        assert_eq!(lines.len(), 2);
        let render = |l: &RLine| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let (r0, r1) = (render(&lines[0]), render(&lines[1]));
        assert!(r0.contains("North") && r0.contains('5'));
        assert!(r1.contains("South") && r1.contains("10"));
        // The larger value gets the longer bar.
        let bars = |s: &str| s.chars().filter(|&c| c == '\u{2588}').count();
        assert!(bars(&r1) > bars(&r0), "10 should outbar 5: {r0:?} {r1:?}");
    }

    #[test]
    fn num_short_is_compact() {
        assert_eq!(num_short(42.0), "42");
        assert_eq!(num_short(1500.0), "1.5k");
        assert_eq!(num_short(2_000_000.0), "2.0M");
        assert_eq!(num_short(3.25), "3.25");
    }

    #[test]
    fn drawings_render_and_toggle() {
        use gridcore::sheet::{ChartData, ChartSeries, Drawing, DrawingKind};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].drawings.push(Drawing {
            anchor_ix: 0,
            from: (1, 1),
            to: (14, 8),
            kind: DrawingKind::Chart(ChartData {
                title: "Revenue".into(),
                kind: "bar".into(),
                categories: vec!["North".into()],
                series: vec![ChartSeries {
                    name: "y".into(),
                    values: vec![10.0],
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        let mut app = App::new(pkg, "t.xlsx");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        assert!(
            format!("{:?}", term.backend().buffer()).contains("Revenue"),
            "chart title should be drawn"
        );
        // The Objects toggle hides them.
        app.show_drawings = false;
        term.draw(|f| draw(&mut app, f)).unwrap();
        assert!(
            !format!("{:?}", term.backend().buffer()).contains("Revenue"),
            "chart should be hidden when Objects is off"
        );
    }

    #[test]
    fn image_drawing_falls_back_to_box() {
        use gridcore::sheet::{Drawing, DrawingKind};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].drawings.push(Drawing {
            anchor_ix: 0,
            from: (1, 1),
            to: (6, 5),
            kind: DrawingKind::Image {
                part: "xl/media/image1.png".into(),
                name: "Logo".into(),
            },
        });
        // No picker is set in tests, so a picture renders its labelled box.
        let mut app = App::new(pkg, "t.xlsx");
        assert!(app.picker.is_none());
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Logo"), "picture label missing");
        assert!(text.contains("picture"), "picture caption missing");
    }

    #[test]
    fn ctrl_p_creates_pivot_from_selection() {
        let mut pkg = new_xlsx();
        {
            let sh = &mut pkg.workbook.sheets[0];
            for (c, h) in ["Region", "Sales"].iter().enumerate() {
                sh.set_cell(0, c as u32, Cell::text(h));
            }
            for (i, (r, v)) in [("East", 10.0), ("West", 20.0), ("East", 30.0)]
                .iter()
                .enumerate()
            {
                sh.set_cell(i as u32 + 1, 0, Cell::text(r));
                sh.set_cell(i as u32 + 1, 1, Cell::number(*v));
            }
        }
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        // Select A1:B4 and hit Ctrl-P.
        app.anchor = Some((0, 0));
        app.cur = (3, 1);
        app.open_pivot_editor();
        // A pivot exists on a fresh sheet with the editor open.
        assert_eq!(app.pkg.workbook.pivots.len(), 1);
        assert!(app.pivot_edit.is_some());
        assert_eq!(app.pkg.workbook.sheets[app.sheet].name, "Pivot");
        let piv = &app.pkg.workbook.pivots[0];
        assert_eq!(piv.fields, vec!["Region", "Sales"]);
        assert_eq!(piv.data_fields[0].name, "Sum of Sales");
        assert!(piv.edited);
        // Add Region to rows through the editor: Fields pane, 'r'.
        app.pivot_editor_key(KeyCode::Char('r'), false);
        let val = |app: &App, r: u32, c: u32| {
            app.pkg.workbook.sheets[1]
                .cell(r, c)
                .map(|cl| cl.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(val(&app, 3, 0), CellValue::Text("East".into()));
        assert_eq!(val(&app, 3, 1), CellValue::Number(40.0));
        assert_eq!(val(&app, 5, 1), CellValue::Number(60.0));
        // The created pivot survives a save/reload as a real pivot.
        let bytes = app.package_bytes();
        let pkg2 = load_xlsx(&bytes).unwrap();
        assert_eq!(pkg2.workbook.pivots.len(), 1);
        assert!(!pkg2.workbook.pivots[0].unsupported);
        assert_eq!(pkg2.workbook.pivots[0].row_fields, vec![0]);
    }

    #[test]
    fn pivot_editor_reorders_fields() {
        use gridcore::pivot::{DataField, Pivot, PivotSource};
        let mut pkg = new_xlsx();
        {
            let sh = &mut pkg.workbook.sheets[0];
            for (c, h) in ["Region", "Product", "Sales"].iter().enumerate() {
                sh.set_cell(0, c as u32, Cell::text(h));
            }
            for (i, (r, p, v)) in [("East", "Pen", 10.0), ("West", "Pad", 20.0)]
                .iter()
                .enumerate()
            {
                sh.set_cell(i as u32 + 1, 0, Cell::text(r));
                sh.set_cell(i as u32 + 1, 1, Cell::text(p));
                sh.set_cell(i as u32 + 1, 2, Cell::number(*v));
            }
        }
        pkg.workbook.pivots.push(Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 4, 0, 4),
            source: PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 2, 2),
            },
            fields: vec!["Region".into(), "Product".into(), "Sales".into()],
            row_fields: vec![0, 1],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Sales".into(),
                field: 2,
                agg: Agg::Sum,
            }],
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: false,
            grand_cols: false,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        });
        let mut app = App::new(pkg, "test.xlsx");
        app.os_clip = None;
        app.open_pivot_editor();
        // Rows pane: move Product above Region.
        app.pivot_editor_key(KeyCode::Tab, false); // rows
        app.pivot_editor_key(KeyCode::Down, false); // select Product
        app.pivot_editor_key(KeyCode::Up, true); // Shift-Up: reorder
        assert_eq!(app.pkg.workbook.pivots[0].row_fields, vec![1, 0]);
        assert!(app.pkg.workbook.pivots[0].edited);
        // The refreshed header shows Product as the outer label column.
        let v = app.pkg.workbook.sheets[0]
            .cell(0, 4)
            .map(|cl| cl.value.clone());
        assert_eq!(v, Some(CellValue::Text("Product".into())));
        // Edges are no-ops.
        app.pivot_editor_key(KeyCode::Up, true);
        app.pivot_editor_key(KeyCode::Up, true);
        assert_eq!(app.pkg.workbook.pivots[0].row_fields, vec![1, 0]);
    }

    #[test]
    fn robustness_fixes() {
        // Whole-sheet clear/copy iterate only the used range, not the grid.
        let mut pkg = new_xlsx();
        pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(1.0));
        pkg.workbook.sheets[0].set_cell(1, 1, Cell::number(2.0));
        let mut app = App::new(pkg, "t.xlsx");
        app.os_clip = None;
        // Select A1 : XFD1048576 (the whole grid) and clear — must be instant.
        app.cur = (MAX_ROWS - 1, MAX_COLS - 1);
        app.anchor = Some((0, 0));
        app.clear_selection();
        assert_eq!(app.sheet().cell(0, 0), None);
        assert_eq!(app.sheet().cell(1, 1), None);

        // parse_input never yields a non-finite number.
        assert!(matches!(parse_input("1e999").value, CellValue::Text(_)));
        assert!(matches!(parse_input("1e999%").value, CellValue::Text(_)));

        // Display-width fit is exact for wide glyphs.
        assert_eq!(disp_width("中文"), 4);
        assert_eq!(fit("中", 4, false).chars().count(), 3); // 2 cols glyph + 2 pad spaces = 4 cols
        assert_eq!(disp_width(&fit("中文字", 4, false)), 4);
    }

    #[test]
    fn arg_parsing_guards() {
        assert!(
            parse_args(&[
                "a.xlsx".into(),
                "--recalc".into(),
                "o.xlsx".into(),
                "--csv".into(),
                "o.csv".into()
            ])
            .is_err()
        );
        assert!(parse_args(&["-".into()]).is_err());
        assert!(parse_args(&["a.xlsx".into(), "--recalc".into(), "o.xlsx".into()]).is_ok());
        let p = parse_args(&["a.xlsx".into(), "--pdf".into(), "o.pdf".into()]).unwrap();
        assert_eq!(p.pdf_out.as_deref(), Some("o.pdf"));
        assert!(parse_args(&["a.xlsx".into(), "--pdf".into()]).is_err());
        assert!(
            parse_args(&[
                "a.xlsx".into(),
                "--pdf".into(),
                "o.pdf".into(),
                "--csv".into(),
                "o.csv".into()
            ])
            .is_err()
        );
    }

    /// #882: `--read-only` and `-r` are flags, not files or unknown options,
    /// and go with the headless modes too.
    #[test]
    fn read_only_flag_parses() {
        for flag in ["--read-only", "-r"] {
            let p = parse_args(&["a.xlsx".into(), flag.into()]).unwrap();
            assert!(p.read_only, "{flag}");
            assert_eq!(p.inputs, vec!["a.xlsx".to_string()]);
        }
        assert!(!parse_args(&["a.xlsx".into()]).unwrap().read_only);
        let p =
            parse_args(&["-r".into(), "a.xlsx".into(), "--csv".into(), "o.csv".into()]).unwrap();
        assert!(p.read_only && p.csv_out.is_some());
        assert!(parse_args(&["--read-onl".into()]).is_err(), "a near miss");
    }

    // ---- #882: Open/New over unsaved changes, and read-only ----

    /// A scratch folder holding `a.xlsx` (A1 = 1) and `b.xlsx` (A1 = 2), and
    /// an app on `a.xlsx`.
    fn two_books(name: &str) -> (std::path::PathBuf, App) {
        use gridcore::sheet::Cell;
        let dir = macro_dir(name);
        for (file, v) in [("a.xlsx", 1.0), ("b.xlsx", 2.0)] {
            let mut pkg = new_xlsx();
            pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(v));
            std::fs::write(dir.join(file), save_xlsx(&pkg)).unwrap();
        }
        let a = dir.join("a.xlsx");
        let pkg = load_xlsx(&std::fs::read(&a).unwrap()).unwrap();
        let mut app = App::new(pkg, a.to_str().unwrap());
        app.os_clip = None;
        (dir, app)
    }

    fn a1(app: &App) -> f64 {
        match app.pkg.workbook.sheets[0].cell(0, 0).map(|c| &c.value) {
            Some(gridcore::sheet::CellValue::Number(n)) => *n,
            other => panic!("A1 is {other:?}"),
        }
    }

    fn type_into_a1(app: &mut App, v: f64) {
        use gridcore::sheet::Cell;
        app.pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(v));
        app.modified = true;
    }

    fn y() -> KeyEvent {
        KeyEvent::from(KeyCode::Char('y'))
    }

    fn n() -> KeyEvent {
        KeyEvent::from(KeyCode::Char('n'))
    }

    /// Open from the backstage over unsaved changes asks; No keeps them.
    #[test]
    fn open_over_unsaved_changes_asks_and_no_keeps_them() {
        let (dir, mut app) = two_books("discard-no");
        type_into_a1(&mut app, 9.0);
        let b = dir.join("b.xlsx");
        app.apply_backstage_event(backstage::BackstageEvent::Open(b.clone()));
        let c = app.confirm.as_ref().expect("Open asks first");
        assert_eq!(
            c.prompt(),
            "Discard changes to \"a.xlsx\" and open \"b.xlsx\"?"
        );
        assert!(app.backstage.is_none());
        assert_eq!(a1(&app), 9.0, "nothing is opened before the answer");
        assert!(!app.confirm_key(n()));
        assert!(app.confirm.is_none());
        assert!(app.modified);
        assert_eq!(a1(&app), 9.0, "No keeps the changes");
        assert!(app.path.ends_with("a.xlsx"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Yes discards the changes and opens the other file.
    #[test]
    fn open_over_unsaved_changes_yes_opens() {
        let (dir, mut app) = two_books("discard-yes");
        type_into_a1(&mut app, 9.0);
        let b = dir.join("b.xlsx");
        app.apply_backstage_event(backstage::BackstageEvent::Open(b.clone()));
        assert!(!app.confirm_key(y()));
        assert!(app.confirm.is_none());
        assert_eq!(Path::new(&app.path), b);
        assert!(!app.modified);
        assert_eq!(a1(&app), 2.0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// With nothing to lose, Open goes ahead without asking.
    #[test]
    fn open_with_no_changes_does_not_ask() {
        let (dir, mut app) = two_books("discard-clean");
        let b = dir.join("b.xlsx");
        app.apply_backstage_event(backstage::BackstageEvent::Open(b.clone()));
        assert!(app.confirm.is_none());
        assert_eq!(Path::new(&app.path), b);
        assert_eq!(a1(&app), 2.0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// New from the backstage over unsaved changes asks too.
    #[test]
    fn new_over_unsaved_changes_asks() {
        let (dir, mut app) = two_books("discard-new");
        type_into_a1(&mut app, 9.0);
        app.apply_backstage_event(backstage::BackstageEvent::New);
        assert_eq!(
            app.confirm.as_ref().expect("New asks first").prompt(),
            "Discard changes to \"a.xlsx\" and start a new workbook?"
        );
        assert!(!app.confirm_key(n()));
        assert_eq!(a1(&app), 9.0);
        assert!(app.path.ends_with("a.xlsx"));
        app.apply_backstage_event(backstage::BackstageEvent::New);
        assert!(!app.confirm_key(y()));
        assert_eq!(app.path, "untitled.xlsx");
        assert!(!app.modified);
        let clean = App::new(new_xlsx(), "c.xlsx");
        let mut clean = clean;
        clean.apply_backstage_event(backstage::BackstageEvent::New);
        assert!(clean.confirm.is_none(), "nothing to lose");
        assert_eq!(clean.path, "untitled.xlsx");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Opened read-only: the title says so, and Ctrl+S never writes the file,
    /// it opens Save As with Excel's words.
    #[test]
    fn read_only_save_opens_save_as_and_writes_nothing() {
        let (dir, mut app) = two_books("ro-save");
        let a = dir.join("a.xlsx");
        let before = std::fs::read(&a).unwrap();
        app.set_read_only(a.to_str().unwrap());
        assert!(app.bound_read_only());
        assert_eq!(
            window_title("xlsxy", &app.path, false, app.bound_read_only()),
            "xlsxy - a.xlsx [Read-Only]"
        );
        type_into_a1(&mut app, 9.0);
        app.save();
        let refusal = "\"a.xlsx\" is read-only. Save a copy under a new name.";
        assert_eq!(app.status.as_deref(), Some(refusal));
        assert!(
            matches!(
                app.prompt.as_ref().map(|p| &p.kind),
                Some(PromptKind::SaveAs)
            ),
            "Save opens Save As"
        );
        assert_eq!(std::fs::read(&a).unwrap(), before);
        assert!(app.modified);
        // The routes without a dialog refuse in the same words.
        app.prompt = None;
        assert_eq!(app.save_current(), Err(refusal.to_string()));
        assert!(!app.vim_run_command("wq"), ":wq does not quit");
        assert_eq!(std::fs::read(&a).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Save As to the read-only file is refused; to another name it writes,
    /// and the workbook is an ordinary one bound to that name.
    #[test]
    fn read_only_save_as_refuses_the_source_and_rebinds_elsewhere() {
        let (dir, mut app) = two_books("ro-save-as");
        let a = dir.join("a.xlsx");
        let before = std::fs::read(&a).unwrap();
        app.set_read_only(a.to_str().unwrap());
        type_into_a1(&mut app, 9.0);
        app.request_save_as(a.to_str().unwrap().to_string());
        assert_eq!(
            app.status.as_deref(),
            Some("\"a.xlsx\" is read-only. Save a copy under a new name.")
        );
        assert_eq!(std::fs::read(&a).unwrap(), before);
        assert!(app.path.ends_with("a.xlsx"), "still bound to the source");
        // The same file under another spelling is still the source.
        let roundabout = dir.join(".").join("a.xlsx");
        assert!(!app.save_as(roundabout.to_str().unwrap().to_string()));
        assert_eq!(std::fs::read(&a).unwrap(), before);

        let c = dir.join("c.xlsx");
        app.request_save_as(c.to_str().unwrap().to_string());
        assert_eq!(Path::new(&app.path), c);
        assert!(!app.modified);
        assert!(!app.bound_read_only());
        type_into_a1(&mut app, 10.0);
        app.save();
        assert!(app.prompt.is_none(), "an ordinary Save");
        assert!(!app.modified);
        let back = load_xlsx(&std::fs::read(&c).unwrap()).unwrap();
        assert_eq!(
            back.workbook.sheets[0].cell(0, 0).map(|c| c.value.clone()),
            Some(gridcore::sheet::CellValue::Number(10.0))
        );
        assert_eq!(std::fs::read(&a).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Reverting re-reads the same file and keeps it read-only; an
    /// interactive Open, even of the same file, is an ordinary open.
    #[test]
    fn read_only_survives_reload_and_ends_at_open() {
        let (dir, mut app) = two_books("ro-reload");
        let a = dir.join("a.xlsx");
        app.set_read_only(a.to_str().unwrap());
        type_into_a1(&mut app, 9.0);
        app.reload().unwrap();
        assert_eq!(a1(&app), 1.0);
        assert!(app.bound_read_only(), "reload keeps read-only");
        app.apply_backstage_event(backstage::BackstageEvent::Open(a.clone()));
        assert!(app.confirm.is_none());
        assert!(!app.bound_read_only(), "Open is an ordinary open");
        assert!(app.read_only.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An import is bound to a new `.xlsx`, never its source, so read-only
    /// changes nothing for it: no caption, and Save writes the new file.
    #[test]
    fn read_only_import_saves_its_new_binding() {
        let dir = macro_dir("ro-import");
        let csv = dir.join("data.csv");
        std::fs::write(&csv, "1,2\n").unwrap();
        let (pkg, path, source, _) =
            load_workbook(csv.to_str().unwrap(), &TextOpen::from_prefs()).unwrap();
        let mut app = App::new(pkg, &path);
        app.os_clip = None;
        app.import_source = source;
        app.set_read_only(csv.to_str().unwrap());
        assert!(!app.bound_read_only());
        assert!(app.save_current().is_ok());
        assert!(Path::new(&app.path).exists());
        assert_eq!(std::fs::read_to_string(&csv).unwrap(), "1,2\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// r2 M1: the control surface's `wb.open` and `wb.reload` keep the source
    /// read-only, whatever is opened in between.
    #[test]
    fn read_only_survives_the_control_surfaces_open() {
        use ctlcore::json::Json;
        let (dir, mut app) = two_books("ro-wb-open");
        let a = dir.join("a.xlsx");
        let b = dir.join("b.xlsx");
        let before = std::fs::read(&a).unwrap();
        let refusal = "\"a.xlsx\" is read-only. Save a copy under a new name.".to_string();
        app.set_read_only(a.to_str().unwrap());
        let open = |app: &mut App, p: &Path| {
            control::dispatch(
                app,
                "wb.open",
                &Json::obj(vec![("path", Json::Str(p.to_string_lossy().into()))]),
            )
            .unwrap();
        };
        open(&mut app, &a);
        assert_eq!(
            control::dispatch(&mut app, "wb.save", &Json::obj(vec![])),
            Err(refusal.clone())
        );
        control::dispatch(&mut app, "wb.reload", &Json::obj(vec![])).unwrap();
        assert_eq!(
            control::dispatch(&mut app, "wb.save", &Json::obj(vec![])),
            Err(refusal.clone())
        );
        open(&mut app, &b);
        assert!(
            control::dispatch(&mut app, "wb.save", &Json::obj(vec![])).is_ok(),
            "another file saves"
        );
        let read_only = |app: &mut App| {
            control::dispatch(app, "wb.path", &Json::obj(vec![]))
                .unwrap()
                .get("read_only")
                .and_then(Json::as_bool)
        };
        assert_eq!(read_only(&mut app), Some(false), "b.xlsx is not the source");
        open(&mut app, &a);
        assert_eq!(read_only(&mut app), Some(true));
        assert_eq!(
            control::dispatch(&mut app, "wb.save", &Json::obj(vec![])),
            Err(refusal)
        );
        assert_eq!(std::fs::read(&a).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// r2 m1: `--read-only` needs an input that exists.
    #[test]
    fn read_only_needs_an_existing_input() {
        let (dir, _app) = two_books("ro-missing");
        let parse = |args: &[&str]| {
            parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
        };
        let a = dir.join("a.xlsx").to_string_lossy().into_owned();
        let missing = dir.join("new.xlsx").to_string_lossy().into_owned();
        assert_eq!(read_only_input_error(&parse(&["-r", &a])), None);
        assert_eq!(
            read_only_input_error(&parse(&[&missing])),
            None,
            "not read-only"
        );
        assert_eq!(
            read_only_input_error(&parse(&["-r", &missing])),
            Some(format!("cannot open {missing} read-only: no such file"))
        );
        assert_eq!(
            read_only_input_error(&parse(&["--read-only"])),
            Some("--read-only needs a file to open".to_string())
        );
        let folder = dir.to_string_lossy().into_owned();
        assert!(read_only_input_error(&parse(&["-r", &folder])).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// r3 M1: `xlsxy -r notes.txt` imports through the wizard; finishing it
    /// keeps the text file refused, through every later Save As.
    #[test]
    fn read_only_text_import_keeps_its_source_refused() {
        let dir = macro_dir("ro-txt-import");
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "1\t2\n").unwrap();
        let before = std::fs::read(&txt).unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.os_clip = None;
        app.set_read_only(txt.to_str().unwrap());
        app.open_startup_wizard(txt.to_str().unwrap());
        app.finish_text_dialog();
        assert!(app.text_dialog.is_none());
        assert!(app.read_only.is_some());
        assert!(!app.startup_import, "the flag is spent");
        app.request_save_as(dir.join("other.xlsx").to_string_lossy().into_owned());
        assert!(dir.join("other.xlsx").is_file());
        app.request_save_as(txt.to_string_lossy().into_owned());
        assert_eq!(
            app.status.as_deref(),
            Some("\"notes.txt\" is read-only. Save a copy under a new name.")
        );
        assert_eq!(std::fs::read(&txt).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// r4: an interactive import ends read-only like any Open that loads,
    /// the read-only text file's own included; a cancelled startup wizard
    /// leaves no flag behind for a later import.
    #[test]
    fn interactive_import_ends_read_only() {
        let dir = macro_dir("ro-txt-reimport");
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "1\t2\n").unwrap();
        let mut app = App::new(new_xlsx(), "untitled.xlsx");
        app.os_clip = None;
        app.set_read_only(txt.to_str().unwrap());
        app.open_startup_wizard(txt.to_str().unwrap());
        assert!(app.startup_import);
        app.text_dialog_key(KeyCode::Esc);
        assert!(!app.startup_import, "cancel clears the flag");
        assert!(app.read_only.is_some(), "and keeps read-only");
        app.apply_backstage_event(backstage::BackstageEvent::Open(txt.clone()));
        assert!(app.text_dialog.is_some());
        app.finish_text_dialog();
        assert!(app.read_only.is_none(), "an interactive re-import ends it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// r3 M2: File > Export never writes the read-only CSV, even after a
    /// Save As has bound the workbook beside it.
    #[test]
    fn read_only_export_refuses_the_source() {
        let dir = macro_dir("ro-export");
        let csv = dir.join("data.csv");
        std::fs::write(&csv, "1,2\n").unwrap();
        let before = std::fs::read(&csv).unwrap();
        let (pkg, path, source, _) =
            load_workbook(csv.to_str().unwrap(), &TextOpen::from_prefs()).unwrap();
        let mut app = App::new(pkg, &path);
        app.os_clip = None;
        app.import_source = source;
        app.set_read_only(csv.to_str().unwrap());
        app.request_save_as(dir.join("data.xlsx").to_string_lossy().into_owned());
        assert_eq!(Path::new(&app.path), dir.join("data.xlsx"));
        assert!(app.import_source.is_none());
        app.export_csv();
        assert_eq!(
            app.status.as_deref(),
            Some("Export failed: \"data.csv\" is read-only. Save a copy under a new name.")
        );
        assert_eq!(std::fs::read(&csv).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// r3: the backup and a Web Page's supporting files are guarded too.
    #[test]
    fn read_only_guards_backups_and_supporting_files() {
        let (dir, mut app) = two_books("ro-helpers");
        let a = dir.join("a.xlsx");
        let before = std::fs::read(&a).unwrap();
        app.set_read_only(a.to_str().unwrap());
        let refused = |r: io::Result<()>, name: &str| {
            let e = r.expect_err("refused");
            assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(
                e.to_string(),
                format!("\"{name}\" is read-only. Save a copy under a new name.")
            );
        };
        refused(app.write_supporting(&a, b"x"), "a.xlsx");
        refused(app.write_export(None, &a, b"x"), "a.xlsx");
        assert_eq!(std::fs::read(&a).unwrap(), before);
        // A backup that would land on the read-only file is refused too.
        let held = dir.join("Backup of b.xlk");
        std::fs::write(&held, b"kept").unwrap();
        app.set_read_only(held.to_str().unwrap());
        refused(app.write_backup(&dir.join("b.xlsx")), "Backup of b.xlk");
        assert_eq!(std::fs::read(&held).unwrap(), b"kept");
        assert!(
            app.write_supporting(&dir.join("page_files").join("s.css"), b"x")
                .is_ok()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The refusal names the file, not its folder.
    #[test]
    fn read_only_refusal_names_the_file() {
        assert_eq!(
            read_only_refusal("some/dir/a.xlsx"),
            "\"a.xlsx\" is read-only. Save a copy under a new name."
        );
    }

    /// r1 M1: an Open that loads nothing leaves the read-only file guarded:
    /// a missing file, and a text file whose wizard is cancelled.
    #[test]
    fn read_only_survives_an_open_that_fails_or_is_cancelled() {
        let (dir, mut app) = two_books("ro-open-fails");
        let a = dir.join("a.xlsx");
        let before = std::fs::read(&a).unwrap();
        let refusal = Err("\"a.xlsx\" is read-only. Save a copy under a new name.".to_string());
        app.set_read_only(a.to_str().unwrap());

        // Nothing to lose, so the Open goes straight to the load, which fails.
        app.apply_backstage_event(backstage::BackstageEvent::Open(dir.join("missing.xlsx")));
        assert!(app.status.as_deref().unwrap().starts_with("Open failed"));
        assert!(app.path.ends_with("a.xlsx"));
        assert_eq!(app.save_current(), refusal);

        // Over unsaved changes, Yes and then a failed load.
        type_into_a1(&mut app, 9.0);
        app.apply_backstage_event(backstage::BackstageEvent::Open(dir.join("missing.xlsx")));
        assert!(!app.confirm_key(y()));
        assert_eq!(app.save_current(), refusal);

        // A text file opens its wizard; Esc cancels it and nothing loads.
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "1\t2\n").unwrap();
        app.modified = false;
        app.apply_backstage_event(backstage::BackstageEvent::Open(txt));
        assert!(app.text_dialog.is_some());
        app.text_dialog_key(KeyCode::Esc);
        assert!(app.text_dialog.is_none());
        assert!(app.bound_read_only());
        assert_eq!(app.save_current(), refusal);
        assert_eq!(std::fs::read(&a).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A finished import and New are other workbooks: read-only ends.
    #[test]
    fn read_only_ends_when_another_workbook_is_installed() {
        let (dir, mut app) = two_books("ro-installed");
        let a = dir.join("a.xlsx");
        app.set_read_only(a.to_str().unwrap());
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "1\t2\n").unwrap();
        app.apply_backstage_event(backstage::BackstageEvent::Open(txt));
        app.finish_text_dialog();
        assert!(app.text_dialog.is_none());
        assert!(app.read_only.is_none());
        app.set_read_only(a.to_str().unwrap());
        app.apply_backstage_event(backstage::BackstageEvent::New);
        assert!(app.read_only.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// #724: a new workbook with 1, 2, 3 in A1:A3 (and `extra` cells set
    /// as they would load), its engine built and recalculated.
    fn app_with_numbers(extra: &[((u32, u32), gridcore::sheet::Cell)]) -> App {
        use gridcore::sheet::Cell;
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        for r in 0..3 {
            app.pkg.workbook.sheets[0].set_cell(r, 0, Cell::number(r as f64 + 1.0));
        }
        for ((r, c), cell) in extra {
            app.pkg.workbook.sheets[0].set_cell(*r, *c, cell.clone());
        }
        app.rebuild_engine();
        app
    }

    fn saved_sheet1(app: &App) -> (SheetPackage, String) {
        let re = load_xlsx(&save_xlsx(&app.pkg)).unwrap();
        let ws = String::from_utf8_lossy(re.part("xl/worksheets/sheet1.xml").unwrap()).into_owned();
        (re, ws)
    }

    #[test]
    fn typed_spill_survives_rebuild_engine() {
        // #724: inserting a row rebuilds the engine; the typed dynamic array
        // still spills (it used to collapse to one value).
        use gridcore::sheet::Cell;
        let mut app = app_with_numbers(&[]);
        app.apply(vec![(0, 2, Cell::formula("SEQUENCE(3)"))]);
        app.cur = (0, 2);
        app.anchor = None;
        app.row_op(true);
        assert_eq!(app.sheet().cell(1, 2).unwrap().spill, Some((3, 1)));
        assert_eq!(
            app.sheet().cell(3, 2).unwrap().value,
            gridcore::sheet::CellValue::Number(3.0)
        );
    }

    #[test]
    fn typed_scalar_filter_spills_after_rebuild() {
        // A typed FILTER with no match yet is still a modern formula after a
        // rebuild: once rows match, it spills.
        use gridcore::sheet::Cell;
        let mut app = app_with_numbers(&[]);
        app.apply(vec![(0, 2, Cell::formula("FILTER(A1:A3,A1:A3>5)"))]);
        app.rebuild_engine();
        app.apply(vec![
            (0, 0, Cell::number(6.0)),
            (1, 0, Cell::number(7.0)),
            (2, 0, Cell::number(8.0)),
        ]);
        assert_eq!(app.sheet().cell(0, 2).unwrap().spill, Some((3, 1)));
    }

    #[test]
    fn typed_spill_survives_structural_undo() {
        use gridcore::sheet::Cell;
        let mut app = app_with_numbers(&[]);
        app.apply(vec![(0, 2, Cell::formula("SEQUENCE(3)"))]);
        app.cur = (0, 2);
        app.anchor = None;
        app.row_op(true);
        app.undo();
        assert_eq!(app.sheet().cell(0, 2).unwrap().spill, Some((3, 1)));
    }

    #[test]
    fn undo_restores_legacy_formula_unmarked() {
        // Undo is not typing: a loaded legacy formula typed over and undone
        // does not come back as a dynamic array.
        use gridcore::sheet::Cell;
        let mut app = app_with_numbers(&[((1, 1), Cell::formula("A1:A3*2"))]);
        app.apply(vec![(1, 1, Cell::number(5.0))]);
        app.undo();
        assert_eq!(app.sheet().cell(1, 1).unwrap().spill, None);
        let (re, ws) = saved_sheet1(&app);
        assert!(
            ws.contains(r#"<c r="B2"><f>A1:A3*2</f><v>2</v></c>"#),
            "{ws}"
        );
        assert!(!ws.contains("cm="), "{ws}");
        assert!(re.part("xl/metadata.xml").is_none());
    }

    #[test]
    fn redo_restores_typed_dynamic() {
        use gridcore::sheet::Cell;
        let mut app = app_with_numbers(&[]);
        app.apply(vec![(0, 2, Cell::formula("SEQUENCE(3)"))]);
        app.undo();
        app.redo();
        assert_eq!(app.sheet().cell(0, 2).unwrap().spill, Some((3, 1)));
        let (_, ws) = saved_sheet1(&app);
        assert!(
            ws.contains(r#"<c r="C1" cm="1"><f t="array" ref="C1:C3">_xlfn.SEQUENCE(3)</f>"#),
            "{ws}"
        );
    }

    #[test]
    fn undo_restores_cse_f_attrs() {
        use gridcore::sheet::Cell;
        let mut cse = Cell::formula("A1:A3*2");
        cse.f_attrs = Some(r#" t="array" ref="B1:B3""#.into());
        let mut app = app_with_numbers(&[((0, 1), cse)]);
        app.apply(vec![(0, 1, Cell::number(5.0))]);
        app.undo();
        let b1 = app.sheet().cell(0, 1).unwrap();
        assert_eq!(b1.f_attrs.as_deref(), Some(r#" t="array" ref="B1:B3""#));
        assert!(!b1.is_dynamic());
    }

    // ---- #672: Excel's editing options ----

    fn opts_app() -> App {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn press_mod(app: &mut App, code: KeyCode, m: KeyModifiers) {
        handle_key(app, KeyEvent::new(code, m));
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    fn value_at(app: &App, r: u32, c: u32) -> CellValue {
        app.sheet()
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn put(app: &mut App, r: u32, c: u32, text: &str) {
        let cell = entry_cell(&mut app.pkg.workbook, app.sheet, r, c, text, None).unwrap();
        app.pkg.workbook.sheets[app.sheet].set_cell(r, c, cell);
        app.rebuild_engine();
    }

    #[test]
    fn enter_moves_the_way_the_options_say() {
        for (dir, want) in [
            (EnterMove::Down, (3, 2)),
            (EnterMove::Right, (2, 3)),
            (EnterMove::Up, (1, 2)),
            (EnterMove::Left, (2, 1)),
        ] {
            let mut app = opts_app();
            app.edit_opts.enter_move = dir;
            app.cur = (2, 2);
            type_text(&mut app, "x");
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.cur, want, "{dir:?} after a commit");
            assert_eq!(value_at(&app, 2, 2), CellValue::Text("x".into()));
            // Not editing: Enter moves the same way; Shift+Enter the other.
            app.cur = (2, 2);
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.cur, want, "{dir:?} on the grid");
            press_mod(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
            assert_eq!(app.cur, (2, 2), "{dir:?} Shift+Enter back");
        }
        let mut app = opts_app();
        app.edit_opts.move_after_enter = false;
        app.cur = (2, 2);
        type_text(&mut app, "y");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            (app.cur, app.edit.is_none()),
            ((2, 2), true),
            "commits and stays"
        );
        press_mod(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(app.cur, (2, 2));
        // Tab is not an Enter option.
        type_text(&mut app, "z");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.cur, (2, 3));
    }

    #[test]
    fn a_typed_number_takes_the_fixed_decimal_and_the_status_says_so() {
        use ratatui::backend::TestBackend;
        let mut app = opts_app();
        let screen = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
            term.draw(|f| draw(app, f)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..24)
                .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(!screen(&mut app).contains("Fixed Decimal"));
        app.edit_opts.fixed_decimal = true;
        app.edit_opts.places = 2;
        assert!(screen(&mut app).contains("Fixed Decimal"));
        assert_eq!(app.status_words(), ["Fixed Decimal"]);
        type_text(&mut app, "1234");
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 0, 0), CellValue::Number(12.34));
        type_text(&mut app, "1.5");
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 1, 0), CellValue::Number(1.5));
        app.edit_opts.places = -2;
        type_text(&mut app, "7");
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 2, 0), CellValue::Number(700.0));
        // An F2 edit that changes an integer is typed again, so it shifts
        // (Excel does the same); one left untouched is not re-read.
        app.edit_opts.places = 2;
        put(&mut app, 5, 0, "1234");
        app.cur = (5, 0);
        press(&mut app, KeyCode::F(2));
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 5, 0), CellValue::Number(1234.0));
        app.cur = (5, 0);
        press(&mut app, KeyCode::F(2));
        type_text(&mut app, "5");
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 5, 0), CellValue::Number(123.45));
    }

    #[test]
    fn autocomplete_proposes_and_any_commit_takes_it() {
        let mut app = opts_app();
        put(&mut app, 0, 0, "Apple");
        put(&mut app, 1, 0, "Banana");
        app.cur = (2, 0);
        type_text(&mut app, "AP");
        let e = app.edit.as_ref().unwrap();
        assert_eq!((e.text.as_str(), e.cursor), ("APple", 2));
        assert_eq!(e.proposal, Some((2, "Apple".to_string())));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            value_at(&app, 2, 0),
            CellValue::Text("Apple".into()),
            "the matched value's case"
        );
        // Backspace drops only the suffix; a typed char matches again.
        app.cur = (3, 0);
        type_text(&mut app, "b");
        assert_eq!(app.edit.as_ref().unwrap().text, "banana");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.edit.as_ref().unwrap().text, "b");
        assert_eq!(app.edit.as_ref().unwrap().proposal, None);
        type_text(&mut app, "a");
        assert_eq!(app.edit.as_ref().unwrap().text, "banana");
        type_text(&mut app, "x");
        assert_eq!(
            app.edit.as_ref().unwrap().text,
            "bax",
            "x replaced the suffix"
        );
        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Esc);
        // A click on another cell commits with the proposal taken.
        app.cur = (3, 0);
        type_text(&mut app, "ban");
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(&mut app, f)).unwrap();
        let (x, y) = (app.grid_area.x + app.gutter_w + 1, app.grid_area.y + 8);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::empty(),
            },
        );
        assert!(app.edit.is_none());
        assert_eq!(value_at(&app, 3, 0), CellValue::Text("Banana".into()));
        // Home in a typed (type-over) entry is a caret move too: the text
        // stays as shown and typing goes where the caret went.
        app.cur = (4, 0);
        type_text(&mut app, "ap");
        press(&mut app, KeyCode::Home);
        let e = app.edit.as_ref().unwrap();
        assert_eq!(
            (e.text.as_str(), e.cursor, &e.proposal),
            ("apple", 0, &None)
        );
        type_text(&mut app, "x");
        assert_eq!(app.edit.as_ref().unwrap().text, "xapple");
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 4, 0), CellValue::Text("xapple".into()));
        app.cur = (5, 1);
        put(&mut app, 4, 1, "Plum");
        type_text(&mut app, "p");
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            value_at(&app, 5, 1),
            CellValue::Text("plum".into()),
            "End kept the text as typed, not the value's case"
        );
        // A caret move (in an F2 editor) keeps the text, marker dropped.
        app.cur = (5, 0);
        press(&mut app, KeyCode::F(2));
        type_text(&mut app, "ap");
        assert_eq!(app.edit.as_ref().unwrap().text, "apple");
        press(&mut app, KeyCode::Home);
        assert_eq!(app.edit.as_ref().unwrap().proposal, None);
        press(&mut app, KeyCode::Enter);
        assert_eq!(value_at(&app, 5, 0), CellValue::Text("apple".into()));
        // Off: nothing is proposed.
        app.edit_opts.autocomplete = false;
        app.cur = (6, 0);
        type_text(&mut app, "ap");
        assert_eq!(app.edit.as_ref().unwrap().text, "ap");
    }

    #[test]
    fn a_double_click_edits_or_jumps_to_the_precedent() {
        let mut app = opts_app();
        put(&mut app, 0, 0, "=SUM(C3:D4)+B1");
        put(&mut app, 0, 5, "7");
        // Each pair of presses starts a second after the last one.
        let base = Instant::now();
        let mut n = 0u64;
        let mut pair = |app: &mut App, r: u32, c: u32, gap_ms: u64| {
            n += 1;
            let t = base + Duration::from_secs(n);
            app.last_click = None;
            app.click_cell(r, c, t);
            let one = app.edit.is_none();
            app.click_cell(r, c, t + Duration::from_millis(gap_ms));
            one
        };
        // On: a double-click edits, formula or not.
        assert!(pair(&mut app, 0, 0, 200), "one click only selects");
        assert_eq!(app.edit.as_ref().unwrap().text, "=SUM(C3:D4)+B1");
        app.cancel_edit();
        // Too slow: not a double-click.
        pair(&mut app, 0, 5, 900);
        assert!(app.edit.is_none());
        // Another cell: not a double-click.
        app.click_cell(0, 5, base);
        app.click_cell(0, 0, base + Duration::from_millis(100));
        assert!(app.edit.is_none());
        // Off: a formula jumps to its first precedent, a constant edits.
        app.edit_opts.edit_in_cell = false;
        pair(&mut app, 0, 0, 200);
        assert!(app.edit.is_none());
        assert_eq!((app.cur, app.anchor), ((2, 2), Some((3, 3))));
        pair(&mut app, 0, 5, 200);
        assert_eq!(app.edit.as_ref().unwrap().text, "7");
        app.cancel_edit();
        // A key between the presses breaks the pair.
        let t = base + Duration::from_secs(100);
        app.click_cell(0, 5, t);
        press(&mut app, KeyCode::Right);
        app.click_cell(0, 5, t + Duration::from_millis(100));
        assert!(app.edit.is_none());
        // A hyperlink is followed and starts no double-click.
        app.pkg.workbook.sheets[0]
            .hyperlinks
            .insert((7, 7), "#Sheet1!H8".into());
        pair(&mut app, 7, 7, 100);
        assert!(app.edit.is_none());
    }

    #[test]
    fn a_precedent_on_another_sheet_switches_to_it() {
        let mut app = opts_app();
        app.pkg.workbook.sheets.push(gridcore::sheet::Sheet {
            name: "Data".into(),
            ..Default::default()
        });
        put(&mut app, 0, 0, "=Data!B2*2");
        put(&mut app, 0, 1, "=1+2");
        app.edit_opts.edit_in_cell = false;
        app.cur = (0, 1);
        app.cell_double_click(0, 1);
        assert_eq!(
            (app.sheet, app.cur),
            (0, (0, 1)),
            "no precedent: nothing moves"
        );
        assert!(app.status.as_deref().unwrap().contains("No precedent"));
        app.pkg.workbook.sheets[1].hidden = true;
        app.cur = (0, 0);
        app.cell_double_click(0, 0);
        assert_eq!(app.sheet, 0, "a hidden sheet is not shown");
        app.pkg.workbook.sheets[1].hidden = false;
        app.cell_double_click(0, 0);
        assert_eq!((app.sheet, app.cur, app.anchor), (1, (1, 1), None));
    }

    #[test]
    fn ctrl_shift_u_expands_the_formula_bar_even_mid_edit() {
        use ratatui::backend::TestBackend;
        let mut app = opts_app();
        let grid_h = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
            term.draw(|f| draw(app, f)).unwrap();
            app.grid_area.height
        };
        let collapsed = grid_h(&mut app);
        type_text(&mut app, "abc");
        // Legacy terminals send Ctrl+U for Ctrl+Shift+U: both toggle.
        press_mod(
            &mut app,
            KeyCode::Char('U'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(app.fx_expanded);
        let e = app.edit.as_ref().unwrap();
        assert_eq!((e.text.as_str(), e.cursor), ("abc", 3), "the edit goes on");
        assert_eq!(grid_h(&mut app), collapsed - 3);
        press_mod(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(!app.fx_expanded);
        assert_eq!(grid_h(&mut app), collapsed);
        assert_eq!(app.edit.as_ref().unwrap().text, "abc");
    }

    #[test]
    fn vim_plain_u_still_undoes() {
        let mut app = opts_app();
        type_text(&mut app, "5");
        press(&mut app, KeyCode::Enter);
        app.vim = Some(VimState {
            mode: VimMode::Normal,
            pending: '\0',
            cmdline: None,
        });
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(value_at(&app, 0, 0), CellValue::Empty);
        press_mod(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(app.fx_expanded, "Ctrl+U is the formula bar's, even in vim");
    }

    #[test]
    fn the_editing_options_are_set_in_file_options_and_persist() {
        let mut app = opts_app();
        app.open_backstage();
        let rows: Vec<String> = app
            .backstage
            .as_ref()
            .unwrap()
            .options
            .iter()
            .map(|o| o.key.clone())
            .collect();
        assert!(rows.contains(&"edit_fixed_decimal".to_string()));
        assert!(
            !rows.contains(&"edit_fill_handle".to_string()),
            "xlsxy has no fill handle"
        );
        {
            let bs = app.backstage.as_mut().unwrap();
            bs.item = backstage::Item::Options;
            bs.pane = backstage::Pane::Options;
            bs.option_sel = rows.iter().position(|k| k == "edit_fixed_decimal").unwrap();
        }
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        app.backstage_key(key(KeyCode::Char(' ')));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Right));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Left));
        app.backstage_key(key(KeyCode::Down));
        app.backstage_key(key(KeyCode::Enter));
        let want = EditOptions {
            fixed_decimal: true,
            places: 3,
            enter_move: EnterMove::Left,
            edit_in_cell: false,
            ..EditOptions::default()
        };
        assert_eq!(app.edit_opts, want);
        // Saved with the other preferences and read back on the next start.
        let text = app.view_prefs_text();
        assert!(text.contains("convert_dates=1"), "{text}");
        let mut again = opts_app();
        again.apply_view_prefs(&text);
        assert_eq!(again.edit_opts, want);
        // And the reopened page shows them.
        again.open_backstage();
        let bs = again.backstage.as_ref().unwrap();
        assert_eq!(bs.option_int("edit_fixed_decimal_places"), Some(3));
        assert_eq!(bs.option_choice("edit_move_direction"), Some(3));
        assert_eq!(bs.option_check("edit_in_cell"), Some(false));
    }
}

#[cfg(test)]
mod table_command_tests {
    use super::*;
    use gridcore::edit::parse_input;
    use gridcore::sheet::Cell;

    /// Item/Qty over A1:B3 as `Table1`, `=SUM(Table1[Qty])` in D1, the
    /// cursor inside the table.
    fn app_with_table() -> App {
        let mut app = App::new(new_xlsx(), "t.xlsx");
        app.os_clip = None;
        {
            let sh = &mut app.pkg.workbook.sheets[0];
            sh.set_cell(0, 0, Cell::text("Item"));
            sh.set_cell(0, 1, Cell::text("Qty"));
            sh.set_cell(1, 0, Cell::text("Pen"));
            sh.set_cell(1, 1, Cell::number(3.0));
            sh.set_cell(2, 0, Cell::text("Pad"));
            sh.set_cell(2, 1, Cell::number(5.0));
        }
        app.rebuild_engine();
        app.cur = (1, 0);
        app.anchor = None;
        app.format_as_table();
        assert_eq!(app.pkg.workbook.tables[0].name, "Table1");
        app.apply_on(0, vec![(0, 3, parse_input("=SUM(Table1[Qty])"))]);
        app.cur = (1, 0);
        app.status = None;
        app
    }

    fn formula(app: &App, r: u32, c: u32) -> Option<String> {
        app.sheet().cell(r, c).and_then(|cl| cl.formula.clone())
    }

    fn value(app: &App, r: u32, c: u32) -> CellValue {
        app.sheet()
            .cell(r, c)
            .map(|cl| cl.value.clone())
            .unwrap_or_default()
    }

    fn type_prompt(app: &mut App, text: &str) {
        app.prompt.as_mut().expect("a prompt").text = text.to_string();
        app.commit_prompt();
    }

    #[test]
    fn table_commands_need_a_cell_in_a_table() {
        let mut app = app_with_table();
        app.cur = (5, 5);
        for act in [
            ribbon::Act::TableName,
            ribbon::Act::ResizeTable,
            ribbon::Act::ConvertToRange,
        ] {
            app.status = None;
            app.ribbon_act(act);
            assert_eq!(app.status.as_deref(), Some("Select a cell in a table"));
            assert!(app.prompt.is_none());
        }
        assert_eq!(app.pkg.workbook.tables.len(), 1);
    }

    #[test]
    fn table_name_renames_the_table_and_its_users_in_one_undo_step() {
        let mut app = app_with_table();
        app.model_measures.push(gridcore::model::Measure {
            name: "Total".into(),
            formula: "SUM(Table1[Qty])".into(),
        });
        app.model_rels.push(Relationship {
            from: ("Table1".into(), "Item".into()),
            to: ("Items".into(), "Item".into()),
        });
        app.ribbon_act(ribbon::Act::TableName);
        assert_eq!(app.prompt.as_ref().unwrap().text, "Table1");
        type_prompt(&mut app, "Sales");
        assert_eq!(app.status.as_deref(), Some("Renamed table Table1 to Sales"));
        assert_eq!(app.pkg.workbook.tables[0].name, "Sales");
        assert_eq!(formula(&app, 0, 3).as_deref(), Some("SUM(Sales[Qty])"));
        assert_eq!(value(&app, 0, 3), CellValue::Number(8.0));
        assert_eq!(app.model_measures[0].formula, "SUM(Sales[Qty])");
        assert_eq!(app.model_rels[0].from.0, "Sales");

        app.undo();
        assert_eq!(app.pkg.workbook.tables[0].name, "Table1");
        assert_eq!(formula(&app, 0, 3).as_deref(), Some("SUM(Table1[Qty])"));
        assert_eq!(app.model_measures[0].formula, "SUM(Table1[Qty])");
        assert_eq!(app.model_rels[0].from.0, "Table1");
        app.redo();
        assert_eq!(app.pkg.workbook.tables[0].name, "Sales");
        assert_eq!(app.model_rels[0].from.0, "Sales");
    }

    #[test]
    fn undoing_a_rename_keeps_later_model_edits() {
        let mut app = app_with_table();
        app.model_rels.push(Relationship {
            from: ("Table1".into(), "Item".into()),
            to: ("Items".into(), "Item".into()),
        });
        app.rename_table("Table1", "Sales").unwrap();
        // Model edits are not undo steps: one added, one removed.
        app.model_measures.push(gridcore::model::Measure {
            name: "Total".into(),
            formula: "SUM(Sales[Qty])".into(),
        });
        app.model_rels.clear();
        app.undo();
        assert_eq!(app.pkg.workbook.tables[0].name, "Table1");
        assert_eq!(app.model_measures.len(), 1, "the later measure stays");
        assert_eq!(app.model_measures[0].formula, "SUM(Table1[Qty])");
        assert!(
            app.model_rels.is_empty(),
            "the removed relationship stays gone"
        );
        app.redo();
        assert_eq!(app.model_measures[0].formula, "SUM(Sales[Qty])");
    }

    #[test]
    fn undoing_a_rename_puts_the_pivot_source_back() {
        let mut app = app_with_table();
        let frame = gridcore::frame::Frame::from_range(&app.pkg.workbook, 0, (0, 0, 2, 1));
        let dest = app.pkg.add_sheet("Pivot");
        app.pkg
            .add_pivot(
                gridcore::pivot::PivotSource::Table("Table1".into()),
                frame.names.clone(),
                gridcore::pivot::DataField {
                    name: "Sum of Qty".into(),
                    field: 1,
                    agg: gridcore::frame::Agg::Sum,
                },
                dest,
                (2, 0),
            )
            .unwrap();
        app.rebuild_engine();
        app.rename_table("Table1", "Sales").unwrap();
        let source = |app: &App| app.pkg.workbook.pivots[0].source.clone();
        assert_eq!(
            source(&app),
            gridcore::pivot::PivotSource::Table("Sales".into())
        );
        app.undo();
        assert_eq!(
            source(&app),
            gridcore::pivot::PivotSource::Table("Table1".into())
        );
        let err = app.convert_table("Table1").unwrap_err();
        assert!(err.contains("uses this table"), "{err}");
    }

    #[test]
    fn table_name_shows_why_a_name_is_refused() {
        let mut app = app_with_table();
        let steps = app.undo.len();
        app.ribbon_act(ribbon::Act::TableName);
        type_prompt(&mut app, "B2");
        assert_eq!(
            app.status.as_deref(),
            Some("\"B2\" looks like a cell reference")
        );
        assert_eq!(app.pkg.workbook.tables[0].name, "Table1");
        assert_eq!(app.undo.len(), steps, "a refusal is no undo step");
    }

    #[test]
    fn resize_table_prefills_the_range_and_resizes() {
        let mut app = app_with_table();
        app.ribbon_act(ribbon::Act::ResizeTable);
        assert_eq!(app.prompt.as_ref().unwrap().text, "A1:B3");
        type_prompt(&mut app, "a1:c4");
        assert_eq!(app.status.as_deref(), Some("Resized Table1 to A1:C4"));
        let t = &app.pkg.workbook.tables[0];
        assert_eq!(t.range, (0, 0, 3, 2));
        assert_eq!(t.columns, vec!["Item", "Qty", "Column3"]);
        assert_eq!(value(&app, 0, 2), CellValue::Text("Column3".into()));
        app.undo();
        assert_eq!(app.pkg.workbook.tables[0].range, (0, 0, 2, 1));
        assert_eq!(app.sheet().cell(0, 2).map(|c| c.value.clone()), None);

        app.ribbon_act(ribbon::Act::ResizeTable);
        type_prompt(&mut app, "D10:E12");
        assert_eq!(
            app.status.as_deref(),
            Some("The header row must stay in row 1")
        );
        app.ribbon_act(ribbon::Act::ResizeTable);
        type_prompt(&mut app, "nonsense");
        assert_eq!(app.status.as_deref(), Some("\"nonsense\" isn't a range"));
        assert_eq!(app.pkg.workbook.tables[0].range, (0, 0, 2, 1));
    }

    #[test]
    fn convert_to_range_then_undo_saves_the_table_again() {
        let mut app = app_with_table();
        app.ribbon_act(ribbon::Act::ConvertToRange);
        assert_eq!(app.status.as_deref(), Some("Converted Table1 to a range"));
        assert!(app.pkg.workbook.tables.is_empty());
        assert_eq!(formula(&app, 0, 3).as_deref(), Some("SUM($B$2:$B$3)"));
        assert_eq!(value(&app, 0, 3), CellValue::Number(8.0));
        let saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&app.pkg)).unwrap();
        assert!(saved.workbook.tables.is_empty());

        app.undo();
        assert_eq!(app.pkg.workbook.tables.len(), 1);
        let re = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&app.pkg)).unwrap();
        assert_eq!(re.workbook.tables.len(), 1);
        assert_eq!(
            re.workbook.sheets[0].cell(0, 3).unwrap().formula.as_deref(),
            Some("SUM(Table1[Qty])")
        );
    }

    #[test]
    fn convert_to_range_is_refused_while_the_data_model_uses_the_table() {
        let mut app = app_with_table();
        app.model_measures.push(gridcore::model::Measure {
            name: "Total".into(),
            formula: "SUM(table1[Qty])".into(),
        });
        app.ribbon_act(ribbon::Act::ConvertToRange);
        assert_eq!(app.status.as_deref(), Some("The data model uses Table1"));
        assert_eq!(app.pkg.workbook.tables.len(), 1);
    }

    #[test]
    fn undo_of_a_row_insert_puts_the_table_back() {
        let mut app = app_with_table();
        app.cur = (0, 0);
        app.row_op(true);
        assert_eq!(app.pkg.workbook.tables[0].range, (1, 0, 3, 1));
        app.undo();
        assert_eq!(app.pkg.workbook.tables[0].range, (0, 0, 2, 1));
    }
}
