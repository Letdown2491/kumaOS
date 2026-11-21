//! The lean transport: one thread per relay, blocking WebSockets, and
//! the honest failure story the plan promised.
//!
//! What replaces nostr-sdk's pool is a few hundred lines, and this file
//! is most of them. Each relay gets a thread that connects, subscribes
//! once — requests addressed to the bunker pubkey, kind 24133 — and
//! then loops: read what arrives, drain what the bunker wants
//! published, and reconnect with a growing backoff when the relay
//! dies. The transport holds no secrets; the events it forwards go to
//! the bunker's brain, which is where every decision lives.
//!
//! The one scary failure — the bunker silently stops answering — is
//! watched elsewhere: the doctor's liveness probe, the bar widget's
//! offline state, and the smoke stage. What the transport owes them is
//! the truth about connection state, which is why every state change
//! is visible on the status channel rather than swallowed.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nostr::prelude::*;
use tungstenite::client::IntoClientRequest;
use tungstenite::Bytes;
use tungstenite::Message;

use crate::wake::{
    poll_readable, poll_wait, send_paced, wakeup_pair, RawSocket, WakeSink, WakeSource,
};

/// The subscription id every relay thread uses. One subscription per
/// relay; the id is what the relay echoes back, so it only needs to be
/// distinguishable in a log.
pub(crate) const SUBSCRIPTION_ID: &str = "kuma-bunker";

/// The backoff ceiling: a relay that has been down for this long keeps
/// trying on the minute, because the bunker being reachable when the
/// relay comes back is the whole job.
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_START: Duration = Duration::from_secs(1);

/// The paced send's budget — the stalled-peer fuse the blocking write
/// timeout used to arm: a peer that stops reading fills the kernel
/// send buffer, send_paced waits for write-readiness up to this long,
/// and then the road tears down into the same backoff a read death
/// takes.
const WRITE_PACE: Duration = Duration::from_secs(10);

/// The inbound frame cap, matching the relay's own MAX_FRAME_BYTES: a
/// bunker's honest traffic is kind-24133 wrappers, and tungstenite's
/// default (64 MiB) is an invitation a hostile relay should not be
/// handed.
const MAX_INBOUND_BYTES: usize = 1_048_576;

/// The frame log's sample period: one frame line per road per period,
/// suppressed frames counted and named when the sample resumes. The
/// wire's own rate, not this number, is what a flood's cost would
/// otherwise be.
const FRAME_SAMPLE: Duration = Duration::from_secs(10);

/// How a road probes a quiet wire. Every `ping_every`, the road sends
/// a ping, and so many pings may go unanswered — no inbound frame of
/// any kind between them — before the road calls the connection dead
/// and walks off into the backoff that reconnects it. The intervals
/// are durations, not beat counts: the loop sleeps in `poll(2)` until
/// the wire, the wakeup pipe, or the next ping deadline, so idle
/// costs one wake per ping interval, not one per second.
#[derive(Clone, Copy)]
struct Timing {
    ping_every: Duration,
    max_unanswered: u32,
}

impl Default for Timing {
    fn default() -> Self {
        // The fuse the one-second beat used to arm: a ping every
        // thirty seconds, two unanswered before the third is the walk
        // — ~90 seconds of silence and the road reconnects. The
        // guarantees did not move; only the wakes did (86,400 a day
        // per road down to 2,880, and the pipe makes every enqueue
        // instant where the beat made it wait a second).
        Self { ping_every: Duration::from_secs(30), max_unanswered: 2 }
    }
}

/// What a relay thread reports upward. Connection state is a fact the
/// surfaces render, not a log line to hope for.
#[derive(Debug, Clone, PartialEq)]
pub enum RelayState {
    Connected,
    Disconnected,
}

/// One relay's line in the status answer: its URL and where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayStatus {
    pub url: String,
    pub state: RelayState,
}

/// Which roads an outbound event takes. The bunker answers apps, and
/// an answer encrypted to one app does not belong on every relay's
/// wire: a relay operator learns which app talks to which bunker from
/// the traffic alone, and an answer aimed at another app is not that
/// relay's business.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fan {
    /// The declared set's roads — the bunker's answers on the relays
    /// the declaration chose.
    Own,
    /// One app's road: the declared set AND the app's own relays. A
    /// bunker:// app's road is the declared set alone; a
    /// nostrconnect app's adds the relays its URI named.
    App(PublicKey),
    /// Only one app's own relays — the nostrconnect handshake, whose
    /// road the NIP spells as the URI's relays and nothing else.
    OnlyApp(PublicKey),
}

