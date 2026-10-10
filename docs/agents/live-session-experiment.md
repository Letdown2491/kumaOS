# Live session experiment: session-niri.kdl cold-start test (2026-10-04)

Prototyping a per-session niri config (`~/Documents/kumaui/session-niri.kdl`)
on the live kumaOS machine, outside the image build.

## What was deployed on the machine (not in the image)

(RETIRED 2026-10-04: both files below were removed after the reboot
test; see "Where we are". Kept for the record.)

- `/etc/systemd/user/niri.service.d/kumaos.conf`: drop-in resetting
  `ExecStart` to `/usr/bin/niri --session -c /etc/kumaos/session-niri.kdl`.
  No other drop-ins existed; stock unit is `/usr/lib/systemd/user/niri.service`.
- `/etc/kumaos/session-niri.kdl`: the config (persists; validated with
  `niri validate`). Lives in `/etc`, **not** `/usr/share/kumaos`, because
  `/usr` is read-only (bootc/ostree image, `kuma:latest` 44.5.0).
- A transient `rpm-ostree usroverlay` also put a copy at
  `/usr/share/kumaos/session-niri.kdl`: **discarded at reboot**, dead end,
  do not point anything at it.

## Machine facts learned

- This host boots a bootc/ostree image; `/usr` is read-only. Persistent
  machine-local files go in `/etc`.
- `sudo` is NOPASSWD for the local user (`/etc/sudoers.d/opencode`), added for
  this experiment; delete when done.

## Where we are

- [x] Config validates (`niri validate`)
- [x] Drop-in installed, `daemon-reload` done
- [x] Reboot, log in once (cold-start case)
- [x] Conclusion: the experiment is RETIRED. The `-c` flag shadows niri's
      whole config resolution: the session came up, but
      `/etc/niri/config.kdl` (all binds, the `local.kdl` include) was
      never read, and the keybindings went missing until the drop-in was
      removed. A per-session config is a replacement, not a delta; the
      deltas belong in the image's config generation.
- [x] The two tweaks the experiment chased (layout background-color,
      hotkey-overlay skip-at-startup) landed in the image's sed chain in
      `src/containerfile/blocks.rs` (commit 3e351ba). Drop-in removed
      2026-10-04.

## Known risks

- The `ExecStart=` reset line assumes no image-side drop-in for
  `niri.service`; the image currently ships none, but a future image
  that does would conflict.
- If the experiment lands, the real home for the wiring is the image
  build (`src/containerfile/blocks.rs`), not this machine-local override.
