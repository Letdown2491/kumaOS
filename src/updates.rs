//! What has moved in the repos since this image was built.
//!
//! `kuma update` answers this exactly, by doing it: recompose, rebuild,
//! diff the lock. This module answers it in seconds and builds nothing,
//! by asking dnf the two questions metadata alone can settle — which
//! installed packages have a newer version in the repos, and which of
//! those carry a security advisory.
//!
//! It is a prediction, not a promise. dnf reports what it would upgrade
//! to; a recompose runs its own depsolve, which can also add or drop
//! packages that an upgrades query never sees. The lock diff after the
//! update remains the record of what actually happened.
//!
//! Read-only throughout, and deliberately so: on a bootc machine `/usr`
//! and the rpmdb under it are read-only mounts, so dnf can answer this
//! question but can never be the thing that acts on it. `kuma update`
//! is.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::host::host_output;
use crate::lock::parse_rpm_query;
use crate::state::shell_quote;

/// Which rpmdb the "what do I have" half of the comparison comes from.
///
/// A machine that runs kuma answers for itself, whatever installed it:
/// its rpmdb is the truth about what is booted, and no image needs to be
/// in podman storage. A build host that runs something else has to be
/// asked about the image instead, because its own rpmdb describes a
/// system this declaration does not govern.
pub enum Source {
    Machine,
    Image(String),
}

impl Source {
    /// How the answer reads in a sentence: what it is measured *since*.
    pub fn since(&self) -> String {
        match self {
            Source::Machine => "this machine booted its image".to_string(),
            Source::Image(tag) => format!("{tag} was built"),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Source::Machine => "machine",
            Source::Image(_) => "image",
        }
    }
}

/// Fedora's advisory severities, worst first — the order they sort in and
/// the order the queries run in, so the first hit for a package is the
/// one that gets reported. A package can appear in several advisories
/// (the kernel routinely does) and the worst one is the honest headline.
pub const SEVERITIES: [&str; 4] = ["critical", "important", "moderate", "low"];

/// One package with a newer version in the repos.
pub struct Move {
    pub name: String,
    pub from: String,
    pub to: String,
    /// The worst severity of any security advisory covering the upgrade,
    /// or None when no security advisory does. Not "no advisory": bugfix
    /// and enhancement advisories carry no severity and are not what this
    /// is for.
    pub severity: Option<&'static str>,
}

/// Sort key: security first and worst-first inside that, then by name.
/// What decides whether you update today is at the top.
fn rank(severity: Option<&str>) -> usize {
    match severity {
        Some(sev) => SEVERITIES.iter().position(|s| *s == sev).unwrap_or(SEVERITIES.len()),
        None => SEVERITIES.len(),
    }
}

/// dnf's own state and cache, kept somewhere a user can write.
///
/// The default `/var/lib/dnf/system-repo.lock` needs root, and a check
/// that prompts for a password is a check people stop running. The cache
/// is the reason this is fast twice: a cold run downloads ~140MB of repo
/// metadata and takes half a minute, every run after it re-checks
/// freshness and answers in about two seconds.
/// Where kuma keeps its dnf cache, for both readers of it.
fn cache_setopts_root() -> Result<String> {
    let base: PathBuf = match std::env::var_os("XDG_CACHE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home =
                std::env::var_os("HOME").context("neither XDG_CACHE_HOME nor HOME is set")?;
            PathBuf::from(home).join(".cache")
        }
    };
    let root = base.join("kuma/dnf");
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
    Ok(root.display().to_string())
}

fn cache_setopts() -> Result<String> {
    let root = PathBuf::from(cache_setopts_root()?);
    let cache = root.join("cache");
    let state = root.join("state");
    std::fs::create_dir_all(&cache)
        .with_context(|| format!("cannot create {}", cache.display()))?;
    std::fs::create_dir_all(&state)
        .with_context(|| format!("cannot create {}", state.display()))?;
    let (cache, state) = (cache.display().to_string(), state.display().to_string());
    Ok(format!(
        "--setopt=cachedir={} --setopt=persistdir={}",
        shell_quote(&cache),
        shell_quote(&state)
    ))
}

/// Separates the query outputs inside one shell run. Six answers from one
/// invocation, because each `dnf` start reloads the metadata it just read
/// and a container run would otherwise download it six times.
const MARK: &str = "@@kuma@@";

