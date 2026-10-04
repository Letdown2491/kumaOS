//! `kuma backup`: reach the offsite copies the timer makes.
//!
//! [snapshots] answers a mistake and this answers the disk, but the
//! shape of the two commands is deliberately the same: list what exists,
//! then bring one path back, dry run first. What differs is where the
//! copies live and that reaching them costs a credential, which is why
//! everything here goes through sudo and `kuma snapshot` does not.
//!
//! Seeding is a verb rather than something the timer does. The first
//! copy of a home directory is tens of gigabytes, and a background job
//! that decides to start sending those while somebody is tethered on a
//! train is a tool that gets uninstalled. Every later copy is the
//! difference since the last one, which is what a timer is for.

use crate::config::Config;
use crate::host::{host_output, run_host};
use crate::response;
use crate::state::{print_actions, shell_quote, Action};
use anyhow::{bail, Result};
use std::path::Path;

/// Where the credential the declaration names lives on this machine.
///
/// The declaration names it and never holds it: a file written to be
/// committed, and baked world-readable into every image built from it,
/// is the wrong place for a secret. What the declaration does carry is
/// that a credential exists and what it is called, which is what makes
/// this path computable and therefore checkable.
pub fn secret_path(config: &Config) -> String {
    format!("{SECRETS_DIR}/{}.env", config.backup.secret)
}

/// Where a credential the declaration names lives, and the one command
/// that makes one. Both are printed to people by two different surfaces
/// (`kuma backup` and `kuma doctor`), so they are written once: an
/// instruction that differs between the two places somebody meets it is
/// worse than either version alone.
pub const SECRETS_DIR: &str = "/var/lib/kuma/secrets";

/// Both halves, because no machine has the directory. The image does not
/// ship it, and bootc fills /var once at install, so an image that gains
/// [backup] later would not add it either. `install -m 0600 <file>` on
/// its own fails with "No such file or directory" everywhere this is
/// printed.
pub fn provision_command(secret: &str) -> String {
    format!("sudo install -d -m 0700 {SECRETS_DIR} && sudo install -m 0600 /dev/null {secret}")
}

/// Written only by a run that copied something, which is what lets
/// doctor tell a machine backing up nightly from one whose unit exits 0
/// every night having found nothing to do.
pub const STAMP: &str = "/var/lib/kuma/backup-last";

/// One door to restic, and it is `sudo sh -c` rather than a helper baked
/// into the image on purpose: `kuma install --restore` runs from live
/// media, where no image of the target machine exists yet, so a helper
/// in an image is precisely the thing that is absent when it matters
/// most. This is the same three lines wherever it runs.
///
/// Everything after `_` becomes `"$@"`, so a path with a space in it
/// stays one argument and nothing is ever handed back to a shell to
/// re-parse. The secret arrives as a path rather than as a value, so it
/// is never in argv, which `ps` shows to everybody on the machine.
fn restic_argv(secret: &str, repo: &str, args: &[&str]) -> Vec<String> {
    restic_argv_within(secret, repo, None, args)
}

/// The same door with a clock on it, for the one call that has to answer
/// rather than succeed.
///
/// restic treats a missing bucket as a transient error and retries it
/// with exponential backoff, so asking "is there a repository here" of a
/// machine nobody has seeded takes minutes to answer "no". There is no
/// flag for it: `--retry-lock` is about locks and
/// `--stuck-request-timeout` defaults to five minutes. Only the probe
/// gets a deadline; a backup or a restore is allowed to take as long as
/// it takes.
fn restic_argv_within(
    secret: &str,
    repo: &str,
    seconds: Option<u32>,
    args: &[&str],
) -> Vec<String> {
    // RESTIC_CACHE_DIR for the same reason the units set it: restic
    // treats an unopenable cache as fatal, and neither a systemd service
    // nor `sudo` can be relied on to leave a usable HOME behind. Naming
    // it also means the verb and the timer share one cache rather than
    // filling two.
    // READ AND EXPORT, never source. `. file` runs whatever is on a
    // right-hand side, so a repository password of `$(curl ...|sh)`
    // executed as root here. `export "$line"` does not: the result of a
    // parameter expansion is not rescanned, so the value arrives
    // literally. Measured both ways, and the same change went into the
    // first-boot restore unit, which uses systemd's EnvironmentFile= for
    // the same reason.
    //
    // The two now agree because `usable` below refuses every value they
    // could read differently, so this loop and systemd's parser see one
    // file one way.
    let open = format!(
        // `|| [ -n "$line" ]` because a file whose last line carries no
        // newline otherwise loses that variable, and systemd's parser does
        // not. Measured: without it, a one-line restore.env written by
        // `printf` reached restic with no password at all.
        "while IFS= read -r line || [ -n \"$line\" ]; do \
           case \"$line\" in \"\"|\\#*) continue ;; esac; \
           export \"$line\"; \
         done < \"$1\"; export RESTIC_REPOSITORY=\"$2\"; \
         export RESTIC_CACHE_DIR=/var/cache/restic; install -d -m 0700 /var/cache/restic; \
         shift 2; exec {}restic \"$@\"",
        seconds.map(|s| format!("timeout {s} ")).unwrap_or_default()
    );
    let mut argv: Vec<String> =
        ["sudo", "sh", "-c", &open, "_", secret, repo].iter().map(|s| s.to_string()).collect();
    argv.extend(args.iter().map(|a| a.to_string()));
    argv
}

