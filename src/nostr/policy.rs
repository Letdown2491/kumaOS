//! The policy engine: who may do what, who asks, and what was decided.
//!
//! The model is Opal's, borrowed whole because it is right, with one
//! deliberate divergence the plan recorded: a newly paired app defaults
//! to **Ask** for everything, not Basic — for a signer a person opts
//! into on purpose, the strict default is the honest one, and relaxing
//! to Basic is one toggle in the panel.
//!
//! * **Basic** — everyday methods sign unattended; sensitive ones ask.
//! * **Ask** — everything asks.
//! * **Trust** — everything signs unattended, sensitive included. This
//!   is what makes Trust the loudest thing in the layer, and why the
//!   doctor grades any app holding it Warn by name.
//!
//! Sensitive — the safe-list direction: at Basic, only kinds the
//! explicit safe list vouches for sign unattended, and everything else
//! asks — the sensitive set the docs narrate (profile and follow
//! writes, relay-list and mute-list writes, deletions, DMs, client
//! auth, the wallet kinds) and every unknown kind alike, plus the
//! decrypt methods, which read what was meant to be private, and
//! NIP-04 encryption, whose whole job is private messages. A sensitive ask may be remembered for
//! at most an hour; that is the longest standing grant the engine can
//! mint, and `approve --remember` is how.
//!
//! Every decision lands in the activity log with its reason, so the
//! panel and the doctor answer "what did this app do" from the same
//! record the decisions made. Privacy mode is the default: entries
//! carry the method, the event kind, and the verdict — never the
//! params, never the content.
//!
//! The kill switch is `lock`: it drops the keys, and a bunker with no
//! keys refuses everything by construction. The engine does not keep a
//! second switch, because two switches are two ways to believe the
//! bunker is dead when it is not.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use nostr::key::PublicKey;
use nostr::nips::nip46::NostrConnectMethod;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::bunker::Decision;

/// The three levels a paired app can hold. The default for a newly
/// paired app is `Ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Basic,
    Ask,
    Trust,
}

/// Kinds whose writes sign unattended at Basic — the everyday social
/// surface: notes, reposts, reactions, comments, long-form, zap
/// receipts, pin and follow-set lists, blossom authorizations. The
/// blossom kind's safety is the verb's, not the kind's:
/// `is_sensitive` reads the auth's `t` tag, and a `delete`
/// authorization asks the way kind 5 does.
/// The direction is signet's, borrowed with one divergence: only an
/// explicitly safe kind rides, and anything unknown asks — the mute
/// list (10000) is deliberately absent from this list, because a
/// mute-list write reshapes who the identity hears, and the plan
/// counted it sensitive for that reason.
const SAFE_KINDS: &[u16] =
    &[1, 6, 7, 16, 1111, 30023, 30024, 1808, 9735, 10001, 30000, 30001, 24242];

/// Whether this method call reads a private payload or writes a part
/// of the identity an explicit safe list does not vouch for. The
/// decrypt methods always do; NIP-04 encryption does, whose whole job
/// is private messages; a sign_event does for every kind the safe
/// list does not name — the sensitive set the docs narrate (profile,
/// follows, deletions, relay and mute lists, DMs, client auth, the
/// wallet kinds) asks the same way an unknown kind does. An
/// unreadable event asks: what cannot be read cannot be vouched for.
/// The kind is read leniently — the strict typed parse refused whole
/// events over unexpected fields, and sensitivity that fails open into
/// "ask" is the safe direction; a failure that called an event
/// everyday would be the other thing.
fn is_sensitive(method: &NostrConnectMethod, params: &[String]) -> bool {
    match method {
        NostrConnectMethod::Nip04Decrypt | NostrConnectMethod::Nip44Decrypt => true,
        // NIP-04's whole job is private messages: encrypting one is
        // writing one. NIP-44 is general-purpose — blossom auth,
        // arbitrary blobs — and rides at Basic like an everyday sign.
        NostrConnectMethod::Nip04Encrypt => true,
        NostrConnectMethod::SignEvent => match params.first().and_then(|json| event_kind(json)) {
            // A blossom authorization rides the safe list only when
            // the verb it authorizes is not destructive: get, upload
            // and list are the everyday surface, and a `delete` auth
            // can destroy content at a server — a deletion wearing
            // another kind, which asks the way kind 5 does. A verb
            // that cannot be read asks too: what cannot be read
            // cannot be vouched for.
            Some(24242) => !matches!(
                params.first().and_then(|json| blossom_auth_verb(json)).as_deref(),
                Some("get" | "upload" | "list")
            ),
            Some(kind) => !SAFE_KINDS.contains(&(kind as u16)),
            None => true,
        },
        _ => false,
    }
}

/// One paired app: identity, level, and when it was paired. The file
/// form is what persists; the daemon restarts to find its pairings
/// intact, because re-pairing every app after every reboot is how a
/// person stops trusting the feature.
///
/// `name` and `image` are the client metadata the connect may carry
/// (NIP-46's optional fourth param) — the app's own claim about
/// itself, unauthenticated by the protocol's own word, so the panel
/// renders it as a display hint and nothing authorizes by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paired {
    pub pubkey: String,
    pub level: Level,
    pub paired_at: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    /// The permissions the client asked for in its `nostrconnect://`
    /// URI — `method[:kind]` commas, the client's own request. A
    /// display hint like the name: the policy engine's levels decide,
    /// and the perms never widen anything.
    #[serde(default)]
    pub perms: Option<String>,
    /// The client's canonical url, when it claimed one — where a name
    /// is derived at display time for a client that never named
    /// itself. Stored rather than derived-at-pairing so a real name
    /// arriving later still wins: the derivation runs in the views,
    /// name first, url second, fragment last.
    #[serde(default)]
    pub url: Option<String>,
    /// The tombstone: when the person revoked this app. Revocation is
    /// a state, not a deletion — the record stays so the refusal has
    /// teeth across restarts and the panel can offer the way back.
    /// `None` is an app in good standing.
    #[serde(default)]
    pub revoked_at: Option<u64>,
    /// How many requests the app has made — the list's second line,
    /// the shape Signet's app card carries. A count is not a history:
    /// the log is where the asks are named.
    #[serde(default)]
    pub request_count: u64,
    /// When the app last asked, and in what shape the answer went.
    #[serde(default)]
    pub last_used_at: Option<u64>,
}

