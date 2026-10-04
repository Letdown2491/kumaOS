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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use nostr::prelude::*;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Bytes;
use tungstenite::Message;

/// The subscription id every relay thread uses. One subscription per
/// relay; the id is what the relay echoes back, so it only needs to be
/// distinguishable in a log.
pub(crate) const SUBSCRIPTION_ID: &str = "kuma-bunker";

/// The backoff ceiling: a relay that has been down for this long keeps
/// trying on the minute, because the bunker being reachable when the
/// relay comes back is the whole job.
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_START: Duration = Duration::from_secs(1);

/// How a road probes a quiet wire. The beat is one second (the read
/// timeout); every so many beats the road sends a ping, and so many
/// pings may go unanswered — no inbound frame of any kind between
/// them — before the road calls the connection dead and walks off
/// into the backoff that reconnects it.
#[derive(Clone, Copy)]
struct Timing {
    beats_per_ping: u32,
    max_unanswered: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self { beats_per_ping: 30, max_unanswered: 2 }
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
    own: bool,
    app: Option<PublicKey>,
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

/// The pool: owns the relay roads, hands bunker-bound events to the
/// `inbound` receiver, and publishes the bunker's answers down the
/// road each answer names — fanning out, because two roads may both
/// carry one answer.
pub struct RelayPool {
    /// Every road with its sender beside it: the fan-out's whole
    /// book.
    roads: Vec<(Road, Sender<(Event, Fan)>)>,
    own_urls: Vec<String>,
    /// The nostrconnect apps' relays, by the client pubkey that
    /// presented its URI.
    app_urls: HashMap<PublicKey, Vec<String>>,
}

impl RelayPool {
    /// Spawn one thread per relay. `inbound` receives every kind 24133
    /// event any relay hands over; `status` receives each relay's state
    /// changes, and starts empty — a relay that has said nothing yet
    /// has no status, which the surfaces render as "connecting".
    pub fn spawn(
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: Sender<Event>,
        status: Sender<RelayStatus>,
    ) -> Self {
        Self::spawn_with_timing(relays, bunker_pubkey, inbound, status, Timing::default())
    }

    /// `spawn` with the probe cadence spelled out — the tests' road to
    /// a fast fuse. Production callers take the default.
    fn spawn_with_timing(
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: Sender<Event>,
        status: Sender<RelayStatus>,
        timing: Timing,
    ) -> Self {
        let mut pool = Self { roads: Vec::new(), own_urls: Vec::new(), app_urls: HashMap::new() };
        pool.spawn_own(relays, bunker_pubkey, &inbound, &status, timing);
        pool
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_road(
        &mut self,
        url: String,
        own: bool,
        app: Option<PublicKey>,
        bunker_pubkey: PublicKey,
        inbound: &Sender<Event>,
        status: &Sender<RelayStatus>,
        timing: Timing,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let (outbound, outbound_rx) = channel::<(Event, Fan)>();
        let handle = std::thread::spawn({
            let stop = stop.clone();
            let inbound = inbound.clone();
            let status = status.clone();
            move || {
                one_relay(url, bunker_pubkey, inbound, status, outbound_rx, stop, timing);
            }
        });
        self.roads.push((Road { own, app, stop, handle }, outbound));
    }

    fn spawn_own(
        &mut self,
        relays: Vec<String>,
        bunker_pubkey: PublicKey,
        inbound: &Sender<Event>,
        status: &Sender<RelayStatus>,
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
        inbound: &Sender<Event>,
        status: &Sender<RelayStatus>,
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
    /// nobody else's to stop.
    pub fn drop_app(&mut self, app: &PublicKey) {
        self.app_urls.remove(app);
        self.roads.retain(|(road, _)| {
            let stays = road.app.as_ref() != Some(app);
            if !stays {
                road.stop.store(true, Ordering::SeqCst);
            }
            stays
        });
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
    /// down gets its copy on reconnect — the thread's first act after
    /// a subscribe is to drain its queue — so an answer is never lost
    /// to a relay that blinks while the bunker was thinking.
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
                outbound
                    .send((event.clone(), fan.clone()))
                    .map_err(|_| anyhow::anyhow!("the relay threads are gone"))?;
            }
        }
        Ok(())
    }

