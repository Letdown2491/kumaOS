//! `kuma snapshot`: reach the snapshots the timer takes.
//!
//! Snapshots nobody can restore from are half a feature, and the half
//! that is missing is the one people need on the worst day they have.
//! The store is browsable on purpose (see SNAPSHOT_SCRIPT), so a file
//! manager already works; this is the same job for people who live in a
//! terminal, and the one place that knows the retention policy without
//! being told.
//!
//! Deliberately file-level. Rolling a whole subvolume back means
//! swapping what `/var/home` *is* while processes hold files open in it,
//! which is a reboot-shaped operation wearing a command's clothes. What
//! this restores is a path, which is what the overwhelmingly common
//! accident actually costs.

use crate::config::Config;
use crate::host::run_host;
use crate::response;
use crate::state::{print_actions, Action};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// The names SNAPSHOT_SCRIPT writes, and nothing else: a directory
/// someone dropped in the store is not a snapshot and is never offered
/// as one.
fn is_snapshot_id(name: &str) -> bool {
    let bytes = name.as_bytes();
    name.len() == 17
        && bytes[10] == b'T'
        && name[..10].split('-').count() == 3
        && name.chars().enumerate().all(|(i, c)| match i {
            4 | 7 => c == '-',
            10 => c == 'T',
            _ => c.is_ascii_digit(),
        })
}

fn store(config: &Config) -> PathBuf {
    Path::new(&config.snapshots.target).join(".snapshots")
}

/// Newest first, which is the order every question about a snapshot is
/// asked in.
fn ids(store: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| is_snapshot_id(name))
        .collect();
    out.sort_by(|a, b| b.cmp(a));
    out
}

/// Where the store is and how much is in it, for doctor. Shared rather
/// than reimplemented there so one naming rule decides what counts as a
/// snapshot: a health check that counted directories this command won't
/// offer would call a machine healthy for holding things it cannot
/// restore from.
pub fn store_state(config: &Config) -> (PathBuf, usize) {
    let store = store(config);
    let count = ids(&store).len();
    (store, count)
}

/// `2026-08-08T134347` reads as a timestamp to a machine and as noise to
/// a person; this is the same instant with separators people expect.
fn humanize(id: &str) -> String {
    format!("{} {}:{}:{}", &id[..10], &id[11..13], &id[13..15], &id[15..17])
}

/// The path a restore would read from and write to, resolved against one
/// snapshot. Returns None when the snapshot predates the path.
fn source_in(store: &Path, id: &str, relative: &Path) -> Option<PathBuf> {
    let candidate = store.join(id).join(relative);
    candidate.exists().then_some(candidate)
}

