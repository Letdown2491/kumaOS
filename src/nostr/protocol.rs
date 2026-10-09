//! The socket protocol and the request handler behind it.
//!
//! The shape is the house response shape (docs/agents.md): every answer
//! carries `ok` first, a failure carries `error`, and a caller that reads
//! one document reads them all. Requests are newline-delimited JSON on a
//! unix socket; the verbs here are the vault's, and they will be joined —
//! not changed — by the policy and pairing verbs the policy engine
//! brings, because the CLI and the shell's signer plugin both talk to
//! this one surface and a verb that changes meaning under a plugin is a
//! bug that ships twice.
//!
//! Everything here is offline-testable: [`Daemon`] is generic over the
//! vault's store, and the socket layer at the bottom of the stack is the
//! only thing that knows a network exists.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use nostr::event::Event;
use nostr::key::PublicKey;
use nostr::key::{Keys, SecretKey};
use nostr::nips::nip19::ToBech32;
use serde::{Deserialize, Serialize};

use super::bunker::Bunker;
use super::keys;
use super::policy::Level;
use super::pool::{RelayPool, RelayState, RelayStatus};
use super::vault::Vault;

/// A request, one line of JSON. `confirm` is the socket spelling of the
/// house `--yes`: destructive verbs answer a dry run without it.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    /// Provision the vault: generated, or imported from an nsec, hex
    /// secret key, or NIP-06 mnemonic.
    Setup {
        mode: SetupMode,
    },
    Unlock,
    Lock,
    /// The keep-alive: resets the inactivity clock without unlocking.
    /// The surfaces that know the person is present — the panel, the
    /// CLI — send it, so a switch nobody interacted with is one that
    /// means it.
    Touch,
    /// Deletes the vault. `confirm` defaults to false, so a bare
    /// destroy is the dry run — the cost is named before it is paid.
    Destroy {
        #[serde(default)]
        confirm: bool,
    },
    /// Wrap the vault's key fresh under a passphrase the person chose
    /// and answer the `ncryptsec1` string — the backup that makes
    /// `destroy` survivable on purpose. The passphrase rides the same
    /// loopback socket the import's secret rides in on: mode 0600, the
    /// person's own uid, the boundary the layer already trusts.
    Export {
        passphrase: String,
    },
    /// The pending asks, for the CLI's `prompts` and the panel.
    Prompts,
    /// The activity log, oldest first — what was asked, by whom, and
    /// how it went. The last 500 entries, persisted across restarts.
    Log,
    /// Answer an ask with yes; `remember_hours` grants the method a
    /// standing yes for that long — an hour at most by the verb's own
    /// ceiling.
    Approve {
        id: String,
        #[serde(default)]
        remember_hours: Option<u64>,
    },
    /// Answer an ask with no.
    Deny {
        id: String,
    },
    /// The paired apps and their levels.
    Apps,
    /// Forget a paired app.
    Revoke {
        app: String,
    },
    /// Set a paired app's policy level: ask, basic, or trust. Trust is
    /// the indefinite approval — every method signs unattended — and
    /// the panel is the only road that offers it.
    Level {
        app: String,
        level: Level,
    },
    /// Mint a fresh pairing nonce and re-arm. Every URI printed before
    /// this verb dies with it: stored copies point at a nonce the
    /// bunker no longer answers, and the apps holding them must be
    /// given the new URI. Pairings survive; the front door changes.
    Rotate,
    /// Mint a one-time pairing secret and answer the URI that carries
    /// it. The act of creating a pairing URI; the connect that uses
    /// it burns it. The label is the person's name for the app the
    /// URI is for, which the connect pairs under.
    Mint {
        #[serde(default)]
        label: Option<String>,
    },
    /// Begin a `nostrconnect://` pairing from the client's URI — the
    /// person's paste is the approval, the handshake the daemon's act.
    Connect {
        uri: String,
    },
    /// Clear a revocation's tombstone. The way back in is still a
    /// freshly minted URI — the un-revoke opens the door, the mint
    /// hands over the key.
    Unrevoke {
        app: String,
    },
    /// Remove a paired app outright: the record and its standing
    /// answers go, and a freshly minted URI pairs it again. Not the
    /// tombstone — revoke is the ban, delete is the removal.
    Delete {
        app: String,
    },
    /// Name a paired app: the person's word, which outranks the
    /// client's own metadata claim on every surface after it.
    Label {
        app: String,
        name: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum SetupMode {
    Generate,
    Import {
        secret: String,
        /// The passphrase an `ncryptsec` was wrapped in. Absent for the
        /// formats whose key rides in the clear; required by the one
        /// format that carries it wrapped.
        #[serde(default)]
        passphrase: Option<String>,
    },
}

/// An answer, one line of JSON, `ok` first.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Response {
    Ok(OkResponse),
    Err(ErrResponse),
}

#[derive(Debug, Serialize)]
pub struct ErrResponse {
    pub ok: bool,
    pub error: String,
}

/// The `ok: true` variants. Untagged serialization keeps the wire shape
/// exactly as wide as the verb that answered, so `ping` carries nothing
/// and `status` carries the facts a caller renders.
#[derive(Debug, Serialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum OkResponse {
    Ping { ok: bool },
    Status { ok: bool, vault: VaultFact },
    Setup { ok: bool, pubkey: String },
    Unlock { ok: bool, pubkey: String },
    Lock { ok: bool },
    Touch { ok: bool },
    DestroyDryRun { ok: bool, would: String },
    Destroy { ok: bool },
    Export { ok: bool, ncryptsec: String },
    Prompts { ok: bool, prompts: Vec<super::policy::PromptView> },
    Log { ok: bool, log: Vec<super::policy::LogEntry> },
    Approve { ok: bool },
    Deny { ok: bool },
    Apps { ok: bool, apps: Vec<super::policy::Paired> },
    Revoke { ok: bool, removed: bool },
    Level { ok: bool },
    Rotate { ok: bool, uri: String },
    Mint { ok: bool, uri: String },
    Connect { ok: bool, name: Option<String>, relays: Vec<String> },
    Unrevoke { ok: bool, cleared: bool },
    Delete { ok: bool, removed: bool },
    Label { ok: bool, named: bool },
}

