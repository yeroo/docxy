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
    Exit,
}