/// Keys whose values three different readers would not agree about.
///
/// This file has three readers and they never had one meaning. Until
/// 0.17 the verb SOURCED it, which expands `$(...)`, backticks and
/// `$VAR` and removes quotes; systemd's `EnvironmentFile=` in the timer
/// and the restore unit PARSES it, which does none of that but does
/// strip one pair of surrounding quotes; and the verb's loop now takes
/// the value literally. A password of `$(id -u)` was therefore three
/// different passwords depending on which one opened the repository, and
/// nothing said so.
///
/// The narrow subset all three agree about is a value with no `$`, no
/// backtick, no quote and no backslash. Anything else is refused rather
/// than guessed, because guessing here means either opening the wrong
/// repository or being unable to open the right one.
///
/// **This is a real migration, not a lint.** A machine whose repository
/// was initialised through the old sourcing path encrypted it with the
/// EXPANDED value, so its password is not what is written in the file.
/// `restic passwd` is the way out, and the message says so.
pub fn ambiguous_values(text: &str) -> Vec<String> {
    // split('\n') rather than lines(): lines() strips a trailing \r,
    // which is exactly the byte a CRLF file hides from this check while
    // the shell reader keeps it.
    text.split('\n')
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (key, value) = trimmed.split_once('=')?;
            // A line that changes under trim reads differently through
            // the parsers that consume this file: the shell loop keeps
            // the whitespace (`IFS= read -r`) and the env-file reader
            // does not, so the trim difference is refused on the same
            // terms as a metacharacter. Edge whitespace on the key or
            // the value (`KEY =v`, `KEY= v`) is the same disagreement
            // with the `=` in the middle, where a whole-line trim
            // cannot see it.
            if trimmed != line
                || key != key.trim()
                || value != value.trim()
                || value.contains(['$', '`', '"', '\'', '\\'])
            {
                Some(key.trim().to_string())
            } else {
                None
            }
        })
        .collect()
}

fn ready(config: &Config) -> Result<(String, String)> {
    if !config.backup.enable {
        bail!(
            "[backup] is not enabled in this declaration. Add it, then \
             `kuma build` and `kuma switch`."
        );
    }
    let secret = secret_path(config);
    // Read before use, so a file three readers disagree about is refused
    // here rather than silently meaning something different in the timer.
    if let Ok(text) = std::fs::read_to_string(&secret) {
        let ambiguous = ambiguous_values(&text);
        if !ambiguous.is_empty() {
            bail!(
                "{secret} sets {} with a value carrying a quote, a backslash, `$` or a \
                 backtick. Three things read this file and they do not agree about such a \
                 value: this verb, the backup timer, and the first-boot restore, so the \
                 password one of them uses is not the password another one uses. Rewrite \
                 the value as plain text. If the repository was created before kuma 0.17 \
                 it was encrypted with the EXPANDED value, so change the password with \
                 `restic passwd` first or the repository will not open.",
                ambiguous.join(", ")
            );
        }
    }
    if !Path::new(&secret).exists() {
        bail!(
            "no credential at {secret}. The declaration names it; this machine has \
             not been given it. Create it 0600 root with the repository's keys in \
             it (RESTIC_PASSWORD, and the backend's own variables), then try again."
        );
    }
    Ok((secret, config.backup.repo.clone()))
}

