//! The vault: where the daemon's nostr key lives when it is not in
//! memory.
//!
//! The design is a gate-style lock. The Secret Service's login
//! collection is the wall — it is already
//! PAM-unlocked at greetd, so a fresh machine needs no second secret and
//! no new passphrase UX — and the vault is honest about that being a
//! gate: `lock` drops the key from memory and refuses to sign, `unlock`
//! re-reads it, and neither pretends to survive an attacker who is
//! already running as the user with the session unlocked. The upgrade to
//! an independent-passphrase vault later is a change of what fills the
//! same blob, not a schema change.
//!
//! What the keyring holds is not a bare secret key but a NIP-49
//! `ncryptsec` wrapped with a random passphrase carried beside it. Today
//! that wrap adds nothing the keyring does not already provide — the
//! wall and the wrap live in the same item — and that is the point: the
//! stored format is the format the future mode needs, so upgrading the
//! wall never migrates data.
//!
//! Storage itself sits behind [`SecretStore`] so every behavior the
//! daemon will get is testable offline against an in-memory store; the
//! real backend is the oo7 adapter at the bottom of this file, which is
//! compile-checked here and exercised by the smoke stage on a machine
//! that has a Secret Service.

use anyhow::{anyhow, bail, Context, Result};
use nostr::key::PublicKey;
use nostr::key::SecretKey;
use nostr::nips::nip19::ToBech32;
use serde::{Deserialize, Serialize};

use super::keys;

/// The label the Secret Service item carries, and the attributes it is
/// found by. One vault per login collection: the daemon has one key, the
/// thing a paired app is talking to is *the* bunker, and a second
/// concurrent vault is a way to sign with the wrong identity.
pub const VAULT_LABEL: &str = "kuma-nostr vault";

/// The attributes every store operation searches and writes by. These
/// are metadata the Secret Service holds in the clear; they name the
/// item, never its contents.
pub const VAULT_ATTRIBUTES: [(&str, &str); 2] = [("app", "kuma"), ("account", "nostr-vault")];

/// The bytes inside the keyring item. Versioned so the
/// independent-passphrase upgrade is a new version beside the old one
/// rather than a reinterpretation of the same bytes.
///
/// `wrap` is the random passphrase the `ncryptsec` was wrapped with —
/// kept here, beside it, because in gate mode the wall is the keyring
/// and a second item would be one more thing to lose. The independent
/// vault removes this field and asks a person instead; the `ncryptsec`
/// format does not change.
///
/// `pubkey` rides in the clear because it is not secret and because a
/// locked daemon still owes the surfaces an identity.
///
/// `secrets` are the outstanding one-time pairing secrets — one per
/// minted URI, burned by the connect that used it. Version 2 carried
/// one reusable nonce; version 3 makes every URI one-shot: a minted
/// pairing pairs one app once, and a second connect with the same
/// secret is refused. A version 2 blob migrates on first read — its
/// nonce becomes one outstanding secret, so a URI printed before the
/// upgrade still works, once.
///
/// `labels` (version 4) name the app a minted URI is for — the
/// person's word at mint time, which the connect that burns the
/// secret pairs under. A map beside the secrets, not a second list:
/// a secret without a label is the common case, and the client's own
/// metadata claim is the fallback. Version 3 blobs read with an
/// empty map and need no migration.
#[derive(Serialize, Deserialize)]
struct VaultBlob {
    v: u8,
    wrap: String,
    ncryptsec: String,
    pubkey: String,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
}

const BLOB_VERSION: u8 = 4;
/// The versions this binary reads and migrates: 3 made every URI
/// one-shot (its blobs read with an empty label map), 2 carried one
/// reusable nonce, 1 carried none at all. Anything else is refused,
/// because a future format read as this one is a key silently misread.
const BLOB_VERSION_THREE: u8 = 3;
const BLOB_VERSION_TWO: u8 = 2;
const BLOB_VERSION_ONE: u8 = 1;