/// One relay thread: its identity, its own stop flag, and its own
/// queue. An app's roads tear down individually on revocation — a
/// shared flag would stop every road to stop one — and a shared
/// queue would hand each event to one thread where two roads may
/// need it.
struct Road {
    url: String,
    own: bool,
    app: Option<PublicKey>,
    stop: Arc<AtomicBool>,
    /// Wakes the road's poll out of its sleep: after every enqueue
    /// (the frame leaves instantly, not on a beat) and every stop.
    wake: WakeSink,
    handle: std::thread::JoinHandle<()>,
}

/// The pool: owns the relay roads, hands bunker-bound events to the
/// `inbound` receiver, and publishes the bunker's answers down the
/// road each answer names — fanning out, because two roads may both
/// carry one answer.
/// The inbound channel's bound: the relay threads' shed point — the
/// try_send at the producer sheds at this many queued events. See
/// Daemon::new for why the bound sits on this side and not status's.
pub(crate) const INBOUND_CAP: usize = 1000;

/// The outbound and status channels' bound. The outbound queue holds
/// one clone of each unpublished answer per road: without a bound, a
/// road whose thread sits in backoff (holding the receiver) or stalls
/// mid-write grows it at the bunker's publish rate for as long as the
/// outage lasts — the daemon's one unbounded path, on a process that
/// runs for weeks. At the cap the producer sheds, exactly as the
/// inbound side sheds a backlog replay; a shed answer is a
/// requester's timeout, the same end an unbounded queue gave everyone
/// eventually, without the RSS. The status queue's drain keeps only
/// the newest state per URL, so a shed transition is the message the
/// drain would have overwritten anyway.
pub(crate) const OUTBOUND_CAP: usize = 1000;
pub(crate) const STATUS_CAP: usize = 100;

pub struct RelayPool {
    /// Every road with its sender beside it: the fan-out's whole
    /// book.
    roads: Vec<(Road, SyncSender<(Event, Fan)>)>,
    own_urls: Vec<String>,
    /// The nostrconnect apps' relays, by the client pubkey that
    /// presented its URI.
    app_urls: HashMap<PublicKey, Vec<String>>,
    /// Events the roads SHED at the inbound bound since the pool's
    /// birth, across every road and every reconnect (#82's fix shape
    /// 1): the shed was a silent `let _` — a dropped sign request's
    /// only symptom was a client timeout, the journal said nothing,
    /// and nothing downstream could see the drop at all. The counter
    /// is the fact; what grades it rides the status document.
    shed: Arc<AtomicU64>,
}

impl RelayPool {
    /// Spawn one thread per relay. `inbound` receives every kind 24133
    /// event any relay hands over; `status` receives each relay's state
    /// changes, and starts empty — a relay that has said nothing yet
    /// has no status, which the surfaces render as "connecting".
    pub fn spawn(
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: SyncSender<Event>,
        status: SyncSender<RelayStatus>,
    ) -> Self {
        Self::spawn_with_timing(relays, bunker_pubkey, inbound, status, Timing::default())
    }

    /// `spawn` with the probe cadence spelled out — the tests' road to
    /// a fast fuse. Production callers take the default.
    fn spawn_with_timing(
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: SyncSender<Event>,
        status: SyncSender<RelayStatus>,
        timing: Timing,
    ) -> Self {
        let mut pool = Self {
            roads: Vec::new(),
            own_urls: Vec::new(),
            app_urls: HashMap::new(),
            shed: Arc::new(AtomicU64::new(0)),
        };
        pool.spawn_own(relays, bunker_pubkey, &inbound, &status, timing);
        pool
    }

