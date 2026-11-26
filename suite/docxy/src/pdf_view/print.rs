//! Print and export as drawn (#2258), the parent's side: the Print dialog
//! (Acrobat's: printer, copies, pages, page sizing, orientation, paper,
//! comments and forms) and the two export dialogs (pages as PNG pictures,
//! pages as a PDF), what their choices become on the wire, and the
//! printers. Nothing here reads the PDF: a typed range is parsed against
//! the page count and labels `info` gave, and the job goes to the worker
//! child, which lays the sheets out from the page as drawn and spools them
//! (`worker/doc.rs`).
//!
//! The printers are listed on a thread of their own before the dialog
//! opens (`lpstat` and the queue's PPD on CUPS take a while), never on the
//! view's worker thread, which would hold its pages. A harness instance
//! never prints: its printers are [`harness_printers`] and its jobs go to a
//! folder ([`harness_spool`]).

use super::worker::{Export, Orient, PrintContent, PrintJob, SecurityMeta, Sizing, Spool};
use crate::dialog::catalog;
use crate::dialog::{Button, ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use std::path::{Path, PathBuf};

/// The Print dialog's Pages choices.
pub(crate) const PAGES: [&str; 3] = ["All", "Current page", "Pages"];
/// Page sizing, as Acrobat names it.
pub(crate) const SIZING: [&str; 4] = [
    "Fit",
    "Actual size",
    "Shrink oversized pages",
    "Custom scale",
];
/// The sheets' orientation.
pub(crate) const ORIENTATION: [&str; 3] = ["Auto portrait/landscape", "Portrait", "Landscape"];
/// Comments and forms.
pub(crate) const CONTENT: [&str; 3] = ["Document and markups", "Document", "Form fields only"];
/// The PNG export's resolutions, dots per inch.
pub(crate) const DPIS: [u32; 4] = [72, 150, 300, 600];
/// The PNG export's resolution when the dialog opens.
pub(crate) const DEFAULT_DPI: usize = 1;
/// What the printer list shows when the system has none.
pub(crate) const NO_PRINTERS: &str = "No printers found";
/// The status line while the printers are listed, before the dialog opens.
pub(crate) const LOOKING: &str = "Looking for printers…";
/// The status line while a job is laid out and sent.
pub(crate) const PRINTING: &str = "Printing…";
/// The status line while an export is drawn and written.
pub(crate) const EXPORTING: &str = "Exporting…";
/// Export on a PDF tab in a harness, which never opens the Save dialog: the
/// harness's `save-as` verb names the file instead.
pub(crate) const EXPORT_HARNESS: &str =
    "A harness instance cannot open the Save dialog; use the harness save-as verb to name the file";
/// The most copies a job asks for, as CUPS's `-n` takes them.
pub(crate) const MAX_COPIES: u32 = 999;
/// The custom scale's bounds, percent.
pub(crate) const MIN_SCALE: f64 = 1.;
pub(crate) const MAX_SCALE: f64 = 1000.;

/// A printer as the dialog offers it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PrinterInfo {
    /// What the spooler knows it by (`lp -d`).
    pub name: String,
    /// What the list shows: the printer's own description and its name
    /// when they differ (a CUPS queue cannot hold dots or spaces).
    pub shown: String,
    pub default: bool,
    /// Its default paper, as an index into `pdfcraft_print::PAPERS`, when the
    /// spooler says and it is one of them.
    pub paper: Option<usize>,
}

/// The system's printers, the default first, each with its default paper:
/// `lpstat` and the queues' PPDs on CUPS, `EnumPrinters` on Windows. Slow:
/// called on a thread of its own.
pub(crate) fn discover() -> Vec<PrinterInfo> {
    let printers = pdfcraft_print::spool::printers();
    let mut out: Vec<PrinterInfo> = printers
        .into_iter()
        .map(|p| {
            let paper = pdfcraft_print::printer_default_paper(&p.name)
                .and_then(|(_, size)| pdfcraft_print::matching_paper(size));
            let shown = if p.description.trim().is_empty() || p.description == p.name {
                p.name.clone()
            } else {
                format!("{} ({})", p.description, p.name)
            };
            PrinterInfo {
                name: p.name,
                shown,
                default: p.default,
                paper,
            }
        })
        .collect();
    out.sort_by_key(|p| !p.default);
    out
}

