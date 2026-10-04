# Fedora 45 rebase — impact research

Status: research only, nothing implemented. Sources: Fedora change proposals (the
discussion.fedoraproject.org announcement threads, which mirror the bot-protected
wiki pages), GitHub release notes, and — for package-level facts — the Fedora 45
Beta Everything repo itself (mirrors.kernel.org, fetched 2026-09-18/19, plus two
RPM payloads downloaded and unpacked). Fedora 45 Beta shipped 2026-09-15, GA is
targeted October 2026, so anything below marked beta-stage can still move before
GA. Kernel 7.2 and GNOME 51 are out of scope (not deltas: F44 already ships 7.2;
kuma ships niri/COSMIC).

Verdict: the 45 rebase is a small code change with one decision attached. Exactly
two things break outright, and both are cheap: (1) Fedora 45 moved vendor PAM
stacks out of `/etc/pam.d/` — greetd's and cosmic-greeter's among them — which
breaks both halves of kuma's keyring assert in every desktop build
(src/containerfile/blocks.rs:359-363, goldens confirm the pinned strings), and
(2) bootc-image-builder, whose container kuma pins by tag, was archived on
2026-06-18 and merged into `osbuild/image-builder`; the pinned `:latest` has been
frozen since that date (verified via skopeo). The oo7 secrets change is the
highest-visibility F45 change but turns out to be mostly a decision rather than a
breakage: `gnome-keyring` and `gnome-keyring-pam` are still packaged in F45
Beta, and F45's greetd PAM stack still calls `pam_gnome_keyring.so` — it merely
gained `pam_oo7.so` lines alongside. Podman 6.1, the RPM signature-enforcement
default, and the dnf5 vendor-change default all deserve a smoke test against
kuma's specific invocations but showed no confirmed breakage. dracut, bootc,
kmscon, and the Anaconda changes are verify-only.

## 1. PAM stacks moved from /etc to /usr/lib — the keyring assert breaks outright (confirmed)

- F45's greetd package (greetd-0.10.3-10.fc45) no longer ships
  `/etc/pam.d/greetd`. The payload now lives at `/usr/lib/pam.d/greetd` (verified
  by unpacking the F45 Beta RPM; F44's greetd-0.10.3-6.fc44 owns
  `/etc/pam.d/greetd` on this machine). Same for cosmic-greeter
  (cosmic-greeter-1.6.0-3.fc45 ships `/usr/lib/pam.d/cosmic-greeter`, nothing in
  `/etc/pam.d/`). This is part of F45's broader /usr-vs-/etc relocation push
  (repo configs to /usr, PAM stacks to /usr/lib/pam.d); Linux-PAM (1.7.2-4.fc45)
  falls back to the vendor dir when /etc has no file.
- Kuma asserts `test -f /usr/lib64/security/pam_gnome_keyring.so` and
  `grep -q pam_gnome_keyring /etc/pam.d/{service}` — blocks.rs:361, emitted for
  greetd at blocks.rs:2896 and cosmic-greeter at blocks.rs:3010; the goldens pin
  the exact strings (goldens/niri.Containerfile:54-55,
  goldens/cosmic.Containerfile:12-13). On F45 the grep target does not exist, so
  every niri and COSMIC build fails loudly at build time. That is the assert
  doing its job; the fix is the grep path, not the removal of the assert.
- Content-wise the F45 stacks are good news: they KEEP
  `-auth optional pam_gnome_keyring.so` and
  `-session optional pam_gnome_keyring.so auto_start` and ADD
  `-auth optional pam_oo7.so` / `-session optional pam_oo7.so auto_start`
  (verified in both RPM payloads). So the greeter unlock path kuma asserts on
  still exists as of Beta.
- Fix: make `keyring_pam()` grep the right file —
  `grep -q pam_gnome_keyring /etc/pam.d/$service /usr/lib/pam.d/$service`
  (grep -q on multiple files succeeds if either matches), or resolve the path at
  runtime. Update both goldens. Worth a comment in blocks.rs noting the file may
  live in either directory depending on release.
