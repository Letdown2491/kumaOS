# agents

## Building and testing

This host has no C compiler and no glibc development files: `cargo build`
and `cargo test` cannot link here, not even for build scripts. The build
environment is the `kuma-dev-gcc` container, which carries gcc and shares
the toolchain:

```
# from the checkout's root; ~ is this machine's home, which is the
# only identity the tree is allowed to carry
podman run --rm -v "$PWD":/kuma:Z \
  -v ~/"\.cargo":/mnt/cargo:Z -v ~/"\.rustup":/mnt/rustup:Z \
  -w /kuma -e RUSTUP_HOME=/mnt/rustup -e CARGO_HOME=/mnt/cargo \
  localhost/kuma-dev-gcc bash -c 'export PATH=/mnt/cargo/bin:$PATH; cargo test'
```

Host `cargo check` is a trap, not a shortcut: it works until the
container rebuilds `target/`, then poisons the shared artifacts in both
directions. Build and test in the container only.

## Commit before build

Commit everything first, then build. The build script stamps the working
tree's identity into the binary (`build.rs`): an uncommitted tree ships a
`-dirty` binary, and a binary installed locally with that stamp is one
that answers for code no commit describes. The tree that produced a
running binary should always be a commit you can name.

## Pushes wait for the user

Commits are the agent's to make — the tree stays clean and every change
is named by a commit — but `git push`, branch or tag, moves only on an
explicit go: a tag is a release trigger and a branch push starts a
35-minute battery, and neither is the agent's call to spend.

## The nostr layer's dev loop (44.4.0)

The daemon on a kuma machine is deployed from the checkout: a user-level
drop-in (`~/.config/systemd/user/kuma-nostrd.service.d/override.conf`)
re-points `ExecStart` at `target/release/kuma-nostrd`, marked TEMPORARY
until the image ships the fixed daemon. So the deploy is: commit, then
`cargo build --release` + `cargo install --path .` in the container
(the CLI comes from `~/.cargo/bin`; the daemon runs from
`target/release`), then `systemctl --user restart kuma-nostrd`.

## The desktop shell's dev loop (44.4.0)

The shell is the kumaui tree (`~/Documents/kumaui`), not this repo: a
GPUI program whose binary lands at `/usr/bin/kuma-shell` in the image
and whose dev deploy rides the same unit. Build and test only inside
its `localhost/kuma-dev-rust` container (`./scripts/build.sh`), never
on the host — this machine has no C compiler, and a host build poisons
shared target artifacts. The host install (`cp` from the container's
`target/release`) fails with "Text file busy" while the session is
running; stop `kuma-shell.service` first, run `~/.local/bin/kuma-shell`
by hand for the verification pass, and put the service back after. Two
shells cannot share the layer surfaces or the logind lock listener, so
a quiet moment is a requirement, not a preference.

An image build needs the release binaries beside the running kuma in
the build context (`kuma-shell` and `kuma-greeter`, with
`kuma-nostrd`/`kuma-nostr` when the nostr layer is enabled) — the same
road every release ships.

## The gpui host API's loaded facts (each learned the hard way)

- A Luau local read before its declaration exists resolves to the
  global — nil. The panel forward-declares `render` for this reason;
  every helper a callback calls must be declared above the callback's
  definer too. (The noctalia plugin is gone; the fact stays because the
  pattern recurs in any event-callback host language.)
- A flex container (column/row/scroll) centers its children on the
  cross axis by default: pass `align = "stretch"` for full-width. The
  docs page says the stretch is the default; the reference
  implementation's own layout notes say center. The notes are right.
- A clickable container (onClick on row/column) is wrapped
  content-sized by the host: the card's width does not survive it.
  Clicks go on buttons or inner rows.
- A fetch's failure is not a fact about the world: keep last-known
  state, or the empty state impersonates the list for a poll cycle.
- `NoDisplay=true` on a scheme-handler desktop file hides it from
  xdg-desktop-portal's chooser — a flatpak browser then reports "no
  supported apps". Handlers stay visible.
- The argv form of any subprocess call is the road for every argument
  that is not a literal: a nostrconnect URI joined into a shell line is
  shattered by its own `&`s.
- `cx.spawn` takes the 2-arg form `async move |this, cx|`; `cx.listener`
  closures are `Fn` — clone captured ids before the listener AND inside
  it. Goldens pin bytes; a moving golden is the review.

## The vm smoke's loaded facts (44.4.0)

- Auditing "does the distro have X" must grep `src/containerfile/blocks.rs`
  — the package lists AND the session niri KDL live there, so keybinds
  (screenshot, recording, media) are invisible to a search of the shell's
  source tree. An audit that missed them filed a screenshot issue for a
  feature that shipped (Mod+Print: grim | slurp | swappy, blocks.rs ~2418;
  closed within the hour).

- A `-u` unit filter misses the "Started" line: `journalctl -u` filters
  by `_SYSTEMD_UNIT`, and a start line belongs to the manager that
  wrote it, not the unit it names. The user manager forwards to the
  system journal and the smoketest account reads it back; a `--user`
  probe came back empty while the system journal carried the line.
  The failure dump's `journalctl -b` shape is the proven reader.
- Displayless (`virtio-vga` + `QEMU_DISPLAY: none`), niri dies on
  early import and the pre-fix shell followed — "window not found" —
  restart-looping until the unit's start limit ended the waves: a
  probe needing a live shell process was a coin flip wearing a
  deadline. kumaui's idle-without-windows fix (Oct 2026, e90884c)
  keeps the shell alive holding its DBus names through output loss,
  and the notification probe is armed again. The sleep-inhibitor
  check stayed retired: kuma-shell holds no logind delay inhibitor
  by design (a hung shell cannot stall sleep), so that probe
  asserted noctalia's contract, not this desktop's. Two lessons: a
  probe that needs a live process asks the runner's timing unless
  the process is guaranteed alive; and a fix's report that claims
  more than its diff gets caught by the gate that runs the diff.
