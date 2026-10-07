# Changelog

## Unreleased

- **A hardening floor, taken from secureblue's audited set and adapted
  to what kuma's gate can run.** Every image ships six new files:
  `/usr/lib/sysctl.d/70-kuma-hardening.conf` (the kernel's cheap
  answers to the exploit classes that start in userspace — ptrace
  restricted to your own descendants, kernel pointers gone from
  `/proc`, perf root-only, kexec disabled, SysRq off, coredumps to
  `/bin/false`, the TCP/ICMP set, and the rest), 
  `/usr/lib/bootc/kargs.d/05-kuma-hardening.toml` (`init_on_free`,
  `page_alloc.shuffle`, `vsyscall=none`, `vdso32=0`,
  `module.sig_enforce`, `rd.shell=0`, `rd.emergency=halt`,
  `systemd.ssh_auto=no`, `random.trust_cpu=off`), a firewalld public
  zone that no longer serves `ssh` to the world, NTS-authenticated
  chrony (`time.cloudflare.com` and `nts.netnod.se` replacing the
  unsigned pool), stable-random Wi-Fi MACs per connection, and
  faillock at secureblue's numbers (50 failures, a day's lock) wired
  in through `authselect enable-feature`. The sysctl set subtracted
  everything the running kernel already defaults and everything with a
  price a desktop would feel — `io_uring_disabled` and
  `lockdown`/`nosmt` are named decisions that stay unmade — and the
  kargs set subtracted the three kargs the kernel's config already
  carries. The sshd story is the change's spine: the boot smoke and
  `kuma vm` reach the guest over ssh and the install lap reaches it
  with a password, so the unit stays enabled and the closure moves to
  the firewall, which is where the world's route in actually was. The
  public zone keeps a rich rule admitting ssh from 10.0.2.2 alone —
  qemu's user-net gateway, the address `kuma vm`'s 127.0.0.1 bind
  already assumes — and names its price in the zone description: a
  network numbered 10.0.2.0/24 would reach sshd, which is the cost of
  the test lane living in the shipped zone instead of a fixture. The
  boot smoke gained a hardening lap that asserts the sysctl values,
  the kargs on `/proc/cmdline`, the zone's shape from the running
  firewall, NTS sources, faillock's wiring, and the MAC conf — the
  floor's quiet regression reports itself, and the kargs merge through
  bootc's kargs.d gets its measurement on the next lap. What the user
  feels: `strace -p` and perf on host processes want root, ping goes
  unanswered, the router sees a new stable MAC per Wi-Fi network,
  SysRq is gone, the console before the LUKS prompt is quieter
  (`printk = 3 3 3 3`), and builds pay `init_on_free`'s few percent.

## v44.6.0 (2026-10-09)

- **Koguma, kuma's own file manager, is the image's file manager —
  default and only.** The third kumaui binary rides the same road as
  the shell and the greeter: built at the pinned kumaui commit, staged
  beside the kuma binary, baked in at `/usr/bin/kuma-files`, with the
  desktop entry and icon shipped from the kumaui tree beside it — so
  the launcher lists it under its own name, keywords and all, and the
  mimeapps default hands it `inode/directory` outright. Thunar and its
  archive plugin leave the set in the same release, and the XFCE tail
  that rode in as their dependencies — exo, garcon, xfconf, tumbler,
  the libxfce pair, the panel straggler, ~32 MB installed — leaves
  with them. file-roller stays: it is Koguma's extract catch-all, the
  end of the ladder tar, unzip and the single-file decompressors
  cannot finish (7z/rar). The plan's original order — ship Koguma
  installed-not-default, ride it for a release, drop Thunar after —
  was collapsed by the owner's call, 2026-10-06: the ride runs on the
  drop, on the dev machine, with no net. The smoke launches Koguma in
  the guest through the session's own user manager and holds it to its
  activation socket — a shipped app that cannot launch is exactly the
  thing a smoke exists to catch, and a default that cannot launch is
  a desktop with no file manager at all.

- **Samba browsing arrives — and every gvfs mount gains a plain POSIX
  path.** `gvfs-smb` puts the samba backend in the image for the first
  time: any gvfs consumer can browse `smb://`. And `gvfs-fuse` puts
  the daemon that exposes every active gvfs mount as plain files under
  `/run/user/$UID/gvfs` into the composed image at last — a bare-metal
  install always pulled it in as a weak dependency, the composed image
  did not, so gvfs mounts existed without ever being reachable as
  files. Koguma — the file manager this release makes the default —
  browses every gvfs mount, samba and MTP alike, through those paths
  alone; the image's GIO-native consumers never needed the FUSE
  daemon, which is why the gap could hide until now.

## v44.5.0 (2026-10-05)

- **The image carries its OCI version, and bootloader-update stops
  failing on every boot.** bootc's install writes the installed
  image's OCI version into `/sysroot/.bootc-aleph.json`, and bootupd's
  reader requires it as a plain string — a null fails
  `bootloader-update.service` on every boot. Nothing in kuma's
  pipeline supplied one, so every image kuma has ever built installed
  with `"version": null` and the failed unit, invisible until the boot
  tier started grading failed units and blamed for a week on the
  Fedora float. The image now stamps
  `org.opencontainers.image.version` (the number alone, like
  os-release's), so installs record it and the unit stays clean.
  Existing machines: the failed unit clears on the next image
  install that carries the label.

- **The desktop appears ~5 seconds sooner after logging in.** greetd
  gives the greeter session 5 seconds to exit on its own after a
  successful login before it kills it, and the greeter session was
  `exec niri -- kuma-greeter` — a compositor that never exits when its
  child does. Every login paid the full patience window (measured: ~6 s
  between password accept and the user session starting). The greeter
  session is now a supervisor that runs niri in the background and exits
  the moment kuma-greeter quits, taking niri down with it in under a
  second; the session starts as soon as greetd sees the exit.
- **Doctor's nostr trust warning is one line, not one per app.** Three
  apps holding Trust graded three identical warnings; one warning now
  names the whole pile. Nothing about the grants changed — only how
  the doctor counts them.
- **`kuma clean` names what prune could not take.** A dangling image a
  container still holds is skipped by `podman image prune`, so the
  doctor warned "1 stranded build image" while `kuma clean` answered
  "Nothing to reclaim" — both true, together a lie. Clean now says
  which container holds which image, so reclaiming it is one `podman
  rm` away.
- **The shell logs where its startup milliseconds go.** The startup
  path now logs each boot phase — entering gpui, app closure, session
  connected, settings loaded, surfaces up — with elapsed
  milliseconds, so a slow login can be priced from the journal
  (`journalctl --user -u kuma-shell -b | grep 'boot:'`) instead of
  guessed at. First warm login on the record prices the whole
  shell-side path at 16 ms: the blank-second cost lives in the
  handoff chain, not the shell.
- **The doctor's idle check asks the running shell, not the boot.**
  The check read the whole boot's journal for an idle-watcher
  failure, and success is silent — so a shell that exited mid-boot
  (a logout teardown, a crash `Restart=always` recovered) left its
  watcher's death on record, and every later check that boot graded
  the live shell by its predecessor's corpse. Found on motherbox the
  same day the boot-phase logs landed: a logout/login verification
  run failed the idle lock with a broken pipe from the pre-logout
  instance. The journal read is scoped to the unit's main PID now;
  nothing a reader does differently — a failure it reports is the
  running shell's own.

Entries land with the change they describe; the next tag takes this section
as its release notes. Say what changed and what a reader has to do
differently. Why it changed belongs in the commit that made it.

### Added

- **The login screen is kuma-greeter.** kumaUI's graphical greeter
  replaces tuigreet as the niri desktop's default: greetd now starts a
  minimal niri (`/usr/share/kumaos/greeter-niri.kdl`) hosting
  `/usr/bin/kuma-greeter`, built from the same kumaUI build as the
  shell. It shows the default wallpaper and theme (it cannot know the
  user before login) and takes free-text usernames; PAM goes through
  greetd unchanged. A `Restart=on-failure` drop-in brings greetd back
  in two seconds if the greeter ever dies, and the tuigreet line stays
  in `/etc/greetd/config.toml` as a comment — reverting is a comment
  swap from a TTY. Upgrades get all of it through the usual `/etc`
  merge: an unmodified `/etc/greetd/config.toml` takes the new default
  on upgrade, while a locally edited one keeps winning over the image
  (`kuma doctor` names it, and `sudo cp /usr/etc/greetd/config.toml
  /etc/greetd/config.toml` takes the flip by hand).

### Changed

- **Doctor's idle check knows about the settings.** The check still
  fails when the shell's idle watcher is down, but its text no longer
  claims the timeouts are compiled into the shell — they are the
  defaults, changed in the settings panel or
  `~/.config/kuma-shell/config.toml`'s `[idle]` keys, and the check now
  reads the same file: it reports the machine's actual timeouts rather
  than the defaults, and a deliberately disabled lock (`lock_timeout =
  0`) is graded as a choice rather than a failure.
- **The login handoff reads as a fade.** The gap between the greeter
  quitting and the shell's surfaces appearing was a void with niri's
  hotkey-overlay noise over it. The image's generated niri config now
  skips the overlay at startup and sedes the wallpaper into the
  layout background, so the handoff shows the wallpaper instead. Both
  tweaks are seeded into niri's default config behind grep-guarded
  anchors, so a niri update that moves them fails the build rather
  than silently dropping them.

## v44.4.0 (2026-10-04)

### Fixed

- **A locked session reported itself open.** The lock screen never set
  logind's `LockedHint`, so anything reading the session's state (and
  anything a future greeter would read) saw a session that claimed to
  be unlocked while the lock screen was up. The shell now sets the
  hint through the same logind path that drives the lock, on every
  trigger: `lock-session`, the idle timeout, and sleep.
- **The greeter refused a good password for half a minute after the
  sleep guard ended a session.** The guard terminates the session's
  scope, but the compositor and the shell are user units outside that
  scope — niri.service outlived its session by 28 seconds, and every
  password typed at the greeter in that window was answered by
  niri-session's own "A niri session is already running." check. The
  guard now stops `graphical-session.target` after the terminate —
  the same teardown niri-session performs — under a timeout, so a
  wedged stop cannot stall the sleep it runs under.
- **A laptop got its battery warning from every session it ever
  opened.** `kuma-battery-watch` is spawned by niri's
  `spawn-at-startup`, which re-runs whenever niri's config reloads,
  and a session's end never reaches the loop it starts — so each
  reload and each re-login added another copy, all polling and all
  notifying. Four copies were alive on one machine. The script now
  takes an flock in the user's runtime directory and a second copy
  exits silently: one watcher per account, whichever started first.
- **The mic key muted the speakers.** The image's `XF86AudioMicMute` bind
  spawned `kuma-shell msg mute`, and `mute` is the sink's verb (an alias
  of `volume-mute`): pressing it toggled the output mute, the microphone
  never changed, and the OSD faithfully showed a Volume card. The bind now
  spawns `mic-mute`, whose request targets `@DEFAULT_AUDIO_SOURCE@` with
  the optimistic flip and rollback the other mute keys already use, so
  the OSD raises a Microphone card. The verb needs this cycle's shell,
  so the fix and the shell ride the same image: switching to it is the
  fix. A machine already running a shell that carries the verb can hold
  the key over in `~/.config/niri/local.kdl` (included last; an
  included binds node merges over the image's, per key):

  ```kdl
  XF86AudioMicMute { spawn "kuma-shell" "msg" "mic-mute"; }
  ```

### Added

- **The desktop is kuma-shell, and it locks on idle.** The shell's
  idle contract is the swayidle line kuma has run since before
  noctalia, compiled in rather than configured: lock after 15 minutes
  of stillness, power the monitors off a minute later, and lock when
  the machine is about to sleep. Idleness is the compositor's to say —
  ext-idle-notify-v1 — so a video playing or a download's progress bar
  counts as the activity it is, which an input-polling blanker cannot
  see. All three clauses land in the same lock path logind's `Lock`
  signal drives: one lock screen, one password field, one PAM chain,
  whichever of the three triggers fires. The timeouts are
  `~/.config/kuma-shell/config.toml`'s `[idle]` keys (`lock_timeout`,
  `screen_off_timeout`, `lock_before_suspend`; a 0 disables a clause).
  Hand-edited keys apply at the next shell start, same as kitty's; the
  running shell re-mints its watcher when the settings change under
  it, so a future settings panel gets live-apply for free.

