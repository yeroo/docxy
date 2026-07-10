//! `xlsxy` — terminal viewer/**editor** for `.xlsx` workbooks.
//!
//! Usage:
//!   xlsxy                               open a new blank workbook
//!   xlsxy <file.xlsx>                   open in the editor
//!   xlsxy <in.xlsx> --recalc <out>      headless: recalculate and save
//!   xlsxy <in.xlsx> --csv <out.csv>     headless: export active sheet as CSV
//!
//! The engine lives in the pure `gridcore` crate; this binary is the TUI
//! shell: a cell grid with Excel muscle memory (formula bar, A1 navigation,
//! range selection, ref-translating copy/paste) and a dependency-graph
//! recalculation on every edit.

use std::io;
use std::process::ExitCode;
use std::time::SystemTime;

use gridcore::engine::Engine;
use gridcore::formula::translate_formula;
use gridcore::sheet::{
    Cell, CellValue, MAX_COLS, MAX_ROWS, NumFmt, Sheet, cell_name, col_name, format_value,
    sheet_to_csv,
};
use gridcore::xlsx::{SheetPackage, load_xlsx, new_xlsx, save_xlsx};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RLine, Span as RSpan};
use ratatui::widgets::Paragraph;
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

    // Friendly cross-suggestion for files that belong to the sibling app.
    if let Some(input) = &parsed.input {
        let lower = input.to_ascii_lowercase();
        if lower.ends_with(".docx") || lower.ends_with(".md") || lower.ends_with(".markdown") {
            eprintln!("{input} is a document, not a spreadsheet — try: docxy {input}");
            return ExitCode::from(2);
        }
    }

    let (pkg, path) = match &parsed.input {
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
            if parsed.recalc_out.is_some() || parsed.csv_out.is_some() || parsed.verify {
                eprintln!("error: headless modes (--recalc/--csv/--verify) require an input file");
                return ExitCode::from(2);
            }
            (new_xlsx(), "untitled.xlsx".to_string())
        }
    };

    if parsed.verify {
        let (report, ok) = verify_report(&pkg, &path);
        print!("{report}");
        return if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    if let Some(out) = parsed.recalc_out {
        let mut pkg = pkg;
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
        engine.recalc_all(&mut pkg.workbook);
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

    match run_tui(pkg, &path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Parsed {
    input: Option<String>,
    recalc_out: Option<String>,
    csv_out: Option<String>,
    verify: bool,
    help: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed {
        input: None,
        recalc_out: None,
        csv_out: None,
        verify: false,
        help: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => p.help = true,
            "--verify" => p.verify = true,
            "--recalc" => {
                i += 1;
                p.recalc_out = Some(args.get(i).ok_or("--recalc needs an output path")?.clone());
            }
            "--csv" => {
                i += 1;
                p.csv_out = Some(args.get(i).ok_or("--csv needs an output path")?.clone());
            }
            a if a.starts_with('-') => return Err(format!("unknown option {a}")),
            a => {
                if p.input.is_some() {
                    return Err("more than one input file".to_string());
                }
                p.input = Some(a.to_string());
            }
        }
        i += 1;
    }
    Ok(p)
}

fn print_usage() {
    eprintln!(
        "Xlsxy — terminal .xlsx spreadsheet editor with a real calc engine\n\n\
         USAGE:\n  \
           xlsxy                            new blank workbook\n  \
           xlsxy <file.xlsx>                open a workbook\n  \
           xlsxy <in> --recalc <out.xlsx>   recalculate all formulas, save, exit\n  \
           xlsxy <in> --csv <out.csv>       export the first sheet as CSV, exit\n  \
           xlsxy <in> --verify              conformance scoreboard: recalculate\n  \
                                            and diff against Excel's cached values\n\n\
         EDITOR KEYS:\n  \
           type to replace · F2 edit in place · = starts a formula\n  \
           Enter/Tab commit (move down/right) · Esc cancel · Del clear\n  \
           arrows / PgUp / PgDn move   (Ctrl-arrows jump to data edge)\n  \
           Shift + move select a range   (stats appear in the status bar)\n  \
           Ctrl-C copy   Ctrl-X cut   Ctrl-V paste (relative refs translate)\n  \
           Ctrl-Z undo   Ctrl-Y redo   Ctrl-S save   Ctrl-Q quit\n  \
           Ctrl-PgUp/PgDn or click tabs to switch sheets\n  \
           F7 / F8 shrink / widen the current column\n  \
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

/// The conformance oracle: every formula cell in a real .xlsx carries the
/// value Excel last computed. Recalculate everything with our engine and
/// diff — the resulting scoreboard measures calculation fidelity on real
/// workbooks and catches semantic regressions.
fn verify_report(pkg: &SheetPackage, path: &str) -> (String, bool) {
    use gridcore::formula::{is_volatile, parse};
    use gridcore::sheet::Workbook;

    let original: &Workbook = &pkg.workbook;
    let mut wb = pkg.workbook.clone();
    let mut engine = Engine::new(&wb);
    engine.clock = now_serial();
    engine.seed = entropy_seed();
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
            if engine.is_unsupported((s, r, c)) {
                unsupported += 1;
                continue;
            }
            if parse(src).map(|ast| is_volatile(&ast)).unwrap_or(false) {
                volatile += 1;
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
    (out, mismatched == 0)
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
    undo: Vec<UndoGroup>,
    redo: Vec<UndoGroup>,
    clip: Option<ClipData>,
    os_clip: Option<arboard::Clipboard>,
    clip_text: Option<String>,
    confirm_quit: bool,
    quit: bool,
    // Geometry captured during draw, for mouse hit-testing.
    grid_area: Rect,
    gutter_w: u16,
    vis_cols: Vec<(u32, u16, u16)>, // (col, x, width)
    tab_spans: Vec<(usize, u16, u16)>,
}

impl App {
    fn new(pkg: SheetPackage, path: &str) -> App {
        let mut engine = Engine::new(&pkg.workbook);
        engine.clock = now_serial();
        engine.seed = entropy_seed();
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
            quit: false,
            grid_area: Rect::default(),
            gutter_w: 4,
            vis_cols: Vec::new(),
            tab_spans: Vec::new(),
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
        match self.sheet().cell(r, c) {
            None => String::new(),
            Some(cell) => {
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
        }
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
        self.undo.push(group);
        self.redo.clear();
        self.modified = true;
    }

    fn undo(&mut self) {
        if let Some(group) = self.undo.pop() {
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
            self.redo.push(group);
            self.modified = true;
            self.status = Some("Undid".to_string());
        } else {
            self.status = Some("Nothing to undo".to_string());
        }
    }

    fn redo(&mut self) {
        if let Some(group) = self.redo.pop() {
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
            self.undo.push(group);
            self.modified = true;
            self.status = Some("Redid".to_string());
        } else {
            self.status = Some("Nothing to redo".to_string());
        }
    }

    // --- clipboard -----------------------------------------------------------

    fn copy(&mut self, cut: bool) {
        let (r1, c1, r2, c2) = self.selection();
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
                    tsv.push_str(&format_value(
                        &cl.value,
                        self.pkg.workbook.styles.xf(cl.style).numfmt,
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
            // External TSV/plain text.
            let mut changes = Vec::new();
            for (dr, line) in text.trim_end_matches('\n').split('\n').enumerate() {
                for (dc, field) in line.trim_end_matches('\r').split('\t').enumerate() {
                    let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                    if r >= MAX_ROWS || c >= MAX_COLS {
                        continue;
                    }
                    let style = self.sheet().cell(r, c).map(|x| x.style).unwrap_or(0);
                    let mut cell = parse_input(field);
                    cell.style = style;
                    changes.push((r, c, cell));
                }
            }
            self.apply(changes);
            self.status = Some("Pasted".to_string());
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

    fn save(&mut self) {
        let bytes = save_xlsx(&self.pkg);
        match std::fs::write(&self.path, &bytes) {
            Ok(()) => {
                self.modified = false;
                self.status = Some(format!("Saved {} ({} bytes)", self.path, bytes.len()));
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    fn clear_selection(&mut self) {
        let (r1, c1, r2, c2) = self.selection();
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
        self.sheet = ((cur + delta).rem_euclid(n)) as usize;
        self.cur = (0, 0);
        self.top = 0;
        self.left = 0;
        self.anchor = None;
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
            return Cell::number(n / 100.0);
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
    if area.height < 5 || area.width < 12 {
        return;
    }
    let formula_bar = Rect::new(area.x, area.y, area.width, 1);
    let col_hdr = Rect::new(area.x, area.y + 1, area.width, 1);
    let grid = Rect::new(area.x, area.y + 2, area.width, area.height - 4);
    let tabs_line = Rect::new(area.x, area.y + area.height - 2, area.width, 1);
    let hint_line = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    app.grid_area = grid;

    // Row gutter sized for the largest visible row number.
    let max_row = app.top + grid.height as u32;
    app.gutter_w = (max_row + 1).to_string().len().max(3) as u16 + 1;

    // Visible columns.
    app.vis_cols.clear();
    {
        let mut x = app.gutter_w;
        let mut col = app.left;
        while x < grid.width && col < MAX_COLS {
            let w = app.col_disp_width(col).min(grid.width - x);
            app.vis_cols.push((col, x, w));
            x += w;
            col += 1;
        }
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
    let sheet = app.sheet();
    let styles = &app.pkg.workbook.styles;
    let date1904 = app.pkg.workbook.date1904;
    let mut lines: Vec<RLine> = Vec::with_capacity(grid.height as usize);
    for vy in 0..grid.height {
        let row = app.top + vy as u32;
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
                Some(cl) => format_value(&cl.value, xf.numfmt, date1904),
                None => String::new(),
            };
            let right = matches!(cell.map(|cl| &cl.value), Some(CellValue::Number(_)))
                && xf.numfmt != NumFmt::Text;
            let display = fit(&text, w as usize, right);
            let mut style = Style::new();
            if xf.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if xf.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if let Some((cr, cg, cb)) = xf.color {
                style = style.fg(Color::Rgb(cr, cg, cb));
            }
            let selected = row >= r1 && row <= r2 && col >= c1 && col <= c2;
            let is_cursor = (row, col) == (r, c);
            if is_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            } else if selected {
                style = style.bg(Color::DarkGray).fg(Color::White);
            }
            spans.push(RSpan::styled(display, style));
        }
        lines.push(RLine::from(spans));
    }
    f.render_widget(Paragraph::new(lines), grid);

    // --- sheet tabs + stats ---------------------------------------------------
    app.tab_spans.clear();
    let mut tab_spans_ui: Vec<RSpan> = vec![RSpan::raw(" ")];
    let mut x: u16 = 1;
    for (i, s) in app.pkg.workbook.sheets.iter().enumerate() {
        let label = format!(" {} ", s.name);
        let w = label.chars().count() as u16;
        let style = if i == app.sheet {
            Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED)
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
    let hint = if app.confirm_quit {
        "Unsaved changes — press Ctrl-Q again to quit without saving, Esc to stay".to_string()
    } else if let Some(s) = &app.status {
        s.clone()
    } else if app.edit.is_some() {
        "Enter commit ↓ · Tab commit → · Esc cancel".to_string()
    } else {
        format!(
            "{}{}  ^S save  ^Q quit  ^Z undo  ^C/^X/^V clip  F2 edit  = formula  F7/F8 col width",
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

/// Pad/clip to exactly `w` columns (char-count based; wide glyphs are rare in
/// spreadsheets and only cost alignment, not correctness).
fn fit(s: &str, w: usize, right: bool) -> String {
    let count = s.chars().count();
    if count >= w {
        let cut: String = s.chars().take(w.saturating_sub(1)).collect();
        format!("{cut} ")
    } else if right {
        format!("{}{} ", " ".repeat(w - count - 1), s)
    } else {
        format!("{}{}", s, " ".repeat(w - count))
    }
}

fn center(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count >= w {
        return s.chars().take(w).collect();
    }
    let lead = (w - count) / 2;
    format!("{}{}{}", " ".repeat(lead), s, " ".repeat(w - count - lead))
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
            app.quit
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

    if app.confirm_quit {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') if ctrl => return true,
            _ => {
                app.confirm_quit = false;
                return false;
            }
        }
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
            // Sheet tabs live on the line right below the grid.
            let tabs_y = app.grid_area.y + app.grid_area.height;
            if !drag && m.row == tabs_y {
                for &(i, x1, x2) in &app.tab_spans {
                    if m.column >= x1 && m.column < x2 {
                        if i != app.sheet {
                            app.sheet = i;
                            app.cur = (0, 0);
                            app.top = 0;
                            app.left = 0;
                            app.anchor = None;
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
            let row = app.top + (m.row - g.y) as u32;
            let mut col = None;
            for &(cidx, x, w) in &app.vis_cols {
                if m.column >= x && m.column < x + w {
                    col = Some(cidx);
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
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Terminal shell
// ---------------------------------------------------------------------------

fn run_tui(pkg: SheetPackage, path: &str) -> io::Result<()> {
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
    let result = loop {
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
}
