# LaresOS security items — portability research

Status: research only, nothing implemented. Sources: primary upstream docs and
source repos, plus secureblue's `live` branch (fetched 2026-09-12/13). Where a
claim could not be verified from a primary source it is marked. Scores are
1–5 fit against kuma's compose model and contract philosophy.

Context: LaresOS (~/Documents/lares, DESIGN.md §6) ships a hardened image:
USBGuard default-deny, hardened_malloc, kernel kargs/sysctls, SUID inventory,
sshd off, greenboot greeter check, FDE pre-checked, signed images, capability
profiles. Kuma already has: greenboot + doctor grading, LUKS2 at install,
cosign-signed releases, firewalld, SELinux label handling, Flatpak
`[overrides]` convergence, snapshots, backups. The items below are the gap.

---

## 1. SUID/SGID inventory in CI — fit 5/5

- `find /usr -xdev -type f -perm /6000` enumerates; a golden-diff assertion
  (fail on any path in the image absent from a committed golden) slots beside
  kuma's existing `SWEEP`/`LINT` verdict blocks and `src/containerfile/goldens/`
  test pattern. Zero runtime surface, no schema, no contract impact.
- secureblue goes further: blanket `chmod ug-s` with an allowlist, then deletes
  `sudo`/`su`/`pkexec`/`chsh`/`chfn` and replaces with file capabilities and
  `run0` (files/scripts/removesuid.sh). Kuma ships `sudo` deliberately
  (src/compose.rs:137) — removal is a declaration-level policy question, not
  plumbing, and out of scope for a first step.
- Caveats: package updates churn the inventory (fail-on-new tolerates removals
  for free); glibc-hwcaps dirs multiply paths (compare globs); layered RPMs at
  update time can re-add bits — a `kuma doctor` companion check is worth
  considering later.
- https://github.com/secureblue/secureblue/blob/live/files/scripts/removesuid.sh

## 2. Kernel hardening sysctls + kargs — fit 4/5, with real collisions

