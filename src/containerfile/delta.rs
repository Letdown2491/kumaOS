//! The delta compiler: a declaration, compiled onto a release image.
//!
//! `kuma upgrade` on a machine whose release enters from local builds
//! pulls the published release, reads the declaration baked into it
//! (the example's), diffs it against this machine's, and compiles the
//! DIVERGENCES as a Containerfile layer stack FROM the release digest.
//! The base image keeps its own binaries, units, and identity — the
//! delta carries only what the declaration moves.
//!
//! The diff is declaration-shaped, not field-list-shaped: every field
//! the compiler tolerantly reads gets diffed, and each divergence is
//! routed through the recipe's own generators (the same dnf layer
//! shape, the same preset grammar, the same snapshot templates a full
//! build would use). A field the compiler cannot carry as a delta
//! refuses loudly rather than silently dropping a choice: `[user]`,
//! `[backup]`, CAs, timezone, and hostname ride a full rebuild
//! (`kuma update`), which moves the base pin with them.
//!
//! Tolerance runs one way. Unknown keys at the top level and unknown
//! top-level sections are ignored — this read happens exactly once per
//! release, at the worst moment, and a newer schema must not brick the
//! upgrade. But a section this kuma KNOWS that fails to deserialize (a
//! meaning moved inside a table it diffs on) refuses: half-reading a
//! section whose meaning moved is exactly the silent choice loss the
//! whole-declaration diff exists to prevent. A release whose
//! `schema_version` outruns the compiler's refuses for the same reason.

use super::blocks::{
    dnf_install, dnf_layer, nostrd_exec_line, snapshot_script, snapshot_timer, SNAPSHOT_SERVICE,
};
use super::emit::{self, Content, Emitter};
use crate::config::{self, Config, Desktop};
use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// One release resolution: the moving tag the machine tracks, the
/// digest it held when this delta compiled, and the release image's
/// version label — which the delta re-prints, because a FROM line does
/// not carry labels across and bootupd needs the version on every
/// installed image.
pub(crate) struct Release {
    pub reference: String,
    pub digest: String,
    pub version: String,
}

/// The nostr unit files the base image actually carries, probed at
/// delta time. A 44.9.0-shaped base shipped the layer only when its
/// example enabled it; from 44.10.0 every image carries it inert and
/// the preset decides. The probe is what lets the compiler tell
/// "disabled" from "absent" without version arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BaseUnits {
    pub nostrd: bool,
    pub relay: bool,
    pub serve: bool,
}

/// The release image's baked declaration, tolerantly read: each known
/// section deserialized into the REAL config types (so the diff
/// compares like with like, defaults included). Unknown top-level
/// sections are invisible to this struct; a known section that fails
/// to deserialize (a meaning moved inside it) is the caller's loud
/// refusal.
struct Baked {
    schema_version: u32,
    desktop: Desktop,
    firmware: Option<Vec<String>>,
    shell: Option<String>,
    locale: Option<String>,
    timezone: Option<String>,
    hostname: Option<String>,
    ca_certificates: BTreeMap<String, String>,
    brew_flag: bool,
    packages: config::Packages,
    services: config::Services,
    nostr: config::Nostr,
    snapshots: config::Snapshots,
    // Kept unread on purpose: the tolerant read's parse IS the point —
    // a [user] section whose meaning moved inside fails to deserialize,
    // and that refusal is the delta compiler's duty. The diff never
    // reads the baked value; the declared one is what a machine may
    // carry.
    #[expect(dead_code)]
    user: Option<config::User>,
    backup: config::Backup,
    overrides: BTreeMap<String, config::AppOverride>,
}

impl Baked {
    fn extract(root: &toml::Value) -> Result<Baked> {
        fn section<T: DeserializeOwned>(
            root: &toml::Value,
            key: &str,
            default: impl FnOnce() -> T,
        ) -> Result<T> {
            match root.get(key) {
                None => Ok(default()),
                Some(value) => T::deserialize(value.clone()).with_context(|| {
                    format!(
                        "the release image's baked [{key}] section speaks a schema this \
                         kuma cannot read; the delta compiler refuses rather than guess"
                    )
                }),
            }
        }

        let system = section::<config::System>(root, "system", Default::default)?;
        Ok(Baked {
            schema_version: section(root, "schema_version", || 1)?,
            desktop: system.desktop,
            firmware: system.firmware.clone(),
            shell: system.shell.clone(),
            locale: system.locale.clone(),
            timezone: system.timezone.clone(),
            hostname: system.hostname.clone(),
            ca_certificates: system.ca_certificates.clone(),
            brew_flag: system.brew,
            packages: section(root, "packages", Default::default)?,
            services: section(root, "services", Default::default)?,
            nostr: section(root, "nostr", Default::default)?,
            snapshots: section(root, "snapshots", Default::default)?,
            user: section(root, "user", || None)?,
            backup: section(root, "backup", Default::default)?,
            overrides: section(root, "overrides", Default::default)?,
        })
    }
}

