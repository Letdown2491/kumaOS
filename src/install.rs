//! Installing kuma onto a disk.
//!
//! The account is the whole difficulty. A published image declares no
//! `[user]`, because the image is shared and the person is not, so a
//! machine installed from one has no account and no root password and no
//! way in. Anaconda's create-a-user screen used to cover that, and live
//! media has no Anaconda.
//!
//! So the installer's real output is a declaration. It asks, writes
//! `/var/lib/kuma/user` on the target, and `kuma-user-sync` creates the
//! account at first boot exactly as it does for a declared one. The
//! installer does not create users; it writes down what the machine
//! should converge to, which is the same thing kuma does everywhere else.
//!
//! Getting that file onto the target needs no post-install mounting.
//! `bootc install` copies the filesystem of the container it runs inside,
//! so kuma derives a one-layer image carrying the file and installs from
//! that, while `--target-imgref` records the *published* image as what
//! the machine fetches for subsequent updates. The installed system
//! therefore has an account and still tracks the public tag.
//!
//! Scope is deliberately whole-disk: kuma owns the partitioning (see
//! `partition`) and asks for its two sizes, but not where the partitions
//! go. Installing beside another system is a separate decision and a
//! later one.
//!
//! Encryption is offered rather than assumed. It is asked for, not
//! declared, because it is a property of a disk and not of an image: the
//! same declaration installed onto two machines can encrypt one and not
//! the other, and no image can carry the answer. It is also the one
//! answer here that cannot be revised without reinstalling, which is why
//! it is asked before anything is written rather than defaulted either
//! way.
//!
//! A swapfile is asked for on the same grounds and one of them is
//! weaker: the size is a property of this machine's memory, which no
//! image can know, but unlike encryption it *can* be added afterwards.
//! `kuma hibernate` is that verb. It is offered here anyway because the
//! installer is the only place that can make the file before anything
//! else is on the disk, and because a machine that is asked once at
//! install is a machine nobody has to remember to go back to.

use anyhow::{bail, Context, Result};

use crate::hibernate;

/// What was asked for, before any of it is checked.
///
/// A struct rather than ten positional parameters: this is the one
/// verb here that cannot be undone, and a call whose arguments are told
/// apart by position is a poor place to get one wrong.
pub struct Request {
    pub image: String,
    /// What a bare `kuma install` resolves to here, which is the
    /// published image on a machine and the media's own image on
    /// installer media that recorded a pullable one. Carried so the
    /// command a dry run prints names `--image` when, and only when,
    /// running it without the flag would install something else.
    pub default_image: String,
    /// What the installed machine fetches updates from, when that is not
    /// the image being installed. Installing a locally built image and
    /// tracking the published tag is the case this exists for.
    pub update_from: Option<String>,
    pub user: Option<String>,
    pub groups: Vec<String>,
    pub hostname: Option<String>,
    pub shell: Option<String>,
    /// Asked for on a terminal when this is false, so that the flag is a
    /// way to answer the question early rather than the only way to
    /// answer it at all.
    pub encrypt: bool,
    /// The swapfile size as it was typed, or None for "ask on a
    /// terminal, and take no for an answer anywhere else". Carried
    /// unparsed so that a bad size is reported by the one place that
    /// knows how to explain it, rather than by clap.
    pub swap: Option<String>,
    /// The ESP and /boot sizes as they were typed, or None each for
    /// "ask on a terminal with the default shown; the default when
    /// nobody is there to ask". Carried unparsed for the same reason
    /// `swap` is, and there is no "none" for either: every install
    /// writes both partitions.
    pub esp: Option<String>,
    pub boot: Option<String>,
    /// A file naming the repository and its credentials, written onto
    /// the target so its first boot puts the home directory back.
    pub restore: Option<std::path::PathBuf>,
    /// The install-time facts for the target's provenance file, as
    /// sembled by the caller: it knows the declaration, the lock and the
    /// media that this verb can only be told about.
    pub provenance: Provenance,
    pub yes: bool,
    pub json: bool,
}

/// What the person answered, and what the target will converge to.
pub struct Account {
    pub name: String,
    pub password_hash: String,
    pub groups: Vec<String>,
    /// None means whatever `useradd` defaults to, which is what a
    /// declaration that names no shell also gets.
    pub shell: Option<String>,
}

/// The file `kuma-user-sync` sources, in the format it already reads.
///
/// Deliberately the same shape as the baked `/usr/lib/kuma/user` rather
/// than a new one: the converger gains a second source, not a second
/// parser, and a machine installed this way is indistinguishable at boot
/// from one whose declaration named the account.
pub fn user_file(account: &Account, ssh_key: Option<&str>) -> String {
    let mut out = format!("KUMA_USER='{}'\n", account.name);
    // Same key the baked declaration writes, so the converger cannot
    // tell the two apart. Absent rather than empty when unset: the sync
    // script tests `[ -n "${KUMA_SHELL:-}" ]`, so an empty value would
    // read as "set" and hand useradd nothing.
    if let Some(shell) = &account.shell {
        out.push_str(&format!("KUMA_SHELL='/usr/bin/{shell}'\n"));
    }
    if !account.groups.is_empty() {
        out.push_str(&format!("KUMA_GROUPS='{}'\n", account.groups.join(" ")));
    }
    out.push_str(&format!("KUMA_PASSWORD_HASH='{}'\n", account.password_hash));
    // The one answer a VM disk needs that the image cannot carry: the
    // key that lets the machine running the build reach the disk it
    // built. Real installs never name one — a person installs from a
    // terminal and keeps their own keys — so the field stays empty
    // everywhere else and the converger's handler never fires.
    //
    // A public key's comment is free text, and this file is sourced by
    // bash: a raw quote would end the value and swallow the rest of the
    // file. The standard single-quote escape keeps the key byte-for-byte
    // what sshd should see.
    if let Some(key) = ssh_key {
        let escaped = key.trim().replace('\'', "'\\''");
        out.push_str(&format!("KUMA_SSH_KEY='{escaped}'\n"));
    }
    out
}

/// The install-time facts written onto the target beside the account
/// and hostname.
///
/// The image carries the declaration; it cannot carry what only the
/// installer knows — when the machine was installed, with which kuma,
/// from what media, and which base digest the lock resolved that day.
/// bootc records its own facts at the same root (.bootc-aleph.json);
/// this is kuma's half of the same story, and the two answer different
/// questions: aleph says what bootc installed, this says who drove it.
///
/// `None` fields are honest unknowns, not omissions: a machine
/// installed where no lock was written cannot name a base digest, and
/// an image that came from a registry cannot have its digest read
/// locally. Readers grade nothing on absence — this file is
/// self-description, and it exists only where an install ran.
pub struct Provenance {
    /// The kuma that ran the install, as `--version` prints it.
    pub kuma: String,
    /// When the install ran, RFC3339 UTC.
    pub installed_at: String,
    /// sha256 of the declaration the install was driven from, when one
    /// was in hand. The hash rather than the file: the image already
    /// carries the declaration, and what ages well here is the fact of
    /// which one it was.
    pub declaration: Option<String>,
    /// The base digest the lock beside that declaration had resolved.
    /// Absent wherever no lock was written: a fresh checkout's first
    /// install, or media with no build of its own.
    pub base: Option<String>,
    /// The image ref that was installed.
    pub image: String,
    /// Its digest, when the image was local enough to ask.
    pub image_digest: Option<String>,
    /// What physically installed the machine: "host" (kuma install run
    /// from an installed machine), "live media" (the ISO), or "vm disk"
    /// (a `kuma vm` build, which installs by the same path).
    pub media: String,
}

