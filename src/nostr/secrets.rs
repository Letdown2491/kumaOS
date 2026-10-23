//! The Secret Service, spoken directly.
//!
//! The vault stores through the login collection of `org.freedesktop.secrets`,
//! spoken here through zbus 6 — deliberately without the oo7 client crate,
//! whose dependency family (the standalone zvariant 5.x crates) RustSec has
//! declared unmaintained (#51), and whose release cadence would put this
//! tree's wall on someone else's calendar. The spec-level API is the contract
//! both daemons serve — gnome-keyring today, oo7-daemon from Fedora 45 — and
//! the three operations the vault needs are the whole client.
//!
//! One new connection per operation, as oo7 did before: vault operations are
//! rare (setup, unlock, destroy), and a held D-Bus connection is one more
//! file descriptor for the hardened unit to justify.
//!
//! Prompts are deliberately unimplemented. The login collection is
//! PAM-unlocked at greetd, the hardened daemon has no portal dialog to show,
//! and any reply that arrives as a prompt path instead of a real object is an
//! error naming what the gate expects to have happened already.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use zbus::{Connection, ObjectPath, OwnedObjectPath, OwnedVariant, Proxy, Variant};

const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const DEFAULT_COLLECTION: &str = "default";
const CONTENT_TYPE: &str = "application/octet-stream";

/// The root path is how the API says "a prompt is needed" where an object
/// path was expected. The gate expects PAM to have done the unlocking.
fn wants_prompt(path: &str) -> bool {
    path == "/"
}

async fn connect() -> Result<Connection> {
    zbus::Connection::session().await.context("cannot reach the session bus")
}

async fn service_proxy(conn: &Connection) -> Result<Proxy<'_>> {
    Proxy::new(conn, SERVICE, SERVICE_PATH, SERVICE_IFACE)
        .await
        .context("cannot address the Secret Service")
}

/// A plain session opened on the service. The secret struct every read and
/// write carries names the session it belongs to; "plain" wraps nothing — the
/// spec's input for it is an empty string — and the wall, not the wire, is
/// where this vault keeps its security.
async fn open_session(conn: &Connection) -> Result<OwnedObjectPath> {
    let service = service_proxy(conn).await?;
    let (_, session): (OwnedVariant, OwnedObjectPath) = service
        .call("OpenSession", &("plain", Variant::from("")))
        .await
        .context("cannot open a Secret Service session")?;
    Ok(session)
}

/// The default collection: the login keyring the spec's `default` alias
/// names. Absent is an error, not a prompt — a machine with no default
/// collection has no keyring wall at all.
async fn default_collection(conn: &Connection) -> Result<OwnedObjectPath> {
    let service = service_proxy(conn).await?;
    let collection: OwnedObjectPath = service
        .call("ReadAlias", &DEFAULT_COLLECTION)
        .await
        .context("cannot read the default collection alias")?;
    if wants_prompt(collection.as_str()) {
        bail!("the Secret Service names no default collection");
    }
    Ok(collection)
}

async fn item_proxy<'a>(conn: &'a Connection, path: &'a OwnedObjectPath) -> Result<Proxy<'a>> {
    Proxy::new(conn, SERVICE, path, ITEM_IFACE)
        .await
        .with_context(|| anyhow!("cannot address a Secret Service item ({path})"))
}

/// The items matching the attributes, exactly as the spec matches: every
/// attribute must be present and equal. Searched through the service, so a
/// locked collection answers the same as an empty one — the vault's `None`
/// on load means "no vault here yet", not "wall".
///
/// The reply's shape is the daemon's dialect: the spec says one array, and
/// gnome-keyring answers a structure of two — the second never filled. Both
/// read the same way: the first array is the answer.
async fn search_items(
    conn: &Connection,
    attributes: &HashMap<String, String>,
) -> Result<Vec<OwnedObjectPath>> {
    let service = service_proxy(conn).await?;
    let reply = service
        .call_method("SearchItems", &(attributes,))
        .await
        .context("cannot search the Secret Service")?;
    let items = match reply.body().signature().to_string().as_str() {
        "ao" => {
            let (items,): (Vec<OwnedObjectPath>,) = reply.body().deserialize()?;
            items
        }
        "(aoao)" => {
            let (items, _): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
                reply.body().deserialize()?;
            items
        }
        other => bail!("the Secret Service answered SearchItems with an unknown shape ({other})"),
    };
    Ok(items)
}

