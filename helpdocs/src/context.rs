//! Context help: a ribbon command id (`bold`), a dialog id (`dialog:font`) or
//! an area (a ribbon tab name, slugified) to a page. It never fails on a
//! manual with pages: the exact key, else the area's page, else the root.

use crate::{Manual, slug};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A page lists the id in its `keys`.
    Exact,
    /// The area's page: the page whose id is the area, else the first page
    /// (in contents order) whose `area` is it.
    Area,
    /// The manual's first page.
    Root,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub page: String,
    pub via: Via,
}

/// The page for `id` in `area` (a tab name or its slug). `None` only for a
/// manual without pages.
pub fn resolve(manual: &Manual, id: &str, area: Option<&str>) -> Option<Target> {
    let hit = |page: &str, via| {
        Some(Target {
            page: page.to_string(),
            via,
        })
    };
    // Pages are in contents order, so the first claim wins.
    if let Some(p) = manual.pages.iter().find(|p| p.keys.iter().any(|k| k == id)) {
        return hit(&p.id, Via::Exact);
    }
    resolve_area(manual, area)
}

/// The page for `area` itself (F1 with no command: #1855), never a key's: the
/// area's page, else the root. `None` only for a manual without pages.
pub fn resolve_area(manual: &Manual, area: Option<&str>) -> Option<Target> {
    let hit = |page: &str, via| {
        Some(Target {
            page: page.to_string(),
            via,
        })
    };
    if let Some(area) = area.map(slug).filter(|a| !a.is_empty()) {
        if let Some(p) = manual.page(&area) {
            return hit(&p.id, Via::Area);
        }
        if let Some(p) = manual
            .pages
            .iter()
            .find(|p| p.area.as_deref() == Some(area.as_str()))
        {
            return hit(&p.id, Via::Area);
        }
    }
    manual.pages.first().and_then(|p| hit(&p.id, Via::Root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::fixture;

    fn at(page: &str, via: Via) -> Option<Target> {
        Some(Target {
            page: page.into(),
            via,
        })
    }

    #[test]
    fn an_exact_key_wins() {
        let m = fixture("word");
        assert_eq!(
            resolve(&m, "bold", Some("Home")),
            at("home-font", Via::Exact)
        );
        // Even from another area.
        assert_eq!(
            resolve(&m, "bold", Some("Insert")),
            at("home-font", Via::Exact)
        );
    }

    #[test]
    fn a_dialog_key() {
        let m = fixture("word");
        assert_eq!(
            resolve(&m, "dialog:font", None),
            at("home-font", Via::Exact)
        );
        assert_eq!(
            resolve(&m, "dialog:nothing", None),
            at("welcome", Via::Root)
        );
    }

    #[test]
    fn an_unknown_command_falls_back_to_its_area() {
        let m = fixture("word");
        assert_eq!(
            resolve(&m, "no-such-cmd", Some("Home")),
            at("home", Via::Area)
        );
        assert_eq!(
            resolve(&m, "no-such-cmd", Some("insert")),
            at("insert", Via::Area)
        );
        // An area named only in a page's front matter.
        assert_eq!(
            resolve(&m, "no-such-cmd", Some("Keys")),
            at("keyboard", Via::Area)
        );
    }

    #[test]
    fn an_area_alone_never_matches_a_key() {
        let mut m = fixture("word");
        // A page claiming the area's slug as a key does not win over the
        // area's own page.
        m.pages
            .iter_mut()
            .find(|p| p.id == "keyboard")
            .unwrap()
            .keys
            .push("home".into());
        assert_eq!(
            resolve(&m, "home", Some("Home")),
            at("keyboard", Via::Exact)
        );
        assert_eq!(resolve_area(&m, Some("Home")), at("home", Via::Area));
        assert_eq!(resolve_area(&m, Some("Keys")), at("keyboard", Via::Area));
        assert_eq!(resolve_area(&m, Some("Mailings")), at("welcome", Via::Root));
        assert_eq!(resolve_area(&m, None), at("welcome", Via::Root));
        assert_eq!(resolve_area(&Manual::default(), Some("Home")), None);
    }

    #[test]
    fn an_unknown_area_falls_back_to_the_root() {
        let m = fixture("word");
        assert_eq!(
            resolve(&m, "no-such-cmd", Some("Mailings")),
            at("welcome", Via::Root)
        );
        assert_eq!(resolve(&m, "no-such-cmd", None), at("welcome", Via::Root));
        assert_eq!(resolve(&Manual::default(), "bold", None), None);
    }
}
