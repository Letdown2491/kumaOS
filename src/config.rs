use anyhow::{bail, Context, Result};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const CURRENT_SCHEMA: u32 = 1;
// The compose environment a compose runs in when a declaration names
// none. It followed the quay float until the 2026-09-30 boot failures
// got blamed on the float — a composed base whose install writes
// "version": null into .bootc-aleph.json, which bootupd's reader
// refuses (#18) — and was pinned by digest to a ghcr mirror of the
// last float that installed green. The pin guarded nothing: its tools
// proved byte-identical to the floating env's, and the real root cause
// was kuma's own composed bases carrying no version label, fixed in
// the generated Containerfile and base-independent. The float is
// followed again; the heartbeat below is its tripwire. The mirror
// package (kuma-compose-env) is the pin's leftover; its last-good tag
// now names the September float the pin served and has no consumer.
pub const DEFAULT_BASE: &str = "quay.io/fedora/fedora-bootc:44";

/// A kuma system declaration: the one file that describes a machine.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub system: System,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub packages: Packages,
    #[serde(default)]
    pub services: Services,
    #[serde(default)]
    pub snapshots: Snapshots,
    /// Offsite copies of what [snapshots] keeps locally. Names the
    /// credential it opens the repository with; never holds it.
    #[serde(default)]
    pub backup: Backup,
    /// Flatpak permission overrides, declared per app and converged per
    /// key. Everything else in the same file stays whoever's it was,
    /// which is what lets Flatseal and kuma both write one app without
    /// deleting each other's lines.
    #[serde(default)]
    pub overrides: Overrides,
    /// The nostr layer: a bunker daemon, its CLI, and the relays they
    /// talk to.
    #[serde(default)]
    pub nostr: Nostr,
}

/// The nostr layer's declaration.
///
/// `enable` gates the whole render: the daemon and CLI binaries, the
/// hardened user unit, the plugin and its bind when they land. Absent
/// and `enable = false` ship nothing — the same grammar as [backup],
/// and a toggle never destroys anything: the vault lives in the login
/// keyring, which is user state an image update does not touch, so a
/// disable is reversible by construction.
///
/// The relays are the bunker's roads, declared as full URLs. Absent,
/// the public relay is the road: a fresh install pairs desktop, web and
/// phone apps with nothing configured, and no tailscale story to
/// maintain — the payload is NIP-44 end-to-end, so the relay carries
/// ciphertext and metadata, not messages. An explicit empty list is the
/// local-only spelling, for a machine whose relay is its own.
#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Nostr {
    #[serde(default)]
    pub enable: bool,
    /// The relays the bunker talks to, `wss://` to the world and
    /// `ws://` only to loopback — plaintext kind 24133 traffic to
    /// anything else is exactly the shape of mistake a validator
    /// exists to catch. Every entry is validated here, at build: a
    /// typo is a build failure rather than a shrug.
    #[serde(default = "default_relays")]
    pub relays: Vec<String>,
    /// The local relay, which is opt-in now: the public relay is the
    /// default signing road, and a loopback relay is for the machine
    /// that wants its desktop apps pairing with nothing leaving the
    /// house.
    #[serde(default)]
    pub relay: NostrRelay,
    /// The inactivity switch's window, in seconds: after this long
    /// with no unlock and no keep-alive, the daemon locks itself — the
    /// dead man's switch, whose act is the panel's own lock. Absent or
    /// 0 leaves the switch off, the desktop default: the keyring is
    /// PAM-open here, and a switch on by default would lock the bunker
    /// while the person is away. The floor is an hour — a fuse shorter
    /// than that trips on lunch — and a violation is a build failure,
    /// like every other lie the declaration can tell.
    #[serde(default)]
    pub inactivity_lock_secs: Option<u64>,
    /// The tailnet exposure switch: serve the local relay on the
    /// machine's ts.net name. Absent means local-only, because widening
    /// who can reach the relay is a decision about the trust boundary
    /// and the grammar does not make it for anyone — it is not implied
    /// by the relay and tailscaled both being on, a pair of facts that
    /// say nothing about intent. Serving needs the local relay and
    /// tailscaled; the doctor says so when one is missing.
    #[serde(default)]
    pub serve: bool,
}

/// The local relay's switch. Absent means off — the public relay is
/// the default signing road, and the loopback relay is the machine
/// that asks for it by name.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NostrRelay {
    #[serde(default)]
    pub enable: bool,
}

/// The serde default that says what absence means: the public relay
/// the layer's installs pair against.
fn default_relays() -> Vec<String> {
    vec!["wss://relay.nip46.com".to_string()]
}

/// Local btrfs snapshots of the machine state a declaration cannot
/// reproduce. kuma.toml rebuilds a *system*; it does not rebuild
/// /var/home, and a tool that makes machines feel disposable owes that
/// part an answer.
///
/// Deliberately the cheap half of the problem: this survives a bad
/// update, an overwrite, or a deleted directory, and not a dead disk or
/// a stolen laptop. [backup] is the other half, and it reads from here
/// rather than from the live subvolume, which is why enabling it
/// requires enabling this.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Snapshots {
    #[serde(default)]
    pub enable: bool,
    /// The btrfs subvolume to snapshot. Snapshots land in
    /// `<target>/.snapshots`, a nested subvolume path btrfs leaves out
    /// of the snapshots themselves, so they never nest.
    #[serde(default = "default_snapshot_target")]
    pub target: String,
    /// A systemd OnCalendar expression: "hourly", "daily", or something
    /// like "*-*-* 03:00:00".
    #[serde(default = "default_snapshot_interval")]
    pub interval: String,
    /// Keep this many of the newest snapshots, whatever their age.
    #[serde(default = "default_keep_recent")]
    pub keep_recent: u32,
    /// Then additionally keep the newest snapshot from each of this many
    /// further days, so the total ceiling is keep_recent + keep_daily.
    #[serde(default = "default_keep_daily")]
    pub keep_daily: u32,
}

fn default_snapshot_target() -> String {
    "/var/home".to_string()
}
fn default_snapshot_interval() -> String {
    "hourly".to_string()
}
fn default_keep_recent() -> u32 {
    24
}
fn default_keep_daily() -> u32 {
    7
}

impl Default for Snapshots {
    fn default() -> Self {
        Self {
            enable: false,
            target: default_snapshot_target(),
            interval: default_snapshot_interval(),
            keep_recent: default_keep_recent(),
            keep_daily: default_keep_daily(),
        }
    }
}

/// Offsite copies of the one thing a rebuild cannot produce.
///
/// [snapshots] survives a mistake; this survives the disk. It reads from
/// a read-only snapshot rather than from the live subvolume, so files
/// cannot change under it mid-run, which is why `enable` here requires
/// `snapshots.enable` and says so at `kuma check` rather than at 3am.
///
/// **The credential is named here and held on the machine, and that is
/// not a hole in the declaration.** A declaration is written to be
/// committed and is baked world-readable into an image, which is the
/// wrong place for a secret and the same boundary capture.rs draws
/// around [user]. Naming it keeps this file complete: that a credential
/// exists, and what it is called, is declared here; only its value lives
/// elsewhere. The practical consequence is about recovery rather than
/// about the file, namely that restoring a machine takes two things
/// instead of one.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    #[serde(default)]
    pub enable: bool,
    /// A restic repository, spelled the way restic spells it:
    /// `s3:https://host/bucket`, `sftp:user@host:/path`,
    /// `rclone:remote:path`, `b2:bucket`.
    ///
    /// One string rather than a `provider` key beside it, because
    /// restic's own scheme prefix already says which backend this is and
    /// two places to say one thing can disagree.
    #[serde(default)]
    pub repo: String,
    /// The name of the credential this repository is opened with, never
    /// the credential. The machine holds the value at
    /// `/var/lib/kuma/secrets/<secret>.env`, 0600 root, put there by
    /// hand.
    #[serde(default = "default_backup_secret")]
    pub secret: String,
    /// A systemd OnCalendar expression, as in [snapshots].
    #[serde(default = "default_backup_interval")]
    pub interval: String,
    /// Retention handed to `restic forget`. Zero disables that tier;
    /// all three at zero is refused, since it would delete every
    /// snapshot on the run that made it.
    #[serde(default = "default_backup_keep_daily")]
    pub keep_daily: u32,
    #[serde(default = "default_backup_keep_weekly")]
    pub keep_weekly: u32,
    #[serde(default = "default_backup_keep_monthly")]
    pub keep_monthly: u32,
    /// Excluded on top of the curated set, never instead of it. The
    /// curated entries are the trees a declaration already rebuilds, so
    /// dropping them would mean paying to store what kuma can recreate.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Carry `/etc/NetworkManager/system-connections` alongside home.
    ///
    /// **Off by default, which is a deliberate trade rather than an
    /// oversight.** Those files are the one thing on the machine nothing
    /// else can recreate: not this declaration, which calls them out of
    /// scope, not the image, which ships the directory empty, and not
    /// home, since they live in /etc. Restore without them and you retype
    /// every network password from memory.
    ///
    /// They are also a per-network passphrase in the clear, so including
    /// them puts those in the repository. restic encrypts client-side, so
    /// the far end never sees them, but it is still a decision rather
    /// than a default, and the conservative answer is the one that does
    /// not move secrets off the machine on somebody's behalf.
    ///
    /// The cost of that default is that a working backup quietly omits
    /// the only unrecoverable thing, so `kuma doctor` says which way this
    /// is set. An omission you read on an ordinary day is a choice; one
    /// you discover during a restore is a trap.
    #[serde(default)]
    pub network_connections: bool,
}

fn default_backup_secret() -> String {
    "backup".to_string()
}
fn default_backup_interval() -> String {
    "daily".to_string()
}
fn default_backup_keep_daily() -> u32 {
    7
}
fn default_backup_keep_weekly() -> u32 {
    4
}
fn default_backup_keep_monthly() -> u32 {
    6
}

impl Default for Backup {
    fn default() -> Self {
        Self {
            enable: false,
            repo: String::new(),
            secret: default_backup_secret(),
            interval: default_backup_interval(),
            keep_daily: default_backup_keep_daily(),
            keep_weekly: default_backup_keep_weekly(),
            keep_monthly: default_backup_keep_monthly(),
            exclude: Vec::new(),
            network_connections: false,
        }
    }
}