    /// Events shed at the inbound bound since the pool's birth —
    /// across every road and every reconnect. Zero on a healthy
    /// machine; anything else is a sign request's timeout explained.
    pub fn shed(&self) -> u64 {
        self.shed.load(Ordering::Relaxed)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_road(
        &mut self,
        url: String,
        own: bool,
        app: Option<PublicKey>,
        bunker_pubkey: PublicKey,
        inbound: &SyncSender<Event>,
        status: &SyncSender<RelayStatus>,
        timing: Timing,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let (outbound, outbound_rx) = sync_channel::<(Event, Fan)>(OUTBOUND_CAP);
        let (wake_read, wake_write) =
            wakeup_pair().expect("a wakeup pipe is two fds, this cannot fail sanely");
        let handle = std::thread::spawn({
            let stop = stop.clone();
            let inbound = inbound.clone();
            let status = status.clone();
            let shed = self.shed.clone();
            let thread_url = url.clone();
            move || {
                one_relay(RelayWiring {
                    url: thread_url,
                    bunker_pubkey,
                    inbound,
                    status,
                    outbound: outbound_rx,
                    stop,
                    wake: wake_read,
                    timing,
                    shed,
                });
            }
        });
        // The road keeps the url beside the thread: what drop_app
        // reads to tell a URL nobody serves anymore from one the
        // declared set still runs. The wake sink rides with it — the
        // producers (the fan-out's send, the stop flags) need to reach
        // a loop that is asleep in poll, not spinning.
        self.roads.push((Road { url, own, app, stop, wake: wake_write, handle }, outbound));
    }

    fn spawn_own(
        &mut self,
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: &SyncSender<Event>,
        status: &SyncSender<RelayStatus>,
        timing: Timing,
    ) {
        for url in relays {
            if self.own_urls.contains(&url) {
                continue;
            }
            self.own_urls.push(url.clone());
            self.spawn_road(url, true, None, bunker_pubkey, inbound, status, timing);
        }
    }

    /// A nostrconnect app's own relays, as roads: the same
    /// subscription, the app's road in. A URL the declared set
    /// already runs is skipped — one road, one thread — and so is one
    /// the app itself already has.
    pub fn subscribe_relays(
        &mut self,
        app: &PublicKey,
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: &SyncSender<Event>,
        status: &SyncSender<RelayStatus>,
    ) {
        for url in relays {
            // The dedup reads the books before anything is borrowed
            // for the spawn.
            let known = self.own_urls.contains(&url)
                || self.app_urls.get(app).is_some_and(|urls| urls.contains(&url));
            if known {
                continue;
            }
            self.app_urls.entry(*app).or_default().push(url.clone());
            self.spawn_road(
                url,
                false,
                Some(*app),
                bunker_pubkey,
                inbound,
                status,
                Timing::default(),
            );
        }
    }

    /// Tear one app's roads down: the revocation's own half. The
    /// threads exit on their next beat; the declared set's roads are
    /// nobody else's to stop. Returns the URLs the dropped roads
    /// served that no remaining road serves — the caller's state map
    /// names them, and a stale Connected there is a lie.
    pub fn drop_app(&mut self, app: &PublicKey) -> Vec<String> {
        let gone = self.app_urls.remove(app).unwrap_or_default();
        self.roads.retain(|(road, _)| {
            let stays = road.app.as_ref() != Some(app);
            if !stays {
                road.stop.store(true, Ordering::SeqCst);
                // The thread is asleep in poll, not spinning: stop has
                // to ring the bell or the teardown waits for the next
                // ping deadline.
                road.wake.wake();
            }
            stays
        });
        gone.into_iter()
            .filter(|url| !self.roads.iter().any(|(road, _)| road.url == *url))
            .collect()
    }

    /// Whether an app's roads are still threaded — what the teardown
    /// test reads.
    #[cfg(test)]
    pub(crate) fn app_road_count(&self, app: &PublicKey) -> usize {
        self.roads.iter().filter(|(road, _)| road.app.as_ref() == Some(app)).count()
    }

    /// Publish an event down the declared set's roads.
    pub fn publish(&self, event: &Event) -> Result<()> {
        self.send(event, Fan::Own)
    }

    /// Publish an answer down one app's road: the declared set, plus
    /// the app's own relays when it has any.
    pub fn publish_for(&self, event: &Event, app: &PublicKey) -> Result<()> {
        self.send(event, Fan::App(*app))
    }

    /// Publish the handshake down the client's relays and nothing
    /// else's — the NIP spells that road as the URI's. An app whose
    /// URI named the declared set has no roads of its own: the
    /// declared set IS its road, and the fan becomes the plain one.
    pub fn publish_only_to(&self, event: &Event, app: &PublicKey) -> Result<()> {
        let has_own_road = self.app_urls.get(app).is_some_and(|urls| !urls.is_empty());
        self.send(event, if has_own_road { Fan::OnlyApp(*app) } else { Fan::Own })
    }

    /// The fan-out: one send per road the fan names. A road that is
    /// down keeps what it was handed up to the channel's bound — the
    /// thread's first act after a subscribe is to drain its queue, so
    /// a relay that blinks still gets every answer — but the bound is
    /// where a road that stays down stops mattering: the shed at the
    /// producer (the same rule the inbound side runs) is what keeps a
    /// dead road from growing the daemon's RSS at the publish rate.
    fn send(&self, event: &Event, fan: Fan) -> Result<()> {
        if self.roads.is_empty() {
            return Err(anyhow::anyhow!("the relay threads are gone"));
        }
        for (road, outbound) in &self.roads {
            let mine = match &fan {
                Fan::Own => road.own,
                // An app's road is the declared set plus its own
                // relays, so the declared threads carry it too.
                Fan::App(who) => road.own || road.app.as_ref() == Some(who),
                Fan::OnlyApp(who) => road.app.as_ref() == Some(who),
            };
            if mine {
                match outbound.try_send((event.clone(), fan.clone())) {
                    Ok(()) => {
                        // The wake is what makes this send instant:
                        // the road sleeps in poll, and the frame it
                        // just queued leaves when the bell rings, not
                        // on the next beat of a one-second spin.
                        road.wake.wake();
                    }
                    // Full is the shed: the road's queue is at its
                    // bound, the answer's requester times out, and
                    // nothing grows.
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        return Err(anyhow::anyhow!("the relay threads are gone"));
                    }
                }
            }
        }
        Ok(())
    }

