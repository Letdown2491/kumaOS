//! kuma's own base, composed from Fedora's package repos.
//!
//! When a declaration names no `system.base`, kuma does not build on
//! fedora-bootc — it composes its own base with `rpm-ostree compose
//! image`, the same tool and building blocks Fedora uses to make
//! fedora-bootc. The compose starts from Fedora's minimal bootc core
//! (bootc + systemd + kernel + dnf, shipped as a manifest inside the
//! fedora-bootc image) and adds only what a real machine needs, so the
//! general-purpose weight fedora-bootc carries — cloud agents,
//! cross-arch emulators, the AWS SDK — never enters, because it was
//! never included. Fedora stays the package source; kuma builds no
//! packages and no kernels.
//!
//! The composed image is content-addressed: its tag embeds a hash of
//! the manifest that produced it, so the Containerfile can name its
//! base deterministically before any compose has run, an unchanged
//! manifest reuses the image already in storage, and a changed one
//! can never silently reuse a stale base (the podman-cache trap the
//! spike hit). The tag lives under `localhost/`, which podman never
//! resolves against a registry — the other spike trap, a pruned local
//! digest turning into an attempted network pull.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::host::{host_output, note, run_host};

/// The environment the compose runs in: it supplies rpm-ostree, the
/// repo definitions, and Fedora's minimal manifest. Package *versions*
/// come from the repos at compose time, not from this image, so its
/// exact age matters little; `kuma update` still pulls it fresh so the
/// repo definitions and the minimal manifest can't go stale forever.
pub const COMPOSE_ENV: &str = crate::config::DEFAULT_BASE;

/// Fedora's minimal bootc manifest, inside COMPOSE_ENV. Its summary is
/// literally "Effectively just bootc, systemd, kernel, and dnf as a
/// starting point": kernel + all hardware modules, bootupd/grub,
/// selinux, tpm2, microcode, dracut, composefs. Maintained by Fedora.
const MINIMAL_MANIFEST: &str = "/usr/share/doc/bootc-base-imagectl/manifests/minimal/manifest.yaml";

/// The per-vendor firmware a base ships by default, so a machine that
/// declares nothing about its hardware still boots with working GPU,
/// wifi, and audio. `system.firmware` trims it to named members.
///
/// **Do not rebuild this list from `dnf repoquery --recommends
/// linux-firmware` alone.** That was how it was first curated, it
/// returns exactly 13 packages, and the list matched them perfectly:
/// the transcription was right and the method was wrong. Intel's wifi
/// and audio firmware are recommended by `linux-firmware` and by
/// nothing else either (`--whatrecommends iwlwifi-mvm-firmware` is
/// empty), because Fedora expects comps groups to name them. The
/// result was a "broad" set with no iwlwifi and no SOF, so every Intel
/// laptop booted a published image with no wireless and no sound, and
/// since installing pulls over the network, could not install either.
/// Found by asking whether the ISO works on hardware that is not the
/// author's; invisible before that, because the one declaration anybody
/// booted pinned `firmware` to three AMD/MediaTek packages.
///
/// So: recommends, PLUS the explicitly-named gaps below. Server NICs,
/// TV tuners and SDRs are deliberately absent; this is consumer
/// hardware coverage, not every blob Fedora packages.
///
/// Curated against Fedora 44. Revisit on release bumps, and when you
/// do, check a real laptop rather than the dependency graph.
pub const FIRMWARE_PACKAGES: &[&str] = &[
    // Named explicitly: nothing recommends these.
    "alsa-sof-firmware",    // audio on essentially every modern Intel/AMD laptop
    "intel-vsc-firmware",   // MIPI webcams on recent Intel laptops
    "iwlwifi-dvm-firmware", // Intel wifi, older generations
    "iwlwifi-mld-firmware", // Intel wifi, newest generations
    "iwlwifi-mvm-firmware", // Intel wifi, the bulk of the last decade
    // Recommends of linux-firmware.
    "amd-gpu-firmware",
    "amd-ucode-firmware",
    "atheros-firmware",
    "brcmfmac-firmware",
    "cirrus-audio-firmware",
    "intel-audio-firmware",
    "intel-gpu-firmware",
    "mt7xxx-firmware",
    "nvidia-gpu-firmware",
    "nxpwireless-firmware",
    "qcom-wwan-firmware",
    "realtek-firmware",
    "tiwilink-firmware",
];

