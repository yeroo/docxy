//! The suite's native dialogs answered, and its window clicked, from outside
//! the process (#2195).
//!
//! On Windows the suite shows Open's `IFileDialog` from a foreground task
//! (`PickRoute::Deferred`, #2192). While it is up gpui runs no other task,
//! so the control pump answers no verb until the dialog closes: the harness
//! cannot answer it through the app. [`answer`] finds the dialog by window
//! enumeration (a visible top-level `#32770` of the suite's process, owned by
//! a window of that process: `set_parent` makes the suite window its owner),
//! holds it up for [`HOLD`] so a ticker armed in the suite (`arm-task`) comes
//! due while it is shown, then cancels it or types a path into its File name
//! box and presses OK, and waits for it to go.
//!
//! [`post_click`] is the other half: a harness verb's pointer input is
//! dispatched by the control pump, from inside a foreground task, where
//! gpui_windows holds `wake_posted` set and no other task can run nested in
//! a modal loop. #2192's crash needs a listener that runs from a real window
//! message instead, with the App borrowed and no wake pending. So the click
//! is posted to the suite's window as `WM_MOUSEMOVE`, `WM_LBUTTONDOWN` and
//! `WM_LBUTTONUP`, which gpui_windows turns into the same mouse events a real
//! pointer makes (`handle_mouse_down_msg` reads the position from `lparam`).
//! `SendInput` would not do: the sweep's suite runs on a desktop that is
//! never the input desktop.
//!
//! Like `window.rs` (#1226) the window is found with `EnumWindows`, which
//! sees only the calling thread's desktop; `uiharness run --desktop` attaches
//! the main thread, which runs the steps.
//!
//! The decisions are pure functions over enumerated window records, tested
//! on every OS; only the Win32 calls are Windows-only.

use std::time::Duration;

/// How long a found dialog is left up before it is answered: the Common
/// Item Dialog builds its children after it shows, and a ticker armed at
/// 50 ms comes due many times over meanwhile.
pub const HOLD: Duration = Duration::from_millis(1500);
/// How long to wait for the dialog to appear, and then to go.
pub const WAIT: Duration = Duration::from_secs(20);
/// How often the windows are enumerated while waiting.
pub const POLL: Duration = Duration::from_millis(50);
/// How long a posted Cancel or OK is given before the fallback is tried.
pub const FALLBACK_AFTER: Duration = Duration::from_secs(5);

/// The dialog class every Win32 dialog box has, the Common Item Dialog too.
pub const DIALOG_CLASS: &str = "#32770";
/// The Vista-style dialog's File name box (`cmb13`), a `ComboBoxEx32`.
pub const FILE_NAME_COMBO: i32 = 0x47C;
/// `MK_LBUTTON`: the left button is down (wparam of a mouse message).
pub const MK_LBUTTON: usize = 0x0001;

/// One top-level window, as enumerated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub class: String,
    /// The process of its owner window, if it has one.
    pub owner_pid: Option<u32>,
    pub visible: bool,
}

/// The suite's own dialog among `windows`: visible, of [`DIALOG_CLASS`], in
/// process `pid` and owned by a window of that process. The first one, in
/// enumeration (z) order, is the topmost.
pub fn pick_dialog(windows: &[TopWindow], pid: u32) -> Option<isize> {
    windows
        .iter()
        .find(|w| w.visible && w.pid == pid && w.class == DIALOG_CLASS && w.owner_pid == Some(pid))
        .map(|w| w.hwnd)
}

/// What a step that found no dialog saw of `pid`'s windows, for its error.
pub fn describe_windows(windows: &[TopWindow], pid: u32) -> String {
    let mine: Vec<String> = windows
        .iter()
        .filter(|w| w.pid == pid)
        .map(|w| {
            format!(
                "{:#x} {}{}{}",
                w.hwnd,
                w.class,
                if w.visible { "" } else { " (hidden)" },
                match w.owner_pid {
                    Some(o) if o == pid => " owned".to_string(),
                    Some(o) => format!(" owned by process {o}"),
                    None => String::new(),
                }
            )
        })
        .collect();
    if mine.is_empty() {
        format!("process {pid} has no top-level window on this desktop")
    } else {
        format!("process {pid}'s top-level windows: {}", mine.join(", "))
    }
}

/// A mouse message's `lparam`: x in the low word, y in the high one, both
/// signed 16-bit client coordinates (`MAKELPARAM`).
pub fn mouse_lparam(x: i32, y: i32) -> isize {
    let lo = (x as i16 as u16) as u32;
    let hi = (y as i16 as u16) as u32;
    ((hi << 16) | lo) as i32 as isize
}

/// The centre of a rectangle given as origin and size, in whole pixels.
pub fn centre(x: i32, y: i32, w: u32, h: u32) -> (i32, i32) {
    (x + (w / 2) as i32, y + (h / 2) as i32)
}

