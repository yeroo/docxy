//! X11 client-window capture. Use a private Xvfb + window manager display:
//! GetImage does not define pixels obscured by other windows. We reject
//! overlapping top-level windows and off-screen clients instead of accepting
//! a plausible but incomplete image. Native Wayland windows are not X clients.
use super::{Capture, How, Image, is_blank};
use crate::image::RectPx;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, Format, ImageFormat, ImageOrder, MapState, VisualClass, Visualtype,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;

fn error(e: impl std::fmt::Display) -> String {
    format!("X11 capture: {e}")
}

fn atom(conn: &RustConnection, name: &[u8]) -> Result<u32, String> {
    Ok(conn
        .intern_atom(false, name)
        .map_err(error)?
        .reply()
        .map_err(error)?
        .atom)
}

fn property(
    conn: &RustConnection,
    window: Window,
    atom: u32,
    kind: AtomEnum,
) -> Result<Vec<u32>, String> {
    let reply = conn
        .get_property(false, window, atom, kind, 0, 65_536)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    if reply.bytes_after != 0 {
        return Err(error("window property is too large"));
    }
    Ok(reply.value32().map(|v| v.collect()).unwrap_or_default())
}

fn rect(conn: &RustConnection, window: Window, root: Window) -> Result<RectPx, String> {
    let g = conn
        .get_geometry(window)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    let p = conn
        .translate_coordinates(window, root, 0, 0)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    if !p.same_screen {
        return Err(error("window is on a different X screen"));
    }
    Ok(RectPx::new(
        i32::from(p.dst_x),
        i32::from(p.dst_y),
        u32::from(g.width),
        u32::from(g.height),
    ))
}

fn overlaps(a: RectPx, b: RectPx) -> bool {
    i64::from(a.x) < b.right()
        && i64::from(b.x) < a.right()
        && i64::from(a.y) < b.bottom()
        && i64::from(b.y) < a.bottom()
}

/// Find the reparenting WM's frame, then reject any viewable root sibling
/// above it that overlaps our client. This deliberately errs on refusal for
/// shaped/transparent overlays: a false refusal is better than wrong pixels.
fn unobscured(
    conn: &RustConnection,
    window: Window,
    root: Window,
    wanted: RectPx,
) -> Result<(), String> {
    let mut frame = window;
    let mut found_root = false;
    for _ in 0..64 {
        let tree = conn
            .query_tree(frame)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if tree.parent == root {
            found_root = true;
            break;
        }
        if tree.parent == 0 || tree.parent == frame {
            break;
        }
        frame = tree.parent;
    }
    if !found_root {
        return Err(error("could not locate the window's root ancestor"));
    }
    let tree = conn
        .query_tree(root)
        .map_err(error)?
        .reply()
        .map_err(error)?;
    let index = tree
        .children
        .iter()
        .position(|w| *w == frame)
        .ok_or_else(|| error("window was reparented during capture"))?;
    // QueryTree's children are in bottom-to-top stacking order.
    for &above in &tree.children[index + 1..] {
        let a = conn
            .get_window_attributes(above)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if a.map_state == MapState::VIEWABLE && a.class == WindowClass::INPUT_OUTPUT {
            let mut bounds = rect(conn, above, root)?;
            let g = conn
                .get_geometry(above)
                .map_err(error)?
                .reply()
                .map_err(error)?;
            let border = i32::from(g.border_width);
            bounds.x -= border;
            bounds.y -= border;
            bounds.w += u32::from(g.border_width) * 2;
            bounds.h += u32::from(g.border_width) * 2;
            if overlaps(wanted, bounds) {
                return Err(error(format!(
                    "window is obscured by X window {above:#x}; use scripts/ui-linux.py for an isolated display"
                )));
            }
        }
    }
    Ok(())
}

