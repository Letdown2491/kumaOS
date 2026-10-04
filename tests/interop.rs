//! The interop lane: one scripted NIP-46 client, any relay, the same
//! assertions.
//!
//! The plan's equivalence proof lives here: the same client runs
//! against the Go original and against the Rust port, and the same
//! behaviors must answer on both wires — the OK replies, the forwarded
//! frames, the refusals with their rules named. What the script
//! asserts is structure, not wording: a refusal's reason is an
//! implementation's own voice, and requiring the two relays to speak
//! identical prose would test the prose.
//!
//! The lane runs when `NIP46_RELAY_URL` names a relay; the offline
//! suite (no relay named) skips it with a line, because the suite
//! stays offline and the lane is CI's. The probes that need tuned
//! knobs — the flood and the eviction — run when the lane's env names
//! them, which CI does for both relays identically:
//!
//! ```console
//! $ NIP46_RELAY_URL=ws://127.0.0.1:3334 NIP46_RATE_LIMIT=5 \
//!     NIP46_EVICTION_WAIT_SECS=90 cargo test --test interop
//! ```

use std::time::{Duration, Instant};

use nostr::event::{Event, EventBuilder, FinalizeEvent, Kind, Tag};
use nostr::key::Keys;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message;

type Ws = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

fn plain(socket: &mut Ws) -> &mut std::net::TcpStream {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        other => unreachable!("the lane speaks ws:// only, not {other:?}"),
    }
}

/// Read one frame, tolerating the heartbeat: the lane's sockets carry
/// a one-second read timeout, and a would-block is not an answer.
fn read_frame(socket: &mut Ws) -> Option<serde_json::Value> {
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                return serde_json::from_str(&text).ok();
            }
            Ok(Message::Ping(payload)) => {
                socket.send(Message::Pong(payload)).ok()?;
            }
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return None
            }
            Err(_) => return None,
        }
    }
}

