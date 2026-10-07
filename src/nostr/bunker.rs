//! The bunker's brain: NIP-46 request events in, response events out.
//!
//! What lives here is the part of NIP-46 that is protocol rather than
//! policy: decrypting a kind 24133 request, dispatching its method,
//! wrapping the answer as a kind 24133 response encrypted back to the
//! asking app. What does *not* live here is the decision of whether a
//! consequential method runs — that is the [`Gate`]'s job, and the
//! policy engine implements it.
//!
//! The seam is split in two on purpose, and the reason is a deadlock:
//! an Ask decision waits on a person, and the person answers through a
//! socket verb that needs the same lock the worker holds. So the bunker
//! offers [`Bunker::plan`] — decrypt and dispatch, no waiting — and
//! [`Bunker::execute`] — run the decided method. The worker plans
//! under the lock, awaits the decision with the lock released, and
//! re-locks only to execute; an approval arriving a minute later finds
//! a lock that was free the whole time.
//!
//! The key doing the signing is the one the vault holds — a fresh
//! signer when setup generated, the imported identity itself when setup
//! imported. Either way the vault key is the bunker's whole identity:
//! apps see its pubkey and learn nothing else until a method answer
//! tells them, and `get_public_key` answers with its public half.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use nostr::key::{Keys, PublicKey};
use nostr::nips::nip44::Nip44;
use nostr::nips::nip46::{
    NostrConnectMessage, NostrConnectMethod, NostrConnectResponse, ResponseResult,
};
use nostr::prelude::*;

use super::policy::unix_now;

/// The client metadata a connect may carry (NIP-46's optional fourth
/// param): the app's own name, url and image, unauthenticated — the
/// panel's display hint, never an authorization input.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientMeta {
    pub name: Option<String>,
    pub url: Option<String>,
    pub image: Option<String>,
}

impl ClientMeta {
    /// Lenient by design: metadata is a courtesy, and a malformed or
    /// absent blob pairs the same as an honest one.
    fn parse(raw: Option<&String>) -> Option<Self> {
        let raw = raw?;
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        Some(Self {
            name: value["name"].as_str().map(str::to_string),
            url: value["url"].as_str().map(str::to_string),
            image: value["image"].as_str().map(str::to_string),
        })
    }
}

/// A name derived from a URL's host, last two labels, lowercased: the
/// name a client never claimed, read off the address it did claim.
/// `https://account.nostr.build/login` derives `nostr.build`; an IP
/// literal, a bare host or a single label derives nothing, because a
/// wrong-looking name is a hint and a wrong-confident one is a lie.
/// The naive last-two-labels rule mislabels multi-part suffixes
/// (account.bbc.co.uk derives `co.uk`) — cosmetic, rare, and the PSL
/// is a dependency's worth of correctness the wild has not asked for.
pub fn name_from_url(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split('/').next()?;
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next()?;
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    // An IPv4 literal is four labels and no name; the numeric check
    // also refuses a dotted-decimal masquerading as a domain.
    if labels.iter().all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(labels[labels.len() - 2..].join(".").to_lowercase())
}

/// An outstanding pairing secret, with the name the person gave the
/// URI it rides in — `--for` at mint, empty when the URI was minted
/// unnamed. The connect that burns the secret pairs under its label,
/// which outranks the client's own metadata claim: the person named
/// the door, and the app walking through it does not get to rename it.
#[derive(Debug, Clone, PartialEq)]
pub struct Outstanding {
    pub secret: String,
    pub label: Option<String>,
}

/// A `nostrconnect://` URI, parsed: the client-initiated pairing the
/// NIP defines — the client's pubkey, the relays it listens on, the
/// secret the signer must echo, and the optional metadata the client
/// claims for itself.
#[derive(Debug, Clone, PartialEq)]
pub struct NostrConnectParts {
    pub client_pubkey: PublicKey,
    pub relays: Vec<String>,
    pub secret: String,
    pub perms: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
}

/// Parse and validate a `nostrconnect://` URI. Every failure names
/// itself, because the person holding the URI is the one who can fix
/// it. The relays get the declaration's own rule — `wss://` to the
/// world, `ws://` to loopback only — reused rather than re-worded, for
/// the same reason: plaintext kind 24133 traffic announces which app
/// talks to which bunker, and the URI does not get a wider rule than
/// the declaration that runs the bunker.
pub fn parse_nostrconnect_uri(uri: &str) -> Result<NostrConnectParts> {
    let rest = uri
        .strip_prefix("nostrconnect://")
        .ok_or_else(|| anyhow!("the URI must start with nostrconnect://"))?;
    let (pubkey_str, query) = match rest.split_once('?') {
        Some((pk, q)) => (pk, q),
        None => (rest, ""),
    };
    let client_pubkey = PublicKey::parse(pubkey_str)
        .map_err(|e| anyhow!("the client pubkey must be 64 hex characters: {e}"))?;
    let mut relays = Vec::new();
    let mut secret = None;
    let mut perms = None;
    let mut name = None;
    let mut url = None;
    for pair in query.split('&') {
        let (key, value) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        match key {
            "relay" => relays.push(percent_decode(value)?),
            "secret" => secret = Some(percent_decode(value)?),
            "perms" => perms = Some(percent_decode(value)?),
            "name" => name = Some(percent_decode(value)?),
            // The url rides captured, not discarded: it is where a
            // name is derived from when the client claims none.
            "url" => url = Some(percent_decode(value)?),
            // image rides along unnamed: the panel's display hint, not
            // the daemon's business.
            _ => {}
        }
    }
    if relays.is_empty() {
        bail!("the URI carries no relay to answer the client on");
    }
    for relay in &relays {
        // The declaration's own metadata rule, restated here rather
        // than linked: the lib's posture is that the system tool does
        // not link this layer, so the rule lives on both sides, with
        // twin tests proving the same behavior on each. A client
        // relay the URI invites gets no wider a rule than the
        // declaration that runs the bunker — plaintext kind 24133
        // traffic to a non-loopback host announces which app talks to
        // which bunker, and that is not kuma's to leak.
        let (scheme, rest) = relay.split_once("://").ok_or_else(|| {
            anyhow!("relay {relay:?} has no scheme; relays are wss:// or ws:// to loopback")
        })?;
        match scheme {
            "wss" => {}
            "ws" => {
                let authority = rest.split('/').next().unwrap_or_default();
                let host = authority.split(':').next().unwrap_or(authority);
                let loopback = matches!(host, "127.0.0.1" | "::1" | "localhost");
                if !loopback {
                    bail!("relay {relay:?} is ws:// to a non-loopback host: that is plaintext \
                           on the wire, and the metadata alone is not kuma's to leak");
                }
            }
            other => bail!("relay {relay:?} has scheme {other:?}; relays are wss://, or ws:// to loopback only"),
        }
    }
    let secret = secret.ok_or_else(|| {
        anyhow!("the URI carries no secret, and a connect without one is a spoofed one")
    })?;
    Ok(NostrConnectParts { client_pubkey, relays, secret, perms, name, url })
}

/// The decision a gate hands back for a consequential method. The
/// reason is not decoration: it is what the response tells the app and
/// what the activity log records.
#[derive(Debug, Clone)]
pub enum Decision {
    Allow,
    Deny(String),
}

/// Whether a method a paired app asked for may run. `connect` and `ping`
/// never reach the gate — they are protocol, not policy — and every
/// other method does, paired or not: the gate sees the whole
/// consequential surface, which is where the activity log's complete
/// answer comes from.
///
/// The future is awaited by the bunker worker with the daemon's lock
/// released — that is the whole point of the plan/execute split: an Ask
/// may wait on a person, and the person's answer arrives through a
/// socket verb that needs the lock free.
pub trait Gate {
    fn decide(
        &self,
        app: &PublicKey,
        method: &NostrConnectMethod,
        params: &[String],
    ) -> impl std::future::Future<Output = Decision> + Send;
}

/// A gate that refuses everything consequential. What the tests run
/// against when the question is the choreography and not the policy,
/// and the honest posture for a daemon nobody has configured yet.
pub struct DenyAll;

impl Gate for DenyAll {
    async fn decide(
        &self,
        _app: &PublicKey,
        method: &NostrConnectMethod,
        _params: &[String],
    ) -> Decision {
        Decision::Deny(format!("the {method:?} method waits for the policy engine to land"))
    }
}

/// What planning produced: a finished response event, or a request
/// waiting on a gate. `Ignore` is the quiet path — noise, misdelivery,
/// undecryptable — and is answered with nothing, because a refusal
/// that names nothing helps nobody.
#[derive(Debug)]
pub enum Plan {
    Ignore,
    Answer(Event),
    /// A connect that verified: the ack rides in `answer`, the daemon
    /// records the pairing — named by the person's mint label when the
    /// secret carried one, the client's own metadata otherwise — with
    /// the perms the connect requested, and burns the secret the
    /// connect used — one-time, so the second connect with the same
    /// secret is refused.
    Paired {
        answer: Event,
        app: PublicKey,
        metadata: Option<ClientMeta>,
        burned: Option<Outstanding>,
        perms: Option<String>,
    },
    /// A relay list the bunker served a paired app — `switch_relays`,
    /// a newer method than this tree's types carry, so it travels as
    /// raw JSON and comes back here: the answer carries the list, and
    /// the daemon records the serving so the activity log's answer
    /// stays complete. Not a gate decision — protocol, like the ping.
    RelaysServed {
        answer: Event,
        app: PublicKey,
    },
    /// A logout the app asked for: the answer acks, and the daemon
    /// removes the pairing — record, session, standing grants —
    /// because the caller's own request is the only authority it
    /// needs. Self-scoped by construction: no param names a target.
    Ended {
        answer: Event,
        app: PublicKey,
    },
    /// A request shed for its sender's own rate: answered with
    /// nothing, so one app cannot spend the shared relays for every
    /// other app on the key. The daemon records the shedding.
    Shed {
        app: PublicKey,
    },
    Ask {
        /// The full request event: `execute` re-reads the app's pubkey
        /// and the correlation id from it.
        request: Event,
        id: String,
        method: NostrConnectMethod,
        params: Vec<String>,
    },
}