impl Provenance {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "kuma": self.kuma,
            "installed_at": self.installed_at,
            "declaration": self.declaration,
            "base": self.base,
            "image": self.image,
            "image_digest": self.image_digest,
            "media": self.media,
        })
        .to_string()
    }
}

/// The one-layer image that carries the answers onto the target.
///
/// Into /var, not /etc. bootc fills /var from the image once at install
/// and never touches it again; /etc is three-way merged on every update,
/// and a file the installer shipped as image content is not a local
/// modification, so the merge against a published image that has no such
/// file deletes it. The account would outlive the file describing it.
///
/// 0600 on the user file for the same reason the baked one is: it holds a
/// password hash and only the root-run converger reads it. This image is
/// thrown away once the install finishes; nothing tags it for keeping.
pub fn install_containerfile(source: &str, account: &Account, restore: bool) -> String {
    let shell = account.shell.as_deref();
    // `useradd -s /usr/bin/nonsense` does not fail. It makes the account
    // with a shell that is not there and the machine comes up unable to
    // log anybody in, which is the exact failure this verb exists to
    // prevent. A declaration gets the same guard at image build time;
    // an install of somebody else's image has no build of its own until
    // this one, so this is where it goes. Only for an explicit --shell:
    // a shell the image declared was already checked when it was built.
    let guard = match shell {
        Some(shell) => format!("RUN test -x /usr/bin/{shell}\n"),
        None => String::new(),
    };
    format!(
        "# Generated by kuma for `kuma install`. One layer over the image\n\
         # being installed, carrying what the target converges to.\n\
         FROM {source}\n\
         {guard}\
         COPY --chmod=600 kuma-user /var/lib/kuma/user\n\
         COPY kuma-hostname /var/lib/kuma/hostname\n\
         COPY kuma-install /var/lib/kuma/install.json\n\
         {restore}{}",
        drop_foreign_autologin(&account.name),
        restore = if restore { RESTORE_LAYER } else { "" }
    )
}

/// The request and the credential, written onto the target exactly the
/// way the account is: bootc fills `/var` from the image once, at
/// install, and never again, which is what install-time answers need.
///
/// 0600 because it is a credential, and in `/var/lib/kuma/secrets` so it
/// sits where the machine's own credential would, under the fixed name
/// the first-boot unit looks for. The declaration's `secret` name is not
/// used here: the machine being restored has no declaration of its own
/// yet, and the file it is given is the one that names the repository.
const RESTORE_LAYER: &str =
    "COPY --chmod=600 kuma-restore-secret /var/lib/kuma/secrets/restore.env\n\
     COPY kuma-restore-request /var/lib/kuma/restore-request\n";

/// What a restore file has to say before an install will accept it.
///
/// `RESTIC_REPOSITORY` rather than a second flag, because the machine
/// being restored has no declaration yet and the address has to come
/// from somewhere. That makes the whole recovery one file: put it on the
/// stick beside the ISO and a dead disk needs nothing else typed.
///
/// Checked here rather than at first boot, where the only person who
/// could read the error has already walked away.
pub fn restore_file_is_usable(text: &str) -> Result<(), String> {
    let names: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim()))
        .collect();
    if !names.contains(&"RESTIC_REPOSITORY") {
        return Err("the restore file must set RESTIC_REPOSITORY: the machine being restored has \
             no declaration yet, so the address has to come from the file"
            .to_string());
    }
    if !names.iter().any(|n| n.starts_with("RESTIC_PASSWORD")) {
        return Err("the restore file sets no RESTIC_PASSWORD (or RESTIC_PASSWORD_FILE), so \
             nothing could open the repository"
            .to_string());
    }
    // And the values, which this used to skip entirely while `kuma
    // backup` refused them. This is the door the file comes in through
    // and the best place to say no: refusing here costs somebody a
    // sentence, and not refusing here means a machine that installs,
    // reboots, and fails its restore with the repository password read
    // differently from the one that encrypted it.
    let ambiguous = crate::backup::ambiguous_values(text);
    if !ambiguous.is_empty() {
        return Err(format!(
            "the restore file sets {} with a value carrying a quote, a backslash, `$` or a \
             backtick. The first-boot restore reads this file through systemd and `kuma \
             backup` reads it with a shell loop, and they do not agree about such a value, \
             so write it as plain text. A repository made before kuma 0.17 was encrypted \
             with the expanded value and needs `restic passwd` first.",
            ambiguous.join(", ")
        ));
    }
    Ok(())
}

/// Where the greeter autologins somebody who will not exist here.
///
/// An image built from a declaration with `autologin = true` bakes that
/// account's name into the greeter's config. Installed for a different
/// account, the name resolves to nobody: greetd fails
/// `pam_acct_mgmt: USER_UNKNOWN`, restarts five times, and gives up, so
/// the machine boots to a console nobody can log in at. Found on a real
/// install, where every other part had converged correctly.
///
/// Dropped rather than pointed at the new account, because kuma already
/// answered this question once: `kuma-user-sync` clears the account keys
/// between the baked declaration and the installer's file so that "an
/// image that declared a user cannot lend its password to somebody
/// else's account". Autologin is a property of that same account, and
/// lending it is the same move.
///
/// Cut at the section header, because kuma writes this block last in
/// both files it appears in, and because a greeter that has lost its
/// autologin still greets. Only when the names differ: installing an
/// image onto a machine for the account it already declares is the case
/// where autologin was meant.
///
/// The name goes into single quotes in generated shell. That is safe
/// because `ask_account` has already put it through `validate_name`,
/// which rejects quotes and every shell metacharacter; it is not safe on
/// its own, and this is the sentence that says so.
fn drop_foreign_autologin(name: &str) -> String {
    format!(
        "# A greeter cannot autologin an account this machine will not\n\
         # have. Runs inside the build, so nothing has to read the image\n\
         # from outside it.\n\
         RUN for conf in {} {}; do \\\n    \
         [ -f \"$conf\" ] || continue; \\\n    \
         grep -q '^\\[initial_session\\]' \"$conf\" || continue; \\\n    \
         [ \"$(sed -n 's/^user *= *\"\\(.*\\)\"/\\1/p' \"$conf\" | tail -1)\" = '{name}' ] \\\n      \
         || sed -i '/^\\[initial_session\\]/,$d' \"$conf\"; \\\n\
         done\n",
        crate::containerfile::GREETD_CONF,
        crate::containerfile::COSMIC_GREETER_CONF,
    )
}

/// What installing this image puts on the disk that a published one
/// would not.
///
/// A shared image declares no `[user]`, which is why the installer asks
/// for one. An image built from somebody's own declaration does declare
/// one, and installing it carries that account's name and password hash
/// onto the disk in the baked declaration, for an account this machine
/// will not even create. `kuma iso` already warns when a declared user
/// rides into installer media; this is the same hazard by the same
/// route, and it went unremarked until an install produced a machine
/// nobody could log in to.
pub fn baked_user_warning(baked: &str, installing: &str) -> Option<String> {
    let config: crate::config::Config = toml::from_str(baked).ok()?;
    let declared = config.user?;
    if declared.name == installing {
        return None;
    }
    let mut out = format!(
        "note: this image declares the account '{}', which is not the '{installing}' being\n\
         created here. Its password hash rides along in the image's baked declaration,\n\
         so this disk carries a credential for an account it will never have.",
        declared.name
    );
    if declared.autologin {
        out.push_str(
            "\nIts autologin has been dropped from the greeter, which would otherwise try to\n\
             log in a user that does not exist and fail to start at all.",
        );
    }
    out.push_str("\n\nImages meant to be installed elsewhere should declare no [user].");
    Some(out)
}