/// Compile the declaration's divergences from the release's baked
/// declaration into a delta plan: Containerfile text plus every file
/// it stages. The caller writes the context — the declaration itself
/// rides as `kuma.toml` (the plan names it through the supplied-file
/// door), and no binary is ever staged: the base image's are newer
/// than anything this kuma could put beside them.
pub(crate) fn compile(
    declared: &Config,
    base_text: &str,
    release: &Release,
    units: BaseUnits,
) -> Result<emit::Plan> {
    let root: toml::Value =
        toml::from_str(base_text).context("the release image's baked declaration is unreadable")?;
    let baked = Baked::extract(&root)?;

    if baked.schema_version > declared.schema_version {
        bail!(
            "the release's declaration speaks schema {} while this kuma writes {}: the \
             delta compiler predates the release. Upgrade kuma first (dnf or the release \
             image), then `kuma upgrade`.",
            baked.schema_version,
            declared.schema_version
        );
    }

    // ---- everything the compiler refuses, collected before it emits ----
    let mut refusals: Vec<String> = Vec::new();
    if declared.system.desktop != baked.desktop {
        refusals.push(format!(
            "  - desktop: the declaration says {:?}, the release image's baked one says \
             {:?}. The release tag follows the declaration's desktop, so this is a \
             release built from a differently-shaped example; `kuma update`.",
            declared.system.desktop, baked.desktop
        ));
    }
    if declared.system.timezone != baked.timezone {
        refusals.push("  - [system] timezone: `kuma update` full-rebuilds".to_string());
    }
    if declared.system.hostname != baked.hostname {
        refusals.push("  - [system] hostname: `kuma update` full-rebuilds".to_string());
    }
    if declared.system.ca_certificates != baked.ca_certificates {
        refusals.push("  - [system] ca_certificates: `kuma update` full-rebuilds".to_string());
    }
    if declared.user.is_some() {
        refusals.push(
            "  - [user]: an account declaration rides the image (hash, keys, autologin), \
             and the delta compiler does not carry it; `kuma update` full-rebuilds"
                .to_string(),
        );
    }
    if declared.backup != baked.backup {
        refusals.push(
            "  - [backup]: the repository secret rides the image with it, and the delta \
             build carries no machine secrets; `kuma update` full-rebuilds"
                .to_string(),
        );
    }
    if declared.nostr.serve != baked.nostr.serve {
        refusals.push(
            "  - [nostr] serve: the tailnet face rides units the delta does not carry; \
             `kuma update` full-rebuilds"
                .to_string(),
        );
    }
    let declared_brew = declared.system.brew || !declared.packages.brew.is_empty();
    let baked_brew = baked.brew_flag || !baked.packages.brew.is_empty();
    if declared_brew != baked_brew {
        refusals.push(
            "  - [system] brew: the machinery (setup unit, git-core, tar) is recipe \
             content the delta does not carry; `kuma update` full-rebuilds"
                .to_string(),
        );
    }
    if !refusals.is_empty() {
        bail!(
            "the delta compiler carries packages, firmware, services, [nostr] arming and \
             relays, shell, locale, snapshots, and the flatpak/brew/override lists. Your \
             declaration moves more than that:\n{}\n\n\
             `kuma update` full-rebuilds from the declaration — it moves the base pin, so \
             everything the release carries rides along too.",
            refusals.join("\n")
        );
    }

    // ---- the plan ----
    let mut e = Emitter::new(declared);
    e.raw(&format!(
        "\n\
         # A delta build: the declaration compiled onto the release by kuma {}.\n\
         # The release image ({}) carries its own binaries, units, and identity;\n\
         # this layer carries only what the declaration moves. `kuma update` is\n\
         # the full-build road.\n",
        crate::KUMA_VERSION,
        release.reference,
    ));
    // FROM by digest: the release is what the machine shares with the
    // fleet, and the digest is what `upgrade --check` compared. The
    // reference's tag is dropped (`name@digest` is the shape every tool
    // agrees on); the pull already happened against it.
    e.raw(&format!("FROM {}\n", crate::lock::pinned_ref(&release.reference, &release.digest)));

    // Labels. The version label is the RELEASE's (the machine reports
    // the release it runs on, never the compiler's number — branding
    // stays the base's for the same reason), the builder label stays
    // the true generator, and io.kuma.delta records that a delta
    // compiled the image, which is what keeps the stale-build probe
    // honest: under a delta the builder label is always older than the
    // running kuma, and that is not staleness.
    e.raw("\n");
    e.raw("LABEL io.kuma.image=\"1\"\n");
    e.raw(&format!("LABEL io.kuma.builder=\"{}\"\n", crate::VERSION));
    e.raw(&format!("LABEL io.kuma.delta=\"{}\"\n", crate::KUMA_VERSION));
    e.raw(&format!("LABEL org.opencontainers.image.version=\"{}\"\n", release.version));

    // The declaration rides again: every later read of the baked config
    // (doctor, the next upgrade's routing, install) must speak for THIS
    // machine, not the release example's.
    let declaration = e.supplied("kuma.toml");
    e.copy(&declaration, "/usr/lib/kuma/kuma.toml");

    // ---- packages: the set difference, both directions ----
    let add = diff_sets(&declared.packages.rpm, &baked.packages.rpm);
    let remove = diff_sets(&baked.packages.rpm, &declared.packages.rpm);
    // Removals first: the install's dependency resolution should see
    // the world the declaration describes, not the one plus what is
    // leaving.
    if !remove.is_empty() {
        e.raw(&dnf_layer(&format!("dnf -y remove {}", remove.join(" "))));
    }
    if !add.is_empty() {
        e.raw(&dnf_install(&add.join(" ")));
    }

    // ---- firmware: the effective set, both directions ----
    // None means the broad set on both sides, so an untrimmed
    // declaration delta-building from a trimmed release installs the
    // trim back — the declaration wins, and "leave it alone" means the
    // broad set, not "whatever the release example chose".
    let broad = || crate::compose::FIRMWARE_PACKAGES.iter().map(|s| s.to_string()).collect();
    let declared_fw: BTreeSet<String> =
        declared.system.firmware.clone().unwrap_or_else(broad).into_iter().collect();
    let baked_fw: BTreeSet<String> =
        baked.firmware.clone().unwrap_or_else(broad).into_iter().collect();
    let fw_remove: Vec<String> = baked_fw.difference(&declared_fw).cloned().collect();
    let fw_add: Vec<String> = declared_fw.difference(&baked_fw).cloned().collect();
    if !fw_remove.is_empty() {
        e.raw(&dnf_layer(&format!("dnf -y remove {}", fw_remove.join(" "))));
    }
    if !fw_add.is_empty() {
        e.raw(&dnf_install(&fw_add.join(" ")));
    }

    // ---- shell and locale ----
    if declared.system.shell != baked.shell {
        if let Some(shell) = &declared.system.shell {
            // The same assert the full build bakes: the shell package
            // rides the rpm delta above, and a declaration naming a
            // shell no package provides fails here, at build.
            e.raw(&format!("RUN test -x /usr/bin/{shell}\n"));
        }
    }
    if declared.system.locale != baked.locale {
        match &declared.system.locale {
            Some(locale) => {
                if let Some(lang) = super::langpack(locale) {
                    e.raw(&dnf_install(&format!("glibc-langpack-{lang}")));
                }
                e.raw(&format!("RUN echo 'LANG={locale}' > /etc/locale.conf\n"));
            }
            // The release's example pinned a locale and the declaration
            // does not: take the file back and the base default
            // (C.UTF-8) stands again.
            None => e.raw("RUN rm -f /etc/locale.conf\n"),
        }
    }

    // ---- snapshots: the recipe's own templates, re-rendered ----
    // Snapshot artifacts are declaration-rendered (script, timer, and a
    // constant unit), so a divergence re-emits the whole block from
    // this kuma's templates — the same bytes a full build by this kuma
    // would bake. The converger scripts the base already carries stay
    // untouched; only the declaration-shaped files move.
    if declared.snapshots != baked.snapshots {
        // The door needs the owning block: the timer is a kuma unit,
        // and its installer-media disposition lives in the snapshots
        // block's units table.
        let block = super::blocks::BLOCKS
            .iter()
            .find(|b| b.name == "snapshots")
            .expect("the snapshots block is in the registry");
        e.enter_block(block);
        e.raw("\n");
        if declared.snapshots.enable {
            let snapshot = e.stage("kuma-snapshot", snapshot_script(declared));
            let service = e.stage("kuma-snapshot.service", SNAPSHOT_SERVICE);
            let timer =
                e.stage("kuma-snapshot.timer", snapshot_timer(&declared.snapshots.interval));
            e.raw(&dnf_install("btrfs-progs"));
            e.copy_exec(&snapshot, "/usr/libexec/kuma-snapshot");
            e.copy(&service, "/usr/lib/systemd/system/kuma-snapshot.service");
            e.copy(&timer, "/usr/lib/systemd/system/kuma-snapshot.timer");
            e.enable(&["kuma-snapshot.timer"]);
        } else {
            // The base enabled the timer and the declaration does not:
            // take the units back out. rm over recipe-owned paths — the
            // same paths this block's enable side just names.
            e.raw(
                "RUN systemctl disable kuma-snapshot.timer \\\n    \
                 && rm -f /usr/lib/systemd/system/kuma-snapshot.service \\\n    \
                         /usr/lib/systemd/system/kuma-snapshot.timer \\\n    \
                         /usr/libexec/kuma-snapshot\n",
            );
        }
    }

    // ---- nostr: one preset file, one drop-in ----
    // Enablement rides /usr/lib/systemd/user-preset/50-kuma-nostr.preset
    // (units and binaries ship in every base from 44.10.0 on), so the
    // on/off flip is a one-file delta: rewrite the preset, apply it.
    // A custom relay set is a drop-in overriding the unit's ExecStart —
    // the sandbox around it is the base's, and evolves with the base.
    if declared.nostr != baked.nostr {
        if declared.nostr.enable && !units.nostrd {
            bail!(
                "the declaration arms [nostr] but the release image ships no nostr layer \
                 ({} predates units-always releases). `kuma update` full-rebuilds once; \
                 after that the layer is always present and upgrades delta it.",
                release.reference
            );
        }
        if declared.nostr.enable && declared.nostr.relay.enable && !units.relay {
            bail!(
                "the declaration arms the local relay but the release image ships no \
                 nip46-relay unit; `kuma update` full-rebuilds once, and the relay rides \
                 every later release."
            );
        }
        let armed_nostrd = declared.nostr.enable;
        let armed_relay = declared.nostr.enable && declared.nostr.relay.enable;
        let baked_nostrd = baked.nostr.enable;
        let baked_relay = baked.nostr.enable && baked.nostr.relay.enable;
        if (armed_nostrd, armed_relay) != (baked_nostrd, baked_relay) {
            // Explicit both ways for every unit the base carries: a
            // preset with no opinion leaves a unit's existing
            // enablement standing, and the relay must not survive its
            // own disarm by silence.
            let mut lines: Vec<String> = Vec::new();
            let mut preset_units: Vec<&str> = Vec::new();
            if units.nostrd {
                lines.push(format!(
                    "{} kuma-nostrd.service",
                    if armed_nostrd { "enable" } else { "disable" }
                ));
                preset_units.push("kuma-nostrd.service");
            }
            if units.relay {
                lines.push(format!(
                    "{} nip46-relay.service",
                    if armed_relay { "enable" } else { "disable" }
                ));
                preset_units.push("nip46-relay.service");
            }
            let preset = e.stage("50-kuma-nostr.preset", lines.join("\n") + "\n");
            e.raw("\n");
            e.copy(&preset, "/usr/lib/systemd/user-preset/50-kuma-nostr.preset");
            let block = super::blocks::BLOCKS
                .iter()
                .find(|b| b.name == "nostr")
                .expect("the nostr block is in the registry");
            e.enter_block(block);
            e.preset_global(&preset_units);
        }
        // The relay set, when the daemon is armed at all: an ExecStart
        // drop-in restating the line with the declaration's own relays.
        // The arg grammar is this kuma's; the daemon is the release's —
        // if a release renames a flag, the drop-in says the old word to
        // it and the bunker fails at boot, loudly, on the machine that
        // declared the road. The version guard catches the binary's
        // side of that on the next build.
        if declared.nostr.enable {
            let declared_line = nostrd_exec_line(&declared.nostr);
            let baked_line = nostrd_exec_line(&baked.nostr);
            if declared_line != baked_line {
                let dropin = e.stage(
                    "10-kuma-nostr-relays.conf",
                    format!("[Service]\nExecStart=\nExecStart={declared_line}\n"),
                );
                e.copy(
                    &dropin,
                    "/usr/lib/systemd/user/kuma-nostrd.service.d/10-kuma-nostr-relays.conf",
                );
            }
        }
    }

    // ---- services: the set difference, both directions ----
    // User-declared units, not kuma's — the same raw shape the
    // services block uses, because they have no installer-media
    // disposition to be held to.
    let declared_enable: BTreeSet<String> = declared.services.enable.iter().cloned().collect();
    let baked_enable: BTreeSet<String> = baked.services.enable.iter().cloned().collect();
    let declared_disable: BTreeSet<String> = declared.services.disable.iter().cloned().collect();
    let baked_disable: BTreeSet<String> = baked.services.disable.iter().cloned().collect();
    // To enable: newly declared, plus anything the base explicitly
    // disabled that the declaration no longer disables.
    let mut enable: Vec<String> = declared_enable.difference(&baked_enable).cloned().collect();
    enable.extend(baked_disable.difference(&declared_disable).cloned());
    let mut disable: Vec<String> = declared_disable.difference(&baked_disable).cloned().collect();
    disable.extend(baked_enable.difference(&declared_enable).cloned());
    enable.sort();
    enable.dedup();
    disable.sort();
    disable.dedup();
    if !enable.is_empty() || !disable.is_empty() {
        let mut cmds: Vec<String> = Vec::new();
        if !enable.is_empty() {
            cmds.push(format!("systemctl enable {}", enable.join(" ")));
        }
        if !disable.is_empty() {
            cmds.push(format!("systemctl disable {}", disable.join(" ")));
        }
        e.raw(&format!("\nRUN {}\n", cmds.join(" && ")));
    }

    // ---- the converged lists: re-baked, never installed ----
    // Flatpaks, brews, and overrides are runtime state; the image
    // carries the LISTS so convergence targets the declaration. The
    // delta re-bakes them and nothing else — the converger scripts and
    // units are the base's, newer than this compiler's.
    if declared.packages.flatpak != baked.packages.flatpak {
        let mut list = declared.packages.flatpak.join("\n");
        if !list.is_empty() {
            list.push('\n');
        }
        let flatpaks = e.stage("flatpaks", list);
        e.raw("\n");
        e.copy(&flatpaks, "/usr/lib/kuma/flatpaks");
    }
    if declared_brew && declared.packages.brew != baked.packages.brew {
        let mut list = declared.packages.brew.join("\n");
        if !list.is_empty() {
            list.push('\n');
        }
        let brews = e.stage("brews", list);
        e.raw("\n");
        e.copy(&brews, "/usr/lib/kuma/brews");
    }
    if declared.overrides != baked.overrides {
        // Both stores, always — the base's converger treats absence as
        // "nothing to do", and the same always-both rule applies to a
        // delta: an emptied override set has keys to take back.
        let mut stores: Vec<(String, Content)> = Vec::new();
        for scope in [config::Scope::System, config::Scope::User] {
            let apps: Vec<(String, Content)> = declared
                .overrides
                .iter()
                .filter(|(_, over)| over.scope == scope)
                .map(|(app, over)| (app.clone(), Content::Text(crate::overrides::render(over))))
                .collect();
            stores.push((scope.as_str().to_string(), Content::Tree(apps)));
        }
        let overrides = e.stage_tree("overrides", stores);
        e.raw("\n");
        e.copy(&overrides, "/usr/lib/kuma/overrides");
    }

    e.reconcile();
    Ok(e.finish())
}

