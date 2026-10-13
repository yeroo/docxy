//! Acting on the test instance's window from outside the process (#1226).
//!
//! [`request_close`] asks the OS to close the suite's window the way its X,
//! Alt+F4 or the window manager would, so gpui's own should-close callback
//! runs the suite's close handler. A harness verb cannot do this: it runs
//! inside the app's update, where gpui's callback cannot borrow the app and
//! falls back to closing without asking the handler at all.

/// Ask the OS to close `pid`'s top-level window, as its close button does.
/// Returns once the request is posted; the app decides, so a caller waits for
/// the outcome (the process exiting, or a question it asks).
///
/// ⚠️ Windows: the window is found with `EnumWindows`, which sees only the
/// calling thread's desktop. A child started with
/// [`crate::launch::launch_on_desktop`] needs
/// [`crate::desktop::Desktop::attach_current_thread`] first.
#[cfg(windows)]
pub fn request_close(pid: u32) -> Result<(), String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

    let hwnd = crate::capture::window_of_pid(pid)?;
    // SAFETY: posting a message to a window handle that may since have gone
    // fails cleanly; it never touches memory of ours.
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }
        .map_err(|e| format!("PostMessageW(WM_CLOSE) to process {pid}'s window: {e}"))
}

/// Ask the OS to close `pid`'s top-level window, as its close button does:
/// a `WM_DELETE_WINDOW` message sent to the client window. gpui reads it
/// itself, so the window manager plays no part in delivering it.
#[cfg(target_os = "linux")]
pub fn request_close(pid: u32) -> Result<(), String> {
    use crate::capture::linux::{atom, client_of_pid, connect};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, EventMask};

    let fail = |e: &dyn std::fmt::Display| format!("X11 close of process {pid}'s window: {e}");
    // The lookup is capture's, whose errors name the capture: a failed close
    // says it is the close.
    let lookup = |e: String| fail(&e.strip_prefix("X11 capture: ").unwrap_or(&e));
    let conn = connect().map_err(lookup)?;
    let (window, _, _) = client_of_pid(&conn, pid).map_err(lookup)?;
    let protocols = atom(&conn, b"WM_PROTOCOLS").map_err(lookup)?;
    let delete = atom(&conn, b"WM_DELETE_WINDOW").map_err(lookup)?;
    let event = delete_window_event(window, protocols, delete);
    conn.send_event(false, window, EventMask::NO_EVENT, event)
        .map_err(|e| fail(&e))?
        .check()
        .map_err(|e| fail(&e))?;
    conn.flush().map_err(|e| fail(&e))
}

/// Not yet on macOS: the window belongs to another process there, and no
/// macOS machine has driven it.
#[cfg(not(any(windows, target_os = "linux")))]
pub fn request_close(_pid: u32) -> Result<(), String> {
    Err("request_close: closing the window through the OS is not supported on macOS".to_string())
}

/// The ICCCM `WM_DELETE_WINDOW` client message for `window`: the event a
/// window manager sends when its close button is pressed.
#[cfg(target_os = "linux")]
fn delete_window_event(
    window: x11rb::protocol::xproto::Window,
    protocols: u32,
    delete: u32,
) -> x11rb::protocol::xproto::ClientMessageEvent {
    x11rb::protocol::xproto::ClientMessageEvent::new(
        32,
        window,
        protocols,
        [delete, x11rb::CURRENT_TIME, 0, 0, 0],
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_close_message_is_a_wm_protocols_delete_window_to_the_client() {
        let event = delete_window_event(0x4a0_0001, 101, 202);
        assert_eq!(event.format, 32);
        assert_eq!(event.window, 0x4a0_0001);
        assert_eq!(event.type_, 101);
        assert_eq!(event.data.as_data32(), [202, x11rb::CURRENT_TIME, 0, 0, 0]);
    }
}
