//! A help manual bundle (#1022): one directory per editor, published from the
//! spec repos, read by every host. See `docs/manual/README.md` for the format.
//!
//! - [`md`] parses the markdown subset the pages are written in.
//! - [`bundle`] loads a manual and reports what is wrong with it.
//! - [`search`] ranks pages for a query.
//! - [`context`] resolves a ribbon command, dialog or area id to a page.
//! - [`coverage`] is the `every_ribbon_command_has_a_help_target` report.

pub mod bundle;
pub mod context;
pub mod coverage;
pub mod md;
pub mod search;

pub use bundle::{Entry, Manual, Page, Problem};
pub use context::{Target, Via, resolve, resolve_area};
pub use coverage::{Coverage, coverage};
pub use search::{Hit, search};

/// The GitHub-style slug of a heading, also used for areas (a ribbon tab's
/// name): lowercase, spaces become `-`, every other character but letters,
/// digits, `-` and `_` is dropped. `Gantt Chart Format` is
/// `gantt-chart-format`; `Header & Footer` is `header--footer`, as GitHub
/// makes it. Idempotent.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c == ' ' {
            out.push('-');
        } else if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Whether `id` is a valid page id: `[a-z0-9-]+`.
pub fn valid_page_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slug("Gantt Chart Format"), "gantt-chart-format");
        assert_eq!(slug("Header & Footer"), "header--footer");
        assert_eq!(slug("What's new?"), "whats-new");
        assert_eq!(slug(&slug("Page Layout")), "page-layout");
    }

    #[test]
    fn page_ids_are_lowercase_words() {
        assert!(valid_page_id("home-font2"));
        assert!(!valid_page_id("Home"));
        assert!(!valid_page_id("a_b"));
        assert!(!valid_page_id(""));
    }
}
