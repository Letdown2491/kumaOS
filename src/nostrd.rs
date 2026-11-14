//! `kuma-nostrd` — the nostr layer's daemon.
//!
//! A small executable whose surface an auditor can hold in their head:
//! it owns the vault, arms the bunker when the gate is open, and
//! answers the socket protocol. It runs as a user unit under the
//! graphical session (the hardened unit the declaration block bakes);
//! the relay set comes in as arguments until the declaration block
//! exists to carry it.
//!
//! The startup posture is the plan's gate made concrete: the keyring is
//! PAM-unlocked by the session, so the daemon auto-unlocks and comes up
//! answering — a reboot is invisible to a paired phone. The unlock rides
//! a short retry burst, because the session's first minute is when the
//! Secret Service is at its flakiest and one refused handshake should
//! not cost the whole session. A vault that will not open says so on
//! stderr and the daemon keeps running locked; `kuma-nostr status` and
//! the doctor both say so.

use std::sync::{Arc, Mutex};

use anyhow::Context;
use clap::Parser;
use kuma::nostr::protocol::Daemon;
use kuma::nostr::socket;
use kuma::nostr::vault::{KeyringStore, Vault};

#[derive(Parser)]
#[command(
    name = "kuma-nostrd",
    about = "The kumaOS nostr layer's daemon",
    version,
    verbatim_doc_comment
)]
struct Args {
    /// The socket to answer on; the default is
    /// `$XDG_RUNTIME_DIR/kuma-nostr.sock`.
    #[arg(long)]
    socket: Option<std::path::PathBuf>,
    /// A relay to talk to, as many times as the set needs. Until the
    /// declaration block carries the relay list, this argument is the
    /// only way one exists.
    #[arg(long = "relay")]
    relays: Vec<String>,
    /// The inactivity switch's window, in seconds: after this long
    /// with no unlock and no keep-alive, the daemon locks itself —
    /// the dead man's switch, whose act is the same lock verb the
    /// panel has. 0 or absent leaves the switch off, the desktop
    /// default: the keyring is PAM-open here, and a switch on by
    /// default would lock the bunker while the person is away. The
    /// floor is an hour — a fuse shorter than that trips on lunch.
    #[arg(long)]
    inactivity_lock_secs: Option<u64>,
}

