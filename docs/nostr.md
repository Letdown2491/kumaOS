# The nostr layer

kumaOS can run a **bunker**: a small daemon that holds a nostr signing key
and signs events for the apps you pair with it. Your phone's nostr app
connects to the bunker and asks it to sign. Local programs can ask too,
through a private socket. The point is that your key never has to live on
your phone or inside every app that wants to use it.

You turn the layer on with the `[nostr]` section of your declaration:

```toml
[nostr]
enable = true
```

Turning it off later is safe. The key lives in your login keyring and the
pairings in the daemon's own state, both outside the system image, so an
update or a re-enable finds everything where it was left.

## Setup and pairing

Setup happens once, on the machine:

```console
$ kuma-nostr setup        # asks whether to make a new key or import one
```

Then pair an app. From the desktop panel (`Mod+Ctrl+N`, the Pair tab) or
from a terminal:

```console
$ kuma-nostr bunker --qr  # mints a pairing URI, as text and QR
$ kuma-nostr connect <uri>  # or: pair a client's own nostrconnect:// invite
$ kuma-nostr prompts      # what is waiting on you
$ kuma-nostr approve <id>
$ kuma-nostr log          # what was asked, and how it went
```

A pairing URI works for exactly one app, once. The first connect burns its
secret, so mint a new URI for each app. If an app gives you a
`nostrconnect://` invite instead, paste it into `connect` — pasting is the
approval, and the handshake happens on the client's own relays. If that
fails, the error says why: a missing secret, no relay, or a plaintext
relay to a machine that is not yours.

The panel does all of this with buttons, and also feeds the bunker's
keep-alive while you are using it and watches for new asks while it is
open.

## What an app can do

Pairing gives an app the right to *ask*. It never hands over key material.
What the app may sign is decided by the policy engine, and a newly paired
app starts at the most careful level:

- **Ask** — every real request pops a prompt naming the app and the act
  ("Sign a note", "Update relay list"), with the event's content shown for
  you to read. Sensitive requests get an extra cue. Nothing is signed
  until you answer. An unanswered ask times out after five minutes and the
  app gets the refusal.
- **Basic** — the everyday social stuff signs without asking: notes,
  reposts, reactions, long-form. Anything sensitive still asks: profile
  and follows, relay and mute lists, deletions, every decrypt, private
  messages (NIP-04), and anything the safe list does not name.
- **Trust** — signs everything unattended. `kuma doctor` flags any app at
  Trust by name, because a standing grant is the loudest thing in the
  layer.

If you approve an ask, you can let the same answer stand for an hour at
most. That ceiling is built into the verb; nothing in the layer can create
a longer-lasting grant. And if an app retries a request while you are
still reading the first copy, the retries join it: one card, a count, one
answer for everyone waiting.

Every decision lands in an activity log that survives restarts and keeps
the last 500 entries. The record holds the method, the event kind, and the
verdict — never the event's content. Read it from the panel's Activity tab
or `kuma-nostr log`.

## The key, and where it lives

The key is stored in your login keyring — the same place your browser's
certificates and wifi passwords go — wrapped with a passphrase (NIP-49,
stored as `ncryptsec`). Honest footnote: the passphrase is stored next to
the key, so today the wrapping adds nothing the keyring doesn't already
provide. It exists so that switching to a vault with an independent
passphrase later is a change of what fills the same file, not a migration.

Your session unlocks the keyring when you log in, so the daemon comes up
answering on its own. `kuma-nostr lock` drops the key from memory until
`unlock` re-reads it. What the lock protects against is the narrow case: a
guest at the keyboard, a script you didn't watch. It does not protect
against someone already running as your user inside your unlocked
session, and nothing running there could.

You can also arm an inactivity lock: give the daemon a window (an hour at
minimum) with `--inactivity-lock-secs`, and it locks itself when nothing
has used or fed it for that long. This is off by default. Starting the
daemon again starts a fresh window.

One more thing worth knowing: the signing key is not your identity. The
layer generates a dedicated signer key, so an app learns your real npub
only if you import one and answer a `get_public_key` with it. To a relay
operator, the bunker is pseudonymous.

## Removing an app

Three verbs, two outcomes:

- `revoke` tombstones the pairing. The app is refused whatever it sends,
  the record stays so it can't sneak back with a cached secret, and
  `unrevoke` brings it back (with a freshly minted URI — the original
  burned on first use).
- `delete` removes the record outright. A fresh URI can pair the same app
  again.
- An app's own `logout` is a delete the app asked for.

A known app that reconnects is recognized by its identity, so a client
restarting doesn't need a new URI. `rotate` invalidates every outstanding
pairing URI at once, for the moment you think one leaked.

## What a relay sees

Relays only ever carry signing requests and answers (kinds 24133 and
24135), and every payload is end-to-end encrypted (NIP-44). A relay
operator can see which app asked which bunker, how often, and how big.
They never see your feed, your profile, or what was signed.

The layer runs its own relay on your machine, loopback-only and in
memory, so the bunker works on first boot with nothing configured. Relays
you declare are fallbacks: `wss://` for the outside world, `ws://` for
loopback only, and a declaration asking for plaintext to the public
internet is refused at build time. The project runs a public relay at
`wss://relay.nip46.com`; using it is opt-in, and its operator lives under
the same metadata rule as any other.

## What the sandbox bounds

The daemon runs as a regular user service under your graphical session,
inside the full systemd sandbox: no new privileges, read-only system and
home, one writable directory for its own state, a reduced syscall filter,
and no capabilities. It answers on a socket at
`$XDG_RUNTIME_DIR/kuma-nostr.sock`, mode 0600, and any peer whose uid
isn't yours is dropped before its first byte. Reaching the socket buys a
program the right to ask. The policy engine still decides.

## Losing the key

If you delete the vault (`kuma-nostr destroy`), you lose the nostr
identity and nothing else on the machine. The bunker's key is unrelated
to the image signing key, to disk encryption, and to your account
password. Because destroy is unrecoverable, it is a dry run until you
confirm it.