/// Write the delta build's context: the compiled Containerfile, the
/// declaration it carries, and every file the plan stages. No binaries
/// ride — a delta build COPYs nothing from beside the running kuma,
/// because the base image's are newer than anything this kuma could
/// stage; the declaration is the context's one supplied file.
pub(crate) fn write_context(
    config: &Config,
    base_text: &str,
    release: &Release,
    units: BaseUnits,
    config_text: &str,
    dir: &std::path::Path,
) -> Result<()> {
    let plan = compile(config, base_text, release, units)?;
    std::fs::write(dir.join("kuma.toml"), config_text)?;
    std::fs::write(dir.join("Containerfile"), &plan.text)?;
    emit::materialize(&plan.files, dir)
}

/// Set difference, sorted: what is in `declared` and not in `base`.
fn diff_sets(declared: &[String], base: &[String]) -> Vec<String> {
    let base: HashSet<&String> = base.iter().collect();
    let mut out: Vec<String> = declared.iter().filter(|p| !base.contains(*p)).cloned().collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(toml: &str) -> Config {
        let config: Config = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        config
    }

    fn release() -> Release {
        Release {
            reference: "ghcr.io/letdown2491/kumaos:niri".to_string(),
            digest: "sha256:aaaabbbbccccddddaaaabbbbccccddddaaaabbbbccccddddaaaabbbbccccdddd"
                .to_string(),
            version: "44.10.0".to_string(),
        }
    }

    /// The release example's shape, as 44.10.0's baked declaration
    /// reads: niri, fish, snapshots on, nostr present-but-inert.
    const BASE: &str = "schema_version = 1\n\
                        [system]\n\
                        desktop = \"niri\"\n\
                        shell = \"fish\"\n\
                        \n\
                        [packages]\n\
                        rpm = [\"fish\", \"git-core\", \"distrobox\", \"tailscale\"]\n\
                        \n\
                        [snapshots]\n\
                        enable = true\n";

    fn units() -> BaseUnits {
        BaseUnits { nostrd: true, relay: true, serve: false }
    }

    fn compile_ok(declared_toml: &str, base: &str) -> emit::Plan {
        compile(&declared(declared_toml), base, &release(), units()).unwrap()
    }

    #[test]
    fn an_identical_declaration_compiles_from_plus_labels_only() {
        let plan = compile_ok(
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             shell = \"fish\"\n\
             [packages]\n\
             rpm = [\"git-core\", \"fish\", \"distrobox\", \"tailscale\"]\n\
             [snapshots]\n\
             enable = true\n",
            BASE,
        );
        // Order-insensitive list compare: the rpm lists name the same
        // packages in a different order, so no dnf layer at all.
        assert!(!plan.text.contains("dnf "));
        assert!(plan.text.contains("FROM ghcr.io/letdown2491/kumaos@sha256:aaaabbbb"));
        assert!(plan.text.contains("LABEL org.opencontainers.image.version=\"44.10.0\""));
        assert!(plan.text.contains("LABEL io.kuma.delta="));
        // The declaration always rides.
        assert!(plan.text.contains("COPY kuma.toml /usr/lib/kuma/kuma.toml"));
        assert!(plan.files.is_empty());
    }

    #[test]
    fn a_package_delta_installs_and_removes() {
        let plan = compile_ok(
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             shell = \"fish\"\n\
             [packages]\n\
             rpm = [\"fish\", \"htop\"]\n\
             [snapshots]\n\
             enable = true\n",
            BASE,
        );
        let text = &plan.text;
        // Removals first, installs after: the dependency resolution
        // should see the destination world.
        let rm = text.find("dnf -y remove").expect("removal layer");
        let add = text.find("dnf -y install").expect("install layer");
        assert!(rm < add);
        assert!(text.contains("dnf -y remove distrobox git-core tailscale"));
        // The install rides the keepcache flag the dnf layer shape
        // carries; assert the whole shape, verb to package.
        assert!(text.contains("dnf -y install --setopt=keepcache=1 htop"));
    }

    #[test]
    fn firmware_flips_both_ways_against_a_trimmed_release() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 shell = \"fish\"\n\
                 firmware = [\"amd-gpu-firmware\"]\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            // The release trimmed to one package the declaration does
            // not name, and the declaration names one the release does
            // not carry.
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             firmware = [\"iwlwifi-mld-firmware\"]\n",
            &release(),
            units(),
        )
        .unwrap();
        assert!(plan.text.contains("dnf -y remove iwlwifi-mld-firmware"));
        assert!(plan.text.contains("dnf -y install --setopt=keepcache=1 amd-gpu-firmware"));
    }

    #[test]
    fn untrimmed_declaration_reinstalls_a_trimmed_release() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             firmware = [\"amd-gpu-firmware\"]\n",
            &release(),
            units(),
        )
        .unwrap();
        // None means the broad set: the release trimmed to
        // amd-gpu-firmware, the declaration names nothing, so nothing
        // is removed and the rest of the broad set comes back. (The
        // snapshot enable below is an incidental delta of this fixture;
        // the firmware install is the image's FIRST install layer.)
        assert!(!plan.text.contains("dnf -y remove"));
        let install = plan
            .text
            .lines()
            .find(|l| l.contains("dnf -y install"))
            .expect("the firmware install layer");
        assert!(install.contains("amd-ucode-firmware"), "{install}");
        // The one package the release kept is already there.
        assert!(!install.contains("amd-gpu-firmware"), "{install}");
    }

    #[test]
    fn nostr_flip_writes_a_preset_and_applies_it() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n\
                 [nostr]\n\
                 enable = true\n\
                 [nostr.relay]\n\
                 enable = true\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap();
        assert!(plan.text.contains(
            "COPY 50-kuma-nostr.preset /usr/lib/systemd/user-preset/50-kuma-nostr.preset"
        ));
        assert!(plan.text.contains("RUN systemctl --global preset kuma-nostrd.service"));
        let preset = plan.files.get("50-kuma-nostr.preset").unwrap();
        assert_eq!(
            *preset,
            Content::Text("enable kuma-nostrd.service\nenable nip46-relay.service\n".to_string())
        );
    }

    #[test]
    fn nostr_off_against_an_armed_base_disarms_explicitly() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            // The base armed both; the declaration (and its defaults)
            // arm neither.
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             [nostr]\n\
             enable = true\n\
             [nostr.relay]\n\
             enable = true\n",
            &release(),
            units(),
        )
        .unwrap();
        let preset = plan.files.get("50-kuma-nostr.preset").unwrap();
        assert_eq!(
            *preset,
            Content::Text("disable kuma-nostrd.service\ndisable nip46-relay.service\n".to_string())
        );
    }

    #[test]
    fn a_custom_relay_set_is_an_execstart_dropin() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n\
                 [nostr]\n\
                 enable = true\n\
                 relays = [\"wss://myrelay.example\"]\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap();
        let dropin = plan.files.get("10-kuma-nostr-relays.conf").expect("the drop-in rides");
        let Content::Text(text) = dropin else {
            panic!("the drop-in is text");
        };
        assert!(text.starts_with("[Service]\nExecStart=\nExecStart=/usr/bin/kuma-nostrd"));
        assert!(text.contains("--relay wss://myrelay.example"));
        // The base's road stays in the unit; the drop-in only restates.
        assert!(!text.contains("--relay wss://relay.nip46.com"));
    }

    #[test]
    fn services_delta_enables_and_disables_both_directions() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [services]\n\
                 enable = [\"tailscaled.service\", \"podman.socket\"]\n",
            ),
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             [services]\n\
             enable = [\"cockpit.socket\"]\n\
             disable = [\"podman.socket\"]\n",
            &release(),
            units(),
        )
        .unwrap();
        let line = plan
            .text
            .lines()
            .find(|l| l.starts_with("RUN systemctl"))
            .expect("the services delta line");
        // podman.socket: the base disabled it and the declaration
        // enables it — one word of direction, not two.
        assert!(line.contains("systemctl enable podman.socket"));
        assert!(line.contains("systemctl disable cockpit.socket"));
        // tailscaled is newly declared, so it is in the enable half too.
        assert!(line.contains("tailscaled.service"));
    }

    #[test]
    fn a_divergent_refusal_field_names_itself_and_the_way_out() {
        let err = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 timezone = \"Europe/Berlin\"\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("timezone"), "{msg}");
        assert!(msg.contains("kuma update"), "{msg}");
    }

    #[test]
    fn a_user_section_refuses_even_when_the_base_has_none() {
        let err = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n\
                 [user]\n\
                 name = \"builder\"\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("[user]"));
    }

    #[test]
    fn unknown_top_level_sections_are_ignored() {
        // A 45.0.0-shaped base with a section this kuma has never heard
        // of: the upgrade goes through, because the read happens once,
        // at the worst moment.
        let plan = compile_ok(
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             shell = \"fish\"\n\
             [packages]\n\
             rpm = [\"fish\"]\n\
             [snapshots]\n\
             enable = true\n",
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             shell = \"fish\"\n\
             [packages]\n\
             rpm = [\"fish\"]\n\
             [snapshots]\n\
             enable = true\n\
             [a-new-section]\n\
             key = \"value\"\n",
        );
        assert!(plan.text.contains("FROM ghcr.io/"));
    }

    #[test]
    fn an_unknown_key_inside_a_known_section_refuses() {
        // Different from a whole new section: [system] grew a key this
        // kuma's type rejects, which means the section's MEANING moved
        // — the compiler would half-read the rest of it. Refusal, not
        // guesswork.
        let err = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             future_feature = true\n",
            &release(),
            units(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("[system]"));
    }

    #[test]
    fn a_newer_schema_version_refuses() {
        let err = compile(
            &declared("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "schema_version = 2\n[system]\ndesktop = \"niri\"\n",
            &release(),
            units(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("schema 2"));
    }

    #[test]
    fn a_moved_section_meaning_refuses_loudly() {
        // [services] gained a key this kuma's type rejects: the section
        // deserialize fails and the compiler says which section moved.
        let err = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            "schema_version = 1\n\
             [system]\n\
             desktop = \"niri\"\n\
             [services]\n\
             enable = []\n\
             future_field = true\n",
            &release(),
            units(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("[services]"));
    }

    #[test]
    fn snapshot_interval_move_reemits_the_timer() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n\
                 interval = \"daily\"\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap();
        assert!(plan.text.contains("COPY --chmod=755 kuma-snapshot /usr/libexec/kuma-snapshot"));
        assert!(plan
            .text
            .contains("COPY kuma-snapshot.timer /usr/lib/systemd/system/kuma-snapshot.timer"));
        assert!(plan.files.contains_key("kuma-snapshot.timer"));
    }

    #[test]
    fn flatpak_and_override_lists_rebake_without_installing() {
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [packages]\n\
                 flatpak = [\"org.mozilla.firefox\"]\n\
                 [snapshots]\n\
                 enable = true\n\
                 [overrides.\"org.mozilla.firefox\"]\n\
                 sockets = [\"x11\"]\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap();
        assert!(plan.text.contains("COPY flatpaks /usr/lib/kuma/flatpaks"));
        assert!(plan.text.contains("COPY overrides /usr/lib/kuma/overrides"));
        // No install layer at all: the lists are not packages. (The
        // rpm removal from the base's example list rides, and only
        // that.)
        assert!(!plan.text.contains("dnf -y install"));
        // Both stores ride, system and user, whatever the declared
        // scope — the base's converger treats absence as nothing to do.
        let Content::Tree(stores) = plan.files.get("overrides").unwrap() else {
            panic!("the overrides stage is a tree");
        };
        assert_eq!(stores.len(), 2);
    }

    #[test]
    fn brew_machinery_appearance_refuses() {
        let err = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 brew = true\n\
                 [snapshots]\n\
                 enable = true\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("brew"));
    }

    #[test]
    fn an_absent_nostr_layer_in_the_base_refuses_arming() {
        let err = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 [snapshots]\n\
                 enable = true\n\
                 [nostr]\n\
                 enable = true\n",
            ),
            BASE,
            &release(),
            // A 44.9.0-shaped base: the example never enabled nostr,
            // so the units are simply not there.
            BaseUnits { nostrd: false, relay: false, serve: false },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("ships no nostr layer"));
    }

    /// The delta golden: one representative declaration onto one
    /// representative base, pinned byte for byte — the layer order,
    /// the label spell, the preset grammar. The two labels that name
    /// the compiler are normalized (they move with every commit, same
    /// as the full-build goldens' builder line).
    #[test]
    fn the_delta_containerfile_is_byte_stable() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/containerfile/goldens");
        let golden = dir.join("niri-delta.Containerfile");
        let plan = compile(
            &declared(
                "schema_version = 1\n\
                 [system]\n\
                 desktop = \"niri\"\n\
                 shell = \"fish\"\n\
                 firmware = [\"amd-gpu-firmware\", \"iwlwifi-mld-firmware\"]\n\
                 locale = \"de_DE.UTF-8\"\n\
                 [packages]\n\
                 rpm = [\"fish\", \"htop\"]\n\
                 flatpak = [\"org.mozilla.firefox\"]\n\
                 [services]\n\
                 enable = [\"podman.socket\"]\n\
                 [snapshots]\n\
                 enable = true\n\
                 interval = \"daily\"\n\
                 [nostr]\n\
                 enable = true\n\
                 relays = [\"wss://relay.example\"]\n",
            ),
            BASE,
            &release(),
            units(),
        )
        .unwrap();
        let out = normalize_compiler(&plan.text);
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&golden, &out).unwrap();
            return;
        }
        let expected = normalize_compiler(&std::fs::read_to_string(&golden).unwrap_or_default());
        assert_eq!(out, expected, "the delta golden moved; see the test's doc");
    }

    /// Both compiler labels normalized: the builder stamp moves with
    /// every commit and the delta label with every version bump, and
    /// neither is about the declaration.
    fn normalize_compiler(text: &str) -> String {
        text.lines()
            .map(|l| match l {
                _ if l.starts_with("LABEL io.kuma.builder=") => {
                    "LABEL io.kuma.builder=<version-and-commit>".to_string()
                }
                _ if l.starts_with("LABEL io.kuma.delta=") => {
                    "LABEL io.kuma.delta=<compiler-version>".to_string()
                }
                _ => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }
}
