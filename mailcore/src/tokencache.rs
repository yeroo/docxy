//! Persists a [`TokenSet`] between runs so the user stays signed in.
//!
//! Where the token set lives (#1877):
//!
//! - **A platform secret store**, when one is reachable: the Secret Service
//!   (`org.freedesktop.secrets` over the D-Bus session bus) on Linux, the
//!   login Keychain on macOS. The file at `path` then holds only a
//!   *reference* (`{"secret_store":…,"key":…,"account":…}`), no secret.
//! - **The file itself** otherwise: DPAPI-encrypted on Windows (which has no
//!   store here), and elsewhere plaintext JSON protected by 0600 permissions
//!   alone, the fallback when no store is reachable (headless, a session
//!   without a keyring). [`save_reporting`] / [`load_reporting`] say which,
//!   so the engine can warn once ([`Storage::PlainFile`]).
//!
//! A token file found while a store is reachable is migrated into it on
//! load: the file is replaced by a reference (an atomic rename over it; the
//! old blocks are not zero-filled). The store-generic core (`save_in`,
//! `load_in`, `clear_in`) takes the store as an argument, which is what the
//! tests drive with a fake; the public functions pass [`platform_store`].
//!
//! Every file is written atomically (a `.tmp` sibling, then `rename` over
//! the real path), created 0600 on unix, so a crash mid-write never leaves a
//! half-written cache. Secrets are never logged by this module.
//!
//! The platform store is skipped under mailcore's own `cfg(test)` only. An
//! embedder's tests that reach the token store (an engine on
//! `crate::testserver::FakeServer`, say) set `MAILCORE_TOKEN_STORE=file`,
//! which keeps the token in the file, or they get the developer's keyring
//! (on Linux, the Secret Service over the session bus).

use crate::auth::TokenSet;
use crate::json::{self, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
mod keychain;
#[cfg(target_os = "linux")]
mod secret_service;

/// Why a [`SecretStore`] call did not succeed.
// No secret store off Linux and macOS: only the tests build these there.
#[cfg_attr(
    all(not(test), not(any(target_os = "linux", target_os = "macos"))),
    allow(dead_code)
)]
#[derive(Debug)]
pub(crate) enum SecretStoreError {
    /// No store to talk to (no session bus, no Secret Service, no keychain)
    /// or one that cannot be used now. Cheap to find out and not
    /// remembered: a store that appears later is used.
    Unavailable(String),
    /// The user dismissed the store's unlock prompt, or nobody answered it
    /// in time. [`Latched`] remembers it, so the user is not asked again
    /// for the rest of the run.
    Refused(String),
    /// The store answered with an error.
    Failed(io::Error),
}

impl SecretStoreError {
    /// As an `io::Error`: an unavailable or refused store is
    /// `io::ErrorKind::NotConnected`, `secret store unavailable: <reason>`
    /// (the engine shows that one); a failure is the store's own error.
    fn into_io(self) -> io::Error {
        match self {
            SecretStoreError::Unavailable(m) | SecretStoreError::Refused(m) => unavailable(&m),
            SecretStoreError::Failed(e) => e,
        }
    }
}

/// `secret store unavailable: <reason>`, of kind `NotConnected`.
fn unavailable(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        format!("secret store unavailable: {reason}"),
    )
}

/// A store of secrets by key: the platform's (Secret Service, Keychain) or
/// a test's fake.
pub(crate) trait SecretStore {
    /// The store's name, written into the reference file
    /// (`"secret-service"`, `"keychain"`).
    fn name(&self) -> &'static str;
    /// The secret under `key`; `Ok(None)` when there is none.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    /// Stores `secret` under `key`, replacing what was there. `label` is
    /// what a keyring browser shows for it, where the store has labels.
    fn set(&self, key: &str, label: &str, secret: &[u8]) -> Result<(), SecretStoreError>;
    /// Deletes the secret under `key`. A missing one is `Ok`.
    fn delete(&self, key: &str) -> Result<(), SecretStoreError>;
}

/// Where a token set ended up after a save or a load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// In the platform secret store; the file holds a reference.
    SecretStore,
    /// In the file, encrypted (DPAPI on Windows).
    EncryptedFile,
    /// In the file, unencrypted: protected only by its 0600 permissions.
    PlainFile,
}

/// How this platform's token file is protected.
const FILE_STORAGE: Storage = if cfg!(windows) {
    Storage::EncryptedFile
} else {
    Storage::PlainFile
};

/// A store that stops asking once the user refused its prompt: after the
/// first [`SecretStoreError::Refused`], every `get` and `set` is
/// `Unavailable` without reaching `inner`. Without it every hourly token
/// refresh would raise the keyring's unlock prompt again, or wait out a
/// prompt nobody can see (or a wedged service, `secret_service`'s method
/// timeout). `delete` is a sign-out, which the user asked for: it always
/// reaches `inner`, so a refresh token stored before the refusal is not
/// left behind once the file no longer points at it.
// No secret store off Linux and macOS: only the tests build these there.
#[cfg_attr(
    all(not(test), not(any(target_os = "linux", target_os = "macos"))),
    allow(dead_code)
)]
pub(crate) struct Latched<S> {
    inner: S,
    refused: OnceLock<String>,
}