/// Whether a client point lies inside a client area of `w` by `h`: a click
/// outside it would land on nothing of the app's.
pub fn inside_client(x: i32, y: i32, w: i32, h: i32) -> bool {
    (0..w).contains(&x) && (0..h).contains(&y)
}

/// Find `pid`'s dialog, hold it, answer it (Cancel, or `path` typed into its
/// File name box and OK), and wait until it is gone. Returns what happened,
/// for the step's report.
#[cfg(windows)]
pub fn answer(pid: u32, path: Option<&std::path::Path>) -> Result<String, String> {
    win::answer(pid, path)
}

/// Off Windows there is no such dialog to answer.
#[cfg(not(windows))]
pub fn answer(_pid: u32, _path: Option<&std::path::Path>) -> Result<String, String> {
    Err("native-dialog is Windows-only".to_string())
}

/// Post a left click at the desktop point (`x`, `y`) to `pid`'s window:
/// a move there, `between()` (the caller's pacing: it waits for a frame,
/// which the press does not need, as gpui hit-tests each mouse event), then
/// the press and the release.
#[cfg(windows)]
pub fn post_click(
    pid: u32,
    x: i32,
    y: i32,
    between: &mut dyn FnMut() -> Result<(), String>,
) -> Result<String, String> {
    win::post_click(pid, x, y, between)
}