- Both seams already exist in kuma's compose: `DESKTOP_KARGS` ships as
  `/usr/lib/bootc/kargs.d/10-kuma-desktop.toml` (src/containerfile/blocks.rs:479,
  2822 — verified landing on this machine's /proc/cmdline), and a sysctl.d file
  drop into `/usr/lib/sysctl.d/` is applied by systemd-sysctl every boot.
- secureblue's set (files/system/usr/lib/sysctl.d/55-hardening.conf,
  kargs.d/10-secureblue.toml) is curated but **cannot be adopted verbatim**:
  - `lockdown=` (integrity OR confidentiality) blocks hibernation
    (kernel/power/hibernate.c gates on `security_locked_down(LOCKDOWN_HIBERNATION)`)
    — kuma ships `kuma hibernate`. This rules out the lockdown karg.
  - `module.sig_enforce=1` forecloses any future unsigned akmod (NVIDIA) story.
  - Baked kargs can only ever be added to: bootc calls removing a base-image
    karg locally "undefined behavior"
    (https://github.com/bootc-dev/bootc/blob/main/docs/src/building/kernel-arguments.md)
    — choose a conservative default set.
  - Performance-sensitive: `init_on_free=1`, `mitigations=auto,nosmt` (secureblue
    isolates nosmt as opt-in; halves cores), `slab_debug=FZ`.
- Additive-on-Fedora-44 highlights (measured live on a stock kuma machine):
  `kernel.kptr_restrict=2` (Fedora ships 0), `kernel.yama.ptrace_scope=1`
  (Fedora ships 0 via elfutils), `kernel.sysrq=0` (Fedora 16),
  `fs.suid_dumpable=0` (systemd ships 2), `kernel.io_uring_disabled=2`,
  `kernel.kexec_load_disabled=1`, `vm.unprivileged_userfaultfd=0`,
  `kernel.unprivileged_bpf_disabled` (Fedora kernel-default 2 already beats
  secureblue's 1), `kernel.core_pattern=|/bin/false`, TCP/ICMP hardening.
  `init_on_alloc=1` is redundant (CONFIG_INIT_ON_ALLOC_DEFAULT_ON=y in Fedora's
  kernel config).
- `kernel.unprivileged_userns_clone` is a Debian patch, not a mainline sysctl —
  nothing for kuma to set (https://docs.kernel.org/admin-guide/sysctl/kernel.html).
- kargs: https://secureblue.dev/articles/kargs · sysctls:
  https://github.com/secureblue/secureblue/blob/live/files/system/usr/lib/sysctl.d/55-hardening.conf

## 3. `kuma run no-home|airlock` (bwrap sandbox runner) — fit 4/5

- Targets the npm/pip postinstall problem: pip docs admit "running arbitrary
  code from distributions" by default; npm runs lifecycle scripts on every
  install. bubblewrap is unprivileged (setuid mode removed upstream) and
  **already in kuma's base** (bubblewrap-0.12.0, pulled by ostree-libs et al;
  headless composes unverified).
- Lares reference implementation: src/sandbox.rs (~75 lines, tested),
  `bwrap --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp --tmpfs "$HOME"
  --bind "$PWD" "$PWD" --setenv HOME "$HOME" --unsetenv SSH_AUTH_SOCK
  --unsetenv GPG_AGENT_INFO [--unshare-net] -- cmd…`. Verified working under
  SELinux Enforcing on this machine; netns shows only loopback.
- Port must add what lares lacks: `--new-session` (without it the child can
  TIOCSTI keystrokes back into the terminal — bwrap(1) says this "can lead to
  out-of-sandbox command execution") and `--die-with-parent`.
- Caveats: inside podman it fails by default (`CLONE_NEWNS` EPERM); works with
  `--cap-add SYS_ADMIN --security-opt seccomp=unconfined` — so scope the verb
  to runtime use (vm/host), not build-time. cwd stays writable by design.
- Shape: new verb, contract-clean (additions are free, docs/contract.md). Not a
  compose block — sandboxing is per-invocation state, not declaration state.
- https://github.com/containers/bubblewrap/blob/main/bwrap.xml ·
  https://pip.pypa.io/en/stable/topics/secure-installs/ · lares src/sandbox.rs

## 4. TPM-bound LUKS auto-unlock — fit 3/5

- One command: `systemd-cryptenroll --tpm2-device=auto --tpm2-pcrs=7
  /dev/disk/by-uuid/<luks uuid>`; token rides in the LUKS2 header, initrd's
  systemd-cryptsetup tries it automatically — no crypttab edit, no karg change.
- **PCR 7 only** (Secure Boot state) is the low-brittle choice: kernel updates
  never break it; SB flips and dbx updates (fwupd ships these; Microsoft's
  2026 3rd-party-CA rollover is a concrete upcoming driver) cause a passphrase
  prompt, not a brick — the passphrase slot kuma always creates is the
  guaranteed fallback ("the user is queried for a password", systemd-cryptsetup
  man). Binding PCR 11 ties unlock to exact kernel/initrd measurements —
  breaks every kernel update.
- Kuma's `--no-hostonly` initramfs already carries the TPM2 unlock stack on
  this machine (tpm2-tools installed; dracut's 11systemd-cryptsetup includes
  tpm2-tss in non-hostonly builds) — deserves one CI assert.
- Fit: install-time question beside `ask_encrypt` (src/install.rs:616) when
  `/dev/tpmrm0` exists, and/or a `kuma tpm` verb in the `kuma hibernate` mold
  (enable/status/recovery-key/re-enroll after a dbx break). Machine-local
  state, like hostname/timezone — kept out of the declaration by the same
  design principle. `kuma doctor` could grade "TPM slot enrolled AND a
  fallback slot exists".
- Anaconda ships nothing automatic (anaconda.conf has no TPM knob); Bluefin
  ships a manual ujust toggle that binds **no PCRs** (never breaks, weaker
  model): https://github.com/projectbluefin/common/blob/main/system_files/shared/usr/bin/luks-tpm2-autounlock
- https://www.freedesktop.org/software/systemd/man/latest/systemd-cryptenroll.html ·
  https://www.freedesktop.org/software/systemd/man/latest/crypttab.html

## 5. USBGuard default-deny — fit 3/5

- Packages (`usbguard`, `usbguard-notifier`) bake trivially, but the policy
  must be generated **on the real machine** — `usbguard generate-policy`
  authorizes currently-connected devices, and a build container has none.
  secureblue deliberately ships packages only + a post-install setup command,
  and does NOT enable the service in its preset.
- Footguns: device unplugged at policy time is locked out; devices without a
  serial get via-port-bound rules (move the plug, rule breaks); an unrecognized
  hub/dock blocks its children; Bluetooth input bypasses USBGuard entirely
  (it mediates USB uevents only); IPC ACL misconfiguration is its own attack
  surface ("will allow them to manipulate the authorization state", daemon.conf.5).
- The notifier notifies only — allowing stays a CLI action.
- Fit question for kuma: install-time step (like the TPM ask) vs a `kuma` verb.
  Needs a first-run story either way; silent default-on risks locking a real
  keyboard.
- https://github.com/USBGuard/usbguard/blob/main/doc/man/usbguard-daemon.conf.5.adoc ·
  https://github.com/secureblue/secureblue/blob/live/files/justfiles/common/utilities.just (lines 445–464)

## 6. hardened_malloc — fit 3/5, imports a trust root

- **Not in Fedora repos** (no dist-git package). secureblue packages it in COPR
  `secureblue/packages` (https://github.com/secureblue/hardened_malloc) —
  adopting it means adding a COPR to kuma's compose, a new trust root that
  clashes with kuma's local-first/Fedora-supplies-the-packages stance. Decision
  needed before anything else.
- Deployment is five file drops + env vars (secureblue's set-ld-preload.sh,
  profile.d, environment.d, system.conf.d, pam_env.conf) with a 0600
  `/etc/ld.so.preload`; Flatpak side is two global override lines
  (`--filesystem=host-os:ro` + `LD_PRELOAD=…`) which fit kuma's existing
  `kuma flatpak-overrides` seam (src/containerfile/blocks.rs:504).
- Compat tax is real: Electron apps crash ("fatal allocator error" — Signal,
  VSCode, Discord; secureblue issue #193), RLIMIT_AS self-limiting software
  needs the companion `no_rlimit_as` shim, and the opt-out story
  (`with-standard-malloc` wrapper / Flatseal per-app removal) has no natural
  place in a declaration-only tool. Upstream recommends global + exceptions.
- `vm.max_map_count=1048576` is already Fedora 44 default (systemd-udev).
- Unverified (do not act): Firefox/jemalloc folklore — no primary statement
  either way; no numeric memory-overhead figures exist upstream.
- https://github.com/GrapheneOS/hardened_malloc/blob/master/README.md ·
  https://github.com/secureblue/secureblue/blob/live/files/scripts/set-ld-preload.sh

## 7. sshd off by default — fit 2/5, recommend not flipping

- Fedora enables sshd by preset (`90-default.preset` in fedora-release-common)
  and `openssh-server` is mandatory in @core. Kuma enables it **by name** in
  every image (src/containerfile/blocks.rs:3170) because `kuma vm` and the
  boot smoke tests ssh in; tests pin this (mod.rs:707, 728).
- The machine-level opt-out already exists (`[services] disable =
  ["sshd.service"]`, tested to beat kuma's default). secureblue disables via a
  `35-*.preset` + mask; Bazzite ships a runtime toggle only.
- A flip buys a headline and costs serial-console plumbing across every CI job
  the contract's cross-version test depends on. Documentation-only move.

## Recommended order (for discussion)

1. **SUID golden-diff CI block** — free, zero surface.
2. **Conservative sysctl.d hardening file** — additive-on-F44 entries only,
   no performance or compat cost.
3. **`kuma run no-home|airlock`** — port lares' sandbox.rs with
   `--new-session`/`--die-with-parent` added; contract addition is free.
4. **Conservative kargs subset** — everything except `lockdown=`,
   `module.sig_enforce=`, nosmt, init_on_free (hibernate + future-akmod +
   perf collisions); remember kargs are additive forever.
5. **TPM auto-unlock** — install-time ask and/or `kuma tpm` verb; PCR 7 only.
6. Decide later / policy first: **hardened_malloc** (COPR trust-root question),
   **USBGuard** (first-run story), **sshd flip** (recommend no).
