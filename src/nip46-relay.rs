//! `nip46-relay` — the NIP-46 relay, ported to Rust.
//!
//! A small relay for signing traffic and nothing else: kinds 24133 and
//! 24135, in memory only, evicted after ten minutes, rate-limited per
//! connection. It binds loopback by default — the way kuma's bunker
//! reaches it — and a host that wants it wider than loopback passes
//! `--bind` deliberately, because plaintext-wider-than-loopback is a
//! decision about metadata and not a default. What the relay never
//! does: touch disk, decrypt anything, or hold a key. MIT, a binary
//! anyone can host — that sentence is true from this repo.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use nostr::event::Event;
use tungstenite::Message;

use kuma::relay::{EventStore, RateLimiter, Rejected, RelayFilter};
use kuma::wake::{poll_wait, send_paced, wakeup_pair, RawSocket, WakeSource};

/// The shared server state: each connection's outbound queue and its
/// subscriptions, found by connection id. The queue's tuple carries
/// the wakeup sink — the producer's bell for a connection asleep in
/// poll — beside the kick flag the broadcaster pulls.
type Subscriptions = Arc<Mutex<HashMap<u64, Vec<(String, RelayFilter)>>>>;
type Outbound =
    Arc<Mutex<HashMap<u64, (SyncSender<Message>, Arc<AtomicBool>, kuma::wake::WakeSink)>>>;

/// How often a connection's loop checked for work when it ran on a
/// one-second read timeout. The loops sleep in `poll(2)` now — an
/// enqueue rings the connection's wakeup pipe and the frame leaves
/// instantly; idle costs no wake at all. The constant survives as the
/// paced send's budget: the stalled-reader fuse the blocking write
/// timeout used to arm, since a kicked-full queue and a dead reader
/// are the same discovery either way.
const BEAT: Duration = Duration::from_secs(1);

/// How many TCP connections the relay serves at once. Each is an OS
/// thread for as long as it lives; the cap is what keeps a dial loop
/// from turning the relay into a thread farm. Generous against any
/// real bunker-plus-apps population, tight against a dial loop.
const MAX_CONNECTIONS: usize = 512;

/// Subscriptions one connection may hold at a time. Each is a
/// broadcast fan-out multiplier; a client registering thousands of
/// distinct ids would turn every broadcast into thousands of queue
/// writes.
const MAX_SUBSCRIPTIONS: usize = 32;

/// Bytes one wire frame may carry. A NIP-46 frame is a signed event —
/// a few KiB at the edge — and the library's 64 MiB default is a
/// memory-exhaustion invitation on a socket anyone can dial.
const MAX_FRAME_BYTES: usize = 1_048_576;

/// Seconds the TCP handshake (the HTTP upgrade read) may take before
/// the connection is refused. One connection's beat replaces it once
/// the upgrade is done.
const HANDSHAKE_SECS: u64 = 10;

/// How deep a connection's outbound queue may get before the relay
/// decides the reader stopped reading. Real clients queue single
/// digits; the cap is generosity itself. A queue that will not take a
/// frame means a peer holding the socket open and reading nothing —
/// without a cap, that peer's queue grows at the publish rate for as
/// long as it holds on.
const QUEUE_CAP: usize = 1_000;

#[derive(Parser)]
#[command(
    name = "nip46-relay",
    about = "A NIP-46 relay: signing traffic only, in memory, ten minutes",
    version,
    verbatim_doc_comment
)]
struct Args {
    /// The address to bind. The default is loopback, which is what the
    /// bunker behind it expects; widening it is a metadata decision.
    #[arg(long, default_value = "127.0.0.1:7777")]
    bind: String,
    /// How many minutes an event stays in memory. The Go original's
    /// default and this port's agree: ten.
    #[arg(long, default_value_t = kuma::relay::EVENT_TTL_MINUTES)]
    keep_minutes: u64,
    /// How many minutes each way an event's created_at may sit from
    /// now. The Go original's default and this port's agree: one.
    #[arg(long, default_value_t = kuma::relay::ACCEPT_WINDOW_MINUTES)]
    accept_window_minutes: u64,
    /// Events per pubkey per minute. The Go original's default and
    /// this port's agree: a hundred.
    #[arg(long, default_value_t = kuma::relay::RATE_LIMIT_PER_MINUTE)]
    rate_limit: usize,
}

/// How long a pubkey's budget outlives its pubkey's last event. A
/// limiter idle this long holds an expired window either way — the
/// sliding minute pops the same entries on the next admit — so
/// removing it is invisible to an honest client, and it is what
/// reclaims a flood's throwaway keys, one per event, whose otherwise
/// immortal entries would outlive the flood by forever.
const LIMITER_IDLE_SECS: u64 = 600;

/// The limiter map's hard ceiling, the bunker's shed-oldest rule
/// copied: even a flood outpacing the idle sweep cannot grow the map
/// past this. Twenty thousand keys is the honest-client count times
/// nobody's margin — the map's whole job is one entry per bunkered
/// pubkey.
const LIMITER_CAP: usize = 20_000;

