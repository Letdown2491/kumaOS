//! The dev-install lifecycle: how a development build rides a running
//! machine without pretending to be a release.
//!
//! The loop this serves is the one the dev docs describe: build in the
//! container, land the binaries in `~/.local/bin` (PATH-preferred over
//! the image's `/usr/bin` for the CLI, and the home the stamp gate
//! resolves siblings beside), restart the session pieces by hand. What
//! that loop never had was a record. A machine carrying four binaries
//! from three different commits is not a hypothesis — it is how the
//! 44.9.0 story happened — and nothing on the machine could answer
//! "what is dev-built here, from what, and is any of it stale".
//!
//! So: `kuma dev install` stages a checkout's built binaries atomically
//! and writes a receipt. The receipt is a lease, not enforcement — the
//! image is always the authority, and the lease retires itself the
//! moment the image catches up to what it borrowed around.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The receipt: what a dev install put on this machine, from which
/// checkouts, at what commit. `~/.local/share/kuma/dev-install.json` —
/// machine state in the data dir, next to the convergence stamps the
/// doctor reads the same way.
pub fn receipt_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("kuma/dev-install.json")
}

/// One checkout a dev install staged from.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Checkout {
    pub path: String,
    pub version: String,
    pub commit: String,
}

/// One binary the install staged.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Binary {
    pub name: String,
    /// Where the staged bytes came from.
    pub from: String,
    pub commit: String,
    pub version: String,
    pub sha256: String,
}

/// The receipt's document shape.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Receipt {
    pub installed_at: String,
    pub checkouts: Vec<Checkout>,
    pub binaries: Vec<Binary>,
}

impl Receipt {
    pub fn read() -> Option<Receipt> {
        let text = std::fs::read_to_string(receipt_path()).ok()?;
        let receipt: Receipt = serde_json::from_str(&text).ok()?;
        // The names are joined into paths — at stage, at retire, at
        // the daemon adopt — and the receipt is a file any user-level
        // process can edit. A name with a `/` in it (or `..`, or an
        // absolute path, which PathBuf::join lets replace the base
        // whole) would walk outside ~/.local/bin at exactly the
        // moment `retire` reaches for `remove_file`. An unauthenticated
        // lease earns the same treatment as an unparseable one: None,
        // the lease is gone, and doctor grades the machine on what it
        // finds there.
        receipt.binaries.iter().all(|b| is_bare_name(&b.name)).then_some(receipt)
    }

    pub fn write(&self) -> Result<()> {
        let path = receipt_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        // Written through a `.new`+`mv` like every other file this verb
        // touches: a reader between truncate and write would see a
        // half-receipt, and doctor grades what it finds.
        let new = path.with_extension("json.new");
        std::fs::write(&new, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&new, &path)?;
        Ok(())
    }

    /// The newest version any checkout in this receipt claims.
    pub fn version(&self) -> &str {
        self.checkouts.iter().map(|c| c.version.as_str()).max().unwrap_or("")
    }

    /// Remove everything the receipt staged, then the receipt itself.
    /// Best effort on the binaries — a hand already deleted one is the
    /// common way a receipt goes stale — and loud about what went.
    /// When the receipt staged the nostr daemon, the override goes
    /// with them: it is the only thing pointing the unit at the
    /// staged binary, and an override at a deleted binary is a daemon
    /// that will not come back after the next reboot. With the
    /// override gone, the unit's baked ExecStart answers — the image's
    /// binary again, which is what "reverts to the image" means.
    pub fn retire(&self) -> Vec<String> {
        let mut retired = Vec::new();
        for binary in &self.binaries {
            let dest = bin_home().join(&binary.name);
            if std::fs::remove_file(&dest).is_ok() {
                retired.push(dest.display().to_string());
            }
        }
        if self.binaries.iter().any(|b| b.name == "kuma-nostrd") {
            let override_path = daemon_override();
            if std::fs::remove_file(&override_path).is_ok() {
                retired.push(override_path.display().to_string());
            }
            for failure in bounce_daemon() {
                note(&failure);
            }
        }
        let _ = std::fs::remove_file(receipt_path());
        retired
    }
}

/// A receipt's binary name may only be a bare filename: it is joined
/// into ~/.local/bin on every road that consumes it, and `../..` or an
/// absolute path would take the join outside the directory the lease
/// owns — `PathBuf::join` with an absolute name replaces the base
/// entirely.
fn is_bare_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

