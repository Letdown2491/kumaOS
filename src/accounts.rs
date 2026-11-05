//! The additional accounts' bridge: declaration to converger, across
//! the gap a stale compiler leaves.
//!
//! The delta is compiled by the machine's own binary, which is the
//! release the machine is leaving. When that binary predates the
//! account emission — exactly one transition can: the emission's first
//! release is the oldest compiler a machine can hold while its target
//! carries the machinery — the staged deployment carries no dropin, and
//! the ride it re-bakes still names the accounts. What the converger
//! reads (`/usr/lib/kuma/users.d/`) is image content, and `/usr` is
//! read-only at runtime, so nothing at boot could write the account
//! where the converger looks.
//!
//! The heal closes it from the other side: the ride is machine-named
//! data the new binary can read, so the missing account files go into
//! machine state (`/var/lib/kuma/users.d/`), which the converger's own
//! loop picks up — the same rule the primary account runs on, where
//! `/var/lib/kuma/user` fills what the image does not name.
//!
//! The ownership rule is the dropin's. A dropin is compiled from the
//! current declaration by the current compiler and is the fresher
//! answer; a machine-state file exists only while no dropin names the
//! account, and the heal retires its own files the moment a dropin
//! appears. The alternative — a stale machine file sourced after the
//! image loop — would re-apply the old values over the dropin's
//! (`usermod -s` sets, it does not append), and the account would flip
//! back a little more every boot.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::config;
use crate::state;

/// The image's dropin directory: what the converger's first loop reads.
pub(crate) const IMAGE_USERS_D: &str = "/usr/lib/kuma/users.d";

/// The machine-state directory: what the heal writes and the
/// converger's second loop reads.
pub(crate) const MACHINE_USERS_D: &str = "/var/lib/kuma/users.d";

/// The KUMA_* keys one additional account renders to, in the order the
/// converger sources them. Shared with the delta compiler's dropin
/// emission: one owner of the shape, so the boot-time heal and the
/// build-time delta cannot drift apart. `KUMA_SSH_KEY` is deliberately
/// not here — the delta rides keys as image content under `/etc`, and
/// only the heal's machine-state files carry the key in the file.
pub(crate) fn account_keys(name: &str, extra: &config::AdditionalUser) -> String {
    let mut decl = format!("KUMA_USER='{name}'\n");
    if let Some(shell) = &extra.shell {
        decl.push_str(&format!("KUMA_SHELL='/usr/bin/{shell}'\n"));
    }
    if !extra.groups.is_empty() {
        decl.push_str(&format!("KUMA_GROUPS='{}'\n", extra.groups.join(" ")));
    }
    if let Some(hash) = &extra.password_hash {
        decl.push_str(&format!("KUMA_PASSWORD_HASH='{hash}'\n"));
    }
    decl
}

/// The verb behind `kuma-user-sync.service`'s ExecStartPre: fill the
/// account holes the ride names and no dropin carries, and retire the
/// machine-state files whose accounts a dropin now owns.
///
/// Silent when there is nothing to do — this runs on every boot of
/// every kuma machine, and a converger that narrates itself into the
/// journal on every boot is a converger whose one interesting line
/// nobody sees. Each filled or retired account is the one interesting
/// line, printed when it happens.
///
/// Loud on refusal: a ride that does not parse, or one whose schema is
/// ahead of this binary's, fails the unit rather than guessing — the
/// contract pins the schema, so a failure here is a broken promise, and
/// a broken promise is a human's to see. That also fails the sync the
/// ExecStartPre belongs to, deliberately: the account data is not to be
/// trusted, and converging the accounts around it would be guessing.
pub(crate) fn heal() -> Result<()> {
    let ride = Path::new(state::BAKED_CONFIG);
    let text = match std::fs::read_to_string(ride) {
        // Not a kuma image's shape — the unit ships in every image,
        // including ones whose declaration rides nowhere. Nothing to
        // fill, nothing to retire.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        other => Some(other.context("the baked declaration is unreadable")?),
    };
    for line in heal_from(text.as_deref(), Path::new(IMAGE_USERS_D), Path::new(MACHINE_USERS_D))? {
        println!("{line}");
    }
    Ok(())
}