/// The hostname the installed machine takes.
///
/// Machine state, like the account: a published image cannot know it, and
/// every machine installed from one would otherwise answer to the same
/// name. Defaulted rather than demanded, because a name is the least
/// consequential thing being decided here.
pub fn ask_hostname(given: Option<String>) -> Result<String> {
    use std::io::IsTerminal;
    if let Some(name) = given {
        crate::config::validate_name(&name, "hostname", &['.', '-'])?;
        return Ok(name);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(DEFAULT_HOSTNAME.to_string());
    }
    eprint!("Hostname for the installed machine [{DEFAULT_HOSTNAME}]: ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let name = line.trim();
    if name.is_empty() {
        return Ok(DEFAULT_HOSTNAME.to_string());
    }
    crate::config::validate_name(name, "hostname", &['.', '-'])?;
    Ok(name.to_string())
}

/// Matches what every kuma image bakes, so accepting the default changes
/// nothing rather than writing a file that says what was already true.
pub const DEFAULT_HOSTNAME: &str = "kumaos";
// Read by containerfile.rs for both the os-release DEFAULT_HOSTNAME and
// the /etc/hostname fallback, so the invariant this comment used to only
// assert is now the same string in every place that depends on it.

/// Reasons not to write to this disk, in the order a person would want
/// to hear them.
///
/// Pure over the mount table so the dangerous branch is testable without
/// a spare disk. Every other kuma verb is reversible: `switch` stages,
/// `rollback` exists, a bad build is a build. This one is not, so the
/// checks that stop it are the part worth being sure of.
pub fn disk_objections(
    disk: &str,
    proc_mounts: &str,
    lsblk_mountpoints: &str,
    to_file: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // A file target is a disk image, written through a loopback device.
    // It is not a device path and must not be judged as one.
    //
    // The mount checks below still run, and for a file the caller has to
    // work for that: `lsblk` refuses a file path, so it resolves the
    // image to the loop devices backing it (`losetup -j`) and asks about
    // those. Before that, this function received an empty string for
    // every file target and the comment here claimed a protection that
    // could not fire, with a test that passed only because it fed the
    // input by hand.
    if !to_file && !disk.starts_with("/dev/") {
        out.push(format!("{disk} is not a device path"));
    }

    // lsblk first, because it is the only one that sees through LUKS and
    // LVM. /proc/mounts names the *mapper* device for an encrypted root,
    // so a disk whose every partition is inside a crypt container has no
    // line in it that mentions the disk at all. On a machine with /boot
    // encrypted too, a check built only on /proc/mounts finds nothing to
    // object to and cheerfully wipes the running system.
    for mount in lsblk_mountpoints.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if seen.insert(mount.to_string()) {
            out.push(format!("something on {disk} is in use at {mount}"));
        }
    }

    // And /proc/mounts, which needs no external command: if lsblk is
    // missing or fails, this is the whole guard rather than none of it.
    // Matches /dev/sda1 for /dev/sda and /dev/nvme0n1p1 for /dev/nvme0n1.
    for line in proc_mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(source), Some(target)) = (fields.next(), fields.next()) else {
            continue;
        };
        let is_partition = source == disk
            || source.strip_prefix(disk).is_some_and(|rest| {
                let rest = rest.strip_prefix('p').unwrap_or(rest);
                !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
            });
        if is_partition && seen.insert(target.to_string()) {
            out.push(format!("{source} from {disk} is mounted at {target}"));
        }
    }
    out
}

/// Objections to a disk that is a member of a volume living beyond it.
///
/// A physical volume or a raid member is data's door into a volume whose
/// other members sit on other disks, and nothing has to be mounted for
/// either to be true — that is exactly why the mount checks above see
/// nothing. Wiping one member can break a VG or an array that still
/// holds the only copy of something. `crypto_LUKS` is deliberately
/// absent: an old install's encrypted root is the ordinary reinstall
/// case, and an unopened container endangers only itself.
///
/// Pure over `lsblk -no FSTYPE <disk>`'s output (one row per node in the
/// device's tree, disk included), so the caller can tolerate an absent
/// lsblk the same way it does for the mount checks.
pub fn membership_objections(fstypes: &str) -> Vec<String> {
    fstypes
        .lines()
        .map(str::trim)
        .filter(|t| matches!(*t, "LVM2_member" | "linux_raid_member"))
        .map(|t| {
            format!("{t} signature: this disk is a member of a volume elsewhere, and wiping it breaks that volume")
        })
        .collect()
}

/// Why this reference cannot be what a machine updates from.
///
/// `localhost/...` is not a registry anybody else can reach: on the
/// installed machine it means that machine, which has no registry
/// running, so the first `kuma update` fails with a connection refused
/// naming a host nobody meant. The image installs fine and the machine
/// is stranded on it forever, which is worth catching before a disk is
/// written rather than weeks later.
pub fn unreachable_update_source(reference: &str) -> Option<String> {
    if reference.starts_with("localhost/") {
        return Some(format!(
            "{reference} is local to the machine running this install.\n\n\
             The installed machine records it as where updates come from, and\n\
             `localhost` there means itself. It has no registry, so it would\n\
             never update.\n\n\
             Install a published image, or keep this one and say where the\n\
             machine should update from:\n\n  \
             --update-from ghcr.io/<owner>/kuma:niri"
        ));
    }
    None
}

/// Which image a bare `kuma install` writes, given what the media it is
/// running on recorded about itself, and what to say about the choice.
///
/// Installing pulls from a registry rather than copying the media, so
/// the media's own image is only installable when it came from one. The
/// three cases are worth keeping apart:
///
/// - Nothing recorded: an ordinary machine, or media built before kuma
///   recorded this. The published image, silently, as before.
/// - A registry reference: install what this media is. Somebody who
///   built media from their declaration and booted it was looking at
///   their own system, and installing something else is a surprise no
///   message makes acceptable.
/// - A `localhost/` reference: the common case, since `kuma build`
///   writes one. It cannot be pulled from anywhere, so the published
///   image is the only thing that can be installed, and the difference
///   has to be said out loud rather than discovered afterwards.
pub fn image_for_media(recorded: Option<&str>) -> (String, Option<String>) {
    let published = crate::PUBLISHED_IMAGE.to_string();
    let Some(recorded) = recorded.map(str::trim).filter(|r| !r.is_empty()) else {
        return (published, None);
    };
    if unreachable_update_source(recorded).is_none() {
        return (recorded.to_string(), None);
    }
    let note = format!(
        "This media was built from {recorded}, which is local to the machine\n\
         that built it and cannot be pulled from here. Installing {published}\n\
         instead, so the installed machine has somewhere to update from.\n\n\
         To install what this media is running, push that image to a registry\n\
         and name it: kuma install --image ghcr.io/<owner>/kuma:<tag>"
    );
    (published, Some(note))
}