/// The base manifest a declaration composes to. Generated the way the
/// Containerfile is: kuma owns the curation, the declaration only
/// narrows it (firmware). Deterministic — the content tag hashes this.
pub fn manifest(config: &Config) -> String {
    let firmware: Vec<&str> = match &config.system.firmware {
        Some(trim) => {
            // Sorted so two declarations naming the same set in a
            // different order compose (and content-address) the same base.
            let mut named: Vec<&str> = trim.iter().map(String::as_str).collect();
            named.sort_unstable();
            named.dedup();
            named
        }
        None => FIRMWARE_PACKAGES.to_vec(),
    };
    let firmware_lines: String = firmware.iter().map(|pkg| format!("  - {pkg}\n")).collect();
    format!(
        r#"# kuma's base image manifest. Generated by kuma — edit the
# declaration, not this file. Composed from Fedora's package repos with
# rpm-ostree, the same building blocks Fedora uses for fedora-bootc.

metadata:
  summary: kuma base — Fedora minimal bootc core plus hardware enablement

edition: "2024"
recommends: false

# Fedora's minimal core: everything that makes this a valid, bootable,
# updatable bootc image, maintained by Fedora.
include:
  - {MINIMAL_MANIFEST}

packages:
  # networking: minimal has none; a machine is useless without it
  - NetworkManager NetworkManager-wifi wpa_supplicant
  - systemd-resolved

  # firmware: `recommends: false` (from minimal) means linux-firmware
  # alone ships ZERO vendor blobs — every one is named explicitly
  - linux-firmware
{firmware_lines}
  # the archive repo, so older builds stay reachable after mirrors move on
  - fedora-repos-archive

  # base infra minimal omits but any real machine needs
  - shadow-utils    # useradd/usermod; kuma-user-sync converges the account
  - sudo            # the wheel/privilege model every account assumes
  - chrony          # NTP; the desktop enables chronyd.service
  - openssh-server  # remote access; VMs and smoke tests ssh in
  - passwd          # password management
  - ncurses         # clear, tput, reset: ncurses-base ships terminfo and
                    # ncurses-libs the library, but the commands every
                    # interactive shell assumes live in neither
  - cryptsetup      # LUKS unlock at boot
  # plymouth draws the LUKS prompt on encrypted machines (dracut renders it
  # through plymouth when the module is present), so base layer, not desktop:
  # - plymouth: daemon, theme loader, dracut module
  # - plymouth-plugin-script: runs spinner_alt's .script; nothing pulls it in
  # - plymouth-plugin-label: renders Image.Text(); a runtime-dlopened plugin
  #   in its own package, absent it draws nothing while the machine waits
  # - dejavu-sans-fonts: Image.Text() needs a font; the base carries none,
  #   and a missing one means a BLANK password prompt
  # The theme files themselves are baked by the Containerfile (assets/CREDITS.md).
  - plymouth plymouth-plugin-script plymouth-plugin-label dejavu-sans-fonts
  - fwupd           # LVFS firmware updates; security maintenance on real
                    # hardware, and nothing else in the image can do it
"#
    )
}

/// The content-addressed tag the manifest composes to. Pure function of
/// the declaration, so the Containerfile can FROM it before any compose
/// has run, and tests can assert it without touching podman.
pub fn content_tag(config: &Config) -> String {
    let hash = Sha256::digest(manifest(config).as_bytes());
    // 12 hex chars: the tag is a cache key scoped to one machine's
    // podman storage, not a global identity — collision space is "how
    // many manifests has this machine ever had".
    let short: String = hash.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("localhost/kuma-base:m{short}")
}

