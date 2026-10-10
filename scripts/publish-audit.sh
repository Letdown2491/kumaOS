#!/usr/bin/env bash
# Fail if an image is not safe to publish.
#
# A published image is pulled by strangers, so it must carry no identity:
# not the account of whoever built it, not their hostname, not their ssh
# keys, and not the paths of their home directory. Most of that follows
# from building it from a declaration with no [user], but not all of it,
# which is the reason this is a check and not a rule in a document.
#
# Usage: scripts/publish-audit.sh [image]   (default localhost/kuma:latest)
#
# Mounting an image needs a user namespace, so the script re-execs itself
# under `podman unshare` when it is not already root. Reading the files
# from a *running* container would be easier and wrong: podman bind-mounts
# its own /etc/hostname over the image's, so the one file most likely to
# carry a machine's name is the one a `podman run` cannot show you.
set -euo pipefail

if [ -z "${KUMA_AUDIT_INNER:-}" ] && [ "$(id -u)" -ne 0 ]; then
    exec env KUMA_AUDIT_INNER=1 podman unshare "$0" "$@"
fi

image=${1:-localhost/kuma:latest}
failures=0

ok() { printf 'ok    %s\n' "$1"; }
bad() {
    printf 'FAIL  %s\n' "$1"
    [ $# -gt 1 ] && printf '      %s\n' "$2"
    failures=$((failures + 1))
}

mnt=$(podman image mount "$image")
cleanup() { podman image umount "$image" >/dev/null 2>&1 || true; }
trap cleanup EXIT

decl="$mnt/usr/lib/kuma/kuma.toml"

# The declaration is baked world-readable so that `kuma init` and the
# passwordless probe can read it. That is a deliberate tradeoff for a
# personal image and exactly why a published one must declare no account:
# publishing the declaration publishes the password hash with it.
# Every negative check below reports success when the thing it looks for
# is simply absent, which is the right shape for "no secret here" and the
# wrong shape for "I looked in the wrong place". These two paths exist in
# every kuma image, so their absence means the layout moved or the mount
# is empty, and every "ok" printed after that would be an image nobody
# actually audited.
for anchor in /usr/bin/kuma /usr/lib/kuma/kuma.toml; do
    [ -e "$mnt$anchor" ] || bad "$anchor is missing" \
        "this audit's other checks look for absences, so it cannot vouch for an image it cannot find"
done

if [ ! -f "$decl" ]; then
    bad "no baked declaration at /usr/lib/kuma/kuma.toml" "not a kuma image?"
else
    if grep -qE '^\s*\[user\]' "$decl"; then
        bad "the baked declaration has a [user] section" \
            "build from a declaration with no [user]; examples/niri.toml is one"
    else
        ok "baked declaration declares no [user]"
    fi
    if grep -qE '^\s*hostname\s*=' "$decl"; then
        bad "the baked declaration pins a hostname" \
            "a published image must not carry the builder's machine name"
    else
        ok "baked declaration pins no hostname"
    fi
fi

# Written only when [user] is declared, and 0600 rather than 0644, which
# makes it the easiest of these to forget: it does not show up in a
# world-readable file listing.
if [ -e "$mnt/usr/lib/kuma/user" ]; then
    bad "/usr/lib/kuma/user exists" "it carries KUMA_USER and KUMA_PASSWORD_HASH"
else
    ok "no baked user declaration"
fi

# kumaos: the default hostname since the rebrand (21f93f9) — the system
# says kumaOS wherever a person reads its name, and new installs follow.
# Any other value came from a declaration and names somebody's machine.
host=$(cat "$mnt/etc/hostname" 2>/dev/null || echo "<missing>")
if [ "$host" = "kumaos" ]; then
    ok "hostname is the default (kumaos)"
else
    bad "hostname is '$host', not the default" "it names the machine that built this"
fi

# Autologin is a property of the image, so it rides into every machine
# installed from it, naming an account those machines will not have.
for greeter in etc/greetd/config.toml etc/greetd/cosmic-greeter.toml; do
    [ -f "$mnt/$greeter" ] || continue
    if grep -q 'initial_session' "$mnt/$greeter"; then
        bad "/$greeter has an initial_session" "a published image must not autologin"
    else
        ok "/$greeter has no autologin"
    fi
done

if [ -d "$mnt/etc/kuma/keys" ] && [ -n "$(ls -A "$mnt/etc/kuma/keys" 2>/dev/null)" ]; then
    bad "/etc/kuma/keys is not empty" "declared ssh public keys are baked in"
else
    ok "no baked ssh keys"
fi

# The one leak a sanitized declaration does not fix. Rust records source
# paths for panic messages, so a binary built in someone's home carries
# that path forever. `grep -a` rather than `strings`, which the image has
# no reason to ship.
#
# linuxbrew is excluded because it is not a person: kuma writes
# Homebrew's fixed install prefix into brew-profile.sh, the sync unit's
# ConditionPathExists, and the shell profiles, so those strings are the
# program's own content and identical on every machine. Without the
# exclusion this check fails on every kuma binary ever built, which is
# how a gate that cries wolf stops being read.
if [ -f "$mnt/usr/bin/kuma" ]; then
    # No `head` to bound the output, deliberately: with one, the early
    # exit SIGPIPEs the upstream grep, pipefail reads the death as a
    # failed condition, and a binary full of paths prints "embeds no
    # build paths". Reading everything is what makes the pass mean
    # something; there is no `2>/dev/null` either, for the same reason.
    # kuma-shell and kuma-greeter ride into the image beside kuma
    # (COPY --chmod=755 kuma-shell /usr/bin/kuma-shell and the greeter
    # the same), so the leak applies to them the same. An image without
    # them skips the check: nothing shipped, nothing to name.
    for bin in kuma kuma-shell kuma-greeter; do
        # kuma-shell rides into the image beside kuma (COPY --chmod=755
        # kuma-shell /usr/bin/kuma-shell), so the leak applies to it and
        # the greeter the same. An image without them skips the check:
        # nothing shipped, nothing to name.
        [ -f "$mnt/usr/bin/$bin" ] || continue
        if paths=$(grep -aoE '/(var/)?home/[a-z_][a-z0-9_-]*/' "$mnt/usr/bin/$bin" |
            grep -v linuxbrew | sort -u) && [ -n "$paths" ]; then
            bad "/usr/bin/$bin embeds build paths: $(echo "$paths" | tr '\n' ' ')" \
                "build it in CI, or set trim-paths in the release profile"
        else
            ok "/usr/bin/$bin embeds no build paths"
        fi
    done
fi

# Every package built from mesa's source -- Fedora's drivers and
# libraries and rpmfusion's freeworld build alike -- must sit at one EVR.
# rpmfusion resolves its freeworld build from its own repo, and when
# Fedora has pushed a newer mesa than rpmfusion's matching build, dnf
# answers the dependency by DOWNGRADING mesa to the freeworld build's
# version. 44.6.0's first published tag shipped exactly that (issue
# #36): a mesa stack at 26.0.3 whose LLVM segfaults its own shader JIT
# inside the greeter, on GPU-less machines, probabilistically.
#
# What the rpmdb can see is inconsistency: a partial drag leaves some
# mesa-sourced packages behind the freeworld build, and that names the
# failure. A full-stack drag is invisible here -- the whole set moves
# together and is self-consistent at the old EVR -- so this check is a
# floor, not the whole guard; the validation laps on a GPU-less runner
# are what catches the rest, which is the promotion gate's job.
mesa_all=$(rpm --root="$mnt" -qa --qf '%{NAME} %{EVR} %{SOURCERPM}\n' 2>/dev/null \
    | awk '$3 ~ /^mesa-[0-9]/ {print $1, $2}')
if [ -z "$mesa_all" ]; then
    bad "no mesa-sourced packages in the rpmdb" \
        "the freeworld guard cannot grade an image that does not ship them"
else
    evrs=$(awk '{print $2}' <<< "$mesa_all" | sort -u | sort -V)
    if [ "$(wc -l <<< "$evrs")" -eq 1 ]; then
        ok "mesa stack and freeworld build at one EVR ($(head -n1 <<< "$evrs"))"
    else
        newest=$(tail -n1 <<< "$evrs")
        while read -r pkg evr; do
            [ -n "$pkg" ] || continue
            bad "$pkg $evr is older than the freeworld build's $newest" \
                "the freeworld resolution dragged mesa down (issue #36); wait for rpmfusion to catch up and compose again"
        done <<< "$(awk -v newest="$newest" '$2 != newest {print $1, $2}' <<< "$mesa_all")"
    fi
fi

echo
if [ "$failures" -gt 0 ]; then
    echo "$failures check(s) failed: do not publish $image"
    exit 1
fi
echo "$image carries no identity"
