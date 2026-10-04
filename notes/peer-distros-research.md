# Peer distros and the bootc ecosystem — research for the 45.0.0 plan

Status: research only, nothing implemented. Sources: GitHub repos and release notes,
official docs sites, Fedora change pages, fetched 2026-09-26 (four parallel passes:
vanilla Atomic, Universal Blue/Bluefin/Aurora, Bazzite, adjacent projects + bootc
plumbing). Companion to [fedora-45-rebase.md](fedora-45-rebase.md), which covers the
package-level deltas of the F45 rebase itself; this file covers what the ecosystem
around us is doing and what is worth adopting, avoiding, or deliberately differing
from. Everything here is input to the 45.0.0 plan, not commitments.

Verdict: the rebase doc's section 3 (frozen bootc-image-builder) is now confirmed as
the ecosystem's consensus problem — every peer has already left that container, by
three different routes, and all three routes are documented and production-proven
(§1). The most actionable cheap items are convergence-timer guardrails (§3) and
install-provenance metadata (§7); greenboot-rs turned out to be a non-issue for F45
itself (§2), and kuma's QEMU e2e already covers what the peer testsuite advertises
(§9). The big-ticket items — sealed images (§6), NVIDIA akmods in-image (§8),
SBOM/attestation publishing (§4), and the BIB exit for disks and installer media
(§1) — are decisions, not work.

## 1. Everyone has left bootc-image-builder — three successor paths, all in production

