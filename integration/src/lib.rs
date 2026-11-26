//! An integration plugin: one shared library that gives a host its product
//! names and icons, its Microsoft 365 sign-in settings, a silent token
//! source and the person's organization and desktop. `docs/integration-plugins.md`
//! is the contract.
//!
//! [`abi`] and [`protocol`] are what a plugin written in Rust needs (depend
//! with `default-features = false`). The `host` feature adds the host side:
//! [`host`] and [`win_icon`].

pub mod abi;
pub mod protocol;

#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub mod win_icon;

#[cfg(feature = "host")]
pub use host::{global, start};