impl VaultBlob {
    fn new(key: &SecretKey, wrap: String, secrets: Vec<String>) -> Result<Self> {
        let ncryptsec = keys::to_ncryptsec(key, &wrap)?.to_bech32()?;
        let pubkey = keys::public_key_hex(key);
        Ok(Self {
            v: BLOB_VERSION,
            wrap,
            ncryptsec,
            pubkey,
            secrets,
            labels: std::collections::HashMap::new(),
        })
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let blob: Self = serde_json::from_slice(bytes)?;
        if blob.v != BLOB_VERSION
            && blob.v != BLOB_VERSION_THREE
            && blob.v != BLOB_VERSION_TWO
            && blob.v != BLOB_VERSION_ONE
        {
            bail!("vault blob is version {}, this binary reads {BLOB_VERSION}", blob.v);
        }
        Ok(blob)
    }
}

/// Where the vault's bytes live. Async because the real backend is the
/// Secret Service over D-Bus; every implementor is honest about failing
/// when the service is absent rather than pretending to have stored
/// something.
///
/// Not dyn-compatible on purpose: the vault is generic over its store,
/// and the daemon holds its backend as a concrete type. An object-safe
/// seam would invite runtime backends, and the choice of wall is not a
/// runtime decision. The `async fn in trait` form is kept over the
/// desugared `impl Future + Send` spelling for the same reason in the
/// other direction: these futures are awaited on the runtime that owns
/// the store and are never shipped across a thread, and a caller that
/// someday needs `Send` should be making that a reviewed signature
/// change, not inheriting a silent bound.
#[allow(async_fn_in_trait)]
pub trait SecretStore {
    /// The stored payload, or `None` when no vault exists here yet.
    async fn load(&self) -> Result<Option<Vec<u8>>>;
    /// Write the payload, replacing whatever was there.
    async fn save(&self, payload: &[u8]) -> Result<()>;
    /// Forget the vault entirely. The key it held is gone when this
    /// returns; that is what "delete the vault" means and the caller
    /// should have said so already.
    async fn remove(&self) -> Result<()>;
}

/// An in-memory store: the vault's behaviors without a Secret Service.
/// What the offline suite runs against, and what a future session-only
/// mode would want if one ever earns its keep.
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<Option<Vec<u8>>>);

/// Shared ownership, so a test can hold the store a vault borrows and
/// read what the vault wrote — the migration proof's window.
impl SecretStore for std::sync::Arc<MemoryStore> {
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        (**self).load().await
    }

    async fn save(&self, payload: &[u8]) -> Result<()> {
        (**self).save(payload).await
    }

    async fn remove(&self) -> Result<()> {
        (**self).remove().await
    }
}

impl SecretStore for MemoryStore {
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.0.lock().expect("memory store lock").clone())
    }

    async fn save(&self, payload: &[u8]) -> Result<()> {
        *self.0.lock().expect("memory store lock") = Some(payload.to_vec());
        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        *self.0.lock().expect("memory store lock") = None;
        Ok(())
    }
}

impl MemoryStore {
    /// The stored bytes, for the tests that prove what a migration
    /// wrote.
    #[cfg(test)]
    fn peek(&self) -> Option<Vec<u8>> {
        self.0.lock().expect("memory store lock").clone()
    }
}

/// The gate. Holds the store and — when unlocked — the key. Cloning the
/// key out is deliberately not offered: callers sign through the vault
/// so that a lock is a lock, and the daemon's request loop will borrow
/// for the duration of one signature.
pub struct Vault<S: SecretStore> {
    store: S,
    key: Option<SecretKey>,
    /// The outstanding one-time pairing secrets, as the blob last said.
    /// Not deep secrets — they ride in URIs a person copies — but they
    /// are the daemon's to mint and burn, and nobody else's to guess.
    secrets: Vec<String>,
    /// The names the person gave outstanding secrets at mint, as the
    /// blob last said. A secret without an entry here pairs under the
    /// client's own metadata claim, or under nothing.
    labels: std::collections::HashMap<String, String>,
}

impl<S: SecretStore> Vault<S> {
    pub fn new(store: S) -> Self {
        Self { store, key: None, secrets: Vec::new(), labels: std::collections::HashMap::new() }
    }

    pub fn is_unlocked(&self) -> bool {
        self.key.is_some()
    }

    /// The outstanding one-time secrets, for arming the bunker's
    /// connect verification. `None`-shaped as an empty slice before
    /// the blob's first read — a bunker armed against nothing lets
    /// the person's gate be the door, which is the nonce-less
    /// behavior the layer has always had.
    pub fn secrets(&self) -> &[String] {
        &self.secrets
    }