/// The per-sender rate: tokens refill at this many per second, up to
/// the burst. Signet's own numbers — a sustained ten requests a
/// second per app, thirty of headroom for the batches a feed fetch
/// makes. Where signet queues an over-budget request briefly, this
/// bunker sheds: the worker is one thread, and a delay there is a
/// delay for every app behind it.
const RATE_REFILL_PER_SEC: f64 = 10.0;
const RATE_BURST: f64 = 30.0;
/// The bound the buckets shed to, and what idle means for one — a
/// sender silent this long has a full bucket by refill, so its state
/// is not worth keeping under a flood of throwaway pubkeys.
const RATE_BUCKETS_MAX: usize = 1000;
const RATE_BUCKET_STALE: Duration = Duration::from_secs(600);

/// A sender's token bucket: what the refill has accumulated, and when
/// it last moved. Keyed by the verified pubkey, so the budget is the
/// sender's own — one app cannot spend another's.
struct Bucket {
    tokens: f64,
    seen: std::time::Instant,
}

/// The replay gates. A NIP-46 request arrives through relays that
/// redeliver what they hold — a reconnect re-floods the subscription's
/// backlog — and a request captured once can be re-fed forever. Three
/// cheap plaintext gates stand before any crypto: the id was not
/// processed inside the window, the created_at is plausible, and the
/// request does not travel backwards in its own sender's time.
/// Marking happens before dispatch, so a redelivery while an Ask is
/// pending cannot queue a second prompt.
const DEDUP_TTL_SECS: u64 = 600;
/// The freshness window is also the residual replay exposure: a
/// request older than this is refused even by an empty dedup cache,
/// so a cache eviction under flood opens no long-lived door.
const FRESHNESS_WINDOW_SECS: u64 = 600;
const FUTURE_DRIFT_SECS: u64 = 120;
/// How far behind a sender's own newest request a still-fresh one may
/// sit — relay reordering and client clock skew, nothing more.
const WATERMARK_SLACK_SECS: u64 = 60;
/// The bound both caches shed to. Five thousand ids cover ten minutes
/// of heavy use; the shed keeps the bound honest under a flood.
const REPLAY_CACHE_MAX: usize = 5000;

#[derive(Default)]
struct Replay {
    /// Event id → expiry. What a relay redelivering its backlog hits.
    seen: HashMap<String, u64>,
    /// Sender → (its newest created_at, expiry). What a replay survives
    /// dedup-cache eviction to hit: the sender's own time cannot go
    /// backwards, and the mark is keyed by the verified pubkey, so a
    /// flood cannot abuse it across senders.
    watermark: HashMap<PublicKey, (u64, u64)>,
}

impl Replay {
    /// Whether this is a first, fresh sighting of the event. Anything
    /// else — duplicate, stale, future, or backwards in its sender's
    /// time — is false, and the caller answers it with nothing.
    fn admit(&mut self, event: &Event) -> bool {
        let now = unix_now();
        if self.seen.get(&event.id.to_string()).is_some_and(|&expires| expires > now) {
            return false;
        }
        let created_at = event.created_at.as_secs();
        if created_at + FRESHNESS_WINDOW_SECS < now || created_at > now + FUTURE_DRIFT_SECS {
            return false;
        }
        match self.watermark.get(&event.pubkey) {
            Some(&(newest, expires))
                if expires > now && created_at + WATERMARK_SLACK_SECS < newest =>
            {
                return false;
            }
            _ => {}
        }
        self.make_room(now);
        self.seen.insert(event.id.to_string(), now + DEDUP_TTL_SECS);
        match self.watermark.get_mut(&event.pubkey) {
            // Still alive and not behind this event: the mark stays.
            Some(entry) if entry.1 > now && created_at < entry.0 => {}
            Some(entry) => *entry = (created_at, now + DEDUP_TTL_SECS),
            None => {
                self.watermark.insert(event.pubkey, (created_at, now + DEDUP_TTL_SECS));
            }
        }
        true
    }

    /// Room for one more: expired entries shed first, then — a flood
    /// of fresh ids — the soonest-expiring eighth. A dedup miss under
    /// flood costs one recheck; the freshness gate and the watermark
    /// carry what the cache sheds.
    fn make_room(&mut self, now: u64) {
        fn shed<V, F: Fn(&V) -> u64>(
            map: &mut HashMap<impl Eq + std::hash::Hash, V>,
            now: u64,
            expiry: F,
        ) {
            if map.len() < REPLAY_CACHE_MAX {
                return;
            }
            map.retain(|_, v| expiry(v) > now);
            if map.len() < REPLAY_CACHE_MAX {
                return;
            }
            let mut expiries: Vec<u64> = map.values().map(&expiry).collect();
            expiries.sort_unstable();
            let cutoff = expiries[expiries.len() / 8];
            map.retain(|_, v| expiry(v) > cutoff);
        }
        shed(&mut self.seen, now, |&expires| expires);
        shed(&mut self.watermark, now, |&(_, expires)| expires);
    }
}

/// The bunker: the signer keys, the apps that have connected, and the
/// pairing nonce the URI carries.
pub struct Bunker {
    keys: Keys,
    /// The paired apps — the live half of "paired", the engine's
    /// record the durable half. A connect opens a session, arming
    /// seeds it from the record, and revocation evicts it, so the
    /// two halves agree at every moment one of them changes.
    sessions: HashSet<PublicKey>,
    /// The outstanding one-time pairing secrets, as the vault armed
    /// them. A connect's echo that verifies burns itself here; the
    /// daemon burns the stored copy when the `Paired` answer lands,
    /// so the two halves agree.
    expected_secrets: Vec<Outstanding>,
    /// The tombstoned pubkeys — the apps the person revoked. The
    /// teeth of revocation: a connect from one of these is refused
    /// whatever secret it carries, because the person's word outranks
    /// any URI. The engine's record is the durable side; this is the
    /// live side, marked and cleared beside it.
    revoked: HashSet<PublicKey>,
    /// The relay set the bunker answers on — what `switch_relays`
    /// serves a paired app. Arming hands it in; it travels as an
    /// argument until the declaration block exists to carry it, the
    /// same gap the relays themselves have.
    relays: Vec<String>,
    /// The per-sender refill and burst, fields rather than constants
    /// so a test can shrink them to a size it sees shed. The
    /// constants are the production values; nothing else writes.
    rate_refill_per_sec: f64,
    rate_burst: f64,
    /// The per-sender token buckets — the rate one app cannot spend
    /// another's. Bounded like the replay caches: stale buckets shed
    /// first, then the oldest eighth.
    rate: HashMap<PublicKey, Bucket>,
    /// The replay gates every request passes before any crypto runs.
    replay: Replay,
}

impl Bunker {
    pub fn new(keys: Keys, expected_secrets: Vec<Outstanding>) -> Self {
        Self {
            keys,
            sessions: HashSet::new(),
            expected_secrets,
            revoked: HashSet::new(),
            relays: Vec::new(),
            rate: HashMap::new(),
            rate_refill_per_sec: RATE_REFILL_PER_SEC,
            rate_burst: RATE_BURST,
            replay: Replay::default(),
        }
    }

    /// The relay set `switch_relays` serves. Arming calls this beside
    /// the seeding; a bunker without it answers the method with an
    /// empty list, which is the truth it holds.
    pub fn with_relays(&mut self, relays: Vec<String>) {
        self.relays = relays;
    }

    /// Begin a `nostrconnect://` pairing: the person's paste is the
    /// approval, so the pairing lands here — the session opens — and
    /// the handshake event is what the daemon publishes on the
    /// client's relays. Per the NIP the handshake is a connect
    /// *response*: `{id, result: secret}`, whose author is how the
    /// client learns the bunker's pubkey and whose result is what the
    /// client validates against the URI's own secret. A client that
    /// never receives it just never connects; the pairing record
    /// sits unused until the person revokes it.
    pub fn start_handshake(&mut self, uri: &str) -> Result<(Event, NostrConnectParts)> {
        let parts = parse_nostrconnect_uri(uri)?;
        self.sessions.insert(parts.client_pubkey);
        // The id is nobody's correlation — a response to no request —
        // so it is random, and its randomness is its whole job.
        let message = NostrConnectMessage::Response {
            id: handshake_id(),
            result: Some(parts.secret.clone()),
            error: None,
        };
        let content = self.keys.nip44_encrypt(&parts.client_pubkey, &message.as_json())?;
        let event = EventBuilder::new(Kind::from_u16(24133), content)
            .tag(Tag::public_key(parts.client_pubkey))
            .finalize(&self.keys)?;
        Ok((event, parts))
    }

    /// The outstanding secrets, refreshed: a mint adds a door, a burn
    /// closes one, and the vault's own list is the durable side this
    /// mirrors. Arming hands the first list in.
    pub fn with_secrets(&mut self, secrets: Vec<Outstanding>) {
        self.expected_secrets = secrets;
    }

    /// Tombstone an app: the connect-refusing side of revocation.
    /// The session's eviction is the verb's own separate act — the
    /// tombstone is what outlives it.
    pub fn mark_revoked(&mut self, app: &PublicKey) {
        self.revoked.insert(*app);
    }

    /// Clear a tombstone: the person un-revoked, and a freshly minted
    /// URI is the way back in.
    pub fn mark_unrevoked(&mut self, app: &PublicKey) {
        self.revoked.remove(app);
    }