/// What `status` says, and what `doctor` will grade through it later.
#[derive(Debug, Serialize)]
pub struct VaultFact {
    /// A vault exists in the store at all.
    pub exists: bool,
    /// The gate is open in this daemon.
    pub unlocked: bool,
    /// The bunker's public identity, when a vault exists — from the
    /// bunker when unlocked, from the stored blob when locked. An npub —
    /// the form everything downstream renders — never the raw key.
    pub pubkey: Option<String>,
    /// The relay set the bunker talks to.
    pub relays: Vec<String>,
    /// The relays currently reporting a live connection.
    pub connected: Vec<String>,
    /// The pairing URI an app logs in with — bunker:// with the
    /// relays and the nonce the connect must echo. Present only while
    /// the bunker is armed: a locked daemon has no answer to give a
    /// connect, and a URI that promised one would be a lie with a
    /// sixty-second fuse.
    pub uri: Option<String>,
    /// The inactivity switch, when it is armed: the window and what
    /// remains of it. A switch that is off is absent — `status` says
    /// nothing about a switch nobody configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactivity: Option<InactivityFact>,
}

/// The inactivity switch's state at the moment `status` asked: the
/// window it was configured with, and what remains of it. Remaining
/// floor-clips at zero; a switch past its window with the gate still
/// open is one the watchdog's next beat is about to close.
#[derive(Debug, Clone, Serialize)]
pub struct InactivityFact {
    pub window_secs: u64,
    pub remaining_secs: u64,
}

/// One line in, one line out, over a newline.
pub fn decode(line: &str) -> Result<Request> {
    serde_json::from_str(line).map_err(|e| anyhow!("unparsable request: {e}"))
}

pub fn encode(response: &Response) -> String {
    let mut line = serde_json::to_string(response).expect("responses serialize");
    line.push('\n');
    line
}

/// The daemon's brain: the vault, the bunker it arms when the gate
/// opens, and the relay pool that carries the bunker — behind one
/// mutex, because the socket verbs and the bunker worker both hold it,
/// and every verb's answer is the state's truth at that instant.
///
/// The relay set comes in at construction and the verbs do not change
/// it: relays are a declaration fact (the block, when it lands), not a
/// socket verb, and a caller who wants a different set restarts the
/// daemon with a different declaration behind it.
pub struct Daemon<S: super::vault::SecretStore> {
    vault: Vault<S>,
    relays: Vec<String>,
    bunker: Option<Bunker>,
    pool: Option<RelayPool>,
    engine: super::policy::Engine,
    /// The sender every spawned pool gets a clone of, and the receiver
    /// the bunker worker consumes; made once in [`Daemon::new`], so a
    /// lock-unlock cycle spawns a fresh pool onto a channel the worker
    /// is already reading.
    inbound: Sender<Event>,
    status_tx: Sender<RelayStatus>,
    status_rx: Receiver<RelayStatus>,
    /// The last thing each relay said, as the Status verb drains the
    /// channel. Stale by design between statuses: the doctor's liveness
    /// probe is the live answer, this is the rendered one.
    relay_states: HashMap<String, RelayState>,
    /// The inactivity switch's window, when it is armed. `None` is the
    /// switch off — the desktop daemon's posture is the PAM-open
    /// keyring, and a switch on by default would lock the bunker while
    /// the person is away from the keyboard, which is the opposite of
    /// the surprise-free boot the layer promises.
    inactivity: Option<Duration>,
    /// The last reset the switch saw — construction, an unlock, or a
    /// keep-alive. Requests do not reset it: the switch's question is
    /// whether a person is present, not whether an app is talking.
    last_activity: std::time::Instant,
}

impl<S: super::vault::SecretStore> Daemon<S> {
    /// Build the daemon and hand back the receiver the bunker worker
    /// consumes. The split is explicit because the worker is the
    /// binary's to spawn — it needs the daemon's own `Arc` around it,
    /// which does not exist until after construction. `state_dir` is
    /// where pairings persist; `None` is memory-only, which is what the
    /// offline tests run against.
    pub fn new(
        vault: Vault<S>,
        relays: Vec<String>,
        state_dir: Option<std::path::PathBuf>,
    ) -> (Self, Receiver<Event>) {
        let (inbound, inbound_rx) = channel();
        let (status_tx, status_rx) = channel();
        let daemon = Self {
            vault,
            relays,
            bunker: None,
            pool: None,
            engine: super::policy::Engine::new(state_dir),
            inbound,
            status_tx,
            status_rx,
            relay_states: HashMap::new(),
            inactivity: None,
            last_activity: std::time::Instant::now(),
        };
        (daemon, inbound_rx)
    }

    /// Arm the inactivity switch. The window is the daemon operator's
    /// decision, carried as an argument until the declaration block
    /// exists to carry it — the same gap the relay set has — and the
    /// floor is enforced where the argument is parsed, not here.
    pub fn with_inactivity(&mut self, window: Option<Duration>) {
        self.inactivity = window;
        self.last_activity = std::time::Instant::now();
    }

    /// The keep-alive: the clock starts over. Unlocking touches too —
    /// a fresh gate is a present person by definition.
    fn touch(&mut self) {
        self.last_activity = std::time::Instant::now();
    }

    /// Whether the window has passed on an open gate — the only state
    /// the watchdog acts on. A locked daemon's switch has nothing to
    /// do: the lock is the switch's own act, and an unlock resets the
    /// clock by construction.
    fn inactivity_expired(&self) -> bool {
        self.vault.is_unlocked()
            && self.inactivity.is_some_and(|window| self.last_activity.elapsed() > window)
    }

    /// The lock's body, shared by the verb and the watchdog: a bunker
    /// that is being locked stops being armed first, so there is no
    /// moment where the keys are gone and the bunker still answers.
    fn lock_switch(&mut self) {
        self.teardown_bunker();
        self.vault.lock();
    }

    /// The switch's state for `status`: the window and what remains.
    /// `None` when the switch is off.
    fn inactivity_fact(&self) -> Option<InactivityFact> {
        let window = self.inactivity?;
        let remaining = window.saturating_sub(self.last_activity.elapsed());
        Some(InactivityFact { window_secs: window.as_secs(), remaining_secs: remaining.as_secs() })
    }

    /// The watchdog: a thread that wakes on its beat and closes the
    /// switch when the window has passed. It holds no state of its
    /// own — every beat re-locks the daemon and asks the one question
    /// — and a switch that is off spawns no thread, because a switch
    /// nobody configured has nothing to watch. The beat is a minute:
    /// longer than the floor's granularity needs to be exact, shorter
    /// than any window a person would set.
    pub fn spawn_inactivity_watchdog(
        daemon: &std::sync::Arc<std::sync::Mutex<Self>>,
        beat: Duration,
    ) -> Option<std::thread::JoinHandle<()>>
    where
        S: std::marker::Send + 'static,
    {
        // A switch that is off spawns no thread, because a switch
        // nobody configured has nothing to watch.
        daemon.lock().expect("the daemon lock").inactivity.as_ref()?;
        let daemon = Arc::clone(daemon);
        Some(std::thread::spawn(move || loop {
            std::thread::sleep(beat);
            let mut daemon = daemon.lock().expect("the daemon lock");
            if daemon.inactivity_expired() {
                eprintln!("kuma-nostrd: the inactivity window closed; the switch locks");
                daemon.lock_switch();
            }
        }))
    }