/// A disk somebody might install onto, as `lsblk` describes it.
pub struct Disk {
    pub path: String,
    pub size: String,
    pub model: String,
    /// Everything mounted anywhere on it, found through partitions and
    /// through LUKS and LVM children. Non-empty means refuse.
    pub mounts: Vec<String>,
}

/// The disks worth offering, from `lsblk -J -o NAME,SIZE,MODEL,TYPE,MOUNTPOINTS`.
///
/// Pure over the JSON so the list a person chooses from is testable
/// without spare hardware, which matters more here than usual: choosing
/// wrong is not recoverable.
///
/// zram and loop devices are dropped. Both report `type: "disk"`, and
/// neither is a thing anyone can install onto: one is compressed RAM,
/// the other is a file. A list that offers them invites a mistake in the
/// one place a mistake is permanent.
pub fn disks_from_lsblk(json: &str) -> Result<Vec<Disk>> {
    fn mounts_of(node: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(points) = node.get("mountpoints").and_then(|v| v.as_array()) {
            for point in points.iter().filter_map(|p| p.as_str()) {
                if !point.is_empty() {
                    out.push(point.to_string());
                }
            }
        }
        // Recursive, because the mount that matters is usually two levels
        // down: a partition holding a LUKS container holding the root.
        for child in node.get("children").and_then(|v| v.as_array()).into_iter().flatten() {
            mounts_of(child, out);
        }
    }

    let root: serde_json::Value =
        serde_json::from_str(json).context("cannot read the disk list from lsblk")?;
    let mut out = Vec::new();
    for dev in root.get("blockdevices").and_then(|v| v.as_array()).into_iter().flatten() {
        if dev.get("type").and_then(|v| v.as_str()) != Some("disk") {
            continue;
        }
        let name = dev.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if name.is_empty() || name.starts_with("zram") || name.starts_with("loop") {
            continue;
        }
        let mut mounts = Vec::new();
        mounts_of(dev, &mut mounts);
        out.push(Disk {
            path: format!("/dev/{name}"),
            size: dev.get("size").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
            model: dev.get("model").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(),
            mounts,
        });
    }
    Ok(out)
}

/// Ask which disk, listing what was found.
///
/// Never picks for you, not even when exactly one disk is free. Every
/// other kuma verb can be undone; this one writes a partition table.
/// A single-candidate machine is also the most likely place for the one
/// disk to be the one you are running from.
///
/// In-use disks stay on the list, marked and refused, rather than being
/// hidden. Hiding them makes somebody wonder where their disk went and
/// look for it among the ones that are left.
pub fn choose_disk(mut disks: Vec<Disk>) -> Result<Disk> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!("no --disk given, and nothing to ask: pass --disk when not on a terminal");
    }
    if disks.is_empty() {
        bail!("no disks found to install onto");
    }
    println!("\nDisks on this machine:\n");
    for (index, disk) in disks.iter().enumerate() {
        let model = if disk.model.is_empty() { String::new() } else { format!("  {}", disk.model) };
        // Naming every mount is unreadable and adds nothing: an encrypted
        // root reports seven, and the reader needs to know the disk is
        // busy, not the shape of its filesystem tree.
        let state = if disk.mounts.is_empty() {
            String::new()
        } else {
            let shown = disk.mounts.iter().take(2).cloned().collect::<Vec<_>>().join(", ");
            let rest = disk.mounts.len().saturating_sub(2);
            let more = if rest > 0 { format!(" and {rest} more") } else { String::new() };
            format!("   in use at {shown}{more}  (refused)")
        };
        println!("  {}  {:<14} {:>8}{}{}", index + 1, disk.path, disk.size, model, state);
    }
    let free = disks.iter().filter(|d| d.mounts.is_empty()).count();
    if free == 0 {
        bail!("every disk here is in use; nothing can be installed onto safely");
    }
    loop {
        print!("\nWhich disk should kuma install onto? [1-{}, or q] ", disks.len());
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!("no answer");
        }
        let answer = line.trim();
        if answer.eq_ignore_ascii_case("q") {
            bail!("nothing was changed");
        }
        match answer.parse::<usize>() {
            Ok(n) if n >= 1 && n <= disks.len() => {
                let disk = &disks[n - 1];
                if !disk.mounts.is_empty() {
                    println!("{} is in use at {}.", disk.path, disk.mounts.join(", "));
                    continue;
                }
                // The whole Disk, not its path: what it knows about being
                // mounted is the same question the objection check asks,
                // and asking twice is how two answers start to differ.
                return Ok(disks.swap_remove(n - 1));
            }
            _ => println!("Answer with a number from the list, or q to stop."),
        }
    }
}

/// EFI boot entries that appeared while installing, as `efibootmgr`
/// numbers them.
///
/// Installing writes a boot entry into the firmware of the machine doing
/// the installing, because `bootupctl` names the ESP it just wrote and
/// asks the firmware to remember it. That is exactly right for a disk
/// and useless for a file: the entry points at a partition inside an
/// image, sorts itself to the front of the boot order, and names a
/// device no firmware can ever find.
///
/// bootc has no flag for this. `--generic-image` skips the firmware but
/// also installs every bootloader type, and this layout has no BIOS Boot
/// Partition, so it fails at `grub2-install` after the disk is already
/// written. So kuma does not prevent the entry; it notices it and says
/// which one it is.
///
/// A diff of two `efibootmgr` listings rather than a search for kuma's
/// own name: the entry is named by the image's os-release, an install of
/// somebody else's image can call it anything, and the only thing kuma
/// knows for certain is that it was not there a minute ago.
pub fn new_efi_entries(before: &str, after: &str) -> Vec<String> {
    after
        .lines()
        .filter(|line| line.starts_with("Boot") && !before.lines().any(|old| old == *line))
        .filter_map(|line| {
            // `Boot0008* Kuma\tHD(1,GPT,...)`. The number is fixed width
            // and the rest is a device path nobody needs here.
            let rest = line.strip_prefix("Boot")?;
            let (number, _) = rest.split_at(rest.find(['*', ' '])?);
            (number.len() == 4 && number.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| number.to_string())
        })
        .collect()
}

