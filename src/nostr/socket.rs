//! The unix socket the daemon answers on, and the framing both ends of
//! the layer agree on.
//!
//! One request line in, one response line out. Reaching the socket
//! grants nothing by itself: the socket lives under `XDG_RUNTIME_DIR`
//! (0700, user-owned), is chmod 0600, and a peer whose uid is not the
//! daemon's own is dropped before their first byte is read. The last
//! check is what "the paired socket is the local channel" leans on —
//! a same-uid process is inside the trust boundary already, and
//! anything else is not getting past the kernel.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};

use super::protocol::{self, Daemon};
use super::vault::SecretStore;

/// The request line's cap: a real request carries an event's params or
/// an ncryptsec — kilobytes — and nothing honest arrives megabyte-
/// sized on a 0600 socket whose peer is the same uid. The bound is
/// what turns a runaway sender into a refused line instead of the
/// daemon's RSS.
const MAX_LINE: usize = 1024 * 1024;

/// The socket path under the session's runtime directory. The same name
/// the unit will own, the CLI will look for, and the doctor's socket
/// check will probe.
pub const SOCKET_NAME: &str = "kuma-nostr.sock";

/// Where the socket lives: the session's runtime directory, which is the
/// one place a per-user, session-scoped, root-owned-by-nobody file
/// belongs. Refuses to guess when the session did not say.
pub fn default_socket_path() -> Result<PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map_err(|_| anyhow!("XDG_RUNTIME_DIR is not set: run the daemon inside a user session"))?;
    Ok(dir.join(SOCKET_NAME))
}

/// Bind the listening socket: unlink a stale path first (a daemon that
/// died without cleanup must not wedge the next one), bind, chmod 0600.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("removing the stale socket at {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 on {}", path.display()))?;
    Ok(listener)
}

/// Serve connections until the process is signalled. Each connection is
/// one thread with a tokio runtime of its own; the daemon behind them
/// is a mutex, because the verbs are short and the vault's answer must
/// be the vault's truth.
pub fn serve<S: SecretStore + Send + 'static>(
    listener: UnixListener,
    daemon: Arc<Mutex<Daemon<S>>>,
) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let daemon = daemon.clone();
                if !peer_is_self(&stream) {
                    eprintln!("kuma-nostrd: refused a peer that is not this user");
                    continue;
                }
                std::thread::spawn(move || {
                    if let Err(e) = one_connection(stream, daemon) {
                        eprintln!("kuma-nostrd: connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("kuma-nostrd: accepting on the socket failed: {e}");
                return;
            }
        }
    }
}

/// One connection: lines until the peer hangs up. A read error ends the
/// connection; a request error is *answered* — the caller is told their
/// line was refused, and the connection lives.
///
/// The runtime is the connection's own, built here and driven by
/// `Runtime::block_on` from this thread. The vault's keyring calls are
/// D-Bus calls whose executor tasks are spawned onto a runtime, and a
/// current-thread runtime polls its spawned tasks only while its own
/// thread is inside `block_on` — bridging into a runtime owned by
/// another thread would never poll them, and the first keyring verb
/// would wedge forever holding the daemon's lock. A runtime of one's
/// own drives what the request spawns, and the vault's futures stay
/// un-`Send`, as the vault intends. It matches the store's own shape:
/// the Secret Service client opens a new D-Bus connection per
/// operation, so nothing outlives the request that needed it.
fn one_connection<S: SecretStore>(stream: UnixStream, daemon: Arc<Mutex<Daemon<S>>>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building the connection runtime")?;
    let mut writer = stream.try_clone().context("cloning the socket for writing")?;
    let mut reader = BufReader::new(stream);
    loop {
        // A fresh buffer per line, and a hard cap on the read. The
        // line a request rides can carry a secret — import, export —
        // and `clear()` keeps the allocation: the buffer would hold
        // the last request's material until the connection's EOF.
        // Fresh per line, and the cap keeps a runaway sender from
        // growing the daemon's RSS to its own heart's content: over
        // the cap the answer is the error and the door closes behind
        // it, the tail of an over-long line being nobody's request.
        let mut line = String::new();
        let read = reader.by_ref().take(MAX_LINE as u64 + 1).read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if read > MAX_LINE {
            let answer =
                protocol::err_response(anyhow!("the request line exceeds the {MAX_LINE}-byte cap"));
            writer.write_all(protocol::encode(&answer).as_bytes())?;
            writer.flush()?;
            return Ok(());
        }
        let answer = match protocol::decode(line.trim_end()) {
            Ok(request) => {
                let mut daemon = daemon.lock().expect("daemon lock");
                runtime.block_on(daemon.handle(request))
            }
            Err(e) => protocol::err_response(e),
        };
        writer.write_all(protocol::encode(&answer).as_bytes())?;
        writer.flush()?;
    }
}

