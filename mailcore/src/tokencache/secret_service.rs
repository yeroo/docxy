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
//! [`StoreError::Refused`]. No session bus, no Secret Service, or no
//! default collection is [`StoreError::Unavailable`]; any other D-Bus error
//! is [`StoreError::Failed`].

use super::{SecretStore, StoreError};
use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::time::Duration;
use zbus::blocking::{Connection, MessageIterator};
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
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

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
    fn open() -> Result<Session, StoreError> {
        let conn = Connection::session()
            .map_err(|e| StoreError::Unavailable(format!("no D-Bus session bus: {e}")))?;
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
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), StoreError> {
        call(
            &self.conn,
            SERVICE_PATH,
            SERVICE,
            "SearchItems",
            &(attributes(key),),
        )
    }

    /// Unlocks `objects`, through the service's prompt if it asks for one.
    fn unlock(&self, objects: &[OwnedObjectPath]) -> Result<(), StoreError> {
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
            call(&self.conn, SERVICE_PATH, SERVICE, "Unlock", &(objects,))?;
        self.prompt(&prompt)
    }

    /// Runs the prompt at `path` (`/` is none) and waits for its answer.
    /// The `Completed` signal is subscribed to before the prompt is shown,
    /// so a fast answer is not missed.
    fn prompt(&self, path: &ObjectPath<'_>) -> Result<(), StoreError> {
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
        let mut signals =
            MessageIterator::for_match_rule(rule, &self.conn, Some(4)).map_err(failed)?;
        let () = call(&self.conn, path.as_str(), PROMPT, "Prompt", &("",))?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let dismissed = signals.next().map(|m| {
                m.map_err(failed).and_then(|m| {
                    m.body()
                        .deserialize::<(bool, OwnedValue)>()
                        .map(|(dismissed, _)| dismissed)
                        .map_err(failed)
                })
            });
            let _ = tx.send(dismissed);
        });
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok(Some(Ok(false))) => Ok(()),
            Ok(Some(Ok(true))) => Err(StoreError::Refused(
                "the unlock prompt was dismissed".into(),
            )),
            Ok(Some(Err(e))) => Err(e),
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(StoreError::Unavailable(
                "the Secret Service went away during its prompt".into(),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _: Result<(), _> = call(&self.conn, path.as_str(), PROMPT, "Dismiss", &());
                Err(StoreError::Refused(
                    "nobody answered the unlock prompt".into(),
                ))
            }
        }
    }

    /// Deletes the item at `item`, through a prompt if the service asks.
    fn delete_item(&self, item: &ObjectPath<'_>) -> Result<(), StoreError> {
        let prompt: OwnedObjectPath = call(&self.conn, item.as_str(), ITEM, "Delete", &())?;
        self.prompt(&prompt)
    }
}

impl SecretStore for SecretService {
    fn name(&self) -> &'static str {
        "secret-service"
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
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
            return Err(StoreError::Unavailable("the item stayed locked".into()));
        };
        let (secret,): (Secret,) = call(&s.conn, item.as_str(), ITEM, "GetSecret", &(&s.session,))?;
        Ok(Some(secret.2))
    }

    fn set(&self, key: &str, label: &str, secret: &[u8]) -> Result<(), StoreError> {
        let s = Session::open()?;
        let collection: OwnedObjectPath =
            call(&s.conn, SERVICE_PATH, SERVICE, "ReadAlias", &("default",))?;
        if collection.as_str() == "/" {
            return Err(StoreError::Unavailable("no default keyring".into()));
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
        // `get` and go stale.
        let inside = format!("{}/", collection.as_str());
        let (unlocked, locked) = s.search(key)?;
        for other in unlocked.iter().chain(&locked) {
            if other.as_str() != item.as_str() && !other.as_str().starts_with(&inside) {
                s.delete_item(other)?;
            }
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        let s = Session::open()?;
        let (unlocked, locked) = s.search(key)?;
        for item in unlocked.iter().chain(&locked) {
            s.delete_item(item)?;
        }
        Ok(())
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
) -> Result<R, StoreError>
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
/// owns or can activate `org.freedesktop.secrets`) is `Unavailable`;
/// anything else is `Failed`.
fn classify(e: zbus::Error) -> StoreError {
    let unknown = |name: &str| {
        matches!(
            name,
            "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
                | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound"
        )
    };
    match &e {
        zbus::Error::MethodError(name, _, _) if unknown(name.as_str()) => {
            StoreError::Unavailable(format!("no Secret Service: {e}"))
        }
        zbus::Error::FDO(fdo)
            if matches!(
                **fdo,
                zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
            ) =>
        {
            StoreError::Unavailable(format!("no Secret Service: {e}"))
        }
        _ => failed(e),
    }
}

fn failed(e: impl std::fmt::Display) -> StoreError {
    StoreError::Failed(io::Error::other(format!("Secret Service: {e}")))
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
        let _ = std::fs::remove_dir_all(&dir);
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