/// What `prompts` shows: the ask, enough to decide on. The summary is
/// the glance in words ("Sign a note"); the rest is the decision's
/// material, structured rather than a JSON wall — the kind and its
/// name, the content whole, the sensitive cue, and a detail line for
/// the payload-shaped verbs.
#[derive(Debug, Clone, Serialize)]
pub struct PromptView {
    pub id: String,
    pub app: String,
    /// The raw method, as the app sent it — the panel maps it to a glyph.
    pub method: String,
    pub summary: String,
    /// The event's kind and its name, for a signature ask.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_label: Option<String>,
    /// The event's content, whole — the judgment is about these words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Whether the kind is one of the identity-, privacy- or
    /// wallet-touching ones — the cue that says look twice.
    pub sensitive: bool,
    /// How many identical asks are waiting behind this one card: a
    /// client that retries while the person reads joins the first
    /// ask instead of stacking a second card, and one answer serves
    /// every waiter.
    pub retries: u64,
    /// A payload shape the struct fields do not carry (the decrypts'
    /// target pubkey). The signature's payload rides `content` now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

struct Prompt {
    app: PublicKey,
    method: NostrConnectMethod,
    params: Vec<String>,
    summary: String,
    /// Every ask that joined this card, each with a response to
    /// receive: the first connect's waiter plus one per identical
    /// retry. One decision answers the lot.
    responders: Vec<oneshot::Sender<Decision>>,
}

/// One line of the activity log: what was asked, by whom, and why it
/// went the way it went. Privacy mode is structural — there is no field
/// a param could hide in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: u64,
    pub app: String,
    pub method: String,
    pub summary: String,
    pub verdict: String,
}

#[derive(Default)]
struct Inner {
    apps: Vec<Paired>,
    prompts: Vec<(String, Prompt)>,
    /// `{pubkey}:{method}` until unix-seconds — the remembered answers,
    /// an hour at most by construction of the verb.
    remembered: HashMap<String, u64>,
    log: Vec<LogEntry>,
    next_id: u64,
}

/// How long an Ask waits for a person before it denies itself. A
/// prompt that waited forever was a signature waiting to happen —
/// approved a week later, it executed. Five minutes is what the app
/// on the other side is willing to wait anyway.
const PROMPT_TTL_SECS: u64 = 300;
/// The activity log's cap: the last 500 entries survive, which is
/// more than a person reads and few enough that the file stays a file.
const LOG_CAP: usize = 500;

/// The engine: the [`super::bunker::Gate`] the bunker's worker asks,
/// and the state the socket verbs (`prompts`, `approve`, `deny`,
/// `apps`, `revoke`) drive. Cloneable on purpose — the handle is cheap
/// and the state is shared — because the verbs and the gate are two
/// roads into one decision record.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Mutex<Inner>>,
    state_path: Option<PathBuf>,
    /// How long an ask waits. The constant in production; the test
    /// seam shrinks it to something a test can wait out.
    prompt_ttl: Duration,
}