/// The primary account, created and converged by a boot service, not at
/// image build time, because /home is machine state (/var/home) and an
/// image-built home directory never materializes on `bootc switch`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct User {
    pub name: String,
    /// Login shell by name ("fish"); the binary must exist in the image,
    /// checked at build time.
    #[serde(default)]
    pub shell: Option<String>,
    /// Converged additively: declared groups are ensured, groups granted
    /// imperatively on the machine are left alone.
    #[serde(default = "default_groups")]
    pub groups: Vec<String>,
    /// crypt(5) hash (e.g. `openssl passwd -6`), applied only when the
    /// account is first created; after that the password is machine state.
    #[serde(default)]
    pub password_hash: Option<String>,
    /// OpenSSH public keys, served from /etc/kuma/keys/<name> alongside
    /// the user's own ~/.ssh/authorized_keys (never overwriting it).
    #[serde(default)]
    pub ssh_keys: Vec<String>,
    /// Log this user straight into the desktop at boot (greetd
    /// initial_session). The greeter still appears after logout.
    #[serde(default)]
    pub autologin: bool,
}

fn default_groups() -> Vec<String> {
    vec!["wheel".to_string()]
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct System {
    /// Unset — the default — means kuma composes its own base from
    /// Fedora's repos (see compose.rs). Set it to any bootc image
    /// reference to build FROM that instead: the escape hatch for
    /// debugging against fedora-bootc, a mirror, or a test tag.
    #[serde(default)]
    pub base: Option<String>,
    /// Trim the composed base's firmware to these packages (members of
    /// compose::FIRMWARE_PACKAGES). Unset ships the broad set: every
    /// vendor's GPU/wifi/audio blobs, so unknown hardware still boots
    /// with everything working. Only meaningful when the base is
    /// composed — an explicit `base` image rejects it.
    #[serde(default)]
    pub firmware: Option<Vec<String>>,
    #[serde(default)]
    pub desktop: Desktop,
    /// Certificate authorities this machine trusts on top of the ones
    /// Fedora ships, keyed by the name each one gets on disk.
    ///
    /// The certificate itself goes in the file, in PEM, because a
    /// declaration that points at a path somewhere else is not one file
    /// any more. A CA certificate is public by construction, which is
    /// what makes that safe here and unsafe for anything in [user].
    #[serde(default)]
    pub ca_certificates: BTreeMap<String, String>,
    /// The login shell accounts made on this machine get, when nothing
    /// else says otherwise.
    ///
    /// Separate from `[user].shell` on purpose. That one describes a
    /// person, so shareable media cannot carry it: a published image
    /// declares no `[user]` at all, which left an image that installs
    /// fish with no way to say "and use it". This is a property of the
    /// image rather than of anybody, so it survives that rule, and
    /// `kuma install` reads it from the declaration baked into whatever
    /// it is installing.
    ///
    /// Not defaulted to a shell kuma ships, because kuma ships none:
    /// fish and the rest come from a declaration's package list, so a
    /// default would refuse to install any image that omitted it.
    #[serde(default)]
    pub shell: Option<String>,
    /// Homebrew in /home/linuxbrew: imperative CLI tools that survive image
    /// updates and need no reboot. Installed by a first-boot service.
    #[serde(default)]
    pub brew: bool,
    /// Pin an IANA timezone (e.g. "America/Denver") in the image. Usually
    /// unset: timezone is machine state, set once via timedatectl and kept
    /// across image updates; `kuma vm` mirrors the host.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Pin /etc/hostname. Usually unset: hostname is machine state
    /// (`hostnamectl set-hostname` persists), and the image ships
    /// /etc/hostname "kuma" as the merge default when unset.
    #[serde(default)]
    pub hostname: Option<String>,
    /// System locale, e.g. "en_US.UTF-8". Installs the matching glibc
    /// langpack and sets /etc/locale.conf. Unset keeps the base default
    /// (C.UTF-8).
    #[serde(default)]
    pub locale: Option<String>,
}

impl Config {
    /// The image the Containerfile builds FROM: the declared base, or —
    /// the kuma default — the content-addressed tag its own composed
    /// base will carry. A pure function of the declaration, so builds,
    /// `kuma generate`, and tests all agree without touching podman.
    pub fn base_ref(&self) -> String {
        self.system.base.clone().unwrap_or_else(|| crate::compose::content_tag(self))
    }
}

/// A desktop is a curated set (compositor, greeter, portals, audio, fonts),
/// not a package list; the curation is Kuma's job. Niri is hand-assembled
/// (a compositor needs a desktop built around it); COSMIC curates itself
/// and kuma adds only hardware enablement and identity.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Desktop {
    #[default]
    None,
    Niri,
    Cosmic,
}

#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Packages {
    #[serde(default)]
    pub rpm: Vec<String>,
    /// Flatpaks are runtime state, not image content: converged on boot and
    /// by `kuma sync` — additions install, removals uninstall — with no
    /// rebuild and no reboot.
    #[serde(default)]
    pub flatpak: Vec<String>,
    /// Homebrew formulae, converged like flatpaks: additions install,
    /// removals uninstall. Only formulae this list ever named are removal
    /// candidates; ad-hoc `brew install` on the machine stays yours.
    /// A non-empty list implies system.brew.
    #[serde(default)]
    pub brew: Vec<String>,
}

#[derive(Debug, Deserialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Services {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
}

/// Flatpak permission overrides, keyed by application ID.
pub type Overrides = BTreeMap<String, AppOverride>;

/// Which override store a declaration writes into. Flatpak keeps two,
/// and they are not alternatives: the system store covers every account
/// on the machine, the user store covers one and is the only one
/// Flatseal can write. System is the default because it is where kuma
/// installs the apps it declares.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default, JsonSchema, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    System,
    User,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::System => "system",
            Scope::User => "user",
        }
    }
}

/// What a bus name may do. Spelled as flatpak spells it, because these
/// values are written into the override file verbatim.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BusPolicy {
    None,
    See,
    Talk,
    Own,
}

impl BusPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            BusPolicy::None => "none",
            BusPolicy::See => "see",
            BusPolicy::Talk => "talk",
            BusPolicy::Own => "own",
        }
    }
}

/// One application's declared overrides.
///
/// The shape is the override file's own, not `flatpak override`'s flag
/// strings: the six `[Context]` list keys, then the three sections that
/// hold name/value pairs. That is deliberate. From a flag string kuma
/// cannot tell which file key a `--nofilesystem` lands in without
/// reimplementing flatpak's parser, and not knowing the key means the
/// only safe convergence is to replace the whole file, which is exactly
/// the authority this feature refuses to take.
#[derive(Debug, Deserialize, Default, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AppOverride {
    /// Which store this app's overrides are written into.
    #[serde(default)]
    pub scope: Scope,
    /// Paths, as flatpak spells them: `home`, `xdg-download:ro`,
    /// `~/src`, and `!` in front of any of them to take it away.
    #[serde(default)]
    pub filesystems: Vec<String>,
    #[serde(default)]
    pub sockets: Vec<String>,
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub shared: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub persistent: Vec<String>,
    /// Environment variables the app is launched with.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Session bus names, each mapped to none, see, talk, or own.
    #[serde(default)]
    pub session_bus: BTreeMap<String, BusPolicy>,
    #[serde(default)]
    pub system_bus: BTreeMap<String, BusPolicy>,
}