/// What one account file carries, for the tests: the keys the
/// converger sources. The mode is the emission's own (0600 — the file
/// holds a password hash), asserted at the write.
fn account_file(name: &str, extra: &config::AdditionalUser) -> String {
    let mut decl = account_keys(name, extra);
    if !extra.ssh_keys.is_empty() {
        decl.push_str(&format!("KUMA_SSH_KEY='{}'\n", extra.ssh_keys.join("\n")));
    }
    decl
}

/// The heal against paths, so it can run against a temp tree. Returns
/// one line per move, in the machine's own voice, empty when quiet.
fn heal_from(
    ride: Option<&str>,
    image_users_d: &Path,
    machine_users_d: &Path,
) -> Result<Vec<String>> {
    let mut moved = Vec::new();
    let accounts = match ride {
        None => BTreeMap::new(),
        Some(text) => {
            let value: toml::Value =
                toml::from_str(text).context("the baked declaration is unreadable")?;
            if let Some(schema) = value.get("schema_version").and_then(|v| v.as_integer()) {
                if schema as u32 > config::CURRENT_SCHEMA {
                    bail!(
                        "the baked declaration speaks schema {schema}, this kuma understands {}",
                        config::CURRENT_SCHEMA
                    );
                }
            }
            value
                .get("other_users")
                .map(|v| {
                    BTreeMap::<String, config::AdditionalUser>::deserialize(v.clone()).context(
                        "the baked declaration's [other_users] speaks a schema this \
                                  kuma cannot read",
                    )
                })
                .transpose()?
                .unwrap_or_default()
        }
    };

    for (name, extra) in &accounts {
        if image_users_d.join(name).exists() {
            // The dropin owns the account now; the machine-state file
            // was the heal's filler and its reason is gone.
            let stale = machine_users_d.join(name);
            if stale.exists() {
                std::fs::remove_file(&stale)?;
                moved.push(format!(
                    "{name}: the delta carries the dropin now; the heal's \
                                    file stands down"
                ));
            }
            continue;
        }
        let file = machine_users_d.join(name);
        // Write-then-rename, the entry writer's own shape: a crash
        // mid-write leaves no half file for the converger to source.
        std::fs::create_dir_all(machine_users_d)?;
        let tmp = machine_users_d.join(format!(".{name}.kuma-heal"));
        std::fs::write(&tmp, account_file(name, extra))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, &file)?;
        moved.push(format!(
            "{name}: the ride names this account and no dropin carries \
                            it; the converger's machine-state loop will"
        ));
    }
    // A machine-state file the ride no longer names converges an
    // account the declaration moved past — the file is the heal's, and
    // the heal does not keep files without a reason.
    if let Ok(entries) = std::fs::read_dir(machine_users_d) {
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            let named = path.file_name().and_then(|n| n.to_str());
            let Some(name) = named else { continue };
            if name.starts_with('.') || accounts.contains_key(name) {
                continue;
            }
            std::fs::remove_file(&path)?;
            moved.push(format!(
                "{name}: the ride no longer names this account; the \
                                heal's file stands down"
            ));
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ride-shaped declaration with one additional account.
    fn ride(accounts: &str) -> String {
        format!("schema_version = 1\n[system]\ndesktop = \"niri\"\n{accounts}")
    }

    /// The AdditionalUser map the toml round-trip produces, for
    /// comparing against what the heal read out of a ride.
    fn declared_accounts(toml_text: &str) -> BTreeMap<String, config::AdditionalUser> {
        toml::from_str::<toml::Value>(toml_text)
            .unwrap()
            .get("other_users")
            .map(|v| BTreeMap::deserialize(v.clone()))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn the_gap_fills_with_the_emission_s_own_shape() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image-users.d");
        let machine = dir.path().join("machine-users.d");
        let text = ride("[other_users.deltauser]\nshell = \"fish\"\npassword_hash = \"$6$hash\"\n");
        let accounts = declared_accounts(&text);

        let moved = heal_from(Some(&text), &image, &machine).unwrap();
        assert_eq!(moved.len(), 1);
        assert!(moved[0].contains("deltauser"));

        let file = machine.join("deltauser");
        let contents = std::fs::read_to_string(&file).unwrap();
        assert!(contents.contains("KUMA_USER='deltauser'"));
        assert!(contents.contains("KUMA_SHELL='/usr/bin/fish'"));
        assert!(contents.contains("KUMA_PASSWORD_HASH="));
        // The mode is the emission's own: the file carries a hash.
        let mode = std::fs::metadata(&file).unwrap().permissions();
        assert_eq!(mode.mode() & 0o777, 0o600, "a hash-carrying file is 0600");
        // And the same shape the delta's dropin renders.
        let extra = accounts.get("deltauser").unwrap();
        assert!(contents.starts_with(&account_keys("deltauser", extra)));

        // Stable across passes: the ride owns the file, and a second
        // pass rewrites the same bytes — which is also how a NEW ride's
        // values (the next gap transition) converge on the next boot.
        let before = std::fs::read_to_string(&file).unwrap();
        let again = heal_from(Some(&text), &image, &machine).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    #[test]
    fn ssh_keys_ride_the_machine_state_file_the_gap_did_not_copy_them() {
        let dir = tempfile::tempdir().unwrap();
        let machine = dir.path().join("machine-users.d");
        let text = ride("[other_users.deltauser]\nssh_keys = [\"ssh-ed25519 AAA\"]\n");
        let accounts = declared_accounts(&text);
        let extra = accounts.get("deltauser").unwrap();
        // One owner of the shape: the keys the delta's dropin renders.
        assert_eq!(
            account_keys("deltauser", extra),
            "KUMA_USER='deltauser'\nKUMA_GROUPS='wheel'\n"
        );
        heal_from(Some(&text), &dir.path().join("no-image"), &machine).unwrap();
        let contents = std::fs::read_to_string(machine.join("deltauser")).unwrap();
        assert!(contents.contains("KUMA_SSH_KEY='ssh-ed25519 AAA'"));
    }

    #[test]
    fn a_dropin_takes_the_account_back() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image-users.d");
        let machine = dir.path().join("machine-users.d");
        std::fs::create_dir_all(&image).unwrap();
        let text = ride("[other_users.deltauser]\nshell = \"fish\"\n");
        heal_from(Some(&text), &image, &machine).unwrap();
        assert!(machine.join("deltauser").exists());

        // The dropin appears: the heal retires its own file, and the
        // next pass is quiet — the ownership rule's other half.
        std::fs::write(image.join("deltauser"), "KUMA_USER='deltauser'\n").unwrap();
        let moved = heal_from(Some(&text), &image, &machine).unwrap();
        assert_eq!(moved.len(), 1);
        assert!(moved[0].contains("stands down"));
        assert!(!machine.join("deltauser").exists());
        assert!(heal_from(Some(&text), &image, &machine).unwrap().is_empty());
    }

    #[test]
    fn an_account_the_ride_dropped_is_retired_not_converged() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image-users.d");
        let machine = dir.path().join("machine-users.d");
        std::fs::create_dir_all(&machine).unwrap();
        std::fs::write(machine.join("ghost"), "KUMA_USER='ghost'\n").unwrap();

        // A ride that names nothing: ghost's file stands down, and a
        // machine file is never written from nothing.
        let moved = heal_from(Some(&ride("")), &image, &machine).unwrap();
        assert_eq!(moved.len(), 1);
        assert!(moved[0].contains("ghost"));
        assert!(!machine.join("ghost").exists());
    }

    #[test]
    fn no_ride_is_a_quiet_pass() {
        let dir = tempfile::tempdir().unwrap();
        let machine = dir.path().join("machine-users.d");
        assert!(heal_from(None, &dir.path().join("no-image"), &machine).unwrap().is_empty());
        assert!(!machine.exists(), "a machine with no ride writes nothing");
    }

    #[test]
    fn a_ride_this_kuma_cannot_read_is_a_loud_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let ahead = "schema_version = 99\n[other_users.deltauser]\nshell = \"fish\"\n";
        assert!(heal_from(Some(ahead), dir.path(), dir.path()).is_err());
        // A meaning moved inside the section: the refusal is the
        // contract's, spelled out, not a silent hole.
        let moved_meaning = "schema_version = 1\n[other_users.deltauser]\nshel = \"fish\"\n";
        let err =
            format!("{:#}", heal_from(Some(moved_meaning), dir.path(), dir.path()).unwrap_err());
        assert!(err.contains("cannot read"), "{err}");
    }
}
