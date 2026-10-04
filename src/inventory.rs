//! What this machine has installed, and what of it convergence owns.
//!
//! Two verbs used to gather this separately and compare against it
//! with their own copies of the rule: `kuma diff` and `capture` asked
//! their way, and the bare-`kuma` probe asked another, and the two
//! removal rules drifted apart once — a machine with an app from a
//! store read as drifted to the summary and correct to the diff. The
//! observation and the rule live here now, so that cannot happen by
//! accident again: every consumer asks the same questions and applies
//! the same ownership test, and differs only in which declaration it
//! compares against (the working copy for diff and capture, the image's
//! baked lists for the probe, since baked is what convergence follows).
//!
//! What is deliberately NOT unified is the cost: the probe runs on
//! every bare `kuma` and pays for nothing it does not count, so it
//! takes [`Machine::drift`] rather than [`observe`], which also asks
//! about rpm, user flatpaks and brew leaves for the diff verbs.

use crate::config::Config;
use crate::host::host_output;
use std::collections::BTreeSet;
use std::path::Path;

/// The brew binary's path, the one gate on asking about brew at all: a
/// machine without it has nothing to classify, whether a declaration
/// names formulae or not.
pub(crate) const BREW: &str = "/home/linuxbrew/.linuxbrew/bin/brew";
const BREW_CELLAR: &str = "/home/linuxbrew/.linuxbrew/Cellar";
const BREW_STATE: &str = "/home/linuxbrew/.linuxbrew/.kuma-brews";

/// What convergence installed, and therefore all it may remove. The same
/// file `kuma diff` reads; the brew half has always had its equivalent.
/// Read by the probe and by doctor too, so it is defined once here.
pub const FLATPAK_STATE: &str = "/var/lib/kuma/flatpaks-installed";

pub(crate) struct Machine {
    pub rpm: Option<BTreeSet<String>>,
    pub flatpak_system: Option<BTreeSet<String>>,
    /// `flatpak --user` installs: the documented imperative escape hatch.
    /// Convergence never touches these, and capture only takes one when
    /// it is named.
    pub flatpak_user: BTreeSet<String>,
    /// Apps the sync has ever installed (its state file): the only system
    /// apps convergence considers its own to remove. A store installs
    /// system-wide too, so scope alone can't tell whose an app is.
    pub flatpak_state: BTreeSet<String>,
    pub brew_installed: Option<BTreeSet<String>>,
    /// Explicit installs only. A dependency is baggage that arrived with
    /// a choice, not a choice.
    pub brew_leaves: BTreeSet<String>,
    /// Formulae the sync has ever installed (its state file): the only
    /// ones convergence considers its own to remove. Tap-qualified
    /// spellings ("owner/tap/tool") are read as their last segment, the
    /// name the Cellar and `brew list` report, so the record and the
    /// installed set can be compared by name at all.
    pub brew_state: BTreeSet<String>,
}

/// Which `[packages]` list an item belongs to.
pub enum List {
    Flatpak,
    Brew,
}

/// Ask the machine what it has. Read-only, and every query is allowed to
/// fail: an answer nobody could observe must never turn into a claim.
pub(crate) fn observe(config: &Config) -> Machine {
    // One rpm -qa beats a spawn per declared package, and rpm being
    // absent reads as "nothing to check" rather than "everything is
    // missing". Nothing declares rpm, nothing to ask.
    let ask = |args: &[&str]| host_output(args).ok().map(|out| owned_set(&out));

    let rpm = (!config.packages.rpm.is_empty())
        .then(|| ask(&["rpm", "-qa", "--qf", "%{NAME}\n"]))
        .flatten();
    let flatpak_system = ask(&["flatpak", "list", "--system", "--app", "--columns=application"]);
    let flatpak_user =
        ask(&["flatpak", "list", "--user", "--app", "--columns=application"]).unwrap_or_default();

    let brew_installed = brew_installed();
    // Nothing installed, nothing to classify.
    let brew_leaves = brew_installed
        .as_ref()
        .filter(|installed| !installed.is_empty())
        .and_then(|installed| leaves_from_receipts(installed).or_else(|| ask(&[BREW, "leaves"])))
        .unwrap_or_default();
    let brew_state = short_set(&std::fs::read_to_string(BREW_STATE).unwrap_or_default());
    let flatpak_state = owned_set(&std::fs::read_to_string(FLATPAK_STATE).unwrap_or_default());

    Machine {
        rpm,
        flatpak_system,
        flatpak_user,
        flatpak_state,
        brew_installed,
        brew_leaves,
        brew_state,
    }
}

/// The mutable edge only: what the drift summary counts, asked for no
/// more than it needs. The same queries `observe` makes minus the ones
/// only the diff verbs use — rpm lives in the image, user flatpaks and
/// brew leaves belong to capture — so the bare-`kuma` probe, which runs
/// on every invocation an agent makes, pays the same price it always
/// did: one flatpak spawn and file reads.
impl Machine {
    pub(crate) fn drift() -> Machine {
        Machine {
            rpm: None,
            flatpak_system: host_output(&[
                "flatpak",
                "list",
                "--system",
                "--app",
                "--columns=application",
            ])
            .ok()
            .map(|out| owned_set(&out)),
            flatpak_user: BTreeSet::new(),
            flatpak_state: owned_set(&std::fs::read_to_string(FLATPAK_STATE).unwrap_or_default()),
            brew_installed: brew_installed(),
            brew_leaves: BTreeSet::new(),
            brew_state: short_set(&std::fs::read_to_string(BREW_STATE).unwrap_or_default()),
        }
    }
}

