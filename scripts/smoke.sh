#!/usr/bin/env bash
# kuma smoke tests: build every committed example, and optionally boot it.
#
# The promise under test is the one the stability plan opens with: a
# declaration that validates either becomes a running system matching it,
# or fails loudly. `cargo test` already checks what can be checked without
# a machine (every example compiles to an image that keeps kuma's floor);
# this is the part that needs real podman, real bootc, and a real boot.
#
# Five stages, cheapest first:
#
#   check     parse and validate the declaration          (no podman)
#   image     build it and inspect the built layers       (podman)
#   install   write an encrypted disk and verify it       (podman + sudo)
#             offline, through a loop device
#   boot      make a disk, boot it headless, ask the      (podman + kvm + sudo)
#             machine whether the boot was healthy
#   published install an image kuma published and boot    (podman + kvm + sudo
#             the disk that install wrote                  + sshpass + network)
#
# The boot stage's verdict is greenboot's own: the same check that decides
# whether this machine would roll an update back is the one that decides
# whether the test passed. On a desktop image that includes reaching the
# greeter, so "boots fine into a black screen" fails here rather than on
# your laptop.
#
# Usage:
#   scripts/smoke.sh                   # check + image, every example
#   scripts/smoke.sh --boot            # check, image and boot, every example
#   scripts/smoke.sh --install         # check, image and install
#   scripts/smoke.sh --iso             # build the live ISO and boot it
#   scripts/smoke.sh --boot minimal    # just one, by example name
#   scripts/smoke.sh --keep            # leave images and disks behind
#   scripts/smoke.sh --published ghcr.io/letdown2491/kuma:niri
#   scripts/smoke.sh --published <image> --encrypted   # LUKS, unlocked
#                                                      # at the console
#   scripts/smoke.sh --published <image> --hibernate --secure-boot
#
# --hibernate installs with a swapfile, then asks the machine to suspend
# to disk and come back. It is the only stage where the verdict is not
# "did it boot" but "is this the same boot": a machine that hibernates,
# powers off, and then starts fresh looks identical from the outside and
# has silently lost everything that was open. So the assertion is the
# kernel's own boot_id, which is generated at boot and lives in the
# memory a real resume restores, plus a marker left in tmpfs.
#
# It also asks a resumed machine to power off. A guest resets instead,
# every time, because it hibernates under one firmware instance and
# resumes under another; hardware asked the same question on 2026-08-21
# went off and stayed off. So that check warns and the summary repeats
# it, rather than failing a run over a difference the harness invents.
#
# --secure-boot adds a SECOND boot of the same disk, on firmware with
# Microsoft's keys enrolled. It is not a second attempt at hibernating: a
# kernel locked down under Secure Boot refuses hibernation outright, so
# such a machine can never demonstrate a resume. What it tests is whether
# kuma says so. Doctor grading a Secure Boot machine `ok` on the strength
# of a correct swapfile, while the kernel would never do it, is the bug
# the first run of this stage found, and this is the check that would
# have caught it.
#
# --published builds nothing and reads no example: it installs what is on
# the registry and boots the result, so it is the only stage that can go
# red without anyone having committed anything.
#
# --install is separate from --boot rather than another step of it: it is
# the only stage that needs no KVM, and it answers a different question.
# Boot asks whether a machine works; install asks whether the disk it was
# written onto is the one that was described.
#
# --iso is the third question and the only one about the artifact a
# stranger downloads: it builds the live ISO, refuses one too big to ride
# a release, and boots it under UEFI to ask whether a desktop came up.
# It talks to the guest over the serial console because installer media
# has no disk to inspect and its account has no password for ssh to use.
#
# Env: KUMA (default target/debug/kuma), QEMU_DISPLAY (default
#      egl-headless), QEMU_VGA (default virtio-vga-gl).
set -euo pipefail

cd "$(dirname "$0")/.."

KUMA=${KUMA:-target/debug/kuma}
# Headless and GL-capable: niri's DRM backend refuses a device it cannot
# allocate through — it skips software EGL renderers for the renderer and
# then finds no GBM allocator on a 3D-less virtio-gpu — and a compositor
# with zero outputs closes the greeter's layer surface the moment it asks
# (measured 2026-10-05 in the VM: greeter exits rc=0 within a second,
# greetd reads "greeter exited without creating a session"). The guest's
# GL is virgl's: virtio-vga-gl hands the guest a 3D device, whose guest
# driver mesa allocates through on llvmpipe alone — no host GPU needed.
#
# The display backend is plain egl-headless — no gl=on. This is the
# configuration the runner's full battery last went green with
# (db1c650, 2026-10-03: image, boot, install, iso, dead-disk, and
# hibernate all passed on a hosted runner), and the distinction is not
# cosmetic: with gl=on the DISPLAY builds its GL context through a host
# DRM render node and dies without one ("egl: no drm render node
# available", measured on a runner), while without it the display is a
# plain scanout surface and only the DEVICE touches EGL — lazily, on
# the guest's first virgl submit, through a path that demonstrably
# works nodeless. The runner cannot be given a render node: its azure
# kernel's modules-extra package carries no vgem (measured 2026-10-05:
# the package installs, modprobe vgem still fails).
#
# The two display backends this script shipped between those points
# both died on a GPU-less host and are recorded to keep them dead:
# gtk,gl=on against an Xvfb the script started — qemu died at the
# guest's first mode-set with "eglMakeCurrent failed: EGL_BAD_ACCESS"
# followed by an epoxy assert (boot stage, twice locally and once on
# the runner) — and egl-headless,gl=on on a vgem node, which is green
# locally and impossible on the runner for the reason above.
QEMU_DISPLAY=${QEMU_DISPLAY:-egl-headless}
QEMU_VGA=${QEMU_VGA:-virtio-vga-gl}

# The runner has no GPU: without LIBGL_ALWAYS_SOFTWARE, mesa's EGL refuses
# the software path and qemu dies at its first boot ("OpenGL is not
# supported by the display"). This is exported once here, not per-site:
# per-site prefixes are exactly how one of four qemu sites ends up without
# it (measured Oct 05 — install and dead-disk died instantly at their
# first boot while iso, which had the variable inline, ran 21 minutes).
export LIBGL_ALWAYS_SOFTWARE=1

# Only a display that asks for GL needs a render node (the virgl device's
# own EGL path does not). A GPU-less machine has none until vgem provides
# one; on a host with a real GPU this is a no-op (the node exists; vgem
# is neither needed nor loaded). The runner's kernels cannot load vgem,
# so this must never fire for the default display — and does not: the
# case below only calls it when QEMU_DISPLAY itself carries gl=on.
ensure_render_node() {
    ls /dev/dri/renderD* >/dev/null 2>&1 && return 0
    echo "   .. no render node; loading vgem"
    sudo modprobe vgem 2>/dev/null || true
    ls /dev/dri/renderD* >/dev/null 2>&1 || {
        echo "smoke: no DRM render node and vgem would not provide one" >&2
        echo "smoke: (the kernel needs vgem: modprobe vgem, or install the modules-extra package)" >&2
        exit 1
    }
}

BOOT=0
ISO=0
INSTALL=0
# The ISO rides a GitHub release, and a release asset is capped at 2 GB.
# Failing below the cap rather than at it: an ISO that only just fits is
# one desktop package away from not fitting, and finding that out when a
# tag is already pushed means a release with no installer.
ISO_MAX_BYTES=${ISO_MAX_BYTES:-1900000000}
PUBLISHED=""
DEAD_DISK=0
UPGRADE_TO=""
ENCRYPTED=0
HIBERNATE=0
SECURE_BOOT=0
KEEP=0
SELECTED=()

while [ $# -gt 0 ]; do
    case "$1" in
        --boot) BOOT=1 ;;
        --iso) ISO=1 ;;
        --install) INSTALL=1 ;;
        --published) PUBLISHED=${2:?--published needs an image reference}; shift ;;
        --dead-disk) DEAD_DISK=1 ;;
        --upgrade-to) UPGRADE_TO=${2:?--upgrade-to needs an image reference}; shift ;;
        --encrypted) ENCRYPTED=1 ;;
        --hibernate) HIBERNATE=1 ;;
        --secure-boot) SECURE_BOOT=1 ;;
        --keep) KEEP=1 ;;
        # The header, however long it has become. A line range went
        # stale the moment the header grew: --hibernate added fourteen
        # lines and --help silently stopped printing --install, --iso and
        # the environment variables, while still looking like complete
        # help. Reading until the comments stop cannot drift.
        -h|--help) awk 'NR == 1 { next } /^#/ { print; next } { exit }' "$0"; exit 0 ;;
        -*) echo "unknown flag: $1" >&2; exit 2 ;;
        *) SELECTED+=("$1") ;;
    esac
    shift
done

[ -x "$KUMA" ] || { echo "no kuma binary at $KUMA (cargo build first, or set KUMA)" >&2; exit 2; }

# A stale binary is the stale-disk trap wearing a different hat: every
# stage passes and none of it tested your change. `cargo test` and
# `cargo clippy` both leave target/debug/kuma untouched, so this is easy
# to hit. Refuse instead of warning, because a smoke test you have to
# remember to distrust is not a gate. (This build isn't run for you:
# on a bootc host the toolbox owns the compiler.)
stale=$(find src Cargo.toml Cargo.lock -newer "$KUMA" -print -quit 2>/dev/null || true)
[ -z "$stale" ] || {
    echo "$KUMA is older than $stale; rebuild it first (cargo build)" >&2
    exit 2
}

# And the same question about the image, which is the half that guard did
# not cover and which cost a run of the slowest stage here.
#
# `--published localhost/...` installs a tag built from this tree by an
# earlier invocation, and nothing made that tag when the tree changed. A
# fix that lives in the image rather than in the binary — a unit, a
# policy rule, the kuma that gets baked in — is then absent from the
# machine under test, and every assertion runs against a system that
# predates the thing being tested. That is exactly the shape the binary
# guard above exists to refuse, so it is refused the same way.
#
# Only for a localhost tag. An image from a registry was not built from
# this tree and cannot be stale against it, which is the whole point of
# the published stage.
case "$PUBLISHED" in
    localhost/*)
        built=$(podman image inspect --format '{{.Created.Format "2006-01-02T15:04:05Z07:00"}}' \
                "$PUBLISHED" 2>/dev/null || true)
        if [ -n "$built" ]; then
            newer=$(find src Cargo.toml Cargo.lock -newermt "$built" -print -quit 2>/dev/null || true)
            [ -z "$newer" ] || {
                echo "$PUBLISHED was built at $built, before $newer changed." >&2
                echo "It would install a machine that predates what you are testing." >&2
                echo "Rebuild it first:  ./scripts/smoke.sh --keep <example>" >&2
                exit 2
            }
        fi
        ;;
esac

PASS=(); FAIL=()
# Warnings outlive the stage that raised one. Every stage runs inside a
# subshell, so a variable would never come back, and a line printed two
# thousand lines before the summary is a line nobody reads. The summary
# reads this file instead, and refuses to say "all good" over it.
WARNLOG=${TMPDIR:-/tmp}/kuma-smoke-warnings.$$
: >"$WARNLOG"
note() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok()   { printf '   ok   %s\n' "$*"; }
# Neither a pass nor a failure: something this harness measured, that
# hardware has measured differently. Use it only where the hardware
# result is written down and dated; anything else is a FAIL wearing a
# quieter word.
warn() { printf '   warn %s\n' "$*"; printf '%s\n' "$*" >>"$WARNLOG"; }
# Read back by EVERY summary, and there are three: the example sweep, the
# published stage and the dead-disk stage each end in their own block and
# exit before reaching the next. The first version of this only printed in
# the sweep's summary -- and the published stage is the one that actually
# raises warnings, so run 14 finished "all good" over a warning with the
# readback sitting in a branch it never reached.
show_warnings() {
    [ -s "$WARNLOG" ] || { rm -f "$WARNLOG"; return 0; }
    while IFS= read -r warning; do printf '   warn: %s\n' "$warning"; done <"$WARNLOG"
    rm -f "$WARNLOG"
}
# exit, not return. Each example's stages run inside `if ( ... )`, and
# bash disables `set -e` for the whole dynamic extent of a command whose
# exit status is being tested, so a `|| bad` that only returned printed
# FAIL and then carried on to the next assertion and the summary. This
# harness reported "all good" over a failing unit exactly once, which is
# once more than a test harness gets to.
bad()  {
    printf '   FAIL %s\n' "$*"
    # The guest's own account, while there is one to ask. The EXIT trap
    # takes qemu down with the assertion, and the job-level capture
    # steps that would tail the console have now been skipped by an
    # unlucky step outcome twice; nothing after this line can ask the
    # machine anything. No guest function means the stage never booted
    # a VM, and `guest true` is the five-second answer to whether ssh
    # is still up; both skip quietly. The user journal is the half that
    # matters: the shell, niri and every lock path log there.
    if declare -F guest >/dev/null && guest true; then
        printf '   --- the guest, asked before the trap takes it down ---\n'
        guest 'systemctl --failed --no-legend --plain' || true
        # Each failed unit's own journal lines. The list above names a
        # unit without saying why, and the severity and grep pulls below
        # have both missed a converger's error before: unit stderr logs
        # at info, and a mid-boot failure falls out of any tail once the
        # later lines arrive.
        # shellcheck disable=SC2016  # $unit must expand on the guest, not here
        guest 'systemctl --failed --no-legend --plain | while read -r unit _; do journalctl -b -u "$unit" --no-pager | tail -20; done' || true
        guest 'systemctl --user --failed --no-pager --plain' || true
        guest 'loginctl list-sessions --no-legend' || true
        # The harness's own polling opens an ssh connection every few
        # seconds, and each one logs a disconnect, a logind session
        # lifecycle and an audit line: a raw tail is the harness
        # describing itself, measured on both 2026-08-22 runs, which
        # lost the lines the dump exists to keep. So ask positively
        # for the names a failing desktop would mention, and take the
        # wide tails with the harness's own noise cut out. greetd and
        # the session log at info, so severity filters alone miss them.
        guest 'journalctl -b -p err --no-pager | tail -20' || true
        guest 'journalctl -b -u greetd.service --no-pager | tail -20' || true
        # The greeter chain's stderr lands in the journal tagged
        # kuma-greeter (see GREETER_SESSION): niri's protocol errors and
        # the greeter's own log lines are the evidence a dead login
        # screen leaves. AVC denials are audit noise to the wide greps
        # but exactly the verdict an SELinux-bound greeter leaves.
        # The greeter chain logs to the journal tagged kuma-greeter
        # (see GREETER_SESSION). The wrapper's exit markers are the
        # story, and they get their own grep: WARN/ERROR lines from
        # drm, zbus and layer-shell run past thirty on a bad boot and
        # would bury them in any shared tail. naga's shader-compile
        # debug runs to thousands of lines, so the raw tail is small.
        guest 'journalctl -b --no-pager -t kuma-greeter | grep -a "wrapper:" | tail -30' || true
        guest 'journalctl -b --no-pager -t kuma-greeter | grep -aE "ERROR|panic" | tail -30' || true
        guest 'journalctl -b --no-pager -t kuma-greeter | tail -250' || true
        # oomd kills cgroups from userspace: no kernel oom-kill line,
        # just a line in its own unit that no tag grep above reads.
        guest 'journalctl -b --no-pager -u systemd-oomd | tail -10' || true
        # And whatever else ended the boot, its last words are here:
        # the unfiltered end of the journal, no tag can exclude it.
        guest 'journalctl -b --no-pager | tail -40' || true
        guest 'journalctl -b --no-pager | grep -iE "avc.*denied|selinux" | tail -20' || true
        # a compositor or GPU client dying without its own log line
        # leaves a kernel line instead: traps/segfault, oom-kill
        guest 'journalctl -k -b --no-pager | grep -iE "segfault|general protection|traps:|oom-kill|killed process" | tail -10' || true
        # And a process that dumped core left its stack on the guest.
        # coredumpctl reads it back even with no debuginfo installed:
        # the crashing frame names its own shared object, which is the
        # difference between "rc=139 after 1s" and a culprit. The list
        # first (what dumped, when), then each regular suspect's stack.
        guest 'coredumpctl --no-pager --since=-2h list 2>/dev/null | tail -5' || true
        # shellcheck disable=SC2016  # $p must expand on the guest, not here
        guest 'for p in kuma-greeter kuma-shell niri kuma-files; do coredumpctl --no-pager info "$p" 2>/dev/null | tail -50; done' || true
        guest 'journalctl -b --no-pager | grep -iE "greetd|niri|noctalia|kuma-shell|kuma-greeter" | tail -30' || true
        guest 'journalctl -b --no-pager | grep -vE "sshd|logind|audit|session-[0-9]+" | tail -30' || true
        guest 'journalctl --user -b --no-pager | grep -vE "sshd|logind|audit|session-[0-9]+" | tail -30' || true
    fi
    exit 1
}

# One value out of the declaration, by dotted key, so every assertion
# below reads what the example actually asks for instead of a copy of it
# that can drift. Lists print space-separated; a true boolean prints
# "true" and a false or absent one prints nothing, so every caller can
# ask the same `[ -n ... ]` question of any key.
declared() {
    python3 -c '
import tomllib, sys
with open(sys.argv[1], "rb") as f:
    node = tomllib.load(f)
for key in sys.argv[2].split("."):
    node = node.get(key) if isinstance(node, dict) else None
if isinstance(node, bool):
    print("true" if node else "")
elif isinstance(node, list):
    print(" ".join(str(item) for item in node))
elif node is not None:
    print(node)
' "$1" "$2"
}

# The UEFI firmware pair, printed as "CODE VARS", or nothing and a
# non-zero status if this machine has none.
#
# Both UEFI stages ask this question and they used to ask it differently.
# The ISO stage searched for VARS separately from CODE and never named
# Ubuntu's _4M files, so on a hosted runner it matched none of its six
# candidates and failed *after* building a 1.8 GB image, with an error
# telling you to install a package the workflow had already installed.
# One list, one rule, one place to fix it next time.
#
# The _4M names are Ubuntu's and come first because that is what CI runs;
# the unsuffixed pair is Fedora's. VARS is derived from CODE by name
# rather than searched for, because the two have to be the same build: a
# 4M vars file against 2M code does not boot. A candidate whose VARS is
# missing is skipped rather than fatal, so a half-installed path cannot
# hide a working one further down the list.
find_ovmf() {
    local candidate vars
    for candidate in /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/OVMF/OVMF_CODE.fd \
                     /usr/share/edk2/ovmf/OVMF_CODE.fd /usr/share/qemu/OVMF_CODE.fd \
                     /usr/share/edk2-ovmf/x64/OVMF_CODE.fd; do
        [ -f "$candidate" ] || continue
        vars=${candidate//CODE/VARS}
        [ -f "$vars" ] || continue
        printf '%s %s\n' "$candidate" "$vars"
        return 0
    done
    echo "   .. looked for OVMF in:" >&2
    ls -1 /usr/share/OVMF /usr/share/edk2/ovmf /usr/share/qemu \
          /usr/share/edk2-ovmf/x64 2>/dev/null >&2 || true
    return 1
}

# The Secure Boot firmware pair, printed as "CODE VARS", or nothing and a
# non-zero status.
#
# Spelled as explicit pairs rather than derived from the CODE name the way
# `find_ovmf` does it, because for this pair the derivation is wrong on
# the distribution CI runs. Ubuntu's secure-boot code is
# `OVMF_CODE_4M.secboot.fd` and the vars file that goes with it is
# `OVMF_VARS_4M.ms.fd`: different infix, and substituting CODE for VARS
# yields `OVMF_VARS_4M.secboot.fd`, which does not exist. Fedora does use
# a matching `.secboot` name for both. One list of pairs is the only
# spelling that is right on both.
#
# `.ms.` is not a detail either: it is the whole point. Those vars have
# Microsoft's keys enrolled, which is what makes a Secure Boot test mean
# anything for kuma, since what kuma ships is shim and shim is signed by
# Microsoft. Vars with no keys enrolled boot everything and would turn
# this into an expensive way to boot normally.
find_ovmf_secboot() {
    local pair code vars
    for pair in "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd /usr/share/OVMF/OVMF_VARS_4M.ms.fd" \
                "/usr/share/OVMF/OVMF_CODE.secboot.fd /usr/share/OVMF/OVMF_VARS.ms.fd" \
                "/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd /usr/share/edk2/ovmf/OVMF_VARS.secboot.fd" \
                "/usr/share/edk2-ovmf/x64/OVMF_CODE.secboot.fd /usr/share/edk2-ovmf/x64/OVMF_VARS.secboot.fd"; do
        code=${pair%% *}
        vars=${pair##* }
        if [ ! -f "$code" ] || [ ! -f "$vars" ]; then continue; fi
        printf '%s %s\n' "$code" "$vars"
        return 0
    done
    echo "   .. looked for Secure Boot OVMF in:" >&2
    ls -1 /usr/share/OVMF /usr/share/edk2/ovmf /usr/share/edk2-ovmf/x64 2>/dev/null >&2 || true
    return 1
}

# --- stage: image ------------------------------------------------------
# What a successful build already proves is not worth re-asserting (dnf
# resolved, the lint passed, every RUN test -f held). These are the things
# a build can succeed *without*.
smoke_image() {
    local file=$1 tag=$2

    "$KUMA" --config "$file" check >/dev/null || bad "check: $file"
    ok "declaration validates"

    # A base already in local storage is the one podman builds on, however
    # old it is, and the lock then records that digest while `update
    # --check` below asks the registry. The two disagree the moment Fedora
    # pushes a new base, which is true news and not what that assertion is
    # about. CI never meets this because a fresh runner has nothing local
    # to be stale; a laptop that has been building for a month always does.
    if grep -q '^base *=' "$file"; then
        local base
        base=$(sed -n 's/^ *base *= *"\(.*\)".*/\1/p' "$file" | head -1)
        [ -n "$base" ] || bad "cannot read system.base out of $file"
        echo "   .. pulling $base so the build and the registry agree"
        podman pull -q "$base" >/dev/null || bad "cannot pull $base"
    fi

    "$KUMA" --config "$file" build --tag "$tag" >/dev/null || bad "build failed"
    ok "image builds"

    # Self-describing: the machine carries the declaration it was made
    # from, and `kuma init` on it must reproduce this file exactly.
    podman run --rm "$tag" cat /usr/lib/kuma/kuma.toml > /tmp/kuma-smoke-baked.toml
    diff -q "$file" /tmp/kuma-smoke-baked.toml >/dev/null \
        || bad "baked declaration differs from $file"
    rm -f /tmp/kuma-smoke-baked.toml
    ok "baked declaration is byte-identical"

    # The branding sed runs in the last layer over a file the base owns;
    # a silent no-op there is invisible until a machine says "Fedora".
    podman run --rm "$tag" sh -c '. /usr/lib/os-release && [ "$ID" = kuma ]' \
        || bad "os-release ID is not kuma"
    ok "identity is kuma's"

    podman run --rm "$tag" test -f /usr/libexec/greenboot/greenboot \
        || bad "greenboot missing: this image cannot roll back a bad update"
    ok "boot health present"

    # The lock is written by the build, and the pin is only real if the
    # next build actually resolves it. `generate` prints what a build
    # would do, so it proves the wiring without a second build.
    local lock="${file%.toml}.lock"
    [ -f "$lock" ] || bad "no lock written beside $file"
    grep -q '^digest = "sha256:' "$lock" || bad "lock records no base digest"
    if grep -q '^base *=' "$file"; then
        "$KUMA" --config "$file" generate | grep -qE '^FROM .+@sha256:' \
            || bad "builds would ignore the locked digest"
        ok "lock pins the base by digest"

        # The digest just recorded is the one this build used, so the registry
        # has to agree the base is current. A "moved" here means the lock is
        # recording a different KIND of digest than the tag resolves to (the
        # per-architecture manifest instead of the OCI index), which is a
        # permanent false alarm rather than news. That shipped once.
        # Reported with what it actually said. This failed once for a
        # reason the message could not express (a base that had genuinely
        # moved, before the pull above existed), and a failure that names
        # only its own assertion sends somebody looking in the wrong place.
        local checked
        checked=$("$KUMA" --config "$file" update --check 2>&1 || true)
        case "$checked" in
            *"is current"*) ;;
            *) bad "update --check disagrees with the lock this build just wrote: $checked" ;;
        esac
        ok "check agrees with the fresh lock"
    else
        # Composed base: the pin is the content-addressed tag itself.
        # Builds FROM it (a localhost/ tag never touches a registry) and
        # the lock's reference must be that tag, so a manifest change
        # reads as a moved reference.
        "$KUMA" --config "$file" generate | grep -qE '^FROM localhost/kuma-base:m' \
            || bad "builds don't FROM the composed content tag"
        grep -q '^ref = "localhost/kuma-base:m' "$lock" \
            || bad "lock doesn't reference the composed base tag"
        ok "lock records the composed base"

        # Captured rather than piped: the check's answer names "composed"
        # in its first line and then lists every package the repos have
        # moved, hundreds of lines when Fedora's backlog is deep. `grep
        # -q` stops at the match and closes the pipe, kuma's next write
        # dies with EPIPE, and under pipefail the panic's exit status
        # fails this pipeline though the word it greps for was there.
        # A capture reads to EOF; nothing downstream can close early.
        checked=$("$KUMA" --config "$file" update --check 2>&1 || true)
        grep -q 'composed' <<<"$checked" \
            || bad "update --check doesn't explain composed-base updates: $checked"
        ok "check explains recompose semantics"
    fi
}

