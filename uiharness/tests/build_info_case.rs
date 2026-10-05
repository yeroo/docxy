//! `cases/build-info.uit` (#1023) must parse under the script grammar, so a typo
//! in it fails here, without a display, and not only in the sweep.

use uiharness::script::parse_script;

#[test]
fn the_build_info_case_parses() {
    let script = parse_script(include_str!("../cases/build-info.uit")).unwrap();
    assert_eq!(script.cases.len(), 2);
    assert!(
        script.cases[0].name.contains("app-info"),
        "{:?}",
        script.cases[0].name
    );
}