    pub async fn handle(&mut self, request: Request) -> Response {
        match request {
            Request::Ping => Response::Ok(OkResponse::Ping { ok: true }),
            Request::Status => {
                // A store failure is not "no vault" — saying so would turn
                // a dead keyring into advice to run setup, and a second
                // setup would then be refused on a vault nobody can see.
                // The failure is the answer.
                match self.vault.stored().await {
                    Ok(exists) => {
                        // Drain what the relays said since last time;
                        // their last word is the rendered truth.
                        while let Ok(RelayStatus { url, state }) = self.status_rx.try_recv() {
                            self.relay_states.insert(url, state);
                        }
                        let unlocked = self.vault.is_unlocked();
                        let pubkey = self.bunker_pubkey().await;
                        let uri = match (&pubkey, self.vault.uri_secret(), self.bunker.is_some()) {
                            (Some(npub), Some(secret), true) => {
                                PublicKey::parse(npub).ok().map(|pk| {
                                    super::bunker::bunker_uri(&pk, &self.relays, Some(secret))
                                })
                            }
                            _ => None,
                        };
                        let connected = self
                            .relay_states
                            .iter()
                            .filter(|(_, state)| **state == RelayState::Connected)
                            .map(|(url, _)| url.clone())
                            .collect();
                        Response::Ok(OkResponse::Status {
                            ok: true,
                            vault: VaultFact {
                                exists,
                                unlocked,
                                pubkey,
                                relays: self.relays.clone(),
                                connected,
                                uri,
                                inactivity: self.inactivity_fact(),
                            },
                        })
                    }
                    Err(e) => err_response(anyhow!("cannot read the vault: {e}")),
                }
            }
            Request::Setup { mode } => self.setup(mode).await,
            Request::Unlock => self.unlock().await,
            Request::Lock => {
                self.lock_switch();
                Response::Ok(OkResponse::Lock { ok: true })
            }
            Request::Touch => {
                self.touch();
                Response::Ok(OkResponse::Touch { ok: true })
            }
            Request::Destroy { confirm } => {
                if !confirm {
                    return Response::Ok(OkResponse::DestroyDryRun {
                        ok: true,
                        would: "delete the vault from the keyring; the key is \
                                unrecoverable afterwards"
                            .into(),
                    });
                }
                // The pool stops before the key does: a bunker that can
                // still be asked to sign while its vault is being
                // deleted is answering with a dead identity.
                self.teardown_bunker();
                match self.vault.destroy().await {
                    Ok(()) => Response::Ok(OkResponse::Destroy { ok: true }),
                    Err(e) => err_response(e),
                }
            }
            Request::Export { passphrase } => match self.export(&passphrase) {
                Ok(ncryptsec) => Response::Ok(OkResponse::Export { ok: true, ncryptsec }),
                Err(e) => err_response(e),
            },
            Request::Prompts => {
                Response::Ok(OkResponse::Prompts { ok: true, prompts: self.engine.prompts() })
            }
            Request::Log => Response::Ok(OkResponse::Log { ok: true, log: self.engine.log() }),
            Request::Approve { id, remember_hours } => {
                // The ceiling is the verb's own: more than an hour is
                // not a remember, it is a Trust that forgot its name.
                let remember = match remember_hours {
                    Some(hours) if hours > 1 => {
                        return err_response(anyhow!(
                            "a remember is an hour at most; longer wants Trust"
                        ));
                    }
                    Some(hours) => Some(Duration::from_secs(hours * 3600)),
                    None => None,
                };
                match self.engine.approve(&id, remember) {
                    Ok(()) => Response::Ok(OkResponse::Approve { ok: true }),
                    Err(e) => err_response(anyhow!("{e}")),
                }
            }
            Request::Deny { id } => match self.engine.deny(&id) {
                Ok(()) => Response::Ok(OkResponse::Deny { ok: true }),
                Err(e) => err_response(anyhow!("{e}")),
            },
            Request::Apps => Response::Ok(OkResponse::Apps { ok: true, apps: self.engine.apps() }),
            Request::Revoke { app } => {
                let removed = self.engine.revoke(&app);
                // The tombstone, the eviction, and the road teardown
                // are one act: the refusal is live, and the app's own
                // traffic cannot re-pair what the person removed —
                // its relays stop being the bunker's business.
                if removed {
                    if let (Some(bunker), Ok(pubkey)) =
                        (self.bunker.as_mut(), PublicKey::parse(&app))
                    {
                        bunker.mark_revoked(&pubkey);
                        bunker.evict(&app);
                        if let Some(pool) = self.pool.as_mut() {
                            pool.drop_app(&pubkey);
                        }
                    }
                }
                Response::Ok(OkResponse::Revoke { ok: true, removed })
            }
            Request::Unrevoke { app } => {
                let cleared = self.engine.unrevoke(&app);
                if cleared {
                    if let (Some(bunker), Ok(pubkey)) =
                        (self.bunker.as_mut(), PublicKey::parse(&app))
                    {
                        bunker.mark_unrevoked(&pubkey);
                    }
                }
                Response::Ok(OkResponse::Unrevoke { ok: true, cleared })
            }
            Request::Delete { app } => {
                // The deletion takes the live session with it, as the
                // revoke does: the record and the roads go together,
                // and what pairs next does it with a fresh URI.
                let removed = self.engine.delete(&app);
                if removed {
                    if let (Some(bunker), Ok(pubkey)) =
                        (self.bunker.as_mut(), PublicKey::parse(&app))
                    {
                        bunker.evict(&app);
                        if let Some(pool) = self.pool.as_mut() {
                            pool.drop_app(&pubkey);
                        }
                    }
                }
                Response::Ok(OkResponse::Delete { ok: true, removed })
            }
            Request::Level { app, level } => match self.engine.set_level(&app, level) {
                Ok(()) => Response::Ok(OkResponse::Level { ok: true }),
                Err(e) => err_response(anyhow!("{e}")),
            },
            Request::Connect { uri } => {
                // The person's paste is the approval: parse, open the
                // session, publish the handshake on the client's
                // relays, record the pairing. A locked bunker has no
                // key to sign the handshake with, and a URI that
                // promises one would be a lie.
                if self.bunker.is_none() {
                    return err_response(anyhow!(
                        "the bunker is locked; unlock it and the pairing can land"
                    ));
                }
                match self.bunker.as_mut().expect("checked").start_handshake(&uri) {
                    Ok((handshake, parts)) => {
                        if let Some(pool) = self.pool.as_mut() {
                            pool.subscribe_relays(
                                &parts.client_pubkey,
                                parts.relays.clone(),
                                self.bunker.as_ref().expect("checked").public_key(),
                                &self.inbound,
                                &self.status_tx,
                            );
                        }
                        self.engine.pair_with_metadata(
                            &parts.client_pubkey,
                            parts.name.clone(),
                            None,
                            parts.perms.clone(),
                            // The url is stored, not derived here: the
                            // derivation runs in the views, so a real
                            // name arriving later still wins.
                            parts.url.clone(),
                        );
                        let published = match self.pool.as_ref() {
                            Some(pool) => pool.publish_only_to(&handshake, &parts.client_pubkey),
                            None => Err(anyhow!("the bunker is not running")),
                        };
                        if let Err(e) = published {
                            return err_response(anyhow!("the handshake was not published: {e}"));
                        }
                        Response::Ok(OkResponse::Connect {
                            ok: true,
                            name: parts.name,
                            relays: parts.relays,
                        })
                    }
                    Err(e) => err_response(anyhow!("the URI did not parse: {e}")),
                }
            }
            Request::Rotate => match self.rotate().await {
                Ok(uri) => Response::Ok(OkResponse::Rotate { ok: true, uri }),
                Err(e) => err_response(e),
            },
            Request::Mint { label } => {
                // A mint is the act of creating a pairing URI: one
                // secret, one URI, one connect. Status shows the
                // latest mint while it lives; it does not mint,
                // because a status with side effects lies about its
                // own name. A locked bunker refuses — a URI minted
                // beside a closed gate is a URI nobody can answer.
                if self.bunker.is_none() {
                    return err_response(anyhow!(
                        "the bunker is locked; unlock it and the pairing URI comes with it"
                    ));
                }
                match self.vault.mint_secret(label).await {
                    Ok(secret) => {
                        // The bunker hears about the new door now — a
                        // secret the live list never saw would refuse
                        // the very connect the URI invites.
                        if let Some(bunker) = self.bunker.as_mut() {
                            bunker.with_secrets(self.vault.outstanding());
                        }
                        let pubkey = self.bunker.as_ref().expect("the armed bunker").public_key();
                        Response::Ok(OkResponse::Mint {
                            ok: true,
                            uri: super::bunker::bunker_uri(&pubkey, &self.relays, Some(&secret)),
                        })
                    }
                    Err(e) => err_response(e),
                }
            }
            Request::Label { app, name } => {
                let named = self.engine.rename(&app, &name);
                Response::Ok(OkResponse::Label { ok: true, named })
            }
        }
    }