/// Shed the idle keys, then — if a flood outpaced the sweeper's beat —
/// the oldest, down to nine tenths of the cap so the next few inserts
/// do not re-sweep per event. Cost is a sort of the map, paid only
/// when the cap has actually been reached.
fn sweep_limiters(limiters: &mut HashMap<nostr::key::PublicKey, RateLimiter>, now: u64) {
    let floor = now.saturating_sub(LIMITER_IDLE_SECS);
    limiters.retain(|_, lim| lim.last_seen().is_some_and(|at| at >= floor));
    if limiters.len() < LIMITER_CAP {
        return;
    }
    let target = LIMITER_CAP * 9 / 10;
    let mut by_age: Vec<(nostr::key::PublicKey, u64)> =
        limiters.iter().map(|(key, lim)| (*key, lim.last_seen().unwrap_or(0))).collect();
    by_age.sort_unstable_by_key(|&(_, at)| at);
    let shed = limiters.len().saturating_sub(target);
    for (key, _) in by_age.into_iter().take(shed) {
        limiters.remove(&key);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let listener =
        TcpListener::bind(&args.bind).with_context(|| format!("binding {}", args.bind))?;
    eprintln!("nip46-relay: carrying kinds 24133 and 24135 on {}", args.bind);

    let store = Arc::new(Mutex::new(EventStore::new(kuma::relay::RelayConfig {
        ttl_secs: args.keep_minutes * 60,
        accept_window_secs: args.accept_window_minutes * 60,
        rate_limit: args.rate_limit,
    })));
    // Each connection's outbound queue and subscription set, found by
    // its id; the counter hands the ids out.
    let outbound: Outbound = Arc::new(Mutex::new(HashMap::new()));
    let subscriptions: Subscriptions = Arc::new(Mutex::new(HashMap::new()));
    let next_id = AtomicU64::new(1);
    // The budget is the pubkey's, not the connection's: one map for the
    // relay's whole life, so a reconnect meets the budget it left and
    // cannot refresh a flood by walking back in. The capacity is the
    // configured rate, the same number the Go original takes from its
    // env.
    let limiters: Arc<Mutex<HashMap<nostr::key::PublicKey, RateLimiter>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // The evictor is the store's own heartbeat, not any connection's:
    // the Go original's sweepLoop runs on a quarter of the TTL, never
    // under fifteen seconds, whether or not anyone is connected. A
    // sweep tied to a connection's read loop dies with the connection
    // and lags the TTL by a minute. The limiter map rides the same
    // beat: its idle keys are reclaims, not resets.
    let evictor_store = store.clone();
    let evictor_limiters = limiters.clone();
    let every = (args.keep_minutes * 60 / 4).max(15);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(every));
        evictor_store.lock().expect("the event store").evict(now_secs());
        sweep_limiters(&mut evictor_limiters.lock().expect("the rate limiter map"), now_secs());
    });

    // The doors are bounded: connections (each an OS thread), frame
    // size (set inside one_connection), handshake time (set here), and
    // subscriptions per connection (set in the REQ arm). None of it
    // matters on the loopback default; all of it matters the day
    // `--bind` or `serve` points the relay somewhere public.
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("nip46-relay: accepting failed: {e}");
                continue;
            }
        };
        if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            eprintln!("nip46-relay: connection cap {MAX_CONNECTIONS} reached; refusing");
            continue;
        }
        active.fetch_add(1, Ordering::SeqCst);
        struct ActiveGuard(Arc<AtomicUsize>);
        impl Drop for ActiveGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active_guard = ActiveGuard(active.clone());
        // The handshake has ten seconds to land: a TCP client that
        // opens and stalls parks its thread inside the upgrade read
        // for no longer than this. one_connection replaces it with the
        // beat once the upgrade is done.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(HANDSHAKE_SECS)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(HANDSHAKE_SECS)));
        let id = next_id.fetch_add(1, Ordering::SeqCst);
        let store = store.clone();
        let outbound = outbound.clone();
        let subscriptions = subscriptions.clone();
        let limiters = limiters.clone();
        std::thread::spawn(move || {
            let _active_guard = active_guard;
            if let Err(e) = one_connection(
                id,
                stream,
                store,
                outbound,
                subscriptions,
                limiters,
                args.rate_limit,
            ) {
                eprintln!("nip46-relay: connection {id} ended: {e:#}");
            }
        });
    }
    Ok(())
}

