//! Day-2 visibility: `kuma diff` shows drift between the declaration and
//! this machine, `kuma doctor` checks the machine itself. Both are
//! read-only about the machine — convergence stays with the boot services
//! and timers, so a diff is safe to run out of curiosity. The one thing
//! doctor writes is kuma's own deployed-image stamp, which it refreshes
//! from the truth it alone (having root) can see.

use crate::config::Config;
use crate::hibernate;
use crate::host::{host_output, host_output_any};
use crate::inventory::{observe, to_set, List, Machine};
use crate::response;
use crate::snapshot;
use crate::state::{action_json, print_actions, Action, BAKED_BREWS, BAKED_CONFIG, BAKED_FLATPAKS};
use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The declaration this image was built from, baked in at build time.
/// Doctor reads it to learn what the machine was meant to do, which is a
/// better question than what its unit files happen to say.
/// The account a declaration asked for, baked in at build time.
const BAKED_USER: &str = "/usr/lib/kuma/user";
/// The account an installer asked for, written onto the target. A
/// published image declares none, so on an installed machine this is the
/// only one of the two that exists.
const INSTALLED_USER: &str = "/var/lib/kuma/user";

/// One drift observation: what would change ("add"/"remove"/"mismatch"),
/// which item, and the note that carries its consequence or cure.
struct DiffEntry {
    change: &'static str,
    item: String,
    note: String,
}

/// A [packages]/[services] section of the diff. `skipped` is a section-
/// level caveat ("flatpak unavailable") — while one is present, "no
/// drift" would be a claim the observation can't back.
struct DiffSection {
    name: &'static str,
    entries: Vec<DiffEntry>,
    skipped: Option<String>,
}

/// What this machine actually has, gathered in one pass. `diff` renders
/// it as drift and `capture` filters it into declaration edits, and both
/// read this same observation on purpose: a diff that threatens to remove
/// something capture won't offer to keep is the worst bug this pair could
/// have, and sharing the source makes it unrepresentable rather than
/// merely unlikely.
///
/// `None` means "couldn't look" (tool absent, brew not bootstrapped),
/// which is not the same as "nothing there" and never reads as drift.
/// Something this machine has that the declaration does not name.
pub(crate) struct Candidate {
    /// The [packages] list it would join. Only ever "flatpak" or "brew";
    /// capture.rs carries the reasoning for what can never be here.
    pub list: &'static str,
    pub item: String,
    /// Convergence removes this on its next run, so declaring it is the
    /// only way to keep it. The rest are merely unreproducible.
    pub doomed: bool,
    /// Declaring it changes what it *is* rather than just writing it
    /// down: a --user flatpak becomes a system one, installed for every
    /// account and owned by convergence from then on. Never in the
    /// default set.
    pub promotes: bool,
}

/// The undeclared half of the comparison, which `diff` reports and
/// `capture` offers to keep. Pure over the observation, so the rules for
/// what counts as a choice are testable without a machine to run on.
///
/// rpm is deliberately absent, and not because it is hard: on a bootc
/// machine you *cannot* imperatively install one, so [packages].rpm is
/// already declarative by construction and there is nothing to capture.
/// The mutable edge is exactly flatpak and brew, which is exactly this.
pub(crate) fn candidates(config: &Config, machine: &Machine) -> Vec<Candidate> {
    let flatpak: BTreeSet<&str> = config.packages.flatpak.iter().map(String::as_str).collect();
    let brew: BTreeSet<&str> = config.packages.brew.iter().map(String::as_str).collect();
    let mut out: Vec<Candidate> = Vec::new();

    if let Some(installed) = &machine.flatpak_system {
        for app in installed.iter().filter(|a| !flatpak.contains(a.as_str())) {
            let doomed = machine.convergence_removes(List::Flatpak, app, &flatpak);
            out.push(Candidate { list: "flatpak", item: app.clone(), doomed, promotes: false });
        }
    }

    if let Some(installed) = &machine.brew_installed {
        for f in installed.iter().filter(|f| !brew.contains(f.as_str())) {
            // A dependency of a choice is neither the owner's nor a
            // choice, so it is never offered; convergence taking back
            // what it installed is the exception.
            let doomed = machine.convergence_removes(List::Brew, f, &brew);
            if !doomed && !machine.brew_leaves.contains(f) {
                continue;
            }
            out.push(Candidate { list: "brew", item: f.clone(), doomed, promotes: false });
        }
    }

    for app in &machine.flatpak_user {
        if flatpak.contains(app.as_str())
            || machine.flatpak_system.as_ref().is_some_and(|s| s.contains(app))
        {
            continue;
        }
        out.push(Candidate { list: "flatpak", item: app.clone(), doomed: false, promotes: true });
    }

    // Urgent first (declare it or lose it), opt-in last, alphabetical
    // inside each band so two runs read the same.
    out.sort_by(|a, b| {
        (!a.doomed, a.promotes, a.list, &a.item).cmp(&(!b.doomed, b.promotes, b.list, &b.item))
    });
    out
}

/// Three-way, because that's how changes actually flow: kuma.toml is the
/// truth, the image carries a baked copy of the declaration, and the
/// machine converges to the IMAGE's copy — so config edits that were never
/// built show up here as "image declaration behind kuma.toml", not as
/// drift the next convergence run would fix.
pub fn diff(config: &Config, config_path: &Path, json: bool) -> Result<()> {
    let machine = observe(config);
    let found = candidates(config, &machine);
    let mut sections: Vec<DiffSection> = Vec::new();
    let mut stale_image = false;

    // rpm lives in the image itself; missing means the declaration was
    // never built (or the build was never switched to).
    let mut entries = Vec::new();
    if let Some(installed) = &machine.rpm {
        entries = config
            .packages
            .rpm
            .iter()
            .filter(|pkg| !installed.contains(pkg.as_str()))
            .map(|pkg| DiffEntry {
                change: "add",
                item: pkg.to_string(),
                note: "declared, missing from the running image".into(),
            })
            .collect();
    }
    sections.push(DiffSection { name: "packages.rpm", entries, skipped: None });

    let declared: BTreeSet<&str> = config.packages.flatpak.iter().map(String::as_str).collect();
    let mut entries = Vec::new();
    let mut skipped = None;
    match &machine.flatpak_system {
        Some(installed) => {
            for app in declared.iter().filter(|a| !installed.contains(**a)) {
                entries.push(DiffEntry {
                    change: "add",
                    item: app.to_string(),
                    note: "declared, not installed (convergence installs it)".into(),
                });
            }
            for c in found.iter().filter(|c| c.list == "flatpak" && c.doomed) {
                entries.push(DiffEntry {
                    change: "remove",
                    item: c.item.clone(),
                    note: "installed, not declared (convergence removes it)".into(),
                });
            }
        }
        None => skipped = Some("flatpak unavailable, skipped".to_string()),
    }
    stale_image |= image_list_stale(BAKED_FLATPAKS, &declared);
    sections.push(DiffSection { name: "packages.flatpak", entries, skipped });

    let declared: BTreeSet<&str> = config.packages.brew.iter().map(String::as_str).collect();
    let mut entries = Vec::new();
    let mut skipped = None;
    if !declared.is_empty() || Path::new(crate::inventory::BREW).exists() {
        match &machine.brew_installed {
            Some(installed) => {
                for f in declared.iter().filter(|f| !installed.contains(**f)) {
                    entries.push(DiffEntry {
                        change: "add",
                        item: f.to_string(),
                        note: "declared, not installed (convergence installs it)".into(),
                    });
                }
                for c in found.iter().filter(|c| c.list == "brew" && c.doomed) {
                    entries.push(DiffEntry {
                        change: "remove",
                        item: c.item.clone(),
                        note: "no longer declared (convergence removes it)".into(),
                    });
                }
            }
            None => skipped = Some("brew not bootstrapped yet; first boot installs it".to_string()),
        }
    }
    stale_image |= image_list_stale(BAKED_BREWS, &declared);
    sections.push(DiffSection { name: "packages.brew", entries, skipped });

    // Ad-hoc installs are the non-doomed half of the same set:
    // convergence leaves them alone, so the only thing undeclared costs
    // them is that a rebuild elsewhere wouldn't reproduce them. Per-user
    // flatpaks are excluded because declaring one would change what it
    // is, and diff has never reported them as anything to reconcile.
    let adhoc_brews: Vec<String> =
        found.iter().filter(|c| c.list == "brew" && !c.doomed).map(|c| c.item.clone()).collect();
    let adhoc_flatpaks: Vec<String> = found
        .iter()
        .filter(|c| c.list == "flatpak" && !c.doomed && !c.promotes)
        .map(|c| c.item.clone())
        .collect();

    // Service state is machine state (an /etc overlay change survives image
    // updates), so the cure is systemctl, not a rebuild — name it when it
    // plainly applies.
    let mut entries = Vec::new();
    // One call for every unit this declaration mentions, in both
    // directions, rather than one spawn per line of [services].
    let mentioned: Vec<&str> =
        config.services.enable.iter().chain(&config.services.disable).map(String::as_str).collect();
    let states = unit_states(&mentioned);
    let state_of = |unit: &str| states.get(unit).cloned().unwrap_or_else(|| "not found".into());
    for svc in &config.services.enable {
        let state = state_of(svc);
        if state != "enabled" && state != "alias" {
            let cure = if state == "disabled" {
                format!("; `sudo systemctl enable {svc}` reconciles")
            } else {
                String::new()
            };
            entries.push(DiffEntry {
                change: "mismatch",
                item: svc.clone(),
                note: format!("declared enable, currently {state}{cure}"),
            });
        }
    }
    for svc in &config.services.disable {
        if state_of(svc) == "enabled" {
            entries.push(DiffEntry {
                change: "mismatch",
                item: svc.clone(),
                note: format!("declared disable, currently enabled; `sudo systemctl disable {svc}` reconciles"),
            });
        }
    }
    sections.push(DiffSection { name: "services", entries, skipped: None });

    // Permissions are machine state the same way services are, and the
    // store is shared with whoever else edits it, so a difference here
    // is as likely to be a Flatseal toggle worth keeping as it is a
    // drift worth erasing. Which is the point of reporting it at all.
    let home = std::env::var("HOME").unwrap_or_default();
    let entries = crate::overrides::drift(&config.overrides, Path::new("/"), Path::new(&home))
        .iter()
        .map(|d| DiffEntry {
            change: d.change,
            item: d.item(),
            note: match d.change {
                "add" => "declared, not applied (convergence sets it)".into(),
                _ => "kuma set it, no longer declared (convergence removes it)".into(),
            },
        })
        .collect();
    sections.push(DiffSection { name: "overrides", entries, skipped: None });
    stale_image |= crate::overrides::image_stale(&config.overrides, Path::new("/"));

    let drift = sections.iter().any(|s| !s.entries.is_empty());
    let converge_hint = sections
        .iter()
        .any(|s| s.name != "packages.rpm" && s.name != "services" && !s.entries.is_empty());
    // Drift is a fork, not an error: everything undeclared can be kept by
    // writing it down as easily as it can be erased by converging. When
    // convergence is about to destroy something, the keeping edge goes
    // first, because that is the one with a deadline.
    let mut capture = (!found.iter().all(|c| c.promotes)).then(|| {
        Action::new("capture", "kuma capture", "keep them: declare what this machine already runs")
    });
    let mut actions: Vec<Action> = Vec::new();
    if found.iter().any(|c| c.doomed) {
        actions.extend(capture.take());
    }
    if stale_image {
        actions.push(Action::new(
            "build",
            "kuma build",
            "bake the edit; `kuma switch` and reboot carry it to the machine",
        ));
    } else if converge_hint {
        actions.push(Action::new(
            "sync",
            "kuma sync",
            "converge now; otherwise the boot/daily run picks this up",
        ));
    }
    actions.extend(capture);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&diff_json(
                config_path,
                &sections,
                &adhoc_brews,
                &adhoc_flatpaks,
                stale_image,
                drift,
                &actions
            ))?
        );
        return Ok(());
    }

    let observed_all = sections.iter().all(|s| s.skipped.is_none());
    for section in &sections {
        if section.entries.is_empty() && section.skipped.is_none() {
            continue;
        }
        println!("{}", section.name);
        for e in &section.entries {
            let mark = match e.change {
                "add" => '+',
                "remove" => '-',
                _ => '!',
            };
            println!("  {mark} {}  {}", e.item, e.note);
        }
        if let Some(reason) = &section.skipped {
            println!("  ({reason})");
        }
    }
    if !adhoc_flatpaks.is_empty() {
        println!("Ad-hoc flatpaks, kept as yours: {}", adhoc_flatpaks.join(", "));
    }
    if !adhoc_brews.is_empty() {
        println!("Ad-hoc brews, kept as yours: {}", adhoc_brews.join(", "));
    }
    if stale_image {
        println!("\nThe image's baked declaration is behind {}.", config_path.display());
    } else if !actions.is_empty() {
        println!();
    }
    if !actions.is_empty() {
        print_actions(&actions);
    }
    if !drift && !stale_image && observed_all {
        println!("No drift: this machine matches {}.", config_path.display());
    }
    Ok(())
}

fn diff_json(
    config_path: &Path,
    sections: &[DiffSection],
    adhoc_brews: &[String],
    adhoc_flatpaks: &[String],
    stale_image: bool,
    drift: bool,
    actions: &[Action],
) -> serde_json::Value {
    serde_json::json!({
        "config": config_path.display().to_string(),
        "drift": drift,
        "image_declaration_stale": stale_image,
        "sections": sections.iter()
            .filter(|s| !s.entries.is_empty() || s.skipped.is_some())
            .map(|s| serde_json::json!({
                "name": s.name,
                "skipped": s.skipped,
                "entries": s.entries.iter().map(|e| serde_json::json!({
                    "change": e.change, "item": e.item, "note": e.note,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        "adhoc_brews": adhoc_brews,
        "adhoc_flatpaks": adhoc_flatpaks,
        "actions": actions.iter().map(action_json).collect::<Vec<_>>(),
    })
}

/// The baked copy at /usr/lib/kuma/<list> is what convergence follows;
/// absent (not a kuma image, list never declared) means nothing to lag.
pub(crate) fn image_list_stale(path: &str, declared: &BTreeSet<&str>) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => to_set(&text) != *declared,
        Err(_) => false,
    }
}

/// Whether what this image baked is behind the declaration in hand.
///
/// Only the lists a converger reads, because that is the question being
/// asked: `kuma sync` starts convergers, convergers read
/// `/usr/lib/kuma`, and an edit that has not been baked cannot reach
/// them however many times you run it. rpm is out of scope here for the
/// opposite reason to usual: it never converges at all, so a stale rpm
/// list is a rebuild you have not done rather than a sync that lied.
pub(crate) fn baked_is_behind(config: &Config, root: &Path) -> bool {
    let flatpak: BTreeSet<&str> = config.packages.flatpak.iter().map(String::as_str).collect();
    let brew: BTreeSet<&str> = config.packages.brew.iter().map(String::as_str).collect();
    let list = |name: &str| root.join("usr/lib/kuma").join(name).to_string_lossy().to_string();
    image_list_stale(&list("flatpaks"), &flatpak)
        || image_list_stale(&list("brews"), &brew)
        || crate::overrides::image_stale(&config.overrides, root)
}

/// Everything doctor asks about a unit, for every unit, in one systemctl
/// call. There were one or two spawns per unit, and batching them was
/// right for the reason below rather than for the number this comment
/// used to give: it claimed 140ms a spawn, and a later measurement on
/// this machine put it at 15-20ms for a five-unit, five-property query.
/// The saving is real and small; the reason to do it is that one call
/// cannot disagree with itself about which unit answered.
///
/// Keyed by the unit rather than positional: `--value` is terser but
/// separates its answers with blank lines, so lining them up with the
/// units asked for means relying on a shape systemd never promised, and
/// getting it wrong pairs one unit's verdict with another's name. Asking
/// for Id as well makes every answer say who it belongs to, which makes
/// that mistake unrepresentable rather than merely unlikely.
#[derive(Default)]
struct UnitFacts {
    active: String,
    result: String,
    /// The last run's invocation, which is how its own output is found
    /// again after it has exited.
    invocation: String,
    /// When the last run ended, as a unix epoch. Absent for a unit that
    /// has never run.
    last_exit: Option<i64>,
}

fn unit_facts(units: &[&str]) -> BTreeMap<String, UnitFacts> {
    if units.is_empty() {
        return BTreeMap::new();
    }
    let mut args = vec![
        "systemctl",
        "show",
        // Timestamps default to a locale-shaped string; this one is
        // arithmetic, so ask for the epoch rather than parse prose.
        "--timestamp=unix",
        "-p",
        "Id",
        "-p",
        "ActiveState",
        "-p",
        "Result",
        "-p",
        "InvocationID",
        "-p",
        "ExecMainExitTimestamp",
    ];
    args.extend(units);
    parse_unit_facts(&host_output_any(&args).unwrap_or_default())
}

/// Split from the call so the property that matters is testable: every
/// field lands on the unit whose `Id=` preceded it. `systemctl show`
/// emits one flat stream for many units, so a key attributed to the
/// wrong Id would misreport one unit's health as another's.
fn parse_unit_facts(text: &str) -> BTreeMap<String, UnitFacts> {
    let mut facts: BTreeMap<String, UnitFacts> = BTreeMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            "Id" => current = value.to_string(),
            "ActiveState" => facts.entry(current.clone()).or_default().active = value.to_string(),
            "Result" => facts.entry(current.clone()).or_default().result = value.to_string(),
            "InvocationID" => {
                facts.entry(current.clone()).or_default().invocation = value.to_string()
            }
            // `@1787093909` with --timestamp=unix, and empty for a unit
            // that has never run.
            "ExecMainExitTimestamp" => {
                facts.entry(current.clone()).or_default().last_exit =
                    value.strip_prefix('@').and_then(|e| e.parse().ok())
            }
            _ => {}
        }
    }
    facts
}

/// How much of a failing unit's own words a health report will carry.
/// Long enough for the sentence that names the cause, short enough that
/// a report stays a column and not a transcript. Set by the line this
/// was built for: at 100 the flatpak failure could show which app broke
/// or why, but not both.
const REASON_MAX: usize = 120;

/// One line, bounded, no control characters: a journal line is arbitrary
/// text from whatever the unit ran, and this one gets printed inside a
/// health report.
///
/// What gets dropped is the middle, not the tail. Errors nest their
/// context front to back, so the first clause names the thing that
/// failed and the last one says why: flatpak's was "Failed to update
/// org.mozilla.firefox: While pulling … : Decompressed delta part
/// exceeds configured limit". Cutting the end kept the half a person
/// could already guess from the unit's name and threw away the half
/// they came for.
/// crypt(5) hashes, masked wherever a unit's own output is about to be
/// repeated back.
///
/// `doctor --report` exists to be pasted, which is why the declaration
/// it carries goes through `redact_declaration` first. A unit's journal
/// is a second way into that same report and this is its equivalent:
/// `kuma-user-sync` holds the declared hash in its environment and pipes
/// it to `chpasswd -e`, so the material is one `set -x` away from the
/// journal. Nothing echoes it today. That is a fact about the current
/// script rather than a property of the report, and the report is the
/// place this project already decided not to rely on luck.
///
/// Matches the shape rather than any particular hash, so it also covers
/// a hash the machine has and the declaration does not.
fn mask_hashes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        let (before, from_dollar) = rest.split_at(at);
        out.push_str(before);
        // `$id$` opens every crypt hash: $6$, $y$, $2b$, $argon2id$.
        let id: String =
            from_dollar[1..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let opens = !id.is_empty() && from_dollar[1 + id.len()..].starts_with('$');
        if !opens {
            out.push('$');
            rest = &from_dollar[1..];
            continue;
        }
        out.push_str("<redacted>");
        // A hash runs to the next whitespace; everything after it on the
        // line is still the sentence somebody needs.
        let end = from_dollar.find(char::is_whitespace).unwrap_or(from_dollar.len());
        rest = &from_dollar[end..];
    }
    out.push_str(rest);
    out
}

fn one_line_reason(line: &str) -> String {
    const JOIN: &str = " ... ";
    let line = mask_hashes(line);
    let clean: String = line.trim().chars().filter(|c| !c.is_control()).collect();
    let chars: Vec<char> = clean.chars().collect();
    if chars.len() <= REASON_MAX {
        return clean;
    }
    let room = REASON_MAX - JOIN.chars().count();
    let head = room / 2;
    let tail = room - head;
    let front: String = chars[..head].iter().collect();
    let back: String = chars[chars.len() - tail..].iter().collect();
    // Land on word boundaries when one is within reach. A cut through
    // the middle of a word reads as corruption rather than as elision
    // ("While pullin ... ssed delta part"), and the few characters it
    // costs are characters nobody could read anyway. A boundary further
    // off than this is not worth the text it would eat, so the hard cut
    // stands: long paths and URLs have no spaces to find.
    const NUDGE: usize = 12;
    let front = match front.rfind(' ') {
        Some(cut) if front.len() - cut <= NUDGE => &front[..cut],
        _ => front.as_str(),
    };
    let back = match back.find(' ') {
        Some(cut) if cut <= NUDGE => &back[cut..],
        _ => back.as_str(),
    };
    format!("{}{JOIN}{}", front.trim_end(), back.trim_start())
}

/// The last thing a failed unit said, scoped to the invocation that
/// failed.
///
/// `Result=exit-code` is true and inert: it says a run failed and
/// withholds the only sentence a person can act on, which cost a
/// journalctl round trip every time. Scoping by invocation ID rather
/// than filtering the unit's journal by string is what keeps systemd's
/// own "Failed to start" lines out of it; those are about the unit, not
/// from it, and they say nothing the grade did not already say.
fn last_run_reason(invocation: &str) -> Option<String> {
    if invocation.is_empty() {
        return None;
    }
    let out = host_output_any(&[
        "journalctl",
        &format!("_SYSTEMD_INVOCATION_ID={invocation}"),
        "-o",
        "cat",
        "-n",
        "50",
        "--no-pager",
    ])
    .ok()?;
    let line = out.lines().rev().find(|l| !l.trim().is_empty())?;
    let reason = one_line_reason(line);
    if reason.is_empty() {
        None
    } else {
        Some(reason)
    }
}

/// Every unit's enablement in one call.
///
/// `systemctl is-enabled` takes many units and answers one line each, in
/// the order asked, so a declaration with a long `[services]` block cost
/// one spawn per entry for no reason. is-enabled exits non-zero when any
/// unit is disabled but still names every state on stdout, so the status
/// is ignored and the lines are read regardless.
///
/// A unit systemd does not know produces a line too, so pairing by
/// position holds; anything short of that is reported as not found
/// rather than silently shifting every later answer up by one.
fn unit_states(units: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if units.is_empty() {
        return out;
    }
    let mut args = vec!["systemctl", "is-enabled"];
    args.extend_from_slice(units);
    let answered = host_output_any(&args).unwrap_or_default();
    let mut lines = answered.lines();
    for unit in units {
        let state = lines.next().unwrap_or("").trim();
        let state = if state.is_empty() { "not found" } else { state };
        out.insert((*unit).to_string(), state.to_string());
    }
    out
}

#[derive(Debug, PartialEq)]
enum Grade {
    Ok,
    Warn,
    Fail,
}

/// One doctor check's verdict, held rather than printed so the same run
/// can render as text or as JSON for agents.
struct Finding {
    grade: Grade,
    name: String,
    detail: String,
    fix: Option<Action>,
}

/// Machine health: the deployment, the convergence machinery, and the
/// hardware basics a desktop lives on.
///
/// Two claims this used to make are no longer true and are corrected
/// rather than deleted, because both were load-bearing when written.
/// "No config needed" stopped holding when snapshots, backup and the
/// /etc check began reading the baked declaration to know what the
/// machine promised. "Read-only" stopped holding when the deployment
/// check began healing the stamp bare `kuma` reads; that write is kuma's
/// own metadata rather than machine state, and it is argued at its own
/// site.
/// A finding that has a cure carries it as an action — a diagnosis
/// without its next command is a dead end.
/// Does this fstab still have an active `/` entry, the one kuma-fstab-sync
/// exists to comment out? The field test rather than a regex over the line:
/// "root" appears inside subvolume names and inside /var/roothome, and a
/// loose match there decides whether a real failure gets excused.
fn fstab_declares_root(text: &str) -> bool {
    text.lines().any(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next()) {
            (Some(dev), Some(target)) => !dev.starts_with('#') && target == "/",
            _ => false,
        }
    })
}