- **The Nostr Signer is the shell's own panel.** The approval face the
  nostr layer shipped as a noctalia plugin is native now, and arrives
  with the shell: the bar's shield glyph counts pending asks, the
  panel (Mod+Ctrl+N, or a `nostrconnect://` link clicked anywhere)
  carries ask cards with Approve / Deny and an hour's remember, the
  app list with its levels and revoke and delete, the activity log
  newest-first, and the Pair pane with the vault's gates. Two things
  the plugin could not do are in: avatars on ask cards and the app
  list (fetched over https, cached in your state directory, an
  identicon when an app has none), and copy-to-clipboard pairing —
  the fresh URI lands in the clipboard with a mint-and-copy button
  instead of a QR code to scan with the device you are holding.

- **`Mod+S` opens the shell's settings panel.** The panel existed behind
  the bar's gear icon only, with a debug env var as the only other road.
  It now answers `kuma-shell msg settings` (toggle semantics: the same
  press closes it), and the image ships the keybind, named on the hotkey
  overlay like the record and screenshot binds. The key works against a
  shell that carries the verb, so it and the shell above ride the same
  image.

### Changed

- **The desktop shell is kuma-shell.** noctalia 5.2.0 leaves the image
  and the shell built from the kumaui tree takes its place — one
  process for the bar, notifications, wallpaper, idle, lock, control
  centre and the Nostr Signer, supervised by `kuma-shell.service` as
  before, with the binary staged into the build context beside kuma
  the way the nostr binaries ride. The keybinds follow: `Mod+D` opens
  the shell's launcher, the media and brightness keys go through its
  msg interface (the same sysmon the bar's widgets read, so key and
  widget cannot disagree), and `Super+Alt+L` targets logind —
  `loginctl lock-session` — which is the same road the idle timeout
  and sleep take, so the one lock screen answers all of them.
  `Mod+Ctrl+N` opens the Nostr Signer, and the `nostrconnect://`
  scheme handler lands in it with the offer. Two binds left and did
  not come back: `Mod+Ctrl+V` (clipboard history) and `Mod+Ctrl+W`
  (wallpaper) opened panels the shell does not have, and a bind that
  advertises a dead panel is worse than no bind; the shell's control
  centre and the Nostr Signer are new work away, not part of the
  switch. The kitty palette is static now — chosen once, shipped in
  the image — because the wallpaper-derived render died with noctalia.
  `kuma doctor` follows the shell: the environ check grades the cursor
  pair the unit must hand it, the idle check grades the watcher's own
  journal line instead of a config's promises, and the shell-config
  drift check is gone — the image's policy is in the binary, so there
  is no baked config for a machine to drift from. The sleep guard
  checks the process by name and no longer pings a session bus the
  shell never owned. Install note: the release's `kuma-shell` binary
  must sit beside `kuma` when building an image, exactly like the
  nostr binaries.

### Added

- **The lock screen authenticates under its own name.** The image
  ships `/etc/pam.d/kuma-lock`, the locker-shaped stack — auth riding
  `system-auth`, account auto-permitting — that makes the first entry
  of the desktop shell's `kuma-lock` → swaylock → vlock chain real
  instead of borrowing kbd's vlock file. No password or session
  modules, on purpose: the shell never opens a PAM session, and
  keeping pam_unix out of the account phase keeps its setuid journal
  noise out of every unlock. Unlock attempts are named `kuma-lock` in
  the journal, and nothing to do differently either way —
  `loginctl unlock-session` stays the backdoor.

- **The activity log persists, and the panel reads it.** What was
  asked, by whom, and how it went now survives daemon restarts —
  `log.json` beside the pairings, capped at the last 500 entries —
  and `kuma-nostr log` reads it. The panel grows an Activity tab
  where the same entries render newest-first, named by app and time.
- **A retried ask joins the first.** A client that retries the same
  request while the person is reading no longer stacks a pile of
  identical prompts: the retry joins the first card, the card counts
  it ("asked 3× · Sign a note"), and one answer serves every waiter,
  each through its own response id.

- **Asks and pairings speak the client's name, in Signet's words.**
  The pairing record's name is the client's own handshake metadata —
  the spec's optional connect fields (perms, name, image) ride the
  `bunker://` flow now exactly as they ride `nostrconnect://`, read
  as display hints and never as authorization, with an off-spec
  client's metadata blob at the third position recognized by its
  brace. An app that stays anonymous shows as a pubkey fragment, and
  the panel's avatar fetch is https-only with an identicon fallback.
  The paired-apps list reads like Signet's: two lines of fact per
  app — name, level badge, paired/how many asks/last used — and a
  tap opens the app's own view where the acts (level, revoke, delete)
  live, because a list is not a control panel.
- **Ask prompts read like decisions.** A signature ask says what the
  event would do in words — "Sign a note", "Send DM", "Update relay
  list" — with the kind and its name, the content whole, and a
  sensitive-action cue on the kinds that change identity, spend
  privacy or carry weight. The strict event parse that turned every
  odd-shaped event into "sign an unreadable event" is retired: the
  kind is read leniently, sensitivity fails open into "ask", and an
  event that truly cannot be read shows itself instead of a
  verdict-shaped shrug.

- **`kuma-nostr delete <app>` removes a paired app outright.** The
  record and its standing answers go, the live session goes with them,
  and a freshly minted URI pairs the same app again — no un-revoke,
  because deletion forgot rather than banned. Revoke stays the ban:
  the tombstone an app cannot cross until `unrevoke`. The panel offers
  both — trash deletes, shield revokes — on live and revoked cards
  alike.

- **The nostr layer.** A declaration with `[nostr]` enabled turns on a
  bunker: `kuma-nostrd`, a daemon holding a nostr signing key in your
  login keyring and answering paired apps over the relays; `kuma-nostr`,
  the CLI (`setup`, `generate`, `import`, `unlock`, `lock`, `touch`,
  `status`, `bunker --qr`, `connect`, `prompts`, `approve`, `deny`,
  `apps`, `revoke`, `unrevoke`, `level`, `rotate`, `destroy`); and the
  face in the shell — the bar glyph that counts pending asks and the
  approval panel behind it (see the Nostr Signer entry above, which is
  where that face lives now). A freshly paired app can ask for
  everything and signs nothing until a person answers; relaxing an app
  to Basic signs only the kinds an explicit safe list vouches for —
  notes, reposts, reactions, long-form, the everyday social surface —
  and every kind it does not name asks, the decrypts, NIP-04
  encryption, and NIP-44's general-purpose encryption riding like an
  everyday sign; Trust signs everything and `kuma doctor` grades it
  Warn by name. An approved ask can be remembered for an hour at most,
  and an unanswered one times out after five minutes — the app gets its
  refusal, and the log records the expiry. The pairings survive daemon
  restarts and lock/unlock cycles; a prompt approved is a prompt that
  executes in its window, not one that waited a week. Nothing to do
  differently unless the layer is wanted — absent or off, the image
  ships none of it — and a toggle never destroys anything: the key
  lives in your keyring, user state no image update touches, so a
  disable is reversible. The release carries the layer's two binaries
  beside kuma, because a nostr-enabled image stages them from beside
  the running binary: install them to the same place when the layer is
  wanted. The trust model is written down in SECURITY.md.

- **The full NIP-46 method surface.** A paired app can ask the bunker
  to `nip04_encrypt`, `nip04_decrypt`, `nip44_encrypt`, or
  `nip44_decrypt` for a third party; ask which relays it answers on
  (`switch_relays`); and end its own pairing (`logout` — the goodbye
  removes the record, the session, and the standing grants, and cannot
  reach any other app).