- Bazzite moved its ISO to **titanoboa** (Zeglius's GH action from
  ondrejbudai/bootc-isos): a lorax-based *live installer* that boots a real desktop,
  with Anaconda for the install itself
  (https://github.com/ublue-os/bazzite/blob/main/installer/build.sh,
  https://github.com/ublue-os/bazzite/blob/main/.github/workflows/build_iso.yml).
  Their legacy ISOs survive solely because the new one cannot do manual partitioning
  yet (https://docs.bazzite.gg/General/Installation_Guide/install-guide/).
- BlueBuild's `generate-iso` wraps **JasonN3/build-container-installer** — Anaconda,
  builds from a published image or straight from a recipe
  (https://blue-build.org/how-to/generate-iso/,
  https://github.com/JasonN3/build-container-installer/).
- Fedora's own Atomic Desktops switched ISOs from lorax to **image-builder**
  (osbuild) in F45, gained qcow2 + raw artifacts, and lorax is being retired
  deliverable-by-deliverable (https://fedoraproject.org/wiki/Changes/BuildAtomicDesktopsWithImageBuilder,
  FESCo #3545). The BIB lineage continues inside osbuild/image-builder (v75–v83
  restore the anaconda-iso/def paths), but the CLI and def formats are still moving.
- Common thread: nobody treats the frozen `quay.io/centos-bootc/bootc-image-builder:latest`
  pin as a future. Kuma's rebase doc already carries the decision (digest-pin now,
  migrate later). The new information is that the Anaconda-based live-installer
  pattern (titanoboa / build-container-installer) is the best-trodden path to an ISO
  that installs *kuma's* partitioning rather than bootc's — and that "live ISO boots
  the actual desktop" is a UX bar peers now meet.
- **Corrected on review of the tree (2026-09-26): kuma already ships the destination.**
  `kuma iso --live` builds a live ISO whose root IS the kuma image, boots a
  try-before-installing desktop, and installs from the live session by pulling the
  recorded image (liveiso.rs:1-36; LIVE_SOURCE at :64-76). It implements the same
  "container-native ISO contract v0.1.0" titanoboa implements, assembled directly
  (liveiso.rs:23-30) — the shape Bazzite's Dakota and Bluefin's unified ISO are only
  now alpha-ing toward. Releases attach this ISO (release.yml:19, asset
  `kuma-x86_64.iso`), so BIB's remaining surface is two things: the `kuma vm` qcow2
  disk (main.rs:2287-2300) and the legacy default `kuma iso` (BIB anaconda-iso,
  main.rs:2306) which strangers do not download. The "which installer path" decision
  therefore collapses to: exit BIB for qcow2 (loop + kuma's own install path), digest-pin,
  and decide whether the legacy anaconda-iso default earns its keep when its manual-
  partitioning value is nil against kuma's fixed three-partition model.

## 2. greenboot shell is deprecated; greenboot-rs is the bootc-native successor (status warning)

- The shell implementation is deprecated in favor of **greenboot-rs**; the RPM
  `greenboot >= 0.16.0` is built from the Rust rewrite, "designed for bootc based
  systems" (https://github.com/fedora-iot/greenboot,
  https://github.com/fedora-iot/greenboot-rs).
- Kuma ships `greenboot` explicitly, declines `greenboot-default-health-checks` (DNS
  false-negatives), copies `50-kuma-greeter.sh` into
  `/usr/lib/greenboot/check/required.d/`, and converges the GRUB boot_counter
  snippet via `kuma-boot-health-sync` (src/containerfile/mod.rs:511-518, goldens).
  That is the load-bearing automatic-rollback mechanism, and the greeter check is
  required.d — exactly the interface the rewrite promises to keep.
- F45 packaging state (checked 2026-09-26, packages.fedoraproject.org): F45 ships
  **greenboot-0.15.8-4.fc45** — still the shell implementation; the 0.16/greenboot-rs
  line has not landed in Fedora yet. So the F45 rebase needs **no** greenboot work:
  the package, the required.d path, and the boot_counter machinery kuma builds on
  are unchanged. The migration is an F46+ tracking item, not a 45.0.0 item: when
  Fedora bumps to greenboot-rs, re-verify `/usr/lib/greenboot/check/required.d/`,
  `boot-complete.target`, and the custom.cfg convergence `kuma-boot-health-sync`
  performs. Greenboot-rs also carries watchdog-grace-period logic kuma's own
  boot-health path may want to mirror then (don't blame an update for a watchdog
  reboot).
- Confidence: deprecation and design confirmed from the repos; F45 version
  confirmed from the package index.

## 3. Convergence and update timers: the uupd pattern + bootc's staging verbs

- uupd (uBlue's Go updater: bootc + flatpak + distrobox + brew in one shot) gates
  its run on **battery ≥ 20%, CPU ≤ 50%, mem ≤ 90%, and a per-run network byte
  budget**, with `RandomizedDelaySec=15m`, `Persistent=true`, `Wants=network-online.target`,
  and `Restart=on-failure` to survive wake-from-suspend races
  (https://github.com/ublue-os/uupd).
- Corrected on review of the tree (2026-09-26): kuma's timers already carry
  `Persistent=true` and `RandomizedDelaySec` (flatpak sync blocks.rs:565-575,
  backup :815, snapshots :986), the service orders after network-online with
  retries and low CPU/IO weights (blocks.rs:543-560), and permissions converge
  boot-only by design (blocks.rs:492-511). The **remaining gap is guardrails only**:
  the daily timer that carries flatpak/brew installs fires on a sleeping laptop's
  catch-up whether it is on battery or metered. uupd's battery threshold and
  network byte budget — or the GNOME/COSMIC metered-connection flag — are the
  adoptable pieces, and they are config/script work inside the existing units.
- **Metered-connection awareness** is the user-facing pause switch uBlue documents
  (GNOME Settings metered flag pauses updates) — niri/COSMIC equivalents worth a
  look (https://docs.projectbluefin.io/administration/).
- bootc 1.16 added a **staged-update lifecycle**: `upgrade --check`,
  `--download-only`, `--from-downloaded`, `--apply`, with staged-but-unapplied
  deployments discarded at reboot while the image data stays cached
  (https://github.com/bootc-dev/bootc/blob/main/docs/src/upgrades.md,
  https://developers.redhat.com/articles/2026/02/18/control-updates-download-only-mode-bootc).
  "Fetch at night, apply on reboot" is now a first-class bootc pattern and maps
  cleanly onto kuma's existing update/switch split.
- Aurora **retired its `stable-daily` stream** (May 2026) citing maintenance burden —
  a caution if kuma ever grows published channels (https://docs.getaurora.dev/blog/retiring-stable-daily).

## 4. Supply chain: SBOMs, attestations, verification commands in release notes

- Bluefin release pages ship a four-step verification (cosign keyless → oras SBOM →
  SBOM attestation → slsa-verifier Build L2) and "variants promoted" digest tables;
  images carry a Syft SPDX SBOM attached as an OCI referrer and cosign-signed;
  GitHub `actions/attest-build-provenance` supplies SLSA provenance
  (https://github.com/projectbluefin/bluefin/releases,
  https://docs.projectbluefin.io/supply-chain/).
- cosign is at v3.x with `.sigstore.json` bundles and SPDX SBOM assets on releases
  (https://github.com/sigstore/cosign/releases/tag/v3.1.3).
- uBlue verifies *base images* by cosign inside the build (digest-pinned
  `image-versions.yml`), not just at consumption time
  (https://raw.githubusercontent.com/ublue-os/bluefin/main/Justfile).
- Kuma already signs releases (cosign verify-blob in SECURITY.md). The deltas worth
  considering: embed verification commands in release notes, attach an SBOM to the
  ISO/image artifacts, and digest-pin + verify anything kuma COPYs from a registry
  (the BIB pin is the known case; rebase doc §3 already mandates digest-pinning).

## 5. Update-size work: rechunk/chunkah, zstd:chunked, unified storage (experimental)

- uBlue still rechunks images to flatten layers for resumable pulls; the tooling
  moved to **chunkah** (https://github.com/coreos/chunkah,
  https://github.com/ublue-os/image-template). Fedora is moving pushes to
  zstd:chunked (https://gitlab.com/fedora/bootc/tracker/-/issues/9).
- Caveats that matter only if kuma ever publishes chunked images: zstd:chunked
  pulls **fail on bootc's experimental composefs backend** today
  (https://github.com/bootc-dev/bootc/issues/2408), and unified storage
  (sharing the container store with podman) is still experimental and blocked on
  containers/container-libs#144
  (https://github.com/bootc-dev/bootc/blob/main/docs/src/experimental-unified-storage.md).
- For kuma's local-first model the practical win is smaller base-image pulls for
  daily rebuilds (`kuma update`); nothing to adopt until kuma publishes images, at
  which point chunked layers + a rechunk pass are the recipe peers use.

## 6. Sealed images — the long-term verified-boot direction (test-stage)

- siosm's "sealed Atomic desktops" test images combine systemd-boot + signed UKIs
  embedding `composefs.digest=` with fs-verity composefs root: a verified
  firmware→kernel→root chain enabling TPM passwordless disk unlock
  (https://tim.siosm.fr/blog/2026/04/28/sealed-atomic-desktops-test-images/,
  https://github.com/travier/fedora-atomic-desktops-sealed). bootc's experimental
  composefs backend + UKI flow is the plumbing
  (https://bootc-dev.github.io/bootc/experimental-composefs.html).
- Churn warning: the EROFS digest format flip-flopped across bootc 1.16.x and the
  on-disk format is unstable — track, don't adopt
  (https://github.com/bootc-dev/bootc/blob/main/docs/src/experimental-composefs.md).
- Relevance to kuma: cosign-signed releases + an image digest that *is* the boot
  record is exactly kuma's self-describing principle carried into firmware. Nothing
  to do for 45; worth a line in the contract's "what can still move".

## 7. Install provenance: `.bootc-aleph.json` (cheap, fits kuma's principles)

- bootc writes `.bootc-aleph.json` at the physical root recording the source image
  ref+digest, timestamps, bootc/kernel versions
  (https://github.com/bootc-dev/bootc/blob/main/docs/src/bootc-install.md).
- Kuma already records resolution in `kuma.lock` and the image carries its
  declaration; adding install-time provenance (declaration hash, install date, kuma
  version, base digest) is a small, contract-compatible addition to
  `kuma doctor`/`kuma diff` self-description. Also adoptable: bootc's
  `/usr/lib/bootc/kargs.d` + install config drop-ins as the image-side
  counterpart to kuma's machine-side settings split
  (https://github.com/bootc-dev/bootc/blob/main/docs/src/bootc-install.md).

## 8. Hardware enablement: three patterns, kuma's is one of them

- **akmods prebuilt into images** (uBlue): NVIDIA closed + open, zfs, xone/xpadneo,
  wl — cached as OCI images consumed via `COPY --from=`, per-kernel-version,
  secure-boot-signed with a MOK key users enroll at first boot
  (https://github.com/ublue-os/akmods). Fedora Atomic's own team answers NVIDIA
  questions with "use Bazzite/Aurora" (https://tim.siosm.fr/blog/2026/04/28/fedora-atomic-desktops-44/).
  Kuma's nouveau-only stance is a documented, deliberate difference ("Not yet" in
  README); the adoptable middle path would be an *optional* `[packages].akmods`
  lane that bakes akmods at build time from those caches — it violates nothing in
  the contract and converts kuma's loudest missing feature into a declaration. The
  counter-argument: it drags kernel-version coupling and MOK/secure-boot enrollment
  into kuma's promise surface. A decision, not work.
- **Per-device runtime allowlists** (Bazzite): one image + checked-in device lists
  (`steamos-manager-hardware`, `powerstation-hardware`) enable quirks per machine
  (https://github.com/ublue-os/bazzite/blob/main/system_files/desktop/shared/usr/libexec/hwsupport/steamos-manager-hardware).
  Relevant only if kuma grows device-tuned desktops; kuma's firmware trim
  (`[system].firmware`) already covers the build-time half.
- **Support tiers as a doc** (uBlue borrows Homebrew's): publish which hardware is
  Tier 1/2/3 to set expectations (https://docs.projectbluefin.io/installation/) —
  pairs with kuma's honest Status section.

## 9. Testing: corrected — kuma already has the e2e this described

- **Wrong on first write, and worth recording why.** The research flagged uBlue's
  testsuite (behave/Gherkin headless-Wayland desktop tests in QEMU, screenshots
  attached to releases, promotion gated on e2e:
  https://github.com/projectbluefin/testsuite) as the single biggest quality lever
  kuma was missing. Reading kuma's own CI before publishing the claim shows the
  e2e already exists and covers more: `boot` installs to a real disk and boots
  with the greeter verdict via greenboot over virtio-gpu/llvmpipe, `install`
  covers the btrfs-only paths on a disk kuma itself partitioned, `hibernate`
  asserts the same boot_id across resume under Secure Boot, `dead-disk` destroys
  a disk and restores it, and `iso` boots the live ISO from serial console
  (.github/workflows/ci.yml:209-485, scripts/smoke.sh). The claim was generated
  from peer sources without checking home first.
- What genuinely remains after the correction, none of it load-bearing:
  screenshots attached to release artifacts (cosmetic evidence for humans, vs
  kuma's console-log artifacts), and COSMIC boot coverage — a deliberate,
  documented policy (ci.yml:221-232), not an oversight.

## 10. Adjacent declarative projects — lessons and validation

- **BlueBuild** (Rust CLI, recipe → Containerfile → OCI): still has **no runtime
  convergence**; on-device verbs are switch/upgrade/rebase only, and the promised
  convergence UI ("Workshop") is a WIP Tauri app with no release
  (https://github.com/blue-build/cli, https://github.com/blue-build/workshop).
  Validation: kuma's boot-time convergence + `kuma capture` drift proposals remain
  unique in this space. Their module system (OCI-addressable script modules) is
  the interesting bit if kuma ever grows third-party block types.
- **Vanilla OS 3 "Reunion" shipped** (2026-08-24; ABRoot v2 dual-root on LVM-thin,
  updates as OCI images, local package changes generate local OCI layers via
  `abroot pkg`, reproducible build goal) (https://github.com/Vanilla-OS/live-iso/releases/tag/3.0,
  https://github.com/Vanilla-OS/ABRoot). Lesson: "local deltas as OCI layers over a
  pinned base" is a representation kuma.lock could grow into; reimplementing
  deployment/rollback (as ABRoot did) is a maintenance pit kuma avoids by riding
  bootc.
- **openSUSE transactional-update 5.0**: replaced overlayfs /etc layering with
  per-snapshot /etc subvolumes + explicit boot-time sync — a second ecosystem
  (after bootc's own 3-way merge) concluding that **union overlays for /etc are
  the wrong tool** (https://github.com/openSUSE/transactional-update/blob/master/NEWS).
  Kuma's merge-not-replace /etc story is on the right side of that history.
- **blendOS v5 alpha**: `tracks` as URLs — the declaration points at a remote
  definition of distro+DE, switchable one line
  (https://ruds.io/posts/blendOS-v5-distro-switching). Interesting idea; alpha
  quality; kuma's `system.base` already covers the practical subset.

## 11. Ecosystem status warnings (things peers already migrated off)

- bootc-image-builder: archived/frozen (rebase doc §3; §1 above).
- greenboot shell implementation: deprecated → greenboot-rs (§2).
- `bazzite-arch` distrobox image: **archived** (2026-03); the pattern moved to
  generic distrobox containers declared via `distrobox-assemble` ini files — a
  declarative-container convention kuma could ship for dev containers
  (https://github.com/ublue-os/bazzite-arch,
  https://docs.bazzite.gg/Installing_and_Managing_Software/Distrobox/).
- bootc releases weekly from main, no branches/backports — pin bootc per kuma
  release and expect to bump it deliberately, not via Fedora mass-rebuild
  (https://github.com/bootc-dev/bootc/blob/main/RELEASES.md).
- systemd sysext/confext: explicitly unsupported on bootc systems — do not build
  kuma features on them (https://github.com/bootc-dev/bootc/pull/2436).

## 12. What this suggests for 45.0.0 (and what waits)

Cheap, contract-compatible, rebase-adjacent:
1. ~~Add the F45 greenboot-rs packaging check to the rebase test plan~~ — corrected:
   F45 still ships shell greenboot (0.15.8); nothing to do. Track greenboot-rs for
   F46+ (§2).
2. Convergence-timer guardrails: battery + metered/network-capacity gating inside
   the existing install-carrying timer (§3) — the only genuinely missing piece;
   timer semantics already exist.
3. Adopt bootc's `--download-only` staging for `kuma update` if trivially
   expressible through the existing bootc invocations (§3).
4. Digest-pin the frozen BIB image and record the freeze date (already decided in
   the rebase doc; the ecosystem evidence for doing it is now unanimous, §1).
5. Install-provenance file at install time (§7) — small, self-describing win.
6. ~~Unattended QEMU e2e boot test~~ — corrected: kuma's CI already installs, boots,
   hibernates, restores, and boots the live ISO (§9).

Decisions (not 45.0.0 blockers, worth scheduling discussion):
7. Optional akmods lane vs staying nouveau-only (§8).
8. ~~ISO strategy follow-up~~ — corrected (§1): the live installer is already the
   release artifact. Remaining: exit BIB for qcow2 (loop + kuma's own install path),
   digest-pin, and decide whether the legacy anaconda-iso default earns its keep.
   The qcow2 exit has a consistency payoff the peers don't get: the CI install job's
   comment (ci.yml:264-273) records that boot-job disks are ext4 BIB output, so
   btrfs-only paths go unbooted — VM disks built by kuma's own installer would make
   the daily boot job exercise the real layout.

Track only: sealed images/composefs backend (§6), chunked-layer publishing (§5),
SBOM/attestation publishing (§4), distrobox-assemble declaration support (§11).
