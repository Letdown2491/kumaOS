
# A delta build: the declaration compiled onto the release by kuma 44.11.0.
# The release image (ghcr.io/letdown2491/kumaos:niri) carries its own binaries, units, and identity;
# this layer carries only what the declaration moves. `kuma update` is
# the full-build road.
FROM ghcr.io/letdown2491/kumaos@sha256:aaaabbbbccccddddaaaabbbbccccddddaaaabbbbccccddddaaaabbbbccccdddd

LABEL io.kuma.image="1"
LABEL io.kuma.builder=<version-and-commit>
LABEL io.kuma.delta=<compiler-version>
LABEL org.opencontainers.image.version="44.10.0"
COPY kuma.toml /usr/lib/kuma/kuma.toml
RUN --mount=type=cache,target=/var/cache/libdnf5 \
    dnf -y remove distrobox git-core tailscale
RUN --mount=type=cache,target=/var/cache/libdnf5 \
    dnf -y install --setopt=keepcache=1 htop
RUN --mount=type=cache,target=/var/cache/libdnf5 \
    dnf -y remove alsa-sof-firmware amd-ucode-firmware atheros-firmware brcmfmac-firmware cirrus-audio-firmware intel-audio-firmware intel-gpu-firmware intel-vsc-firmware iwlwifi-dvm-firmware iwlwifi-mvm-firmware mt7xxx-firmware nvidia-gpu-firmware nxpwireless-firmware qcom-wwan-firmware realtek-firmware tiwilink-firmware
RUN --mount=type=cache,target=/var/cache/libdnf5 \
    dnf -y install --setopt=keepcache=1 glibc-langpack-de
RUN echo 'LANG=de_DE.UTF-8' > /etc/locale.conf

RUN --mount=type=cache,target=/var/cache/libdnf5 \
    dnf -y install --setopt=keepcache=1 btrfs-progs
COPY --chmod=755 kuma-snapshot /usr/libexec/kuma-snapshot
COPY kuma-snapshot.service /usr/lib/systemd/system/kuma-snapshot.service
COPY kuma-snapshot.timer /usr/lib/systemd/system/kuma-snapshot.timer
RUN systemctl enable kuma-snapshot.timer

COPY 50-kuma-nostr.preset /usr/lib/systemd/user-preset/50-kuma-nostr.preset
RUN systemctl --global preset kuma-nostrd.service nip46-relay.service
COPY 10-kuma-nostr-relays.conf /usr/lib/systemd/user/kuma-nostrd.service.d/10-kuma-nostr-relays.conf

RUN systemctl enable podman.socket

COPY flatpaks /usr/lib/kuma/flatpaks