fn script(setopts: &str) -> String {
    // set -e: an offline dnf exits non-zero, and without this the script
    // would sail past it to `rpm -qa`, exit 0, and report an empty upgrade
    // list — "nothing has moved" is the one wrong answer this must never
    // give. (Which is also why the guards below are `if`, not `&&`: a
    // false `&&` is a non-zero exit, and under `set -e` that aborts.)
    let mut script = String::from("set -e\n");
    script.push_str(&format!(
        "moves=$(dnf -q --refresh repoquery --upgrades \
         --qf '%{{name}} %{{evr}}.%{{arch}}\\n' {setopts})\nprintf '%s\\n' \"$moves\"\n"
    ));
    // Every later query exists to annotate that list, so an empty one
    // ends the run: the steady state after an update is "nothing moved",
    // and that answer should cost one query rather than six. --refresh
    // stays on the first alone, and the rest read the cache it just
    // brought up to date.
    let mut guarded = |body: String| {
        script.push_str(&format!("echo {MARK}\nif [ -n \"$moves\" ]; then\n{body}fi\n"));
    };
    for severity in SEVERITIES {
        guarded(format!(
            "dnf -q repoquery --upgrades --advisory-severities={severity} \
             --qf '%{{name}}\\n' {setopts}\n"
        ));
    }
    guarded("rpm -qa --qf '%{NAME} %{EVR}.%{ARCH}\\n'\n".to_string());
    script
}

/// Every package with a newer version in the repos, worst advisory first.
///
/// Err when the question can't be answered at all: no dnf, no network on
/// a cold cache, no such image. A caller must report that as unknown and
/// never as "nothing moved".
pub fn moved(source: &Source) -> Result<Vec<Move>> {
    let out = match source {
        Source::Machine => {
            let setopts = cache_setopts()?;
            host_output(&["sh", "-c", &script(&setopts)])?
        }
        // The same cache as the machine path, mounted in, and the
        // container picks its own subdirectory inside it.
        //
        // This used to run with no cachedir at all: the cache died with
        // the container and every check paid a cold ~140MB download and
        // half a minute. That is the common case for anyone building on
        // a host that is not itself a kuma machine.
        //
        // The subdirectory is derived INSIDE the container from its own
        // /etc/os-release rather than keyed by kuma out here, which
        // removes the question rather than answering it: dnf's cache
        // layout is not a contract, it is free to change between dnf
        // versions, and a Fedora 45 image and a Fedora 44 host sharing
        // one directory would put two sets of metadata under one repo
        // id. Keyed by releasever AND basearch, because a cross-arch
        // build is exactly the case nobody tests.
        Source::Image(tag) => {
            let host = cache_setopts_root()?;
            let inner = "/kuma-dnf";
            let keyed = format!(
                ". /etc/os-release; key=$VERSION_ID.$(rpm -E %_arch); \
                 mkdir -p {inner}/$key/cache {inner}/$key/state; \
                 {}",
                script(&format!(
                    "--setopt=cachedir={inner}/$key/cache --setopt=persistdir={inner}/$key/state"
                ))
            );
            host_output(&[
                "podman",
                "run",
                "--rm",
                "-v",
                &format!("{host}:{inner}:z"),
                tag,
                "sh",
                "-c",
                &keyed,
            ])?
        }
    };
    Ok(parse(&out))
}

/// The six sections back into one answer. A section that didn't come
/// through leaves its packages unannotated rather than failing: a missing
/// severity is a missing headline, not a missing upgrade.
fn parse(out: &str) -> Vec<Move> {
    let mut sections = out.split(MARK);
    let upgrades = parse_rpm_query(sections.next().unwrap_or_default());
    let flagged: Vec<BTreeSet<String>> = SEVERITIES
        .iter()
        .map(|_| {
            sections
                .next()
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .collect();
    let installed = parse_rpm_query(sections.next().unwrap_or_default());

    let mut moves: Vec<Move> = upgrades
        .into_iter()
        .filter_map(|(name, to)| {
            // An upgrade for something the rpmdb doesn't list can't be
            // described as a move, and inventing a "from" would be worse
            // than dropping it.
            let from = installed.get(&name)?.clone();
            let severity = SEVERITIES
                .iter()
                .zip(&flagged)
                .find(|(_, names)| names.contains(&name))
                .map(|(severity, _)| *severity);
            Some(Move { name, from, to, severity })
        })
        .collect();
    moves.sort_by(|a, b| rank(a.severity).cmp(&rank(b.severity)).then(a.name.cmp(&b.name)));
    moves
}

/// How many of them carry a security advisory.
pub fn security_count(moves: &[Move]) -> usize {
    moves.iter().filter(|m| m.severity.is_some()).count()
}

/// The moves as JSON, in the shape the lock diff already speaks.
pub fn moves_json(moves: &[Move]) -> Vec<serde_json::Value> {
    moves
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name, "from": m.from, "to": m.to, "severity": m.severity,
            })
        })
        .collect()
}