- Confidence: confirmed by primary artifact (RPM payload inspection), not
  beta-stage inference.

## 2. oo7 becomes the default secrets provider — a decision, not a breakage (change confirmed; behavior beta-stage)

- Change proposal (system-wide, approved, in F45 Beta):
  https://fedoraproject.org/wiki/Changes/oo7_Secrets_Service_Provider —
  announcement thread with the full scope:
  https://discussion.fedoraproject.org/t/f45-change-proposal-oo7-secrets-service-provider-system-wide/195274
  (FESCo ticket #3640). Scope: package `oo7-daemon`, the PAM module, and support
  code; "Add oo7 PAM module to relevant PAM configs where gnome-keyring and
  kwallet PAM modules are listed"; "Adjust comps to replace gnome-keyring with
  oo7-daemon"; "Adjust dependencies in desktops to use oo7 instead of
  gnome-keyring". Release notes claim automatic migration of existing GNOME
  Keyring/KWallet data. Nothing in the scope retires the gnome-keyring packages,
  and the F45 Beta repo still carries `gnome-keyring-50.0-6.fc45` and
  `gnome-keyring-pam-50.0-6.fc45` — both remain installable and the F45 greetd
  PAM stack still calls the module (section 1).
- What oo7 is (upstream, now under linux-credentials:
  https://github.com/linux-credentials/oo7): a Rust org.freedesktop.secrets
  server (`oo7-daemon`), an xdg-desktop-portal Secret backend (`oo7-portal`), a
  secret-tool replacement (`oo7-cli`), and its own PAM module `pam_oo7.so`.
  Its PAM module is NOT pam_gnome_keyring-compatible — it is a different design:
  the user's oo7-daemon listens on `$XDG_RUNTIME_DIR/oo7/pam.sock` and the PAM
  module sends the login password over that socket during auth/session
  (https://github.com/linux-credentials/oo7/blob/main/pam/README.md). It wants to
  be on all three stacks (auth, session with auto_start, password — the last
  handles keyring re-encryption on password change). F45 packages it separately:
  `pam_oo7-0.7.0~alpha-1.fc45`, `oo7-daemon-0.7.0~alpha-1.fc45` (versions
  alpha-stage — flag for GA re-check).
- What kuma does today: installs `gnome-keyring` + `gnome-keyring-pam`
  explicitly on niri and COSMIC (blocks.rs:34, 39 — weak-deps-off installs,
  nothing else pulls them) and asserts module + stack (section 1). Both survive
  F45 as-is.
- What has to be decided/verified in kuma:
  - If both gnome-keyring-daemon and oo7-daemon run, they compete for
    org.freedesktop.secrets — upstream notes the daemon must not run alongside
    gnome-keyring-daemon (oo7 README). F45 desktop dependencies shifting to oo7
    means a kuma image can end up with both daemons installed and the winner
    determined by bus-name ordering. Pick a lane: either stay
    gnome-keyring-explicit (then verify oo7-daemon is not dragged in by
    something kuma ships, and that the greeter unlock still works), or switch
    the desktop blocks to `oo7-daemon` + `pam_oo7` and assert
    `/usr/lib64/security/pam_oo7.so` and the pam_oo7 lines instead. The
    goldens/greeter check machinery (blocks.rs:359-363) is exactly where the
    switch lands.
  - Beta-stage migration quality: a Silverblue 44→45-beta upgrader reports the
    automatic gnome-keyring→oo7 migration failing
    (https://discussion.fedoraproject.org/t/issues-with-credentials-on-fedora-45-beta/202421).
    Fresh installs (kuma's case) are less exposed, but the oo7 stack is 0.7.0
    alpha — treat "secrets work under greetd" as a first-boot smoke test on the
    45 rebase, whichever lane is picked.
  - Non-GNOME greeter compatibility: the F45 greetd stack shipping pam_oo7
    lines is Fedora's own statement that pam_oo7 under greetd is a supported
    combination; the module is `optional`-prefixed, so a not-running daemon
    degrades to a locked keyring, never a failed login.
- Confidence: change scope and package existence confirmed from primary sources;
  runtime behavior of oo7 under greetd is beta-stage (alpha upstream version,
  one migration bug report).

## 3. bootc-image-builder is archived; the pinned image is frozen (confirmed)

- osbuild/bootc-image-builder was archived 2026-06-18, "merged into
  https://github.com/osbuild/image-builder" (README banner on the archived
  repo). kuma pins `quay.io/centos-bootc/bootc-image-builder:latest`
  (src/main.rs:77) and uses it for qcow2 disks (main.rs:2280) and
  `anaconda-iso` installer media (main.rs:2336), extracting installer defs from
  `/usr/share/bootc-image-builder/defs/fedora-*.yaml` (main.rs:2317-2333).
- skopeo reports the quay tag's Created as **2026-06-18T11:31Z** — the pinned
  `:latest` is the last image built before the archive. It still exists and
  still works, but it will never see another update: no newer Anaconda, no new
  fedora def files, no fixes. The def kuma lifts and re-mounts under the
  image's own name (main.rs:2315-2333) comes from that frozen tree.
- The successor is alive and carries the BIB lineage: releases v75–v83 of
  osbuild/image-builder (July–September 2026) show "Restore anaconda-iso/iso
  type to bootc-image-builder" (v76), "enable building the bootc-image-builder
  container" (v77, with a `bib_legacy` code path), bootc ISO tests on aarch64
  (v82), and continued def-format work ("defs: allow templating filesystem
  partition labels" v83, "drop the ImageTypeYAML property from defs.imageType"
  v83). The merged project's own README documents a new, different CLI
  (`image-builder build --distro fedora-43 <type>`, container
  `ghcr.io/osbuild/image-builder-cli`) for the package-based flow, with BIB's
  container-input flow living on inside it.
- Fedora itself moved to this family: "Build Atomic Desktops disk images with
  image-builder" (https://discussion.fedoraproject.org/t/f45-change-proposal-build-atomic-desktops-disk-images-with-image-builder-systemwide/179631)
  switches Atomic Desktop ISOs from lorax to image-builder and adds qcow2/raw
  artifacts; Fedora Magazine's beta announcement confirms it shipped in Beta.
  That is Fedora's build pipeline, not BIB's CLI, but it tells you where the
  maintenance attention is.
- What has to change in kuma: at minimum pin the existing quay image by digest
  (`:latest` on a frozen repo is a lie waiting to be discovered) and record the
  freeze date in the const comment at main.rs:77. The real decision is the
  migration: track the merged repo's BIB container (rebuilt from v77 onward)
  or the image-builder CLI, and re-verify the def-file path kuma extracts
  (`/usr/share/bootc-image-builder/defs/`) against whichever container kuma
  adopts — the def format is still being changed upstream (v83). None of this
  blocks the 45 rebase; none of it survives long either.
- Confidence: confirmed by primary sources (archive banner, release notes,
  skopeo measurement).

## 4. Podman 6.1 — big jump, no confirmed breakage in kuma's verb set (versions confirmed; behavior per release notes)

- F44 ships podman 5.8.7 (this machine); F45 Beta ships `podman-6.1.0-2.fc45`
  with `containers-common-0.69.0-1.fc45` (Beta repo listing). Fedora Magazine's
  beta announcement names Podman 6 as an F45 headline. 6.0.0 breaking changes:
  https://github.com/podman-container-tools/podman/releases/tag/v6.0.0
- What was removed and why kuma doesn't care: slirp4netns and `--network-cmd-path`
  (kuma never sets them; pasta is already the default), CNI and iptables
  (netavark/nftables only), cgroups v1 hosts, Intel Macs. None of kuma's
  invocations reference any of these.
- kuma's verbs, checked against the 6.0.0/6.1.0 notes: `podman pull`,
  `image inspect --format` (incl. `{{len .RepoTags}}`), `image exists`,
  `image prune -f`, `rmi`, `tag`, `rm --force`, `manifest inspect`
  (src/lock.rs:323), `images -f dangling=true -q` (src/inspect.rs:2624),
  `ps -a --external` — none appear in any breaking or behavior-change entry.
  The 6.0 breaking list is about volume prune/list filter semantics, quadlet
  layout, machine providers, and image-ID future-proofing — not these.
- `--pull=never` on `podman run` (src/main.rs:1602, load-bearing and
  test-pinned): no change in the 6.0.0 notes; the flag is not mentioned in any
  breaking entry. Beta-stage verify: run the existing os-release test against a
  podman 6 host.
- The one real surface is configuration parsing: 6.0 rewrote how
  containers.conf/storage.conf/registries.conf are read
  (https://github.com/podman-container-tools/podman/blob/main/contrib/design-docs/config-file-parsing.md).
  New model: the "main" file is first-match-wins across
  `$XDG_CONFIG_HOME` > `/etc/containers` > `/usr/share/containers`, then
  `.conf.d` drop-ins from all three are merged in lexicographic order;
  storage.conf's `rootless_storage_path` is deprecated; `podman info` no longer
  prints the storage.conf path. Kuma writes a complete
  `/etc/containers/storage.conf` into the live ISO (src/liveiso.rs:171) and
  mounts one into the partitioning container (src/partition.rs:822). Under the
  new rules `/etc/containers/storage.conf` still outranks everything as the
  main file, so a full-file write remains authoritative — the semantics to
  re-verify on 6.1 are (a) that a partial kuma storage.conf can no longer
  inherit from a distro-shipped `/usr/share/containers/storage.conf` main file
  (it can't; /etc wins) and (b) that nothing in the container-libs 0.69
  defaults moved under kuma's feet.
- Rootless networking defaults did not change in 6.0 (rootlessport stays the
  default forwarder; pasta-as-forwarder is opt-in experimental), so `podman
  build`/`run` inside kuma's workflows have no new default to absorb.
- Confidence: versions confirmed from the Beta repo; "no verb changes" is
  release-notes-negative (absence of evidence) — treat the verb smoke test in
  the test plan as the actual check.

## 5. RPM signature enforcement at the rpm level — verify the RPM Fusion URL install (mechanism confirmed; build-time behavior beta-stage)

- Change: https://fedoraproject.org/wiki/Changes/Enforcing_signature_checking_by_default
  / https://discussion.fedoraproject.org/t/f45-change-proposal-enforcing-signature-checking-by-default-systemwide/169774
  — mechanism is a one-liner in rpm: `%_pkgverify_level` default `digest` →
  `all`, i.e. packages need a verified signature AND digest or rpm refuses
  (override: `--nosignature`). Deferred from F44, live in rawhide since
  2026-02-16 in rpm >= 6.0.1-5; F45 ships rpm 6.1 (Magazine test-days roundup:
  https://fedoramagazine.org/test-days-for-fedora-45-help-us-test-the-big-changes/).
- The change text says systems using "official or 3rd party repositories" are
  unaffected — dnf/dnf5 has enforced repo signatures all along and imports
  repo keys. The exposed corner is @commandline packages: kuma installs the
  RPM Fusion free-release RPM by URL inside the build container
  (src/containerfile/blocks.rs:311). That RPM is signed with RPM Fusion's key,
  which is not in Fedora's keyring; whether the non-interactive
  `dnf -y install <url>` still auto-imports it under the enforced default is
  the question. dnf5 grew a `gpgcheck_policy` config (5.4.3.0) and per-element
  verify-level plumbing, but its `--no-gpgchecks` integration with rpm's
  verify level was still an open issue when the change landed
  (https://github.com/rpm-software-management/dnf5/issues/2479, linked from the
  change's Dependencies section). Nothing upstream says the URL install breaks;
  nothing primary says it doesn't.
- What to do in kuma: no code change up front. Add the test-plan check; if it
  fails, the fix is to import the RPM Fusion key before the URL install
  (distribution-gpg-keys ships it, or `rpmkeys --import` the key from the
  rpmfusion site) rather than reaching for --nosignature.
- Adjacent, same rebase: "Disable Vendor Change by Default" sets
  `allow_vendor_change = False` in dnf5's distro defaults
  (`/usr/share/dnf5/libdnf.conf.d/20-fedora-defaults.conf`, per FESCo ticket
  https://forge.fedoraproject.org/fesco/tickets/issues/3643; proposal thread
  https://discussion.fedoraproject.org/t/f45-change-proposal-disable-vendor-change-by-default-system-wide/195269).
  Kuma's mesa-freeworld step swaps Fedora's `mesa-va-drivers` for RPM Fusion's
  `mesa-va-drivers-freeworld` via `--allowerasing` (blocks.rs:311) — that is an
  obsoletion-plus-erase rather than a same-name vendor switch, so it should be
  out of scope of the vendor policy, but dnf5 5.4.3.0 also added
  `--[no-]allow-vendor-change` and warns when upgrades are skipped by the
  restriction; verify the install still resolves on F45.
- `update --check`'s repoquery (src/updates.rs:135-148) is query-side only —
  neither change touches it. F45 Beta ships dnf5 5.4.3.0, an older minor than
  F44's current 5.4.5.0, so no new repoquery behavior arrives with the rebase;
  5.4.5.0's "repoquery: Report when no repositories are enabled" may arrive via
  updates later
  (https://github.com/rpm-software-management/dnf5/releases).
- Confidence: mechanism confirmed from the change thread; the RPM Fusion URL
  install question is explicitly unresolved — beta-stage test required.

## 6. kmscon replaces the fbcon VT console — low exposure (change confirmed; kuma path unaffected)

- Change: https://fedoraproject.org/wiki/Changes/UseKmsconVTConsole /
  https://discussion.fedoraproject.org/t/f45-change-proposal-usekmsconvtconsole-systemwide/172602
  (delayed from F44, shipped in F45 Beta). kmscon-10.0.3-2.fc45 (+ -gl/-pango
  subpackages) is installed by default and `autovt@.service` is repointed to
  `kmsconvt@.service`, so switching to a VT gets a userspace terminal instead
  of the kernel's fbcon. The proposal states plainly: fbcon stays compiled
  into the kernel, the boot process (including LUKS password fallback) is
  unaffected, and failure falls back to getty/fbcon.
- Kuma's exposure is the `console=ttyS0,115200 console=tty0` kargs in the live
  ISO grub entries (src/liveiso.rs:470, 475). Those are kernel-console
  selections, orthogonal to which userspace terminal emulator renders the VT:
  the serial console path never touches kmscon, and tty0 still reaches the
  kernel console with fbcon available behind it. Plymouth's boot-console
  interaction is explicitly out of the change's blast radius per the proposal
  text ("won't affect ... the boot process").
- Known regressions from the change thread (all pre-beta testing on F43,
  2025-11): a 100%-CPU spin traced to an SELinux AVC on
  netlink_kobject_uevent (policy fix identified), nvidia-open quirks, and font
  resize glitches; also "starting graphical applications from the VT (startx,
  sway-from-tty) needs kmscon-launch-gui" because DRM master is exclusive —
  irrelevant to kuma, whose sessions are started by greetd, not from a VT.
- What to do in kuma: nothing in code. Test-plan items: serial-console boot of
  the live ISO, and one VT-switch check on an installed 45 machine. If a
  machine's VT is broken by kmscon, the revert is "remove kmscon" (contingency
  in the proposal).
- Confidence: change text confirmed; F45-beta regression reports for the
  shipped configuration not yet surveyed — beta-stage.

## 7. dracut 108 → 111 — modules intact (versions confirmed; module set verified in repo)

- F44 ships dracut-108-8 (this machine); F45 Beta ships `dracut-111-2.fc45`
  with `dracut-live-111-2.fc45`, `dracut-config-generic`, `dracut-squash`,
  `dracut-network` all present (Beta repo listing). The `dmsquash-live` module
  that kuma's live ISO needs is inside the dracut-live subpackage kuma already
  installs (src/liveiso.rs:133-136, 149) — the subpackage survived, so the
  module did.
- The `resume` module kuma's hibernate story relies on
  (src/hibernate.rs:7-8 — dracut's `resume` module plus
  systemd-hibernate-resume riding in Fedora's initramfs) shows no rename or
  removal in upstream dracut release notes; dracut-058's changelog still fixes
  bugs in `resume` and `dmsquash-live`
  (https://github.com/dracutdevs/dracut/releases — note the Fedora package is
  built from the distro-maintained fork, so upstream releases lag).
- The plymouth module kuma forces with `--add plymouth` (blocks.rs:3294,
  3330) is likewise untouched; the `--add-confdir` and `--no-hostonly`
  invocation pattern has no known F45 issue.
- What to do in kuma: nothing. Test plan covers it (initramfs regen in a 45
  build; live ISO boot; one hibernate cycle).
- Confidence: package-level confirmed; module-content level beta-stage (dracut
  111's full changelog not audited, only absence of rename/removal signals).

## 8. Anaconda: WebUI for Atomic ISOs, Stratis partitioning — no impact on kuma's own ISO (confirmed changes; low relevance)

- Two F45 changes: "Anaconda WebUI Fedora Atomic" switches Fedora's Atomic
  Desktop installer ISOs to the WebUI installer
  (https://fedoraproject.org/wiki/Changes/Anaconda_WebUI_Fedora_Atomic, F45
  Beta per Magazine) and "Stratis Storage in Anaconda" adds native
  Stratis/kickstart/WebUI (Cockpit Storage) partitioning
  (https://fedoraproject.org/wiki/Changes/StratisAnacondaSupport).
  F45 Beta packages anaconda-45.22-1.fc45 and anaconda-webui-83-1.fc45.
- Kuma's installer media is its own `anaconda-iso` built by BIB with the def
  file kuma lifts out of the BIB container (main.rs:2309-2336) — it carries
  whatever Anaconda the (frozen, June 2026) BIB image ships, i.e. the classic
  GTK flow, unchanged by the rebase. Fedora's Atomic ISO switch happens
  upstream of kuma and changes nothing in that path; it only reinforces
  section 3 (the tooling kuma pins is the thing Fedora moved off of).
- Anaconda's ostree/bootc install classes: nothing in the change threads or
  BIB/image-builder notes indicates a break for the `ostreecontainer`
  kickstart path BIB generates; the bootc payload install is handled by
  anaconda-dracut + the ostreecontainer command, untouched in 45.22.
- Stratis: kuma's partitioning script (src/partition.rs) builds ext4/xfs/btrfs
  layouts itself and hands bootc a mounted target; Anaconda partitioning is
  not involved. No action.
- Confidence: changes confirmed; kuma-impact analysis is structural (no
  touching points), beta-stage only insofar as the installer ISO boot test in
  the plan.

## 9. bootc 1.16.9 vs 1.16.10 — same minor, no delta (confirmed)

- F44 (this machine) has bootc-1.16.10-1; F45 Beta ships bootc-1.16.9-1
  (branched slightly earlier). Same minor series — no rebase-induced behavior
  change at all, and the 1.16.x release notes
  (https://github.com/bootc-dev/bootc/releases) show nothing touching
  `bootc switch` or `bootc install to-filesystem` semantics (kuma's call is
  src/partition.rs:824-831 with `--skip-finalize` and mount specs; upstream
  changes in the window are /etc-merge improvements, live-ISO /sysroot
  read-only support — a convenience for kuma's live story, if anything — and
  CI updates for F45 branching). Composefs work continues but nothing forced.
- Confidence: confirmed from release notes plus package versions.

## Test plan (run against a 45-based image before the v45.0.0 release)

1. Keyring assert (the known breakage): build a niri and a COSMIC declaration;
   expect the current assert to fail on the missing `/etc/pam.d/greetd`
   (`/etc/pam.d/cosmic-greeter`); after the blocks.rs fix, confirm the build
   passes with the grep hitting `/usr/lib/pam.d/…`, and that
   `/usr/lib64/security/pam_gnome_keyring.so` still exists
   (gnome-keyring-pam-50.0-6.fc45). Then boot one 45 machine and verify the
   login keyring actually unlocks at the greeter (this is where the oo7
   decision and the alpha-version daemon meet reality).
2. oo7 lane check: on the built 45 desktop image, `rpm -q oo7-daemon` — if it
   arrived via desktop dependencies, decide explicitly (keep gnome-keyring and
   drop oo7-daemon, or adopt oo7) rather than letting bus-name order pick the
   secrets backend. `secret-tool lookup` round-trip after login.
3. Podman 6 verb smoke test (extend the existing os-release test): on a
   podman-6.1 host run, in order — `pull`, `image inspect --format
   {{len .RepoTags}}`, `image exists`, `tag`, `rmi`, `image prune -f`,
   `manifest inspect` against a multi-arch registry, `images -f
   dangling=true -q`, and the `--pull=never` os-release probe on an image
   that is not in any registry (must not pull). Also boot the live ISO and
   confirm podman works with the baked `/etc/containers/storage.conf`
   (liveiso.rs:171) and that the partition-time storage.conf mount
   (partition.rs:822) still produces a working `bootc install to-filesystem`.
4. RPM Fusion URL install: fresh 45 build (cold dnf cache mount) of the
   mesa-freeworld block — the release-RPM-by-URL install must succeed
   non-interactively under enforced rpm signatures, and
   `mesa-va-drivers-freeworld` must still win the `--allowerasing` swap under
   `allow_vendor_change=False`. Check dnf5's output for the new vendor-change
   warning.
5. dnf repoquery on 45 repos: `kuma update --check` against a 45 machine
   (metadata refresh + `--advisory-severities` queries) returns moves, and an
   offline run fails rather than reporting empty (the set -e contract in
   updates.rs:127-154).
6. Live ISO: UEFI boot in GNOME Boxes with the serial console attached —
   grub entries' `console=ttyS0,115200 console=tty0` still yield a readable
   boot and a working serial login (kmscon does not own ttyS0); the
   dmsquash-live initramfs (dracut 111) mounts the squashfs; firefox layer
   and storage.conf are in place.
7. Installer ISO: build `anaconda-iso` with the frozen BIB image, boot it,
   complete an install to disk — confirms the def extraction
   (main.rs:2315-2333) and the whole BIB pipeline against 45-era tooling on
   the host side.
8. dracut/hibernate: 45 image build completes the `--add-confdir ... --add
   plymouth` regen (blocks.rs:3323-3331); on an installed 45 machine with
   swap, `kuma hibernate` setup writes `resume=`/`resume_offset=` and the
   dracut-111 initramfs resumes.
9. doctor on 45: `podman ps -a --external` and the dangling-image probe
   (inspect.rs:2622-2640) still return the shapes the check greps for
   (podman 6's `--external` storage entries and `-working-container` naming).

Unresolved from primary sources: (a) whether `dnf -y install
<rpmfusion-release-url>` still auto-imports the key non-interactively under
the enforced `%_pkgverify_level=all` default (test 4 answers it); (b) what the
merged osbuild/image-builder project publishes as the long-term replacement
container for BIB consumers and whether
`/usr/share/bootc-image-builder/defs/` keeps its path/format there (track
releases v83+); (c) whether gnome-keyring/gnome-keyring-pam get retired in a
later F45 update — the change proposal does not retire them, but the oo7
default makes them maintenance-mode; re-check at GA.
