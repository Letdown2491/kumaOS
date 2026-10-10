# Shell startup research: login → bar visible (2026-10-04)

Research only; no source changed. Machine: `motherbox`, image `kuma:latest`
44.5.0 (greenboot line, journal 01:33:40). Companion note:
`docs/agents/live-session-experiment.md` (same candid style, same machine).

## 0. Correction to this morning's numbers

The boot at Oct 04 01:33 was measured **before the current fixes**. The
~11.5 s exec→first-log-line gap from that boot is stale; the current
shell-side gap is **about 5 s** (per the session's owner). Two fixes
already landed that plausibly account for most of the difference:

- The greeter post-auth handoff optimization, commit `a98a1c5` in the
  kuma repo, measured at ~5 s saved on the greeter→session path
  (journal 01:33:47.572 PAM auth → 01:33:48.669 greeter session closed:
  under a second, where it used to be several).
- Startup instrumentation landed in the shell itself the same morning:
  kumaui commit `df205c3` "Shell logs boot-phase timings at startup"
  (`~/Documents/kumaui/kuma-shell/src/main.rs:26-30,43,49,52,56,121`).
  The 01:33 boot predates it: the image binary `/usr/bin/kuma-shell`
  has no `boot:` strings, and the instrumented `~/.local/bin/kuma-shell`
  (the unit's override `ExecStart=%h/.local/bin/kuma-shell`, per
  `systemctl --user cat kuma-shell.service`) was built at Oct 04 01:56,
  *after* the measured boot. So the 01:33 boot ran an uninstrumented
  binary and we have no phase breakdown yet. **Next login on the live
  machine gives the phase table for free**; that is the first thing to
  capture.

So: treat **~5 s inside the shell binary (exec → first frame)** as the
residual cost. Everything below decomposes it.

## 1. The OS-side chain is not the problem (measured)

From `journalctl -b -o short-precise`, Oct 04 01:33 boot:

- 01:33:47.572 greetd PAM auth of the local user completes
- 01:33:48.669 greeter session closed
- 01:33:48.958 PAM session opened
- 01:33:49.310 `systemd[1517]: Starting niri.service`: the **user
  manager was warm** (started 01:33:48.925, "Startup finished in
  199ms" at 01:33:48.965; journal `systemd[1517]` lines). No cold
  user-manager span this boot.
- 01:33:49.659 niri.service started (`journalctl -b -u niri.service`:
  niri's own log shows config loaded 01:33:49.366, Wayland socket
  listening 01:33:49.600, `Started niri.service` 01:33:49.659).
- 01:33:49.681 `Started kuma-shell.service` (exec'd).

Total PAM→shell-exec: **~0.7 s**. The `session-deploy.md` analysis of
the serial handoff (`~/Documents/kumaui/docs/session-deploy.md`,
"The blank-second problem") is correct in structure but its spans 1 and
2 are now small on this machine: greetd→niri-start ~0.6 s, niri-start→
shell-exec ~0.4 s (the "niri-session wrapper → import-environment →
niri.service → graphical-session.target" systemctl round trips all fit
inside those). Niri itself is fast: exec→socket-listening in ~0.29 s
(01:33:49.336 "starting version 26.04" → 01:33:49.600 "listening on
Wayland socket", journal `-u niri.service`).

The cold-user-manager hypothesis in session-deploy.md ("if the first
span dominates on the first login after boot, the user manager is
starting cold") did **not** apply on this boot: the user manager
started in 199 ms because greetd's PAM session poked it before niri
needed it. On a *cold* boot with no prior login it could still bite;
worth one confirmatory login next measurement pass.

## 2. What the 01:34:01 "first log lines" actually were (measured; they are not the shell)

The Chrome-extension error lines at 01:34:01.186 are **not kuma-shell
and do not mark the shell's first frame**:

- `journalctl -b --no-pager _PID=2097` → the process is
  `/app/chromium/chrome --enable-features=WebRTCPipeWireCapturer …`:
  the **Flatpak Chromium** (`/app` is the flatpak sandbox root). Its
  stderr lands in `kuma-shell.service`'s journal only because it runs
  inside that unit's cgroup (something spawned it from the shell's
  process tree, most plausibly the user opening the browser through
  the shell moments after login; PID 2097/2288 vs the shell's own
  later "kept alive" PIDs 5321+).
- The kumaui tree embeds **no webview**: grep for
  `webview|chromium|cef|webkit|zeno` over `kumaui/*/src` matches only
  test fixtures (`kuma-shell/src/sway.rs:390-396`, a window-title
  test) and the default-browser probe
  (`kuma-shell/src/settings.rs:811`). No CEF/WebKit dependency exists
  in `kuma-shell/Cargo.toml`.
- The shell binary contains no `kept alive` string either
  (`strings ~/.local/bin/kuma-shell`, no match), so the "kept alive"
  lines (01:35:45 onward) are also from child processes in the
  cgroup, not the shell.

Consequence: the journal does **not** timestamp the bar appearing, and
the 01:34:01 lines are red herrings. The real instrument is the new
`boot:` log chain (§3).

## 3. The shell's startup path (source read, `~/Documents/kumaui/kuma-shell/src/main.rs`)

Work before the bar's first frame, in order:

1. `env_logger` init, arg parse, WAYLAND_DISPLAY check
   (`main.rs:13-41`). Trivial.
2. `application().with_assets(KumaAssets).run(...)` (`main.rs:45-48`)
   → `gpui_platform::application()` (`vendor/zed/crates/gpui_platform/…:
   `current_platform(false)` → `gpui_linux::current_platform`).
   This is the big unknown, **GPUI Linux platform init**: Wayland
   connect (niri's socket is up, so no waiting there), EGL/wgpu GPU
   device + surface creation on the amdgpu render node, and the text
   system's font discovery. GPUI's SVG renderer pays attention to
   fontconfig (`vendor/zed/crates/gpui/src/svg_renderer.rs:324` notes
   fontconfig override behavior); the font stack's cold-vs-warm
   fontconfig cache cost (`~/.cache/fontconfig`, present and warm on
   this machine: 30+ `*.cache-11` files) is the classic first-login
   tax and is **unmeasured here**.
3. Inside the app closure (`main.rs:49-121`): niri IPC connect
   (`session::connect`), `Settings::load` (TOML parse + theme
   refresh), then a run of subsystem starts (wallpaper, night-light,
   sysmon, OSD, notifications (DBus), tray (DBus StatusNotifier),
   nostr, weather, lock, idle) before `surfaces::init`/`ensure` open
   the layer-shell surfaces (`main.rs:106-121`). Most of these are
   async starts (smol/DBus), but anything that blocks the closure
   delays `surfaces::ensure`. The instrumentation exists precisely to
   price this; until a login is captured, the split between "gpui
   init" and "closure work" is **inferred, not measured**.
4. `surfaces::ensure(cx)` opens the bar/dock layer surfaces;
   `boot: surfaces up` (`main.rs:121`) is the closest thing to a
   first-frame marker in the log today. Note it marks *request*
   submitted, not pixels shown; niri's log (or a wayland round-trip
   `wl_display.sync` after the first commit) would be the exact
   first-frame proof.

No other blocking network calls were found on the startup path
(weather/nostr start async runners; their first fetches are spawned,
not awaited).

## 4. Ordering facts (measured / unit files read)

- `/usr/lib/systemd/user/niri.service`: `Type=notify`, `BindsTo=`
  and `Before=graphical-session.target`, `Wants=graphical-session-pre.target`,
  `After=graphical-session-pre.target`. No serializing surprises.
- `/usr/lib/systemd/user/kuma-shell.service` (+ image generation in
  `src/containerfile/blocks.rs`, `SHELL_SERVICE` ~line 2035):
  `Type=simple`, `After=graphical-session.target`,
  `PartOf=graphical-session.target`, `WantedBy=graphical-session.target`,
  `Restart=always`, `Slice=session.slice`. On this boot
  graphical-session.target was reached 01:33:49.659 and the shell
  exec'd 01:33:49.681, **22 ms** of target-chain overhead. The
  `Type=simple` semantics mean systemd considers the shell "started"
  at exec, so nothing on the systemd side waits for the shell to
  actually draw; Type=simple vs notify is irrelevant to time-to-bar.
- The `session-deploy.md` re-anchoring candidate (`After=niri.service`
  instead of `After=graphical-session.target`) can therefore buy at
  most the ~22 ms target span measured here. It is harmless but near-
  worthless on the evidence of this boot; the doc's own measurement
  discipline points away from it.

## 5. Ranked candidate improvements

Expected savings are estimates against the residual ~5 s shell-side
cost; the phase split is not yet measured, so ranks 1–2 are first
*measure*, then *fix what the numbers say*.

1. **Capture the `boot:` phase table on the live machine** (no code
   change; already shipped by kumaui `df205c3`). One reboot + login,
   then `journalctl --user -u kuma-shell -b -o short-precise | grep boot:`
   prices: entering-gpui → app-closure → session-connected →
   settings-loaded → surfaces-up. Everything below is ranked on
   priors until this exists. Cost: zero. Risk: zero.
   **Measured 2026-10-04 (warm login, motherbox):** entering gpui
   0 ms, app closure 15 ms, session connected 15 ms, settings loaded
   15 ms, surfaces up 16 ms. The shell-side path is ~16 ms warm, so
   ranks 3–4 lose their cold-cache priors for any login after the
   day's first; only a cold-boot capture can price them.
2. **Add a first-frame marker** (kumaui, tiny): after the bar surface's
   first draw, request a wayland round-trip (`wl_display.sync`) and log
   `boot: first frame, {}ms`. Also log niri's receipt of the layer
   surface (visible in `journalctl -u niri.service` at DEBUG). Without
   this, "surfaces up" is the best proxy and over- or under-states by
   a compositor frame or a full closure of async starts. Cost: ~30
   lines. Risk: none.
3. **If gpui/EGL/GPU init dominates** (likely candidate for the bulk
   of ~5 s on a cold boot: libshaderc/wgpu pipeline compile, EGL
   device probe, font system cold cache): the shell-side lever is
   deferring *everything not needed for the bar*: move
   `Settings::load`'s theme refresh and the non-surface subsystem
   starts (weather, nostr, tray, notifications) to a `cx.spawn` after
   `surfaces::ensure` (`main.rs:58-83` today runs before the bar
   opens). If the closure is 100s of ms this is free; if gpui init
   itself is ~4 s, only a GPUI-side fix (or pre-warming, below)
   helps. Needs the §5.1 numbers. Risk: low-medium. The panels'
   constructors touch the settings handle; defer construction, not
   just start.
4. **Pre-warm the user's font/GPU caches at boot, not at login**
   (image-side, `blocks.rs`): a oneshot system service after
   `graphical.target` that runs `fc-cache -f` for the system scope
   (or simply runs the shell binary with `--help`-grade init under a
   throwaway headless env) as a *different* user does not warm the
   user cache; a user-level oneshot queued by the *first* login
   (Before=kuma-shell.service) would. Evidence that this matters is
   absent until the cold-boot vs warm-relogin comparison in §5.1 is
   run. Cost: a unit + 10 lines. Risk: low; may buy 0.
5. **Re-anchor `kuma-shell.service` to `After=niri.service`**
   (`blocks.rs`, and pre-named in `session-deploy.md`): buys the
   target-chain span, measured at **22 ms** this boot. Do it for
   tidiness, not for seconds.
6. **MALLOC tuning already applied** (user override
   `MALLOC_ARENA_MAX=2` etc. in
   `~/.config/systemd/user/kuma-shell.service.d/override.conf`): no
   further malloc-side win expected at startup.

## 6. Measured vs inferred, plainly

- Measured: all timestamps in §1; the Chrome PIDs' identity (§2); the
  absence of webview deps (§2); the unit files' contents (§4); the
  instrumented binary's build time postdating the 01:33 boot (§0).
- Inferred, needs one instrumented login: the gpui-init vs
  closure-work split (§3, §5.3); cold-vs-warm fontconfig cost (§5.4);
  whether the ~5 s current figure varies cold-boot vs re-login
  (`session-deploy.md`'s cold-user-manager caveat did not apply this
  boot but was not tested cold-shell either; the 01:33 boot was the
  day's first login).
- Known stale: the 11.5 s figure (pre-fix). Current estimate from the
  session owner: ~5 s.

## Sources

- Journal (host motherbox, boot of Oct 04 2026):
  `journalctl -b -o short-precise` (system), `journalctl -b --user -o
  short-precise` (user manager, 01:33:48.925–48.965),
  `journalctl -b -u niri.service`, `journalctl -b _PID=2097`.
- `~/Documents/kumaui/kuma-shell/src/main.rs:26-30,43-121` (startup
  path + `boot:` instrumentation).
- `~/Documents/kumaui/kuma-shell/Cargo.toml`, `settings.rs:811`,
  `sway.rs:390-396` (no webview; browser probe only).
- `~/Documents/kumaui/vendor/zed/crates/gpui_platform/gpui_platform.rs`
  (`application()` → `current_platform` → `gpui_linux`).
- `~/Documents/kumaui/vendor/zed/crates/gpui/src/svg_renderer.rs:324`
  (fontconfig involvement in GPUI rendering).
- `~/Documents/kumaui/docs/session-deploy.md` ("The blank-second
  problem", re-anchoring candidate, measurement recipe, cold
  user-manager hypothesis).
- `~/Documents/kuma/docs/agents/live-session-experiment.md` (style
  template; bootc/`/usr` read-only machine facts).
- Unit files: `/usr/lib/systemd/user/niri.service`,
  `/usr/lib/systemd/user/kuma-shell.service`,
  `~/.config/systemd/user/kuma-shell.service.d/override.conf`
  (`systemctl --user cat`).
- Image side: `src/containerfile/blocks.rs` in this repo
  (`SHELL_SERVICE` ~2035, niri session config ~1970); read locations
  per the session brief; generation content confirmed via the
  installed units above.
- kumaui commit `df205c3` "Shell logs boot-phase timings at startup";
  kuma commit `a98a1c5` (greeter handoff optimization, per session brief).
