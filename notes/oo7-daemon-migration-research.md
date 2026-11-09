# The oo7-daemon decision, post-GA — adopt now or wait

Status: research written 2026-11-08, two days after 45.1.0 and five after
45.0.0 (the F45 rebase, #43) shipped on the stay lane. Successor to
notes/45.0.0-oo7-research.md (the beta-era pass, 2026-10-29), which ruled
"stay on gnome-keyring for 45.0.0, adopt in a later release" and named the
preconditions the adopt lane waits on. This file re-checks those
preconditions against post-GA ground truth and sizes the actual migration
(#43's remaining named residue). Labels: CONFIRMED = verified from a
primary source on 2026-11-08 unless noted; REPORTED = secondary source;
UNVERIFIED/UNKNOWN = no source found.

## Verdict

**Wait — not because oo7 lost the argument, but because the clock is
working for us and three of the four adopt-lane preconditions resolve
themselves within weeks.** The situation since the beta-era pass:

- **The single hardest precondition resolved three days ago.** oo7 0.7.0
  — the first non-beta release, carrying "Support for unencrypted
  keyrings" (the rhbz 2538274 class) and the refined oo7-pam login
  helper — shipped 2026-11-05
  (https://github.com/linux-credentials/oo7/releases/tag/0.7.0).
  CONFIRMED. The beta-era note called a non-beta release with the
  unencrypted-migration fix "the load-bearing one" for autologin
  machines. It exists now.
- **But Fedora does not ship it yet.** As of 2026-11-08 the newest
  fc45 koji build is still `oo7-0.7.0~beta^1.e91b98a-1.fc45` — the
  2026-10-29 snapshot
  (https://koji.fedoraproject.org/koji/packageinfo?packageID=43902).
  CONFIRMED. Adopting now means adopting the beta snapshot that
  predates the unencrypted-keyring fix, with
  [rhbz 2538274](https://bugzilla.redhat.com/show_bug.cgi?id=2538274)
  still NEW (REST API, last change 2026-10-27). CONFIRMED.
- **And two post-0.7.0 issues sit exactly on our migration path.** #650
  "Empty systemd credential fails to unlock an unencrypted keyring"
  (open) and #651 "Writes fail with 'The target file has changed' after
  a v0 keyring is migrated via the PAM listener" (open) — both filed
  against the freshly released 0.7.0, both in the
  autologin/keyring-migration corner this image's fleet lives in
  (https://github.com/linux-credentials/oo7/issues/650,
  https://github.com/linux-credentials/oo7/issues/651). CONFIRMED.
- **The upstream trajectory is no longer in doubt** — GNOME 51 ships
  oo7 as the key storage, Arch extra packages oo7 0.7.0 as the
  org.freedesktop.secrets provider (conflicting with gnome-keyring),
  postmarketOS switched its GNOME/COSMIC/Phosh UIs, KDE Linux has an
  open work item, and two Fedora Atomic remixes (AstrOS, Margine OS)
  already ship it. §4. Adopting is the destination; nothing about that
  changed.

The verdict in one line: the vault rewrite (#51) paid its premium
precisely so this swap could be made calmly; the calm moment is the
first F45 stable update carrying oo7 0.7.0, not today's beta snapshot.
45.2 or 45.3, on the signals in "The signals that flip the answer" (§7).

## 1. What this image actually uses gnome-keyring for (the inventory)

This is the section the verdict depends on. The claim to test: "adopting
oo7 drops gnome-keyring from the image" — so what does the image lose?

**gnome-keyring is three services wearing one package** — the Secret
Service daemon, an SSH agent (built into gnome-keyring-daemon), and a
PKCS#11 module for certificate/key storage. Measured against kumaOS:

- **Secret Service (`org.freedesktop.secrets`) — used. This is the whole
  seam.**
  - Package lists: `gnome-keyring` + `gnome-keyring-pam` in the niri set
    (src/containerfile/blocks.rs:32,37) and the COSMIC set
    (blocks.rs:222,223). Four lines; the comments already explain the
    `-`-prefix silent-skip failure mode. CONFIRMED (source).
  - The nostr vault: src/nostr/secrets.rs speaks spec-level
    `org.freedesktop.secrets` over zbus 6 — three operations
    (load/save/remove), the login collection via the `default` alias,
    and it already handles both daemon dialects (gnome-keyring's
    `(aoao)` SearchItems shape vs the strict variant; bare-arg Delete).
    Zero changes in either lane — that was the point of #51. CONFIRMED
    (source).
  - PAM unlock: `keyring_pam()` (blocks.rs:462-466, post-`8906e62`
    two-directory form) asserts pam_gnome_keyring.so exists and that
    greetd's and cosmic-greeter's stacks call it. Goldens pin these
    bytes (src/containerfile/mod.rs:3689, 4095-4136, 4376). CONFIRMED
    (source).
  - Everything else that speaks libsecret — flatpak'd browsers storing
    passwords, seahorse, any GTK app — is a *user-visible* consumer, not
    a kuma one; it speaks the spec, so it follows whatever daemon claims
    the name. The niri set ships xdg-desktop-portal-gnome/gtk, which
    depend on nothing keyring-shaped. CONFIRMED (package lists).
- **SSH agent — structurally unused in this image, probably already
  inert.** grep over the whole tree finds no `SSH_AUTH_SOCK`, no
  gcr-ssh-agent, no ssh-agent wiring anywhere in src/ or docs/. The
  niri session's `spawn-at-startup` lines (blocks.rs:2329-2341) name
  kuma-clipboard-bridge, kuma-xsettings, blueman-applet, udiskie,
  kuma-battery-watch — not gnome-keyring-daemon; niri runs no XDG
  autostart. The daemon comes up via pam_gnome_keyring's
  `auto_start`, but nothing in this image exports the agent's socket to
  user shells, so the classic "ssh keys held in the login keyring"
  experience is not something this desktop ever provided. A user who
  wanted it didn't get it. CONFIRMED (repo structure) / the inertness is
  inference, not runtime-measured — UNVERIFIED at runtime. Outside
  gnome-keyring the capability exists as gcr's ssh-agent user unit,
  plain ssh-agent, or systemd's ssh support — all unshipped here, so
  dropping gnome-keyring removes nothing users were getting.
- **PKCS#11 — nothing consumes it.** No p11-kit stanza, no
  gnome-keyring-pkcs11 reference, no NSS config anywhere in the tree.
  p11-kit's *trust* module is a different package (p11-kit) and does not
  ride gnome-keyring. CONFIRMED (repo structure); the F45 RPM payload
  for gnome-keyring's pkcs11 subpackage wasn't separately enumerated —
  UNVERIFIED at the payload level, immaterial.

**Other places the keyring surfaces in the repo** (the enablement-gap
checklist for the adopt day):

- docs/nostr.md says "your login keyring" four times (lines 25, 104,
  107, 111) and "Your session unlocks the keyring when you log in" —
  phrasing that stays true under either daemon, since PAM does the
  unlocking either way. CONFIRMED (source).
- The declaration comment corpus: "the key lives in your login keyring"
  appears in every frozen declaration (tests/declarations/v44.4.0 through
  v45.2.0). Same neutrality. CONFIRMED (source).
- vm-smoke has no keyring or secret-service step — the smoke's greeter
  check and boot assertions never touch the vault; the only Secret
  Service test in the tree is the `#[ignore]`d
  `live_secret_service_roundtrip` (src/nostr/secrets.rs:203), run on
  hardware, daemon-agnostic by construction. CONFIRMED (tree read).
- kumaui: no keyring client in the tree (beta-era pass CONFIRMED
  2026-10-29; re-grepped 2026-11-08, still nothing).

Net: **what adopting actually drops is a Secret Service daemon whose
only wired consumer in the image is the nostr vault, plus two
capabilities (SSH agent, PKCS#11) this desktop never surfaced.** The
four package-list lines, the PAM assert, and the goldens are the entire
code-level cost.

## 2. oo7-daemon: what it is and its maturity

- **It is the `oo7` repo's server component** —
  https://github.com/linux-credentials/oo7, MIT, Rust: oo7-daemon (the
  `org.freedesktop.secrets` server, server/ dir: service, collection,
  item, session, prompt, login, migration), oo7-portal (the
  `org.freedesktop.impl.portal.Secret` backend), oo7-cli, the oo7
  client library (the crate kumaOS dropped in #51), pam_oo7
  (pam/), cargo/git credential helpers, KWallet migration tools.
  CONFIRMED (repo tree + README).
- **The spec surface is real and now seasoned by two default-ships.**
  The server implements the Secret Service spec including prompts
  (server/src/prompt/), the `default` alias (served at the alias path
  since 0.5.0, PR #267), collection create/delete/change signals,
  GNOME and Plasma prompters, and the v0 (gnome-keyring) keyring
  migration. It has been the default in GNOME OS nightly since mid-2026
  and shipped as Fedora 45's default at GA. CONFIRMED (repo tree,
  release notes, §4). The client-side caveat from the daemon's own
  README: it is a D-Bus service, and kumaOS's zbus client never goes
  near the oo7 crate — the daemon change cannot touch the vault's code
  path. CONFIRMED (secrets.rs imports).
- **Release cadence and maintenance shape:** tags 0.7.0.alpha
  (2026-07-17), 0.7.0.beta (2026-09-21), **0.7.0 (2026-11-05)** —
  first stable since 0.6.0 (2026-02-21). Repo active the day this was
  written (last push 2026-11-08), 467 stars, 61 forks, 19 open issues.
  The open-issue shape (§ Verdict's #650/#651, plus #603, #614 below)
  is the honest read: race and edge-condition reports in the login/
  migration paths, not structural rot. CONFIRMED (GitHub API).
- **One crate still, many bins:** the daemon is not a separate product
  from the crate kumaOS dropped; 0.7.0's MSRV bump (1.95) and client
  changes ride the same repo. Irrelevant to kumaOS post-#51 — the tree
  carries no oo7 dependency. CONFIRMED.
- **Dialect deltas the vault already handles:** SearchItems shape,
  Delete's bare-arg marshalling, locked collections answering empty
  searches (oo7's #614 — libsecret consumers get no unlock prompt
  where gnome-keyring would prompt; the vault's gate design never
  prompts, so #614 costs kuma nothing beyond the error it already
  names). CONFIRMED (secrets.rs + issue text).

## 3. What gnome-keyring's departure does and doesn't end

- **gnome-keyring is not dead upstream, but its maintenance is
  thinning.** GNOME GitLab shows translation-only commits since the
  51.1-era code work (last substantive merge Sep 17, a session-unref
  fix; since then translations only, latest 2026-10-19)
  (https://gitlab.gnome.org/GNOME/gnome-keyring). The Fedora change
  thread's own words: "gnome-keyring appears to be on its way to
  unmaintained / obsolete status. Unless somebody steps up..." 
  (https://discussion.fedoraproject.org/t/f45-change-proposal-oo7-secrets-service-provider-system-wide/195274,
  comment by bilelmoussaoui). REPORTED (thread text); the commit
  listing is CONFIRMED.
- **Fedora keeps the packages either way.** F45's released Everything
  repo carries gnome-keyring-51.1-1.fc45 and gnome-keyring-pam-51.1-1.fc45
  alongside the oo7 set
  (https://mirrors.kernel.org/fedora/releases/45/Everything/x86_64/os/Packages/g/).
  Staying is always technically available; the stay lane does not
  expire on a date. CONFIRMED.

## 4. Distro adoption and trajectory

- **Fedora 45** — the change ("Replace gnome-keyring with oo7 as the
  default secret service", FESCo #3540,
  https://fedoraproject.org/wiki/Changes/oo7_Secrets_Service_Provider)
  shipped with GA: stock F45 desktop installs get oo7-daemon as the
  `org.freedesktop.secrets` owner and gnome-keyring gone
  (REPORTED — community doc
  https://fedolinux.com/en/oo7-secrets/, consistent with the change
  text and the blocker-thread record). kumaOS's 45.x images are *not*
  stock here: our explicit package lists keep gnome-keyring, which is
  why the stay lane works. The F45-packaged oo7 is still the
  2026-10-29 beta^1 snapshot (§ Verdict). CONFIRMED (koji, repodata).
- **GNOME itself has already crossed.** The GNOME 51 release notes:
  "Secure key storage: GNOME now stores and retrieves passwords and
  keys using oo7, a new component which delivers enhanced security"
  (https://release.gnome.org/51/). CONFIRMED. GDM's upstream PAM
  configs gained oo7 support, distros catching up (Arch forum thread,
  merged patch, 2026-10). REPORTED.
- **Arch**: `oo7 0.7.0-1` in extra since 2026-11-06, maintainer arojas,
  `Provides: org.freedesktop.secrets`, **conflicts with gnome-keyring**
  (https://archlinux.org/packages/search/json/?q=oo7). CONFIRMED. The
  Arch wiki documents a real migration wart: stale
  `~/.local/share/keyrings/default` + empty `default.keyring` from old
  GKR installs confuse some apps after migration
  (https://wiki.archlinux.org/title/Oo7). CONFIRMED (wiki).
- **postmarketOS/Alpine**: pmaports switched GNOME, COSMIC and Phosh
  UIs from gnome-keyring to oo7 (June 2026), with a GNOME-session oo7
  backport to Alpine aports, an oo7-portal switch for Phosh, and the
  commit message pointing at KDE Linux work item 566 for upstream KDE
  (pmaports MR 8798,
  https://gitlab.postmarketos.org/postmarketOS/pmaports). REPORTED
  (commit messages read on a mirror; the MR is real and specific).
  Sxmo specifically: UNVERIFIED — no source found either way.
- **Fedora Atomic remixes** — the closest analogues to kumaOS: AstrOS
  announced shipping oo7 instead of gnome-keyring on 2026-11-06
  (https://astros-linux.org/blog/switching-to-oo7/), and Margine OS
  (issue #603's reporter) ships the daemon with its own D-Bus
  activation file plus a niri.service drop-in — the exact shape this
  image would need. REPORTED/CONFIRMED (blog; issue text).
- **Debian, openSUSE**: no oo7 packaging found in either's search
  (both search surfaces bot-guarded on 2026-11-08) — UNVERIFIED; no
  evidence of a default switch, and Debian's gnome-keyring remains.
  The signal there is silence, not opposition.

## 5. THE migration question: does oo7-daemon read gnome-keyring's existing `~/.local/share/keyrings/`?

Yes — with named edges. This is the answer at the source, not a forum:

- **The daemon migrates v0 (gnome-keyring) keyrings server-side.** The
  0.7.0.alpha release notes: migrated keyrings are **no longer deleted**
  — a `<keyring-name>.stamp` file prevents re-migration and cleanup of
  the old data is deliberately left to the user
  (https://github.com/linux-credentials/oo7/releases/tag/0.7.0.alpha).
  CONFIRMED. So the nostr vault's ncryptsec item in the login
  collection survives the cutover by design: the first oo7 login
  migrates the file in place and the vault's next `load` reads it back
  through the spec API.
- **The same release added `oo7-cli migrate`** — "migrate v0
  (gnome-keyring) or KWallet keyrings to the v1 format... allowing
  users to test migration before switching." That is the migration
  test's rehearsal tool. CONFIRMED.
- **Unencrypted/passwordless keyrings — the autologin shape — failed
  migration on the F45-shipped snapshot** (rhbz 2538274, "File header
  doesn't match FILE_HEADER", NEW since 2026-09-22). The fix is the
  0.7.0 stable note "Support for unencrypted keyrings". CONFIRMED
  (bugzilla REST + release notes). **But** the fix has not been
  packaged in Fedora yet (§ Verdict), and #650 (empty systemd
  credential fails to unlock an unencrypted keyring) is open *against
  0.7.0* — the class of bug is not fully closed upstream, let alone in
  the F45 repo. CONFIRMED.
- **Fedora's upgrade record is the honest preview of a fleet cutover:**
  the change's blocker list was dominated by upgrade-flow breakage —
  dual-daemon state (#2225), missing pam_oo7 lines from a bad rebuild
  (#2208), SSH passphrases (#2245), portal missing post-upgrade
  (#2186) — all documented in the beta-era note with sources; and the
  enablement mechanism itself (first-install-only RPM user-unit
  scriptlets, no preset) provably fails on ostree/bootc deploys
  (Silverblue thread 202421, `systemctl --user enable --now
  oo7-daemon.service` as the user-side fix). CONFIRMED (prior note's
  sourcing stands; nothing there moved).

## 6. The kumaOS migration shape, sized

#43's three named residues, against everything above:

**(a) The vault re-prove.** Already largely banked: the vault client is
daemon-agnostic by construction (#51/#52), proven on F44's
gnome-keyring at 44.13.0 and on F45's gnome-keyring by every 45.0/45.1
machine since (the client can't tell the difference — that's the
construction). What a *deliberate* oo7 lap adds is the other half of
the sentence: the vault proven against oo7-daemon specifically, which
the change's second desktop was supposed to buy for free and the stay
lane deferred. Cheap: one smoke-adjacent lap with the swapped packages,
running `live_secret_service_roundtrip` plus a real vault
unlock/setup/destroy against the migrated collection. A half-day of
machine time, not a code lane.

**(b) The enablement gap.** This is the real work, and it is more than
the package-list swap:

1. Swap `gnome-keyring gnome-keyring-pam` → `oo7-daemon pam_oo7` in
   both desktop lists (blocks.rs:32/37, 222/223). Version-locked pair —
   they must ride lock regens together. CONFIRMED (prior note §3).
2. Rewrite `keyring_pam()` → assert `pam_oo7.so`, grep the greeter
   stacks for `pam_oo7`, regenerate every golden that pins the bytes
   (mod.rs:3689, 4095-4136, 4376). Do not assume the stacks keep the
   pam_oo7 twins — #2208 proved a rebuild can drop them; the assert
   exists precisely because this class of failure is silent.
3. **Ship the enablement ourselves — upstream provides none for a
   session like ours.** Upstream's server/data ships only
   oo7-daemon.service (no D-Bus activation file, no preset — repo tree
   at 0.7.0 and main, CONFIRMED), the Fedora spec's scriptlets don't
   apply on bootc, and the only thing that starts oo7-daemon on stock
   F45 is gnome-session's target — issue #603's reporter measured
   `secret-tool` failing in niri and Sway sessions on F45 with
   "The name org.freedesktop.secrets was not provided by any .service
   files" (https://github.com/linux-credentials/oo7/issues/603).
   CONFIRMED. The proven downstream idiom (Margine OS, same reporter):
   a `/usr/share/dbus-1/services/org.freedesktop.secrets.service` file
   naming `SystemdService=dbus-org.freedesktop.secrets.service`, plus
   the user unit enabled via a preset or `default.target.wants` baked
   into the layer, plus the daemon up early enough for pam_oo7's login
   helper (it waits 120s). A bootc image wants all three pieces and
   owns them forever after — this is the piece that makes the swap
   more than four lines.
4. Watch SELinux: rhbz 2537728 (oo7-daemon failing under staff_u) is
   open; this image is a selinux-targeted appliance and the smoke is
   the detector. UNVERIFIED for our actual user context.

**(c) The migration test.** The honest test, per §5's facts:

- **Fresh install**: oo7-daemon enabled at first login, PAM unlock
  works, vault setup/unlock/destroy green, browser/flatpak secrets
  round-trip through libsecret consumers.
- **Existing-install upgrade — the fleet's actual road**: a machine
  with a provisioned nostr vault in a gnome-keyring v0 login
  collection (both password-protected AND the autologin-produced
  passwordless shape — the latter is rhbz 2538274's input and #650's
  still-open corner), upgraded to the oo7 image, rebooted, PAM
  unlocks the migrated collection, **and `kuma vault` load returns the
  signing key**. If the passwordless case fails, the lane is NO-GO
  for autologin declarations (the beta note's ruling, unchanged — and
  now also upstream's own #650).
- Rollback story: migration is non-destructive post-0.7.0.alpha (the
  `.stamp` scheme leaves old files), so a machine that rides back to a
  gnome-keyring image has its old keyring files on disk — but only if
  the user didn't clean up, and only from a 0.7.0-based daemon. The
  nostr layer's own key-export path (docs/nostr.md's backup guidance)
  is the real safety net and should be named in the release notes for
  the cutover release.

## 7. Verdict

**Not now. The first F45 stable update carrying oo7 0.7.0 stable is the
calm window — 45.2/45.3 territory.**

Pros of adopting (all real, none urgent):

- The destination is upstream consensus: GNOME 51, Arch, Fedora 45's
  default, postmarketOS, KDE Linux in flight, both Atomic-remix
  analogues. gnome-keyring's upstream has gone translation-only. The
  stay lane doesn't expire, but every quarter it ages.
- The client-side premium is already paid — #51's spec-level zbus
  client means the vault needs zero changes. The entire code cost is
  the assert, the goldens, and the enablement plumbing.
- oo7's security posture is genuinely more modern (systemd-creds
  credential plumbing, hardened units), and the unencrypted-keyring
  fix means autologin machines — this image's supported shape — stop
  being a migration edge case.
- Doing it deliberately *now-ish* means doing it before a Fedora 46
  rebase forces the question mid-rebase, when blast radii stack.

Cons of adopting now:

- Fedora ships the beta^1 snapshot, not 0.7.0 — adopting today adopts
  the exact build whose passwordless-keyring bug (rhbz 2538274, NEW)
  maps onto this image's autologin support.
- #650 and #651 are open against 0.7.0 — the migration/PAM path is
  still warm, and the load it fails under (empty credential, v0
  migration via PAM listener) is the fleet's everyday shape.
- The enablement gap means we ship and forever own D-Bus activation +
  preset plumbing upstream provides to no one outside GNOME — surface
  area with a silent failure mode (locked keyring, vault error) that
  the smoke must learn to detect.
- Zero user-visible benefit on this image: the SSH agent and PKCS#11
  halves were never wired, so gnome-keyring here *is* the vault's
  backend and nothing else — and the vault cannot tell which daemon
  serves it.

Cons of waiting: none measured. gnome-keyring 51.1 is packaged,
patched, and PAM-wired in F45; the F45->46 rebase re-opens this file
naturally; and every month of wait buys upstream bug-fix distance.

**The signals that flip the answer** (the 45.2 discussion's checklist):

1. F45 (or the kuma lock) carries oo7 0.7.0 stable — koji build ≥
   0.7.0-1.fc45, or we decide to package-override (heavier road, own
   burden).
2. #650 and #651 closed, or reproduced-fixed and understood.
3. rhbz 2538274 confirmed fixed by a shipped oo7.
4. Upstream merges the D-Bus activation file (#603) — at which point
   the enablement gap shrinks to "ship the preset" — or we accept
   owning Margine OS's idiom permanently.
5. A gnome-keyring retirement signal from Fedora (the beta note's
   trigger) — if Fedora forces the swap, the road above becomes the
   plan, with the migration test as the gate.

The failure mode of each road, stated plainly:

- **Adopt-now failure mode**: an autologin fleet machine upgrades, its
  passwordless keyring fails migration or the empty-credential unlock
  (#650), the nostr daemon comes up vault-less, and the machine's
  identity is stranded until a manual `oo7-cli migrate`/key re-import.
  The vault error is loud but the *loss* is the thing.
- **Wait failure mode**: oo7 is adopted at 46-rebase time instead of
  45.x, and the swap's blast radius stacks on the rebase's — the exact
  coupling #52's ruling ("features and rebase never ride together")
  exists to prevent. Mitigated by sequencing the oo7 lap one release
  *ahead of* the rebase, not with it.
- **Stay-forever failure mode**: gnome-keyring's maintenance goes to
  zero and Fedora retires the packages out from under the stay lane —
  discovered, not chosen. Watch the devel list; the doctor/smoke can't
  see it coming.

## Open questions the 45.2 discussion owns

- Does the adopt lap ride one release before the 46 rebase, or with
  it? (The #52 sequencing rule says one ahead; confirm against the
  release calendar.)
- Own the D-Bus activation + preset plumbing if #603 is still open at
  adopt time — or hold for upstream? (It shrinks a ~day of owned
  surface to a preset file.)
- Is the autologin + passwordless-keyring combination a supported
  declaration shape worth gating on, or does the adopt lane's fine
  print get to say "autologin declarations migrate first to a
  password-protected keyring"? That is a product call, not a research
  one.
- Does the smoke gain a keyring step (vault roundtrip against the
  live daemon) for the adopt lap only, or permanently? The §ignored
  test needs a session bus; the smoke's VM has one.
- SELinux: does oo7-daemon behave under this image's actual user
  context, or does the policy need a ride? (rhbz 2537728 is the only
  data point and it isn't ours.)