    /// Stop the threads and wait for them.
    pub fn shutdown(self) {
        for (road, _) in &self.roads {
            road.stop.store(true, Ordering::SeqCst);
            road.wake.wake();
        }
        for (road, _) in self.roads {
            let _ = road.handle.join();
        }
    }
}

/// The wiring one relay thread serves for its whole life, and what
/// `connect_and_serve` borrows per attempt. One struct, not eight
/// parameters: the arguments really do travel as one bundle, and the
/// arity gate is the referee that said so.
struct RelayWiring {
    url: String,
    bunker_pubkey: PublicKey,
    inbound: SyncSender<Event>,
    status: SyncSender<RelayStatus>,
    outbound: Receiver<(Event, Fan)>,
    stop: Arc<AtomicBool>,
    /// The wakeup pipe's read end: what makes an enqueue instant and
    /// a stop honored while the loop sleeps in poll instead of
    /// spinning on a one-second read timeout.
    wake: WakeSource,
    timing: Timing,
    shed: Arc<AtomicU64>,
}

/// One relay's whole life: connect, subscribe, serve, die, retry. Never
/// panics and never gives up until `stop` — a relay that comes back has
/// to find the bunker waiting.
fn one_relay(w: RelayWiring) {
    let mut backoff = BACKOFF_START;
    loop {
        if w.stop.load(Ordering::SeqCst) {
            return;
        }
        let connected = std::time::Instant::now();
        match connect_and_serve(&w) {
            Ok(()) => return,
            Err(e) => {
                let _ = w
                    .status
                    .try_send(RelayStatus { url: w.url.clone(), state: RelayState::Disconnected });
                if w.stop.load(Ordering::SeqCst) {
                    return;
                }
                // The reason goes to stderr, where the unit's journal
                // holds it; the status channel is what surfaces read.
                eprintln!("kuma-nostrd: {}: {e:#}", w.url);
                // A connection that held longer than the ceiling's own
                // reach was a healthy road, not the retry of a dying
                // one: its next reconnect goes from the floor again.
                // Without the reset, one flap to the max costs every
                // later recovery a full minute, forever.
                std::thread::sleep(backoff);
                if connected.elapsed() > BACKOFF_MAX {
                    backoff = BACKOFF_START;
                } else {
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    }
}

fn connect_and_serve(w: &RelayWiring) -> Result<()> {
    let url = &w.url;
    // The frame cap the relay's own listener already enforces: a
    // bunker's honest traffic is kind-24133 wrappers, and tungstenite's
    // 64 MiB default is, in the tree's own relay-side words, an
    // invitation. The client reads from relays it chose, but a hostile
    // relay is still a relay — the cap is one MiB here too.
    let mut config = tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(MAX_INBOUND_BYTES);
    config.max_frame_size = Some(MAX_INBOUND_BYTES);
    let (mut socket, _response) = tungstenite::client::connect_with_config(
        url.as_str().into_client_request().context("the relay URL is not a request")?,
        Some(config),
        3,
    )
    .with_context(|| format!("connecting to {url}"))?;
    // The poll-based loop needs a nonblocking socket: the thread
    // sleeps in poll(2) and reads only when the wire is readable, so
    // a read never waits and an idle road wakes only at the ping
    // deadline (the one-second read-timeout spin woke it 86,400 times
    // a day to learn nothing). Sends pace through send_paced, the
    // blocking write's ten-second budget spelled for a nonblocking
    // socket.
    socket.get_ref().set_raw_nonblocking(true).context("setting the relay socket nonblocking")?;
    let Some(sock_fd) = socket.get_ref().raw_fd() else {
        return Err(anyhow!("unsupported relay transport"));
    };

    let filter = serde_json::json!({
        "kinds": [24133],
        "#p": [w.bunker_pubkey.to_string()],
    });
    socket.send(Message::text(serde_json::json!(["REQ", SUBSCRIPTION_ID, filter]).to_string()))?;
    let _ = w.status.try_send(RelayStatus { url: url.to_string(), state: RelayState::Connected });

    // The probe's books: when the last ping went out, and the pings
    // sent with no inbound frame between them. Any frame at all
    // — pong, event, notice — is proof the wire lives, and zeroes the
    // unanswered count. The fuse's guarantees are the one-second
    // beat's, spelled in durations now that the loop sleeps in poll
    // instead of counting heartbeats.
    let mut last_ping = std::time::Instant::now();
    let mut unanswered: u32 = 0;
    // The frame log's sampler: one line per sample period, with the
    // suppressed count named when the sample resumes. Per-frame was a
    // journald flood in waiting — a relay redelivering its backlog or
    // a peer spraying garbage wrote at the wire's rate, uncapped.
    let mut last_frame =
        std::time::Instant::now().checked_sub(FRAME_SAMPLE).unwrap_or_else(std::time::Instant::now);
    let mut suppressed: u64 = 0;

    // The poll set: the wire and the wakeup pipe. Enqueues and stops
    // ring the pipe; the ping deadline bounds every sleep; the socket
    // wakes it the moment a frame arrives.
    let mut fds = [
        libc::pollfd { fd: sock_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: w.wake.fd(), events: libc::POLLIN, revents: 0 },
    ];

    loop {
        if w.stop.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }

        // Outbound first, so an answer that arrived while the loop
        // slept goes out before anything else is read. Queue, then
        // pipe, then queue once more — every queued message precedes
        // its byte, so this order can strand neither.
        let mut drain_once = || -> Result<()> {
            let mut drained = Vec::new();
            while let Ok((event, _fan)) = w.outbound.try_recv() {
                drained.push(event);
            }
            for event in drained {
                send_paced(
                    &mut socket,
                    Message::text(serde_json::json!(["EVENT", event]).to_string()),
                    WRITE_PACE,
                )
                .map_err(|e| anyhow::anyhow!(e).context("publishing to the relay"))?;
            }
            Ok(())
        };
        drain_once()?;
        w.wake.drain();
        drain_once()?;

        // The ping fuse: a ping per interval, and past the allowance
        // of unanswered pings the road calls the connection dead and
        // the backoff outside reconnects it. What the fuse is for: a
        // connection that died in silence — the state every socket is
        // in after the machine sleeps, where the relay's FIN was lost
        // mid-suspend — never errors on read. The kernel still calls
        // it ESTABLISHED; only our own probe can learn the truth.
        if last_ping.elapsed() >= w.timing.ping_every {
            last_ping = std::time::Instant::now();
            unanswered += 1;
            if unanswered > w.timing.max_unanswered {
                return Err(anyhow!("the relay went quiet: {unanswered} pings with no answer"));
            }
            send_paced(&mut socket, Message::Ping(Bytes::new()), WRITE_PACE)
                .map_err(|e| anyhow::anyhow!(e).context("pinging the relay"))?;
        }

        // Sleep until the wire, the pipe, or the next ping deadline.
        // Any inbound frame between pings resets the fuse; this is
        // also why the read below drains until WouldBlock — a TLS
        // stream may hold already-decrypted frames its kernel socket
        // no longer reports readable, and leaving one behind would
        // stall it until the next ping deadline.
        let budget =
            w.timing.ping_every.saturating_sub(last_ping.elapsed()).as_millis().clamp(1, 60_000)
                as i32;
        poll_wait(&mut fds, budget)?;
        if poll_readable(&fds[1]) {
            // The bell: enqueue or stop. The top of the loop drains
            // both and re-checks stop.
            continue;
        }
        if !poll_readable(&fds[0]) {
            // Spurious wake or the ping budget elapsed; the fuse
            // check at the top handles the deadline.
            continue;
        }
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    unanswered = 0;
                    let events = events_from_relay_message(&text, SUBSCRIPTION_ID);
                    if last_frame.elapsed() >= FRAME_SAMPLE {
                        if suppressed > 0 {
                            eprintln!(
                                "kuma-nostrd: {url}: {suppressed} frame(s) arrived between samples"
                            );
                            suppressed = 0;
                        }
                        eprintln!(
                            "kuma-nostrd: frame: {}",
                            text.chars().take(120).collect::<String>()
                        );
                        if !events.is_empty() {
                            // One line per bunker-addressed event batch:
                            // when an app's ask never becomes a prompt,
                            // this is the line that says whether the relay
                            // ever spoke.
                            eprintln!(
                                "kuma-nostrd: relayed {} event(s) from the wire",
                                events.len()
                            );
                        }
                        last_frame = std::time::Instant::now();
                    } else {
                        suppressed += 1;
                    }
                    for event in events {
                        // The shed, not the block: a relay replaying a
                        // backlog can read frames off the wire faster than
                        // the worker plans them, and the channel's bound
                        // is where the flood stops. A shed event is a
                        // requester's timeout — the same end a jammed
                        // queue would give it, without the RSS. The shed
                        // is counted now, so the silence has a number
                        // (#82's fix shape 1).
                        if w.inbound.try_send(event).is_err() {
                            w.shed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Ok(Message::Ping(payload)) => {
                    unanswered = 0;
                    send_paced(&mut socket, Message::Pong(payload), WRITE_PACE)
                        .map_err(|e| anyhow::anyhow!(e).context("answering the relay's ping"))?;
                }
                Ok(_) => unanswered = 0,
                Err(tungstenite::Error::Io(e))
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                {
                    // The wire is drained: back to poll.
                    break;
                }
                Err(tungstenite::Error::Protocol(
                    tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                )) => return Err(anyhow!("the relay reset the connection")),
                Err(e) => return Err(anyhow::anyhow!(e).context("reading from the relay")),
            }
        }
    }
}

/// Parse a relay's `["EVENT", <sub>, <event>]` frames into events. The
/// subscription id is checked because a relay that ignores the filter
/// must not hand the bunker another app's traffic; a frame that parses
/// to nothing is skipped — relays are noisy, and the bunker's own
/// filter is the second line of defense, not the first.
fn events_from_relay_message(text: &str, expected_subscription: &str) -> Vec<Event> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let array = value.as_array().unwrap_or(&Vec::new()).clone();
    let kind_word = array.first().and_then(|v| v.as_str());
    if kind_word != Some("EVENT") {
        return Vec::new();
    }
    let subscription = array.get(1).and_then(|v| v.as_str());
    if subscription != Some(expected_subscription) {
        return Vec::new();
    }
    match array.get(2) {
        Some(event) => match serde_json::from_value::<Event>(event.clone()) {
            Ok(event) => vec![event],
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::test_relay::{wait_for, StubRelay};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn a_flood_beyond_the_bound_is_counted_not_silent() {
        let signer = Keys::generate();
        let app = Keys::generate();
        let bunker_pubkey = signer.public_key();

        // A bound of one, never drained: the first event fills it and
        // every event after sheds. The channel's cap, not the pool's
        // INBOUND_CAP constant, is what the test squeezes — the cap is
        // the daemon's choice, the counting is the pool's behavior at
        // any bound (#82's fix shape 1).
        let (inbound_tx, _inbound_rx) = sync_channel::<Event>(1);
        let (status_tx, _status_rx) = sync_channel::<RelayStatus>(STATUS_CAP);
        let flood: Vec<Event> = (0..8)
            .map(|i| {
                EventBuilder::new(Kind::NostrConnect, format!("flood {i}"))
                    .tag(Tag::public_key(bunker_pubkey))
                    .finalize(&app)
                    .unwrap()
            })
            .collect();
        let stub = StubRelay::start(flood);
        let pool = RelayPool::spawn(vec![stub.url.clone()], bunker_pubkey, inbound_tx, status_tx);

        // Seven of the eight have nowhere to land: the receiver holds
        // its one slot forever. The counter is what turns that from a
        // requester's unexplained timeout into a fact.
        wait_for("the shed to count the flood", 100, || pool.shed() >= 7);
    }

    #[test]
    fn drop_app_returns_the_urls_nobody_serves_anymore() {
        let signer = Keys::generate();
        let app = Keys::generate();
        let bunker_pubkey = signer.public_key();
        let own = StubRelay::start(vec![]);
        let app_relay = StubRelay::start(vec![]);
        let (inbound_tx, _inbound_rx) = sync_channel::<Event>(INBOUND_CAP);
        let (status_tx, _status_rx) = sync_channel::<RelayStatus>(STATUS_CAP);
        let mut pool = RelayPool::spawn(
            vec![own.url.clone()],
            bunker_pubkey,
            inbound_tx.clone(),
            status_tx.clone(),
        );
        pool.subscribe_relays(
            &app.public_key(),
            vec![app_relay.url.clone()],
            bunker_pubkey,
            &inbound_tx,
            &status_tx,
        );
        assert_eq!(pool.app_road_count(&app.public_key()), 1, "the app's road threaded");
        let gone = pool.drop_app(&app.public_key());
        assert_eq!(pool.app_road_count(&app.public_key()), 0);
        // The app's relay is served by nobody now — it is the answer —
        // and the declared set's road keeps it out of that answer.
        assert_eq!(gone, vec![app_relay.url.clone()]);
        assert!(pool.drop_app(&app.public_key()).is_empty(), "a second drop names nothing");
    }

    #[test]
    fn a_request_event_flows_relay_to_pool_and_an_answer_flows_back() {
        let signer = Keys::generate();
        let app = Keys::generate();
        let bunker_pubkey = signer.public_key();

        // The app's request, encrypted and signed exactly as the bunker
        // tests build one — here it travels over a real socket instead.
        let message = NostrConnectMessage::request(
            &NostrConnectRequest::from_message(NostrConnectMethod::Ping, vec![]).unwrap(),
        );
        let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app)
            .unwrap();

        let stub = StubRelay::start(vec![request.clone()]);
        let (inbound_tx, inbound_rx) = sync_channel::<Event>(INBOUND_CAP);
        let (status_tx, status_rx) = sync_channel::<RelayStatus>(STATUS_CAP);
        let pool = RelayPool::spawn(vec![stub.url.clone()], bunker_pubkey, inbound_tx, status_tx);

        // The request arrives on the inbound channel; receiving waits
        // for it rather than polling, because a poll would eat the
        // event the assertion below wants.
        let delivered = inbound_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(delivered.id, request.id);
        assert_eq!(delivered.kind, Kind::NostrConnect);

        // The connection state was reported upward.
        wait_for("the connected status", 100, || {
            status_rx.try_iter().any(|s| s.state == RelayState::Connected)
        });

        // The bunker's answer goes out through the pool and lands in
        // the stub's received pile as an EVENT frame.
        let answer = EventBuilder::new(Kind::from_u16(24133), "answer-content")
            .tag(Tag::public_key(app.public_key()))
            .finalize(&signer)
            .unwrap();
        pool.publish(&answer).unwrap();
        wait_for("the answer to reach the relay", 100, || {
            stub.received().iter().any(|frame| frame.contains(&answer.id.to_string()))
        });
        let published = stub
            .received()
            .into_iter()
            .find(|frame| frame.contains("\"EVENT\""))
            .expect("the answer was published as an EVENT frame");

        // The frame shape is the publish protocol's own: an array whose
        // second member is the event.
        let parsed: serde_json::Value = serde_json::from_str(&published).unwrap();
        assert_eq!(parsed[0], "EVENT");
        assert_eq!(parsed[1]["id"], answer.id.to_string());
        pool.shutdown();
    }

    #[test]
    fn frames_from_another_subscription_are_not_forwarded() {
        let signer = Keys::generate();
        let app = Keys::generate();
        let bunker_pubkey = signer.public_key();

        // The same request event, but delivered by a stub whose
        // subscription id lies: the pool filters it the way it must
        // filter a relay that ignored the filter.
        let message = NostrConnectMessage::request(
            &NostrConnectRequest::from_message(NostrConnectMethod::Ping, vec![]).unwrap(),
        );
        let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app)
            .unwrap();

        // The stub announces the event under a foreign subscription id.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let foreign = serde_json::json!(["EVENT", "someone-else", request]).to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            loop {
                match socket.read().unwrap() {
                    Message::Text(_) => break,
                    _ => continue,
                }
            }
            socket.send(Message::text(foreign)).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        });

        let (inbound_tx, inbound_rx) = sync_channel::<Event>(INBOUND_CAP);
        let (status_tx, _status_rx) = sync_channel::<RelayStatus>(STATUS_CAP);
        let pool = RelayPool::spawn(vec![url], bunker_pubkey, inbound_tx, status_tx);
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            inbound_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "a foreign subscription's event must not reach the bunker"
        );
        pool.shutdown();
    }

    // The parser's own table, for the frames a live relay actually
    // sends: ours, foreign, malformed, and the NOTICE chatter that is
    // not an event at all.
    #[test]
    fn the_frame_parser_distinguishes_the_four_kinds_of_chatter() {
        let event = EventBuilder::new(Kind::TextNote, "x").finalize(&Keys::generate()).unwrap();
        let ours = serde_json::json!(["EVENT", SUBSCRIPTION_ID, event]).to_string();
        assert_eq!(events_from_relay_message(&ours, SUBSCRIPTION_ID).len(), 1);

        let foreign = serde_json::json!(["EVENT", "other", event]).to_string();
        assert!(events_from_relay_message(&foreign, SUBSCRIPTION_ID).is_empty());

        assert!(events_from_relay_message("not json", SUBSCRIPTION_ID).is_empty());
        assert!(events_from_relay_message(r#"["NOTICE","hi"]"#, SUBSCRIPTION_ID).is_empty());
        assert!(events_from_relay_message(r#"["EVENT"]"#, SUBSCRIPTION_ID).is_empty());
    }

    // The post-hibernate road: the relay's side died while the machine
    // slept, and nothing will ever arrive on the socket again — the
    // FIN was lost mid-sleep, no ping is coming, no close. The kernel
    // still calls the connection ESTABLISHED, so only the road's own
    // probe can learn the truth. The regression the sleep bug wrote:
    // a stub that answers the subscribe and then goes silent forever
    // must be walked off — the road reconnects, and the second
    // connection at the listener is the proof.
    #[test]
    fn a_road_whose_relay_went_silent_reconnects() {
        let signer = Keys::generate();
        let bunker_pubkey = signer.public_key();

        // The stub accepts, shakes hands, reads the subscribe, and
        // then plays dead: no frames, no reads, no close — the
        // connection is a black hole, exactly as a relay left behind
        // by a suspend is. The listener keeps accepting: a second
        // acceptance is the observable a reconnect has.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let connections = Arc::new(AtomicUsize::new(0));
        let connections_for_thread = connections.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                connections_for_thread.fetch_add(1, Ordering::SeqCst);
                let Ok(mut socket) = tungstenite::accept(stream) else { return };
                loop {
                    match socket.read() {
                        Ok(Message::Text(_)) => break,
                        Ok(_) => continue,
                        Err(_) => return,
                    }
                }
                // Never closed, never answered again: dropping would
                // send a FIN, and a FIN is the one honesty the dead
                // road has.
                std::mem::forget(socket);
            }
        });

        let (inbound_tx, _inbound_rx) = sync_channel::<Event>(INBOUND_CAP);
        let (status_tx, status_rx) = sync_channel::<RelayStatus>(STATUS_CAP);
        let pool = RelayPool::spawn_with_timing(
            vec![url],
            bunker_pubkey,
            inbound_tx,
            status_tx,
            Timing { ping_every: Duration::from_millis(300), max_unanswered: 2 },
        );

        // The first connection lands — the road came up — and after
        // the silence the road must walk off the dead socket: a
        // second connection, inside the probe window.
        wait_for("the first connection", 100, || connections.load(Ordering::SeqCst) >= 1);
        wait_for("the road to walk off the silent socket", 200, || {
            connections.load(Ordering::SeqCst) >= 2
        });
        let _ = status_rx;
        pool.shutdown();
    }
}