fn main() -> anyhow::Result<()> {
    // The relay roads are wss:// in the real world, and rustls 0.23
    // refuses to pick a crypto provider when the dependency graph
    // carries two — ring here, aws-lc-rs through another door — so the
    // first TLS connect panicked and the pool's retries hit the same
    // wall: a bunker deaf on every wss relay it advertises. Installing
    // one by name is the whole fix; ring is already the tree's own.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("installing the crypto provider");

    let args = Args::parse();
    let window = match args.inactivity_lock_secs {
        None | Some(0) => None,
        Some(secs) if secs < 3600 => {
            anyhow::bail!("the inactivity window is {secs}s; the floor is one hour (3600)")
        }
        Some(secs) => Some(std::time::Duration::from_secs(secs)),
    };
    let socket_path = match &args.socket {
        Some(path) => path.clone(),
        None => socket::default_socket_path()?,
    };

    // The runtime is the startup's: the unlock below runs on it, on
    // this thread. Once serve() takes this thread the runtime parks
    // for good — which is why the worker builds its own (see the
    // spawn below), and each socket connection builds one of its own
    // (socket::serve): a keyring call bridged into a runtime another
    // thread owns would never have its D-Bus executor polled — the
    // daemon would hold its lock forever, listening and never
    // answering.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building the keyring runtime")?;

    // The state dir is the daemon's own: where pairings persist. The
    // path mirrors the unit's ReadWritePaths (%h/.local/state/kuma-nostr
    // — %h is HOME), and the unit's ExecStartPre creates it before the
    // sandbox mounts; the engine's own writes then land in a dir that
    // exists and is the one the sandbox allows.
    let state_dir = std::env::var("HOME")
        .ok()
        .map(|home| std::path::PathBuf::from(home).join(".local/state/kuma-nostr"));
    let (daemon, inbound_rx) =
        Daemon::new(Vault::new(KeyringStore), args.relays.clone(), state_dir);
    let mut daemon = daemon;
    daemon.with_inactivity(window);
    let engine = daemon.engine();

    // The startup posture: come up answering. The retry burst rides
    // over the session's first-minute races — the Secret Service
    // activating, handing off between the PAM-spawned daemon and a
    // DBus-activated one, or its known 51.x abort-on-OpenSession —
    // where a single attempt would strand the bunker locked for the
    // whole session. A vault that will not open is a locked daemon,
    // not a dead one.
    match runtime.block_on(daemon.startup_unlock_with_retries(3, std::time::Duration::from_secs(2)))
    {
        Ok(npub) => eprintln!("kuma-nostrd: vault unlocked, bunker live as {npub}"),
        Err(e) => {
            eprintln!("kuma-nostrd: starting locked: {e:#}");
        }
    }

    let daemon = Arc::new(Mutex::new(daemon));

    // The inactivity watchdog, when the switch is armed: it wakes on
    // the minute, asks the one question, and closes the switch the
    // same way the panel's lock verb does. Off, it is nothing at all.
    Daemon::spawn_inactivity_watchdog(&daemon, std::time::Duration::from_secs(60));

    // The bunker worker: relay-delivered events in, answers published
    // out. The worker is the planner — plan under the lock, in queue
    // order — and nothing else: asks and pairing burns hand their
    // waits to per-task threads, because a person's think-time and a
    // keyring's D-Bus round trip are nobody else's head of line. It
    // lives for the process; a lock just makes the bunker's answers
    // None until the next unlock.
    let worker = daemon.clone();
    std::thread::spawn(move || {
        // The worker is the sole planner: admission, the replay
        // gates, the rate buckets — everything that decides whether a
        // request counts — happens here, in queue order, under the
        // daemon lock. What it no longer does is wait: an ask's
        // decide/execute/publish and a pairing's burn ride per-task
        // threads with runtimes of their own (spawn_ask,
        // spawn_paired_completion), because a person's think-time and
        // a keyring's D-Bus round trip are nobody else's head of
        // line. That is what the old single-thread loop was: one app's
        // five-minute card parked every other app's every request
        // behind it, and at the inbound bound the shed dropped them
        // silently (#82).
        loop {
            let Ok(event) = inbound_rx.recv() else {
                return;
            };
            let plan = { worker.lock().expect("the daemon lock").plan_bunker_event(&event) };
            let Some(plan) = plan else { continue };
            match plan {
                kuma::nostr::bunker::Plan::Ignore => continue,
                kuma::nostr::bunker::Plan::Paired { answer, app, metadata, burned, perms } => {
                    // The name is the person's word first — a label
                    // minted onto the URI outranks the client's own
                    // metadata claim — and the client's self-report
                    // second. Neither is an authorization input; both
                    // are what the ask cards show. The url is stored,
                    // not derived here: the views derive the display
                    // name from it, so a real name arriving later
                    // still wins.
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
                            // The burn is a keyring write — D-Bus, and
                            // the 51.x storm's own stage. Off the
                            // worker it goes (#82's road): a first
                            // URI connect no longer parks the whole
                            // transport on a D-Bus round trip, and
                            // the ack publishes when the burn says
                            // its word, not before.
                            Daemon::spawn_paired_completion(&worker, answer, burned);
                        }
                        None => {
                            if let Err(e) = worker.lock().expect("the daemon lock").publish(&answer)
                            {
                                eprintln!("kuma-nostrd: the answer was not published: {e:#}");
                            }
                        }
                    }
                }
                kuma::nostr::bunker::Plan::Answer(answer) => {
                    if let Err(e) = worker.lock().expect("the daemon lock").publish(&answer) {
                        eprintln!("kuma-nostrd: the answer was not published: {e:#}");
                    }
                }
                kuma::nostr::bunker::Plan::RelaysServed { answer, app } => {
                    engine.noted(
                        &app.to_string(),
                        "switch_relays",
                        "the bunker's relay list".into(),
                        "served",
                    );
                    if let Err(e) = worker.lock().expect("the daemon lock").publish(&answer) {
                        eprintln!("kuma-nostrd: the answer was not published: {e:#}");
                    }
                }
                kuma::nostr::bunker::Plan::Shed { app } => {
                    engine.noted(&app.to_string(), "rate_limit", "over its rate".into(), "shed");
                }
                kuma::nostr::bunker::Plan::Ended { answer, app } => {
                    engine.logout(&app.to_string());
                    if let Err(e) = worker.lock().expect("the daemon lock").publish(&answer) {
                        eprintln!("kuma-nostrd: the answer was not published: {e:#}");
                    }
                }
                kuma::nostr::bunker::Plan::Ask { .. } => {
                    // The ask's whole life off the worker: decide (the
                    // wait on a person, up to the prompt TTL) parks a
                    // per-ask thread, not the transport — every other
                    // app's asks plan, decide, and answer while one
                    // card waits (#82). The worker keeps planning; the
                    // prompt cap and the per-sender buckets are the
                    // bounds, and both were already concurrency-safe.
                    Daemon::spawn_ask(&worker, engine.clone(), plan);
                }
            }
        }
    });

    eprintln!("kuma-nostrd: listening on {}", socket_path.display());
    let listener = socket::bind(&socket_path)?;
    socket::serve(listener, daemon);
    // serve() only returns on an accept failure, which is fatal here.
    Ok(())
}