/// One connection's whole life: subscribe, publish, read, leave.
fn one_connection(
    id: u64,
    stream: std::net::TcpStream,
    store: Arc<Mutex<EventStore>>,
    outbound: Outbound,
    subscriptions: Subscriptions,
    limiters: Arc<Mutex<HashMap<nostr::key::PublicKey, RateLimiter>>>,
    rate_limit: usize,
) -> Result<()> {
    // tungstenite 0.26's config struct is non-exhaustive, so the one
    // field this relay cares about is set on the default.
    let mut config = tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(MAX_FRAME_BYTES);
    config.max_frame_size = Some(MAX_FRAME_BYTES);
    let mut socket = tungstenite::accept_with_config(stream, Some(config))?;
    // The poll-based loop needs a nonblocking socket: the thread
    // sleeps in poll(2) and reads only when the wire is readable, so
    // an idle connection costs no wake at all — the one-second
    // read-timeout heartbeat woke every connection every second of
    // its life (the resource audit's HIGH: a quarter-million CPU
    // wakes a day across the layer, mostly here). Sends pace through
    // `say`, whose budget is the stalled-reader fuse.
    socket.get_mut().set_nonblocking(true).context("setting the socket nonblocking")?;
    let sock_fd = socket.get_ref().raw_fd().context("the connection's raw fd")?;
    // Bounded, with a kick switch: a reader that stops reading must
    // cost the relay a bounded queue, not an unbounded one. The flag
    // is what a broadcaster pulls when the queue will not take a
    // frame; the connection closes itself the moment the kick's wake
    // rings its pipe.
    let kicked = Arc::new(AtomicBool::new(false));
    let (tx, rx): (SyncSender<Message>, Receiver<Message>) = sync_channel(QUEUE_CAP);
    let (wake, wake_write) = wakeup_pair().context("the connection's wakeup pipe")?;
    outbound.lock().expect("the outbound map").insert(id, (tx.clone(), kicked.clone(), wake_write));

    // The leaving guard: the maps' entries die with this frame, on
    // every exit path. Cleanup after `serve_connection` returns is the
    // happy path only — a panic inside (a poisoned lock's expect)
    // unwinds past it and leaks both entries until a broadcast kick
    // trips over them. The guard's Drop runs on the unwind too, and
    // takes the lock through the poison: a panicked peer thread has
    // nothing to say about this connection's leaving.
    struct Leaving {
        id: u64,
        subscriptions: Subscriptions,
        outbound: Outbound,
    }
    impl Drop for Leaving {
        fn drop(&mut self) {
            // A gone connection is not a subscriber, and its queued
            // frames are nobody's.
            self.subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.id);
            self.outbound
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.id);
        }
    }
    let _leaving = Leaving { id, subscriptions: subscriptions.clone(), outbound: outbound.clone() };

    // The direct expression: the guard outlives it, so the maps'
    // entries drop after the connection's last frame either way.
    serve_connection(
        id,
        &mut socket,
        sock_fd,
        &store,
        &outbound,
        &subscriptions,
        &limiters,
        rate_limit,
        &rx,
        &kicked,
        &wake,
    )
}

/// One inline frame, at the blocking send's pace: the socket is
/// nonblocking for the poll loop, so a full kernel send buffer waits
/// for write-readiness up to BEAT instead of either erroring a
/// connection the old blocking write would have paced through or
/// parking the loop on a stalled reader forever.
#[allow(clippy::result_large_err)]
// Same as send_paced, which this forwards to: the Err is tungstenite's
// own Error, whose size is that crate's design.
fn say(
    socket: &mut tungstenite::WebSocket<std::net::TcpStream>,
    message: Message,
) -> tungstenite::Result<()> {
    send_paced(socket, message, BEAT)
}