/// Off Windows the click is not posted; say so.
#[cfg(not(windows))]
pub fn post_click(
    _pid: u32,
    _x: i32,
    _y: i32,
    _between: &mut dyn FnMut() -> Result<(), String>,
) -> Result<String, String> {
    Err("window click is Windows-only".to_string())
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::time::Instant;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE, WPARAM};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::WindowsAndMessaging::{
        BM_CLICK, EnumChildWindows, EnumWindows, GW_OWNER, GetClassNameW, GetClientRect,
        GetDlgCtrlID, GetDlgItem, GetWindow, GetWindowThreadProcessId, IDCANCEL, IDOK, IsWindow,
        IsWindowVisible, PostMessageW, SendMessageW, WM_CLOSE, WM_COMMAND, WM_GETTEXT,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETTEXT,
    };
    use windows::core::BOOL;

    fn class_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: the buffer is ours and its length is passed with it.
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    fn pid_of(hwnd: HWND) -> u32 {
        let mut pid = 0u32;
        // SAFETY: a stale handle makes this fail, leaving `pid` 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        pid
    }

    unsafe extern "system" fn top_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Vec` `top_windows` passes, alive for the
        // whole enumeration.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<TopWindow>) };
        let owner = unsafe { GetWindow(hwnd, GW_OWNER) }
            .ok()
            .filter(|h| !h.0.is_null());
        out.push(TopWindow {
            hwnd: hwnd.0 as isize,
            pid: pid_of(hwnd),
            class: class_of(hwnd),
            owner_pid: owner.map(pid_of),
            visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        });
        TRUE
    }

    /// Every top-level window on this thread's desktop, in z order.
    fn top_windows() -> Vec<TopWindow> {
        let mut out: Vec<TopWindow> = Vec::new();
        // SAFETY: the callback only writes through the pointer to `out`.
        unsafe {
            let _ = EnumWindows(Some(top_proc), LPARAM(&mut out as *mut _ as isize));
        }
        out
    }

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as *mut core::ffi::c_void)
    }

    fn is_window(h: HWND) -> bool {
        // SAFETY: `IsWindow` only tests the handle.
        unsafe { IsWindow(Some(h)) }.as_bool()
    }

    struct Child {
        hwnd: HWND,
        class: String,
        id: i32,
        visible: bool,
    }

    unsafe extern "system" fn child_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: as `top_proc`.
        let out = unsafe { &mut *(lparam.0 as *mut Vec<Child>) };
        out.push(Child {
            hwnd,
            class: class_of(hwnd),
            id: unsafe { GetDlgCtrlID(hwnd) },
            visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        });
        TRUE
    }

    /// Every descendant of `parent`.
    fn children(parent: HWND) -> Vec<Child> {
        let mut out: Vec<Child> = Vec::new();
        // SAFETY: as `top_windows`.
        unsafe {
            let _ = EnumChildWindows(
                Some(parent),
                Some(child_proc),
                LPARAM(&mut out as *mut _ as isize),
            );
        }
        out
    }

    fn describe_children(dlg: HWND) -> String {
        let kids: Vec<String> = children(dlg)
            .iter()
            .map(|c| {
                format!(
                    "{}#{:#x}{}",
                    c.class,
                    c.id,
                    if c.visible { "" } else { "(hidden)" }
                )
            })
            .collect();
        format!("the dialog's controls: {}", kids.join(" "))
    }

    /// The File name box's edit: the `Edit` inside [`FILE_NAME_COMBO`], else
    /// the first visible `Edit` in the dialog.
    fn file_name_edit(dlg: HWND) -> Result<HWND, String> {
        // SAFETY: `GetDlgItem` only looks the id up.
        if let Ok(combo) = unsafe { GetDlgItem(Some(dlg), FILE_NAME_COMBO) }
            && let Some(edit) = children(combo).into_iter().find(|c| c.class == "Edit")
        {
            return Ok(edit.hwnd);
        }
        children(dlg)
            .into_iter()
            .find(|c| c.class == "Edit" && c.visible)
            .map(|c| c.hwnd)
            .ok_or_else(|| format!("no File name box: {}", describe_children(dlg)))
    }

    fn text_of(h: HWND) -> String {
        let mut buf = vec![0u16; 4096];
        // SAFETY: the system marshals WM_GETTEXT across processes into our
        // buffer, whose length in characters is the wparam.
        let n = unsafe {
            SendMessageW(
                h,
                WM_GETTEXT,
                Some(WPARAM(buf.len())),
                Some(LPARAM(buf.as_mut_ptr() as isize)),
            )
        };
        String::from_utf16_lossy(&buf[..(n.0.max(0) as usize).min(buf.len())])
    }

    fn post(h: HWND, msg: u32, wparam: usize, lparam: isize) -> Result<(), String> {
        // SAFETY: posting to a handle that has gone fails cleanly.
        unsafe { PostMessageW(Some(h), msg, WPARAM(wparam), LPARAM(lparam)) }
            .map_err(|e| format!("PostMessageW({msg:#x}): {e}"))
    }

    /// Wait until `dlg` is gone, up to `within`.
    fn gone_within(dlg: HWND, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while is_window(dlg) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
        true
    }

    pub(super) fn answer(pid: u32, path: Option<&std::path::Path>) -> Result<String, String> {
        let started = Instant::now();
        let deadline = started + WAIT;
        let dlg = loop {
            let windows = top_windows();
            if let Some(h) = pick_dialog(&windows, pid) {
                break hwnd(h);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no native dialog of process {pid} appeared within {WAIT:?}; {}",
                    describe_windows(&windows, pid)
                ));
            }
            std::thread::sleep(POLL);
        };
        let found_ms = started.elapsed().as_millis();
        std::thread::sleep(HOLD);
        if !is_window(dlg) {
            return Err(format!(
                "the dialog ({:#x}) went by itself during the {HOLD:?} hold",
                dlg.0 as isize
            ));
        }
        let what = match path {
            None => {
                post(dlg, WM_COMMAND, IDCANCEL.0 as usize, 0)?;
                if !gone_within(dlg, FALLBACK_AFTER) {
                    post(dlg, WM_CLOSE, 0, 0)?;
                }
                "cancelled".to_string()
            }
            Some(path) => {
                let edit = file_name_edit(dlg)?;
                let wide: Vec<u16> = path
                    .as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                // SAFETY: `wide` is NUL-terminated and outlives the call; the
                // system marshals WM_SETTEXT's string across processes.
                unsafe {
                    SendMessageW(
                        edit,
                        WM_SETTEXT,
                        Some(WPARAM(0)),
                        Some(LPARAM(wide.as_ptr() as isize)),
                    );
                }
                let want = path.to_string_lossy();
                let got = text_of(edit);
                if got != want {
                    return Err(format!(
                        "the File name box holds '{got}' after setting '{want}'"
                    ));
                }
                // SAFETY: as `file_name_edit`.
                let ok = unsafe { GetDlgItem(Some(dlg), IDOK.0) }.ok();
                let ok_raw = ok.map_or(0, |h| h.0 as isize);
                // `MAKEWPARAM(IDOK, BN_CLICKED)`: BN_CLICKED is 0.
                post(dlg, WM_COMMAND, IDOK.0 as usize, ok_raw)?;
                if !gone_within(dlg, FALLBACK_AFTER)
                    && let Some(ok) = ok
                {
                    post(ok, BM_CLICK, 0, 0)?;
                }
                format!("opened {want}")
            }
        };
        if !gone_within(dlg, WAIT) {
            let windows = top_windows();
            return Err(format!(
                "the dialog ({:#x}) is still up {WAIT:?} after it was {what} \
                 (it may be showing a message box of its own); {}; {}",
                dlg.0 as isize,
                describe_windows(&windows, pid),
                describe_children(dlg)
            ));
        }
        Ok(format!(
            "dialog {:#x} found after {found_ms} ms, held {HOLD:?}, {what}",
            dlg.0 as isize
        ))
    }

    pub(super) fn post_click(
        pid: u32,
        x: i32,
        y: i32,
        between: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<String, String> {
        // The rect is in real pixels; so must this process's view of them be.
        crate::capture::become_dpi_aware();
        let win = crate::capture::window_of_pid(pid)?;
        let mut p = POINT { x, y };
        // SAFETY: `p` is ours; a stale handle makes the call fail.
        if !unsafe { ScreenToClient(win, &mut p) }.as_bool() {
            return Err(format!("ScreenToClient of process {pid}'s window failed"));
        }
        let mut client = RECT::default();
        // SAFETY: as above.
        unsafe { GetClientRect(win, &mut client) }
            .map_err(|e| format!("GetClientRect of process {pid}'s window: {e}"))?;
        let (cw, ch) = (client.right - client.left, client.bottom - client.top);
        if !inside_client(p.x, p.y, cw, ch) {
            return Err(format!(
                "the point ({x},{y}) is ({},{}) in the window's {cw}x{ch} client area: outside it",
                p.x, p.y
            ));
        }
        let at = mouse_lparam(p.x, p.y);
        post(win, WM_MOUSEMOVE, 0, at)?;
        between()?;
        post(win, WM_LBUTTONDOWN, MK_LBUTTON, at)?;
        post(win, WM_LBUTTONUP, 0, at)?;
        Ok(format!(
            "posted a click at client ({},{}) of window {:#x}",
            p.x, p.y, win.0 as isize
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(hwnd: isize, pid: u32, class: &str, owner_pid: Option<u32>, visible: bool) -> TopWindow {
        TopWindow {
            hwnd,
            pid,
            class: class.to_string(),
            owner_pid,
            visible,
        }
    }

    /// Only a visible `#32770` of the suite's process, owned by one of its
    /// windows, is the suite's dialog; the topmost such one wins.
    #[test]
    fn the_dialog_is_the_suites_own_visible_owned_32770() {
        let pid = 42;
        let windows = [
            // Another process's dialog, owned by our window.
            w(1, 7, DIALOG_CLASS, Some(pid), true),
            // The suite's main window.
            w(2, pid, "Zed::Window", None, true),
            // An unowned dialog of ours.
            w(3, pid, DIALOG_CLASS, None, true),
            // A hidden one.
            w(4, pid, DIALOG_CLASS, Some(pid), false),
            // Owned by another process's window.
            w(5, pid, DIALOG_CLASS, Some(7), true),
            // The one.
            w(6, pid, DIALOG_CLASS, Some(pid), true),
            w(8, pid, DIALOG_CLASS, Some(pid), true),
        ];
        assert_eq!(pick_dialog(&windows, pid), Some(6));
        assert_eq!(pick_dialog(&windows[..5], pid), None);
        assert_eq!(pick_dialog(&[], pid), None);
    }

    #[test]
    fn a_missing_dialog_lists_the_processes_windows() {
        let windows = [
            w(0x10, 42, "Zed::Window", None, true),
            w(0x20, 42, DIALOG_CLASS, Some(7), false),
            w(0x30, 7, "Other", None, true),
        ];
        let d = describe_windows(&windows, 42);
        assert!(d.contains("0x10 Zed::Window"), "{d}");
        assert!(d.contains("0x20 #32770 (hidden) owned by process 7"), "{d}");
        assert!(!d.contains("Other"), "{d}");
        assert!(describe_windows(&windows, 9).contains("no top-level window"));
    }

    /// `MAKELPARAM` packs x low and y high, each as a signed 16-bit word.
    #[test]
    fn a_mouse_lparam_packs_client_coordinates() {
        assert_eq!(mouse_lparam(0, 0), 0);
        assert_eq!(mouse_lparam(10, 20), (20 << 16) | 10);
        assert_eq!(mouse_lparam(0xFFFF, 0) & 0xFFFF, 0xFFFF);
        // gpui reads them back with `signed_loword`/`signed_hiword`.
        let l = mouse_lparam(-3, 700);
        assert_eq!((l & 0xFFFF) as u16 as i16, -3);
        assert_eq!(((l >> 16) & 0xFFFF) as u16 as i16, 700);
    }

    #[test]
    fn the_click_lands_on_the_rects_centre_inside_the_client_area() {
        assert_eq!(centre(100, 40, 80, 30), (140, 55));
        assert_eq!(centre(0, 0, 1, 1), (0, 0));
        assert!(inside_client(0, 0, 800, 600));
        assert!(inside_client(799, 599, 800, 600));
        assert!(!inside_client(800, 10, 800, 600));
        assert!(!inside_client(-1, 10, 800, 600));
    }

    /// Off Windows neither half runs: the step says so instead of failing
    /// somewhere less clear.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_both_say_they_are_windows_only() {
        assert!(answer(1, None).unwrap_err().contains("Windows-only"));
        let e = post_click(1, 0, 0, &mut || Ok(())).unwrap_err();
        assert!(e.contains("Windows-only"), "{e}");
    }
}