/// Read frames until one satisfies `want`, with a deadline. The
/// lane's patience: relays forward on their own beats.
fn wait_for(
    socket: &mut Ws,
    seconds: u64,
    mut want: impl FnMut(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if Instant::now() > deadline {
            return None;
        }
        if let Some(frame) = read_frame(socket) {
            if want(&frame) {
                return Some(frame);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A valid kind 24133 request, addressed to the bunker key, fresh.
fn request(bunker_pk: &nostr::key::PublicKey, app: &Keys, label: &str) -> Event {
    EventBuilder::new(Kind::from_u16(24133), label)
        .tag(Tag::public_key(*bunker_pk))
        .finalize(app)
        .unwrap()
}

#[test]
fn the_lane_probes_the_relay() {
    let Some(url) = std::env::var("NIP46_RELAY_URL").ok() else {
        println!("the interop lane needs NIP46_RELAY_URL; the offline suite skips it");
        return;
    };

    let bunker = Keys::generate();
    let bunker_pk = bunker.public_key();
    let app = Keys::generate();

    let (mut client, _) = tungstenite::connect(&url).unwrap();
    plain(&mut client).set_read_timeout(Some(Duration::from_secs(1))).unwrap();

    // The scoping door: an unscoped NIP-46 query is refused with a
    // CLOSED naming the rule; a scoped one answers with a replay (here
    // empty) and the EOSE that ends the catch-up.
    let scoped_filter = serde_json::json!({
        "kinds": [24133],
        "#p": [bunker_pk.to_string()],
        "limit": 100,
    });
    client
        .send(Message::text(serde_json::json!(["REQ", "lane-scoped", scoped_filter]).to_string()))
        .unwrap();
    let eose = wait_for(&mut client, 10, |frame| frame[0] == "EOSE" && frame[1] == "lane-scoped");
    assert!(eose.is_some(), "a scoped subscription answers with an EOSE");

    client
        .send(Message::text(
            serde_json::json!(["REQ", "lane-firehose", {"kinds": [24133]}]).to_string(),
        ))
        .unwrap();
    let closed =
        wait_for(&mut client, 10, |frame| frame[0] == "CLOSED" && frame[1] == "lane-firehose");
    let closed = closed.expect("an unscoped NIP-46 query is refused");
    assert!(
        closed[2].as_str().unwrap().contains("scoped"),
        "the refusal names the scoping rule: {closed}"
    );

    // The publish door: a fresh valid request is accepted and held —
    // the OK is the answer, and a resubscription's replay is the
    // holding's proof.
    let fresh = request(&bunker_pk, &app, "lane-fresh");
    client.send(Message::text(serde_json::json!(["EVENT", fresh]).to_string())).unwrap();
    let ok =
        wait_for(&mut client, 10, |frame| frame[0] == "OK" && frame[1] == fresh.id.to_string());
    let ok = ok.expect("a valid publish answers OK");
    assert_eq!(ok[2], true, "a fresh valid event is accepted: {ok}");

    // A duplicate answers true as well: the event IS held.
    client.send(Message::text(serde_json::json!(["EVENT", fresh]).to_string())).unwrap();
    let dup =
        wait_for(&mut client, 10, |frame| frame[0] == "OK" && frame[1] == fresh.id.to_string());
    assert_eq!(dup.expect("a duplicate answers")[2], true);

    // The replay: a new subscription with the same filter receives the
    // held event, then its own EOSE.
    client
        .send(Message::text(serde_json::json!(["REQ", "lane-replay", scoped_filter]).to_string()))
        .unwrap();
    let replayed = wait_for(&mut client, 10, |frame| {
        frame[0] == "EVENT" && frame[2]["id"] == fresh.id.to_string()
    });
    assert!(replayed.is_some(), "a held event is replayed to a new subscription");

    // The refusals, each with a reason that names its rule.
    let refusals: Vec<(&str, Event)> = vec![
        (
            "a text note is not signing traffic",
            EventBuilder::new(Kind::TextNote, "not my business").finalize(&app).unwrap(),
        ),
        (
            "a stale event is outside the window",
            EventBuilder::new(Kind::from_u16(24133), "stale")
                .tag(Tag::public_key(bunker_pk))
                .custom_created_at(nostr::types::Timestamp::from(
                    nostr::types::Timestamp::now().as_secs() - 7200,
                ))
                .finalize(&app)
                .unwrap(),
        ),
        ("a tampered event does not verify", {
            let mut tampered = request(&bunker_pk, &app, "honest");
            tampered.content = "tampered".to_string();
            tampered
        }),
    ];
    for (what, event) in refusals {
        client.send(Message::text(serde_json::json!(["EVENT", event]).to_string())).unwrap();
        let ok =
            wait_for(&mut client, 10, |frame| frame[0] == "OK" && frame[1] == event.id.to_string());
        let ok = ok.unwrap_or_else(|| panic!("{what}: no OK arrived"));
        assert_eq!(ok[2], false, "{what}: the refusal: {ok}");
        assert!(
            !ok[3].as_str().unwrap_or_default().is_empty(),
            "{what}: the refusal names its rule"
        );
    }

    // The forwarding: a second connection subscribes, the first
    // publishes, the frame crosses — the lane a bunker and a phone
    // run.
    let (mut subscriber, _) = tungstenite::connect(&url).unwrap();
    plain(&mut subscriber).set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    subscriber
        .send(Message::text(serde_json::json!(["REQ", "lane-sub", scoped_filter]).to_string()))
        .unwrap();
    wait_for(&mut subscriber, 10, |frame| frame[0] == "EOSE");

    let crossing = request(&bunker_pk, &app, "lane-crossing");
    client.send(Message::text(serde_json::json!(["EVENT", crossing]).to_string())).unwrap();
    let forwarded = wait_for(&mut subscriber, 10, |frame| {
        frame[0] == "EVENT" && frame[2]["id"] == crossing.id.to_string()
    });
    assert!(forwarded.is_some(), "a held-and-fresh event is forwarded to subscribers");

    // The flood: a budget configured by the lane is a budget both
    // relays enforce. A second pubkey spends its own budget — the
    // limit's unit is the pubkey, and this pubkey's is fresh.
    if let Ok(budget) = std::env::var("NIP46_RATE_LIMIT").map(|n| n.parse::<usize>().unwrap()) {
        let flooder = Keys::generate();
        let mut last_ok = None;
        for i in 0..(budget + 2) {
            let event = request(&bunker_pk, &flooder, &format!("flood-{i}"));
            client.send(Message::text(serde_json::json!(["EVENT", event]).to_string())).unwrap();
            let ok = wait_for(&mut client, 10, |frame| {
                frame[0] == "OK" && frame[1] == event.id.to_string()
            });
            last_ok = Some(ok.expect("every event answers"));
        }
        assert_eq!(last_ok.unwrap()[2], false, "the {budget}th-and-a-bit event is refused");
    }

    // The eviction: with a one-minute retention the lane waits out the
    // TTL and the replay comes back empty. The offline core's tests
    // prove the same behavior with a wound clock; this probe proves
    // the wire's clock actually runs.
    if let Some(wait) =
        std::env::var("NIP46_EVICTION_WAIT_SECS").ok().map(|s| s.parse::<u64>().unwrap())
    {
        let expired = request(&bunker_pk, &app, "lane-expiring");
        client.send(Message::text(serde_json::json!(["EVENT", expired]).to_string())).unwrap();
        wait_for(&mut client, 10, |frame| frame[0] == "OK" && frame[1] == expired.id.to_string());
        // The wait is not silence: a relay pings a quiet wire and walks
        // off one that pongs nothing (khatru pings every 30s and drops
        // the connection after 60s of them), so the lane keeps reading
        // — and read_frame answers the heartbeat — while the store's
        // clock runs out. The road learned this first; the lane obeys
        // the same rule.
        let wake = Instant::now() + Duration::from_secs(wait);
        while Instant::now() < wake {
            read_frame(&mut client);
            std::thread::sleep(Duration::from_millis(50));
        }
        client
            .send(Message::text(
                serde_json::json!(["REQ", "lane-after-eviction", scoped_filter]).to_string(),
            ))
            .unwrap();
        // The replay is every EVENT frame between the REQ and its
        // EOSE; the expired event must not be in it.
        let mut replayed_ids = Vec::new();
        loop {
            let frame = read_frame(&mut client);
            match frame {
                Some(f) if f[0] == "EOSE" && f[1] == "lane-after-eviction" => break,
                Some(f) if f[0] == "EVENT" => {
                    replayed_ids.push(f[2]["id"].as_str().unwrap_or_default().to_string());
                }
                Some(_) => continue,
                None => panic!("the replay after eviction never ended"),
            }
        }
        assert!(
            !replayed_ids.contains(&expired.id.to_string()),
            "an evicted event is not replayed: {replayed_ids:?}"
        );
    }
}
