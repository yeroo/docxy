//! The window's side of a PDF view's Print and exports (#2258): Ctrl+P and
//! File > Print open the Print dialog once the printers are listed; File >
//! Export shows the two exports, each a dialog and then the Save dialog;
//! their OK hands the job to the view's worker, after the form values
//! filled before it, and the status line says how it went. The dialogs and
//! what they become are `print.rs`'s.

use super::print::{self, Output, PrinterInfo, ViewFacts};
use super::worker::{Export, Op, Spool};
use crate::dialog::{ButtonRole, DialogOwner};
use crate::*;

/// Print or export on a PDF tab that is not open (failed, waiting for its
/// password, or not opened yet).
pub(crate) const PDF_NOT_OPEN_OUTPUT: &str =
    "This PDF is not open, so there is nothing to print or export";

impl Docxy {
    /// The active PDF view, open: its facts for the dialogs. A PDF tab that
    /// is not open says so; any other tab says nothing.
    fn pdf_output_facts(&mut self) -> Option<ViewFacts> {
        let tab = self.tabs.get_mut(self.active)?;
        if tab.kind != Kind::Pdf {
            return None;
        }
        match &tab.surface {
            Surface::Pdf(v) if v.lock.is_none() => Some(ViewFacts::of(v)),
            _ => {
                super::host::note_save_refusal(tab, PDF_NOT_OPEN_OUTPUT);
                None
            }
        }
    }