    /// The outstanding secrets with the names the person minted them
    /// under, in pair order — what the bunker arms against, so a
    /// connect that burns a secret pairs under the name the URI
    /// carried. An unlabeled secret pairs under the client's own
    /// metadata claim, or under nothing.
    pub fn outstanding(&self) -> Vec<super::bunker::Outstanding> {
        self.secrets
            .iter()
            .map(|secret| super::bunker::Outstanding {
                secret: secret.clone(),
                label: self.labels.get(secret).cloned(),
            })
            .collect()
    }

    /// The secret the pairing URI advertises: the latest mint, while
    /// it is still outstanding. A burned URI is not advertised — a
    /// status that showed a dead URI would be a URI that lies.
    pub fn uri_secret(&self) -> Option<&String> {
        self.secrets.last()
    }

    /// Whether a vault exists in the store, without opening it: what
    /// `status` reports and what distinguishes "locked" from "never set
    /// up" for every caller downstream.
    pub async fn stored(&self) -> Result<bool> {
        Ok(self.store.load().await?.is_some())
    }

    /// The bunker's public identity, read from the blob without
    /// unlocking: what `status` names while locked and what the doctor
    /// grades without asking the gate to open. `None` when no vault
    /// exists.
    pub async fn stored_pubkey(&self) -> Result<Option<PublicKey>> {
        match self.store.load().await? {
            Some(bytes) => {
                let blob = VaultBlob::decode(&bytes)?;
                let pubkey = PublicKey::parse(&blob.pubkey)
                    .map_err(|e| anyhow!("the stored pubkey is broken: {e}"))?;
                Ok(Some(pubkey))
            }
            None => Ok(None),
        }
    }

    /// The key, borrowed only while unlocked. A locked vault answers
    /// `None`, and every caller treats that as the refusal it is.
    pub fn key(&self) -> Option<&SecretKey> {
        self.key.as_ref()
    }

    /// First provisioning: wrap the key and store it, leaving the vault
    /// unlocked. Refuses to overwrite an existing vault — replacing a
    /// key is `destroy` followed by `setup`, spelled, because the
    /// mistake it prevents is signing under an identity nobody
    /// remembers choosing.
    pub async fn setup(&mut self, key: &SecretKey) -> Result<()> {
        if self.store.load().await?.is_some() {
            bail!("a vault already exists; destroy it first");
        }
        self.store_and_unlock(key).await
    }

    /// Re-read the key from storage. Idempotent on an already-unlocked
    /// vault — the CLI verb answers "already unlocked" the same way.
    /// A version 1 blob migrates (a nonce minted and persisted beside
    /// the key), a version 2 blob migrates (its reusable nonce
    /// becomes one outstanding one-time secret, so an old URI works
    /// once more — exactly once), and a version 3 blob migrates by
    /// gaining the label map, empty.
    pub async fn unlock(&mut self) -> Result<()> {
        if self.key.is_some() {
            return Ok(());
        }
        let bytes =
            self.store.load().await?.ok_or_else(|| anyhow!("no vault exists in this store"))?;
        let mut blob = VaultBlob::decode(&bytes)?;
        // The key opens before anything migrates: a passphrase that
        // fails is a failure that changed nothing.
        let key = keys::decrypt_ncryptsec(&blob.ncryptsec, &blob.wrap)?;
        if blob.v != BLOB_VERSION {
            match blob.v {
                // The v2 shape's reusable nonce is read from the raw
                // bytes — the v3 struct's default swallowed it at
                // decode. It becomes one outstanding one-time secret:
                // a URI printed before the upgrade works once more,
                // exactly once.
                BLOB_VERSION_TWO => {
                    let raw: serde_json::Value = serde_json::from_slice(&bytes)
                        .context("reading the version 2 blob's own shape")?;
                    blob.secrets =
                        raw["secret"].as_str().map(|s| vec![s.to_string()]).unwrap_or_default();
                }
                // The v1 shape had no nonce at all: one is minted now,
                // so a URI exists for the bunker to verify against.
                BLOB_VERSION_ONE => blob.secrets = vec![generate_wrap()?],
                // The v3 shape is this one without the label map: the
                // empty map the decode defaulted in is the whole
                // migration. Nothing to move.
                BLOB_VERSION_THREE => {}
                _ => bail!("a blob version decoded but cannot migrate"),
            }
            blob.v = BLOB_VERSION;
        }
        if blob.secrets.is_empty() {
            blob.secrets = vec![generate_wrap()?];
        }
        self.store.save(&serde_json::to_vec(&blob).context("serializing the vault blob")?).await?;
        self.secrets = blob.secrets;
        self.labels = blob.labels;
        self.key = Some(key);
        Ok(())
    }