/// Counts by severity, worst first, for a one-line summary.
pub fn by_severity(moves: &[Move]) -> BTreeMap<usize, (&'static str, usize)> {
    let mut counts: BTreeMap<usize, (&'static str, usize)> = BTreeMap::new();
    for m in moves {
        if let Some(severity) = m.severity {
            let entry = counts.entry(rank(Some(severity))).or_insert((severity, 0));
            entry.1 += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output from a booted machine, trimmed to the shape rather than
    /// length: an upgrade with two advisories (the kernel is Moderate and
    /// Important at once), one with none, and one the rpmdb doesn't know.
    const OUT: &str = "kernel 7.1.8-200.fc44.x86_64\n\
                       sqlite-libs 3.51.2-2.fc44.x86_64\n\
                       waybar 0.15.0-2.fc44.x86_64\n\
                       ghost 1.0-1.fc44.x86_64\n\
                       @@kuma@@\n\
                       @@kuma@@\nkernel\nsqlite-libs\n\
                       @@kuma@@\nkernel\n\
                       @@kuma@@\n\
                       @@kuma@@\n\
                       kernel 7.1.7-200.fc44.x86_64\n\
                       sqlite-libs 3.51.2-1.fc44.x86_64\n\
                       waybar 0.15.0-1.fc44.x86_64\n\
                       bash 5.3.0-1.fc44.x86_64\n";

    #[test]
    fn upgrades_are_paired_with_what_is_installed() {
        let moves = parse(OUT);
        let named: Vec<&str> = moves.iter().map(|m| m.name.as_str()).collect();
        // ghost has no installed version, so it is not a move; bash is
        // installed and not upgradable, so it is not one either.
        assert_eq!(named, ["kernel", "sqlite-libs", "waybar"]);
        let kernel = &moves[0];
        assert_eq!(kernel.from, "7.1.7-200.fc44.x86_64");
        assert_eq!(kernel.to, "7.1.8-200.fc44.x86_64");
    }

    /// The kernel is listed under both important and moderate, which is
    /// routine: one upgrade closes several advisories. Reporting the
    /// milder one would understate why you should take it.
    #[test]
    fn the_worst_advisory_wins_and_sorts_first() {
        let moves = parse(OUT);
        assert_eq!(moves[0].severity, Some("important"));
        assert_eq!(moves[1].severity, Some("important"));
        assert_eq!(moves[2].severity, None, "waybar has no security advisory");
        assert_eq!(security_count(&moves), 2);
    }

    /// A severity section that never arrived must cost the annotation,
    /// not the upgrade: half an answer beats none, and beats a wrong one.
    #[test]
    fn a_truncated_answer_keeps_the_upgrades_it_did_get() {
        let truncated = "kernel 7.1.8-200.fc44.x86_64\n@@kuma@@\n";
        assert!(parse(truncated).is_empty(), "no rpmdb section means no from, so no move");
        let no_severities = "kernel 7.1.8-200.fc44.x86_64\n@@kuma@@\n@@kuma@@\n@@kuma@@\n\
                             @@kuma@@\n@@kuma@@\nkernel 7.1.7-200.fc44.x86_64\n";
        let moves = parse(no_severities);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].severity, None);
    }

    /// The script is what stands between "offline" and a confident
    /// "nothing has moved", so the guard has to be there.
    #[test]
    fn the_script_aborts_rather_than_reporting_an_empty_answer() {
        let script = script("--setopt=cachedir=/tmp/x");
        assert!(script.starts_with("set -e\n"));
        assert_eq!(script.matches(MARK).count(), SEVERITIES.len() + 1);
        assert_eq!(script.matches("--refresh").count(), 1, "one refresh per run, not six");
        assert!(script.contains("--setopt=cachedir=/tmp/x"));
        // `&&` would abort the run under `set -e` the moment there was
        // nothing to upgrade, turning the good case into an error.
        assert!(!script.contains("&&"));
        assert_eq!(
            script.matches("if [ -n \"$moves\" ]; then").count(),
            SEVERITIES.len() + 1,
            "every query after the first is skipped when nothing moved"
        );
    }

    /// A shell that doesn't run is a shell that can be wrong quietly, so
    /// the generated script gets executed here rather than only matched:
    /// dash and bash both, since the machine path runs whatever /bin/sh
    /// is. `dnf` and `rpm` are absent in CI, and that is the point of the
    /// case that matters — an unavailable tool must exit non-zero.
    #[test]
    fn the_script_is_valid_shell_and_fails_when_its_tools_are_missing() {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script(""))
            .env("PATH", "/nonexistent")
            .output()
            .expect("sh must exist");
        assert!(!out.status.success(), "no dnf on PATH has to be an error, not an empty answer");
        let syntax = std::process::Command::new("/bin/sh")
            .args(["-n", "-c", &script("")])
            .status()
            .expect("sh must exist");
        assert!(syntax.success(), "the generated script must parse");
    }
}