- **Both pairing flows.** `bunker://` — the daemon mints the URI, the
  app connects — and `nostrconnect://` — the client's own invite,
  pasted into `kuma-nostr connect <uri>` or the panel, whose paste is
  the approval: the handshake goes out on the client's own relays and
  the URI's secret echoes back as the result the client validates
  against spoofing. Every pairing URI carries a one-time secret, and
  the connect that uses it burns it — a URI pairs one app once, and a
  second connect with the same secret is refused; `kuma-nostr bunker`
  mints a fresh URI per call, and `rotate` still invalidates every
  outstanding secret at a stroke. The client's name and its requested
  permissions ride the pairing record as display hints, never
  authorization; every URI refusal names itself — a missing secret, no
  relay, a plaintext relay to a non-loopback host.

- **Revocation is a state.** `revoke` tombstones the pairing — the
  app's connect is refused whatever it carries, the tombstone survives
  restarts — and `unrevoke` clears it; the way back in is still a
  freshly minted URI, because the app's original secret burned at its
  first connect. The pairing is the bond: a known app's own reconnect
  re-verifies by identity, so a client that restarted itself needs no
  fresh URI.

- **The bunker refuses replays and sheds over-budget apps.** A relay
  redelivering its kind-24133 backlog changes nothing: an event id is
  answered at most once per window, a request implausibly old or
  future-dated drops, and one travelling backwards in its sender's own
  time drops with it. A token bucket per sender — ten a second
  refilling, thirty of burst headroom — sheds over-budget requests
  with no response, recording the shed in the activity log, so one app
  cannot spend the shared relays for every other.

- **An answer travels the road its p-tag names.** Each relay road is
  its own queue, and an answer goes to the declared set plus whichever
  app's relays its p-tag names — a nostrconnect handshake travels only
  its URI's relays. Revoking an app tears its relay roads down; one
  app's relay going down never touches the others'.

- **The inactivity switch.** Opt-in: the declaration's
  `inactivity_lock_secs` (or `kuma-nostrd --inactivity-lock-secs`)
  locks the vault — the same lock the panel's verb runs — after that
  long with no unlock and no keep-alive (`kuma-nostr touch` resets it
  without unlocking). The floor is one hour, 0 or absent is off, and
  off is the default: the desktop daemon's posture is the PAM-open
  keyring, and a switch on by default would lock the bunker while the
  person is away. The unit's exec line carries the flag when armed,
  and `kuma doctor` grades the armed state in words a person reads.

- **A local relay, and the tailnet mode.** Opt-in
  (`[nostr.relay] enable = true`): a relay on the machine itself —
  `nip46-relay`, ported from the Go original and carrying only kind
  24133/24135 traffic, in-memory, evicted after ten minutes, bound to
  loopback. When enabled, the daemon's relay list is local first, the
  declared fallbacks after, and declaring relays never removes the
  local one; the default declaration runs the bunker on the public
  relay alone, which is what makes a fresh install pairable from
  anywhere with nothing configured. When the declaration runs
  `tailscaled.service`, a converge script exposes the local relay to
  the tailnet under the machine's ts.net name — the relay is then
  reachable by a paired phone with no third party at all. Absent
  tailscale nothing is refused: the bunker is local-only, and doctor
  says so.

### Fixed

- **The bunker survives a sleep.** A relay connection that died while
  the machine was suspended stayed dead forever: the socket sat
  ESTABLISHED, the read timed out on the beat and nothing probed the
  wire, so after waking the daemon was deaf — your phone's asks
  answered by nobody until the next reboot, the status surfaces told
  nothing. The road now pings a quiet wire every thirty seconds and
  walks off after three pings with no answer, landing in the backoff
  that reconnects it; worst case a minute and a half of silence
  before the bunker is reachable again.
- **The nostr panel answers while it is open.** An ask arriving while
  you are reading the panel now shows up in it: the panel polls while
  open (the frame tick, asked for on open and given back on close)
  instead of refreshing only on open and on its own acts — which is
  why the bar's ask count moved and the panel's list did not.
- **A failed panel act says so.** A verb the CLI cannot land — a dead
  socket, a daemon refusal — now surfaces its error instead of
  repainting in silence, which is what once made a working revoke
  look broken.
- **`bunker --json` prints the document again.** It printed the bare
  URI, which no JSON parser can read — the panel's Copy fresh URI
  decoded `nil`, copied nothing, and the clipboard kept whatever was
  there before. Every verb's `--json` now prints the same shape, the
  one `docs/agents.md` always promised (`uri` in the document), and
  the button answers when a mint refuses or returns malformed.

### Changed

- **The system calls itself kumaOS.** The display name — os-release `NAME`
  and `PRETTY_NAME`, so the GRUB menu, fastfetch, and `hostnamectl` all
  follow — the login greeter's greeting, the fedora-release shim, and the
  fastfetch wordmark say kumaOS; the default hostname for new installs is
  `kumaos`. The binary, the crate, `ID=kuma`, and every machine-facing
  identifier stay `kuma`, and existing machines keep whatever hostname they
  already have. The repository is `Letdown2491/kumaos`; GitHub redirects the
  old address, and the cosign identity regexp in SECURITY.md moves with it —
  verifications of releases tagged after the rename must use the new path.

## v44.3.0 (2026-09-27)

### Fixed

- **A `kuma vm` disk no longer carries the install's dead blocks.** The
  install script trimmed the filesystem before throwing anything away:
  the store subvolume holding the image blobs and tmp's staged copies
  were still allocated at trim time, and deleting them afterwards frees
  space without discarding any — so the `qemu-img convert` that makes
  the qcow2 copied up to two gigabytes of nothing into the artifact.
  The trim now runs in the cleanup trap, after the store subvolume and
  tmp are gone and while the filesystem can still hear it; the ordering
  that keeps the mapper closing after the unmounts is pinned by test.
  Nothing a reader does changes, and the disk a machine boots from is
  byte-for-byte the same system; the qcow2 is just smaller.

- **An unparseable declaration can no longer quote a secret line back.**
  The toml crate's parse errors point at the mistake by quoting the line
  they span, and a `password_hash` with one wrong character in it is
  exactly such a line. That error text is the string kuma builds to be
  pasted — `kuma --json` carries it as the config fact and the edit
  affordance carries it as the reason — so the probe keeps the position
  and the verdict and drops the quoting, and `kuma check`'s
  not-a-crypt-hash message names the key instead of echoing the value.
  `doctor --report` already redacted; now everything that pastes does.

- **A credential file the readers would disagree about is refused in
  full.** The refusal list covered `$`, backticks, quotes and backslashes
  in values, but a line that merely *changes under trim* slipped past it:
  a `RESTIC_PASSWORD` saved by a Windows editor arrives with a trailing
  `\r`, the shell reader keeps that byte and the env-file reader does
  not, and the two log into the repository with different passwords. The
  check reads the raw text now and refuses any line whose trimmed form
  differs — CRLF endings, leading whitespace, a space before the `=`,
  trailing spaces on the value — naming the key either way. `kuma
  backup`, `kuma install --restore` and `kuma doctor` all run the same
  check; a file that was fine stays fine, and one that came off a stick
  through Windows says what is wrong with it instead of failing at the
  far end.

- **The restore suggestion survives a paste.** `kuma backup --restore`'s
  dry run names the command that performs the write, and it interpolated
  the path bare, so `/var/home/me/My Files/x` pasted as two arguments.
  The path is shell-quoted in the suggestion, in the JSON document and
  in the prose alike, the way `kuma capture` already quotes its own.

### Changed

- **Boot convergence stops where the declaration ends.** The units that
  converge flatpaks and brew at boot also updated every application on
  the machine and pruned unused runtimes — unscoped, ungated, on every
  boot, so a laptop on a metered connection paid a Flathub visit every
  morning to learn nothing had changed. Boot now answers the
  declaration's question only: what is named gets installed, what kuma
  installed and the declaration dropped is removed, and a machine that
  already matches its file runs nothing at all — no process, no vendor,
  no network. Keeping applications current, declared or ad-hoc, is the
  daily timer's job behind the battery-and-metered gate 44.1.0 added,
  which is where the docs already put it. `kuma sync` starts the boot
  unit and converges without updating. What to do differently: only if
  you relied on reboots to update applications you installed yourself —
  then the daily timer does what it was always for, or `flatpak update`
  by hand does it now.

- **Converged boots stop remounting /boot for nothing.** On every boot,
  `kuma-boot-health-sync` remounted /boot read-write, grepped two files,
  and remounted it back — including on the converged path where it
  writes nothing, which after the first boot is every boot. The greps
  run on the read-only mount now, and the remount happens only on the
  paths that write. Nothing to do differently; the boot journal is two
  lines quieter.

- **`kuma iso --live` stops re-downloading the tools' metadata.** The
  assembly container installed squashfs-tools and xorriso from a cold
  dnf cache on every build; a named volume now carries that cache
  between builds, the same pattern the compose's package cache already
  uses. Second and later builds on one machine skip most of the wait.

- **`kuma install` refuses a disk that belongs to a volume elsewhere.**
  The preflight asked whether anything on the disk is mounted, which an
  LVM physical volume and a raid member need not be: their other members
  sit on other disks, and wiping one of them breaks a VG or an array
  that may hold the only copy of something. One more lsblk asks what
  filesystem types the device tree carries, and `LVM2_member` or
  `linux_raid_member` anywhere in it is an objection named in the
  refusal, before the plan prints and before a password is asked. An
  unopened LUKS container is deliberately not objected to — reinstalling
  over an old kuma machine is the ordinary case, and its container
  endangers only itself.

## v44.2.0 (2026-09-26)

### Changed

- **`kuma vm` disks are built by kuma's own installer, and boot a btrfs
  root.** The disk half of `kuma vm` stopped handing the image to the
  frozen bootc-image-builder container (archived upstream 2026-06-18) and
  instead installs it the way `kuma install` installs a machine: the same
  partition layout, the same script, against a sparse raw file reached
  through a loop device, converted to qcow2 at the same
  `qcow2/disk.qcow2` path as before. Nothing to do differently — the
  verb, its flags and the output location are unchanged. What a reader
  gets: a VM disk that is laid out like the machine `kuma install` writes
  instead of an ext4 image, so the daily boot checks exercise the same
  snapshot, subvolume and converger paths real machines run; and the
  last load-bearing use of the frozen container is gone from disk builds
  (its pinned image still builds the deprecated `kuma iso` default, which
  flips to `--live` in 45.0.0). The convenience account on the console is
  unchanged — name and password `kuma`, wheel — and still trusts the
  host's ssh key, now delivered through the account file the first-boot
  converger reads rather than a bib blueprint.