/// What was asked for, in one value.
///
/// Same shape as `install::Request` and for the same reason: threading
/// the resolved declaration path through pushed this past the argument
/// count clippy allows, and a pile of bools at a call site is where
/// `yes` and `json` get swapped by somebody editing in a hurry.
pub struct Request<'a> {
    pub init: bool,
    pub list: bool,
    pub restore: Option<&'a str>,
    pub from: Option<&'a str>,
    pub yes: bool,
    pub json: bool,
}

pub fn backup(config: &Config, config_path: &Path, ask: Request<'_>) -> Result<()> {
    if ask.init {
        return seed(config, ask.json);
    }
    if let Some(path) = ask.restore {
        return restore_path(config, path, ask.from, ask.yes, ask.json);
    }
    if ask.list {
        return list_snapshots(config, ask.json);
    }
    status(config, config_path, ask.json)
}

/// Create the repository, then hand the first copy to the unit that
/// already knows how to make one.
///
/// Deliberately not a second implementation of the backup itself. The
/// converger knows the excludes, the snapshot to read from, the bind
/// mount that keeps restic incremental, and the stamp; a `--init` that
/// copied files itself would be a second answer to all four, free to
/// drift from the first.
fn seed(config: &Config, json: bool) -> Result<()> {
    let (secret, repo) = ready(config)?;

    // No "is there one yet" probe first, deliberately. Asking costs a
    // round of restic's retry backoff on exactly the machine that has
    // never been seeded, and `restic init` gives the answer for free: it
    // creates the bucket when there is none and refuses when there is
    // already a repository, which are the only two answers the question
    // had.
    //
    // The first version did ask, and classified the reply by matching
    // restic's words in the error. It failed on a real machine, because
    // host_output keeps the last two lines of stderr and restic's last
    // two are its shutdown rather than its reason: an ordinary unseeded
    // repository read as unreachable and seeding refused itself.
    // Prose on stdout would land in the middle of the document a caller
    // asked for, and this is the verb that runs longest, so an agent
    // driving it is exactly who notices. Same bargain the mutating verbs
    // make: one document, or none of it.
    let say = |line: &str| {
        if !json {
            println!("{line}");
        }
    };

    say(&format!("Creating a repository at {repo}"));
    if let Err(e) = run_host(&restic_argv(&secret, &repo, &["init"])) {
        let said = format!("{e:#}").to_lowercase();
        if said.contains("already exists") || said.contains("already initialized") {
            if json {
                response::Response::new()
                    .field("repo", repo.as_str())
                    .field("seeded", false)
                    .field("why", "the repository already exists")
                    .print(true, "");
            } else {
                say(&format!("{repo} already holds a repository; nothing to seed."));
                say("The timer copies the difference from here on. \
                     `kuma backup --list` shows what is in it.");
            }
            return Ok(());
        }
        return Err(e);
    }
    say("");
    say("Now making the first copy. This is the whole of your data rather than");
    say("the difference since yesterday, so it takes as long as it takes.");
    run_host(&["sudo", "systemctl", "start", "kuma-backup.service"])?;
    if json {
        response::Response::new()
            .field("repo", repo.as_str())
            .field("seeded", true)
            .field("stamp", STAMP)
            .print(true, "");
    } else {
        say("");
        say("Done. `kuma doctor` grades how fresh it stays from here.");
    }
    Ok(())
}

