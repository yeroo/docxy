//! Persists a [`TokenSet`] to disk between runs so the user stays signed in.
//!
//! The JSON payload is encrypted at rest via the platform seam
//! [`protect`]/[`unprotect`] before it touches disk, and writes are atomic
//! (write to a `.tmp` sibling, then `rename` over the real path) so a crash
//! mid-write can never leave a half-written or corrupt cache file. Secrets
//! (the access/refresh tokens) are never logged by this module.

use crate::auth::TokenSet;
use crate::json::{self, Value};
use std::fs;
use std::io;
use std::path::Path;

/// Serializes `t` to JSON, encrypts it via [`protect`], and writes it
/// atomically to `path` (temp file + rename, so a concurrent reader or a
/// crash mid-write never observes a partial file).
pub fn save(path: &Path, t: &TokenSet) -> io::Result<()> {
    let v = Value::Object(vec![
        (
            "access_token".to_string(),
            Value::Str(t.access_token.clone()),
        ),
        (
            "refresh_token".to_string(),
            Value::Str(t.refresh_token.clone()),
        ),
        (
            "expires_at_unix".to_string(),
            Value::Num(t.expires_at_unix as f64),
        ),
        ("account".to_string(), Value::Str(t.account.clone())),
    ]);
    let plaintext = v.to_string().into_bytes();
    let ciphertext = protect(&plaintext)?;

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, &ciphertext)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Loads and decrypts the token cache at `path`. `Ok(None)` if the file
/// doesn't exist (the common case: no one has signed in yet); any other I/O
/// failure, or a cache that fails to decrypt/parse, is returned as `Err`.
pub fn load(path: &Path) -> io::Result<Option<TokenSet>> {
    let ciphertext = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let plaintext = unprotect(&ciphertext)?;
    let text = String::from_utf8(plaintext)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let v = json::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let access_token = v
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refresh_token = v
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let expires_at_unix = v
        .get("expires_at_unix")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as u64;
    let account = v
        .get("account")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(Some(TokenSet {
        access_token,
        refresh_token,
        expires_at_unix,
        account,
    }))
}

// --- Platform seam -------------------------------------------------------
//
// `protect`/`unprotect` are the only functions that know how the bytes are
// secured at rest.

/// Encrypts `bytes` for storage. On Windows this is DPAPI `CryptProtectData`
/// scoped to the current user (no extra entropy, no UI); elsewhere it's the
/// identity transform (see the non-Windows `unprotect` below for the caveat).
#[cfg(windows)]
fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };

    // `CryptProtectData` takes a non-const in-blob pointer at the FFI
    // boundary even though it only reads from it; keep our own mutable copy
    // so we never hand out a pointer derived from the caller's `&[u8]`.
    let mut input = bytes.to_vec();
    let blob_in = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut blob_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: `blob_in.pbData` points at `input`, which is alive for the
    // whole call and whose length exactly matches `cbData`. All other
    // pointer args we pass are null, which the API documents as valid
    // ("no description"/"no optional entropy"/"no reserved"/"no UI
    // prompt"). `blob_out` is a valid, aligned, writable out-param.
    let ok = unsafe {
        CryptProtectData(
            &blob_in,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut blob_out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(blob_out, LocalFree))
}

/// Decrypts bytes produced by [`protect`]. On Windows: DPAPI
/// `CryptUnprotectData`.
#[cfg(windows)]
fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    };

    let mut input = bytes.to_vec();
    let blob_in = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut blob_out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: same reasoning as `protect` above; `ppszDataDescr` (the only
    // extra out-param `CryptUnprotectData` has) is also null, which is
    // documented as valid when the caller doesn't want the description
    // back.
    let ok = unsafe {
        CryptUnprotectData(
            &blob_in,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut blob_out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take_blob(blob_out, LocalFree))
}

/// Copies a `CRYPT_INTEGER_BLOB` produced by DPAPI into an owned `Vec`, then
/// frees the buffer DPAPI allocated for it. Shared by `protect`/`unprotect`
/// so the "copy out, then always free" sequence can't be forgotten on one
/// path (that would leak the DPAPI-allocated buffer).
#[cfg(windows)]
fn take_blob(
    blob: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
    local_free: unsafe extern "system" fn(
        windows_sys::Win32::Foundation::HLOCAL,
    ) -> windows_sys::Win32::Foundation::HLOCAL,
) -> Vec<u8> {
    // SAFETY: `blob.pbData`/`blob.cbData` were just filled in by a
    // successful `CryptProtectData`/`CryptUnprotectData` call, which
    // documents `pbData` as pointing to `cbData` valid bytes that the
    // caller now owns. Guard the zero-length case explicitly:
    // `slice::from_raw_parts` requires a non-null, well-aligned pointer
    // even for an empty slice, and DPAPI's `pbData` for a zero-length blob
    // is not documented to be non-null.
    let out = if blob.cbData == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec()
    };
    if !blob.pbData.is_null() {
        // SAFETY: `blob.pbData` was allocated by DPAPI (via `LocalAlloc`
        // internally) and must be released with `LocalFree`; we've already
        // copied its contents above, so freeing it here doesn't leave any
        // dangling references.
        unsafe {
            local_free(blob.pbData as windows_sys::Win32::Foundation::HLOCAL);
        }
    }
    out
}

/// Identity transform: v1 has no non-Windows at-rest encryption.
// note: at-rest encryption is Windows-only in v1. On other platforms the
// cache is protected only by file permissions (0o600, set in `save`), not
// by encryption.
#[cfg(not(windows))]
fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}

/// Identity transform, matching `protect` above.
#[cfg(not(windows))]
fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenSet;

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!("lookxy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("token.bin");
        let t = TokenSet {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at_unix: 123,
            account: "me@epam.com".into(),
        };
        save(&p, &t).unwrap();
        let got = load(&p).unwrap().unwrap();
        assert_eq!(got.refresh_token, "RT");
        assert_eq!(got.account, "me@epam.com");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_absent_is_none() {
        let p = std::env::temp_dir().join("lookxy-nonexistent-xyz.bin");
        let _ = std::fs::remove_file(&p);
        assert!(load(&p).unwrap().is_none());
    }

    #[test]
    fn encrypted_bytes_are_not_plaintext() {
        // On Windows the on-disk bytes must not contain the token verbatim.
        let dir = std::env::temp_dir().join(format!("lookxy-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.bin");
        let t = TokenSet {
            access_token: "SECRET_TOKEN_VALUE".into(),
            refresh_token: "RT".into(),
            expires_at_unix: 1,
            account: "".into(),
        };
        save(&p, &t).unwrap();
        let raw = std::fs::read(&p).unwrap();
        #[cfg(windows)]
        assert!(!raw.windows(18).any(|w| w == b"SECRET_TOKEN_VALUE"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