/// The bytes of an item's secret, read through the session.
async fn item_secret(
    conn: &Connection,
    item: &OwnedObjectPath,
    session: &ObjectPath<'_>,
) -> Result<Vec<u8>> {
    let proxy = item_proxy(conn, item).await?;
    let secret: (OwnedObjectPath, Vec<u8>, Vec<u8>, String) = proxy
        .call("GetSecret", &(session,))
        .await
        .with_context(|| anyhow!("the keyring item would not open ({item})"))?;
    Ok(secret.2)
}

/// Load the payload, or `None` when no vault exists here yet.
pub async fn load(attributes: &HashMap<String, String>) -> Result<Option<Vec<u8>>> {
    let conn = connect().await?;
    let session = open_session(&conn).await?;
    let items = search_items(&conn, attributes).await?;
    match items.first() {
        Some(item) => Ok(Some(item_secret(&conn, item, &session).await?)),
        None => Ok(None),
    }
}

/// Write the payload, replacing whatever was there.
pub async fn save(label: &str, attributes: &HashMap<String, String>, payload: &[u8]) -> Result<()> {
    let conn = connect().await?;
    let session = open_session(&conn).await?;
    let collection = default_collection(&conn).await?;

    let mut properties: HashMap<&str, Variant> = HashMap::new();
    properties.insert("org.freedesktop.Secret.Item.Label", Variant::from(label));
    properties.insert("org.freedesktop.Secret.Item.Attributes", Variant::from(attributes.clone()));
    let secret = (session.as_ref(), Vec::<u8>::new(), payload.to_vec(), CONTENT_TYPE);

    let collection_proxy = Proxy::new(&conn, SERVICE, collection.as_ref(), COLLECTION_IFACE)
        .await
        .context("cannot address the default collection")?;
    let (item, _prompt): (OwnedObjectPath, OwnedObjectPath) = collection_proxy
        .call("CreateItem", &(properties, secret, true))
        .await
        .context("the keyring refused the vault item")?;
    if wants_prompt(item.as_str()) {
        // A prompt path where the item was expected means the collection
        // would have to be unlocked first. The gate expects PAM to have
        // done it.
        bail!("the login collection is locked; the gate expects PAM to have unlocked it at greetd");
    }
    Ok(())
}

/// Forget the matching items entirely. Every match is deleted: the vault is
/// one item per collection by its own ruling, but the delete sweeps what the
/// search answers, so a stray cannot survive as a silent shadow vault.
pub async fn remove(attributes: &HashMap<String, String>) -> Result<()> {
    let conn = connect().await?;
    let _session = open_session(&conn).await?;
    let items = search_items(&conn, attributes).await?;
    for item in &items {
        let proxy = item_proxy(&conn, item).await?;
        // One OUT arg marshals bare — no wrapper tuple. A prompt path of
        // "/" is the spec's "nothing to prompt for": the delete is done.
        // A real prompt path means the item sits behind a lock the gate
        // expects PAM to have opened already.
        let prompt: OwnedObjectPath = proxy
            .call("Delete", &())
            .await
            .with_context(|| anyhow!("the keyring refused the delete ({item})"))?;
        if !wants_prompt(prompt.as_str()) {
            bail!("deleting the vault item would need a prompt; the gate expects PAM to have unlocked the collection at greetd");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live proof: save, load back, and remove, against whatever daemon
    /// answers the session bus. Throwaway attributes, so the proof can never
    /// touch the real vault item — different keys, different search. Ignored
    /// by default because CI has no session bus; run on hardware (or in a
    /// container with the session socket mounted):
    /// `cargo test -- --ignored live_secret_service`
    #[tokio::test]
    #[ignore = "live: needs a session bus with a Secret Service behind it"]
    async fn live_secret_service_roundtrip() {
        let attributes: HashMap<String, String> =
            [("app", "kuma-live-proof"), ("account", "throwaway")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

        save("kuma live proof", &attributes, b"the roundtrip bytes")
            .await
            .expect("save against the live Secret Service");
        let loaded = load(&attributes).await.expect("load against the live Secret Service");
        assert_eq!(loaded.as_deref(), Some(b"the roundtrip bytes".as_slice()));
        remove(&attributes).await.expect("remove against the live Secret Service");
        let gone = load(&attributes).await.expect("load after remove");
        assert_eq!(gone, None, "the proof item survived its own delete");
    }
}