/// Whether a reference has the exact shape content_tag() mints:
/// `localhost/kuma-base:m` + 12 lowercase hex chars. `kuma clean` prunes
/// only tags matching this, so a user's own base tags are never touched.
pub fn is_content_tag(reference: &str) -> bool {
    reference
        .strip_prefix("localhost/kuma-base:m")
        .is_some_and(|h| h.len() == 12 && h.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
}

pub fn image_exists(reference: &str) -> bool {
    host_output(&["podman", "image", "exists", reference]).is_ok()
}

/// Compose the base for this declaration and tag it with its content
/// tag. Slow (a full depsolve + install from Fedora's repos) but
/// heavily cached: the named volume keeps downloaded packages across
/// composes, and callers skip the whole thing when the tag exists.
pub fn compose(config: &Config, tag: &str) -> Result<()> {
    let dir = tempfile::tempdir().context("cannot create compose directory")?;
    let work = dir.path();
    std::fs::write(work.join("kuma-base.yaml"), manifest(config))
        .context("cannot write base manifest")?;
    // rpm-ostree creates the OCI directory itself but not its parent;
    // without this the compose does all its work and dies at the export.
    std::fs::create_dir(work.join("out")).context("cannot create compose output dir")?;

    note("Composing kuma's base from Fedora's repos (this is the slow, cached step)...");
    let work_str = path_str(work)?;
    // --privileged: rpm-ostree's compose needs to create device nodes
    // and set filecaps; rootless-privileged is still confined to the
    // user's own storage. The package cache lives in a named volume so
    // recomposes don't re-download Fedora.
    run_host(&[
        "podman",
        "run",
        "--rm",
        "--privileged",
        "-v",
        &format!("{work_str}:/work:z"),
        "-v",
        "kuma-compose-cache:/cache",
        COMPOSE_ENV,
        "rpm-ostree",
        "compose",
        "image",
        "--initialize",
        "--format=oci",
        "--source-root=/",
        "--cachedir=/cache",
        "/work/kuma-base.yaml",
        "/work/out/kuma-base",
    ])?;

    // The OCI directory the compose wrote becomes a first-class image in
    // podman storage; `podman pull` prints the image ID last.
    let pulled = host_output(&["podman", "pull", &format!("oci:{work_str}/out/kuma-base")])?;
    let id = pulled
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .context("podman pull printed no image id")?;
    run_host(&["podman", "tag", id, tag])?;
    note(&format!("Composed base ready: {tag}."));
    Ok(())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("non-UTF-8 compose path")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        let config: Config = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn broad_manifest_names_every_vendor_firmware() {
        let out = manifest(&config("schema_version = 1"));
        for pkg in FIRMWARE_PACKAGES {
            assert!(out.contains(&format!("- {pkg}")), "missing {pkg}");
        }
        assert!(out.contains(MINIMAL_MANIFEST));
        assert!(out.contains("recommends: false"));
    }

    /// The test above iterates the list it is checking, so deleting an
    /// entry keeps it green. These names are written out a second time on
    /// purpose, because dropping one of them has already shipped: the set
    /// was curated from `dnf repoquery --recommends linux-firmware`, which
    /// returns neither Intel's wifi nor SOF audio, and every Intel laptop
    /// that booted the published image had no wireless and no sound. It
    /// could not even install, since installing pulls over the network.
    ///
    /// Nobody who develops on AMD hardware will notice this breaking.
    /// That is what the list is for.
    #[test]
    fn the_default_firmware_still_covers_hardware_nobody_here_runs() {
        let out = manifest(&config("schema_version = 1"));
        for pkg in [
            "iwlwifi-mvm-firmware", // Intel wifi, the bulk of the last decade
            "iwlwifi-mld-firmware", // Intel wifi, newest generations
            "iwlwifi-dvm-firmware", // Intel wifi, older generations
            "alsa-sof-firmware",    // audio on essentially every modern laptop
            "intel-gpu-firmware",   //
            "intel-audio-firmware", //
            "intel-vsc-firmware",   // MIPI webcams on recent Intel laptops
            "nvidia-gpu-firmware",  //
            "atheros-firmware",     //
            "brcmfmac-firmware",    // Broadcom wifi, most older Macs
            "realtek-firmware",     //
        ] {
            assert!(out.contains(&format!("- {pkg}")), "{pkg} left the default firmware set");
        }
    }

    /// The composed base must carry plymouth, its two plugins, and a
    /// font. The whole line is asserted rather than each name searched
    /// for, because the names share a manifest line and a bare
    /// `contains("- plymouth")` would keep passing after `plymouth`
    /// itself was deleted, matched by the prefix of
    /// `- plymouth-plugin-script`. Each fails silently when missing:
    /// nothing recommends either plugin, and a base without the script
    /// one falls back to plymouth's text theme while a base without the
    /// label one draws no prompt text at all.
    #[test]
    fn the_composed_base_carries_plymouth_for_the_luks_prompt() {
        let out = manifest(&config("schema_version = 1"));
        assert!(
            out.contains(
                "- plymouth plymouth-plugin-script plymouth-plugin-label dejavu-sans-fonts"
            ),
            "the plymouth set left the base package list"
        );
    }

    #[test]
    fn firmware_trim_narrows_and_is_order_independent() {
        let a = config(
            "schema_version = 1\n[system]\nfirmware = [\"mt7xxx-firmware\", \"amd-gpu-firmware\"]\n",
        );
        let b = config(
            "schema_version = 1\n[system]\nfirmware = [\"amd-gpu-firmware\", \"mt7xxx-firmware\"]\n",
        );
        let out = manifest(&a);
        assert!(out.contains("- amd-gpu-firmware"));
        assert!(!out.contains("- nvidia-gpu-firmware"));
        assert_eq!(content_tag(&a), content_tag(&b));
    }

    #[test]
    fn content_tag_moves_with_the_manifest_and_only_the_manifest() {
        let broad = config("schema_version = 1");
        let trimmed = config("schema_version = 1\n[system]\nfirmware = [\"amd-gpu-firmware\"]\n");
        // Packages layered on top don't touch the base identity.
        let with_rpms = config("schema_version = 1\n[packages]\nrpm = [\"fish\"]\n");
        assert_ne!(content_tag(&broad), content_tag(&trimmed));
        assert_eq!(content_tag(&broad), content_tag(&with_rpms));
        assert!(content_tag(&broad).starts_with("localhost/kuma-base:m"));
    }

    /// The prune in `kuma clean` trusts this shape check to separate
    /// kuma-minted tags from anything a user tagged by hand.
    #[test]
    fn content_tag_shape_is_recognized_and_nothing_else() {
        assert!(is_content_tag(&content_tag(&config("schema_version = 1"))));
        assert!(is_content_tag("localhost/kuma-base:mefa4beb53f41"));
        for not_ours in [
            "localhost/kuma-base:spike3", // hand-named
            "localhost/kuma-base:latest",
            "localhost/kuma-base:mEFA4BEB53F41", // uppercase never minted
            "localhost/kuma-base:mefa4beb53f4",  // 11 hex
            "localhost/kuma-base:mefa4beb53f412", // 13 hex
            "localhost/kuma:latest",
            "quay.io/fedora/fedora-bootc:44",
        ] {
            assert!(!is_content_tag(not_ours), "{not_ours}");
        }
    }
}