/// A harness instance's printers (#2258): two made-up ones, the first the
/// default, neither with a default paper; nothing asks the system.
pub(crate) fn harness_printers() -> Vec<PrinterInfo> {
    ["Test Printer", "Second Printer"]
        .iter()
        .enumerate()
        .map(|(i, name)| PrinterInfo {
            name: name.to_string(),
            shown: name.to_string(),
            default: i == 0,
            paper: None,
        })
        .collect()
}

/// Where a harness instance's print jobs go: `print-jobs` in its sandbox.
pub(crate) fn harness_spool(config_root: &Path) -> PathBuf {
    config_root.join("print-jobs")
}

/// The paper the dialog starts at: the printer's default paper when the
/// spooler says, else the standard paper the first page matches (either
/// way round), else US Letter, PdfCraft's own first choice. No locale is
/// read.
pub(crate) fn starting_paper(
    printer: Option<&PrinterInfo>,
    first_page: Option<(f32, f32)>,
) -> usize {
    printer
        .and_then(|p| p.paper)
        .or_else(|| {
            first_page
                .and_then(|(w, h)| pdfcraft_print::matching_paper((f64::from(w), f64::from(h))))
        })
        .unwrap_or(0)
}

/// What the dialogs need of the open view: its page count, labels, the page
/// showing, and its security when encrypted.
#[derive(Clone, Debug, Default)]
pub(crate) struct ViewFacts {
    pub labels: Vec<String>,
    pub current: usize,
    pub first_page: Option<(f32, f32)>,
    pub security: Option<SecurityMeta>,
}

impl ViewFacts {
    pub(crate) fn of(v: &super::PdfView) -> ViewFacts {
        ViewFacts {
            labels: v.meta.pages.iter().map(|p| p.label.clone()).collect(),
            current: v.current(),
            first_page: v.meta.pages.first().map(|p| (p.w, p.h)),
            security: v.meta.security.clone(),
        }
    }

    fn count(&self) -> usize {
        self.labels.len()
    }
}

/// Why the file does not let this be done, from its security (#2258): the
/// child refuses too, whatever sends it.
pub(crate) fn refusal(facts: &ViewFacts, what: Output) -> Option<&'static str> {
    use super::worker::doc_refusals as r;
    let s = facts.security.as_ref()?;
    if s.owner {
        return None;
    }
    match what {
        Output::Print if !s.print => Some(r().0),
        Output::Png if !s.copy => Some(r().1),
        Output::Pdf if !s.extract => Some(r().2),
        _ => None,
    }
}

/// What a dialog makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Output {
    Print,
    Png,
    Pdf,
}

fn choice<S: AsRef<str>>(
    name: &'static str,
    label: &str,
    kind: ControlKind,
    items: &[S],
    at: usize,
) -> Control {
    let mut c = Control::new(name, label, kind, Value::Choice(Some(at)));
    c.items = items.iter().map(|s| s.as_ref().to_string()).collect();
    c
}

fn text(name: &'static str, label: &str, kind: ControlKind, value: &str) -> Control {
    Control::new(name, label, kind, Value::Text(value.into()))
}

fn form(
    id: crate::dialog::DialogId,
    title: &str,
    owner: DialogOwner,
    controls: Vec<Control>,
    accept: &str,
) -> Dialog {
    let mut d = Dialog::message(id, title, String::new(), &[], owner);
    d.text = None;
    d.controls = controls;
    d.buttons = vec![
        // Enter presses it, as OK.
        Button {
            default: true,
            ..Button::new(accept, ButtonRole::Accept)
        },
        Button::new("Cancel", ButtonRole::Cancel),
    ];
    d.react = Some(crate::dialog::Reaction(react));
    d.mark_opened();
    d
}