// No secret store off Linux and macOS: only the tests build these there.
#[cfg_attr(
    all(not(test), not(any(target_os = "linux", target_os = "macos"))),
    allow(dead_code)
)]
impl<S> Latched<S> {
    pub(crate) const fn new(inner: S) -> Self {
        Latched {
            inner,
            refused: OnceLock::new(),
        }
    }

    fn call<T>(
        &self,
        f: impl FnOnce(&S) -> Result<T, SecretStoreError>,
    ) -> Result<T, SecretStoreError> {
        if let Some(reason) = self.refused.get() {
            return Err(SecretStoreError::Unavailable(format!(
                "{reason} (earlier in this run)"
            )));
        }
        let r = f(&self.inner);
        if let Err(SecretStoreError::Refused(reason)) = &r {
            let _ = self.refused.set(reason.clone());
        }
        r
    }
}

impl<S: SecretStore> SecretStore for Latched<S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        self.call(|s| s.get(key))
    }
    fn set(&self, key: &str, label: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        self.call(|s| s.set(key, label, secret))
    }
    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.inner.delete(key)
    }
}

/// The platform's secret store, or `None`: on Windows (DPAPI keeps the
/// file), under `cfg(test)` (no unit test reaches the developer's keyring),
/// and when `MAILCORE_TOKEN_STORE=file` (a host's own tests set it). One
/// process-wide [`Latched`] store, so a refusal outlives an engine restart.
#[cfg(all(not(test), target_os = "linux"))]
pub(crate) fn platform_store() -> Option<&'static dyn SecretStore> {
    static STORE: Latched<secret_service::SecretService> =
        Latched::new(secret_service::SecretService);
    (!file_only()).then_some(&STORE as &dyn SecretStore)
}

/// See the Linux `platform_store`.
#[cfg(all(not(test), target_os = "macos"))]
pub(crate) fn platform_store() -> Option<&'static dyn SecretStore> {
    static STORE: Latched<keychain::Keychain> = Latched::new(keychain::Keychain);
    (!file_only()).then_some(&STORE as &dyn SecretStore)
}

/// See the Linux `platform_store`.
#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn platform_store() -> Option<&'static dyn SecretStore> {
    None
}

/// `MAILCORE_TOKEN_STORE=file`: keep the token in the file.
#[cfg(all(not(test), any(target_os = "linux", target_os = "macos")))]
fn file_only() -> bool {
    std::env::var_os("MAILCORE_TOKEN_STORE").is_some_and(|v| v == "file")
}

/// Saves `t` to the platform store (the file at `path` then holds a
/// reference), or to the file itself when no store takes it.
pub fn save(path: &Path, t: &TokenSet) -> io::Result<()> {
    save_reporting(path, t).map(|_| ())
}

/// [`save`], saying where the token set went.
pub fn save_reporting(path: &Path, t: &TokenSet) -> io::Result<Storage> {
    save_in(path, t, platform_store())
}

/// Loads the token set saved at `path`, from the store its reference names
/// or from the file itself (migrating a token file into the store). See
/// [`load_in`] for the cases.
pub fn load(path: &Path) -> io::Result<Option<TokenSet>> {
    load_reporting(path).map(|r| r.map(|(t, _)| t))
}

/// [`load`], saying where the token set now lives.
pub fn load_reporting(path: &Path) -> io::Result<Option<(TokenSet, Storage)>> {
    load_in(path, platform_store())
}

/// Signs out: deletes the store's item and the file at `path`. See
/// [`clear_in`].
pub fn clear(path: &Path) -> io::Result<()> {
    clear_in(path, platform_store())
}

/// The account saved at `path` (a reference or a token file), with no
/// store call: lookxy asks after every sync pass on its UI thread, where a
/// keyring prompt must not block. `None` with no file or an unreadable one.
pub fn account(path: &Path) -> Option<String> {
    let v = read_file(path).ok()??;
    v.get("account").and_then(Value::as_str).map(str::to_string)
}

/// The store-generic save. With a store that takes the token JSON, writes a
/// reference to `path` and reports [`Storage::SecretStore`]. With no store,
/// or one that is unavailable, refused or failed, writes the token file
/// (DPAPI on Windows, 0600 plaintext elsewhere): a store failure never
/// fails the save, as losing the sign-in would be worse than the warned
/// fallback. A reference that named another key has that item deleted
/// once the new reference is written (errors ignored: the token is saved),
/// unless the file at that key still refers to it (a copied cache).
/// An error is a failure to write the file; whatever file was there is left
/// as it was (an old reference to this key still points at the item the
/// store now holds; an old token file is migrated by the next load).
pub(crate) fn save_in(
    path: &Path,
    t: &TokenSet,
    store: Option<&dyn SecretStore>,
) -> io::Result<Storage> {
    let json = token_json(t);
    if let Some(store) = store
        && let Ok(key) = store_key(path)
    {
        // The item the file pointed at until now, if under another key (a
        // moved directory, the cache reached through another path): read
        // before the write replaces the reference, deleted once it has.
        let previous = referenced_key(path).filter(|k| *k != key);
        if store.set(&key, &label(&t.account), json.as_bytes()).is_ok() {
            write_file(path, &reference_json(store.name(), &key, &t.account))?;
            // Unless another cache still uses it: a copied cache directory's
            // original still holds a reference to its own key. Checked
            // after the write, so a file reached under the old name (the
            // same file under another path) now names the new key.
            if let Some(previous) = previous
                && referenced_key(Path::new(&previous)).as_deref() != Some(previous.as_str())
            {
                let _ = store.delete(&previous);
            }
            return Ok(Storage::SecretStore);
        }
    }
    write_file(path, &json)?;
    Ok(FILE_STORAGE)
}