/// Is this installer media rather than a machine?
///
/// A marker file, not an inference. The tempting test is "a kuma image
/// that is not ostree-booted", but that is also true of `podman run` on
/// the image and of an image being built, and a check that quietly
/// changes its mind about what it is looking at is worse than one that
/// asks. The live layer writes this; nothing else does.
fn live_media() -> bool {
    Path::new(crate::liveiso::LIVE_MARKER).exists()
}

/// What this media was built from, if it is media and it recorded one.
///
/// `None` on a machine, and on media built by a kuma that predates the
/// record, which is why every caller has to have an answer for "nothing
/// recorded" rather than treating it as an error.
pub fn live_source() -> Option<String> {
    std::fs::read_to_string(crate::liveiso::LIVE_SOURCE)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// A kuma machine that kuma actually converges: the image is kuma's AND
/// it was booted as a deployment. The second half is what separates a
/// running machine from live media or a container of the same image, and
/// it is the condition kuma's own boot units already use.
fn booted_kuma_machine() -> bool {
    Path::new("/usr/lib/kuma").is_dir() && Path::new("/run/ostree-booted").exists()
}

pub fn doctor(json: bool, as_report: bool) -> Result<()> {
    // Started first and collected last. These two podman calls were
    // ~60% of a doctor run and nothing else here waits on them, so they
    // run alongside everything that follows instead of after it.
    //
    // Measured on 44.0.0, a kuma 44 machine, warm caches, sudo declined
    // so the root checks fail fast: `kuma doctor --json` answers in
    // ~6.6 s ± 0.6 (15 runs) while the bare `kuma --json` probe answers
    // in ~154 ms. kuma's own CPU is under 2 s of the 6.6 — the rest is
    // waiting on the tools each check spawns, which is also why the
    // release profile's LTO does not move this number. A faster doctor
    // is fewer or more parallel probes, not faster Rust.
    let leftovers = build_leftovers_probe();
    let mut findings: Vec<Finding> = Vec::new();
    let mut report = |grade: Grade, name: &str, detail: String, fix: Option<Action>| {
        findings.push(Finding { grade, name: name.to_string(), detail, fix });
    };

    let live = live_media();
    // Live media has no deployment by design, so asking about one can
    // only produce a warning about a fact rather than a problem.
    //
    // Fetched even on live media when a report was asked for: a report
    // from installer media is exactly the case where somebody needs to
    // see that there is no deployment, rather than see the question
    // skipped.
    let status = (!live || as_report).then(bootc_status).flatten();
    if !live {
        check_deployment(status.as_ref(), &mut report);
    }

    match host_output_any(&["systemctl", "--failed", "--plain", "--no-legend"]) {
        Ok(out) if out.is_empty() => {
            report(Grade::Ok, "units", "no failed systemd units".into(), None)
        }
        Ok(out) => {
            let names: Vec<&str> =
                out.lines().filter_map(|l| l.split_whitespace().next()).collect();
            // Anaconda writes a `/` line into fstab that composefs can't
            // remount, so this one fails on every boot of an installed
            // system — real, but cosmetic; don't bury real failures in it.
            //
            // Narrowed to the cause rather than the unit name. kuma-fstab-sync
            // now comments that line out on first boot, so a machine can fail
            // this unit for the known reason (line still active, converger has
            // not run yet) or for an unknown one (line already gone, so
            // whatever broke is not this). Excusing the unit by name would
            // keep calling the second case benign forever, which is how a
            // workaround outlives its bug and starts hiding news.
            let root_line_active = std::fs::read_to_string("/etc/fstab")
                .map(|f| fstab_declares_root(&f))
                .unwrap_or(false);
            let (benign, real): (Vec<&str>, Vec<&str>) = names.iter().partition(|n| {
                **n == "systemd-remount-fs.service"
                    && Path::new("/run/ostree-booted").exists()
                    && root_line_active
            });
            if !real.is_empty() {
                let fix = Action::new(
                    "inspect",
                    format!("systemctl status {}", real[0]),
                    "see why it failed",
                );
                report(Grade::Fail, "units", format!("failed: {}", real.join(", ")), Some(fix));
            }
            if !benign.is_empty() {
                report(
                    Grade::Warn,
                    "units",
                    "systemd-remount-fs failed, known-benign: Anaconda's fstab `/` line can't be remounted over composefs".into(),
                    Some(Action::new(
                        "reboot",
                        "sudo systemctl reboot".to_string(),
                        "kuma-fstab-sync has commented the line out; the unit stops failing next boot",
                    )),
                );
            }
        }
        Err(_) => report(Grade::Warn, "units", "systemctl unavailable".into(), None),
    }

    // Three different places kuma can be asked how a machine is doing,
    // and only one of them has a machine.
    //
    // The checks below all grade a system kuma converges: timers that
    // should be running, a bootloader that should count boot attempts,
    // an /etc that should match the image. Live media runs none of that
    // on purpose and an unbooted image has not started yet, so reporting
    // those as failures states a deliberate design as a fault. That
    // matters most exactly where it looked worst: the first `kuma
    // doctor` a newcomer runs is the one inside the live session.
    if live {
        report(
            Grade::Ok,
            "live media",
            "running from installer media; nothing converges and nothing persists".into(),
            None,
        );
    } else if booted_kuma_machine() {
        check_convergence(&mut report);
        check_install_provenance(&mut report);
        check_overrides(&override_roots(), &mut report);
        check_enablements(Path::new(ETC_UNITS), &mut report);
        // The same shape one directory over: `systemctl --global enable`
        // writes here, so an image can leave a broken one behind exactly
        // like a system unit can. A person's own ~/.config/systemd/user is
        // deliberately not read; that is theirs.
        check_enablements(Path::new("/etc/systemd/user"), &mut report);
        check_snapshots(&mut report);
        check_shell(&mut report);
        check_niri_shadow(&mut report);
        check_backup(&mut report);
        check_nostr(&mut report);
        check_boot_health(&mut report);
        check_boot_titles(Path::new(crate::bootentries::ENTRIES), Path::new("/"), &mut report);
        check_encryption(&mut report);
        check_hibernate(&mut report);
        check_etc_drift(&mut report);
    } else if Path::new("/usr/lib/kuma").is_dir() {
        report(
            Grade::Warn,
            "kuma",
            "a kuma image, but not booted as a deployment; convergence checks skipped".into(),
            None,
        );
    } else {
        // The same machine state `kuma sync` and bare `kuma` both name an
        // edge for, and doctor's own rule eight lines above the checks
        // says a diagnosis without its next command is a dead end. This
        // is also the machine most people run doctor on first.
        report(
            Grade::Warn,
            "kuma",
            "not running a kuma image; convergence checks skipped".into(),
            Some(Action::new(
                "adopt",
                "kuma build",
                "build an image from a declaration, then `kuma switch` boots this machine into it",
            )),
        );
    }

    // Any kuma image, booted or on live media, because installer media is
    // where the published image is pulled and an unbooted one still holds
    // the policy that will govern it. Not on a host that merely builds
    // kuma: that machine will never `bootc upgrade` from kuma's registry,
    // so the answer there is true and beside the point, and doctor has
    // already said it is not running a kuma image.
    if Path::new("/usr/lib/kuma").is_dir() {
        check_signature_policy(&mut report);
    }
    check_gpu(&mut report);
    check_build_leftovers(leftovers, &mut report);

    match host_output(&["df", "-h", "--output=pcent,avail", "/sysroot"])
        .or_else(|_| host_output(&["df", "-h", "--output=pcent,avail", "/"]))
    {
        Ok(out) => {
            let fields: Vec<&str> = out.lines().nth(1).unwrap_or("").split_whitespace().collect();
            let pcent: u32 =
                fields.first().and_then(|p| p.trim_end_matches('%').parse().ok()).unwrap_or(0);
            let detail = format!("{}% used, {} free", pcent, fields.get(1).unwrap_or(&"?"));
            if pcent >= 90 {
                let fix = Action::new("clean", "kuma clean", "reclaim build leftovers first");
                report(Grade::Warn, "disk", detail, Some(fix));
            } else {
                report(Grade::Ok, "disk", detail, None);
            }
        }
        Err(_) => report(Grade::Warn, "disk", "df unavailable".into(), None),
    }

    let fails = findings.iter().filter(|f| matches!(f.grade, Grade::Fail)).count();
    let warns = findings.iter().filter(|f| matches!(f.grade, Grade::Warn)).count();
    if as_report {
        println!("{}", serde_json::to_string_pretty(&report_json(&findings, status.as_ref()))?);
        if fails > 0 {
            // The report is the machine-readable ending: the verdict
            // rides inside it as `ok` and `error`, and a second document
            // here would make stdout two documents back to back.
            return Err(
                response::Emitted(format!("{fails} check(s) failed, {warns} warning(s)")).into()
            );
        }
    } else if json {
        println!("{}", serde_json::to_string_pretty(&doctor_json(&findings))?);
        if fails > 0 {
            // Same ending rule as the report: the document carries the
            // verdict, and the marker below keeps main from appending a
            // second one.
            return Err(
                response::Emitted(format!("{fails} check(s) failed, {warns} warning(s)")).into()
            );
        }
    } else {
        for f in &findings {
            let mark = match f.grade {
                Grade::Ok => "ok  ",
                Grade::Warn => "warn",
                Grade::Fail => "FAIL",
            };
            println!("{mark}  {}: {}", f.name, f.detail);
            if let Some(fix) = &f.fix {
                println!("      → {}   {}", fix.cmd, fix.why);
            }
        }
        println!();
        match (fails, warns) {
            (0, 0) => println!("All checks passed."),
            (0, _) => println!("{warns} warning(s)."),
            _ => {}
        }
        // Non-zero exit on failed checks either way; in prose the
        // summary rides the error, because stdout here is for people.
        if fails > 0 {
            bail!("{fails} check(s) failed, {warns} warning(s)");
        }
    }
    Ok(())
}

/// What a stranger pastes when their machine did not come up.
///
/// `--json` answers "what does doctor think", which is enough for an
/// agent already standing on the machine. Somebody filing a bug is not
/// standing on it, and the three things always asked first — which kuma,
/// which image, what did you declare — are exactly the three `--json`
/// does not carry.
///
/// The declaration is the machine's own baked copy rather than whatever
/// kuma.toml happens to be in the reporter's working directory, because
/// the question is what built this machine.
fn report_json(findings: &[Finding], status: Option<&serde_json::Value>) -> serde_json::Value {
    let os = os_release_fields();
    let slots = status.map(crate::deployment::Deployments::from_status_json);
    // Built on top of the `--json` object rather than beside it, so
    // `checks` and `summary` sit at the top level in both and anything
    // that already reads `--json` reads a report unchanged.
    let mut out = doctor_json(findings);
    let extra = serde_json::json!({
        "kuma": {
            "version": crate::VERSION,
            // Which verb produced this, so a pasted report is not mistaken
            // for `--json` output with fields mysteriously added.
            "report": "doctor",
        },
        "machine": {
            "pretty_name": os.get("PRETTY_NAME"),
            "id": os.get("ID"),
            "version_id": os.get("VERSION_ID"),
            "version_codename": os.get("VERSION_CODENAME"),
            "booted_image": slots.as_ref().and_then(|d| d.booted.image.clone()),
            "booted_digest": slots.as_ref().and_then(|d| d.booted.digest.clone()),
            "staged": slots.as_ref().is_some_and(|d| d.staged.is_some()),
            "rollback": slots.as_ref().is_some_and(|d| d.rollback.is_some()),
            "live_media": live_media(),
            "booted_kuma_machine": booted_kuma_machine(),
        },
        "declaration": declaration_for_report(),
    });
    if let (Some(out), Some(extra)) = (out.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// The baked declaration with `user.password_hash` removed.
///
/// Fails closed in both directions. A file that will not parse is
/// omitted entirely rather than pasted raw, because "cannot parse" is not
/// a reason to publish a hash; and the redaction is asserted afterwards,
/// so a future key that also holds a secret cannot slip out by being
/// added somewhere this function does not look.
fn declaration_for_report() -> serde_json::Value {
    let path = Path::new(BAKED_CONFIG);
    let Ok(text) = std::fs::read_to_string(path) else {
        return serde_json::json!({ "present": false });
    };
    match redact_declaration(&text) {
        Some(redacted) => serde_json::json!({
            "present": true,
            "path": BAKED_CONFIG,
            "toml": redacted,
        }),
        None => serde_json::json!({
            "present": true,
            "path": BAKED_CONFIG,
            "omitted": "kuma could not parse this declaration, and will not paste one it cannot redact",
        }),
    }
}

const REDACTED: &str = "<redacted by kuma doctor --report>";

/// Keys whose value is never safe to paste into a bug report.
///
/// `password_hash` is the only one the schema has today. The rest are
/// named ahead of themselves: a report is pasted by somebody who will not
/// read it first, so the list is the cheap half of the bargain and being
/// wrong about it is the expensive half. `nsec` stays on the list even
/// though the work that would have declared one was dropped, for the same
/// reason the others are on it: no declaration has to contain a key for
/// redacting it to be free.
const SECRET_KEYS: &[&str] = &["password_hash", "password", "nsec", "private_key", "secret"];

/// The redaction itself. `None` means "could not be made safe", which the
/// caller must treat as "do not include".
///
/// `toml_edit` rather than a line rewrite: the value can be single- or
/// double-quoted, literal or basic, and a regex over lines gets one of
/// those wrong eventually. Parsing cannot.
///
/// Two layers, and the second is what makes the first safe to be wrong.
/// The walk redacts every key in `SECRET_KEYS` wherever it sits, nested
/// tables included; the guard afterwards re-reads the *output* and omits
/// the declaration entirely if any of those keys survived unredacted. So
/// a secret in a shape the walk does not reach — an array of tables, say,
/// which this schema does not have and a later one might — costs a
/// missing field in a report rather than a published secret.
fn redact_declaration(text: &str) -> Option<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().ok()?;
    redact_table(doc.as_table_mut());
    let out = doc.to_string();

    // The belt to the parser's braces, and the reason the doc comment
    // above can promise anything at all. A key is matched exactly rather
    // than by prefix: `token_path` is not a secret and omitting a whole
    // declaration over it would be its own kind of wrong.
    let survived = out.lines().any(|line| {
        let Some((key, _)) = line.split_once('=') else { return false };
        let key = key.trim().trim_matches('"');
        SECRET_KEYS.contains(&key) && !line.contains(REDACTED)
    });
    if survived {
        return None;
    }
    Some(out)
}

/// Replace every `SECRET_KEYS` value in this table and its children.
fn redact_table(table: &mut dyn toml_edit::TableLike) {
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for key in keys {
        if SECRET_KEYS.contains(&key.as_str()) {
            table.insert(&key, toml_edit::value(REDACTED));
            continue;
        }
        if let Some(child) = table.get_mut(&key).and_then(|item| item.as_table_like_mut()) {
            redact_table(child);
        }
    }
}

/// os-release as a map. `/etc` first because a machine may have been
/// given a local one, `/usr/lib` second because that is where kuma's
/// branding actually writes.
fn os_release_fields() -> BTreeMap<String, String> {
    ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|text| {
            text.lines()
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn doctor_json(findings: &[Finding]) -> serde_json::Value {
    let grade = |g: &Grade| match g {
        Grade::Ok => "ok",
        Grade::Warn => "warn",
        Grade::Fail => "fail",
    };
    let fails = findings.iter().filter(|f| matches!(f.grade, Grade::Fail)).count();
    let warns = findings.iter().filter(|f| matches!(f.grade, Grade::Warn)).count();
    let mut document = serde_json::json!({
        // The verdict, first, the same key every other --json document
        // carries: false when any check failed, and `error` below names
        // the count.
        "ok": fails == 0,
        "checks": findings.iter().map(|f| serde_json::json!({
            "grade": grade(&f.grade),
            "name": f.name,
            "detail": f.detail,
            "fix": f.fix.as_ref().map(action_json),
        })).collect::<Vec<_>>(),
        "summary": {
            "fails": fails,
            "warns": warns,
        },
    });
    if fails > 0 {
        document["error"] = format!("{fails} check(s) failed, {warns} warning(s)").into();
    }
    document
}

/// When a booted image stops being merely old and starts being worth
/// saying out loud. Fedora moves the kernel every couple of weeks, so a
/// month means at least one missed and usually two. Nothing here applies
/// anything: an image update replaces the whole OS and needs a reboot,
/// which is a human's call. Knowing is what a machine can automate.
const STALE_IMAGE_DAYS: u64 = 30;

/// Epoch seconds from an RFC 3339 timestamp. Everything after the
/// minutes is ignored — seconds, fractional seconds, and the offset —
/// because bootc writes UTC and none of it can move an answer measured
/// in days. The inverse of lock.rs's formatter, which hand-rolls the
/// same civil-date arithmetic in the other direction, and hand-rolled
/// here for the same reason: one timestamp is not worth a date crate.
fn epoch_from_rfc3339(stamp: &str) -> Option<i64> {
    let (date, time) = stamp.split_once('T')?;
    let mut fields = date.split('-');
    let year = leading_number(fields.next()?)?;
    let month = leading_number(fields.next()?)?;
    let day = leading_number(fields.next()?)?;
    let mut clock = time.split(':');
    let hour = leading_number(clock.next()?)?;
    let minute = leading_number(clock.next()?)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    // Howard Hinnant's days_from_civil, the inverse of the era shift in
    // lock.rs: March-based years, so the leap day lands last.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let yoe = shifted - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60)
}

/// The digits a field starts with: `"56Z"` is a seconds field, `"34"` a
/// minutes one, and both have to parse the same way.
fn leading_number(field: &str) -> Option<i64> {
    let digits: String = field.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whole days between an RFC 3339 timestamp and now. A timestamp in the
/// future (clock skew, a machine that booted before its RTC was set)
/// reads as zero rather than as an enormous unsigned number.
fn days_since(stamp: &str) -> Option<u64> {
    let then = epoch_from_rfc3339(stamp)?;
    days_since_epoch(then)
}

fn days_since_epoch(then: i64) -> Option<u64> {
    Some(whole_days_between(then, now_epoch()?))
}

fn now_epoch() -> Option<i64> {
    Some(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// Clock skew is real on a machine whose RTC has not been set: a
/// timestamp in the future reads as zero rather than as an enormous
/// unsigned number.
fn whole_days_between(then: i64, now: i64) -> u64 {
    now.saturating_sub(then).max(0) as u64 / 86_400
}

/// How long a machine may go without converging before that is a
/// finding.
///
/// The convergers run at boot and on a daily timer with
/// `Persistent=true`, so a machine that has been asleep catches up when
/// it wakes and a machine that is running converges every day. Seven
/// days is therefore seven missed firings plus every boot in between: it
/// cannot be reached by a laptop that was shut for a week, only by a
/// loop that has stopped turning.
const STALE_CONVERGENCE_DAYS: u64 = 7;

/// Days since the last run, when that is long enough to report.
///
/// This is the quiet half of the failure that cost a day on 2026-08-18.
/// The loud half is a converger whose last run failed, which `Result=`
/// already answers. The quiet half is a converger whose last run
/// succeeded three weeks ago, because "last run succeeded" and "timer
/// active" are both true of a machine that has silently stopped
/// converging, and nothing else here can tell them apart.
///
/// Only asked of units a timer runs again. A boot-only unit's last run
/// is the last boot, so its age measures uptime and answers nothing
/// about the machine's health.
///
/// `None` for a machine that is fine, and also for a missing timestamp:
/// grading a machine unhealthy because systemd stopped emitting a field
/// would be reporting on systemd, not on the machine.
fn convergence_staleness(
    on_a_timer: bool,
    last_exit: Option<i64>,
    now: Option<i64>,
) -> Option<u64> {
    if !on_a_timer {
        return None;
    }
    let days = whole_days_between(last_exit?, now?);
    (days >= STALE_CONVERGENCE_DAYS).then_some(days)
}

/// bootc status needs root; a sudo prompt out of `kuma doctor` is the
/// price of seeing the deployment at all, same as `kuma switch` pays.
///
/// Fetched once per run and shared, because `--report` wants the same
/// answer and two sudo prompts for one question is a worse tool.
fn bootc_status() -> Option<serde_json::Value> {
    host_output(&["sudo", "bootc", "status", "--format", "json"])
        .ok()
        .and_then(|out| serde_json::from_str(&out).ok())
}

fn check_deployment(
    status: Option<&serde_json::Value>,
    report: &mut impl FnMut(Grade, &str, String, Option<Action>),
) {
    let json = match status {
        Some(json) => json.clone(),
        None => {
            report(
                Grade::Warn,
                "deployment",
                "bootc status unavailable (not a bootc system, or sudo declined)".into(),
                None,
            );
            return;
        }
    };
    let deps = crate::deployment::Deployments::from_status_json(&json);
    let mut detail = match deps.booted.image.clone() {
        Some(image) => format!("booted on {image}"),
        None => {
            report(Grade::Warn, "deployment", "no booted bootc deployment".into(), None);
            return;
        }
    };
    let mut fix = None;
    if deps.staged.is_some() {
        detail.push_str("; a new deployment is staged");
        fix = Some(Action::new("reboot", "sudo systemctl reboot", "boot the staged deployment"));
    }
    if deps.rollback.is_some() {
        detail.push_str("; rollback available (kuma rollback)");
    }
    report(Grade::Ok, "deployment", detail, fix);

    // How old the running bytes are. On a machine that only updates when
    // asked, nothing else on it will ever mention this: the flatpak and
    // brew timers converge their own layers daily and say so, while the
    // kernel underneath them waits for a human. bootc carries the image's
    // creation time, and an image that never recorded one is no answer
    // rather than a wrong one.
    if let Some(days) = deps.booted.timestamp.as_deref().and_then(days_since) {
        // A staged deployment is a newer image already waiting and the
        // reboot that takes it is named above; a second alarm for a fire
        // already out is how a health check teaches people to skim it.
        let stale = days >= STALE_IMAGE_DAYS && deps.staged.is_none();
        let detail = match days {
            0 => "booted image was built today".to_string(),
            1 => "booted image is 1 day old".to_string(),
            days => format!("booted image is {days} days old"),
        };
        let fix = stale.then(|| {
            Action::new(
                "update",
                "kuma update",
                "recompose against the repos' current packages and rebuild",
            )
        });
        report(if stale { Grade::Warn } else { Grade::Ok }, "deployment", detail, fix);
    }

    // A build that was never switched to is the easy thing to forget.
    // Two storages are in play: the rootless build, and root's copy that
    // bootc actually deploys. Manifest digests don't survive the
    // save/load sync between them — only image IDs (config digests) do —
    // so compare IDs across storages and digests only within root's.
    //
    // Compare against the tag the machine actually deploys, not a
    // hardcoded one: a deployment that intentionally tracks another tag
    // (an overridden build, a test image) must never be told the
    // unrelated default build is "newer" — following that suggestion
    // would switch the machine off the tag its admin chose.
    let compared_tag = deps
        .booted
        .transport
        .as_deref()
        .filter(|transport| *transport == "containers-storage")
        .and_then(|_| deps.booted.image.clone())
        .unwrap_or_else(|| crate::DEFAULT_TAG.to_string());
    if let Ok(local_id) = crate::image_id(&compared_tag) {
        let root = host_output(&[
            "sudo",
            "podman",
            "image",
            "inspect",
            "--format",
            "{{.Id}} {{.Digest}}",
            &compared_tag,
        ])
        .unwrap_or_default();
        let (root_id, root_digest) = root.split_once(' ').unwrap_or(("", ""));
        let deployed: Vec<&str> = [Some(&deps.booted), deps.staged.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|slot| slot.digest.as_deref())
            .collect();
        let deployment_current = !root_digest.is_empty() && deployed.contains(&root_digest);
        if local_id != root_id || (!deployed.is_empty() && !deployment_current) {
            let switch_cmd = if compared_tag == crate::DEFAULT_TAG {
                "kuma switch".to_string()
            } else {
                format!("kuma switch --tag {compared_tag}")
            };
            report(
                Grade::Warn,
                "deployment",
                format!("{compared_tag} is newer than the deployment"),
                Some(Action::new("switch", switch_cmd, "stage the newer build")),
            );
        }
        // Doctor holds root and the truth, so refresh the stamp the
        // passwordless bare-`kuma` probe reads — this heals it after
        // out-of-band changes (`bootc rollback`, raw `bootc switch`).
        // Kuma's own metadata, not machine state, so doctor stays
        // read-only about the machine itself. When the deployment no
        // longer matches root storage's tag, the deployed ID is unknowable
        // here — drop the stamp so the probe skips the check.
        //
        // Only when it is actually wrong. This ran on every doctor,
        // spawning sudo and sh to write back a value that was already
        // there, which is two processes and a write of pure waste on the
        // common path.
        let stamp = std::fs::read_to_string(crate::state::DEPLOYED_ID_FILE).ok();
        let heal = if deployment_current {
            let current = stamp.as_deref().map(str::trim);
            (current != Some(root_id)).then(|| {
                format!(
                    "mkdir -p /var/lib/kuma && printf '%s\\n' {} > {}",
                    crate::state::shell_quote(root_id),
                    crate::state::DEPLOYED_ID_FILE
                )
            })
        } else {
            stamp.is_some().then(|| format!("rm -f {}", crate::state::DEPLOYED_ID_FILE))
        };
        if let Some(heal) = heal {
            let _ = host_output(&["sudo", "sh", "-c", &heal]);
        }
    }
}

/// Where flatpak keeps per-app permission overrides: one store for the
/// system installation, one per person.
const SYSTEM_OVERRIDES: &str = "/var/lib/flatpak/overrides";
const USER_OVERRIDES: &str = ".local/share/flatpak/overrides";

/// Both stores on this machine: the system one, and the home of whoever
/// is asking. Doctor runs as a person, so their overrides are the ones
/// they can act on; reaching into every home would be root rummaging
/// through accounts to report on files it has no business converging.
fn override_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(SYSTEM_OVERRIDES)];
    if let Ok(home) = std::env::var("HOME") {
        roots.push(PathBuf::from(home).join(USER_OVERRIDES));
    }
    roots
}

/// Override files that are symlinks to somewhere that does not exist,
/// with what each one points at.
///
/// Takes its roots so the finding path is testable. A healthy machine
/// has nothing here, which is exactly the branch that never runs and so
/// never gets checked; `scan_etc` and `kuma-fstab-sync` are parameterised
/// for the same reason.
///
/// Only symlinks are examined. A regular override file is somebody's
/// settings, whatever wrote it, and none of kuma's business until the
/// declaration learns to own these.
/// Symlinks in one directory whose target is not there.
///
/// exists() follows the link, so on its own it already skips every
/// regular file and every symlink that resolves. The is_symlink() guard
/// earns its line in the race the pair leaves open: a file deleted
/// between the read_dir and the exists() would otherwise be reported as
/// pointing at nothing, with nothing to name as its target.
fn broken_links(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() && !path.exists() {
            found.push(path);
        }
    }
    found
}

fn dangling_overrides(roots: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut found: Vec<(PathBuf, PathBuf)> = roots
        .iter()
        .flat_map(|root| broken_links(root))
        .map(|path| {
            let target = std::fs::read_link(&path).unwrap_or_default();
            (path, target)
        })
        .collect();
    found.sort();
    found
}

/// An override pointing at nothing is inherited wreckage: a distribution
/// that shipped its own overrides directory, a machine that was
/// something else before it was this, and a symlink that survived in
/// /var across every image switch because nothing has ever looked at it.
///
/// Graded warn rather than fail. flatpak tolerates it, the machine is
/// not broken, and nothing kuma converges depends on it. `[overrides]`
/// cannot take it back either: a link pointing at nothing has no keys to
/// read, so convergence has nothing to own and capture has nothing to
/// propose. Saying so remains the whole of what kuma can honestly do.
fn check_overrides(
    roots: &[PathBuf],
    report: &mut impl FnMut(Grade, &str, String, Option<Action>),
) {
    for (link, target) in dangling_overrides(roots) {
        let name = link.file_name().unwrap_or_default().to_string_lossy().to_string();
        let sudo = if link.starts_with(SYSTEM_OVERRIDES) { "sudo " } else { "" };
        let fix = Action::new(
            "remove-override",
            format!("{sudo}rm {}", link.display()),
            "drop an override pointing at nothing",
        );
        report(
            Grade::Warn,
            "flatpak overrides",
            format!("{name} points at {}, which does not exist", target.display()),
            Some(fix),
        );
    }
}

/// Where systemd records that a unit was enabled: a symlink under
/// `/etc/systemd/system` in the target's `.wants` or `.requires`
/// directory, pointing at the unit file it should start.
const ETC_UNITS: &str = "/etc/systemd/system";

/// Enablement symlinks whose unit file is gone.
///
/// Deliberately narrow, for the reason `check_etc_drift` explains at
/// length: `/etc` on a real machine carries dozens of legitimately local
/// files, and a check that lists them all is a check people learn to
/// scroll past. A link into a `.wants` directory is different in kind.
/// It is not somebody's configuration, it is a machine saying it will
/// start something at every boot, and when the target is missing that
/// sentence is simply false.
fn dangling_enablements(root: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let Ok(targets) = std::fs::read_dir(root) else { return found };
    for target in targets.flatten() {
        let dir = target.path();
        let name = target.file_name().to_string_lossy().to_string();
        if !name.ends_with(".wants") && !name.ends_with(".requires") {
            continue;
        }
        for path in broken_links(&dir) {
            let unit = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            found.push((path, unit));
        }
    }
    found.sort();
    found
}

/// A unit enabled into a target that has no unit file behind it.
///
/// Found by reading a real machine: `default.target.wants/nostrd.service`
/// pointed at a unit file that had been deleted, `systemctl is-enabled`
/// answered `not-found`, and every other surface kuma has said the
/// machine was in sync. Nothing fails, because a unit that was never
/// found can never fail, which is exactly why nothing reported it.
///
/// Warn rather than fail, for the same reason as the override sibling:
/// the machine boots and works. What it is not doing is the thing it
/// says it does.
fn check_enablements(root: &Path, report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    for (link, unit) in dangling_enablements(root) {
        let fix = Action::new(
            "remove-enablement",
            format!("sudo rm {}", link.display()),
            "drop an enablement for a unit that does not exist",
        );
        // "enablement" rather than "units", which the failed-unit check
        // already answers to. Two different questions under one name
        // collide for anything keying on `doctor --json`, and every
        // other check name in the report is unique.
        report(
            Grade::Warn,
            "enablement",
            format!("{unit} is enabled but has no unit file; it has never started"),
            Some(fix),
        );
    }
}

/// Whether there is an account for `kuma-user-sync` to converge, asked
/// of both files the converger itself reads.
///
/// Baked, or installed, and on the whole install path only the second
/// one exists: a published image declares no account, `kuma install`
/// writes the answer onto the target, and the unit creates it at first
/// boot. Asking only about the baked file left that machine's one
/// account-creating unit ungraded, which is where a failure costs the
/// most, because a machine whose account was never created is a machine
/// nobody can log in to and doctor had nothing to say about it.
///
/// Takes both paths so the branch is testable: on a real machine only
/// one of the four combinations is ever live.
fn user_sync_has_an_account(baked: &Path, installed: &Path) -> bool {
    baked.exists() || installed.exists()
}

/// The oneshots record their last run in Result=; the timers are what
/// keeps long-uptime machines converged.
///
/// The third column is whether a timer runs this unit again, and it is
/// what makes "how long since it last ran" a question worth asking.
/// `kuma-user-sync.service` is `WantedBy=multi-user.target` and nothing
/// else, so its last run is always the last boot: asking a machine with
/// eight days of uptime why its account has not been converged in eight
/// days is asking about the uptime, not about the account.
fn convergence_targets(
    has_account: bool,
    has_flatpaks: bool,
    has_brews: bool,
) -> Vec<(&'static str, &'static str, bool)> {
    let mut targets: Vec<(&str, &str, bool)> = Vec::new();
    if has_account {
        targets.push(("kuma-user-sync.service", "user", false));
    }
    if has_flatpaks {
        targets.push(("kuma-flatpak-sync.service", "flatpak sync", true));
        targets.push(("kuma-flatpak-sync.timer", "flatpak sync", false));
    }
    if has_brews {
        targets.push(("kuma-brew-sync.service", "brew sync", true));
        targets.push(("kuma-brew-sync.timer", "brew sync", false));
    }
    targets
}

fn check_convergence(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let targets = convergence_targets(
        user_sync_has_an_account(Path::new(BAKED_USER), Path::new(INSTALLED_USER)),
        Path::new(BAKED_FLATPAKS).exists(),
        Path::new(BAKED_BREWS).exists(),
    );
    let names: Vec<&str> = targets.iter().map(|(unit, _, _)| *unit).collect();
    let facts = unit_facts(&names);
    for (unit, name, on_a_timer) in &targets {
        let Some(fact) = facts.get(*unit) else {
            report(Grade::Warn, name, format!("{unit} state unavailable"), None);
            continue;
        };
        if unit.ends_with(".timer") {
            if fact.active == "active" {
                report(Grade::Ok, name, format!("{unit} active"), None);
            } else {
                let fix = Action::new(
                    "start",
                    format!("sudo systemctl start {unit}"),
                    "restart the convergence timer",
                );
                report(Grade::Fail, name, format!("{unit} is not active"), Some(fix));
            }
        } else if fact.active == "active" || fact.active == "activating" {
            // Asked before the result, because systemd reports
            // Result=success for a unit that has not finished: a first
            // boot downloading a gigabyte of flatpaks was graded "last
            // run succeeded" while the run was still going, which is a
            // true field and a false sentence.
            report(Grade::Ok, name, format!("{unit} is running now"), None);
        } else if fact.result == "success" {
            match convergence_staleness(*on_a_timer, fact.last_exit, now_epoch()) {
                Some(days) => {
                    let fix = Action::new("sync", "kuma sync", "converge this machine now");
                    let detail =
                        format!("{unit} last converged {days} days ago; it runs at boot and daily");
                    report(Grade::Fail, name, detail, Some(fix));
                }
                None => report(Grade::Ok, name, format!("{unit} last run succeeded"), None),
            }
        } else {
            let fix = Action::new("sync", "kuma sync", "re-run convergence now");
            let detail = match last_run_reason(&fact.invocation) {
                Some(reason) => format!("{unit} last run: {} ({reason})", fact.result),
                None => format!("{unit} last run: {}", fact.result),
            };
            report(Grade::Fail, name, detail, Some(fix));
        }
    }
    if Path::new(BAKED_FLATPAKS).exists() {
        match host_output(&["flatpak", "remotes", "--system", "--columns=name"]) {
            Ok(out) if out.lines().any(|l| l.trim() == "flathub") => {
                report(Grade::Ok, "flathub", "remote configured".into(), None)
            }
            _ => {
                let fix = Action::new(
                    "add-remote",
                    format!(
                        "sudo flatpak remote-add --if-not-exists flathub {}",
                        crate::containerfile::FLATHUB_URL
                    ),
                    "restore the remote",
                );
                report(
                    Grade::Fail,
                    "flathub",
                    "remote missing; flatpak convergence cannot install".into(),
                    Some(fix),
                );
            }
        }
    }

    // Convergence the daily timer deferred is a decision the machine
    // made, and the gate's stamp is where the machine said so. Doctor is
    // where that answer is surfaced, because the alternative is a machine
    // whose browser silently stops updating while the timer stays green.
    // Informational only — skipping a run on a dying battery is the gate
    // working, never a fault — and silence is the common case: a machine
    // that never skips is the default, not an achievement to grade.
    if let Ok(text) = std::fs::read_to_string("/var/lib/kuma/convergence-skipped") {
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        if !lines.is_empty() {
            // Each stamp line is "<UTC timestamp> <reason>"; the reasons
            // are what a reader acts on, the count is how often.
            let mut reasons: Vec<&str> =
                lines.iter().filter_map(|l| l.split_once(' ')).map(|(_, r)| r).collect();
            reasons.sort_unstable();
            reasons.dedup();
            report(
                Grade::Ok,
                "convergence",
                format!(
                    "the daily run was skipped {} time(s) since it last ran: {}",
                    lines.len(),
                    reasons.join("; ")
                ),
                None,
            );
        }
    }
}

/// The install record, when this machine has one. bootc writes its own
/// facts to the same root; this is the kuma half — who ran the install,
/// when, from what, and what the lock had resolved. Informational by
/// design and silent when absent: every machine that updated into the
/// feature has no file, absence is ambiguous (pre-feature install vs
/// never-kuma), and a check that nagged its most loyal machines about a
/// fact nothing can act on would be a broken check. Corrupt counts as
/// absent for the same reason: an unparseable self-description answers
/// nothing, and saying so on every run is noise.
fn check_install_provenance(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let Ok(text) = std::fs::read_to_string("/var/lib/kuma/install.json") else { return };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let mut said = String::new();
    for (key, label) in [("kuma", "by kuma"), ("installed_at", "on"), ("media", "from")] {
        if let Some(value) = parsed[key].as_str() {
            if !said.is_empty() {
                said.push_str(", ");
            }
            said.push_str(&format!("{label} {value}"));
        }
    }
    if let Some(base) = parsed["base"].as_str() {
        said.push_str(&format!(", base {}", crate::short(base)));
    }
    if said.is_empty() {
        return;
    }
    report(Grade::Ok, "install", said, None);
}

/// Whether a `stat -c %i` answer names the root of a btrfs subvolume.
///
/// Inode 256 is what every subvolume root has, and it is the only form
/// of the question that can be asked without root: `btrfs subvolume
/// show` searches the B-tree, which needs privilege, and it fails with
/// an error for "not a subvolume" and for "not allowed to look" alike.
/// A check built on that would grade a working machine broken the moment
/// it ran unprivileged.
///
/// `None` for an answer that isn't a number, which means the question
/// could not be asked and nothing should be graded on it.
fn subvolume_root(stat_output: &str) -> Option<bool> {
    stat_output.trim().parse::<u64>().ok().map(|inode| inode == 256)
}

/// How long a machine may go without a completed backup before that is
/// worth saying out loud.
///
/// Two missed firings plus slack, never less than a week. A week cannot
/// be reached by a laptop that was shut: the timer is `Persistent=true`,
/// so a machine that was asleep at the appointed hour runs on the next
/// wake. Reaching it means the loop has stopped turning, or the far end
/// has been unreachable the whole time, or nobody ever provisioned the
/// credential.
///
/// Derived from the declared interval rather than fixed, because a
/// declaration that asks for monthly copies is not unhealthy on day
/// eight. Only the words systemd's calendar spells as plain periods are
/// recognised; anything more elaborate falls back to the floor, which
/// errs toward asking rather than toward silence.
fn backup_stale_after_days(interval: &str) -> u64 {
    let period = match interval.trim() {
        "weekly" => 7,
        "monthly" => 31,
        "quarterly" => 92,
        "semiannually" => 183,
        "yearly" | "annually" => 365,
        // hourly, daily, and every explicit OnCalendar expression
        // somebody writes by hand, which is almost always sub-daily.
        _ => 1,
    };
    (period * 2).max(7)
}

/// Epoch seconds from the stamp the backup writes, which is the first
/// field of a line whose second field is the same moment for people.
///
/// `None` for a file that is absent or unparseable, and the caller must
/// treat those as "no backup has completed" rather than as an error:
/// they are the same thing to whoever needs the data back.
fn backup_stamp(text: &str) -> Option<i64> {
    text.split_whitespace().next()?.parse().ok()
}

/// Absolute-path programs a niri config's binds and startup list spawn,
/// in order of appearance, deduplicated.
///
/// Deliberately narrower than everything niri can run, because doctor's
/// PATH is not the session's: a bare `spawn "wpctl"` resolves through the
/// session's PATH, which carries brew and the user's own bins, and a
/// `spawn-sh` line is a whole shell line rather than a program, so
/// neither can be checked from here without risking a false alarm about
/// a key that works. Absolute paths carry their own answer, and every
/// program the image's binds spawn is one — so the class that goes
/// stale, a config copied out of an older image, is exactly the class
/// this sees.
///
/// Line comments are skipped; a `/-` node comment that folds a spawn
/// into its block would still be seen, which errs toward reporting a key
/// as dead that is commented out — the survivable direction, since the
/// opposite is how a dead key hides from the one check that reads every
/// bind.
fn niri_spawn_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let rest = match trimmed.find("spawn-at-startup \"") {
            Some(i) => &trimmed[i + "spawn-at-startup \"".len()..],
            None => match trimmed.find("spawn \"") {
                Some(i) => &trimmed[i + "spawn \"".len()..],
                None => continue,
            },
        };
        let Some(arg) = rest.split('"').next() else { continue };
        if arg.starts_with('/') && !out.iter().any(|seen| seen == arg) {
            out.push(arg.to_string());
        }
    }
    out
}

/// Whether a machine's own niri config shadows the image's, and what
/// that costs.
///
/// niri takes `~/.config/niri/config.kdl` INSTEAD of /etc/niri/config.kdl
/// rather than merging the two, so one copied file silently unpins every
/// bind, startup service and window rule the image ships — and the copy
/// goes stale the moment an image update rewrites the config it was
/// copied from, which is the machine this check was written on: its
/// media keys spawned a binary from the image before the last rename.
/// The image's config ends with `include optional=true` on
/// `.config/niri/local.kdl` precisely so a full copy is never needed;
/// that file's deltas survive either way.
///
/// A shadow with every absolute spawn resolvable is a Warn: nothing it
/// binds is dead today, but the same staleness applies and nothing here
/// can see bare-name spawns to say otherwise. Dead absolute paths are a
/// Fail naming them, because a bind that spawns a missing program is a
/// key that does nothing.
///
/// Takes the sysroot so the branches are testable; on a machine the
/// argument is `/`.
fn check_niri_shadow_at(
    sysroot: &Path,
    report: &mut impl FnMut(Grade, &str, String, Option<Action>),
) {
    // Only where the image ships a niri config. A headless or COSMIC
    // machine has no session config to shadow.
    if !sysroot.join("etc/niri/config.kdl").exists() {
        return;
    }
    let homes = match std::fs::read_dir(sysroot.join("var/home")) {
        Ok(entries) => entries,
        // No home root to shadow from (a container of the image): nothing
        // to report and nothing to be right about.
        Err(_) => return,
    };
    let mut homes: Vec<PathBuf> =
        homes.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    homes.sort();
    let mut shadowed = false;
    for home in homes {
        let shadow = home.join(".config/niri/config.kdl");
        if !shadow.exists() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&shadow) else { continue };
        let user = home.file_name().and_then(|n| n.to_str()).unwrap_or("an account").to_string();
        shadowed = true;
        let dead: Vec<String> = niri_spawn_targets(&text)
            .into_iter()
            .filter(|t| !sysroot.join(t.trim_start_matches('/')).exists())
            .collect();
        let aside = format!("{}.bak", shadow.display());
        if dead.is_empty() {
            report(
                Grade::Warn,
                "niri config",
                format!(
                    "{user}'s niri config shadows the image's, so image updates to the \
                     session — new binds, startup services, window rules — do not reach \
                     this desktop"
                ),
                Some(Action::new(
                    "move",
                    format!("mv {} {aside}", shadow.display()),
                    "the image's config includes local.kdl, so the machine's own deltas survive",
                )),
            );
        } else {
            report(
                Grade::Fail,
                "niri config",
                format!(
                    "{user}'s niri config shadows the image's, and its binds spawn \
                     programs this image does not have ({}): those keys do nothing",
                    dead.join(", ")
                ),
                Some(Action::new(
                    "move",
                    format!("mv {} {aside}", shadow.display()),
                    "let the image's config run again, then log out and back in; \
                     local.kdl deltas still apply",
                )),
            );
        }
    }
    if !shadowed {
        report(Grade::Ok, "niri config", "the session runs the image's niri config".into(), None);
    }
}

fn check_niri_shadow(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    check_niri_shadow_at(Path::new("/"), report);
}

/// The one process every lock on this desktop goes through.
///
/// Idle lock, the keybind and lock-before-suspend all run through the
/// shell. Until 0.17 it was started by `spawn-at-startup`, which lands in
/// a transient scope that cannot be restarted, so a crash took the lock
/// with it and nothing said so. It is a supervised unit now, and this is
/// the readback: a fact a command can check, which is the whole reason
/// the unit exists rather than the spawn.
///
/// Asked only where kuma put a shell and only inside a session. A server,
/// a COSMIC image, or an ssh login to a machine sitting at its greeter
/// has no shell to miss, and grading one there would be this check
/// reporting the wrong desktop rather than a broken one.
/// The variables the shell has to have been handed, read off the running
/// process rather than off any file. Returns the first one missing.
///
/// `/proc/<pid>/environ` because that is the only place the answer is
/// not a claim: the unit can say it, the niri config can say it, and
/// neither settles what the process that is drawing the screen was
/// given. Same user, so it reads without sudo.
/// The parse, kept apart from the journal so it can be tested against a
/// log rather than against a machine.
///
/// The LAST failure line is the finding, because a later failure
/// replaces an earlier one as this moment's answer. Success is silent —
/// the watcher logs nothing when it is running — so a failure followed
/// by silence still reads as a failure: conservative on purpose, and
/// the message quotes the journal so a stale report can be checked
/// against the machine it names.
fn idle_watcher_failure(log: &str) -> Option<String> {
    log.lines()
        .rev()
        .find(|line| line.contains("idle watcher stopped") || line.contains("idle locking is off"))
        .map(str::to_string)
}

/// The idle watcher's failure line, if the running shell's journal
/// carries one.
///
/// Outer `None` — the journal could not be read, which is not a
/// finding. Inner `Some(line)` — the watcher logged a failure. Inner
/// `None` — the journal is readable and says nothing is wrong.
///
/// The read is scoped to the unit's main PID, not to the boot: a
/// shell that exited mid-boot — a logout teardown, a crash that
/// `Restart=always` recovered — logged its watcher's death, and the
/// boot's journal keeps that line forever after. The "shell" check
/// just graded *this* process active, so *this* process is the
/// witness; a dead instance cannot testify about the live one. A unit
/// that names no PID falls back to the whole boot, where the older,
/// conservative reading applies.
fn shell_idle_broken() -> Option<Option<String>> {
    let pid = host_output_any(&[
        "systemctl",
        "--user",
        "show",
        "kuma-shell.service",
        "-p",
        "MainPID",
        "--value",
    ])
    .ok()?;
    let pid = pid.trim();
    // A running unit's MainPID is its process; "0" (or empty) is
    // systemd's answer for none — leave the read unscoped rather than
    // asking the journal for a process that does not exist.
    let mut args: Vec<String> =
        ["journalctl", "--user", "-b", "-u", "kuma-shell.service", "--no-pager", "-o", "cat"]
            .into_iter()
            .map(String::from)
            .collect();
    if !pid.is_empty() && pid != "0" {
        args.push(format!("_PID={pid}"));
    }
    let log = host_output_any(&args).ok()?;
    Some(idle_watcher_failure(&log))
}

/// The shell's configured idle lock, in seconds, read off the same file
/// the shell reads: `XDG_CONFIG_HOME` (or `~/.config`)
/// `kuma-shell/config.toml`'s `[idle] lock_timeout`.
///
/// `None` — nothing configured (no file, no `[idle]` section, a file
/// that does not parse), which means the defaults run; the shell treats
/// an unparsable config the same way, so doctor agrees with the shell
/// on every input. `Some(0)` — the lock is disabled, and deliberately:
/// a 0 is a choice made in the settings panel or by hand, not a
/// broken machine.
fn shell_idle_lock() -> Option<u64> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    let text = std::fs::read_to_string(base.join("kuma-shell/config.toml")).ok()?;
    configured_idle_lock(&text)
}

