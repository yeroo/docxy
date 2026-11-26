//! The library the fixture plugin imports in its `needs-dependency` build.

#[unsafe(no_mangle)]
pub extern "C" fn fixture_needed_value() -> u32 {
    7
}