/// A path inside the snapshot target, as the target sees it. Absolute,
/// under the target, and free of `..` — this feeds a `cp` that runs as
/// whoever invoked it, so a path that climbs out of the target is a bug
/// worth refusing rather than resolving.
fn relative_to_target(config: &Config, path: &str) -> Result<PathBuf> {
    let target = Path::new(&config.snapshots.target);
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!(
            "{} is not an absolute path; name the file as it lives on the machine, \
             e.g. {}/you/notes.md",
            path.display(),
            target.display()
        );
    }
    if path.components().any(|c| c.as_os_str() == "..") {
        bail!("{} contains `..`; name the path without it", path.display());
    }
    let relative = path.strip_prefix(target).map_err(|_| {
        anyhow::anyhow!(
            "{} is outside {}, which is the only subvolume this declaration snapshots",
            path.display(),
            target.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        bail!(
            "{} is the subvolume itself; restore a path inside it, not the whole thing",
            target.display()
        );
    }
    Ok(relative.to_path_buf())
}

pub fn snapshot(
    config: &Config,
    config_path: &Path,
    restore: Option<&str>,
    from: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<()> {
    let store = store(config);
    let ids = ids(&store);
    match restore {
        Some(path) => restore_path(config, &store, &ids, path, from, yes, json),
        None => list(config, config_path, &store, &ids, json),
    }
}

fn list(
    config: &Config,
    config_path: &Path,
    store: &Path,
    ids: &[String],
    json: bool,
) -> Result<()> {
    let mut actions: Vec<Action> = Vec::new();
    if !config.snapshots.enable {
        // Turning snapshots on is a hand edit of a table kuma has no verb
        // for, so this points at the file the way an invalid config does
        // rather than at a `kuma edit` that has never existed.
        actions.push(Action::new(
            "edit",
            format!("$EDITOR {}", config_path.display()),
            "add [snapshots] enable = true, then kuma build and kuma switch",
        ));
    } else {
        actions.push(Action::new(
            "take",
            "sudo systemctl start kuma-snapshot.service",
            "take one now instead of waiting for the timer",
        ));
        if !ids.is_empty() {
            actions.push(Action::new(
                "restore",
                "kuma snapshot --restore <path>",
                "bring a path back from the newest snapshot that has it",
            ));
        }
    }

    if json {
        response::Response::new()
            .field("declared", config.snapshots.enable)
            .field("target", config.snapshots.target.clone())
            .field("store", store.display().to_string())
            .field("keep_recent", config.snapshots.keep_recent)
            .field("keep_daily", config.snapshots.keep_daily)
            .field(
                "snapshots",
                ids.iter()
                    .map(|id| serde_json::json!({ "id": id, "taken": humanize(id) }))
                    .collect::<Vec<_>>(),
            )
            .actions(&actions)
            .print(true, "");
        return Ok(());
    }

    if !config.snapshots.enable {
        println!("[snapshots] is not enabled in this declaration.");
    } else if ids.is_empty() {
        // An enabled declaration with an empty store is the normal state
        // between switching and the first timer tick, and it is also what
        // a target that cannot be snapshotted looks like forever. The two
        // are worth telling apart rather than hedging, which is what this
        // said until a machine spent a night in the second state reading
        // like the first: `kuma doctor` grades the target directly.
        println!("No snapshots yet in {}.", store.display());
        println!(
            "The timer takes the first one on its next run. If it does not, ask \
             `kuma doctor`: a {} that isn't a btrfs subvolume is skipped entirely, \
             and doctor is what checks.",
            config.snapshots.target
        );
    } else {
        println!("{} snapshots of {}", ids.len(), config.snapshots.target);
        for (i, id) in ids.iter().enumerate() {
            let tag = if i == 0 { "  (newest)" } else { "" };
            println!("  {}  {}{tag}", id, humanize(id));
        }
        println!(
            "\nKeeping the newest {} plus one a day for {} further days.",
            config.snapshots.keep_recent, config.snapshots.keep_daily
        );
    }
    if !actions.is_empty() {
        println!();
        print_actions(&actions);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn restore_path(
    config: &Config,
    store: &Path,
    ids: &[String],
    path: &str,
    from: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<()> {
    let relative = relative_to_target(config, path)?;
    if ids.is_empty() {
        bail!("no snapshots in {} to restore from (run `kuma snapshot` for why)", store.display());
    }
    if let Some(id) = from {
        if !ids.iter().any(|known| known == id) {
            bail!("no snapshot {id:?}; `kuma snapshot` lists the ones this machine has");
        }
    }

    // Named snapshot or not, the answer is the newest one that actually
    // has the path: a file created on Tuesday is not in Monday's snapshot,
    // and silently restoring nothing would be the worst outcome here.
    let search: Vec<&String> = match from {
        Some(id) => ids.iter().filter(|known| *known == id).collect(),
        None => ids.iter().collect(),
    };
    let found = search
        .iter()
        .find_map(|id| source_in(store, id, &relative).map(|source| ((*id).clone(), source)));
    let Some((id, source)) = found else {
        match from {
            Some(id) => bail!("{path} is not in snapshot {id}"),
            None => bail!("{path} is in none of the {} snapshots on this machine", ids.len()),
        }
    };

    let destination = Path::new(&config.snapshots.target).join(&relative);
    let overwrites = destination.exists();
    let action = (!yes).then(|| {
        let mut cmd = format!("kuma snapshot --restore {path}");
        if let Some(id) = from {
            cmd.push_str(&format!(" --from {id}"));
        }
        cmd.push_str(" --yes");
        Action::new(
            "restore",
            cmd,
            if overwrites {
                "do it: the copy on the machine now is replaced and not kept"
            } else {
                "do it: write the file back where it was"
            },
        )
    });

    if json {
        let mut document = response::Response::new()
            .field("path", path)
            .field("from", id.as_str())
            .field("source", source.display().to_string())
            .field("overwrites", overwrites)
            .field("applied", yes);
        if !yes {
            // The contract's marker, not a field the verb computes: an
            // agent polling for effect reads it the same way it reads
            // every other preview's document.
            document = document.dry_run();
        }
        if let Some(a) = &action {
            document = document.action(a.clone());
        }
        document.print(true, "");
    } else {
        println!("{path}");
        println!("  from  {} ({})", id, humanize(&id));
        if overwrites {
            println!("  note  a copy exists on the machine now and this replaces it");
        }
    }

    if !yes {
        if !json {
            println!();
            print_actions(&action.into_iter().collect::<Vec<_>>());
        }
        return Ok(());
    }

    // -a to keep ownership, mode, and timestamps; --reflink=auto so a
    // restore on the same filesystem shares extents instead of copying
    // gigabytes. -T so restoring a directory replaces that directory
    // rather than nesting a copy inside it.
    run_host(&[
        "cp",
        "-a",
        "-T",
        "--reflink=auto",
        &source.display().to_string(),
        &destination.display().to_string(),
    ])?;
    if !json {
        println!("\nRestored {path}.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    fn enabled() -> Config {
        config("schema_version = 1\n[snapshots]\nenable = true\n")
    }

    #[test]
    fn only_the_scripts_own_names_are_snapshots() {
        assert!(is_snapshot_id("2026-08-08T134347"));
        for impostor in [
            "not-a-snapshot",
            "2026-08-08",
            "2026-08-08T13434",
            "2026-08-08T1343470",
            "2026-08-08X134347",
            "2026-08-08Tabcdef",
            "..",
        ] {
            assert!(!is_snapshot_id(impostor), "{impostor:?} is not a snapshot name");
        }
    }

    /// The restore path feeds a `cp`, so anything that could climb out of
    /// the subvolume has to die before it gets there.
    #[test]
    fn a_restore_path_cannot_leave_the_target() {
        let config = enabled();
        for bad in ["/etc/shadow", "relative/path", "/var/home/../etc/shadow", "/var/home"] {
            assert!(relative_to_target(&config, bad).is_err(), "{bad:?} should be refused");
        }
        assert_eq!(
            relative_to_target(&config, "/var/home/myuser/notes.md").unwrap(),
            Path::new("myuser/notes.md")
        );
    }

    /// Newest first, because that is the order every question about a
    /// snapshot gets asked in, and impostors never enter the list.
    #[test]
    fn snapshots_are_listed_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        for name in
            ["2026-08-06T120000", "2026-08-08T134347", "2026-08-07T090000", "not-a-snapshot"]
        {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        assert_eq!(
            ids(dir.path()),
            ["2026-08-08T134347", "2026-08-07T090000", "2026-08-06T120000"]
        );
    }

    /// Doctor grades a machine on this count, so it has to mean what the
    /// restore path means. A store holding only things `kuma snapshot`
    /// would refuse to offer is an empty store, and has to read as one.
    #[test]
    fn the_store_count_is_what_a_restore_could_actually_use() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(&format!(
            "schema_version = 1\n[snapshots]\nenable = true\ntarget = {:?}\n",
            dir.path().to_str().unwrap()
        ));
        let (store, count) = store_state(&config);
        assert_eq!(store, dir.path().join(".snapshots"));
        // A store that doesn't exist yet counts as empty rather than
        // erroring: it is exactly the state between switch and first tick.
        assert_eq!(count, 0);

        std::fs::create_dir(&store).unwrap();
        for name in ["not-a-snapshot", "2026-08-08"] {
            std::fs::create_dir(store.join(name)).unwrap();
        }
        assert_eq!(store_state(&config).1, 0);

        std::fs::create_dir(store.join("2026-08-08T134347")).unwrap();
        assert_eq!(store_state(&config).1, 1);
    }

    /// A file made on Tuesday is not in Monday's snapshot. Walking newest
    /// first and taking the first hit is what makes "restore my notes"
    /// mean the most recent copy that exists.
    #[test]
    fn the_newest_snapshot_holding_the_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["2026-08-06T120000", "2026-08-07T090000", "2026-08-08T134347"] {
            std::fs::create_dir_all(dir.path().join(id).join("myuser")).unwrap();
        }
        let relative = Path::new("myuser/notes.md");
        // present in the oldest two only: the newest predates nothing, it
        // simply no longer has the file
        for id in ["2026-08-06T120000", "2026-08-07T090000"] {
            std::fs::write(dir.path().join(id).join(relative), "hi").unwrap();
        }
        let ids = ids(dir.path());
        let hit = ids.iter().find_map(|id| source_in(dir.path(), id, relative));
        assert_eq!(hit.unwrap(), dir.path().join("2026-08-07T090000").join(relative));
    }

    #[test]
    fn missing_snapshots_are_an_error_not_a_silent_success() {
        let config = enabled();
        let store = tempfile::tempdir().unwrap();
        let err = restore_path(
            &config,
            store.path(),
            &[],
            "/var/home/myuser/notes.md",
            None,
            false,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no snapshots"));
    }
}