### Deprecated

- **`kuma iso` without `--live`.** The legacy Anaconda media it builds is
  the last job of the frozen bootc-image-builder container, strangers
  download the live ISO instead (it is what releases attach), and
  Anaconda's manual partitioning buys nothing against kuma's fixed
  three-partition model. It keeps working all through 44.x, warning in
  its output; the default flips to `--live` in 45.0.0. What to use
  instead: `kuma iso --live`. The one real trade-off: a live install
  pulls the image over the network by design — an offline installer
  would be a new flag, not this default.

## v44.1.0 (2026-09-25)

### Added

- **An install records its own provenance on the machine.** Every install
  — `kuma install` from a host or live media, and a `kuma vm` disk, which
  installs by the same path — writes `/var/lib/kuma/install.json` beside
  the account and hostname: which kuma ran the install, when, from what
  media, the declaration it was driven from (hashed), the base digest the
  lock had resolved, and the image that landed. bootc records its own
  facts at the same root; this is the kuma half of the story, and the two
  answer different questions. `kuma doctor` says it back as an
  informational check, and stays silent on machines that have no record —
  every machine updated into this release is one, and absence is
  ambiguous, so nothing is graded on it.
- **The daily convergence timer waits for power and a real connection.**
  The timer that carries flatpak and brew installs fired on a sleeping
  laptop's catch-up whether the machine was on battery or paying by the
  megabyte on a metered connection; its run now passes a gate that reads
  the battery's own sysfs files and NetworkManager's `Metered` property
  (the same toggle the GNOME settings pause honours, so niri and COSMIC
  machines get it for free) and waits below 20% battery. A skipped run
  is a decision the machine reports, not a failure: the day's timer
  stays green, nothing installs, and `kuma doctor` says how many runs
  were skipped and why. Nothing to do differently — and the gate belongs
  to the timer only: converging at boot is the promise, and `kuma sync`
  always runs when asked.

- **`kuma doctor` grades a niri config that shadows the image's.** niri
  takes `~/.config/niri/config.kdl` instead of `/etc/niri/config.kdl`
  rather than merging, so one copied file unpins every bind, startup
  service and window rule the image ships, and the copy goes stale the
  moment an image update rewrites the config it was copied from —
  measured on a machine whose media keys still spawned the binary from
  before the last rename, where doctor had nothing to say while the keys
  did nothing. The check reads each account's shadow only to resolve the
  absolute paths its binds spawn: one naming a program the image does
  not ship is a Fail that names it, a shadow whose every spawn resolves
  is a Warn, and a machine running the image's config is Ok. Bare-name
  spawns and `spawn-sh` lines are deliberately unreadable from doctor —
  its PATH is not the session's — so the check misses those rather than
  cry wolf about keys that work. The fix it suggests moves the copy
  aside, which is enough because the image's config ends with an include
  of the account's `local.kdl`: the machine's own deltas survive.

### Changed