/// Capture the largest mapped client owned by the harness PID. EWMH's client
/// list survives reparenting; PID matching keeps unrelated apps out of evidence.
pub fn capture_pid(pid: u32) -> Result<Capture, String> {
    let (conn, _) = x11rb::connect(None).map_err(|e| error(format!(
        "cannot connect to DISPLAY ({e}); run through scripts/ui-linux.py (native Wayland capture is not supported)"
    )))?;
    let clients_atom = atom(&conn, b"_NET_CLIENT_LIST")?;
    let pid_atom = atom(&conn, b"_NET_WM_PID")?;
    let mut best = None;
    let mut best_area = 0;
    for screen in &conn.setup().roots {
        let clients = property(&conn, screen.root, clients_atom, AtomEnum::WINDOW)?;
        for window in clients {
            // A client may disappear between reading the list and its properties.
            let Ok(owners) = property(&conn, window, pid_atom, AtomEnum::CARDINAL) else {
                continue;
            };
            if owners.first().copied() != Some(pid) {
                continue;
            }
            let Ok(cookie) = conn.get_window_attributes(window) else {
                continue;
            };
            let Ok(attrs) = cookie.reply() else { continue };
            if attrs.map_state != MapState::VIEWABLE || attrs.class != WindowClass::INPUT_OUTPUT {
                continue;
            }
            let r = rect(&conn, window, screen.root)?;
            let area = u64::from(r.w) * u64::from(r.h);
            if area > best_area {
                best_area = area;
                best = Some((window, screen, attrs.visual));
            }
        }
    }
    let (window, screen, visual_id) = best.ok_or_else(|| error(format!(
        "process {pid} has no mapped X11 client in _NET_CLIENT_LIST; use an X11 window manager (scripts/ui-linux.py). Native Wayland windows cannot be captured this way"
    )))?;
    let before = rect(&conn, window, screen.root)?;
    if before.x < 0
        || before.y < 0
        || before.right() > i64::from(screen.width_in_pixels)
        || before.bottom() > i64::from(screen.height_in_pixels)
    {
        return Err(error(
            "client window extends beyond the X screen; enlarge the virtual display",
        ));
    }
    unobscured(&conn, window, screen.root, before)?;
    // Check the allocation before requesting the server's potentially large reply.
    let max_len = u64::from(before.w) * u64::from(before.h) * 4;
    if max_len == 0 || max_len > MAX_IMAGE_BYTES as u64 {
        return Err(error("window dimensions exceed the capture budget"));
    }
    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            window,
            0,
            0,
            before.w as u16,
            before.h as u16,
            u32::MAX,
        )
        .map_err(error)?
        .reply()
        .map_err(error)?;
    let after = rect(&conn, window, screen.root)?;
    if before != after {
        return Err(error("window moved or resized during capture; retry"));
    }
    unobscured(&conn, window, screen.root, after)?;
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == visual_id)
        .ok_or_else(|| error("window visual is absent from the X server setup"))?;
    if reply.visual != visual_id {
        return Err(error("GetImage returned a different visual"));
    }
    let format = conn
        .setup()
        .pixmap_formats
        .iter()
        .find(|f| f.depth == reply.depth)
        .ok_or_else(|| error("unknown pixmap depth"))?;
    let image = decode(
        before.w,
        before.h,
        &reply.data,
        format,
        visual,
        conn.setup().image_byte_order,
    )?;
    if is_blank(&image) {
        return Err(error("window capture is blank; wait for the app to render"));
    }
    Ok(Capture {
        image,
        origin: (before.x, before.y),
        how: How::X11,
    })
}

