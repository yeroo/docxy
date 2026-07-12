//! `xlsxy` — terminal viewer/**editor** for `.xlsx` workbooks.
//!
//! Usage:
//!   xlsxy                               open a new blank workbook
//!   xlsxy <file.xlsx>                   open in the editor
//!   xlsxy <in.xlsx> --recalc <out>      headless: recalculate and save
//!   xlsxy <in.xlsx> --csv <out.csv>     headless: export the first sheet as CSV
//!
//! The engine lives in the pure `gridcore` crate; this binary is the TUI
//! shell: a cell grid with Excel muscle memory (formula bar, A1 navigation,
//! range selection, ref-translating copy/paste) and a dependency-graph
//! recalculation on every edit.

use std::io;
use std::process::ExitCode;
use std::time::SystemTime;

mod backstage;
mod ribbon;

use gridcore::comments::Comment;
use gridcore::engine::Engine;
use gridcore::formula::translate_formula;
use gridcore::frame::Agg;
use gridcore::model::{
    DataModel, MODEL_PART, ModelSpec, Relationship, model_part_xml, model_pivot, parse_model_part,
};
use gridcore::sheet::{
    Align, Cell, CellValue, MAX_COLS, MAX_ROWS, NumFmt, Sheet, Xf, cell_name, col_name,
    format_with, sheet_to_csv,
};
use gridcore::xlsx::{SheetPackage, load_xlsx, new_xlsx, save_xlsx};

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
use ratatui::widgets::{Clear, Paragraph};
use ratatui::{Frame, Terminal};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
            let pkg = match load_xlsx(&data) {
                Ok(p) => p,
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

    let (pkg, path) = match parsed.inputs.first() {
        // CSV/TSV imports as a one-sheet workbook (Ctrl-S then writes
        // .xlsx — the path is rebound so a spreadsheet never lands in a
        // text file). The delimiter is sniffed.
        Some(input)
            if input.to_ascii_lowercase().ends_with(".csv")
                || input.to_ascii_lowercase().ends_with(".tsv") =>
        {
            match std::fs::read_to_string(input) {
                Ok(text) => {
                    let stem = std::path::Path::new(input)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "import".to_string());
                    let base = &input[..input.len() - 4];
                    (csv_to_pkg(&text, &stem), format!("{base}.xlsx"))
                }
                Err(e) => {
                    eprintln!("error: cannot read {input}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some(input) => match std::fs::read(input) {
            Ok(data) => match load_xlsx(&data) {
                Ok(pkg) => (pkg, input.clone()),
                Err(e) => {
                    eprintln!("error: {input}: {e}");
                    return ExitCode::FAILURE;
                }
            },
            // A nonexistent .xlsx path opens a new workbook bound to it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => (new_xlsx(), input.clone()),
            Err(e) => {
                eprintln!("error: cannot read {input}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            if parsed.recalc_out.is_some() || parsed.csv_out.is_some() {
                eprintln!("error: headless modes (--recalc/--csv) require an input file");
                return ExitCode::from(2);
            }
            (new_xlsx(), "untitled.xlsx".to_string())
        }
    };

    if let Some(out) = parsed.recalc_out {
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
        let bytes = save_xlsx(&pkg);
        if let Err(e) = std::fs::write(&out, &bytes) {
            eprintln!("error: cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out} ({} bytes)", bytes.len());
        return ExitCode::SUCCESS;
    }

    if let Some(out) = parsed.csv_out {
        let sheet = &pkg.workbook.sheets[0];
        let csv = sheet_to_csv(sheet, &pkg.workbook.styles, pkg.workbook.date1904);
        if let Err(e) = std::fs::write(&out, csv.as_bytes()) {
            eprintln!("error: cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out} ({} bytes)", csv.len());
        return ExitCode::SUCCESS;
    }

    let welcome = parsed.inputs.is_empty();
    match run_tui(pkg, &path, welcome, parsed.vim) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Load a workbook from disk (`.xlsx`, or `.csv`/`.tsv` imported as one sheet),
/// returning the package and the path it should save back to.
fn load_workbook(path: &str) -> Result<(SheetPackage, String), String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".csv") || lower.ends_with(".tsv") {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let stem = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "import".to_string());
        let base = &path[..path.len() - 4];
        Ok((csv_to_pkg(&text, &stem), format!("{base}.xlsx")))
    } else {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let pkg = load_xlsx(&data).map_err(|e| e.to_string())?;
        Ok((pkg, path.to_string()))
    }
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

/// Import CSV text as a fresh one-sheet workbook.
fn csv_to_pkg(text: &str, sheet_name: &str) -> SheetPackage {
    let frame = gridcore::frame::Frame::from_csv(text);
    let mut pkg = new_xlsx();
    let sh = &mut pkg.workbook.sheets[0];
    if !sheet_name.is_empty() {
        sh.name = sheet_name.chars().take(31).collect();
    }
    for (c, name) in frame.names.iter().enumerate() {
        sh.set_cell(0, c as u32, Cell::text(name));
    }
    for (c, col) in frame.cols.iter().enumerate() {
        for (r, v) in col.iter().enumerate() {
            let value = match v {
                gridcore::formula::Value::Empty => continue,
                gridcore::formula::Value::Num(n) => CellValue::Number(*n),
                gridcore::formula::Value::Str(s) => CellValue::Text(s.clone()),
                gridcore::formula::Value::Bool(b) => CellValue::Bool(*b),
                gridcore::formula::Value::Err(e) => CellValue::Error(e.code().to_string()),
            };
            sh.set_cell(
                r as u32 + 1,
                c as u32,
                Cell {
                    value,
                    ..Cell::default()
                },
            );
        }
    }
    pkg
}

struct Parsed {
    inputs: Vec<String>,
    recalc_out: Option<String>,
    csv_out: Option<String>,
    verify: bool,
    help: bool,
    vim: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed {
        inputs: Vec::new(),
        recalc_out: None,
        csv_out: None,
        verify: false,
        help: false,
        vim: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => p.help = true,
            "--vim" => p.vim = true,
            "--verify" => p.verify = true,
            "--recalc" => {
                i += 1;
                p.recalc_out = Some(args.get(i).ok_or("--recalc needs an output path")?.clone());
            }
            "--csv" => {
                i += 1;
                p.csv_out = Some(args.get(i).ok_or("--csv needs an output path")?.clone());
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
        + usize::from(p.verify);
    if modes > 1 {
        return Err("choose only one of --recalc, --csv, --verify".to_string());
    }
    Ok(p)
}

fn print_usage() {
    eprintln!(
        "Xlsxy — terminal .xlsx spreadsheet editor with a real calc engine\n\n\
         USAGE:\n  \
           xlsxy                            new blank workbook\n  \
           xlsxy <file.xlsx>                open a workbook\n  \
           xlsxy <file.csv|.tsv>            import CSV/TSV as a new workbook\n  \
           xlsxy <in> --recalc <out.xlsx>   recalculate all formulas, save, exit\n  \
           xlsxy <in> --csv <out.csv>       export the first sheet as CSV, exit\n  \
           xlsxy <in> --verify              conformance scoreboard: recalculate\n  \
                                            and diff against Excel's cached values\n  \
           xlsxy <file> --vim               modal (vim) navigation: hjkl, v, dd, :w :q\n\n\
         EDITOR KEYS:\n  \
           type to replace · F2 edit in place · = starts a formula\n  \
           Enter/Tab commit (move down/right) · Esc cancel · Del clear\n  \
           arrows / PgUp / PgDn move   (Ctrl-arrows jump to data edge)\n  \
           Shift + move select a range   (stats appear in the status bar)\n  \
           Ctrl-C copy   Ctrl-X cut   Ctrl-V paste (relative refs translate)\n  \
           Ctrl-Z undo   Ctrl-Y redo   Ctrl-S save   Ctrl-Q quit\n  \
           Ctrl-PgUp/PgDn or click tabs to switch sheets\n  \
           Ctrl-D/Ctrl-R fill down/right   Ctrl-F find (F3 next)\n  \
           F5 insert rows  Shift-F5 delete rows  F6/Shift-F6 same for columns\n  \
           Ctrl-T add sheet  Shift-F2 rename sheet  Shift-Del delete sheet\n  \
           F12 Save As   F7 / F8 shrink / widen the current column\n  \
           mouse: click to move · drag to select · wheel to scroll"
    );
}

/// Current time as an Excel serial (UTC — std has no timezone database).
fn now_serial() -> Option<f64> {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some(secs / 86_400.0 + 25_569.0)
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
    let serial = now_serial().unwrap_or(1.0);
    match gridcore::sheet::serial_to_parts(serial, false) {
        Some(p) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            p.year, p.month, p.day, p.hour, p.minute, p.second
        ),
        None => "1899-12-31T00:00:00Z".to_string(),
    }
}

/// Welcome-screen menu items (index maps to `start_activate`).
const START_ITEMS: [(&str, &str); 3] = [
    ("New workbook", "Start a fresh blank .xlsx"),
    ("Open…", "Browse for a workbook or CSV"),
    ("Quit", "Exit xlsxy"),
];

/// Render the first sheet of a workbook (or a CSV) as preview text lines,
/// bounded so a huge file can't stall the browser.
fn preview_lines(path: &str, width: usize) -> Vec<String> {
    let (pkg, _) = match load_workbook(path) {
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
/// file has unsaved changes).
fn window_title(app: &str, path: &str, modified: bool) -> String {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    format!("{}{app} - {name}", if modified { "* " } else { "" })
}

/// Path of the persisted view-preferences file (XDG / APPDATA).
fn view_prefs_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))?;
    Some(dir.join("xlsxy").join("view.conf"))
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

/// The text a cell would show in the formula bar (`=formula`, or the value as
/// re-entered) — the surface Find/Replace operates on.
fn input_text_of(cell: &Cell) -> String {
    if let Some(f) = &cell.formula {
        format!("={f}")
    } else {
        match &cell.value {
            CellValue::Empty => String::new(),
            CellValue::Number(n) => gridcore::sheet::fmt_general(*n),
            CellValue::Text(s) => s.clone(),
            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellValue::Error(e) => e.clone(),
        }
    }
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
}

/// One undoable action: cell states before/after, per address.
struct UndoGroup {
    sheet: usize,
    changes: Vec<(u32, u32, Option<Cell>, Option<Cell>)>,
}

/// Sheets + defined names — the whole calculated state, snapshotted around
/// structural edits (row/column insert-delete, sheet rename) whose inverse
/// is not expressible as per-cell changes.
#[derive(Clone)]
struct WbSnapshot {
    sheets: Vec<gridcore::sheet::Sheet>,
    names: Vec<gridcore::sheet::DefinedName>,
}

enum UndoAction {
    Cells(UndoGroup),
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

/// An internal clipboard: a rect of cells plus its source corner so pasted
/// formulas can shift their relative references (Excel semantics).
#[derive(Clone)]
struct ClipData {
    cells: Vec<Vec<Option<Cell>>>,
    from: (u32, u32),
    cut: bool,
}

struct App {
    pkg: SheetPackage,
    engine: Engine,
    path: String,
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
    confirm_quit: bool,
    confirm_delete_sheet: bool,
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
    comment_sel: usize,
    // The File backstage (folder browser / preview / info / save-as).
    backstage: Option<backstage::Backstage>,
    // The welcome screen shown when launched with no file.
    start_screen: bool,
    start_sel: usize,
    // The formatting popup (number format / font & fill color).
    format_picker: Option<FormatPicker>,
    // View preferences (persisted to a config file).
    formula_view: bool,
    light_theme: bool,
    auto_hide_ribbon: bool,
    /// The pending Find text while the Replace-with prompt is open.
    replace_find: Option<String>,
    /// Vim-mode state (Some when launched with `--vim`).
    vim: Option<VimState>,
    /// The sheet-picker popup: the highlighted sheet index while open.
    sheet_picker: Option<usize>,
    // Geometry captured during draw, for mouse hit-testing.
    grid_area: Rect,
    gutter_w: u16,
    vis_cols: Vec<(u32, u16, u16)>, // (col, x, width)
    vis_rows: Vec<u32>,             // sheet row per screen line (freeze-aware)
    tab_spans: Vec<(usize, u16, u16)>,
    ribbon_rows: u16,
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
        App {
            pkg,
            engine,
            path: path.to_string(),
            sheet: 0,
            cur: (0, 0),
            anchor: None,
            top: 0,
            left: 0,
            edit: None,
            modified: false,
            status: None,
            undo: Vec::new(),
            redo: Vec::new(),
            clip: None,
            os_clip: arboard::Clipboard::new().ok(),
            clip_text: None,
            confirm_quit: false,
            confirm_delete_sheet: false,
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
            comment_sel: 0,
            backstage: None,
            start_screen: false,
            start_sel: 0,
            format_picker: None,
            formula_view: false,
            light_theme: false,
            auto_hide_ribbon: false,
            replace_find: None,
            vim: None,
            sheet_picker: None,
            grid_area: Rect::default(),
            gutter_w: 4,
            vis_cols: Vec::new(),
            vis_rows: Vec::new(),
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

    fn start_edit(&mut self, initial: Option<char>) {
        let text = match initial {
            Some(ch) => ch.to_string(),
            None => self.current_input_text(),
        };
        let cursor = text.chars().count();
        self.edit = Some(EditState {
            text,
            cursor,
            replace: initial.is_some(),
        });
        self.anchor = None;
    }

    /// What editing an existing cell starts from: the formula with `=`, or
    /// the value as it would be re-entered.
    fn current_input_text(&self) -> String {
        let (r, c) = self.cur;
        self.sheet()
            .cell(r, c)
            .map(input_text_of)
            .unwrap_or_default()
    }

    /// Commit the editor text into the current cell. Returns false (and
    /// stays in edit mode) when a formula doesn't parse.
    fn commit_edit(&mut self) -> bool {
        let Some(edit) = self.edit.take() else {
            return true;
        };
        let text = edit.text;
        if let Some(body) = text.strip_prefix('=') {
            if !body.is_empty() {
                if let Err(e) = Engine::validate(body) {
                    self.status = Some(format!("formula error: {e}"));
                    self.edit = Some(EditState {
                        cursor: text.chars().count(),
                        text,
                        replace: false,
                    });
                    return false;
                }
            }
        }
        let (r, c) = self.cur;
        let style = self.sheet().cell(r, c).map(|x| x.style).unwrap_or(0);
        let mut cell = parse_input(&text);
        cell.style = style;
        self.apply(vec![(r, c, cell)]);
        true
    }

    fn cancel_edit(&mut self) {
        self.edit = None;
    }

    /// Apply cell changes as one undo group, through the engine.
    fn apply(&mut self, changes: Vec<(u32, u32, Cell)>) {
        if changes.is_empty() {
            return;
        }
        self.engine.clock = now_serial();
        let sheet_idx = self.sheet;
        let mut group = UndoGroup {
            sheet: sheet_idx,
            changes: Vec::with_capacity(changes.len()),
        };
        for (r, c, cell) in changes {
            let before = self.sheet().cell(r, c).cloned();
            self.engine
                .set_cell(&mut self.pkg.workbook, (sheet_idx, r, c), cell.clone());
            let after = self.pkg.workbook.sheets[sheet_idx].cell(r, c).cloned();
            group.changes.push((r, c, before, after));
        }
        self.undo.push(UndoAction::Cells(group));
        self.redo.clear();
        self.modified = true;
    }

    /// Snapshot-run-snapshot for structural edits (row/col ops, renames):
    /// the inverse isn't per-cell, so undo restores the whole grid state.
    fn structural(&mut self, op: impl FnOnce(&mut gridcore::sheet::Workbook)) {
        let before = WbSnapshot {
            sheets: self.pkg.workbook.sheets.clone(),
            names: self.pkg.workbook.defined_names.clone(),
        };
        op(&mut self.pkg.workbook);
        self.rebuild_engine();
        let after = WbSnapshot {
            sheets: self.pkg.workbook.sheets.clone(),
            names: self.pkg.workbook.defined_names.clone(),
        };
        self.undo.push(UndoAction::Structural { before, after });
        self.redo.clear();
        self.modified = true;
        self.clamp_cursor();
    }

    fn restore(&mut self, snap: &WbSnapshot) {
        self.pkg.workbook.sheets = snap.sheets.clone();
        self.pkg.workbook.defined_names = snap.names.clone();
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
            Some(UndoAction::Cells(group)) => {
                self.sheet = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
                for &(r, c, ref before, _) in group.changes.iter().rev() {
                    let cell = before.clone().unwrap_or_default();
                    self.engine
                        .set_cell(&mut self.pkg.workbook, (group.sheet, r, c), cell);
                }
                if let Some(&(r, c, _, _)) = group.changes.first() {
                    self.cur = (r, c);
                    self.ensure_visible();
                }
                self.redo.push(UndoAction::Cells(group));
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

    fn redo(&mut self) {
        match self.redo.pop() {
            Some(UndoAction::Cells(group)) => {
                self.sheet = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
                for &(r, c, _, ref after) in group.changes.iter() {
                    let cell = after.clone().unwrap_or_default();
                    self.engine
                        .set_cell(&mut self.pkg.workbook, (group.sheet, r, c), cell);
                }
                if let Some(&(r, c, _, _)) = group.changes.first() {
                    self.cur = (r, c);
                    self.ensure_visible();
                }
                self.undo.push(UndoAction::Cells(group));
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
                    tsv.push_str(&format_with(
                        &self.pkg.workbook.styles.xf(cl.style),
                        &cl.value,
                        self.pkg.workbook.date1904,
                    ));
                }
                row.push(cell);
            }
            tsv.push('\n');
            rows.push(row);
        }
        self.clip = Some(ClipData {
            cells: rows,
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
                let mut changes = Vec::new();
                if clip.cut {
                    // A cut clears its source (once).
                    let (fr, fc) = clip.from;
                    for (dr, row) in clip.cells.iter().enumerate() {
                        for (dc, cell) in row.iter().enumerate() {
                            if cell.is_some() {
                                changes.push((fr + dr as u32, fc + dc as u32, Cell::default()));
                            }
                        }
                    }
                    self.clip = Some(ClipData {
                        cut: false,
                        ..clip.clone()
                    });
                }
                let (dr_all, dc_all) = (
                    r0 as i64 - clip.from.0 as i64,
                    c0 as i64 - clip.from.1 as i64,
                );
                for (dr, row) in clip.cells.iter().enumerate() {
                    for (dc, cell) in row.iter().enumerate() {
                        let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                        if r >= MAX_ROWS || c >= MAX_COLS {
                            continue;
                        }
                        let mut new_cell = cell.clone().unwrap_or_default();
                        if !clip.cut {
                            // Copies translate relative refs; cuts keep them.
                            if let Some(f) = &new_cell.formula {
                                if let Some(t) = translate_formula(f, dr_all, dc_all) {
                                    new_cell.formula = Some(t);
                                }
                            }
                        }
                        // Overwrite position wins over source-clear on overlap.
                        changes.retain(|&(cr, cc, _)| (cr, cc) != (r, c));
                        changes.push((r, c, new_cell));
                    }
                }
                self.apply(changes);
                self.status = Some("Pasted".to_string());
                return;
            }
        }
        if let Some(text) = os_text {
            // External TSV/plain text. Cap the paste so a hostile/huge
            // clipboard can't lock the UI in per-cell recalcs.
            const MAX_PASTE_CELLS: usize = 100_000;
            let mut changes = Vec::new();
            let mut truncated = false;
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
                    let mut cell = parse_input(field);
                    // A pasted `=…` that doesn't parse would freeze as an
                    // unsupported cell; demote it to literal text instead
                    // (entry-time editing rejects such input outright).
                    if let Some(f) = &cell.formula {
                        if Engine::validate(f).is_err() {
                            cell = Cell {
                                value: CellValue::Text(field.to_string()),
                                style,
                                ..Cell::default()
                            };
                        }
                    }
                    cell.style = style;
                    changes.push((r, c, cell));
                }
            }
            self.apply(changes);
            self.status = Some(if truncated {
                format!("Pasted (clipped to {MAX_PASTE_CELLS} cells)")
            } else {
                "Pasted".to_string()
            });
        }
    }

    // --- movement ------------------------------------------------------------

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
        self.cur = (nr, nc);
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
        self.cur = (r, c);
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
        self.pkg.remove_part(MODEL_PART);
        if !self.model_rels.is_empty() || !self.model_measures.is_empty() {
            let xml = model_part_xml(&self.model_rels, &self.model_measures);
            self.pkg.set_part(MODEL_PART, xml.into_bytes());
        }
        save_xlsx(&self.pkg)
    }

    fn save(&mut self) {
        let bytes = self.package_bytes();
        match std::fs::write(&self.path, &bytes) {
            Ok(()) => {
                self.modified = false;
                self.status = Some(format!("Saved {} ({} bytes)", self.path, bytes.len()));
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
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
        self.pkg
            .add_threaded_comment(self.sheet, r, c, &author, text, &iso_now());
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
        self.pkg.set_comment(self.sheet, r, c, &author, text);
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
        let snapshot: Vec<(u32, u32, Option<Cell>)> = {
            let sheet = self.sheet();
            let mut v = Vec::new();
            for r in r1..=r2 {
                for c in c1..=c2 {
                    v.push((r, c, sheet.cell(r, c).cloned()));
                }
            }
            v
        };
        let mut changes = Vec::new();
        for (r, c, existing) in snapshot {
            let cur = existing.as_ref().map(|cl| cl.style).unwrap_or(0);
            let mut xf = self.pkg.workbook.styles.xf(cur);
            f(&mut xf);
            let idx = self.pkg.workbook.styles.intern(xf);
            // Preserve value/formula/spill; change only the style.
            let mut cell = existing.unwrap_or_default();
            cell.style = idx;
            changes.push((r, c, cell));
        }
        self.apply(changes);
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
                self.apply_format(move |x| {
                    x.code = code.clone();
                    x.numfmt = code
                        .as_deref()
                        .map(gridcore::sheet::classify_format_code)
                        .unwrap_or(NumFmt::General);
                });
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
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let on = v.trim() == "1";
                match k.trim() {
                    "formula_view" => self.formula_view = on,
                    "light_theme" => self.light_theme = on,
                    "auto_hide_ribbon" => self.auto_hide_ribbon = on,
                    "show_comments" => self.show_comments = on,
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
        let body = format!(
            "formula_view={}\nlight_theme={}\nauto_hide_ribbon={}\nshow_comments={}\n",
            self.formula_view as u8,
            self.light_theme as u8,
            self.auto_hide_ribbon as u8,
            self.show_comments as u8,
        );
        let _ = std::fs::write(&p, body);
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
            FillDown => self.fill(true),
            FillRight => self.fill(false),
            InsertRow => self.row_op(true),
            InsertCol => self.col_op(true),
            DeleteRow => self.row_op(false),
            DeleteCol => self.col_op(false),
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
            NewComment => self.start_comment(),
            NewNote => self.start_note(),
            DeleteComment => self.delete_comment(),
            PrevComment => self.nav_comment(-1),
            NextComment => self.nav_comment(1),
            ToggleComments => self.toggle_comments(),
            FormulaView => self.toggle_formula_view(),
            FreezePanes => self.toggle_freeze(),
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

    /// Open the File backstage rooted at the current file's directory.
    fn open_backstage(&mut self) {
        let dir = std::path::Path::new(&self.path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
        self.backstage = Some(backstage::Backstage::open(dir));
        self.ribbon_focus = ribbon::Focus::None;
    }

    /// Replace the whole editing session with a freshly loaded workbook.
    fn open_workbook(&mut self, path: &str) {
        match load_workbook(path) {
            Ok((pkg, p)) => {
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
                self.path = p;
                self.model_rels = rels;
                self.model_measures = meas;
                self.comments = comments;
                self.reset_view();
                self.modified = false;
                self.backstage = None;
                self.start_screen = false;
                self.status = Some(format!("Opened {}", self.path));
            }
            Err(e) => self.status = Some(format!("Open failed: {e}")),
        }
    }

    /// Start a fresh blank workbook (discarding the current one).
    fn new_workbook(&mut self) {
        let pkg = new_xlsx();
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        self.engine = engine;
        self.pkg = pkg;
        self.path = "untitled.xlsx".to_string();
        self.model_rels = Vec::new();
        self.model_measures = Vec::new();
        self.comments = Vec::new();
        self.reset_view();
        self.modified = false;
        self.backstage = None;
        self.start_screen = false;
        self.status = Some("New workbook".to_string());
    }

    fn reset_view(&mut self) {
        self.sheet = 0;
        self.cur = (0, 0);
        self.top = 0;
        self.left = 0;
        self.anchor = None;
        self.edit = None;
        self.undo.clear();
        self.redo.clear();
        self.comment_sel = 0;
    }

    /// Export the current sheet to a `.csv` next to the workbook.
    fn export_csv(&mut self) {
        let csv = sheet_to_csv(
            self.sheet(),
            &self.pkg.workbook.styles,
            self.pkg.workbook.date1904,
        );
        let out = match self.path.rsplit_once('.') {
            Some((base, _)) => format!("{base}.csv"),
            None => format!("{}.csv", self.path),
        };
        match std::fs::write(&out, csv.as_bytes()) {
            Ok(()) => self.status = Some(format!("Exported {out} ({} bytes)", csv.len())),
            Err(e) => self.status = Some(format!("Export failed: {e}")),
        }
        self.backstage = None;
    }

    /// Fill the backstage preview with the highlighted workbook's first sheet.
    fn bs_update_preview(&mut self, width: usize) {
        let Some(bs) = &self.backstage else { return };
        let path = bs.selected_file();
        // Nothing to do if the same file at the same width is already rendered.
        if bs.preview_path == path && bs.preview_w == width && !bs.preview.is_empty() {
            return;
        }
        let lines = match &path {
            Some(p) => preview_lines(&p.to_string_lossy(), width),
            None => Vec::new(),
        };
        if let Some(bs) = &mut self.backstage {
            bs.preview = lines;
            bs.preview_path = path;
            bs.preview_w = width;
            bs.preview_scroll = 0;
        }
    }

    /// Route a key to the backstage. Returns true when the app should exit.
    fn backstage_key(&mut self, code: KeyCode) -> bool {
        use backstage::{Item, Pane};
        let Some(bs) = &mut self.backstage else {
            return false;
        };
        match bs.pane {
            Pane::Menu => match code {
                KeyCode::Esc => self.backstage = None,
                KeyCode::Up => {
                    let i = backstage::ITEMS
                        .iter()
                        .position(|x| *x == bs.item)
                        .unwrap_or(0);
                    bs.item = backstage::ITEMS[i.saturating_sub(1)];
                }
                KeyCode::Down => {
                    let i = backstage::ITEMS
                        .iter()
                        .position(|x| *x == bs.item)
                        .unwrap_or(0);
                    bs.item = backstage::ITEMS[(i + 1).min(backstage::ITEMS.len() - 1)];
                }
                KeyCode::Enter | KeyCode::Right => return self.bs_activate_item(),
                _ => {}
            },
            Pane::Browser => match code {
                KeyCode::Esc | KeyCode::Tab => bs.pane = Pane::Menu,
                KeyCode::Up => bs.move_sel(false),
                KeyCode::Down => bs.move_sel(true),
                KeyCode::Left | KeyCode::Backspace => bs.go_up(),
                KeyCode::Enter | KeyCode::Right => {
                    if let Some(path) = bs.enter() {
                        let p = path.to_string_lossy().into_owned();
                        self.open_workbook(&p);
                    }
                }
                _ => {}
            },
            Pane::Preview => match code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Tab => bs.pane = Pane::Browser,
                KeyCode::Up => bs.preview_scroll = bs.preview_scroll.saturating_sub(1),
                KeyCode::Down => {
                    bs.preview_scroll =
                        (bs.preview_scroll + 1).min(bs.preview.len().saturating_sub(1))
                }
                _ => {}
            },
            Pane::SaveAs => return self.bs_saveas_key(code),
        }
        // Reflect Open/Info highlighting into the item field.
        if let Some(bs) = &mut self.backstage {
            if bs.pane == Pane::Menu && matches!(code, KeyCode::Up | KeyCode::Down) {
                // Sync the right pane to the highlighted item.
                if bs.item == Item::Open || bs.item == Item::Info {
                    bs.preview_path = None; // force a refresh next draw
                }
            }
        }
        false
    }

    /// Activate the highlighted backstage menu item. Returns true to exit.
    fn bs_activate_item(&mut self) -> bool {
        use backstage::{Item, Pane};
        let Some(bs) = &mut self.backstage else {
            return false;
        };
        match bs.item {
            Item::New => self.new_workbook(),
            Item::Open => bs.pane = Pane::Browser,
            Item::Info => bs.pane = Pane::Preview,
            Item::Save => {
                self.save();
                self.backstage = None;
            }
            Item::SaveAs => {
                bs.name_input = std::path::Path::new(&self.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "untitled.xlsx".to_string());
                bs.name_cursor = bs.name_input.chars().count();
                bs.name_focus = true;
                bs.pane = Pane::SaveAs;
            }
            Item::Export => self.export_csv(),
            Item::Exit => {
                if self.modified {
                    self.confirm_quit = true;
                    self.backstage = None;
                } else {
                    return true;
                }
            }
        }
        false
    }

    /// Save As pane: type a name (or browse to a folder) then commit.
    fn bs_saveas_key(&mut self, code: KeyCode) -> bool {
        use backstage::Pane;
        let Some(bs) = &mut self.backstage else {
            return false;
        };
        match code {
            KeyCode::Esc => bs.pane = backstage::Pane::Menu,
            KeyCode::Tab => bs.name_focus = !bs.name_focus,
            KeyCode::Enter => {
                let name = bs.name_input.trim().to_string();
                if name.is_empty() {
                    return false;
                }
                let full = bs.dir.join(&name);
                self.path = full.to_string_lossy().into_owned();
                self.backstage = None;
                self.save();
                return false;
            }
            _ if bs.name_focus => match code {
                KeyCode::Left => bs.name_cursor = bs.name_cursor.saturating_sub(1),
                KeyCode::Right => {
                    bs.name_cursor = (bs.name_cursor + 1).min(bs.name_input.chars().count())
                }
                KeyCode::Backspace => {
                    if bs.name_cursor > 0 {
                        let idx = char_index(&bs.name_input, bs.name_cursor - 1);
                        bs.name_input.remove(idx);
                        bs.name_cursor -= 1;
                    }
                }
                KeyCode::Char(ch) => {
                    let idx = char_index(&bs.name_input, bs.name_cursor);
                    bs.name_input.insert(idx, ch);
                    bs.name_cursor += 1;
                }
                _ => {}
            },
            KeyCode::Up => bs.move_sel(false),
            KeyCode::Down => bs.move_sel(true),
            KeyCode::Left | KeyCode::Backspace => bs.go_up(),
            KeyCode::Right => {
                if let Some(e) = bs.selected() {
                    if e.is_dir {
                        bs.enter();
                    }
                }
            }
            _ => {}
        }
        let _ = Pane::Menu;
        false
    }

    // --- welcome / start screen ----------------------------------------------

    /// Route a key on the welcome screen. Returns true to exit.
    fn start_screen_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Up => self.start_sel = self.start_sel.saturating_sub(1),
            KeyCode::Down => self.start_sel = (self.start_sel + 1).min(START_ITEMS.len() - 1),
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Char(c @ '1'..='3') => {
                self.start_sel = c as usize - '1' as usize;
                return self.start_activate();
            }
            KeyCode::Enter => return self.start_activate(),
            _ => {}
        }
        false
    }

    fn start_activate(&mut self) -> bool {
        match self.start_sel {
            0 => self.new_workbook(), // New workbook
            1 => {
                // Open → jump into the backstage browser.
                self.start_screen = false;
                self.open_backstage();
                if let Some(bs) = &mut self.backstage {
                    bs.pane = backstage::Pane::Browser;
                }
            }
            _ => return true, // Quit
        }
        false
    }

    fn clear_selection(&mut self) {
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

    /// Ctrl-D / Ctrl-R: fill the selection from its first row/column,
    /// translating relative refs — or, on a single cell, pull from the
    /// neighbor above/left.
    fn fill(&mut self, down: bool) {
        let (r1, c1, r2, c2) = self.selection();
        let single = r1 == r2 && c1 == c2;
        let mut changes = Vec::new();
        let copy_from = |sr: u32, sc: u32, tr: u32, tc: u32, changes: &mut Vec<_>| {
            let mut cell = self.pkg.workbook.sheets[self.sheet]
                .cell(sr, sc)
                .cloned()
                .unwrap_or_default();
            if let Some(f) = &cell.formula {
                if let Some(t) = translate_formula(f, tr as i64 - sr as i64, tc as i64 - sc as i64)
                {
                    cell.formula = Some(t);
                }
            }
            changes.push((tr, tc, cell));
        };
        if single {
            let (r, c) = self.cur;
            if down && r > 0 {
                copy_from(r - 1, c, r, c, &mut changes);
            } else if !down && c > 0 {
                copy_from(r, c - 1, r, c, &mut changes);
            }
        } else if down {
            for c in c1..=c2 {
                for r in r1 + 1..=r2 {
                    copy_from(r1, c, r, c, &mut changes);
                }
            }
        } else {
            for r in r1..=r2 {
                for c in c1 + 1..=c2 {
                    copy_from(r, c1, r, c, &mut changes);
                }
            }
        }
        if changes.is_empty() {
            return;
        }
        let n = changes.len();
        self.apply(changes);
        self.status = Some(format!(
            "Filled {n} cell{} {}",
            if n == 1 { "" } else { "s" },
            if down { "down" } else { "right" }
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
        let sheet = self.sheet();
        let changes: Vec<(u32, u32, Cell)> = sheet
            .cells
            .iter()
            .filter_map(|(&(r, c), cell)| {
                let text = input_text_of(cell);
                if text.contains(find) {
                    let mut newcell = parse_input(&text.replace(find, with));
                    newcell.style = cell.style;
                    Some((r, c, newcell))
                } else {
                    None
                }
            })
            .collect();
        let n = changes.len();
        if n == 0 {
            self.status = Some(format!("Not found: {find}"));
            return;
        }
        self.apply(changes);
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
            KeyCode::Char('q') if ctrl => {
                if self.modified {
                    self.confirm_quit = true;
                } else {
                    return true;
                }
            }
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
            "wq" | "x" => {
                self.save();
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
            PromptKind::SaveAs => {
                if !text.is_empty() {
                    self.path = text;
                    self.save();
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

    fn delete_current_sheet(&mut self) {
        let name = self.pkg.workbook.sheets[self.sheet].name.clone();
        if self.pkg.remove_sheet(self.sheet) {
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

/// Interpret typed input as Excel would: formulas, numbers (incl. percent),
/// booleans, error constants, text.
fn parse_input(text: &str) -> Cell {
    if let Some(body) = text.strip_prefix('=') {
        if !body.is_empty() {
            return Cell::formula(body);
        }
    }
    if text.is_empty() {
        return Cell::default();
    }
    let t = text.trim();
    if let Ok(n) = t.parse::<f64>() {
        if n.is_finite() {
            return Cell::number(n);
        }
    }
    if let Some(pct) = t.strip_suffix('%') {
        if let Ok(n) = pct.trim().parse::<f64>() {
            let v = n / 100.0;
            if v.is_finite() {
                return Cell::number(v);
            }
        }
    }
    if t.eq_ignore_ascii_case("TRUE") {
        return Cell {
            value: CellValue::Bool(true),
            ..Cell::default()
        };
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return Cell {
            value: CellValue::Bool(false),
            ..Cell::default()
        };
    }
    if gridcore::formula::ExcelError::from_code(t).is_some() {
        return Cell {
            value: CellValue::Error(t.to_ascii_uppercase()),
            ..Cell::default()
        };
    }
    Cell::text(text)
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
    // Full-screen surfaces paint over everything else.
    if app.start_screen {
        draw_start_screen(app, f, area);
        return;
    }
    if app.backstage.is_some() {
        let preview_w = (area.width as usize * 6 / 10).max(10);
        app.bs_update_preview(preview_w);
        draw_backstage(app, f, area);
        return;
    }
    // --- ribbon (tab strip, plus body + hint when engaged) --------------------
    let toggles = app.ribbon_toggles();
    app.ribbon.set_toggles(toggles);
    let engaged = app.ribbon_focus != ribbon::Focus::None;
    let ribbon_h: u16 = if engaged {
        7
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
        f.render_widget(Paragraph::new(body), Rect::new(area.x, y, area.width, 5));
        y += 5;
        f.render_widget(
            Paragraph::new(app.ribbon.render_hint(app.ribbon_focus, area.width)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }

    let formula_bar = Rect::new(area.x, y, area.width, 1);
    let col_hdr = Rect::new(area.x, y + 1, area.width, 1);
    let grid_h = area.height.saturating_sub(ribbon_h + 4);
    let mut grid = Rect::new(area.x, y + 2, area.width, grid_h);
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

    // Row gutter sized for the largest visible row number.
    let max_row = app.top + grid.height as u32;
    app.gutter_w = (max_row + 1).to_string().len().max(3) as u16 + 1;

    // Visible columns: frozen columns (0..fc) pinned, then scrollable from left.
    app.vis_cols.clear();
    {
        let mut x = app.gutter_w;
        let push = |app: &mut App, col: u32, x: &mut u16| {
            if *x < grid.width && col < MAX_COLS {
                let w = app.col_disp_width(col).min(grid.width - *x);
                app.vis_cols.push((col, *x, w));
                *x += w;
                true
            } else {
                false
            }
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
    app.vis_rows.clear();
    for row in 0..fr {
        if app.vis_rows.len() >= grid.height as usize {
            break;
        }
        app.vis_rows.push(row);
    }
    let mut row = app.top.max(fr);
    while app.vis_rows.len() < grid.height as usize && row < MAX_ROWS {
        app.vis_rows.push(row);
        row += 1;
    }

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
    if let Some(e) = &app.edit {
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
    f.render_widget(Paragraph::new(RLine::from(spans)), formula_bar);

    // --- column headers ------------------------------------------------------
    let mut hdr_spans: Vec<RSpan> =
        vec![RSpan::styled(" ".repeat(app.gutter_w as usize), HDR_STYLE)];
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
    let sheet = app.sheet();
    let styles = &app.pkg.workbook.styles;
    let date1904 = app.pkg.workbook.date1904;
    let mut lines: Vec<RLine> = Vec::with_capacity(grid.height as usize);
    for &row in &vis_rows {
        let mut spans: Vec<RSpan> = Vec::with_capacity(app.vis_cols.len() + 1);
        let gut_style = if row == r { HDR_CUR } else { HDR_STYLE };
        spans.push(RSpan::styled(
            format!("{:>w$} ", row + 1, w = app.gutter_w as usize - 1),
            gut_style,
        ));
        for &(col, _, w) in &app.vis_cols {
            let cell = sheet.cell(row, col);
            let xf = cell.map(|cl| styles.xf(cl.style)).unwrap_or_default();
            let text = match cell {
                Some(cl) if formula_view && cl.formula.is_some() => {
                    format!("={}", cl.formula.as_ref().unwrap())
                }
                Some(cl) => format_with(&xf, &cl.value, date1904),
                None => String::new(),
            };
            let numeric = matches!(cell.map(|cl| &cl.value), Some(CellValue::Number(_)))
                && xf.numfmt != NumFmt::Text;
            // Formula view is always left-aligned (Excel shows the raw text).
            let display = if formula_view {
                fit(&text, w as usize, false)
            } else {
                match xf.align {
                    Align::Left => fit(&text, w as usize, false),
                    Align::Right => fit(&text, w as usize, true),
                    Align::Center => center(&text, w as usize),
                    Align::General => fit(&text, w as usize, numeric),
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

    // --- sheet picker -----------------------------------------------------------
    if let Some(sel) = app.sheet_picker {
        draw_sheet_picker(app, sel, f, grid);
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
        // Same width whether active or not, so tabs stay aligned.
        let label = if active {
            format!("[ {} ]", s.name)
        } else {
            format!("  {}  ", s.name)
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
    } else if app.confirm_quit {
        "Unsaved changes — press Ctrl-Q again to quit without saving, Esc to stay".to_string()
    } else if app.confirm_delete_sheet {
        format!(
            "Delete sheet '{}'? Shift-Del again to confirm, any key to cancel",
            app.pkg.workbook.sheets[app.sheet].name
        )
    } else if let Some(s) = &app.status {
        s.clone()
    } else if app.edit.is_some() {
        "Enter commit ↓ · Tab commit → · Esc cancel".to_string()
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

/// The welcome screen shown when launched with no file.
fn draw_start_screen(app: &App, f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let w = 52u16.min(area.width.saturating_sub(2));
    let h = (START_ITEMS.len() as u16 + 6).min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 3;
    let mut lines: Vec<RLine> = vec![
        RLine::from(RSpan::styled(
            "  xlsxy",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        RLine::from(RSpan::styled(
            "  a terminal spreadsheet editor with a real calc engine",
            Style::new().add_modifier(Modifier::DIM),
        )),
        RLine::from(""),
    ];
    for (i, (label, desc)) in START_ITEMS.iter().enumerate() {
        let sel = i == app.start_sel;
        let style = if sel {
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(RLine::from(vec![
            RSpan::styled(format!("  {}. {label}", i + 1), style),
            RSpan::styled(
                format!("   {desc}"),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ]));
    }
    lines.push(RLine::from(""));
    lines.push(RLine::from(RSpan::styled(
        "  ↑↓ select · Enter open · 1-3 · Esc quit",
        Style::new().add_modifier(Modifier::DIM),
    )));
    f.render_widget(Paragraph::new(lines), Rect::new(x, y, w, h));
}

/// The File backstage: a left menu, plus a folder browser + workbook preview
/// (or the Info screen).
fn draw_backstage(app: &App, f: &mut Frame, area: Rect) {
    use backstage::{Item, Pane};
    let Some(bs) = &app.backstage else { return };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(RLine::from(RSpan::styled(
            fit(" File", area.width as usize, false),
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let menu_w = 16u16.min(area.width / 2);
    let body_y = area.y + 2;
    let body_h = area.height.saturating_sub(3);
    let mut mlines: Vec<RLine> = Vec::new();
    for it in backstage::ITEMS {
        let selected = it == bs.item;
        let style = if selected && bs.pane == Pane::Menu {
            Style::new().fg(Color::Black).bg(Color::White)
        } else if selected {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Gray)
        };
        mlines.push(RLine::from(RSpan::styled(
            fit(&format!("  {}", it.label()), menu_w as usize, false),
            style,
        )));
    }
    f.render_widget(
        Paragraph::new(mlines),
        Rect::new(area.x, body_y, menu_w, body_h),
    );

    let right = Rect::new(area.x + menu_w + 1, body_y, area.width - menu_w - 1, body_h);
    if bs.item == Item::Info {
        draw_bs_info(app, f, right);
    } else {
        draw_bs_browser(f, right, bs);
    }

    let hint = match bs.pane {
        Pane::Menu => "↑↓ menu · Enter choose · Esc close",
        Pane::Browser => "↑↓ files · Enter open · ← up · Tab menu · Esc back",
        Pane::Preview => "↑↓ scroll · Esc back",
        Pane::SaveAs => "type name · Tab switch field · Enter save · Esc back",
    };
    f.render_widget(
        Paragraph::new(RLine::styled(
            fit(hint, area.width as usize, false),
            Style::new().fg(Color::Black).bg(Color::Yellow),
        )),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

fn draw_bs_info(app: &App, f: &mut Frame, area: Rect) {
    let wb = &app.pkg.workbook;
    let sheets: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
    let info = |k: &str, v: String| {
        RLine::from(vec![
            RSpan::styled(
                format!("  {k:<12}"),
                Style::new().add_modifier(Modifier::DIM),
            ),
            RSpan::raw(v),
        ])
    };
    let lines = vec![
        RLine::from(RSpan::styled(
            "  Info",
            Style::new().add_modifier(Modifier::BOLD),
        )),
        RLine::from(""),
        info("Path", app.path.clone()),
        info(
            "Modified",
            if app.modified { "yes" } else { "no" }.to_string(),
        ),
        info(
            "Sheets",
            format!("{} ({})", sheets.len(), sheets.join(", ")),
        ),
        info("Comments", app.comments.len().to_string()),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_bs_browser(f: &mut Frame, area: Rect, bs: &backstage::Backstage) {
    use backstage::Pane;
    let list_w = (area.width * 4 / 10).clamp(18, area.width.saturating_sub(10));
    let mut y = area.y;
    // Current directory.
    f.render_widget(
        Paragraph::new(RLine::styled(
            fit(&bs.dir.to_string_lossy(), list_w as usize, false),
            Style::new().add_modifier(Modifier::DIM),
        )),
        Rect::new(area.x, y, list_w, 1),
    );
    y += 1;
    // Save As name field.
    if bs.pane == Pane::SaveAs {
        let field_style = if bs.name_focus {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        f.render_widget(
            Paragraph::new(RLine::from(vec![
                RSpan::styled("name: ", Style::new().add_modifier(Modifier::DIM)),
                RSpan::styled(bs.name_input.clone(), field_style),
            ])),
            Rect::new(area.x, y, list_w, 1),
        );
        y += 1;
    }
    let list = Rect::new(area.x, y, list_w, area.y + area.height - y);
    let list_focused =
        matches!(bs.pane, Pane::Browser) || (bs.pane == Pane::SaveAs && !bs.name_focus);
    let vis = list.height.max(1) as usize;
    let top = bs.sel.saturating_sub(vis.saturating_sub(1));
    let mut lines: Vec<RLine> = Vec::new();
    for (i, e) in bs.entries.iter().enumerate().skip(top).take(vis) {
        let mark = if e.is_dir { "▸ " } else { "  " };
        let size = e.size_str();
        let label = fit(
            &format!("{mark}{}", e.name),
            list_w as usize - size.len() - 1,
            false,
        );
        let base = if e.locked {
            Style::new().add_modifier(Modifier::DIM)
        } else if e.is_dir {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new()
        };
        let style = if i == bs.sel && list_focused {
            base.add_modifier(Modifier::REVERSED)
        } else {
            base
        };
        lines.push(RLine::from(vec![
            RSpan::styled(label, style),
            RSpan::styled(format!(" {size}"), Style::new().add_modifier(Modifier::DIM)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), list);

    // Preview to the right.
    let prev = Rect::new(
        area.x + list_w + 1,
        area.y,
        area.width - list_w - 1,
        area.height,
    );
    let mut plines: Vec<RLine> = vec![RLine::from(RSpan::styled(
        "Preview",
        Style::new().add_modifier(Modifier::DIM),
    ))];
    for l in bs
        .preview
        .iter()
        .skip(bs.preview_scroll)
        .take(prev.height.saturating_sub(1) as usize)
    {
        plines.push(RLine::from(RSpan::raw(fit(l, prev.width as usize, false))));
    }
    f.render_widget(Paragraph::new(plines), prev);
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
    let w = ((widest + 6) as u16).clamp(16, grid.width.saturating_sub(2));
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

/// Returns true when the app should exit.
fn handle_event(app: &mut App, ev: Event) -> bool {
    match ev {
        Event::Key(key) => handle_key(app, key),
        Event::Mouse(m) => {
            handle_mouse(app, m);
            false
        }
        Event::Resize(_, _) => false,
        _ => false,
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }
    app.status = None;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Full-screen surfaces own the keyboard entirely.
    if app.start_screen {
        return app.start_screen_key(key.code);
    }
    if app.backstage.is_some() {
        return app.backstage_key(key.code);
    }

    if app.confirm_quit {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') if ctrl => return true,
            _ => {
                app.confirm_quit = false;
                return false;
            }
        }
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

    if app.confirm_delete_sheet {
        app.confirm_delete_sheet = false;
        if key.code == KeyCode::Delete && shift {
            app.delete_current_sheet();
        } else {
            app.status = Some("Sheet deletion cancelled".to_string());
        }
        return false;
    }

    // --- ribbon ---------------------------------------------------------------
    let overlay_open = app.pivot_edit.is_some()
        || app.model_view.is_some()
        || app.prompt.is_some()
        || app.edit.is_some()
        || app.format_picker.is_some()
        || app.sheet_picker.is_some();
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

    // --- sheet picker ---------------------------------------------------------
    if app.sheet_picker.is_some() {
        app.sheet_picker_key(key.code);
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
                app.prompt = None;
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

    // --- edit mode -----------------------------------------------------------
    if app.edit.is_some() {
        let replace = app.edit.as_ref().is_some_and(|e| e.replace);
        match key.code {
            KeyCode::Esc => app.cancel_edit(),
            KeyCode::Enter => {
                if app.commit_edit() {
                    app.move_cur(if shift { -1 } else { 1 }, 0, false);
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
                if let Some(e) = &mut app.edit {
                    if e.cursor > 0 {
                        let idx = char_index(&e.text, e.cursor - 1);
                        e.text.remove(idx);
                        e.cursor -= 1;
                    }
                }
            }
            KeyCode::Delete => {
                if let Some(e) = &mut app.edit {
                    if e.cursor < e.text.chars().count() {
                        let idx = char_index(&e.text, e.cursor);
                        e.text.remove(idx);
                    }
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                if let Some(e) = &mut app.edit {
                    let idx = char_index(&e.text, e.cursor);
                    e.text.insert(idx, ch);
                    e.cursor += 1;
                }
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
            if app.modified {
                app.confirm_quit = true;
                return false;
            }
            return true;
        }
        KeyCode::Char('s') | KeyCode::Char('S') if ctrl => app.save(),
        KeyCode::Char('z') | KeyCode::Char('Z') if ctrl => app.undo(),
        KeyCode::Char('y') | KeyCode::Char('Y') if ctrl => app.redo(),
        KeyCode::Char('c') | KeyCode::Char('C') if ctrl => app.copy(false),
        KeyCode::Char('x') | KeyCode::Char('X') if ctrl => app.copy(true),
        KeyCode::Char('v') | KeyCode::Char('V') if ctrl => app.paste(),
        KeyCode::Char('d') | KeyCode::Char('D') if ctrl => app.fill(true),
        KeyCode::Char('r') | KeyCode::Char('R') if ctrl => app.fill(false),
        KeyCode::Char('f') | KeyCode::Char('F') if alt => app.open_backstage(),
        KeyCode::Char('o') | KeyCode::Char('O') if ctrl => {
            app.open_backstage();
            if let Some(bs) = &mut app.backstage {
                bs.pane = backstage::Pane::Browser;
            }
        }
        KeyCode::Char('b') | KeyCode::Char('B') if ctrl => app.toggle_bold(),
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
            app.confirm_delete_sheet = true;
            return false;
        }
        KeyCode::Char('a') | KeyCode::Char('A') if ctrl => {
            let (rows, cols) = app.sheet().used_size();
            if rows > 0 {
                app.anchor = Some((0, 0));
                app.cur = (rows - 1, cols.max(1) - 1);
            }
        }
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
        KeyCode::Enter => app.move_cur(if shift { -1 } else { 1 }, 0, false),
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
        KeyCode::Char(ch) if !ctrl => app.start_edit(Some(ch)),
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

fn handle_mouse(app: &mut App, m: MouseEvent) {
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
                return;
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
                        return;
                    }
                }
                return;
            }
            // Grid?
            let g = app.grid_area;
            if m.row < g.y || m.row >= g.y + g.height || m.column < g.x + app.gutter_w {
                return;
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
            let Some(col) = col else { return };
            if app.edit.is_some() {
                // Clicking outside while editing commits first.
                if !app.commit_edit() {
                    return;
                }
            }
            if drag {
                if app.anchor.is_none() {
                    app.anchor = Some(app.cur);
                }
            } else {
                app.anchor = None;
            }
            app.cur = (row, col);
            if !drag {
                app.follow_hyperlink(row, col);
            }
        }
        _ => {}
    }
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

fn run_tui(pkg: SheetPackage, path: &str, welcome: bool, vim: bool) -> io::Result<()> {
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
    app.load_view_prefs();
    if vim {
        app.vim = Some(VimState {
            mode: VimMode::Normal,
            pending: '\0',
            cmdline: None,
        });
    }
    app.start_screen = welcome;
    let mut last_title = String::new();
    let result = loop {
        // Reflect the file + dirty state in the terminal window title.
        let title = window_title("xlsxy", &app.path, app.modified);
        if title != last_title {
            let _ = execute!(io::stdout(), SetTitle(&title));
            last_title = title;
        }
        if let Err(e) = terminal.draw(|f| draw(&mut app, f)) {
            break Err(e);
        }
        // Same frame pacing as docxy: batch fast input into ~30fps redraws,
        // block when idle.
        let drawn = std::time::Instant::now();
        let frame = std::time::Duration::from_millis(33);
        let mut quit = false;
        let mut got_event = false;
        let mut err = None;
        loop {
            let timeout = if got_event {
                match frame.checked_sub(drawn.elapsed()) {
                    Some(rem) if !rem.is_zero() => rem,
                    _ => break,
                }
            } else {
                std::time::Duration::from_secs(3600)
            };
            match event::poll(timeout) {
                Ok(false) => break,
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        got_event = true;
                        if handle_event(&mut app, ev) {
                            quit = true;
                            break;
                        }
                    }
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                },
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = err {
            break Err(e);
        }
        if quit {
            break Ok(());
        }
    };

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
    fn window_title_format() {
        assert_eq!(
            window_title("xlsxy", "/tmp/report.xlsx", false),
            "xlsxy - report.xlsx"
        );
        assert_eq!(
            window_title("xlsxy", "/tmp/report.xlsx", true),
            "* xlsxy - report.xlsx"
        );
        assert_eq!(
            window_title("xlsxy", "book.xlsx", true),
            "* xlsxy - book.xlsx"
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
        app.start_sel = 0; // New workbook
        assert!(!app.start_activate());
        assert!(!app.start_screen);
        // Quit returns true (exit).
        app.start_screen = true;
        app.start_sel = 2;
        assert!(app.start_activate());
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
        app.fill(true);
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
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
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
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
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
        let pkg = csv_to_pkg("Region,Sales\nEast,10\n\"West, far\",20.5\n", "sales");
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
            grand_rows: false,
            grand_cols: false,
            subtotals: false,
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
    }
}
