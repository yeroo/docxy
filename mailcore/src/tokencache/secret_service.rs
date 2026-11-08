//! The token cache's store on Linux (#1877): the Secret Service
//! (`org.freedesktop.secrets`, which GNOME Keyring and KWallet provide) over
//! the D-Bus session bus, through `zbus`'s pure-Rust blocking client.
//!
//! The session is `plain`: the secret crosses the local session bus
//! unencrypted, as libsecret's does without its DH option, so no crypto
//! crates. Each call connects afresh; nothing is held between calls.
//!
//! An item carries the attributes `xdg:schema = mailcore.TokenCache`,
//! `application = mailcore` and `cache = <key>` (the cache file's absolute
//! path), and is created in the `default` collection (the login keyring).
//! A locked item or collection is unlocked through the service's prompt,
//! waited for up to [`PROMPT_TIMEOUT`]; dismissed or unanswered is
//! [`SecretStoreError::Refused`], and so is a method call the service does
//! not answer within [`METHOD_TIMEOUT`] (a wedged service), so the latch
//! stops asking it. No session bus, no Secret Service, or no default
//! collection is [`SecretStoreError::Unavailable`]; any other D-Bus error is
//! [`SecretStoreError::Failed`]. Locked items are unlocked before they are
//! read or deleted (deleting a locked item fails with `IsLocked`).

use super::{SecretStore, SecretStoreError};
use futures_lite::StreamExt;
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";

/// The `xdg:schema` attribute, which libsecret and Seahorse group items by.
const SCHEMA: &str = "mailcore.TokenCache";
/// The `application` attribute.
const APPLICATION: &str = "mailcore";

/// How long an unlock prompt may stay unanswered before the call gives up
/// (and the prompt is dismissed).
pub(crate) const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one D-Bus method call may wait for the service's reply: zbus
/// waits forever by default, which a wedged service would turn into an
/// engine thread blocked for good.
pub(crate) const METHOD_TIMEOUT: Duration = Duration::from_secs(25);

/// The Secret Service on the session bus.
pub struct SecretService;

/// A connection with an open `plain` session.
struct Session {
    conn: Connection,
    session: OwnedObjectPath,
}

/// The secret struct `(oayays)`: session, parameters, value, content type.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

impl Session {
    fn open() -> Result<Session, SecretStoreError> {
        let conn = zbus::blocking::connection::Builder::session()
            .map(|b| b.method_timeout(METHOD_TIMEOUT))
            .and_then(|b| b.build())
            .map_err(|e| SecretStoreError::Unavailable(format!("no D-Bus session bus: {e}")))?;
        let (_, session): (OwnedValue, OwnedObjectPath) = call(
            &conn,
            SERVICE_PATH,
            SERVICE,
            "OpenSession",
            &("plain", Value::from("")),
        )?;
        Ok(Session { conn, session })
    }

    /// The items whose attributes match `key`'s: `(unlocked, locked)`.
    fn search(
        &self,
        key: &str,
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), SecretStoreError> {
        call(
            &self.conn,
            SERVICE_PATH,
            SERVICE,
            "SearchItems",
            &(attributes(key),),
        )
    }

    /// Unlocks `objects`, through the service's prompt if it asks for one.
    fn unlock(&self, objects: &[OwnedObjectPath]) -> Result<(), SecretStoreError> {
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
            call(&self.conn, SERVICE_PATH, SERVICE, "Unlock", &(objects,))?;
        self.prompt(&prompt)
    }

