//! F1 help (#1856): where this editor's manual comes from. The reader, its
//! keys and its drawing are `backstagecore::help`, shared with the other
//! terminal editors.

#[cfg(not(test))]
use helpdocs::Manual;
#[cfg(not(test))]
use std::sync::Arc;

/// The manual this editor reads (`docs/manual/<editor>/`).
const EDITOR: &str = "project";

/// The Project manual built in from `docs/manual/project/` by `build.rs` (empty
/// until one is published there).
#[cfg_attr(test, allow(dead_code))]
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/manual.rs"));
}

/// The installed manual, else the built-in one, read once per process.
/// Unit tests never read either: they put a manual on the `App`.
#[cfg(not(test))]
pub(crate) fn manual() -> Option<Arc<Manual>> {
    static MANUAL: std::sync::OnceLock<Option<Arc<Manual>>> = std::sync::OnceLock::new();
    MANUAL
        .get_or_init(|| helpdocs::locate::load(EDITOR, embedded::MANUAL).map(Arc::new))
        .clone()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use helpdocs::Manual;
    use std::path::Path;
    use std::sync::Arc;

    /// `helpdocs/tests/fixture/<editor>` as a manual.
    pub(crate) fn fixture(editor: &str) -> Arc<Manual> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../helpdocs/tests/fixture")
            .join(editor);
        Arc::new(Manual::load(&dir).expect("the helpdocs fixture loads"))
    }

    #[test]
    fn this_editors_fixture_is_a_manual() {
        assert!(!fixture(EDITOR).pages.is_empty());
    }
}