    /// Drop the key from memory. The blob stays; `unlock` brings the
    /// same identity back. The outstanding secrets stay too — a lock
    /// is momentary, and a URI that outlived the lock still pairs.
    pub fn lock(&mut self) {
        self.key = None;
    }

    /// Mint a fresh one-time pairing secret and persist it. The secret
    /// is what the new URI carries and the connect burns; minting does
    /// not touch the key, the outstanding others, or anything else —
    /// a second URI is a second door, not a replacement. The label is
    /// the person's name for the app the URI is for, riding the secret
    /// so the connect that burns it pairs under that name.
    pub async fn mint_secret(&mut self, label: Option<String>) -> Result<String> {
        let bytes =
            self.store.load().await?.ok_or_else(|| anyhow!("no vault exists in this store"))?;
        let mut blob = VaultBlob::decode(&bytes)?;
        let fresh = generate_wrap()?;
        if let Some(label) = &label {
            blob.labels.insert(fresh.clone(), label.clone());
        }
        blob.secrets.push(fresh.clone());
        self.store.save(&serde_json::to_vec(&blob).context("serializing the vault blob")?).await?;
        self.secrets = blob.secrets;
        self.labels = blob.labels;
        Ok(fresh)
    }

    /// Burn a one-time secret: the connect that verified against it
    /// used it up. Answers whether it was outstanding, so the caller
    /// can tell an honest burn from a repeat. The label goes with it —
    /// a burned URI's name is not anyone's business.
    pub async fn burn_secret(&mut self, secret: &str) -> Result<bool> {
        let bytes =
            self.store.load().await?.ok_or_else(|| anyhow!("no vault exists in this store"))?;
        let mut blob = VaultBlob::decode(&bytes)?;
        let before = blob.secrets.len();
        blob.secrets.retain(|s| s != secret);
        if blob.secrets.len() == before {
            return Ok(false);
        }
        blob.labels.remove(secret);
        self.store.save(&serde_json::to_vec(&blob).context("serializing the vault blob")?).await?;
        self.secrets = blob.secrets;
        self.labels = blob.labels;
        Ok(true)
    }

    /// Mint a fresh one-time secret and invalidate every outstanding
    /// one: every URI printed before this call points at a secret the
    /// bunker no longer answers. The key is untouched — rotation is a
    /// front-door surgery, not a re-provisioning. The labels go with
    /// the secrets they named; the fresh URI has no name yet.
    pub async fn rotate_secret(&mut self) -> Result<String> {
        let bytes =
            self.store.load().await?.ok_or_else(|| anyhow!("no vault exists in this store"))?;
        let mut blob = VaultBlob::decode(&bytes)?;
        let fresh = generate_wrap()?;
        blob.secrets = vec![fresh.clone()];
        blob.labels = std::collections::HashMap::new();
        blob.v = BLOB_VERSION;
        self.store.save(&serde_json::to_vec(&blob).context("serializing the vault blob")?).await?;
        self.secrets = blob.secrets;
        self.labels = blob.labels;
        Ok(fresh)
    }

    /// Forget the vault: the stored blob and the in-memory key both go.
    /// The key is unrecoverable afterwards, which is the contract; the
    /// outstanding secrets go with it, because a URI that outlived its
    /// vault would point at nothing.
    pub async fn destroy(&mut self) -> Result<()> {
        self.lock();
        self.secrets = Vec::new();
        self.labels = std::collections::HashMap::new();
        self.store.remove().await
    }

