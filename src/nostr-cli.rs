//! `kuma-nostr` — the nostr layer's CLI.
//!
//! The human interface and the shell signer plugin's transport, in one
//! binary: every verb is one request line to the daemon's socket and one
//! answer rendered. The CLI reads no keys and holds no state — the vault
//! is the daemon's, and that separation is what lets the panel shell
//! this binary without widening the trust boundary.
//!
//! The verbs the layer has so far: `setup` (which asks rather than
//! guesses), `generate`, `import`, `export` (the backup that makes
//! `destroy` survivable on purpose), `unlock`, `lock`, `status`,
//! `destroy`. Pairing, prompts and the bunker ride the same socket.

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "kuma-nostr", about = "The kumaOS nostr layer's CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Talk to a socket somewhere other than the default, which is
    /// `$XDG_RUNTIME_DIR/kuma-nostr.sock`. Works before or after the
    /// subcommand.
    #[arg(long, global = true)]
    socket: Option<std::path::PathBuf>,
    /// Print the daemon's JSON document instead of the rendered line —
    /// the plugin's transport and the house convention for anything a
    /// program reads (docs/agents.md).
    #[arg(long, global = true)]
    json: bool,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Provision the vault. On a terminal this asks which road: a new
    /// key, or one you already hold. It never guesses — in a pipe it
    /// refuses and names the verbs that do the work.
    Setup,
    /// Create a new identity: a fresh key, generated and stored.
    Generate,
    /// Bring an existing key: an nsec, a hex secret key, a NIP-06
    /// mnemonic, or an `ncryptsec` with the passphrase it was wrapped
    /// in.
    ///
    /// The secret is read from stdin, never a flag: a secret on the
    /// command line lands in shell history and in `ps`, and neither
    /// forgets. A terminal is prompted with echo off; a pipe is read
    /// as one line, so a scripted setup stays scriptable. An
    /// `ncryptsec` is asked for its passphrase on a second prompt, or
    /// a second line on a pipe.
    Import,
    /// Re-read the key from the keyring.
    Unlock,
    /// Drop the key from memory; the stored vault stays.
    Lock,
    /// The keep-alive: reset the inactivity clock without unlocking.
    /// The panel sends this when the person is clearly present, so a
    /// switch with nobody near it is one that means it.
    Touch,
    /// Begin a pairing from a `nostrconnect://` URI — the client's
    /// own invite, the person's paste the approval. The handshake
    /// goes out on the client's relays, and the pairing lands in
    /// `apps` like any other.
    Connect {
        /// The URI, whole — scheme, client pubkey, relays, secret.
        #[arg(allow_hyphen_values = true, num_args = 1..)]
        uri: Vec<String>,
    },
    /// What the daemon holds: whether a vault exists and is unlocked.
    Status,
    /// The `bunker://` URI a remote app pairs with — as text, and as a
    /// QR when asked. The URI carries the bunker pubkey and the relay
    /// set the daemon is running. Naming the app here is the person's
    /// word at the door: the connect that burns this URI's secret
    /// pairs under the name.
    Bunker {
        /// Render a QR beside the URI line.
        #[arg(long)]
        qr: bool,
        /// The name the pairing records — what the ask cards show.
        #[arg(long = "for")]
        for_app: Option<String>,
    },
    /// Delete the vault. The key is unrecoverable afterwards.
    Destroy {
        /// Carry the flag; without it this is a dry run.
        #[arg(long)]
        yes: bool,
    },
    /// Wrap the vault's key under a passphrase you choose and write the
    /// `ncryptsec1` string to a file — the backup that makes `destroy`
    /// survivable on purpose. `import` reads the file back, here or on
    /// the next machine. The file is created 0600 and never
    /// overwritten: a backup clobbered by a second run is a backup you
    /// forgot you had lost.
    Export {
        /// Where the wrapped key lands. Required without `--json`, which
        /// answers the raw document and lets the caller do the writing.
        #[arg(long, required_unless_present = "json")]
        output: Option<std::path::PathBuf>,
    },
    /// The asks waiting on a person, newest last.
    Prompts,
    /// The activity log, oldest first: what was asked, by whom, and
    /// how it went. The last 500 entries, persisted across restarts.
    Log,
    /// Answer an ask with yes. `--remember 1` grants the same method a
    /// standing yes for an hour — the longest a remember can be.
    Approve {
        /// The ask's id, from `prompts`.
        id: String,
        /// Hours to remember, at most 1.
        #[arg(long)]
        remember: Option<u64>,
    },
    /// Answer an ask with no.
    Deny { id: String },
    /// The paired apps and their policy levels.
    Apps,
    /// Forget a paired app: it answers as unpaired from then on, and
    /// no URI in its hands pairs it again — the tombstone stays until
    /// `unrevoke`.
    Revoke { app: String },
    /// Clear a revocation's tombstone. The way back in is still a
    /// freshly minted URI (`kuma-nostr bunker`).
    Unrevoke { app: String },
    /// Remove a paired app outright: the record and its standing
    /// answers go, and a freshly minted URI pairs it again. Not the
    /// tombstone — that is `revoke`.
    Delete { app: String },
    /// Set a paired app's policy level: `ask`, `basic`, or `trust`.
    /// Trust is the indefinite approval — every method signs
    /// unattended — and is graded loudly by the doctor.
    Level {
        app: String,
        #[arg(default_value = "ask")]
        level: String,
    },
    /// Name a paired app: the person's word, which outranks the
    /// client's own metadata claim on every surface after it.
    Label { app: String, name: String },
    /// Mint a fresh pairing nonce and re-arm: every URI printed before
    /// this dies with it, so the apps holding stored copies need the
    /// new one.
    Rotate,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.socket.map(Ok).unwrap_or_else(kuma::nostr::socket::default_socket_path)?;

    let bunker_verb = matches!(cli.command, Command::Bunker { .. });
    let bunker_qr = matches!(cli.command, Command::Bunker { qr: true, .. });
    let export_path = match &cli.command {
        Command::Export { output } => output.as_deref(),
        _ => None,
    };

    // The identity verbs share one pre-check, because the worst order is
    // ask-then-refuse: the person pastes their secret key and only then
    // hears that the daemon refuses it. Status first; the refusal, if
    // there is one, costs nothing and names the verb that moves past it.
    let mut client = kuma::nostr::client::connect(Some(&path))?;
    let creates_identity =
        matches!(cli.command, Command::Setup | Command::Generate | Command::Import);
    if creates_identity {
        let status = client.status()?;
        if status["vault"]["exists"].as_bool() == Some(true) {
            anyhow::bail!(
                "a vault already exists; destroy it first (`kuma-nostr destroy --yes`) \
                 — replacing a key is spelled"
            );
        }
    }

    // The export reads the key, so its worst order is ask-then-refuse
    // too: a passphrase typed twice and only then a locked gate. Status
    // first, by the same logic as the identity verbs above.
    if matches!(cli.command, Command::Export { .. }) {
        let status = client.status()?;
        let vault = &status["vault"];
        if vault["exists"].as_bool() != Some(true) {
            anyhow::bail!("no vault; nothing to export (set one up with `kuma-nostr setup`)");
        }
        if vault["unlocked"].as_bool() != Some(true) {
            anyhow::bail!("the vault is locked; unlock it first (`kuma-nostr unlock`)");
        }
    }

    let request = match &cli.command {
        Command::Setup => {
            // The umbrella stops being the decision. On a terminal it
            // asks; in a pipe there is nobody to ask, and guessing
            // "generate" would mint identities nobody chose — so it
            // refuses and names the two verbs that do the work.
            match ask_road()? {
                Road::Generate => r#"{"cmd":"setup","mode":{"how":"generate"}}"#.to_string(),
                Road::Import => import_request()?,
            }
        }
        Command::Generate => r#"{"cmd":"setup","mode":{"how":"generate"}}"#.to_string(),
        Command::Import => import_request()?,
        Command::Unlock => r#"{"cmd":"unlock"}"#.to_string(),
        Command::Lock => r#"{"cmd":"lock"}"#.to_string(),
        Command::Touch => r#"{"cmd":"touch"}"#.to_string(),
        Command::Connect { uri } => {
            // A pasted URI is one argument in shells that keep their
            // spaces and several in shells that do not — rejoin
            // without guessing which.
            let uri = uri.join(" ");
            serde_json::json!({ "cmd": "connect", "uri": uri }).to_string()
        }
        Command::Status => r#"{"cmd":"status"}"#.to_string(),
        Command::Bunker { for_app, .. } => {
            let label = for_app
                .as_deref()
                .map(|name| format!(r#","label":{}"#, json_string(name)))
                .unwrap_or_default();
            format!(r#"{{"cmd":"mint"{label}}}"#)
        }
        Command::Destroy { yes } => format!(r#"{{"cmd":"destroy","confirm":{yes}}}"#),
        Command::Export { .. } => {
            let passphrase = read_new_passphrase()?;
            format!(r#"{{"cmd":"export","passphrase":{}}}"#, json_string(&passphrase))
        }
        Command::Prompts => r#"{"cmd":"prompts"}"#.to_string(),
        Command::Log => r#"{"cmd":"log"}"#.to_string(),
        Command::Approve { id, remember } => {
            format!(
                r#"{{"cmd":"approve","id":{},"remember_hours":{}}}"#,
                json_string(id),
                remember.map_or("null".into(), |h| h.to_string())
            )
        }
        Command::Deny { id } => format!(r#"{{"cmd":"deny","id":{}}}"#, json_string(id)),
        Command::Apps => r#"{"cmd":"apps"}"#.to_string(),
        Command::Revoke { app } => format!(r#"{{"cmd":"revoke","app":{}}}"#, json_string(app)),
        Command::Unrevoke { app } => {
            format!(r#"{{"cmd":"unrevoke","app":{}}}"#, json_string(app))
        }
        Command::Delete { app } => format!(r#"{{"cmd":"delete","app":{}}}"#, json_string(app)),
        Command::Level { app, level } => format!(
            r#"{{"cmd":"level","app":{},"level":{}}}"#,
            json_string(app),
            json_string(level)
        ),
        Command::Label { app, name } => {
            format!(r#"{{"cmd":"label","app":{},"name":{}}}"#, json_string(app), json_string(name))
        }
        Command::Rotate => r#"{"cmd":"rotate"}"#.to_string(),
    };

    let value = client.ask(&request)?;

    if bunker_verb {
        // `--json` is the document, like every verb's `--json` — the
        // panel reads `uri` out of it. The render below is the
        // person-facing one: URI, and the QR beside it when asked.
        if cli.json {
            println!("{value}");
            return Ok(());
        }
        return bunker_verb_render(&value, bunker_qr);
    }

    if cli.json {
        println!("{value}");
        return Ok(());
    }

    if let Some(path) = export_path {
        return export_write(&value, path);
    }

    render(&value)
}

/// The one place the CLI formats a secret into a request line. Values
/// are JSON strings through and through — never pasted, never echoed.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string serializes")
}

/// The secret's one road in: stdin. A terminal prompts with echo off
/// (rpassword); a pipe is read as one line, so the setup a script runs
/// is the same setup a person runs. The trimmed value is what the
/// parsers accept — a trailing newline is an editor's, not the key's.
fn read_secret() -> Result<String> {
    use std::io::IsTerminal;
    let secret = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("paste the nsec, hex key, or mnemonic: ")?
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    };
    let trimmed = secret.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("no secret arrived on stdin");
    }
    Ok(trimmed)
}

/// The ncryptsec's other half, by the same road as the secret: a
/// terminal prompts with echo off, a pipe is read as one line.
fn read_passphrase() -> Result<String> {
    use std::io::IsTerminal;
    let passphrase = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("the passphrase the ncryptsec was wrapped with: ")?
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    };
    let trimmed = passphrase.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("an ncryptsec needs the passphrase it was wrapped with");
    }
    Ok(trimmed)
}

/// The export's passphrase is chosen, not pasted: a terminal asks twice
/// and refuses a mismatch; a pipe reads one line and trusts the
/// script's own care. The daemon refuses weak ones either way — the
/// file is a nostr identity in transit, and the bar is the vault
/// mode's own.
fn read_new_passphrase() -> Result<String> {
    use std::io::IsTerminal;
    let first = if std::io::stdin().is_terminal() {
        let a = rpassword::prompt_password(
            "a passphrase for the exported file (16+ characters, letters and digits): ",
        )?;
        let b = rpassword::prompt_password("again: ")?;
        if a.trim() != b.trim() {
            anyhow::bail!("the two passphrases do not match");
        }
        a
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line
    };
    let trimmed = first.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("no passphrase arrived on stdin");
    }
    Ok(trimmed)
}

/// The two roads `setup` offers a person at a terminal. Enter alone is
/// the default road — most setups have no key to bring.
fn ask_road() -> Result<Road> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "say which: `kuma-nostr generate` mints a new identity, \
             `kuma-nostr import` brings one you already hold"
        );
    }
    print!("generate a new identity, or import one you already hold? [G/i] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    match line.trim().to_ascii_lowercase().as_str() {
        "" | "g" | "generate" => Ok(Road::Generate),
        "i" | "import" => Ok(Road::Import),
        other => anyhow::bail!("that is not one of the two roads: {other:?}"),
    }
}

