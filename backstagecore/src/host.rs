use std::path::{Path, PathBuf};

/// Format-specific content the backstage needs from its host app: which file
/// extensions the folder browser lists/opens, the default Save As name, a
/// rendered preview of the highlighted file, the Info pane's content, and the
/// app's ribbon accent color.
pub trait BackstageHost {
    fn extensions(&self) -> &'static [&'static str];
    fn default_save_name(&self) -> String;
    fn preview_lines(&self, path: &Path, width: usize) -> Vec<String>;
    fn info_lines(&self) -> Vec<ratatui::text::Line<'static>>;
    fn accent(&self) -> ratatui::style::Color;
    /// The *Save as type* Save As opens on (an index into the host's
    /// types): the type the document is bound to. `None` lets the name's
    /// extension pick it. Only hosts with a type list need it.
    fn default_save_type(&self) -> Option<usize> {
        None
    }
    /// Editable rows the Info pane lists below [`BackstageHost::info_lines`]:
    /// (label, current value). With any (or [`BackstageHost::info_custom_row`]),
    /// Info takes the focus like Options does, and Enter on a row returns
    /// [`BackstageEvent::EditInfo`]. None by default: Info stays read-only.
    fn info_fields(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Whether the Info pane ends with a `Custom property…` row (its
    /// [`BackstageEvent::EditInfo`] index is `info_fields().len()`).
    fn info_custom_row(&self) -> bool {
        false
    }
    /// Whether the document is in Excel's Compatibility Mode (an Excel
    /// 97-2003 file, #874): the Info pane then starts with a Compatibility
    /// Mode section whose `Convert` row returns [`BackstageEvent::Convert`],
    /// before the editable rows.
    fn compat_mode(&self) -> bool {
        false
    }
}

/// The app-level action requested by a `key`/`mouse` call on [`crate::Backstage`].
/// The host is responsible for actually performing it (and, other than `None`,
/// for dropping/closing the backstage afterward as appropriate).
#[derive(Debug, Clone)]
pub enum BackstageEvent {
    /// Nothing for the host to do; the backstage handled the input itself.
    None,
    /// Esc: close the backstage panel.
    Close,
    New,
    Open(PathBuf),
    Save,
    SaveAs {
        dir: PathBuf,
        name: String,
    },
    Export,
    /// One of the extra exports [`crate::Backstage::with_extra_exports`]
    /// listed under the quick export, by index.
    ExportExtra(usize),
    Exit,
    /// Enter (or a second click) on the Info pane's property row `i`: an
    /// index into [`BackstageHost::info_fields`], or `info_fields().len()`
    /// for the custom-property row after them. The Compatibility Mode
    /// `Convert` row isn't counted ([`BackstageEvent::Convert`]).
    EditInfo(usize),
    /// Enter (or a second click) on the Info pane's `Convert` row, shown
    /// while [`BackstageHost::compat_mode`]: convert the document to the
    /// current file format.
    Convert,
}