/// The controls every dialog has for its pages: All, Current page, or the
/// pages typed.
fn page_controls() -> [Control; 2] {
    [
        choice("pages", "Pages to print:", ControlKind::Radio, &PAGES, 0),
        text("range", "&Pages:", ControlKind::Text, ""),
    ]
}

/// Typing a range picks Pages, and typing a percent picks Custom scale, as
/// in Acrobat's dialog.
fn react(d: &mut Dialog, i: usize, _before: &Value) {
    let pick = match (d.controls[i].name, &d.controls[i].value) {
        ("range", Value::Text(t)) if !t.trim().is_empty() => Some(("pages", 2)),
        ("percent", Value::Text(t)) if !t.trim().is_empty() => Some(("sizing", 3)),
        _ => None,
    };
    if let Some((name, at)) = pick
        && let Some(c) = d.controls.iter_mut().find(|c| c.name == name)
    {
        c.value = Value::Choice(Some(at));
    }
}

/// Acrobat's Print dialog on the view (#2258), the printers listed already.
pub(crate) fn print_dialog(facts: &ViewFacts, printers: &[PrinterInfo]) -> Dialog {
    let names: Vec<String> = if printers.is_empty() {
        vec![NO_PRINTERS.to_string()]
    } else {
        printers.iter().map(|p| p.shown.clone()).collect()
    };
    let default = printers.iter().position(|p| p.default).unwrap_or(0);
    let papers: Vec<&str> = pdfcraft_print::PAPERS.iter().map(|(n, _)| *n).collect();
    let paper = starting_paper(printers.get(default), facts.first_page);
    let [pages, range] = page_controls();
    let controls = vec![
        choice(
            "printer",
            "&Printer:",
            ControlKind::Dropdown,
            &names,
            default,
        ),
        text("copies", "&Copies:", ControlKind::Number, "1"),
        pages,
        range,
        choice("sizing", "Page sizing:", ControlKind::Radio, &SIZING, 0),
        text("percent", "Custom &scale (%):", ControlKind::Number, "100"),
        choice(
            "orientation",
            "Orientation:",
            ControlKind::Radio,
            &ORIENTATION,
            0,
        ),
        choice("paper", "Pape&r:", ControlKind::Dropdown, &papers, paper),
        choice(
            "content",
            "Comments and &forms:",
            ControlKind::Dropdown,
            &CONTENT,
            0,
        ),
    ];
    form(
        catalog::PDF_PRINT,
        "Print",
        DialogOwner::PdfPrint,
        controls,
        "Print",
    )
}

/// Save pages as PNG pictures (#2258).
pub(crate) fn png_dialog() -> Dialog {
    let [pages, range] = page_controls();
    let dpis: Vec<String> = DPIS.iter().map(|d| format!("{d} dpi")).collect();
    let controls = vec![
        pages,
        range,
        choice(
            "dpi",
            "&Resolution:",
            ControlKind::Dropdown,
            &dpis,
            DEFAULT_DPI,
        ),
    ];
    form(
        catalog::PDF_EXPORT_PNG,
        "Save Pages as PNG Pictures",
        DialogOwner::PdfExportPng,
        controls,
        "Save…",
    )
}

/// Save pages as a PDF (#2258).
pub(crate) fn pdf_dialog() -> Dialog {
    let [pages, range] = page_controls();
    form(
        catalog::PDF_EXPORT_PDF,
        "Save Pages as a PDF",
        DialogOwner::PdfExportPdf,
        vec![pages, range],
        "Save…",
    )
}