/// Whether to encrypt the root, asked unless `--encrypt` already said so.
///
/// Not a default in either direction. Encryption on by default would
/// hand somebody a machine that stops at a passphrase prompt they never
/// asked for and cannot remove without reinstalling; off by default and
/// never asked is how a laptop ends up unencrypted because nothing
/// mentioned it. So the flag answers it early, a terminal is asked, and
/// a pipe with no flag gets the safe-to-be-wrong answer: an unencrypted
/// machine can be reinstalled, and a machine whose passphrase nobody
/// chose cannot be booted.
pub fn ask_encrypt(flagged: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if flagged {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    loop {
        // stderr, like `ask_account` and `ask_hostname`. A prompt on
        // stdout would land inside the JSON document that `--json --yes`
        // promises is the only thing there.
        eprint!("\nEncrypt this disk with a passphrase? [y/N] ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(false);
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => eprintln!("Answer y or n."),
        }
    }
}

/// Whether this machine gets a swapfile, and how big.
///
/// Two questions rather than one, in the shape `ask_encrypt` already
/// uses: a yes or no that defaults to no, and only a yes earns the
/// second. A single "size, or none" prompt would make everybody answer a
/// question about gibibytes in order to decline a feature.
///
/// Off when nobody is there to ask, for the same reason encryption is: a
/// pipe driving an install gets what it asked for on the command line
/// and nothing it did not.
///
/// The size is re-asked rather than fatal when it does not fit. Every
/// other answer in this interview is typed once and cannot be wrong in a
/// way the installer can see; this one can be too big for the disk, and
/// the disk is standing right there to be measured against. Failing the
/// whole install over a number somebody can retype would mean answering
/// the account and hostname questions again for nothing.
pub fn ask_swap(
    flagged: Option<&str>,
    ram_mib: Option<u64>,
    default_mib: Option<u64>,
    spare_mib: u64,
) -> Result<Option<u64>> {
    use std::io::{IsTerminal, Write};
    if let Some(text) = flagged {
        let asked = hibernate::parse_size(text)?;
        if let Some(mib) = asked {
            // A flag is not a conversation: this one cannot be re-asked,
            // so it fails here, before the disk is touched.
            if let Some(why) = hibernate::objections(mib, spare_mib, hibernate::SPARE_AT_INSTALL)
                .into_iter()
                .next()
            {
                bail!("{why}");
            }
        }
        return Ok(asked);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    // A default only counts as one if it fits. On a disk with no room to
    // spare, offering the size of memory would be offering a number the
    // next line refuses.
    let default_mib = default_mib.filter(|mib| *mib <= spare_mib);
    loop {
        // stderr, like every other prompt here: stdout is where
        // `--json --yes` promises exactly one document.
        eprint!("\nCreate a swapfile so this machine can hibernate? [y/N] ");
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(None);
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => break,
            "" | "n" | "no" => return Ok(None),
            _ => eprintln!("Answer y or n."),
        }
    }
    loop {
        let memory = match ram_mib {
            Some(mib) => format!(", this machine has {} of memory", hibernate::size_text(mib)),
            None => String::new(),
        };
        match default_mib {
            Some(mib) => {
                eprint!("Size? [{}{memory}] ", hibernate::size_text(mib))
            }
            None => eprint!("Size?{} ", if memory.is_empty() { "" } else { &memory[2..] }),
        }
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(default_mib);
        }
        let answer = line.trim();
        if answer.is_empty() {
            match default_mib {
                Some(mib) => return Ok(Some(mib)),
                None => {
                    eprintln!("No default here; type a size like 16G, or none.");
                    continue;
                }
            }
        }
        let parsed = match hibernate::parse_size(answer) {
            Ok(parsed) => parsed,
            Err(why) => {
                eprintln!("{why}");
                continue;
            }
        };
        let Some(mib) = parsed else { return Ok(None) };
        match hibernate::objections(mib, spare_mib, hibernate::SPARE_AT_INSTALL).into_iter().next()
        {
            Some(why) => eprintln!("{why}"),
            None => return Ok(Some(mib)),
        }
    }
}

/// One size question, for `ask_sizes`.
///
/// Every install writes both partitions, so there is no yes or no in
/// front, the shape `ask_swap` uses: the question shows the default and
/// enter takes it, the shape `ask_hostname` uses. A bad answer is
/// re-asked rather than fatal, the shape `ask_swap` uses, because the
/// disk is standing right there to be measured against and a typo costs
/// one retype.
///
/// A flag answers early and fatally, exactly like `ask_swap`'s flagged
/// branch: a flag is not a conversation, and a bad one has to stop the
/// install before anything else is asked rather than mid-interview.
/// Nobody on the other end of a pipe gets the default, like both of
/// those.
fn ask_size(
    given: Option<&str>,
    which: crate::partition::Which,
    label: &str,
    default_mib: u64,
) -> Result<u64> {
    use std::io::{IsTerminal, Write};
    if let Some(text) = given {
        return crate::partition::resolve_size(text, which);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(default_mib);
    }
    loop {
        // stderr, like every other prompt here: stdout is where
        // `--json --yes` promises exactly one document.
        eprint!("\n{label} [{}]? ", hibernate::size_text(default_mib));
        std::io::stderr().flush()?;
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Ok(default_mib);
        }
        let answer = line.trim();
        if answer.is_empty() {
            return Ok(default_mib);
        }
        match crate::partition::resolve_size(answer, which) {
            Ok(mib) => return Ok(mib),
            Err(why) => eprintln!("{why}"),
        }
    }
}

/// The ESP and /boot sizes, asked one question at a time when nobody
/// flagged either.
///
/// Both questions are asked with their defaults, because the sizes
/// cannot be revised without reinstalling but the defaults are right
/// for almost every disk: a person who does not care presses enter
/// twice, and one who does is not asked to go edit a plan after the
/// fact. Asking both even when one was flagged would re-ask an
/// answered question, so each is asked only when its flag is absent.
///
/// Asked before the plan is computed, like encryption and for the same
/// reason: the plan prints these sizes, and a layout shown before its
/// sizes were known would be describing a disk nobody had decided on.
pub fn ask_sizes(esp: Option<&str>, boot: Option<&str>) -> Result<crate::partition::Sizes> {
    Ok(crate::partition::Sizes {
        esp_mib: ask_size(
            esp,
            crate::partition::Which::Esp,
            "ESP system partition size",
            crate::partition::Sizes::DEFAULT.esp_mib,
        )?,
        boot_mib: ask_size(
            boot,
            crate::partition::Which::Boot,
            "/boot size",
            crate::partition::Sizes::DEFAULT.boot_mib,
        )?,
    })
}

/// The passphrase that unlocks the disk at every boot.
///
/// Asked twice on a terminal, because a mistyped one is not discovered
/// until the machine will not boot and there is nothing left to compare
/// it against. From stdin otherwise, one line, ahead of the account
/// password for the same reason it is asked first: it decides the shape
/// of the disk, and the account does not.
///
/// No minimum length and no strength opinion. Unlike everything else
/// decided here, this one *can* be changed later on the installed
/// machine (`cryptsetup luksChangeKey`), so a rule invented here would
/// be a rule nothing enforces afterwards.
pub fn ask_passphrase() -> Result<String> {
    use std::io::IsTerminal;
    let passphrase = if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password("Passphrase to unlock this disk at boot: ")?;
        let again = rpassword::prompt_password("Retype the disk passphrase: ")?;
        if first != again {
            bail!("passphrases don't match");
        }
        first
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_string()
    };
    if passphrase.is_empty() {
        bail!("empty passphrase: the disk would be encrypted with nothing");
    }
    Ok(passphrase)
}