# --- stage: install ----------------------------------------------------
# What `kuma install` writes, checked on the disk rather than by booting
# it. Booting proves a machine works; this proves the one thing a boot
# cannot report, because a disk written wrongly is not recoverable by the
# person holding it: the container opens with the passphrase that was
# typed, and the bootloader asks the initramfs to unlock the container
# that is actually there. Get either wrong and the install succeeds, the
# machine is unbootable, and whatever was on that disk is gone.
#
# Encrypted only. The encrypted path is a superset: it writes the same
# table, the same filesystems and the same account file, plus a container
# and a karg. Running both would double the slowest stage here to check
# the same partition table twice. Custom partition sizes ride the same
# install rather than a second one, for the same reason: they change what
# the table says, and the table is read once.
smoke_install() {
    local file=$1 tag=$2 name=$3
    local dir="vm-smoke/$name-install"
    local raw="$dir/disk.raw"
    local pass="smoke-passphrase"
    local user="smoketest"

    mkdir -p "$dir"
    rm -f "$raw"
    # Sparse, so this costs no disk until the install fills it, and above
    # the floor partition::plan refuses below: 17.4G for the sizes this
    # stage asks for, 16G for the defaults.
    truncate -s 24G "$raw"

    # Two lines on stdin, in the order the interview asks: the disk
    # passphrase, then the account password. Neither is ever a flag.
    #
    # --update-from because the image being installed is a localhost tag,
    # which kuma refuses to record as an update source: on the installed
    # machine `localhost` means itself. The reference here is never
    # fetched by this test; it exists because the installed machine has to
    # record somewhere real to update from.
    echo "   .. installing to a disk image (needs sudo; this is the slow part)"
    printf '%s\n%s\n' "$pass" "$pass" \
        | "$KUMA" install --disk "$raw" --image "$tag" \
            --update-from ghcr.io/example/kuma:niri \
            --user "$user" --encrypt --swap 1G --esp 1G --boot 3G --yes >/dev/null \
        || bad "install failed"
    ok "installed"

    # Everything below reads the disk as root through a loop device, and
    # unwinds in the order it was set up. EXIT rather than RETURN for the
    # same reason the boot stage uses it: a failed assertion exits this
    # subshell and a RETURN trap would never fire, leaving a loop device
    # and an open mapper behind to confuse the next run.
    local loop mnt="$dir/mnt" mapper="kuma-smoke-$name"
    loop=$(sudo losetup -fP --show "$raw") || bad "cannot attach $raw"
    # shellcheck disable=SC2064
    trap "sudo umount -R '$mnt' 2>/dev/null || true
          sudo cryptsetup close '$mapper' 2>/dev/null || true
          sudo losetup -d '$loop' 2>/dev/null || true" EXIT
    mkdir -p "$mnt"

    # The sizes the flags asked for, read back off the table. The
    # defaults would also be a valid table, so what the assertions compare
    # against is what was asked for: a layout that silently fell back to
    # the defaults is the failure they are there to catch.
    local esp_mib boot_mib
    esp_mib=$(( $(lsblk -bno SIZE "${loop}p1") / 1048576 ))
    boot_mib=$(( $(lsblk -bno SIZE "${loop}p2") / 1048576 ))
    [ "$esp_mib" -eq 1024 ] || bad "the ESP is ${esp_mib}M, not the 1024M --esp asked for"
    [ "$boot_mib" -eq 3072 ] || bad "/boot is ${boot_mib}M, not the 3072M --boot asked for"
    ok "the ESP and /boot carry the sizes the flags asked for"

    sudo cryptsetup isLuks "${loop}p3" || bad "the root partition holds no LUKS container"
    printf '%s' "$pass" \
        | sudo cryptsetup luksOpen --test-passphrase --key-file - "${loop}p3" \
        || bad "the passphrase that was typed does not open the container"
    ok "the root partition is LUKS and opens with the passphrase"

    # The karg names the container's own UUID. The mapper reports the
    # UUID of the filesystem inside it, which is a real number that
    # unlocks nothing, and the difference is invisible until a machine
    # boots to an initramfs waiting for a device that will never appear.
    local luks_uuid
    luks_uuid=$(sudo blkid -s UUID -o value "${loop}p3")
    [ -n "$luks_uuid" ] || bad "no LUKS UUID on the root partition"
    sudo mount "${loop}p2" "$mnt" || bad "cannot mount /boot"
    sudo grep -rq "rd.luks.uuid=luks-$luks_uuid" "$mnt/loader/entries" \
        || bad "no boot entry unlocks luks-$luks_uuid"
    ok "the bootloader unlocks the container that is there"

    # Read here, checked below. The resume pair is only meaningful next to
    # the file it points at, and that file is inside the container this
    # has not opened yet.
    local resume_karg offset_karg
    resume_karg=$(sudo grep -rho 'resume=UUID=[^ ]*' "$mnt/loader/entries" | head -1)
    offset_karg=$(sudo grep -rho 'resume_offset=[0-9]*' "$mnt/loader/entries" | head -1)
    [ -n "$resume_karg" ] || bad "--swap was asked for and no boot entry names a resume device"
    [ -n "$offset_karg" ] || bad "--swap was asked for and no boot entry names a resume offset"
    sudo umount "$mnt"

    # And the answers the installer was given, inside the container.
    printf '%s' "$pass" | sudo cryptsetup open --key-file - "${loop}p3" "$mapper" \
        || bad "cannot open the container"
    sudo mount -o subvol=root "/dev/mapper/$mapper" "$mnt" || bad "cannot mount the root"

    # Neither /var nor /etc is where it looks. This is an ostree
    # deployment: the subvolume holds /ostree/deploy/<stateroot>/var, and
    # the merged /etc lives inside the deployment directory under a
    # checksum nobody can predict. Naming them as if the subvolume were
    # the root is how this assertion read correctly and failed the first
    # time it ran.
    local user_file
    user_file=$(sudo find "$mnt/ostree/deploy" -maxdepth 5 -path '*/var/lib/kuma/user' -print -quit)
    [ -n "$user_file" ] || bad "no /var/lib/kuma/user on the installed root"
    sudo grep -q "KUMA_USER='$user'" "$user_file" \
        || bad "the installer's account file does not name $user"
    ok "the account to converge is written where kuma-user-sync reads it"

    local host_file
    host_file=$(sudo find "$mnt/ostree/deploy" -maxdepth 5 -path '*/var/lib/kuma/hostname' -print -quit)
    [ -n "$host_file" ] || bad "no /var/lib/kuma/hostname for first boot to apply"
    ok "the hostname to apply is written beside it"

    # The install's own provenance, beside the same two files. It exists
    # because THIS stage ran an install; the digest it records must be
    # the image the stage staged, or the file is decoration.
    local provenance
    provenance=$(sudo find "$mnt/ostree/deploy" -maxdepth 5 -path '*/var/lib/kuma/install.json' -print -quit)
    [ -n "$provenance" ] || bad "no /var/lib/kuma/install.json on the installed root"
    sudo grep -q "$tag" "$provenance" \
        || bad "install.json does not name the image that was installed"
    ok "the install recorded its own provenance"

    # The two fstab lines that make the swapfile swap. Without them the
    # machine has a resume offset pointing at a file nothing ever
    # activates, so it can never write an image to hibernate from.
    local fstab
    fstab=$(sudo find "$mnt/ostree/deploy" -maxdepth 5 -path '*/deploy/*/etc/fstab' -print -quit)
    [ -n "$fstab" ] || bad "no /etc/fstab on the installed root"
    sudo grep -q '/var/swap/swapfile none swap' "$fstab" \
        || bad "nothing in fstab activates the swapfile"
    sudo grep -q '/var/swap btrfs subvol=swap' "$fstab" \
        || bad "nothing in fstab mounts the subvolume the swapfile is on"
    ok "the installed fstab mounts and activates the swapfile"

    # The lid's half of the same setup, in the deployment etc the fstab
    # was found in: a swapfile with a lid that only suspends is a machine
    # doctor grades as hibernating on paper only, straight off the
    # install.
    local lid_dropin
    lid_dropin="$(dirname "$fstab")/systemd/logind.conf.d/kuma-suspend-then-hibernate.conf"
    sudo test -f "$lid_dropin" \
        || bad "no suspend-then-hibernate lid setting beside the installed fstab"
    sudo grep -q 'HandleLidSwitch=suspend-then-hibernate' "$lid_dropin" \
        || bad "the installed lid setting does not suspend-then-hibernate"
    ok "the installed lid suspends, then hibernates"

    # No /var/home assertion here on purpose: the image ships none, and
    # tmpfiles creates it at first boot. Whether it is a subvolume is a
    # question for a booted machine, and smoke_boot asks it.

    # The greeter must not autologin an account this machine will not
    # have. A committed example declares no [user], so there is nothing to
    # strip here and the assertion is that nothing crept in.
    local greetd_conf
    greetd_conf=$(sudo find "$mnt/ostree/deploy" -maxdepth 6 \
                  -path '*/etc/greetd/config.toml' -print -quit)
    if [ -n "$greetd_conf" ]; then
        local autologin
        autologin=$(sudo sed -n 's/^user *= *"\(.*\)"/\1/p' "$greetd_conf" | tail -1)
        case "$autologin" in
            ""|greetd|"$user") ok "no greeter autologins an account this disk lacks" ;;
            *) bad "greetd autologins '$autologin', which this machine has no account for" ;;
        esac
    else
        # Out loud, not skipped. A headless image ships no greeter config,
        # so this says nothing about the case the check exists for, and a
        # silent pass would read as if it had.
        ok "no greeter on this image, so that path is unchecked here"
    fi

    sudo umount -R "$mnt"

    # The assertion this whole feature turns on, made against a disk kuma
    # has just written rather than against its intent.
    #
    # A resume_offset that does not describe the swapfile is the one
    # failure here that is silent in both directions: the machine
    # hibernates successfully, powers off, boots fresh, and the session is
    # gone with nothing logged. So the number in the boot entry is
    # compared against the number btrfs reports for the file itself, which
    # is the same question `kuma doctor` asks on a running machine.
    #
    # The swapfile is at the filesystem top level, beside the root
    # subvolume rather than inside it, because bootc requires the root it
    # installs onto to be empty.
    sudo mount -o subvolid=5 "/dev/mapper/$mapper" "$mnt" || bad "cannot mount the top level"
    [ -f "$mnt/swap/swapfile" ] || bad "--swap was asked for and there is no swapfile"
    local fs_uuid actual
    fs_uuid=$(sudo blkid -s UUID -o value "/dev/mapper/$mapper")
    [ "$resume_karg" = "resume=UUID=$fs_uuid" ] \
        || bad "the boot entry says $resume_karg, but the filesystem is UUID=$fs_uuid"
    actual=$(sudo btrfs inspect-internal map-swapfile -r "$mnt/swap/swapfile") \
        || bad "the kernel would refuse the swapfile kuma made"
    [ "$offset_karg" = "resume_offset=$actual" ] \
        || bad "the boot entry says $offset_karg, but the swapfile starts at page $actual"
    ok "the resume offset in the boot entry is the offset the swapfile has"
    sudo umount "$mnt"

    sudo cryptsetup close "$mapper"
    sudo losetup -d "$loop"
    trap - EXIT
    [ $KEEP -eq 1 ] || rm -f "$raw"
    ok "disk verified"
}

# The same three questions after every boot, asked in three places: is it
# reachable, has it finished starting, and does its own health check pass.
# The three copies were still identical when this was extracted (same
# 420s and 600s deadlines, same qemu-alive check, same greenboot verdict),
# which is the moment to do it rather than after one of them has quietly
# grown a fix the others lack.
#
# Calls `guest`, which each stage defines for its own connection before
# reaching here. That is the one implicit thing about it, and the reason
# it takes qemu and the log rather than reading them from scope too.
await_healthy_boot() {
    local qemu=$1 log=$2 reached=$3 healthy=$4 when=${5:-} ssh_deadline=${6:-420}

    local deadline=$((SECONDS + ssh_deadline))
    until guest true; do
        kill -0 "$qemu" 2>/dev/null || bad "qemu died${when}; console at $log"
        [ $SECONDS -lt $deadline ] || bad "no ssh within ${ssh_deadline}s${when}; console at $log"
        sleep 5
    done
    ok "$reached"

    # Let boot finish before judging it: a first boot creates the user and
    # converges flatpaks and brew, and greenboot runs after all of it.
    #
    # 1200s, and the number has a measurement behind it now. What
    # dominates a first boot is Homebrew's own bootstrap: kuma-brew-sync
    # downloads portable-ruby and then clones homebrew-core, which is a
    # large git repository over whatever network is available. A first
    # boot measured here reached "Initialized empty Git repository" one
    # second in and was still there nine minutes later, while flatpak
    # convergence had finished in under two minutes and greenboot in
    # eighteen. This deadline exists to bound a hang, not to assert a
    # speed, and at 600s it was failing runs over a slow clone.
    echo "   .. waiting for the boot to settle"
    deadline=$((SECONDS + 1200))
    until [[ "$(guest systemctl is-system-running)" =~ ^(running|degraded)$ ]]; do
        if [ $SECONDS -ge $deadline ]; then
            # What is still going, rather than only that something is.
            # Without this the message is "boot never settled" and the
            # only way on is to boot the disk by hand and ask it, which
            # is exactly what the first one of these cost.
            echo "   .. jobs still running:" >&2
            guest systemctl list-jobs --no-pager >&2 || true
            echo "   .. failed units:" >&2
            guest systemctl --failed --no-legend --no-pager >&2 || true
            bad "boot never settled${when}; console at $log"
        fi
        sleep 10
    done

    # The verdict from the machine's own health check rather than from
    # anything this script knows: on a desktop image a green greenboot
    # means the greeter came up, which is the regression class that boots
    # "fine" into a black screen.
    local verdict
    verdict=$(guest systemctl is-active greenboot-healthcheck.service || true)
    [ "$verdict" = active ] || bad "greenboot verdict${when}: $verdict (console at $log)"
    ok "$healthy"
}