    /// The rate a test can afford to exercise: the same bucket shape
    /// at a size the test sees shed.
    #[cfg(test)]
    fn with_rate(&mut self, refill_per_sec: f64, burst: f64) {
        self.rate_refill_per_sec = refill_per_sec;
        self.rate_burst = burst;
    }

    /// The bunker's public identity, hex — what `get_public_key`
    /// answers and what a `bunker://` URI is built around.
    pub fn public_key(&self) -> PublicKey {
        self.keys.public_key()
    }

    /// Seed the sessions from the persisted pairings — what arming
    /// hands the bunker so a fresh set is not a forgetting. Each
    /// seeded app reaches the gate without re-connecting; a connect
    /// still pairs on its own for the apps the state has never seen.
    /// The durable side stays the policy engine's record: a revoked
    /// app is not in it, and so is not seeded.
    pub fn seed(&mut self, paired: impl IntoIterator<Item = PublicKey>) {
        self.sessions.extend(paired);
    }

    /// Forget one app's session — what the revoke verb does the
    /// moment the engine's record goes, so the refusal is live and
    /// the app's own traffic cannot re-pair what the person removed.
    /// An unparsable id evicts nothing; the verb already answered
    /// "no such app" for anything the record did not know.
    pub fn evict(&mut self, app: &str) {
        if let Ok(pubkey) = PublicKey::parse(app) {
            self.sessions.remove(&pubkey);
        }
    }

    /// Whether an app is paired. The policy engine replaces the storage
    /// with per-app policy and persisted pairing; the question stays.
    pub fn is_paired(&self, app: &PublicKey) -> bool {
        self.sessions.contains(app)
    }

    /// Plan one event: everything that is protocol answers immediately
    /// (as a finished response event), everything consequential becomes
    /// a [`Pending`] for the caller to decide outside any lock and hand
    /// back through [`Bunker::execute`]. Noise is [`Plan::Ignore`] —
    /// not a 24133, not addressed to this bunker, or undecryptable,
    /// where there is no request id to answer and a refusal that names
    /// nothing helps nobody.
    pub fn plan(&mut self, event: &Event) -> Plan {
        if event.kind != Kind::NostrConnect {
            return Plan::Ignore;
        }
        let self_pubkey = self.public_key();
        if !event.tags.public_keys().any(|p| p == self_pubkey) {
            return Plan::Ignore;
        }
        // The replay gates: a request is answered at most once, only
        // while fresh, and never backwards in its sender's own time.
        // A refusal here names nothing, like the noise below.
        if !self.replay.admit(event) {
            return Plan::Ignore;
        }
        // The sender's own budget: past the burst there is no queue —
        // the worker is one thread, and a delay for one app is a
        // delay for every app behind it. The shed is silence here and
        // a record in the daemon's log.
        if !self.admit_rate(&event.pubkey) {
            return Plan::Shed { app: event.pubkey };
        }
        let Some(plaintext) = self.keys.nip44_decrypt(&event.pubkey, &event.content).ok() else {
            return Plan::Ignore;
        };
        // A request whose method the crate's enum does not know parses
        // as a typed *response* — every field optional but the id. The
        // method field's own name is the tell: a response carries
        // none, so a plaintext that says `method` first gets the raw
        // path before the typed parse can bury it.
        let Ok(plain) = serde_json::from_str::<serde_json::Value>(&plaintext) else {
            return Plan::Ignore;
        };
        if matches!(plain["method"].as_str(), Some("switch_relays") | Some("logout")) {
            return self.plan_raw(event, plain);
        }
        let Ok(message) = NostrConnectMessage::from_json(&plaintext) else {
            return Plan::Ignore;
        };
        let (id, method, params) = match message {
            NostrConnectMessage::Request { id, method, params } => (id, method, params),
            NostrConnectMessage::Response { .. } => return Plan::Ignore,
        };
        let response = match method {
            NostrConnectMethod::Connect => {
                // Params are [user_pubkey, secret?]. Three doors, in
                // order:
                //
                // The tombstone: a revoked app is refused whatever it
                // carries — the person's word outranks any URI.
                //
                // The bond: a connect from an already-paired app is
                // the app reconnecting by its own identity — a client
                // restart is not a stranger, the request's signature
                // is the proof of who asks, and no secret is spent.
                // The pairing is the bond; the secret only gated the
                // first pairing.
                //
                // The secret: a new app's echo that verifies burns
                // itself, so a minted pairing pairs one app once and
                // a second connect with the same secret is refused.
                // The compare is constant-time, because a comparison
                // that leaks its own progress is a lock that shows
                // its keys. A connect without an echo is refused —
                // a scraped pubkey opens asks on nobody, and an
                // all-burned bunker's door closes too: the person
                // mints a fresh URI when they want a new pairing,
                // and nothing pairs until they do.
                let secret = params.get(1).cloned();
                // The third param is the requested perms (a comma list
                // of method[:kind]); the fourth is the client's own
                // metadata — the spec puts metadata at four with an
                // empty-string placeholder at three when there are no
                // perms to ask for. Off-spec clients have been seen
                // putting the metadata blob at three, so a third param
                // that opens a brace parses as metadata and the perms
                // read is what falls through empty. Both are display
                // hints; neither is an authorization input.
                let perms = params.get(2).and_then(|raw| {
                    let trimmed = raw.trim();
                    let is_meta = trimmed.starts_with('{');
                    (!trimmed.is_empty() && !is_meta).then(|| trimmed.to_string())
                });
                let metadata =
                    ClientMeta::parse(params.get(3)).or_else(|| ClientMeta::parse(params.get(2)));
                if self.revoked.contains(&event.pubkey) {
                    eprintln!(
                        "kuma-nostrd: connect from {}: refused, the app is revoked",
                        event.pubkey
                    );
                    return match self.response_event(
                        event,
                        &id,
                        NostrConnectResponse::with_error("this app is revoked"),
                    ) {
                        Some(answer) => Plan::Answer(answer),
                        None => Plan::Ignore,
                    };
                }
                let known = self.is_paired(&event.pubkey);
                // The burn is located now and executed when the answer
                // exists — a live list that burned before a wrap that
                // failed would disagree with the stored list, and a
                // restart would flip the disagreement back open.
                let mut burn_at = match (&secret, known) {
                    (Some(provided), false) => self
                        .expected_secrets
                        .iter()
                        .position(|s| constant_time_eq(&s.secret, provided)),
                    _ => None,
                };
                let verified = known || burn_at.is_some();
                eprintln!(
                    "kuma-nostrd: connect from {}: {}",
                    event.pubkey,
                    if verified {
                        "paired"
                    } else {
                        "refused, the connect did not echo a live pairing secret"
                    }
                );
                if !verified {
                    return match self.response_event(
                        event,
                        &id,
                        NostrConnectResponse::with_error(
                            "the connect did not carry the secret the pairing URI carries",
                        ),
                    ) {
                        Some(answer) => Plan::Answer(answer),
                        None => Plan::Ignore,
                    };
                }
                self.sessions.insert(event.pubkey);
                // The answer is ack, whatever the app echoed: the
                // secret was the URI's own, the verify above is the
                // proof of readership, and the result's job is the
                // one word every client checks. (The echo-back-the-
                // secret shape belongs to the nostrconnect:// flow,
                // where the app minted the secret and the signer proves
                // it read that URI instead.) The burned secret rides
                // out to the daemon — with the name the person minted
                // it under, when it carried one — whose stored copy
                // dies with it.
                let answer = self.response_event(
                    event,
                    &id,
                    NostrConnectResponse::with_result(ResponseResult::Ack),
                );
                return match answer {
                    Some(answer) => {
                        let burned = burn_at.take().map(|at| self.expected_secrets.remove(at));
                        Plan::Paired { answer, app: event.pubkey, metadata, burned, perms }
                    }
                    None => Plan::Ignore,
                };
            }
            NostrConnectMethod::Ping => ResponseResult::Pong,
            method => {
                if !self.is_paired(&event.pubkey) {
                    return match self.response_event(
                        event,
                        &id,
                        NostrConnectResponse::with_error("this app is not paired"),
                    ) {
                        Some(answer) => Plan::Answer(answer),
                        None => Plan::Ignore,
                    };
                }
                return Plan::Ask { request: event.clone(), id, method, params };
            }
        };
        // The protocol answers and the refusal share one road out: a
        // finished response event when the wrap succeeds, Ignore when
        // it does not — an app whose channel broke sees silence and
        // retries, which is what the retry is for.
        match self.response_event(event, &id, NostrConnectResponse::with_result(response)) {
            Some(answer) => Plan::Answer(answer),
            None => Plan::Ignore,
        }
    }

    /// Run a planned method under its decided answer. The decision is
    /// the gate's; the signing is the bunker's; the response event is
    /// the app's half of the channel again.
    pub fn execute(
        &self,
        request: &Event,
        id: &str,
        method: &NostrConnectMethod,
        params: &[String],
        decision: Decision,
    ) -> Option<Event> {
        let response = match decision {
            Decision::Allow => self.run(method, params),
            Decision::Deny(reason) => NostrConnectResponse::with_error(reason),
        };
        self.response_event(request, id, response)
    }