fn text_of(d: &Dialog, name: &str) -> String {
    match d.value(name) {
        Some(Value::Text(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn choice_of(d: &Dialog, name: &str) -> usize {
    match d.value(name) {
        Some(Value::Choice(Some(i))) => *i,
        _ => 0,
    }
}

/// The pages a dialog's Pages choice and range pick, 0-based, in order:
/// the range parsed as PdfCraft's Print parses it (numbers, labels, open
/// ranges like `9-`), its refusals in PdfCraft's own words.
pub(crate) fn chosen_pages(d: &Dialog, facts: &ViewFacts) -> Result<Vec<usize>, String> {
    let count = facts.count();
    match choice_of(d, "pages") {
        0 => Ok((0..count).collect()),
        1 => Ok(vec![facts.current.min(count.saturating_sub(1))]),
        _ => {
            let range = text_of(d, "range");
            if range.is_empty() {
                return Err("Type the pages to print, such as 1-3, 6, 9-".into());
            }
            select(count, &range, &facts.labels)
        }
    }
}

/// `range` of a document of `count` pages labelled `labels`.
pub(crate) fn select(count: usize, range: &str, labels: &[String]) -> Result<Vec<usize>, String> {
    pdfcraft_print::select_pages(
        count,
        Some(range),
        labels,
        pdfcraft_print::range::Subset::All,
        false,
    )
    .map_err(|e| e.said(&pdfcraft_print::in_english))
    .and_then(|p| {
        if p.is_empty() {
            Err(pdfcraft_print::PrintError::NoPages.said(&pdfcraft_print::in_english))
        } else {
            Ok(p)
        }
    })
}

/// The Print dialog's choices as a job for the worker, or why OK refuses
/// (the dialog stays open).
pub(crate) fn print_job(
    d: &Dialog,
    facts: &ViewFacts,
    printers: &[PrinterInfo],
    title: &str,
    spool: Spool,
) -> Result<PrintJob, String> {
    if let Some(why) = refusal(facts, Output::Print) {
        return Err(capitalised(why));
    }
    let printer = printers.get(choice_of(d, "printer")).ok_or(NO_PRINTERS)?;
    let copies: u32 = text_of(d, "copies")
        .parse()
        .ok()
        .filter(|n| (1..=MAX_COPIES).contains(n))
        .ok_or_else(|| format!("Copies takes a whole number from 1 to {MAX_COPIES}"))?;
    let pages = chosen_pages(d, facts)?;
    let sizing = match choice_of(d, "sizing") {
        0 => Sizing::Fit,
        1 => Sizing::Actual,
        2 => Sizing::Shrink,
        _ => {
            let percent: f64 = text_of(d, "percent")
                .trim_end_matches('%')
                .trim()
                .parse()
                .ok()
                .filter(|p: &f64| p.is_finite() && (MIN_SCALE..=MAX_SCALE).contains(p))
                .ok_or_else(|| {
                    format!("Custom scale takes a percent from {MIN_SCALE} to {MAX_SCALE}")
                })?;
            Sizing::Custom { percent }
        }
    };
    let (paper_name, paper) = pdfcraft_print::PAPERS
        .get(choice_of(d, "paper"))
        .map(|(n, s)| (Some(n.to_string()), *s))
        .unwrap_or((None, pdfcraft_print::PAPERS[0].1));
    Ok(PrintJob {
        pages,
        paper_name,
        paper,
        sizing,
        orientation: match choice_of(d, "orientation") {
            1 => Orient::Portrait,
            2 => Orient::Landscape,
            _ => Orient::Auto,
        },
        content: match choice_of(d, "content") {
            1 => PrintContent::Document,
            2 => PrintContent::FormFieldsOnly,
            _ => PrintContent::DocumentAndMarkups,
        },
        printer: Some(printer.name.clone()),
        copies,
        collate: true,
        title: title.to_string(),
        spool,
    })
}

/// An export dialog's choices, or why OK refuses.
pub(crate) fn export_of(d: &Dialog, facts: &ViewFacts) -> Result<Export, String> {
    let (what, png) = match d.owner {
        DialogOwner::PdfExportPng => (Output::Png, true),
        _ => (Output::Pdf, false),
    };
    if let Some(why) = refusal(facts, what) {
        return Err(capitalised(why));
    }
    let pages = chosen_pages(d, facts)?;
    Ok(if png {
        // A page named twice is one picture (#2258 r1).
        let pages = super::worker::distinct(&pages);
        let dpi = DPIS
            .get(choice_of(d, "dpi"))
            .copied()
            .unwrap_or(DPIS[DEFAULT_DPI]);
        Export::Png {
            pages,
            dpi: f64::from(dpi),
        }
    } else {
        Export::Pdf { pages }
    })
}

/// The name an export's Save dialog offers: the PDF's own name with the
/// export's extension, and for a PDF of some of its pages `<name>_pages`.
pub(crate) fn export_name(file: &str, export: &Export) -> String {
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    match export {
        Export::Png { pages, .. } if pages.len() == 1 => {
            format!("{stem}_Page_{}.png", pages[0] + 1)
        }
        Export::Png { .. } => format!("{stem}.png"),
        Export::Pdf { .. } => format!("{stem}_pages.pdf"),
    }
}

/// `path` with the extension `ext` (#2258): one without an extension gets
/// it (a native dialog may not add its filter's), and says it did; one
/// with `ext` (any case) is kept; any other is refused, `what` naming the
/// file (`"These pages are"`, `"A PDF is"`), since the dialog confirmed
/// replacing that name and no other. Save As of a PDF (#2255) uses it too.
pub(crate) fn with_extension(
    path: PathBuf,
    ext: &str,
    what: &str,
) -> Result<(PathBuf, bool), String> {
    match path.extension() {
        None => {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(format!(".{ext}"));
            Ok((path.with_file_name(name), true))
        }
        Some(e) if e.eq_ignore_ascii_case(ext) => Ok((path, false)),
        Some(e) => Err(format!(
            "{what} saved as a .{ext} file, not .{}: choose a name that ends in .{ext}",
            e.to_string_lossy()
        )),
    }
}

/// The path an export's Save dialog gave, with the export's extension
/// ([`with_extension`]), and whether it was added to the name picked.
pub(crate) fn export_target(path: PathBuf, export: &Export) -> Result<(PathBuf, bool), String> {
    with_extension(path, export.extension(), "These pages are")
}

/// The Save dialog's answer for an export (#2258 r1): its target, but a
/// name the extension was added to that is a file already is refused (the
/// dialog confirmed replacing the name typed, not that one).
pub(crate) fn picked_export_target(path: PathBuf, export: &Export) -> Result<PathBuf, String> {
    let (target, added) = export_target(path, export)?;
    if added && target.exists() {
        return Err(format!(
            "{} already exists: type the whole name, with .{}, to replace it",
            target.display(),
            export.extension()
        ));
    }
    Ok(target)
}

/// The status line after a print: "Sent N pages to <printer>".
pub(crate) fn printed_status(job: &PrintJob, sheets: usize) -> String {
    let s = if sheets == 1 { "" } else { "s" };
    let printer = job.printer.as_deref().unwrap_or("the default printer");
    let copies = if job.copies > 1 {
        format!(", {} copies", job.copies)
    } else {
        String::new()
    };
    format!("Sent {sheets} page{s} to {printer}{copies}")
}

/// The status line after an export.
pub(crate) fn exported_status(files: &[PathBuf], dpi_used: Option<f64>) -> String {
    let at = dpi_used
        .map(|d| {
            format!(
                " (at {} dpi: the most a page of this size is drawn at)",
                d.floor()
            )
        })
        .unwrap_or_default();
    match files {
        [one] => format!("Saved {}{at}", one.display()),
        many => {
            let dir = many
                .first()
                .and_then(|p| p.parent())
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            format!("Saved {} pictures in {dir}{at}", many.len())
        }
    }
}

pub(crate) fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "print_tests.rs"]
pub(crate) mod tests;
