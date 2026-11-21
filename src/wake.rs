//! The wakeup pipe: how a sleeping loop learns the world changed.
//!
//! The nostr layer's loops used to multiplex "frames to send" against
//! "frames arriving" with a one-second read timeout — a heartbeat spin
//! that woke the CPU once per second per connection, every second of
//! uptime, on a machine where the entire timer fleet costs ~28 wakes a
//! day. The resource audit priced it: a quarter-million CPU wakes a
//! day on an idle, nostr-armed desktop, doing nothing. The poll-based
//! loops these helpers serve instead sleep in `poll(2)` until the
//! socket, this pipe, or the next liveness deadline wakes them: an
//! enqueue wakes instantly, idle wakes only at the ping deadline, and
//! nothing spins.

use std::io;
use std::io::{Read as _, Write as _};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

/// The poll side of a wakeup pipe: the read end a loop sleeps on.
pub struct WakeSource {
    read: UnixStream,
}

/// The producer side: every enqueuer holds a clone and calls `wake`
/// after each successful enqueue. The UnixStream's write half is what
/// clones — a pipe end, cheap to share.
#[derive(Clone)]
pub struct WakeSink {
    write: Arc<UnixStream>,
}

/// A pipe for one loop: the source is polled, the sink is written.
pub fn wakeup_pair() -> io::Result<(WakeSource, WakeSink)> {
    let (read, write) = UnixStream::pair()?;
    read.set_nonblocking(true)?;
    write.set_nonblocking(true)?;
    Ok((WakeSource { read }, WakeSink { write: Arc::new(write) }))
}

impl WakeSource {
    /// The read end's fd, for the poll set.
    pub fn fd(&self) -> i32 {
        self.read.as_raw_fd()
    }

    /// Empty the pipe. Call after draining the queue, never before:
    /// every queued message is followed by a byte, so a byte drained
    /// before its message would leave the message unwoken-but-read
    /// and a stale byte in the pipe. Drained queue-first, the
    /// invariant is one-directional and a stale byte costs one
    /// spurious wake, never a spin.
    pub fn drain(&self) {
        let mut buf = [0u8; 4096];
        // The shared-reference Read impl: a pipe read on &self is the
        // nonblocking read this type was configured for at creation.
        loop {
            match (&self.read).read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}

impl WakeSink {
    /// Best effort by design: an error means the reader is gone, and
    /// a loop that exited is not waiting to be woken. std ignores
    /// SIGPIPE, so a dead reader surfaces as EPIPE here, never as a
    /// signal.
    pub fn wake(&self) {
        let _ = (&*self.write).write_all(b"x");
    }
}

/// poll(2) on a small fd set, retrying EINTR. Returns on timeout
/// (nothing readable within the budget) or readiness; the caller
/// inspects the individual `revents`.
pub fn poll_wait(fds: &mut [libc::pollfd], timeout_ms: i32) -> io::Result<()> {
    loop {
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(());
    }
}

/// Whether a poll set entry came back readable.
pub fn poll_readable(fd: &libc::pollfd) -> bool {
    fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0
}

/// The raw socket fd behind a WebSocket's stream, for the poll set.
/// TLS streams wrap the TcpStream; the plain arm is what every test
/// and every ws:// relay uses.
pub trait RawSocket {
    fn raw_fd(&self) -> Option<i32>;
    /// The stream's own nonblocking switch — poll-based reads need it,
    /// and TLS wrappers must forward it to the TcpStream they hold.
    fn set_raw_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
}

impl RawSocket for std::net::TcpStream {
    fn raw_fd(&self) -> Option<i32> {
        Some(self.as_raw_fd())
    }
    fn set_raw_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.set_nonblocking(nonblocking)
    }
}

impl RawSocket for tungstenite::stream::MaybeTlsStream<std::net::TcpStream> {
    fn raw_fd(&self) -> Option<i32> {
        match self {
            tungstenite::stream::MaybeTlsStream::Plain(stream) => stream.raw_fd(),
            tungstenite::stream::MaybeTlsStream::Rustls(stream) => stream.sock.raw_fd(),
            other => {
                let _ = other;
                None
            }
        }
    }
    fn set_raw_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match self {
            tungstenite::stream::MaybeTlsStream::Plain(stream) => {
                stream.set_raw_nonblocking(nonblocking)
            }
            tungstenite::stream::MaybeTlsStream::Rustls(stream) => {
                stream.sock.set_raw_nonblocking(nonblocking)
            }
            other => {
                let _ = other;
                Err(io::Error::new(io::ErrorKind::Unsupported, "unsupported relay transport"))
            }
        }
    }
}

/// Send one WebSocket message the way the blocking loops used to: on
/// `WouldBlock` — a peer whose kernel send buffer is full — wait for
/// write-readiness and retry, up to `max_wait`. Without the wait, a
/// nonblocking send errors a connection that a blocking send would
/// have paced through; with an unbounded wait, a stalled peer parks
/// the loop forever. Ten seconds of a full send buffer is a dead peer
/// by any honest measure.
pub fn send_paced<S: std::io::Read + std::io::Write + RawSocket>(
    socket: &mut tungstenite::WebSocket<S>,
    message: tungstenite::Message,
    max_wait: std::time::Duration,
) -> tungstenite::Result<()> {
    let start = std::time::Instant::now();
    loop {
        match socket.send(message.clone()) {
            Ok(()) => return Ok(()),
            Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                if start.elapsed() >= max_wait {
                    return Err(tungstenite::Error::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the peer stopped accepting frames",
                    )));
                }
                let Some(fd) = socket.get_ref().raw_fd() else {
                    // No fd to wait on: fail the way a paced send's
                    // deadline would.
                    return Err(tungstenite::Error::Io(io::Error::other(
                        "unsupported relay transport",
                    )));
                };
                let mut pfd = [libc::pollfd { fd, events: libc::POLLOUT, revents: 0 }];
                let remaining =
                    max_wait.saturating_sub(start.elapsed()).as_millis().clamp(1, i32::MAX as u128)
                        as i32;
                poll_wait(&mut pfd, remaining)?;
                // Loop: retry the send either way — readiness is a
                // hint, the send is the verdict.
            }
            Err(e) => return Err(e),
        }
    }
}