/// The `[idle] lock_timeout` of a shell config's text. A missing
/// section, a missing key, or a file that does not parse all read as
/// "nothing configured": the defaults run, which is how the shell
/// itself treats every one of those inputs.
fn configured_idle_lock(text: &str) -> Option<u64> {
    #[derive(serde::Deserialize, Default)]
    struct Idle {
        lock_timeout: Option<u64>,
    }
    #[derive(serde::Deserialize, Default)]
    struct ShellConfig {
        #[serde(default)]
        idle: Idle,
    }
    toml::from_str::<ShellConfig>(text).ok()?.idle.lock_timeout
}

fn shell_env_missing() -> Option<String> {
    let pid = host_output_any(&[
        "systemctl",
        "--user",
        "show",
        "kuma-shell.service",
        "-p",
        "MainPID",
        "--value",
    ])
    .ok()?;
    let pid = pid.trim();
    if pid.is_empty() || pid == "0" {
        return None;
    }
    // Unreadable environ is not a finding: a machine where this cannot
    // be asked is not a machine where it is known to be wrong.
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let set: Vec<&[u8]> = environ.split(|b| *b == 0).collect();
    // The variables the unit states, in the order the unit states them.
    // kuma-shell's own config is compiled in, so the cursor pair is all
    // the environ contract left.
    ["XCURSOR_THEME=Adwaita", "XCURSOR_SIZE=24"]
        .into_iter()
        .find(|want| !set.contains(&want.as_bytes()))
        .map(|want| want.split('=').next().unwrap_or(want).to_string())
}