/// Where dev binaries stage to: the home the stamp gate already
/// resolves siblings beside. `~/.local/bin`.
fn bin_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/bin"))
        .unwrap_or_else(|| PathBuf::from(".local/bin"))
}

/// The nostr daemon's user unit, and the drop-in a dev deploy of the
/// daemon writes. The image bakes the unit with ExecStart at the
/// image's binary; systemd does not consult PATH, so a staged daemon
/// runs only if the override points at it — and only with the
/// override REMOVED does retirement truly revert the daemon to the
/// image's binary.
const DAEMON_UNIT: &str = "kuma-nostrd.service";

pub(crate) fn daemon_override() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| {
            PathBuf::from(home).join(".config/systemd/user/kuma-nostrd.service.d/override.conf")
        })
        .unwrap_or_else(|| {
            PathBuf::from(".config/systemd/user/kuma-nostrd.service.d/override.conf")
        })
}

/// Restart the nostr daemon if its user unit is running. A stopped
/// daemon stays stopped: a dev install adopts the daemon, retirement
/// reverts it, and neither one starts a unit the machine had
/// deliberately down.
///
/// The failure stories come back to the caller — the 45.2.1 lesson at
/// this call site: a swallowed restart left the OLD daemon on the
/// image's binary while install's own print claimed the staged one
/// (#77 finding 8). Silence is the one thing a failed bounce may not
/// be.
fn bounce_daemon() -> Vec<String> {
    let mut failures = Vec::new();
    if !Command::new("systemctl").args(["--user", "daemon-reload"]).status().map(|s| s.success()).unwrap_or(false) {
        failures.push("systemctl --user daemon-reload failed".to_string());
    }
    let active = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", DAEMON_UNIT])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if active
        && !Command::new("systemctl")
            .args(["--user", "restart", DAEMON_UNIT])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    {
        failures.push(format!(
            "systemctl --user restart {DAEMON_UNIT} failed — the daemon may still \
             run the image's binary until it is restarted for real"
        ));
    }
    failures
}

/// The arguments the image's unit bakes onto the daemon's ExecStart —
/// everything after the binary path. A dev deploy that swapped only
/// the binary and dropped the arguments ran a bunker with an EMPTY
/// relay set: no road threads, no wire, no error anywhere — the daemon
/// listens and never hears a note (the silent afternoon of Nov 10,
/// found by the note that never sent). The unit's arguments are part
/// of its posture; the override replaces the binary, not the posture.
/// The parse behind `unit_execstart_args`: the LAST `ExecStart=` in
/// the fragments that are NOT the dev override, minus its binary —
/// systemd applies fragments in printed order and a later non-empty
/// assignment replaces an earlier one, so the first line was this
/// bug's twin from the other side: a delta's relay drop-in
/// (10-kuma-nostr-relays.conf, sorted before override.conf) restates
/// the declared relay line, and reading the first non-override
/// assignment adopted the image unit's relays over the declaration's,
/// silently, on exactly the machines whose declaration had moved.
/// Pure so the test can hold it against the real interleaving
/// `systemctl cat` prints — the override's own bare ExecStart is last,
/// and taking the wrong line is exactly the empty-argument bug this
/// exists to prevent.
fn execstart_args_from_cat(text: &str, override_path: &std::path::Path) -> Option<String> {
    let override_fragment = override_path.display().to_string();
    let mut current_is_override = false;
    let mut last: Option<String> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("# ") {
            // Only a fragment header — a path — moves which fragment
            // the lines belong to. A non-path comment (`# Managed by
            // kuma dev install`, the override's own furniture) must
            // leave the state alone: flipping on every comment would
            // read the override's ExecStart back as the unit's and
            // stage the staged binary as its own argument source.
            if path == override_fragment {
                current_is_override = true;
            } else if path.starts_with('/') {
                current_is_override = false;
            }
            continue;
        }
        if current_is_override {
            continue;
        }
        if let Some(rest) = line.strip_prefix("ExecStart=") {
            let mut words = rest.split_whitespace();
            let _binary = words.next().unwrap_or_default();
            last = Some(words.collect::<Vec<_>>().join(" "));
        }
    }
    last
}