    /// The methods the gate allowed. One arm per method family, sharing
    /// the response shape: a typed result the crate serializes, or an
    /// error the app sees.
    fn run(&self, method: &NostrConnectMethod, params: &[String]) -> NostrConnectResponse {
        match method {
            NostrConnectMethod::GetPublicKey => {
                NostrConnectResponse::with_result(ResponseResult::GetPublicKey(self.public_key()))
            }
            NostrConnectMethod::SignEvent => {
                let json = match params.first() {
                    Some(json) => json,
                    None => return NostrConnectResponse::with_error("sign_event wants an event"),
                };
                // Lenient where this parse was strict: the client's
                // event is read as the fields the NIP defines, and
                // anything else it carries (a pre-computed id, a sig,
                // wrapper extras) is the client's own business, not a
                // refusal. The gate said yes; the hands must not know
                // better and decline — an error here read to the
                // person's approval as a denial. What is signed is
                // exactly what the prompt showed: kind, content,
                // tags, created_at.
                let parsed = match serde_json::from_str::<serde_json::Value>(json) {
                    Ok(value) => value,
                    Err(e) => {
                        return NostrConnectResponse::with_error(format!(
                            "unreadable unsigned event: {e}"
                        ))
                    }
                };
                let kind = match parsed.get("kind") {
                    Some(serde_json::Value::Number(n)) => {
                        n.as_u64().and_then(|k| u16::try_from(k).ok()).map(Kind::from_u16)
                    }
                    Some(serde_json::Value::String(s)) => s.parse().ok().map(Kind::from_u16),
                    _ => None,
                };
                let kind = match kind {
                    Some(kind) => kind,
                    None => return NostrConnectResponse::with_error("the event carries no kind"),
                };
                let content =
                    parsed.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let tags = match parsed.get("tags") {
                    Some(value) => {
                        match serde_json::from_value::<Vec<Vec<String>>>(value.clone()) {
                            Ok(list) => Tags::parse(list).map_err(|e| {
                                NostrConnectResponse::with_error(format!(
                                    "the event's tags do not parse: {e}"
                                ))
                            }),
                            Err(e) => Err(NostrConnectResponse::with_error(format!(
                                "the event's tags do not parse: {e}"
                            ))),
                        }
                    }
                    None => Ok(Tags::new()),
                };
                let tags = match tags {
                    Ok(tags) => tags,
                    Err(response) => return response,
                };
                let created_at = parsed
                    .get("created_at")
                    .and_then(|t| t.as_u64())
                    .map(Timestamp::from)
                    .unwrap_or_else(Timestamp::now);
                let unsigned =
                    UnsignedEvent::new(self.public_key(), created_at, kind, tags, content);
                match self.keys.sign_event(unsigned) {
                    Ok(signed) => NostrConnectResponse::with_result(ResponseResult::SignEvent(
                        Box::new(signed),
                    )),
                    Err(e) => NostrConnectResponse::with_error(format!("signing failed: {e}")),
                }
            }
            NostrConnectMethod::Nip04Encrypt
            | NostrConnectMethod::Nip04Decrypt
            | NostrConnectMethod::Nip44Encrypt
            | NostrConnectMethod::Nip44Decrypt => self.third_party_crypto(method, params),
            other => {
                NostrConnectResponse::with_error(format!("the {other:?} method is not implemented"))
            }
        }
    }

    /// The third-party crypto surface: transform a payload for someone
    /// who is not the asking app. Params are [pubkey, payload]; the
    /// answer carries the transformed payload and nothing else — a
    /// ciphertext the peer can open, or a plaintext the app handed
    /// over, never both halves of the same conversation.
    fn third_party_crypto(
        &self,
        method: &NostrConnectMethod,
        params: &[String],
    ) -> NostrConnectResponse {
        use nostr::nips::nip04::Nip04;
        let (peer, payload) = match params {
            [pk, payload] => match PublicKey::parse(pk) {
                Ok(peer) => (peer, payload.clone()),
                Err(e) => {
                    return NostrConnectResponse::with_error(format!("unreadable pubkey: {e}"))
                }
            },
            _ => return NostrConnectResponse::with_error("the method wants [pubkey, payload]"),
        };
        let attempt = match method {
            NostrConnectMethod::Nip04Encrypt => self
                .keys
                .nip04_encrypt(&peer, &payload)
                .map(|ciphertext| ResponseResult::Nip04Encrypt { ciphertext }),
            NostrConnectMethod::Nip04Decrypt => self
                .keys
                .nip04_decrypt(&peer, &payload)
                .map(|plaintext| ResponseResult::Nip04Decrypt { plaintext }),
            NostrConnectMethod::Nip44Encrypt => self
                .keys
                .nip44_encrypt(&peer, &payload)
                .map(|ciphertext| ResponseResult::Nip44Encrypt { ciphertext }),
            NostrConnectMethod::Nip44Decrypt => self
                .keys
                .nip44_decrypt(&peer, &payload)
                .map(|plaintext| ResponseResult::Nip44Decrypt { plaintext }),
            _ => return NostrConnectResponse::with_error("not a third-party crypto method"),
        };
        match attempt {
            Ok(result) => NostrConnectResponse::with_result(result),
            Err(e) => {
                NostrConnectResponse::with_error(format!("the payload did not transform: {e}"))
            }
        }
    }

    /// Whether the sender's own budget admits one more request: the
    /// bucket refills by the elapsed time, and a token is spent. A
    /// new sender starts full — the burst is the allowance a batch
    /// needs, not a debt to pay into.
    fn admit_rate(&mut self, app: &PublicKey) -> bool {
        self.bucket_room();
        let now = std::time::Instant::now();
        let bucket = self.rate.entry(*app).or_insert(Bucket { tokens: self.rate_burst, seen: now });
        let elapsed = now.duration_since(bucket.seen).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate_refill_per_sec).min(self.rate_burst);
        bucket.seen = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Room for one more sender: idle buckets shed first, then — a
    /// flood of throwaway pubkeys — the oldest eighth. A shed bucket
    /// refills by construction; the sender it forgot was idle or
    /// already over budget.
    fn bucket_room(&mut self) {
        if self.rate.len() < RATE_BUCKETS_MAX {
            return;
        }
        let now = std::time::Instant::now();
        self.rate.retain(|_, bucket| now.duration_since(bucket.seen) < RATE_BUCKET_STALE);
        if self.rate.len() < RATE_BUCKETS_MAX {
            return;
        }
        let mut seens: Vec<std::time::Instant> = self.rate.values().map(|b| b.seen).collect();
        seens.sort_unstable();
        let cutoff = seens[seens.len() / 8];
        self.rate.retain(|_, bucket| bucket.seen > cutoff);
    }

    /// The methods the crate's own message type does not know —
    /// `switch_relays` and `logout`, newer than the types this tree
    /// grew up with. Parsed as raw JSON, answered as raw responses: a
    /// paired app gets the bunker's relay list, a goodbye removes its
    /// own pairing, and anything else is the noise it looks like.
    fn plan_raw(&mut self, event: &Event, value: serde_json::Value) -> Plan {
        let (Some(id), Some(method)) = (
            value["id"].as_str().map(str::to_string),
            value["method"].as_str().map(str::to_string),
        ) else {
            return Plan::Ignore;
        };
        match method.as_str() {
            "switch_relays" => {
                if !self.is_paired(&event.pubkey) {
                    return Plan::Ignore;
                }
                let Ok(relays) = serde_json::to_string(&self.relays) else {
                    return Plan::Ignore;
                };
                match self.raw_response(event, &id, relays) {
                    Some(answer) => Plan::RelaysServed { answer, app: event.pubkey },
                    None => Plan::Ignore,
                }
            }
            "logout" => {
                // The session's owner acts at the moment of the
                // goodbye: the caller's pairing ends here, and the
                // daemon's half (the engine's record) follows the
                // `Ended` arm. A goodbye from an app with no session
                // acks and removes nothing — the courtesy the spec
                // asks.
                let paired = self.is_paired(&event.pubkey);
                self.sessions.remove(&event.pubkey);
                let answer = self.raw_response(event, &id, "ack".to_string());
                match answer {
                    Some(answer) if paired => Plan::Ended { answer, app: event.pubkey },
                    Some(answer) => Plan::Answer(answer),
                    None => Plan::Ignore,
                }
            }
            _ => Plan::Ignore,
        }
    }

    /// The same wrap as [`Bunker::response_event`], for an answer the
    /// typed response enum cannot carry: a result the method defined
    /// after this tree's types did — a relay list is a JSON array, an
    /// ack is a word.
    fn raw_response(&self, request: &Event, request_id: &str, result: String) -> Option<Event> {
        let message = NostrConnectMessage::Response {
            id: request_id.to_string(),
            result: Some(result),
            error: None,
        };
        let content = self.keys.nip44_encrypt(&request.pubkey, &message.as_json()).ok()?;
        EventBuilder::new(Kind::from_u16(24133), content)
            .tag(Tag::public_key(request.pubkey))
            .finalize(&self.keys)
            .ok()
    }

    /// Wrap an answer: kind 24133, encrypted back to the app that asked,
    /// per NIP-46's own "Response Events `kind:24133`" — the request
    /// and the answer share the kind, and a response on 24135 (the old
    /// revision this crate's types grew up with) is a letter mailed to
    /// a box nobody checks: every current client listens on 24133.
    /// p-tagged to them. The e-tag back to the request event is not
    /// written: the id inside the payload is the correlation, and a
    /// second one invites disagreement about which one counts.
    fn response_event(
        &self,
        request: &Event,
        request_id: &str,
        response: NostrConnectResponse,
    ) -> Option<Event> {
        let message = NostrConnectMessage::response(request_id, response);
        let content = self.keys.nip44_encrypt(&request.pubkey, &message.as_json()).ok()?;
        EventBuilder::new(Kind::from_u16(24133), content)
            .tag(Tag::public_key(request.pubkey))
            .finalize(&self.keys)
            .ok()
    }
}