fn check_shell(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    // Only where the IMAGE ships the unit. A machine built before 0.17
    // starts its shell from a niri spawn and is perfectly correct, and
    // grading it against a unit its image never had would be this check
    // reporting the wrong version rather than a broken machine. That is
    // the same mistake three smoke assertions made this cycle.
    if !Path::new("/usr/lib/systemd/user/kuma-shell.service").exists() {
        return;
    }
    if !Path::new("/etc/niri/config.kdl").exists() {
        return;
    }
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    match host_output_any(&["systemctl", "--user", "is-active", "kuma-shell.service"]) {
        Ok(state) if state.trim() == "active" => {
            // Running is not the same as running as kuma's desktop.
            //
            // 0.17.0 moved the shell out of a niri spawn and into this
            // unit, and left NOCTALIA_CONFIG_HOME behind in niri's
            // `environment` block, which a unit does not inherit. The
            // machine booted, the unit was active, the config was in
            // the image and the niri file still named the variable, so
            // every check passed while the desktop drew stock noctalia.
            // Nothing on that machine was readable as wrong except the
            // process itself, so the process is what this asks. The
            // kuma-shell needs less of an environ — its defaults are
            // compiled in — but what its unit states must reach it, or
            // the cursor over the bar and the lock screen is the
            // compositor's default.
            if let Some(missing) = shell_env_missing() {
                report(
                    Grade::Fail,
                    "shell",
                    format!(
                        "the desktop shell is running without {missing}, so the cursor over \
                         its surfaces is the compositor's default rather than the image's"
                    ),
                    Some(Action::new(
                        "read",
                        "systemctl --user cat kuma-shell.service",
                        "the unit has to set the variable itself; niri's \
                         environment block does not reach it",
                    )),
                );
                return;
            }
            report(
                Grade::Ok,
                "shell",
                "the desktop shell is running under supervision".into(),
                None,
            );
            // Supervised and configured is still not locking.
            //
            // Every niri image kuma ever built set an idle lock at 15
            // minutes and screen-off at 16, and no machine ever armed
            // either: noctalia wanted each behavior to name an `action`
            // and kuma gave it only a timeout, so both were dropped at
            // startup, invisibly. Those numbers are the shell's
            // defaults now, and they are configurable — 44.4.0 put the
            // [idle] keys in ~/.config/kuma-shell/config.toml, and the
            // settings panel edits them live. Doctor reads the same
            // file, so the text says what this machine does rather
            // than what shells do, and a deliberately disabled lock is
            // named as a choice instead of failed as a bug.
            let disabled = shell_idle_lock() == Some(0);
            match shell_idle_broken() {
                Some(Some(line)) if !disabled => report(
                    Grade::Fail,
                    "idle lock",
                    format!(
                        "the shell's idle watcher is not running — its journal says \
                         \"{line}\" — so this desktop does not lock itself on idle"
                    ),
                    Some(Action::new(
                        "read",
                        "journalctl --user -b -u kuma-shell.service | grep -i idle",
                        "the watcher's failures name their cause; the timeouts themselves \
                         live in the settings panel (Mod+S) or \
                         ~/.config/kuma-shell/config.toml's [idle] keys",
                    )),
                ),
                Some(Some(line)) => report(
                    Grade::Ok,
                    "idle lock",
                    format!(
                        "idle locking is disabled by config ([idle] lock_timeout = 0), so \
                         the watcher's failure — \"{line}\" — breaks nothing this machine \
                         asked for"
                    ),
                    None,
                ),
                Some(None) if disabled => report(
                    Grade::Ok,
                    "idle lock",
                    "idle locking is disabled by config ([idle] lock_timeout = 0): this \
                     desktop does not lock itself on idle by choice"
                        .into(),
                    None,
                ),
                Some(None) => {
                    let lock = match shell_idle_lock() {
                        Some(seconds) => format!("lock after {seconds} seconds"),
                        None => "lock after 15 minutes".to_string(),
                    };
                    report(
                        Grade::Ok,
                        "idle lock",
                        format!(
                            "the shell's idle watcher is running: {lock}, screens off a \
                             minute later, lock before sleep"
                        ),
                        None,
                    )
                }
                None => report(
                    Grade::Warn,
                    "idle lock",
                    "cannot read the shell's journal, so whether it locks on idle is \
                     unknown"
                        .into(),
                    None,
                ),
            }
        }
        Ok(state) => report(
            Grade::Fail,
            "shell",
            format!(
                "kuma-shell.service is {}, so nothing on this desktop locks: not on idle, \
                 not on the keybind, and not before suspend",
                state.trim()
            ),
            Some(Action::new(
                "start",
                "systemctl --user start kuma-shell.service",
                "bring the shell back, which brings the lock back with it",
            )),
        ),
        Err(_) => report(Grade::Warn, "shell", "cannot ask systemd about the shell".into(), None),
    }
}

/// A backup fails quietly in more ways than a snapshot does. The unit
/// exits 0 on a machine with no credential, no snapshot and no
/// repository, all three deliberately, so "last run succeeded" is true
/// of a machine that has never copied a byte. The timer being active is
/// true of a machine whose repository has been unreachable for a month.
/// Neither is answerable from `Result=`, which is why the converger
/// stamps only on a run that actually copied something, and why this
/// grades the stamp rather than the unit.
fn check_backup(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let Ok(config) = Config::load(Path::new(BAKED_CONFIG)) else {
        // check_snapshots already named an unreadable baked declaration;
        // saying it twice in one report is noise.
        return;
    };
    if !config.backup.enable {
        return;
    }

    // What it covers, on every run and before any grade. This is the
    // whole reason `network_connections` defaults off rather than being
    // absent: an omission you read on an ordinary day is a choice, and
    // one you discover during a restore is a trap.
    let carries = if config.backup.network_connections {
        "network connections included"
    } else {
        "network connections NOT included, so a restore needs those passwords retyped"
    };
    report(Grade::Ok, "backup", format!("covers {}; {carries}", config.snapshots.target), None);

    // The declaration names a credential; this machine either has it or
    // has not been finished being set up. Naming it is what makes that
    // answerable at all: an unnamed credential could only fail inside
    // the unit at whatever hour the timer fires.
    // The verb's own answer rather than a second copy of the same format
    // string: doctor naming a path `kuma backup` does not would send
    // somebody to provision a file nothing ever reads.
    let secret = crate::backup::secret_path(&config);
    // Existence is not the promise. SECURITY.md says "mode 0600, owned
    // by root", and kuma prints the command that makes it so, but the
    // operator creates this file by hand and a plain `>` redirect leaves
    // it 0644. That is a repository password readable by every local
    // account, permanently, on a machine reporting itself healthy. A
    // promise a stranger relies on is one doctor has to grade.
    if let Ok(meta) = std::fs::metadata(&secret) {
        use std::os::unix::fs::MetadataExt;
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 || meta.uid() != 0 {
            let fix = Action::new(
                "restrict",
                format!("sudo chown root:root {secret} && sudo chmod 600 {secret}"),
                "make the credential unreadable to everyone but root",
            );
            report(
                Grade::Fail,
                "backup",
                format!(
                    "{secret} is mode {mode:04o} owned by uid {}; the repository password \
                     is readable by other accounts on this machine",
                    meta.uid()
                ),
                Some(fix),
            );
        }
    }
    // A value the readers disagree about, named before the night the
    // timer needs it rather than after. Only when the file is readable:
    // it is 0600 root by design, so an ordinary `kuma doctor` cannot see
    // it and says nothing rather than guessing. A root-run doctor can,
    // and `kuma backup` refuses outright, which is the enforcing path.
    if let Ok(text) = std::fs::read_to_string(&secret) {
        let ambiguous = crate::backup::ambiguous_values(&text);
        if !ambiguous.is_empty() {
            report(
                Grade::Fail,
                "backup",
                format!(
                    "{secret} sets {} with a value this machine's three readers would read \
                     differently (the verb, the timer, and the first-boot restore), so the \
                     password one uses is not the password another uses",
                    ambiguous.join(", ")
                ),
                Some(Action::new(
                    "rewrite",
                    format!("sudoedit {secret}"),
                    "write the value as plain text; a repository made before 0.17 needs \
                     `restic passwd` first, because it was encrypted with the expanded value",
                )),
            );
        }
    }
    if !Path::new(&secret).exists() {
        let fix = Action::new(
            "provision",
            crate::backup::provision_command(&secret),
            "make the credential the declaration names, then put the repository's keys in it",
        );
        report(
            Grade::Warn,
            "backup",
            format!("no credential at {secret}; nothing has been copied and nothing will be"),
            Some(fix),
        );
        return;
    }

    let facts = unit_facts(&["kuma-backup.timer", "kuma-backup.service"]);
    match facts.get("kuma-backup.timer") {
        Some(fact) if fact.active == "active" => report(
            Grade::Ok,
            "backup",
            format!("kuma-backup.timer active ({})", config.backup.interval),
            None,
        ),
        Some(_) => {
            let fix = Action::new(
                "start",
                "sudo systemctl start kuma-backup.timer",
                "resume scheduled backups",
            );
            report(Grade::Fail, "backup", "kuma-backup.timer is not active".into(), Some(fix));
        }
        None => report(Grade::Warn, "backup", "kuma-backup.timer state unavailable".into(), None),
    }

    if let Some(fact) = facts.get("kuma-backup.service") {
        if !fact.result.is_empty() && fact.result != "success" {
            let detail = match last_run_reason(&fact.invocation) {
                Some(reason) => format!("kuma-backup.service last run failed: {reason}"),
                None => "kuma-backup.service last run failed".to_string(),
            };
            let fix = Action::new(
                "logs",
                "journalctl -u kuma-backup.service -n 50",
                "read what the last backup said",
            );
            report(Grade::Fail, "backup", detail, Some(fix));
        }
    }

    // The stamp, which is the only surface that can tell a machine
    // copying nightly from one that has quietly stopped.
    let stamp =
        std::fs::read_to_string(crate::backup::STAMP).ok().as_deref().and_then(backup_stamp);
    let stale_after = backup_stale_after_days(&config.backup.interval);
    match (stamp, now_epoch()) {
        (Some(then), Some(now)) => {
            let days = whole_days_between(then, now);
            if days >= stale_after {
                let fix = Action::new(
                    "logs",
                    "journalctl -u kuma-backup.service -n 50",
                    "find out what has been stopping it",
                );
                report(
                    Grade::Warn,
                    "backup",
                    format!(
                        "last completed backup was {days} days ago; \
                         the timer is active, so something is failing quietly"
                    ),
                    Some(fix),
                );
            } else {
                let when = match days {
                    0 => "today".to_string(),
                    1 => "yesterday".to_string(),
                    n => format!("{n} days ago"),
                };
                report(Grade::Ok, "backup", format!("last completed backup {when}"), None);
            }
        }
        (None, _) => {
            let fix = Action::new(
                "seed",
                "sudo kuma backup --init",
                "make the first copy, deliberately, while plugged in",
            );
            report(
                Grade::Warn,
                "backup",
                "no backup has ever completed on this machine".into(),
                Some(fix),
            );
        }
        // No clock is not a backup problem, and grading one on it would
        // be reporting on the RTC.
        (Some(_), None) => {}
    }
}