- `egl-headless` wants a host DRM node a runner lacks — even with
  `LIBGL_ALWAYS_SOFTWARE=1`: qemu's GL helpers open a render node
  before mesa's software path gets a word in ("egl: no drm render
  node available", measured Oct 05 on a runner and on a local
  mount namespace with /dev/dri bind-hidden). And the "displayless"
  `QEMU_DISPLAY: none` is worse in a subtler way: with
  no display frontend the guest's virtio-gpu reports every connector
  DISCONNECTED, so niri comes up with zero outputs and any
  layer-shell client (the greeter) cannot map its window — gpui
  quits when its last window closes, rc=0, and greetd reads "greeter
  exited without creating a session". The greeter was never buggy;
  the VM had no screen.
- A connected connector is not enough, Oct 05: a 3D-less virtio-gpu
  (dmesg `features: -virgl`) gives niri's DRM backend a device it
  cannot allocate through — software EGL renderers are skipped for
  the renderer, GBM finds no allocator — so niri still comes up with
  zero outputs and the greeter dies the same clean rc=0 a second
  after start. The guest needs virgl: `virtio-gpu-gl`. Its guest
  driver allocates through llvmpipe alone, no host GPU. The
  end-to-end repro took one evening locally: same disk, same binary,
  greeter died in 1s on virtio-vga and ran indefinitely on
  virtio-gpu-gl.
- The runner image has no libEGL: any GL display makes qemu abort at
  its first line ("Couldn't open libEGL.so.1", core dumped) and the
  stage reads "qemu died". The kvm action installs libegl1 and
  libgl1 for the same reason it installs ovmf. And `xvfb-run` is
  the wrong way to head a GL display: it runs the command as a
  child, not exec, so the script's `$!` names the wrapper shell and
  every `kill $qemu` orphans the VM. One Xvfb per run, `DISPLAY`
  exported, qemu the direct child — that was the contract until
  egl-headless replaced the X server entirely (Oct 05, ab6e736; the
  gtk-on-Xvfb pairing killed qemu three ways); the orphan-VM trap
  stays recorded for the next host that heads a display.
- The greeter chain's stderr (niri's protocol errors, kuma-greeter's
  log lines) goes to VT1 and dies with the greetd session: the
  journal never sees why a login screen failed, and "greeter exited
  without creating a session" is greetd's whole testimony. The
  wrapper retargets the chain to the journal with `exec 2> >(logger
  -t kuma-greeter)` — the journal is what the failure dump reads. A
  log FILE cannot work there: /var/log is var_log_t, which xdm_t
  cannot write, and the 0700 greetd cache dir cannot be read back by
  the smoke's ssh user (both measured, Oct 04).
- The audit gate reads the last 50 commit messages as well as the
  tree: a fix that quotes what it scrubbed puts the name back into
  history where no later commit can reach — rewriting costs a
  force-push the branch rule exists to prevent. Describe the match
  ("the builder's name"), never quote it.
- A KDL config string embedded as an `r#"…"#` literal breaks the
  compile the moment the config gains a `"#` sequence (a color like
  `background-color "#11111B"`): the literal ends at the color. Use
  `r##"…"##` for anything that quotes. The staging goldens caught it
  only after rustfmt and the identity checks were already fixed —
  `cargo test` in the container is the gate that actually runs first.
- The runner's ground moves under a green CI, Oct 05: the 20261004
  image's kernel (6.17.0-1022-azure) dropped vgem from modules-extra,
  and every VM lap after the image update died at qemu's first line
  ("egl: no drm render node available") with no repo change to blame.
  The guest's GL is virgl's, and virgl builds its host context through
  a DRM render node a GPU-less runner does not have; the kvm action
  builds vgem out of tree against the running kernel's headers when
  the module will not load (the WSL trick: one file, seconds, no
  reboot).
- Existence is not accessibility, Oct 05: an insmod'd node ships 0600
  root:root — devtmpfs's default, and the runner's udev rules do not
  widen a faux-bus vgem — while the smoke's qemu runs as the runner
  user. qemu's node scan silently skips the EACCES and reports "no
  node", and the ls check passed, because ls opens nothing. The node
  is chmod'd 666 like /dev/kvm, and the gate is the qemu user's own
  access(2).
- An anonymous curl against raw.githubusercontent.com is rate-limited
  (HTTP 429) from Actions egress, Oct 05 — the iso lap died on the
  third fetch, after the expensive apt steps had already run. Small
  frozen upstream sources vendor into the repo (the vgem pair,
  torvalds v6.17 verbatim, beside the action): a build step with no
  network has no network to lose.
- The release's live ISO step is the one qemu site no pre-tag run
  exercises, Oct 05: ci's iso job rides the smoke defaults, and the
  rolling channel's publish=false skips the ISO entirely, so display
  pins older than a default change strand there until a release pays
  for them — the vnc-era pair that bc4f4e1's virgl fix missed, caught
  by reading the tag road, not by a run (8d823ad).
- ssh refuses a world-readable private key outright, and BatchMode
  swallows the refusal, Oct 05: the stage read "no ssh within 1800s"
  while the guest sat at its login prompt, reachable and willing, for
  half an hour. The key is 0600 at write and at the use site both, so
  an artifact kept by an older binary still works.

## Agent skills

### Issue tracker

Issues are tracked as GitHub Issues on Letdown2491/kumaOS via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles, each label string equal to its name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
