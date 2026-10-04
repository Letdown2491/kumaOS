//! The offline relay harness: a stub that speaks the real protocol over
//! a real socket.
//!
//! The plan's relay harness has two phases — the Go nip46-relay
//! bootstraps interop, the Rust port becomes the fixture — but neither
//! belongs in the offline suite, and the pool and the daemon need
//! *something* to be tested against that behaves like a relay: accepts,
//! reads a subscribe, hands out scripted events, records what it is
//! published. This is that something. It lives behind `#[cfg(test)]`
//! and never ships.

use std::net::TcpListener;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nostr::prelude::*;
use tungstenite::Message;

/// One relay's worth of stub: an ephemeral `ws://` URL and the pile of
/// frames the client published.
pub struct StubRelay {
    pub url: String,
    received: Arc<Mutex<Vec<String>>>,
    inject: Sender<Event>,
}

impl StubRelay {
    /// Serve one connection on an ephemeral port: read the subscribe,
    /// send the scripted events in order, then collect what the client
    /// publishes until the socket dies. A relay holds the connection
    /// open — the would-block of its read timeout is a heartbeat, not
    /// a fault — because a stub that dies early sends an RST that can
    /// eat a frame in flight, and the failure that produces is a race
    /// nobody can debug.
    pub fn start(scripted: Vec<Event>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_for_thread = received.clone();
        let (inject, inject_rx) = channel::<Event>();
        let inject_rx = Arc::new(Mutex::new(inject_rx));
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let Ok(mut socket) = tungstenite::accept(stream) else {
                return;
            };
            // The subscribe arrives; its shape is what the pool's
            // filter test asserts through the parser directly.
            loop {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        received_for_thread.lock().unwrap().push(text.to_string());
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
            for event in scripted {
                let frame =
                    serde_json::json!(["EVENT", crate::nostr::pool::SUBSCRIPTION_ID, event])
                        .to_string();
                if socket.send(Message::text(frame)).is_err() {
                    return;
                }
            }
            socket.get_mut().set_read_timeout(Some(Duration::from_millis(100))).ok();
            loop {
                // Events injected after the connection came up go out
                // on the next beat — how a test puts a request in
                // flight once the bunker is listening.
                let injected: Vec<Event> = {
                    let rx = inject_rx.lock().unwrap();
                    std::iter::from_fn(|| rx.try_recv().ok()).collect()
                };
                for event in injected {
                    let frame =
                        serde_json::json!(["EVENT", crate::nostr::pool::SUBSCRIPTION_ID, event])
                            .to_string();
                    if socket.send(Message::text(frame)).is_err() {
                        return;
                    }
                }
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        received_for_thread.lock().unwrap().push(text.to_string());
                    }
                    Ok(Message::Close(_)) => return,
                    // WouldBlock is the read timeout beat, not a
                    // fault: a relay holds the connection open.
                    Err(tungstenite::Error::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        continue
                    }
                    Err(_) => return,
                    _ => continue,
                }
            }
        });
        Self { url, received, inject }
    }

    /// Everything the client published so far, frames and all.
    pub fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }

    /// Put an event in flight to the subscriber, after the connection
    /// is up.
    pub fn inject(&self, event: &Event) {
        self.inject.send(event.clone()).expect("the stub thread is alive");
    }
}

/// Wait until the closure is true, because a relay thread and a
/// subscriber meet in the middle: neither side knows who arrived first.
pub fn wait_for(description: &str, tries: u32, mut check: impl FnMut() -> bool) {
    for _ in 0..tries {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {description}");
}