impl Engine {
    fn with_ttl(state_dir: Option<PathBuf>, prompt_ttl: Duration) -> Self {
        let engine = Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            state_path: state_dir,
            prompt_ttl,
        };
        engine.load_apps();
        engine.load_log();
        engine
    }

    /// `state_dir` is where `apps.json` lives; `None` makes the engine
    /// memory-only, which is what the offline tests run against.
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        Self::with_ttl(state_dir, Duration::from_secs(PROMPT_TTL_SECS))
    }

    /// The same engine with a window a test can afford to wait out.
    #[cfg(test)]
    fn with_prompt_ttl(state_dir: Option<PathBuf>, ttl: Duration) -> Self {
        Self::with_ttl(state_dir, ttl)
    }

    /// The state file the pairings persist to, if this engine persists.
    fn apps_file(&self) -> Option<PathBuf> {
        self.state_path.as_ref().map(|dir| dir.join("apps.json"))
    }

    /// The activity log's file, beside the pairings. Newest last, so a
    /// reader appends; the cap lives at the write.
    fn log_file(&self) -> Option<PathBuf> {
        self.state_path.as_ref().map(|dir| dir.join("log.json"))
    }

    fn load_log(&self) {
        let Some(file) = self.log_file() else { return };
        let Ok(text) = std::fs::read_to_string(&file) else { return };
        match serde_json::from_str::<Vec<LogEntry>>(&text) {
            Ok(log) => {
                self.inner.lock().expect("the policy lock").log = log;
            }
            Err(e) => {
                eprintln!("kuma-nostrd: {file:?} did not parse; starting with an empty log: {e}")
            }
        }
    }

    fn persist_log(&self, inner: &Inner) {
        let Some(file) = self.log_file() else { return };
        if let Some(parent) = file.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("kuma-nostrd: cannot create {parent:?}: {e}");
                return;
            }
        }
        match serde_json::to_string_pretty(&inner.log) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&file, text) {
                    eprintln!("kuma-nostrd: cannot write {file:?}: {e}");
                }
            }
            Err(e) => eprintln!("kuma-nostrd: the activity log did not serialize: {e}"),
        }
    }

    /// The activity log's one door: push, cap, persist. The log is the
    /// layer's memory — what this app asked, what it got — and a
    /// memory that died at every restart answered nothing, so it rides
    /// `log.json` beside the pairings, capped at the last 500 entries.
    /// Every push in the engine goes through here, so the cap cannot
    /// be forgotten at a new call site.
    fn record_log(&self, inner: &mut Inner, entry: LogEntry) {
        inner.log.push(entry);
        if inner.log.len() > LOG_CAP {
            let trim = inner.log.len() - LOG_CAP;
            inner.log.drain(0..trim);
        }
        self.persist_log(inner);
    }

    /// The activity log, oldest first — what the `log` verb answers
    /// and what the panel's Activity tab renders newest-first.
    pub fn log(&self) -> Vec<LogEntry> {
        self.inner.lock().expect("the policy lock").log.clone()
    }

    fn load_apps(&self) {
        let Some(file) = self.apps_file() else { return };
        let Ok(text) = std::fs::read_to_string(&file) else { return };
        let Ok(apps) = serde_json::from_str::<Vec<Paired>>(&text) else {
            eprintln!("kuma-nostrd: {file:?} did not parse; starting with no pairings");
            return;
        };
        self.inner.lock().expect("the policy lock").apps = apps;
    }

    fn persist_apps(&self, inner: &Inner) {
        let Some(file) = self.apps_file() else { return };
        if let Some(parent) = file.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("kuma-nostrd: cannot create {parent:?}: {e}");
                return;
            }
        }
        match serde_json::to_string_pretty(&inner.apps) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&file, text) {
                    eprintln!("kuma-nostrd: cannot write {file:?}: {e}");
                }
            }
            Err(e) => eprintln!("kuma-nostrd: the pairing list did not serialize: {e}"),
        }
    }

    /// Pair an app, or find it already paired. The default level is
    /// Ask, and no path changes a level except the panel's toggle —
    /// pairing is not a decision the daemon makes for anyone. The
    /// metadata (the app's name and image, its own unverified claim)
    /// lands on the first pairing and fills in on reconnects: an app
    /// that ships a name later gets the better label.
    pub fn pair_with_metadata(
        &self,
        app: &PublicKey,
        name: Option<String>,
        image: Option<String>,
        perms: Option<String>,
        url: Option<String>,
    ) {
        let mut inner = self.inner.lock().expect("the policy lock");
        let hex = app.to_string();
        if let Some(paired) = inner.apps.iter_mut().find(|p| p.pubkey == hex) {
            if paired.name.is_none() {
                paired.name = name;
            }
            if paired.image.is_none() {
                paired.image = image;
            }
            // A nostrconnect URI's perms fill in on pairing; they do
            // not overwrite a claim the app made later.
            if paired.perms.is_none() {
                paired.perms = perms;
            }
            if paired.url.is_none() {
                paired.url = url;
            }
            return;
        }
        inner.apps.push(Paired {
            pubkey: hex.clone(),
            level: Level::Ask,
            paired_at: unix_now(),
            name,
            image,
            perms,
            url,
            revoked_at: None,
            request_count: 0,
            last_used_at: None,
        });
        self.record_log(
            &mut inner,
            LogEntry {
                at: unix_now(),
                app: hex,
                method: "connect".into(),
                summary: "paired".into(),
                verdict: "paired at ask".into(),
            },
        );
        self.persist_apps(&inner);
    }

    fn pair(&self, app: &PublicKey) {
        self.pair_with_metadata(app, None, None, None, None);
    }

    /// The paired apps, for the `apps` verb.
    pub fn apps(&self) -> Vec<Paired> {
        self.inner.lock().expect("the policy lock").apps.clone()
    }

    /// Forget an app. Answers whether one was actually removed, so the
    /// verb can tell the caller "no such app" instead of nodding.
    /// Revoke an app: the tombstone's act. The record stays — revoked,
    /// not deleted, so the refusal has teeth across restarts and the
    /// panel can offer the way back — while the standing answers go,
    /// because a grant the person erased does not come back with the
    /// un-revoke. Answers whether a paired app was found, so the verb
    /// can tell the caller "no such app".
    pub fn revoke(&self, app: &str) -> bool {
        let mut inner = self.inner.lock().expect("the policy lock");
        let Some(paired) = inner.apps.iter_mut().find(|p| p.pubkey == app) else {
            return false;
        };
        let fresh = paired.revoked_at.is_none();
        paired.revoked_at = Some(unix_now());
        inner.remembered.retain(|key, _| !key.starts_with(&format!("{app}:")));
        if fresh {
            self.record_log(
                &mut inner,
                LogEntry {
                    at: unix_now(),
                    app: app.to_string(),
                    method: "revoke".into(),
                    summary: "the person revoked the app".into(),
                    verdict: "tombstoned".into(),
                },
            );
        }
        self.persist_apps(&inner);
        true
    }

    /// Clear a tombstone: the person un-revoked. The way back in is a
    /// freshly minted URI — the app's original secret burned at its
    /// first connect — so the un-revoke opens the door without
    /// opening the gate: the pairing lands when the app presents the
    /// new URI.
    pub fn unrevoke(&self, app: &str) -> bool {
        let mut inner = self.inner.lock().expect("the policy lock");
        let Some(paired) = inner.apps.iter_mut().find(|p| p.pubkey == app) else {
            return false;
        };
        if paired.revoked_at.is_none() {
            return false;
        }
        paired.revoked_at = None;
        self.record_log(
            &mut inner,
            LogEntry {
                at: unix_now(),
                app: app.to_string(),
                method: "unrevoke".into(),
                summary: "the person un-revoked the app".into(),
                verdict: "cleared".into(),
            },
        );
        self.persist_apps(&inner);
        true
    }

    /// The person's label for an app: what the ask cards and the
    /// pairing list show instead of a pubkey fragment. The person's
    /// word outranks the client's own metadata claim, so this sets
    /// rather than fills. Answers whether a paired app was found.
    pub fn rename(&self, app: &str, name: &str) -> bool {
        let mut inner = self.inner.lock().expect("the policy lock");
        let Some(paired) = inner.apps.iter_mut().find(|p| p.pubkey == app) else {
            return false;
        };
        paired.name = Some(name.to_string());
        self.record_log(
            &mut inner,
            LogEntry {
                at: unix_now(),
                app: app.to_string(),
                method: "label".into(),
                summary: "the person named the app".into(),
                verdict: "named".into(),
            },
        );
        self.persist_apps(&inner);
        true
    }

    /// The person's delete: the record and its standing answers go
    /// together — the same deletion the app's own goodbye performs,
    /// logged as the person's act. Not a tombstone: revoke is the
    /// ban, delete is the removal, and re-pairing is a fresh URI
    /// either way.
    pub fn delete(&self, app: &str) -> bool {
        let mut inner = self.inner.lock().expect("the policy lock");
        let removed = remove_app(&mut inner, app);
        if removed {
            self.record_log(
                &mut inner,
                LogEntry {
                    at: unix_now(),
                    app: app.to_string(),
                    method: "delete".into(),
                    summary: "the person deleted the app".into(),
                    verdict: "removed".into(),
                },
            );
            self.persist_apps(&inner);
        }
        removed
    }

    /// The app's own goodbye: the removal delete does, logged as the
    /// caller's act rather than the person's. A logout from an app
    /// with no session removes nothing and is still answered — the
    /// ack is the courtesy, the log the record only when there was
    /// something to remove.
    pub fn logout(&self, app: &str) -> bool {
        let mut inner = self.inner.lock().expect("the policy lock");
        let removed = remove_app(&mut inner, app);
        if removed {
            self.record_log(
                &mut inner,
                LogEntry {
                    at: unix_now(),
                    app: app.to_string(),
                    method: "logout".into(),
                    summary: "the app ended its own session".into(),
                    verdict: "removed".into(),
                },
            );
            self.persist_apps(&inner);
        }
        removed
    }

    /// A protocol-level fact the bunker served or shed without the
    /// gate — `switch_relays` names its relays to any paired app, a
    /// sender over its rate is answered with nothing — recorded so
    /// the activity log's answer stays complete. The verdict is a
    /// fact here, not a decision.
    pub fn noted(&self, app: &str, method: &str, summary: String, verdict: &str) {
        let mut inner = self.inner.lock().expect("the policy lock");
        self.record_log(
            &mut inner,
            LogEntry {
                at: unix_now(),
                app: app.to_string(),
                method: method.into(),
                summary,
                verdict: verdict.into(),
            },
        );
    }

    /// The pending asks, for the `prompts` verb and the panel. The
    /// signature's material is structured here — kind, its name, the
    /// content whole — because the decision's rows are the daemon's
    /// vocabulary to speak, not the panel's to parse.
    pub fn prompts(&self) -> Vec<PromptView> {
        let inner = self.inner.lock().expect("the policy lock");
        inner
            .prompts
            .iter()
            .map(|(id, prompt)| {
                let event_json = (prompt.method == NostrConnectMethod::SignEvent)
                    .then(|| prompt.params.first())
                    .flatten();
                let kind = event_json.and_then(|json| event_kind(json));
                PromptView {
                    id: id.clone(),
                    app: prompt.app.to_string(),
                    method: format!("{:?}", prompt.method),
                    summary: prompt.summary.clone(),
                    kind,
                    kind_label: kind.and_then(kind_label).map(str::to_string),
                    content: event_json.and_then(|json| event_content(json)),
                    sensitive: is_sensitive(&prompt.method, &prompt.params),
                    retries: prompt.responders.len() as u64,
                    detail: detail(&prompt.method, &prompt.params),
                }
            })
            .collect()
    }

    /// Answer an ask with yes. `remember` grants the same method a
    /// standing answer for the given duration — the longest the engine
    /// can mint, and the only standing grant besides Trust.
    pub fn approve(&self, id: &str, remember: Option<Duration>) -> Result<()> {
        self.answer(id, Decision::Allow, remember)
    }

    /// Answer an ask with no.
    pub fn deny(&self, id: &str) -> Result<()> {
        self.answer(id, Decision::Deny("denied".into()), None)
    }

    fn answer(&self, id: &str, decision: Decision, remember: Option<Duration>) -> Result<()> {
        let mut inner = self.inner.lock().expect("the policy lock");
        let index = inner
            .prompts
            .iter()
            .position(|(prompt_id, _)| prompt_id == id)
            .ok_or_else(|| anyhow!("no prompt {id}"))?;
        let (_, prompt) = inner.prompts.remove(index);
        if let Some(duration) = remember {
            let key = remember_key(&prompt.app, &prompt.method);
            inner.remembered.insert(key, unix_now() + duration.as_secs());
        }
        // The channel is the single arbiter of who decides: a send
        // that lands is the decision, and a send that fails means the
        // window closed first and the wait is gone. One answer goes to
        // every waiter the card collected — the first ask and its
        // retries together — and the verdict is honest about whether
        // anyone was still there to receive it.
        let answer_verdict = match &decision {
            Decision::Allow => "allowed".to_string(),
            Decision::Deny(reason) => format!("denied: {reason}"),
        };
        let mut received = false;
        for responder in prompt.responders {
            if responder.send(decision.clone()).is_ok() {
                received = true;
            }
        }
        let verdict = if received {
            answer_verdict
        } else {
            "an answer came after the window closed".to_string()
        };
        self.record_log(
            &mut inner,
            LogEntry {
                at: unix_now(),
                app: prompt.app.to_string(),
                method: format!("{:?}", prompt.method),
                summary: prompt.summary.clone(),
                verdict,
            },
        );
        Ok(())
    }

    /// The activity log, newest last.
    pub fn activity(&self) -> Vec<LogEntry> {
        self.inner.lock().expect("the policy lock").log.clone()
    }

    /// Set an app's level — the panel's one toggle, the thing that
    /// relaxes a new app's Ask to Basic or escalates to the standing
    /// grant the doctor will warn about. Unknown apps are refused:
    /// leveling an app that never paired is a typo, not a policy.
    pub fn set_level(&self, app: &str, level: Level) -> Result<()> {
        let mut inner = self.inner.lock().expect("the policy lock");
        let paired = inner
            .apps
            .iter_mut()
            .find(|p| p.pubkey == app)
            .ok_or_else(|| anyhow!("no paired app {app}"))?;
        paired.level = level;
        self.persist_apps(&inner);
        Ok(())
    }
}

