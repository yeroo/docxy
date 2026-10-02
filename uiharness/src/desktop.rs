//! A named Win32 desktop the suite can run on, never switched in — the
//! opt-in answer to the suite's window taking the user's keyboard mid-pass
//! (#722). [`Desktop::create`] makes (or opens) it,
//! [`crate::launch::launch_on_desktop`]
//! starts the suite there, and a capture thread reaches the window through
//! [`Desktop::attach_current_thread`]. Off Windows the type still exists so
//! callers compile, but creating one is an error.

#[cfg(windows)]
use windows::Win32::System::StationsAndDesktops::HDESK;

/// A Win32 desktop on this session's window station — `WinSta0\<name>` in
/// full, though only `name` is ever passed: the `WinSta0\` prefix is formed
/// by Windows, and a [`Desktop`] never leaves this window station.
pub struct Desktop {
    name: String,
    #[cfg(windows)]
    handle: HDESK,
}

// The handle is owned: `Drop` closes it exactly once, and a borrow cannot
// outlive the value it borrows, so the handle is valid for every call made
// through one. `SetThreadDesktop` changes only the calling thread, so the
// wrapper is safe to share and to move to a capture thread.
unsafe impl Send for Desktop {}
unsafe impl Sync for Desktop {}

/// Whether `name` can be handed to `CreateDesktopW`: not empty, and no
/// backslash — the `WinSta0\NAME` form is composed by Windows from the
/// window station and the name, never passed as one string.
pub fn valid_desktop_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a desktop name must not be empty".to_string());
    }
    if name.contains('\\') {
        return Err(format!(
            "a desktop name must not contain a backslash: {name:?} \
             (WinSta0\\NAME is formed by Windows, not passed as one name)"
        ));
    }
    Ok(())
}

impl Desktop {
    /// Create the desktop `WinSta0\<name>`, or open it if it already exists
    /// (a crash may have left one behind; a name carrying the process id, as
    /// the tests use, only narrows that to the same process).
    #[cfg(windows)]
    pub fn create(name: &str) -> Result<Desktop, String> {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Foundation::GENERIC_ALL;
        use windows::Win32::System::StationsAndDesktops::CreateDesktopW;
        use windows::core::PCWSTR;

        valid_desktop_name(name)?;
        let wide: Vec<u16> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is a NUL-terminated desktop name and outlives the
        // call; the other pointers are null. `GENERIC_ALL` is the oracle
        // launcher's access right, passed by the named constant's value.
        let handle = unsafe {
            CreateDesktopW(
                PCWSTR(wide.as_ptr()),
                None,
                None,
                Default::default(),
                GENERIC_ALL.0,
                None,
            )
        }
        .map_err(|e| format!("--desktop {name:?}: CreateDesktopW: {e}"))?;
        Ok(Desktop {
            name: name.to_string(),
            handle,
        })
    }

    /// Off Windows a separate desktop is not a thing; say so where the
    /// option is used rather than silently ignoring it.
    #[cfg(not(windows))]
    pub fn create(name: &str) -> Result<Desktop, String> {
        valid_desktop_name(name)?;
        Err("--desktop: a separate desktop is Windows-only".to_string())
    }

    /// The `name` it was created with, for `STARTUPINFOW.lpDesktop` and for
    /// matching a later `uiharness --desktop NAME` to the desktop an
    /// instance was left on.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Attach the calling thread to this desktop, so desktop-scoped calls —
    /// `EnumWindows` above all — see what is on it. Fails on a thread that
    /// owns a window or a hook, and the error says so: an un-attached
    /// capture reports "no visible top-level window", which looks like a
    /// suite bug when it is only the wrong desktop.
    #[cfg(windows)]
    pub fn attach_current_thread(&self) -> Result<(), String> {
        use windows::Win32::System::StationsAndDesktops::SetThreadDesktop;

        // SAFETY: `handle` is live (see the Send/Sync note above) and is not
        // closed by this call.
        unsafe { SetThreadDesktop(self.handle) }.map_err(|e| {
            format!(
                "--desktop {}: SetThreadDesktop: {e} \
                 (the calling thread must own no windows or hooks)",
                self.name
            )
        })
    }

    /// Off Windows there is nothing to attach to.
    #[cfg(not(windows))]
    pub fn attach_current_thread(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Desktop {
    fn drop(&mut self) {
        use windows::Win32::System::StationsAndDesktops::CloseDesktop;

        // SAFETY: the handle is ours and this `Drop` runs exactly once. The
        // desktop object outlives the close for anything attached to or
        // running on it — callers keep this value alive through the run.
        unsafe {
            let _ = CloseDesktop(self.handle);
        }
    }
}

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