# --- stage: published --------------------------------------------------
#
# Installs an image kuma published, then boots the disk that install
# wrote. Every other stage here builds its own image from the tree, so
# this is the only one that asks whether what is on the registry works,
# and the only one that can fail because of something nobody committed.
# It lives behind its own flag and its own workflow for that reason.
#
# It also reaches a branch nothing else does. `smoke_boot` already asks
# whether /var/home is its own btrfs subvolume, and on a
# bootc-image-builder disk it never can be: those roots are ext4 (see
# BIB_ROOTFS), so the check correctly says the question does not arise
# and has therefore never once run in anger. **Only `kuma install` writes
# btrfs**, so booting an installed disk is the only way that assertion
# executes, and `kuma-home-subvol` is the only thing standing between
# `[snapshots]` and an hourly timer that snapshots nothing.
#
# Unencrypted unless --encrypted asks, and the encrypted arm is why the
# console below is a socket rather than a file: a LUKS root stops in the
# initramfs for a passphrase, and this is the only stage with a console
# to type one on, so it is the only place "boots from the prompt up" is
# asked at all. `smoke_install` still covers the offline half, reading
# the container through a loop device instead of booting it.
smoke_published() {
    local image=$1 name=$2 port=$3
    local dir="vm-smoke/$name"
    local raw="$dir/disk.raw"
    local log="$dir/console.log"
    local user="smoketest"
    local pass="smoke-account-password"
    local disk_pass="smoke-disk-passphrase"
    local sock="$dir/console.sock"
    # The guest's memory and the hibernate swapfile are one decision, not
    # two: the doctor grades hibernate by whether the file can hold the
    # machine's image (a file under RAM grades `Short`, and this lap
    # asserts `ok`), so the swap size derives from this number below
    # rather than being written beside it. They drifted once: 3b9e010
    # bumped every qemu line to 8192 and left the 4G behind, and the
    # nightly went red for three days on an honest `Short`.
    local vm_mem_mib=8192

    mkdir -p "$dir"
    rm -f "$raw"
    truncate -s 24G "$raw"
    # Once per run, not once per boot. The chardev appends now, which is
    # what lets the boot that hibernates and the boot that resumes be read
    # side by side; the cost is that a failed run leaves its directory
    # behind and the next run's log opens with the previous run's tail.
    # Run 10's log began with three firmware banners and a GPT UUID that
    # belonged to a disk that no longer existed.
    : >"$log"
    # The unlock log appends across this run's boots for the same reason
    # — the boot that first unlocked and the boot the autologin gate
    # reboots into read side by side — so it is emptied once per run
    # here, beside the console it answers. A failed run leaves its
    # directory behind, and without this the next run's first-boot check
    # would read the previous run's "typed the passphrase" as this one's.
    : >"$dir/unlock.log"

    # --update-from only when the machine is meant to move somewhere else
    # later. It is the flag that says "install this, but track that", and
    # until now it was exercised only with a reference nothing ever
    # fetched, so this is the first time it points at something real.
    local update_from=()
    if [ -n "$UPGRADE_TO" ]; then
        update_from=(--update-from "$UPGRADE_TO")
    else
        case "$image" in
            # kuma refuses to record a localhost tag as an update source,
            # and it is right to: on the installed machine `localhost`
            # means itself, so the machine could never update. The
            # reference here is never fetched, exactly as in
            # smoke_install; it exists so the install has somewhere real
            # to write down.
            localhost/*) update_from=(--update-from ghcr.io/example/kuma:niri) ;;
        esac
    fi

    # One line on stdin without --encrypt, two with: the interview asks
    # for the disk passphrase first and the account password second, and
    # neither is ever a flag.
    local encrypt_args=() answers
    if [ $ENCRYPTED -eq 1 ]; then
        encrypt_args=(--encrypt)
        answers=$(printf '%s\n%s\n' "$disk_pass" "$pass")
    else
        answers=$(printf '%s\n' "$pass")
    fi

    # Passed rather than left to the interview because there is no
    # terminal here, and asked for explicitly rather than defaulted so
    # that a change to the default cannot silently make this stage test
    # a different thing. The size derives from vm_mem_mib above: a whole
    # gibibyte above the RAM the guest was given, which is what the
    # installer would propose for itself (MemTotal reads a little under
    # the RAM the machine was given).
    local swap_args=()
    [ $HIBERNATE -eq 1 ] && swap_args=(--swap "$((vm_mem_mib / 1024))G")

    echo "   .. installing $image (needs sudo; this is the slow part)"
    printf '%s\n' "$answers" \
        | "$KUMA" install --disk "$raw" --image "$image" \
            "${update_from[@]}" "${encrypt_args[@]}" "${swap_args[@]}" \
            --user "$user" --hostname smoketest --yes >/dev/null \
        || bad "installing $image failed"
    ok "installed $image${encrypt_args[*]:+ (encrypted)}${swap_args[*]:+ (with a swapfile)}"

    # A console the serial log can actually capture.
    #
    # The image sets no console= karg, correctly: a desktop has no reason
    # to log to a serial port. The consequence here is that the worst
    # failure produces the least evidence — a machine that never boots
    # writes firmware and GRUB output and then nothing, which is exactly
    # what a UEFI/BIOS mismatch looked like: a zero-byte log and a
    # seven-minute ssh timeout with no way to tell them apart. Added to
    # the installed disk rather than the image, so it is a property of
    # the thing under test here and not of what kuma ships.
    #
    # --hibernate needs two more kargs than the rest, and run 9 is why.
    # `quiet` is in the image's kargs and it is right to be, but the only
    # thing the hibernation path prints above it is `PM: Image not
    # found`. A resume that finds its image and then dies says nothing at
    # all, which on the console is indistinguishable from a resume that
    # worked: run 9 reset silently between the initramfs and real root
    # and left no word for either. Dropping `quiet` puts the rest of the
    # PM messages on the wire.
    #
    # no_console_suspend, because the console is suspended for exactly
    # the part of the restore most likely to kill the machine. Without it
    # those messages are produced and then dropped on the floor.
    local karg_edit='s/^options .*/& console=ttyS0/'
    [ $HIBERNATE -eq 1 ] \
        && karg_edit='s/ quiet / /; s/^options .*/& console=ttyS0 no_console_suspend/'

    local kloop kboot
    kloop=$(sudo losetup -fP --show "$raw") || bad "cannot attach $raw to add a console karg"
    kboot="$dir/bootmnt"
    mkdir -p "$kboot"
    if sudo mount "${kloop}p2" "$kboot" 2>/dev/null; then
        sudo sed -i "$karg_edit" "$kboot"/loader/entries/*.conf 2>/dev/null \
            && ok "serial console added to the boot entry" \
            || echo "   .. no loader entry to add a console to; the log will be firmware only" >&2
        sudo umount "$kboot"
    else
        echo "   .. could not mount /boot; the log will be firmware only" >&2
    fi
    sudo losetup -d "$kloop"
    rmdir "$kboot" 2>/dev/null || true

    # UEFI firmware, and this is not optional. `kuma install` writes a GPT
    # with an ESP, which is a UEFI layout; qemu defaults to SeaBIOS, which
    # finds nothing bootable and says so on the VGA console that
    # `-display none` throws away. The failure is therefore completely
    # silent: the install succeeds, the guest never boots, ssh times out
    # after seven minutes and the serial log is zero bytes. Nothing needed
    # this before because the boot stage's disks come from
    # bootc-image-builder and boot under BIOS.
    #
    # VARS is copied because pflash wants it writable, and a per-run copy
    # means EFI boot entries cannot leak from one run into the next.
    #
    # The plain pair always, because --secure-boot adds a boot rather
    # than replacing one. The first version of this stage booted
    # everything under Secure Boot and could not get past its own
    # CanHibernate check, which was the right answer to the wrong
    # question: a locked-down kernel refuses to hibernate, so a machine
    # under Secure Boot can never prove that resume works.
    local ovmf ovmf_code ovmf_vars
    ovmf=$(find_ovmf) \
        || bad "no OVMF firmware; an installed disk is UEFI and will not boot on SeaBIOS"
    ovmf_code=${ovmf%% *}
    ovmf_vars=${ovmf##* }
    cp "$ovmf_vars" "$dir/OVMF_VARS.fd"

    # And the Secure Boot pair beside it, for the second boot.
    local sb_code="" sb_vars=""
    if [ $SECURE_BOOT -eq 1 ]; then
        local sb
        sb=$(find_ovmf_secboot) \
            || bad "no Secure Boot OVMF firmware; --secure-boot cannot be answered here"
        sb_code=${sb%% *}
        sb_vars=${sb##* }
        cp "$sb_vars" "$dir/OVMF_VARS.secboot.fd"
        echo "   .. Secure Boot firmware, Microsoft's keys enrolled: $sb_code"
    fi

    # A console that can be typed into, not only read.
    #
    # `-serial file:` is write-only, which is fine until something has to
    # answer a prompt. An encrypted root stops in the initramfs asking
    # for a passphrase, so the socket form is what makes booting one
    # testable at all; the chardev logs to the same file either way, so
    # the artifact is unchanged. Both cases use it, because two console
    # paths would mean the encrypted one is the only one nobody exercises.
    #
    # logappend, because this stage boots the same disk more than once
    # and qemu truncates a chardev logfile when it opens it. The boot
    # that hibernated was overwritten by the boot that tried to resume,
    # so the two halves of the claim could never be read side by side.
    local serial=(-chardev "socket,id=con,path=$sock,server=on,wait=off,logfile=$log,logappend=on"
                  -serial chardev:con)

    # The passphrase typed at the console, factored out of boot_vm
    # because the autologin gate below needs it a second time: its
    # reboot stops an encrypted root for the passphrase exactly as the
    # first boot does, and by then the unlocker that answered there has
    # been killed once ssh came up. Appends rather than truncates, so
    # every boot's answer sits in one artifact the way every boot's
    # console sits in one log; the once-per-run emptying lives at the
    # top of the stage, beside the console's.
    # Sets `unlocker` for the caller, 0 when this stage never encrypted.
    start_unlocker() {
        unlocker=0
        if [ $ENCRYPTED -eq 1 ]; then
            echo "   .. answering the passphrase prompt on the console"
            scripts/console-unlock.py "$sock" "$disk_pass" 420 \
                >>"$dir/unlock.log" 2>&1 &
            unlocker=$!
        fi
    }

    # A function rather than a command, because --hibernate boots this
    # same disk twice and the second boot has to be identical to the
    # first. Two copies of a fifteen-argument qemu line is two chances
    # for the resume to be measured against a machine that differs from
    # the one that hibernated, which is the one difference this stage
    # cannot afford. It sets `qemu` and `unlocker` for the caller.
    local qemu=0 unlocker=0
    boot_vm() {
        # "plain" or "secure". Secure Boot needs SMM and a pflash marked
        # secure: OVMF keeps the authenticated variables that hold the
        # enrolled keys in System Management RAM, so without both the
        # firmware comes up reporting Secure Boot disabled and the test
        # quietly measures nothing.
        #
        # disable_s3 rides with it, because OVMF's own guidance is that
        # S3 and Secure Boot together are unsafe: a resume from RAM
        # re-enters the firmware without re-authenticating. S4 is
        # untouched, and S4 is what hibernate uses.
        local mode=${1:-plain} code=$ovmf_code vars="$dir/OVMF_VARS.fd"
        local machine=(-machine q35) globals=()
        if [ "$mode" = secure ]; then
            code=$sb_code
            vars="$dir/OVMF_VARS.secboot.fd"
            machine=(-machine "q35,smm=on")
            globals=(-global "driver=cfi.pflash01,property=secure,value=on"
                     -global "ICH9-LPC.disable_s3=1")
        fi
        qemu-system-x86_64 \
            -enable-kvm -cpu host -smp 4 -m "$vm_mem_mib" \
            "${machine[@]}" "${globals[@]}" \
            -drive "if=pflash,format=raw,readonly=on,file=$code" \
            -drive "if=pflash,format=raw,file=$vars" \
            -drive "file=$raw,if=virtio,format=raw" \
            -device "$QEMU_VGA" -display "$QEMU_DISPLAY" \
            -nic "user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$port-:22" \
            "${serial[@]}" &
        qemu=$!

        # Typed while the boot is still in the initramfs, so this runs
        # beside the wait rather than before it. A resume from an
        # encrypted disk stops for the passphrase exactly as a cold boot
        # does: the initramfs has to open the container before it can
        # read the swapfile it is resuming from.
        start_unlocker
        # EXIT rather than RETURN, for the reason smoke_boot gives: a
        # failed assertion leaves this subshell without ever returning.
        # The unlocker goes with it, or a failed encrypted run leaves a
        # python process holding a socket in a directory the cleanup is
        # about to delete. Re-armed on every boot, because the pids
        # change and a trap holding the old ones kills nothing.
        #
        # The unlocker pid rides only when there is one. An unencrypted
        # stage leaves it 0, and `kill 0` signals the whole process
        # group, this script included: the trap then took the harness
        # down beside qemu, the step ended 143, GitHub read it as
        # cancelled rather than failed, and every `if: failure()` or
        # `cancelled()` capture step was skipped. The one run that
        # needed the console produced no console at all, 2026-08-21,
        # and again 2026-08-22 when the real-session gate went red.
        # shellcheck disable=SC2064
        if [ "$unlocker" != 0 ]; then
            trap "kill $qemu $unlocker 2>/dev/null || true" EXIT
        else
            trap "kill $qemu 2>/dev/null || true" EXIT
        fi
    }
    boot_vm

    # Password auth, because `kuma install` has no way to plant a key:
    # the account it creates exists only on the installed machine and
    # nothing has ever logged into it. PubkeyAuthentication=no keeps a
    # runner's own agent from being offered first and eating the attempt.
    #
    # ServerAlive*, because this stage now asks a machine to disappear on
    # purpose. ConnectTimeout only bounds the handshake; a connection
    # that is already open when the guest stops existing has nothing to
    # notice it, and waits on TCP for as long as the kernel allows. Three
    # missed probes at five seconds gives up in fifteen.
    local ssh_opts=(-p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
                    -o ConnectTimeout=5 -o LogLevel=ERROR
                    -o ServerAliveInterval=5 -o ServerAliveCountMax=3
                    -o PubkeyAuthentication=no -o PreferredAuthentications=password
                    "$user@127.0.0.1")
    # shellcheck disable=SC2029  # client-side expansion is the point.
    guest() { sshpass -p "$pass" ssh "${ssh_opts[@]}" "$@" 2>/dev/null; }

    # sudo over ssh has no terminal to ask on, and this account is in
    # wheel rather than NOPASSWD, so the password goes in on stdin,
    # piped locally: in no argv, not ssh's and not the guest's echo's,
    # which is where the earlier `echo '$pass' |` spelling put it for
    # the guest's ps to see. `-p ''` drops the prompt, which would
    # otherwise land in the output being parsed.
    gsudo() { guest "sudo -S -p '' $*" <<<"$pass"; }

    # One ssh hop into a machine that has just booted or just resumed is
    # this stage's one flaky thing. The plain hibernate cycle learned it
    # as "Run 12" and retries its post-resume /proc/uptime read by hand;
    # the suspend-then-hibernate cycle learned it as ci 33127646466
    # (2026-08-27), which failed at "could not stage the harness's short
    # hibernate delay" with every resume assertion already green, and a
    # rerun of the same commit went green. Retry rather than trust the
    # first packet after a wake, with a deadline rather than a bare loop
    # so a machine that is genuinely unreachable still fails this stage
    # on purpose instead of hanging CI. For reads and staging only: a
    # command that changes what the machine is doing, like the suspend
    # start, must not be retried, because the retry cannot tell a lost
    # packet from an answer it did not want.
    guest_retry() {
        local deadline=$((SECONDS + 90)) out
        until out=$(guest "$@"); do
            [ $SECONDS -lt $deadline ] || return 1
            sleep 5
        done
        printf '%s\n' "$out"
    }

    # Parsed on this side, never in the guest: anything with quotes in it
    # loses them crossing ssh, and a python one-liner is all quotes.
    booted_digest() {
        gsudo bootc status --format json \
            | python3 -c 'import sys,json; print(json.load(sys.stdin)["status"]["booted"]["image"]["imageDigest"])' \
            2>/dev/null || true
    }

    # One named check's grade, so an assertion can say what it expects
    # instead of scanning for whatever failed. Scanning only finds `fail`,
    # and a check whose bad states are graded `warn` is invisible to it.
    # "absent" rather than empty when doctor has no such check, so a
    # renamed check reads as a missing answer instead of a passing one.
    doctor_grade() {
        gsudo kuma doctor --json \
            | python3 -c 'import sys,json; print(next((c["grade"] for c in json.load(sys.stdin)["checks"] if c["name"]==sys.argv[1]), "absent"))' "$1" \
            2>/dev/null || true
    }

    # The words beside the grade. A check can be the right grade for the
    # wrong reason, and the Secure Boot half below needs to know that
    # doctor's warning actually names lockdown rather than warning about
    # something else entirely.
    doctor_detail() {
        gsudo kuma doctor --json \
            | python3 -c 'import sys,json; print(next((c["detail"] for c in json.load(sys.stdin)["checks"] if c["name"]==sys.argv[1]), ""))' "$1" \
            2>/dev/null || true
    }

    echo "   .. waiting for ssh on $port"
    # 1800, not 420: this is a first boot, and the first boot recomposes
    # kuma's composed base before sshd ever starts - 553 packages, measured
    # at roughly a quarter hour on this host and slower on a runner's
    # disk. The 420s deadline predates the composed base (e1d943c) and
    # cannot see sshd through the deploy.
    await_healthy_boot "$qemu" "$log" \
        "installed machine booted and is reachable" \
        "greenboot says this boot is healthy" "" 1800

    # Reaching ssh at all proves the root was unlocked, but not that the
    # passphrase did it: a machine that never encrypted anything also
    # boots. So the claim is checked against what the console actually
    # saw, which is the difference between testing encryption and testing
    # that a disk boots.
    if [ $ENCRYPTED -eq 1 ]; then
        kill "$unlocker" 2>/dev/null || true
        wait "$unlocker" 2>/dev/null || true
        grep -q 'typed the passphrase' "$dir/unlock.log" 2>/dev/null \
            || { cat "$dir/unlock.log" >&2 2>/dev/null || true
                 bad "no passphrase prompt appeared; console at $log"; }
        ok "the encrypted root unlocked from a passphrase typed at the console"
    fi

    # The reason this stage exists. A `kuma install` root is btrfs, so
    # "not btrfs" is a failure rather than a question that does not arise.
    local home_fs home_inode home_was_subvol=no
    home_fs=$(guest findmnt -no FSTYPE -T /var/home)
    [ "$home_fs" = btrfs ] || bad "/var/home is $home_fs on an installed disk; expected btrfs"
    home_inode=$(guest stat -c %i /var/home)
    [ "$home_inode" = 256 ] && home_was_subvol=yes

    # Read before any upgrade, because the interesting question in that
    # path is whether this changes. Measured rather than assumed from the
    # version under test: `--published` takes any reference, so "the old
    # one predates the policy" is only true of the fixtures used today.
    local signatures_before
    signatures_before=$(doctor_grade signatures)

    # Strict only when the image under test is meant to have the
    # converger. In the cross-version path the point is to install a
    # version from BEFORE a fix and see what upgrading does about it, so
    # its absence is the premise rather than the failure. Reported either
    # way, because a silent skip here would read as a pass.
    # An ordering cycle does not stop a boot. systemd breaks one by
    # deleting a job, and the job it deletes could be the converger's,
    # with the machine coming up healthy and one unit having silently
    # never run. Widening a unit's Before= is precisely the change that
    # introduces one, so this is checked rather than assumed.
    if gsudo journalctl -b --no-pager | grep -qi 'ordering cycle'; then
        gsudo journalctl -b --no-pager | grep -i -A 3 'ordering cycle' >&2 || true
        bad "systemd broke an ordering cycle this boot; a unit may never have run"
    fi
    ok "no ordering cycle this boot"

    # Collected the same way whichever fault fired, because the two look
    # identical from outside and the guest's kernel log never reaches the
    # serial console: the installed image sets no console= karg, so this
    # is the only chance to ask while the machine is still up.
    # What the machine says about a unit that went wrong, for any unit.
    #
    # Written for kuma-home-subvol and immediately needed for firewalld,
    # which is the argument for not writing it per unit: the failure
    # worth diagnosing is rarely the one anticipated, and a guest that
    # has already been powered off cannot be asked anything.
    unit_evidence() {
        local unit
        for unit in "$@"; do
            echo "   .. $unit says:" >&2
            gsudo systemctl --no-pager -l status "$unit" >&2 2>&1 || true
            gsudo journalctl --no-pager -b -u "$unit" >&2 2>&1 || true
            echo "   .. what ran before $unit:" >&2
            gsudo systemd-analyze critical-chain "$unit" >&2 2>&1 || true
        done
    }

    home_evidence() {
        unit_evidence kuma-home-subvol.service
        echo "   .. /var/home contains:" >&2
        gsudo ls -Al /var/home >&2 2>&1 || true
    }

    # Every converger kuma ships, not one named unit.
    #
    # `systemctl is-system-running` reports a unit that died and a unit
    # that declined identically as "degraded", and this stage accepts
    # degraded as settled, so a dead converger hides in the aggregate.
    # kuma-home-subvol was found that way only because it was asked about
    # by name, after hiding for an unknown number of releases. Asking
    # about the whole family costs nothing and catches the next one.
    local failed_kuma
    failed_kuma=$(guest systemctl list-units --failed --plain --no-legend \
        | awk '{print $1}' | grep '^kuma-' || true)
    if [ -n "$failed_kuma" ]; then
        case "$failed_kuma" in *kuma-home-subvol*) home_evidence ;; esac
        bad "failed kuma units: $(echo "$failed_kuma" | tr '\n' ' ')(console at $log)"
    fi
    ok "no kuma unit failed"

    if [ -z "$UPGRADE_TO" ]; then
        if [ "$home_was_subvol" != yes ]; then
            # Say why, not just that. This has come back intermittently on
            # the same published image (two subvolumes and one directory
            # across three boots), and the guest's kernel log never
            # reaches the serial console because the installed image sets
            # no console= karg, so the evidence has to be collected here
            # while the machine is still up.
            home_evidence
            bad "/var/home is not a subvolume (inode $home_inode); snapshots would take nothing"
        fi
        ok "/var/home is its own subvolume"
    elif [ "$home_was_subvol" = yes ]; then
        ok "/var/home is its own subvolume before upgrading"
    else
        ok "/var/home is NOT a subvolume on $image (inode $home_inode), which is what upgrading is being asked about"
    fi

    # The machine's own verdict, not this script's reading of it.
    #
    # Everything above asks a question this harness knows how to ask.
    # doctor asks every question kuma knows how to ask, so checking that
    # it finds nothing failing means each check added to doctor becomes a
    # boot assertion with no change here. Skipped in the upgrade path,
    # where the whole point is a machine installed before a fix and
    # therefore known to be deficient; that path checks doctor's verdict
    # on the specific thing it is testing instead.
    if [ -z "$UPGRADE_TO" ]; then
        local doctor_failing
        # Name and detail, not just the check's name: "units" on its own
        # says a unit failed without saying which, and doctor already
        # knows which.
        doctor_failing=$(gsudo kuma doctor --json \
            | python3 -c 'import sys,json; print("; ".join(c["name"] + ": " + c["detail"] for c in json.load(sys.stdin)["checks"] if c["grade"] == "fail"))' \
            2>/dev/null || true)
        if [ -n "$doctor_failing" ]; then
            echo "   .. units the machine considers failed:" >&2
            local failed_units
            failed_units=$(guest systemctl list-units --failed --plain --no-legend \
                | awk '{print $1}' || true)
            echo "$failed_units" >&2
            # shellcheck disable=SC2086  # each name is a separate argument
            [ -z "$failed_units" ] || unit_evidence $failed_units
            bad "kuma doctor fails: $doctor_failing"
        fi
        ok "kuma doctor finds nothing failing"

        # kuma's verbs reach the desktop through freedesktop desktop
        # entries. A malformed one is not an error anywhere: every
        # launcher skips it in silence, so the symptom is a verb that is
        # simply absent, on a surface no automated boot would otherwise
        # touch. The build validates what it generates; this checks that
        # what it generated survived into a booted machine.
        #
        # ONE STRING, not `guest sh -c '...'`. ssh joins its arguments
        # with spaces and hands the result to the guest's login shell,
        # which re-parses it: the quotes are this side's and never
        # arrive. `sh -c ls <paths>` runs a bare `ls` with the paths as
        # $0 and $1, so it listed $HOME, which on a fresh install is
        # empty. The check reported "found 0" on a machine carrying all
        # eight, and only the install stage could ever see it.
        # Koguma's entry is excluded from the seam's count: the eight
        # generated verbs are the seam's own contract, and Koguma's
        # entry is a real app's, shipped from the kumaui tree — one
        # glob catches both, and only the count here is entitled to be
        # exactly eight.
        local seam_entries
        seam_entries=$(guest 'ls /usr/share/applications/kuma-*.desktop 2>/dev/null | grep -v kuma-files | wc -l' || echo 0)
        [ "$seam_entries" -eq 8 ] || bad "expected 8 seam entries, found $seam_entries"
        guest 'desktop-file-validate /usr/share/applications/kuma-*.desktop' \
            || bad "kuma's desktop entries do not validate on the booted machine"
        guest test -x /usr/libexec/kuma-launch \
            || bad "the entries' Exec is not executable on the booted machine"
        ok "the seam ships $seam_entries entries and they validate"

        # Koguma, on its own: the entry beside the eight generated
        # verbs, the Exec the entry names, and the icon it asks for —
        # the same three questions the build asked, answered on the
        # booted machine. The validate glob above already carries it.
        guest 'test -f /usr/share/applications/kuma-files.desktop' \
            || bad "Koguma ships no desktop entry on the booted machine"
        guest 'command -v kuma-files >/dev/null' \
            || bad "Koguma's entry names kuma-files, which is not in the image"
        guest 'test -f /usr/share/icons/hicolor/256x256/apps/kuma-files.png' \
            || bad "Koguma's icon did not land where the theme looks for it"
        ok "Koguma ships installed, entry and icon and all"

        # The shell, asked the only way this stage can ask.
        #
        # THERE IS NO SESSION HERE. The installed machine declares no
        # [user], so greetd autologins nobody, niri never starts and
        # neither does the shell. The check this replaced was written as
        # `kuma menu --list` with "without a display" in its first line
        # for exactly that reason, and asserting that noctalia is
        # RUNNING failed a machine that was perfectly correct.
        #
        # So what gets asked is the wiring: the shell is in the image,
        # and the session starts it under supervision. Whether it then
        # draws a bar is a question only a real session answers, and
        # nothing in CI has one.
        #
        # Asked of the niri image only, and asked by its config rather
        # than by the tag: --published takes any image, and a COSMIC one
        # failing these would be this harness reporting the wrong
        # desktop rather than a broken one.
        #
        # The shell ships no config file of its own — the unit's env pair
        # is the whole surface the session hands it — so the probes here
        # are the binary and the unit. The noctalia config probes this
        # block carried left the rename in everything but their message
        # strings, and no run reached them until one survived the gates
        # ahead of this one; the first image to get this far failed them
        # while carrying the shell the whole time.
        if guest test -f /etc/niri/config.kdl; then
            guest 'command -v kuma-shell >/dev/null' \
                || bad "the shell is not in the image"
            # Started by a SUPERVISED unit, not a niri spawn. A spawn
            # lands in a transient scope, a scope cannot restart, and
            # every lock on this desktop runs through that one process,
            # so a crash took the lock screen with it silently.
            guest 'test -f /usr/lib/systemd/user/kuma-shell.service' \
                || bad "the image ships no unit to run the shell"
            guest 'test -L /etc/systemd/user/graphical-session.target.wants/kuma-shell.service' \
                || bad "kuma-shell.service is not enabled, so nothing starts the shell"
            guest 'grep -q Restart=always /usr/lib/systemd/user/kuma-shell.service' \
                || bad "the shell unit would not come back from a crash"
            ok "the shell is installed and started under supervision"

            # And the guard that refuses to sleep without it.
            guest 'test -x /usr/libexec/kuma-sleep-guard' \
                || bad "no sleep guard, so a shell-less session suspends unlocked"
            guest 'systemctl is-enabled kuma-sleep-guard.service' >/dev/null \
                || bad "kuma-sleep-guard.service is not enabled"
            ok "a session with no shell ends rather than suspending unlocked"

            # INSTALLED, not running, and that is the stronger question.
            # niri Recommends alacritty, waybar, swaylock and fuzzel, so
            # dropping a name from NIRI_PACKAGES does not remove it and
            # the image quietly keeps a bar and a lock screen nothing
            # starts. That trap was found by hand once, by building the
            # image and asking it, and nothing has guarded it since.
            local displaced
            for displaced in waybar mako fuzzel swaylock swayidle swaybg wob wlsunset; do
                guest "command -v $displaced >/dev/null" \
                    && bad "$displaced is installed beside the shell that replaced it"
            done
            ok "nothing the shell replaced survived into the image"

            # Mod+D, read out of the baked config rather than assumed.
            # niri's stock bind spawns fuzzel, which this image does not
            # have, so a merge that stopped substituting leaves the
            # most-used key on the machine spawning nothing at all. The
            # probe reads the bind's own tokens; the rename (dd38c18)
            # changed the bind and left this regex behind, and no run
            # reached the difference until one survived the gates ahead.
            guest 'grep -qE "Mod\\+D.*kuma-shell.*launcher-toggle" /etc/niri/config.kdl' \
                || bad "Mod+D does not open the shell's launcher in the baked config"
            guest grep -q fuzzel /etc/niri/config.kdl \
                && bad "the baked niri config still names fuzzel, which is not in the image"
            ok "Mod+D opens the launcher, and no bind names a program that left"

            # ---- a real login, and the three things only a session can
            # answer.
            #
            # Everything above is the image. Nothing in CI had ever run a
            # kuma DESKTOP: the published stage installs from a
            # declaration with no [user], so greetd autologins nobody and
            # niri never starts. That is why "the shell owns
            # notifications", "it registers the lock-before-suspend
            # inhibitor" and the fail-open lock itself were all measured
            # by hand on somebody's laptop and by nothing else.
            #
            # greetd's [initial_session] is the autologin kuma already
            # writes when a declaration asks for it, so this asks for it
            # here and reboots: greetd honors the block only on a boot's
            # first start, which is also the only way a person gets the
            # session — there is no restart path to it.
            # Only when the image does not already autologin. Appending
            # a second [initial_session] is invalid TOML and greetd would
            # refuse to start, which would fail this gate for a reason
            # that has nothing to do with the desktop. The declaration
            # this stage installs from declares no [user], so today it
            # never has one; a future one might.
            if guest 'grep -q "^\[initial_session\]" /etc/greetd/config.toml'; then
                ok "the image already autologins; the session is a real one either way"
            else
                # Staged in /tmp and appended by a sudo'd cat, never by a
                # `sudo printf ... >> /etc/...`: the remote shell opens
                # the redirection as the unprivileged user, so the write
                # is denied and the failure hides in guest's 2>/dev/null,
                # with set -e off in this subshell to keep it quiet.
                # Measured 2026-08-22: the block never landed, greetd
                # fell back to its greeter, and the shell gate below read
                # a harness bug as a product bug.
                guest "printf '\n[initial_session]\ncommand = \"niri-session\"\nuser = \"$user\"\n' \
                    > /tmp/kuma-initial-session" \
                    || bad "could not stage the autologin block in the guest"
                gsudo "sh -c 'cat /tmp/kuma-initial-session >> /etc/greetd/config.toml'"
                gsudo 'grep -q "^\[initial_session\]" /etc/greetd/config.toml' \
                    || bad "the autologin block never landed in greetd's config"

                # Reboot, not `systemctl restart greetd`: greetd runs
                # initial_session only on the first start of a boot, and
                # a restart opens the greeter instead. Measured twice on
                # 2026-08-22 — once in this stage's journal, once by hand
                # against the same disk, which then booted straight into
                # niri. What this gate asserts is that the machine BOOTS
                # into the session, so boot it. The wait keys on boot_id
                # rather than ssh answering, because sshd keeps answering
                # for the old boot for several seconds after the call.
                #
                # An encrypted root stops for its passphrase on this
                # boot too, and the first time this gate met one — the
                # 0.18 publish, 2026-08-28 — nobody answered it: the
                # unlocker that typed it on the first boot had been
                # killed as soon as ssh came up, so the machine sat in
                # the initramfs for all 300s and the run read "did not
                # come back" when the truth was "nobody typed". A second
                # unlocker now starts before the call, so it is watching
                # the console before the prompt arrives; its window
                # (420s) outlives the wait (300s), so it cannot give up
                # on a boot this gate is still willing to take.
                local old_boot new_boot reboot_deadline unlock_mark
                old_boot=$(guest 'cat /proc/sys/kernel/random/boot_id')
                # Where this reboot's half of the unlock log starts, so
                # the readback below cannot be answered by the first
                # boot's record. Same mark the console checks use.
                unlock_mark=$(( $(stat -c %s "$dir/unlock.log" 2>/dev/null || echo 0) + 1 ))
                start_unlocker
                # Re-armed with the new unlocker's pid, for the reason
                # boot_vm gives: a failed assertion has to take this
                # unlocker with it, or the run leaves a python process
                # holding a console socket in a directory the cleanup is
                # about to delete.
                # shellcheck disable=SC2064
                if [ "$unlocker" != 0 ]; then
                    trap "kill $qemu $unlocker 2>/dev/null || true" EXIT
                fi
                gsudo "systemd-run --no-block systemctl reboot" || true
                reboot_deadline=$((SECONDS + 300))
                until new_boot=$(guest 'cat /proc/sys/kernel/random/boot_id') \
                    && [ -n "$new_boot" ] && [ "$new_boot" != "$old_boot" ]; do
                    kill -0 "$qemu" 2>/dev/null \
                        || bad "qemu died under the autologin reboot; console at $log"
                    [ $SECONDS -lt $reboot_deadline ] \
                        || bad "the machine did not come back from its autologin reboot"
                    sleep 5
                done
                # The same readback the first boot does, bounded to this
                # reboot's half of the log: reaching ssh proves the root
                # unlocked, but only the log proves the passphrase did
                # it, and a reboot that somehow stopped prompting is a
                # different machine than the one this gate claims to
                # reboot.
                if [ "$unlocker" != 0 ]; then
                    kill "$unlocker" 2>/dev/null || true
                    wait "$unlocker" 2>/dev/null || true
                    tail -c "+$unlock_mark" "$dir/unlock.log" 2>/dev/null \
                        | grep -q 'typed the passphrase' \
                        || bad "no passphrase was typed for the rebooted machine; console at $log"
                    ok "the encrypted root unlocked again from a passphrase typed at the console"
                fi
                ok "rebooted into the boot the autologin belongs to"
            fi

            # seat0 alone is not the question: greetd's own greeter holds
            # a seat0 session on tty1, and `grep seat0` passed on it all
            # night on 2026-08-22 while no user session existed. The
            # question is a seat0 session belonging to the declared user.
            local session_deadline=$((SECONDS + 120))
            until guest "loginctl list-sessions --no-legend \
                | grep -qE '^ *[a-z0-9]+ +[0-9]+ +$user +seat0( |$)'"; do
                [ $SECONDS -lt $session_deadline ] \
                    || bad "no graphical session for $user within 120s of enabling autologin"
                sleep 5
            done
            ok "the greeter logged $user into a session"

            # The shell's unit has to actually be started by the session.
            # What CI cannot ask is whether it is DRAWING: the runner's VM
            # has no GPU, niri's outputs die on the early import
            # (DeviceMissing on the render node), and the shell's layer
            # surfaces come back closed — which noctalia survived by
            # staying alive with no surfaces, and kuma-shell does not
            # (GPUI exits on a window it cannot find, and the unit
            # restart-loops into start-limit). Whether the shell renders
            # is a question only real hardware answers; what a headless
            # session proves is that the session started the unit, and
            # that supervision held on the way down.
            # The system journal, text-grepped — the dump's own proven
            # shape: the user manager forwards unit lines there, and this
            # account reads them back (the failure dump proves both).
            # Polled, because the unit starts when the session's target
            # does — seconds after loginctl can already see the session.
            local shell_deadline=$((SECONDS + 60))
            until guest 'journalctl -b --no-pager | grep -q "Started kuma-shell.service"'; do
                [ $SECONDS -lt $shell_deadline ] || bad "the shell never started in a real session"
                sleep 5
            done
            ok "the session started the shell's unit; whether it draws is real hardware's question"

            # Notifications: mako left with the swap, and the shell took
            # the name. Since kumaui's idle-without-windows fix the shell
            # holds the name for the session's life, displayless or not;
            # polled anyway, because the unit starts seconds after
            # loginctl can already see the session.
            local owner_call='busctl --user call org.freedesktop.DBus'
            owner_call="$owner_call /org/freedesktop/DBus org.freedesktop.DBus"
            owner_call="$owner_call GetNameOwner s org.freedesktop.Notifications"
            local notif_deadline=$((SECONDS + 60))
            until guest "XDG_RUNTIME_DIR=/run/user/\$(id -u) $owner_call" >/dev/null; do
                [ $SECONDS -lt $notif_deadline ] || bad "nothing owns org.freedesktop.Notifications in a live session"
                sleep 5
            done
            ok "the shell owns org.freedesktop.Notifications"

            # Koguma, launched the way a session would launch it: through
            # the user manager, which niri-session has already imported
            # the session's environment into — the same road autostart
            # apps ride — so the app gets its display. Launched, not
            # assumed present: this is the image's first user-facing
            # kumaui app and the first regular-window GPUI program on
            # this qemu road, and a shipped app that cannot launch is
            # exactly the class of thing this smoke exists to catch. The
            # activation socket is the app's own single-instance answer:
            # the process is alive and it holds the port a second
            # instance would take.
            guest 'systemd-run --user --unit=kuma-files-smoke kuma-files' \
                || bad "kuma-files would not launch from the session's user manager"
            local koguma_deadline=$((SECONDS + 60))
            # shellcheck disable=SC2016  # $(id -u) must expand on the guest, not here
            until guest 'pgrep -x kuma-files >/dev/null && test -S "/run/user/$(id -u)/kuma-files.sock"'; do
                [ $SECONDS -lt $koguma_deadline ] \
                    || bad "kuma-files launched but never answered: no process or no activation socket"
                sleep 5
            done
            ok "Koguma launched in the session and answered on its activation socket"
            # Closed by its own unit, not pkill: the probe launched it
            # through the user manager, so the user manager is also the
            # one that can prove it comes back down.
            guest 'systemctl --user stop kuma-files-smoke.service'
            guest pgrep -x kuma-files >/dev/null \
                && bad "kuma-files survived its own unit being stopped"

            # Lock before suspend: kuma-shell deliberately holds no logind
            # delay inhibitor — the shipped sleep guard's own prose says
            # so, so a hung shell cannot stall sleep the way the hung
            # noctalia could — and lock-before-suspend is niri's
            # `lock_before_suspend`, best-effort, same as any locker.
            # A probe here asserts noctalia's contract, not this
            # desktop's. The guard below is the property's readback: a
            # session with no shell ends instead of suspending unlocked.

            # And the guard for when the shell is not there at all.
            #
            # THE SESSION IT KILLED, by id, not "any seat0 session":
            # greetd puts the greeter back the moment a session ends, so
            # a fresh seat0 session appears within a second and "no
            # graphical session" would be false while the property still
            # held. Naming the id is the difference between testing the
            # thing and testing the timing.
            local before_id
            before_id=$(guest "loginctl list-sessions --no-legend | awk '\$4 == \"seat0\" {print \$1; exit}'")
            [ -n "$before_id" ] || bad "no seat0 session to test the sleep guard against"
            guest 'systemctl --user stop kuma-shell.service'
            guest pgrep -x kuma-shell >/dev/null \
                && bad "the shell survived its own unit being stopped"
            gsudo "/usr/libexec/kuma-sleep-guard" || true
            guest "loginctl list-sessions --no-legend | awk '{print \$1}' | grep -qx '$before_id'" \
                && bad "session $before_id had no shell and the guard left it to suspend into"
            ok "a session with no shell is ended rather than suspended into"
        fi

        # Named rather than left to the scan above, because the ways this
        # control goes missing are all graded `warn`: no policy file, one
        # that will not parse, or one that does not name kuma's
        # repository. Only a policy that names a key it does not have, or
        # one with nowhere to look for signatures, grades `fail`. So the
        # scan sees the half-broken states and is blind to the absent
        # one, which is the likeliest of the three and the one an /etc
        # merge can cause.
        #
        # `ok` is the requirement rather than "not fail" because every
        # image writes the policy, the key and the registries.d entry
        # unconditionally (containerfile.rs: "on every image rather than
        # only on published ones"). A machine that boots a kuma image and
        # does not require kuma's signature has lost something between
        # the image and the deployment.
        local signatures_grade
        signatures_grade=$(doctor_grade signatures)
        [ "$signatures_grade" = ok ] || bad \
            "doctor grades signatures '$signatures_grade': this machine does not refuse an unsigned kuma image"
        ok "doctor grades signatures ok: an update that is not kuma's is refused"
    fi

    # The account the installer was told to make, on the machine it made
    # it on. Nothing before this stage has booted a disk whose user came
    # from the install interview rather than from a declaration.
    guest id "$user" >/dev/null || bad "$user does not exist on the installed machine"
    guest id -nG "$user" | tr ' ' '\n' | grep -qx wheel || bad "$user is not in wheel"
    [ "$(guest hostnamectl hostname)" = smoketest ] || bad "hostname did not converge"
    ok "the installed account and hostname converged"

    # --- the hibernate half --------------------------------------------
    #
    # Every other assertion in this file asks whether a machine booted.
    # This one asks whether it is the SAME boot, because that is the only
    # question hibernate turns on. A machine that writes its memory to
    # disk, powers off, and then starts fresh is indistinguishable from a
    # working one by every check above: ssh answers, greenboot is green,
    # the account is there. What is gone is whatever was open, and nothing
    # logs it.
    if [ $HIBERNATE -eq 1 ]; then
        # Both swap areas, which is the measurement behind a claim kuma's
        # design rests on and had only ever asserted: every image ships
        # zram-generator-defaults, so the machine has compressed swap in
        # memory at priority 100, and you cannot hibernate into memory.
        # If systemd were to choose that one there would be nowhere to
        # write an image to.
        local swaps
        swaps=$(guest "cat /proc/swaps" || true)
        grep -q '/var/swap/swapfile' <<<"$swaps" \
            || bad "the swapfile is not active swap; /proc/swaps says: $swaps"
        grep -q 'zram' <<<"$swaps" \
            || bad "zram is not active, so this run cannot say systemd picked the file over it"

        # And in the right order. The point of giving the file a negative
        # priority is that ordinary paging keeps going to zram, which is
        # faster, and the disk is only reached under real pressure. The
        # property is what is asserted rather than the exact number: the
        # first run to look found the kernel had assigned -1 where the
        # fstab line asked for -2, and -1 is just as far below zram's
        # 100, so pinning the number would fail a machine that is right.
        local zram_pri file_pri
        zram_pri=$(awk '$1 ~ /zram/ { print $NF }' <<<"$swaps")
        file_pri=$(awk '$1 == "/var/swap/swapfile" { print $NF }' <<<"$swaps")
        if [ -z "$zram_pri" ] || [ -z "$file_pri" ]; then
            bad "cannot read swap priorities from: $swaps"
        fi
        [ "$file_pri" -lt "$zram_pri" ] \
            || bad "the swapfile has priority $file_pri against zram's $zram_pri, so paging would hit the disk first"
        ok "both swap areas are active, and the file ($file_pri) sits below zram ($zram_pri)"

        # doctor's verdict before anything is asked to sleep. This is what
        # ties the check to reality: doctor compares the resume_offset the
        # kernel was given against the offset the file actually has, and
        # if that comparison is wrong here, the resume below is what
        # proves it wrong.
        local hib_grade
        hib_grade=$(doctor_grade hibernate)
        [ "$hib_grade" = ok ] || bad \
            "doctor grades hibernate '$hib_grade' on a machine installed with --swap"
        ok "doctor grades hibernate ok before the machine is asked to do it"

        # The lid's verdict beside it: this machine was installed with
        # --swap, so the install wrote the suspend-then-hibernate setting
        # and doctor must say so. This is what ties that file, which the
        # disk asserts in the install stage, to the running machine's
        # account of itself.
        local lid_grade
        lid_grade=$(doctor_grade lid)
        [ "$lid_grade" = ok ] || bad \
            "doctor grades lid '$lid_grade' on a machine installed with --swap"
        ok "doctor grades the lid suspend-then-hibernate before the machine is asked to do it"

        # Whether the kernel will do it at all, asked of the file that
        # decides: /sys/power/state lists `disk` only when
        # hibernation_available() says so. A file rather than a service,
        # so it cannot be unavailable for reasons of its own, and it is
        # the same file doctor reads.
        local offers
        offers=$(guest "cat /sys/power/state 2>&1" || true)
        grep -qw disk <<<"$offers" \
            || bad "the kernel does not offer hibernation (/sys/power/state: ${offers:-unreadable})"
        ok "the kernel offers hibernation: $offers"

        # logind adds the question the file cannot answer, which is
        # whether there is enough swap to hold an image.
        #
        # Three outcomes, not two, and the middle one is why. The first
        # version discarded this query's stderr and mangled its output,
        # so when it returned nothing the run stopped with
        # `CanHibernate=nothing` and no way to tell an absent busctl from
        # a refusing logind. It also parsed `s "yes"` with
        # `tr -d 's" '`, which deletes the s in "yes" and yields `ye`, so
        # the success case could never have passed either. Now the raw
        # reply is kept and shown, and a query that will not answer is
        # reported rather than fatal: this run exists to prove a resume,
        # and a diagnostic that cannot speak is not a reason to stop
        # before the thing being tested. A definite refusal still is.
        # logind answers with one of four words and only two of them are
        # refusals. `yes` is allowed; `challenge` means available but
        # needing authentication, which over ssh it always does, because
        # polkit wants an active session and this is not one. `no` is
        # not permitted and `na` is not available at all, which is what a
        # kernel with hibernation locked down reports.
        #
        # Reading `challenge` as a failure cost a run: it is the word a
        # correctly configured machine gives here, and the one observed
        # immediately before a successful hibernate. It is also moot for
        # this stage, which starts the unit directly rather than asking
        # logind, precisely to get out from under that authentication.
        local can_raw can
        can_raw=$(guest "busctl call org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager CanHibernate 2>&1" || true)
        can=$(printf '%s' "$can_raw" | cut -s -d'"' -f2)
        case "$can" in
            yes|challenge) ok "logind says this machine can hibernate ($can)" ;;
            "")  echo "   .. logind gave no answer to CanHibernate (raw: ${can_raw:-no output at all})." >&2
                 echo "   .. going on, because the kernel offers hibernation and the swapfile is active." >&2 ;;
            *)   bad "logind says CanHibernate=$can on a machine with an active swapfile and a resume karg (raw: $can_raw)" ;;
        esac

        # The marker, and it is two markers for two different claims.
        #
        # boot_id is generated by the kernel at boot and lives in the
        # memory a real resume restores, so it cannot be forged by a
        # machine that merely rebooted well. The file in /run is tmpfs,
        # which a cold boot starts empty, so its presence says userspace
        # memory came back too and not only the kernel's.
        # --- the resume itself, up to three cycles --------------------
        #
        # One cycle used to be the whole test, and run 13 is why it is
        # not. That run resumed correctly -- image loaded, platform NVS
        # restored, `Waking up from system sleep state S4`, tasks
        # restarted -- and then the guest reset itself seven seconds
        # later, unasked, with nothing on the console and nothing in the
        # resumed boot's journal. Hardware does not do this: a real laptop
        # hibernated, resumed and stayed up twice on 2026-08-21, and
        # powered off cleanly from a resumed boot. It is the same family
        # as the poweroff reset below, and it lands about one run in two.
        #
        # So the cycle is retried rather than the assertion weakened. A
        # clean attempt asserts the boot_id exactly as strictly as before;
        # only a machine that resets three times running gets the warning,
        # and a machine that never resumed still fails on the spot. What
        # tells those apart is the console, which records the S4 wake
        # whether or not the guest survives it.
        local attempt=0 resumed=0 reset_seen=0 console_mark
        while [ $attempt -lt 3 ]; do
            attempt=$((attempt + 1))

            local before_boot_id before_uptime resume_pages
            before_boot_id=$(guest cat /proc/sys/kernel/random/boot_id)
            before_uptime=$(guest "cut -d' ' -f1 /proc/uptime")
            # Where the kernel will look on the way back, asked of the kernel
            # rather than of the boot entry, so the check below compares the
            # disk against what is actually loaded.
            resume_pages=$(guest "cat /sys/power/resume_offset")
            [ -n "$before_boot_id" ] || bad "could not read the boot_id before hibernating"
            # The other half of the same rule. An empty reading here makes the
            # awk comparison after the resume `b >= 0`, which every possible
            # uptime satisfies, so a missed reading would have passed the
            # check instead of failing it.
            [ -n "$before_uptime" ] || bad "could not read /proc/uptime before hibernating"
            gsudo "touch /run/kuma-resumed" >/dev/null 2>&1 || true
            guest "test -f /run/kuma-resumed" \
                || bad "could not leave a marker in /run, so a resume could not be told from a reboot"
            ok "marked the running boot: ${before_boot_id:0:8}, up ${before_uptime}s"

            # `systemctl hibernate` is the wrong lever here, and the first run
            # to get this far is what proved it:
            #
            #     systemctl[4857]: Call to Hibernate failed: Access denied
            #
            # That verb asks logind, and logind gates hibernation on polkit,
            # whose policy for it is auth_admin_keep for anything that is not
            # an active session. CI has no session and no polkit agent to
            # answer with, so the request is refused before the kernel is
            # ever asked. It is refused for root too, because polkit is
            # asking about the session rather than about the uid. The same
            # cause is why the CanHibernate query above answers "Access
            # denied" rather than yes or no.
            #
            # None of that is kuma's, and none of it reaches a person
            # hibernating from their desktop, whose session IS active and
            # whom polkit allows. It is a property of driving a machine over
            # ssh, so the gate has to drive it another way.
            #
            # systemd-hibernate.service is the unit logind would have
            # started. systemctl talks to the manager over
            # /run/systemd/private when it runs as root, and polkit does not
            # sit in front of that.
            #
            # --no-block so the call returns rather than being killed halfway
            # through by the ssh session it is about to take down with it,
            # and the output is kept rather than discarded, because throwing
            # it away is what turned this into two wasted runs.
            echo "   .. hibernating"
            local said
            said=$(gsudo "systemctl start --no-block systemd-hibernate.service 2>&1" || true)
            [ -n "$said" ] && echo "   .. $said"

            # Powering off is half the claim. A machine that writes an image
            # and then keeps running has not hibernated, and one that never
            # writes one has not either; qemu exiting is how the guest says
            # it reached S4 and stopped.
            local waited=0
            while kill -0 "$qemu" 2>/dev/null && [ $waited -lt 300 ]; do
                sleep 5
                waited=$((waited + 5))
            done
            kill -0 "$qemu" 2>/dev/null \
                && bad "still running 300s after systemctl hibernate; console at $log"
            ok "the machine powered off after ${waited}s"

            # Powering off is not the same as writing an image, and until
            # this check existed the difference was invisible. "It booted
            # fresh" is the identical observation whether nothing was written
            # or something was written somewhere the resume cannot find, and
            # those are different bugs in different halves of the system. One
            # of them cost an evening of booting the abandoned disk by hand
            # to guess between them.
            #
            # The swap header sits in the last ten bytes of the first page of
            # the swap area: `SWAPSPACE2` for ordinary swap, `S1SUSPEND` once
            # it holds a hibernation image. Read straight out of the disk
            # image while nothing has it open, at the offset the kernel was
            # told to resume from.
            if [ -n "$resume_pages" ]; then
                local part_start byte sig
                part_start=$(sudo sfdisk -J "$raw" 2>/dev/null \
                    | python3 -c 'import sys,json; print(json.load(sys.stdin)["partitiontable"]["partitions"][2]["start"])' \
                    2>/dev/null || true)
                if [ -n "$part_start" ]; then
                    byte=$(( part_start * 512 + resume_pages * 4096 + 4086 ))
                    # tr, because an unwritten swap header is mostly NULs and
                    # bash warns on every one of them crossing a command
                    # substitution. The warning is harmless and looks like a
                    # fault in the middle of a run that is being read closely.
                    sig=$(sudo dd if="$raw" bs=1 skip="$byte" count=10 status=none 2>/dev/null | tr -d '\0' || true)
                    case "$sig" in
                        S1SUSPEND)
                            ok "a hibernation image is on the disk where resume_offset points" ;;
                        *)
                            bad "no hibernation image at resume_offset ($resume_pages pages into the root partition): the swap header there reads '${sig:-nothing}'. The machine powered off without leaving one where the kernel will look for it." ;;
                    esac
                fi
            fi

            # Where this attempt starts in the console log, so the check
            # below reads only this cycle rather than an earlier one.
            console_mark=$(( $(stat -c %s "$log" 2>/dev/null || echo 0) + 1 ))
            echo "   .. starting it again"
            boot_vm plain
            await_healthy_boot "$qemu" "$log" \
                "it came back up and is reachable" \
                "greenboot still says this boot is healthy" \
                " after resuming"

            local after_boot_id after_uptime
            after_boot_id=$(guest cat /proc/sys/kernel/random/boot_id)
            [ -n "$after_boot_id" ] || bad "could not read the boot_id after resuming"
            if [ "$after_boot_id" = "$before_boot_id" ]; then
                resumed=1
                break
            fi

            # A new boot_id is two completely different machines, and
            # only the console separates them: one never resumed, which
            # is the bug this job exists to catch, and the other resumed
            # and then fell over, which is the hypervisor's.
            if tail -c "+$console_mark" "$log" 2>/dev/null \
                | grep -q "Waking up from system sleep state S4"; then
                reset_seen=$((reset_seen + 1))
                echo "   .. attempt $attempt resumed and then the guest reset itself; going again" >&2
                continue
            fi

            bad "the machine did not resume: boot_id moved ${before_boot_id:0:8} -> ${after_boot_id:0:8} and the console shows no S4 wake for this attempt, so it booted fresh and whatever was open is gone. Console at $log"
        done

        if [ $resumed -eq 1 ]; then
            guest "test -f /run/kuma-resumed" || bad \
                "same boot_id but the tmpfs marker is gone, which should be impossible; console at $log"
            # Read with retries, and never silently. `guest` throws stderr
            # away, and `set -e` is disabled for the whole dynamic extent a
            # stage runs in, so a dropped ssh session arrives here as an
            # empty string that flows straight into the comparison below.
            # Run 12 died exactly that way: same boot_id, marker present,
            # every other assertion passed, and the harness still announced
            # "the clock says this is a new boot" over a reading it never
            # got. A check that cannot see has to say it cannot see, not
            # convict the machine of the thing it failed to measure.
            local uptime_tries=0
            after_uptime=$(guest "cut -d' ' -f1 /proc/uptime" || true)
            while [ -z "$after_uptime" ] && [ $uptime_tries -lt 5 ]; do
                sleep 3
                uptime_tries=$((uptime_tries + 1))
                after_uptime=$(guest "cut -d' ' -f1 /proc/uptime" || true)
            done
            [ -n "$after_uptime" ] || bad \
                "could not read /proc/uptime after resuming, in 6 tries over 15s. The resume itself passed: boot_id is still ${before_boot_id:0:8} and the tmpfs marker survived. This is the harness losing ssh, not a fresh boot; console at $log"
            awk -v a="$before_uptime" -v b="$after_uptime" 'BEGIN { exit !(b + 0 >= a + 0) }' \
                || bad "uptime went backwards ($before_uptime -> $after_uptime), so the clock says this is a new boot"
            ok "resumed on attempt $attempt: same boot_id, the tmpfs marker survived, uptime continued ${before_uptime}s -> ${after_uptime}s"

            # And the machine's own verdict on itself afterwards, because the
            # offset check is the thing most likely to be silently wrong and
            # a resume that worked once is not proof it will work again.
            hib_grade=$(doctor_grade hibernate)
            [ "$hib_grade" = ok ] || bad \
                "doctor grades hibernate '$hib_grade' after a successful resume"
            ok "doctor still grades hibernate ok on the resumed machine"

            # A fresh boot before this cycle, and the reason is the one
            # thing a resumed kernel under KVM cannot provide: a clock
            # it can wait on. The plain cycle's image was written by one
            # qemu process and restored by another, and the restored
            # kernel's sched_clock came back negative — printk timestamps
            # read [18446743922.xxx], which is roughly -150s formatted
            # as unsigned 2^64, while /proc/uptime stayed truthful,
            # which is why every check above still passed. The first
            # run of this cycle, 2026-08-28, hung the S3 entry on
            # exactly that machine: the kernel printed "PM: suspend
            # entry (deep)" and "Filesystems sync" and never reached
            # "Freezing user space processes", the systemd-sleep call
            # never returned, and user.slice stayed frozen for the rest
            # of the run. A bounded wait that spins on local_clock()
            # does not come back from a wrapped clock, and a fresh boot
            # resets the TSC with it. No real machine resumes across
            # hypervisor instances, so this measures the machine a person
            # actually has.
            #
            # And the machine cannot be asked to REBOOT its way to that
            # boot, measured later the same day: the reboot's shutdown
            # ran to completion — every filesystem unmounted, every swap
            # deactivated — and the guest hung after "Rebooting." with
            # "clocksource: Watchdog remote CPU 1 read timed out" as
            # the last console line, never reaching its firmware again.
            # The transition that does work from a resumed boot is the
            # one the Secure Boot half below has always used: ask for
            # poweroff, and take whichever of the three outcomes arrives.
            local old_boot
            old_boot=$(guest 'cat /proc/sys/kernel/random/boot_id')
            gsudo "systemd-run --no-block systemctl poweroff" >/dev/null 2>&1 || true
            local s2h_off=0
            while kill -0 "$qemu" 2>/dev/null && [ $s2h_off -lt 180 ]; do
                sleep 5
                s2h_off=$((s2h_off + 5))
            done
            if ! kill -0 "$qemu" 2>/dev/null; then
                # It went off. The image the plain cycle left was
                # invalidated by its own resume, so booting the disk
                # again is a cold boot, not a second resume.
                boot_vm plain
            else
                # It did not go off. Usually it reset, which is the
                # artifact the Secure Boot half grades from the same
                # transition, about one run in two — and the boot it
                # reset into is itself a fresh kernel in the same qemu,
                # which is exactly what this cycle needs, so wait for
                # it to answer and ride it. It can also have gone off
                # late, or wedged, as the hosted runner did on
                # 2026-08-21; qemu's exit is checked every round so
                # "late" is not read as "never".
                local s2h_back=0
                while :; do
                    guest true && break
                    kill -0 "$qemu" 2>/dev/null || break
                    [ $s2h_back -lt 300 ] || break
                    sleep 5
                    s2h_back=$((s2h_back + 5))
                done
                if guest true; then
                    echo "   .. it reset rather than powering off; riding the boot it reset into" >&2
                elif ! kill -0 "$qemu" 2>/dev/null; then
                    boot_vm plain
                else
                    # Wedged: neither off nor back. Take it down by
                    # force; the console above shows every filesystem
                    # unmounted when this hang happens, and the fresh
                    # boot journals whatever a power loss leaves.
                    echo "   .. it neither powered off nor came back; taking it down" >&2
                    tail -40 "$log" >&2 || true
                    kill -9 "$qemu" 2>/dev/null || true
                    wait "$qemu" 2>/dev/null || true
                    boot_vm plain
                fi
            fi

            # However the fresh boot arrived, it is only usable as one:
            # the boot_id must have moved, or the machine never went
            # down and the cycle below would suspend the wrapped clock
            # this whole block exists to get away from.
            local new_boot s2h_fresh_deadline
            s2h_fresh_deadline=$((SECONDS + 300))
            until new_boot=$(guest 'cat /proc/sys/kernel/random/boot_id') \
                && [ -n "$new_boot" ] && [ "$new_boot" != "$old_boot" ]; do
                kill -0 "$qemu" 2>/dev/null \
                    || bad "qemu died before suspend-then-hibernate; console at $log"
                [ $SECONDS -lt $s2h_fresh_deadline ] \
                    || bad "the machine did not come back up for suspend-then-hibernate"
                sleep 5
            done
            ok "cold-booted onto a fresh clock before suspend-then-hibernate"

            # The sleep guard refuses to suspend a session that has no
            # shell in it, and the fresh boot just ended the session the
            # plain cycle hibernated from. The appended autologin block
            # is persistent, so the machine comes back into a session on
            # its own; wait for that before asking it to sleep.
            local s2h_session_deadline=$((SECONDS + 120))
            until guest "loginctl list-sessions --no-legend \
                | grep -qE '^ *[a-z0-9]+ +[0-9]+ +$user +seat0( |$)'"; do
                [ $SECONDS -lt $s2h_session_deadline ] \
                    || bad "no graphical session for $user after the fresh boot before suspend-then-hibernate"
                sleep 5
            done

            # The marker is tmpfs, so no fresh boot carries the one the
            # plain cycle left; this cycle proves its own resume, so it
            # marks the boot it is about to suspend.
            gsudo "touch /run/kuma-resumed" >/dev/null 2>&1 || true

            # --- the whole point of 0.18's item 1 ------------------------
            #
            # A suspend-then-hibernate cycle, which is a different machine
            # path than the plain one above: suspend to RAM first, wake on
            # the RTC alarm systemd-sleep sets, then hibernate from that
            # suspended state. The lid setting itself cannot be driven
            # from here (no VM has a lid), so this drives the unit the lid
            # would start; what the lid adds is only logind's choice of
            # this unit, and that choice is asserted as a file on the
            # installed disk and as doctor's `lid` grade above.
            #
            # The delay is harness-only, and it exists because a VM has no
            # battery: with none and no HibernateDelaySec, systemd waits
            # 2h before waking to hibernate, which no run has. On real
            # hardware the product deliberately sets no delay, because a
            # battery makes the low-battery alarm a better trigger than
            # any number. Written into the machine rather than the
            # command line because systemd-sleep reads it at unit start.
            #
            # Staged in /tmp and installed by a sudo'd cat, never by a
            # `sudo printf ... > /etc/...`: the remote shell opens the
            # redirection as the unprivileged user, so the write is
            # denied and the failure hides in guest's 2>/dev/null, with
            # set -e off in this subshell to keep it quiet. The
            # autologin block fell into that trap on 2026-08-22 and this
            # cycle fell into it on its own first run, 2026-08-28: the
            # section's one and only execution failed before the machine
            # ever got to suspend, on a harness bug the autologin fix
            # had already documented.
            echo "   .. suspend-then-hibernate"
            # Staged as one unit and retried as one unit, because the
            # read-back below cannot tell a lost packet from a lost
            # file: this block sits in the window where the plain
            # cycle's Run 12 and this cycle's ci 33127646466 both lost
            # ssh, and every step is idempotent, so re-running the
            # whole block is safe where re-running one step of it would
            # leave the halves disagreeing.
            local staged=0 stage_deadline
            stage_deadline=$((SECONDS + 90))
            until [ "$staged" -eq 1 ]; do
                guest "printf '%s\n' '[Sleep]' 'HibernateDelaySec=15' > /tmp/kuma-smoke-s2h.conf" \
                    || true
                gsudo "mkdir -p /etc/systemd/sleep.conf.d" || true
                gsudo "sh -c 'cat /tmp/kuma-smoke-s2h.conf > /etc/systemd/sleep.conf.d/kuma-smoke.conf'" \
                    || true
                # Read back, because a delay that never landed is a 2h
                # suspend: the 300s ceiling below would report "never
                # hibernated" about a machine the harness never
                # configured. The read-back is the retry condition: it
                # is the only step whose success every other one exists
                # to produce.
                if gsudo 'grep -q "^HibernateDelaySec=15$" /etc/systemd/sleep.conf.d/kuma-smoke.conf'; then
                    staged=1
                elif [ $SECONDS -lt "$stage_deadline" ]; then
                    sleep 5
                else
                    bad "could not stage the harness's short hibernate delay, over 90s of tries. The machine answered the session check moments before, so this is the harness losing ssh rather than a machine fault; console at $log"
                fi
            done

            local s2h_boot_id s2h_uptime s2h_attempt=0 s2h_waited=0 s2h_done="" s2h_reset=""
            # The wake-alarm race this fixture loses, and how it is
            # answered now. systemd arms the hibernate delay as a
            # boottime alarm, suspends, and on waking asks that alarm
            # whether it fired. A guest clock that comes back even a
            # fraction of a second behind the deadline makes the wake
            # read as a manual one, and a machine with no battery is
            # never contradicted: the unit returns WITHOUT hibernating,
            # no error, no failed unit, a machine perfectly healthy and
            # still up -- which is exactly what a 300s timeout reports.
            # QEMU's clock warps under this fixture in ways chronyd and
            # tailscaled both log, so the race is the fixture's; kernel
            # and RTC agree on hardware, and it does not exist there.
            #
            # Until 2026-09-15 the race lost about once a month and the
            # second attempt absorbed it. From 2026-09-16 it lost BOTH
            # attempts on five nights in seven (runs 35080773077,
            # 35207190041, 35329249805, 35502747816, 35588564265), each
            # red night spending ten minutes in two 300s waits for a
            # poweroff the machine had already declined to schedule,
            # while the plain cycle's hibernate resumed on its first
            # attempt every one of those nights. The measured shortfall
            # is the guest's boottime landing 0.15-0.3s under the
            # deadline (14.85s and 14.71s against 15, in 35588564265),
            # which no HibernateDelaySec sized in whole seconds can be
            # trusted to clear.
            #
            # So the race is no longer retried, it is answered. When the
            # machine wakes and the unit returns without hibernating,
            # the wake's one remaining half IS hibernating, and the
            # harness does that directly, over the same
            # manager-not-logind path the plain cycle uses. Everything
            # the cycle asserts survives the change: the image-on-disk
            # check reads the same offset, the resume is still held to
            # the same boot_id, and the one thing no longer asserted is
            # that systemd's own classification of the wake agrees with
            # the clock -- which is systemd's promise to keep and this
            # fixture's to lose.
            while [ "$s2h_attempt" -lt 2 ] && [ -z "$s2h_done" ]; do
                s2h_attempt=$((s2h_attempt + 1))
                s2h_reset=""

                s2h_boot_id=$(guest_retry cat /proc/sys/kernel/random/boot_id)
                s2h_uptime=$(guest_retry "cut -d' ' -f1 /proc/uptime")
                [ -n "$s2h_boot_id" ] || bad "could not read the boot_id before suspend-then-hibernate, over 90s of tries; console at $log"
                [ -n "$s2h_uptime" ] || bad "could not read /proc/uptime before suspend-then-hibernate, over 90s of tries; console at $log"
                # Re-marked per attempt: a wake that ends in a reset hands
                # the retry a fresh boot with an empty tmpfs, and the
                # resume below is held to whatever boot this attempt
                # marked.
                gsudo "touch /run/kuma-resumed" >/dev/null 2>&1 || true

                # Same unit-starting trick as the plain cycle: logind's polkit
                # is not in the way of the manager, and --no-block because the
                # suspend is about to take the ssh session with it.
                local s2h_said
                s2h_said=$(gsudo "systemctl start --no-block systemd-suspend-then-hibernate.service 2>&1" || true)
                [ -n "$s2h_said" ] && echo "   .. $s2h_said"

                # Suspend first, for at least the 15s delay, then the image
                # write, then S4 powers off and qemu exits. The plain cycle's
                # 300s ceiling covers suspend, wake, a direct hibernate and
                # the write, with room to spare.
                #
                # The wake is read off the console rather than waited out:
                # a resumed kernel prints "PM: suspend exit" within seconds,
                # the mark bounds the read to this attempt, and a wake seen
                # while qemu is still alive answers the race now instead of
                # at the ceiling.
                local s2h_console_mark s2h_woke=0 s2h_state="" s2h_image=""
                s2h_console_mark=$(( $(stat -c %s "$log" 2>/dev/null || echo 0) + 1 ))
                s2h_waited=0
                while kill -0 "$qemu" 2>/dev/null && [ "$s2h_waited" -lt 300 ]; do
                    sleep 5
                    s2h_waited=$((s2h_waited + 5))
                    if ! kill -0 "$qemu" 2>/dev/null; then
                        s2h_done=1
                        break
                    fi
                    if [ "$s2h_woke" -eq 0 ] \
                        && tail -c "+$s2h_console_mark" "$log" 2>/dev/null \
                            | grep -q 'PM: suspend exit'; then
                        s2h_woke=1
                        # The guest is awake, and one ssh round trip
                        # separates the race from everything it could be
                        # mistaken for, on the same three facts the old
                        # post-timeout check used: the boot is still the
                        # one that suspended (a wake that ends in a reset
                        # is the fixture's other artifact, and the attempt
                        # loop is what absorbs it), the unit has exited
                        # (still active after this window is a sleep stuck
                        # mid-cycle), and this boot wrote no image (the
                        # race's signature is suspend, wake, give up --
                        # "manual wakeup", silently, by design). Read
                        # through guest_retry: these are the first packets
                        # after a wake, the exact window Run 12 and
                        # ci 33127646466 lost.
                        local s2h_classify_deadline s2h_now_id
                        s2h_classify_deadline=$((SECONDS + 45))
                        while :; do
                            # qemu first, every pass: a race the guest WON
                            # looks identical to a hung sleep from here --
                            # the unit reads active straight through the
                            # image write, and the machine then powers off
                            # under the reads -- and only qemu's exit tells
                            # those apart.
                            if ! kill -0 "$qemu" 2>/dev/null; then
                                s2h_done=1
                                break
                            fi
                            s2h_state=$(guest_retry 'systemctl show systemd-suspend-then-hibernate.service -p ActiveState --value' || true)
                            # The count's zero answer has to survive its
                            # exit code: grep -c prints 0 and FAILS when
                            # nothing matched, and the lost race this
                            # recovery exists to answer is exactly that
                            # -- a wake with no hibernation entry yet.
                            # Fed through guest_retry, whose retry is
                            # exit-code-driven, the honest zero burned
                            # its whole 90s and came back empty, the
                            # "0" branch below never matched, and the
                            # recovery could not recover: every lost
                            # race landed on the fail instead. `|| true`
                            # inside the guest makes zero a successful
                            # answer; a lost ssh still exits nonzero
                            # and still retries.
                            s2h_image=$(guest_retry 'journalctl -b -k --no-pager | grep -c "PM: hibernation: hibernation entry" || true' || true)
                            s2h_now_id=$(guest_retry cat /proc/sys/kernel/random/boot_id || true)
                            if [ -n "$s2h_now_id" ] && [ "$s2h_now_id" != "$s2h_boot_id" ]; then
                                echo "   .. attempt $s2h_attempt woke into a reset (the same artifact the plain cycle retries)" >&2
                                s2h_reset=1
                                break
                            fi
                            if [ -n "$s2h_now_id" ] && [ "$s2h_now_id" = "$s2h_boot_id" ] \
                                && [ "$s2h_state" = "inactive" ] && [ "${s2h_image:-1}" = "0" ]; then
                                echo "   .. woke and systemd took the wake for a manual one (the wake-alarm race); hibernating directly"
                                local s2h_recovered
                                s2h_recovered=$(gsudo "systemctl start --no-block systemd-hibernate.service 2>&1" || true)
                                [ -n "$s2h_recovered" ] && echo "   .. $s2h_recovered"
                                break
                            fi
                            if [ "$s2h_state" != "active" ] && [ "$s2h_state" != "activating" ]; then
                                bad "the suspend-then-hibernate unit reads ${s2h_state:-unknown} with ${s2h_image:-unknown} hibernation images this boot after a wake, so the sleep is hung or never entered. Console at $log"
                            fi
                            [ $SECONDS -lt "$s2h_classify_deadline" ] \
                                || bad "the unit still reads ${s2h_state:-unknown} past the classification window, and the boot is still ${s2h_now_id:-unreadable}: either the sleep is hung or the harness lost ssh. Console at $log"
                            sleep 5
                        done
                        if [ -n "$s2h_reset" ]; then
                            break
                        fi
                    fi
                done

                if [ -n "$s2h_done" ]; then
                    break
                fi
                if [ -n "$s2h_reset" ]; then
                    if [ "$s2h_attempt" -lt 2 ]; then
                        continue
                    fi
                    bad "the guest reset itself after the wake on both attempts, which no alarm race explains. Console at $log"
                fi
                if [ "$s2h_woke" -eq 0 ]; then
                    bad "no wake on the console inside 300s of suspend-then-hibernate: the machine neither woke nor powered off, which no retry of the alarm answers. Console at $log"
                fi
                bad "still running 300s into suspend-then-hibernate on attempt $s2h_attempt with the wake already seen: the unit reads ${s2h_state:-unknown} with ${s2h_image:-unknown} hibernation images this boot, so the direct hibernate never powered the machine off. Console at $log"
            done
            ok "suspended, woke on the alarm, hibernated, powered off after ${s2h_waited}s"

            # The image must be on the disk for this cycle too, at the
            # same offset: a suspend-then-hibernate that reached S4 without
            # writing one is the identical silent failure the plain cycle's
            # signature check exists for.
            if [ -n "$resume_pages" ]; then
                local part_start byte sig
                part_start=$(sudo sfdisk -J "$raw" 2>/dev/null \
                    | python3 -c 'import sys,json; print(json.load(sys.stdin)["partitiontable"]["partitions"][2]["start"])' \
                    2>/dev/null || true)
                if [ -n "$part_start" ]; then
                    byte=$(( part_start * 512 + resume_pages * 4096 + 4086 ))
                    sig=$(sudo dd if="$raw" bs=1 skip="$byte" count=10 status=none 2>/dev/null | tr -d '\0' || true)
                    case "$sig" in
                        S1SUSPEND) ok "the suspend-then-hibernate image is on the disk where resume_offset points" ;;
                        *) bad "no hibernation image after suspend-then-hibernate: the swap header at resume_offset reads '${sig:-nothing}'" ;;
                    esac
                fi
            fi

            echo "   .. starting it again"
            # A fresh mark: the console is one file across boots, and the
            # S4 wake that answers the boot_id question below is in THIS
            # boot's output, not the plain cycle's.
            local s2h_console_mark
            s2h_console_mark=$(( $(stat -c %s "$log" 2>/dev/null || echo 0) + 1 ))
            boot_vm plain
            await_healthy_boot "$qemu" "$log" \
                "it came back up after suspend-then-hibernate" \
                "greenboot still says this boot is healthy" \
                " after the s2h resume"

            local s2h_after_id s2h_after_uptime
            s2h_after_id=$(guest_retry cat /proc/sys/kernel/random/boot_id)
            [ -n "$s2h_after_id" ] || bad "could not read the boot_id after the s2h resume, over 90s of tries. The healthy-boot checks above passed, so this is the harness losing ssh rather than a machine verdict; console at $log"
            if [ "$s2h_after_id" != "$s2h_boot_id" ]; then
                # The same distinction the retry loop makes above: a
                # resumed-then-reset guest is QEMU's artifact, a never-
                # resumed one is the product's bug.
                if tail -c "+$s2h_console_mark" "$log" 2>/dev/null \
                    | grep -q "Waking up from system sleep state S4"; then
                    warn "the s2h resume loaded the image and then the guest reset (same QEMU artifact as the plain cycle)"
                else
                    bad "the machine did not resume from suspend-then-hibernate: boot_id moved ${s2h_boot_id:0:8} -> ${s2h_after_id:0:8} with no S4 wake on the console. Console at $log"
                fi
            else
                guest "test -f /run/kuma-resumed" || bad \
                    "same boot_id after s2h but the tmpfs marker is gone; console at $log"
                s2h_after_uptime=$(guest_retry "cut -d' ' -f1 /proc/uptime")
                [ -n "$s2h_after_uptime" ] || bad \
                    "could not read /proc/uptime after the s2h resume, over 90s of tries. The resume itself passed: boot_id is still ${s2h_boot_id:0:8} and the tmpfs marker survived. This is the harness losing ssh, not a fresh boot; console at $log"
                awk -v a="$s2h_uptime" -v b="$s2h_after_uptime" 'BEGIN { exit !(b + 0 >= a + 0) }' \
                    || bad "uptime went backwards across the s2h cycle ($s2h_uptime -> $s2h_after_uptime)"
                ok "resumed from suspend-then-hibernate: same boot_id, marker survived, uptime continued"
            fi
        else
            warn "the guest resumed and then reset itself on all $reset_seen attempts (known QEMU artifact; hardware resumes and stays up, 2026-08-21)"
            echo "        Every attempt loaded the image and reached \`Waking up from"
            echo "        system sleep state S4\`, so the resume worked each time and"
            echo "        the guest then reset with nothing on the console. What this"
            echo "        run did NOT assert is that a resumed machine keeps running;"
            echo "        everything up to and including the resume is asserted above."
            echo "        Console at $log"
        fi
    fi

    # --- the Secure Boot half ------------------------------------------
    #
    # The same disk, on firmware with Microsoft's keys enrolled. This is
    # not a second test of hibernating, and trying to make it one is what
    # the first version of this stage got wrong: a locked-down kernel
    # refuses hibernation, so a machine under Secure Boot can never
    # demonstrate a resume. The question here is whether kuma SAYS SO.
    #
    # That is the failure this half exists for, and kuma shipped with it.
    # The first run of this gate found `kuma doctor` grading hibernate
    # `ok` on a Secure Boot machine, on the strength of a correct
    # swapfile and correct kernel arguments, while logind answered
    # CanHibernate `na` and the kernel would never have done it.
    #
    # Deliberately not hard-coded to today's answer. If a future kernel
    # hibernates under Secure Boot, this asserts doctor says `ok`; while
    # it refuses, this asserts doctor warns and names the reason. What is
    # pinned is that kuma agrees with the kernel, not what the kernel
    # says.
    #
    # Whether a resumed machine can be switched off is its own question,
    # and run 10 is why it is asked separately from getting to the Secure
    # Boot half. That run resumed, proved the resume, and then reset
    # instead of powering off, leaving a cold boot sitting at a login
    # prompt while qemu stayed alive. What it does NOT do is skip the
    # shutdown: the guest's journal shows systemd stopping units
    # normally and the machine dying about 180ms in. Booting the same
    # disk cold and asking it the same way powers off in ten seconds and
    # ends with `reboot: Power down`, so this belongs to having resumed
    # and not to the image. A resumed machine also powers off correctly
    # through sysrq, which puts it in the shutdown path rather than in
    # the kernel's.
    #
    # WARNS RATHER THAN FAILS, and here is the measurement that decides
    # it. On 2026-08-21 a physical machine hibernated, resumed, and was
    # asked for `systemctl poweroff` on that same resumed boot: it went
    # off and stayed off. A guest cannot reproduce that, because it
    # hibernates under one firmware instance and resumes under another,
    # which no machine with a case does. So this check grades an artifact
    # of the harness, and failing the gate on it would mean the gate can
    # never go green over a product that works.
    #
    # It is still asked, still captures the dying boot's journal, and
    # still surfaces in the summary, because the day it stops happening
    # is worth knowing and so is the day it starts happening on hardware.
    # What it no longer does is decide the run. The cold-boot retry below
    # stays fatal: a machine that will not power off from a cold boot is
    # not this artifact, it is a broken image.
    local resumed_poweroff=""
    if [ $SECURE_BOOT -eq 1 ]; then
        echo "   .. powering off to boot the same disk under Secure Boot"
        gsudo "systemd-run --no-block systemctl poweroff" >/dev/null 2>&1 || true
        local off=0
        while kill -0 "$qemu" 2>/dev/null && [ $off -lt 180 ]; do
            sleep 5
            off=$((off + 5))
        done
        if kill -0 "$qemu" 2>/dev/null; then
            resumed_poweroff=reset
            echo "   .. it did not go off; waiting for the boot it reset into" >&2
            # Ride the boot it reset into rather than killing qemu: a cold
            # boot on this disk powers off correctly, so this reaches
            # Secure Boot from a cleanly stopped machine instead of from
            # whatever a SIGKILL leaves on the filesystem.
            #
            # Three things can happen from here, and the first CI run to
            # reach this point found the third. It can come back, which is
            # what a laptop-hosted run does. It can go off late, after the
            # 180s above but before this gives up -- and reading that as
            # "it never came back" would be the same misdiagnosis this
            # file keeps having to unlearn, so qemu's own exit is checked
            # every time round. Or it can wedge: neither off nor back,
            # which is what the hosted runner did on 2026-08-21.
            local back=0 came_back=0
            while :; do
                if guest true; then came_back=1; break; fi
                kill -0 "$qemu" 2>/dev/null || break
                [ $back -lt 300 ] || break
                sleep 5
                back=$((back + 5))
            done

            if [ $came_back -eq 1 ]; then
                # The dying boot's own account, taken while it is still the
                # previous boot. The console cannot carry this and never
                # could: `fbcon: Taking over console` moves systemd's output
                # off ttyS0 on a desktop image, which is why run 10 read as
                # "no shutdown output at all" and why that reading was wrong.
                # The journal shows systemd running an ordinary shutdown and
                # the machine dying about 180ms into it, with nothing logged
                # at error level.
                gsudo "journalctl -b -1 --no-pager -o short-monotonic" \
                    >"$dir/poweroff-reset.log" 2>/dev/null || true

                gsudo "systemd-run --no-block systemctl poweroff" >/dev/null 2>&1 || true
                off=0
                while kill -0 "$qemu" 2>/dev/null && [ $off -lt 180 ]; do
                    sleep 5
                    off=$((off + 5))
                done
                kill -0 "$qemu" 2>/dev/null \
                    && bad "it would not power off from a cold boot either; console at $log"
                echo "   .. off, from the boot it reset into"
            elif kill -0 "$qemu" 2>/dev/null; then
                # Wedged. Take it down by force rather than losing the
                # Secure Boot half, which is a separate question about a
                # separate boot and has nothing to do with this one. The
                # console tail goes to the job log here and not only to
                # the artifact, because the run that needed it most is the
                # run whose artifact upload never happened.
                resumed_poweroff=wedged
                echo "   .. it neither powered off nor came back in ${back}s; taking it down" >&2
                echo "   .. last 40 lines of console:" >&2
                tail -40 "$log" >&2 || true
                kill -9 "$qemu" 2>/dev/null || true
                wait "$qemu" 2>/dev/null || true
            else
                echo "   .. it went off ${back}s after being asked, later than the 180s allowed"
            fi
        else
            ok "the resumed machine powered off rather than resetting"
        fi

        boot_vm secure
        await_healthy_boot "$qemu" "$log" \
            "the disk kuma installed boots on firmware with Microsoft's keys enrolled" \
            "greenboot says the Secure Boot machine is healthy" \
            " under Secure Boot"

        # Asked of the firmware variable rather than of mokutil, which the
        # image need not ship. The first four bytes are the EFI attributes
        # and the fifth is the value.
        local sb lockdown
        sb=$(guest "od -An -t u1 -j4 -N1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c 2>/dev/null | tr -d ' '" || true)
        [ "$sb" = 1 ] || bad \
            "the guest reports Secure Boot ${sb:-absent}: this half measured nothing"
        lockdown=$(guest "cat /sys/kernel/security/lockdown 2>/dev/null" || true)
        ok "Secure Boot is on; kernel lockdown reads: ${lockdown:-unreadable}"

        # /sys/power/state is the authority, because the kernel lists
        # `disk` there only when hibernation_available() says so, and that
        # is exactly !security_locked_down(LOCKDOWN_HIBERNATION). It is
        # also the file doctor reads, so this compares kuma against its
        # own source rather than against a guess.
        local sb_offers can_raw can grade detail
        sb_offers=$(guest "cat /sys/power/state 2>&1" || true)
        can_raw=$(guest "busctl call org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager CanHibernate 2>&1" || true)
        can=$(printf '%s' "$can_raw" | cut -s -d'"' -f2)
        grade=$(doctor_grade hibernate)
        detail=$(doctor_detail hibernate)
        echo "   .. /sys/power/state: ${sb_offers:-unreadable}; logind: ${can_raw:-no answer}"

        if grep -qw disk <<<"$sb_offers"; then
            case "$can" in
                yes|challenge) ;;
                *) bad "the kernel offers hibernation ($sb_offers) but logind says CanHibernate=${can:-nothing}" ;;
            esac
            [ "$grade" = ok ] || bad \
                "this kernel hibernates under Secure Boot and doctor grades it '$grade': $detail"
            ok "this kernel hibernates under Secure Boot, and doctor agrees"
        else
            case "$can" in
                yes|challenge)
                    bad "logind offers hibernation ($can) while the kernel does not list disk ($sb_offers)" ;;
            esac
            [ "$grade" != ok ] || bad \
                "doctor grades hibernate ok on a machine whose kernel refuses it (lockdown: ${lockdown:-unknown}); this is the promise kuma must not make"
            [ "$grade" = warn ] || bad \
                "doctor grades hibernate '$grade'; a correct setup that the kernel refuses is a warning, not a $grade"
            grep -qi "locked down" <<<"$detail" || bad \
                "doctor warns without naming lockdown, so nobody can act on it: $detail"
            ok "the kernel refuses hibernation under Secure Boot, and doctor says so instead of claiming ready"
        fi
    fi

    if [ -n "$resumed_poweroff" ]; then
        case "$resumed_poweroff" in
            wedged)
                warn "the resumed guest neither powered off nor came back, and was taken down by force (known QEMU artifact; hardware powers off, 2026-08-21)"
                echo "        It took the poweroff, stopped answering, and reached"
                echo "        neither S5 nor a fresh boot inside five minutes. There is"
                echo "        no journal to read, because the machine never came back to"
                echo "        be asked; the console tail is above and the whole log is"
                echo "        at $log"
                ;;
            *)
                warn "the resumed guest reset instead of powering off (known QEMU artifact; hardware powers off, 2026-08-21)"
                echo "        systemd runs an ordinary shutdown and the machine dies partway"
                echo "        through it; the console cannot show that, because fbcon takes"
                echo "        ttyS0 on a desktop image. The dying boot's own journal is at"
                echo "        $dir/poweroff-reset.log."
                ;;
        esac
        echo "        The same disk cold-booted powers off correctly, and so does real"
        echo "        hardware after a real resume, so this belongs to hibernating under"
        echo "        one OVMF instance and resuming under another."
    fi

    # --- the cross-version half ----------------------------------------
    #
    # Nothing has ever checked that a machine installed at one version can
    # reach a later one. Every other stage builds and boots a single
    # image, so "does an existing machine survive moving forward" has been
    # a promise rather than a result, and it is the promise 44.0 rests on.
    #
    # bootc, not `kuma update`: kuma's own update_check says so in as many
    # words. `kuma update` is the builder's verb, which pulls a base and
    # rebuilds; a machine running a published image asks bootc to re-pull
    # its origin, and --update-from above is what pointed that origin at
    # something newer than what was installed.
    if [ -n "$UPGRADE_TO" ]; then
        local before after
        before=$(booted_digest)
        [ -n "$before" ] || bad "could not read the booted digest before upgrading"

        echo "   .. upgrading to $UPGRADE_TO (pulling inside the guest)"
        # Captured, not discarded: a three-second failure says nothing on
        # the console, and the run this comment answers died reporting
        # only that bootc disagreed. The pull's own words — unknown
        # manifest, refused signature, a network the pasta race dropped —
        # are the diagnosis, and they were going to /dev/null.
        gsudo bootc upgrade >"$dir/bootc-upgrade.log" 2>&1 \
            || { tail -20 "$dir/bootc-upgrade.log"; bad "bootc upgrade failed; its own output is above, console at $log"; }

        # Staged, or there is nothing to reboot into and a green reboot
        # below would mean nothing at all.
        gsudo bootc status | grep -qiE '^  Staged|staged image' \
            || bad "nothing staged after bootc upgrade; the origin may not have moved"
        ok "the newer image staged"

        echo "   .. rebooting into it"
        gsudo systemctl reboot >/dev/null 2>&1 || true
        # Let it actually go down before waiting for it to come back, or
        # the first successful ssh is to the machine that is still
        # shutting down and every assertion runs against the old boot.
        local gone=0
        while guest true 2>/dev/null && [ $gone -lt 60 ]; do sleep 2; gone=$((gone + 2)); done

        await_healthy_boot "$qemu" "$log" \
            "the upgraded machine came back" \
            "the upgraded machine boots and greenboot says it is healthy" \
            " after the upgrade" 1800

        after=$(booted_digest)
        [ -n "$after" ] || bad "could not read the booted digest after upgrading"
        [ "$before" != "$after" ] \
            || bad "still booted on $before after the upgrade; nothing actually moved"
        ok "moved from ${before:0:19} to ${after:0:19}"

        # The half that a reboot alone would not catch. /var is shared
        # across deployments by design, so an image can move forward and
        # leave the machine's own state behind.
        guest id "$user" >/dev/null || bad "$user did not survive the upgrade"
        ok "the account survived the version jump"

        # Whether a fix that ships in the newer image reaches a machine
        # that was installed before it. `kuma-home-subvol` only acts while
        # /var/home is empty, and after a first boot it never is, so an
        # older machine cannot acquire the subvolume by upgrading. Never a
        # silent pass in either direction: losing it is a regression, and
        # not gaining it is the finding this job exists to produce.
        local home_now=no
        [ "$(guest stat -c %i /var/home)" = 256 ] && home_now=yes
        if [ "$home_was_subvol" = yes ]; then
            [ "$home_now" = yes ] || bad "/var/home stopped being a subvolume across the upgrade"
            ok "/var/home is still its own subvolume"
        elif [ "$home_now" = yes ]; then
            ok "upgrading turned /var/home into a subvolume"
        else
            ok "/var/home is still not a subvolume after upgrading, so [snapshots] on this machine would take nothing"
        fi

        # And now ask the machine to grade itself, rather than trusting
        # this script's reading of an inode. doctor already owns this
        # knowledge: check_snapshots fails a machine whose target is a
        # directory, with "the timer runs and takes nothing". Checking
        # that its verdict agrees with the filesystem turns that comment
        # into a result, and would catch doctor going quiet about a
        # machine that is still broken.
        local snapshots_grade
        snapshots_grade=$(doctor_grade snapshots)
        case "$home_now:$snapshots_grade" in
            yes:ok)   ok "doctor agrees the snapshot target is usable" ;;
            no:fail)  ok "doctor fails this machine's snapshot check, which is the correct verdict" ;;
            *:absent) bad "doctor reported no snapshots check; the declaration should have enabled it" ;;
            *)        bad "doctor says snapshots is '$snapshots_grade' while /var/home is-a-subvolume=$home_now" ;;
        esac

        # The other side of the same question, for a control rather than a
        # feature: the signature policy ships in every image from v0.10.0,
        # so an older machine can only acquire it through the /etc merge.
        # Reported in all three directions and failed in only one, the
        # same shape as /var/home above: losing the policy is a regression
        # this job must catch, and not gaining it is a finding this job
        # exists to produce rather than a broken script.
        local signatures_after
        signatures_after=$(doctor_grade signatures)
        case "$signatures_before:$signatures_after" in
            *:ok)
                ok "the signature policy reached a machine installed before it existed (was '$signatures_before')" ;;
            ok:*)
                bad "the signature policy was ok before the upgrade and is '$signatures_after' after it" ;;
            # `absent` before an upgrade is the honest answer from a kuma
            # too old to have the check. After one it is not: the machine
            # is running the new image's doctor, so no answer means the
            # check was renamed or doctor failed on the guest, and an
            # assertion that cannot see is not an assertion that passed.
            # Empty is the same thing arriving through doctor_grade's
            # `|| true` rather than through its sentinel.
            *:absent|*:)
                bad "no signatures grade after the upgrade ('$signatures_after'); doctor could not answer on the upgraded machine" ;;
            *)
                ok "upgrading did NOT bring the signature policy ('$signatures_before' then '$signatures_after'); a machine installed at that version still accepts an unsigned kuma image" ;;
        esac
    fi

    echo "   .. powering off"
    gsudo systemctl poweroff >/dev/null 2>&1 || true
    local waited=0
    while kill -0 $qemu 2>/dev/null && [ $waited -lt 30 ]; do sleep 1; waited=$((waited + 1)); done
    kill $qemu 2>/dev/null || true
    wait $qemu 2>/dev/null || true
    trap - EXIT
    [ $KEEP -eq 1 ] || sudo rm -rf "$dir"
}

# --- stage: dead disk --------------------------------------------------
# The gate 0.14 turns on: a dead disk is recoverable, proven by a command
# rather than by somebody remembering they once restored something.
#
# Install a machine, put files in it, back it up, destroy the disk, and
# install again with --restore. The files either come back or they do
# not, and nothing about that needs a person to interpret it.
#
# MinIO stands in for the far end. The point under test is kuma's half:
# whether the declaration carries a backup, whether the converger copies
# a snapshot, and whether a fresh install can put a home directory back
# on its first boot. A real repository somewhere else answers a question
# about somebody's network, not about this code.
#
# The guest reaches the runner at 10.0.2.2, which is what qemu's user
# networking calls the host, so nothing here needs a bridge or root.
# The S3 the dead-disk stage copies into and restores from. MinIO's
# community registries stopped answering on 2026-09-23 -- unauthorized on
# every tag and digest, on all three registries, measured -- so the
# fixture's S3 is Garage now, pinned by digest: the v2.4.1 multi-arch
# index, verified 2026-09-26. Garage needs its bucket made by its own CLI
# rather than by restic's MakeBucket, so start_s3 does that dance and the
# key it creates is an OUTPUT: the guest signs with whatever these hold,
# which is why they are exported here instead of named as constants.
S3_PORT=19000
RESTIC_PASS=smoke-restic-password
S3_IMAGE=docker.io/dxflrs/garage@sha256:9c96caa2612d3411acc5b0e6701fb238dbfba33e533a6d7d3d811a4b12d0d020
S3_KEY_ID=""
S3_SECRET=""

start_s3() {
    podman rm -f kuma-smoke-s3 >/dev/null 2>&1 || true
    local conf
    conf=$(mktemp -d)
    # The two secrets here are fixture material, deliberately not
    # anybody's, the same way the smoke account's password is. The rpc
    # one has a shape Garage enforces: 32 bytes of hex.
    printf '%s\n' \
        'metadata_dir = "/tmp/garage-meta"' \
        'data_dir = "/tmp/garage-data"' \
        'replication_factor = 1' \
        'rpc_bind_addr = "[::]:3901"' \
        'rpc_secret = "1111111111111111111111111111111111111111111111111111111111111111"' \
        '[s3_api]' \
        's3_region = "kuma"' \
        'api_bind_addr = "[::]:3900"' \
        'root_domain = ".s3.garage.localhost"' \
        '[admin]' \
        'api_bind_addr = "[::]:3909"' \
        "admin_token = \"kumasmoke-admin\"" \
        > "$conf/garage.toml"
    podman run -d --name kuma-smoke-s3 \
        -p "127.0.0.1:$S3_PORT:3900" \
        -v "$conf/garage.toml:/etc/garage.toml:ro,Z" \
        "$S3_IMAGE" /garage -c /etc/garage.toml server >/dev/null \
        || bad "cannot start the S3 the backup copies into"
    # No -f: an anonymous GET answers 403, which is still the server
    # speaking, and that is all this wait is for.
    local waited=0
    until curl -s -o /dev/null "http://127.0.0.1:$S3_PORT/" 2>/dev/null; do
        sleep 1
        waited=$((waited + 1))
        [ $waited -lt 60 ] || bad "the S3 never came up on $S3_PORT"
    done
    # A fresh node serves nothing until it has a role: one zone, 2 GB of
    # pretend disk, applied as the first layout version of a container
    # that is always new.
    local node
    node=$(podman exec kuma-smoke-s3 /garage -c /etc/garage.toml status 2>/dev/null \
        | awk '/^[0-9a-f]{16}/ {print $1; exit}')
    [ -n "$node" ] || bad "the S3 reported no node id"
    podman exec kuma-smoke-s3 /garage -c /etc/garage.toml layout assign -z kuma -c 2000M "$node" >/dev/null \
        || bad "cannot stage the S3 layout"
    podman exec kuma-smoke-s3 /garage -c /etc/garage.toml layout apply --version 1 >/dev/null \
        || bad "cannot apply the S3 layout"
    podman exec kuma-smoke-s3 /garage -c /etc/garage.toml bucket create kuma >/dev/null \
        || bad "cannot create the backup bucket"
    podman exec kuma-smoke-s3 /garage -c /etc/garage.toml key create kumasmoke >/dev/null \
        || bad "cannot create the backup key"
    podman exec kuma-smoke-s3 /garage -c /etc/garage.toml bucket allow --read --write --owner kuma --key kumasmoke >/dev/null \
        || bad "cannot grant the bucket to the key"
    S3_KEY_ID=$(podman exec kuma-smoke-s3 /garage -c /etc/garage.toml key info kumasmoke 2>/dev/null \
        | awk '/Key ID:/ {print $3}')
    S3_SECRET=$(podman exec kuma-smoke-s3 /garage -c /etc/garage.toml key info kumasmoke --show-secret 2>/dev/null \
        | awk '/Secret key:/ {print $3}')
    [ -n "$S3_KEY_ID" ] && [ -n "$S3_SECRET" ] || bad "cannot read the S3 key back"
    ok "the S3 is up on $S3_PORT"
}

stop_s3() {
    podman rm -f kuma-smoke-s3 >/dev/null 2>&1 || true
}

# A declaration that backs up, derived from the committed one rather than
# written here, so this stage cannot drift into testing a machine nobody
# ships.
dead_disk_declaration() {
    local out=$1
    cat examples/niri.toml > "$out"
    cat >> "$out" <<TOML

[backup]
enable = true
repo = "s3:http://10.0.2.2:$S3_PORT/kuma"
secret = "backup"
interval = "daily"
network_connections = true
TOML
    # Declared here because this stage installs rather than rebuilds, and
    # a declared timezone reaching an installed machine is a claim 0.13
    # started grading and nothing had ever executed. The zone is one
    # nobody's runner is already in, so a pass cannot be a coincidence.
    #
    # Inserted into the [system] the example already has rather than
    # appended as a second one, which TOML refuses outright: a table can
    # only be opened once.
    sed -i '/^\[system\]$/a timezone = "Pacific/Auckland"' "$out" \
        || bad "cannot declare a timezone in $out"
    grep -q '^timezone = "Pacific/Auckland"$' "$out" \
        || bad "$out has no [system] table to declare a timezone in"
}

smoke_dead_disk() {
    local name=$1 port=$2
    local dir="vm-smoke/$name"
    local raw="$dir/disk.raw"
    local log="$dir/console.log"
    local user="smoketest"
    local pass="smoke-account-password"
    local decl="$dir/backup.toml"
    local tag="localhost/kuma-smoke-backup:latest"
    local secret="$dir/restore.env"

    mkdir -p "$dir"
    start_s3
    trap 'stop_s3' EXIT

    dead_disk_declaration "$decl"
    echo "   .. building an image that declares a backup"
    "$KUMA" build --config "$decl" --tag "$tag" >/dev/null \
        || bad "the declaration that backs up does not build"
    ok "built $tag"

    # The one file a restore needs, and the same file the machine itself
    # is given. It names the repository because a machine being restored
    # has no declaration yet.
    cat > "$secret" <<ENV
RESTIC_REPOSITORY=s3:http://10.0.2.2:$S3_PORT/kuma
RESTIC_PASSWORD=$RESTIC_PASS
AWS_ACCESS_KEY_ID=$S3_KEY_ID
AWS_SECRET_ACCESS_KEY=$S3_SECRET
ENV

    dead_disk_install "$tag" "$dir" "$raw" "$user" "$pass" "" || return 1
    dead_disk_run "$dir" "$raw" "$log" "$port" "$user" "$pass" seed || return 1

    # The disk is gone. Not wiped, gone: a machine that no longer exists
    # is the case the whole feature is for, and truncating one that still
    # has a partition table would leave the test easier than reality.
    rm -f "$raw"
    ok "the disk is gone"

    dead_disk_install "$tag" "$dir" "$raw" "$user" "$pass" "$secret" || return 1
    dead_disk_run "$dir" "$raw" "$log" "$port" "$user" "$pass" verify || return 1

    stop_s3
    trap - EXIT
    [ $KEEP -eq 1 ] || sudo rm -rf "$dir"
}

dead_disk_install() {
    local tag=$1 dir=$2 raw=$3 user=$4 pass=$5 restore=$6
    local restore_args=()
    [ -n "$restore" ] && restore_args=(--restore "$restore")
    truncate -s 24G "$raw"
    echo "   .. installing${restore:+ with --restore} (needs sudo; the slow part)"
    printf '%s\n' "$pass" \
        | "$KUMA" install --disk "$raw" --image "$tag" \
            --update-from ghcr.io/example/kuma:niri \
            "${restore_args[@]}" \
            --user "$user" --hostname smoketest --yes >/dev/null \
        || bad "installing${restore:+ with --restore} failed"
    ok "installed${restore:+ with --restore}"

    # Same serial console the published stage adds, and for the same
    # reason: without it a machine that never boots produces no evidence.
    local kloop kboot
    kloop=$(sudo losetup -fP --show "$raw") || bad "cannot attach $raw"
    kboot="$dir/bootmnt"
    mkdir -p "$kboot"
    if sudo mount "${kloop}p2" "$kboot" 2>/dev/null; then
        sudo sed -i 's/^options .*/& console=ttyS0/' "$kboot"/loader/entries/*.conf 2>/dev/null || true
        sudo umount "$kboot"
    fi
    sudo losetup -d "$kloop" || true
}