/// The arguments the dev override itself carries: the same parser,
/// with the override path unmatched, so every line counts and the
/// LAST ExecStart is the live one. Pure, and pinned by the same test
/// suite that holds the fragment interleavings.
pub(crate) fn override_execstart_args(text: &str) -> Option<String> {
    execstart_args_from_cat(text, std::path::Path::new("/not/the/override.conf"))
}

/// The unit's current ExecStart arguments (everything after the
/// binary, from the non-override fragments). pub(crate) because the
/// doctor's override audit diffs them against the override's own —
/// the 45.2.1 shape at the override itself (#77 fix shape 3).
pub(crate) fn unit_execstart_args() -> Result<String> {
    let out = Command::new("systemctl")
        .args(["--user", "cat", DAEMON_UNIT])
        .output()
        .with_context(|| format!("cannot ask systemd about {DAEMON_UNIT}"))?;
    if !out.status.success() {
        bail!("{DAEMON_UNIT} does not exist on this machine");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    execstart_args_from_cat(&text, &daemon_override()).ok_or_else(|| {
        anyhow::anyhow!(
            "the image's {DAEMON_UNIT} carries no ExecStart — \
             the override would not know what arguments to keep"
        )
    })
}

/// Point the nostr daemon at the staged binary and bounce it — the
/// last step of the ritual the nostr dev loop ran by hand (override,
/// daemon-reload, restart). Only acts when the receipt stages the
/// daemon AND the machine has the user unit: a machine without the
/// nostr layer gets nothing written and says nothing.
fn adopt_daemon(receipt: &Receipt) -> Result<Option<String>> {
    if !receipt.binaries.iter().any(|b| b.name == "kuma-nostrd") {
        return Ok(None);
    }
    let unit = Command::new("systemctl")
        .args(["--user", "cat", DAEMON_UNIT])
        .status()
        .with_context(|| format!("cannot ask systemd about {DAEMON_UNIT}"))?;
    if !unit.success() {
        // Two stories hide behind that failure, and they used to share
        // one silent exit (finding 9, #77): the unit is not on this
        // machine (no nostr layer — nothing to adopt, the quiet
        // Ok(None) is right), and systemctl --user could not be asked
        // at all (no session bus over SSH — the binaries stage, the
        // receipt claims the daemon, and the daemon was never
        // adopted). The unit FILE disambiguates: the image bakes it,
        // so its presence behind a failed ask is the interrogation's
        // failure, and loud.
        let baked = std::path::PathBuf::from("/usr/lib/systemd/user").join(DAEMON_UNIT);
        if baked.exists() {
            bail!(
                "the nostr layer is on this machine ({DAEMON_UNIT} is baked), but \
                 systemctl --user could not be asked — is there a session bus? \
                 The daemon was NOT adopted: the override is unwritten and the \
                 staged binary is not running"
            );
        }
        // No nostr layer on this machine: the daemon stages for an
        // image that may carry it, and there is nothing here to adopt.
        return Ok(None);
    }
    // The arguments ride along: the image's unit may bake flags the
    // daemon cannot answer without (the relay set is one — an empty
    // set is a bunker with no wire, silently). Read them from the
    // unit BEFORE the override is written: after it, the override's
    // own bare ExecStart is what systemd would answer with.
    let args = unit_execstart_args()?;
    let args_line = if args.is_empty() { String::new() } else { format!(" {args}") };
    let override_path = daemon_override();
    std::fs::create_dir_all(
        override_path.parent().with_context(|| "the override path has no parent".to_string())?,
    )?;
    std::fs::write(
        &override_path,
        format!(
            "# Managed by `kuma dev install` — removed when the lease retires,\n\
             # so the daemon reverts to the image's binary.\n\
             [Service]\nExecStart=\nExecStart={}{args_line}\n",
            bin_home().join("kuma-nostrd").display()
        ),
    )
    .with_context(|| format!("cannot write {}", override_path.display()))?;
    for failure in bounce_daemon() {
        note(&failure);
    }
    Ok(Some(override_path.display().to_string()))
}

/// The verb: stage one or more checkouts' built binaries onto this
/// machine, atomically, and write the receipt naming all of them.
pub fn install(checkouts: &[PathBuf], json: bool) -> Result<()> {
    if checkouts.is_empty() {
        bail!("name at least one checkout: kuma dev install ~/Documents/kuma");
    }
    let mut receipt =
        Receipt { installed_at: utc_now(), checkouts: Vec::new(), binaries: Vec::new() };
    for dir in checkouts {
        stage_checkout(dir, &mut receipt)
            .with_context(|| format!("cannot stage {}", dir.display()))?;
    }
    // Everything verified before anything moved: the staging is last.
    for binary in &receipt.binaries {
        let dest = bin_home().join(&binary.name);
        let new = dest.with_extension("new");
        std::fs::copy(&binary.from, &new)
            .with_context(|| format!("cannot stage {} from {}", dest.display(), binary.from))?;
        set_executable(&new)?;
        // The verify-then-copy gap: between the hash the check took
        // and this copy, the source could have changed (a concurrent
        // build in the same checkout). The receipt must answer for the
        // bytes actually staged, so the `.new` file is hashed and the
        // rename happens only if it matches — a mismatch fails the
        // install loudly instead of shipping a binary the receipt
        // vouches for falsely.
        let staged_sha = sha256_file(&new)
            .with_context(|| format!("cannot hash the staged copy of {}", dest.display()))?;
        if staged_sha != binary.sha256 {
            let _ = std::fs::remove_file(&new);
            bail!(
                "{} changed between verify and stage. The receipt names \
                 sha256 {}; the staged copy hashes to {}. Nothing moved — \
                 rebuild and install again.",
                binary.from,
                binary.sha256,
                staged_sha
            );
        }
        // A running shell or daemon keeps the old inode; the rename is
        // why a hand-run kuma-shell survives the next install.
        std::fs::rename(&new, &dest)
            .with_context(|| format!("cannot move {} into place", dest.display()))?;
    }
    receipt.write()?;
    let names: Vec<&str> = receipt.binaries.iter().map(|b| b.name.as_str()).collect();
    // The daemon adopt comes after the receipt: the receipt is the
    // record that a dev daemon deploy exists, and the override is the
    // only way the staged daemon runs — systemd does not consult PATH.
    let daemon = adopt_daemon(&receipt)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "receipt": receipt_path().display().to_string(),
                "checkouts": receipt.checkouts,
                "installed": names,
                "daemon_override": daemon,
            })
        );
        return Ok(());
    }
    println!("Staged {} binary/binaries beside the machine:", names.len());
    for b in &receipt.binaries {
        println!("  {} ({}, {})", b.name, b.version, b.commit);
    }
    if let Some(path) = &daemon {
        println!(
            "\nThe nostr daemon is pointed at the staged binary (override at {path}; \
             a stopped unit stays stopped)."
        );
    }
    println!(
        "\nThe receipt at {} is a lease: it retires itself when this machine's \
         image reaches {}.\n",
        receipt_path().display(),
        receipt.version()
    );
    Ok(())
}