/// The `key` of the reference at `path`; `None` when the file is missing,
/// unreadable, a token file, or a reference without a key.
fn referenced_key(path: &Path) -> Option<String> {
    let v = read_file(path).ok()??;
    v.get("secret_store")?;
    reference_key(&v).ok().map(str::to_string)
}

/// The store-generic load:
///
/// - no file: `Ok(None)`, with no store call (a never-signed-in start
///   stays off D-Bus);
/// - a reference: the item under its `key`. A missing item (the user
///   deleted it) is `Ok(None)`. No store, or one unavailable or refused, is
///   an `io::ErrorKind::NotConnected` error (`secret store unavailable:
///   …`), and a store failure its own error; the reference stays on disk
///   either way, as a keyring locked now may open next time. A reference
///   without a `key` is `InvalidData`;
/// - a token file (from before #1877, or the fallback): the token set. With
///   a store, it is migrated: stored, then the file replaced by a reference
///   ([`Storage::SecretStore`]). A store that does not take it, or a
///   reference that cannot be written, leaves the file as it was
///   ([`Storage::PlainFile`]), and the next load tries again.
pub(crate) fn load_in(
    path: &Path,
    store: Option<&dyn SecretStore>,
) -> io::Result<Option<(TokenSet, Storage)>> {
    let Some(v) = read_file(path)? else {
        return Ok(None);
    };
    if v.get("secret_store").is_some() {
        let key = reference_key(&v)?;
        let Some(store) = store else {
            return Err(unavailable("no secret store on this system"));
        };
        let Some(secret) = store.get(key).map_err(SecretStoreError::into_io)? else {
            return Ok(None);
        };
        let text = String::from_utf8(secret).map_err(invalid)?;
        let t = token_from(&json::parse(&text).map_err(invalid)?);
        return Ok(Some((t, Storage::SecretStore)));
    }
    let t = token_from(&v);
    if let Some(store) = store
        && let Ok(key) = store_key(path)
    {
        if store
            .set(&key, &label(&t.account), token_json(&t).as_bytes())
            .is_ok()
            && write_file(path, &reference_json(store.name(), &key, &t.account)).is_ok()
        {
            let _ = fs::remove_file(tmp_path(path));
            return Ok(Some((t, Storage::SecretStore)));
        }
    }
    Ok(Some((t, FILE_STORAGE)))
}