/// Snapshots fail quietly by design, and correctly so: the script
/// degrades rather than erroring when the target isn't btrfs, and the
/// timer is Persistent with a jittered delay, so nothing complains on a
/// machine taking no snapshots at all. Both choices are right for the
/// machine and wrong for the person, who otherwise learns on the one day
/// they wanted a file back. This is the check that asks out loud.
fn check_snapshots(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let config = match Config::load(Path::new(BAKED_CONFIG)) {
        Ok(config) => config,
        // The build validated this file before baking it in, so a copy
        // that won't load is worth naming even though this check can't
        // say more than that.
        Err(_) => {
            report(Grade::Warn, "snapshots", format!("cannot read {BAKED_CONFIG}"), None);
            return;
        }
    };
    // A declaration that never asked for snapshots is not unhealthy for
    // not having any, and saying so on every run would train people to
    // read past it.
    if !config.snapshots.enable {
        return;
    }

    // Ask the filesystem first: on a target that can't snapshot, the
    // timer being active is true and beside the point.
    let target = &config.snapshots.target;
    let fstype = host_output(&["findmnt", "-no", "FSTYPE", "-T", target]).unwrap_or_default();
    let fstype = fstype.trim();
    if !fstype.is_empty() && fstype != "btrfs" {
        report(
            Grade::Fail,
            "snapshots",
            format!("{target} is {fstype}, not btrfs; no snapshot can ever be taken"),
            // No action, for the same reason as the sibling below, and
            // said out loud here because it was not. The cure is an edit
            // to the declaration ([snapshots].target, or enable = false)
            // followed by a rebuild, and doctor reads the declaration the
            // image baked rather than the one somebody edits, so it
            // cannot name that file without guessing at a path that may
            // not be on this machine at all.
            None,
        );
        return;
    }

    // btrfs is necessary and not sufficient. A snapshot is taken of a
    // subvolume, and on a machine installed before kuma made one of
    // /var/home, the target is an ordinary directory inside the
    // deployment's /var. The snapshot script exits 0 on it, so the unit
    // succeeds, the timer is active, the store stays empty, and every
    // line here reads healthy while nothing will ever be taken.
    //
    // Unfixable from here: converting a directory that holds home
    // directories into a subvolume means moving them, which is not
    // something a health check should offer to do while people are
    // logged in. Saying so is the whole of the job.
    if let Some(false) =
        host_output(&["stat", "-c", "%i", target]).ok().as_deref().and_then(subvolume_root)
    {
        report(
            Grade::Fail,
            "snapshots",
            format!(
                "{target} is a directory, not a btrfs subvolume; \
                 the timer runs and takes nothing"
            ),
            None,
        );
        return;
    }

    let facts = unit_facts(&["kuma-snapshot.timer", "kuma-snapshot.service"]);
    match facts.get("kuma-snapshot.timer") {
        Some(fact) if fact.active == "active" => report(
            Grade::Ok,
            "snapshots",
            format!("kuma-snapshot.timer active ({})", config.snapshots.interval),
            None,
        ),
        Some(_) => {
            let fix = Action::new(
                "start",
                "sudo systemctl start kuma-snapshot.timer",
                "resume scheduled snapshots",
            );
            report(Grade::Fail, "snapshots", "kuma-snapshot.timer is not active".into(), Some(fix));
        }
        None => {
            report(Grade::Warn, "snapshots", "kuma-snapshot.timer state unavailable".into(), None)
        }
    }

    if let Some(fact) = facts.get("kuma-snapshot.service") {
        if !fact.result.is_empty() && fact.result != "success" {
            let fix = Action::new(
                "inspect",
                "systemctl status kuma-snapshot.service",
                "see why the snapshot failed",
            );
            report(
                Grade::Fail,
                "snapshots",
                format!("kuma-snapshot.service last run: {}", fact.result),
                Some(fix),
            );
        }
    }

    // The store is the only evidence the whole chain ran. A unit can be
    // active and successful and still have written nothing.
    let (store, count) = snapshot::store_state(&config);
    if count == 0 {
        // Warn, not fail: between switching to a declaration that asks
        // for snapshots and the timer's first tick, empty is the correct
        // state rather than a broken one.
        let fix = Action::new(
            "snapshot",
            "sudo systemctl start kuma-snapshot.service",
            "take the first one now instead of waiting for the timer",
        );
        report(
            Grade::Warn,
            "snapshots",
            format!("none taken yet in {}", store.display()),
            Some(fix),
        );
    } else {
        report(Grade::Ok, "snapshots", format!("{count} in {}", store.display()), None);
    }
}

/// What a machine has done to a file its own image ships.
#[derive(PartialEq, Debug)]
enum EtcState {
    /// Not in /usr/etc: this image doesn't ship it, so there is nothing
    /// to shadow.
    NotShipped,
    Matches,
    /// Edited locally. ostree carries the difference between /etc and
    /// /usr/etc forward onto every future deployment, so this copy wins
    /// over the image's, permanently and silently.
    Shadowed,
    /// Deleted locally, which carries forward the same way: the image's
    /// file stays gone across updates.
    Removed,
}

fn classify_etc(image: Option<&[u8]>, live: Option<&[u8]>) -> EtcState {
    match (image, live) {
        (None, _) => EtcState::NotShipped,
        (Some(_), None) => EtcState::Removed,
        (Some(from_image), Some(on_disk)) if from_image == on_disk => EtcState::Matches,
        _ => EtcState::Shadowed,
    }
}

struct EtcScan {
    /// Paths the image actually ships, which is what a percentage or a
    /// count should be taken over.
    owned: usize,
    shadowed: Vec<String>,
    removed: Vec<String>,
}

/// Compare each owned path under two roots. `paths` are absolute
/// (`/etc/greetd/config.toml`), so both roots get the same relative tail.
fn scan_etc(paths: &[String], image_root: &Path, live_root: &Path) -> EtcScan {
    let mut scan = EtcScan { owned: 0, shadowed: Vec::new(), removed: Vec::new() };
    for path in paths {
        let tail = path.trim_start_matches('/');
        let from_image = std::fs::read(image_root.join(tail)).ok();
        if from_image.is_none() {
            continue;
        }
        scan.owned += 1;
        let on_disk = std::fs::read(live_root.join(tail)).ok();
        match classify_etc(from_image.as_deref(), on_disk.as_deref()) {
            EtcState::Shadowed => scan.shadowed.push(path.clone()),
            EtcState::Removed => scan.removed.push(path.clone()),
            EtcState::Matches | EtcState::NotShipped => {}
        }
    }
    scan
}

/// /etc drift: is anything the image ships being overridden locally?
///
/// This is the general form of a trap that cost real time here. A var was
/// added to /etc/environment by hand to fix a display bug, later baked
/// into the image properly, and the hand-edited copy went on winning: the
/// declared version could never be tested, and nothing anywhere said so.
/// ostree's /etc merge is the mechanism, and it is working as designed;
/// what's missing is anyone telling you it happened.
///
/// Two design choices worth keeping. It compares /etc against /usr/etc
/// directly instead of shelling out to `ostree admin config-diff`, which
/// needs root: a health check that prompts for a password is a health
/// check people stop running, and /usr/etc is world-readable. And it only
/// looks at paths kuma's own image writes, computed from the machine's
/// baked declaration, because config-diff on a real system reports dozens
/// of legitimately-local files (machine-id, fstab, ssh host keys) and a
/// check that cries wolf teaches you to ignore it.
fn check_etc_drift(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    // The machine's own declaration says what its image owns.
    let Ok(config) = Config::load(Path::new(BAKED_CONFIG)) else {
        return;
    };
    // "/usr" and "/" in production; parameterized so the states this
    // check exists to catch can be exercised against a temp tree instead
    // of only ever running the everything-is-fine branch.
    let scan =
        scan_etc(&crate::containerfile::etc_paths(&config), Path::new("/usr"), Path::new("/"));
    let EtcScan { owned, shadowed, removed } = scan;
    if owned == 0 {
        return;
    }
    // Restoring the image's copy is `cp`, not `rm`: a deletion is itself
    // a local modification and carries forward as one, so removing the
    // file would keep it removed rather than let the image's version back.
    let restore = |path: &str| {
        Action::new(
            "restore",
            format!("sudo cp /usr{path} {path}"),
            "put the image's copy back so future updates flow through",
        )
    };
    if !shadowed.is_empty() {
        report(
            Grade::Warn,
            "etc",
            // No capture edge here, deliberately, and the message says so
            // rather than leaving the asymmetry with `kuma diff` looking
            // like an oversight. Package drift is a fork because a package
            // is your choice; /etc content is kuma's curation, so an edit
            // worth keeping belongs in the image rather than in your
            // declaration. That is how the COSMIC scanout vars got fixed.
            format!(
                "local edits shadow the image: {}. These win over every future image, so the declared version never applies. An edit worth keeping belongs in the image, not the declaration",
                shadowed.join(", ")
            ),
            Some(restore(&shadowed[0])),
        );
    }
    if !removed.is_empty() {
        report(
            Grade::Warn,
            "etc",
            format!("deleted locally, and staying deleted across updates: {}", removed.join(", ")),
            Some(restore(&removed[0])),
        );
    }
    if shadowed.is_empty() && removed.is_empty() {
        report(
            Grade::Ok,
            "etc",
            format!("{owned} files this image owns in /etc, none shadowed locally"),
            None,
        );
    }
}

/// Boot health: is the greenboot auto-rollback machinery in the image,
/// did THIS boot pass its checks, and does the bootloader actually
/// consult the boot counter — the part that can silently be missing on
/// machines whose bootloader config predates greenboot in the image.
/// Whether the boot menu names what it boots.
///
/// This grade exists because there is a repair behind it. ostree leaves
/// the entry titles alone whenever a deploy reuses the kernel and the
/// kargs, which is every kuma deploy that does not move the base, so
/// each title drifts one deployment behind the slot it labels.
/// `kuma-boot-titles.service` rewrites them at boot and after the
/// shutdown rotation; this is the check that says whether it did, and
/// the same pass is the fix it prescribes.
///
/// Takes its paths so the finding path is testable: on a machine where
/// the unit is working, the interesting branch is the one that never
/// runs.
fn check_boot_titles(
    entries: &Path,
    sysroot: &Path,
    report: &mut impl FnMut(Grade, &str, String, Option<Action>),
) {
    if !entries.is_dir() {
        return;
    }
    let stale = crate::bootentries::stale(entries, sysroot);
    if stale.is_empty() {
        report(Grade::Ok, "boot menu", "entry titles name the deployments they boot".into(), None);
        return;
    }
    for retitle in stale {
        report(
            Grade::Warn,
            "boot menu",
            format!(
                "{} is titled \"{}\" but boots {}",
                retitle.name(),
                crate::bootentries::without_slot(&retitle.from),
                crate::bootentries::without_slot(&retitle.to)
            ),
            Some(Action::new(
                "retitle",
                "sudo kuma boot-titles",
                "name each entry after the deployment it boots",
            )),
        );
    }
}

/// The nostr layer's grades, all informational-or-warn, absence silent:
/// a machine whose declaration never enabled the layer says nothing
/// here, because a feature somebody did not choose is not a finding.
///
/// The checks are the daemon's own answers read through its socket —
/// the unit active, the vault open, which relays answer — and two
/// things doctor is where they stay visible: an app holding Trust, the
/// standing grant the plan calls the loudest thing in the layer, and
/// the reachability fact, because an off-tailnet phone showing a dead
/// bunker needs the why and not just the symptom.
///
/// Everything here assumes a socket that might not answer: the doctor
/// runs as the user in the session, so the check is real, and a
/// refusal is a Warn with the fix — never a Fail, never a crash.
fn check_nostr(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let Ok(config) = Config::load(Path::new(BAKED_CONFIG)) else {
        // check_snapshots already named an unreadable baked declaration.
        return;
    };
    if !config.nostr.enable {
        return;
    }

    // The reachability fact, from the declaration rather than the
    // daemon: the relay set is what the unit runs, and the local relay
    // the layer bakes is this machine's own — so the fact is about
    // what the public set is. A ts.net entry says the tailnet is a
    // first-class caller; an empty list says the bunker is this
    // machine's and the CLI's alone.
    report(Grade::Ok, "nostr", reachability_wording(&config.nostr.relays), None);

    // The inactivity switch, from the declaration: the unit is what
    // carries it, so the declared window is the fact. Armed says so
    // and names the window; the off switch is silent, because the
    // desktop posture is the PAM-open keyring and a switch nobody
    // configured is not a finding. The floor is validated at build,
    // so an armed window here is floor-honest by construction.
    match config.nostr.inactivity_lock_secs {
        Some(0) => {}
        Some(secs) => report(
            Grade::Ok,
            "nostr",
            format!(
                "the inactivity switch is armed: the vault locks itself after {} of nothing \
                 unlocking or keeping it alive",
                human_secs(secs)
            ),
            None,
        ),
        None => {}
    }

    // The tailnet exposure, graded against the declaration: the baked
    // unit exists exactly when the declaration said serve, so the
    // question is whether tailscale's own state agrees. Asking it can
    // fail three ways (no operator, daemon down, command missing) and
    // all three grade warn — a check that could not be made is not a
    // broken machine.
    let serve_declared = config.nostr.serve;
    if serve_declared
        && (!config.nostr.relay.enable
            || !config.services.enable.iter().any(|s| s == "tailscaled.service"))
    {
        // A spoken serve with nothing to serve it: the relay is what
        // gets mapped and tailscaled is what answers for the tailnet,
        // and the unit that was not baked is the honest sign.
        report(
            Grade::Warn,
            "nostr",
            "the declaration says serve, but the local relay and tailscaled are what serving needs, and one of them is not enabled"
                .into(),
            None,
        );
    } else if serve_declared {
        match host_output(&["tailscale", "serve", "status"]) {
            Ok(status) if status.contains(":7777") => {
                report(Grade::Ok, "nostr", "the local relay is served on the tailnet".into(), None);
            }
            Ok(_) => report(
                Grade::Warn,
                "nostr",
                "the declaration says serve, and tailscale's serve config does not name the relay"
                    .into(),
                Some(Action::new(
                    "converge",
                    "sudo systemctl start kuma-nostr-serve.service".to_string(),
                    "the serve unit re-runs the mapping; convergence is the re-run",
                )),
            ),
            Err(_) => report(
                Grade::Warn,
                "nostr",
                "tailscale could not be asked about the serve mapping".into(),
                None,
            ),
        }
    }

    // The daemon's own answers, through its socket. Every question is
    // one ask; the socket not answering is the first finding, and the
    // rest are not asked.
    let mut client = match kuma::nostr::client::connect(None) {
        Ok(client) => client,
        Err(_) => {
            let (grade, detail, fix) = match unit_active() {
                Some(false) => (
                    Grade::Warn,
                    "the bunker's daemon is not running".to_string(),
                    Some(Action::new(
                        "start",
                        "systemctl --user start kuma-nostrd.service".to_string(),
                        "the unit is installed and enabled; it just is not running",
                    )),
                ),
                _ => (
                    Grade::Warn,
                    "the bunker's daemon is not answering on its socket".to_string(),
                    Some(Action::new(
                        "restart",
                        "systemctl --user restart kuma-nostrd.service".to_string(),
                        "a daemon that is running but not answering is one to restart",
                    )),
                ),
            };
            report(grade, "nostr", detail, fix);
            return;
        }
    };

    // The vault and the gate, from the daemon's own status.
    match client.status() {
        Ok(status) => {
            let vault = &status["vault"];
            match (vault["exists"].as_bool(), vault["unlocked"].as_bool()) {
                // Locked and never set up are different facts with
                // different roads: unlock on a missing vault is an
                // error, and advice that names it turns a fresh machine
                // into a dead end.
                (Some(false), _) => report(
                    Grade::Warn,
                    "nostr",
                    "the vault is not set up yet; setup creates the bunker's identity".into(),
                    Some(Action::new(
                        "setup",
                        "kuma-nostr setup".to_string(),
                        "it asks which road; `kuma-nostr import` brings a key you already hold",
                    )),
                ),
                (_, Some(true)) => report(
                    Grade::Ok,
                    "nostr",
                    "the vault is open and the bunker answers".into(),
                    None,
                ),
                (_, Some(false)) => report(
                    Grade::Warn,
                    "nostr",
                    "the bunker is locked; it pairs but signs nothing".into(),
                    Some(Action::new(
                        "unlock",
                        "kuma-nostr unlock".to_string(),
                        "the keyring is open in this session, so unlocking costs nothing",
                    )),
                ),
                _ => report(
                    Grade::Warn,
                    "nostr",
                    "the daemon's status did not say whether the vault is open".into(),
                    None,
                ),
            }
        }
        Err(e) => report(
            Grade::Warn,
            "nostr",
            format!("the daemon did not answer a status ask: {e}"),
            None,
        ),
    }

    // The loudest thing in the layer, by name — one warning for the
    // whole pile, so three apps on Trust read as one finding instead of
    // the same finding three times.
    if let Ok(apps) = client.apps() {
        let trusted = trust_apps(&apps);
        if !trusted.is_empty() {
            report(
                Grade::Warn,
                "nostr",
                trust_warning_wording(&trusted),
                Some(Action::new(
                    "review",
                    "kuma-nostr apps".to_string(),
                    "the levels are the panel's toggles and the doctor keeps them visible",
                )),
            );
        }
    }
}

/// The standing-grant warning's wording: one line for the whole pile of
/// apps on Trust, so three grants read as one finding instead of the
/// same finding three times.
fn trust_warning_wording(trusted: &[String]) -> String {
    let (subject, holds) = if trusted.len() == 1 {
        ("1 app".to_string(), "holds")
    } else {
        (format!("{} apps", trusted.len()), "hold")
    };
    format!(
        "{subject} {holds} Trust: a standing grant, signing without asking: {}",
        trusted.join(", ")
    )
}

/// The reachability fact's wording: what a person with a phone that
/// cannot reach the bunker needs first — the why, not the symptom.
/// A ts.net entry in the declared set means the tailnet is a
/// first-class caller; an empty set means the bunker is this
/// machine's and the CLI's alone.
fn reachability_wording(relays: &[String]) -> String {
    let tailnet = relays.iter().any(|relay| relay.contains(".ts.net"));
    if relays.is_empty() {
        "the bunker is local-only: no relays declared, so only this machine can reach it".into()
    } else if tailnet {
        "the bunker answers on the tailnet and the declared relays".into()
    } else {
        "the bunker answers through the declared public relays".into()
    }
}

/// A window in words: 86400 reads as "1 day", 3600 as "1 hour" — the
/// doctor's grades are read by a person, and a person does not divide
/// by 3600 for pleasure.
fn human_secs(secs: u64) -> String {
    match secs {
        s if s % 86400 == 0 => {
            format!("{} day{}", s / 86400, if s / 86400 > 1 { "s" } else { "" })
        }
        s if s % 3600 == 0 => {
            format!("{} hour{}", s / 3600, if s / 3600 > 1 { "s" } else { "" })
        }
        s if s % 60 == 0 => {
            format!("{} minute{}", s / 60, if s / 60 > 1 { "s" } else { "" })
        }
        s => format!("{s}s"),
    }
}

/// Whether the bunker's user unit is active, as `Some(active)` — `None`
/// when systemd could not be asked, which the caller renders as its
/// own warn rather than as a unit verdict.
fn unit_active() -> Option<bool> {
    let out = host_output(&["systemctl", "--user", "is-active", "kuma-nostrd.service"]).ok()?;
    Some(out.trim() == "active")
}

