//! Central DOCX mutation authorization policy.
//!
//! Route adapters deliberately depend on this module instead of interpreting
//! OOXML protection metadata or comparing display labels. The complete route
//! inventory lives in `docs/docx-mutation-inventory.md`; every route selects
//! one of the five semantic mutation kinds below before it changes App state.

use docxcore::package::{Protection, ProtectionEditMode};

/// The semantic effect of a DOCX mutation, independent of the UI/control route
/// that requested it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MutationKind {
    Content,
    Structure,
    Formatting,
    Comment,
    PackageMetadata,
}

/// Why an enforced protection declaration denied a mutation.
///
/// Variants are intentionally more specific than [`MutationKind`]. Their
/// stable codes are part of the control/MCP contract, while their messages are
/// concise enough to show directly in the TUI status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProtectionDenial {
    ReadOnly,
    CommentsOnly,
    FormattingLocked,
    FormsUnsupported,
    TrackedChangesUnsupported,
    UnsupportedMode(String),
}

impl ProtectionDenial {
    /// Stable, machine-readable code for control and MCP callers.
    pub(crate) fn code(&self) -> &'static str {
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
    pub(crate) fn tui_status(&self) -> String {
        format!("Edit blocked: {}.", self.explanation())
    }

    /// Stable control error envelope. The prefix and code can be parsed without
    /// depending on the human-readable explanation after the second colon.
    pub(crate) fn control_error(&self) -> String {
        format!("protection_denied:{}: {}", self.code(), self.explanation())
    }
}

/// Authorize a mutation against structured package protection metadata.
///
/// Recommendation-only `w:writeProtection` is not an enforcement signal, but a
/// password/hash-backed declaration is a real write lock and is treated as
/// read-only until password verification exists. Enforced form, tracked-change,
/// and unknown modes fail closed because docxy cannot currently produce
/// conforming edits for those modes. A formatting lock is evaluated after the
/// edit-mode restriction, so comments remain possible in comments-only
/// documents even when formatting is also locked.
pub(crate) fn authorize(
    protection: &Protection,
    mutation: MutationKind,
) -> Result<(), ProtectionDenial> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::package::ProtectionEnforcement;

    const ALL_MUTATIONS: [MutationKind; 5] = [
        MutationKind::Content,
        MutationKind::Structure,
        MutationKind::Formatting,
        MutationKind::Comment,
        MutationKind::PackageMetadata,
    ];

    fn protected(mode: Option<ProtectionEditMode>, formatting_locked: bool) -> Protection {
        Protection {
            enforcement: ProtectionEnforcement::Enforced,
            edit_mode: mode,
            formatting_locked,
            ..Protection::default()
        }
    }

    fn assert_all_allowed(protection: &Protection) {
        for mutation in ALL_MUTATIONS {
            assert_eq!(authorize(protection, mutation), Ok(()), "{mutation:?}");
        }
    }

    fn assert_all_denied(protection: &Protection, expected: ProtectionDenial) {
        for mutation in ALL_MUTATIONS {
            assert_eq!(
                authorize(protection, mutation),
                Err(expected.clone()),
                "{mutation:?}"
            );
        }
    }

    #[test]
    fn absent_disabled_and_advisory_protection_allow_every_mutation() {
        assert_all_allowed(&Protection::default());

        let disabled = Protection {
            enforcement: ProtectionEnforcement::Disabled,
            edit_mode: Some(ProtectionEditMode::ReadOnly),
            formatting_locked: true,
            ..Protection::default()
        };
        assert_all_allowed(&disabled);

        let advisory = Protection {
            advisory_write_protection: true,
            ..Protection::default()
        };
        assert_all_allowed(&advisory);
    }

    #[test]
    fn read_only_denies_every_mutation_class() {
        assert_all_denied(
            &protected(Some(ProtectionEditMode::ReadOnly), false),
            ProtectionDenial::ReadOnly,
        );
    }

    #[test]
    fn password_backed_write_protection_denies_every_mutation_class() {
        let protection = Protection {
            enforced_write_protection: true,
            ..Protection::default()
        };
        assert_all_denied(&protection, ProtectionDenial::ReadOnly);
    }

    #[test]
    fn comments_mode_allows_only_comment_mutations() {
        let protection = protected(Some(ProtectionEditMode::Comments), true);
        for mutation in ALL_MUTATIONS {
            let expected = if mutation == MutationKind::Comment {
                Ok(())
            } else {
                Err(ProtectionDenial::CommentsOnly)
            };
            assert_eq!(authorize(&protection, mutation), expected, "{mutation:?}");
        }
    }

    #[test]
    fn formatting_only_blocks_formatting_and_allows_other_mutations() {
        for mode in [None, Some(ProtectionEditMode::Unrestricted)] {
            let protection = protected(mode, true);
            for mutation in ALL_MUTATIONS {
                let expected = if mutation == MutationKind::Formatting {
                    Err(ProtectionDenial::FormattingLocked)
                } else {
                    Ok(())
                };
                assert_eq!(authorize(&protection, mutation), expected, "{mutation:?}");
            }
        }
    }

    #[test]
    fn unrestricted_enforcement_without_formatting_lock_allows_every_mutation() {
        assert_all_allowed(&protected(Some(ProtectionEditMode::Unrestricted), false));
        assert_all_allowed(&protected(None, false));
    }

    #[test]
    fn forms_tracked_changes_and_unknown_modes_fail_closed() {
        assert_all_denied(
            &protected(Some(ProtectionEditMode::Forms), false),
            ProtectionDenial::FormsUnsupported,
        );
        assert_all_denied(
            &protected(Some(ProtectionEditMode::TrackedChanges), false),
            ProtectionDenial::TrackedChangesUnsupported,
        );
        assert_all_denied(
            &protected(
                Some(ProtectionEditMode::Unknown("futureMode".to_string())),
                false,
            ),
            ProtectionDenial::UnsupportedMode("futureMode".to_string()),
        );
    }

    #[test]
    fn denial_codes_and_messages_are_stable_and_actionable() {
        let cases = [
            (
                ProtectionDenial::ReadOnly,
                "read_only",
                "protection_denied:read_only: the document is protected read-only",
            ),
            (
                ProtectionDenial::CommentsOnly,
                "comments_only",
                "protection_denied:comments_only: only comment edits are allowed",
            ),
            (
                ProtectionDenial::FormattingLocked,
                "formatting_locked",
                "protection_denied:formatting_locked: document formatting is locked",
            ),
            (
                ProtectionDenial::FormsUnsupported,
                "forms_unsupported",
                "protection_denied:forms_unsupported: form-fields-only editing is not supported; use Word to edit form fields",
            ),
            (
                ProtectionDenial::TrackedChangesUnsupported,
                "tracked_changes_unsupported",
                "protection_denied:tracked_changes_unsupported: tracked-only editing is not supported; use Word to create tracked changes",
            ),
            (
                ProtectionDenial::UnsupportedMode("vendorMode".to_string()),
                "unsupported_mode",
                "protection_denied:unsupported_mode: the document uses unsupported protection mode 'vendorMode'",
            ),
        ];

        for (denial, code, control) in cases {
            assert_eq!(denial.code(), code);
            assert_eq!(denial.control_error(), control);
            let status = denial.tui_status();
            assert!(status.starts_with("Edit blocked: "), "{status}");
            assert!(status.ends_with('.'), "{status}");
        }
    }
}