/// Verify one checkout — clean tree, every staged binary stamped for
/// its HEAD — and record it. Nothing moves until every checkout passes:
/// half a dev install, some binaries from an older commit than the rest,
/// is exactly the state the receipt exists to end.
fn stage_checkout(dir: &Path, receipt: &mut Receipt) -> Result<()> {
    let git = |args: &[&str]| -> Result<String> {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
        if !out.status.success() {
            bail!("`git {}` failed: not a checkout, or a checkout git cannot read", args.join(" "));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let commit = git(&["rev-parse", "--short=7", "HEAD"])?;
    let dirty = git(&["status", "--porcelain"])?;
    if !dirty.is_empty() {
        // The count, never the names: a commit message that quotes what
        // a fix scrubbed put the name back into history once; a status
        // line here could do the same on a terminal someone shares.
        let count = dirty.lines().count();
        bail!(
            "{count} uncommitted change(s) in {}. A dev install stamps the \
             tree's identity onto the machine; a dirty tree is a stamp no \
             commit describes. Commit first.",
            dir.display()
        );
    }
    let version = checkout_version(dir)?;
    let release = dir.join("target/release");
    // The stamp gate is about to RUN every binary it is about to
    // stage — that is the check. For a checkout this account built,
    // that crosses no line: cargo already ran that tree's build
    // scripts. But `kuma dev install ~/some-cloned-repo` would execute
    // a stranger's build output before any check names it, so the
    // tree the binaries come from must be this account's own: refuse
    // a target/ owned by anyone else, and say plainly that the verb
    // executes what it verifies.
    use std::os::unix::fs::MetadataExt;
    let owner = std::fs::metadata(&release)
        .with_context(|| format!("cannot read {} — build first", release.display()))?
        .uid();
    if owner != unsafe { libc::getuid() } {
        bail!(
            "{} is owned by uid {}, not yours. `dev install` executes the \
             build output it verifies — every target/release binary answers \
             --version as the check itself — so it only stages a checkout \
             this account built.",
            release.display(),
            owner
        );
    }
    let mut staged = 0;
    let entries = std::fs::read_dir(&release)
        .with_context(|| format!("cannot read {} — build first", release.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_executable(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        // Build scripts and helper binaries share target/release; the
        // stamp gate is the filter. A binary that answers nothing is
        // not ours to judge; one that answers for another commit is
        // exactly the stale sibling the image's own gate fails on.
        let out = Command::new(&path).arg("--version").output();
        let Ok(out) = out else { continue };
        if !out.status.success() {
            continue;
        }
        let version_line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match stamp_sha(&version_line) {
            // No commit token in the answer: the nostr road's trio —
            // their `--version` is clap's crate version, and the
            // image's own staging gate holds them to exactly that:
            // the stamp must contain this checkout's version, because
            // cargo builds the four together and a partial install is
            // the stale-sibling shape the gate exists for. The same
            // rule here is what makes "deploys ALL siblings" true and
            // dev staging promise what image staging promises.
            None => {
                if !version_line.contains(&version) {
                    continue;
                }
            }
            // A stale sibling — the exact state the image's own gate
            // fails on, refused here for the same reason: the receipt
            // would name a tree while staging bytes from another.
            Some(sha) if sha != commit => bail!(
                "{} answers --version for commit {}, not this checkout's \
                 HEAD ({}). Rebuild so every staged binary is stamped for \
                 the tree the receipt will name.",
                path.display(),
                sha,
                commit
            ),
            Some(_) => {}
        }
        let sha256 = sha256_file(&path)?;
        receipt.binaries.push(Binary {
            name: name.to_string(),
            from: path.display().to_string(),
            commit: commit.clone(),
            version: version_token(&version_line),
            sha256,
        });
        staged += 1;
    }
    if staged == 0 {
        bail!(
            "nothing in {} is stamped for HEAD ({}). The stamp gate selects \
             what stages: build the checkout first, and commit so the build \
             is not -dirty.",
            release.display(),
            commit
        );
    }
    receipt.checkouts.push(Checkout { path: dir.display().to_string(), version, commit });
    Ok(())
}

/// The checkout's version, from its Cargo.toml's [package] version —
/// the same number the built binary's stamp carries.
fn checkout_version(dir: &Path) -> Result<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).context("cannot read Cargo.toml")?;
    let value: toml::Value = toml::from_str(&text).context("cannot parse Cargo.toml")?;
    value
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("Cargo.toml carries no [package].version"))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// sha256 of a file — the receipt records it, and the doctor's net
/// compares it: the staged bytes must still be the bytes the receipt
/// vouched for. `pub(crate)` because the doctor check is the other
/// reader.
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    // sha256sum is on every machine this verb can run on: it is the
    // same tool the image's verification story names.
    let out = Command::new("sha256sum").arg(path).output()?;
    if !out.status.success() {
        bail!("sha256sum failed for {}", path.display());
    }
    let line = String::from_utf8_lossy(&out.stdout);
    Ok(line.split_whitespace().next().unwrap_or_default().to_string())
}

/// The 7-hex commit token a `--version` stamp carries — kuma's own
/// shape is `45.2.0 (<sha> <date>)`, and the stamp gate's other side
/// (the kumaui siblings) is matched format-agnostically: any 7-hex
/// run in the answer is the commit.
fn stamp_sha(version_line: &str) -> Option<String> {
    version_line
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|w| w.len() == 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .map(String::from)
}

fn utc_now() -> String {
    let out = Command::new("date").arg("-u").arg("+%Y-%m-%dT%H:%M:%SZ").output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Whether the lease has lapsed: the running image now ships at least
/// the version the dev install was staged around, so the image's own
/// binaries are the ones the receipt was waiting for and the dev
/// install is borrowing nothing anymore.
///
/// The comparison reads the image's binary at /usr/bin/kuma — not the
/// one on PATH, which a dev install is exactly the thing that shadows.
pub fn lapsed(receipt: &Receipt) -> bool {
    let image_version = image_version();
    let Some(image) = dotted(&image_version) else { return false };
    let Some(dev) = dotted(receipt.version()) else { return false };
    image >= dev
}

/// The version token in a `--version` line: the first token that
/// parses as a dotted number. clap prints `{name} {version}`, so the
/// line opens with the binary's own name — `kuma 45.2.0 (afea6df …)` —
/// and the first token is the NAME. Reading the name as the version is
/// how a lease that can never lapse gets written: `dotted("kuma")` is
/// nothing, and nothing never reaches the image's version.
fn version_token(line: &str) -> String {
    line.split_whitespace().find(|t| dotted(t).is_some()).unwrap_or_default().to_string()
}

/// The image's own kuma version — the dotted token of
/// `/usr/bin/kuma --version`, not the one on PATH, which a dev install
/// is exactly the thing that shadows.
fn image_version() -> String {
    Command::new("/usr/bin/kuma")
        .arg("--version")
        .output()
        .map(|o| version_token(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// "45.2.0" → (45, 2, 0): the dotted prefix of a version token, numeric.
fn dotted(version: &str) -> Option<Vec<u64>> {
    if version.is_empty() {
        return None;
    }
    let core: Vec<&str> = version.split('-').next().unwrap_or(version).split('.').collect();
    if core.is_empty() {
        return None;
    }
    core.iter().map(|n| n.parse::<u64>().ok()).collect()
}

/// Retire the lease when it has lapsed. The upgrade and update paths
/// call this before their own work: an image that just moved past the
/// dev install is the moment the lease ends, and the machines where it
/// matters most are the ones whose next command is the upgrade that
/// ends it.
///
/// Silence when there is no receipt or the lease holds; the retirement
/// itself is loud on stderr (a JSON caller's progress stream), because
/// it changed the machine in a way the receipt used to deny.
pub fn retire_if_lapsed() {
    let Some(receipt) = Receipt::read() else { return };
    if !lapsed(&receipt) {
        return;
    }
    let daemon = receipt.binaries.iter().any(|b| b.name == "kuma-nostrd");
    let retired = receipt.retire();
    note(&format!(
        "Retired the dev install (a lease, lapsed when this image reached {}): {}.{}",
        receipt.version(),
        if retired.is_empty() { "nothing left to remove".to_string() } else { retired.join(", ") },
        if daemon {
            " The nostr daemon's override is gone: it runs the image's binary again."
        } else {
            ""
        }
    ));
}

fn note(text: &str) {
    eprintln!("\n{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The override's args preservation. The image's unit bakes the
    /// relay set onto its ExecStart, and a dev deploy that dropped it
    /// ran a bunker with no wire at all — silent, listening, deaf.
    /// The parser reads the unit's OWN fragment out of `systemctl
    /// cat`'s multi-fragment output and keeps everything after the
    /// binary.
    #[test]
    fn the_override_keeps_the_image_units_arguments() {
        // The real interleaving: the image fragment first, the dev
        // override last, the override's own ExecStart the bare
        // staged binary. The parser must take the image's line.
        let text = "\
# /usr/lib/systemd/user/kuma-nostrd.service
[Unit]
Description=kumaOS nostr bunker

[Service]
ExecStart=/usr/bin/kuma-nostrd --relay wss://relay.nip46.com
ExecStartPre=+/usr/bin/mkdir -p %h/.local/state/kuma-nostr
# /var/home/myuser/.config/systemd/user/kuma-nostrd.service.d/override.conf
# Managed by `kuma dev install`
[Service]
ExecStart=
ExecStart=/var/home/myuser/.local/bin/kuma-nostrd
";
        let args = execstart_args_from_cat(
            text,
            &PathBuf::from(
                "/var/home/myuser/.config/systemd/user/kuma-nostrd.service.d/override.conf",
            ),
        )
        .expect("the image's ExecStart is in the cat output");
        assert_eq!(
            args, "--relay wss://relay.nip46.com",
            "the image's relay argument must survive the override"
        );
        // And the honest absences: an image unit with no arguments
        // keeps none, and a unit cat with no ExecStart at all names
        // the gap instead of inventing a bare daemon.
        let bare = "ExecStart=/usr/bin/kuma-nostrd\n";
        assert_eq!(
            execstart_args_from_cat(bare, &PathBuf::from("/no/override.conf")).as_deref(),
            Some(""),
        );
        assert_eq!(execstart_args_from_cat("[Unit]\n", &PathBuf::from("/no/override.conf")), None);

        // The delta machine's interleaving — the twin this parse
        // nearly shipped: a delta staging the declaration's relay set
        // writes a DROP-IN (sorted before the dev override, after the
        // image's fragment), restating ExecStart with the declared
        // relays. Taking the FIRST non-override line adopted the
        // image's relays over the declaration's — silent, on exactly
        // the machines whose declaration had moved. The LAST
        // non-override line is the declaration's.
        let delta = "\
# /usr/lib/systemd/user/kuma-nostrd.service
[Service]
ExecStart=/usr/bin/kuma-nostrd --relay wss://relay.nip46.com
# /var/home/myuser/.config/systemd/user/kuma-nostrd.service.d/10-kuma-nostr-relays.conf
[Service]
ExecStart=
ExecStart=/usr/bin/kuma-nostrd --relay wss://my-share.brainstick.wtf
# /var/home/myuser/.config/systemd/user/kuma-nostrd.service.d/override.conf
# Managed by `kuma dev install`
[Service]
ExecStart=
ExecStart=/var/home/myuser/.local/bin/kuma-nostrd
";
        let override_path = PathBuf::from(
            "/var/home/myuser/.config/systemd/user/kuma-nostrd.service.d/override.conf",
        );
        let args = execstart_args_from_cat(delta, &override_path)
            .expect("the drop-in's ExecStart is in the cat output");
        assert_eq!(
            args, "--relay wss://my-share.brainstick.wtf",
            "the declaration's relay drop-in wins over the image's fragment"
        );

        // And a non-path comment INSIDE the managed override must not
        // flip the fragment state: the override carries its own
        // comments, and a flip would read the override's bare
        // ExecStart back as the unit's — the staged binary standing in
        // for the arguments, the empty-argument bug by another door.
        let commented = "\
# /usr/lib/systemd/user/kuma-nostrd.service
ExecStart=/usr/bin/kuma-nostrd --relay wss://relay.nip46.com
# /var/home/myuser/.config/systemd/user/kuma-nostrd.service.d/override.conf
# Managed by `kuma dev install`
# staged by the dev lease; retired by `kuma dev retire`
[Service]
ExecStart=
ExecStart=/var/home/myuser/.local/bin/kuma-nostrd
";
        let args = execstart_args_from_cat(commented, &override_path)
            .expect("the image's ExecStart is in the cat output");
        assert_eq!(args, "--relay wss://relay.nip46.com");
    }

    /// The override audit's own read (#77 fix shape 3): the override's
    /// LAST ExecStart minus the binary, from the override file's text
    /// alone. The doctor diffs this against the unit's current
    /// non-override arguments — install-time args surviving an image
    /// whose unit moved is the 45.2.1 shape at the override itself.
    #[test]
    fn the_override_audit_reads_the_overrides_own_arguments() {
        let override_text = "\
# Managed by `kuma dev install` — removed when the lease retires,
# so the daemon reverts to the image's binary.
[Service]
ExecStart=
ExecStart=/var/home/myuser/.local/bin/kuma-nostrd --relay wss://stale.example
";
        assert_eq!(
            override_execstart_args(override_text).as_deref(),
            Some("--relay wss://stale.example"),
            "the override's own arguments, whatever the unit says now"
        );
        // An override written when the unit carried no arguments reads
        // back empty — which is a FACT to diff, not an absence.
        let bare = "ExecStart=\nExecStart=/var/home/myuser/.local/bin/kuma-nostrd\n";
        assert_eq!(override_execstart_args(bare).as_deref(), Some(""));
        // And an unparseable override names the gap.
        assert_eq!(override_execstart_args("[Unit]\n"), None);
    }

    /// The stamp gate's parsing side. The kuma shape is
    /// `45.2.0 (<7-hex> <date>)`; the kumaui siblings print their own
    /// shapes, and the gate is format-agnostic on purpose — any 7-hex
    /// word is a commit, which is what made the image-side gate able to
    /// pin binaries it did not print itself.
    #[test]
    fn the_stamp_sha_is_any_seven_hex_word() {
        assert_eq!(
            stamp_sha("45.2.0 (abc1234 2026-11-10)").as_deref(),
            Some("abc1234"),
            "kuma's own stamp shape"
        );
        assert_eq!(
            stamp_sha("kuma-shell 0.9.0 (7c0ffee)").as_deref(),
            Some("7c0ffee"),
            "the sibling shape: no parens around the sha"
        );
        assert_eq!(
            stamp_sha("0.4.1-abc1234-x").as_deref(),
            Some("abc1234"),
            "a sha riding dashes, not spaces"
        );
        assert_eq!(stamp_sha("45.2.0 (unknown 2026-11-10)"), None, "no sha, no opinion");
        assert_eq!(stamp_sha(""), None);
        // Six hex is not seven, eight is not seven: the length is the
        // contract.
        assert_eq!(stamp_sha("abcdef 89abcd abcdef01"), None, "6 and 8 hex are not the stamp's 7");
    }

    /// The lapse comparison's parser: a version token's dotted numeric
    /// prefix, comparing lexicographically on the tuple. Deltas and
    /// suffixes ride along in the token but do not decide the compare —
    /// the image either reached the version or it did not.
    #[test]
    fn dotted_reads_the_numeric_prefix_and_nothing_else() {
        assert_eq!(dotted("45.2.0"), Some(vec![45, 2, 0]));
        assert_eq!(dotted("45.2.0-dirty"), Some(vec![45, 2, 0]), "-dirty does not break the parse");
        assert_eq!(dotted(""), None, "an unreadable image version never lapses");
        assert_eq!(dotted("unknown"), None);
        // The comparison itself: image catches up → lapsed; dev ahead
        // → holding.
        let image = dotted("45.3.0");
        let dev = dotted("45.2.0");
        assert!(image > dev, "an image past the lease lapses it");
        assert!(dotted("45.2.0") <= dotted("45.2.0"), "equal versions lapse: the image caught up");
        assert!(dotted("45.1.9") < dotted("45.2.0"), "the dev build is ahead: the lease holds");
    }

    /// The version token's parse: clap prints `{name} {version}`, so
    /// the first token is the binary's NAME. Reading the name as the
    /// version made `dotted` answer nothing and a lease that could
    /// never lapse — the bug this pins.
    #[test]
    fn the_version_token_is_the_first_dotted_word_not_the_first_word() {
        assert_eq!(version_token("kuma 45.2.0 (abc1234 2026-11-10)"), "45.2.0");
        assert_eq!(version_token("kuma-nostrd 45.2.0"), "45.2.0");
        assert_eq!(version_token("kuma-shell 0.9.0 (7c0ffee)"), "0.9.0");
        assert_eq!(version_token(""), "");
        // A name that is all digits-and-nothing-else could impersonate
        // a version; nothing in this repo is named that, and if one
        // ever is, the parse's failure mode is a version that reads as
        // the name's digits — caught here the day the name changes.
        assert_eq!(version_token("2026 45.2.0"), "2026", "the FIRST dotted word wins");
    }

    /// The receipt's document shape: what it serializes as, and that a
    /// round trip keeps the parts a doctor check and the lapse compare
    /// each read. Written here once because the file on disk is the
    /// only contract between `kuma dev install`, `kuma doctor`, and a
    /// human reading it in a year.
    #[test]
    fn the_receipt_round_trips_its_own_parts() {
        let text = serde_json::json!({
            "installed_at": "2026-11-10T00:00:00Z",
            "checkouts": [
                {"path": "/var/home/me/Documents/kuma", "version": "45.2.0", "commit": "abc1234"}
            ],
            "binaries": [
                {"name": "kuma", "from": "/kuma/target/release/kuma", "commit": "abc1234",
                 "version": "45.2.0 (abc1234 2026-11-10)", "sha256": "f".repeat(64)}
            ],
        })
        .to_string();
        let receipt: Receipt = serde_json::from_str(&text).expect("the written shape reads back");
        assert_eq!(receipt.version(), "45.2.0", "the lease's version is the checkouts' max");
        assert_eq!(receipt.binaries[0].name, "kuma");
        assert_eq!(receipt.binaries[0].sha256.len(), 64);

        // Two checkouts, one older: the newest decides when the lease
        // lapses — retiring while any staged tree is still ahead of the
        // image would remove binaries the image cannot replace yet.
        let two: Receipt = serde_json::from_str(
            &serde_json::json!({
                "installed_at": "",
                "checkouts": [
                    {"path": "/a", "version": "45.2.0", "commit": "aaaaaaa"},
                    {"path": "/b", "version": "45.3.0", "commit": "bbbbbbb"}
                ],
                "binaries": [],
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(two.version(), "45.3.0");
    }
}