impl AppOverride {
    /// The `[Context]` lists, paired with the key each one is written
    /// as. One place decides the key names, so the parser, the file
    /// writer and the converger cannot disagree about them.
    pub fn context_lists(&self) -> [(&'static str, &Vec<String>); 6] {
        [
            ("filesystems", &self.filesystems),
            ("sockets", &self.sockets),
            ("devices", &self.devices),
            ("shared", &self.shared),
            ("features", &self.features),
            ("persistent", &self.persistent),
        ]
    }

    /// An app that declares nothing. Worth naming because it reads as a
    /// declaration and converges to nothing at all.
    pub fn is_empty(&self) -> bool {
        self.context_lists().iter().all(|(_, v)| v.is_empty())
            && self.environment.is_empty()
            && self.session_bus.is_empty()
            && self.system_bus.is_empty()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "cannot read {}; run `kuma init` to start one here, or point --config at yours",
                path.display()
            )
        })?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("invalid config in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CURRENT_SCHEMA {
            bail!(
                "unsupported schema_version {} (this kuma understands {})",
                self.schema_version,
                CURRENT_SCHEMA
            );
        }
        if let Some(base) = &self.system.base {
            validate_name(base, "system.base", &['/', ':', '.', '-', '_', '@'])?;
        }
        if let Some(firmware) = &self.system.firmware {
            if self.system.base.is_some() {
                bail!(
                    "system.firmware trims kuma's composed base; it means nothing \
                     when system.base names an image: remove one of the two"
                );
            }
            for pkg in firmware {
                if !crate::compose::FIRMWARE_PACKAGES.contains(&pkg.as_str()) {
                    bail!(
                        "system.firmware entry {pkg:?} is not a firmware package kuma \
                         knows; the set is: {}",
                        crate::compose::FIRMWARE_PACKAGES.join(", ")
                    );
                }
            }
        }
        if let Some(tz) = &self.system.timezone {
            validate_name(tz, "system.timezone", &['/', '-', '_', '+'])?;
        }
        if let Some(hostname) = &self.system.hostname {
            validate_name(hostname, "system.hostname", &['-', '.'])?;
        }
        if let Some(locale) = &self.system.locale {
            validate_name(locale, "system.locale", &['_', '.', '-', '@'])?;
        }
        // Same character rules as user.shell: both end up as
        // /usr/bin/<name> in a shell fragment the boot converger sources.
        if let Some(shell) = &self.system.shell {
            validate_name(shell, "system.shell", &['-'])?;
        }
        if let Some(user) = &self.user {
            validate_name(&user.name, "user.name", &['-', '_'])?;
            if let Some(shell) = &user.shell {
                validate_name(shell, "user.shell", &['-'])?;
            }
            for group in &user.groups {
                validate_name(group, "user.groups", &['-', '_'])?;
            }
            if let Some(hash) = &user.password_hash {
                // crypt(5) alphabet plus '=' for rounds= parameters; all
                // inert inside the single-quoted declaration file the
                // sync script sources
                validate_name(hash, "user.password_hash", &['$', '.', '/', '='])?;
                validate_password_hash(hash)?;
            }
            for key in &user.ssh_keys {
                let looks_like_key = ["ssh-", "ecdsa-", "sk-"].iter().any(|p| key.starts_with(p));
                if !looks_like_key || key.contains('\n') {
                    bail!("user.ssh_keys entry doesn't look like a single-line OpenSSH public key");
                }
            }
        }
        for pkg in &self.packages.rpm {
            validate_name(pkg, "packages.rpm", &['.', '-', '_', '+', ':'])?;
        }
        for app in &self.packages.flatpak {
            validate_name(app, "packages.flatpak", &['.', '-', '_'])?;
        }
        for formula in &self.packages.brew {
            // '@' for versioned formulae (node@22), '/' for taps (owner/tap/tool)
            validate_name(formula, "packages.brew", &['.', '-', '_', '+', '@', '/'])?;
        }
        for svc in self.services.enable.iter().chain(&self.services.disable) {
            validate_name(svc, "services", &['.', '-', '_', '@'])?;
        }
        for (name, pem) in &self.system.ca_certificates {
            // The name becomes a file name under the anchors directory.
            validate_name(name, "system.ca_certificates", &['.', '-', '_'])?;
            if !pem.contains("-----BEGIN CERTIFICATE-----")
                || !pem.contains("-----END CERTIFICATE-----")
            {
                bail!(
                    "system.ca_certificates {name:?} is not a PEM certificate \
                     (expected a -----BEGIN CERTIFICATE----- block)"
                );
            }
            // A private key here would be baked world-readable into every
            // image built from this file and pushed to a registry. It is
            // the same mistake password_hash guards against, one file
            // format over, and the shape says so plainly enough to
            // refuse rather than warn.
            if pem.contains("PRIVATE KEY") {
                bail!(
                    "system.ca_certificates {name:?} contains a private key. \
                     Anchors are public certificates; a key here would be baked \
                     world-readable into every image built from this declaration"
                );
            }
        }
        for (app, over) in &self.overrides {
            validate_name(app, "overrides", &['.', '-', '_'])?;
            if over.is_empty() {
                bail!(
                    "overrides.{app:?} declares no permissions; an empty table converges \
                     nothing and reads like it does, so say what you mean or drop it"
                );
            }
            // A permission token's alphabet is wider than a package
            // name's: `!` takes a permission away, `:ro` and `:create`
            // qualify a path, and a path is spelled with `/` and `~`.
            for (key, values) in over.context_lists() {
                let field = format!("overrides.{app}.{key}");
                for value in values {
                    // A space is in here because a path is: `home/My
                    // Documents` is an ordinary directory and flatpak
                    // takes it. Nothing here reaches a shell — the
                    // converger is this binary, writing a keyfile — so
                    // the only question a character has to answer is
                    // whether flatpak can mean it.
                    validate_name(value, &field, &['!', '/', '.', '-', '_', ':', '~', ' '])?;
                }
            }
            for (name, value) in &over.environment {
                let field = format!("overrides.{app}.environment");
                validate_name(name, &field, &['_'])?;
                // The value is written as a keyfile line, so anything
                // that could start a second line has to be refused here
                // rather than silently truncating the file on the machine.
                if value.contains('\n') || value.contains('\r') {
                    bail!("{field} value for {name:?} contains a newline");
                }
            }
            for (bus, _) in over.session_bus.iter().chain(&over.system_bus) {
                // `*` because flatpak takes `org.example.*` as a subtree.
                validate_name(bus, &format!("overrides.{app}.bus"), &['.', '-', '_', '*'])?;
            }
        }
        if self.snapshots.enable {
            let target = &self.snapshots.target;
            if !target.starts_with('/') || target.contains("..") {
                bail!("snapshots.target must be an absolute path with no `..` (got {target:?})");
            }
            validate_name(target, "snapshots.target", &['/', '.', '-', '_'])?;
            // Lands in a unit's OnCalendar=, so the systemd calendar
            // alphabet and nothing else.
            validate_name(
                &self.snapshots.interval,
                "snapshots.interval",
                &['*', '-', ':', ' ', ',', '.', '/', '~'],
            )?;
            // A retention that keeps nothing deletes each snapshot on the
            // run that takes it: busywork that looks like a backup.
            if self.snapshots.keep_recent == 0 && self.snapshots.keep_daily == 0 {
                bail!(
                    "snapshots keeps nothing: keep_recent and keep_daily are both 0, \
                     so every snapshot would be deleted by the run that took it"
                );
            }
        }
        if self.backup.enable {
            if self.backup.repo.is_empty() {
                bail!("backup.enable is set and backup.repo names nowhere to put it");
            }
            // A repository with a password in it puts the secret back in
            // the file this design exists to keep it out of, and it would
            // arrive by copy-paste from a working restic command line
            // rather than by anyone deciding to. Refused rather than
            // warned about, for the same reason a private key in
            // system.ca_certificates is: the file gets committed, and
            // baked world-readable into every image built from it.
            //
            // `sftp:user@host:/path` is ordinary and must pass, so what
            // is rejected is a colon inside the userinfo, not an `@`.
            if let Some(at) = self.backup.repo.find('@') {
                let before = &self.backup.repo[..at];
                let userinfo = match before.rsplit_once("://") {
                    Some((_, after)) => after,
                    None => before.split_once(':').map_or(before, |(_, rest)| rest),
                };
                if userinfo.contains(':') {
                    bail!(
                        "backup.repo carries a password. The repository address belongs \
                         here and the credential does not: name it with backup.secret and \
                         put the value in /var/lib/kuma/secrets/<name>.env instead"
                    );
                }
            }
            // The repository address lands inside single quotes in a
            // script that runs as root on a timer
            // (`export RESTIC_REPOSITORY='{repo}'`), so a quote in it
            // closes the string and whatever follows runs. Every other
            // declaration value reaching generated shell goes through
            // validate_name, and SECURITY.md states that as a rule with
            // no exceptions. This was the exception.
            //
            // Nobody crosses a privilege boundary today: whoever writes
            // the declaration already has root at build time through
            // packages.rpm. It matters because declarations are meant to
            // arrive from elsewhere later, and because a boundary with
            // one hole in it is not a boundary.
            //
            // The alphabet is restic's own: a scheme, a host, a port, a
            // path, and the `@` an sftp address needs.
            validate_name(
                &self.backup.repo,
                "backup.repo",
                &[':', '/', '.', '-', '_', '@', '+', '~'],
            )?;
            // Reading from a live subvolume means restic sees files
            // change under it, so the answer is "which snapshot" rather
            // than "whatever /var/home looked like across ten minutes".
            if !self.snapshots.enable {
                bail!(
                    "backup.enable requires snapshots.enable: a backup reads from a \
                     read-only snapshot so nothing changes under it mid-run"
                );
            }
            // Becomes a file name under /var/lib/kuma/secrets.
            validate_name(&self.backup.secret, "backup.secret", &['.', '-', '_'])?;
            validate_name(
                &self.backup.interval,
                "backup.interval",
                &['*', '-', ':', ' ', ',', '.', '/', '~'],
            )?;
            if self.backup.keep_daily == 0
                && self.backup.keep_weekly == 0
                && self.backup.keep_monthly == 0
            {
                bail!(
                    "backup keeps nothing: keep_daily, keep_weekly and keep_monthly are \
                     all 0, so every backup would be pruned by the run that made it"
                );
            }
            for path in &self.backup.exclude {
                if !path.starts_with('/') && !path.starts_with('~') {
                    bail!(
                        "backup.exclude {path:?} is neither absolute nor rooted at ~, and \
                         a relative exclude means whatever the unit's directory happens to be"
                    );
                }
                validate_name(path, "backup.exclude", &['/', '.', '-', '_', '~', '*', ' '])?;
            }
        }
        if self.nostr.enable {
            // The daemon's unit is a user unit under the graphical
            // session; on a declaration with no desktop it would ship
            // and never run, which is a silent pass by another name.
            // A system-level always-on bunker is a deployment mode worth
            // wanting and is tracked, not smuggled into this one.
            if matches!(self.system.desktop, Desktop::None) {
                bail!(
                    "nostr.enable requires a desktop: the bunker's unit is a user unit \\
                     under the graphical session, and a headless machine has none"
                );
            }
            for relay in &self.nostr.relays {
                validate_relay(relay)?;
            }
            match self.nostr.inactivity_lock_secs {
                None | Some(0) => {}
                Some(secs) if secs < 3600 => bail!(
                    "nostr.inactivity_lock_secs is {secs}s; the floor is one hour (3600), \\
                     because a fuse shorter than that trips on lunch"
                ),
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// A relay address, validated at build: `wss://` to the world, and
/// `ws://` only to loopback. The rule is not encryption theater —
/// NIP-44 already encrypts the payload end to end — it is metadata: a
/// plaintext WebSocket announces to the network path which app is
/// talking to which bunker, how often, and how big. A declaration that
/// leaks kind 24133 traffic by configuration is exactly the shape of
/// mistake a validator exists to catch.
///
/// This is one of two twins: the nostrconnect URI's parser restates
/// the same rule rather than link the layer into the system tool —
/// the lib's posture is that kuma does not link it — and the twin
/// tests on each side prove the same behavior.
fn validate_relay(relay: &str) -> Result<()> {
    let (scheme, rest) = match relay.split_once("://") {
        Some((scheme, rest)) => (scheme, rest),
        None => {
            bail!("nostr relay {relay:?} has no scheme; relays are wss:// or ws:// to loopback")
        }
    };
    match scheme {
        "wss" => {}
        "ws" => {
            let authority = rest.split('/').next().unwrap_or_default();
            let host = match authority.strip_prefix('[') {
                // [::1]:7777 — the bracketed form, where the port's
                // colon is not the host's.
                Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
                None => authority.split(':').next().unwrap_or(authority),
            };
            let loopback = matches!(host, "127.0.0.1" | "::1" | "localhost");
            if !loopback {
                bail!(
                    "nostr relay {relay:?} is ws:// to a non-loopback host: that is \\
                     plaintext on the wire, and the metadata alone (which app, which \\
                     bunker, how often) is not kuma's to leak. Point wss:// at it, or \\
                     ws:// at 127.0.0.1, ::1, or localhost only"
                );
            }
        }
        other => bail!(
            "nostr relay {relay:?} has scheme {other:?}; relays are wss://, or ws:// \\
             to loopback only"
        ),
    }
    Ok(())
}

/// Entries end up inside generated RUN instructions, so restrict them to a
/// conservative character set rather than trusting shell quoting.
/// A password hash that crypt can never accept has to fail here, at
/// `kuma check`, not eight minutes later on the machine.
///
/// Found by the smoke tests: every committed example shipped the
/// placeholder `password_hash = '...'`, which validated, built, and then
/// took down kuma-user-sync on first boot, because `chpasswd -e` rejects
/// it ("invalid password hash") and the sync script runs under
/// `set -euo pipefail`. The account ended up with no password and the
/// only evidence was a failed unit nobody was looking at. A declaration
/// that validates must either become a running system or fail at the
/// earliest stage, and this was neither.
///
/// Deliberately shape-only, not an allowlist of crypt ids: `$6$`, yescrypt
/// `$y$`, bcrypt `$2b$`, and whatever glibc adds next all have to keep
/// working, and rejecting a hash someone's machine already accepts would
/// be a worse bug than the one this fixes. Anything of the form
/// `$id$[params$]salt$hash` with non-empty fields passes.
fn validate_password_hash(hash: &str) -> Result<()> {
    let fields: Vec<&str> = hash.strip_prefix('$').unwrap_or("").split('$').collect();
    let well_formed =
        hash.starts_with('$') && fields.len() >= 3 && fields.iter().all(|f| !f.is_empty());
    if !well_formed {
        // The value is not echoed, deliberately: `kuma check` output is
        // pasted, and a hash that fails this check is still a secret
        // somebody meant. Naming the key says which line to fix; the
        // position in the parse error above it says where.
        bail!(
            "user.password_hash is not a crypt(5) hash (expected `$id$salt$hash`, e.g. from `kuma passwd`); \
             a placeholder here builds fine and then fails on the machine at first boot"
        );
    }
    Ok(())
}

pub(crate) fn validate_name(value: &str, field: &str, extra: &[char]) -> Result<()> {
    if value.is_empty() {
        bail!("{field} contains an empty entry");
    }
    // A leading dash turns a "name" into a flag for whatever command the
    // list feeds — dnf, systemctl, flatpak install (as root, every boot).
    // No real package, service, zone, or locale starts with one.
    if value.starts_with('-') {
        bail!("{field} entry {value:?} starts with '-': names cannot be options");
    }
    // Punctuation on its own is never a name, and two of these fields
    // turn their value into a path component: `".."` passed every other
    // rule here, validated clean, and then failed the build with an I/O
    // error about a directory. Nothing legitimate anywhere in this file
    // is spelled without a letter or a digit.
    if !value.chars().any(|c| c.is_ascii_alphanumeric()) {
        bail!("{field} entry {value:?} has no letter or digit in it, so it is not a name");
    }
    for ch in value.chars() {
        if !ch.is_ascii_alphanumeric() && !extra.contains(&ch) {
            bail!("{field} entry {value:?} contains unsupported character {ch:?}");
        }
    }
    Ok(())
}

/// Commands as a doc page gives them: `$ ` prefixed, with the trailing
/// comment (which is prose, not argument) removed, in the order a reader
/// meets them and without repeats.
///
/// Shared with main.rs, which parses the same lines against the real CLI
/// definition. One reader for both, because two would drift and the
/// thing being guarded against here is drift.
#[cfg(test)]
pub(crate) fn documented_commands(doc: &str) -> Vec<String> {
    let text = std::fs::read_to_string(format!("{}/{doc}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut seen: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("$ ") else { continue };
        // No documented command contains a literal '#', so this splits
        // off the explanatory comment and nothing else.
        let cmd = rest.split('#').next().unwrap_or("").trim().to_string();
        if !cmd.is_empty() && !seen.contains(&cmd) {
            seen.push(cmd);
        }
    }
    seen
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The examples now end in `.toml` like any declaration, which makes
    /// them indistinguishable by extension from a real one someone keeps
    /// beside them. `.gitignore` reserves exactly two shapes in that
    /// directory for local declarations, `kuma.toml` and `*.kuma.toml`;
    /// the example walkers skip both, so a personal declaration can never
    /// be pulled into a test that asserts what a *committed* example says.
    pub(crate) fn is_local_declaration(path: &Path) -> bool {
        path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("kuma.toml"))
    }
    #[test]
    fn the_inactivity_window_is_floor_validated_at_build() {
        let base = "schema_version = 1\n[system]\ndesktop = \"niri\"\n";
        let window = |nostr_tail: &str| {
            let toml = format!("{base}[nostr]\nenable = true{nostr_tail}\n");
            let config: Config = toml::from_str(&toml).unwrap();
            config.validate()
        };
        // The floor: a fuse shorter than an hour trips on lunch, and
        // the declaration that tells one is a build failure, not a
        // shrug.
        assert!(window("\ninactivity_lock_secs = 1800")
            .unwrap_err()
            .to_string()
            .contains("floor is one hour"));
        // The honest shapes: absent, the explicit off, the floor
        // itself, and a day.
        assert!(window("").is_ok());
        assert!(window("\ninactivity_lock_secs = 0").is_ok());
        assert!(window("\ninactivity_lock_secs = 3600").is_ok());
        assert!(window("\ninactivity_lock_secs = 86400").is_ok());
    }

    #[test]
    fn the_nostr_block_validates_its_relay_list_at_build() {
        let base = "schema_version = 1\n[system]\ndesktop = \"niri\"\n";
        let valid = |relays: &str| {
            let toml = format!("{base}[nostr]\nenable = true\nrelays = [{relays}]\n");
            let config: Config = toml::from_str(&toml).unwrap();
            config.validate()
        };
        assert!(valid("\"wss://relay.nip46.com\"").is_ok());
        assert!(valid("\"ws://127.0.0.1:7777\"").is_ok());
        assert!(valid("\"ws://localhost:7777\"").is_ok());
        assert!(valid("\"ws://[::1]:7777\"").is_ok());
        // Mixed sets are the normal case: local first, fallbacks after.
        assert!(valid("\"wss://relay.nip46.com\", \"ws://127.0.0.1:7777\"").is_ok());

        // The rule is metadata, not encryption theater: NIP-44 already
        // encrypts the payload, and the wire still announces which app
        // talks to which bunker, how often, and how big.
        let plaintext = valid("\"ws://relay.example.com\"").unwrap_err();
        assert!(plaintext.to_string().contains("plaintext on the wire"));
        assert!(valid("\"http://relay.example.com\"").is_err());
        assert!(valid("\"relay.example.com\"").is_err());
    }

    #[test]
    fn nostr_enable_is_refused_headless_and_defaults_to_nothing() {
        // Headless: the unit is a user unit under the graphical session,
        // so shipping it to a machine with no desktop is shipping a
        // daemon that can never run — a silent pass by another name.
        let headless: Config = toml::from_str(
            "schema_version = 1\n[nostr]\nenable = true\nrelays = [\"wss://relay.nip46.com\"]\n",
        )
        .unwrap();
        let error = headless.validate().unwrap_err();
        assert!(error.to_string().contains("requires a desktop"));

        // Absent and enable = false both ship nothing, and validate
        // without a desktop fine.
        let off: Config = toml::from_str(
            "schema_version = 1\n[nostr]\nenable = false\nrelays = [\"wss://relay.nip46.com\"]\n",
        )
        .unwrap();
        off.validate().unwrap();
        let absent: Config = toml::from_str("schema_version = 1\n").unwrap();
        absent.validate().unwrap();
        assert!(!absent.nostr.enable);
    }

    #[test]
    fn committed_examples_stay_valid() {
        // The examples are documentation that can rot; this keeps every
        // one honest: schema-valid, no real identity in any committed face.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "toml") || is_local_declaration(&path) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let config: Config =
                toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            config.validate().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            if let Some(user) = &config.user {
                assert_eq!(user.name, "me", "{}", path.display());
                // No hash in a committed example, in either direction: a
                // real one would be identity in git, and a placeholder is
                // worse than nothing since it validates here and then
                // fails on the machine (see validate_password_hash). The
                // examples show the line commented out instead.
                assert_eq!(user.password_hash, None, "{}", path.display());
            }
            checked += 1;
        }
        assert!(checked >= 2, "expected the committed examples, found {checked}");
    }

    /// The placeholder that started this: it passed every check kuma had,
    /// built an image, and then failed kuma-user-sync on first boot
    /// because chpasswd rejects it. Rejecting it at validate time is the
    /// promise ("fail at the earliest stage") being kept.
    ///
    /// The accept list matters more than the reject list here. Rejecting
    /// a hash that a machine already accepts would lock someone out of
    /// their own declaration, so anything crypt-shaped passes: every id,
    /// with or without parameters.
    #[test]
    fn password_hash_must_be_one_crypt_could_accept() {
        let hashed = |h: &str| {
            let toml =
                format!("schema_version = 1\n[user]\nname = \"me\"\npassword_hash = '{h}'\n");
            toml::from_str::<Config>(&toml).unwrap().validate()
        };

        // sha512crypt, as `openssl passwd -6` emits it
        assert!(hashed("$6$Xy1z$abcDEF.ghi/JKL0123456789").is_ok());
        // what `kuma passwd` emits: a rounds= parameter field
        assert!(hashed("$6$rounds=656000$Xy1z$abcDEF.ghi/JKL0123456789").is_ok());
        // yescrypt (Fedora's default) and bcrypt: no id allowlist here
        assert!(hashed("$y$j9T$Xy1z$abcDEF0123456789").is_ok());
        assert!(hashed("$2b$12$Xy1zabcDEF0123456789").is_ok());

        // the placeholder, and the shapes next to it
        assert!(hashed("...").is_err(), "the placeholder every example shipped");
        assert!(hashed("$6$").is_err(), "no salt, no hash");
        assert!(hashed("$6$$abcdef").is_err(), "empty salt field");
        assert!(hashed("hunter2").is_err(), "a plaintext password is not a hash");
    }

    /// The README tells people to download one filename and the release
    /// workflow publishes another, and nothing else connects the two. A
    /// rename on either side leaves an install command that 404s, which
    /// is invisible here and immediate for anyone following the front
    /// door. So the workflow's asset name is the assertion.
    ///
    /// The name is deliberately unversioned, which is what lets the
    /// README hold a `releases/latest/download/` URL at all; this pins
    /// that too, since putting the version back would break the link
    /// silently one release later.
    #[test]
    fn the_readme_downloads_what_the_release_workflow_publishes() {
        let workflow = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/.github/workflows/release.yml"
        ))
        .unwrap();
        let target = workflow
            .lines()
            .find_map(|l| l.trim().strip_prefix("TARGET: "))
            .expect("the workflow names a build target");
        let asset = format!("kuma-{target}");
        assert!(
            workflow.contains("echo \"name=kuma-${TARGET}\""),
            "the workflow builds its asset name from TARGET, unversioned"
        );

        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
        assert!(
            readme.contains(&format!("releases/latest/download/{asset}")),
            "README should download {asset} from the latest release"
        );

        // SECURITY.md quotes the verify command against the same asset, and
        // a verify command naming a file nobody has is worse than none: it
        // reads as a check that passed.
        let security =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/SECURITY.md")).unwrap();
        assert!(
            security.contains(&format!("{asset}.bundle")) && security.contains(&asset),
            "SECURITY.md should verify {asset} against its bundle"
        );
    }

    /// docs/contract.md states what 44.0 promises, and a promise page
    /// nobody links is a promise nobody can find. README's Documentation
    /// list is the front door, SECURITY.md is named there as the other
    /// half of the statement, and both directions are pinned here, along
    /// with the page still being the promises rather than a stub.
    #[test]
    fn the_contract_doc_exists_and_both_pages_name_it() {
        let contract =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/contract.md"))
                .unwrap();
        assert!(
            contract.contains("## The two promises"),
            "docs/contract.md should carry the promises themselves"
        );

        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
        assert!(
            readme.contains("docs/contract.md"),
            "README's Documentation list should link the contract"
        );

        let security =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/SECURITY.md")).unwrap();
        assert!(
            security.contains("docs/contract.md"),
            "SECURITY.md should name the contract as the other half of the statement"
        );
    }

    /// SECURITY.md says every release asset is signed with Sigstore, and
    /// that sentence has already been false once: the ISO was wired up
    /// unsigned and it took a person reading the workflow to notice. A
    /// claim in the file people read before trusting you should not
    /// depend on somebody re-reading a workflow every time an asset is
    /// added, so the workflow answers for itself here.
    ///
    /// Every file handed to `gh release create` or `gh release upload`
    /// has to be something `cosign sign-blob` signed, or one of the two
    /// files that ride along with a signed one: its checksum and its
    /// bundle.
    #[test]
    fn every_release_asset_is_signed() {
        let workflow = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/.github/workflows/release.yml"
        ))
        .unwrap();

        // What the signing steps produce a bundle for. Quoted, which is
        // what separates a command the workflow runs from the one it
        // quotes at the reader in the release notes.
        let signed: Vec<&str> = workflow
            .lines()
            .filter_map(|l| l.trim().strip_prefix("--bundle '"))
            .filter_map(|r| r.split_once(".bundle'"))
            .map(|(base, _)| base)
            .collect();
        assert!(
            !signed.is_empty(),
            "no cosign sign-blob bundle found; has the signing step moved?"
        );

        let check = |asset: &str| {
            let base = asset.trim_end_matches(".sha256").trim_end_matches(".bundle");
            assert!(
                signed.contains(&base),
                "{base} is attached to a release but nothing signs it; SECURITY.md says every release asset is signed"
            );
        };

        for line in workflow.lines() {
            let t = line.trim();

            // Assets given on the command's own line. The first quoted
            // argument to either verb is the tag, not a file.
            let cmd =
                t.strip_prefix("gh release create").or_else(|| t.strip_prefix("gh release upload"));
            if let Some(rest) = cmd {
                for asset in rest.split('\'').skip(1).step_by(2).skip(1) {
                    check(asset);
                }
                continue;
            }

            // Assets on continuation lines, one per line, which is the
            // shape both sites use today. Read without tracking which
            // command they belong to: a quoted token alone on a line is
            // an asset list or a sign-blob subject, and the file has
            // nothing else shaped that way. The alternative is following
            // backslash continuations, which the release notes heredoc
            // breaks by containing lines that end without one.
            let t = t.trim_end_matches('\\').trim();
            if let Some(asset) = t.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
                check(asset);
            }
        }
    }

    /// The same trap, one asset over, and a worse one to fall into: the
    /// ISO is what a stranger downloads first, and it is 1.8 GB of it.
    /// The walkthrough leads with that URL, so a rename on either side
    /// makes step one of getting started a 404.
    ///
    /// Unversioned for the same reason the binary is, and pinned here for
    /// the same reason: putting the version back would break every
    /// `releases/latest/download/` link silently, one release later.
    #[test]
    fn the_docs_download_the_iso_the_release_workflow_publishes() {
        let workflow = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/.github/workflows/release.yml"
        ))
        .unwrap();
        let asset = workflow
            .lines()
            .find_map(|l| l.trim().strip_prefix("name=\"").map(|r| r.trim_end_matches('"')))
            .expect("the release workflow names its ISO asset");
        assert!(asset.ends_with(".iso"), "expected an ISO asset name, got {asset}");
        assert!(
            !asset.contains("${{"),
            "the ISO asset name is unversioned, so the docs can hold a latest/download URL: {asset}"
        );

        let url = format!("releases/latest/download/{asset}");
        for doc in ["README.md", "docs/getting-started.md"] {
            let text =
                std::fs::read_to_string(format!("{}/{doc}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            assert!(text.contains(&url), "{doc} should download {asset} from the latest release");
        }

        // Same standard the binary is held to: a verify command naming a
        // file nobody has reads as a check that passed.
        let security =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/SECURITY.md")).unwrap();
        assert!(
            security.contains(&format!("{asset}.bundle")),
            "SECURITY.md should verify {asset} against its bundle"
        );
    }

    /// What executes a command the walkthrough hands somebody.
    enum Proof {
        /// A file that runs it, and the text in that file which proves
        /// it. The text is read and checked, so an entry here is
        /// evidence rather than a claim.
        Runs(&'static str, &'static str),
        /// Nothing runs it, recorded as a decision rather than left as
        /// an oversight. Shrinking this list is what turns "somebody
        /// walked the walkthrough once" into a gate.
        Unexecuted(&'static str),
    }

    /// Every command in the walkthrough, and what proves it.
    ///
    /// The walkthrough is the front door: it is what a stranger types,
    /// in order, having never seen kuma. A command in it that nothing
    /// executes is a claim, and a command that drifts from what kuma
    /// actually accepts is a 404 with extra steps.
    ///
    /// This table cannot make an unrun command run. What it does is stop
    /// the count being unknown: a new command in the docs fails this
    /// test until somebody decides which list it belongs in, and a
    /// `Runs` entry whose evidence disappears fails the moment CI stops
    /// running it.
    const WALKTHROUGH: &[(&str, Proof)] = &[
        (
            "curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-x86_64.iso",
            Proof::Unexecuted("the asset name is asserted by the sibling test; the download itself is not run"),
        ),
        (
            "sudo dd if=kuma-x86_64.iso of=/dev/sdX bs=4M status=progress",
            Proof::Unexecuted("writes a physical disk; CI boots the ISO file directly instead"),
        ),
        ("kuma install", Proof::Unexecuted("smoke.sh always passes --disk; the interactive live-media form is not run")),        ("kuma", Proof::Unexecuted("the bare status line is not run anywhere")),
        ("kuma init", Proof::Unexecuted("nothing runs it; the starter declaration is only read as a fixture")),
        (
            "curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-x86_64-unknown-linux-musl",
            Proof::Unexecuted("the asset name is asserted by the sibling test; the download itself is not run"),
        ),
        ("chmod +x kuma-x86_64-unknown-linux-musl", Proof::Unexecuted("plain shell, nothing of kuma's to prove")),
        ("sudo mv kuma-x86_64-unknown-linux-musl /usr/local/bin/kuma", Proof::Unexecuted("plain shell, nothing of kuma's to prove")),
        // The nostr layer's pair, the release carrying them beside kuma
        // the same way the image build stages them from beside it.
        (
            "curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-nostrd-x86_64-unknown-linux-musl",
            Proof::Unexecuted("the asset name is asserted by the sibling test; the download itself is not run"),
        ),
        (
            "curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-nostr-x86_64-unknown-linux-musl",
            Proof::Unexecuted("the asset name is asserted by the sibling test; the download itself is not run"),
        ),
        ("chmod +x kuma-nostrd-x86_64-unknown-linux-musl kuma-nostr-x86_64-unknown-linux-musl", Proof::Unexecuted("plain shell, nothing of kuma's to prove")),
        ("sudo mv kuma-nostrd-x86_64-unknown-linux-musl /usr/local/bin/kuma-nostrd", Proof::Unexecuted("plain shell, nothing of kuma's to prove")),
        ("sudo mv kuma-nostr-x86_64-unknown-linux-musl /usr/local/bin/kuma-nostr", Proof::Unexecuted("plain shell, nothing of kuma's to prove")),
        // Both of these the dead-disk stage runs for real, which is the
        // only place in the project where "a backup works" is a fact
        // rather than a claim: it seeds a repository, destroys the disk,
        // and installs a machine back out of it.
        (
            "sudo kuma backup --init",
            Proof::Runs("scripts/smoke.sh", "sudoq \"kuma backup --init\""),
        ),
        (
            "sudo kuma install --disk /dev/nvme0n1 --restore recovery.env",
            Proof::Runs("scripts/smoke.sh", "restore_args=(--restore \"$restore\")"),
        ),
        ("kuma check", Proof::Runs("scripts/smoke.sh", "\"$KUMA\" --config \"$file\" check")),
        ("kuma build", Proof::Runs("scripts/smoke.sh", "\"$KUMA\" --config \"$file\" build --tag")),
        ("kuma switch --yes", Proof::Unexecuted("mutates a deployment; nothing runs it, and it is how a built image is taken")),
        // smoke.sh runs `vm --no-run --rebuild` and then boots the disk
        // with qemu itself, so kuma's own boot path is the half nothing
        // covers, and that half is what the walkthrough is selling.
        ("kuma vm", Proof::Unexecuted("smoke.sh runs vm --no-run to build a disk and boots it with qemu directly")),
        ("kuma vm --apply", Proof::Unexecuted("smoke.sh runs vm --no-run; nothing exercises --apply")),
        ("kuma iso --live", Proof::Runs("scripts/smoke.sh", "iso --live --tag")),
        (
            "kuma install --image ghcr.io/<owner>/kuma:<tag>",
            Proof::Runs("scripts/smoke.sh", "install --disk \"$raw\" --image"),
        ),
        ("kuma doctor", Proof::Runs("scripts/smoke.sh", "kuma doctor --json")),
        ("kuma update --check", Proof::Runs("scripts/smoke.sh", "update --check")),
        ("kuma update --yes", Proof::Unexecuted("builds and stages a deployment; nothing runs it")),
        ("kuma rollback --yes", Proof::Unexecuted("mutates the boot order; nothing runs it")),
        // The installer's half of hibernate is proven for real: the
        // install stage asks for a swapfile and then compares the offset
        // in the boot entry against the offset btrfs reports for the file
        // it points at, which is the one disagreement that loses a
        // session without logging anything. The verb's half is not: it
        // needs a booted machine to run against and a reboot to prove,
        // and neither of those is what smoke_install has.
        (
            "kuma hibernate",
            Proof::Unexecuted("the dry run needs a booted kuma machine; smoke_install writes a disk without booting it"),
        ),
        (
            "kuma hibernate --yes",
            Proof::Unexecuted("makes a swapfile and stages kernel arguments on a live machine; the installer path that writes the same file and the same arguments is covered by smoke_install"),
        ),
        (
            "kuma hibernate --off --yes",
            Proof::Unexecuted("nothing runs it; it undoes a state no stage reaches"),
        ),
        (
            "kuma-nostr setup",
            Proof::Unexecuted(
                "this machine's vault predates the release and a second setup is refused by \
                 design; the daemon-side verb is covered by the protocol tests",
            ),
        ),
        (
            "kuma-nostr bunker --qr",
            Proof::Unexecuted(
                "minted with --json on a live machine this session; --qr is the sibling \
                 render of the same verb",
            ),
        ),
        (
            "kuma-nostr connect <uri>",
            Proof::Unexecuted(
                "needs a client's own nostrconnect:// invite; the daemon-side verb is \
                 covered end to end by the socket tests over a real relay",
            ),
        ),
        ("kuma-nostr prompts", Proof::Unexecuted("run as --json this session; the plain form is the same verb without the document")),
        ("kuma-nostr log", Proof::Unexecuted("run as --json by the panel's Activity tab; the plain form is the same verb without the document")),
        (
            "kuma-nostr approve <id>",
            Proof::Unexecuted(
                "this session's prompts were answered through the panel; the CLI form is \
                 the same verb in front of the same engine",
            ),
        ),
    ];

    /// Every verb is named somewhere a person reads.
    ///
    /// The sibling test walks the docs and checks each command against
    /// the CLI. This walks it the other way, which is the direction a
    /// new verb goes missing in: a verb once shipped, worked, and was
    /// documented nowhere, and every test passed. A verb nobody can find
    /// is a feature nobody has.
    ///
    /// The exceptions are named rather than filtered by a rule, so that
    /// adding one is a decision somebody makes on purpose.
    #[test]
    fn every_verb_is_documented_somewhere() {
        use clap::CommandFactory;
        /// Whether the docs name `kuma <verb>` as a command rather than
        /// as the first two words of a longer one. "kuma builds an
        /// image" is prose about the tool, and a substring match cannot
        /// tell it from the verb `build` being documented.
        fn names_command(docs: &str, verb: &str) -> bool {
            let needle = format!("kuma {verb}");
            docs.match_indices(&needle).any(|(at, _)| {
                docs[at + needle.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric() && next != '-' && next != '_')
            })
        }
        /// Verbs deliberately absent from the prose.
        const UNDOCUMENTED: &[(&str, &str)] = &[
            (
                "boot-titles",
                "hidden; kuma-boot-titles.service runs it and nothing asks a person to",
            ),
            (
                "flatpak-overrides",
                "hidden; two units and `kuma sync` run it, and the feature it converges \
                 is documented as [overrides] rather than as a command",
            ),
        ];
        let docs: String = [
            "README.md",
            "docs/getting-started.md",
            "docs/concepts.md",
            "docs/desktops.md",
            "docs/agents.md",
        ]
        .iter()
        .map(|name| {
            std::fs::read_to_string(format!("{}/{name}", env!("CARGO_MANIFEST_DIR")))
                .unwrap_or_default()
        })
        .collect();
        // The matcher, proven on prose before it is trusted on the
        // pages: every verb passes today, so nothing about running it
        // over the docs could show it apart.
        assert!(names_command("run `kuma build` first", "build"), "a command is a mention");
        assert!(
            !names_command("kuma builds an image from it", "build"),
            "prose about the tool is not the verb being documented"
        );

        let mut missing: Vec<String> = Vec::new();
        for sub in crate::Cli::command().get_subcommands() {
            let verb = sub.get_name();
            if UNDOCUMENTED.iter().any(|(name, _)| *name == verb) {
                continue;
            }
            if !names_command(&docs, verb) {
                missing.push(verb.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "these verbs exist and no page mentions them: {}",
            missing.join(", ")
        );
    }

    /// Commands as the walkthrough gives them: `$ ` prefixed, with the
    /// trailing comment (which is prose, not argument) removed.
    fn walkthrough_commands() -> Vec<String> {
        super::documented_commands("docs/getting-started.md")
    }

    /// Publishing gates the mutable tag on the validation. The laps
    /// install and boot the digest-pinned staging ref this run pushed;
    /// the promote job moves the tags people pull only if every lap came
    /// back green. Two of the first three publishes failed their
    /// validations after the tags were already live (44.3.0's
    /// signatures, 44.6.0's greeter) — under the old order the laps
    /// could tell somebody the same day, but what they said was that
    /// the broken thing was public. Composes of one commit are not
    /// byte-stable (src/lock.rs: layered packages resolve live on
    /// purpose), so gating on "the same image built again" is not an
    /// option: the bytes that go live must be the bytes that were
    /// validated.
    #[test]
    fn publishing_triggers_the_workflow_that_validates_what_was_published() {
        let read = |name: &str| {
            std::fs::read_to_string(format!(
                "{}/.github/workflows/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let publish = read("publish.yml");
        assert!(
            publish.contains("uses: ./.github/workflows/published.yml"),
            "publish.yml should call the workflow that validates what it published"
        );
        assert!(
            publish.contains("needs: publish"),
            "the validation runs after the staging push, so the laps see this run's bytes"
        );
        assert!(
            publish
                .contains("image: ${{ needs.publish.outputs.staging }}@${{ needs.publish.outputs.digest }}"),
            "the validation should install the digest-pinned staging ref, not a default and not an unpinned tag: \
             a tag can move under a concurrent publish and put different bytes under this run's verdict"
        );
        assert!(
            publish.contains("needs: [publish, validate]"),
            "the promote job waits on the validation: the tags move only when every lap is green"
        );
        assert!(
            publish.contains("crane tag"),
            "the promotion re-points the tags at the validated digest without moving bytes, \
             so the digest the signature names is the digest the tags serve"
        );
        assert!(
            read("published.yml").contains("workflow_call:"),
            "published.yml has to be callable for publish.yml to call it"
        );
    }

    /// The half of "your declaration keeps working" that can be checked
    /// today.
    ///
    /// Schema v1 is claimed permanent, and until now nothing held anyone
    /// to it: every field could be renamed, every default changed, and
    /// the only thing that would notice is somebody upgrading a machine
    /// whose declaration predates the change. `tests/declarations/` holds
    /// every distinct shape this project has ever shipped as an example,
    /// named for the release that introduced it, and all of them have to
    /// parse and validate on the kuma being built now.
    ///
    /// The examples are a proxy for declarations nobody here can see.
    /// They are the ones that shipped as documentation, so they are the
    /// ones a stranger copied.
    ///
    /// This says nothing about the other direction, a declaration from a
    /// newer kuma read by an older one, which is a hard parse error today
    /// (`deny_unknown_fields`) and the design question 0.14 exists for.
    #[test]
    fn every_declaration_this_project_ever_shipped_still_works() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/declarations");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).expect("the corpus directory exists").flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let config: Config =
                toml::from_str(&text).unwrap_or_else(|e| panic!("{name} no longer parses: {e}"));
            config.validate().unwrap_or_else(|e| panic!("{name} no longer validates: {e}"));
            checked += 1;
        }
        assert!(checked >= 7, "the corpus should hold every shape that shipped, found {checked}");

        // What keeps the corpus from rotting: whatever ships today is
        // what somebody copies tomorrow, so it has to be in here before
        // it becomes history.
        for example in
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/examples")).unwrap().flatten()
        {
            let path = example.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            // `.gitignore` keeps `examples/*.kuma.toml` and
            // `examples/kuma.toml` out of the repo: those are somebody's
            // own machine, sitting in the same directory as the ones that
            // ship. Reading them would fail this test on the machine that
            // has one and pass in CI, which is worse than either.
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name == "kuma.toml" || name.ends_with(".kuma.toml") {
                continue;
            }
            let current = std::fs::read_to_string(&path).unwrap();
            let in_corpus = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .any(|f| std::fs::read_to_string(f.path()).map(|t| t == current).unwrap_or(false));
            assert!(
                in_corpus,
                "{} changed and is not in tests/declarations/; copy it there as \
                 <next-version>-<name>.toml so it keeps parsing after it ships",
                path.display()
            );
        }
    }

    /// The gate item of 0.12, and the honest version of "somebody walked
    /// the walkthrough and it worked".
    #[test]
    fn every_command_in_the_walkthrough_is_executed_or_named_as_unexecuted() {
        let documented = walkthrough_commands();
        assert!(
            documented.len() > 10,
            "the walkthrough should hand somebody more than a handful of commands"
        );

        for cmd in &documented {
            assert!(
                WALKTHROUGH.iter().any(|(known, _)| known == cmd),
                "the walkthrough gained a command nothing has decided about: {cmd}\n\
                 add it to WALKTHROUGH as Runs(file, evidence) or Unexecuted(reason)"
            );
        }
        for (known, _) in WALKTHROUGH {
            assert!(
                documented.iter().any(|cmd| cmd == known),
                "WALKTHROUGH names a command the docs no longer give: {known}"
            );
        }

        // Every claim of coverage is read back out of the file that
        // makes it, so this cannot decay into a list of good intentions.
        let mut executed = 0;
        for (cmd, proof) in WALKTHROUGH {
            match proof {
                Proof::Runs(file, evidence) => {
                    let text =
                        std::fs::read_to_string(format!("{}/{file}", env!("CARGO_MANIFEST_DIR")))
                            .unwrap_or_else(|_| {
                                panic!("{cmd} claims {file} runs it, and there is no such file")
                            });
                    assert!(
                        text.contains(evidence),
                        "{cmd} claims {file} runs it, but {file} no longer contains: {evidence}"
                    );
                    executed += 1;
                }
                Proof::Unexecuted(reason) => {
                    assert!(!reason.trim().is_empty(), "{cmd} is unexecuted for no stated reason");
                }
            }
        }
        assert!(executed > 0, "nothing in the walkthrough is executed by anything");
    }

    /// Everything on a kuma machine that outlives an image update, and
    /// what kuma does about each kind of it.
    ///
    /// This is the sweep that opened 0.13, turned into something that
    /// cannot rot. The finding it came from is that `/etc` alone carried
    /// 81 files the image never shipped, from a unit enabled with no
    /// unit file to a distribution's leftovers, and every one of them was
    /// outside every surface kuma had. The answer was not to declare all
    /// of it. It was to decide, per kind, whether kuma owns it, reports
    /// it, or deliberately leaves it, and then to write the third one
    /// down where a person can read it.
    ///
    /// **There is no fourth arm, and that is the test.** A kind of state
    /// nobody classified is the bug this rung exists to end, so a new
    /// section in the declaration fails here until it has a row, and a
    /// row claiming the docs say something fails until they do.
    #[test]
    fn every_kind_of_persistent_state_is_classified() {
        /// Where a kind of state is answered.
        enum Owned {
            /// The declaration owns it; names the section that does.
            Declared(&'static str),
            /// `kuma doctor` reports it; names the label it grades under.
            Graded(&'static str),
            /// Deliberately outside; names a phrase the docs must carry,
            /// because "out of scope" is only honest when it is written
            /// somewhere a person will find it.
            OutOfScope(&'static str),
        }
        use Owned::*;
        const STATE: &[(&str, Owned)] = &[
            ("flatpak permission overrides", Declared("overrides")),
            ("installed flatpaks", Declared("packages")),
            ("brew formulae", Declared("packages")),
            ("rpm packages", Declared("packages")),
            ("system unit enablement", Declared("services")),
            ("timezone, locale, hostname, desktop, trusted CAs", Declared("system")),
            ("the declared account", Declared("user")),
            ("local btrfs snapshots", Declared("snapshots")),
            ("offsite copies of what snapshots keep", Declared("backup")),
            // The nostr layer's identity: the vault lives in the login
            // keyring (user state, not the image's), the pairings and
            // the activity log in the daemon's own state dir — all of
            // it user state an image update does not touch.
            ("the nostr vault and pairings", Declared("nostr")),
            // New with [backup], and a kind of its own: a secret the
            // declaration names and deliberately does not hold, sitting
            // in /var where it survives every rebuild. Graded rather
            // than declared, because what can be checked is whether the
            // machine has the thing its declaration asked for.
            ("a credential the declaration names", Graded("backup")),
            ("/etc files this image owns", Graded("etc")),
            ("a unit enabled with no unit file", Graded("enablement")),
            ("a flatpak override pointing at nothing", Graded("flatpak overrides")),
            ("/etc files the image never shipped", OutOfScope("never shipped")),
            ("dconf and GSettings", OutOfScope("dconf")),
            ("the portal permission store", OutOfScope("portal permission")),
            ("units in a user's own manager", OutOfScope("systemd --user")),
            ("network connections and their secrets", OutOfScope("NetworkManager")),
            ("machine identity", OutOfScope("host keys")),
            ("home directories", OutOfScope("/var/home")),
            ("device pairings", OutOfScope("pairings")),
        ];

        let schema = serde_json::to_value(schemars::schema_for!(Config)).unwrap();
        let properties = schema["properties"].as_object().expect("the schema has properties");
        let doctor =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/inspect.rs"))
                .unwrap();
        // Only the section that exists to say what is out of scope
        // counts. A word that happens to appear elsewhere on the page is
        // not the docs telling anyone anything, and scoping the search
        // is what stops this from passing on a coincidence.
        let page =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/concepts.md"))
                .unwrap();
        const HEADING: &str = "## What a declaration does not reproduce";
        let docs = page
            .split_once(HEADING)
            .map(|(_, rest)| rest.split("\n## ").next().unwrap_or(rest).to_string())
            .unwrap_or_else(|| panic!("docs/concepts.md has no `{HEADING}` section"));

        for (kind, owned) in STATE {
            match owned {
                Declared(section) => assert!(
                    properties.contains_key(*section),
                    "{kind} claims [{section}] declares it, and the schema has no such section"
                ),
                // The label as doctor writes it, quotes included, so a
                // renamed check fails here rather than leaving a kind of
                // state claiming a grade nobody emits.
                Graded(label) => assert!(
                    doctor.contains(&format!("\"{label}\"")),
                    "{kind} claims doctor grades it as `{label}`, and no check reports under that name"
                ),
                OutOfScope(phrase) => assert!(
                    docs.contains(*phrase),
                    "{kind} is called out of scope, and the section that exists to say so \
                     never mentions {phrase:?}"
                ),
            }
        }

        // The anti-rot half: a new section in the declaration is a new
        // kind of state, and it does not get to arrive unclassified.
        for section in properties.keys() {
            if section == "schema_version" {
                continue;
            }
            assert!(
                STATE.iter().any(|(_, owned)| matches!(owned, Declared(s) if s == section)),
                "[{section}] is in the schema and nothing here says what it covers"
            );
        }
    }

    /// The release workflow pulls a tag's notes out of CHANGELOG.md and
    /// refuses to publish without them, which is what stops the file
    /// rotting. That check runs after the tag exists, and a tag is the one
    /// thing here that cannot be taken back, so the same question is asked
    /// where a version bump is still a working tree: the bump moves
    /// Cargo.toml, Cargo.lock, and the changelog together or it fails.
    #[test]
    fn the_changelog_has_a_section_for_this_version() {
        let changelog =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/CHANGELOG.md")).unwrap();
        let heading = format!("## v{}", env!("CARGO_PKG_VERSION"));
        assert!(
            changelog.lines().any(|l| l == heading || l.starts_with(&format!("{heading} "))),
            "CHANGELOG.md needs a `{heading}` section before this version can be released"
        );
        // Where the next release's notes accumulate. Without the heading
        // there is nowhere for an entry to land in the same push as its
        // change, and a section written at tag time is written from memory.
        assert!(
            changelog.lines().any(|l| l == "## Unreleased"),
            "CHANGELOG.md needs an `## Unreleased` section for the next release's notes"
        );
    }

    /// The README's example declaration is the first thing anyone copies,
    /// so it is held to the same bar as the committed examples. It has
    /// been wrong before: it showed `password_hash = '...'` for months,
    /// which is a placeholder the parser now rejects precisely because it
    /// built an image that failed on first boot.
    #[test]
    fn the_readme_example_is_a_valid_declaration() {
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
        let block = readme
            .split_once("```toml\n")
            .and_then(|(_, rest)| rest.split_once("```"))
            .map(|(block, _)| block)
            .expect("README has a ```toml example");
        let config: Config = toml::from_str(block).expect("README example parses");
        config.validate().expect("README example validates");
        assert!(config.user.is_some(), "the example shows a declared [user]");
    }

    /// Getting-started's example is the one a newcomer copies, which makes
    /// it the worst place for a declaration that does not build. The
    /// README's is read; this one is typed.
    #[test]
    fn the_getting_started_example_is_a_valid_declaration() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/getting-started.md"
        ))
        .unwrap();
        let block = doc
            .split_once("```toml\n")
            .and_then(|(_, rest)| rest.split_once("```"))
            .map(|(block, _)| block)
            .expect("getting-started has a ```toml example");
        let config: Config = toml::from_str(block).expect("getting-started example parses");
        config.validate().expect("getting-started example validates");

        // The walkthrough tells the reader to run `kuma passwd` and paste
        // the result in, so the example must not already carry one: a
        // published hash in the document everyone copies is the one place
        // it would spread furthest.
        assert!(
            config.user.as_ref().is_some_and(|u| u.password_hash.is_none()),
            "the walkthrough's example must declare a user and no password hash"
        );
    }

    /// niri.toml says it shows every field the schema accepts,
    /// which is a promise a reader can't check and a schema change can
    /// quietly break. `services.disable` and `system.brew` were both
    /// missing when this was written.
    ///
    /// Commented-out fields count: an optional field is documented by
    /// showing its shape, not by being switched on in the example.
    #[test]
    fn the_full_example_documents_every_field() {
        let schema = serde_json::to_value(schemars::schema_for!(Config)).unwrap();
        let example =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/niri.toml"))
                .unwrap();

        // Field names from the schema itself, so a new one is covered the
        // day it lands rather than the day someone remembers.
        let mut fields: Vec<String> = Vec::new();
        let mut collect = |object: &serde_json::Value| {
            if let Some(props) = object["properties"].as_object() {
                fields.extend(props.keys().cloned());
            }
        };
        collect(&schema);
        if let Some(defs) = schema["$defs"].as_object() {
            for definition in defs.values() {
                collect(definition);
            }
        }
        assert!(fields.len() > 10, "schema walk found only {fields:?}");

        let mut documented_names: std::collections::BTreeSet<String> = Default::default();
        for line in example.lines() {
            let line = line.trim_start().trim_start_matches('#').trim_start();
            if let Some(header) = line.strip_prefix('[').and_then(|rest| rest.split(']').next()) {
                for segment in header.split('.') {
                    let name = segment.trim().trim_matches('"');
                    if !name.is_empty() {
                        documented_names.insert(name.to_string());
                    }
                }
                continue;
            }
            if let Some((key, _)) = line.split_once('=') {
                let key = key.trim().trim_matches('"');
                if !key.is_empty() {
                    documented_names.insert(key.to_string());
                }
            }
        }

        for field in &fields {
            // Every name the example spells anywhere: the key on the
            // left of an `=`, and each segment of a table header, so a
            // field nested two deep (`[system.ca_certificates]`) counts
            // exactly like a top-level one. Commented lines count too:
            // an optional field is documented by showing its shape, not
            // by being switched on.
            //
            // Quoted names inside a header split into segments as well,
            // so `[overrides."org.example.App"]` contributes `org`,
            // `example` and `App`. Harmless, because no schema field is
            // spelled like a fragment of a reverse-DNS id.
            let documented = documented_names.contains(field);
            assert!(documented, "examples/niri.toml never mentions `{field}`");
        }
    }

    /// Two fields turn their value straight into a path component: an
    /// override's app id and a CA anchor's name. `".."` satisfied every
    /// other rule, validated clean, and then failed the build with an
    /// I/O error about a directory rather than a message anyone could
    /// act on. The rule is general because the reason is: nothing in
    /// this file is legitimately spelled without a letter or a digit.
    #[test]
    fn a_name_made_only_of_punctuation_is_refused_before_it_becomes_a_path() {
        let dots = "schema_version = 1\n[overrides.\"..\"]\nsockets = [\"wayland\"]\n";
        let config: Config = toml::from_str(dots).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("not a name"), "{err}");

        let anchor = "schema_version = 1\n[system.ca_certificates]\n\
                      \".\" = \"\"\"\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n\"\"\"\n";
        let config: Config = toml::from_str(anchor).unwrap();
        assert!(config.validate().is_err());

        // and the ordinary shapes still pass, including the dotted ones
        for good in ["org.mozilla.firefox", "my-root-ca", "America/Denver", "en_US.UTF-8"] {
            validate_name(good, "probe", &['.', '-', '_', '/']).unwrap();
        }
    }

    /// An anchor is public by construction, which is the whole reason
    /// this key can exist in a file that gets committed and baked
    /// world-readable. A private key pasted here would be that reasoning
    /// turned inside out, so the shape is checked rather than trusted.
    #[test]
    fn a_ca_anchor_must_be_a_certificate_and_never_a_key() {
        let with = |body: &str| {
            format!("schema_version = 1\n[system.ca_certificates]\nmine = \"\"\"\n{body}\"\"\"\n")
        };
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
        let config: Config = toml::from_str(&with(cert)).unwrap();
        config.validate().unwrap();

        let config: Config = toml::from_str(&with("not a certificate at all\n")).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("PEM certificate"), "{err}");

        let key = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n\
                   -----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n";
        let config: Config = toml::from_str(&with(key)).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("private key"), "{err}");
        assert!(err.contains("world-readable"), "the refusal says why: {err}");
    }

    /// The whole point of naming the credential is that the credential
    /// is not in the file, and the way that gets undone is not by anyone
    /// deciding to: it is by pasting a restic command line that already
    /// The address lands in single quotes inside a root script, so a
    /// quote in it closes the string. Every other value that reaches
    /// generated shell is validated; this was the one that was not.
    #[test]
    fn a_repo_that_could_close_its_own_quoting_is_refused() {
        let with = |repo: &str| {
            let toml = format!(
                "schema_version = 1\n[snapshots]\nenable = true\n\
                 [backup]\nenable = true\nrepo = {repo:?}\n"
            );
            toml::from_str::<Config>(&toml).unwrap().validate()
        };
        for hostile in
            ["b2:kuma'; touch /tmp/pwned; :'", "b2:kuma$(id)", "b2:kuma`id`", "--kuma", "b2:k uma"]
        {
            assert!(with(hostile).is_err(), "should be refused: {hostile:?}");
        }
        for real in [
            "s3:https://minio.example:9000/kuma",
            "sftp:user@host:/srv/kuma",
            "rclone:start9:kuma",
            "b2:kuma-backups",
            "/mnt/usb/kuma",
        ] {
            with(real).unwrap_or_else(|e| panic!("{real} should validate: {e}"));
        }
    }

    /// worked. `sftp:user@host:/path` is ordinary and has to keep
    /// passing, so what is refused is a colon inside the userinfo.
    #[test]
    fn a_repo_carrying_its_own_password_is_refused() {
        let with_password = |repo: &str| {
            let toml = format!(
                "schema_version = 1\n[snapshots]\nenable = true\n\
                 [backup]\nenable = true\nrepo = {repo:?}\n"
            );
            toml::from_str::<Config>(&toml).unwrap().validate()
        };

        for repo in
            ["s3:https://KEY:SECRET@minio.example:9000/kuma", "sftp:user:hunter2@host:/srv/kuma"]
        {
            let err = with_password(repo).unwrap_err().to_string();
            assert!(err.contains("password"), "{repo}: {err}");
            assert!(err.contains("backup.secret"), "the refusal says where it goes: {err}");
        }

        // An `@` is not the tell. These are how people legitimately
        // address a repository and every one has to survive.
        for repo in [
            "sftp:user@host:/srv/kuma",
            "s3:https://minio.example:9000/kuma",
            "rclone:start9:kuma",
            "b2:kuma-backups",
        ] {
            with_password(repo).unwrap_or_else(|e| panic!("{repo} should validate: {e}"));
        }
    }

    /// Reading from a live subvolume means restic sees files change
    /// underneath it, so a backup has to name which snapshot it copied.
    /// Refused at `kuma check`, where a person is looking, rather than
    /// at 3am inside a unit nobody reads.
    #[test]
    fn a_backup_without_snapshots_is_refused() {
        let no_snapshots: Config =
            toml::from_str("schema_version = 1\n[backup]\nenable = true\nrepo = \"b2:kuma\"\n")
                .unwrap();
        let err = no_snapshots.validate().unwrap_err().to_string();
        assert!(err.contains("snapshots.enable"), "{err}");

        let nowhere: Config = toml::from_str(
            "schema_version = 1\n[snapshots]\nenable = true\n[backup]\nenable = true\n",
        )
        .unwrap();
        assert!(nowhere.validate().is_err(), "enable with no repo names nowhere to put it");

        // And the whole section disabled is nobody's problem: a machine
        // that never backs up must not fail a build over it.
        let disabled: Config =
            toml::from_str("schema_version = 1\n[backup]\nrepo = \"\"\n").unwrap();
        disabled.validate().unwrap();
    }

    /// Same shape as the snapshots rule beside it, and the same reason:
    /// a policy that prunes everything is busywork wearing a backup's
    /// clothes. Three tiers rather than two, because restic's forget
    /// takes daily/weekly/monthly.
    #[test]
    fn a_backup_retention_that_keeps_nothing_is_rejected() {
        let head = "schema_version = 1\n[snapshots]\nenable = true\n\
                    [backup]\nenable = true\nrepo = \"b2:kuma\"\n";
        let keeps_nothing: Config =
            toml::from_str(&format!("{head}keep_daily = 0\nkeep_weekly = 0\nkeep_monthly = 0\n"))
                .unwrap();
        assert!(keeps_nothing.validate().is_err());

        let one_tier: Config =
            toml::from_str(&format!("{head}keep_daily = 0\nkeep_weekly = 0\nkeep_monthly = 1\n"))
                .unwrap();
        one_tier.validate().unwrap();
    }

    /// An exclude is handed to restic, which resolves a relative path
    /// against whatever directory the unit happens to be in. That is not
    /// a thing anyone means, and it fails by quietly excluding nothing.
    #[test]
    fn a_relative_exclude_is_refused() {
        let head = "schema_version = 1\n[snapshots]\nenable = true\n\
                    [backup]\nenable = true\nrepo = \"b2:kuma\"\n";
        let relative: Config = toml::from_str(&format!("{head}exclude = [\"Videos\"]\n")).unwrap();
        assert!(relative.validate().is_err());

        let rooted: Config =
            toml::from_str(&format!("{head}exclude = [\"~/Videos\", \"/var/home/x/.cache\"]\n"))
                .unwrap();
        rooted.validate().unwrap();
    }

    /// A retention that keeps nothing would delete each snapshot on the
    /// run that took it: an expensive way to look protected.
    #[test]
    fn a_retention_that_keeps_nothing_is_rejected() {
        let keeps_nothing: Config = toml::from_str(
            "schema_version = 1\n[snapshots]\nenable = true\nkeep_recent = 0\nkeep_daily = 0\n",
        )
        .unwrap();
        assert!(keeps_nothing.validate().is_err());

        // one tier is enough; and the same policy disabled is nobody's
        // problem, so it must not fail a build that never snapshots
        let one_tier: Config = toml::from_str(
            "schema_version = 1\n[snapshots]\nenable = true\nkeep_recent = 0\nkeep_daily = 1\n",
        )
        .unwrap();
        one_tier.validate().unwrap();
        let disabled: Config =
            toml::from_str("schema_version = 1\n[snapshots]\nkeep_recent = 0\nkeep_daily = 0\n")
                .unwrap();
        disabled.validate().unwrap();
    }

    /// The target is baked into a root script that deletes subvolumes, so
    /// path traversal and relative paths are refused at `kuma check`.
    #[test]
    fn a_snapshot_target_must_be_an_absolute_path() {
        for bad in ["/var/home/../etc", "var/home", "/var/home; rm -rf /"] {
            let config: Config = toml::from_str(&format!(
                "schema_version = 1\n[snapshots]\nenable = true\ntarget = \"{bad}\"\n"
            ))
            .unwrap();
            assert!(config.validate().is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn minimal_config_parses_with_defaults() {
        let config: Config = toml::from_str("schema_version = 1").unwrap();
        config.validate().unwrap();
        // The default base is kuma's own composed one, not fedora-bootc.
        assert_eq!(config.system.base, None);
        assert!(config.base_ref().starts_with("localhost/kuma-base:m"));
        assert!(config.packages.rpm.is_empty());
    }

    #[test]
    fn firmware_trim_is_validated() {
        let unknown: Config =
            toml::from_str("schema_version = 1\n[system]\nfirmware = [\"warp-core-firmware\"]\n")
                .unwrap();
        assert!(unknown.validate().is_err());
        let with_image_base: Config = toml::from_str(
            "schema_version = 1\n[system]\nbase = \"quay.io/x/y:1\"\nfirmware = [\"amd-gpu-firmware\"]\n",
        )
        .unwrap();
        assert!(with_image_base.validate().is_err());
        let good: Config = toml::from_str(
            "schema_version = 1\n[system]\nfirmware = [\"amd-gpu-firmware\", \"mt7xxx-firmware\"]\n",
        )
        .unwrap();
        good.validate().unwrap();
    }

    #[test]
    fn wrong_schema_version_rejected() {
        let config: Config = toml::from_str("schema_version = 2").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn leading_dash_option_injection_rejected() {
        // "--nogpgcheck" would ride into `dnf -y install` as a flag; the
        // flatpak list feeds root-run `flatpak install` on every boot
        for toml in [
            "schema_version = 1\n[packages]\nrpm = [\"--nogpgcheck\"]\n",
            "schema_version = 1\n[packages]\nflatpak = [\"--reinstall\"]\n",
            "schema_version = 1\n[services]\nenable = [\"--global\"]\n",
        ] {
            let config: Config = toml::from_str(toml).unwrap();
            assert!(config.validate().is_err(), "should reject: {toml}");
        }
    }

    #[test]
    fn shell_metacharacters_rejected() {
        let config: Config =
            toml::from_str("schema_version = 1\n[packages]\nrpm = [\"fish; rm -rf /\"]\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn brew_formula_names_validated() {
        let config: Config = toml::from_str(
            "schema_version = 1\n[packages]\nbrew = [\"node@22\", \"oven-sh/bun/bun\"]\n",
        )
        .unwrap();
        config.validate().unwrap();

        let config: Config =
            toml::from_str("schema_version = 1\n[packages]\nbrew = [\"rg $(id)\"]\n").unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_fields_rejected() {
        let result: Result<Config, _> =
            toml::from_str("schema_version = 1\n[packages]\ndnf = [\"fish\"]\n");
        assert!(result.is_err());
    }

    #[test]
    fn user_defaults_and_validation() {
        let config: Config =
            toml::from_str("schema_version = 1\n[user]\nname = \"mira\"\n").unwrap();
        config.validate().unwrap();
        assert_eq!(config.user.unwrap().groups, vec!["wheel"]);

        // hash outside the crypt(5) alphabet (it gets single-quoted into a
        // sourced shell file) is rejected
        let config: Config = toml::from_str(
            "schema_version = 1\n[user]\nname = \"m\"\npassword_hash = \"$6$a b\"\n",
        )
        .unwrap();
        assert!(config.validate().is_err());

        let config: Config = toml::from_str(
            "schema_version = 1\n[user]\nname = \"m\"\nssh_keys = [\"not-a-key\"]\n",
        )
        .unwrap();
        assert!(config.validate().is_err());
    }
}
