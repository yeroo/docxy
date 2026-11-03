//! The `every_ribbon_command_has_a_help_target` report: how each command id
//! resolves. It stays a report until a manual marks an area strict
//! (`# strict: home` at the top of its contents); from then on a command of
//! that area without a page of its own is a failure.

use crate::{Manual, Via, resolve, slug};
use std::fmt::Write;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    /// `(id, area)` pairs with a page of their own (area as a slug).
    pub exact: Vec<(String, String)>,
    /// Resolved through their area's page.
    pub by_area: Vec<(String, String)>,
    /// Resolved only to the manual's first page.
    pub by_root: Vec<(String, String)>,
    /// No page at all (a manual without pages).
    pub unresolved: Vec<(String, String)>,
    /// Ids of a strict area without a page of their own.
    pub strict_failures: Vec<(String, String)>,
    /// Strict areas no id belongs to (a misspelt or renamed tab).
    pub unused_strict: Vec<String>,
}

impl Coverage {
    /// Nothing unresolved, no strict failure and no unused strict area.
    pub fn passes(&self) -> bool {
        self.unresolved.is_empty()
            && self.strict_failures.is_empty()
            && self.unused_strict.is_empty()
    }

    /// A count line, then one line per id not exactly covered.
    pub fn report(&self) -> String {
        let mut out = format!(
            "{} exact, {} by area, {} by root, {} unresolved, {} strict failures\n",
            self.exact.len(),
            self.by_area.len(),
            self.by_root.len(),
            self.unresolved.len(),
            self.strict_failures.len()
        );
        for area in &self.unused_strict {
            let _ = writeln!(out, "  unused strict area: {area}");
        }
        for (label, list) in [
            ("strict failure", &self.strict_failures),
            ("unresolved", &self.unresolved),
            ("by root", &self.by_root),
            ("by area", &self.by_area),
        ] {
            for (id, area) in list {
                let _ = writeln!(out, "  {label}: {id} ({area})");
            }
        }
        out
    }
}

/// Resolve every `(id, area)` pair, the area as the ribbon names its tab. A
/// pair given twice counts once.
pub fn coverage<'a>(
    manual: &Manual,
    ids: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Coverage {
    let mut c = Coverage::default();
    let mut seen: Vec<(String, String)> = Vec::new();
    for (id, area) in ids {
        let pair = (id.to_string(), slug(area));
        if seen.contains(&pair) {
            continue;
        }
        seen.push(pair.clone());
        let via = resolve(manual, id, Some(area)).map(|t| t.via);
        if via != Some(Via::Exact) && manual.strict.contains(&pair.1) {
            c.strict_failures.push(pair.clone());
        }
        match via {
            Some(Via::Exact) => c.exact.push(pair),
            Some(Via::Area) => c.by_area.push(pair),
            Some(Via::Root) => c.by_root.push(pair),
            None => c.unresolved.push(pair),
        }
    }
    c.unused_strict = manual
        .strict
        .iter()
        .filter(|a| !seen.iter().any(|(_, area)| area == *a))
        .cloned()
        .collect();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual(contents: &str) -> Manual {
        Manual::from_files([
            ("contents.txt", contents),
            ("root.md", ""),
            ("home.md", "---\nkeys: bold\n---\n"),
        ])
    }

    fn pair(id: &str, area: &str) -> (String, String) {
        (id.into(), area.into())
    }

    #[test]
    fn ids_are_sorted_by_how_they_resolve() {
        let m = manual("root\nhome\n");
        let c = coverage(
            &m,
            [
                ("bold", "Home"),
                ("italic", "Home"),
                ("table", "Insert"),
                ("bold", "Home"),
            ],
        );
        assert_eq!(c.exact, vec![pair("bold", "home")]);
        assert_eq!(c.by_area, vec![pair("italic", "home")]);
        assert_eq!(c.by_root, vec![pair("table", "insert")]);
        assert!(c.passes());
        assert!(c.report().contains("by root: table (insert)"));
    }

    #[test]
    fn a_strict_area_fails_on_a_fallback() {
        let m = manual("# strict: Home\nroot\nhome\n");
        let c = coverage(
            &m,
            [("bold", "Home"), ("italic", "Home"), ("table", "Insert")],
        );
        assert_eq!(c.strict_failures, vec![pair("italic", "home")]);
        assert!(!c.passes());
        assert!(c.report().contains("strict failure: italic (home)"));
        // Every Home id with a page of its own passes.
        assert!(coverage(&m, [("bold", "Home"), ("table", "Insert")]).passes());
    }

    #[test]
    fn a_strict_area_no_id_uses_fails() {
        let m = manual("# strict: Hom\nroot\nhome\n");
        let c = coverage(&m, [("bold", "Home")]);
        assert_eq!(c.unused_strict, vec!["hom"]);
        assert!(!c.passes());
        assert!(c.report().contains("unused strict area: hom"));
    }

    #[test]
    fn an_empty_manual_resolves_nothing() {
        let c = coverage(&Manual::default(), [("bold", "Home")]);
        assert_eq!(c.unresolved, vec![pair("bold", "home")]);
        assert!(!c.passes());
    }
}
