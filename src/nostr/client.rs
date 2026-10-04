//! The socket client: what the CLI, doctor, and the panel all are.
//!
//! One connection, one request line, one JSON answer — the same
//! newline-delimited protocol the daemon serves, from the other chair.
//! The client holds no state and reads no keys; every answer is the
//! daemon's verdict, which is what makes shelling this binary safe for
//! the panel and safe for doctor: the trust boundary is the socket,
//! and nobody here holds a key.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result};

/// Connect to the daemon at `path`, or at the session's default.
pub fn connect(path: Option<&Path>) -> Result<Client> {
    let resolved = match path {
        Some(p) => p.to_path_buf(),
        None => super::socket::default_socket_path()?,
    };
    let stream = UnixStream::connect(&resolved)
        .with_context(|| format!("the daemon is not answering on {}", resolved.display()))?;
    Ok(Client { stream })
}

pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// One request line in, one JSON document out. The document carries
    /// `ok` first; parsing it here means every caller gets the same
    /// shape and a protocol break is one error, not four.
    pub fn ask(&mut self, request: &str) -> Result<serde_json::Value> {
        self.stream.write_all(request.as_bytes())?;
        self.stream.write_all(b"\n")?;
        let mut answer = String::new();
        BufReader::new(self.stream.try_clone().context("cloning the socket")?)
            .read_line(&mut answer)?;
        if answer.is_empty() {
            anyhow::bail!("the daemon closed the connection without answering");
        }
        let value: serde_json::Value = serde_json::from_str(answer.trim())
            .context("the daemon's answer was not one JSON document")?;
        Ok(value)
    }

    /// The daemon's status document, or the error it answered with.
    pub fn status(&mut self) -> Result<serde_json::Value> {
        let value = self.ask(r#"{"cmd":"status"}"#)?;
        require_ok(&value)
    }

    /// Begin a nostrconnect:// pairing from the client's URI.
    pub fn connect(&mut self, uri: &str) -> Result<serde_json::Value> {
        let value = self.ask(&serde_json::json!({ "cmd": "connect", "uri": uri }).to_string())?;
        require_ok(&value)
    }

    /// The paired apps and their levels.
    pub fn apps(&mut self) -> Result<serde_json::Value> {
        let value = self.ask(r#"{"cmd":"apps"}"#)?;
        require_ok(&value)
    }
}

fn require_ok(value: &serde_json::Value) -> Result<serde_json::Value> {
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(value.clone())
    } else {
        anyhow::bail!(
            "{}",
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the daemon refused without saying why")
        );
    }
}
