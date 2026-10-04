#!/usr/bin/env bash
# cargo test, inside the container that can link on a host that cannot.
#
# CONTRIBUTING.md and scripts/Containerfile.dev tell the why: a kuma
# machine is image-based and ships no compiler, and layering one onto a
# bootc host to work on the tool that builds bootc hosts is the wrong
# shape. The container adds gcc; the toolchain, target/ and the registry
# cache come from the mounted home, so a run in here shares everything
# with one outside it.
#
# Usage:
#   scripts/test.sh                    # the full suite
#   scripts/test.sh --test golden      # any cargo test filter

set -euo pipefail
cd "$(dirname "$0")/.."

if ! podman image exists kuma-dev-gcc; then
    echo "no kuma-dev-gcc image; building it once from scripts/Containerfile.dev"
    podman build -t kuma-dev-gcc -f scripts/Containerfile.dev .
fi

exec podman run --rm --userns=keep-id --security-opt label=disable \
    -v "$HOME:$HOME" -w "$PWD" -e "HOME=$HOME" \
    kuma-dev-gcc \
    sh -c 'export PATH="$HOME/.cargo/bin:$PATH"; exec cargo test "$@"' sh "$@"