# Boot the disk, do one job over ssh, shut it down.
dead_disk_run() {
    local dir=$1 raw=$2 log=$3 port=$4 user=$5 pass=$6 job=$7
    # find_ovmf prints "CODE VARS", and taking the whole line as the code
    # path is how the first draft of this broke: qemu got one -drive
    # argument naming two files. Split the same way smoke_published does,
    # so there is one account of where firmware lives.
    #
    # The plain pair always, because --secure-boot adds a boot rather
    # than replacing one. The first version of this stage booted
    # everything under Secure Boot and could not get past its own
    # CanHibernate check, which was the right answer to the wrong
    # question: a locked-down kernel refuses to hibernate, so a machine
    # under Secure Boot can never prove that resume works.
    local ovmf ovmf_code ovmf_vars
    ovmf=$(find_ovmf) \
        || bad "no OVMF firmware; an installed disk is UEFI and will not boot on SeaBIOS"
    ovmf_code=${ovmf%% *}
    ovmf_vars=${ovmf##* }
    cp "$ovmf_vars" "$dir/OVMF_VARS.fd"

    # And the Secure Boot pair beside it, for the second boot.
    local sb_code="" sb_vars=""
    if [ $SECURE_BOOT -eq 1 ]; then
        local sb
        sb=$(find_ovmf_secboot) \
            || bad "no Secure Boot OVMF firmware; --secure-boot cannot be answered here"
        sb_code=${sb%% *}
        sb_vars=${sb##* }
        cp "$sb_vars" "$dir/OVMF_VARS.secboot.fd"
        echo "   .. Secure Boot firmware, Microsoft's keys enrolled: $sb_code"
    fi || bad "cannot stage the OVMF vars"

    qemu-system-x86_64 \
        -enable-kvm -cpu host -smp 4 -m 8192 \
        -machine q35 \
        -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code" \
        -drive "if=pflash,format=raw,file=$dir/OVMF_VARS.fd" \
        -drive "file=$raw,if=virtio,format=raw" \
        -device "$QEMU_VGA" -display "$QEMU_DISPLAY" \
        -nic "user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$port-:22" \
        -serial "file:$log" &
    local qemu=$!
    # shellcheck disable=SC2064
    trap "kill $qemu 2>/dev/null || true; stop_s3" EXIT

    #
    # ServerAlive*, because this stage now asks a machine to disappear on
    # purpose. ConnectTimeout only bounds the handshake; a connection
    # that is already open when the guest stops existing has nothing to
    # notice it, and waits on TCP for as long as the kernel allows. Three
    # missed probes at five seconds gives up in fifteen.
    local ssh_opts=(-p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
                    -o ConnectTimeout=5 -o LogLevel=ERROR
                    -o ServerAliveInterval=5 -o ServerAliveCountMax=3
                    -o PubkeyAuthentication=no -o PreferredAuthentications=password
                    "$user@127.0.0.1")
    guest() { sshpass -p "$pass" ssh "${ssh_opts[@]}" "$@" 2>/dev/null; }
    sudoq() { guest "sudo -S -p '' $1" <<<"$pass"; }

    echo "   .. waiting for ssh on $port"
    local deadline=$((SECONDS + 600))
    until guest true; do
        [ $SECONDS -lt $deadline ] || bad "no ssh within 600s ($job); console at $log"
        kill -0 $qemu 2>/dev/null || bad "qemu exited before ssh ($job); console at $log"
        sleep 5
    done
    ok "ssh is up ($job)"

    if [ "$job" = seed ]; then
        # Files a person would miss, in the place the declaration covers.
        guest "mkdir -p ~/Documents && echo 'the thing that must survive' > ~/Documents/marker.txt" \
            || bad "cannot write the marker"
        guest "head -c 5000000 /dev/urandom > ~/Documents/bulk.bin" || bad "cannot write bulk"

        # The one thing outside home that nothing else can recreate, and
        # the one this stage did not used to check. A review found the
        # restore asking only for /var/home while the backup stored this
        # too, so the machine came back complete except for every network
        # password, and this gate passed anyway. Staged through /tmp and
        # installed, the same shape the credential above uses, because
        # the quoting for writing into /etc over ssh as root is worse
        # than a second command.
        guest "printf '[wifi]\npsk=smoke-wifi-secret\n' > /tmp/smoke.nmconnection" \
            || bad "cannot stage a network connection"
        sudoq "install -d -m 0700 /etc/NetworkManager/system-connections" \
            || bad "cannot make the connections directory"
        sudoq "install -m 0600 /tmp/smoke.nmconnection \
            /etc/NetworkManager/system-connections/smoke.nmconnection" \
            || bad "cannot install the network connection"
        ok "wrote the files a restore has to bring back, home and wifi"

        # The credential the declaration names. Provisioned by hand here
        # exactly as a person would, which is also what proves the
        # doctor grade for its absence was reachable a moment ago.
        sudoq "install -d -m 0700 /var/lib/kuma/secrets" || bad "cannot make the secrets directory"
        guest "cat > /tmp/backup.env" <<ENV || bad "cannot stage the credential"
RESTIC_PASSWORD=$RESTIC_PASS
AWS_ACCESS_KEY_ID=$S3_KEY_ID
AWS_SECRET_ACCESS_KEY=$S3_SECRET
ENV
        sudoq "install -m 0600 /tmp/backup.env /var/lib/kuma/secrets/backup.env" \
            || bad "cannot install the credential"
        ok "credential provisioned"

        # A backup copies a snapshot, so there has to be one. The timer
        # would take it within the hour; this stage has minutes.
        sudoq "systemctl start kuma-snapshot.service" || bad "snapshot service failed"
        guest "ls /var/home/.snapshots | head -1" | grep -q . \
            || bad "no snapshot was taken, so there is nothing to copy"
        ok "a snapshot exists to copy from"

        sudoq "kuma backup --init" || {
            sudoq "journalctl -u kuma-backup.service -n 40 --no-pager" || true
            bad "kuma backup --init failed"
        }
        # The converger exits 0 on three "not ready" states and says which
        # in its own log, so a missing stamp is never a mystery unless the
        # log is thrown away. It was, once, and cost a run.
        if ! guest "test -f /var/lib/kuma/backup-last"; then
            echo "   .. the unit exited cleanly and copied nothing. It said:"
            sudoq "systemctl status kuma-backup.service --no-pager -l" 2>&1 | sed "s/^/      /"
            sudoq "journalctl -u kuma-backup.service -n 40 --no-pager" 2>&1 | sed "s/^/      /"
            bad "the backup left no stamp, so doctor would call it stale"
        fi
        ok "seeded, and the run stamped itself"

        # The whole point of the stamp: doctor has to be able to see it.
        #
        # Grading every backup check rather than grepping for the word,
        # which is what this did and which could not fail: check_backup
        # emits an unconditional "covers ..." line the moment
        # backup.enable is true, and smoke.sh set that itself. The
        # assertion proved only that this script wrote its own
        # declaration. A missing stamp would have stayed green.
        local grades
        grades=$(sudoq "kuma doctor --json" \
            | python3 -c 'import sys,json
d=json.load(sys.stdin)
print(" ".join(c["grade"] for c in d["checks"] if c["name"]=="backup") or "absent")' 2>/dev/null) \
            || bad "cannot read doctor --json"
        case "$grades" in
            absent) bad "doctor reports no backup check at all" ;;
            *fail*|*warn*) bad "doctor grades the backup $grades after a successful seed" ;;
            "") bad "doctor reports no backup check at all" ;;
        esac
        ok "doctor grades every backup check ok ($grades)"

        # The claim 0.13 taught doctor to grade and nothing had ever
        # run: a declared timezone produces exactly one file, by `ln
        # -sfn`, which is neither a COPY nor a shell redirect and so
        # fell outside every check until then.
        guest "readlink -f /etc/localtime" | grep -q 'Pacific/Auckland' \
            || bad "the declared timezone never reached the installed machine"
        ok "a declared timezone reaches an installed machine"
    else
        guest "cat ~/Documents/marker.txt" | grep -q 'the thing that must survive' \
            || bad "the marker did not come back; console at $log"
        guest "test -s ~/Documents/bulk.bin" || bad "the bulk file did not come back"
        guest "stat -c %U ~/Documents/marker.txt" | grep -qx "$user" \
            || bad "the restored file belongs to the wrong account"
        ok "the files came back, owned by the account that lost them"

        # The whole reason network_connections is a knob. Everything
        # else can be rebuilt from the declaration; this cannot be
        # rebuilt from anything.
        sudoq "cat /etc/NetworkManager/system-connections/smoke.nmconnection" \
            | grep -q smoke-wifi-secret \
            || bad "the network connection did not come back; a restore would cost every wifi password"
        ok "the wifi password came back too"

        guest "test ! -f /var/lib/kuma/restore-request" \
            || bad "the restore request survived, so every boot would restore again"
        ok "the request was cleared"
    fi

    sudoq "systemctl poweroff" >/dev/null 2>&1 || true
    local waited=0
    while kill -0 $qemu 2>/dev/null && [ $waited -lt 60 ]; do sleep 1; waited=$((waited + 1)); done
    kill $qemu 2>/dev/null || true
    wait $qemu 2>/dev/null || true
    trap - EXIT
}