    /// Ctrl+P, File > Print (#2258): the printers are listed on a thread of
    /// their own (a harness's are made up, at once), then the Print dialog
    /// opens on the view, if it is still the one in front.
    pub(crate) fn pdf_print(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(facts) = self.pdf_output_facts() else {
            return self.refocus(window, cx);
        };
        if let Some(why) = print::refusal(&facts, Output::Print) {
            self.set_status(print::capitalised(why));
            return self.refocus(window, cx);
        }
        if self.backstage {
            self.backstage_back(window, cx);
        }
        if self.harness {
            let active = self.active;
            if let Some(tab) = self.tabs.get_mut(active) {
                printers_arrived(tab, print::harness_printers(), true);
            }
            return cx.notify();
        }
        self.set_status(print::LOOKING);
        let id = self.active_tab_id();
        let (tx, rx) = flume::bounded(1);
        let spawned = std::thread::Builder::new()
            .name("printers".into())
            .spawn(move || {
                let _ = tx.send(print::discover());
            });
        if spawned.is_err() {
            let active = self.active;
            if let Some(tab) = self.tabs.get_mut(active) {
                printers_arrived(tab, Vec::new(), true);
            }
            return cx.notify();
        }
        cx.spawn_in(window, async move |this, cx| {
            let printers = rx.recv_async().await.unwrap_or_default();
            let _ = this.update_in(cx, |this, _, cx| {
                // The tab that asked, wherever it is now: in front it gets
                // the dialog; behind, its status only stops looking.
                let front = this.active_tab_is(&id);
                let asked = this
                    .tabs
                    .iter()
                    .position(|t| Some((t.title.clone(), t.path.clone())) == id);
                if let Some(tab) = asked.and_then(|i| this.tabs.get_mut(i)) {
                    printers_arrived(tab, printers, front);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// File > Export's two choices on a PDF tab (#2258): their dialog.
    pub(crate) fn pdf_export_dialog(
        &mut self,
        what: Output,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(facts) = self.pdf_output_facts() else {
            return self.refocus(window, cx);
        };
        if let Some(why) = print::refusal(&facts, what) {
            self.set_status(print::capitalised(why));
            return self.refocus(window, cx);
        }
        if self.backstage {
            self.backstage_back(window, cx);
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let d = match what {
            Output::Png => print::png_dialog(),
            _ => print::pdf_dialog(),
        };
        tab.dialogs.push(d);
        cx.notify();
    }

    /// A press on the Print dialog or an export dialog (#2258): Cancel
    /// closes it; Print sends the job; an export's Save… asks where (a
    /// harness waits for its `save-as` verb), then sends it. A choice OK
    /// refuses keeps the dialog open and says why. `None` for any other
    /// dialog.
    pub(crate) fn pdf_output_click(
        &mut self,
        button: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Result<(), String>> {
        let tab = self.tabs.get_mut(self.active)?;
        let top = tab.dialogs.top()?;
        let owner = top.owner;
        if !matches!(
            owner,
            DialogOwner::PdfPrint | DialogOwner::PdfExportPng | DialogOwner::PdfExportPdf
        ) {
            return None;
        }
        let role = top
            .buttons
            .iter()
            .find(|b| b.label.eq_ignore_ascii_case(button))
            .map(|b| b.role);
        let Some(role) = role else {
            return Some(Err(format!("the dialog has no '{button}' button")));
        };
        if role != ButtonRole::Accept {
            tab.dialogs.pop();
            self.refocus(window, cx);
            return Some(Ok(()));
        }
        let title = tab.title.to_string();
        let harness = self.harness;
        let Surface::Pdf(v) = &mut tab.surface else {
            tab.dialogs.pop();
            return Some(Err(PDF_NOT_OPEN_OUTPUT.into()));
        };
        let d = tab.dialogs.top()?;
        let facts = ViewFacts::of(v);
        if owner == DialogOwner::PdfPrint {
            let spool = if harness {
                Spool::Dir {
                    dir: print::harness_spool(&config_root()),
                }
            } else {
                Spool::System
            };
            let job = match print::print_job(d, &facts, &v.printers, &title, spool) {
                Ok(job) => job,
                Err(why) => return Some(Err(why)),
            };
            tab.dialogs.pop();
            v.commit_focus();
            v.form_ops.push_back(Op::Print { job });
            tab.status = print::PRINTING.into();
            self.refocus(window, cx);
            cx.notify();
            return Some(Ok(()));
        }
        let export = match print::export_of(d, &facts) {
            Ok(e) => e,
            Err(why) => return Some(Err(why)),
        };
        tab.dialogs.pop();
        v.commit_focus();
        if harness {
            v.export_waiting = Some(export);
            tab.status = print::EXPORT_HARNESS.into();
            self.refocus(window, cx);
            cx.notify();
            return Some(Ok(()));
        }
        self.pdf_export_save_dialog(export, window, cx);
        Some(Ok(()))
    }

    /// The native Save dialog for an export, then the export to the file
    /// picked.
    fn pdf_export_save_dialog(
        &mut self,
        export: Export,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let filter = match export {
            Export::Png { .. } => "PNG picture",
            Export::Pdf { .. } => "PDF",
        };
        let name = print::export_name(&tab.title, &export);
        let mut dialog = rfd::FileDialog::new()
            .add_filter(filter, &[export.extension()])
            .set_file_name(name);
        if let Some(dir) = tab.path.as_deref().and_then(std::path::Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        let id = self.active_tab_id();
        self.native_dialog(
            dialog,
            |d| d.save_file(),
            move |this, target, window, cx| {
                let Some(target) = target.filter(|_| this.active_tab_is(&id)) else {
                    return this.refocus(window, cx);
                };
                let sent = print::picked_export_target(target, &export)
                    .and_then(|target| this.pdf_export_to(export, target));
                if let Err(why) = sent {
                    this.set_status(why);
                }
                this.refocus(window, cx);
            },
            window,
            cx,
        );
    }

    /// Hand an export to the active view's worker, to `target`, which the
    /// caller checked (`print::export_target`): the path the export writes,
    /// or why not.
    pub(crate) fn pdf_export_to(
        &mut self,
        export: Export,
        target: std::path::PathBuf,
    ) -> Result<std::path::PathBuf, String> {
        let tab = self.tabs.get_mut(self.active).ok_or(PDF_NOT_OPEN_OUTPUT)?;
        let Surface::Pdf(v) = &mut tab.surface else {
            return Err(PDF_NOT_OPEN_OUTPUT.into());
        };
        v.form_ops.push_back(Op::ExportTo {
            export,
            path: target.clone(),
        });
        tab.status = print::EXPORTING.into();
        Ok(target)
    }
}

impl Docxy {
    /// File > Export on a PDF tab (#2258): pages as PNG pictures, or as a
    /// PDF of their own. `None` unless it is selected on a PDF tab.
    pub(crate) fn pdf_export_page(
        &self,
        bg: Hsla,
        fg: Hsla,
        dim: Hsla,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.bs_export {
            return None;
        }
        self.tabs.get(self.active).filter(|t| t.kind == Kind::Pdf)?;
        let mut rows = v_flex().gap_3();
        for (what, id, name, sub) in [
            (
                Output::Png,
                "pdf-export-png",
                "Save pages as PNG pictures",
                "Each page as a picture, as it is drawn, at the resolution you choose",
            ),
            (
                Output::Pdf,
                "pdf-export-pdf",
                "Save pages as a PDF",
                "The pages you choose as a new PDF, with the values filled in",
            ),
        ] {
            rows = rows.child(
                v_flex()
                    .id(id)
                    .relative()
                    .w(px(420.))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(dim)
                    .cursor_pointer()
                    .hover(|d| d.border_color(rgb(crate::accent::brand())))
                    .child(
                        div()
                            .text_color(fg)
                            .font_weight(FontWeight::BOLD)
                            .child(name),
                    )
                    .child(div().text_size(px(12.)).text_color(dim).child(sub))
                    .child(probe(&self.probes, id))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pdf_export_dialog(what, window, cx)
                    })),
            );
        }
        Some(
            v_flex()
                .w_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(fg)
                        .child("Export"),
                )
                .child(rows)
                .into_any_element(),
        )
    }
}

/// The printers listed for `tab`'s Print (#2258 r1): in front (`front`),
/// its view open and no dialog on it, the Print dialog opens with them; in
/// every case its status stops saying it is looking for printers. Whether
/// the dialog opened.
pub(crate) fn printers_arrived(tab: &mut DocTab, printers: Vec<PrinterInfo>, front: bool) -> bool {
    if tab.status.as_ref() == print::LOOKING {
        tab.status = SharedString::default();
    }
    let Surface::Pdf(v) = &mut tab.surface else {
        return false;
    };
    if !front || v.lock.is_some() || tab.dialogs.top().is_some() {
        return false;
    }
    let d = print::print_dialog(&ViewFacts::of(v), &printers);
    v.printers = printers;
    tab.dialogs.push(d);
    true
}