/// The apps holding Trust, named — the standing grants, by npub. A
/// non-array answer is no apps rather than an error: the daemon said
/// nothing, and doctor does not invent findings.
fn trust_apps(apps: &serde_json::Value) -> Vec<String> {
    apps["apps"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|app| app["level"] == "trust")
                .filter_map(|app| app["pubkey"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn check_boot_health(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    if !Path::new("/usr/libexec/greenboot/greenboot").exists() {
        report(
            Grade::Warn,
            "boot health",
            "greenboot not in this image; automatic rollback of failed boots arrives with the next rebuild".into(),
            Some(Action::new("update", "kuma update", "rebuild on a kuma that bakes boot health")),
        );
        return;
    }
    // RemainAfterExit keeps the health-check oneshot `active` after
    // success, so is-active is this boot's green/red verdict.
    match host_output_any(&["systemctl", "is-active", "greenboot-healthcheck.service"]).as_deref() {
        Ok("active") => {
            report(Grade::Ok, "boot health", "this boot passed its health checks".into(), None)
        }
        Ok("failed") => report(
            Grade::Fail,
            "boot health",
            "this boot failed health checks; greenboot may reboot toward rollback".into(),
            Some(Action::new(
                "inspect",
                "systemctl status greenboot-healthcheck.service",
                "see which check failed",
            )),
        ),
        Ok(state) => report(
            Grade::Warn,
            "boot health",
            format!("health check is {state}: boot still settling, or the unit is not enabled"),
            None,
        ),
        Err(_) => report(Grade::Warn, "boot health", "systemctl unavailable".into(), None),
    }
    // The counter lives in GRUB: its config must decrement boot_counter
    // and fall back when it hits zero. Fresh installs carry it in
    // grub.cfg (bootupd assembles greenboot's snippet); machines
    // installed before greenboot get it converged into custom.cfg by
    // kuma-boot-health-sync — grep both, or converged machines warn
    // forever. /boot/grub2 is 0700 on Fedora, hence sudo; `true` keeps
    // no-match from reading as sudo-declined.
    let cfg = host_output(&[
        "sudo", "sh", "-c",
        "grep -h boot_counter /boot/grub2/grub.cfg /boot/grub2/custom.cfg /boot/efi/EFI/fedora/grub.cfg 2>/dev/null; true",
    ]);
    match cfg {
        Ok(out) if !out.trim().is_empty() => report(
            Grade::Ok,
            "boot health",
            "bootloader counts boot attempts; fallback armed".into(),
            None,
        ),
        Ok(_) => report(
            Grade::Warn,
            "boot health",
            "bootloader has no boot_counter fallback (config predates greenboot in the image); without it a failing update reboot-loops instead of rolling back".into(),
            Some(Action::new(
                "converge",
                "sudo /usr/libexec/kuma-boot-health-sync",
                "install the grub fallback hook now (also runs on every boot)",
            )),
        ),
        Err(_) => report(
            Grade::Warn,
            "boot health",
            "grub.cfg unreadable (sudo declined?)".into(),
            None,
        ),
    }
}

/// Build leftovers eat disk quietly: rebuilds strand dangling images
/// (~3.5 GB each), and interrupted builds abandon buildah working
/// containers that pin their layers while being invisible to
/// `podman images` — one was once found holding 68 GB.
/// The two podman questions this check needs, asked off the main thread.
///
/// Measured on a machine with 232 image records: `podman images -f
/// dangling=true -q` takes 1.13 s and `podman ps -a --external` 40 ms,
/// together roughly 60% of a `kuma doctor`. The cost is enumeration
/// rather than filtering — `podman images -q` with any filter, or none,
/// costs the same — so it grows with how much podman storage has piled
/// up, which is exactly what this check exists to notice. It gets
/// slowest on the machines that most need it.
///
/// Nothing else in doctor depends on these, so they run while the rest
/// of the report is being gathered and are collected where they are
/// needed. Deliberately not the sudo-bearing probes: a password prompt
/// arriving from a background thread would interleave with the output.
fn build_leftovers_probe() -> std::thread::JoinHandle<(Option<usize>, Option<usize>)> {
    std::thread::spawn(|| {
        let dangling = host_output(&["podman", "images", "-f", "dangling=true", "-q"])
            .map(|out| out.lines().filter(|l| !l.trim().is_empty()).count())
            .ok();
        let abandoned = host_output_any(&[
            "podman",
            "ps",
            "-a",
            "--external",
            "--format",
            "{{.Names}} {{.Status}}",
        ])
        .map(|out| {
            out.lines()
                .filter(|l| l.contains("-working-container") && l.ends_with(" Storage"))
                .count()
        })
        .ok();
        (dangling, abandoned)
    })
}

fn check_build_leftovers(
    probe: std::thread::JoinHandle<(Option<usize>, Option<usize>)>,
    report: &mut impl FnMut(Grade, &str, String, Option<Action>),
) {
    // A panicked probe is a thread that told us nothing, which is the
    // same answer as podman being absent.
    let (dangling, abandoned) = probe.join().unwrap_or((None, None));
    let (dangling, abandoned) = (dangling.ok_or(()), abandoned.ok_or(()));
    match (dangling, abandoned) {
        (Err(_), Err(_)) => {} // no podman here — nothing to check
        (dangling, abandoned) => {
            let (dangling, abandoned) = (dangling.unwrap_or(0), abandoned.unwrap_or(0));
            if dangling == 0 && abandoned == 0 {
                report(Grade::Ok, "storage", "no build leftovers".into(), None);
            } else {
                // `kuma clean` reports every image object the prune cascade
                // deletes, so counting the same way needs a dry run podman
                // doesn't have — name the stranded builds instead.
                let mut parts = Vec::new();
                if dangling > 0 {
                    parts.push(format!(
                        "{dangling} stranded build image(s) plus their cached layers"
                    ));
                }
                if abandoned > 0 {
                    parts.push(format!("{abandoned} abandoned build container(s)"));
                }
                report(
                    Grade::Warn,
                    "storage",
                    parts.join(", "),
                    Some(Action::new("clean", "kuma clean", "reclaim them")),
                );
            }
        }
    }
}

/// Kernel-side only (driver bound, render node present) — userspace probes
/// need tools the image deliberately doesn't carry.
/// Whether the root filesystem sits inside a LUKS container.
///
/// Pure over what `findmnt` and `lsblk` answer, because the interesting
/// case is a machine this one is not: a check that can only run on an
/// encrypted machine would ship having never taken the other branch.
///
/// `findmnt` prints a btrfs subvolume as `/dev/mapper/luks-x[/root]`, so
/// the source has to be cut at the bracket before anything can be asked
/// about the device. `None` means the question could not be answered,
/// which is a reason to say nothing rather than to guess.
fn root_encrypted(findmnt_source: &str, lsblk_types: &str) -> Option<bool> {
    let source = root_device(findmnt_source);
    if source.is_empty() {
        return None;
    }
    let types: Vec<&str> = lsblk_types.split_whitespace().collect();
    if types.is_empty() {
        return None;
    }
    // lsblk names the whole stack under the device; a crypt layer
    // anywhere in it is the root sitting inside a container.
    Some(types.contains(&"crypt"))
}

/// The device out of what `findmnt` printed, which appends the btrfs
/// subvolume in brackets. Shared with the caller, which needs the same
/// answer to ask lsblk about it, and had its own copy of this until the
/// two could have disagreed.
pub fn root_device(findmnt_source: &str) -> &str {
    findmnt_source.trim().split('[').next().unwrap_or_default().trim()
}

/// Whether this machine can really come back from a hibernate.
///
/// Silent on a machine that never asked for one, like the checks that
/// grade a declared feature only where it is declared: a machine with no
/// swapfile suspends fine and promises nothing, and a permanent line
/// saying so would be doctor listing features rather than grading them.
///
/// Loud everywhere else, because every way this breaks is quiet. The
/// worst of them is an offset that no longer matches the file: the
/// machine writes memory to disk, powers off, boots fresh, and the only
/// evidence is a session that is gone. Nothing else on a running system
/// compares those two numbers, which is the whole reason this check is
/// worth its two `sudo` calls.
fn check_hibernate(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    // Probed once for both gradings: the lid verdict is a pure function
    // of the hibernate one plus one world-readable file, and asking the
    // machine twice for the sake of two check names is the lesser
    // version of the four-sudo probe this check used to be.
    let status = hibernate::probe();
    let verdict = hibernate::verdict(&status);
    match &verdict {
        hibernate::Verdict::NotSet => {}
        // Warn, like the bootc and grub.cfg reads that also need root.
        // A check that cannot run says so; it does not invent an answer.
        hibernate::Verdict::Unasked(detail) => {
            report(Grade::Warn, "hibernate", detail.clone(), None)
        }
        hibernate::Verdict::Ready(detail) => report(Grade::Ok, "hibernate", detail.clone(), None),
        hibernate::Verdict::Short(detail) => report(
            Grade::Warn,
            "hibernate",
            detail.clone(),
            Some(Action::new(
                "resize",
                "kuma hibernate --off --yes, then kuma hibernate --size <bigger> --yes",
                "a swapfile cannot be grown in place: its offset would move",
            )),
        ),
        // Warn, not fail: nothing kuma set up is wrong, and nothing kuma
        // can do will change it. It is still not silent, because the
        // machine is carrying a swapfile it will never use, and the one
        // legal move from here is to take that space back.
        hibernate::Verdict::Refused(detail) => report(
            Grade::Warn,
            "hibernate",
            detail.clone(),
            Some(Action::new(
                "reclaim",
                "kuma hibernate --off --yes",
                "take the swapfile back, or turn Secure Boot off in firmware and reboot",
            )),
        ),
        hibernate::Verdict::Broken(detail) => report(
            Grade::Fail,
            "hibernate",
            detail.clone(),
            Some(Action::new(
                "repair",
                "kuma hibernate --yes",
                "relabel the swapfile and put the kernel arguments back in step",
            )),
        ),
    }
    // The lid, beside hibernate and after it: a separate setting with a
    // separate name, so an assertion (and a reader) can ask for it
    // alone, and so hibernate's own grade stays about the swapfile and
    // the kernel arguments.
    match hibernate::lid_verdict(&verdict, status.lid_dropin) {
        hibernate::LidVerdict::Silent => {}
        hibernate::LidVerdict::Ok(detail) => report(Grade::Ok, "lid", detail, None),
        hibernate::LidVerdict::Missing(detail) => report(
            Grade::Fail,
            "lid",
            detail,
            Some(Action::new(
                "repair",
                "kuma hibernate --yes",
                "write the lid's suspend-then-hibernate setting; it rides the repair",
            )),
        ),
        hibernate::LidVerdict::Stale(detail) => report(
            Grade::Warn,
            "lid",
            detail,
            Some(Action::new(
                "take back",
                "kuma hibernate --off --yes",
                "remove the lid setting along with the rest of what it no longer rides on",
            )),
        ),
    }
}

fn check_encryption(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    // /sysroot on a booted ostree deployment, / everywhere else.
    let source = host_output(&["findmnt", "-no", "SOURCE", "/sysroot"])
        .or_else(|_| host_output(&["findmnt", "-no", "SOURCE", "/"]))
        .unwrap_or_default();
    let device = root_device(&source);
    let types = if device.is_empty() {
        String::new()
    } else {
        host_output(&["lsblk", "-no", "TYPE", device]).unwrap_or_default()
    };
    // Silent when unknowable. Encryption is a choice, so neither answer
    // is a fault and neither carries a fix: this line exists so that a
    // machine can say which choice was made, since nothing else on a
    // running system does.
    match root_encrypted(&source, &types) {
        Some(true) => {
            report(Grade::Ok, "encryption", "root is a LUKS volume, unlocked at boot".into(), None)
        }
        Some(false) => report(
            Grade::Ok,
            "encryption",
            "root is not encrypted; that was decided when this disk was partitioned".into(),
            None,
        ),
        None => {}
    }
}

/// Does this machine actually refuse an unsigned kuma image?
///
/// SECURITY.md says published images are signed, and a signature nobody
/// checks is a claim rather than a control. The check is the policy file
/// as the machine will really read it, not kuma's intent: an image built
/// before this shipped, or a hand-edited policy, both land here.
/// The key a policy requires for `repo`, or `None` if it requires none.
///
/// Split out from the check so it can be tested against the policy kuma
/// actually ships rather than against a hand-written copy of it. The two
/// halves of this feature live in different modules and are useless
/// apart: a policy nothing grades, or a grader that misreads the policy.
/// The second is what happened. `serde_json`'s `pointer` reads `/` as a
/// path separator, so `/transports/docker/ghcr.io/letdown2491/kuma`
/// addressed four nested objects instead of one key containing slashes,
/// found nothing, and reported that an image shipping the policy did not
/// have it. Nothing caught that until doctor ran inside a real image.
fn signature_key_for(policy_text: &str, repo: &str) -> Option<String> {
    let policy: serde_json::Value = serde_json::from_str(policy_text).ok()?;
    let rules = policy.get("transports")?.get("docker")?.get(repo)?.as_array()?;
    let signed =
        rules.iter().find(|r| r.get("type").and_then(|t| t.as_str()) == Some("sigstoreSigned"))?;
    Some(
        signed
            .get("keyPath")
            .and_then(|k| k.as_str())
            .unwrap_or(crate::containerfile::COSIGN_PUB_PATH)
            .to_string(),
    )
}

/// Does one registries.d file turn sigstore attachments on for `repo`?
///
/// containers/image scopes these by prefix, most specific first: an entry
/// for `ghcr.io` governs `ghcr.io/letdown2491/kuma` just as well as one
/// naming the repository outright, and `default-docker` governs
/// everything. Testing for the full repository string alone graded a
/// machine whose broader entry works perfectly as `Fail: updates would be
/// refused`, which is the worst direction for this check to be wrong in:
/// it sends somebody to fix a machine that is not broken.
///
/// Deliberately not a YAML parse. These files are three lines, kuma has
/// no YAML dependency and would not gain one for this, and the question
/// asked here is narrow enough that a prefix over the scope lines answers
/// it. The cost is that a scope buried in a comment would count; the
/// alternative was a check that failed working machines.
fn sigstore_attachments_cover(text: &str, repo: &str) -> bool {
    if !text.contains("use-sigstore-attachments") {
        return false;
    }
    // Every prefix of the repository that could legally be a scope, plus
    // the catch-all, so the widest working configuration still passes.
    let mut scopes = vec!["default-docker".to_string()];
    let parts: Vec<&str> = repo.split('/').collect();
    for n in 1..=parts.len() {
        scopes.push(parts[..n].join("/"));
    }
    text.lines()
        .map(|l| l.trim().trim_end_matches(':').trim())
        .any(|key| scopes.iter().any(|s| s == key))
}

fn check_signature_policy(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let repo = crate::published_repo();
    let rebuild =
        || Some(Action::new("rebuild", "kuma update", "rebuild on an image that ships the policy"));
    let Ok(text) = std::fs::read_to_string("/etc/containers/policy.json") else {
        report(
            Grade::Warn,
            "signatures",
            "no container signature policy; kuma's published images would not be verified".into(),
            rebuild(),
        );
        return;
    };
    if serde_json::from_str::<serde_json::Value>(&text).is_err() {
        report(Grade::Warn, "signatures", "cannot parse /etc/containers/policy.json".into(), None);
        return;
    }
    let Some(key) = signature_key_for(&text, repo) else {
        report(
            Grade::Warn,
            "signatures",
            format!("{repo} is not required to be signed by this machine's policy"),
            rebuild(),
        );
        return;
    };
    // A rule naming a key that is not there fails closed at pull time,
    // which is safe but arrives as a confusing error during an update
    // rather than as an answer here.
    let key = key.as_str();
    if !Path::new(key).exists() {
        report(
            Grade::Fail,
            "signatures",
            format!("policy requires a signature for {repo} but its key is missing ({key})"),
            rebuild(),
        );
        return;
    }
    // The other half of the pair. cosign stores a signature as a separate
    // tag beside the image, and without this the policy has nothing to
    // check and refuses every pull.
    let attachments = std::fs::read_dir("/etc/containers/registries.d")
        .map(|entries| {
            entries.flatten().any(|e| {
                std::fs::read_to_string(e.path())
                    .map(|t| sigstore_attachments_cover(&t, repo))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !attachments {
        report(
            Grade::Fail,
            "signatures",
            format!("policy requires a signature for {repo} but nothing tells it where signatures live; updates would be refused"),
            rebuild(),
        );
        return;
    }
    report(
        Grade::Ok,
        "signatures",
        format!("{repo} must carry kuma's signature; unsigned images are refused"),
        None,
    );
}

fn check_gpu(report: &mut impl FnMut(Grade, &str, String, Option<Action>)) {
    let mut drivers: Vec<String> = Vec::new();
    if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
        for card in cards.flatten() {
            let name = card.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            if let Ok(driver) = std::fs::read_link(card.path().join("device/driver")) {
                if let Some(driver) = driver.file_name() {
                    drivers.push(driver.to_string_lossy().into_owned());
                }
            }
        }
    }
    let render = std::fs::read_dir("/dev/dri").is_ok_and(|mut d| {
        d.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("renderD")))
    });
    match (drivers.is_empty(), render) {
        (false, true) => report(
            Grade::Ok,
            "gpu",
            format!("{} bound, render node present", drivers.join(", ")),
            None,
        ),
        (false, false) => report(
            Grade::Warn,
            "gpu",
            format!("{} bound, but no render node; software rendering likely", drivers.join(", ")),
            None,
        ),
        (true, _) => {
            report(Grade::Warn, "gpu", "no GPU driver bound (VM or headless?)".into(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The lines below are the watcher's own failure vocabulary,
    /// copied from the idle module's log calls. The parse takes the
    /// LAST failure line, because a later failure replaces an earlier
    /// one as this moment's answer, and reads a healthy journal — which
    /// says nothing about idle at all — as healthy: a watcher that logs
    /// nothing is the one that is working. There is no "started fine"
    /// line to look for; silence is the success signal.
    #[test]
    fn the_last_word_about_the_idle_watcher_is_the_one_that_counts() {
        // a failure is the finding
        assert_eq!(
            idle_watcher_failure("idle watcher stopped: connection closed"),
            Some("idle watcher stopped: connection closed".to_string())
        );
        // the protocol-missing line is the same finding in other words
        assert_eq!(
            idle_watcher_failure(
                "the compositor does not advertise ext-idle-notify-v1; idle locking is off"
            ),
            Some(
                "the compositor does not advertise ext-idle-notify-v1; idle locking is off"
                    .to_string()
            )
        );
        // silence is health
        assert_eq!(idle_watcher_failure("hot reload: reloaded\nbar started"), None);
        // the last word wins: the newest failure is the live one
        assert_eq!(
            idle_watcher_failure(
                "idle watcher stopped: connection closed\n\
                 idle watcher stopped: compositor went away"
            ),
            Some("idle watcher stopped: compositor went away".to_string())
        );
    }

    /// The shell's [idle] section is optional and partial-tolerant: a
    /// config that predates it, or names only some keys, must read as
    /// "the defaults run" rather than as an error, because that is
    /// exactly how the shell reads it. A 0 is not missing — it is the
    /// user's "never lock on idle", and doctor's grading branches on
    /// the difference.
    #[test]
    fn the_shells_idle_config_is_optional_until_it_says_zero() {
        // nothing configured: the defaults run
        assert_eq!(configured_idle_lock(""), None);
        assert_eq!(configured_idle_lock("[notifications]\ndnd = true\n"), None);
        // a partial section: only one key named is still a valid config
        assert_eq!(configured_idle_lock("[idle]\nscreen_off_timeout = 60\n"), None);
        assert_eq!(configured_idle_lock("[idle]\nlock_timeout = 600\n"), Some(600));
        // the disabled lock: a fact about a choice, not an absence
        assert_eq!(configured_idle_lock("[idle]\nlock_timeout = 0\n"), Some(0));
        // a file that does not parse is the shell's default too
        assert_eq!(configured_idle_lock("[idle\nlock_timeout = 600"), None);
    }

    /// `systemctl show` answers for many units in one flat stream, and
    /// every key belongs to the `Id=` above it. Attributing one unit's
    /// invocation to another would quote the wrong unit's failure back
    /// at a person, which is worse than quoting none.
    #[test]
    fn unit_facts_keep_every_key_with_its_own_unit() {
        let text = "Id=kuma-flatpak-sync.service\n\
                    ActiveState=failed\n\
                    Result=exit-code\n\
                    InvocationID=aaaa1111\n\
                    ExecMainExitTimestamp=@1787093909\n\
                    Id=kuma-brew-sync.service\n\
                    ActiveState=active\n\
                    Result=success\n\
                    InvocationID=bbbb2222\n\
                    ExecMainExitTimestamp=\n";
        let facts = parse_unit_facts(text);
        let flatpak = &facts["kuma-flatpak-sync.service"];
        assert_eq!((flatpak.active.as_str(), flatpak.result.as_str()), ("failed", "exit-code"));
        assert_eq!(flatpak.invocation, "aaaa1111");
        assert_eq!(flatpak.last_exit, Some(1_787_093_909));
        let brew = &facts["kuma-brew-sync.service"];
        assert_eq!(brew.invocation, "bbbb2222");
        assert_eq!(brew.last_exit, None, "a unit that never ran has no exit time");
    }

    /// The finding here only exists on machines that were something else
    /// first, which is why the scan takes its roots: on a healthy
    /// machine this branch never runs, and a branch that never runs is
    /// not covered by anything.
    #[test]
    fn an_override_pointing_at_nothing_is_found_and_the_rest_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("overrides");
        std::fs::create_dir_all(&root).unwrap();

        // Somebody's real settings, whoever wrote them.
        std::fs::write(root.join("com.google.Chrome"), "[Context]\n").unwrap();
        // A symlink that resolves: still somebody's settings.
        let real = dir.path().join("elsewhere");
        std::fs::write(&real, "[Context]\n").unwrap();
        std::os::unix::fs::symlink(&real, root.join("org.chromium.Chromium")).unwrap();
        // The wreckage: a distribution's overrides directory that this
        // machine does not have, inherited across an image switch.
        let gone = Path::new("/usr/share/ublue-os/flatpak-overrides/io.github.kolunmi.Bazaar");
        std::os::unix::fs::symlink(gone, root.join("io.github.kolunmi.Bazaar")).unwrap();

        let roots = [root.clone()];
        let found = dangling_overrides(&roots);
        assert_eq!(found.len(), 1, "only the dangling link is a finding: {found:?}");
        assert_eq!(found[0].0, root.join("io.github.kolunmi.Bazaar"));
        assert_eq!(found[0].1, gone, "the report names what it points at");

        // A directory that does not exist is not an error: plenty of
        // machines have no overrides at all.
        assert!(dangling_overrides(&[dir.path().join("nothing-here")]).is_empty());

        let mut graded = Vec::new();
        check_overrides(&[root], &mut |grade, name, detail, action| {
            graded.push((grade, name.to_string(), detail, action))
        });
        assert_eq!(graded.len(), 1);
        assert!(matches!(graded[0].0, Grade::Warn), "a machine with one is not broken");
        assert!(graded[0].2.contains("io.github.kolunmi.Bazaar"));
        let fix = graded[0].3.as_ref().unwrap();
        assert!(fix.cmd.starts_with("rm ") || fix.cmd.starts_with("sudo rm "), "{}", fix.cmd);
        assert!(
            fix.cmd.ends_with("io.github.kolunmi.Bazaar"),
            "the fix names the link: {}",
            fix.cmd
        );
    }

    /// The quiet failure this check exists for: a converger whose last
    /// run succeeded, weeks ago, on a machine where nothing has
    /// A declaration asking for monthly copies is not unhealthy on day
    /// eight, and one asking for hourly copies is not healthy on day
    /// six. The floor exists because the timer is Persistent: a week
    /// cannot be reached by a laptop that was shut, only by a loop that
    /// has stopped turning.
    #[test]
    fn how_stale_is_too_stale_follows_the_declared_interval() {
        assert_eq!(backup_stale_after_days("daily"), 7, "the floor, not two days");
        assert_eq!(backup_stale_after_days("hourly"), 7);
        assert_eq!(backup_stale_after_days("weekly"), 14, "two missed firings");
        assert_eq!(backup_stale_after_days("monthly"), 62);
        // An expression somebody wrote by hand is almost always
        // sub-daily, and erring toward asking beats erring toward
        // silence on this particular question.
        assert_eq!(backup_stale_after_days("*-*-* 03:00:00"), 7);
    }

    /// The stamp is two fields: epoch for this code, readable for
    /// whoever cats the file. Anything unreadable has to mean "no backup
    /// has completed", because to the person who needs their data back
    /// those are the same thing.
    #[test]
    fn the_stamp_is_read_by_its_first_field() {
        assert_eq!(backup_stamp("1787201660 2026-08-20T04:54:20Z\n"), Some(1_787_201_660));
        assert_eq!(backup_stamp("1787201660\n"), Some(1_787_201_660));
        for junk in ["", "\n", "not-a-number today", "2026-08-20T04:54:20Z"] {
            assert_eq!(backup_stamp(junk), None, "unreadable must not read as fresh: {junk:?}");
        }
    }

    /// converged since. `Result=success` and an active timer are both
    /// true of that machine.
    #[test]
    fn a_machine_that_stopped_converging_is_a_finding() {
        const DAY: i64 = 86_400;
        let now = 1_800_000_000;
        let ago = |days: i64| Some(now - days * DAY);
        let stale = |days: i64| convergence_staleness(true, ago(days), Some(now));

        assert_eq!(stale(0), None, "converged today");
        assert_eq!(stale(6), None, "six days is six timer firings");
        assert_eq!(
            stale(STALE_CONVERGENCE_DAYS as i64),
            Some(STALE_CONVERGENCE_DAYS),
            "the threshold itself reports"
        );
        assert_eq!(stale(30), Some(30));

        // A unit no timer runs again last ran at boot, so its age is the
        // machine's uptime. kuma-user-sync is that unit, and asking it
        // this question failed healthy machines for staying up a week.
        assert_eq!(
            convergence_staleness(false, ago(30), Some(now)),
            None,
            "a boot-only unit is never stale, however long the uptime"
        );

        // Reporting on systemd rather than on the machine is not this
        // check's job: a unit that never ran, or a field that stopped
        // being emitted, is not a stale machine.
        assert_eq!(convergence_staleness(true, None, Some(now)), None, "a unit that never ran");
        assert_eq!(convergence_staleness(true, ago(365), None), None, "an unreadable clock");

        // An unset RTC puts the last run in the future; that is zero days
        // old, not an enormous unsigned number.
        assert_eq!(convergence_staleness(true, Some(now + DAY), Some(now)), None);
    }

    /// Whether a unit is judged on how long ago it last ran is a column
    /// in a table, and a column is a thing somebody can flip. The table
    /// carries its own answer: a service is run again by a timer exactly
    /// when this list also carries that timer, so the two halves cannot
    /// disagree without saying so.
    /// The shape found on a real machine: an enablement whose unit file
    /// is gone. The negatives beside it are what keep the check narrow
    /// enough to be worth reading: a working enablement, a plain file
    /// somebody dropped in /etc/systemd/system, and a directory that is
    /// not a target's wants at all.
    #[test]
    fn a_unit_enabled_with_no_unit_file_is_found_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let wants = root.join("default.target.wants");
        std::fs::create_dir_all(&wants).unwrap();

        // the real unit, enabled the ordinary way
        let real = root.join("real.service");
        std::fs::write(&real, "[Service]\n").unwrap();
        std::os::unix::fs::symlink(&real, wants.join("real.service")).unwrap();
        // the wreckage: enabled, then the unit file deleted
        std::os::unix::fs::symlink(root.join("gone.service"), wants.join("gone.service")).unwrap();
        // a unit file sitting in /etc that nothing enabled is not a finding
        std::fs::write(root.join("idle.service"), "[Service]\n").unwrap();
        // and a directory that is not an enablement directory is skipped
        let other = root.join("some.service.d");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(root.join("also-gone"), other.join("10-x.conf")).unwrap();

        let found = dangling_enablements(root);
        assert_eq!(found.len(), 1, "only the dangling enablement is a finding: {found:?}");
        assert_eq!(found[0].1, "gone.service");

        let mut graded = Vec::new();
        check_enablements(root, &mut |grade, name, detail, action| {
            graded.push((grade, name.to_string(), detail, action));
        });
        assert_eq!(graded.len(), 1);
        assert!(matches!(graded[0].0, Grade::Warn), "the machine boots; it just does not do this");
        assert!(graded[0].2.contains("has never started"), "{:?}", graded[0].2);
        assert!(graded[0].3.as_ref().unwrap().cmd.starts_with("sudo rm "));
        // a machine with nothing enabled locally is silent
        assert!(dangling_enablements(&root.join("nope")).is_empty());
    }

    #[test]
    fn only_units_a_timer_runs_again_are_judged_on_their_age() {
        let all = convergence_targets(true, true, true);
        assert_eq!(all.len(), 5, "an account, and a service plus a timer for each converger");
        for (unit, _, on_a_timer) in &all {
            let Some(stem) = unit.strip_suffix(".service") else {
                assert!(!on_a_timer, "{unit} is a timer; nothing asks a timer its age");
                continue;
            };
            let timer = format!("{stem}.timer");
            let has_timer = all.iter().any(|(u, _, _)| *u == timer);
            assert_eq!(
                *on_a_timer, has_timer,
                "{unit} is judged on its age as if a timer ran it, and {timer} is not here"
            );
        }
        // The one that made this a bug rather than a hypothetical.
        assert!(
            all.iter().any(|(u, _, timed)| *u == "kuma-user-sync.service" && !timed),
            "user-sync runs at boot only; its last run is the last boot"
        );
        // Nothing is graded on a machine that declares neither.
        assert!(convergence_targets(false, false, false).is_empty());
    }

    /// `doctor --report` is written to be pasted, which is why the
    /// declaration inside it is redacted. A failed unit's own words are a
    /// second way into the same report, and `kuma-user-sync` runs with
    /// the declared hash in its environment.
    #[test]
    fn a_quoted_failure_cannot_carry_a_password_hash() {
        let real = "chpasswd: line 1: user 'mira' \
                    $6$rounds=656000$abcdefgh$0123456789abcdef not found";
        let masked = one_line_reason(real);
        assert!(!masked.contains("$6$"), "the hash survived: {masked}");
        assert!(!masked.contains("0123456789abcdef"), "the hash survived: {masked}");
        assert!(masked.contains("<redacted>"));
        // Redaction that eats the sentence would just move the problem.
        assert!(masked.contains("chpasswd") && masked.contains("not found"), "{masked}");

        for shape in ["$y$j9T$abc", "$2b$12$abcdefgh", "$argon2id$v=19$m=64"] {
            let out = mask_hashes(&format!("failed with {shape} here"));
            assert!(!out.contains(shape), "{shape} survived: {out}");
            assert!(out.ends_with(" here"), "the rest of the line survives: {out}");
        }

        // A lone dollar is money, a shell variable, or an exit code, and
        // masking those would make ordinary failures unreadable.
        for kept in ["exit $?", "costs $5 a month", "PATH=$HOME/bin not found"] {
            assert_eq!(mask_hashes(kept), kept);
        }
    }

    /// The reason is arbitrary output from whatever the unit ran, and it
    /// gets printed inside a health report: one line, bounded, and no
    /// control characters that could rewrite the lines around it.
    #[test]
    fn a_failure_reason_stays_one_bounded_line() {
        let plain = "Error: Failed to update org.mozilla.firefox";
        assert_eq!(one_line_reason(plain), plain);
        assert_eq!(one_line_reason("  spaced  "), "spaced");

        let escape = one_line_reason("Error:\u{1b}[2Ktampered\nsecond line");
        assert!(!escape.contains('\u{1b}'), "an escape sequence would rewrite the report");
        assert!(!escape.contains('\n'), "a health report is one line per check");

        let long = one_line_reason(&"x".repeat(500));
        assert!(long.chars().count() <= REASON_MAX);
        assert!(long.contains(" ... "), "a truncated reason says that it was truncated");

        // The real line that motivated all of this. Both ends have to
        // survive: the head names what failed, the tail says why, and
        // an earlier cut kept only the head.
        let real = "Error: Failed to update org.mozilla.firefox: While pulling \
                    app/org.mozilla.firefox/x86_64/stable from remote flathub: Decompressed \
                    delta part exceeds configured limit of 76330069 bytes";
        let shown = one_line_reason(real);
        assert!(shown.chars().count() <= REASON_MAX);
        assert!(shown.contains("org.mozilla.firefox"), "the reason must name what failed");
        assert!(shown.contains("exceeds configured limit"), "the reason must say why");
        // Elision, not damage: neither end may stop mid-word when a
        // space was within reach of the cut.
        let (head, tail) = shown.split_once(" ... ").expect("a truncated reason marks the gap");
        assert!(real.contains(&format!("{head} ")), "the head stopped mid-word: {head}");
        assert!(real.contains(&format!(" {tail}")), "the tail started mid-word: {tail}");

        // A run of characters with no space in it has no boundary to
        // find, and hunting for one would eat the text instead.
        let unbroken = one_line_reason(&format!("prefix {} suffix", "u".repeat(400)));
        assert!(unbroken.chars().count() <= REASON_MAX);
        assert!(unbroken.contains(" ... "));
        assert!(
            unbroken.chars().count() > REASON_MAX / 2,
            "walking to a distant space would throw away most of the line: {unbroken}"
        );
    }

    /// The epoch values come from `date -u -d … +%s`. A date crate would
    /// have brought its own tests; a hand-rolled civil-from-days needs
    /// these, and the leap-year cases are where that arithmetic breaks.
    #[test]
    fn rfc3339_parses_to_the_same_instant_date_reports() {
        assert_eq!(epoch_from_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_from_rfc3339("2026-08-13T00:00:00Z"), Some(1_786_579_200));
        assert_eq!(epoch_from_rfc3339("2026-07-04T13:45:00Z"), Some(1_783_172_700));
        // 2000 is a leap year (divisible by 400), 1900 was not.
        assert_eq!(epoch_from_rfc3339("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(epoch_from_rfc3339("2024-02-29T00:00:00Z"), Some(1_709_164_800));
    }

    /// bootc's timestamp is chrono-serialized, so the tail varies with
    /// what the image recorded. Everything past the minutes is noise to a
    /// number of days, and none of it may turn a real answer into None.
    #[test]
    fn the_tail_of_a_timestamp_is_ignored_not_fatal() {
        let plain = epoch_from_rfc3339("2026-08-13T00:00:00Z");
        assert_eq!(epoch_from_rfc3339("2026-08-13T00:00:00.123456789Z"), plain);
        assert_eq!(epoch_from_rfc3339("2026-08-13T00:00:00+00:00"), plain);
        assert_eq!(epoch_from_rfc3339("2026-08-13T00:00Z"), plain);
    }

    /// An unreadable timestamp has to read as "no answer". The failure
    /// this guards is a garbage field parsing to *some* number and a
    /// machine being told its image is fifty years old.
    #[test]
    fn a_timestamp_that_isnt_one_is_no_answer() {
        for bad in [
            "",
            "2026-08-13",
            "not-a-date",
            "2026-13-01T00:00:00Z", // month 13
            "2026-08-32T00:00:00Z", // day 32
            "2026-08-13T24:00:00Z", // hour 24
            "2026-08-13T00:60:00Z", // minute 60
            "2026-08-13Tmorning",
        ] {
            assert_eq!(epoch_from_rfc3339(bad), None, "{bad}");
        }
    }

    /// Clock skew is real on a machine whose RTC hasn't been set yet, and
    /// the subtraction is unsigned: without the floor, an image stamped
    /// one hour in the future reads as some hundred-million days old and
    /// doctor screams about it.
    #[test]
    fn a_future_timestamp_is_zero_days_old_not_an_enormous_number() {
        assert_eq!(days_since("2099-01-01T00:00:00Z"), Some(0));
        assert_eq!(days_since("1970-01-02T00:00:00Z").map(|d| d > 20_000), Some(true));
        assert_eq!(days_since("nonsense"), None);
    }

    /// This decides whether a failed systemd-remount-fs is excused, so a
    /// loose match here is how a real failure gets called known-benign
    /// forever. The near-misses are the point: `root` is a subvolume name
    /// on every Anaconda btrfs install, and /var/roothome is a real mount.
    #[test]
    fn only_an_active_root_entry_excuses_the_remount_failure() {
        let anaconda = "UUID=86bf4581 / btrfs subvol=root,compress=zstd:1,ro 0 0\n\
                        UUID=4de4aa74 /boot ext4 defaults 1 2\n";
        assert!(fstab_declares_root(anaconda));

        // What kuma-fstab-sync leaves behind: the same line, commented.
        let converged = "# Commented out by kuma-fstab-sync: this machine boots a composefs\n\
                         #UUID=86bf4581 / btrfs subvol=root,compress=zstd:1,ro 0 0\n\
                         UUID=4de4aa74 /boot ext4 defaults 1 2\n";
        assert!(!fstab_declares_root(converged));

        // A space after the comment marker shifts every field right, which
        // is exactly how a naive field test would still see a root mount.
        assert!(!fstab_declares_root("# UUID=86bf4581 / btrfs subvol=root 0 0\n"));

        // Neither of these is a root entry, and both contain "root".
        assert!(!fstab_declares_root("UUID=y /var/roothome btrfs subvol=root 0 0\n"));
        assert!(!fstab_declares_root("UUID=4de4aa74 /boot ext4 defaults 1 2\n"));

        assert!(!fstab_declares_root(""));
    }

    fn config(toml: &str) -> Config {
        toml::from_str(toml).expect("test config parses")
    }

    /// A machine with nothing on it, to be filled in per test. Every
    /// observation present means "looked, found this"; the Nones in the
    /// individual tests mean "couldn't look at all".
    fn machine() -> Machine {
        Machine {
            rpm: None,
            flatpak_system: Some(BTreeSet::new()),
            flatpak_user: BTreeSet::new(),
            flatpak_state: BTreeSet::new(),
            brew_installed: Some(BTreeSet::new()),
            brew_leaves: BTreeSet::new(),
            brew_state: BTreeSet::new(),
        }
    }

    fn items(found: &[Candidate]) -> Vec<&str> {
        found.iter().map(|c| c.item.as_str()).collect()
    }

    /// The whole point of the verb: what convergence is about to destroy
    /// is exactly what capture offers to keep. diff builds its "removes
    /// it" entries from this same set, so the two cannot disagree.
    #[test]
    fn capture_offers_what_convergence_would_destroy() {
        let config = config("schema_version = 1\n[packages]\nflatpak = [\"org.gnome.Loupe\"]\n");
        let mut m = machine();
        m.flatpak_system = Some(set(&["org.gnome.Loupe", "org.gnome.Boxes"]));
        // the sync installed Boxes back when it was declared, so it is
        // convergence's to take back
        m.flatpak_state = set(&["org.gnome.Loupe", "org.gnome.Boxes"]);
        let found = candidates(&config, &m);
        assert_eq!(items(&found), ["org.gnome.Boxes"]);
        assert!(found[0].doomed);
        assert_eq!(found[0].list, "flatpak");
        // and the declared one is never offered back to itself
        assert!(!items(&found).contains(&"org.gnome.Loupe"));
    }

    /// The reason a Flatpak store can exist on a kuma machine at all.
    /// Installing system-wide is no longer evidence that convergence put
    /// it there, so an app the owner installed is offered for capture
    /// and left alone until they decide — while one the sync installed
    /// and the declaration dropped is still removed.
    #[test]
    fn a_store_install_is_the_owners_not_convergences() {
        let config = config("schema_version = 1\n");
        let mut m = machine();
        m.flatpak_system = Some(set(&["io.github.kolunmi.Bazaar", "org.gnome.Boxes"]));
        m.flatpak_state = set(&["org.gnome.Boxes"]);
        let found = candidates(&config, &m);
        assert_eq!(items(&found), ["org.gnome.Boxes", "io.github.kolunmi.Bazaar"], "urgent first");
        assert!(found[0].doomed, "Boxes was convergence's and is now undeclared");
        assert!(!found[1].doomed, "the store put Bazaar there, so it is the owner's");
        assert!(!found[1].promotes, "it is already system-wide; declaring only writes it down");
    }

    /// You cannot imperatively install an rpm on a bootc machine, so
    /// [packages].rpm is already declarative and there is nothing to
    /// capture. An observation full of undeclared rpms changes nothing.
    #[test]
    fn rpm_is_never_a_capture_candidate() {
        let config = config("schema_version = 1\n[packages]\nrpm = [\"fish\"]\n");
        let mut m = machine();
        m.rpm = Some(set(&["fish", "vim", "gcc", "systemd"]));
        assert!(candidates(&config, &m).is_empty());
    }

    /// Leaves, not the full list: a dependency is baggage that arrived
    /// with a choice, and declaring it would pin someone else's
    /// implementation detail into your system definition.
    #[test]
    fn brew_dependencies_are_not_choices() {
        let config = config("schema_version = 1\n");
        let mut m = machine();
        m.brew_installed = Some(set(&["ripgrep", "pcre2"]));
        m.brew_leaves = set(&["ripgrep"]);
        let found = candidates(&config, &m);
        assert_eq!(items(&found), ["ripgrep"]);
        // ad-hoc, so convergence isn't coming for it: undeclared costs it
        // reproducibility, not survival
        assert!(!found[0].doomed);
        assert_eq!(found[0].list, "brew");
    }

    /// A formula the sync installed and the declaration no longer names
    /// is on convergence's removal list, so it is urgent even though the
    /// ad-hoc ones next to it are not.
    #[test]
    fn brew_convergence_owns_what_it_installed() {
        let config = config("schema_version = 1\n");
        let mut m = machine();
        m.brew_installed = Some(set(&["btop", "jq"]));
        m.brew_leaves = set(&["btop", "jq"]);
        m.brew_state = set(&["btop"]);
        let found = candidates(&config, &m);
        assert_eq!(items(&found), ["btop", "jq"], "urgent first");
        assert!(found[0].doomed, "btop was convergence's and is now undeclared");
        assert!(!found[1].doomed, "jq was always the owner's");
    }

    /// The escape hatch stays an escape hatch. Capturing a --user flatpak
    /// installs it system-wide and hands it to convergence, which is a
    /// change to what it is, not just to where it is written down.
    #[test]
    fn user_flatpaks_are_opt_in_only() {
        let config = config("schema_version = 1\n");
        let mut m = machine();
        m.flatpak_user = set(&["org.gnome.Boxes"]);
        let found = candidates(&config, &m);
        assert_eq!(items(&found), ["org.gnome.Boxes"]);
        assert!(found[0].promotes);
        assert!(!found[0].doomed, "convergence never touches --user installs");
    }

    /// The same app installed both ways, or already declared, must not
    /// show up twice or at all.
    #[test]
    fn user_flatpaks_already_covered_are_not_offered_again() {
        let config = config("schema_version = 1\n[packages]\nflatpak = [\"org.gnome.Papers\"]\n");
        let mut m = machine();
        m.flatpak_system = Some(set(&["org.gnome.Boxes"]));
        m.flatpak_user = set(&["org.gnome.Boxes", "org.gnome.Papers"]);
        let found = candidates(&config, &m);
        assert_eq!(items(&found), ["org.gnome.Boxes"], "system install already covers it");
        assert!(!found[0].promotes);
    }

    /// An observation nobody could make is not evidence of an empty
    /// machine: flatpak missing must never read as "everything you have
    /// is undeclared" (or, worse, propose declaring nothing at all).
    #[test]
    fn unobservable_lists_yield_no_candidates() {
        let config = config("schema_version = 1\n");
        let mut m = machine();
        m.flatpak_system = None;
        m.brew_installed = None;
        m.brew_leaves = set(&["ripgrep"]);
        assert!(candidates(&config, &m).is_empty());
    }

    /// The trap `kuma sync` used to walk into: it starts convergers,
    /// convergers read what the image baked, and an edit that has not
    /// been built cannot reach them however often sync runs. Reported
    /// per list, because being ahead in any one of them is enough.
    #[test]
    fn a_declaration_ahead_of_the_image_is_visible_before_converging() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("usr/lib/kuma")).unwrap();
        std::fs::write(root.join("usr/lib/kuma/flatpaks"), "org.mozilla.firefox\n").unwrap();
        std::fs::write(root.join("usr/lib/kuma/brews"), "ripgrep\n").unwrap();

        let matching = config(
            "schema_version = 1\n[packages]\n\
             flatpak = [\"org.mozilla.firefox\"]\nbrew = [\"ripgrep\"]\n",
        );
        assert!(!baked_is_behind(&matching, root), "a built declaration is not behind");

        let edited = config(
            "schema_version = 1\n[packages]\n\
             flatpak = [\"org.mozilla.firefox\", \"org.gnome.Loupe\"]\nbrew = [\"ripgrep\"]\n",
        );
        assert!(baked_is_behind(&edited, root), "an unbuilt flatpak edit is behind");

        let brewed = config(
            "schema_version = 1\n[packages]\n\
             flatpak = [\"org.mozilla.firefox\"]\nbrew = [\"ripgrep\", \"jq\"]\n",
        );
        assert!(baked_is_behind(&brewed, root), "an unbuilt brew edit is behind");

        // permissions count too: they converge from the image the same way
        let permitted = config(
            "schema_version = 1\n[packages]\n\
             flatpak = [\"org.mozilla.firefox\"]\nbrew = [\"ripgrep\"]\n\
             [overrides.\"org.mozilla.firefox\"]\nsockets = [\"wayland\"]\n",
        );
        assert!(baked_is_behind(&permitted, root), "an unbuilt permission edit is behind");
    }

    /// The four states an image-owned /etc file can be in. "Not shipped"
    /// has to be distinct from "matches": a path the declaration implies
    /// but this image never wrote cannot be shadowed, and reporting it
    /// would be the false alarm that gets the whole check ignored.
    #[test]
    fn etc_states_separate_shadowing_from_absence() {
        let image = b"KUMA=1\n".as_slice();
        assert_eq!(classify_etc(Some(image), Some(image)), EtcState::Matches);
        assert_eq!(classify_etc(Some(image), Some(b"KUMA=0\n")), EtcState::Shadowed);
        assert_eq!(classify_etc(Some(image), None), EtcState::Removed);
        // no image copy: nothing to shadow, whatever is on disk
        assert_eq!(classify_etc(None, Some(b"local only\n")), EtcState::NotShipped);
        assert_eq!(classify_etc(None, None), EtcState::NotShipped);
        // byte comparison, so whitespace counts: an /etc file that differs
        // by a trailing newline still wins over the image's copy
        assert_eq!(classify_etc(Some(b"KUMA=1\n"), Some(b"KUMA=1")), EtcState::Shadowed);
    }

    /// The question doctor asks about a snapshot target, and the reason
    /// it asks it by inode: 256 is every subvolume root, and the
    /// privileged way to ask cannot tell "not a subvolume" from "not
    /// allowed to look".
    #[test]
    fn a_subvolume_root_is_inode_256() {
        assert_eq!(subvolume_root("256\n"), Some(true));
        assert_eq!(subvolume_root("256"), Some(true));
        // A directory inside one, which is what an installed /var/home
        // was: a real inode, and not that one.
        assert_eq!(subvolume_root("103740"), Some(false));
        // No answer is not an answer. `stat` on a path that is not there
        // prints nothing to stdout, and grading a machine on that would
        // report a fault where there was only a missing probe.
        assert_eq!(subvolume_root(""), None);
        assert_eq!(subvolume_root("stat: cannot stat"), None);
    }

    /// The case that went unchecked: an image declaring no account,
    /// installed onto a disk, where the only record of the account is the
    /// one the installer wrote. A machine installed from the published
    /// image has exactly this shape, and doctor said nothing about the
    /// unit that creates its only account.
    #[test]
    fn the_account_converger_is_graded_on_an_installed_machine_too() {
        let dir = tempfile::tempdir().unwrap();
        let (baked, installed) = (dir.path().join("usr-user"), dir.path().join("var-user"));

        // A published image before its first boot: nothing to converge.
        assert!(!user_sync_has_an_account(&baked, &installed));

        // The same image after `kuma install` wrote the answer.
        std::fs::write(&installed, "KUMA_USER='test'\n").unwrap();
        assert!(user_sync_has_an_account(&baked, &installed));

        // A declaration that names an account, built and booted directly.
        std::fs::remove_file(&installed).unwrap();
        std::fs::write(&baked, "KUMA_USER='declared'\n").unwrap();
        assert!(user_sync_has_an_account(&baked, &installed));

        // Both, which is an installed machine whose image declared one.
        std::fs::write(&installed, "KUMA_USER='test'\n").unwrap();
        assert!(user_sync_has_an_account(&baked, &installed));
    }

    /// The branch that fires when something is actually wrong, which on a
    /// healthy machine never runs. All four states in one tree: a file
    /// left alone, one edited (the /etc/environment trap), one deleted,
    /// and one the image never shipped.
    #[test]
    fn scanning_etc_finds_the_shadowed_and_the_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (image, live) = (dir.path().join("usr"), dir.path().join("live"));
        let put = |root: &Path, rel: &str, body: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };

        put(&image, "etc/greetd/config.toml", "greeter\n");
        put(&live, "etc/greetd/config.toml", "greeter\n");
        put(&image, "etc/environment", "COSMIC_DISABLE_OVERLAY_SCANOUT=1\n");
        put(&live, "etc/environment", "COSMIC_DISABLE_OVERLAY_SCANOUT=1\nEDITED=1\n");
        put(&image, "etc/xdg/mimeapps.list", "defaults\n");
        // no live copy of mimeapps.list: deleted by hand
        put(&live, "etc/local-only.conf", "not the image's\n");

        let paths: Vec<String> = [
            "/etc/greetd/config.toml",
            "/etc/environment",
            "/etc/xdg/mimeapps.list",
            "/etc/local-only.conf",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let scan = scan_etc(&paths, &image, &live);
        assert_eq!(scan.owned, 3, "the local-only file is not the image's to own");
        assert_eq!(scan.shadowed, ["/etc/environment"]);
        assert_eq!(scan.removed, ["/etc/xdg/mimeapps.list"]);
    }

    /// Both answers, since the machine writing this check can only be
    /// one of them and the other would otherwise ship unexercised.
    #[test]
    fn an_encrypted_root_is_told_apart_from_a_plain_one() {
        // A LUKS root, as an ostree deployment reports it: the mapper
        // plus the subvolume in brackets, and the stack under it.
        assert_eq!(
            root_encrypted("/dev/mapper/luks-f5d1fc89[/root]", "crypt\n"),
            Some(true),
            "a crypt layer means the root is inside a container"
        );
        // lsblk prints the whole stack when asked about a partition.
        assert_eq!(root_encrypted("/dev/nvme0n1p3", "part\ncrypt\n"), Some(true));
        // And a plain install, which is the default and must not be
        // reported as anything else.
        assert_eq!(root_encrypted("/dev/sda3[/root]", "part\n"), Some(false));
        assert_eq!(root_encrypted("/dev/vda3", "part\n"), Some(false));
        // LVM is not encryption.
        assert_eq!(root_encrypted("/dev/mapper/fedora-root", "lvm\n"), Some(false));
        // Unanswerable rather than guessed: no findmnt, no lsblk, or a
        // source that is not a device at all.
        assert_eq!(root_encrypted("", "part\n"), None);
        assert_eq!(root_encrypted("/dev/sda3", ""), None);
        assert_eq!(root_encrypted("  ", "  "), None);
    }

    /// An image that ships none of the paths it declares means the check
    /// has nothing to say, not that everything is fine. Saying "0 files,
    /// all good" would be a green light nobody earned.
    #[test]
    fn an_image_owning_nothing_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let scan = scan_etc(
            &["/etc/greetd/config.toml".to_string()],
            &dir.path().join("usr"),
            &dir.path().join("live"),
        );
        assert_eq!(scan.owned, 0);
        assert!(scan.shadowed.is_empty() && scan.removed.is_empty());
    }

    #[test]
    fn the_reachability_wording_says_what_a_dead_phone_needs() {
        let none: Vec<String> = Vec::new();
        assert!(reachability_wording(&none).contains("local-only"));
        assert!(reachability_wording(&["wss://machine.tailnet.ts.net".into()]).contains("tailnet"));
        assert!(
            reachability_wording(&["wss://relay.nip46.com".into()]).contains("public relays"),
            "a public set is named as one"
        );
    }

    #[test]
    fn trust_apps_names_only_the_standing_grants() {
        let apps: serde_json::Value = serde_json::json!({
            "apps": [
                {"pubkey": "npub1basic", "level": "basic"},
                {"pubkey": "npub1trusted", "level": "trust"},
                {"pubkey": "npub1asking", "level": "ask"},
            ]
        });
        assert_eq!(trust_apps(&apps), vec!["npub1trusted".to_string()]);
        assert!(
            trust_apps(&serde_json::json!({})).is_empty(),
            "a broken answer invents no findings"
        );
        assert!(trust_apps(&serde_json::json!({"apps": []})).is_empty());
    }

    #[test]
    fn the_standing_grant_warning_is_one_line_however_many_apps_hold_it() {
        let one = vec!["npub1trusted".to_string()];
        assert_eq!(
            trust_warning_wording(&one),
            "1 app holds Trust: a standing grant, signing without asking: npub1trusted"
        );
        let three: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            trust_warning_wording(&three),
            "3 apps hold Trust: a standing grant, signing without asking: a, b, c"
        );
    }

    #[test]
    fn doctor_json_carries_findings_and_fixes() {
        let findings = vec![
            Finding {
                grade: Grade::Ok,
                name: "disk".into(),
                detail: "41% used, 280G free".into(),
                fix: None,
            },
            Finding {
                grade: Grade::Fail,
                name: "flathub".into(),
                detail: "remote missing".into(),
                fix: Some(Action::new(
                    "add-remote",
                    "sudo flatpak remote-add flathub",
                    "restore the remote",
                )),
            },
        ];
        let json = doctor_json(&findings);
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "1 check(s) failed, 0 warning(s)");
        assert_eq!(json["checks"][0]["grade"], "ok");
        assert_eq!(json["checks"][0]["fix"], serde_json::Value::Null);
        assert_eq!(json["checks"][1]["grade"], "fail");
        assert_eq!(json["checks"][1]["fix"]["cmd"], "sudo flatpak remote-add flathub");
        assert!(json["checks"][1]["fix"]["why"].is_string());
        assert_eq!(json["summary"]["fails"], 1);
        assert_eq!(json["summary"]["warns"], 0);

        // A clean run reads the same shape with the verdict flipped and
        // no error at all: an absent error key is how the document says
        // there is nothing to read.
        let clean = doctor_json(&[]);
        assert_eq!(clean["ok"], true);
        assert!(clean.get("error").is_none());
    }

    /// Doctor must read the policy kuma ships, and the two live in
    /// different modules, so this asserts them against each other rather
    /// than against a copy. Written after doctor reported "not required
    /// to be signed" while standing inside an image that required it.
    #[test]
    fn doctor_reads_the_policy_kuma_actually_ships() {
        let repo = crate::PUBLISHED_IMAGE.rsplit_once(':').unwrap().0;
        let shipped = crate::containerfile::signature_policy();
        assert_eq!(
            signature_key_for(&shipped, repo).as_deref(),
            Some(crate::containerfile::COSIGN_PUB_PATH),
            "doctor cannot find the requirement in kuma's own policy"
        );

        // A repository name is full of slashes, which is exactly what a
        // JSON Pointer treats as structure. Pinned so the fix cannot be
        // undone by a refactor that looks equivalent.
        assert!(repo.contains('/'), "the guard below only means something for a nested name");

        // Fedora's stock policy, and anything else with no rule for kuma.
        let permissive = r#"{"default":[{"type":"insecureAcceptAnything"}]}"#;
        assert_eq!(signature_key_for(permissive, repo), None);

        // A rule for a different repository must not be mistaken for ours.
        let other = r#"{"transports":{"docker":{"quay.io/other/image":
            [{"type":"sigstoreSigned","keyPath":"/k"}]}}}"#;
        assert_eq!(signature_key_for(other, repo), None);

        // Present but not a signature requirement.
        let unsigned = format!(
            r#"{{"transports":{{"docker":{{"{repo}":[{{"type":"insecureAcceptAnything"}}]}}}}}}"#
        );
        assert_eq!(signature_key_for(&unsigned, repo), None);

        assert_eq!(signature_key_for("not json", repo), None);
    }

    /// The whole point of the verb. A report is pasted into a bug tracker
    /// by somebody who will not read it first, so the hash has to be gone
    /// whichever way TOML let them write it.
    #[test]
    fn a_report_never_carries_the_password_hash() {
        for quoted in [
            r#"password_hash = "$6$abcdefgh$0123456789""#,
            r#"password_hash = '$6$abcdefgh$0123456789'"#,
            r#"password_hash="$6$abcdefgh$0123456789""#,
            r#"   password_hash   =   "$6$abcdefgh$0123456789"   "#,
        ] {
            let text = format!("schema_version = 1\n[user]\nname = \"mira\"\n{quoted}\n");
            let out = redact_declaration(&text).expect("a parseable declaration is redactable");
            assert!(!out.contains("$6$abcdefgh$"), "hash survived redaction of: {quoted}");
            assert!(out.contains("<redacted by kuma doctor --report>"));
            // The rest of the declaration is what makes the report useful,
            // so redaction must not be achieved by dropping everything.
            assert!(out.contains("mira"), "redaction ate the declaration: {quoted}");
            assert!(out.contains("schema_version"));
        }
    }

    /// The doc on `redact_declaration` promises that a key holding a
    /// secret cannot slip out by being added somewhere the walk does not
    /// look. It used to promise that while checking one key by name, so
    /// the promise was worth nothing the moment the schema grew. Both
    /// halves are asserted here: the walk reaches nested tables and
    /// inline ones, and the guard omits the whole declaration rather than
    /// publish a secret it failed to reach.
    #[test]
    fn a_report_never_carries_any_key_on_the_secret_list() {
        // Nested table, inline table, and a key that is not the one the
        // original check knew about.
        let text = concat!(
            "schema_version = 1\n",
            "[user]\n",
            "name = \"mira\"\n",
            "password_hash = \"$6$abc$def\"\n",
            "[announce]\n",
            "nsec = \"nsec1verysecret\"\n",
            "[deep.nested]\n",
            "secret = \"do not paste me\"\n",
        );
        let out = redact_declaration(text).expect("a parseable declaration is redactable");
        for leaked in ["$6$abc$def", "nsec1verysecret", "do not paste me"] {
            assert!(!out.contains(leaked), "{leaked} survived redaction");
        }
        assert!(out.contains("mira"), "redaction ate the declaration");

        // And the backstop: a secret the walk cannot reach costs the
        // report a field, never a published secret. An array of tables is
        // a shape this schema does not have and a later one might.
        let unreachable = "schema_version = 1\n[[machines]]\npassword = \"hunter2\"\n";
        assert!(
            redact_declaration(unreachable).is_none_or(|out| !out.contains("hunter2")),
            "a secret the walk missed was published anyway"
        );
    }

    /// containers/image scopes registries.d by prefix, so an entry for
    /// `ghcr.io` governs kuma's repository just as well as one naming it
    /// outright. Testing for the full repository string alone graded a
    /// machine whose broader entry works as `Fail: updates would be
    /// refused`, which sends somebody to fix a machine that is not
    /// broken.
    #[test]
    fn attachments_are_found_under_any_scope_that_covers_the_repo() {
        let repo = crate::published_repo();
        let (registry, _) = repo.split_once('/').unwrap();
        for scope in [repo, registry, repo.rsplit_once('/').unwrap().0, "default-docker"] {
            let text = format!("docker:\n  {scope}:\n    use-sigstore-attachments: true\n");
            assert!(
                sigstore_attachments_cover(&text, repo),
                "a {scope} scope covers {repo} and was not recognised"
            );
        }
        // A neighbour is not a cover, and neither is a scope with no
        // attachments turned on at all.
        assert!(!sigstore_attachments_cover(
            "docker:\n  ghcr.io/somebody-else:\n    use-sigstore-attachments: true\n",
            repo
        ));
        assert!(!sigstore_attachments_cover(&format!("docker:\n  {repo}:\n    x: y\n"), repo));
    }

    /// The file kuma actually ships has to satisfy the check kuma
    /// actually runs. These two live in different modules and are useless
    /// apart, which is how the pair went out broken once already.
    #[test]
    fn doctor_accepts_the_registries_d_file_kuma_ships() {
        assert!(sigstore_attachments_cover(
            &crate::containerfile::registries_d(),
            crate::published_repo()
        ));
    }

    /// bootc nests the digest inside the slot's image object, beside the
    /// reference rather than beside the slot. Read one level too shallow
    /// it is null on every machine, and null is exactly what a local
    /// image that never had a digest looks like, so the field reads as
    /// working. Every other digest lookup in the crate goes through
    /// `<slot>/image/imageDigest`; this one is pinned so the report
    /// cannot drift away from them again.
    #[test]
    fn a_report_reads_the_digest_where_bootc_writes_it() {
        let status = serde_json::json!({"status": {"booted": {"image": {
            "image": {"image": "ghcr.io/letdown2491/kuma:latest"},
            "imageDigest": "sha256:0123456789abcdef0123456789abcdef",
        }}}});
        let out = report_json(&[], Some(&status));
        assert_eq!(
            out.pointer("/machine/booted_digest").and_then(|v| v.as_str()),
            Some("sha256:0123456789abcdef0123456789abcdef"),
        );
        // The reference sits one level shallower than the digest, so a
        // test that only pinned the digest could be satisfied by moving
        // both.
        assert_eq!(
            out.pointer("/machine/booted_image").and_then(|v| v.as_str()),
            Some("ghcr.io/letdown2491/kuma:latest"),
        );
    }

    /// A declaration with no account is the published-image case, and it
    /// must come through whole rather than being treated as suspect.
    #[test]
    fn a_declaration_without_a_hash_is_untouched() {
        let text = "schema_version = 1\n[packages]\nrpm = [\"fish\"]\n";
        let out = redact_declaration(text).unwrap();
        assert!(out.contains("fish"));
        assert!(!out.contains("redacted"));
    }

    /// Fail closed. "Cannot parse" is not a reason to paste a file that
    /// may hold a hash, so the answer is nothing rather than the raw text.
    #[test]
    fn an_unparseable_declaration_is_omitted_rather_than_pasted() {
        let text = "this is not toml = = = \npassword_hash = \"$6$leak\"\n";
        assert!(redact_declaration(text).is_none());
    }

    #[test]
    fn diff_json_shape_is_stable_for_agents() {
        let sections = vec![
            DiffSection { name: "packages.rpm", entries: vec![], skipped: None },
            DiffSection {
                name: "packages.flatpak",
                entries: vec![DiffEntry {
                    change: "add",
                    item: "org.gnome.Loupe".into(),
                    note: "declared, not installed (convergence installs it)".into(),
                }],
                skipped: None,
            },
            DiffSection {
                name: "packages.brew",
                entries: vec![],
                skipped: Some("brew not bootstrapped yet; first boot installs it".into()),
            },
        ];
        let actions = [Action::new("sync", "kuma sync", "converge now")];
        let adhoc_brews = ["jq".to_string()];
        let adhoc_flatpaks = ["io.github.kolunmi.Bazaar".to_string()];
        let json = diff_json(
            Path::new("kuma.toml"),
            &sections,
            &adhoc_brews,
            &adhoc_flatpaks,
            false,
            true,
            &actions,
        );
        assert_eq!(json["config"], "kuma.toml");
        assert_eq!(json["drift"], true);
        // both ad-hoc lists are always present, so an agent can read
        // "nothing of mine is undeclared" from an empty array
        assert_eq!(json["adhoc_brews"][0], "jq");
        assert_eq!(json["adhoc_flatpaks"][0], "io.github.kolunmi.Bazaar");
        // the empty, unskipped section is elided; the skipped one is kept
        assert_eq!(json["sections"].as_array().unwrap().len(), 2);
        assert_eq!(json["sections"][0]["entries"][0]["change"], "add");
        assert_eq!(json["sections"][0]["entries"][0]["item"], "org.gnome.Loupe");
        assert!(json["sections"][1]["skipped"].is_string());
        assert_eq!(json["actions"][0]["cmd"], "kuma sync");
    }

    /// The check run against a sysroot, with its findings held so each
    /// phase of a test can assert and then move on.
    fn run_niri_shadow_check(root: &Path) -> Vec<(Grade, String, String)> {
        let mut found: Vec<(Grade, String, String)> = Vec::new();
        {
            let mut report = |grade: Grade, name: &str, detail: String, _: Option<Action>| {
                found.push((grade, name.to_string(), detail));
            };
            check_niri_shadow_at(root, &mut report);
        }
        found
    }

    /// The extract has to see the binds the image actually ships, and to
    /// skip everything doctor cannot judge: bare names resolve through
    /// the session's PATH, `spawn-sh` lines are shell rather than a
    /// program, and commented lines name nothing.
    #[test]
    fn niri_spawn_targets_find_absolute_programs_only() {
        use crate::containerfile::{NIRI_MEDIA_BINDS, NIRI_MENU_BIND};

        // Every absolute program the image's media binds name must come
        // through, deduplicated. The media binds now go through the
        // shell's bare-name `kuma-shell msg` interface — doctor's blind
        // spot by design — so the record helper is the only absolute
        // path left in the set, and this pins that.
        assert_eq!(niri_spawn_targets(NIRI_MEDIA_BINDS), ["/usr/libexec/kuma-record".to_string()]);
        assert!(niri_spawn_targets(NIRI_MENU_BIND).is_empty());

        let text = "\
            // spawn \"/commented-out/lines/are-invisible\"\n\
            spawn-at-startup \"blueman-applet\"\n\
            spawn-at-startup \"/usr/libexec/kuma-clipboard-bridge\"\n\
            Mod+T hotkey-overlay-title=null { spawn \"kitty\"; }\n\
            Mod+X { spawn-sh \"/usr/bin/true && echo done\"; }\n\
            XF86AudioMute { spawn \"/usr/libexec/kuma-osd\" \"mute\"; }\n";
        assert_eq!(
            niri_spawn_targets(text),
            ["/usr/libexec/kuma-clipboard-bridge".to_string(), "/usr/libexec/kuma-osd".to_string()]
        );
    }

    /// The four states the check can meet. The dead-bind case is the one
    /// that wrote this check: a config copied out of the image before
    /// the last rename, whose volume keys spawned a binary the current
    /// image does not ship, and doctor said nothing while the keys did
    /// nothing.
    #[test]
    fn a_shadowing_niri_config_is_graded_by_what_its_binds_can_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let put = |rel: &str, body: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        };

        // Not a niri machine: the check has nothing to grade and says
        // nothing.
        let found = run_niri_shadow_check(root);
        assert!(found.is_empty());

        put("etc/niri/config.kdl", "the image's config\n");
        put("var/home/me/.config/niri/local.kdl", "");

        // A niri machine with no shadow: the good state, named.
        let found = run_niri_shadow_check(root);
        assert_eq!(
            found,
            vec![(
                Grade::Ok,
                "niri config".into(),
                "the session runs the image's niri config".into()
            )]
        );

        // The image's config running again under a shadow whose every
        // spawn resolves: staleness, not breakage.
        put("usr/bin/blue", "");
        put("var/home/me/.config/niri/config.kdl", "spawn-at-startup \"/usr/bin/blue\"\n");
        let found = run_niri_shadow_check(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Grade::Warn, "{:?}", found[0].2);
        assert!(found[0].2.contains("me's niri config shadows the image's"));

        // The case that matters: a stale copy binding keys to programs
        // this image does not ship.
        put(
            "var/home/me/.config/niri/config.kdl",
            "XF86AudioRaiseVolume { spawn \"/usr/libexec/lares-osd\" \"volume-up\"; }\n",
        );
        let found = run_niri_shadow_check(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, Grade::Fail, "{:?}", found[0].2);
        assert!(found[0].2.contains("/usr/libexec/lares-osd"), "{:?}", found[0].2);

        // A second account with no shadow of its own changes nothing:
        // the check is per account, the grading is per shadow.
        put("var/home/x/.config/niri/local.kdl", "output \"eDP-1\" { scale 1 }\n");
        let found = run_niri_shadow_check(root);
        assert_eq!(found.len(), 1, "only the shadowing account is graded");
    }
}