/// The `bunker://` URI a QR renders: the bunker pubkey and the relay
/// set, in the form a phone's nostr app parses. Percent-encoding the
/// relay URLs is the spec's own spelling.
/// Constant-time equality for the pairing nonce's echo. The comparison
/// leaks its length and nothing else: a nonce this short never leaves
/// room for a timing oracle to matter, and the discipline costs one
/// fold.
fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// The handshake response's id: eight random bytes, hex — a response
/// to no request needs nothing longer, and nothing about it correlates.
fn handshake_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).unwrap_or_default();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bunker URI an app logs in with: the bunker's key, the relays it
/// answers on, and the pairing nonce the connect must echo.
pub fn bunker_uri(public_key: &PublicKey, relays: &[String], secret: Option<&str>) -> String {
    let mut uri = format!("bunker://{}", public_key);
    for relay in relays {
        let mut encoded = Vec::with_capacity(relay.len());
        for b in relay.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(b)
                }
                _ => encoded.extend_from_slice(format!("%{b:02X}").as_bytes()),
            }
        }
        let encoded = String::from_utf8(encoded).expect("percent-encoded ASCII");
        let sep = if uri.contains('?') { '&' } else { '?' };
        uri.push(sep);
        uri.push_str("relay=");
        uri.push_str(&encoded);
    }
    if let Some(secret) = secret {
        let mut encoded = Vec::with_capacity(secret.len());
        for b in secret.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(b)
                }
                _ => encoded.extend_from_slice(format!("%{b:02X}").as_bytes()),
            }
        }
        let encoded = String::from_utf8(encoded).expect("percent-encoded ASCII");
        uri.push_str("&secret=");
        uri.push_str(&encoded);
    }
    uri
}