/// Ask for the account the target will create at first boot.
///
/// Not a terminal: the name comes from `--user` and stdin supplies the
/// password, one line, nothing else. That is what keeps the password out
/// of argv where `ps` would show it, and it is why there is no
/// `--password` flag. Omitting `--user` there is an error rather than a
/// prompt nobody is present to answer.
pub fn ask_account(
    name: Option<String>,
    groups: Vec<String>,
    shell: Option<String>,
) -> Result<Account> {
    use std::io::IsTerminal;
    let interactive = std::io::stdin().is_terminal();
    let name = match name {
        Some(name) => name,
        None if interactive => {
            eprint!("Account name for the installed machine: ");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
        None => bail!("no account name: pass --user, or run this on a terminal"),
    };
    crate::config::validate_name(&name, "user.name", &['.', '-', '_'])?;
    // Same field and same file format as `user.groups` in a declaration,
    // which is validated; this path was not. `--groups` lands in
    // KUMA_GROUPS=' … ' inside /var/lib/kuma/user, which kuma-user-sync
    // sources as root at first boot, so a quote in it breaks the quoting
    // and runs there. Only whoever runs `sudo kuma install` supplies it,
    // so nobody crosses a boundary; the asymmetry is the bug.
    //
    // Before the password prompt, with the name, because refusing an
    // argument after making somebody type a password twice is a worse
    // way to say the same thing.
    for group in &groups {
        crate::config::validate_name(group, "user.groups", &['.', '-', '_'])?;
    }

    let password = if interactive {
        let first = rpassword::prompt_password(format!("Password for {name}: "))?;
        let again = rpassword::prompt_password("Retype to confirm: ")?;
        if first != again {
            bail!("passwords don't match");
        }
        first
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim_end_matches(['\r', '\n']).to_string()
    };
    if password.is_empty() {
        bail!("empty password: the installed machine would have no way in");
    }
    if let Some(shell) = &shell {
        crate::config::validate_name(shell, "user.shell", &['.', '-', '_'])?;
    }
    Ok(Account { name, password_hash: crate::hash_password(&password)?, groups, shell })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account {
            name: "mira".into(),
            password_hash: "$6$abc$def".into(),
            groups: vec!["wheel".into()],
            shell: Some("fish".into()),
        }
    }

    /// The installer writes what the converger already reads. If these
    /// drift, an installed machine silently comes up with no account,
    /// which is the exact failure this verb exists to prevent.
    #[test]
    fn the_written_file_is_what_user_sync_sources() {
        let text = user_file(&account(), None);
        assert!(text.contains("KUMA_USER='mira'"));
        assert!(text.contains("KUMA_GROUPS='wheel'"));
        assert!(text.contains("KUMA_PASSWORD_HASH='$6$abc$def'"));
        assert!(text.contains("KUMA_SHELL='/usr/bin/fish'"));
        // Unset means absent, not empty: the sync script treats an empty
        // KUMA_SHELL as set and would pass useradd nothing.
        let bare = Account { shell: None, ..account() };
        assert!(!user_file(&bare, None).contains("KUMA_SHELL"));
        // Shell-sourceable: one KEY='value' per line, nothing else.
        for line in text.lines() {
            assert!(line.contains("='"), "not a shell assignment: {line}");
        }
    }

    /// A public key's trailing comment is free text fixed at the moment
    /// the key was generated, so it can hold anything a hostname or a
    /// `-C` once held, quotes included. The user file is sourced by the
    /// converger with set -euo pipefail, so a raw quote does not stop at
    /// looking wrong — it swallows the rest of the file, and the machine
    /// comes up with no account. (The old bib path had the same test
    /// against TOML; the guarantee moved here with the file.)
    #[test]
    fn a_pubkey_comment_cannot_break_the_user_file() {
        let hostile = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 a 'quoted\\name'";
        let text = user_file(&account(), Some(hostile));
        // Escaped, not raw: the value survives sourcing with its quote.
        let sourced = format!("{}\necho \"$KUMA_SSH_KEY\"\n", text);
        let got = std::process::Command::new("bash").arg("-c").arg(&sourced).output().unwrap();
        assert!(got.status.success());
        assert_eq!(String::from_utf8(got.stdout).unwrap().trim(), hostile);
        // And a VM that names no key writes no line at all.
        assert!(!user_file(&account(), None).contains("KUMA_SSH_KEY"));
    }

    /// A mounted partition of the target is the one thing between this
    /// verb and destroying the medium it is running from. On live media
    /// the ISO is mounted, and a person who typed the wrong letter would
    /// otherwise find out afterwards.
    #[test]
    fn a_mounted_partition_of_the_target_is_an_objection() {
        let mounts = "\
/dev/nvme0n1p3 / btrfs rw 0 0
/dev/nvme0n1p2 /boot ext4 rw 0 0
/dev/sr0 /run/initramfs/live iso9660 ro 0 0
tmpfs /tmp tmpfs rw 0 0
";
        let objections = disk_objections("/dev/nvme0n1", mounts, "", false);
        assert_eq!(objections.len(), 2, "both partitions of the target");

        // A different disk is not an objection just because it exists.
        assert!(disk_objections("/dev/sda", mounts, "", false).is_empty());
        // ... and the partition-suffix match must not fire on a disk
        // whose name merely starts the same way.
        assert!(disk_objections("/dev/nvme0", mounts, "", false).is_empty());
    }

    /// The case /proc/mounts cannot see, and the one most likely to be
    /// somebody's actual laptop: every partition inside a LUKS container,
    /// so the mount table names /dev/mapper/luks-... and never the disk.
    /// Without lsblk this disk looks idle and gets wiped while running.
    #[test]
    fn an_encrypted_disk_with_no_direct_mounts_is_still_in_use() {
        let mounts = "\
/dev/mapper/luks-f5d1fc89 /sysroot btrfs rw 0 0
/dev/mapper/luks-f5d1fc89 /var btrfs rw 0 0
";
        assert!(
            disk_objections("/dev/nvme0n1", mounts, "", false).is_empty(),
            "the mount table genuinely cannot see this, which is the point"
        );
        let lsblk = "/var\n/sysroot\n\n/boot\n";
        let objections = disk_objections("/dev/nvme0n1", mounts, lsblk, false);
        assert_eq!(objections.len(), 3, "blank lines are unmounted partitions");
        assert!(objections.iter().all(|o| o.contains("in use at")));
    }

    /// Both sources naming the same mount point is one objection, not two.
    #[test]
    fn the_two_sources_do_not_double_report() {
        let mounts = "/dev/sda1 /boot ext4 rw 0 0\n";
        let objections = disk_objections("/dev/sda", mounts, "/boot\n", false);
        assert_eq!(objections.len(), 1);
    }

    /// A member of a volume beyond this disk is refused with nothing
    /// mounted: that is the case the mount checks cannot see. A plain
    /// filesystem, an unopened LUKS container and an absent lsblk are
    /// the ordinary reinstall, and objecting to any of them would break
    /// it.
    #[test]
    fn a_member_of_another_volume_is_refused_without_being_mounted() {
        let tree = "btrfs\nLVM2_member\nlinux_raid_member\n";
        let objections = membership_objections(tree);
        assert_eq!(objections.len(), 2, "{objections:?}");
        assert!(objections[0].contains("LVM2_member"));
        assert!(objections[1].contains("linux_raid_member"));

        let reinstall = "crypto_LUKS\nbtrfs\nvfat\nswap\n\n";
        assert!(membership_objections(reinstall).is_empty());
        assert!(membership_objections("").is_empty(), "no lsblk objects to nothing");
    }

    /// zram reports `type: "disk"` and would otherwise appear in a list
    /// somebody picks from with no undo. It is compressed RAM.
    #[test]
    fn the_disk_list_offers_only_things_that_can_be_installed_onto() {
        let json = r#"{"blockdevices":[
          {"name":"zram0","size":"8G","model":null,"type":"disk","mountpoints":["[SWAP]"]},
          {"name":"loop0","size":"1G","model":null,"type":"disk","mountpoints":[]},
          {"name":"sr0","size":"1.5G","model":"QEMU DVD-ROM","type":"rom","mountpoints":["/run/initramfs/live"]},
          {"name":"sda","size":"932G","model":"ST1000LM035 ","type":"disk","mountpoints":[]}
        ]}"#;
        let disks = disks_from_lsblk(json).unwrap();
        assert_eq!(disks.len(), 1, "zram, loop and the optical drive are not targets");
        assert_eq!(disks[0].path, "/dev/sda");
        assert_eq!(disks[0].model, "ST1000LM035", "lsblk pads the model");
        assert!(disks[0].mounts.is_empty());
    }

    /// The mount that decides it is usually two levels down: a partition
    /// holding a LUKS container holding the root. A list built from the
    /// top level alone shows the running system's disk as free.
    #[test]
    fn a_disk_is_in_use_when_anything_nested_under_it_is_mounted() {
        let json = r#"{"blockdevices":[
          {"name":"nvme0n1","size":"476.9G","model":"Micron","type":"disk","mountpoints":[],
           "children":[
             {"name":"nvme0n1p1","size":"600M","type":"part","mountpoints":["/boot/efi"]},
             {"name":"nvme0n1p3","size":"474G","type":"part","mountpoints":[],
              "children":[{"name":"luks-abc","size":"474G","type":"crypt","mountpoints":["/sysroot","/var"]}]}
           ]}
        ]}"#;
        let disks = disks_from_lsblk(json).unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].mounts, vec!["/boot/efi", "/sysroot", "/var"]);
    }

    #[test]
    fn a_path_that_is_not_a_device_is_an_objection() {
        assert!(!disk_objections("nvme0n1", "", "", false).is_empty());
        assert!(!disk_objections("/home/me/disk.img", "", "", false).is_empty());
    }

    /// Installing to a file is installing to a disk image, which bootc
    /// writes through a loopback device. The device-path rule has to
    /// stand down for it, and every other check has to stay: a disk
    /// image someone has mounted is exactly as bad to overwrite as a
    /// disk, and more likely to be in use without being noticed.
    /// A machine that cannot update is not a machine anybody wants, and
    /// nothing about the install says so: it succeeds, boots, works, and
    /// fails the first time it is asked to take a new image.
    #[test]
    fn a_local_image_cannot_be_what_a_machine_updates_from() {
        assert!(unreachable_update_source("localhost/kuma:latest").is_some());
        assert!(unreachable_update_source("ghcr.io/someone/kuma:niri").is_none());
        // Not a prefix match on the word: a registry that merely starts
        // with those letters is somebody's real host.
        assert!(unreachable_update_source("localhost.example.com/kuma:v1").is_none());
    }

    /// What a bare `kuma install` writes, which was kuma's published
    /// image on every medium regardless of what that medium was. Someone
    /// could build installer media from their own declaration, boot it,
    /// look at their own desktop, install, and get a different system,
    /// with nothing anywhere saying so.
    #[test]
    fn a_bare_install_writes_the_media_it_can_actually_pull() {
        // An ordinary machine, and media built before this was recorded.
        let (image, note) = image_for_media(None);
        assert_eq!(image, crate::PUBLISHED_IMAGE);
        assert!(note.is_none(), "nothing recorded is not worth a paragraph");

        // Media built from something installable: install what you booted.
        let (image, note) = image_for_media(Some("ghcr.io/someone/kuma:cosmic"));
        assert_eq!(image, "ghcr.io/someone/kuma:cosmic");
        assert!(note.is_none());

        // The common case, since `kuma build` writes a localhost tag. The
        // published image is the only installable answer, and the
        // difference is said rather than left to be discovered.
        let (image, note) = image_for_media(Some("localhost/kuma:latest"));
        assert_eq!(image, crate::PUBLISHED_IMAGE);
        let note = note.expect("a substitution this surprising has to be announced");
        assert!(note.contains("localhost/kuma:latest"));
        assert!(note.contains(crate::PUBLISHED_IMAGE));
        assert!(note.contains("--image"), "and it has to say how to get what you built");

        // A file with a trailing newline is what `printf` writes, and an
        // empty one is a record that failed to say anything.
        assert_eq!(image_for_media(Some("ghcr.io/o/k:t\n")).0, "ghcr.io/o/k:t");
        assert_eq!(image_for_media(Some("  ")).0, crate::PUBLISHED_IMAGE);
    }

    #[test]
    fn a_file_target_is_allowed_but_not_excused() {
        assert!(disk_objections("/var/tmp/kuma.raw", "", "", true).is_empty());
        let mounted = "/dev/loop0 /mnt/img ext4 rw 0 0\n";
        assert!(
            !disk_objections("/var/tmp/kuma.raw", mounted, "/mnt/img\n", true).is_empty(),
            "a mounted image is still in use"
        );
    }

    /// The account rides in as a layer rather than being written after
    /// the fact, and the installed machine still tracks the published
    /// image. Losing either half is silent: the first gives a machine
    /// with no account, the second a machine that can never update.
    #[test]
    fn the_derived_layer_carries_the_answers_at_0600() {
        let bare = Account { shell: None, ..account() };
        let out = install_containerfile("ghcr.io/example/kuma:niri", &bare, false);
        assert!(out.contains("FROM ghcr.io/example/kuma:niri"));
        assert!(out.contains("COPY --chmod=600 kuma-user /var/lib/kuma/user"));
        assert!(out.contains("COPY kuma-hostname /var/lib/kuma/hostname"));
        // Nothing to check when nobody asked for a shell: whatever the
        // image declares was already checked when the image was built.
        assert!(!out.contains("test -x"));
        // /etc is three-way merged against the published image on every
        // update, and a file shipped as image content is not a local
        // modification, so the merge would delete both of these.
        assert!(!out.contains("/etc/kuma/"));
    }

    /// A greeter that autologins the image author's account cannot start
    /// on a machine that has a different one. Proven on a real install:
    /// `pam_acct_mgmt: USER_UNKNOWN` from `getpwnam()` for an account
    /// the machine lacked, five restarts, then no greeter at all.
    #[test]
    fn autologin_for_an_account_this_machine_lacks_is_removed() {
        let out = install_containerfile("localhost/kuma:latest", &account(), false);
        // Both files, because the two desktops autologin through
        // different ones and only one of them would be noticed.
        assert!(out.contains(crate::containerfile::GREETD_CONF));
        assert!(out.contains(crate::containerfile::COSMIC_GREETER_CONF));
        // Cut at the section header, which is where kuma writes it.
        assert!(out.contains(r"sed -i '/^\[initial_session\]/,$d'"));
        // And only when the name differs: an image installed for the
        // account it declares is the case autologin was written for.
        assert!(out.contains("= 'mira' ]"));
        // The greeter survives; only its autologin goes.
        assert!(!out.contains("rm -f /etc/greetd"));
    }

    /// The other half of the same hazard, which no code can undo: the
    /// `user.groups` goes through validate_name from a declaration and
    /// did not from `--groups`, though both end up in the same
    /// root-sourced file.
    #[test]
    fn groups_from_the_command_line_are_validated_like_declared_ones() {
        let hostile = vec!["wheel'; touch /tmp/pwned; :'".to_string()];
        // Matched rather than unwrap_err'd: Account holds a password
        // hash and deliberately does not derive Debug, which is what
        // unwrap_err would require.
        match ask_account(Some("probe".into()), hostile, None) {
            Err(e) => assert!(e.to_string().contains("user.groups"), "{e}"),
            Ok(_) => panic!("a group that closes its own quoting was accepted"),
        }

        // Nothing about the fix may refuse the ordinary case, and the
        // interview is not reachable from a test, so this only asserts
        // the shape a real group name has to keep passing.
        for good in ["wheel", "libvirt", "docker", "video", "kuma-users", "group.with.dots"] {
            crate::config::validate_name(good, "user.groups", &['.', '-', '_'])
                .unwrap_or_else(|e| panic!("{good} should validate: {e}"));
        }
    }

    /// A restore that cannot work is worth refusing while the old
    /// machine's disk is still the only copy of the data. At first boot
    /// the only person who could read the error has walked away.
    #[test]
    fn a_restore_file_that_could_not_open_the_repository_is_refused() {
        // The address has to be in the file: the machine being restored
        // has no declaration yet, so there is nowhere else for it.
        let no_repo = "RESTIC_PASSWORD=hunter2\n";
        assert!(restore_file_is_usable(no_repo).unwrap_err().contains("RESTIC_REPOSITORY"));

        let no_password = "RESTIC_REPOSITORY=b2:kuma\n";
        assert!(restore_file_is_usable(no_password).unwrap_err().contains("RESTIC_PASSWORD"));

        let usable = "# recovery\nRESTIC_REPOSITORY=s3:https://minio.example:9000/kuma\n\
                      RESTIC_PASSWORD=hunter2\nAWS_ACCESS_KEY_ID=k\nAWS_SECRET_ACCESS_KEY=s\n";
        restore_file_is_usable(usable).unwrap();

        // A password held in a file beside it counts, which is the
        // shape restic prefers and the one that keeps a secret out of
        // an environment children inherit.
        let by_file = "RESTIC_REPOSITORY=b2:kuma\nRESTIC_PASSWORD_FILE=/run/key\n";
        restore_file_is_usable(by_file).unwrap();
    }

    /// The request rides in the same layer as the account, because it is
    /// the same kind of thing: an install-time answer that /var carries
    /// once. Without --restore it must leave no trace, so an ordinary
    /// install cannot end up with a unit waiting for a file it will
    /// never get.
    #[test]
    fn a_restore_request_rides_the_install_layer_only_when_asked() {
        let plain = install_containerfile("ghcr.io/example/kuma:niri", &account(), false);
        assert!(!plain.contains("restore"), "{plain}");

        let restoring = install_containerfile("ghcr.io/example/kuma:niri", &account(), true);
        assert!(restoring
            .contains("COPY --chmod=600 kuma-restore-secret /var/lib/kuma/secrets/restore.env"));
        assert!(restoring.contains("/var/lib/kuma/restore-request"));
        // 0600 on the credential and not on the request: one is a
        // secret and the other is a flag saying to look for it.
        let secret_line = restoring
            .lines()
            .find(|l| l.contains("restore.env"))
            .expect("the credential is copied");
        assert!(secret_line.contains("--chmod=600"), "{secret_line}");
    }

    /// image carries a password hash for an account this disk will never
    /// create. `kuma iso` warns about the same thing for media.
    #[test]
    fn installing_someone_elses_image_says_what_rides_along() {
        let baked = "schema_version = 1\n[user]\nname = \"kai\"\n\
                     password_hash = '$6$abc$def'\nautologin = true\n";
        let warning = baked_user_warning(baked, "mira").expect("a declared user is worth saying");
        assert!(warning.contains("kai"));
        assert!(warning.contains("password hash"));
        assert!(warning.contains("autologin"));
        // Nothing to warn about when the image declares the same account
        // it is being installed for, or declares none at all.
        assert!(baked_user_warning(baked, "kai").is_none());
        assert!(baked_user_warning("schema_version = 1\n", "mira").is_none());
        // A declaration without autologin still carries the hash, and
        // still says so, but has no greeter line to explain.
        let quiet = "schema_version = 1\n[user]\nname = \"kai\"\n";
        let warning = baked_user_warning(quiet, "mira").unwrap();
        assert!(warning.contains("password hash") && !warning.contains("autologin"));
    }

    /// An explicit --shell is the one thing here the image has never
    /// seen, so it is the one thing this layer checks. It fails the
    /// build rather than the boot, which is the difference between an
    /// install that stops and a machine nobody can log into.
    #[test]
    fn an_asked_for_shell_is_checked_before_the_account_is_written() {
        let out = install_containerfile("ghcr.io/example/kuma:niri", &account(), false);
        let guard = out.find("RUN test -x /usr/bin/fish").unwrap();
        assert!(guard < out.find("COPY --chmod=600").unwrap());
    }

    /// The entry an install leaves in this machine's firmware, which is
    /// pollution when the target was a file. Found by what changed, not
    /// by name: the name comes from the image being installed.
    #[test]
    fn a_boot_entry_that_was_not_there_before_is_reported() {
        let before = "\
BootCurrent: 0003
BootOrder: 0003,0000
Boot0000* Windows Boot Manager\tHD(1,GPT,c0355a54)/\\EFI\\Microsoft\\Boot\\bootmgfw.efi
Boot0003* Fedora\tHD(1,GPT,f3035b31)/\\EFI\\fedora\\shimx64.efi
";
        let after = "\
BootCurrent: 0003
BootOrder: 0008,0003,0000
Boot0000* Windows Boot Manager\tHD(1,GPT,c0355a54)/\\EFI\\Microsoft\\Boot\\bootmgfw.efi
Boot0003* Fedora\tHD(1,GPT,f3035b31)/\\EFI\\fedora\\shimx64.efi
Boot0008* Kuma\tHD(1,GPT,b0180ce6)/\\EFI\\fedora\\shimx64.efi
";
        assert_eq!(new_efi_entries(before, after), vec!["0008"]);
        // A reordered boot order is not a new entry, and neither is an
        // unchanged listing.
        assert!(new_efi_entries(after, after).is_empty());
        assert!(new_efi_entries(before, before).is_empty());
        // Nothing to say when the firmware cannot be read at all.
        assert!(new_efi_entries("", "").is_empty());
    }

    /// The flag answers the question, and a pipe with no flag answers it
    /// the way that can be undone. Both directions matter: an install
    /// driven by a script must not stop on a prompt nobody is there for,
    /// and it must not quietly encrypt a disk with a passphrase nobody
    /// chose either.
    ///
    /// The terminal branch cannot be reached from a test, which is why
    /// the two branches that can are pinned here.
    #[test]
    fn encryption_is_answered_by_the_flag_or_left_off_when_nobody_is_asked() {
        assert!(ask_encrypt(true).unwrap());
        assert!(!ask_encrypt(false).unwrap(), "piped stdin, no flag");
    }

    /// A name is the least consequential thing being decided here, so it
    /// is defaulted rather than demanded, and the default matches what
    /// every kuma image already bakes.
    #[test]
    fn the_hostname_falls_back_rather_than_failing() {
        assert_eq!(ask_hostname(Some("workshop".into())).unwrap(), "workshop");
        // Piped stdin takes the default instead of blocking on a prompt
        // nobody is there to answer.
        assert_eq!(ask_hostname(None).unwrap(), DEFAULT_HOSTNAME);
        assert!(ask_hostname(Some("not a hostname".into())).is_err());
    }
}
