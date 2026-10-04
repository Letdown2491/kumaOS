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
- `egl-headless` wants a host DRM node a runner lacks. The GPU-less
  console combo is `virtio-vga` plus `none`.
- The greeter chain's stderr (niri's protocol errors, kuma-greeter's
  log lines) goes to VT1 and dies with the greetd session: the
  journal never sees why a login screen failed, and "greeter exited
  without creating a session" is greetd's whole testimony. The chain
  appends to `/var/log/kuma-greeter.log` (tmpfiles-owned, 0600
  greetd) — that file is the reader, not `journalctl`.
- A KDL config string embedded as an `r#"…"#` literal breaks the
  compile the moment the config gains a `"#` sequence (a color like
  `background-color "#11111B"`): the literal ends at the color. Use
  `r##"…"##` for anything that quotes. The staging goldens caught it
  only after rustfmt and the identity checks were already fixed —
  `cargo test` in the container is the gate that actually runs first.

## Agent skills

### Issue tracker

Issues are tracked as GitHub Issues on Letdown2491/kumaOS via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles, each label string equal to its name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