impl super::bunker::Gate for Engine {
    async fn decide(
        &self,
        app: &PublicKey,
        method: &NostrConnectMethod,
        params: &[String],
    ) -> Decision {
        self.pair(app);
        let summary = summarize(method, params);
        let sensitive = is_sensitive(method, params);

        {
            let mut inner = self.inner.lock().expect("the policy lock");
            // The ask is a use of the app's pairing, whatever the level
            // answers: the count and the last-used stamp are the list's
            // second line, and they ride the same lock as the level
            // read so they cannot disagree with a decision made here.
            let paired = inner
                .apps
                .iter_mut()
                .find(|p| p.pubkey == app.to_string())
                .expect("the pairing the pair above just made");
            paired.request_count += 1;
            paired.last_used_at = Some(unix_now());
            let level = paired.level;
            let trusted_everywhere = level == Level::Trust;
            let trusted_here = inner
                .remembered
                .get(&remember_key(app, method))
                .is_some_and(|until| *until > unix_now());
            let allowed_without_asking =
                trusted_everywhere || trusted_here || (level == Level::Basic && !sensitive);
            if allowed_without_asking {
                let verdict = if trusted_everywhere {
                    "allowed (trust)"
                } else if trusted_here {
                    "allowed (remembered)"
                } else {
                    "allowed (basic)"
                };
                self.record_log(
                    &mut inner,
                    LogEntry {
                        at: unix_now(),
                        app: app.to_string(),
                        method: format!("{method:?}"),
                        summary: summary.clone(),
                        verdict: verdict.into(),
                    },
                );
                self.persist_apps(&inner);
                return Decision::Allow;
            }
        }

        // The ask: an id, a channel, and the lock released the moment
        // the prompt is registered — the answer comes back through the
        // oneshot whenever it comes. An identical ask that arrives
        // while this one waits joins it instead of stacking a second
        // card: one decision answers every waiter, each through its
        // own response id.
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut inner = self.inner.lock().expect("the policy lock");
            let joined = inner
                .prompts
                .iter_mut()
                .find(|(_, p)| p.app == *app && p.method == *method && p.params == params);
            match joined {
                Some((existing_id, existing)) => {
                    // The joining ask borrows the card's id for its
                    // expiry bookkeeping; its own response id lives in
                    // its request event, not here.
                    existing.responders.push(tx);
                    existing_id.clone()
                }
                None => {
                    inner.next_id += 1;
                    let id = format!("{}-{:04}", unix_now(), inner.next_id);
                    inner.prompts.push((
                        id.clone(),
                        Prompt {
                            app: *app,
                            method: *method,
                            params: params.to_vec(),
                            summary: summary.clone(),
                            responders: vec![tx],
                        },
                    ));
                    id
                }
            }
        };
        eprintln!("kuma-nostrd: asking {id}: {summary}");
        // The window: a prompt that waited forever was a signature
        // waiting to happen. On expiry the ask denies itself, leaves
        // the queue, and the log records it — the app on the other
        // side gets its refusal, and approving the stale id later is
        // the honest error.
        match tokio::time::timeout(self.prompt_ttl, rx).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => Decision::Deny("the prompt was dropped".into()),
            Err(_) => {
                let mut inner = self.inner.lock().expect("the policy lock");
                // An answer may have landed in the gap between the
                // deadline and this lock: its send failed against a
                // wait already gone, and its verdict is already in the
                // log. Only a prompt still in the queue is one nobody
                // answered — this arm logs the expiry for that one.
                if inner.prompts.iter().any(|(prompt_id, _)| prompt_id == &id) {
                    inner.prompts.retain(|(prompt_id, _)| prompt_id != &id);
                    self.record_log(
                        &mut inner,
                        LogEntry {
                            at: unix_now(),
                            app: app.to_string(),
                            method: format!("{method:?}"),
                            summary,
                            verdict: "expired unanswered".into(),
                        },
                    );
                }
                Decision::Deny("the ask timed out unanswered".into())
            }
        }
    }
}
/// The logout's body: the record and its standing answers go
/// together — the app's own goodbye is a deletion, not a tombstone,
/// because re-pairing is a fresh URI either way. Does not persist;
/// the caller does, and logs what it removed.
fn remove_app(inner: &mut Inner, app: &str) -> bool {
    let before = inner.apps.len();
    inner.apps.retain(|p| p.pubkey != app);
    inner.remembered.retain(|key, _| !key.starts_with(&format!("{app}:")));
    inner.apps.len() < before
}