/// What this machine knows without asking the network, which is the
/// same discipline doctor keeps: a status command that hangs on a train
/// is one people stop running.
fn status(config: &Config, config_path: &Path, json: bool) -> Result<()> {
    let secret = secret_path(config);
    let provisioned = Path::new(&secret).exists();
    let stamp = std::fs::read_to_string(STAMP).ok();
    let last = stamp.as_ref().and_then(|t| t.split_whitespace().nth(1).map(str::to_string));

    let mut actions: Vec<Action> = Vec::new();
    if !config.backup.enable {
        // The declaration this command actually read, not a guess at
        // where one usually lives: a hint naming a different file than
        // the command that printed it sends somebody to edit the wrong
        // machine's description. Every other EDITOR affordance in the
        // tree resolves the path the same way.
        actions.push(Action::new(
            "edit",
            format!("$EDITOR {}", config_path.display()),
            "add [backup] with a repo and a secret name, then kuma build and kuma switch",
        ));
    } else if !provisioned {
        actions.push(Action::new(
            "provision",
            provision_command(&secret),
            "make the credential the declaration names, then put the repository's keys in it",
        ));
    } else if last.is_none() {
        actions.push(Action::new(
            "seed",
            "sudo kuma backup --init",
            "make the first copy, deliberately, while plugged in",
        ));
    } else {
        actions.push(Action::new(
            "list",
            "kuma backup --list",
            "ask the repository what it is holding",
        ));
    }

    if json {
        response::Response::new()
            .field("declared", config.backup.enable)
            .field("repo", config.backup.repo.clone())
            .field("secret", config.backup.secret.clone())
            .field("secret_path", secret.as_str())
            .field("provisioned", provisioned)
            .field("interval", config.backup.interval.clone())
            .field("last_completed", last)
            .field("covers", config.snapshots.target.clone())
            .field("network_connections", config.backup.network_connections)
            .actions(&actions)
            .print(true, "");
        return Ok(());
    }

    if !config.backup.enable {
        println!("[backup] is not enabled in this declaration.");
        print_actions(&actions);
        return Ok(());
    }
    println!("Repository  {}", config.backup.repo);
    println!("Credential  {secret}{}", if provisioned { "" } else { "   (absent)" });
    println!("Covers      {}", config.snapshots.target);
    println!(
        "            network connections {}",
        if config.backup.network_connections {
            "included"
        } else {
            "NOT included; a restore needs those passwords retyped"
        }
    );
    match &last {
        Some(when) => println!("Last copy   {when}"),
        None => println!("Last copy   never"),
    }
    print_actions(&actions);
    Ok(())
}

fn list_snapshots(config: &Config, json: bool) -> Result<()> {
    let (secret, repo) = ready(config)?;
    // Bounded, because a repository that is not there answers this the
    // slow way: restic treats a missing bucket as transient and retries
    // with backoff, so `--list` on an unseeded machine sits silent for
    // minutes. A minute is long enough for a slow link and short enough
    // to be an answer.
    let raw = host_output(&restic_argv_within(
        &secret,
        &repo,
        Some(60),
        &["snapshots", "--tag", "kuma", "--json"],
    ))?;
    if json {
        println!("{raw}");
        return Ok(());
    }
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    let items = parsed.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("{repo} holds no backups this declaration made.");
        return Ok(());
    }
    println!("{} backups in {repo}", items.len());
    for item in &items {
        let id = item["short_id"].as_str().unwrap_or("?");
        let time = item["time"].as_str().unwrap_or("");
        // restic's timestamp is RFC3339 with fractional seconds; the
        // date and the minute are what anybody reads.
        let when = time.get(..16).unwrap_or(time).replace('T', " ");
        println!("  {id}  {when}");
    }
    println!();
    print_actions(&[Action::new(
        "restore",
        "kuma backup --restore <path>",
        "bring a path back from the newest backup that has it",
    )]);
    Ok(())
}