/// Decode server-native ZPixmap bytes, including row padding and visual masks.
/// Depth 24 commonly uses 32 bits per pixel; the unused byte is not alpha.
fn decode(
    w: u32,
    h: u32,
    data: &[u8],
    format: &Format,
    visual: &Visualtype,
    order: ImageOrder,
) -> Result<Image, String> {
    if visual.class != VisualClass::TRUE_COLOR {
        return Err(error("only TrueColor visuals are supported"));
    }
    let bpp = usize::from(format.bits_per_pixel);
    let pad = usize::from(format.scanline_pad);
    if !matches!(bpp, 16 | 24 | 32)
        || !matches!(pad, 8 | 16 | 32)
        || !matches!(order, ImageOrder::LSB_FIRST | ImageOrder::MSB_FIRST)
    {
        return Err(error("unsupported X11 pixel format"));
    }
    let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
    let mut used = 0u32;
    for mask in masks {
        let shifted = mask.checked_shr(mask.trailing_zeros()).unwrap_or(0);
        if mask == 0
            || (bpp < 32 && u64::from(mask) >= (1u64 << bpp))
            || shifted & shifted.wrapping_add(1) != 0
            || used & mask != 0
        {
            return Err(error("invalid or overlapping TrueColor masks"));
        }
        used |= mask;
    }
    let row_bits = (w as usize)
        .checked_mul(bpp)
        .ok_or_else(|| error("image dimensions overflow"))?;
    let stride = row_bits
        .checked_add(pad - 1)
        .ok_or_else(|| error("row stride overflow"))?
        / pad
        * (pad / 8);
    let input_len = stride
        .checked_mul(h as usize)
        .ok_or_else(|| error("image dimensions overflow"))?;
    let output_len = (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| error("image dimensions overflow"))?;
    if w == 0
        || h == 0
        || input_len > MAX_IMAGE_BYTES
        || output_len > MAX_IMAGE_BYTES
        || data.len() < input_len
    {
        return Err(error("empty, oversized or truncated X11 image"));
    }
    let mut rgba = Vec::with_capacity(output_len);
    for row in data[..input_len].chunks_exact(stride) {
        for pixel in row[..w as usize * (bpp / 8)].chunks_exact(bpp / 8) {
            let mut value = 0u32;
            for (i, &byte) in pixel.iter().enumerate() {
                let shift = if order == ImageOrder::LSB_FIRST {
                    i
                } else {
                    pixel.len() - 1 - i
                };
                value |= u32::from(byte) << (shift * 8);
            }
            for mask in masks {
                let shift = mask.trailing_zeros();
                let max = u64::from(mask >> shift);
                let channel = u64::from((value & mask) >> shift);
                rgba.push(((channel * 255 + max / 2) / max) as u8);
            }
            rgba.push(255);
        }
    }
    Image::from_rgba(w, h, rgba).ok_or_else(|| error("invalid RGBA image dimensions"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn visual(red: u32, green: u32, blue: u32) -> Visualtype {
        Visualtype {
            class: VisualClass::TRUE_COLOR,
            red_mask: red,
            green_mask: green,
            blue_mask: blue,
            ..Default::default()
        }
    }
    fn format(bpp: u8) -> Format {
        Format {
            depth: 24,
            bits_per_pixel: bpp,
            scanline_pad: 32,
        }
    }
    #[test]
    fn bgrx_is_opaque_and_rows_honor_padding() {
        let v = visual(0xff0000, 0xff00, 0xff);
        let image = decode(
            1,
            2,
            &[3, 2, 1, 0, 6, 5, 4, 0],
            &format(32),
            &v,
            ImageOrder::LSB_FIRST,
        )
        .unwrap();
        assert_eq!(image.px, [1, 2, 3, 255, 4, 5, 6, 255]);
        let image = decode(
            1,
            2,
            &[1, 2, 3, 99, 4, 5, 6, 99],
            &format(24),
            &v,
            ImageOrder::MSB_FIRST,
        )
        .unwrap();
        assert_eq!(image.px, [1, 2, 3, 255, 4, 5, 6, 255]);
    }
    #[test]
    fn rgb565_scales_channels_and_respects_byte_order() {
        let v = visual(0xf800, 0x07e0, 0x001f);
        let image = decode(
            2,
            1,
            &[0xf8, 0x00, 0x07, 0xe0],
            &format(16),
            &v,
            ImageOrder::MSB_FIRST,
        )
        .unwrap();
        assert_eq!(image.px, [255, 0, 0, 255, 0, 255, 0, 255]);
    }
    #[test]
    fn rejects_truncated_oversized_and_invalid_visuals() {
        let mut v = visual(0xff0000, 0xff00, 0xff);
        assert!(decode(2, 1, &[0; 4], &format(32), &v, ImageOrder::LSB_FIRST).is_err());
        assert!(
            decode(
                u32::MAX,
                u32::MAX,
                &[],
                &format(32),
                &v,
                ImageOrder::LSB_FIRST
            )
            .is_err()
        );
        v.green_mask = v.red_mask;
        assert!(decode(1, 1, &[0; 4], &format(32), &v, ImageOrder::LSB_FIRST).is_err());
        v = visual(0b101, 0xff00, 0xff0000);
        assert!(decode(1, 1, &[0; 4], &format(32), &v, ImageOrder::LSB_FIRST).is_err());
        v.class = VisualClass::PSEUDO_COLOR;
        assert!(decode(1, 1, &[0; 4], &format(32), &v, ImageOrder::LSB_FIRST).is_err());
    }
    #[test]
    fn touching_edges_do_not_obscure_but_overlap_does() {
        let r = RectPx::new(10, 20, 30, 40);
        assert!(!overlaps(r, RectPx::new(40, 20, 5, 5)));
        assert!(overlaps(r, RectPx::new(39, 20, 5, 5)));
    }
}