#[allow(clippy::too_many_arguments)]
fn serve_connection(
    id: u64,
    socket: &mut tungstenite::WebSocket<std::net::TcpStream>,
    sock_fd: i32,
    store: &Arc<Mutex<EventStore>>,
    outbound: &Outbound,
    subscriptions: &Subscriptions,
    limiters: &Arc<Mutex<HashMap<nostr::key::PublicKey, RateLimiter>>>,
    rate_limit: usize,
    rx: &Receiver<Message>,
    kicked: &Arc<AtomicBool>,
    wake: &WakeSource,
) -> Result<()> {
    // The poll set: the wire and this connection's wakeup pipe. An
    // enqueue rings the pipe and the frame leaves instantly; a frame
    // on the wire wakes the read. No ping fuse here — the queue and
    // the kick bound a connection already, and a silent client costs
    // a parked thread, which is what it cost the spin too, minus the
    // wakeups.
    let mut fds = [
        libc::pollfd { fd: sock_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wake.fd(), events: libc::POLLIN, revents: 0 },
    ];
    loop {
        // Queue, then pipe, then queue once more: every queued frame
        // precedes its byte, so this order strands neither — a byte
        // in the pipe always names a frame the second drain takes.
        for message in rx.try_iter() {
            say(socket, message)?;
        }
        wake.drain();
        for message in rx.try_iter() {
            say(socket, message)?;
        }
        // A broadcaster whose queue would not take a frame pulled this:
        // the peer held the socket open and read nothing, and the queue
        // cap decided it had stopped being a subscriber. Gone politely.
        if kicked.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            return Ok(());
        }
        // Read before parking. The naive shape — poll the fd, then
        // read one message — stalls: tungstenite's read pulls a whole
        // TCP segment and keeps the complete frames it did not return
        // in an internal buffer, so after one big read the raw fd goes
        // quiet while read() still has answers (measured: the bounded
        // subscriptions test parked mid-EOSE with eight REQs stranded
        // in that buffer). A WouldBlock is the only "nothing left"
        // verdict tungstenite gives — it empties its own buffer before
        // touching the stream — and it is the poll's trigger.
        let message = match socket.read() {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Ping(payload)) => {
                say(socket, Message::Pong(payload))?;
                continue;
            }
            Ok(Message::Pong(_)) | Ok(Message::Binary(_)) => continue,
            Ok(Message::Close(_)) => {
                // Best effort: the socket is nonblocking and the close
                // frame's flush may not fit the send buffer — the FIN
                // on drop carries the same verdict.
                let _ = socket.close(None);
                return Ok(());
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                // Asleep until the bell or the wire — no beat, no spin.
                poll_wait(&mut fds, -1).context("polling the connection")?;
                continue;
            }
            Err(e) => return Err(anyhow::anyhow!(e).context("reading")),
        };

        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&message) else {
            say(
                socket,
                Message::text(serde_json::json!(["NOTICE", "frames are JSON arrays"]).to_string()),
            )?;
            continue;
        };
        let Some(word) = frame.as_array().and_then(|a| a.first().and_then(|v| v.as_str())) else {
            say(
                socket,
                Message::text(serde_json::json!(["NOTICE", "unparsable frame"]).to_string()),
            )?;
            continue;
        };
        match word {
            "EVENT" => {
                let event: Event =
                    match serde_json::from_value(frame.get(1).cloned().unwrap_or_default()) {
                        Ok(event) => event,
                        Err(e) => {
                            say(
                                socket,
                                Message::text(
                                    serde_json::json!(["NOTICE", format!("unreadable event: {e}")])
                                        .to_string(),
                                ),
                            )?;
                            continue;
                        }
                    };
                let event_id = event.id.to_string();
                let verdict = {
                    let now = now_secs();
                    // The door's order is the Go pipeline's: the
                    // admission questions (signature, kind, window)
                    // first, the pubkey's budget second — a stale event
                    // does not consume a flood's budget.
                    let admission = store
                        .lock()
                        .expect("the event store")
                        .admission(&event, now)
                        .map_err(|rejected| match rejected {
                            Rejected::Kind(kind) => {
                                format!("this relay carries only kinds 24133 and 24135, not {kind}")
                            }
                            Rejected::Timestamp => {
                                "the event is outside the timestamp window".to_string()
                            }
                            Rejected::Signature => {
                                "invalid: the signature does not verify".to_string()
                            }
                            Rejected::Duplicate => String::new(),
                        });
                    if let Err(reason) = admission {
                        Err(reason)
                    } else {
                        // The budget is the pubkey's, not the
                        // connection's: a reconnect does not refresh a
                        // flood's budget. The capacity is the configured
                        // rate, the same number the Go original takes
                        // from its env. A key the map has never met is
                        // the map's only growth, so a new one pays the
                        // shed toll first: the idle go, then the
                        // oldest, and the map stays the size of the
                        // relay's real population however big the
                        // throwaway-key flood behind this event is.
                        let mut limiters = limiters.lock().expect("the rate limiter map");
                        if !limiters.contains_key(&event.pubkey) {
                            sweep_limiters(&mut limiters, now);
                        }
                        let limiter = limiters
                            .entry(event.pubkey)
                            .or_insert_with(|| RateLimiter::with_capacity(rate_limit));
                        if !limiter.admit(now) {
                            Err("rate-limited: too many events, slow down".to_string())
                        } else {
                            store
                                .lock()
                                .expect("the event store")
                                .insert_admitted(event.clone(), now)
                                .map(|_fresh| ())
                                .map_err(|rejected| match rejected {
                                    Rejected::Kind(kind) => {
                                        format!(
                                        "this relay carries only kinds 24133 and 24135, not {kind}"
                                    )
                                    }
                                    Rejected::Timestamp => {
                                        "the event is outside the timestamp window".to_string()
                                    }
                                    Rejected::Signature => {
                                        "invalid: the signature does not verify".to_string()
                                    }
                                    Rejected::Duplicate => String::new(),
                                })
                        }
                    }
                };
                match verdict {
                    Ok(()) => {
                        say(
                            socket,
                            Message::text(
                                serde_json::json!(["OK", event_id, true, ""]).to_string(),
                            ),
                        )?;
                        broadcast(&event, outbound, subscriptions);
                    }
                    Err(reason) if reason.is_empty() => {
                        // A duplicate answers true: the event IS held,
                        // and the sender is not wrong for sending twice.
                        say(
                            socket,
                            Message::text(
                                serde_json::json!(["OK", event_id, true, "duplicate"]).to_string(),
                            ),
                        )?;
                    }
                    Err(reason) => {
                        say(
                            socket,
                            Message::text(
                                serde_json::json!(["OK", event_id, false, reason]).to_string(),
                            ),
                        )?;
                    }
                }
            }
            "REQ" => {
                let Some(subscription_id) =
                    frame.get(1).and_then(|v| v.as_str()).map(str::to_string)
                else {
                    say(
                        socket,
                        Message::text(serde_json::json!(["NOTICE", "REQ wants an id"]).to_string()),
                    )?;
                    continue;
                };
                let filter = frame.get(2).cloned().unwrap_or_default();
                let filter = RelayFilter::from_json(&filter);
                // The scoping door, before anything else: a query that
                // could match NIP-46 traffic and is scoped by nothing
                // is refused with CLOSED, naming the rule, and leaves
                // no subscription behind. The Go original's
                // rejectFilter; the lane probes it with a firehose.
                if let Some(reason) = filter.reject_reason() {
                    say(
                        socket,
                        Message::text(
                            serde_json::json!(["CLOSED", subscription_id, reason]).to_string(),
                        ),
                    )?;
                    continue;
                }
                // Bounded before querying or replaying: each
                // subscription is a broadcast fan-out multiplier, and
                // an over-cap REQ must not force a store scan or emit a
                // spurious EOSE before CLOSED. A re-REQ of an id the
                // connection already holds is the replace rule, not
                // growth, and stays allowed.
                let over_limit = {
                    let subs = subscriptions.lock().expect("the subscription map");
                    subs.get(&id).is_some_and(|held| {
                        held.len() >= MAX_SUBSCRIPTIONS
                            && held.iter().all(|(held_id, _)| *held_id != subscription_id)
                    })
                };
                if over_limit {
                    say(
                        socket,
                        Message::text(
                            serde_json::json!([
                                "CLOSED",
                                subscription_id,
                                "blocked: too many subscriptions on this connection"
                            ])
                            .to_string(),
                        ),
                    )?;
                    continue;
                }
                // Replay what is held, then remember the subscription
                // for what arrives later. The replay first sweeps the
                // TTL: a query never meets an expired event, whatever
                // the evictor's beat is doing — the Go original's
                // evictLocked runs on every save, and this is the read
                // side of the same promise.
                let replay = {
                    let mut store = store.lock().expect("the event store");
                    store.evict(now_secs());
                    store.query(&filter)
                };
                for event in replay {
                    say(
                        socket,
                        Message::text(
                            serde_json::json!(["EVENT", subscription_id, event]).to_string(),
                        ),
                    )?;
                }
                // The frame that ends the catch-up: a client waits on
                // it to know the replay is over and live frames begin.
                say(
                    socket,
                    Message::text(serde_json::json!(["EOSE", subscription_id]).to_string()),
                )?;
                let mut subs = subscriptions.lock().expect("the subscription map");
                let entry = subs.entry(id).or_default();
                // NIP-01's replace rule: a new REQ with a subscription
                // id already in use REPLACES the old filter. Without
                // this, a client that re-REQs in a loop grows its Vec
                // one entry a time for the life of the connection —
                // the spec's replace rule is also the leak's cap.
                entry.retain(|(existing, _)| existing != &subscription_id);
                entry.push((subscription_id, filter));
            }
            "CLOSE" => {
                let Some(subscription_id) = frame.get(1).and_then(|v| v.as_str()) else {
                    continue;
                };
                subscriptions
                    .lock()
                    .expect("the subscription map")
                    .entry(id)
                    .or_default()
                    .retain(|(sub_id, _)| sub_id != subscription_id);
            }
            other => {
                say(
                    socket,
                    Message::text(
                        serde_json::json!([
                            "NOTICE",
                            format!("this relay answers EVENT, REQ and CLOSE, not {other}")
                        ])
                        .to_string(),
                    ),
                )?;
            }
        }
    }
}

