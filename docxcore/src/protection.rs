//! Central DOCX mutation authorization policy shared by every host surface.
//!
//! Route adapters classify an edit by semantic effect, then call [`authorize`]
//! before changing document, history, dirty, or package state. Keeping this in
//! `docxcore` prevents terminal and WASM hosts from drifting apart.

use crate::package::{Protection, ProtectionEditMode};

/// The semantic effect of a DOCX mutation, independent of its UI/control route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Content,
    Structure,
    Formatting,
    Comment,
    PackageMetadata,
}

/// Why an enforced protection declaration denied a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionDenial {
    ReadOnly,
    CommentsOnly,
    FormattingLocked,
    FormsUnsupported,
    TrackedChangesUnsupported,
    UnsupportedMode(String),
}

impl ProtectionDenial {
    /// Stable, machine-readable code for control and MCP callers.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::CommentsOnly => "comments_only",
            Self::FormattingLocked => "formatting_locked",
            Self::FormsUnsupported => "forms_unsupported",
            Self::TrackedChangesUnsupported => "tracked_changes_unsupported",
            Self::UnsupportedMode(_) => "unsupported_mode",
        }
    }

    fn explanation(&self) -> String {
        match self {
            Self::ReadOnly => "the document is protected read-only".to_string(),
            Self::CommentsOnly => "only comment edits are allowed".to_string(),
            Self::FormattingLocked => "document formatting is locked".to_string(),
            Self::FormsUnsupported => {
                "form-fields-only editing is not supported; use Word to edit form fields"
                    .to_string()
            }
            Self::TrackedChangesUnsupported => {
                "tracked-only editing is not supported; use Word to create tracked changes"
                    .to_string()
            }
            Self::UnsupportedMode(mode) => {
                format!("the document uses unsupported protection mode '{mode}'")
            }
        }
    }

    /// Concise user-facing status text for an interactive denial.
    pub fn tui_status(&self) -> String {
        format!("Edit blocked: {}.", self.explanation())
    }

    /// Stable control error envelope.
    pub fn control_error(&self) -> String {
        format!("protection_denied:{}: {}", self.code(), self.explanation())
    }
}

/// Authorize a mutation against structured package protection metadata.
pub fn authorize(protection: &Protection, mutation: MutationKind) -> Result<(), ProtectionDenial> {
    if protection.enforced_write_protection {
        return Err(ProtectionDenial::ReadOnly);
    }
    if !protection.is_enforced() {
        return Ok(());
    }

    match protection.edit_mode.as_ref() {
        Some(ProtectionEditMode::ReadOnly) => return Err(ProtectionDenial::ReadOnly),
        Some(ProtectionEditMode::Comments) => {
            return if mutation == MutationKind::Comment {
                Ok(())
            } else {
                Err(ProtectionDenial::CommentsOnly)
            };
        }
        Some(ProtectionEditMode::Forms) => return Err(ProtectionDenial::FormsUnsupported),
        Some(ProtectionEditMode::TrackedChanges) => {
            return Err(ProtectionDenial::TrackedChangesUnsupported);
        }
        Some(ProtectionEditMode::Unknown(mode)) => {
            return Err(ProtectionDenial::UnsupportedMode(mode.clone()));
        }
        Some(ProtectionEditMode::Unrestricted) | None => {}
    }

    if protection.formatting_locked && mutation == MutationKind::Formatting {
        Err(ProtectionDenial::FormattingLocked)
    } else {
        Ok(())
    }
}
