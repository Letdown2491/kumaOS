# Unit sandboxing — systemd hardening for the converger and backup units

Status: planned 2026-09-27, from the security audit (four sweeps, nothing
above Low; this is the one finding worth work). Not started. Base-independent:
works identically on a Fedora 44 image, which per the doctrine in
[44.2.0-plan.md](44.2.0-plan.md) is exactly what lands before the 45.0.0
rebase — so this is a 44.x item, not a 45.0.0 rider.

## The threat model, stated precisely

Kuma's own script content is not the attacker: the units run shell baked at
image build, fixed text in `blocks.rs` string literals, pinned by goldens,
and every declaration value that reaches them passes an alphabet validator
first. What the units do *invoke* is upstream software running as root:
restic (`kuma-backup`, `kuma-restore`), flatpak (`kuma-flatpak-sync`), brew
(`kuma-brew-setup`). The sandbox exists for the day one of those has a bug
that writes files or reads what it should not — today such a bug writes
anywhere on the machine; sandboxed, it writes inside `ReadWritePaths=` or
fails.

## Scope: four units

Every unit that runs an upstream tool as root with a credential or a
network hop in reach. The timers stay untouched; this is the service half.

- **`kuma-backup.service`** — restic, `RESTIC_PASSWORD` via
  `EnvironmentFile=`. Already carries `PrivateMounts=yes`; adds
  `NoNewPrivileges=yes`, `ProtectSystem=strict`, `PrivateTmp=yes`,
  `ReadWritePaths=/var/lib/kuma`. Reading `/var/home`, `/etc` and the
  network stay open — the whole job is reading the machine and sending
  bytes off it — which is the honest limit of what sandboxing buys here.
- **`kuma-restore.service`** — restic again, first boot. Adds the same
  core set with `ReadWritePaths=/var/lib/kuma:/var/home`: restoring writes
  the home directory back, and nothing else.
- **`kuma-flatpak-sync.service`** — flatpak as root. Same core set with
  `ReadWritePaths=/var/lib/flatpak:/var/lib/kuma`.
- **`kuma-brew-setup.service`** — the curl-tar-chown ladder. Same core set
  with `ReadWritePaths=/home/linuxbrew`; network stays open, since
  fetching the tarball is the job.

The exact directive set per unit is settled at implementation against what
each script actually touches — the list above is the proposal, and the
goldens' diff is the review. The machine-independent assertions (no
`NoNewPrivileges` conflict, `ReadWritePaths` covering every write the
script makes) belong in the nightly, not in a unit test: see below.

## Deliberately out — the boot-critical units

`kuma-boot-titles`, `kuma-boot-health-sync`, `kuma-fstab-sync`,
`kuma-user-sync` and the /etc-drift machinery get nothing. Two reasons,
both worth recording so the pass does not creep onto them later:

1. **Timing.** They run in the first seconds of a boot, before any login
   and before anything user-controlled exists on the machine. The attack
   surface they carry is the boot itself, which is greenboot's problem,
   already answered.
2. **Failure placement.** They write the paths a wrong directive locks
   down: `/boot/loader/entries`, `/etc/fstab`, `/var/lib/kuma/user`. A
   sandbox mistake there is not a red job, it is a machine that cannot
   complete a boot — the exact class of failure the rollback machinery
   exists to catch, aimed at the rollback machinery.

## Test plan

- The unit text is golden-pinned, so every directive shows in the golden
  diff like any other image content — that diff is the review.
- The nightly boot jobs already exercise all four units on a real guest:
  flatpak/brew convergence on the daily boot, backup and restore through
  the dead-disk stage. A wrong `ProtectSystem` or a missing
  `ReadWritePaths` entry goes red there, on the base that moves without
  anyone pushing — which is the release-blocking evidence, not a unit
  test that replays strings.
- One manual check before tagging whatever release carries this: run the
  backup stage with the unit's journal watched, and confirm restic sees
  no new denials. `journalctl -u kuma-backup` naming an EPERM is the
  directive set disagreeing with the script, and the script is not the
  thing to change.

## Contract

None of this touches a verb, a flag, a declaration key or a response
field — it is image content, which the contract names as not an interface.
No deprecation, no changelog entry beyond what a doctor change would
carry if one is added; a doctor check grading that the sandbox is present
is optional and deliberately not committed to here.