    /// The startup posture: auto-unlock when a vault exists. The unit
    /// restarts with the session and the keyring is PAM-open, so the
    /// gate is open by design and the bunker comes up answering; the
    /// lock verb is momentary suspension, not a reboot-persistent
    /// state. A failure is honest on stderr and the daemon keeps
    /// running locked — the doctor will say so too.
    pub async fn startup_unlock(&mut self) -> Result<String> {
        self.vault.unlock().await?;
        let key =
            self.vault.key().ok_or_else(|| anyhow!("unlocked the vault and found no key"))?.clone();
        self.touch();
        Ok(self.arm_bunker(&key))
    }

    async fn setup(&mut self, mode: SetupMode) -> Response {
        let key = match mode {
            SetupMode::Generate => SecretKey::generate(),
            SetupMode::Import { secret, passphrase } => {
                let imported = match secret.trim().starts_with("ncryptsec1") {
                    // The one import that carries its key wrapped: the
                    // passphrase is not decoration, it is the second
                    // half of the secret.
                    true => match passphrase.as_deref() {
                        Some(pass) => {
                            keys::decrypt_ncryptsec(&secret, pass).map_err(|e| e.to_string())
                        }
                        None => {
                            Err("an ncryptsec needs the passphrase it was wrapped with".to_string())
                        }
                    },
                    false => keys::import(&secret).map_err(|e| e.to_string()),
                };
                match imported {
                    Ok(key) => key,
                    Err(e) => return err_response(anyhow!("import failed: {e}")),
                }
            }
        };
        match self.vault.setup(&key).await {
            Ok(()) => {
                let pubkey = self.arm_bunker(&key);
                Response::Ok(OkResponse::Setup { ok: true, pubkey })
            }
            Err(e) => err_response(anyhow!("{e}")),
        }
    }

    async fn unlock(&mut self) -> Response {
        match self.vault.unlock().await {
            Ok(()) => match self.vault.key().cloned() {
                Some(key) => {
                    self.touch();
                    let pubkey = self.arm_bunker(&key);
                    Response::Ok(OkResponse::Unlock { ok: true, pubkey })
                }
                None => err_response(anyhow!("unlocked the vault and found no key")),
            },
            Err(e) => err_response(anyhow!("{e}")),
        }
    }

    /// Arm the bunker on a fresh unlock: keys in, relay pool up. Any
    /// previous pool is torn down first — a lock that left one running
    /// was a bunker that never stopped. The vault's pairing nonce goes
    /// with the keys: the URI is the daemon's to build and the connect
    /// echo is the bunker's to verify.
    fn arm_bunker(&mut self, key: &SecretKey) -> String {
        self.teardown_bunker();
        let keys = Keys::new(key.clone());
        let pubkey = keys.public_key();
        self.pool = Some(RelayPool::spawn(
            self.relays.clone(),
            pubkey,
            self.inbound.clone(),
            self.status_tx.clone(),
        ));
        let mut bunker = Bunker::new(keys, self.vault.outstanding());
        // The persisted pairings ride in: a fresh session set is not
        // a forgetting, and the restart is invisible to a paired app.
        // One source of truth answers "paired" — the engine's record,
        // which revocation tombstones. The tombstoned ride in as
        // tombstones: the bunker refuses them whatever they carry,
        // until the person un-revokes.
        let mut seeded = Vec::new();
        for paired in self.engine.apps() {
            match PublicKey::parse(&paired.pubkey) {
                Ok(pubkey) => {
                    if paired.revoked_at.is_some() {
                        if let Some(bunker) = self.bunker.as_mut() {
                            bunker.mark_revoked(&pubkey);
                        }
                    } else {
                        seeded.push(pubkey);
                    }
                }
                Err(e) => eprintln!("kuma-nostrd: a persisted pairing's pubkey did not parse: {e}"),
            }
        }
        bunker.seed(seeded);
        bunker.with_relays(self.relays.clone());
        self.bunker = Some(bunker);
        public_key_bech32(&pubkey)
    }