/// Parse a `bunker://` URI back into its parts — what the CLI's
/// `bunker` verb renders and what nothing else reuses, but a URI that
/// cannot be parsed back is a URI that cannot be tested.
pub fn parse_bunker_uri(uri: &str) -> Result<(PublicKey, Vec<String>)> {
    let rest = uri.strip_prefix("bunker://").ok_or_else(|| anyhow!("not a bunker:// URI"))?;
    let (pubkey_str, query) = match rest.split_once('?') {
        Some((pk, q)) => (pk, Some(q)),
        None => (rest, None),
    };
    let public_key = PublicKey::parse(pubkey_str).map_err(|e| anyhow!("bad bunker pubkey: {e}"))?;
    let mut relays = Vec::new();
    if let Some(query) = query {
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("relay=") {
                relays.push(percent_decode(value)?);
            }
        }
    }
    Ok((public_key, relays))
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // The query-string spelling of a space — the NIP's own
            // example carries `name=My+Client`.
            b'+' => out.push(b' '),
            b'%' => {
                if i + 3 > bytes.len() {
                    return Err(anyhow!("bad percent-encoding"));
                }
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .map_err(|_| anyhow!("bad percent-encoding"))?;
                let byte =
                    u8::from_str_radix(hex, 16).map_err(|_| anyhow!("bad percent-encoding"))?;
                out.push(byte);
                i += 3;
                continue;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| anyhow!("bad percent-encoding"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test bunker, armed: a secret the test holds in its hand,
    /// so its app can connect the way a real app does — by reading
    /// the URI.
    fn bunker_for_tests() -> (Bunker, String) {
        let secret = "the-test-nonce".to_string();
        (
            Bunker::new(
                Keys::generate(),
                vec![Outstanding { secret: secret.clone(), label: None }],
            ),
            secret,
        )
    }

    /// One keypair standing in for the paired app, with the pieces the
    /// tests need: a request event encrypted and signed as an app
    /// would, and the ability to decrypt what came back.
    struct App {
        keys: Keys,
    }

    impl App {
        fn new() -> Self {
            Self { keys: Keys::generate() }
        }

        fn pubkey(&self) -> PublicKey {
            self.keys.public_key()
        }

        /// The connect a real app sends: it read the URI, so its
        /// params lead with the pubkey it expects to control and
        /// carry the secret's echo behind them.
        fn connect(&self, bunker: &Bunker, secret: &str) -> Event {
            self.request_event(
                &bunker.public_key(),
                NostrConnectMethod::Connect,
                &[bunker.public_key().to_string().as_str(), secret],
            )
        }

        /// The connect, with the app's own request id.
        fn request_event(
            &self,
            bunker: &PublicKey,
            method: NostrConnectMethod,
            params: &[&str],
        ) -> Event {
            self.request_event_with_id(bunker, "test-request-id", method, params)
        }

        fn request_event_with_id(
            &self,
            bunker: &PublicKey,
            id: &str,
            method: NostrConnectMethod,
            params: &[&str],
        ) -> Event {
            let message = NostrConnectMessage::Request {
                id: id.to_string(),
                method,
                params: params.iter().map(|s| s.to_string()).collect(),
            };
            self.request_event_raw(bunker, &message.as_json())
        }

        fn request_event_raw(&self, bunker: &PublicKey, plaintext: &str) -> Event {
            let content = self.keys.nip44_encrypt(bunker, plaintext).unwrap();
            EventBuilder::new(Kind::NostrConnect, content)
                .tag(Tag::public_key(*bunker))
                .finalize(&self.keys)
                .unwrap()
        }

        /// A request carrying its own created_at — what the replay
        /// tests need to stand still or lie about the clock.
        fn request_event_at(
            &self,
            bunker: &PublicKey,
            id: &str,
            method: NostrConnectMethod,
            params: &[&str],
            created_at: u64,
        ) -> Event {
            let message = NostrConnectMessage::Request {
                id: id.to_string(),
                method,
                params: params.iter().map(|s| s.to_string()).collect(),
            };
            let content = self.keys.nip44_encrypt(bunker, &message.as_json()).unwrap();
            EventBuilder::new(Kind::NostrConnect, content)
                .custom_created_at(nostr::types::Timestamp::from(created_at))
                .tag(Tag::public_key(*bunker))
                .finalize(&self.keys)
                .unwrap()
        }

        /// The response the bunker sent, decrypted with the app's own
        /// half of the channel.
        fn decrypt_response(&self, response: &Event) -> NostrConnectMessage {
            let plaintext = self.keys.nip44_decrypt(&response.pubkey, &response.content).unwrap();
            NostrConnectMessage::from_json(&plaintext).unwrap()
        }
    }

    #[tokio::test]
    async fn ping_round_trips_the_crypto_choreography() {
        let (mut bunker, _secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        let request =
            app.request_event_with_id(&bunker_pubkey, "ping-id-1", NostrConnectMethod::Ping, &[]);
        let response = match bunker.plan(&request) {
            Plan::Answer(response) => response,
            other => panic!("a ping is protocol, answered in the plan: {other:?}"),
        };

        assert_eq!(response.kind, Kind::from_u16(24133));
        assert_eq!(response.pubkey, bunker_pubkey);
        assert_eq!(response.tags.public_keys().collect::<Vec<_>>(), vec![app.pubkey()]);
        match app.decrypt_response(&response) {
            NostrConnectMessage::Response { id, result, error } => {
                assert_eq!(id, "ping-id-1");
                assert_eq!(result.as_deref(), Some("pong"));
                assert_eq!(error, None);
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_pairs_and_get_public_key_answers_the_bunker_identity() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        match bunker.plan(&app.connect(&bunker, &secret)) {
            Plan::Paired { .. } => {}
            other => panic!("connect is protocol: {other:?}"),
        }
        assert!(bunker.is_paired(&app.pubkey()));

        let ask_event = app.request_event(&bunker_pubkey, NostrConnectMethod::GetPublicKey, &[]);
        let response = match bunker.plan(&ask_event) {
            Plan::Ask { request, id, method, params } => bunker
                .execute(&request, &id, &method, &params, Decision::Allow)
                .expect("an allowed method answers"),
            other => panic!("get_public_key waits on the gate: {other:?}"),
        };
        match app.decrypt_response(&response) {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None);
                assert_eq!(result.as_deref(), Some(bunker_pubkey.to_string().as_str()));
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_connect_without_the_nonce_opens_nothing() {
        let mut bunker = Bunker::new(
            Keys::generate(),
            vec![Outstanding { secret: "the-nonce".into(), label: None }],
        );
        let app = App::new();
        let bunker_pubkey = bunker.public_key();
        let _secret = "the-nonce";

        // No echo at all: the app that never read the URI.
        let request = app.request_event(&bunker_pubkey, NostrConnectMethod::Connect, &[]);
        match bunker.plan(&request) {
            Plan::Answer(response) => match app.decrypt_response(&response) {
                NostrConnectMessage::Response { error: Some(e), .. } => {
                    assert!(e.contains("secret"), "the refusal names the nonce: {e}");
                }
                other => panic!("the missing echo is a refusal: {other:?}"),
            },
            other => panic!("a refusal is an answer, not a silence: {other:?}"),
        }
        assert!(!bunker.is_paired(&app.pubkey()), "a refused connect pairs nobody");

        // The wrong echo: a guess, or another URI's nonce. Connect's
        // params lead with the pubkey the app expects to control; the
        // nonce's echo rides behind it.
        let request = app.request_event(
            &bunker_pubkey,
            NostrConnectMethod::Connect,
            &[bunker_pubkey.to_string().as_str(), "another-uri-nonce"],
        );
        match bunker.plan(&request) {
            Plan::Answer(_) => {}
            other => panic!("the wrong echo is refused too: {other:?}"),
        }
        assert!(!bunker.is_paired(&app.pubkey()));

        // The right echo: the app read the URI, and the door opens —
        // once. The secret burned with the pairing: a second connect
        // carrying the same secret is refused, because the URI was
        // one app's door, not the front gate's.
        let request = app.request_event(
            &bunker_pubkey,
            NostrConnectMethod::Connect,
            &[bunker_pubkey.to_string().as_str(), "the-nonce"],
        );
        match bunker.plan(&request) {
            Plan::Paired { burned, .. } => {
                assert_eq!(
                    burned.as_ref().map(|o| o.secret.as_str()),
                    Some("the-nonce"),
                    "the echo burned"
                );
            }
            other => panic!("the nonce's echo pairs: {other:?}"),
        }
        assert!(bunker.is_paired(&app.pubkey()));

        let second = App::new();
        let request = second.request_event(
            &bunker_pubkey,
            NostrConnectMethod::Connect,
            &[bunker_pubkey.to_string().as_str(), "the-nonce"],
        );
        match bunker.plan(&request) {
            Plan::Answer(response) => {
                let message = second.decrypt_response(&response);
                match message {
                    NostrConnectMessage::Response { error, .. } => {
                        assert!(error.unwrap().contains("secret"));
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a burned secret is refused: {other:?}"),
        }
        assert!(!bunker.is_paired(&second.pubkey()), "a burned URI pairs nobody");
    }

    #[tokio::test]
    async fn a_connects_metadata_and_perms_ride_the_pairing() {
        // The spec's four-param connect: [bunker, secret, perms,
        // metadata]. The metadata is the app's own claim — a display
        // hint; the perms ride the record too. The person's mint
        // label, when the secret carried one, is what outranks the
        // claim — the worker's name choice, tested by priority there.
        let mut bunker = Bunker::new(
            Keys::generate(),
            vec![Outstanding {
                secret: "the-nonce".into(),
                label: Some("Damus on my phone".into()),
            }],
        );
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        let request = app.request_event(
            &bunker_pubkey,
            NostrConnectMethod::Connect,
            &[
                bunker_pubkey.to_string().as_str(),
                "the-nonce",
                "sign_event:1,nip44_decrypt",
                r#"{"name":"Damus","image":"https://example/daemon.png"}"#,
            ],
        );
        match bunker.plan(&request) {
            Plan::Paired { burned, metadata, perms, .. } => {
                assert_eq!(
                    burned.as_ref().and_then(|o| o.label.clone()),
                    Some("Damus on my phone".into()),
                    "the mint label rides the burn"
                );
                assert_eq!(perms.as_deref(), Some("sign_event:1,nip44_decrypt"));
                assert_eq!(metadata.and_then(|m| m.name), Some("Damus".into()));
            }
            other => panic!("the four-param connect pairs: {other:?}"),
        }

        // The off-spec shape some clients ship: the metadata blob at
        // the third position, no perms anywhere. The brace gives it
        // away, and the perms read falls through empty.
        let mut bunker = Bunker::new(
            Keys::generate(),
            vec![Outstanding { secret: "the-nonce".into(), label: None }],
        );
        let app = App::new();
        let bunker_pubkey = bunker.public_key();
        let request = app.request_event(
            &bunker_pubkey,
            NostrConnectMethod::Connect,
            &[bunker_pubkey.to_string().as_str(), "the-nonce", r#"{"name":"Odd Client"}"#],
        );
        match bunker.plan(&request) {
            Plan::Paired { metadata, perms, .. } => {
                assert_eq!(perms, None, "a metadata blob is not a perms list");
                assert_eq!(metadata.and_then(|m| m.name), Some("Odd Client".into()));
            }
            other => panic!("the shifted metadata still pairs: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_bunker_with_no_outstanding_secret_leaves_the_door_shut() {
        // The all-burned shape: every URI's secret was spent, and no
        // door is open until the person mints a fresh one. The old
        // fallback — an empty vault pairing by the person's gate —
        // died with the one-time secret: post-burn emptiness is the
        // NORMAL state now, and a fallback that fired routinely would
        // let a scraped pubkey open asks on the person after every
        // legitimate pairing spent its door.
        let mut bunker = Bunker::new(Keys::generate(), vec![]);
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        match bunker.plan(&app.request_event(&bunker_pubkey, NostrConnectMethod::Connect, &[])) {
            Plan::Answer(response) => {
                let message = app.decrypt_response(&response);
                match message {
                    NostrConnectMessage::Response { error, .. } => {
                        assert!(error.as_ref().unwrap().contains("secret"), "{error:?}");
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a door with no secret is shut, not open: {other:?}"),
        }
        assert!(!bunker.is_paired(&app.pubkey()));
    }

    #[tokio::test]
    async fn an_unpaired_app_gets_refused() {
        let (mut bunker, _secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        let ask_event = app.request_event(&bunker_pubkey, NostrConnectMethod::GetPublicKey, &[]);
        let response = match bunker.plan(&ask_event) {
            Plan::Answer(response) => response,
            other => panic!("an unpaired app is refused in the plan: {other:?}"),
        };
        match app.decrypt_response(&response) {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(result, None);
                assert!(error.unwrap().contains("not paired"));
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_gate_refusal_is_the_answer_the_app_sees() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        bunker.plan(&app.connect(&bunker, &secret));
        let ask_event = app.request_event(&bunker_pubkey, NostrConnectMethod::GetPublicKey, &[]);
        let response = match bunker.plan(&ask_event) {
            Plan::Ask { request, id, method, params } => bunker
                .execute(
                    &request,
                    &id,
                    &method,
                    &params,
                    DenyAll.decide(&app.pubkey(), &method, &params).await,
                )
                .expect("a denied method still answers"),
            other => panic!("a paired app's ask goes to the gate: {other:?}"),
        };
        match app.decrypt_response(&response) {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(result, None);
                let error = error.unwrap();
                assert!(error.contains("policy engine"), "{error}");
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sign_event_signs_when_allowed_and_the_signature_verifies() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();
        bunker.plan(&app.connect(&bunker, &secret));

        let unsigned = UnsignedEvent::new(
            bunker_pubkey,
            Timestamp::now(),
            Kind::TextNote,
            [],
            "hello from the bunker",
        );
        let response = match bunker.plan(&app.request_event(
            &bunker_pubkey,
            NostrConnectMethod::SignEvent,
            &[unsigned.as_json().as_str()],
        )) {
            Plan::Ask { request, id, method, params } => bunker
                .execute(&request, &id, &method, &params, Decision::Allow)
                .expect("an allowed sign answers"),
            other => panic!("sign_event waits on the gate: {other:?}"),
        };
        match app.decrypt_response(&response) {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "{error:?}");
                let signed = Event::from_json(result.unwrap()).unwrap();
                signed.verify().unwrap();
                assert_eq!(signed.pubkey, bunker_pubkey);
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_allowed_sign_signs_the_event_the_client_shaped() {
        // The client's event as the web's clients ship them: a
        // pre-computed id, a sig field, created_at, the wrapper's own
        // extras. The strict typed parse refused every one of these
        // and answered a person's approval with an error the client
        // showed as a decline. The gate said yes; the signature must
        // follow.
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();
        bunker.plan(&app.connect(&bunker, &secret));

        let client_event = r#"{
            "id": "4b1e7f9cd1e0a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b",
            "pubkey": "0000000000000000000000000000000000000000000000000000000000000000",
            "sig": "the-client-had-a-sig-field-for-some-reason",
            "created_at": 1790740000,
            "kind": 22242,
            "tags": [["u", "https://nostr.build"], ["method", "GET"]],
            "content": ""
        }"#;
        let response = match bunker.plan(&app.request_event(
            &bunker_pubkey,
            NostrConnectMethod::SignEvent,
            &[client_event],
        )) {
            Plan::Ask { request, id, method, params } => bunker
                .execute(&request, &id, &method, &params, Decision::Allow)
                .expect("an allowed sign answers"),
            other => panic!("sign_event waits on the gate: {other:?}"),
        };
        match app.decrypt_response(&response) {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "an approved sign is not a decline: {error:?}");
                let signed = Event::from_json(result.unwrap()).unwrap();
                signed.verify().unwrap();
                assert_eq!(signed.pubkey, bunker_pubkey, "the bunker signs as the user");
                assert_eq!(u16::from(signed.kind), 22242, "the client's kind");
                assert_eq!(signed.content, "", "the client's content");
                assert_eq!(
                    signed.created_at.as_secs(),
                    1790740000,
                    "the client's own timestamp, not the bunker's now"
                );
                assert_eq!(signed.tags.len(), 2, "the client's tags ride");
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    /// Pair the app, then run one ask through the gate with an Allow,
    /// returning the decrypted response message. The shape the
    /// third-party crypto tests all share.
    fn allowed_ask(
        bunker: &mut Bunker,
        app: &App,
        method: NostrConnectMethod,
        params: &[&str],
    ) -> NostrConnectMessage {
        let bunker_pubkey = bunker.public_key();
        match bunker.plan(&app.request_event(&bunker_pubkey, method, params)) {
            Plan::Ask { request, id, method, params } => {
                let response = bunker
                    .execute(&request, &id, &method, &params, Decision::Allow)
                    .expect("an allowed method answers");
                app.decrypt_response(&response)
            }
            other => panic!("the method waits on the gate: {other:?}"),
        }
    }

    #[tokio::test]
    async fn nip44_encrypt_round_trips_to_the_third_party() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let third = App::new();
        bunker.plan(&app.connect(&bunker, &secret));

        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip44Encrypt,
            &[third.pubkey().to_string().as_str(), "a secret for the third party"],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "{error:?}");
                let ciphertext = result.expect("an encrypt answers with a result");
                let plaintext =
                    third.keys.nip44_decrypt(&bunker.public_key(), &ciphertext).unwrap();
                assert_eq!(plaintext, "a secret for the third party");
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn nip44_decrypt_round_trips_from_the_third_party() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let third = App::new();
        let bunker_pubkey = bunker.public_key();
        bunker.plan(&app.connect(&bunker, &secret));

        let ciphertext = third.keys.nip44_encrypt(&bunker_pubkey, "wire secret").unwrap();
        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip44Decrypt,
            &[third.pubkey().to_string().as_str(), ciphertext.as_str()],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "{error:?}");
                assert_eq!(result.as_deref(), Some("wire secret"));
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn nip04_encrypt_round_trips_to_the_third_party() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let third = App::new();
        bunker.plan(&app.connect(&bunker, &secret));

        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip04Encrypt,
            &[third.pubkey().to_string().as_str(), "an old-fashioned secret"],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "{error:?}");
                let ciphertext = result.expect("an encrypt answers with a result");
                let plaintext =
                    third.keys.nip04_decrypt(&bunker.public_key(), &ciphertext).unwrap();
                assert_eq!(plaintext, "an old-fashioned secret");
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn nip04_decrypt_round_trips_from_the_third_party() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let third = App::new();
        let bunker_pubkey = bunker.public_key();
        bunker.plan(&app.connect(&bunker, &secret));

        let ciphertext = third.keys.nip04_encrypt(&bunker_pubkey, "an old wire secret").unwrap();
        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip04Decrypt,
            &[third.pubkey().to_string().as_str(), ciphertext.as_str()],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "{error:?}");
                assert_eq!(result.as_deref(), Some("an old wire secret"));
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_crypto_methods_refuse_malformed_params() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let third = App::new();
        bunker.plan(&app.connect(&bunker, &secret));

        // No params at all: even the pubkey is missing.
        let message = allowed_ask(&mut bunker, &app, NostrConnectMethod::Nip44Encrypt, &[]);
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(result, None);
                assert!(error.as_ref().unwrap().contains("pubkey, payload"), "{error:?}");
            }
            other => panic!("a response came back: {other:?}"),
        }

        // A payload missing: one param is not a method call.
        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip44Encrypt,
            &[third.pubkey().to_string().as_str()],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(result, None);
                assert!(error.as_ref().unwrap().contains("pubkey, payload"), "{error:?}");
            }
            other => panic!("a response came back: {other:?}"),
        }

        // A pubkey that parses as nothing.
        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip44Decrypt,
            &["not-a-pubkey", "payload"],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(result, None);
                assert!(error.as_ref().unwrap().contains("unreadable pubkey"), "{error:?}");
            }
            other => panic!("a response came back: {other:?}"),
        }

        // A ciphertext that decrypts as nothing.
        let message = allowed_ask(
            &mut bunker,
            &app,
            NostrConnectMethod::Nip44Decrypt,
            &[third.pubkey().to_string().as_str(), "not-a-ciphertext"],
        );
        match message {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(result, None);
                assert!(error.as_ref().unwrap().contains("did not transform"), "{error:?}");
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_redelivered_request_is_answered_once() {
        let (mut bunker, _secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        let request =
            app.request_event_with_id(&bunker_pubkey, "ping-dedup", NostrConnectMethod::Ping, &[]);
        assert!(matches!(bunker.plan(&request), Plan::Answer(_)));
        // The same event again — a relay's redelivery, or a capture
        // re-fed: answered with nothing the second time.
        assert!(matches!(bunker.plan(&request), Plan::Ignore));
    }

    #[tokio::test]
    async fn a_stale_or_future_request_is_dropped() {
        let (mut bunker, _secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();
        let now = unix_now();

        // Eleven minutes old and never seen: past the freshness
        // window, dropped before any crypto runs.
        let stale =
            app.request_event_at(&bunker_pubkey, "stale", NostrConnectMethod::Ping, &[], now - 660);
        assert!(matches!(bunker.plan(&stale), Plan::Ignore));

        // Five minutes into the future: a clock that lies.
        let future = app.request_event_at(
            &bunker_pubkey,
            "future",
            NostrConnectMethod::Ping,
            &[],
            now + 300,
        );
        assert!(matches!(bunker.plan(&future), Plan::Ignore));
    }

    #[tokio::test]
    async fn a_replay_below_the_senders_watermark_is_dropped() {
        let (mut bunker, _secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();
        let now = unix_now();

        // The live request: answered, and it sets the sender's mark.
        let live = app.request_event_at(&bunker_pubkey, "live", NostrConnectMethod::Ping, &[], now);
        assert!(matches!(bunker.plan(&live), Plan::Answer(_)));

        // A different id, three hundred seconds behind the sender's
        // own newest: a replay the dedup cache need not catch.
        let behind = app.request_event_at(
            &bunker_pubkey,
            "behind",
            NostrConnectMethod::Ping,
            &[],
            now - 300,
        );
        assert!(matches!(bunker.plan(&behind), Plan::Ignore));

        // Within the slack a clock skew tolerates: admitted.
        let skewy =
            app.request_event_at(&bunker_pubkey, "skewy", NostrConnectMethod::Ping, &[], now - 30);
        assert!(matches!(bunker.plan(&skewy), Plan::Answer(_)));
    }

    #[test]
    fn the_replay_cache_sheds_to_its_bound_under_a_flood() {
        let mut replay = Replay::default();
        let now = unix_now();
        // Twice the bound in fresh first sightings, room made after
        // each: the shed keeps the map under its ceiling.
        for i in 0..(REPLAY_CACHE_MAX as u64 * 2) {
            replay.seen.insert(format!("{i:064x}"), now + DEDUP_TTL_SECS);
            replay.make_room(now);
            assert!(replay.seen.len() <= REPLAY_CACHE_MAX, "the bound broke at {i}");
        }
    }

    #[tokio::test]
    async fn switch_relays_serves_a_paired_app_the_relay_list() {
        let (mut bunker, secret) = bunker_for_tests();
        bunker.with_relays(vec!["wss://one.example".to_string(), "wss://two.example".to_string()]);
        let app = App::new();
        bunker.plan(&app.connect(&bunker, &secret));

        // The crate's own method enum does not know this method, so the
        // request travels as raw JSON the bunker parses itself.
        let request = app.request_event_raw(
            &bunker.public_key(),
            r#"{"id":"relays","method":"switch_relays","params":[]}"#,
        );
        let relays = match bunker.plan(&request) {
            Plan::RelaysServed { answer, .. } => {
                let message = app.decrypt_response(&answer);
                match message {
                    NostrConnectMessage::Response { result, error, .. } => {
                        assert_eq!(error, None);
                        let relays = result.expect("a served list is a result");
                        serde_json::from_str::<Vec<String>>(&relays).unwrap()
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a paired app gets the list: {other:?}"),
        };
        assert_eq!(relays, vec!["wss://one.example", "wss://two.example"]);

        // An unpaired app gets nothing — the list is not for strangers.
        let stranger = App::new();
        let request = stranger.request_event_raw(
            &bunker.public_key(),
            r#"{"id":"relays","method":"switch_relays","params":[]}"#,
        );
        assert!(matches!(bunker.plan(&request), Plan::Ignore));
    }

    #[tokio::test]
    async fn logout_ends_the_callers_own_session_and_nothing_else() {
        let (mut bunker, secret) = bunker_for_tests();
        let app = App::new();
        let other = App::new();
        // Two apps, two doors: the one-time secret burned at the
        // first app's connect does not serve the second.
        bunker.with_secrets(vec![
            Outstanding { secret: secret.clone(), label: None },
            Outstanding { secret: "the-second-nonce".into(), label: None },
        ]);
        bunker.plan(&app.connect(&bunker, &secret));
        bunker.plan(&other.connect(&bunker, "the-second-nonce"));

        // The goodbye is self-scoped: no param names a target, the
        // caller is the target. The ack rides back either way.
        let request = app.request_event_raw(
            &bunker.public_key(),
            r#"{"id":"bye","method":"logout","params":[]}"#,
        );
        match bunker.plan(&request) {
            Plan::Ended { answer, .. } => {
                let message = app.decrypt_response(&answer);
                match message {
                    NostrConnectMessage::Response { result, .. } => {
                        assert_eq!(result.as_deref(), Some("ack"));
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a logout acks: {other:?}"),
        }

        // The caller's session is gone; the other app's is not.
        let ask = app.request_event(&bunker.public_key(), NostrConnectMethod::GetPublicKey, &[]);
        match bunker.plan(&ask) {
            Plan::Answer(response) => {
                let message = app.decrypt_response(&response);
                match message {
                    NostrConnectMessage::Response { error, .. } => {
                        assert!(error.unwrap().contains("not paired"));
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a logged-out app is refused: {other:?}"),
        }
        let still_paired =
            other.request_event(&bunker.public_key(), NostrConnectMethod::GetPublicKey, &[]);
        assert!(
            matches!(bunker.plan(&still_paired), Plan::Ask { .. }),
            "a logout cannot reach another app's session"
        );

        // A goodbye from an app with no session acks anyway — the
        // spec's courtesy — and opens nothing by it.
        let stranger = App::new();
        let request = stranger.request_event_raw(
            &bunker.public_key(),
            r#"{"id":"bye","method":"logout","params":[]}"#,
        );
        match bunker.plan(&request) {
            Plan::Answer(response) => {
                let message = stranger.decrypt_response(&response);
                match message {
                    NostrConnectMessage::Response { result, .. } => {
                        assert_eq!(result.as_deref(), Some("ack"));
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a logout acks: {other:?}"),
        }
        assert!(!bunker.is_paired(&stranger.pubkey()));
    }

    #[tokio::test]
    async fn a_sender_over_its_budget_is_shed_and_others_are_not() {
        let (mut bunker, secret) = bunker_for_tests();
        // A burst of two, refilling at nothing a test can wait out:
        // the connect spends one, one ping spends the last, and the
        // next ping is the sender's own rate talking.
        bunker.with_rate(0.0, 2.0);
        bunker.with_secrets(vec![
            Outstanding { secret: secret.clone(), label: None },
            Outstanding { secret: "the-second-nonce".into(), label: None },
        ]);
        let app = App::new();
        let other = App::new();
        bunker.plan(&app.connect(&bunker, &secret));
        bunker.plan(&other.connect(&bunker, "the-second-nonce"));
        let ping = |bunker: &Bunker, a: &App| {
            a.request_event(&bunker.public_key(), NostrConnectMethod::Ping, &[])
        };
        assert!(matches!(bunker.plan(&ping(&bunker, &app)), Plan::Answer(_)));
        assert!(
            matches!(bunker.plan(&ping(&bunker, &app)), Plan::Shed { .. }),
            "the budget is spent"
        );

        // The other app's budget is the other app's: unaffected — and
        // its answer is the pong a paired app's ping earns, not the
        // refusal of an app that never paired.
        let other_ping = ping(&bunker, &other);
        match bunker.plan(&other_ping) {
            Plan::Answer(answer) => {
                let message = other.decrypt_response(&answer);
                match message {
                    NostrConnectMessage::Response { result, .. } => {
                        assert_eq!(result.as_deref(), Some("pong"));
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("the other app's budget is its own: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_budget_refills_and_a_shed_sender_returns() {
        let (mut bunker, secret) = bunker_for_tests();
        // One token, refilling fast: spend it on the connect, watch
        // the next request shed, and watch the refill admit one more.
        bunker.with_rate(5.0, 1.0);
        let app = App::new();
        bunker.plan(&app.connect(&bunker, &secret));

        let ping = |bunker: &Bunker, a: &App| {
            a.request_event(&bunker.public_key(), NostrConnectMethod::Ping, &[])
        };
        assert!(matches!(bunker.plan(&ping(&bunker, &app)), Plan::Shed { .. }));

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            matches!(bunker.plan(&ping(&bunker, &app)), Plan::Answer(_)),
            "the bucket refilled"
        );
    }

    #[test]
    fn the_nostrconnect_uri_parses_and_names_its_own_failures() {
        let client = Keys::generate().public_key();
        let uri = format!(
            "nostrconnect://{client}?relay=wss%3A%2F%2Frelay.example&secret=the-secret\
             &perms=sign_event%3A1%2Cnip44_encrypt&name=My+Client&url=https%3A%2F%2Fmy.client"
        );
        let parts = parse_nostrconnect_uri(&uri).unwrap();
        assert_eq!(parts.client_pubkey, client);
        assert_eq!(parts.relays, ["wss://relay.example"], "percent-encoded relays decode");
        assert_eq!(parts.secret, "the-secret");
        assert_eq!(parts.perms.as_deref(), Some("sign_event:1,nip44_encrypt"));
        assert_eq!(parts.name.as_deref(), Some("My Client"));
        assert_eq!(parts.url.as_deref(), Some("https://my.client"));

        // Every failure names itself, because the person holding the
        // URI is the one who can fix it.
        let reason = |uri: &str| parse_nostrconnect_uri(uri).unwrap_err().to_string();
        assert!(reason("nostr://nope").contains("nostrconnect://"));
        assert!(reason("nostrconnect://not-a-pubkey?secret=x").contains("64 hex"));
        let relayless = format!("nostrconnect://{client}?secret=x");
        assert!(reason(&relayless).contains("no relay"));
        let secretless = format!("nostrconnect://{client}?relay=wss://relay.example");
        assert!(reason(&secretless).contains("no secret"));
        let plaintext_relay =
            format!("nostrconnect://{client}?relay=http://relay.example&secret=x");
        assert!(reason(&plaintext_relay).contains("wss"));
    }

    #[test]
    fn a_name_derives_from_the_url_a_client_claimed() {
        // The person's example: the login page of a subdomain derives
        // the domain.
        assert_eq!(name_from_url("https://account.nostr.build/login"), Some("nostr.build".into()));
        assert_eq!(name_from_url("https://x21.social"), Some("x21.social".into()));
        // Ports, paths, userinfo and case do not leak into the name.
        assert_eq!(
            name_from_url("https://app.example.com:8080/p/a?x=1"),
            Some("example.com".into())
        );
        assert_eq!(name_from_url("HTTPS://WWW.Yes.Party/"), Some("yes.party".into()));
        // Nothing to derive from: an IP literal, a bare host, junk.
        assert_eq!(name_from_url("https://127.0.0.1"), None);
        assert_eq!(name_from_url("http://localhost:3000"), None);
        assert_eq!(name_from_url(""), None);
    }

    #[tokio::test]
    async fn the_handshake_is_a_connect_response_the_client_validates() {
        let (mut bunker, _secret) = bunker_for_tests();
        let client = Keys::generate();
        let uri = format!(
            "nostrconnect://{}?relay=wss%3A%2F%2Frelay.example&secret=the-secret&name=Test",
            client.public_key()
        );
        let (handshake, parts) = bunker.start_handshake(&uri).unwrap();

        // The pairing landed at the paste, before a byte moved: the
        // person's word was the approval.
        assert!(bunker.is_paired(&client.public_key()));
        assert_eq!(parts.name.as_deref(), Some("Test"));

        // The event is the NIP's shape: kind 24133 to the client, its
        // content the response whose result is the URI's own secret —
        // the proof the client validates.
        assert_eq!(handshake.kind, Kind::from_u16(24133));
        assert_eq!(handshake.pubkey, bunker.public_key());
        let plaintext = client.nip44_decrypt(&handshake.pubkey, &handshake.content).unwrap();
        match NostrConnectMessage::from_json(&plaintext).unwrap() {
            NostrConnectMessage::Response { result, .. } => {
                assert_eq!(result.as_deref(), Some("the-secret"));
            }
            other => panic!("a response came back: {other:?}"),
        }

        // A URI that does not parse pairs nobody and opens nothing.
        let broken = bunker.start_handshake("nostrconnect://not-a-pubkey?secret=x");
        assert!(broken.is_err());
    }

    #[tokio::test]
    async fn a_revoked_app_is_refused_even_with_a_live_secret() {
        let mut bunker = Bunker::new(
            Keys::generate(),
            vec![Outstanding { secret: "the-nonce".into(), label: None }],
        );
        let app = App::new();
        let request = app.request_event(
            &bunker.public_key(),
            NostrConnectMethod::Connect,
            &[bunker.public_key().to_string().as_str(), "the-nonce"],
        );
        assert!(matches!(bunker.plan(&request), Plan::Paired { .. }));

        // The person mints a fresh URI; the revoked app has somehow
        // read it. The tombstone's teeth: revoked beats a live
        // secret, because the person's word outranks any URI.
        bunker.mark_revoked(&app.pubkey());
        bunker.with_secrets(vec![Outstanding { secret: "a-fresh-nonce".into(), label: None }]);
        let request = app.request_event(
            &bunker.public_key(),
            NostrConnectMethod::Connect,
            &[bunker.public_key().to_string().as_str(), "a-fresh-nonce"],
        );
        match bunker.plan(&request) {
            Plan::Answer(response) => {
                let message = app.decrypt_response(&response);
                match message {
                    NostrConnectMessage::Response { error, .. } => {
                        assert!(error.as_ref().unwrap().contains("revoked"), "{error:?}");
                    }
                    other => panic!("a response came back: {other:?}"),
                }
            }
            other => panic!("a revoked app is refused, not paired: {other:?}"),
        }

        // Un-revoking clears the tombstone, and the fresh URI pairs.
        bunker.mark_unrevoked(&app.pubkey());
        let request = app.request_event(
            &bunker.public_key(),
            NostrConnectMethod::Connect,
            &[bunker.public_key().to_string().as_str(), "a-fresh-nonce"],
        );
        assert!(matches!(bunker.plan(&request), Plan::Paired { .. }));
    }

    #[tokio::test]
    async fn a_known_app_reconnects_by_its_identity_not_its_secret() {
        let mut bunker = Bunker::new(
            Keys::generate(),
            vec![Outstanding { secret: "the-nonce".into(), label: None }],
        );
        let app = App::new();
        let secret = "the-nonce";
        let request = app.request_event(
            &bunker.public_key(),
            NostrConnectMethod::Connect,
            &[bunker.public_key().to_string().as_str(), "the-nonce"],
        );
        assert!(matches!(bunker.plan(&request), Plan::Paired { .. }));

        // The client restarted itself; its stored secret burned at the
        // first pairing. The pairing is the bond: the same pubkey's
        // connect acks without a secret, because the request's own
        // signature is the proof of who is asking.
        let request = app.connect(&bunker, secret);
        match bunker.plan(&request) {
            Plan::Paired { burned, .. } => {
                assert_eq!(burned, None, "an identity reconnect burns nothing");
            }
            other => panic!("a known app reconnects: {other:?}"),
        }

        // A stranger without a secret is still a stranger.
        let stranger = App::new();
        let request =
            stranger.request_event(&bunker.public_key(), NostrConnectMethod::Connect, &[]);
        assert!(matches!(bunker.plan(&request), Plan::Answer(_)));
        assert!(!bunker.is_paired(&stranger.pubkey()));
    }

    #[tokio::test]
    async fn noise_is_none_and_never_a_response() {
        let (mut bunker, _secret) = bunker_for_tests();
        let app = App::new();
        let bunker_pubkey = bunker.public_key();

        // Not a 24133: a text note mentioning the bunker pubkey in a tag.
        let note = EventBuilder::new(Kind::TextNote, "noise")
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app.keys)
            .unwrap();
        assert!(matches!(bunker.plan(&note), Plan::Ignore));

        // Addressed to the bunker but undecryptable — random content.
        let garbage = EventBuilder::new(Kind::NostrConnect, "not nip44")
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app.keys)
            .unwrap();
        assert!(matches!(bunker.plan(&garbage), Plan::Ignore));

        // Decryptable but not addressed to this bunker.
        let stranger = App::new();
        let misplaced = stranger.request_event(&stranger.pubkey(), NostrConnectMethod::Ping, &[]);
        assert!(matches!(bunker.plan(&misplaced), Plan::Ignore));
    }

    #[test]
    fn the_bunker_uri_round_trips_through_its_own_parser() {
        let pubkey = Keys::generate().public_key();
        let relays =
            vec!["wss://relay.nip46.com".to_string(), "wss://machine.tailnet.ts.net".to_string()];
        let uri = bunker_uri(&pubkey, &relays, None);
        assert!(uri.starts_with(&format!("bunker://{pubkey}")));
        assert_eq!(parse_bunker_uri(&uri).unwrap(), (pubkey, relays));
        assert!(parse_bunker_uri("nostr://nope").is_err());
        assert!(parse_bunker_uri("bunker://not-a-pubkey").is_err());
    }
}