    /// The shared body of `setup` and a future import-with-replace:
    /// wrap, store, hold. One outstanding secret rides with the first
    /// provisioning — the first URI's secret, minted before anything
    /// could ask for it.
    async fn store_and_unlock(&mut self, key: &SecretKey) -> Result<()> {
        let wrap = generate_wrap()?;
        let blob = VaultBlob::new(key, wrap, vec![generate_wrap()?])?;
        self.store.save(&serde_json::to_vec(&blob).context("serializing the vault blob")?).await?;
        self.secrets = blob.secrets;
        self.key = Some(key.clone());
        Ok(())
    }
}

/// The random wrap passphrase: 32 bytes from the OS RNG, hex-encoded.
/// Hex rather than anything pronounceable — nobody is asked to type it,
/// and its job is to be long enough that NIP-49's passphrase stretching
/// is never the weak link.
fn generate_wrap() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("OS randomness unavailable: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The real store: the Secret Service's login collection through oo7.
/// A new connection per operation, deliberately — vault operations are
/// rare (setup, unlock, destroy), and a held D-Bus connection is one
/// more file descriptor for the hardened unit to justify.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    async fn load(&self) -> Result<Option<Vec<u8>>> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| anyhow!("cannot reach the Secret Service: {e}"))?;
        let items = keyring
            .search_items(&VAULT_ATTRIBUTES)
            .await
            .map_err(|e| anyhow!("cannot search the login collection: {e}"))?;
        match items.first() {
            Some(item) => {
                let secret = item
                    .secret()
                    .await
                    .map_err(|e| anyhow!("the vault item would not open: {e}"))?;
                Ok(Some(secret.as_bytes().to_vec()))
            }
            None => Ok(None),
        }
    }

    async fn save(&self, payload: &[u8]) -> Result<()> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| anyhow!("cannot reach the Secret Service: {e}"))?;
        keyring
            .create_item(VAULT_LABEL, &VAULT_ATTRIBUTES, oo7::Secret::blob(payload), true)
            .await
            .map_err(|e| anyhow!("the keyring refused the vault item: {e}"))?;
        Ok(())
    }

    async fn remove(&self) -> Result<()> {
        let keyring = oo7::Keyring::new()
            .await
            .map_err(|e| anyhow!("cannot reach the Secret Service: {e}"))?;
        keyring
            .delete(&VAULT_ATTRIBUTES)
            .await
            .map_err(|e| anyhow!("the keyring refused the delete: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn unlocked_vault(key: &SecretKey) -> Vault<MemoryStore> {
        let mut vault = Vault::new(MemoryStore::default());
        vault.setup(key).await.expect("setup on an empty store");
        vault
    }

    #[tokio::test]
    async fn the_gate_opens_and_closes_on_one_key() {
        let key = SecretKey::generate();
        let mut vault = unlocked_vault(&key).await;
        assert!(vault.is_unlocked());
        assert_eq!(vault.key(), Some(&key));

        vault.lock();
        assert!(!vault.is_unlocked());
        assert_eq!(vault.key(), None, "a locked vault holds no key");

        vault.unlock().await.unwrap();
        assert_eq!(vault.key(), Some(&key), "unlock re-reads the same identity");
    }

    #[tokio::test]
    async fn setup_refuses_to_overwrite_and_destroy_fulfils_itself() {
        let key = SecretKey::generate();
        let mut vault = unlocked_vault(&key).await;

        let second = SecretKey::generate();
        assert!(vault.setup(&second).await.is_err());
        assert_eq!(vault.key(), Some(&key), "the refused setup changed nothing");

        vault.destroy().await.unwrap();
        assert_eq!(vault.key(), None);
        assert!(vault.unlock().await.is_err(), "a destroyed vault does not come back");
        assert!(vault.store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unlock_without_a_vault_is_an_honest_error() {
        let mut vault = Vault::new(MemoryStore::default());
        assert!(vault.unlock().await.is_err());
    }

    #[tokio::test]
    async fn the_blob_survives_a_round_trip_through_bytes() {
        let key = SecretKey::generate();
        let blob =
            VaultBlob::new(&key, "wrap of substance".into(), vec!["the nonce".into()]).unwrap();
        let bytes = serde_json::to_vec(&blob).unwrap();
        let decoded = VaultBlob::decode(&bytes).unwrap();
        assert_eq!(decoded.wrap, "wrap of substance");
        assert_eq!(keys::decrypt_ncryptsec(&decoded.ncryptsec, &decoded.wrap).unwrap(), key);
    }

    #[tokio::test]
    async fn a_future_blob_version_is_refused_not_reinterpreted() {
        let key = SecretKey::generate();
        let blob = VaultBlob::new(&key, "wrap".into(), vec!["the nonce".into()]).unwrap();
        let bytes = serde_json::to_vec(&blob).unwrap();
        let mut mutated = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        mutated["v"] = serde_json::json!(99);
        assert!(VaultBlob::decode(mutated.to_string().as_bytes()).is_err());
    }

    #[tokio::test]
    async fn the_wrap_is_never_the_same_twice() {
        assert_ne!(generate_wrap().unwrap(), generate_wrap().unwrap());
        assert_eq!(generate_wrap().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn a_version_two_blob_migrates_to_one_outstanding_one_time_secret() {
        // A pre-one-time vault: one reusable nonce. The unlock reads
        // it from the blob's own shape and makes it one outstanding
        // secret — a URI printed before the upgrade works once more,
        // exactly once — and the stored copy is the migrated version.
        let key = SecretKey::generate();
        let wrap = generate_wrap().unwrap();
        let ncryptsec = keys::to_ncryptsec(&key, &wrap).unwrap().to_bech32().unwrap();
        let v2 = serde_json::json!({
            "v": 2,
            "wrap": wrap,
            "ncryptsec": ncryptsec,
            "pubkey": keys::public_key_hex(&key),
            "secret": "the-old-reusable-nonce",
        });
        let store = std::sync::Arc::new(MemoryStore::default());
        store.save(&serde_json::to_vec(&v2).unwrap()).await.unwrap();

        let mut vault = Vault::new(store.clone());
        vault.unlock().await.unwrap();
        assert_eq!(vault.secrets(), ["the-old-reusable-nonce"]);

        let blob: serde_json::Value =
            serde_json::from_slice(&store.peek().expect("the migration wrote a blob")).unwrap();
        assert_eq!(blob["v"], 4, "the migrated blob is the current version");
        assert_eq!(blob["secrets"][0], "the-old-reusable-nonce");
    }

    #[tokio::test]
    async fn a_minted_secret_burns_once_and_is_not_twice() {
        let key = SecretKey::generate();
        let mut vault = unlocked_vault(&key).await;
        let first = vault.mint_secret(None).await.unwrap();
        let second = vault.mint_secret(None).await.unwrap();
        assert_ne!(first, second);
        assert_eq!(vault.secrets().len(), 3, "the provisioning secret and two mints");

        // The connect's burn: once is a burn, twice is a miss.
        assert!(vault.burn_secret(&first).await.unwrap());
        assert!(!vault.burn_secret(&first).await.unwrap(), "a burned secret burns nothing");
        assert_eq!(vault.secrets().len(), 2);

        // The latest mint is what the URI advertises; a burned one is
        // not advertised at all.
        assert_eq!(vault.uri_secret(), Some(&second));
        vault.burn_secret(&second).await.unwrap();
        assert_ne!(vault.uri_secret(), Some(&second), "a burned URI is not advertised");
    }

    #[tokio::test]
    async fn rotation_invalidates_every_outstanding_secret() {
        let key = SecretKey::generate();
        let mut vault = unlocked_vault(&key).await;
        vault.mint_secret(None).await.unwrap();
        vault.mint_secret(None).await.unwrap();
        assert_eq!(vault.secrets().len(), 3);

        let fresh = vault.rotate_secret().await.unwrap();
        assert_eq!(vault.secrets(), [fresh.as_str()], "rotation leaves one door");
    }

    #[tokio::test]
    async fn a_version_one_blob_migrates_and_the_secret_becomes_durable() {
        // A pre-nonce vault: the bytes a 44.4.0 keyring holds. The
        // unlock mints a one-time secret and writes it back, because
        // a URI minted per boot would be a URI every stored copy lies
        // about.
        let key = SecretKey::generate();
        let wrap = generate_wrap().unwrap();
        let ncryptsec = keys::to_ncryptsec(&key, &wrap).unwrap().to_bech32().unwrap();
        let v1 = serde_json::json!({
            "v": 1,
            "wrap": wrap,
            "ncryptsec": ncryptsec,
            "pubkey": keys::public_key_hex(&key),
        });
        let store = std::sync::Arc::new(MemoryStore::default());
        store.save(&serde_json::to_vec(&v1).unwrap()).await.unwrap();

        let mut vault = Vault::new(store.clone());
        vault.unlock().await.unwrap();
        let minted = vault.uri_secret().expect("the migration mints a secret").to_string();
        assert_eq!(minted.len(), 64, "the secret is the same 32-byte hex shape as the wrap");

        // Durability: what the store now holds is the current blob,
        // whose outstanding secret is the one the vault hands out — a
        // stored URI keeps telling the truth across restarts.
        let blob: serde_json::Value =
            serde_json::from_slice(&store.peek().expect("the migration wrote a blob")).unwrap();
        assert_eq!(blob["v"], 4, "the migrated blob is the current version");
        assert_eq!(blob["secrets"][0], minted.as_str(), "the stored secret is the vault's own");
    }

    #[tokio::test]
    async fn a_minted_label_rides_its_secret_and_dies_with_it() {
        let key = SecretKey::generate();
        let mut vault = unlocked_vault(&key).await;

        // The person named the URI at mint: the label answers from the
        // vault's own outstanding list, and the stored blob carries it.
        let labeled = vault.mint_secret(Some("Damus on my phone".into())).await.unwrap();
        assert_eq!(
            vault.outstanding().iter().find(|o| o.secret == labeled).and_then(|o| o.label.clone()),
            Some("Damus on my phone".into()),
            "the label answers beside its secret"
        );
        let blob: serde_json::Value =
            serde_json::from_slice(&vault.store.peek().expect("the mint persisted")).unwrap();
        assert_eq!(blob["labels"][&labeled], "Damus on my phone");

        // A burn takes the label with it: a spent URI's name is not
        // anyone's business.
        assert!(vault.burn_secret(&labeled).await.unwrap());
        let blob: serde_json::Value =
            serde_json::from_slice(&vault.store.peek().expect("the burn persisted")).unwrap();
        assert!(blob["labels"].get(&labeled).is_none(), "the burned label is gone");

        // Rotation clears the map whole: the fresh URI has no name yet.
        vault.mint_secret(Some("a name".into())).await.unwrap();
        let fresh = vault.rotate_secret().await.unwrap();
        assert!(vault.outstanding().iter().all(|o| o.label.is_none()));
        let blob: serde_json::Value =
            serde_json::from_slice(&vault.store.peek().expect("the rotation persisted")).unwrap();
        assert_eq!(blob["labels"].as_object().map(|m| m.len()), Some(0));
        assert_eq!(blob["secrets"][0], fresh.as_str());
    }

    #[tokio::test]
    async fn a_version_three_blob_reads_with_an_empty_label_map() {
        // The 44.4.0 shape: one-shot secrets, no labels. It reads as-is
        // — an absent map is an empty one, and no URI loses its door.
        let key = SecretKey::generate();
        let wrap = generate_wrap().unwrap();
        let ncryptsec = keys::to_ncryptsec(&key, &wrap).unwrap().to_bech32().unwrap();
        let v3 = serde_json::json!({
            "v": 3,
            "wrap": wrap,
            "ncryptsec": ncryptsec,
            "pubkey": keys::public_key_hex(&key),
            "secrets": ["the-v3-secret"],
        });
        let store = std::sync::Arc::new(MemoryStore::default());
        store.save(&serde_json::to_vec(&v3).unwrap()).await.unwrap();

        let mut vault = Vault::new(store.clone());
        vault.unlock().await.unwrap();
        assert_eq!(vault.secrets(), ["the-v3-secret"]);
        assert!(vault.outstanding().iter().all(|o| o.label.is_none()));

        // The next write is the current version, labels included.
        let blob: serde_json::Value =
            serde_json::from_slice(&vault.store.peek().expect("the unlock persisted")).unwrap();
        assert_eq!(blob["v"], 4);
        assert_eq!(blob["labels"].as_object().map(|m| m.len()), Some(0));
    }
}