- **A machine that said nothing about weather stops calling weather
  vendors.** The shell's baked config now ships `[weather] enabled =
  false`, `[location] auto_locate = false` and `[plugins] auto_update =
  "none"`: a machine whose person never mentioned weather still
  geolocated itself by IP, called api.open-meteo.com on every login and
  retried every thirty seconds while the network was still coming up,
  and git-fetched two plugin repositories from github.com at startup —
  eight warnings in the first minute of a fresh offline session,
  measured. This is the same argument the community-template setting
  already makes, applied to the shell's other startup calls: a desktop
  that works offline should not call a vendor to render nothing, and an
  image-declared desktop does not auto-run third-party git repos without
  being asked. Everything disabled here is one settings toggle away per
  machine, and a machine that turns weather on is right to. The build's
  merged-export assert carries the two new values. One limit, the same
  everywhere else in kuma: a machine whose own settings file already
  pins these keys keeps what it pinned, and the image does not reach
  past it. Nothing a reader has to do changes; the weather and location
  warnings leave the journal of a machine that never asked for them.

### Fixed

- **The volume and brightness keys draw the OSD again.** The binds used
  to spawn a `kuma-osd` script that adjusted with `wpctl` and
  `brightnessctl`, on a comment's claim that the shell watched the
  changes and drew its own OSD from a `[osd.kinds]` config key — and no
  noctalia has ever had the key or the watcher. Nothing called the OSD,
  so for the whole life of that script the keys adjusted silently, and
  the visible half of a volume key was missing. The binds go through the
  shell's own `msg` interface now — `noctalia msg volume-up` and friends
  adjust and draw in one step — `kuma-osd` leaves the image, and the
  mute and mic-mute keys ride the same interface. Nothing a reader has
  to do changes; a machine that updates sees the OSD on the next
  keypress.

- **The nightly's S3 stopped being MinIO, and the wake-race recovery
  can finally recover.** Two nightly failures, three nights running,
  neither from a change on main. First, MinIO locked its community
  registries — quay, docker.io and ghcr answer unauthorized on every
  tag and digest now, measured — so the dead-disk stage's S3 is Garage,
  pinned by digest (the v2.4.1 multi-arch index), with the fixture
  staging the layout, bucket and key itself and the generated key
  becoming what the guest signs with. Second, the suspend-then-hibernate
  recovery shipped in 44.0.1 could never have fired: it keys on the
  guest reporting zero hibernation images after a wake, but it read that
  count through a retry that judges by exit code, and `grep -c` answers
  zero by printing 0 and exiting 1 — so the honest zero burned the whole
  retry budget, came back empty, and every lost race landed on the fail
  line instead. The zero is a successful answer now; a lost ssh still
  retries. Nothing a reader has to do changes; the nightly is where both
  are answered.

## v44.0.1 (2026-09-22)

### Fixed

- **The installer and qcow2 builds no longer pull a `:latest` that
  cannot move.** `kuma iso` and `kuma vm` run bootc-image-builder from
  `quay.io/centos-bootc/bootc-image-builder:latest`, whose repository
  was archived on 2026-06-18 and merged into osbuild/image-builder: the
  tag answers pulls but has been frozen at that date ever since, and a
  frozen tag on a frozen repo is still a moving pin -- one push over it
  and every later build silently takes whatever arrived. The image is
  pinned by digest now (the multi-arch index, verified 2026-09-22), so
  the bytes kuma builds against cannot change without a change to kuma.
  The successor repository carries the same container and the same
  `anaconda-iso` type forward; migrating to it is the follow-up, and
  the rebase research records it. Nothing a reader has to do changes.

- **The keyring assert survives Fedora's PAM stacks moving.** The
  build-time check that a desktop's greeter stack still calls
  `pam_gnome_keyring` grepped `/etc/pam.d/<greeter>` only, which is
  where Fedora 44 ships those files -- and Fedora 45 moves them to
  `/usr/lib/pam.d`, where the assert would fail every desktop build
  during the next base rebase. The assert now greps both directories
  and is satisfied by either, which on today's images is the same
  answer it has always given: Fedora 45's own stacks still call the
  module (verified against the beta payloads), so the check tightens
  nothing and loosens nothing. Nothing a reader has to do changes.

- **The nightly hibernate fixture no longer loses the wake-alarm race, or
  ten minutes to it.** The suspend-then-hibernate cycle can wake on the
  alarm with the guest's clock a fraction of a second behind the
  hibernate deadline, and systemd -- never contradicted, with no battery
  to consult -- takes the wake for a manual one and returns without
  hibernating. The cycle retried that once, and from 2026-09-16 the race
  lost both attempts on five nights in seven, each red night spending
  ten minutes in two 300s waits for a poweroff the machine had already
  declined to schedule, while the plain hibernate cycle passed on its
  first attempt every night. The cycle now watches the guest console for
  the wake, and when the unit returns without hibernating it hibernates
  the machine directly, over the same manager path the plain cycle uses:
  the poweroff, the image-on-disk check, and the same-boot resume
  assertions are unchanged. What is no longer asserted is that systemd's
  own classification of the wake agrees with the clock, which is the
  thing the fixture gets wrong and the hardware it models does not. A
  hung sleep still fails, now within a minute of the wake instead of at
  the ceiling, and a wake that ends in the guest resetting itself still
  retries the cycle on a boot the run can hold to. Nothing a reader has
  to do changes; CI minutes change by about ten a red night.

## v44.0.0 (2026-09-08)

### Fixed

- **Strings that reach a shell arrive as one word.** `vm --apply` pasted
  the image tag into a host `sh -c` line and into the guest's root
  `sh -c` unquoted, and `update --check` built its dnf cache paths from
  `$HOME` the same way: a tag or a home directory carrying a quote or a
  space spelled command execution where an argument was meant. All three
  now quote through state's `shell_quote`, and the guest's switch takes
  the tag as `"$1"` rather than as text inside the script. Doctor's
  deployment-stamp heal quotes the image id it writes for the same
  reason. Nothing a reader has to do changes.

- **A `--json` verb that fails after printing its own document now ends
  in one document, not two.** `doctor --json` on a machine with a failed
  check printed its findings document and then the central
  `{"ok": false, "error": …}` failure document after it, and
  `kuma check --json` on an invalid declaration did the same: stdout was
  two JSON documents back to back, which no caller can parse — the
  cross-version job's first sight of an upgraded machine failed on
  exactly that, reading a doctor answer that was neither document. Both
  verbs now end in the one document, with `ok` carrying the verdict and
  `error` naming the failure; the summary still rides stderr and the
  exit stays non-zero. An agent reading either verb gains an `ok` key
  and changes nothing else.

- **Rebuilding no longer poisons the dnf cache.** A cached copy of the
  RPM Fusion release RPM corrupted on any build that found one: librepo
  appended the re-download beside the cached bytes, dnf5 refused the
  result ("not a rpm") and then never re-downloaded it, so every build
  through a shared cache mount after a successful one failed at the mesa
  step — `kuma update`, and the CI image job on the same cache. The mesa
  step clears the commandline cache before the URL install now, at the
  cost of re-downloading 11.5 KiB per build. Nothing a reader has to do
  changes.
- `kuma install` no longer dies inside its own one-layer build when
  root's podman storage has only ever loaded the image — a fresh CI
  runner, mostly. The sync that hands the image to root's store now
  materializes its layer directories, where the first COPY used to fail
  inside an overlay mount ("no such file or directory"). Nothing a
  reader has to do changes.

### Changed

- **Release binaries are smaller, release builds a little slower.**
  `[profile.release]` now builds with thin LTO over one codegen unit and
  strips the symbol table. The binary is baked into every image kuma
  builds and is what cargo-binstall fetches, so its size is image and
  download size; the cost is minutes of release-build time nobody pays
  but the publishing workflow. Nothing a reader has to do changes.

## v0.21.0 (2026-09-04)

### Added

- **The contract, and the version scheme it speaks.** docs/contract.md
  states what kuma 44.0 promises, what later releases may add, and what
  44.0 declines to do, with the reasons. Every 0.x release was an alpha or
  a beta; from the next release the major version names the Fedora base a
  release builds on -- 44.x tracks Fedora 44, 45.x will track Fedora 45 --
  and the major is not a promise boundary: the promises carry through
  every release, and one that ends a promise announces itself beforehand
  through deprecations. README links the contract and SECURITY.md names it
  as the other half of the statement. SECURITY.md also now says what
  happens to the image signing key: what losing it costs, how rotation
  reaches machines while the old key is still held, and why a compromised
  key has no in-band path. Its Not-yet list now states what is actually
  unsigned, an image built from your own declaration and the base, rather
  than all of it. Nothing a reader has to do changes.
- Fedora 46 through 51 bases are named Ephraim, Grizzly, Helarctos, Iorek,
  Jambavan and Kodiak, so the bear no longer waits on a kuma release after
  a Fedora one. Nothing a reader has to do changes.

### Fixed

- **Every `--json` verb now ends machine-readably, whatever the answer.**
  `snapshot` and `backup` answered through hand-built documents that
  predated the response contract, and four of the six were missing keys
  every agent reads first: `backup --restore --json` carried neither `ok`
  nor `actions`, the init documents and `snapshot --json` carried one but
  not the other. All six now go through the one response interface, so
  `ok` and `actions` cannot be forgotten, and a restore preview names the
  command that applies it. The read verbs, which manage their own
  documents, used to fail with empty stdout; a failure in `--json` mode
  is now the same `{"ok": false, "error": …}` document the mutating verbs
  print. agents.md lists hibernate among the mutating verbs, which it
  always was.
- **`kuma hibernate --json` no longer calls a healthy swapfile unusable
  when run without root.** The file's offset lives behind one privileged
  call, and a declined sudo graded identically to a file the kernel would
  refuse: on a machine whose 15G swapfile was present, active and
  correct, the dry run answered `{"ok": false}`. Whether the kernel
  accepts the file is the kernel's own answer, and `/proc/swaps` is
  world-readable, so the unprivileged run now reports the repair it can
  see, with the file's size read from the kernel's table and the one
  thing it could not check named in its warnings. The privileged run is
  unchanged.
- `sync --json`'s "nothing to converge" answer now carries
  `baked_declaration_behind` and the same shape as every other sync
  answer: an agent reading the surface could rely on it everywhere
  except exactly the machine that had nothing to do.
- **The staged working directory is no longer widened to every local
  account for the length of a run.** Every privileged verb stages its
  script and the files it reads into a fresh 0700 tempdir, and until
  now the first root-run chmod'd the whole directory `a+rX`: the
  scripts, the install Containerfile, the fstab, readable by every
  local account from the first run until the verb ended. Nothing that
  reads them needs it, because root reads through a 0700 directory
  without help. The widen is gone, and with it the machinery that
  existed to keep it from touching a credential. A credential is still
  0600 from the moment it exists, and two guards keep it that way: a
  plain file cannot be staged over a credential's name, and a
  credential staged over an existing file is forced back to 0600
  rather than inheriting the wider mode.
- **`kuma doctor` no longer grades the desktop's own settings as a
  warning.** The shell config check compared what the desktop is
  running against what the image baked, and everything it can find
  comes from the shell's state file, the one thing that can make the
  two differ, so a person who customized once carried a warning for
  the life of the machine. The check still names the keys and the
  exporter that answers for them; it grades ok now, because a
  personalization is not a diagnosis. The state file is a full
  snapshot, so a kuma release that changes the image's default for a
  key it covers does not reach a machine whose state file predates it
  -- this check is where that difference is named, and the shell's own
  settings are the way to accept a new default. The state file wins
  over the image's config exactly as before.
- `kuma doctor`'s shell config line now reads "the desktop runs 1 of the
  image's settings differently", which is correct at one key and scopes the
  claim to the keys the image sets -- the rest of a person's desktop
  settings were never the image's to report. The state-file sentence and
  the compare action's description are one clause shorter each. Nothing a
  reader has to do changes.

## v0.20.0 (2026-08-31)

### Fixed

- **`kuma vm`'s disks carry the declaration's shell again when kuma runs
  inside a distrobox.** The one podman call in `vm` that spawned a private
  process did not escape the container kuma itself runs in, so inside one
  it never found the tag it was asked about, and every failure of that
  call is silent by design, so the fallback read as "no shell" and
  nothing could tell. It goes through the same escape every other podman
  call takes, which is where it always should have been.
- **A name capture cannot declare is refused when it is named, before
  it is echoed anywhere.** `kuma capture` narrows to names you type,
  and its dry run repeats those names inside the suggested command,
  the one action in the JSON document an agent is invited to run. A
  name the declaration could never hold, one carrying shell
  metacharacters for instance, rode into that suggestion as itself
  and was refused only later, by the write it would have broken. The
  names now pass the same alphabet the two lists capture writes
  enforce, before anything runs.
- The widen that opens a staged directory to root pruned its
  credentials with a `-path` pattern, and a glob pattern is what that
  is: a `*`, `?` or `[` in the path to a credential (TMPDIR is yours
  to set) would not match the file it named, the prune would miss, and
  the password hash or restore secret would be widened world-readable
  for the length of the run. The pattern is now the escaped literal
  path, and a test runs the real `find` against a directory named to
  try it.

### Changed

- **`kuma capture --json`'s dry run says `"dry_run": true`**, like every
  other gated verb's. It shipped without the field since the verb was
  born, so an agent reading the document had to infer a preview from
  `"written": false`. The document is otherwise unchanged.
- `kuma switch` and `kuma rollback` dry runs print the command they
  preview in the same `→` form every other dry run uses, rather than
  leaving it implicit in the prose.
- `kuma diff` no longer runs `brew list` to see what is installed: the
  Cellar directory says the same thing, and reading it saves the spawn
  on every run. The bare `kuma` probe already read it that way.

## v0.19.0 (2026-08-30)

### Added

- **`kuma install` can name the sizes it used to decide for you.**
  `--esp 1G` and `--boot 4G` set how big the EFI system partition and
  `/boot` are, and the interview asks for both with the defaults shown,
  so a person who does not care presses enter twice. The shape does not
  move: still three partitions, the root still takes what is left, and
  encryption still changes what the root holds rather than the disk. A
  size below what its partition is for is refused with the reason rather
  than a number (256M for the ESP, 1G for `/boot`), and the swapfile
  questions are measured against what the named sizes left over, so a
  disk that fits the defaults can be too small for what was asked, and
  the refusal says so with the arithmetic. The dry run prints the
  resolved sizes in its layout, the JSON surface carries them the same
  way, and the command it hands over keeps the flags it was given.
  Naming nothing changes nothing: the defaults are the sizes every kuma
  machine has been installed with so far.

## v0.18.0 (2026-08-27)

### Added

- **A machine with a swapfile closes its lid into suspend-then-hibernate.**
  Hibernate stops being something only a menu can ask for: `kuma install
  --swap` and `kuma hibernate` now also point the lid at
  suspend-then-hibernate, so a laptop asleep in a bag hibernates before
  the battery dies instead of draining out. On battery nothing times it:
  the machine hibernates on the firmware's own low-battery alarm, which
  knows the battery better than any setting could. Takes effect on the
  reboot the resume kernel arguments already demand; `kuma hibernate
  --off` takes it away with the rest. `kuma doctor` grades the lid
  beside hibernate: a machine that can hibernate but whose lid only
  suspends fails, and a lid setting with no swapfile behind it warns.

- **A hung desktop shell no longer suspends into an unlocked session.**
  The boot-time guard already ended a session whose shell had died; a
  shell that hangs rather than exits passed it, because a process
  existing is not a process locking. The guard now asks the shell, over
  the session bus it owns from its first moment, and ends the session
  when nothing answers twice: the machine sleeps showing a greeter
  either way, but only one of them was showing your work.

- **Every machine boots under a splash, and an encrypted one asks for its
  passphrase through it.** plymouth is base layer now, with a vendored
  spinner theme (spinner_alt, GPL-3.0, credited in `assets/CREDITS.md`):
  desktop boots show a spinner instead of boot text, and the LUKS prompt
  draws as a themed prompt with bullets instead of dracut's bare question.
  Nothing to configure, and a machine that declares no desktop keeps its
  textual boot. The image builds its initramfs with plymouth in it during
  `kuma build`, so the splash arrives with the rebuild, not with the
  machine's next kernel update.

### Fixed

- **The live ISO carries its kernel and initramfs once, not twice, and
  fits a release asset again.** Both files shipped in two places: inside
  the squashfs that becomes the live root, and under `images/pxeboot`,
  which is the copy the boot actually loads. The boot splash grew the
  initramfs by 156 MB (a splash needs a framebuffer, so dracut packs
  every GPU driver's firmware into it), and the doubled copy took the
  ISO from 1.80 GB to 2.11 GB, over the 1.9 GB budget for a GitHub
  release asset, failing the `iso` job. The in-squashfs copies were
  never read: a live boot loads the pxeboot pair, and installing pulls
  its image from a registry. `kuma iso` now excludes both from the
  squashfs; the ISO lands near 1.85 GB. Nothing for a reader to do.

## v0.17.0 (2026-08-22)

### Fixed

- **A credential file is no longer executed as root.** `kuma install
  --restore` writes a repository password to
  `/var/lib/kuma/secrets/restore.env`, and the first-boot restore used to
  read it with a shell, which runs whatever is on the right-hand side. A
  value like `$(...)` executed as root before anybody logged in. Both that
  unit and `kuma backup` now parse the file instead. **If your repository
  password contains a `$`, a backtick, a quote or a backslash, kuma will
  now refuse it by name**, because those characters meant different things
  to different readers of the same file. A repository created before this
  release was encrypted with the expanded value, so change its password
  with `restic passwd` before rewriting the file.

- **`kuma doctor` no longer reports a working swapfile as missing.** The
  hibernate check reads the swapfile through `sudo`, and a declined or
  unpromptable `sudo` graded the same as a broken file. It now says it
  could not ask. This was invisible in a terminal, where `sudo` prompts,
  and reliable for anything reading `doctor --json`.

- **Nothing suspends into an unlocked session.** The desktop shell owns
  idle lock, the lock keybind and lock-before-suspend, and it used to be
  started in a way that could not restart it. It now runs supervised, and
  a machine whose shell is gone ends the session rather than sleeping with
  the desktop on screen. A shell that hangs rather than exits is still not
  covered.

- **The desktop locks on idle again.** Every kuma niri machine has shipped
  an idle lock at 15 minutes and screen-off at 16 that never armed: the
  shell needs each idle behavior to name an `action`, and kuma set only a
  timeout, so both were dropped at startup. The config validated, `noctalia
  config export merged` showed both timeouts, and the machine never locked.
  It says so once in the journal (`idle behavior 'lock' ignored: needs an
  action`), which is where this was found. Rebuild and reboot to arm it; if
  you have set idle behaviors of your own in
  `~/.local/state/noctalia/settings.toml`, check each one names an action.
  **`kuma doctor` grades this now**, from the shell's journal, since that is
  the only place a refused behavior is reported.

- **A mounted disk image is refused again.** `kuma install --disk
  <file>` claimed to check whether the image was already mounted and could
  not: `lsblk` refuses a file path. It resolves the loop device now.

- **The install no longer widens your password hash.** For two process
  spawns, the account hash and the backup password were readable by every
  local account on the machine.

### Changed

- **`kuma doctor` says when your desktop is running something other than
  what the image set.** Settings you change in the shell are yours and the
  image will not overwrite them, but kuma cannot read that file, so
  `kuma diff` never mentioned it. Doctor now names the keys and points at
  `noctalia config export merged`.

- **`kuma doctor` fails a desktop whose shell is drawing its own defaults.**
  The shell reads kuma's config only because the service that starts it hands
  over the path, and one that comes up without it has a different bar, no
  palette taken from the wallpaper, and a first-run wizard. Doctor reads the
  running process rather than any file, because every file on such a machine
  still says the right thing.

- **`kuma build` is about four seconds faster.** It deleted the image it
  replaced by sweeping the whole store; it deletes that one image now.

- **SECURITY.md says that `sshd` is enabled on every image and that the
  firewall lets it through**, which means the account's password answers a
  prompt on port 22. That was true before and undocumented. Turn it off
  with `[services] disable = ["sshd.service"]` if you do not want it.

- **The examples added Trayscale, and the niri example removed
  TextEditor.** A machine rebuilt from an updated example gains
  Trayscale, and a niri one loses TextEditor, because convergence
  removes what it installed. Keep TextEditor by naming it in your own
  declaration.

## v0.16.0 (2026-08-22)

### Added

- **kuma's own verbs are in your launcher.** Eight entries: edit the
  declaration, show drift, review proposals, system health, check for updates,
  rebuild, roll back, snapshots. Type `kuma` into whatever launcher the session
  shipped and they come back. They are ordinary `.desktop` files, so there is
  no plugin to install and nothing to configure, and they are on the COSMIC
  desktop as well as the niri one.

  Each opens a terminal window and holds it open after the verb exits, so
  output you were meant to read is still there. Press enter to close it.

- **The niri desktop is one shell instead of eight programs.** Noctalia draws
  the bar, notifications, wallpaper, OSDs, idle, lock screen, night light and a
  control centre that owns wifi, bluetooth, audio and brightness. waybar, mako,
  fuzzel, wob, swaybg, swayidle, swaylock and wlsunset are gone from the image,
  along with `kuma menu` and the icon theme built for it.

  `Mod+D` opens the shell's launcher, which lists your applications and kuma's
  verbs together, `Mod+Ctrl+V` opens clipboard history and `Mod+Ctrl+W` the
  wallpaper picker; `Mod+Shift+/` lists every bind. Wifi and bluetooth are in
  the control centre rather than in a GTK window from another desktop, and its
  header holds lock, log out, suspend, reboot and shut down.

  Kuma configures it from the image: its own bar layout and fonts, its
  wallpaper, no first-login welcome screen, and two things noctalia ships
  **disabled** that would have been regressions to inherit, locking on idle
  and night light.

- **`kuma menu` is gone.** Everything it offered that was kuma's is in your
  launcher as a desktop entry, on COSMIC as well as niri, which the menu never
  reached. Everything it offered that was a device setting belongs to the
  shell's control centre. `kuma clean` removes the launch-count cache it left
  in your home.

- **`kuma edit`** opens the declaration this machine is actually using, in
  `$EDITOR` and otherwise nano, vim or vi. `kuma edit --print` prints the path
  it resolved without opening anything, which is the answer to "which kuma.toml
  am I editing" when a `./kuma.toml` in the current directory is outranking
  `~/.config/kuma/kuma.toml`.

### Changed

- **The desktop follows one palette.** On the niri desktop, kitty and GTK3
  applications now take their colours from the palette noctalia is showing, on
  every change and again at login. That palette comes from the wallpaper by
  default, so changing the wallpaper changes the terminal, thunar, pavucontrol
  and nm-connection-editor with it; switch the shell to a built-in palette and
  they follow that instead. The image ships `adw-gtk3-theme` and GTK3
  applications now use it rather than Adwaita.

  This includes the terminal's sixteen ANSI colours. A palette generated from a
  wallpaper maps all of them into one hue family, so a diff's `+` and `-` come
  out as tints of the same colour; a palette picked by name keeps real hues.
  The generated colours arrive as `~/.config/kitty/themes/noctalia.conf` and
  `~/.config/gtk-3.0/noctalia.css`: delete them and their include lines to keep
  the image's fixed palette.

### Fixed

- **A VM disk logs you into the shell its image declares.** `kuma vm` gave its
  convenience account a bash login whatever `[system].shell` said, so a
  declaration reading `shell = "fish"` produced a VM that handed back bash.
  `kuma install` already honored it. Rebuild the disk to pick it up.

- **niri's Important Hotkeys overlay says what the keys do.** The binds kuma
  splices into the session carried no titles, so the overlay that opens on
  first login named them by their command lines, one of which was an entire
  `sh -c` pipeline. The media keys are hidden from it now and the rest are
  named. `Super+Alt+S` is gone with them: it toggled a screen reader this
  image has never shipped, and a key that does nothing is worse than no key
  at all when the thing it claims to start is a screen reader.

### Known limits

- **Kuma cannot see the settings you change from the desktop.** The shell
  writes them to `~/.local/state/noctalia/settings.toml`, which wins over the
  config kuma bakes into the image and which the image will never overwrite.
  Nothing in kuma reads that file, so `kuma diff` will say a machine matches
  its declaration while the desktop is visibly running something else.
  `noctalia config export merged` is what shows which settings are in effect.

- **GTK4 applications do not follow the palette**, which on a kuma machine
  mostly means flatpaks. libadwaita ignores a user stylesheet that redefines
  its palette, so they keep their own dark theme while the terminal and every
  GTK3 application move.

## v0.15.0 (2026-08-21)

Swap was always zram, which is memory, so a kuma machine could sleep but never
hibernate. It can now put a swapfile on the disk, point the kernel at it, and
say plainly when the machine will refuse.

### Added

- **Kuma machines can hibernate.** Every machine's swap was zram, which is
  memory, so there was never anywhere to write a hibernate image. Kuma can now
  make a swapfile on the root disk and set the `resume=` and `resume_offset=`
  kernel arguments that resume from it.

- **`kuma install` asks**, after the encryption question, and creates the file
  before it pulls the image. Off unless you say yes. `--swap 16G` answers early
  and `--swap none` declines without being asked. On a disk you chose not to
  encrypt, the install plan says that hibernating writes the contents of memory
  to it in the clear.

- **`kuma hibernate`** does the same on a machine that is already running, so
  this needs no reinstall. It defaults to the size of memory, prints what it
  would do, and changes nothing without `--yes`. `--off --yes` removes the
  swapfile, its fstab lines and the kernel arguments. The kernel arguments take
  effect on the next boot.

  The file is never resized in place: growing it would move it on the disk, and
  the kernel would then resume from the wrong place. Change the size by turning
  it off and on again.

- **`kuma doctor` grades it**, and grades the part that fails silently. If the
  swapfile and the kernel arguments disagree, a hibernated machine boots fresh
  and the session is gone with nothing logged. Doctor compares the two and says
  so. Running `kuma hibernate --yes` on a machine that already has a usable
  swapfile repairs exactly that, leaving the file where it is. Machines with no
  swapfile are not graded, because they promise nothing.

- **Secure Boot machines are told the truth.** A kernel that booted with Secure
  Boot on runs locked down, and a locked-down kernel refuses to hibernate. Kuma
  can still make the swapfile and set the kernel arguments correctly, and the
  machine still will not do it. `kuma install` and `kuma hibernate` say so
  before you spend the disk on it, and `kuma doctor` warns rather than reporting
  a machine ready that never was. If you want hibernate on such a machine, turn
  Secure Boot off in firmware; otherwise `kuma hibernate --off --yes` takes the
  space back.

- **The swapfile is labelled for SELinux.** `systemd-sleep` can only read a
  file typed `swapfile_t`, and the policy's own default for a file under `/var`
  is `var_t`, which it cannot read. A machine with the wrong label has a
  correct swapfile, correct kernel arguments and active swap, and fails at the
  moment you ask it to hibernate. There are two labels to get right, not one:
  the file, and the directory `systemd-sleep` has to search to reach it. Kuma
  images declare that path a swapfile and relabel both at boot, and
  `kuma doctor` grades both.

### Known limits

- **Hibernate does not work under Secure Boot**, and that is the kernel's
  decision rather than kuma's. See above: everything kuma sets up is correct and
  the kernel still refuses. Turning Secure Boot off in firmware is the only way
  to have both.
- **Proven in a virtual machine, not on your hardware.** A gate installs a
  machine, hibernates it, boots it again and asks three questions the answer to
  "did it come up" cannot answer: the kernel's own `boot_id`, a marker in
  tmpfs, and whether uptime continued. All three say the same session came
  back. What that cannot cover is your machine. Lid-close behaviour, firmware
  that mishandles S4, and drivers that do not survive a suspend vary by
  hardware, and none of them are things kuma can test for you.
- **Hibernating over ssh is refused**, and not by kuma. `systemctl hibernate`
  asks logind, which gates it on polkit, whose policy wants an active session;
  an ssh login is not one and there is no agent to answer the prompt. Hibernate
  from the desktop, where your session is active.

## v0.14.0 (2026-08-20)

A declaration describes a system; it never described your files. `[backup]`
copies them somewhere else, and `kuma install --restore` puts a machine back.

### Added

- **`[backup]`** copies what `[snapshots]` keeps to a restic repository, on a
  timer, reading from a snapshot so nothing changes mid-copy. Requires
  `[snapshots].enable`; `kuma check` says so rather than the unit failing at
  3am.

- **The credential is named, not held.** `secret = "backup"` points at
  `/var/lib/kuma/secrets/backup.env`, mode 0600, which you create. A
  declaration is committed and baked world-readable, so it is the wrong place
  for a password; a repository address containing one is refused. Recovering a
  machine therefore needs two things: this file and that credential.

- **`network_connections`** carries `/etc/NetworkManager/system-connections`,
  and is **off by default**. Those files hold a passphrase per network and
  nothing else can recreate them, so `kuma doctor` names which way it is set.

- **`kuma backup`**: bare reports without touching the network, `--init` seeds
  the first copy, `--list` asks the repository, `--restore` brings a path back
  after a dry run.

- **`kuma install --restore <file>`** rebuilds a machine from the repository.
  One file carries the address and its credentials. The restore runs at first
  boot, after `/var/home` becomes a subvolume; if the repository is
  unreachable that boot, the next one tries again.

- **`kuma doctor` grades backups** on a stamp only a run that copied something
  writes, so a machine that has quietly stopped is visible. Staleness follows
  your declared interval. It also grades the credential's mode.

### Changed

- Retention applies every copy; pruning runs weekly, because pruning repacks
  and moves far more data than forgetting a snapshot does.
- `kuma check` on a valid declaration now names the next command, and its JSON
  carries `actions` either way.
- `kuma init` no longer pins `system.base`, so a first declaration composes its
  own base like every published image.
- `doctor`'s dangling-enablement check reports as `enablement` rather than
  `units`, which the failed-unit check already used.
- `kuma switch` pipes the image into root storage instead of staging 1.5 GB
  through a temp file, and `doctor` runs its podman probes concurrently.

### Fixed

- **`kuma install --restore` left the repository credential world-readable**
  for the length of an install.
- `kuma install --json` emitted no JSON on failure and printed progress into
  the document.
- `backup.repo` reached generated shell without validation.
- `kuma install --groups` was unvalidated where a declaration's groups are.
- `kuma-brew-setup` wrote as root into a directory tree a normal account owns;
  it refuses a prefix it does not own.
- A live session no longer arms kuma's timers or converges Flatpak
  permissions.
- `[system.ca_certificates]`, added in 0.13, was documented nowhere.


## v0.13.0 (2026-08-20)

State that survives every rebuild, that the declaration could not express and
nothing on the machine would report. This closes the biggest of it and draws
the line around the rest.

### Added

- **`[overrides]`** declares Flatpak permissions, per app and per scope.
  Convergence is **per key, not per file**: kuma sets the keys you declare,
  removes the keys it set that you stopped declaring, and leaves every other
  line alone, so Flatseal stays usable and this file stays the record. The
  shape is Flatpak's own override file rather than `flatpak override`'s
  flags, and `flatpak override --show` round-trips into it. Applied at boot
  and by `kuma sync`, never on the daily timer, because a permission changing
  under a running app is indistinguishable from a bug.

- **`[system.ca_certificates]`** declares certificate authorities to trust,
  keyed by the name each gets on disk, with the certificate inline: a
  declaration pointing at a path elsewhere is not one file. A private key
  there is refused, since it would be baked world-readable into every image.

- `kuma doctor` reports a unit that is enabled with no unit file, and
  `kuma add --flatpak` refuses an id Flathub does not list.

### Changed

- `kuma sync` says which declaration it converged to, so a machine converging
  to the image's baked lists rather than the file in your hand says so.
- `kuma add`, `kuma remove` and `kuma capture` no longer claim flatpak and
  brew changes apply immediately when they do not.

### Fixed

- A declared `system.timezone` produced exactly one file and nothing graded
  it, because it arrives as a symlink rather than a copy or a redirect.


## v0.12.0 (2026-08-19)

Claims that were true and unchecked became commands that pass or fail, after a
converger stopped converging on the day 0.11.0 shipped and only a person
reading a journal could tell.

### Added

- **`kuma menu`**, bound to `Mod+D` on niri: applications, connect,
  declaration, system, notifications, power, drawn by the launcher kuma
  already ships. It lists applications itself rather than opening a second
  launcher, honouring the desktop entry spec (`NoDisplay`, `TryExec`,
  `Hidden`, `OnlyShowIn`, `NotShowIn`, `Terminal`, field codes, shadowing),
  and orders by launch count. Opening a group narrows what is shown, never
  what typing can reach.

  `kuma menu --list` prints the rows instead of drawing them, for ssh, a VM
  with no session, or working out why a row is missing.

  The build repaints the icons the menu names into `/usr/share/icons/kuma`,
  because Adwaita's symbolic icons hardcode a near-black fill that is
  invisible on kuma's launcher background.

- Suspend, reboot and power off are reachable from the menu, and
  `NetworkManager-tui` ships on niri as what it offers for network settings.
- `kuma doctor` reports an override pointing at nothing, and a machine that
  has stopped converging rather than only one whose last run failed.
- AppImages run without a declaration naming anything.

### Fixed

- Every example declaration this project has shipped is tested against the
  current schema, so an old file keeps working.
- Every command the docs tell you to run is checked against the real CLI, and
  every verb is named somewhere a person reads.
- A keybinding that spawns a kuma verb names one that exists.
- Publishing an image runs the checks that install and boot it.
- One app's broken download no longer fails Flatpak convergence forever.
- `kuma sync` recovers a converger that spent its start limit, and `kuma
  doctor` quotes what a failed one actually said.
- **The boot menu names the version it boots.** Entries had been naming the
  version that previously held the slot.


## v0.11.0 (2026-08-18)

The media is a download. v0.10.0 built the ISO in CI and booted it on every
push to prove it could, and left attaching it to a release switched off until
that job had a history rather than a first day; it has one, so a release now
carries the thing you write to a USB stick. The walkthrough leads with it,
because "describe a machine, build an image, then make your own media" was the
order a project with nothing to download had to teach.

One thing the release also fixes is the assertion that guards v0.10.0's
signature policy, which could not see the policy going missing.

### Added

- Releases carry the live ISO. A tag builds it, boots it, signs it with
  Sigstore like every other release asset, and attaches it to the release that
  already exists, so downloading kuma and installing kuma are the same page.
  Booting it is not a formality: the same script CI runs starts the ISO under
  UEFI and asks the live session whether it reached a desktop, so the file on
  the release page is one that came up rather than one that built. This was
  wired in v0.10.0 and left off, waiting on the job that builds it having a
  run history rather than on a tag being its first real exercise; ci.yml has
  built and booted the ISO on every push to main and on a daily cron since,
  and went green before this was turned on.

### Fixed

- The install-and-boot smoke tests could not see a missing signature policy.
  They asserted that `kuma doctor` reports nothing graded `fail`, and the three
  ways this control goes missing are all graded `warn`: no policy file, one
  that will not parse, or one that does not name kuma's repository. Only a
  policy naming a key it does not have, or one with nowhere to look for
  signatures, was ever `fail`. So the scan saw the half-broken states and was
  blind to the absent one, which is the likeliest of the three and the one an
  `/etc` merge can cause. An installed machine now has to grade `signatures`
  as `ok`, which is the requirement rather than "not fail" because every image
  writes the policy, the key and the registries.d entry unconditionally. The
  cross-version job reports whether upgrading brings the policy to a machine
  installed before it existed, and fails only if an upgrade takes it away.

### Changed

- The getting-started walkthrough leads with installing a machine rather than
  building an image. It was ordered "build an image, then build media" because
  media was something you had to make yourself, and it said so; with media on
  the release page the front door is download, boot, install, and describing
  your own machine is what you do next rather than what you do first. The
  builder's path is unchanged and still there, one step later.

## v0.10.0 (2026-08-17)

The release that makes kuma installable by somebody who is not its author: a
download link becomes a booted machine, with no clone and no toolchain.

### Added

- **CI builds the live ISO, boots it, and keeps it as an artifact**, with a
  size guard below GitHub's 2 GB asset cap. Until now the media a stranger
  downloads was built by hand on one laptop.
- **Every image refuses an unsigned kuma update.** Images carry kuma's public
  key and a `policy.json` naming it, and `kuma doctor` grades that the machine
  actually requires a signature.
- **`kuma doctor --report`** prints what to attach to a bug report: findings,
  version, booted digest, and the declaration with secrets redacted.
- Fedora 45 bases are named Callisto.

### Changed

- A machine says which kuma built it: `PRETTY_NAME` is `Kuma <version>
  (<bear>)`, rewritten even when no bear matches the base.
- `update` and `update --check` report `fedora_release` in one shape, and
  `kuma update` says when it is about to change your Fedora release.
- The live ISO's boot menu carries a serial console.
- Both Font Awesome generations are installed and listed in waybar's font
  stack.
- `SECURITY.md` names the two package sources a desktop brings in beyond
  Fedora's own, and the README says kuma has only been booted on AMD
  graphics.
- The walkthrough describes installing from published media rather than from
  a clone.


## v0.9.0 (2026-08-16)

CI boots and installs what it builds, so a release no longer depends on
somebody booting it by hand.

### Added

- **The boot and install stages run in CI**, on every committed example.
  `scripts/smoke.sh --published <image>` installs an image kuma published and
  boots the disk it wrote; `--upgrade-to <new>` installs an older release and
  moves it forward; `--encrypted` installs a LUKS disk and unlocks it at the
  console.
- The boot checks ask the machine to grade itself with `kuma doctor --json`,
  and no unit named `kuma-*` may be failed.
- The disk under test gets `console=ttyS0`, so a machine that never boots
  still leaves evidence.

### Fixed

- **`kuma-home-subvol` and `firewalld` no longer race for `/var/home`.** The
  converger runs in an early slot instead of ordering itself against a list of
  units, and it now says why it declined rather than exiting silently.
- `kuma install` no longer refuses a disk for want of a tool that is present.
- The bar showed bluetooth twice, and two session services had two launch
  paths each.


## v0.8.1 (2026-08-16)

The snapshot timer takes a snapshot.

### Fixed

- The snapshot script asks `findmnt` which filesystem holds the target
  rather than what is mounted exactly at it. A btrfs subvolume does not
  have to be a mount point, and on a machine kuma installs `/var/home` is
  one nested inside the deployment's `/var`: the bare form printed
  nothing, so the script decided the target was not btrfs and exited 0
  having taken nothing, while `kuma doctor`, which has always asked with
  `-T`, said the target was fine. This was the second half of the same
  bug as the missing subvolume, and it survived fixing the first: an
  install from the v0.8.0 image gets a proper subvolume and still took no
  snapshot until this.

## v0.8.0 (2026-08-15)

Disk encryption, and the install path a stranger takes.

### Added

- **`kuma install --encrypt`** makes the root a LUKS volume. The passphrase is
  asked for on a terminal, read from stdin, and never appears in a flag, a
  file, or the process list. Nothing keeps a copy: a lost passphrase is a lost
  disk.
- `kuma doctor` says whether the root is encrypted.
- `kuma install` says when the image it is installing declares an account of
  its own, and defaults to the image its installer media was built from.
- Bare `kuma` says `converging` while a sync unit is running, rather than
  reporting drift against a machine that is mid-convergence.
- `scripts/smoke.sh --install` writes a real encrypted disk and verifies it.

### Fixed

- **Every image gives `/var/home` a btrfs subvolume on first boot**, without
  which `[snapshots]` silently took nothing on machines kuma installed.
- `kuma doctor` grades `kuma-user-sync` on installed machines, where the
  account is created rather than declared.
- `kuma clean` reclaims what `kuma iso --live` leaves behind.


## v0.7.0 (2026-08-15)

Installing became something a live session can actually do.

### Added

- `[system].shell` declares the login shell accounts get, separately from
  `[user].shell`, so shareable media can carry it.
- `kuma install` partitions the disk itself, takes a file as a target for
  building disk images, refuses a disk under 16G before asking anything, and
  refuses a `localhost/` image as an update source. `--update-from` installs
  one image while tracking another.
- A live session offers `kuma install` as its one affordance.
- The composed base ships `ncurses`.


## v0.6.0 (2026-08-14)

`kuma install`, and the media to run it from.

### Added

- **`kuma install`** installs kuma onto a disk. With no `--disk` it lists what
  it found and asks; `--image` defaults to the published image. Whole-disk and
  destructive: `bootc install to-disk` owns the layout.

  It asks for an account and a hostname, since a published image can declare
  neither, and writes the answers to `/var/lib/kuma/user`, which bootc fills
  from the image once at install and never touches again. Without `--yes` it
  describes what it will ask for rather than asking.

- **`kuma iso --live`** builds installer media in which the image is its own
  live root. Media for trying kuma, not yet for installing from. The live
  session runs SELinux permissive.

- The composed base ships firmware for Intel wifi and SOF audio.


## v0.5.0 (2026-08-13)

The machine notices when its own bytes went stale. Taking a new kernel is
still something you ask for.

### Behavior

- `kuma update --check` reports every package that has moved, for a composed
  base. It asks dnf which installed packages have a newer version in the repos
  and which of those carry security advisories, then prints them worst first
  with a `20 moved, 16 with security advisories (5 important, 11 moderate)`
  summary. Seconds, and it builds nothing. Previously a composed base had no
  cheap question at all and the check could only say so. A declared base still
  reports whether its tag moved, because a rebuild layers rather than upgrades
  and its packages are not in play.
- The check asks the running machine when there is one, so it does not care
  whether kuma arrived by ISO, `kuma switch`, or a rebase, and needs no image
  in podman storage. A host that is not a kuma machine is asked about the image
  it builds instead. The output names which of the two answered.
- Repo metadata for that check is cached under `~/.cache/kuma/dnf`, about
  140MB. The first run fills it and takes roughly half a minute; later runs
  re-check freshness and answer in a few seconds. Nothing needs root: the
  default dnf state directory would have, and a check that prompts for a
  password is a check nobody runs.
- `kuma doctor` reports how old the booted image is and warns past 30 days,
  which on Fedora means at least one kernel you did not take. Nothing applies
  an update on a schedule: an image update replaces the whole OS and lands on
  the next boot, so it stays a decision. A machine with a newer deployment
  already staged is told to reboot rather than warned twice.
- Bare `kuma` reports when an image was built by a different kuma than the one
  running. Images record their builder in the `io.kuma.builder` label, and an
  image without the label counts as different, so this fires on machines built
  before it existed. Previously a machine whose declaration had not moved read
  as `in-sync` no matter how old the binary that built its image was.

### Fixed

- Anaconda writes a `/` line into `/etc/fstab` describing the root as the
  filesystem it installed onto. On a bootc machine the root is a composefs
  overlay, so `systemd-remount-fs` failed on every boot of an ISO-installed
  machine. Images now carry `kuma-fstab-sync`, which comments that line out
  when the kernel reports the root as an overlay and does nothing otherwise. A
  machine installed today fails the unit once and is clean on every boot after.
  `kuma doctor` no longer excuses the failure once the cause is gone.

## v0.4.0 (2026-08-09)

A machine can run kuma, and its disks are built on ext4.

### Behavior

- Every image ships `/usr/bin/kuma`: the binary that built it, copied in rather
  than downloaded. A machine installed from a 0.3.0 image had the baked
  declaration, the convergence units, and the helpers, but nothing to run them,
  so `kuma update --yes` on an ISO-installed machine was a documented promise
  with no binary behind it.
- `kuma vm` disks are built on ext4 rather than xfs. A disk from 0.4.0 is a
  different filesystem than one from 0.3.0. Nothing migrates and nothing needs
  to, since `kuma vm --rebuild` makes the new one.
- Images name `sshd.service` rather than inheriting it from Fedora's preset.
  Behavior is unchanged, since sshd was already enabled on every kuma machine.
  What changes is that `services.disable` can turn it off, and an upstream
  preset change can no longer quietly alter what a kuma machine exposes.
- The example declarations dropped LibreOffice, Bazaar, and org.gnome.Firmware,
  and are renamed to `niri.toml`, `cosmic.toml`, and `minimal.toml`. Rebuilding
  from an updated example takes those three flatpaks back off the machine,
  because convergence removes what it installed. Add one back with
  `kuma add --flatpak`.

### Fixed

- One `kuma vm` build left udisks2 mounts holding a loop device open, and every
  later build from the same declaration then failed on a duplicate filesystem
  UUID, forty lines into an osbuild traceback that named neither the loop
  device nor the mount. ext4 permits duplicate UUIDs, so the collision can no
  longer fail a build, and `kuma vm` names any stale mounts it finds along with
  the commands that clear them.
- `kuma vm` on a host with no ssh key built a VM reachable only by password.
  It generates an ed25519 throwaway into the VM output directory instead, and
  reuses one already sitting there rather than locking out disks beside it. The
  launch message now names the key that will actually work.
- An ISO built from the shipped example installed a machine nobody could log
  into: declaring a `[user]` removes Anaconda's create-a-user screen, and with
  no password hash the account was created locked. The examples no longer
  declare a user, which makes them directly usable as shareable media.

## v0.3.0 (2026-08-08)

The download URL was still serving convergence that let packages rot.

### Behavior

- Convergence updates everything on the machine, not just what the declaration
  names. Both syncs previously upgraded only the declared list, so an
  undeclared flatpak, a brew cask, or a runtime no declared app demanded was
  never updated by anything, on a machine running convergence daily. `brew
  upgrade` and `flatpak update --system` now run without an argument list.
  This takes no authority kuma did not have: membership still comes from the
  declaration, removal still reaches only what convergence installed, and
  `flatpak mask` and `brew pin` still hold a package where it is.

  v0.3.0 exists mainly to get this to the front door. `releases/latest/download/`
  resolves to the newest non-prerelease, so it was still handing out v0.2.0,
  and a machine built from that binary looks healthy while nothing it installs
  outside the declaration ever updates.

## v0.2.0 (2026-08-08)

Getting kuma needed a compiler, and the binary could not say which one it was.

First tagged release. Everything before it is in the git log.

### Behavior

- Kuma is published as a static `x86_64` binary that needs nothing installed
  alongside it, which is the point on the image-based machines most likely to
  want it: podman and no toolchain.
- Every release asset is signed with Sigstore and carries one bundle, verified
  with `cosign verify-blob --bundle`. See [SECURITY.md](SECURITY.md).
- A rolling `latest` prerelease tracks `main` between releases.
- `kuma --version` reports the commit it was built from, and appends `-dirty`
  when that tree had uncommitted changes.
