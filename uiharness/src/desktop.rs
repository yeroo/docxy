//! A named Win32 desktop the suite can run on, never switched in — the
//! opt-in answer to the suite's window taking the user's keyboard mid-pass
//! (#722). The desktop-launch path lives in [`crate::launch`].

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_desktop_name_must_be_non_empty_and_have_no_backslash() {
        assert!(valid_desktop_name("").is_err());
        assert!(valid_desktop_name(r"WinSta0\x").is_err());
        assert!(valid_desktop_name("uiharness").is_ok());
    }
}