enum Road {
    Generate,
    Import,
}

/// The import request line, shared by `import` and the `setup` umbrella.
/// The secret's one road is stdin; an `ncryptsec` takes a second.
fn import_request() -> Result<String> {
    let secret = read_secret()?;
    let mut mode = format!(r#"{{"how":"import","secret":{}}}"#, json_string(&secret));
    if secret.starts_with("ncryptsec1") {
        mode.push_str(&format!(r#","passphrase":{}"#, json_string(&read_passphrase()?)));
    }
    Ok(format!(r#"{{"cmd":"setup","mode":{mode}}}"#))
}

fn render(value: &serde_json::Value) -> Result<()> {
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        let error = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the daemon refused without saying why");
        anyhow::bail!("{error}");
    }
    match value.get("verb").and_then(serde_json::Value::as_str) {
        Some("ping") => println!("daemon is answering"),
        Some("status") => {
            let vault = &value["vault"];
            let exists = vault["exists"].as_bool().unwrap_or(false);
            let unlocked = vault["unlocked"].as_bool().unwrap_or(false);
            let pubkey = vault["pubkey"].as_str();
            match (exists, unlocked, pubkey) {
                (false, _, _) => println!("no vault; run `kuma-nostr setup`"),
                (true, false, Some(npub)) => println!("vault exists, locked, identity {npub}"),
                (true, false, None) => println!("vault exists, locked"),
                (true, true, Some(npub)) => println!("vault exists, unlocked as {npub}"),
                (true, true, None) => println!("vault exists, unlocked"),
            }
            if let Some(switch) = vault["inactivity"].as_object() {
                println!(
                    "inactivity lock: armed, {}s remaining of {}s",
                    switch["remaining_secs"].as_u64().unwrap_or(0),
                    switch["window_secs"].as_u64().unwrap_or(0)
                );
            }
        }
        Some("setup") => println!(
            "vault created, unlocked as {}",
            value["pubkey"].as_str().unwrap_or("(npub unreadable)")
        ),
        Some("unlock") => {
            println!("unlocked as {}", value["pubkey"].as_str().unwrap_or("(npub unreadable)"))
        }
        Some("lock") => println!("locked"),
        Some("touch") => println!("kept alive"),
        Some("connect") => println!(
            "the handshake went out{}; the pairing lands when the client answers",
            value["name"].as_str().map(|n| format!(" to {n}")).unwrap_or_default()
        ),
        Some("destroy_dry_run") => println!(
            "dry run: {}",
            value["would"].as_str().unwrap_or("this would delete the vault")
        ),
        Some("destroy") => println!("vault destroyed"),
        Some("prompts") => {
            let prompts = value["prompts"].as_array().cloned().unwrap_or_default();
            if prompts.is_empty() {
                println!("nothing is waiting on you");
            }
            for prompt in prompts {
                println!(
                    "{}  {}  {}  {}",
                    prompt["id"].as_str().unwrap_or("?"),
                    prompt["app"].as_str().unwrap_or("?"),
                    prompt["method"].as_str().unwrap_or("?"),
                    prompt["summary"].as_str().unwrap_or("")
                );
                if let Some(detail) = prompt["detail"].as_str() {
                    println!("    {detail}");
                }
            }
        }
        Some("log") => {
            let log = value["log"].as_array().cloned().unwrap_or_default();
            if log.is_empty() {
                println!("the log is empty");
            }
            for entry in log {
                println!(
                    "{}  {}  {}  {}",
                    entry["at"].as_u64().unwrap_or(0),
                    entry["app"].as_str().unwrap_or("?").get(..16).unwrap_or("?").to_string() + "…",
                    entry["summary"].as_str().unwrap_or("?"),
                    entry["verdict"].as_str().unwrap_or("?")
                );
            }
        }
        Some("approve") => println!("approved"),
        Some("deny") => println!("denied"),
        Some("apps") => {
            let apps = value["apps"].as_array().cloned().unwrap_or_default();
            if apps.is_empty() {
                println!("no apps paired");
            }
            for app in apps {
                // The claim order: the name, then the name derived from
                // the url, then the fragment.
                let label = match app["name"].as_str() {
                    Some(name) => name.to_string(),
                    None => {
                        match app["url"].as_str().and_then(kuma::nostr::bunker::name_from_url) {
                            Some(derived) => derived,
                            None => format!("{}…", &app["pubkey"].as_str().unwrap_or("?")[..16]),
                        }
                    }
                };
                println!(
                    "{}  {:?}  paired at {}",
                    label,
                    app["level"],
                    app["paired_at"].as_u64().unwrap_or(0),
                );
            }
        }
        Some("revoke") => {
            if value["removed"].as_bool() == Some(true) {
                println!("revoked");
            } else {
                println!("no such app");
            }
        }
        Some("unrevoke") => {
            if value["cleared"].as_bool() == Some(true) {
                println!("un-revoked; mint a fresh URI to let the app back in");
            } else {
                println!("no revoked app by that pubkey");
            }
        }
        Some("delete") => {
            if value["removed"].as_bool() == Some(true) {
                println!("deleted");
            } else {
                println!("no such app");
            }
        }
        Some("level") => println!("level set"),
        Some("label") => {
            if value["named"].as_bool() == Some(true) {
                println!("labeled");
            } else {
                println!("no such app");
            }
        }
        Some("rotate") => println!(
            "new pairing URI:\n{}",
            value["uri"].as_str().unwrap_or("(the uri did not come back)")
        ),
        _ => println!("{value}"),
    }
    Ok(())
}

/// The export's landing: the daemon's `ncryptsec1` string, written 0600
/// at creation and again after — the ssh key's lesson, that a mode
/// promised at write is a mode you check at the use site — and never
/// over an existing file. The person deletes the old backup on purpose
/// or picks a new name; a silent overwrite is how a good backup
/// becomes a missing one. The confirmation names the file and the road
/// back in.
fn export_write(value: &serde_json::Value, path: &std::path::Path) -> Result<()> {
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!(
            "the daemon refused: {}",
            value["error"].as_str().unwrap_or("no reason given")
        );
    }
    let ncryptsec = value["ncryptsec"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("the daemon answered without the wrapped key"))?;
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(ncryptsec.as_bytes())?;
        file.write_all(b"\n")?;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    println!("exported to {}; import it back with `kuma-nostr import`", path.display());
    Ok(())
}

/// The `bunker` verb: the pairing URI a phone's nostr app scans. The
/// URI is the copyable answer; the QR is the scannable one — both carry
/// the bunker pubkey and the relay set, because a QR that only renders
/// when the URI is not also printed is a URI nobody can paste into a
/// support question. The `--json` document is main()'s, before this
/// render is reached.
fn bunker_verb_render(value: &serde_json::Value, qr: bool) -> Result<()> {
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!(
            "the daemon refused: {}",
            value["error"].as_str().unwrap_or("no reason given")
        );
    }
    // The mint verb refused, or the URI is the answer.
    let uri = value["uri"].as_str().ok_or_else(|| {
        anyhow::anyhow!("the daemon has no pairing URI yet; run `kuma-nostr setup`")
    })?;

    if qr {
        // One quiet-zone module on each side is the minimum a scanner
        // wants; the debug render is the matrix alone, so the padding
        // is printed here.
        let code = qrencode::QrCode::new(uri.as_bytes())?;
        println!();
        println!("{}", " ".repeat(code.width() + 8));
        for line in code.to_debug_str('#', ' ').lines() {
            println!("    {line}    ");
        }
        println!("{}", " ".repeat(code.width() + 8));
    }
    println!("{uri}");
    Ok(())
}