    /// Runs the prompt at `path` (`/` is none) and waits for its answer.
    /// The `Completed` signal is subscribed to before the prompt is shown,
    /// so a fast answer is not missed. The wait races a timer on this
    /// thread, and the subscription is dropped on return: nothing outlives
    /// the call, answered or not.
    fn prompt(&self, path: &ObjectPath<'_>) -> Result<(), SecretStoreError> {
        if path.as_str() == "/" {
            return Ok(());
        }
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(PROMPT)
            .and_then(|b| b.member("Completed"))
            .and_then(|b| b.path(path.to_owned()))
            .map_err(failed)?
            .build();
        let mut signals = futures_lite::future::block_on(zbus::MessageStream::for_match_rule(
            rule,
            self.conn.inner(),
            Some(4),
        ))
        .map_err(failed)?;
        let () = call(&self.conn, path.as_str(), PROMPT, "Prompt", &("",))?;
        let answer = futures_lite::future::block_on(futures_lite::future::or(
            async { Some(signals.next().await) },
            async {
                async_io::Timer::after(PROMPT_TIMEOUT).await;
                None
            },
        ));
        match answer {
            Some(Some(Ok(m))) => {
                let (dismissed, _) = m
                    .body()
                    .deserialize::<(bool, OwnedValue)>()
                    .map_err(failed)?;
                if dismissed {
                    Err(SecretStoreError::Refused(
                        "the unlock prompt was dismissed".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            Some(Some(Err(e))) => Err(failed(e)),
            Some(None) => Err(SecretStoreError::Unavailable(
                "the Secret Service went away during its prompt".into(),
            )),
            None => {
                let _: Result<(), _> = call(&self.conn, path.as_str(), PROMPT, "Dismiss", &());
                Err(SecretStoreError::Refused(
                    "nobody answered the unlock prompt".into(),
                ))
            }
        }
    }

    /// Deletes `key`'s unlocked items other than `kept` outside
    /// `collection`, never prompting: a locked twin is left to sign-out's
    /// `delete`, which unlocks. A `Delete` that asks for a prompt is left
    /// unanswered. Every item is tried; the first error is returned.
    fn delete_unlocked_twins(
        &self,
        key: &str,
        kept: &ObjectPath<'_>,
        collection: &str,
    ) -> Result<(), SecretStoreError> {
        let inside = format!("{collection}/");
        let (unlocked, _) = self.search(key)?;
        let mut first = None;
        for item in unlocked
            .iter()
            .filter(|i| i.as_str() != kept.as_str() && !i.as_str().starts_with(&inside))
        {
            let deleted: Result<OwnedObjectPath, _> =
                call(&self.conn, item.as_str(), ITEM, "Delete", &());
            if let Err(e) = deleted {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Deletes every item of `items`, each tried; the first error is
    /// returned.
    fn delete_all(&self, items: &[OwnedObjectPath]) -> Result<(), SecretStoreError> {
        let mut first = None;
        for item in items {
            if let Err(e) = self.delete_item(item) {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Deletes the item at `item`, through a prompt if the service asks.
    fn delete_item(&self, item: &ObjectPath<'_>) -> Result<(), SecretStoreError> {
        let prompt: OwnedObjectPath = call(&self.conn, item.as_str(), ITEM, "Delete", &())?;
        self.prompt(&prompt)
    }
}

impl SecretStore for SecretService {
    fn name(&self) -> &'static str {
        "secret-service"
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let s = Session::open()?;
        let (mut unlocked, locked) = s.search(key)?;
        if unlocked.is_empty() {
            if locked.is_empty() {
                return Ok(None);
            }
            s.unlock(&locked)?;
            unlocked = s.search(key)?.0;
        }
        let Some(item) = unlocked.first() else {
            return Err(SecretStoreError::Unavailable(
                "the item stayed locked".into(),
            ));
        };
        let (secret,): (Secret,) = call(&s.conn, item.as_str(), ITEM, "GetSecret", &(&s.session,))?;
        Ok(Some(secret.2))
    }

    fn set(&self, key: &str, label: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
        let s = Session::open()?;
        let collection: OwnedObjectPath =
            call(&s.conn, SERVICE_PATH, SERVICE, "ReadAlias", &("default",))?;
        if collection.as_str() == "/" {
            return Err(SecretStoreError::Unavailable("no default keyring".into()));
        }
        s.unlock(std::slice::from_ref(&collection))?;
        let mut props: HashMap<&str, Value<'_>> = HashMap::new();
        props.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
        props.insert(
            "org.freedesktop.Secret.Item.Attributes",
            Value::from(attributes(key)),
        );
        let value: Secret = (
            s.session.clone(),
            Vec::new(),
            secret.to_vec(),
            "application/json".to_string(),
        );
        let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = call(
            &s.conn,
            collection.as_str(),
            COLLECTION,
            "CreateItem",
            &(props, value, true),
        )?;
        s.prompt(&prompt)?;
        // `replace` replaces only within the default collection: a twin a
        // user moved to another keyring would otherwise be read first by
        // `get` and go stale. Best effort and without a prompt: the token
        // is stored, so a failure here must not make the save fall back to
        // the file, and a locked twin must not raise a prompt on every
        // save (sign-out removes it).
        let _ = s.delete_unlocked_twins(key, &item, collection.as_str());
        Ok(())
    }

    /// Deletes every item of `key`: the unlocked ones first, so a locked
    /// twin whose unlock prompt is refused cannot keep the live token;
    /// then the locked ones, unlocked first (a locked item's `Delete`
    /// fails with `IsLocked`). Everything is tried; the first error is
    /// returned.
    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        let s = Session::open()?;
        let (unlocked, locked) = s.search(key)?;
        let mut first = s.delete_all(&unlocked).err();
        if !locked.is_empty()
            && let Err(e) = s.unlock(&locked).and_then(|()| s.delete_all(&locked))
        {
            first.get_or_insert(e);
        }
        first.map_or(Ok(()), Err)
    }
}

/// The attributes of `key`'s item.
fn attributes(key: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("xdg:schema", SCHEMA),
        ("application", APPLICATION),
        ("cache", key),
    ])
}

/// Calls `method` on the Secret Service object at `path` and reads its
/// reply as `R`.
fn call<B, R>(
    conn: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> Result<R, SecretStoreError>
where
    B: zbus::export::serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let reply = conn
        .call_method(Some(DEST), path, Some(iface), method, body)
        .map_err(classify)?;
    reply.body().deserialize::<R>().map_err(failed)
}

/// A D-Bus error as a store error: no Secret Service on the bus (nothing
/// owns or can activate `org.freedesktop.secrets`) is `Unavailable`; a call
/// not answered within [`METHOD_TIMEOUT`] (`InputOutput(TimedOut)`) is
/// `Refused`, so the latch stops asking; anything else is `Failed`.
fn classify(e: zbus::Error) -> SecretStoreError {
    let unknown = |name: &str| {
        matches!(
            name,
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
                | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
        )
    };
    match &e {
        zbus::Error::InputOutput(io) if io.kind() == io::ErrorKind::TimedOut => {
            SecretStoreError::Refused(format!("the Secret Service did not answer: {e}"))
        }
        zbus::Error::MethodError(name, _, _) if unknown(name.as_str()) => {
            SecretStoreError::Unavailable(format!("no Secret Service: {e}"))
        }
        zbus::Error::FDO(fdo)
            if matches!(
                **fdo,
                zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
            ) =>
        {
            SecretStoreError::Unavailable(format!("no Secret Service: {e}"))
        }
        _ => failed(e),
    }
}

fn failed(e: impl std::fmt::Display) -> SecretStoreError {
    SecretStoreError::Failed(io::Error::other(format!("Secret Service: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::TokenSet;
    use crate::tokencache::{Storage, clear_in, load_in, save_in};

    /// Against a real Secret Service, only with
    /// `MAILCORE_SECRET_SERVICE_TEST=1` (CI runs it under `dbus-run-session`
    /// with a throwaway `gnome-keyring-daemon`). Not through `Latched`, so a
    /// refusal fails loudly.
    #[test]
    fn real_roundtrip() {
        if std::env::var_os("MAILCORE_SECRET_SERVICE_TEST").is_none_or(|v| v != "1") {
            eprintln!("skipped: set MAILCORE_SECRET_SERVICE_TEST=1 to run against a real keyring");
            return;
        }
        let store = SecretService;
        let dir =
            std::env::temp_dir().join(format!("mailcore-secret-service-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token.bin");
        let key = std::path::absolute(&path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let t = TokenSet {
            access_token: "REAL_AT_VALUE".into(),
            refresh_token: "REAL_RT_VALUE".into(),
            expires_at_unix: 42,
            account: "me@x".into(),
            scopes: Some(vec!["Mail.Read".into()]),
        };

        assert_eq!(
            save_in(&path, &t, Some(&store)).unwrap(),
            Storage::SecretStore
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("REAL_RT_VALUE") && !raw.contains("REAL_AT_VALUE"),
            "{raw}"
        );
        let (got, storage) = load_in(&path, Some(&store)).unwrap().unwrap();
        assert_eq!(storage, Storage::SecretStore);
        assert_eq!(fields(&got), fields(&t));

        // A second save replaces the one item.
        let t2 = TokenSet {
            refresh_token: "REAL_RT_2".into(),
            scopes: t.scopes.clone(),
            ..sample_like(&t)
        };
        save_in(&path, &t2, Some(&store)).unwrap();
        let s = Session::open().unwrap();
        let (unlocked, locked) = s.search(&key).unwrap();
        assert_eq!(unlocked.len() + locked.len(), 1);
        let got = load_in(&path, Some(&store)).unwrap().unwrap().0;
        assert_eq!(fields(&got), fields(&t2));

        clear_in(&path, Some(&store)).unwrap();
        assert!(!path.exists());
        assert_eq!(store.get(&key).unwrap(), None);

        // A plaintext file from before #1877 migrates.
        save_in(&path, &t, None).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("REAL_RT_VALUE")
        );
        let (got, storage) = load_in(&path, Some(&store)).unwrap().unwrap();
        assert_eq!((fields(&got), storage), (fields(&t), Storage::SecretStore));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("REAL_RT_VALUE")
        );
        assert!(store.get(&key).unwrap().is_some());
        clear_in(&path, Some(&store)).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);

        // A locked keyring: sign-out unlocks the item before deleting it
        // (a locked item's Delete fails with IsLocked).
        save_in(&path, &t, Some(&store)).unwrap();
        let collection: OwnedObjectPath =
            call(&s.conn, SERVICE_PATH, SERVICE, "ReadAlias", &("default",)).unwrap();
        let _: (Vec<OwnedObjectPath>, OwnedObjectPath) = call(
            &s.conn,
            SERVICE_PATH,
            SERVICE,
            "Lock",
            &(vec![collection.clone()],),
        )
        .unwrap();
        let (unlocked, locked) = s.search(&key).unwrap();
        assert_eq!((unlocked.len(), locked.len()), (0, 1), "the item is locked");
        match clear_in(&path, Some(&store)) {
            // A prompter answered (a desktop session).
            Ok(()) => {}
            // Headless, gnome-keyring dismisses the unlock prompt at once.
            // The sign-out asked to unlock (a Delete of the locked item
            // would have failed with IsLocked, a `Failed`), and the file
            // is gone; once the keyring is open again, the next sign-out
            // deletes the item.
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotConnected, "{e}");
                assert!(e.to_string().contains("dismissed"), "{e}");
                assert!(!path.exists());
                unlock_with_password(&s, &collection, KEYRING_PASSWORD);
                clear_in(&path, Some(&store)).unwrap();
            }
        }
        let (unlocked, locked) = s.search(&key).unwrap();
        assert_eq!(unlocked.len() + locked.len(), 0, "the locked item is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_are_classified() {
        // A method call the service does not answer in time: Refused, so
        // the latch stops asking a wedged service.
        let timeout = zbus::Error::InputOutput(std::sync::Arc::new(io::Error::from(
            io::ErrorKind::TimedOut,
        )));
        assert!(matches!(classify(timeout), SecretStoreError::Refused(_)));
        // Nothing owns org.freedesktop.secrets: Unavailable.
        let unknown = zbus::Error::FDO(Box::new(zbus::fdo::Error::ServiceUnknown(
            "org.freedesktop.secrets".into(),
        )));
        assert!(matches!(
            classify(unknown),
            SecretStoreError::Unavailable(_)
        ));
        // Anything else: Failed.
        let other = zbus::Error::InputOutput(std::sync::Arc::new(io::Error::from(
            io::ErrorKind::BrokenPipe,
        )));
        assert!(matches!(classify(other), SecretStoreError::Failed(_)));
    }

    /// The throwaway keyring's password in CI's recipe
    /// (`echo -n ci-pass | gnome-keyring-daemon --unlock …`).
    const KEYRING_PASSWORD: &str = "ci-pass";

    /// Unlocks `collection` with its password, with no prompt, through
    /// gnome-keyring's own interface (what Seahorse uses).
    fn unlock_with_password(s: &Session, collection: &ObjectPath<'_>, password: &str) {
        let master: Secret = (
            s.session.clone(),
            Vec::new(),
            password.as_bytes().to_vec(),
            "text/plain".to_string(),
        );
        let () = call(
            &s.conn,
            SERVICE_PATH,
            "org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface",
            "UnlockWithMasterPassword",
            &(collection, master),
        )
        .unwrap();
    }

    type Fields = (String, String, u64, String, Option<Vec<String>>);

    /// `t`'s fields, comparable (`TokenSet` has no `Debug`, so it is never
    /// printed).
    fn fields(t: &TokenSet) -> Fields {
        (
            t.access_token.clone(),
            t.refresh_token.clone(),
            t.expires_at_unix,
            t.account.clone(),
            t.scopes.clone(),
        )
    }

    fn sample_like(t: &TokenSet) -> TokenSet {
        TokenSet {
            access_token: t.access_token.clone(),
            refresh_token: t.refresh_token.clone(),
            expires_at_unix: t.expires_at_unix,
            account: t.account.clone(),
            scopes: t.scopes.clone(),
        }
    }
}