    /// Mint a fresh pairing nonce, persist it, and re-arm the bunker
    /// with it. The key is the vault's own — rotation is a URI surgery,
    /// not a re-provisioning — and the old URI dies at the moment the
    /// new one exists.
    async fn rotate(&mut self) -> anyhow::Result<String> {
        let key = self
            .vault
            .key()
            .cloned()
            .ok_or_else(|| anyhow!("the bunker is locked; unlock before rotating"))?;
        let secret = self.vault.rotate_secret().await?;
        let pubkey = self.arm_bunker(&key);
        let _ = pubkey;
        let npub = self
            .vault
            .stored_pubkey()
            .await?
            .ok_or_else(|| anyhow!("rotated and found no identity"))?;
        Ok(super::bunker::bunker_uri(&npub, &self.relays, Some(&secret)))
    }

    /// The export that gives `destroy` a way through on purpose: the
    /// vault's key, wrapped fresh under a passphrase the person chose —
    /// never the stored wrap's random one, which lives beside the key
    /// it protects and would ship with the file. The stored blob is
    /// untouched; this reads, it does not rewrite. The passphrase
    /// clears the same strength bar the independent-passphrase vault
    /// mode will ask, because the file it protects is the same thing:
    /// a nostr identity whose compromise is silent and total.
    fn export(&self, passphrase: &str) -> anyhow::Result<String> {
        let key = self
            .vault
            .key()
            .cloned()
            .ok_or_else(|| anyhow!("the bunker is locked; unlock before exporting"))?;
        keys::passphrase_strength(passphrase)?;
        Ok(keys::to_ncryptsec(&key, passphrase)?.to_bech32()?)
    }

    /// Disarm: pool down, bunker dropped. A locked bunker holds no
    /// secret and no subscription; the apps see silence, which is what
    /// locked means.
    fn teardown_bunker(&mut self) {
        self.bunker = None;
        if let Some(pool) = self.pool.take() {
            pool.shutdown();
        }
    }

    /// Beat one of the bunker's answer: plan the event under the
    /// caller's lock. The lock story is the point of the split — an
    /// Ask waits on a person, and the person's answer arrives through
    /// a socket verb that needs this lock free, so the worker releases
    /// it before beat two and takes it again for
    /// [`Daemon::execute_bunker_event`]. `None` is the quiet path: a
    /// locked bunker, or noise.
    pub fn plan_bunker_event(&mut self, event: &Event) -> Option<super::bunker::Plan> {
        Some(self.bunker.as_mut()?.plan(event))
    }

    /// The connect's other half: the secret the bunker burned live
    /// dies in the stored blob too, so a restart cannot resurrect it.
    pub async fn burn(&mut self, secret: &str) -> Result<()> {
        self.vault.burn_secret(secret).await?;
        Ok(())
    }

    /// Whether an app's relay roads are still threaded — what the
    /// teardown test reads.
    #[cfg(test)]
    pub(crate) fn app_road_count(&self, app: &PublicKey) -> usize {
        self.pool.as_ref().map_or(0, |pool| pool.app_road_count(app))
    }

    /// A handle to the engine for beat two — the decision — which the
    /// worker awaits with no lock held. The engine's state is shared
    /// through its own interior lock; this handle is a window, not a
    /// fork.
    pub fn engine(&self) -> super::policy::Engine {
        self.engine.clone()
    }

    /// Beat three: run the decided ask under the caller's lock. The
    /// parts are the Ask that beat one returned; `decision` is what
    /// beat two waited for. `None` when the bunker went away between
    /// beats (a lock during the wait) — the app sees its own timeout,
    /// and the log still holds the decision.
    pub fn execute_bunker_event(
        &mut self,
        ask: super::bunker::Plan,
        decision: super::bunker::Decision,
    ) -> Option<Event> {
        let crate::nostr::bunker::Plan::Ask { request, id, method, params } = ask else {
            return None;
        };
        self.bunker.as_ref()?.execute(&request, &id, &method, &params, decision)
    }

    /// Publish a bunker answer to every relay that is up.
    /// Publish the bunker's answer down the road its own p-tag names:
    /// the client an answer is encrypted to is the client whose relays
    /// carry it, and the declared set rides every answer regardless —
    /// that is the NIP's addressing, and the pool is the one that
    /// knows the roads.
    pub fn publish(&self, event: &Event) -> Result<()> {
        match &self.pool {
            Some(pool) => match event.tags.public_keys().next() {
                Some(app) => pool.publish_for(event, &app),
                None => pool.publish(event),
            },
            None => Err(anyhow!("the bunker is not running")),
        }
    }

    /// The bunker's public identity: from the armed bunker when
    /// unlocked, from the stored blob when locked, and honestly None
    /// when neither exists.
    async fn bunker_pubkey(&self) -> Option<String> {
        if let Some(bunker) = &self.bunker {
            return Some(public_key_bech32(&bunker.public_key()));
        }
        match self.vault.stored_pubkey().await {
            Ok(Some(pubkey)) => Some(pubkey.to_bech32().expect("an npub encodes")),
            Ok(None) => None,
            // The identity is decoration; a broken blob answers nothing
            // rather than failing a status read.
            Err(_) => None,
        }
    }
}

/// The bunker's public identity in the form everything downstream
/// renders. Deriving it from the key rather than storing it means a
/// stored blob never has a second copy of a public value to disagree
/// with itself.
fn public_key_bech32(key: &PublicKey) -> String {
    key.to_bech32().expect("an npub encodes")
}