# --- stage: iso --------------------------------------------------------
# The artifact a stranger downloads, and until now the only one built by
# hand on one laptop. Three questions, in the order they can go wrong:
# does it assemble, does it still fit a release, and does it boot to a
# desktop rather than to a black screen.
#
# The last one is answered through the serial console because there is no
# other way in. The ISO has no disk to inspect and the live account has
# no password, so sshd will not take it; the console is the channel, and
# `console=ttyS0` on both menu entries is what makes it one.
smoke_iso() {
    local file=$1 tag=$2 name=$3
    local dir="vm-smoke/$name-iso"
    local iso="$dir/KUMA.iso"
    local log="$dir/console.log"
    local sock="$dir/console.sock"

    rm -rf "$dir"; mkdir -p "$dir"
    echo "   .. building live ISO"
    "$KUMA" --config "$file" iso --live --tag "$tag" --output "$dir" >"$dir/build.log" 2>&1 \
        || { tail -20 "$dir/build.log"; bad "ISO build failed"; }
    [ -f "$iso" ] || bad "no ISO at $iso"

    local bytes
    bytes=$(stat -c %s "$iso")
    printf '   ok   ISO built (%.2f GB)\n' "$(echo "$bytes" | awk '{print $1/1e9}')"
    [ "$bytes" -le "$ISO_MAX_BYTES" ] \
        || bad "ISO is $(awk -v b="$bytes" 'BEGIN{printf "%.2f", b/1e9}') GB, over the $(awk -v b="$ISO_MAX_BYTES" 'BEGIN{printf "%.2f", b/1e9}') GB budget for a release asset"
    ok "ISO fits a release asset"

    # UEFI only, deliberately: the ISO carries an EFI System Partition and
    # no BIOS boot image, so a firmware-less qemu silently falls through
    # to "no bootable device" and looks like a broken ISO.
    local ovmf ovmf_code ovmf_vars
    ovmf=$(find_ovmf) \
        || bad "no OVMF firmware found; the ISO is UEFI-only (install edk2-ovmf or ovmf)"
    ovmf_code=${ovmf%% *}
    ovmf_vars=${ovmf##* }
    cp "$ovmf_vars" "$dir/vars.fd"

    qemu-system-x86_64 \
        -enable-kvm -cpu host -smp 4 -m 8192 \
        -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code" \
        -drive "if=pflash,format=raw,file=$dir/vars.fd" \
        -cdrom "$iso" -boot d \
        -device "$QEMU_VGA" -display "$QEMU_DISPLAY" \
        -chardev "socket,id=kumacon,path=$sock,server=on,wait=off" -serial chardev:kumacon \
        >"$dir/qemu.log" 2>&1 &
    local qemu=$!
    # shellcheck disable=SC2064
    trap "kill $qemu 2>/dev/null || true" EXIT

    # qemu creates the socket during startup, not before it, so connecting
    # straight away loses a race that looks exactly like a guest which
    # never booted. Wait for the file, and give up if qemu died instead.
    local waited=0
    while [ ! -S "$sock" ]; do
        kill -0 "$qemu" 2>/dev/null || { tail -5 "$dir/qemu.log"; bad "qemu exited before it opened a console"; }
        [ "$waited" -lt 60 ] || bad "qemu never created $sock"
        sleep 1
        waited=$((waited + 1))
    done

    # Every expansion below belongs to the guest's shell, which is why the
    # heredoc is quoted: expanding any of it here would send this host's
    # answers down the serial line and then assert them against
    # themselves. The `p=` prefix is built at runtime for a second
    # reason — a serial console echoes what you type, so a literal
    # `KUMA_ISO_RUNNING=` in the command would appear in the transcript
    # before the guest had answered anything.
    local probe
    probe=$(cat <<'PROBE'
p=KUMA_ISO
echo "${p}_RUNNING=$(systemctl is-system-running 2>&1)"
echo "${p}_FAILED=$(systemctl --failed --plain --no-legend | awk '{print $1}' | tr '\n' ',')"
echo "${p}_SEAT=$(loginctl list-sessions --no-legend | awk '$4 == "seat0" {print $3}' | head -1)"
echo "${p}_NIRI=$(pgrep -c niri || echo 0)"
echo "${p}_GREETD=$(pgrep -c greetd || echo 0)"
PROBE
)

    echo "   .. booting the ISO (UEFI, serial console)"
    local out
    if ! out=$(python3 scripts/console-session.py "$sock" liveuser "$probe" 420 2>&1); then
        # The whole transcript to the file, a tail to the terminal. It
        # used to be the other way round, which truncated the log on the
        # one path that needs it: a live boot that panics early leaves 30
        # lines of timeout message in the artifact CI uploads, and the
        # kernel output explaining it is what got thrown away.
        printf '%s\n' "$out" >"$log"
        printf '%s\n' "$out" | tail -30
        kill $qemu 2>/dev/null || true
        bad "the live session never reached a usable console (see $log)"
    fi
    printf '%s\n' "$out" >"$log"
    # A serial console speaks CRLF, and every value below is read to end
    # of line. Without this the empty answer to "which units failed" is a
    # lone carriage return, which is not empty, and a perfectly healthy
    # live session fails the check with a blank explanation.
    out=$(printf '%s' "$out" | tr -d '\r')
    ok "live session reached a login prompt and accepted liveuser"

    # `tail -1` throughout: the probe's own echo carries the literal
    # `${p}_RUNNING=` and the real answer comes after it.
    local value
    value=$(printf '%s\n' "$out" | grep -o 'KUMA_ISO_RUNNING=[a-z-]*' | tail -1 | cut -d= -f2)
    [ "$value" = "running" ] || bad "live session is '$value', not running"
    ok "systemd reports the live session running"

    value=$(printf '%s\n' "$out" | grep -o 'KUMA_ISO_FAILED=[^ ]*' | tail -1 | cut -d= -f2 | tr -d ',')
    [ -z "$value" ] || bad "failed units in the live session: $value"
    ok "no failed units"

    # The desktop, which is the whole point of media that says "try kuma".
    # A seat0 session is the autologin one; the serial session this probe
    # runs in has no seat, so it cannot satisfy this by accident.
    value=$(printf '%s\n' "$out" | grep -o 'KUMA_ISO_SEAT=[a-z0-9]*' | tail -1 | cut -d= -f2)
    [ -n "$value" ] || bad "no graphical session on seat0; the live desktop did not come up"
    ok "graphical session on seat0 as $value"

    for unit in NIRI GREETD; do
        value=$(printf '%s\n' "$out" | grep -o "KUMA_ISO_${unit}=[0-9]*" | tail -1 | cut -d= -f2)
        [ "${value:-0}" -gt 0 ] || bad "$(echo "$unit" | tr '[:upper:]' '[:lower:]') is not running in the live session"
    done
    ok "greetd and niri are running"

    kill $qemu 2>/dev/null || true
    wait $qemu 2>/dev/null || true
    trap - EXIT
    if [ $KEEP -eq 0 ]; then
        rm -rf "$dir/vars.fd" "$sock"
    else
        echo "   .. ISO kept at $iso"
    fi
}

# --- stage: boot -------------------------------------------------------
smoke_boot() {
    local file=$1 tag=$2 name=$3 port=$4
    local dir="vm-smoke/$name"
    local disk="$dir/qcow2/disk.qcow2"
    local log="$dir/console.log"

    echo "   .. building disk (kuma's own installer, needs sudo)"
    "$KUMA" --config "$file" vm --tag "$tag" --output "$dir" --no-run --rebuild >/dev/null \
        || bad "disk build failed"
    ok "disk built"

    # The same serial console the install and dead-disk stages add to
    # their raws, and for the same reason: without a console karg a
    # boot that never arrives produces no evidence. Under egl-headless
    # it never arrived at all — the GRUB menu drew on the serial and
    # the countdown never ran for the whole deadline, while the
    # identical disk with the karg boots to ssh in about a minute
    # (found 2026-10-05, reproduced on demand both ways). A QCOW2
    # cannot ride losetup, so the entries are patched through qemu-nbd.
    sudo qemu-nbd -d /dev/nbd0 >/dev/null 2>&1 || true
    sudo modprobe nbd max_part=8 2>/dev/null || true
    if sudo qemu-nbd -c /dev/nbd0 "$disk"; then
        sleep 1
        local kboot="$dir/bootmnt"
        mkdir -p "$kboot"
        if sudo mount /dev/nbd0p2 "$kboot" 2>/dev/null; then
            sudo sed -i 's/^options .*/& console=ttyS0/' "$kboot"/loader/entries/*.conf 2>/dev/null || true
            sudo umount "$kboot"
        else
            bad "cannot mount $disk's second partition to add the console karg"
        fi
        sudo qemu-nbd -d /dev/nbd0 >/dev/null
        ok "serial console on the boot entries"
    else
        bad "cannot attach $disk over nbd to add the console karg (is the nbd module available?)"
    fi

    # UEFI, because the disk is a kuma install: bootc images are
    # UEFI-only, and a plain qemu invocation boots SeaBIOS, which sat
    # there silent for 420s while the disk was fine. The published stage
    # boots the same kind of disk with this firmware pair; the find and
    # the split are its, so there is one account of where firmware
    # lives.
    local ovmf ovmf_code ovmf_vars
    ovmf=$(find_ovmf) \
        || bad "no OVMF firmware; an installed disk is UEFI and will not boot on SeaBIOS"
    ovmf_code=${ovmf%% *}
    ovmf_vars=${ovmf##* }
    cp "$ovmf_vars" "$dir/OVMF_VARS.fd" || bad "cannot stage the OVMF vars"

    qemu-system-x86_64 \
        -enable-kvm -cpu host -smp 4 -m 8192 \
        -machine q35 \
        -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code" \
        -drive "if=pflash,format=raw,file=$dir/OVMF_VARS.fd" \
        -drive "file=$disk,if=virtio,format=qcow2" \
        -device "$QEMU_VGA" -display "$QEMU_DISPLAY" \
        -nic "user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$port-:22" \
        -serial "file:$log" &
    local qemu=$!
    # EXIT, not RETURN: a failed assertion exits this stage's subshell
    # rather than returning, and a RETURN trap would never fire, leaving
    # a headless VM running after a failure.
    # shellcheck disable=SC2064
    trap "kill $qemu 2>/dev/null || true" EXIT

    # BatchMode: this stage calls ssh dozens of times with stderr thrown
    # away, so an auth failure must return rather than stop on a password
    # prompt. Without it a host with no ssh key turns the whole stage
    # interactive and the deadline below never gets to run.
    local ssh_opts=(-p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
                    -o ConnectTimeout=5 -o LogLevel=ERROR -o BatchMode=yes)
    # `kuma vm` writes this only when the host had no key of its own.
    # A kept key may predate the 0600 rule: ssh refuses a world-readable
    # private key outright ("bad permissions", which BatchMode then
    # swallows), so heal the mode here rather than trust whoever wrote it.
    [ -f "$dir/ssh-key" ] && chmod 600 "$dir/ssh-key" && ssh_opts+=(-i "$dir/ssh-key")
    ssh_opts+=(kuma@127.0.0.1)
    # shellcheck disable=SC2029  # client-side expansion is the point: every
    # caller builds the command here and wants the guest to run it literally.
    guest() { ssh "${ssh_opts[@]}" "$@" 2>/dev/null; }

    # The privileged reads (the sysctl floor, the public zone) need root:
    # since the 7.2.9 kernel (the 2026-10-08 :44 float) two of the floor's
    # keys are root-only-readable outright (0600 nodes, EPERM on the
    # read), so the unprivileged read is not an option. The escalation is
    # no second ssh lane — `kuma vm` builds every disk with kuma in
    # wheel, "name and password kuma" (main.rs build_disk: the smoke and
    # `kuma vm --run` both speak to this account by name) — so the key
    # that got us in plus sudo -S with that password on stdin covers
    # every root read. gsudo keeps its own stderr in a file rather than
    # guest's /dev/null: its callers compare stdout, and an empty read is
    # identical whether ssh died or sudo balked — the file is what tells
    # those apart. Beside the lap's dirs, not inside one: the cleanup
    # rm -rf's vm-smoke/$name on the way out, pass or fail, and evidence
    # written there dies with the lap that needs it.
    # shellcheck disable=SC2029  # $* joins the caller's args into the remote
    # sudo command line, on the client, before the guest ever sees one.
    gsudo() { ssh "${ssh_opts[@]}" "sudo -S -p '' $*" 2>>"vm-smoke/gsudo-$name.err" <<<"kuma"; }

    echo "   .. waiting for ssh on $port"
    # 1800: a first boot here too - the qcow2 kuma vm builds recomposes
    # the composed base before sshd starts (see the 1128 note).
    await_healthy_boot "$qemu" "$log" \
        "booted and reachable" \
        "greenboot says this boot is healthy" "" 1800

    # systemd-remount-fs fails on a machine whose fstab still declares the
    # root Anaconda wrote, because composefs cannot remount it. Excused by
    # that cause and not by the unit's name, which is the same narrowing
    # doctor got: kuma-fstab-sync comments the line out on first boot, and
    # a machine kuma installed never had one, so after that a failure of
    # this unit is news like anything else. Excusing it by name meant this
    # stage could never report it again.
    #
    # The fstab is read here rather than parsed in the guest, so the awk
    # program is not going through ssh's word splitting. Matching the
    # mount point as a field is deliberate, and the same rule the
    # converger follows: `root` is a subvolume name on every Anaconda
    # btrfs install, so a line regex matches things that are not the root.
    local failed
    failed=$(guest systemctl --failed --plain --no-legend | awk '{print $1}')
    if guest cat /etc/fstab | awk '$1 !~ /^#/ && $2 == "/" { found = 1 }
                                   END { exit !found }'; then
        failed=$(printf '%s\n' "$failed" | grep -v '^systemd-remount-fs.service$' || true)
    fi
    [ -z "$failed" ] || bad "failed units: $(echo "$failed" | tr '\n' ' ')"
    ok "no failed units"

    # ---- the hardening floor ----
    # The image's own posture, asserted from the guest so a quiet
    # regression reports itself. The sysctl values mirror
    # 70-kuma-hardening.conf in order, the kargs list mirrors
    # 05-kuma-hardening.toml, and the zone is the one the guest is
    # being reached through: ssh works here only because the slirp rule
    # lets it, so green on the lane asserts the rule as much as the
    # harness. A sysctl that drifts means either the file stopped
    # shipping or a base change stopped it applying, and both are news.
    local hard want
    # Read as root. The 7.2.9 kernel (the 2026-10-08 :44 float) makes
    # mmap_rnd_bits and bpf_jit_harden root-only-readable — 0600 nodes,
    # the KASLR entropy and JIT posture held back from unprivileged
    # eyes — so the user view EPERMs two of the ten reads, and with
    # stderr discarded the drift report cannot tell a hidden value from
    # a floor that stopped applying. Root sees the posture either way.
    hard=$(gsudo 'sysctl -n kernel.yama.ptrace_scope kernel.kptr_restrict \
        kernel.perf_event_paranoid kernel.kexec_load_disabled fs.suid_dumpable \
        vm.unprivileged_userfaultfd vm.mmap_rnd_bits net.core.bpf_jit_harden \
        kernel.sysrq net.ipv4.icmp_echo_ignore_all' | tr '\n' ' ' | xargs)
    want="1 2 3 1 0 0 32 2 0 1"
    [ "$hard" = "$want" ] || bad "hardening sysctls drifted: got '$hard', want '$want'"
    ok "hardening sysctls at the floor"

    local cmdline karg
    cmdline=$(guest cat /proc/cmdline)
    for karg in init_on_free=1 page_alloc.shuffle=1 vsyscall=none vdso32=0 \
                module.sig_enforce=1 rd.shell=0 rd.emergency=halt \
                systemd.ssh_auto=no random.trust_cpu=off; do
        case " $cmdline " in
            *" $karg "*) ;;
            *) bad "karg $karg missing from /proc/cmdline: $cmdline" ;;
        esac
    done
    ok "hardening kargs applied"

    # The world's route to sshd ends at the zone; the lane is the rule.
    # Both runtime reads, so a zone file that ships but does not parse
    # is caught here and not by the next person to run firewall-cmd.
    local zone_services rich
    zone_services=$(gsudo firewall-cmd --zone=public --list-services)
    case " $zone_services " in
        *ssh*) bad "the public zone still serves ssh: '$zone_services'" ;;
    esac
    ok "the public zone no longer serves ssh"
    rich=$(gsudo firewall-cmd --zone=public --list-rich-rules)
    case "$rich" in
        *"10.0.2.2"*) ;;
        *) bad "the slirp lane is missing from the zone: '$rich'" ;;
    esac
    ok "the only ssh route is the test lane"

    # NTS: two configured sources and the pool actually gone. Presence
    # rather than sync state — a source that has not finished its
    # handshake yet would flake a boot that is otherwise fine.
    local nts
    nts=$(guest 'grep -c " nts" /etc/chrony.conf')
    [ "${nts:-0}" -ge 2 ] || bad "chrony has $nts NTS server lines; expected two"
    guest 'chronyc -N sources' | grep -q cloudflare \
        || bad "chrony's sources do not include the NTS vendor; the pool may be back"
    ok "time comes in over NTS"

    # faillock: wired into the stack and carrying its numbers.
    guest 'grep -q pam_faillock /etc/pam.d/system-auth' \
        || bad "pam_faillock is not in system-auth; the lockout is decorative"
    guest 'grep -q "^deny = 50" /etc/security/faillock.conf' \
        || bad "faillock.conf lost its deny count"
    ok "login brute force is capped"

    # The Wi-Fi MAC conf is a file the VM cannot exercise (the lane is
    # ethernet), so the assert is the file's, which is still enough to
    # catch it not shipping.
    guest 'grep -q "cloned-mac-address=stable" /etc/NetworkManager/conf.d/kuma-mac.conf' \
        || bad "the Wi-Fi MAC conf stopped shipping"
    ok "Wi-Fi MACs are stable-random per network"

    # The lane's own contract: sshd stays enabled in every image, or the
    # stage that has been talking to the guest all along has nothing to
    # talk to on the next run.
    [ "$(guest systemctl is-enabled sshd.service)" = enabled ] \
        || bad "sshd is not enabled; kuma vm and this stage lost their lane"
    ok "sshd is still the curated default the lane rides on"

    # /var/home has to be its own btrfs subvolume, or `[snapshots]` is a
    # timer that runs hourly and takes nothing: a snapshot is of a
    # subvolume, the script exits 0 on a target that is not one, and the
    # machine reports itself healthy throughout. kuma-home-subvol makes
    # it one on the first boot, while it is still empty, so a booted
    # machine is the only place the answer exists.
    #
    # Conditional, and the condition is not a hedge: these disks are
    # built by bootc-image-builder with an ext4 root (see BIB_ROOTFS),
    # where there is no subvolume to make and the converger is right to
    # do nothing. Said out loud rather than skipped, because a silent
    # pass here would read as if the btrfs case had been checked, and on
    # this harness it never is: only `kuma install` writes btrfs.
    local home_fs home_inode
    home_fs=$(guest findmnt -no FSTYPE -T /var/home)
    if [ "$home_fs" = btrfs ]; then
        home_inode=$(guest stat -c %i /var/home)
        [ "$home_inode" = 256 ] \
            || bad "/var/home is not a subvolume (inode $home_inode); snapshots would take nothing"
        ok "/var/home is its own subvolume"
    else
        ok "/var/home is on $home_fs, so the subvolume question does not arise here"
    fi

    local rpms
    rpms=$(declared "$file" packages.rpm)
    if [ -n "$rpms" ]; then
        # shellcheck disable=SC2086
        guest rpm -q $rpms >/dev/null || bad "declared rpms missing: $rpms"
        ok "declared packages are installed"
    fi

    # The full libav, wherever the binary that needs it rode along. On
    # the same Fedora the free and full builds share the soname (62), so
    # ldconfig cannot tell them apart and package presence is the only
    # honest codec check. Both halves of the swap, then: the full build
    # in — the compose's --allowerasing resolved — and the stripped free
    # build out, or the runtime the baked binary linked against is a
    # lie and its video support decodes nothing.
    if guest 'test -x /usr/bin/kuma-files'; then
        guest 'rpm -q ffmpeg-libs' >/dev/null \
            || bad "kuma-files is in the image but ffmpeg-libs is not"
        if guest 'rpm -q libavcodec-free'; then
            bad "libavcodec-free survived the swap; the free set is still what the image links"
        fi
        ok "the full libav build backs the baked kuma-files"
    fi

    # A binary present is not a binary that execs — the vgem node's
    # lesson one layer up. The baked binaries were born wherever built
    # them (the CI runner, a dev container), and a libav generation
    # mismatch gives a kuma-files that aborts at first exec inside the
    # very image that ships it, with every build gate green: nothing
    # here has ever run it. ldd reads the verdict out of the loader —
    # and it exits 0 with libraries missing on several ld.so versions,
    # so the output is the verdict, not the status.
    local bin missing
    for bin in kuma-shell kuma-greeter kuma-files; do
        guest "test -x /usr/bin/$bin" || continue
        missing=$(guest "ldd /usr/bin/$bin" | grep "not found")
        [ -z "$missing" ] \
            || bad "$bin does not resolve its libraries: $(echo "$missing" | head -3)"
    done
    ok "the baked binaries resolve every library"

    # Everything above this line ran as the wrong account. `kuma vm` writes
    # a bib blueprint with a hardcoded `kuma` user (main.rs, vm_config), so
    # the account this stage logs in as is the disk builder's, created at
    # image-install time and owing nothing to the declaration. The declared
    # account is a different account, made at first boot by kuma-user-sync,
    # and until now nothing here ever looked at it: a declared shell or
    # group could have been wrong in every image kuma ever built and every
    # stage would still have passed.
    #
    # The blueprint account is what makes checking possible, though. None of
    # this needs to log in AS the declared user, so none of it needs a
    # password_hash in a committed example — it asks the machine about an
    # account from a shell it already has.
    local want_user
    want_user=$(declared "$file" user.name)

    # The disk's account is the installer's answer. `kuma vm` builds a
    # disk by installing the image, and the installer's user file
    # deliberately shadows the declaration's: the sync clears the baked
    # keys before sourcing the installer's, so one machine has one
    # account, and the appliance account is the one `kuma vm --apply`
    # reaches through. Under bib the blueprint made kuma beside the
    # declared one; kuma's own installer answers for the disk instead.
    # So the booted checks are about kuma, and the declared user's
    # absence is the precedence working — pinned here so a sync-script
    # change cannot quietly resurrect an account nothing on this disk
    # answers for.
    guest getent passwd kuma >/dev/null \
        || bad "kuma-user-sync never created the installer's account"
    ok "the installer's account exists (kuma)"

    if [ -n "$want_user" ] && guest getent passwd "$want_user" >/dev/null; then
        bad "the declared $want_user exists; the installer's file answers for a disk"
    else
        ok "the declared user yields to the installer's answer"
    fi

    local want_shell got_shell
    want_shell=$(declared "$file" system.shell)
    if [ -n "$want_shell" ]; then
        got_shell=$(guest getent passwd kuma | cut -d: -f7)
        [ "$got_shell" = "/usr/bin/$want_shell" ] \
            || bad "the image's shell /usr/bin/$want_shell, the account has ${got_shell:-none}"
        ok "the account carries the image's shell"
    fi

    got_shell=$(guest id -nG kuma)
    case " $got_shell " in
        *" wheel "*) ok "the appliance account is in wheel" ;;
        *) bad "kuma is not in wheel (has: $got_shell)" ;;
    esac

    # The key that lets this machine reach the disk it built rides the
    # installer's user file (KUMA_SSH_KEY), and the converger serves it
    # from the same directory the declared keys use. Everything this
    # stage does next depends on it: without it the ssh below is a
    # password prompt.
    guest test -f /etc/kuma/keys/kuma \
        || bad "the vm's ssh key never reached /etc/kuma/keys/kuma"
    ok "the vm's ssh key is served"

    if [ -n "$(declared "$file" user.ssh_keys)" ]; then
        guest test -f "/etc/kuma/keys/$want_user" \
            || bad "declared ssh keys never reached /etc/kuma/keys/$want_user"
        ok "declared ssh keys are served"
    fi

    if [ -n "$(declared "$file" user.autologin)" ]; then
        # Two separate claims, and only the second one is the feature.
        # A greeter can be configured for autologin and still not
        # perform it: the COSMIC arm once wrote initial_session into a
        # file that greeter does not read, and asserting the config
        # alone would have called that a pass.
        #
        # Both greetd files are named because the arms write different
        # ones: niri generates config.toml wholesale, COSMIC appends to
        # the one cosmic-greeter.service reads. cat tolerates the
        # absent one.
        guest cat /etc/greetd/config.toml /etc/greetd/cosmic-greeter.toml \
            | grep -q "user = \"$want_user\"" \
            || bad "no greetd initial_session names $want_user"
        ok "greetd is configured to autologin $want_user"

        guest loginctl list-sessions --no-legend | awk '{print $3}' \
            | grep -qx "$want_user" \
            || bad "$want_user has no session, so autologin did not happen"
        ok "autologin put $want_user in a session"
    elif grep -q '^desktop' "$file"; then
        # Not silence: no committed example turns autologin on, so the
        # greetd path above is unexecuted rather than passing.
        ok "autologin not declared here, so that path is unchecked"
    fi

    # Every other check in this file drives kuma from the host, which is
    # how the image shipped for months with no kuma in it at all: the
    # declaration was baked, the units were enabled, the helpers were in
    # /usr/libexec, and nothing ever ran the binary from inside a machine.
    # `generate` is the cheapest verb that needs both halves — a runnable
    # binary and the baked-declaration fallback a machine with no working
    # copy depends on, which is what docs/agents.md promises.
    guest kuma --version >/dev/null || bad "the image ships no runnable kuma"
    guest kuma generate | grep -q '^FROM ' \
        || bad "kuma on the machine cannot read its baked declaration"
    ok "the machine can run its own kuma"

    if grep -q '^desktop' "$file"; then
        [ "$(guest systemctl is-active display-manager.service)" = active ] \
            || bad "greeter is not running"
        ok "greeter is up"
    fi

    # Shutting down is not what this test is about, and the guest's test
    # user is in wheel, which needs a password sudo can't ask for over an
    # ssh session with no tty. So: ask nicely, wait a little, then take
    # the disposable VM out. Waiting on a graceful poweroff that can never
    # arrive is how this hung the first time it ran.
    guest sudo -n systemctl poweroff >/dev/null 2>&1 || true
    local waited=0
    while kill -0 $qemu 2>/dev/null && [ $waited -lt 30 ]; do
        sleep 1
        waited=$((waited + 1))
    done
    kill $qemu 2>/dev/null || true
    wait $qemu 2>/dev/null || true
    trap - EXIT
    ok "shut down"
}

# --- run ---------------------------------------------------------------
port=2300

# The published stage answers a question about the registry, not about
# the examples, so it runs on its own and returns rather than joining the
# loop below. Nothing here builds an image.
# Ends here rather than falling through, the same way --published does.
# This stage picks its own declaration and builds its own image, so
# continuing into the sweep that builds every committed example means
# twenty minutes of work nobody asked for and, worse, a verdict buried
# under four unrelated ones.
if [ $DEAD_DISK -eq 1 ]; then
    note "dead disk: install, back up, destroy, restore, boot"
    if (smoke_dead_disk dead-disk "$port"); then
        note "summary"
        show_warnings
        printf '\n   a dead disk is recoverable\n'
        exit 0
    fi
    stop_s3
    note "summary"
    show_warnings
    printf '\n   FAIL: a dead disk is NOT recoverable\n'
    exit 1
fi

if [ -n "$PUBLISHED" ]; then
    note "published: $PUBLISHED"
    if (smoke_published "$PUBLISHED" published "$port"); then
        PASS+=("published")
    else
        FAIL+=("published")
    fi
    note "summary"
    [ ${#PASS[@]} -gt 0 ] && printf '   pass: %s\n' "${PASS[*]}"
    show_warnings
    if [ ${#FAIL[@]} -gt 0 ]; then
        printf '   FAIL: %s\n' "${FAIL[*]}"
        exit 1
    fi
    printf '\n   all good\n'
    exit 0
fi

for file in examples/*.toml; do
    name=$(basename "$file" .toml)
    if [ ${#SELECTED[@]} -gt 0 ] && ! printf '%s\n' "${SELECTED[@]}" | grep -qx "$name"; then
        continue
    fi
    tag="localhost/kuma-smoke-$name:latest"
    port=$((port + 1))
    example_file=$file

    # Only a display that asks for GL needs a render node; the default
    # (and the runner) must never depend on one.
    case "$QEMU_DISPLAY" in *gl=on*) ensure_render_node ;; esac
    # The boot stage builds from the example plus a [user] block, not from
    # the example as committed.
    #
    # Every committed example leaves [user] commented out, deliberately: a
    # declared account is a property of the image, so it rides into any
    # media built from that file, password hash included. That safety costs
    # the one thing the boot stage most needs to check. The account is made
    # at first boot by kuma-user-sync, so a wrong shell or a missing group
    # could ship in every image kuma builds and every assertion here would
    # print "no [user] declared, so nothing to converge" and pass.
    #
    # Appending rather than editing keeps this honest about what it tests:
    # the file is the example, plus exactly the block being exercised. It
    # costs no extra build, since --boot already runs the image stage.
    #
    # bash, not the example's own shell: /usr/bin/fish is only in the
    # desktop sets, and a declared shell emits a build-time `test -x`
    # guard that would fail the minimal image. No password_hash, because
    # on a disk `kuma vm` builds this account never exists at all — the
    # installer's /var/lib/kuma/user answer (the kuma convenience
    # account) shadows the declared user, by design — so there is no
    # password to write down, and nothing here logs in as this account
    # anyway; the boot probes ask the machine about the user from the
    # shell `kuma vm` already provides.
    if [ $BOOT -eq 1 ]; then
        booted_file="vm-smoke/$name.toml"
        mkdir -p vm-smoke
        cp "$file" "$booted_file"
        cat >>"$booted_file" <<'EOF'

# Appended by scripts/smoke.sh so the boot stage has an account to check.
[user]
name = "smoketest"
shell = "bash"
groups = ["wheel"]
ssh_keys = ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIsmoketestnotarealkey smoke@kuma"]
EOF
        file=$booted_file
    fi

    note "$name"
    # The committed example, never the boot stage's copy: the account an
    # install creates is the installer's answer, and building from a
    # declaration that already names one would test the case where the two
    # agree, which is the case that was never broken.
    if (smoke_image "$file" "$tag" \
        && { [ $INSTALL -eq 0 ] || smoke_install "$example_file" "$tag" "$name"; } \
        && { [ $ISO -eq 0 ] || smoke_iso "$example_file" "$tag" "$name"; } \
        && { [ $BOOT -eq 0 ] || smoke_boot "$file" "$tag" "$name" "$port"; }); then
        PASS+=("$name")
    else
        FAIL+=("$name")
    fi

    if [ $KEEP -eq 0 ]; then
        podman rmi -f "$tag" >/dev/null 2>&1 || true
        # And root's copy, which is a different store with the same tag.
        # `kuma vm` syncs the image there for the install that builds the
        # disk, and nothing ever took it away, so tags from old runs sat
        # in root storage for days. That is not only 7GB of nobody's
        # business: an install resolves this tag against root's store, so
        # a stale copy there means a stage can pass having installed an
        # image from last week. It did, before this line existed.
        sudo podman rmi -f "$tag" >/dev/null 2>&1 || true
        # The lock goes too, so a local run resolves the current base like
        # CI's fresh checkout does. A pin left lying here would quietly
        # freeze the smoke tests against a base the world has moved past,
        # which is the one thing they exist to notice.
        rm -f "${file%.toml}.lock"
        [ -d "vm-smoke/$name" ] && sudo rm -rf "vm-smoke/$name"
        [ -d "vm-smoke/$name-install" ] && sudo rm -rf "vm-smoke/$name-install"
        [ -d "vm-smoke/$name-iso" ] && sudo rm -rf "vm-smoke/$name-iso"
        [ $BOOT -eq 1 ] && rm -f "vm-smoke/$name.toml"
    fi
done

note "summary"
[ ${#PASS[@]} -gt 0 ] && printf '   pass: %s\n' "${PASS[*]}"
show_warnings
if [ ${#FAIL[@]} -gt 0 ]; then
    printf '   FAIL: %s\n' "${FAIL[*]}"
    exit 1
fi
[ ${#PASS[@]} -gt 0 ] || { echo "   no examples matched" >&2; exit 2; }
echo "   all good"