/// The exact event an approval shows, privacy mode's one exception: a
/// signature cannot be judged blind, so the event's content rides in
/// whole — carried in the view's `content` field, not a JSON wall.
/// The decrypt methods name their scope without their payload — the
/// payload is the secret, and the prompt is rendered on screens.
fn detail(method: &NostrConnectMethod, params: &[String]) -> Option<String> {
    match method {
        NostrConnectMethod::Nip04Decrypt | NostrConnectMethod::Nip44Decrypt => {
            params.first().map(|pk| format!("decrypt for {pk}"))
        }
        NostrConnectMethod::Nip04Encrypt => params.first().map(|pk| format!("encrypt for {pk}")),
        _ => None,
    }
}

/// The kind number an event JSON carries, read leniently — a number or
/// a numeric string, and nothing else about the shape matters. The
/// strict typed parse refused whole events over fields it did not
/// expect, and every one of them became "unreadable" to the one person
/// whose decision was being asked.
fn event_kind(json: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    match value.get("kind")? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// The verb a blossom authorization (kind 24242) authorizes — BUD-01's
/// own `t` tag: get, upload, delete, list. Read leniently like the
/// kind, and with the same direction: an unreadable verb is one that
/// asks.
fn blossom_auth_verb(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tags = value.get("tags")?.as_array()?;
    for tag in tags {
        let Some(pair) = tag.as_array() else { continue };
        if pair.first().and_then(|t| t.as_str()) == Some("t") {
            return pair.get(1).and_then(|t| t.as_str()).map(str::to_string);
        }
    }
    None
}

/// The event's content, whole, when it is a string field — the
/// judgment's material.
fn event_content(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.get("content").and_then(|c| c.as_str()).map(str::to_string)
}

/// What a kind number means — the noun for the detail row. The
/// unknown kinds stay honest with their number rather than pretending
/// to a phrase. (Signet's table, kept.)
fn kind_label(kind: u64) -> Option<&'static str> {
    Some(match kind {
        0 => "Metadata",
        1 => "Note",
        3 => "Contacts",
        4 => "DM",
        5 => "Delete",
        6 => "Repost",
        7 => "Reaction",
        8 => "Badge Award",
        9 => "Chat Message",
        10 => "Group Chat",
        1984 => "Report",
        9734 => "Zap Request",
        9735 => "Zap",
        10000 => "Mute List",
        10001 => "Pin List",
        10002 => "Relay List",
        22242 => "HTTP Auth",
        24242 => "Blossom Auth",
        27235 => "HTTP Auth",
        30000 => "Categorized People",
        30001 => "Categorized Bookmarks",
        30023 => "Long-form Content",
        30078 => "App-specific Data",
        _ => return None,
    })
}

/// What a signature ask is, in words — Signet's present-tense table,
/// the sentence a person decides on. The unknown kinds say their
/// number; an unreadable event says so as a shape, not a shrug.
fn sign_event_summary(params: &[String]) -> String {
    match params.first().and_then(|json| event_kind(json)) {
        Some(kind) => match kind {
            0 => "Update profile".into(),
            1 => "Sign a note".into(),
            3 => "Update contacts".into(),
            4 => "Send DM".into(),
            5 => "Delete event".into(),
            6 => "Repost".into(),
            7 => "Sign a reaction".into(),
            9 => "Sign chat message".into(),
            9734 => "Sign zap request".into(),
            9735 => "Sign zap".into(),
            10002 => "Update relay list".into(),
            22242 => "Sign http auth".into(),
            24133 => "Sign NIP-46 response".into(),
            24242 => match params.first().and_then(|json| blossom_auth_verb(json)).as_deref() {
                Some("get") => "Authorize a download (blossom auth)".into(),
                Some("upload") => "Authorize an upload (blossom auth)".into(),
                Some("delete") => "Delete blobs (blossom auth)".into(),
                Some("list") => "List blobs (blossom auth)".into(),
                _ => "Sign blossom authorization".into(),
            },
            27235 => "Sign http auth".into(),
            30023 => "Sign article".into(),
            other => format!("Sign event (kind {other})"),
        },
        None => "Sign event".into(),
    }
}