pub fn err_response(error: anyhow::Error) -> Response {
    Response::Err(ErrResponse { ok: false, error: error.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::vault::{MemoryStore, SecretStore, Vault};
    use nostr::prelude::*;

    async fn daemon() -> Daemon<MemoryStore> {
        Daemon::new(Vault::new(MemoryStore::default()), Vec::new(), None).0
    }

    async fn round_trip(request: &str) -> String {
        let mut daemon = daemon().await;
        let response = daemon.handle(decode(request).unwrap()).await;
        encode(&response)
    }

    /// The status line's vault fact, parsed — what the switch's tests
    /// read.
    fn decode_status_vault(line: &str) -> serde_json::Value {
        let value: serde_json::Value = serde_json::from_str(line).expect("a house line");
        value["vault"].clone()
    }

    #[tokio::test]
    async fn ping_answers_and_carries_nothing() {
        assert_eq!(round_trip(r#"{"cmd":"ping"}"#).await, "{\"verb\":\"ping\",\"ok\":true}\n");
    }

    #[tokio::test]
    async fn an_unparsable_line_is_an_error_not_a_crash() {
        // The verb is unknown, so the request never reaches the daemon:
        // decode refuses, and the socket layer renders that refusal in
        // the one shape a caller can read.
        assert!(decode(r#"{"cmd":"nope"}"#).is_err());
        let line = encode(&err_response(anyhow!("unparsable request")));
        assert!(line.contains("\"ok\":false"));
        assert!(line.contains("unparsable request"));
    }

    #[tokio::test]
    async fn a_paired_app_reaches_the_gate_after_a_restart() {
        use nostr::nips::nip44::Nip44;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        // The state dir is real so the pairings persist; the vault
        // store is shared so the second daemon holds the same identity.
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(MemoryStore::default());
        let mut daemon =
            Daemon::new(Vault::new(store.clone()), Vec::new(), Some(dir.path().to_path_buf())).0;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        // An app pairs — the engine's record is the durable side.
        let app_keys = Keys::generate();
        daemon.engine.pair_with_metadata(&app_keys.public_key(), None, None, None, None);
        drop(daemon);

        // A fresh daemon over the same state: the restart, with the
        // keyring PAM-open — the startup posture comes up answering.
        let mut daemon =
            Daemon::new(Vault::new(store), Vec::new(), Some(dir.path().to_path_buf())).0;
        daemon.startup_unlock().await.unwrap();

        // The app's next request reaches the gate, not a refusal.
        let bunker_pubkey = daemon.bunker.as_ref().expect("armed").public_key();
        let message = NostrConnectMessage::Request {
            id: "after-restart".into(),
            method: NostrConnectMethod::GetPublicKey,
            params: vec![],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        match daemon.plan_bunker_event(&request) {
            Some(crate::nostr::bunker::Plan::Ask { .. }) => {}
            other => panic!("a paired app reaches the gate after a restart: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_paired_app_reaches_the_gate_after_a_lock_and_unlock_cycle() {
        use nostr::nips::nip44::Nip44;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        let app_keys = Keys::generate();
        daemon.engine.pair_with_metadata(&app_keys.public_key(), None, None, None, None);

        // The cycle: lock tears the bunker down, unlock arms it fresh.
        daemon.handle(decode(r#"{"cmd":"lock"}"#).unwrap()).await;
        daemon.handle(decode(r#"{"cmd":"unlock"}"#).unwrap()).await;

        let bunker_pubkey = daemon.bunker.as_ref().expect("armed").public_key();
        let message = NostrConnectMessage::Request {
            id: "after-cycle".into(),
            method: NostrConnectMethod::GetPublicKey,
            params: vec![],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        match daemon.plan_bunker_event(&request) {
            Some(crate::nostr::bunker::Plan::Ask { .. }) => {}
            other => panic!("a paired app reaches the gate after re-arming: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_revoked_app_is_refused_midrun_not_repaired_by_its_own_traffic() {
        use nostr::nips::nip44::Nip44;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        let app_keys = Keys::generate();
        let app = app_keys.public_key();
        daemon.engine.pair_with_metadata(&app, None, None, None, None);

        // The app connects, so the bunker holds a live session too —
        // the engine's record is not the only place "paired" lives.
        let bunker_pubkey = daemon.bunker.as_ref().expect("armed").public_key();
        let secret = daemon
            .vault
            .secrets()
            .last()
            .expect("the armed vault's outstanding secret")
            .to_string();
        let connect = NostrConnectMessage::Request {
            id: "connect".into(),
            method: NostrConnectMethod::Connect,
            params: vec![bunker_pubkey.to_string(), secret],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &connect.as_json()).unwrap();
        let connect_event = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        assert!(matches!(
            daemon.plan_bunker_event(&connect_event),
            Some(crate::nostr::bunker::Plan::Paired { .. })
        ));

        // The revoke verb takes the session with it, live: the next
        // request is a refusal, not a gate — and nothing the app does
        // afterwards re-pairs it.
        let revoke = format!(r#"{{"cmd":"revoke","app":"{}"}}"#, app);
        assert!(matches!(daemon.handle(decode(&revoke).unwrap()).await, Response::Ok(_)));
        let message = NostrConnectMessage::Request {
            id: "after-revoke".into(),
            method: NostrConnectMethod::GetPublicKey,
            params: vec![],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        match daemon.plan_bunker_event(&request) {
            Some(crate::nostr::bunker::Plan::Answer(response)) => {
                let plaintext = app_keys.nip44_decrypt(&bunker_pubkey, &response.content).unwrap();
                assert!(plaintext.contains("not paired"), "{plaintext}");
            }
            other => panic!("a revoked app is refused live, not gated: {other:?}"),
        }
        // And the engine's record did not come back from the ask —
        // it stayed the tombstone the person made.
        let apps = daemon.engine.apps();
        assert_eq!(apps.len(), 1);
        assert!(apps[0].revoked_at.is_some(), "the ask un-tombstoned a revoked app");
    }

    #[tokio::test]
    async fn a_deleted_app_is_gone_not_tombstoned_and_a_fresh_uri_pairs_it_again() {
        use nostr::nips::nip44::Nip44;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        let app_keys = Keys::generate();
        let app = app_keys.public_key();
        daemon.engine.pair_with_metadata(&app, None, None, None, None);

        // The app connects, so the delete has a live session to take.
        let bunker_pubkey = daemon.bunker.as_ref().expect("armed").public_key();
        let secret = daemon
            .vault
            .secrets()
            .last()
            .expect("the armed vault's outstanding secret")
            .to_string();
        let connect = NostrConnectMessage::Request {
            id: "connect".into(),
            method: NostrConnectMethod::Connect,
            params: vec![bunker_pubkey.to_string(), secret],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &connect.as_json()).unwrap();
        let connect_event = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        assert!(matches!(
            daemon.plan_bunker_event(&connect_event),
            Some(crate::nostr::bunker::Plan::Paired { .. })
        ));

        // The delete takes the session with it, live: the next
        // request is a refusal, not a gate.
        let delete = format!(r#"{{"cmd":"delete","app":"{}"}}"#, app);
        assert!(matches!(daemon.handle(decode(&delete).unwrap()).await, Response::Ok(_)));
        let message = NostrConnectMessage::Request {
            id: "after-delete".into(),
            method: NostrConnectMethod::GetPublicKey,
            params: vec![],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        match daemon.plan_bunker_event(&request) {
            Some(crate::nostr::bunker::Plan::Answer(response)) => {
                let plaintext = app_keys.nip44_decrypt(&bunker_pubkey, &response.content).unwrap();
                assert!(plaintext.contains("not paired"), "{plaintext}");
            }
            other => panic!("a deleted app is refused live, not gated: {other:?}"),
        }

        // Deletion, not a tombstone: the record is gone outright.
        let apps = daemon.engine.apps();
        assert!(apps.is_empty(), "a deleted app leaves no record: {apps:?}");

        // And the way back needs no un-revoke: a freshly minted URI
        // pairs the same app again. Deletion forgot; it did not ban.
        daemon.handle(decode(r#"{"cmd":"mint"}"#).unwrap()).await;
        let fresh =
            daemon.vault.secrets().last().expect("the mint's outstanding secret").to_string();
        let reconnect = NostrConnectMessage::Request {
            id: "reconnect".into(),
            method: NostrConnectMethod::Connect,
            params: vec![bunker_pubkey.to_string(), fresh],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &reconnect.as_json()).unwrap();
        let reconnect_event = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        assert!(matches!(
            daemon.plan_bunker_event(&reconnect_event),
            Some(crate::nostr::bunker::Plan::Paired { .. })
        ));
    }

    #[tokio::test]
    async fn a_revoked_app_stays_refused_after_a_restart() {
        use nostr::nips::nip44::Nip44;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(MemoryStore::default());
        let mut daemon =
            Daemon::new(Vault::new(store.clone()), Vec::new(), Some(dir.path().to_path_buf())).0;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        let app_keys = Keys::generate();
        let app = app_keys.public_key();
        daemon.engine.pair_with_metadata(&app, None, None, None, None);
        assert!(daemon.engine.revoke(&app.to_string()));
        drop(daemon);

        let mut daemon =
            Daemon::new(Vault::new(store), Vec::new(), Some(dir.path().to_path_buf())).0;
        daemon.startup_unlock().await.unwrap();

        let bunker_pubkey = daemon.bunker.as_ref().expect("armed").public_key();
        let message = NostrConnectMessage::Request {
            id: "after-revoke".into(),
            method: NostrConnectMethod::GetPublicKey,
            params: vec![],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        match daemon.plan_bunker_event(&request) {
            Some(crate::nostr::bunker::Plan::Answer(response)) => {
                let plaintext = app_keys.nip44_decrypt(&bunker_pubkey, &response.content).unwrap();
                assert!(plaintext.contains("not paired"), "{plaintext}");
            }
            other => panic!("a revoked app is refused, not gated: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_switch_locks_an_idle_daemon_and_touch_resets_it() {
        async fn switched_daemon(window_secs: u64) -> Daemon<MemoryStore> {
            let mut daemon = daemon().await;
            daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;
            daemon.with_inactivity(Some(Duration::from_secs(window_secs)));
            daemon
        }

        // Armed: the status names the switch and what remains of it.
        let mut daemon = switched_daemon(3600).await;
        let line = encode(&daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await);
        assert!(line.contains("inactivity"), "the status carries the switch: {line}");
        let vault = &decode_status_vault(&line);
        assert_eq!(vault["inactivity"]["window_secs"].as_u64(), Some(3600));
        assert!(vault["inactivity"]["remaining_secs"].as_u64().unwrap_or(0) > 3590, "{vault}");

        // The keep-alive resets the clock without unlocking.
        std::thread::sleep(Duration::from_millis(1200));
        let before = decode_status_vault(&encode(
            &daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await,
        ))["inactivity"]["remaining_secs"]
            .as_u64()
            .unwrap_or(0);
        assert!(matches!(
            daemon.handle(decode(r#"{"cmd":"touch"}"#).unwrap()).await,
            Response::Ok(OkResponse::Touch { .. })
        ));
        let after = decode_status_vault(&encode(
            &daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await,
        ))["inactivity"]["remaining_secs"]
            .as_u64()
            .unwrap_or(0);
        assert!(after > before, "a touch leaves more time on the clock: {before} → {after}");
        let line = encode(&daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await);
        assert!(line.contains("\"unlocked\":true"), "a touch did not lock: {line}");
    }

    // The guard is held across the handle's await on purpose: the ask
    // runs on the machine the test owns, and the watchdog's own beat
    // merely waits for the lock — there is no other task in this
    // test's runtime to starve.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn the_watchdog_locks_when_the_window_passes() {
        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;
        daemon.with_inactivity(Some(Duration::from_secs(1)));
        let daemon = std::sync::Arc::new(std::sync::Mutex::new(daemon));

        // The watchdog on a fast beat; the window is a second. Nobody
        // touches, so the switch closes on its own.
        Daemon::spawn_inactivity_watchdog(&daemon, Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(1400));

        let mut daemon = daemon.lock().expect("the daemon lock");
        let line = encode(&daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await);
        assert!(line.contains("\"unlocked\":false"), "the watchdog locked: {line}");
    }

    #[tokio::test]
    async fn a_switch_that_is_off_spawns_no_watchdog() {
        let daemon = std::sync::Arc::new(std::sync::Mutex::new(daemon().await));
        assert!(Daemon::spawn_inactivity_watchdog(&daemon, Duration::from_millis(100)).is_none());
    }

    #[tokio::test]
    async fn each_mint_is_its_own_door_and_the_connect_burns_it() {
        use nostr::nips::nip44::Nip44;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        // Two mints, two doors: the URIs differ, and each carries its
        // own secret.
        let first: serde_json::Value = serde_json::from_str(
            encode(&daemon.handle(decode(r#"{"cmd":"mint"}"#).unwrap()).await).trim(),
        )
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(
            encode(&daemon.handle(decode(r#"{"cmd":"mint"}"#).unwrap()).await).trim(),
        )
        .unwrap();
        let first_uri = first["uri"].as_str().expect("a mint answers a uri").to_string();
        let second_uri = second["uri"].as_str().expect("a mint answers a uri").to_string();
        assert_ne!(first_uri, second_uri, "a mint is a new door, not the same one");
        assert!(first_uri.contains("secret="), "{first_uri}");

        // The connect that reads one door burns it: the second
        // connect with the same secret is refused, and the burned URI
        // stops being advertised.
        let app_keys = Keys::generate();
        let bunker_pubkey = daemon.bunker.as_ref().expect("armed").public_key();
        let secret = first_uri.split("secret=").nth(1).unwrap_or("").to_string();
        let connect = NostrConnectMessage::Request {
            id: "burn".into(),
            method: NostrConnectMethod::Connect,
            params: vec![bunker_pubkey.to_string(), secret.clone()],
        };
        let content = app_keys.nip44_encrypt(&bunker_pubkey, &connect.as_json()).unwrap();
        let connect_event = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app_keys)
            .unwrap();
        assert!(matches!(
            daemon.plan_bunker_event(&connect_event),
            Some(crate::nostr::bunker::Plan::Paired { .. })
        ));
        daemon.burn(&secret).await.unwrap();

        let stranger = Keys::generate();
        let content = stranger.nip44_encrypt(&bunker_pubkey, &connect.as_json()).unwrap();
        let replay = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&stranger)
            .unwrap();
        match daemon.plan_bunker_event(&replay) {
            Some(crate::nostr::bunker::Plan::Answer(response)) => {
                let plaintext = stranger.nip44_decrypt(&bunker_pubkey, &response.content).unwrap();
                assert!(plaintext.contains("secret"), "{plaintext}");
            }
            other => panic!("a burned secret is refused: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_status_walks_the_whole_life_cycle() {
        let mut daemon = daemon().await;

        let no_vault = daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await;
        assert!(!encode(&no_vault).contains("\"exists\":true"));

        let generated =
            daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;
        let line = encode(&generated);
        assert!(line.contains("\"pubkey\":\"npub1"), "setup answers with an npub: {line}");

        let locked = daemon.handle(decode(r#"{"cmd":"lock"}"#).unwrap()).await;
        assert!(encode(&locked).contains("\"ok\":true"));
        let status = daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await;
        let line = encode(&status);
        assert!(line.contains("\"exists\":true"));
        assert!(line.contains("\"unlocked\":false"));
        // A locked vault still names its identity — the blob carries the
        // public half in the clear for exactly this.
        assert!(line.contains("\"pubkey\":\"npub1"), "locked status names the npub: {line}");
        assert!(line.contains("\"relays\":["), "status names the relay set: {line}");

        daemon.handle(decode(r#"{"cmd":"unlock"}"#).unwrap()).await;
        let status = daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await;
        assert!(encode(&status).contains("\"unlocked\":true"));
    }

    #[tokio::test]
    async fn status_never_mistakes_a_dead_store_for_an_empty_one() {
        // A store that fails is the one status answer that must not be
        // rendered as a fact: "no vault" invites a setup that would be
        // refused, and the refusal would name a vault that exists.
        struct FailingStore;
        impl SecretStore for FailingStore {
            async fn load(&self) -> Result<Option<Vec<u8>>> {
                Err(anyhow!("the keyring is not answering"))
            }
            async fn save(&self, _: &[u8]) -> Result<()> {
                Err(anyhow!("the keyring is not answering"))
            }
            async fn remove(&self) -> Result<()> {
                Err(anyhow!("the keyring is not answering"))
            }
        }
        let mut daemon = Daemon::new(Vault::new(FailingStore), Vec::new(), None).0;
        let response = daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await;
        let line = encode(&response);
        assert!(line.contains("\"ok\":false"), "{line}");
        assert!(line.contains("cannot read the vault"), "{line}");
    }

    #[tokio::test]
    async fn setup_refuses_a_second_vault_and_import_rejects_junk() {
        let mut daemon = daemon().await;
        let request = r#"{"cmd":"setup","mode":{"how":"generate"}}"#;
        daemon.handle(decode(request).unwrap()).await;
        let second = daemon.handle(decode(request).unwrap()).await;
        assert!(encode(&second).contains("\"ok\":false"));

        let junk = daemon
            .handle(
                decode(r#"{"cmd":"setup","mode":{"how":"import","secret":"garbage"}}"#).unwrap(),
            )
            .await;
        assert!(encode(&junk).contains("\"ok\":false"));
    }

    #[tokio::test]
    async fn export_round_trips_through_import_and_refuses_locked_or_weak() {
        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;
        let before = decode_status_vault(&encode(
            &daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await,
        ))["pubkey"]
            .as_str()
            .unwrap()
            .to_string();

        // Locked refuses before anything is wrapped — the CLI's
        // pre-check asks the same question for the same reason.
        daemon.handle(decode(r#"{"cmd":"lock"}"#).unwrap()).await;
        let locked = daemon
            .handle(decode(r#"{"cmd":"export","passphrase":"a licence to decode 4412"}"#).unwrap())
            .await;
        let line = encode(&locked);
        assert!(line.contains("\"ok\":false") && line.contains("locked"), "{line}");

        daemon.handle(decode(r#"{"cmd":"unlock"}"#).unwrap()).await;
        let weak = daemon
            .handle(decode(r#"{"cmd":"export","passphrase":"all letters no digits"}"#).unwrap())
            .await;
        let line = encode(&weak);
        assert!(
            line.contains("\"ok\":false"),
            "the strength gate holds on the daemon side too: {line}"
        );

        let good = daemon
            .handle(decode(r#"{"cmd":"export","passphrase":"a licence to decode 4412"}"#).unwrap())
            .await;
        let line = encode(&good);
        assert!(
            line.contains("ncryptsec1"),
            "the answer is the import surface's own format: {line}"
        );

        // The round trip is the whole point: what export wrote, import
        // eats back as the same identity. Destroy first, because a
        // second setup is refused — and the restore is exactly what a
        // person with a lost machine runs.
        let ncryptsec = serde_json::from_str::<serde_json::Value>(line.trim()).unwrap()
            ["ncryptsec"]
            .as_str()
            .unwrap()
            .to_string();
        daemon.handle(decode(r#"{"cmd":"destroy","confirm":true}"#).unwrap()).await;
        let restore = serde_json::json!({
            "cmd": "setup",
            "mode": {"how": "import", "secret": ncryptsec, "passphrase": "a licence to decode 4412"}
        })
        .to_string();
        let restored = daemon.handle(decode(&restore).unwrap()).await;
        let line = encode(&restored);
        assert!(line.contains("\"ok\":true"), "the exported key imports back: {line}");
        let after = decode_status_vault(&encode(
            &daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await,
        ))["pubkey"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(before, after, "the restored identity is the one exported");
    }

    #[tokio::test]
    async fn destroy_is_a_dry_run_until_confirmed() {
        let mut daemon = daemon().await;
        daemon.handle(decode(r#"{"cmd":"setup","mode":{"how":"generate"}}"#).unwrap()).await;

        let dry = daemon.handle(decode(r#"{"cmd":"destroy","confirm":false}"#).unwrap()).await;
        let line = encode(&dry);
        assert!(line.contains("unrecoverable"), "the dry run names the cost: {line}");
        let after_dry = daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await;
        assert!(encode(&after_dry).contains("\"exists\":true"), "a dry run destroys nothing");

        let gone = daemon.handle(decode(r#"{"cmd":"destroy","confirm":true}"#).unwrap()).await;
        assert!(encode(&gone).contains("\"ok\":true"));
        let status = daemon.handle(decode(r#"{"cmd":"status"}"#).unwrap()).await;
        assert!(encode(&status).contains("\"exists\":false"));
    }
}