/// The store-generic sign-out. Removes the file at `path` and the `.tmp`
/// sibling a `save` that died mid-write may have left, then, with a store,
/// deletes the item under `path`'s key and, if the file was a reference
/// naming another key, that one too. The store is asked whatever the file
/// held: a token file saved by the fallback may sit over a reference whose
/// item is still in the store. Everything is tried; the first error
/// (removing the file, then each delete) is returned. A file or item that
/// is already gone is `Ok`.
pub(crate) fn clear_in(path: &Path, store: Option<&dyn SecretStore>) -> io::Result<()> {
    let referenced = referenced_key(path);
    let mut first: Option<io::Error> = match fs::remove_file(path) {
        Ok(()) => None,
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => Some(e),
    };
    let _ = fs::remove_file(tmp_path(path));
    if let Some(store) = store {
        let mut keys = Vec::new();
        match store_key(path) {
            Ok(k) => keys.push(k),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
        if let Some(k) = referenced
            && !keys.contains(&k)
        {
            keys.push(k);
        }
        for key in keys {
            if let Err(e) = store.delete(&key) {
                first.get_or_insert(e.into_io());
            }
        }
    }
    first.map_or(Ok(()), Err)
}

/// The store key for the cache at `path`: its absolute path, so one cache
/// file has one item, and signing in as another account replaces it.
fn store_key(path: &Path) -> io::Result<String> {
    Ok(std::path::absolute(path)?.to_string_lossy().into_owned())
}

/// What a keyring browser shows for the item.
fn label(account: &str) -> String {
    if account.is_empty() {
        "Mail sign-in".to_string()
    } else {
        format!("Mail sign-in for {account}")
    }
}

/// A reference's `key`, `InvalidData` when it has none.
fn reference_key(v: &Value) -> io::Result<&str> {
    v.get("key")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .ok_or_else(|| invalid("a secret-store reference without a key"))
}

/// The reference file's JSON: which store, the item's key, and the account
/// (for [`account`], with no store call). No secret.
fn reference_json(store: &str, key: &str, account: &str) -> String {
    Value::Object(vec![
        ("secret_store".to_string(), Value::Str(store.to_string())),
        ("key".to_string(), Value::Str(key.to_string())),
        ("account".to_string(), Value::Str(account.to_string())),
    ])
    .to_string()
}

/// `t` as the token JSON, stored in the store or the file alike.
fn token_json(t: &TokenSet) -> String {
    let mut fields = vec![
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
    ];
    // Unknown scopes are saved by leaving the key out, so they load as
    // unknown again (#1875).
    if let Some(scopes) = &t.scopes {
        fields.push((
            "scopes".to_string(),
            Value::Array(scopes.iter().cloned().map(Value::Str).collect()),
        ));
    }
    Value::Object(fields).to_string()
}

/// The token set in token JSON. A cache without `scopes` (saved before
/// #1875), or whose `scopes` is not an array of strings, loads with
/// `scopes: None` (unknown), never an error.
fn token_from(v: &Value) -> TokenSet {
    let text = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let scopes = v.get("scopes").and_then(Value::as_array).and_then(|items| {
        items
            .iter()
            .map(|s| s.as_str().map(str::to_string))
            .collect::<Option<Vec<String>>>()
    });
    TokenSet {
        access_token: text("access_token"),
        refresh_token: text("refresh_token"),
        expires_at_unix: v
            .get("expires_at_unix")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as u64,
        account: text("account"),
        scopes,
    }
}

fn invalid(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Reads and decrypts the file at `path` as JSON. `Ok(None)` if it does not
/// exist; any other I/O failure, or a file that fails to decrypt/parse, is
/// `Err`.
fn read_file(path: &Path) -> io::Result<Option<Value>> {
    let ciphertext = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let plaintext = unprotect(&ciphertext)?;
    let text = String::from_utf8(plaintext).map_err(invalid)?;
    json::parse(&text).map(Some).map_err(invalid)
}

/// Encrypts `text` via [`protect`] and writes it atomically to `path`
/// (temp file + rename, so a concurrent reader or a crash mid-write never
/// observes a partial file).
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let ciphertext = protect(text.as_bytes())?;

    // note: assumes a single writer — a concurrent `save` would race on
    // this deterministic tmp name; `rename` below is still atomic, so a
    // race can only lose an update, never corrupt the file.
    let tmp_path = tmp_path(path);
    #[cfg(unix)]
    {
        // On unix `protect` is the identity transform (see `unprotect`
        // below), so a token file's `ciphertext` is the plaintext token
        // JSON: the file must never exist world/group-readable, even
        // momentarily. Create it already restricted to 0600 rather than
        // writing plaintext at default umask perms and tightening afterward.
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp_path)?;
        f.write_all(&ciphertext)?;
    }
    #[cfg(not(unix))]
    fs::write(&tmp_path, &ciphertext)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Test-only: writes a reference to `key` in store `store` at `path`, as
/// `save_in` would after a store took the token (the engine's tests have no
/// store to save through).
#[cfg(test)]
pub(crate) fn write_reference(
    path: &Path,
    store: &str,
    key: &str,
    account: &str,
) -> io::Result<()> {
    write_file(path, &reference_json(store, key, account))
}

/// The temp file `write_file` writes before renaming it over `path`.
fn tmp_path(path: &Path) -> PathBuf {
    path.with_extension("tmp")
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
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
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
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
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
            scopes: None,
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
            scopes: None,
        };
        save(&p, &t).unwrap();
        let raw = std::fs::read(&p).unwrap();
        // On Windows DPAPI must hide the token from the on-disk bytes. Off
        // Windows the cache is plaintext-at-rest by design (`protect` is the
        // identity transform — see I2), so the token appears verbatim; assert
        // that so `raw` is exercised on every platform.
        #[cfg(windows)]
        assert!(!raw.windows(18).any(|w| w == b"SECRET_TOKEN_VALUE"));
        #[cfg(not(windows))]
        assert!(raw.windows(18).any(|w| w == b"SECRET_TOKEN_VALUE"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample() -> TokenSet {
        TokenSet {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at_unix: 123,
            account: "me@x".into(),
            scopes: None,
        }
    }

    /// A fresh directory for one test's cache file.
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lookxy-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scopes_roundtrip() {
        let dir = test_dir("scopes");
        let p = dir.join("token.bin");
        let scopes = vec![
            "https://graph.microsoft.com/Mail.ReadWrite".to_string(),
            "https://graph.microsoft.com/People.Read".to_string(),
            "openid".to_string(),
        ];
        let t = TokenSet {
            scopes: Some(scopes.clone()),
            ..sample()
        };
        save(&p, &t).unwrap();
        assert_eq!(load(&p).unwrap().unwrap().scopes, Some(scopes));
        // Unknown stays unknown: the key is left out.
        save(&p, &sample()).unwrap();
        assert_eq!(load(&p).unwrap().unwrap().scopes, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_without_scopes_loads_as_unknown() {
        // The cache as written before #1875. Through `protect`, as `load`
        // decrypts with DPAPI on Windows.
        let dir = test_dir("old-cache");
        let p = dir.join("token.bin");
        let old =
            r#"{"access_token":"AT","refresh_token":"RT","expires_at_unix":123,"account":"me@x"}"#;
        std::fs::write(&p, protect(old.as_bytes()).unwrap()).unwrap();
        let got = load(&p).unwrap().expect("an old cache still loads");
        assert_eq!(got.access_token, "AT");
        assert_eq!(got.refresh_token, "RT");
        assert_eq!(got.expires_at_unix, 123);
        assert_eq!(got.account, "me@x");
        assert_eq!(got.scopes, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_with_malformed_scopes_loads_as_unknown() {
        let dir = test_dir("bad-scopes");
        let p = dir.join("token.bin");
        for scopes in [r#""Mail.Read""#, r#"["Mail.Read",1]"#, "null"] {
            let json = format!(
                r#"{{"access_token":"AT","refresh_token":"RT","expires_at_unix":1,"account":"","scopes":{scopes}}}"#
            );
            std::fs::write(&p, protect(json.as_bytes()).unwrap()).unwrap();
            let got = load(&p).unwrap().expect("loads");
            assert_eq!(got.scopes, None, "scopes {scopes}");
            assert_eq!(got.refresh_token, "RT");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_saved_token() {
        let dir = std::env::temp_dir().join(format!("lookxy-test-clear-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("token.bin");
        save(&p, &sample()).unwrap();
        // A leftover temp file from an interrupted save goes too.
        std::fs::write(tmp_path(&p), b"partial").unwrap();
        assert_eq!(tmp_path(&p), dir.join("token.tmp"));
        clear(&p).unwrap();
        assert!(!p.exists());
        assert!(!tmp_path(&p).exists());
        assert!(load(&p).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_missing_is_ok() {
        let dir = std::env::temp_dir().join(format!("lookxy-test-clear2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("token.bin");
        clear(&p).unwrap();
        clear(&p).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- The secret store (#1877), over a fake --------------------------

    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    /// What every call of a [`FakeStore`] answers.
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Works,
        Unavailable,
        Refused,
        Fails,
    }

    /// An in-memory store: items by key (label, secret), a mode for all
    /// calls, and call counters.
    struct FakeStore {
        items: RefCell<HashMap<String, (String, Vec<u8>)>>,
        mode: Cell<Mode>,
        gets: Cell<usize>,
        sets: Cell<usize>,
        deletes: RefCell<Vec<String>>,
    }

    impl FakeStore {
        fn new() -> Self {
            FakeStore {
                items: RefCell::new(HashMap::new()),
                mode: Cell::new(Mode::Works),
                gets: Cell::new(0),
                sets: Cell::new(0),
                deletes: RefCell::new(Vec::new()),
            }
        }

        fn with_mode(mode: Mode) -> Self {
            let s = FakeStore::new();
            s.mode.set(mode);
            s
        }

        fn answer(&self) -> Result<(), SecretStoreError> {
            match self.mode.get() {
                Mode::Works => Ok(()),
                Mode::Unavailable => Err(SecretStoreError::Unavailable("no keyring".into())),
                Mode::Refused => Err(SecretStoreError::Refused("dismissed".into())),
                Mode::Fails => Err(SecretStoreError::Failed(io::Error::other("boom"))),
            }
        }

        fn secret(&self, key: &str) -> Option<String> {
            self.items
                .borrow()
                .get(key)
                .map(|(_, s)| String::from_utf8(s.clone()).unwrap())
        }
    }

    impl SecretStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            self.gets.set(self.gets.get() + 1);
            self.answer()?;
            Ok(self.items.borrow().get(key).map(|(_, s)| s.clone()))
        }
        fn set(&self, key: &str, label: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
            self.sets.set(self.sets.get() + 1);
            self.answer()?;
            self.items
                .borrow_mut()
                .insert(key.to_string(), (label.to_string(), secret.to_vec()));
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.deletes.borrow_mut().push(key.to_string());
            self.answer()?;
            self.items.borrow_mut().remove(key);
            Ok(())
        }
    }

    type Fields = (String, String, u64, String, Option<Vec<String>>);

    /// `t`'s fields, comparable (`TokenSet` has no `Debug`, so a token is
    /// never printed).
    fn fields(t: &TokenSet) -> Fields {
        (
            t.access_token.clone(),
            t.refresh_token.clone(),
            t.expires_at_unix,
            t.account.clone(),
            t.scopes.clone(),
        )
    }

    fn secret_sample() -> TokenSet {
        TokenSet {
            access_token: "SECRET_AT".into(),
            refresh_token: "SECRET_RT".into(),
            expires_at_unix: 77,
            account: "me@x".into(),
            scopes: Some(vec!["Mail.ReadWrite".into(), "openid".into()]),
        }
    }

    fn key_of(p: &Path) -> String {
        store_key(p).unwrap()
    }

    /// The file's bytes hold no token value.
    fn assert_no_token_bytes(p: &Path) {
        let raw = fs::read(p).unwrap();
        for secret in [b"SECRET_AT".as_slice(), b"SECRET_RT".as_slice()] {
            assert!(
                !raw.windows(secret.len()).any(|w| w == secret),
                "{} holds a token",
                p.display()
            );
        }
    }

    /// The file at `p` as JSON (through `unprotect`).
    fn file_json(p: &Path) -> Value {
        read_file(p).unwrap().expect("a file")
    }

    #[test]
    fn save_puts_the_token_in_the_store_and_a_reference_in_the_file() {
        let dir = test_dir("store-save");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        let t = secret_sample();
        assert_eq!(save_in(&p, &t, Some(&store)).unwrap(), Storage::SecretStore);
        assert_no_token_bytes(&p);
        let v = file_json(&p);
        assert_eq!(v.get("secret_store").and_then(Value::as_str), Some("fake"));
        assert_eq!(
            v.get("key").and_then(Value::as_str),
            Some(key_of(&p).as_str())
        );
        assert_eq!(v.get("account").and_then(Value::as_str), Some("me@x"));
        let stored = store.secret(&key_of(&p)).expect("an item");
        assert!(stored.contains("SECRET_RT"), "{stored}");
        assert_eq!(store.items.borrow()[&key_of(&p)].0, "Mail sign-in for me@x");
        assert!(!tmp_path(&p).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_reference_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("store-mode");
        let p = dir.join("token.bin");
        save_in(&p, &secret_sample(), Some(&FakeStore::new())).unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_reads_the_referenced_item_scopes_included() {
        let dir = test_dir("store-load");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        let t = secret_sample();
        save_in(&p, &t, Some(&store)).unwrap();
        let (got, storage) = load_in(&p, Some(&store)).unwrap().unwrap();
        assert_eq!(storage, Storage::SecretStore);
        assert_eq!(fields(&got), fields(&t));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_token_file_migrates_into_the_store() {
        let dir = test_dir("store-migrate");
        let p = dir.join("token.bin");
        let t = secret_sample();
        // A cache from before #1877 (or the fallback), and the `.tmp` an
        // interrupted save left.
        assert_eq!(save_in(&p, &t, None).unwrap(), FILE_STORAGE);
        fs::write(tmp_path(&p), b"SECRET_RT partial").unwrap();
        let store = FakeStore::new();
        let (got, storage) = load_in(&p, Some(&store)).unwrap().unwrap();
        assert_eq!(storage, Storage::SecretStore);
        assert_eq!(fields(&got), fields(&t));
        assert!(store.secret(&key_of(&p)).unwrap().contains("SECRET_RT"));
        assert_no_token_bytes(&p);
        assert!(!tmp_path(&p).exists());
        assert!(file_json(&p).get("secret_store").is_some());
        // The next load reads the store.
        let (again, storage) = load_in(&p, Some(&store)).unwrap().unwrap();
        assert_eq!(
            (fields(&again), storage),
            (fields(&t), Storage::SecretStore)
        );
        assert_eq!(store.sets.get(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_deletes_the_item_and_the_files() {
        let dir = test_dir("store-clear");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        fs::write(tmp_path(&p), b"partial").unwrap();
        clear_in(&p, Some(&store)).unwrap();
        assert!(store.items.borrow().is_empty());
        assert!(!p.exists());
        assert!(!tmp_path(&p).exists());
        assert!(load_in(&p, Some(&store)).unwrap().is_none());
        // Signed out already: still `Ok`.
        clear_in(&p, Some(&store)).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_asks_the_store_whatever_the_file_holds() {
        let dir = test_dir("store-clear-plain");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        // The token went to the store, then a start without a store asked
        // for sign-in and the fallback wrote a token file over the
        // reference: the item is still in the store.
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        save_in(&p, &secret_sample(), None).unwrap();
        clear_in(&p, Some(&store)).unwrap();
        assert!(store.items.borrow().is_empty());
        assert!(!p.exists());

        // A reference naming another key (a moved cache): both go.
        store
            .items
            .borrow_mut()
            .insert("elsewhere".into(), ("l".into(), b"x".to_vec()));
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        write_file(&p, &reference_json("fake", "elsewhere", "me@x")).unwrap();
        store.deletes.borrow_mut().clear();
        clear_in(&p, Some(&store)).unwrap();
        assert!(store.items.borrow().is_empty());
        assert_eq!(
            *store.deletes.borrow(),
            vec![key_of(&p), "elsewhere".to_string()]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_the_file_even_when_the_store_is_unavailable() {
        let dir = test_dir("store-clear-down");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        store.mode.set(Mode::Unavailable);
        let e = clear_in(&p, Some(&store)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert!(!p.exists(), "the next start is signed out");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_without_a_store_writes_the_token_file() {
        for mode in [Mode::Unavailable, Mode::Refused, Mode::Fails] {
            let dir = test_dir("store-fallback");
            let p = dir.join("token.bin");
            let store = FakeStore::with_mode(mode);
            let t = secret_sample();
            assert_eq!(save_in(&p, &t, Some(&store)).unwrap(), FILE_STORAGE);
            assert!(store.items.borrow().is_empty());
            let v = file_json(&p);
            assert!(v.get("secret_store").is_none());
            assert_eq!(fields(&token_from(&v)), fields(&t));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600);
            }
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_token_file_loads_untouched_when_the_store_does_not_take_it() {
        for mode in [Mode::Unavailable, Mode::Refused, Mode::Fails] {
            let dir = test_dir("store-load-fallback");
            let p = dir.join("token.bin");
            let t = secret_sample();
            save_in(&p, &t, None).unwrap();
            let before = fs::read(&p).unwrap();
            let store = FakeStore::with_mode(mode);
            let (got, storage) = load_in(&p, Some(&store)).unwrap().unwrap();
            assert_eq!(storage, FILE_STORAGE);
            assert_eq!(fields(&got), fields(&t));
            assert_eq!(fs::read(&p).unwrap(), before);
            let _ = fs::remove_dir_all(&dir);
        }
        // With no store at all, too, and the store is never asked for a
        // missing file.
        let dir = test_dir("store-load-none");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        assert!(load_in(&p, Some(&store)).unwrap().is_none());
        assert_eq!((store.gets.get(), store.sets.get()), (0, 0));
        save_in(&p, &secret_sample(), None).unwrap();
        assert_eq!(load_in(&p, None).unwrap().unwrap().1, FILE_STORAGE);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reference_whose_item_is_gone_loads_as_signed_out() {
        let dir = test_dir("store-gone");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        store.items.borrow_mut().clear();
        assert!(load_in(&p, Some(&store)).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reference_without_its_store_is_an_error_and_stays() {
        let dir = test_dir("store-down");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        let before = fs::read(&p).unwrap();
        for mode in [Mode::Unavailable, Mode::Refused] {
            store.mode.set(mode);
            let e = load_in(&p, Some(&store)).err().expect("an error");
            assert_eq!(e.kind(), io::ErrorKind::NotConnected);
            assert!(
                e.to_string().starts_with("secret store unavailable: "),
                "{e}"
            );
        }
        let e = load_in(&p, None).err().expect("an error");
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        // A store that fails is its own error, not "unavailable".
        store.mode.set(Mode::Fails);
        let e = load_in(&p, Some(&store)).err().expect("an error");
        assert_ne!(e.kind(), io::ErrorKind::NotConnected);
        assert_eq!(fs::read(&p).unwrap(), before, "the reference stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reference_without_a_key_is_invalid() {
        let dir = test_dir("store-nokey");
        let p = dir.join("token.bin");
        write_file(&p, r#"{"secret_store":"fake","account":"me@x"}"#).unwrap();
        let e = load_in(&p, Some(&FakeStore::new()))
            .err()
            .expect("an error");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_another_account_replaces_the_one_item() {
        let dir = test_dir("store-reaccount");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        let other = TokenSet {
            account: "you@y".into(),
            refresh_token: "OTHER_RT".into(),
            ..secret_sample()
        };
        save_in(&p, &other, Some(&store)).unwrap();
        assert_eq!(store.items.borrow().len(), 1);
        assert!(store.secret(&key_of(&p)).unwrap().contains("OTHER_RT"));
        assert_eq!(account(&p).as_deref(), Some("you@y"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn account_reads_the_file_without_the_store() {
        let dir = test_dir("store-account");
        let p = dir.join("token.bin");
        assert_eq!(account(&p), None);
        save_in(&p, &secret_sample(), None).unwrap();
        assert_eq!(account(&p).as_deref(), Some("me@x"));
        let store = FakeStore::new();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        store.mode.set(Mode::Unavailable);
        assert_eq!(account(&p).as_deref(), Some("me@x"));
        assert_eq!(store.gets.get(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn latched_store_stops_asking_after_refusal() {
        let dir = test_dir("store-latch");
        let p = dir.join("token.bin");
        let store = Latched::new(FakeStore::with_mode(Mode::Refused));
        assert_eq!(
            save_in(&p, &secret_sample(), Some(&store)).unwrap(),
            FILE_STORAGE
        );
        assert_eq!(store.inner.sets.get(), 1);
        // Even once the keyring would answer, this run does not ask again.
        store.inner.mode.set(Mode::Works);
        assert_eq!(
            save_in(&p, &secret_sample(), Some(&store)).unwrap(),
            FILE_STORAGE
        );
        assert_eq!(load_in(&p, Some(&store)).unwrap().unwrap().1, FILE_STORAGE);
        assert_eq!(store.inner.sets.get(), 1);
        assert!(matches!(
            store.get("k"),
            Err(SecretStoreError::Unavailable(_))
        ));
        assert_eq!(store.inner.gets.get(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_reports_an_unreachable_store_even_over_a_token_file() {
        // A keyring-less computer: the item may or may not exist, so the
        // sign-out says the store could not be asked, and the file goes.
        let dir = test_dir("store-clear-plain-down");
        let p = dir.join("token.bin");
        save_in(&p, &secret_sample(), None).unwrap();
        let store = FakeStore::with_mode(Mode::Unavailable);
        let e = clear_in(&p, Some(&store)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert_eq!(*store.deletes.borrow(), vec![key_of(&p)]);
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_out_reaches_a_store_the_latch_stopped() {
        // The token was stored, then this run's unlock prompt was refused,
        // so the next save fell back to the file over the reference. The
        // sign-out still asks the store, which deletes the old item.
        let dir = test_dir("store-latch-clear");
        let p = dir.join("token.bin");
        let store = Latched::new(FakeStore::new());
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        store.inner.mode.set(Mode::Refused);
        assert_eq!(
            save_in(&p, &secret_sample(), Some(&store)).unwrap(),
            FILE_STORAGE
        );
        store.inner.mode.set(Mode::Works);
        assert_eq!(
            save_in(&p, &secret_sample(), Some(&store)).unwrap(),
            FILE_STORAGE
        );
        assert_eq!(
            store.inner.items.borrow().len(),
            1,
            "the old item is still stored"
        );
        clear_in(&p, Some(&store)).unwrap();
        assert_eq!(*store.inner.deletes.borrow(), vec![key_of(&p)]);
        assert!(store.inner.items.borrow().is_empty());
        assert!(!p.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_deletes_the_item_a_reference_to_another_key_named() {
        let dir = test_dir("store-save-other-key");
        let p = dir.join("token.bin");
        let store = FakeStore::new();
        // The cache was reached through another path when it was saved.
        store
            .items
            .borrow_mut()
            .insert("/elsewhere/token.bin".into(), ("l".into(), b"OLD".to_vec()));
        write_file(&p, &reference_json("fake", "/elsewhere/token.bin", "me@x")).unwrap();
        assert_eq!(
            save_in(&p, &secret_sample(), Some(&store)).unwrap(),
            Storage::SecretStore
        );
        assert_eq!(
            *store.deletes.borrow(),
            vec!["/elsewhere/token.bin".to_string()]
        );
        let keys: Vec<String> = store.items.borrow().keys().cloned().collect();
        assert_eq!(keys, vec![key_of(&p)]);
        // A reference to this very key deletes nothing.
        store.deletes.borrow_mut().clear();
        save_in(&p, &secret_sample(), Some(&store)).unwrap();
        assert!(store.deletes.borrow().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copied_cache_leaves_the_original_signed_in() {
        // `/a/token.bin` copied to `/b/token.bin`: the copy names `/a`'s
        // key. Its first save stores under its own key and keeps `/a`'s
        // item, which `/a` still uses.
        let root = test_dir("store-copied");
        let (a, b) = (root.join("a"), root.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let (pa, pb) = (a.join("token.bin"), b.join("token.bin"));
        let store = FakeStore::new();
        save_in(&pa, &secret_sample(), Some(&store)).unwrap();
        fs::copy(&pa, &pb).unwrap();
        let other = TokenSet {
            refresh_token: "COPY_RT".into(),
            ..secret_sample()
        };
        assert_eq!(
            save_in(&pb, &other, Some(&store)).unwrap(),
            Storage::SecretStore
        );
        assert!(
            store.deletes.borrow().is_empty(),
            "{:?}",
            store.deletes.borrow()
        );
        let (got, storage) = load_in(&pa, Some(&store)).unwrap().unwrap();
        assert_eq!(
            (fields(&got), storage),
            (fields(&secret_sample()), Storage::SecretStore)
        );
        let copy = load_in(&pb, Some(&store)).unwrap().unwrap().0;
        assert_eq!(copy.refresh_token, "COPY_RT");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_cache_reached_under_another_name_drops_the_old_key() {
        // The same file under two names (a symlinked directory): the old
        // name's file now holds the new reference, so the old item goes.
        let root = test_dir("store-alias");
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let (p_alias, p_real) = (alias.join("token.bin"), real.join("token.bin"));
        let store = FakeStore::new();
        save_in(&p_alias, &secret_sample(), Some(&store)).unwrap();
        save_in(&p_real, &secret_sample(), Some(&store)).unwrap();
        assert_eq!(*store.deletes.borrow(), vec![key_of(&p_alias)]);
        let keys: Vec<String> = store.items.borrow().keys().cloned().collect();
        assert_eq!(keys, vec![key_of(&p_real)]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn latched_store_keeps_asking_when_merely_unavailable() {
        let store = Latched::new(FakeStore::with_mode(Mode::Unavailable));
        assert!(store.get("k").is_err());
        store.inner.mode.set(Mode::Works);
        assert!(store.get("k").unwrap().is_none());
        assert_eq!(store.inner.gets.get(), 2);
    }

    /// A directory nothing can be written into, or `None` where a probe
    /// write still succeeds (running as root).
    #[cfg(unix)]
    fn read_only(dir: &Path) -> Option<()> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o500)).unwrap();
        let probe = dir.join("probe");
        if fs::write(&probe, b"x").is_ok() {
            let _ = fs::remove_file(&probe);
            eprintln!(
                "skipped: {} is writable while read-only (root?)",
                dir.display()
            );
            return None;
        }
        Some(())
    }

    #[cfg(unix)]
    #[test]
    fn a_reference_that_cannot_be_written_leaves_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = test_dir("store-readonly");
        let p = dir.join("token.bin");
        let t = secret_sample();
        save_in(&p, &t, None).unwrap();
        let before = fs::read(&p).unwrap();
        if read_only(&dir).is_some() {
            let store = FakeStore::new();
            // The save: the store took it, the reference did not land.
            assert!(save_in(&p, &t, Some(&store)).is_err());
            assert_eq!(fs::read(&p).unwrap(), before);
            // The migration: the token file stays and says so.
            let (got, storage) = load_in(&p, Some(&store)).unwrap().unwrap();
            assert_eq!((fields(&got), storage), (fields(&t), FILE_STORAGE));
            assert_eq!(fs::read(&p).unwrap(), before);
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
