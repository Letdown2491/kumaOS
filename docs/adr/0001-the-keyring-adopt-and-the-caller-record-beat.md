# 0001 — The keyring adopt, the caller-record beat, and the guard that timing cannot fake

Status: accepted (45.7.2, refined 45.7.3). Supersedes the dbus-activation
story that #116's cut retired.

## The architecture

The login keyring's unlock belongs to PAM, and its bus name belongs to
the session. Those are two different daemons living one half-life each:

- The **login half** is spawned by the greeter's PAM stack
  (`pam_gnome_keyring` in greetd's), `--daemonize --login`. It holds the
  unlocked keyring and nothing on the bus.
- The **adopt half** is `kuma-keyring.service`, a user unit started with
  the graphical session (`WantedBy=graphical-session.target`, preset
  enabled so it reaches the account). `gnome-keyring-daemon --start
  --foreground` finds the login half, adopts it, and owns
  `org.freedesktop.secrets` for the session.

The bunker's asks never open the keyring — they go through it, which is
why `kuma-nostr unlock` is useless when the daemon is dead and why no
prompt is implemented in `secrets.rs`. Ordering between the adopt and
its dependents is real and verified: the unit's `ExecStartPost` holds
until the bus name is owned AND its owner answers a `Peer.Ping` — bus
glue, not the secret module, so the probe cannot trip the very abort it
guards. An exit-zero from the adopt alone barred nothing (45.7.1's
second hardware boot proved that: the unit succeeded, the daemon
aborted on the session's first OpenSession a second later, and every
dependent ordered behind the unit was ordered behind a corpse).

## The mechanism the beat guards

gnome-keyring keeps a record of each caller — the caller's PKCS#11
session — keyed on the caller's unique bus name, and builds that record
from an **idle callback**. An `OpenSession` arriving before the idle
callback has run finds no client record, and the 51.x assertion
cascade (`get_pkcs11_session: assertion 'client' failed`, then
`plain_negotiate` building a GVariant from NULL) aborts the daemon —
taking the PAM unlock it holds down with it. This is upstream
gnome-keyring #190, open since the 40 series, alive in the base's
51.1.

Two consequences the code now states at the fix:

- **The race is microseconds on the ask's own connection, not seconds
  on the daemon's age.** One new connection per operation makes the
  window per operation.
- Therefore `secrets::connect()` sleeps 300ms between connecting and
  the first ask — the **caller-record beat**. It gives the daemon's
  event loop the idle turn that creates the record before the first
  ask goes out: several orders of magnitude wider than the window,
  inaudible next to a vault operation.

A daemon-side fix, if the base ever ships one, retires nothing here.
The beat is harmless after one; nothing watches upstream.

## The refuted alternative, as a warning

The first 45.7.2 fix settled the *daemon*: wait for the service name
to be owned, then wait two seconds for the dust to settle, then ask.
Real hardware refuted it — the same +4s ask won one boot and lost the
next. A timing model that cannot name its clock is a coin flip wearing
a guard: the settle guarded a duration that had nothing to do with the
failure, and the boots that lost did so at identical timings to the
boots that won. The gate was removed whole (helpers included); the
beat replaced it because the beat's clock is the one the failure runs
on — the daemon's idle queue, not the wall.

## The unit's dishonest exit, and the watch (#124)

`--start --foreground` makes the unit's process a *client* whenever a
daemon already exists — and a client that exits 0 when its daemon
dies. Twice on real hardware, the losing boots showed the unit
`inactive (dead)` with `Result=success` inside the crash second:
`Restart=on-failure` never fired, and with dbus activation cut (#116)
the name sat unowned AND unactivatable for the rest of the session —
every later client met `ServiceUnknown` against nobody.

The adopt-and-exit shape cannot be made honest by the exit; the unit
now **holds the watch**: the ExecStart shell adopts, then holds while
the bus name answers a `Peer.Ping`, and exits one when it stops.
`Restart=` finally has a failure to act on, and its restart's adopt
finds no daemon and becomes one — a LOCKED name-holder.

## The failure taxonomy

What the bunker's retry ladder reports names the boot fact and the
next move. Three verdicts, three different worlds:

- **`NoReply`** — the ask killed the daemon (the #190 abort took it
  while answering). The unlock dies with it; nothing client-side
  recovers that. The watch (#124) is what puts a fresh, locked
  name-holder on the bus for the next client.
- **`ServiceUnknown`** — the name is unowned and unactivatable: the
  adopt never started, or the pre-watch corpse case. Diagnosis is the
  unit, not the vault.
- **locked** — a name-holder that answers but cannot read: the
  bootstrap race (a locked daemon won the name around PAM's unlock),
  or the watch's restarted adopt after an abort. The client gets
  honest locked-collection errors. The two-step bootstrap ritual in
  `docs/nostr.md` is the human recovery; the doctor names this state
  explicitly when it sees it.

`ServiceUnknown` against nobody is the one verdict that should now be
impossible while the session lives: the watch exists so the name
always has an owner that says something — even if what it says is
locked.