/// Installed brews are Cellar directory names — a filesystem read, not
/// a multi-second `brew list`. Gated on brew's own existence, so a
/// machine without it reads as "nothing to classify" rather than
/// finding an orphaned Cellar full of formulae nobody can converge.
fn brew_installed() -> Option<BTreeSet<String>> {
    if !Path::new(BREW).exists() {
        return None;
    }
    let cellar = std::fs::read_dir(BREW_CELLAR).ok()?;
    Some(cellar.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
}

/// Leaves without asking brew: the installed formulae nothing else
/// depends on.
///
/// `brew leaves` resolves the dependency graph and costs about 1.2s,
/// more than every other query in `observe` put together. The same graph
/// is already on disk, one `runtime_dependencies` list per formula, and
/// reading all of them takes about 30ms. Same definition, same answer,
/// forty times faster.
///
/// None whenever the receipts can't be trusted (a formula with no
/// readable receipt, or a shape this doesn't recognise) and the caller
/// falls back to asking brew. That fallback is the price of reading a
/// format brew owns and could change.
fn leaves_from_receipts(installed: &BTreeSet<String>) -> Option<BTreeSet<String>> {
    let mut depended_on: BTreeSet<String> = BTreeSet::new();
    for name in installed {
        let mut read_one = false;
        for version in std::fs::read_dir(Path::new(BREW_CELLAR).join(name)).ok()? {
            let receipt = version.ok()?.path().join("INSTALL_RECEIPT.json");
            let Ok(text) = std::fs::read_to_string(&receipt) else { continue };
            let json: serde_json::Value = serde_json::from_str(&text).ok()?;
            for dep in json.get("runtime_dependencies")?.as_array()? {
                let full = dep.get("full_name")?.as_str()?;
                // A tap-qualified dependency (owner/tap/tool) has to match
                // the bare name `brew list` reports.
                depended_on.insert(short(full).to_string());
            }
            read_one = true;
        }
        if !read_one {
            return None;
        }
    }
    Some(installed.difference(&depended_on).cloned().collect())
}

impl Machine {
    /// What convergence would take back: it is installed, kuma is the one
    /// that installed it, and the declaration no longer names it.
    ///
    /// The same rule for flatpaks and for brew formulae, in one place
    /// because they drifted apart once: the brew half consulted its
    /// record of what it had installed and the flatpak half counted every
    /// undeclared app, so a machine with an app from a store looked
    /// drifted to the summary and correct to `kuma diff`.
    pub(crate) fn convergence_removes(
        &self,
        list: List,
        item: &str,
        declared: &BTreeSet<&str>,
    ) -> bool {
        let ours = match list {
            List::Flatpak => &self.flatpak_state,
            List::Brew => &self.brew_state,
        };
        ours.contains(item) && !declared.contains(item)
    }
}

pub(crate) fn to_set(text: &str) -> BTreeSet<&str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
}

/// The same, owned: an observation outlives the command output it was
/// read from, because two verbs consume it.
fn owned_set(text: &str) -> BTreeSet<String> {
    to_set(text).into_iter().map(str::to_string).collect()
}

/// A tapped formula ("owner/tap/tool") installs and reports under its
/// last segment, and the sync's state file records whatever spelling
/// `brew install` was given — which may be the tapped one. One mapping,
/// applied when the record is read, so the two halves of the comparison
/// agree on names.
pub(crate) fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn short_set(text: &str) -> BTreeSet<String> {
    to_set(text).iter().map(|name| short(name).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ownership rule, every side of it. A store-installed app is
    /// the owner's, a declared one is the declaration's business, and
    /// only convergence's own installs that the declaration dropped are
    /// converges back off the machine — the bug this one function
    /// exists to prevent was exactly these three being told apart
    /// differently by two callers.
    #[test]
    fn convergence_removes_only_its_own_undeclared_installs() {
        let machine = Machine {
            rpm: None,
            flatpak_system: None,
            flatpak_user: BTreeSet::new(),
            flatpak_state: set(&["org.example.Ours", "org.example.Declared"]),
            brew_installed: None,
            brew_leaves: BTreeSet::new(),
            brew_state: set(&["ripgrep", "fd"]),
        };
        let declared: BTreeSet<&str> = ["org.example.Declared", "fd"].into_iter().collect();
        assert!(machine.convergence_removes(List::Flatpak, "org.example.Ours", &declared));
        assert!(!machine.convergence_removes(List::Flatpak, "org.example.Declared", &declared));
        // A store put it here system-wide: undeclared, but not kuma's to take.
        let mut bare = Machine {
            rpm: None,
            flatpak_system: None,
            flatpak_user: BTreeSet::new(),
            flatpak_state: set(&["org.example.Ours"]),
            brew_installed: None,
            brew_leaves: BTreeSet::new(),
            brew_state: BTreeSet::new(),
        };
        let nothing: BTreeSet<&str> = BTreeSet::new();
        assert!(!bare.convergence_removes(List::Flatpak, "io.github.somebody.App", &nothing));
        bare.flatpak_state = set(&[]);
        assert!(!bare.convergence_removes(List::Flatpak, "org.example.Ours", &nothing));
        assert!(machine.convergence_removes(List::Brew, "ripgrep", &declared));
        assert!(!machine.convergence_removes(List::Brew, "fd", &declared));
    }

    /// The state file may spell a formula the tapped way; the Cellar
    /// never does. Read through `short_set`, the two agree on the name
    /// — before they did not, and a tapped formula convergence had
    /// installed read as the owner's to `diff` and removable to the
    /// summary at the same time.
    #[test]
    fn tapped_state_names_read_as_their_last_segment() {
        let state = short_set("ripgrep\nhomebrew/core/fzf\n");
        assert!(state.contains("fzf"));
        assert!(!state.iter().any(|name| name.contains('/')));
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }
}