    /// Stop the threads and wait for them.
    pub fn shutdown(self) {
        for (road, _) in &self.roads {
            road.stop.store(true, Ordering::SeqCst);
        }
        for (road, _) in self.roads {
            let _ = road.handle.join();
        }
    }
}

/// One relay's whole life: connect, subscribe, serve, die, retry. Never
/// panics and never gives up until `stop` — a relay that comes back has
/// to find the bunker waiting.
fn one_relay(
    url: String,
    bunker_pubkey: PublicKey,
    inbound: Sender<Event>,
    status: Sender<RelayStatus>,
    outbound: Receiver<(Event, Fan)>,
    stop: Arc<AtomicBool>,
    timing: Timing,
) {
    let mut backoff = BACKOFF_START;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match connect_and_serve(&url, bunker_pubkey, &inbound, &status, &outbound, &stop, timing) {
            Ok(()) => return,
            Err(e) => {
                let _ =
                    status.send(RelayStatus { url: url.clone(), state: RelayState::Disconnected });
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                // The reason goes to stderr, where the unit's journal
                // holds it; the status channel is what surfaces read.
                eprintln!("kuma-nostrd: {url}: {e:#}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

fn connect_and_serve(
    url: &str,
    bunker_pubkey: PublicKey,
    inbound: &Sender<Event>,
    status: &Sender<RelayStatus>,
    outbound: &Receiver<(Event, Fan)>,
    stop: &AtomicBool,
    timing: Timing,
) -> Result<()> {
    let (mut socket, _response) =
        tungstenite::connect(url).with_context(|| format!("connecting to {url}"))?;
    set_read_timeout(&mut socket, Some(Duration::from_secs(1)))?;

    let filter = serde_json::json!({
        "kinds": [24133],
        "#p": [bunker_pubkey.to_string()],
    });
    socket.send(Message::text(serde_json::json!(["REQ", SUBSCRIPTION_ID, filter]).to_string()))?;
    let _ = status.send(RelayStatus { url: url.to_string(), state: RelayState::Connected });

    // The probe's books: the beat counter since the last ping, and the
    // pings sent with no inbound frame between them. Any frame at all
    // — pong, event, notice — is proof the wire lives, and zeroes the
    // unanswered count.
    let mut beats_since_ping: u32 = 0;
    let mut unanswered: u32 = 0;

    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }

        // Outbound first, so an answer that arrived while the read was
        // blocking goes out before anything else is read. The lock is
        // held for the drain of this beat, never across the read.
        // The road's own queue: everything in it is the road's to
        // carry — the fan-out decided that at send time.
        let mut drained = Vec::new();
        while let Ok((event, _fan)) = outbound.try_recv() {
            drained.push(event);
        }
        for event in drained {
            socket.send(Message::text(serde_json::json!(["EVENT", event]).to_string()))?;
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                unanswered = 0;
                eprintln!("kuma-nostrd: frame: {}", text.chars().take(120).collect::<String>());
                let events = events_from_relay_message(&text, SUBSCRIPTION_ID);
                if !events.is_empty() {
                    // One line per bunker-addressed event: when an app's
                    // ask never becomes a prompt, this is the line that
                    // says whether the relay ever spoke.
                    eprintln!("kuma-nostrd: relayed {} event(s) from the wire", events.len());
                }
                for event in events {
                    let _ = inbound.send(event);
                }
            }
            Ok(Message::Ping(payload)) => {
                unanswered = 0;
                socket.send(Message::Pong(payload))?;
            }
            Ok(Message::Pong(_)) => unanswered = 0,
            Ok(_) => unanswered = 0,
            Err(tungstenite::Error::Io(e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                // The read timeout: the loop's heartbeat. What the
                // beat is for: a connection that died in silence —
                // the state every socket is in after the machine
                // sleeps, where the relay's FIN was lost mid-suspend —
                // never errors on read. The kernel still calls it
                // ESTABLISHED; only our own probe can learn the truth.
                // So the beat pings, and pings with no answer at all
                // are the fuse: past the deadline, the road calls the
                // connection dead and the backoff outside reconnects
                // it.
                beats_since_ping += 1;
                if beats_since_ping >= timing.beats_per_ping {
                    beats_since_ping = 0;
                    unanswered += 1;
                    if unanswered > timing.max_unanswered {
                        return Err(anyhow!(
                            "the relay went quiet: {unanswered} pings with no answer"
                        ));
                    }
                    socket.send(Message::Ping(Bytes::new()))?;
                }
            }
            Err(tungstenite::Error::Protocol(
                tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
            )) => return Err(anyhow!("the relay reset the connection")),
            Err(e) => return Err(anyhow::anyhow!(e).context("reading from the relay")),
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

/// The read timeout is the loop's heartbeat: outbound frames go out and
/// the stop flag is honored on its beat. TLS streams wrap the TcpStream;
/// the plain arm is what every test and every ws:// relay uses.
fn set_read_timeout(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    timeout: Option<Duration>,
) -> Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(timeout).context("setting the relay read timeout")
        }
        MaybeTlsStream::Rustls(stream) => {
            stream.get_ref().set_read_timeout(timeout).context("setting the relay read timeout")
        }
        other => Err(anyhow!("unsupported relay transport: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::test_relay::{wait_for, StubRelay};
    use std::sync::atomic::AtomicUsize;

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
        let (inbound_tx, inbound_rx) = channel::<Event>();
        let (status_tx, status_rx) = channel::<RelayStatus>();
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

        let (inbound_tx, inbound_rx) = channel::<Event>();
        let (status_tx, _status_rx) = channel::<RelayStatus>();
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

        let (inbound_tx, _inbound_rx) = channel::<Event>();
        let (status_tx, status_rx) = channel::<RelayStatus>();
        let pool = RelayPool::spawn_with_timing(
            vec![url],
            bunker_pubkey,
            inbound_tx,
            status_tx,
            Timing { beats_per_ping: 1, max_unanswered: 2 },
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