/// The ask's one line, for the prompt and the log alike: the method in
/// words. Never the content — the summary goes in the log, and a log
/// that carries every signed sentence is a diary nobody asked for.
fn summarize(method: &NostrConnectMethod, params: &[String]) -> String {
    match method {
        NostrConnectMethod::SignEvent => sign_event_summary(params),
        NostrConnectMethod::GetPublicKey => "Get public key".into(),
        NostrConnectMethod::Nip04Encrypt => "Encrypt message (NIP-04)".into(),
        NostrConnectMethod::Nip04Decrypt => "Decrypt message (NIP-04)".into(),
        NostrConnectMethod::Nip44Encrypt => "Encrypt message (NIP-44)".into(),
        NostrConnectMethod::Nip44Decrypt => "Decrypt message (NIP-44)".into(),
        NostrConnectMethod::Ping => "Ping".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn remember_key(app: &PublicKey, method: &NostrConnectMethod) -> String {
    format!("{}:{method:?}", app)
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::super::bunker::Gate;
    use super::*;

    fn engine() -> Engine {
        Engine::new(None)
    }

    fn app() -> PublicKey {
        nostr::key::Keys::generate().public_key()
    }

    fn everyday(method: &NostrConnectMethod) -> Vec<String> {
        match method {
            // A text note is as everyday as signing gets.
            NostrConnectMethod::SignEvent => {
                let unsigned = nostr::event::UnsignedEvent::new(
                    nostr::key::Keys::generate().public_key(),
                    nostr::types::Timestamp::now(),
                    nostr::event::Kind::TextNote,
                    [],
                    "hello",
                );
                vec![unsigned.as_json()]
            }
            _ => vec![],
        }
    }

    fn kind_write(kind: u16) -> Vec<String> {
        let unsigned = nostr::event::UnsignedEvent::new(
            nostr::key::Keys::generate().public_key(),
            nostr::types::Timestamp::now(),
            nostr::event::Kind::from_u16(kind),
            [],
            "{}",
        );
        vec![unsigned.as_json()]
    }

    /// A blossom authorization (kind 24242) with its verb in the `t`
    /// tag, built as hand JSON on purpose: the policy layer reads the
    /// event leniently, and this tests the lenient reader against the
    /// shape an app actually sends. An empty verb is the unreadable
    /// case — a `t` tag with nothing usable in it.
    fn blossom_write(verb: &str) -> Vec<String> {
        let event = serde_json::json!({
            "pubkey": nostr::key::Keys::generate().public_key().to_string(),
            "created_at": unix_now(),
            "kind": 24242,
            "tags": [["t", verb], ["expiration", "4102444800"]],
            "content": ""
        });
        vec![event.to_string()]
    }

    fn profile_write() -> Vec<String> {
        let unsigned = nostr::event::UnsignedEvent::new(
            nostr::key::Keys::generate().public_key(),
            nostr::types::Timestamp::now(),
            nostr::event::Kind::Metadata,
            [],
            "{}",
        );
        vec![unsigned.as_json()]
    }

    #[tokio::test]
    async fn a_newly_paired_app_defaults_to_ask_everything() {
        let engine = engine();
        let app = app();
        // The ask runs apart from the answer, the way the worker and
        // the socket verb do: an inline await here would wait on a
        // prompt nobody has answered yet.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        let prompts = engine.prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].method, "GetPublicKey");
        engine.approve(&prompts[0].id, None).unwrap();
        assert!(matches!(ask.await.unwrap(), Decision::Allow));
        assert!(engine.prompts().is_empty(), "an answered prompt leaves the queue");
    }

    #[tokio::test]
    async fn ask_blocks_until_the_answer_arrives() {
        let engine = engine();
        let app = app();
        let engine_for_answer = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_answer.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        let prompt = engine.prompts().into_iter().next().expect("the ask is queued");
        engine.approve(&prompt.id, None).unwrap();
        assert!(matches!(ask.await.unwrap(), Decision::Allow));
    }

    #[tokio::test]
    async fn deny_denies_and_the_reason_travels() {
        let engine = engine();
        let app = app();
        let engine_for_answer = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_answer.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.deny(&engine.prompts()[0].id).unwrap();
        match ask.await.unwrap() {
            Decision::Deny(reason) => assert!(reason.contains("denied")),
            Decision::Allow => panic!("a deny arrived as an allow"),
        }
        let log = engine.activity();
        assert!(log.last().unwrap().verdict.contains("denied"));
    }

    #[tokio::test]
    async fn remember_answers_the_next_one_without_asking() {
        let engine = engine();
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, Some(Duration::from_secs(3600))).unwrap();
        assert!(matches!(ask.await.unwrap(), Decision::Allow));

        // The second ask of the same method is answered by the
        // remember grant: no prompt, allow, and the log says why.
        let second = engine.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await;
        assert!(matches!(second, Decision::Allow));
        assert!(engine.prompts().is_empty());
        assert!(engine.activity().iter().any(|e| e.verdict.contains("remembered")));
    }

    #[tokio::test]
    async fn sensitive_methods_ask_at_basic_and_the_kind_is_what_makes_them_so() {
        let engine = engine();
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::SignEvent, &profile_write()).await
        });
        tokio::task::yield_now().await;
        // Basic would wave a text note through — but a profile write
        // is on the sensitive list, so there is a prompt to answer.
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        assert!(matches!(ask.await.unwrap(), Decision::Allow));

        // The panel's toggle: the app relaxes to Basic, and now the
        // kind is what separates a wave-through from an ask.
        engine.set_level(&app.to_string(), Level::Basic).unwrap();

        // The everyday sign asks nothing.
        let everyday = engine
            .decide(&app, &NostrConnectMethod::SignEvent, &everyday(&NostrConnectMethod::SignEvent))
            .await;
        assert!(matches!(everyday, Decision::Allow));

        // The profile write still does — the spawned shape, because
        // the answer comes from the queue.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::SignEvent, &profile_write()).await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "a sensitive write asks at Basic");
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        assert!(matches!(ask.await.unwrap(), Decision::Allow));
        assert!(engine.prompts().is_empty());
    }

    #[tokio::test]
    async fn trust_is_the_standing_grant_the_doctor_warns_about() {
        let engine = engine();
        let app = app();
        // Pair by asking once, then escalate to Trust the way the panel
        // will: through the engine's own state.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();

        {
            let mut inner = engine.inner.lock().unwrap();
            for paired in inner.apps.iter_mut() {
                paired.level = Level::Trust;
            }
        }
        let sensitive = engine
            .decide(&app, &NostrConnectMethod::Nip44Decrypt, &["pk".into(), "payload".into()])
            .await;
        assert!(matches!(sensitive, Decision::Allow), "trust is a standing grant");
        assert!(engine.prompts().is_empty());
    }

    #[tokio::test]
    async fn at_basic_nip44_encrypt_runs_unattended_and_nip04_encrypt_asks() {
        let engine = engine();
        let app = app();
        // Pair by asking once, then relax to Basic the way the panel will.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
        engine.set_level(&app.to_string(), Level::Basic).unwrap();

        // NIP-44 is general-purpose encryption: it runs without a prompt.
        let allowed = engine
            .decide(&app, &NostrConnectMethod::Nip44Encrypt, &[app.to_string(), "text".into()])
            .await;
        assert!(matches!(allowed, Decision::Allow));
        assert!(engine.prompts().is_empty());

        // NIP-04's job is private messages; encrypting one is writing one.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask
                .decide(&app, &NostrConnectMethod::Nip04Encrypt, &[app.to_string(), "text".into()])
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "nip04_encrypt asks at Basic");
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        assert!(matches!(ask.await.unwrap(), Decision::Allow));
    }

    #[tokio::test]
    async fn at_basic_only_explicitly_safe_kinds_sign_unattended() {
        let engine = engine();
        let app = app();
        // Pair by asking once, then relax to Basic.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
        engine.set_level(&app.to_string(), Level::Basic).unwrap();

        // A safe kind — a text note — signs unattended.
        let allowed = engine.decide(&app, &NostrConnectMethod::SignEvent, &kind_write(1)).await;
        assert!(matches!(allowed, Decision::Allow), "a safe kind rides at Basic");
        assert!(engine.prompts().is_empty());

        // An unknown kind asks: safe by default is the direction.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::SignEvent, &kind_write(9999)).await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "an unknown kind asks at Basic");
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
    }

    #[tokio::test]
    async fn a_blossom_delete_authorization_asks_at_basic() {
        let engine = engine();
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
        engine.set_level(&app.to_string(), Level::Basic).unwrap();

        // The everyday verbs — get, upload, list — ride unattended,
        // exactly what the kind's safe-list entry promised before the
        // verb read existed.
        for verb in ["get", "upload", "list"] {
            let allowed =
                engine.decide(&app, &NostrConnectMethod::SignEvent, &blossom_write(verb)).await;
            assert!(matches!(allowed, Decision::Allow), "blossom {verb} rides at Basic");
            assert!(engine.prompts().is_empty(), "blossom {verb} popped no card");
        }

        // A delete authorization is a deletion wearing another kind:
        // it asks the way kind 5 does.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask
                .decide(&app, &NostrConnectMethod::SignEvent, &blossom_write("delete"))
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "a blossom delete asks at Basic");
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();

        // An auth whose verb cannot be read asks too — the lenient
        // read fails toward the person, not toward the signature.
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::SignEvent, &blossom_write("")).await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "an unreadable blossom verb asks at Basic");
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
    }

    #[tokio::test]
    async fn the_newly_sensitive_kinds_ask_at_basic() {
        let engine = engine();
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
        engine.set_level(&app.to_string(), Level::Basic).unwrap();

        // Kinds 4 (NIP-04 DM), 22242 (client authentication), 24133
        // (nested NIP-46 signing), and the wallet kinds 13194, 23194,
        // 23195: none is on the safe list, so each asks.
        for kind in [4, 22242, 24133, 13194, 23194, 23195] {
            let engine_for_ask = engine.clone();
            let ask = tokio::spawn(async move {
                engine_for_ask.decide(&app, &NostrConnectMethod::SignEvent, &kind_write(kind)).await
            });
            tokio::task::yield_now().await;
            assert_eq!(engine.prompts().len(), 1, "kind {kind} asks at Basic");
            engine.approve(&engine.prompts()[0].id, None).unwrap();
            ask.await.unwrap();
        }
    }

    #[tokio::test]
    async fn an_unreadable_sign_event_asks_even_at_basic() {
        let engine = engine();
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
        engine.set_level(&app.to_string(), Level::Basic).unwrap();

        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask
                .decide(&app, &NostrConnectMethod::SignEvent, &["not json".to_string()])
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "an unreadable event asks");
        engine.approve(&engine.prompts()[0].id, None).unwrap();
        ask.await.unwrap();
    }

    #[tokio::test]
    async fn an_unanswered_prompt_times_out_into_a_denial() {
        let engine = Engine::with_prompt_ttl(None, Duration::from_millis(50));
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        assert_eq!(engine.prompts().len(), 1, "the ask is queued");

        // Nobody answers. The window closes; the app gets its denial,
        // the queue forgets the ask, and the log records the expiry.
        tokio::time::sleep(Duration::from_millis(150)).await;
        match ask.await.unwrap() {
            Decision::Deny(reason) => assert!(reason.contains("timed out"), "{reason}"),
            Decision::Allow => panic!("an unanswered ask timed out into an allow"),
        }
        assert!(engine.prompts().is_empty(), "an expired prompt leaves the queue");
        let log = engine.activity();
        assert!(log.last().unwrap().verdict.contains("expired"), "{log:?}");
    }

    #[tokio::test]
    async fn approving_an_expired_prompt_is_an_honest_error() {
        let engine = Engine::with_prompt_ttl(None, Duration::from_millis(50));
        let app = app();
        let engine_for_ask = engine.clone();
        let ask = tokio::spawn(async move {
            engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
        });
        tokio::task::yield_now().await;
        let id = engine.prompts()[0].id.clone();

        tokio::time::sleep(Duration::from_millis(150)).await;
        ask.await.unwrap();
        assert!(engine.approve(&id, None).is_err(), "an expired id approves nothing");
        // And the decision after expiry is still a denial of record.
        assert!(engine.activity().iter().any(|e| e.verdict.contains("expired")));
    }

    #[tokio::test]
    async fn pairings_persist_through_a_reboot_of_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let app = app();
        {
            let engine = Engine::new(Some(dir.path().to_path_buf()));
            let engine_for_ask = engine.clone();
            let ask = tokio::spawn(async move {
                engine_for_ask.decide(&app, &NostrConnectMethod::GetPublicKey, &[]).await
            });
            tokio::task::yield_now().await;
            engine.approve(&engine.prompts()[0].id, None).unwrap();
            ask.await.unwrap();
        }
        // A fresh engine over the same state dir finds the pairing.
        let engine = Engine::new(Some(dir.path().to_path_buf()));
        let apps = engine.apps();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].pubkey, app.to_string());
        assert_eq!(apps[0].level, Level::Ask);
    }

    #[test]
    fn revoke_tombstones_and_logout_forgets() {
        let engine = engine();
        let app = app();
        engine.pair(&app);

        // Revoke: the record stays, tombstoned — the refusal has
        // teeth across restarts — while a second revoke is no news.
        assert!(engine.revoke(&app.to_string()));
        let record = &engine.apps()[0];
        assert!(record.revoked_at.is_some(), "the tombstone is the record's own");
        assert!(engine.revoke(&app.to_string()), "re-revoking finds the app");

        // Un-revoke clears the tombstone; the record is the app's way
        // back.
        assert!(engine.unrevoke(&app.to_string()));
        assert!(engine.apps()[0].revoked_at.is_none());
        assert!(!engine.unrevoke(&app.to_string()), "un-revoking a clean record is no news");

        // Logout: the app's own goodbye — a deletion, not a
        // tombstone. Re-pairing is a fresh URI either way.
        assert!(engine.logout(&app.to_string()));
        assert!(engine.apps().is_empty());
        assert!(!engine.logout(&app.to_string()), "logging out twice removes nothing");
    }

    #[test]
    fn delete_removes_where_revoke_remembers() {
        let engine = engine();
        let app = app();
        engine.pair(&app);

        // Delete: the record goes, it does not become a tombstone.
        assert!(engine.delete(&app.to_string()));
        assert!(engine.apps().is_empty(), "a deleted app leaves no record");
        assert!(!engine.delete(&app.to_string()), "deleting twice removes nothing");

        // The same pubkey pairs again — deletion forgot, it did not
        // ban: the way back is a fresh URI, not an un-revoke.
        engine.pair(&app);
        let paired = engine.apps();
        assert_eq!(paired.len(), 1);
        assert!(paired[0].revoked_at.is_none(), "a re-paired app is not born revoked");
    }

    #[test]
    fn the_summary_speaks_kinds_and_reads_leniently() {
        // The strict typed parse refused whole events over fields it
        // did not expect; the summary needs one integer, read with
        // that much honesty.
        let event = r#"{"kind":1,"content":"hello","tags":[]}"#;
        assert_eq!(summarize(&NostrConnectMethod::SignEvent, &[event.to_string()]), "Sign a note");

        // A kind as a string, an unknown kind, and a shape that is not
        // an event at all: each says what it is rather than
        // "unreadable".
        let string_kind = r#"{"kind":"10002","content":"","tags":[]}"#;
        assert_eq!(
            summarize(&NostrConnectMethod::SignEvent, &[string_kind.to_string()]),
            "Update relay list"
        );
        let unknown = r#"{"kind":34567,"content":"","tags":[]}"#;
        assert_eq!(
            summarize(&NostrConnectMethod::SignEvent, &[unknown.to_string()]),
            "Sign event (kind 34567)"
        );
        let not_an_event = "hello";
        assert_eq!(
            summarize(&NostrConnectMethod::SignEvent, &[not_an_event.to_string()]),
            "Sign event"
        );

        // The other methods answer in words too — the Debug spelling
        // ("getpublickey") is machine food, not a decision's headline.
        assert_eq!(summarize(&NostrConnectMethod::GetPublicKey, &[]), "Get public key");
        assert_eq!(
            summarize(&NostrConnectMethod::Nip44Decrypt, &["3f7a".to_string()]),
            "Decrypt message (NIP-44)"
        );
    }

    #[tokio::test]
    async fn the_prompt_carries_what_the_decision_needs() {
        let engine = engine();
        let app = app();
        engine.pair(&app);

        // A pending ask renders as the decision's material: the human
        // label, the kind and its name, the content whole, the
        // sensitivity cue — and no JSON wall where a detail should be.
        let params = vec![r#"{"kind":1,"content":"Hello, I'm signing remotely","tags":[]}"#.into()];
        let waiter = {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.decide(&app, &NostrConnectMethod::SignEvent, &params).await
            })
        };

        // The decide task and this test share one thread: yield until
        // the prompt has landed, which is the first poll of decide.
        let views = loop {
            let views = engine.prompts();
            if !views.is_empty() {
                break views;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(views[0].summary, "Sign a note");
        assert_eq!(views[0].kind, Some(1));
        assert_eq!(views[0].kind_label.as_deref(), Some("Note"));
        assert_eq!(views[0].content.as_deref(), Some("Hello, I'm signing remotely"));
        assert!(!views[0].sensitive, "a note is everyday's own kind");
        assert!(views[0].detail.is_none(), "the content carries it; no JSON wall beside it");

        // The answer lands, and the app's second line earns its keep:
        // the ask counted as a use.
        engine.approve(&views[0].id, None).unwrap();
        assert!(matches!(waiter.await.unwrap(), Decision::Allow));
        let paired = &engine.apps()[0];
        assert_eq!(paired.request_count, 1);
        assert!(paired.last_used_at.is_some());

        // A sensitive kind flags itself: a relay-list write is not an
        // everyday note.
        let params = vec![r#"{"kind":10002,"content":"","tags":[]}"#.into()];
        assert!(is_sensitive(&NostrConnectMethod::SignEvent, &params));
        let params = vec!["not an event".to_string()];
        assert!(
            is_sensitive(&NostrConnectMethod::SignEvent, &params),
            "the unreadable fails open into ask"
        );
    }

    #[test]
    fn rename_sets_the_persons_word() {
        let engine = engine();
        let app = app();
        engine.pair_with_metadata(&app, Some("the client's claim".into()), None, None, None);

        // The person's label sets — it does not fill-if-empty, because
        // the person's word outranks the client's own claim.
        assert!(engine.rename(&app.to_string(), "Damus on my phone"));
        assert_eq!(engine.apps()[0].name.as_deref(), Some("Damus on my phone"));

        let stranger = nostr::key::Keys::generate().public_key();
        assert!(!engine.rename(&stranger.to_string(), "x"), "renaming a stranger names nobody");
    }

    #[tokio::test]
    async fn a_retried_ask_joins_the_first_and_one_answer_serves_all() {
        let engine = engine();
        let app = app();
        engine.pair(&app);
        let params = vec![r#"{"kind":1,"content":"hello","tags":[]}"#.into()];

        // The same ask three times while nobody answers: one card, a
        // count of the waiters, not a pile of identical prompts.
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let engine = engine.clone();
                let params = params.clone();
                tokio::spawn(async move {
                    engine.decide(&app, &NostrConnectMethod::SignEvent, &params).await
                })
            })
            .collect();
        let views = loop {
            let views = engine.prompts();
            if views.len() == 1 && views[0].retries == 3 {
                break views;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(views.len(), 1, "identical asks share one card");
        assert_eq!(views[0].retries, 3);

        // One answer, every waiter: each response carries its own id,
        // the decision is shared.
        engine.approve(&views[0].id, None).unwrap();
        for waiter in waiters {
            assert!(matches!(waiter.await.unwrap(), Decision::Allow));
        }
        assert!(engine.prompts().is_empty());
    }

    #[test]
    fn the_activity_log_persists_and_stays_capped() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(Some(dir.path().to_path_buf()));
        let app = app();
        engine.pair(&app);
        for _ in 0..(LOG_CAP + 30) {
            engine.noted(&app.to_string(), "ping", "a probe".into(), "served");
        }
        assert_eq!(engine.log().len(), LOG_CAP, "the cap is the log's own");
        assert_eq!(engine.log().last().unwrap().summary, "a probe", "the newest survive");

        // A restart reads what the last one wrote.
        let engine = Engine::new(Some(dir.path().to_path_buf()));
        assert_eq!(engine.log().len(), LOG_CAP);
        assert_eq!(engine.log().last().unwrap().app, app.to_string());
    }
}