/// File-level, and in place.
///
/// The converger mounts the snapshot over the live path before it
/// copies, so what the repository records is where the files actually
/// live. That is what makes an in-place restore possible at all: the
/// path you name is the path restic knows, and `--target /` puts it
/// back where it was rather than under a staging directory somebody then
/// has to move things out of.
///
/// Dry run by default, the same bargain `kuma snapshot --restore` makes.
/// Writing over a file that is there is the one outcome nobody can undo
/// from here, so it takes a second word.
fn restore_path(
    config: &Config,
    path: &str,
    from: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<()> {
    // Before `ready`, deliberately. A path this command cannot vouch for
    // is wrong whether or not the machine has a credential, and saying
    // "provision a secret first" to somebody who typed a relative path
    // sends them to fix the wrong thing.
    if !Path::new(path).is_absolute() {
        bail!("{path} is not an absolute path; name the file as it lives on the machine");
    }
    if path.contains("..") {
        bail!("{path} contains `..`; name the path without it");
    }
    let (secret, repo) = ready(config)?;
    let snapshot = from.unwrap_or("latest");
    let live = Path::new(path).exists();

    let mut args = vec!["restore", snapshot, "--target", "/", "--include", path];
    if !yes {
        args.push("--dry-run");
    }

    if json {
        let mut document = response::Response::new()
            .field("repo", repo.as_str())
            .field("snapshot", snapshot)
            .field("path", path)
            .field("replaces_live_copy", live);
        if !yes {
            // The contract's marker rather than a computed field, and
            // the affordance the prose path prints: a preview that
            // names no way to apply it leaves an agent at a dead end
            // the document was supposed to prevent.
            document = document.dry_run().action(Action::new(
                "write",
                // Quoted, because this line is shell for whoever pastes
                // it, and a path with a space is an ordinary path. Same
                // discipline capture applies to its own suggestion.
                format!("sudo kuma backup --restore {} --yes", shell_quote(path)),
                "actually write it back",
            ));
        }
        document.print(true, "");
    } else if yes {
        // Present tense for something about to happen. The dry run says
        // "would" because it means it; saying "would" while writing is
        // how somebody reads a finished restore as a preview.
        println!(
            "Restoring {path} from {snapshot}{}",
            if live { ", replacing the copy that is there" } else { "" }
        );
    } else if live {
        println!("{path} exists and would be replaced by the copy in {snapshot} (dry run)");
    } else {
        println!("{path} would be restored from {snapshot} (dry run)");
    }

    // A dry run is a question and has to answer: unbounded, an absent or
    // unreachable repository makes it sit silent through restic's retry
    // backoff, which is the same shape as the probe that was already
    // fixed. Writing keeps no clock, because a restore of a home
    // directory over a slow link takes as long as it takes.
    let argv = if yes {
        restic_argv(&secret, &repo, &args)
    } else {
        restic_argv_within(&secret, &repo, Some(60), &args)
    };
    run_host(&argv)?;
    if !yes && !json {
        println!();
        print_actions(&[Action::new(
            "write",
            format!("sudo kuma backup --restore {} --yes", shell_quote(path)),
            "actually write it back",
        )]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        toml::from_str(toml).expect("valid declaration")
    }

    /// The secret is passed as a path and read inside the shell, never
    /// as a value in argv, because `ps` shows any process's arguments to
    /// every user on the machine for as long as it runs.
    /// The verb reads the credential and never runs it.
    ///
    /// `. file` expands `$(...)` as root; the loop does not. Both were
    /// measured before this was written: sourcing `A=$(id -u)` yields
    /// `1000`, exporting the read line yields the literal `$(id -u)`.
    #[test]
    fn the_credential_is_read_and_never_sourced() {
        let argv = restic_argv("/s.env", "b2:kuma", &["snapshots"]);
        let script = argv.iter().find(|a| a.contains("restic \"$@\"")).expect("the sh -c body");
        assert!(!script.contains(". \"$1\""), "still sourcing: {script}");
        assert!(script.contains("export \"$line\""), "{script}");
        // A last line with no newline is a variable systemd would read
        // and this loop would drop without it.
        assert!(script.contains("|| [ -n \"$line\" ]"), "{script}");
        // And the secret is still a path, never a value: ps shows argv.
        assert!(argv.iter().any(|a| a == "/s.env"));
        assert!(!argv.iter().any(|a| a.contains("RESTIC_PASSWORD")));
    }

    /// Every value three readers would read differently is refused.
    #[test]
    fn a_value_the_readers_disagree_about_is_named() {
        let hostile = "RESTIC_PASSWORD=$(id -u)\nB2_ACCOUNT_ID=plain\n";
        assert_eq!(ambiguous_values(hostile), vec!["RESTIC_PASSWORD"]);
        for bad in ["`id`", "\"quoted\"", "'single'", "back\\slash", "$VAR"] {
            let text = format!("RESTIC_PASSWORD={bad}\n");
            assert_eq!(ambiguous_values(&text), vec!["RESTIC_PASSWORD"], "{bad} slipped through");
        }
        // The ordinary case stays ordinary: comments, blanks, plain
        // values, and punctuation no reader treats specially.
        let fine = "# a comment\n\nRESTIC_PASSWORD=hunter2-with.punct_and/slash\nB2_KEY=abc123\n";
        assert!(ambiguous_values(fine).is_empty());
    }

    /// A line that changes under trim is its own refusal, and a CRLF
    /// file is the ordinary way to get one: the file a person edits on
    /// the stick may carry \r endings, `str::lines` would eat them, and
    /// the shell reader keeps them — so the check reads the raw text.
    #[test]
    fn a_line_that_changes_under_trim_is_refused() {
        let crlf = "RESTIC_PASSWORD=hunter2\r\nB2_ACCOUNT_ID=plain\r\n";
        assert_eq!(ambiguous_values(crlf), vec!["RESTIC_PASSWORD", "B2_ACCOUNT_ID"]);
        for hostile in [
            "RESTIC_PASSWORD=hunter2\r\n", // trailing \r: the CRLF case
            "RESTIC_PASSWORD=hunter2 \n",  // trailing space
            " RESTIC_PASSWORD=hunter2\n",  // leading whitespace on the key
            "RESTIC_PASSWORD =hunter2\n",  // space before the equals sign
        ] {
            assert_eq!(
                ambiguous_values(hostile),
                vec!["RESTIC_PASSWORD"],
                "{hostile:?} slipped through"
            );
        }
    }

    #[test]
    fn the_credential_is_never_an_argument() {
        let argv = restic_argv("/var/lib/kuma/secrets/backup.env", "b2:kuma", &["snapshots"]);
        assert!(argv.iter().any(|a| a == "/var/lib/kuma/secrets/backup.env"));
        assert!(
            argv.iter().any(|a| a.contains("RESTIC_CACHE_DIR=/var/cache/restic")),
            "restic dies without somewhere to cache, and sudo's HOME is not it: {argv:?}"
        );
        assert!(
            argv.iter().all(|a| !a.contains("RESTIC_PASSWORD=")),
            "a value, not a path, would be readable by everybody: {argv:?}"
        );
        // Everything the caller asked for lands after the separator, so
        // a path with a space in it stays one argument.
        let with_space =
            restic_argv("/s.env", "b2:kuma", &["restore", "latest", "--include", "/var/home/a b"]);
        assert_eq!(with_space.last().unwrap(), "/var/home/a b");
    }

    /// Only a question gets a clock. A backup or a restore is allowed to
    /// take as long as it takes, and a deadline on those would kill a
    /// first copy of a home directory over a slow link.
    #[test]
    fn only_the_question_has_a_deadline() {
        let listing = restic_argv_within("/s.env", "b2:kuma", Some(60), &["snapshots"]);
        assert!(
            listing.iter().any(|a| a.contains("timeout 60 restic")),
            "listing an absent repository must not wait out the retry storm: {listing:?}"
        );
        // And the writing paths keep no clock at all.
        let work = restic_argv("/s.env", "b2:kuma", &["backup", "/var/home"]);
        assert!(
            work.iter().all(|a| !a.contains("timeout")),
            "a copy must not be killed by a clock: {work:?}"
        );
    }

    /// A path outside the machine's own naming, or one that climbs, is
    /// refused rather than resolved: this feeds a restore that runs as
    /// root and writes at `/`.
    #[test]
    fn a_restore_refuses_a_path_it_cannot_vouch_for() {
        let declared = config(
            "schema_version = 1\n[snapshots]\nenable = true\n\
             [backup]\nenable = true\nrepo = \"b2:kuma\"\n",
        );
        for bad in ["notes.md", "/var/home/../etc/shadow"] {
            let err = restore_path(&declared, bad, None, false, false).unwrap_err().to_string();
            assert!(
                err.contains("absolute") || err.contains(".."),
                "{bad} should be refused before anything runs: {err}"
            );
        }
    }

    /// Every subcommand needs the same two things, and the message for
    /// each has to say what to do rather than what went wrong.
    #[test]
    fn an_unprovisioned_machine_is_told_what_is_missing() {
        let off = config("schema_version = 1\n");
        let err = ready(&off).unwrap_err().to_string();
        assert!(err.contains("[backup] is not enabled"), "{err}");

        let on = config(
            "schema_version = 1\n[snapshots]\nenable = true\n\
             [backup]\nenable = true\nrepo = \"b2:kuma\"\nsecret = \"nowhere-at-all\"\n",
        );
        let err = ready(&on).unwrap_err().to_string();
        assert!(err.contains("/var/lib/kuma/secrets/nowhere-at-all.env"), "{err}");
        assert!(err.contains("0600"), "the message says how to make it: {err}");
    }
}