/// A held event goes to every connection whose subscriptions match —
/// the sender's own included, because its subscription is its channel.
/// The canonical acquisition order for the relay's two maps:
/// subscriptions first, outbound second, everywhere, forever. This
/// function is the entire lock-rank policy — broadcast's walk and its
/// kick both acquire through it, so the order is structural rather
/// than a convention two call sites have to remember. A future third
/// lock site goes through here too, or the stress test
/// (`broadcast_kick_stress_never_deadlocks`) hangs and names the
/// regression.
/// The two map guards in canonical rank order: subscriptions, then
/// outbound. The type alias exists for the rank's sake — one name for
/// the pair every lock site takes.
type MapGuards<'a> = (
    std::sync::MutexGuard<'a, HashMap<u64, Vec<(String, RelayFilter)>>>,
    std::sync::MutexGuard<
        'a,
        HashMap<u64, (SyncSender<Message>, Arc<AtomicBool>, kuma::wake::WakeSink)>,
    >,
);

fn lock_maps<'a>(subscriptions: &'a Subscriptions, outbound: &'a Outbound) -> MapGuards<'a> {
    let subs = subscriptions.lock().expect("the subscription map");
    let queues = outbound.lock().expect("the outbound map");
    (subs, queues)
}

/// A queue that will not take a frame means a reader that stopped
/// reading: the frame is dropped, the connection is kicked (its flag
/// set, its queue and subscriptions taken away) and it closes itself
/// on its next beat. Kicks are collected during the walk and executed
/// after it — the locks are taken subs-then-outbound everywhere, and
/// a member is not removed from a map mid-iteration.
fn broadcast(event: &Event, outbound: &Outbound, subscriptions: &Subscriptions) {
    let mut kicked: Vec<u64> = Vec::new();
    {
        let (subs, mut queues) = lock_maps(subscriptions, outbound);
        // The event's serialized form is built ONCE per broadcast, not
        // once per matched subscription: the json! macro round-trips
        // the event through a Value tree and back to a string, and
        // doing that per match under both maps' locks was the
        // performance audit's lock-hold multiplier. The frame differs
        // only in sub_id, which splices in as text.
        let event_json = serde_json::to_string(event).unwrap_or_default();
        let event_p: Vec<nostr::key::PublicKey> = event.tags.public_keys().collect();
        for (conn_id, conn_subs) in subs.iter() {
            for (sub_id, filter) in conn_subs {
                if filter.matches_with(event, &event_p) {
                    // The sub_id is client-controlled text: serialize
                    // it, don't splice it raw — the json! path escaped
                    // it, and a format! splice would not.
                    let sub_json = serde_json::to_string(sub_id).unwrap_or_default();
                    let frame = format!("[\"EVENT\",{sub_json},{event_json}]");
                    if let Some((queue, _, wake)) = queues.get_mut(conn_id) {
                        match queue.try_send(Message::text(frame)) {
                            // Ring the pipe: the connection sleeps in
                            // poll, and the frame leaves when the bell
                            // rings, not on the next beat of a spin
                            // there is no longer a spin to have.
                            Ok(()) => wake.wake(),
                            Err(TrySendError::Full(_)) => kicked.push(*conn_id),
                            // Already gone; its own teardown holds the map.
                            Err(TrySendError::Disconnected(_)) => {}
                        }
                    }
                }
            }
        }
    }
    if !kicked.is_empty() {
        // Subs-then-outbound, through the same helper the walk uses:
        // the lock rank lives in ONE function, so a kick and a
        // broadcast running concurrently cannot AB-BA deadlock the
        // relay (the inversion was found by adversarial review; the
        // helper is what keeps the fix from drifting back).
        let (mut subs, mut queues) = lock_maps(subscriptions, outbound);
        for conn_id in kicked {
            if let Some((_, kicked, wake)) = queues.remove(&conn_id) {
                kicked.store(true, Ordering::SeqCst);
                // The kicked connection is asleep in poll like every
                // other: the flag alone would wait for a frame that
                // is never coming. Ring its pipe on the way out.
                wake.wake();
            }
            subs.remove(&conn_id);
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kuma::relay::RelayConfig;
    use nostr::event::{EventBuilder, FinalizeEvent, Kind, Tag};
    use nostr::key::Keys;

    /// A ws:// client's stream is the plain arm; the timeout lives on
    /// the TcpStream inside it.
    fn plain(
        socket: &mut tungstenite::WebSocket<
            tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
        >,
    ) -> &mut std::net::TcpStream {
        match socket.get_mut() {
            tungstenite::stream::MaybeTlsStream::Plain(stream) => stream,
            other => unreachable!("a ws:// client is plain, not {other:?}"),
        }
    }

    /// The relay over a real socket: subscribe, publish, and the event
    /// crosses to a second connection. The three behaviors a bunker's
    /// lane rests on; the store's rules behind them are the core's
    /// tests, and this one proves the wire.
    #[test]
    fn a_publish_crosses_to_a_subscriber_and_answers_ok() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let maps = (
            Arc::new(Mutex::new(EventStore::new(RelayConfig::default()))),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::<nostr::key::PublicKey, RateLimiter>::new())),
        );
        // Two connections before the test drives them: the bunker's
        // stand-in and the app's. The listener is shared across the
        // handler threads, which is exactly what the main loop does.
        let listener = Arc::new(listener);
        // Distinct connection ids — the outbound map and the
        // subscription map are both keyed by it, and a shared 0 would
        // have the second registration overwrite the first, leaving
        // one queue where the lane needs two. (The test used to pass
        // with both at 0, because the app conn received its own
        // broadcast and the assertion could not tell whose lane had
        // actually carried the event.)
        for id in 1..=2u64 {
            let maps = maps.clone();
            let listener = listener.clone();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let _ = one_connection(
                    id,
                    stream,
                    maps.0,
                    maps.1,
                    maps.2,
                    maps.3,
                    kuma::relay::RATE_LIMIT_PER_MINUTE,
                );
            });
        }
        let (mut bunker_conn, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
        let (mut app_conn, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
        plain(&mut bunker_conn).set_read_timeout(Some(BEAT)).unwrap();
        plain(&mut app_conn).set_read_timeout(Some(BEAT)).unwrap();

        // Both subscribe for requests addressed to the bunker.
        let bunker_pk = bunker_conn_public_key();
        let filter = serde_json::json!({
            "kinds": [24133],
            "#p": [bunker_pk.to_string()],
        });
        bunker_conn
            .send(Message::text(serde_json::json!(["REQ", "bunker-sub", filter]).to_string()))
            .unwrap();
        app_conn
            .send(Message::text(serde_json::json!(["REQ", "app-sub", filter]).to_string()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));

        // The app publishes a request addressed to the bunker.
        let app_keys = Keys::generate();
        let request = EventBuilder::new(Kind::from_u16(24133), "payload")
            .tag(Tag::public_key(bunker_pk))
            .finalize(&app_keys)
            .unwrap();
        app_conn.send(Message::text(serde_json::json!(["EVENT", request]).to_string())).unwrap();

        // The app's own OK reply, then the forwarded event on both
        // subscriptions — the app's included, because its subscription
        // is its channel.
        let mut saw_ok = false;
        let mut event_on_bunker = false;
        let mut event_on_app = false;
        for _ in 0..20 {
            if let Ok(Message::Text(text)) = bunker_conn.read() {
                let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                if frame[0] == "EVENT" && frame[1] == "bunker-sub" {
                    assert_eq!(frame[2]["id"], request.id.to_string());
                    event_on_bunker = true;
                }
            }
            if let Ok(Message::Text(text)) = app_conn.read() {
                let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                if frame[0] == "OK" {
                    assert_eq!(frame[1], request.id.to_string());
                    assert_eq!(frame[2], true);
                    saw_ok = true;
                }
                if frame[0] == "EVENT" && frame[2]["id"] == request.id.to_string() {
                    event_on_app = true;
                }
            }
            if saw_ok && event_on_bunker && event_on_app {
                return;
            }
        }
        panic!(
            "the lane did not close the loop: ok={saw_ok} bunker={event_on_bunker} app={event_on_app}"
        );
    }

    /// The lock-rank contract, held under contention: broadcasters and
    /// the kick path hammer the two maps concurrently through
    /// lock_maps, a deliberately tiny queue forces kicks on nearly
    /// every broadcast, and the loop COMPLETING is the assertion — an
    /// AB-BA deadlock hangs this test, and the CI job's timeout names
    /// it. Completion also implies the queue stayed bounded: the cap
    /// turned overflow into kicks rather than growth.
    #[test]
    fn broadcast_kick_stress_never_deadlocks() {
        let outbound: Outbound = Arc::new(Mutex::new(HashMap::new()));
        let subscriptions: Subscriptions = Arc::new(Mutex::new(HashMap::new()));

        // One subscriber whose queue is deliberately tiny: nearly
        // every broadcast fills it, and every fill is a kick racing a
        // broadcast for the two locks.
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let kicked = Arc::new(AtomicBool::new(false));
        let (_, wake_write) = kuma::wake::wakeup_pair().expect("a wakeup pipe is two fds");
        outbound.lock().expect("outbound").insert(1, (tx, kicked.clone(), wake_write));
        let filter = RelayFilter::from_json(&serde_json::json!({ "kinds": [24133] }));
        subscriptions
            .lock()
            .expect("subscriptions")
            .insert(1, vec![("stress-sub".to_string(), filter)]);

        // Time-boxed on purpose: a shared CI runner is a flake source,
        // and a stress test that flaked would go red under the
        // flaky-result policy. The wall-clock ceiling bounds the run
        // to seconds on any machine; contention is asserted POSITIVELY
        // below (broadcasts attempted beyond the eight-slot queue =
        // overflow happened; the kick flag set = the kick path ran),
        // so a fast machine cannot pass vacuously.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for broadcaster in 0..4 {
            let outbound = outbound.clone();
            let subscriptions = subscriptions.clone();
            let stop = stop.clone();
            handles.push(std::thread::spawn(move || {
                let mut i: u64 = 0;
                while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    let event =
                        EventBuilder::new(Kind::from_u16(24133), format!("{broadcaster}:{i}"))
                            .tag(Tag::public_key(Keys::generate().public_key()))
                            .finalize(&Keys::generate())
                            .unwrap();
                    broadcast(&event, &outbound, &subscriptions);
                    i += 1;
                }
            }));
        }
        let mut attempted: u64 = 0;
        while std::time::Instant::now() < deadline {
            let event = EventBuilder::new(Kind::from_u16(24133), "stress")
                .tag(Tag::public_key(Keys::generate().public_key()))
                .finalize(&Keys::generate())
                .unwrap();
            broadcast(&event, &outbound, &subscriptions);
            attempted += 1;
        }
        stop.store(true, Ordering::SeqCst);
        for handle in handles {
            handle.join().expect("a broadcaster panicked under contention");
        }
        // Contention, positively asserted: more events were attempted
        // than the eight-slot queue could hold, and the kick flag set.
        assert!(attempted > 8, "the stress never contended: {attempted} broadcasts");
        assert!(kicked.load(Ordering::SeqCst), "the kick path ran under contention");
        let _ = rx.try_recv();
        // The end state is the kick's own: both maps dropped the
        // kicked subscriber — and the maps agree with each other,
        // which is what a deadlock between their locks would break.
        assert_eq!(outbound.lock().expect("outbound").len(), 0, "the kicked queue is gone");
        assert_eq!(subscriptions.lock().expect("subs").len(), 0, "the kicked subscription is gone");
    }

    /// A frame over the wire cap is refused: the library errors on the
    /// read, the connection ends, and nothing oversized reaches the
    /// store.
    #[test]
    fn an_oversized_frame_ends_the_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let maps = (
            Arc::new(Mutex::new(EventStore::new(RelayConfig::default()))),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::<nostr::key::PublicKey, RateLimiter>::new())),
        );
        let listener = Arc::new(listener);
        let maps = maps.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = one_connection(
                1,
                stream,
                maps.0,
                maps.1,
                maps.2,
                maps.3,
                kuma::relay::RATE_LIMIT_PER_MINUTE,
            );
        });
        let (mut conn, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
        plain(&mut conn).set_read_timeout(Some(BEAT)).unwrap();

        let oversized = "x".repeat(MAX_FRAME_BYTES + 1);
        // The send is allowed to fail: the server rejects the frame at
        // the cap and closes, and the reset can land mid-write — the
        // connection ending is the assertion, and it may surface on
        // the write side before the read loop below sees it. (The
        // rolling lap's musl runner lost this race; the glibc runner
        // won it. A race is not a pass/fail.)
        let _ = conn.send(Message::text(oversized));
        let mut saw_close = false;
        for _ in 0..10 {
            match conn.read() {
                // The relay's own rule, served back at the test: a
                // read timeout is this loop's heartbeat, never a
                // close. Counting it was the Nov 2026 test-honesty
                // audit's HIGH — with the cap deleted, the server
                // replies NOTICE and goes silent, the client's own
                // 1-second timeout landed here, and the test passed
                // with exactly the bug it exists to catch. Continue
                // on the timeout; count only real ends.
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(tungstenite::Error::ConnectionClosed)
                | Err(tungstenite::Error::Protocol(_))
                | Err(tungstenite::Error::Capacity(_))
                | Err(tungstenite::Error::Io(_)) => {
                    saw_close = true;
                    break;
                }
                Ok(_) => {}
                Err(e) => panic!("unexpected websocket error: {e:?}"),
            }
        }
        assert!(saw_close, "the oversized frame ends the connection");
    }

    /// Subscriptions on one connection are bounded: the 33rd distinct
    /// id is CLOSED by name, and a re-REQ of an id the connection
    /// already holds is the replace rule, not growth.
    #[test]
    fn subscriptions_on_one_connection_are_bounded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let maps = (
            Arc::new(Mutex::new(EventStore::new(RelayConfig::default()))),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::<nostr::key::PublicKey, RateLimiter>::new())),
        );
        let listener = Arc::new(listener);
        let maps = maps.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = one_connection(
                7,
                stream,
                maps.0,
                maps.1,
                maps.2,
                maps.3,
                kuma::relay::RATE_LIMIT_PER_MINUTE,
            );
        });
        let (mut conn, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
        plain(&mut conn).set_read_timeout(Some(BEAT)).unwrap();

        for i in 0..(MAX_SUBSCRIPTIONS + 8) {
            conn.send(Message::text(
                serde_json::json!([
                    "REQ",
                    format!("sub-{i}"),
                    { "kinds": [24133], "ids": [format!("{i:064x}")] }
                ])
                .to_string(),
            ))
            .unwrap();
        }
        let mut closed = 0;
        let mut spurious_eose = false;
        let over_cap_id = format!("sub-{MAX_SUBSCRIPTIONS}");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match conn.read() {
                Ok(Message::Text(text)) => {
                    let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if frame[0] == "EOSE" && frame[1] == over_cap_id {
                        spurious_eose = true;
                    }
                    if frame[0] == "CLOSED" && text.contains("too many subscriptions") {
                        closed += 1;
                        break;
                    }
                }
                Err(_) => break,
                _ => {}
            }
        }
        assert!(!spurious_eose, "an over-cap subscription must be CLOSED without EOSE");
        assert!(closed >= 1, "the over-cap subscription is CLOSED by name");
    }

    /// The bunker pubkey the test's filter addresses — a fresh key per
    /// call is fine; the relay carries no state about it.
    fn bunker_conn_public_key() -> nostr::key::PublicKey {
        Keys::generate().public_key()
    }

    /// The door at the wire: a kind the relay does not carry is
    /// refused in the OK reply, with the reason — and nothing is held.
    #[test]
    fn a_kind_the_relay_does_not_carry_is_refused_with_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let maps = (
            Arc::new(Mutex::new(EventStore::new(RelayConfig::default()))),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::<nostr::key::PublicKey, RateLimiter>::new())),
        );
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = one_connection(
                0,
                stream,
                maps.0,
                maps.1,
                maps.2,
                maps.3,
                kuma::relay::RATE_LIMIT_PER_MINUTE,
            );
        });
        let (mut client, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}")).unwrap();
        plain(&mut client).set_read_timeout(Some(BEAT)).unwrap();

        let note = EventBuilder::new(Kind::TextNote, "not my business")
            .finalize(&Keys::generate())
            .unwrap();
        client.send(Message::text(serde_json::json!(["EVENT", note]).to_string())).unwrap();
        for _ in 0..10 {
            if let Ok(Message::Text(text)) = client.read() {
                let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(frame[0], "OK");
                assert_eq!(frame[2], false);
                let reason = frame[3].as_str().unwrap();
                assert!(reason.contains("24133"), "the refusal names the rule: {reason}");
                return;
            }
        }
        panic!("no OK answer for a refused event");
    }
}