/// Whether the process on the other end runs as this daemon's own uid.
/// The socket's 0600 in a 0700 directory is the first wall; this is the
/// second, for filesystems where one of those facts was weaker than the
/// declaration assumed.
fn peer_is_self(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED with a ucred-sized buffer is the documented
    // form; the kernel fills exactly len bytes.
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    ok == 0 && cred.uid == unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::test_relay::wait_for;
    use crate::nostr::vault::{MemoryStore, Vault};
    use nostr::prelude::*;

    /// The socket round-trip: a real unix socket in a temp directory, a
    /// daemon over an in-memory store, a client that is a plain thread.
    /// This is the one test that proves framing, refusal, and the answer
    /// shape survive the actual kernel.
    #[test]
    fn a_client_drives_the_whole_life_cycle_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);
        let listener = bind(&path).unwrap();

        // The file the bind left behind is mode 0600, because anything
        // wider quietly re-draws the trust boundary the uid check draws.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let daemon = Arc::new(Mutex::new(
            Daemon::new(Vault::new(MemoryStore::default()), Vec::new(), None).0,
        ));
        std::thread::spawn(move || {
            serve(listener, daemon);
        });

        let mut client = UnixStream::connect(&path).unwrap();
        let ask = |mut client: &UnixStream, line: &str| -> String {
            client.write_all(line.as_bytes()).unwrap();
            client.write_all(b"\n").unwrap();
            let mut answer = String::new();
            BufReader::new(client.try_clone().unwrap()).read_line(&mut answer).unwrap();
            answer
        };

        // Two setup verbs, one refused: the second vault answer comes
        // back as a failure document, and the connection lives on.
        let first = ask(&mut client, r#"{"cmd":"setup","mode":{"how":"generate"}}"#);
        assert!(first.contains("\"pubkey\":\"npub1"), "{first}");
        let second = ask(&mut client, r#"{"cmd":"setup","mode":{"how":"generate"}}"#);
        assert!(second.contains("\"ok\":false"), "{second}");
        let ping = ask(&mut client, r#"{"cmd":"ping"}"#);
        assert!(ping.contains("\"ok\":true"));

        // A garbage line is answered as a refusal, and the client still
        // gets an answer to the next line on the same connection.
        let junk = ask(&mut client, "this is not json");
        assert!(junk.contains("\"ok\":false"));
        let still_alive = ask(&mut client, r#"{"cmd":"ping"}"#);
        assert!(still_alive.contains("\"ok\":true"));

        // Destroy without confirm is a dry run that destroys nothing.
        let dry = ask(&mut client, r#"{"cmd":"destroy"}"#);
        assert!(dry.contains("unrecoverable"));
        let status = ask(&mut client, r#"{"cmd":"status"}"#);
        assert!(status.contains("\"exists\":true"));
    }

    /// The nostrconnect flow's own crown: the person pastes the
    /// client's URI into the socket, the daemon publishes the
    /// handshake on the client's relays, the client validates its
    /// secret, and the client's method request after that is answered
    /// like any paired app's — the pairing the person's paste made.
    #[test]
    fn a_nostrconnect_uri_pairs_and_the_client_is_served() {
        use crate::nostr::test_relay::StubRelay;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectRequest};

        let stub = StubRelay::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);
        let listener = bind(&path).unwrap();
        let (daemon, inbound_rx) =
            Daemon::new(Vault::new(MemoryStore::default()), vec![stub.url.clone()], None);
        let engine = daemon.engine();
        let daemon = Arc::new(Mutex::new(daemon));
        std::thread::spawn({
            let daemon = daemon.clone();
            move || loop {
                let Ok(event) = inbound_rx.recv() else {
                    return;
                };
                let plan = { daemon.lock().unwrap().plan_bunker_event(&event) };
                let Some(plan) = plan else { continue };
                match plan {
                    crate::nostr::bunker::Plan::Ignore => continue,
                    crate::nostr::bunker::Plan::Answer(answer) => {
                        let _ = daemon.lock().unwrap().publish(&answer);
                    }
                    crate::nostr::bunker::Plan::Ask { .. } => {
                        // The ask rides the production road: decide,
                        // execute, publish off the worker (#82).
                        Daemon::spawn_ask(&daemon, engine.clone(), plan);
                    }
                    crate::nostr::bunker::Plan::Paired { .. }
                    | crate::nostr::bunker::Plan::RelaysServed { .. }
                    | crate::nostr::bunker::Plan::Ended { .. }
                    | crate::nostr::bunker::Plan::Shed { .. } => continue,
                }
            }
        });
        std::thread::spawn({
            let daemon = daemon.clone();
            move || serve(listener, daemon)
        });

        // The daemon is armed before the URI arrives: the socket
        // client set it up.
        let mut client = UnixStream::connect(&path).unwrap();
        let ask = |mut client: &UnixStream, line: &str| -> String {
            client.write_all(line.as_bytes()).unwrap();
            client.write_all(b"\n").unwrap();
            let mut answer = String::new();
            BufReader::new(client.try_clone().unwrap()).read_line(&mut answer).unwrap();
            answer
        };
        ask(&mut client, r#"{"cmd":"setup","mode":{"how":"generate"}}"#);

        // The client minted its own keys and its own secret, and shows
        // the URI the person pastes. Its relay is the same stub — the
        // daemon's own road and the client's overlap here. The relay
        // rides percent-encoded, the spelling the NIP's own example
        // uses.
        let app = Keys::generate();
        let relay_encoded = stub.url.replace(':', "%3A").replace('/', "%2F");
        let uri = format!(
            "nostrconnect://{}?relay={}&secret=the-client-secret&perms=sign_event%3A1&name=Pasted",
            app.public_key(),
            relay_encoded
        );
        let answer = ask(&mut client, &format!(r#"{{"cmd":"connect","uri":"{uri}"}}"#));
        assert!(answer.contains("\"ok\":true"), "the paste paired: {answer}");
        assert!(answer.contains("Pasted"), "the URI's name rode along: {answer}");

        // The handshake crossed the client's relay, and its content is
        // the response whose result is the client's own secret — the
        // proof the client validates against spoofing.
        wait_for("the handshake to reach the client's relay", 100, || {
            stub.received().iter().any(|frame| frame.contains(":24133"))
        });
        let frame = stub
            .received()
            .into_iter()
            .find(|frame| frame.contains(":24133"))
            .expect("the handshake went out");
        let parsed: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let handshake: Event = serde_json::from_value(parsed[1].clone()).unwrap();
        let plaintext = app.nip44_decrypt(&handshake.pubkey, &handshake.content).unwrap();
        match NostrConnectMessage::from_json(&plaintext).unwrap() {
            NostrConnectMessage::Response { result, .. } => {
                assert_eq!(result.as_deref(), Some("the-client-secret"));
            }
            other => panic!("the handshake is a response: {other:?}"),
        }

        // The pairing the paste made, visible like any other: the
        // name and the perms the URI claimed ride the record.
        let apps: serde_json::Value =
            serde_json::from_str(ask(&mut client, r#"{"cmd":"apps"}"#).trim()).unwrap();
        let record = &apps["apps"][0];
        assert_eq!(record["pubkey"].as_str(), Some(app.public_key().to_string().as_str()));
        assert_eq!(record["name"].as_str(), Some("Pasted"));
        assert_eq!(record["perms"].as_str(), Some("sign_event:1"));

        // And the client is served: its method request — through its
        // relay — comes back answered, the way a paired app's does.
        // The bunker's pubkey is the handshake's author, the way the
        // NIP says the client learns it.
        let bunker_pubkey = handshake.pubkey;
        let message = NostrConnectMessage::request(
            &NostrConnectRequest::from_message(
                nostr::nips::nip46::NostrConnectMethod::GetPublicKey,
                vec![],
            )
            .unwrap(),
        );
        let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app)
            .unwrap();
        stub.inject(&request);

        // The pairing landed at Ask — the person's paste approved the
        // pairing, not the methods — so the client's first ask queues
        // a prompt, and the paste's own person answers it here.
        wait_for("the prompt to appear", 100, || {
            ask(&mut client, r#"{"cmd":"prompts"}"#).contains("GetPublicKey")
        });
        let prompts: serde_json::Value =
            serde_json::from_str(ask(&mut client, r#"{"cmd":"prompts"}"#).trim()).unwrap();
        let prompt_id = prompts["prompts"][0]["id"].as_str().unwrap().to_string();
        let approved = ask(
            &mut client,
            &format!(r#"{{"cmd":"approve","id":"{prompt_id}","remember_hours":null}}"#),
        );
        assert!(approved.contains("\"ok\":true"), "{approved}");

        wait_for("the client's answer to come back", 100, || {
            let frames: Vec<String> =
                stub.received().into_iter().filter(|f| f.contains(":24133")).collect();
            frames.len() >= 2
        });
        let frames: Vec<String> =
            stub.received().into_iter().filter(|f| f.contains(":24133")).collect();
        let parsed: serde_json::Value = serde_json::from_str(&frames[1]).unwrap();
        let answer: Event = serde_json::from_value(parsed[1].clone()).unwrap();
        let plaintext = app.nip44_decrypt(&answer.pubkey, &answer.content).unwrap();
        match NostrConnectMessage::from_json(&plaintext).unwrap() {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None, "{error:?}");
                assert_eq!(result.as_deref(), Some(bunker_pubkey.to_string().as_str()));
            }
            other => panic!("a response came back: {other:?}"),
        }
    }

    /// The nostrconnect road, end to end, with the client's relays
    /// distinct from the bunker's: the handshake goes only to the
    /// client's relay, the client's request is answered on both roads,
    /// and revoking tears the client's roads down.
    #[test]
    fn a_nostrconnect_app_s_road_is_its_own_and_teardown_stops_it() {
        use crate::nostr::test_relay::{wait_for, StubRelay};
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectRequest};

        // Two relays: the bunker's own (stub1) and the client's (stub2).
        let own_relay = StubRelay::start(Vec::new());
        let client_relay = StubRelay::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);
        let listener = bind(&path).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (daemon, inbound_rx) =
            Daemon::new(Vault::new(MemoryStore::default()), vec![own_relay.url.clone()], None);
        let engine = daemon.engine();
        let daemon = Arc::new(Mutex::new(daemon));
        std::thread::spawn({
            let daemon = daemon.clone();
            let handle = runtime.handle().clone();
            move || loop {
                let Ok(event) = inbound_rx.recv() else {
                    return;
                };
                let plan = { daemon.lock().unwrap().plan_bunker_event(&event) };
                let Some(plan) = plan else { continue };
                match plan {
                    crate::nostr::bunker::Plan::Ignore => continue,
                    crate::nostr::bunker::Plan::Answer(answer) => {
                        let _ = daemon.lock().unwrap().publish(&answer);
                    }
                    crate::nostr::bunker::Plan::Paired { .. }
                    | crate::nostr::bunker::Plan::RelaysServed { .. }
                    | crate::nostr::bunker::Plan::Ended { .. }
                    | crate::nostr::bunker::Plan::Shed { .. } => continue,
                    crate::nostr::bunker::Plan::Ask { ref request, method, ref params, .. } => {
                        use crate::nostr::bunker::Gate;
                        // The client's first ask: approved here, so the
                        // answer's road is what carries it.
                        let decision =
                            if method == nostr::nips::nip46::NostrConnectMethod::GetPublicKey {
                                crate::nostr::bunker::Decision::Allow
                            } else {
                                handle.block_on(engine.decide(&request.pubkey, &method, params))
                            };
                        let answer =
                            { daemon.lock().unwrap().execute_bunker_event(plan, decision) };
                        if let Some(answer) = answer {
                            let _ = daemon.lock().unwrap().publish(&answer);
                        }
                    }
                }
            }
        });
        std::thread::spawn({
            let daemon = daemon.clone();
            move || serve(listener, daemon)
        });

        let mut client = UnixStream::connect(&path).unwrap();
        let ask = |mut client: &UnixStream, line: &str| -> String {
            client.write_all(line.as_bytes()).unwrap();
            client.write_all(b"\n").unwrap();
            let mut answer = String::new();
            BufReader::new(client.try_clone().unwrap()).read_line(&mut answer).unwrap();
            answer
        };
        ask(&mut client, r#"{"cmd":"setup","mode":{"how":"generate"}}"#).to_string();

        // The client's URI names only the client's relay.
        let app = Keys::generate();
        let relay_encoded = client_relay.url.replace(':', "%3A").replace('/', "%2F");
        let uri = format!(
            "nostrconnect://{}?relay={}&secret=the-client-secret&name=Road",
            app.public_key(),
            relay_encoded
        );
        let answer = ask(&mut client, &format!(r#"{{"cmd":"connect","uri":"{uri}"}}"#));
        assert!(answer.contains("\"ok\":true"), "{answer}");

        // The handshake crossed the client's relay and nothing else's.
        wait_for("the handshake to reach the client's relay", 100, || {
            client_relay.received().iter().any(|frame| frame.contains(":24133"))
        });
        assert!(
            !own_relay.received().iter().any(|frame| frame.contains(":24133")),
            "the handshake is the client's road's business, not the declared set's"
        );
        let frame =
            client_relay.received().into_iter().find(|frame| frame.contains(":24133")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let handshake: Event = serde_json::from_value(parsed[1].clone()).unwrap();
        let bunker_pubkey = handshake.pubkey;
        let plaintext = app.nip44_decrypt(&bunker_pubkey, &handshake.content).unwrap();
        match NostrConnectMessage::from_json(&plaintext).unwrap() {
            NostrConnectMessage::Response { result, .. } => {
                assert_eq!(result.as_deref(), Some("the-client-secret"));
            }
            other => panic!("the handshake is a response: {other:?}"),
        }

        // The client's request crosses its own relay, and the answer
        // comes back down BOTH roads: the declared set and the
        // client's.
        let message = NostrConnectMessage::request(
            &NostrConnectRequest::from_message(
                nostr::nips::nip46::NostrConnectMethod::GetPublicKey,
                vec![],
            )
            .unwrap(),
        );
        let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app)
            .unwrap();
        client_relay.inject(&request);

        wait_for("the answer on the client's relay", 100, || {
            client_relay
                .received()
                .iter()
                .any(|frame| frame.contains(":24133") && !frame.contains(&handshake.id.to_string()))
        });
        wait_for("the answer on the declared set's relay", 100, || {
            own_relay.received().iter().any(|frame| frame.contains(":24133"))
        });

        // The revoke tears the client's roads down: the threads stop,
        // and a request that crosses the client's relay afterwards is
        // never read at all.
        let apps: serde_json::Value =
            serde_json::from_str(ask(&mut client, r#"{"cmd":"apps"}"#).trim()).unwrap();
        let app_hex = apps["apps"][0]["pubkey"].as_str().unwrap().to_string();
        assert!(ask(&mut client, &format!(r#"{{"cmd":"revoke","app":"{app_hex}"}}"#))
            .contains("\"ok\":true"));
        wait_for("the client's roads to tear down", 100, || {
            daemon.lock().unwrap().app_road_count(&app.public_key()) == 0
        });
    }

    /// #82's win, end to end: two apps' asks pending at the same
    /// moment — each parked on its own person — while the daemon's
    /// lock stays free enough for the socket verbs to answer, and
    /// each approval publishes its own answer to its own app, in the
    /// order the person chose, not the order the wire chose. The old
    /// single worker could not have this: one ask parked the planner,
    /// the second app's request never became a prompt, and at the
    /// bound it shed instead.
    #[test]
    fn two_apps_asks_pend_at_once_and_each_answer_carries_its_own_id() {
        use crate::nostr::test_relay::{wait_for, StubRelay};
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod};

        let stub = StubRelay::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);
        let listener = bind(&path).unwrap();
        let (daemon, inbound_rx) =
            Daemon::new(Vault::new(MemoryStore::default()), vec![stub.url.clone()], None);
        let engine = daemon.engine();
        let daemon = Arc::new(Mutex::new(daemon));
        // The worker, in the shape nostrd runs: plan only. Asks ride
        // the production road.
        std::thread::spawn({
            let daemon = daemon.clone();
            move || loop {
                let Ok(event) = inbound_rx.recv() else {
                    return;
                };
                let plan = { daemon.lock().unwrap().plan_bunker_event(&event) };
                let Some(plan) = plan else { continue };
                match plan {
                    crate::nostr::bunker::Plan::Ignore
                    | crate::nostr::bunker::Plan::Paired { .. }
                    | crate::nostr::bunker::Plan::RelaysServed { .. }
                    | crate::nostr::bunker::Plan::Ended { .. }
                    | crate::nostr::bunker::Plan::Shed { .. } => continue,
                    crate::nostr::bunker::Plan::Answer(answer) => {
                        let _ = daemon.lock().unwrap().publish(&answer);
                    }
                    crate::nostr::bunker::Plan::Ask { .. } => {
                        Daemon::spawn_ask(&daemon, engine.clone(), plan);
                    }
                }
            }
        });
        std::thread::spawn({
            let daemon = daemon.clone();
            move || serve(listener, daemon)
        });

        let mut client = UnixStream::connect(&path).unwrap();
        let ask = |mut client: &UnixStream, line: &str| -> String {
            client.write_all(line.as_bytes()).unwrap();
            client.write_all(b"\n").unwrap();
            let mut answer = String::new();
            BufReader::new(client.try_clone().unwrap()).read_line(&mut answer).unwrap();
            answer
        };
        ask(&mut client, r#"{"cmd":"setup","mode":{"how":"generate"}}"#);

        // Two apps pair by paste, the road a person runs twice.
        let relay_encoded = stub.url.replace(':', "%3A").replace('/', "%2F");
        let mut apps = Vec::new();
        for (name, id) in [("PasterA", "ask-a"), ("PasterB", "ask-b")] {
            let app = Keys::generate();
            let uri = format!(
                "nostrconnect://{}?relay={}&secret={name}-secret&name={name}",
                app.public_key(),
                relay_encoded
            );
            let paired = ask(&mut client, &format!(r#"{{"cmd":"connect","uri":"{uri}"}}"#));
            assert!(paired.contains("\"ok\":true"), "{name} paired: {paired}");
            apps.push((app, id));
        }

        // Both apps ask at once — injected back to back, before any
        // person has answered either. The bunker's identity is the
        // handshake's author, the way the client learns it.
        wait_for("the two handshakes to go out", 100, || {
            stub.received().iter().filter(|f| f.contains(":24133")).count() >= 2
        });
        let frames: Vec<String> =
            stub.received().into_iter().filter(|f| f.contains(":24133")).collect();
        let parsed: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
        let handshake: Event = serde_json::from_value(parsed[1].clone()).unwrap();
        let bunker_pubkey = handshake.pubkey;
        for (app, id) in &apps {
            // The id is the test's own — the correlation the answer
            // must echo back.
            let message = NostrConnectMessage::Request {
                id: id.to_string(),
                method: NostrConnectMethod::GetPublicKey,
                params: vec![],
            };
            let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
            let request = EventBuilder::new(Kind::NostrConnect, content)
                .tag(Tag::public_key(bunker_pubkey))
                .finalize(app)
                .unwrap();
            stub.inject(&request);
        }

        // BOTH prompts exist at the same moment — the planner never
        // parked on either person. The socket verbs answer while the
        // asks wait, which is the lock story the fix promises.
        wait_for("both prompts to exist at once", 100, || {
            let prompts: serde_json::Value =
                serde_json::from_str(ask(&mut client, r#"{"cmd":"prompts"}"#).trim()).unwrap();
            prompts["prompts"].as_array().map(|a| a.len()) == Some(2)
        });

        // The person answers in the REVERSE order the wire brought:
        // B first, then A. Each prompt's card names its app, so the
        // approval aims by id, not by arrival.
        let prompts: serde_json::Value =
            serde_json::from_str(ask(&mut client, r#"{"cmd":"prompts"}"#).trim()).unwrap();
        let id_for = |app_hex: &str| -> String {
            prompts["prompts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["app"].as_str() == Some(app_hex))
                .expect("each app has its own card")["id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        for (app, _) in apps.iter().rev() {
            let prompt_id = id_for(&app.public_key().to_string());
            let approved = ask(
                &mut client,
                &format!(r#"{{"cmd":"approve","id":"{prompt_id}","remember_hours":null}}"#),
            );
            assert!(approved.contains("\"ok\":true"), "{approved}");
        }

        // Two answers on the wire, and the correlation is each app's
        // own: A's id to A's key, B's id to B's key — encrypted to the
        // one app, so only its key opens it.
        wait_for("both answers to come back", 100, || {
            let frames: Vec<String> =
                stub.received().into_iter().filter(|f| f.contains(":24133")).collect();
            frames.len() >= 4 // two handshakes + two answers
        });
        for (app, id) in &apps {
            let frame = stub
                .received()
                .into_iter()
                .filter(|f| f.contains(":24133"))
                .find_map(|f| {
                    let parsed: serde_json::Value = serde_json::from_str(&f).unwrap();
                    let event: Event = serde_json::from_value(parsed[1].clone()).ok()?;
                    let plaintext = app.nip44_decrypt(&event.pubkey, &event.content).ok()?;
                    match NostrConnectMessage::from_json(&plaintext).ok()? {
                        NostrConnectMessage::Response { id: rid, result, .. }
                            if result.as_deref() == Some(&bunker_pubkey.to_string()) =>
                        {
                            Some(rid)
                        }
                        _ => None,
                    }
                })
                .unwrap_or_else(|| panic!("an answer came back encrypted to {id}"));
            assert_eq!(&frame, id, "the answer is the app's own ask, by id");
        }
    }

    /// The layer's crown test, and the reason the stub relay exists:
    /// a daemon serving its unix socket, armed by the socket verb
    /// itself, whose bunker answers an app's request across a real
    /// relay socket and gets the answer back to the app's own key.
    /// Every hop is real; only the keyring is a stand-in.
    #[test]
    fn a_request_reaches_the_bunker_and_its_answer_comes_back() {
        use crate::nostr::test_relay::StubRelay;
        use nostr::nips::nip46::{NostrConnectMessage, NostrConnectMethod, NostrConnectRequest};

        // The relay the daemon will be pointed at.
        let stub = StubRelay::start(Vec::new());

        // The daemon, its socket, its worker — the shape nostrd runs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_NAME);
        let listener = bind(&path).unwrap();
        let (daemon, inbound_rx) =
            Daemon::new(Vault::new(MemoryStore::default()), vec![stub.url.clone()], None);
        let engine = daemon.engine();
        let daemon = Arc::new(Mutex::new(daemon));
        // The worker, in the shape nostrd runs: plan under the lock,
        // in queue order — and nothing else. Asks hand their waits to
        // per-task threads (spawn_ask), pairing burns theirs to the
        // completion task, and answers publish straight off the
        // worker. Answers and asks both end at the relay.
        std::thread::spawn({
            let daemon = daemon.clone();
            move || loop {
                let Ok(event) = inbound_rx.recv() else {
                    return;
                };
                let plan = { daemon.lock().unwrap().plan_bunker_event(&event) };
                let Some(plan) = plan else { continue };
                match plan {
                    crate::nostr::bunker::Plan::Ignore => continue,
                    crate::nostr::bunker::Plan::Paired { answer, app, metadata, burned, perms } => {
                        let name = burned
                            .as_ref()
                            .and_then(|o| o.label.clone())
                            .or_else(|| metadata.as_ref().and_then(|m| m.name.clone()));
                        let image = metadata.as_ref().and_then(|m| m.image.clone());
                        engine.pair_with_metadata(
                            &app,
                            name,
                            image,
                            perms,
                            metadata.as_ref().and_then(|m| m.url.clone()),
                        );
                        match burned {
                            Some(burned) => {
                                Daemon::spawn_paired_completion(&daemon, answer, burned);
                            }
                            None => {
                                let _ = daemon.lock().unwrap().publish(&answer);
                            }
                        }
                    }
                    crate::nostr::bunker::Plan::Answer(answer) => {
                        let _ = daemon.lock().unwrap().publish(&answer);
                    }
                    crate::nostr::bunker::Plan::RelaysServed { answer, app } => {
                        engine.noted(
                            &app.to_string(),
                            "switch_relays",
                            "the bunker's relay list".into(),
                            "served",
                        );
                        let _ = daemon.lock().unwrap().publish(&answer);
                    }
                    crate::nostr::bunker::Plan::Shed { app } => {
                        engine.noted(
                            &app.to_string(),
                            "rate_limit",
                            "over its rate".into(),
                            "shed",
                        );
                    }
                    crate::nostr::bunker::Plan::Ended { answer, app } => {
                        engine.logout(&app.to_string());
                        let _ = daemon.lock().unwrap().publish(&answer);
                    }
                    crate::nostr::bunker::Plan::Ask { .. } => {
                        Daemon::spawn_ask(&daemon, engine.clone(), plan);
                    }
                }
            }
        });
        std::thread::spawn({
            let daemon = daemon.clone();
            move || serve(listener, daemon)
        });

        // The socket client arms the bunker by setting it up.
        let mut client = UnixStream::connect(&path).unwrap();
        let ask = |mut client: &UnixStream, line: &str| -> String {
            client.write_all(line.as_bytes()).unwrap();
            client.write_all(b"\n").unwrap();
            let mut answer = String::new();
            BufReader::new(client.try_clone().unwrap()).read_line(&mut answer).unwrap();
            answer
        };
        let setup: serde_json::Value = serde_json::from_str(
            ask(&mut client, r#"{"cmd":"setup","mode":{"how":"generate"}}"#).trim(),
        )
        .unwrap();
        let npub = setup["pubkey"].as_str().expect("setup answers an npub").to_string();

        // The relay saw the subscription.
        wait_for("the bunker's subscribe to reach the relay", 100, || {
            stub.received().iter().any(|frame| frame.contains("REQ"))
        });

        // An app — with its own keys — asks to connect. It read the
        // pairing URI, so the connect echoes the nonce the URI carries:
        // the door a scraped pubkey does not open.
        let status: serde_json::Value =
            serde_json::from_str(ask(&mut client, r#"{"cmd":"status"}"#).trim()).unwrap();
        let uri = status["vault"]["uri"].as_str().expect("the armed bunker carries a pairing uri");
        let secret = uri.split("secret=").nth(1).expect("the uri carries the nonce").to_string();
        let bunker_pubkey = nostr::key::PublicKey::parse(&npub).unwrap();
        let app = Keys::generate();
        let message = NostrConnectMessage::request(
            &NostrConnectRequest::from_message(
                NostrConnectMethod::Connect,
                // Connect's params lead with the pubkey the app
                // expects to control — the bunker's own — and carry
                // the nonce's echo behind it.
                vec![bunker_pubkey.to_string(), secret],
            )
            .unwrap(),
        );
        let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app)
            .unwrap();
        stub.inject(&request);

        // The bunker's answer comes back through the relay, and the
        // app's own half of the channel opens it.
        wait_for("the bunker's answer to come back through the relay", 100, || {
            stub.received().iter().any(|frame| frame.contains(":24133"))
        });
        let answer_frame =
            stub.received().into_iter().find(|frame| frame.contains(":24133")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&answer_frame).unwrap();
        let answer: Event = serde_json::from_value(parsed[1].clone()).unwrap();
        assert_eq!(answer.kind, Kind::from_u16(24133));
        let plaintext = app.nip44_decrypt(&answer.pubkey, &answer.content).unwrap();
        let message = NostrConnectMessage::from_json(&plaintext).unwrap();
        assert!(
            message.is_response(),
            "the app's connect request was answered with a response: {message:?}"
        );

        // Now the policy path: the same app asks for its public key —
        // consequential, so the engine asks — and the worker waits
        // with the daemon's lock free, which is what lets the answer
        // arrive through the very socket that asked.
        let message = NostrConnectMessage::request(
            &NostrConnectRequest::from_message(
                NostrConnectMethod::GetPublicKey,
                vec![bunker_pubkey.to_string()],
            )
            .unwrap(),
        );
        let content = app.nip44_encrypt(&bunker_pubkey, &message.as_json()).unwrap();
        let ask_request = EventBuilder::new(Kind::NostrConnect, content)
            .tag(Tag::public_key(bunker_pubkey))
            .finalize(&app)
            .unwrap();
        stub.inject(&ask_request);

        // The ask queues as a prompt, visible from a fresh client.
        wait_for("the prompt to appear", 100, || {
            let answer = ask(&mut client, r#"{"cmd":"prompts"}"#);
            answer.contains("GetPublicKey")
        });

        // Approve it — the answer travels through the socket verb, and
        // the waiting worker wakes with the lock free to receive it.
        let prompts: serde_json::Value =
            serde_json::from_str(ask(&mut client, r#"{"cmd":"prompts"}"#).trim()).unwrap();
        let prompt_id = prompts["prompts"][0]["id"].as_str().unwrap().to_string();
        let approved = ask(
            &mut client,
            &format!(r#"{{"cmd":"approve","id":"{prompt_id}","remember_hours":null}}"#),
        );
        assert!(approved.contains("\"ok\":true"), "{approved}");

        // The answer crossed back through the relay, and it names the
        // bunker identity — the get_public_key the gate allowed.
        wait_for("the allowed answer to come back through the relay", 100, || {
            let answers: Vec<String> =
                stub.received().into_iter().filter(|frame| frame.contains(":24133")).collect();
            answers.len() >= 2
        });
        let answer_frame =
            stub.received().into_iter().rfind(|frame| frame.contains(":24133")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&answer_frame).unwrap();
        let answer: Event = serde_json::from_value(parsed[1].clone()).unwrap();
        let plaintext = app.nip44_decrypt(&answer.pubkey, &answer.content).unwrap();
        match NostrConnectMessage::from_json(&plaintext).unwrap() {
            NostrConnectMessage::Response { result, error, .. } => {
                assert_eq!(error, None);
                assert_eq!(result.as_deref(), Some(bunker_pubkey.to_string().as_str()));
            }
            other => panic!("a response came back: {other:?}"),
        }
    }
}
