//! Run with scripts/ui-linux.py -- cargo test -p uiharness --test linux_capture -- --ignored
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};
use uiharness::capture::{How, capture_pid};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::wrapper::ConnectionExt as _;

fn wait<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "X11 condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires an isolated X11 display with Openbox; use scripts/ui-linux.py"]
fn captures_owned_client_pixels_and_geometry_and_refuses_unusable_windows() {
    let (conn, screen_idx) = x11rb::connect(None).unwrap();
    let screen = &conn.setup().roots[screen_idx];
    assert_eq!(
        screen.root_depth, 24,
        "this fixture uses a 24-bit RGB visual"
    );
    let window = conn.generate_id().unwrap();
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        100,
        100,
        240,
        160,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().background_pixel(0xff0000),
    )
    .unwrap()
    .check()
    .unwrap();
    let pid = std::process::id();
    let atom = conn
        .intern_atom(false, b"_NET_WM_PID")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    conn.change_property32(PropMode::REPLACE, window, atom, AtomEnum::CARDINAL, &[pid])
        .unwrap()
        .check()
        .unwrap();
    conn.map_window(window).unwrap().check().unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, window, &CreateGCAux::new().foreground(0x00ff00))
        .unwrap()
        .check()
        .unwrap();
    let paint = || {
        conn.poly_fill_rectangle(
            window,
            gc,
            &[Rectangle {
                x: 30,
                y: 20,
                width: 40,
                height: 30,
            }],
        )
        .unwrap()
        .check()
        .unwrap();
    };
    let first = wait(|| {
        paint();
        capture_pid(pid).ok()
    });
    assert_eq!(first.how, How::X11);
    assert_eq!((first.image.w, first.image.h), (240, 160));
    assert_eq!(first.image.pixel(0, 0), Some([255, 0, 0, 255]));
    assert_eq!(first.image.pixel(35, 25), Some([0, 255, 0, 255]));
    let translated = conn
        .translate_coordinates(window, screen.root, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        first.origin,
        (i32::from(translated.dst_x), i32::from(translated.dst_y))
    );

    conn.configure_window(
        window,
        &ConfigureWindowAux::new()
            .x(180)
            .y(170)
            .width(300)
            .height(200),
    )
    .unwrap()
    .check()
    .unwrap();
    let resized = wait(|| {
        paint();
        capture_pid(pid)
            .ok()
            .filter(|c| c.image.w == 300 && c.image.h == 200 && c.origin != first.origin)
    });
    let translated = conn
        .translate_coordinates(window, screen.root, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        resized.origin,
        (i32::from(translated.dst_x), i32::from(translated.dst_y))
    );
    assert_eq!(resized.image.pixel(35, 25), Some([0, 255, 0, 255]));
    // Repainting proves the next capture reads current pixels, not an old image.
    conn.change_gc(gc, &ChangeGCAux::new().foreground(0x0000ff))
        .unwrap()
        .check()
        .unwrap();
    paint();
    assert_eq!(
        capture_pid(pid).unwrap().image.pixel(35, 25),
        Some([0, 0, 255, 255])
    );
    assert!(
        capture_pid(u32::MAX)
            .err()
            .unwrap()
            .contains("no mapped X11 client")
    );

    // An unmanaged foreign overlay must cause refusal, not contaminated evidence.
    let overlay = conn.generate_id().unwrap();
    conn.create_window(
        screen.root_depth,
        overlay,
        screen.root,
        (resized.origin.0 + 5) as i16,
        (resized.origin.1 + 5) as i16,
        30,
        30,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(0xffffff),
    )
    .unwrap()
    .check()
    .unwrap();
    conn.map_window(overlay).unwrap().check().unwrap();
    assert!(capture_pid(pid).err().unwrap().contains("obscured"));
    conn.destroy_window(overlay).unwrap().check().unwrap();
    paint();
    assert!(capture_pid(pid).is_ok());
    conn.unmap_window(window).unwrap().check().unwrap();
    assert!(
        capture_pid(pid)
            .err()
            .unwrap()
            .contains("no mapped X11 client")
    );
}
