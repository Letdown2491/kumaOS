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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use nostr::event::Event;
use tungstenite::Message;

use kuma::relay::{EventStore, RateLimiter, Rejected, RelayFilter};

/// The shared server state: each connection's outbound queue and its
/// subscriptions, found by connection id.
type Subscriptions = Arc<Mutex<HashMap<u64, Vec<(String, RelayFilter)>>>>;
type Outbound = Arc<Mutex<HashMap<u64, Sender<Message>>>>;

/// The heartbeat of every connection thread: outbound frames drain on
/// this cadence. Long enough that an idle relay costs nothing; short
/// enough that a queued answer goes out before its reader gives up.
const BEAT: Duration = Duration::from_secs(1);

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
    // and lags the TTL by a minute.
    let evictor_store = store.clone();
    let every = (args.keep_minutes * 60 / 4).max(15);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(every));
        evictor_store.lock().expect("the event store").evict(now_secs());
    });

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("nip46-relay: accepting failed: {e}");
                continue;
            }
        };
        let id = next_id.fetch_add(1, Ordering::SeqCst);
        let store = store.clone();
        let outbound = outbound.clone();
        let subscriptions = subscriptions.clone();
        let limiters = limiters.clone();
        std::thread::spawn(move || {
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
    let mut socket = tungstenite::accept(stream)?;
    socket.get_mut().set_read_timeout(Some(BEAT))?;
    let (tx, rx): (Sender<Message>, Receiver<Message>) = channel();
    outbound.lock().expect("the outbound map").insert(id, tx.clone());

    let result = serve_connection(
        id,
        &mut socket,
        &store,
        &outbound,
        &subscriptions,
        &limiters,
        rate_limit,
        &rx,
    );

    // Leaving takes the subscriptions and the queue with it: a gone
    // connection is not a subscriber, and its queued frames are nobody's.
    subscriptions.lock().expect("the subscription map").remove(&id);
    outbound.lock().expect("the outbound map").remove(&id);
    result
}

#[allow(clippy::too_many_arguments)]
fn serve_connection(
    id: u64,
    socket: &mut tungstenite::WebSocket<std::net::TcpStream>,
    store: &Arc<Mutex<EventStore>>,
    outbound: &Outbound,
    subscriptions: &Subscriptions,
    limiters: &Arc<Mutex<HashMap<nostr::key::PublicKey, RateLimiter>>>,
    rate_limit: usize,
    rx: &Receiver<Message>,
) -> Result<()> {
    loop {
        // Outbound first, on every beat: answers queued by other
        // connections' events go out before this thread blocks again.
        for message in rx.try_iter() {
            socket.send(message)?;
        }

        let message = match socket.read() {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Ping(payload)) => {
                socket.send(Message::Pong(payload))?;
                continue;
            }
            Ok(Message::Pong(_)) | Ok(Message::Binary(_)) => continue,
            Ok(Message::Close(_)) => {
                socket.close(None)?;
                return Ok(());
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                continue
            }
            Err(e) => return Err(anyhow::anyhow!(e).context("reading")),
        };

        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&message) else {
            socket.send(Message::text(
                serde_json::json!(["NOTICE", "frames are JSON arrays"]).to_string(),
            ))?;
            continue;
        };
        let Some(word) = frame.as_array().and_then(|a| a.first().and_then(|v| v.as_str())) else {
            socket.send(Message::text(
                serde_json::json!(["NOTICE", "unparsable frame"]).to_string(),
            ))?;
            continue;
        };
        match word {
            "EVENT" => {
                let event: Event =
                    match serde_json::from_value(frame.get(1).cloned().unwrap_or_default()) {
                        Ok(event) => event,
                        Err(e) => {
                            socket.send(Message::text(
                                serde_json::json!(["NOTICE", format!("unreadable event: {e}")])
                                    .to_string(),
                            ))?;
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
                        // from its env.
                        let mut limiters = limiters.lock().expect("the rate limiter map");
                        let limiter = limiters
                            .entry(event.pubkey)
                            .or_insert_with(|| RateLimiter::with_capacity(rate_limit));
                        if !limiter.admit(now) {
                            Err("rate-limited: too many events, slow down".to_string())
                        } else {
                            store
                                .lock()
                                .expect("the event store")
                                .insert(event.clone(), now)
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
                        socket.send(Message::text(
                            serde_json::json!(["OK", event_id, true, ""]).to_string(),
                        ))?;
                        broadcast(&event, outbound, subscriptions);
                    }
                    Err(reason) if reason.is_empty() => {
                        // A duplicate answers true: the event IS held,
                        // and the sender is not wrong for sending twice.
                        socket.send(Message::text(
                            serde_json::json!(["OK", event_id, true, "duplicate"]).to_string(),
                        ))?;
                    }
                    Err(reason) => {
                        socket.send(Message::text(
                            serde_json::json!(["OK", event_id, false, reason]).to_string(),
                        ))?;
                    }
                }
            }
            "REQ" => {
                let Some(subscription_id) =
                    frame.get(1).and_then(|v| v.as_str()).map(str::to_string)
                else {
                    socket.send(Message::text(
                        serde_json::json!(["NOTICE", "REQ wants an id"]).to_string(),
                    ))?;
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
                    socket.send(Message::text(
                        serde_json::json!(["CLOSED", subscription_id, reason]).to_string(),
                    ))?;
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
                    socket.send(Message::text(
                        serde_json::json!(["EVENT", subscription_id, event]).to_string(),
                    ))?;
                }
                // The frame that ends the catch-up: a client waits on
                // it to know the replay is over and live frames begin.
                socket.send(Message::text(
                    serde_json::json!(["EOSE", subscription_id]).to_string(),
                ))?;
                subscriptions
                    .lock()
                    .expect("the subscription map")
                    .entry(id)
                    .or_default()
                    .push((subscription_id, filter));
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
                socket.send(Message::text(
                    serde_json::json!([
                        "NOTICE",
                        format!("this relay answers EVENT, REQ and CLOSE, not {other}")
                    ])
                    .to_string(),
                ))?;
            }
        }
    }
}

/// A held event goes to every connection whose subscriptions match —
/// the sender's own included, because its subscription is its channel.
fn broadcast(event: &Event, outbound: &Outbound, subscriptions: &Subscriptions) {
    let subs = subscriptions.lock().expect("the subscription map");
    for (conn_id, conn_subs) in subs.iter() {
        for (sub_id, filter) in conn_subs {
            if filter.matches(event) {
                let frame = serde_json::json!(["EVENT", sub_id, event]).to_string();
                if let Some(queue) = outbound.lock().expect("the outbound map").get_mut(conn_id) {
                    let _ = queue.send(Message::text(frame));
                }
            }
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
        for _ in 0..2 {
            let maps = maps.clone();
            let listener = listener.clone();
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
        let mut event_crossed = false;
        for _ in 0..20 {
            if let Ok(Message::Text(text)) = bunker_conn.read() {
                let frame: serde_json::Value = serde_json::from_str(&text).unwrap();
                if frame[0] == "EVENT" && frame[1] == "bunker-sub" {
                    assert_eq!(frame[2]["id"], request.id.to_string());
                    event_crossed = true;
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
                    event_crossed = true;
                }
            }
            if saw_ok && event_crossed {
                return;
            }
        }
        panic!("the lane did not close the loop: ok={saw_ok} event={event_crossed}");
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
