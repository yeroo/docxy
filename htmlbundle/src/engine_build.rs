//! Build-script helper that produces the `docxwasm` engine a bundle embeds.
//!
//! docxy and the suite call [`prepare_engine`] from their `build.rs`, and only
//! when their `html-export` feature is on, so ordinary builds (and a plain
//! `cargo install docxy`) never need the wasm target.
//!
//! - `DOCXY_HTML_ENGINE=<path>` uses a prebuilt `docxwasm.wasm` as is.
//! - Otherwise it runs a nested
//!   `cargo build -p docxwasm --target wasm32-unknown-unknown --release --locked`
//!   into its own target dir under `OUT_DIR` (no lock contention with the outer
//!   build), with the outer build's flags scrubbed: `RUSTFLAGS` from
//!   `cargo llvm-cov` would ask for coverage instrumentation the wasm target
//!   cannot provide, and a clippy/sccache wrapper has no business there.
//!
//! A failure is returned as an error; the caller panics, so a build that asked
//! for HTML export never silently ships without an engine.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment the nested build must not inherit from the outer cargo.
const SCRUBBED_ENV: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_TARGET_DIR",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "LLVM_PROFILE_FILE",
    "CARGO_INCREMENTAL",
];

/// Env var naming a prebuilt engine to embed instead of building one.
pub const ENGINE_ENV: &str = "DOCXY_HTML_ENGINE";

/// Put `docxwasm.wasm` in `out_dir` and return its path. `workspace_root` is
/// the root workspace (the directory holding the top-level `Cargo.toml`).
/// Emits the `cargo:rerun-if-*` lines for the caller's build script.
pub fn prepare_engine(workspace_root: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    println!("cargo:rerun-if-env-changed={ENGINE_ENV}");
    let dest = out_dir.join("docxwasm.wasm");
    if let Some(prebuilt) = std::env::var_os(ENGINE_ENV).filter(|v| !v.is_empty()) {
        let prebuilt = PathBuf::from(prebuilt);
        println!("cargo:rerun-if-changed={}", prebuilt.display());
        std::fs::copy(&prebuilt, &dest)
            .map_err(|e| format!("{ENGINE_ENV}={}: {e}", prebuilt.display()))?;
        return Ok(dest);
    }

    for dir in ["docxwasm", "docxcore", "opccore"] {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root.join(dir).display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.lock").display()
    );

    let target_dir = out_dir.join("wasm");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(workspace_root.join("Cargo.toml"))
        .args([
            "-p",
            "docxwasm",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--locked",
        ])
        .arg("--target-dir")
        .arg(&target_dir);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("CARGO_LLVM_COV") {
            cmd.env_remove(key);
        }
    }
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run cargo for the docxwasm engine: {e}"))?;
    if !status.success() {
        return Err(format!(
            "building the docxwasm engine failed ({status}). HTML export needs the wasm target: \
             `rustup target add wasm32-unknown-unknown`, or set {ENGINE_ENV} to a prebuilt \
             docxwasm.wasm"
        ));
    }
    let built = target_dir
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("docxwasm.wasm");
    std::fs::copy(&built, &dest).map_err(|e| format!("{}: {e}", built.display()))?;
    Ok(dest)
}
