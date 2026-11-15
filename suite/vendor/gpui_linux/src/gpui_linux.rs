// Modified for docxy (#1966): `wayland_clipboard` made public.

#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;
/// RTF/HTML beside a copy's text on the Wayland selection (#1966).
#[cfg(feature = "wayland")]
pub use linux::wayland_clipboard;
