//! The token cache's store on macOS (#1877): a generic password item in
//! the user's default (login) Keychain, service `mailcore`, account the
//! cache file's absolute path, through `security-framework`.
//!
//! The simple password API sets no label, so Keychain Access lists the
//! item as `mailcore`. The Keychain ties the item's access to the code
//! identity that created it: an unsigned binary that is rebuilt is a new
//! identity, and macOS asks once whether it may read the item. That prompt
//! blocks the engine thread, never the UI (`tokencache::account` reads the
//! file only).
//!
//! No test reaches the real Keychain: a CI runner's may prompt. The logic
//! around it is the store-generic core, tested over a fake.
#![cfg_attr(test, allow(dead_code))]

use super::{SecretStore, StoreError};
use security_framework::passwords;
use std::io;

const SERVICE: &str = "mailcore";

const ERR_SEC_USER_CANCELED: i32 = -128;
const ERR_SEC_NOT_AVAILABLE: i32 = -25291;
const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25294;
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
const ERR_SEC_AUTH_FAILED: i32 = -25293;

/// The user's default Keychain.
pub struct Keychain;

impl SecretStore for Keychain {
    fn name(&self) -> &'static str {
        "keychain"
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        match passwords::get_generic_password(SERVICE, key) {
            Ok(secret) => Ok(Some(secret)),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(classify(e)),
        }
    }

    fn set(&self, key: &str, _label: &str, secret: &[u8]) -> Result<(), StoreError> {
        passwords::set_generic_password(SERVICE, key, secret).map_err(classify)
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        match passwords::delete_generic_password(SERVICE, key) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(classify(e)),
        }
    }
}

/// A Keychain error as a store error: the user cancelling or refusing the
/// prompt is `Refused`; no keychain, or one that cannot ask the user
/// (`errSecInteractionNotAllowed`: no GUI session), is `Unavailable`;
/// anything else is `Failed`.
fn classify(e: security_framework::base::Error) -> StoreError {
    let message = format!("Keychain: {e} ({})", e.code());
    match e.code() {
        ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED => StoreError::Refused(message),
        ERR_SEC_NOT_AVAILABLE | ERR_SEC_NO_SUCH_KEYCHAIN | ERR_SEC_INTERACTION_NOT_ALLOWED => {
            StoreError::Unavailable(message)
        }
        _ => StoreError::Failed(io::Error::other(message)),
    }
}
