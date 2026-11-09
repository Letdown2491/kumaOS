# The nostr NIPs, reviewed for kumaOS

A survey of the current NIP index (nostr-protocol/nips master, Nov 2026) read
against the layer we ship: the bunker (`kuma-nostrd`), the policy engine, the
nip46-relay, and the shell's signer surface. The question each entry answers:
*is there a use for nostr here we are not taking advantage of?*

Already settled, not re-litigated here:

- **NIP-46 bunker** — the layer's core; full method surface including
  `switch_relays` and `logout` (#12, #14).
- **NIP-49 ncryptsec vault** — shipped, with export/import (#21).
- **Blossom signing surface** (NIP-B7 kind 24242) — the bunker signs
  authorizations and delete auths (#27); Koguma's blossom pane is the client.
  Running a blossom *server* was declined (#28's decision comment).
- **NIP-04** — served for old clients only; [unrecommended upstream —
  "deprecated in favor of NIP-17"](https://github.com/nostr-protocol/nips#list) —
  and our policy already treats it as the sensitive act it is. (Note the
  asymmetry: NIP-04 is *final-unrecommended* while NIP-17 itself is still
  `draft`. The old road is formally deprecated in favor of a spec that has not
  landed. That is the ecosystem's way of saying "NIP-17 is where everyone
  already is"; it does not change what we ship.)

## The idea that started this: the declaration as kind 30078

NIP-78 is "bring your own database" ([78.md](https://github.com/nostr-protocol/nips/blob/master/78.md),
still `draft` `optional`): kind `30078`, an addressable event whose `d` tag
names the data, latest-wins by `created_at`. The spec pins nothing about
content — "`content` and other `tags` can be anything or in any format" — so
the TOML-and-NIP-44-to-self scheme below is our choice, not the spec's. It
also defines kind `78` (a *normal* event) for append-shaped data we don't
need today. Upstream guidance (added Sept 2026, [commit
6aeea60](https://github.com/nostr-protocol/nips/commit/6aeea60)): relays
SHOULD require NIP-42 AUTH and serve these events only to their author. Two
facts discipline the design:

- **The AUTH guidance is not enforced in practice.** [strfry's default is
  `restrictedReadKinds = "4, 1059"`](https://github.com/hoytech/strfry/blob/master/strfry.conf) —
  kind 30078 is world-readable on the most common relay software unless an
  operator opts in. The `d` tag is never encrypted anywhere. The operating
  assumption here: **everything but the NIP-44 ciphertext is public** —
  the declaration must be self-encrypting, not relay-protected.
- **The d-tag namespace is convention, and it's crowded but nameable.** Real
  users prefix with the app name; [reverse-DNS is the recommended
  form](https://nostrbook.dev/kinds/30078). Deployed examples: Amethyst's
  synced settings ([PR
  #3755](https://github.com/vitorpamplona/amethyst/pull/3755)), the
  [OrangeCheck protocol family](https://docs.ochk.io/ecosystem/nostr-kind-30078)
  (which splits per-protocol kinds out of the 30078 range), Wisp's wallet
  backup. No collision found for `io.kumaos/declaration`; `kuma-declaration`
  works too but throws away the namespace the convention exists for.

The shape fits the declaration exactly:

- `d = "io.kumaos/declaration"` (or `io.kumaos/declaration/<hostname>` for a fleet)
- content = the declaration's TOML, NIP-44-encrypted to self
- publish on `kuma sync` when the declaration changes; fetch + offer on a
  fresh install or after disk loss

What this buys: a declaration that survives the disk, and a fleet story —
every kuma machine of yours can read the same encrypted 30078 and offer to
converge toward it, with no server of ours anywhere in the path.

The design questions it drags in (this is the wider discussion, shared with
the self-DM idea below):

1. **Which identity signs?** The bunker's npub is pseudonymous and
   machine-local by default. Encrypting to self under that key works and
   recovers through the existing `export`, but the backup is then only as
   durable as the key export. A separate "owner" npub — the same gap the
   machine-to-self notification idea raises — makes the backup and the fleet
   channel identity-bearing in a way nothing in the layer is today.
2. **The read side is the trust boundary.** Signing the publish is a normal
   policy act (an unknown kind prompts at Ask today). But *applying* a
   declaration fetched from a relay is convergence-shaped config arriving
   from outside the machine — it must surface as a diff to read and approve,
   never silently converge. The consent point is the apply, not the fetch.
3. **Policy category.** A declaration publish should be its own named act in
   the policy engine ("Publish declaration backup"), not an anonymous
   `sign_event` the user can't tell apart from a social post.

## The survey

### Storage and roaming — the 30078 family

| NIP | What it is | The kumaOS read |
| --- | --- | --- |
| [NIP-78](https://github.com/nostr-protocol/nips/blob/master/78.md) (`30078`) `draft` | App-specific data; content format and encryption are the app's business; relay-side author-only serving is SHOULD-level AUTH guidance that strfry does not enforce by default | The declaration backup above. The same d-tag pattern carries the whole roaming story: paired apps' policy levels, relay declarations, the inactivity-lock window — restore a machine or stand up a second one and the layer's shape follows you. De-facto convention is plaintext JSON among social clients; our NIP-44-to-self TOML is stricter than the ecosystem norm, which is right for a machine declaration |
| [NIP-51](https://github.com/nostr-protocol/nips/blob/master/51.md) (lists) `draft` | Addressable lists of every shape; private items NIP-44-encrypted in content | The same data wearing list clothes; only worth it if some nostr client should read our state. Nothing today |

### Roads and discovery

| NIP | What it is | The kumaOS read |
| --- | --- | --- |
| [NIP-65](https://github.com/nostr-protocol/nips/blob/master/65.md) (`10002`) `draft` | The author's relay list (read/write markers; kept small, spread to indexer relays) | The bunker's roads are declared statically today; publishing them under the bunker's npub is how a second kuma machine *discovers* the first, and how a phone finds a freshly re-keyed bunker. The spec says "user" but nothing in kind 10002 requires a person — no precedent found for a service/bunker publishing one, and nothing forbids it. Marmot reuses the kind for non-person accounts, which is the closest thing to a sanction |
| [`10050`](https://github.com/nostr-protocol/nips/blob/master/17.md) (NIP-17 + 51) | Relay list to receive DMs | The inbox half of machine-to-self DMs — the notifier's road, published once. Marmot reuses this kind unchanged for its own inbox discovery |
| [NIP-42](https://github.com/nostr-protocol/nips/blob/master/42.md) / [NIP-43](https://github.com/nostr-protocol/nips/blob/master/43.md) | Client AUTH to relays; relay access metadata | Needed the day the declaration store sits on a private relay — the policy already treats kind 22242 as sensitive, which is the right shape |
| [NIP-66](https://github.com/nostr-protocol/nips/blob/master/66.md) | Relay discovery/liveness | Probably not ours; the relay list is declared, not discovered |

### The machine speaking to its owner

| NIP | What it is | The kumaOS read |
| --- | --- | --- |
| [NIP-17](https://github.com/nostr-protocol/nips/blob/master/17.md) + [NIP-59](https://github.com/nostr-protocol/nips/blob/master/59.md) | Gift-wrapped E2EE DMs (kind 14 rumor → kind 13 seal → kind 1059 gift wrap; kind 21059 for ephemeral) | The modern road for "notify my npub": backup failures, doctor reds, snapshot misses, upgrade results. No push infrastructure, no server, encrypted end to end. NIP-17 is still `draft` (NIP-59 is `optional`); it is nonetheless what every client shipped, and NIP-04's deprecation points here. Requires the owner-identity discussion. See the Marmot note below the table |
| [NIP-40](https://github.com/nostr-protocol/nips/blob/master/40.md) (expiration) `draft` | `expiration` tag; relays SHOULD NOT serve expired events and SHOULD drop expired publishes, but MAY persist them indefinitely — deletion is advisory | Any ask-mirroring or status event must expire; a machine's cry for help should not live on a relay forever. But the spec's own warning applies: expiry is availability hygiene, not a security feature — the event is public until every relay stops serving it. Check the relay's NIP-11 `supported_nips` before trusting it |
| [NIP-38](https://github.com/nostr-protocol/nips/blob/master/38.md) (`30315`) `draft` | User statuses: addressable, `d` names the status type, optionally expiring; content is *plaintext* — the spec defines no encryption | A machine could publish a status ("locked", "upgrade pending", "battery red") — the quiet, opt-in cousin of the notifier. Any real privacy would be ours to add (NIP-44 to self), the spec gives none |
| [NIP-62](https://github.com/nostr-protocol/nips/blob/master/62.md) (`62`) `draft` | Request to Vanish: kind 62 with a `relay` tag (specific relay, or `ALL_RELAYS`); relays supporting it MUST fully delete everything from the pubkey — including addressable events — and MUST block re-broadcast; gift wraps p-tagging the key SHOULD go too | `kuma-nostr destroy` could publish one, asking every relay to forget the bunker's events. The spec covers everything we'd publish, addressable events included. Beautiful closure symmetry with the point of no return — but adoption is thin: nostream [added it in April 2026](https://nostrcompass.org/en/newsletters/2026-04-22-newsletter); strfry lists no 62 support and has only an open issue thread. The doc must say deletion is a courtesy the relay opts into, not an effect |

**The Marmot question.** The newer E2EE stack is the [Marmot
protocol](https://github.com/marmot-protocol/marmot) — MLS over nostr relays;
NIP-EE, the MLS NIP it was trialed in, is now [final-*unrecommended*,
"superseded by Marmot"](https://github.com/nostr-protocol/nips/blob/master/EE.md),
and Marmot's own spec and nostr transport binding are marked adopted (2026),
with White Noise the shipping client. It reuses NIP-65/10050 kinds for
account discovery and adds kinds 443/444/445. For machines, Marmot buys
forward secrecy and group state at the price of MLS key-package lifecycle —
state on the machine that must survive reboot and be recoverable. NIP-17's
per-message gift wrap is stateless and fits a notifier; Marmot fits a
conversation. Nothing today needs a conversation with a machine. Revisit if
the fleet ever needs group-encrypted channels (all machines of yours, one
room).

### Verification and the project's own voice

| NIP | What it is | The kumaOS read |
| --- | --- | --- |
| [NIP-85](https://github.com/nostr-protocol/nips/blob/master/85.md) (trusted assertions) `draft` | Signed assertions by a *service provider* about pubkeys (30382), events (30383), addressable events (30384), NIP-73 ids (30385); consumers declare providers via kind 10040. Merged Jan 2026 ([PR #1534](https://github.com/nostr-protocol/nips/pull/1534)), still draft | The temptation row, now deflated: NIP-85's tag vocabulary is Web-of-Trust metrics (rank, follower counts, zap totals), its consumer model is "user picks a ranking provider," and its deployments ([nip85.nostr.band](https://wotathon.nosfabrica.com/why), nostr.wine, [Amethyst](https://nostrcompass.org/en/topics/nip-85)) are all WoT-shaped. A release-digest assertion is a vocabulary the spec leaves to "providers and clients [agreeing] upon" — technically possible via 30384 off the release event's address, but we'd be the only ones speaking our dialect. Plain signed notes or the NIP-82 asset chain (below) say the same thing without borrowing a spec whose shape doesn't fit |
| [NIP-94](https://github.com/nostr-protocol/nips/blob/master/94.md) + `imeta` (`1063`) | File metadata, sha256 in the `x` tag | Release artifacts described as events; pairs with the release sets below |
| [`30063`](https://github.com/nostr-protocol/registry-of-kinds) | Software release | Not what the draft said. NIP-51's table still carries "Release artifact sets" (`e` tags of kind 1063), but the machine-readable registry now defines 30063 as **"Software Release"** per the unmerged [NIP-82 PR #2526 (Zapstore spec)](https://github.com/nostr-protocol/nips/pull/2526): `d = <appid>@<version>`, channel and platform tags, `e` → kind 3063 asset events carrying the sha256. Zapstore ships this wire today, and [Notedeck/Damus' updater consumes the same shape](https://nostrcompass.org/en/newsletters/2026-03-18-newsletter) ("a trusted release pubkey and a relay connection are enough"). Read before inventing ours — this is the deployed standard for "signed release metadata over relays," and a kuma image digest fits its 3063 `x` tag exactly |
| [NIP-34](https://github.com/nostr-protocol/nips/blob/master/34.md) `draft` | git stuff (issues, patches, repos) | A decentralized GitHub. Not for kumaOS itself — the issue tracker is gh — but the relay-and-signature fabric is exactly what release attestations ride |
| [NIP-5A](https://github.com/nostr-protocol/nips/blob/master/5A.md) (nsites) `draft` | Static websites over Blossom: root 15128 / named 35128 / snapshot 5128 (legacy 34128 deprecated); sha256 per path plus an aggregate `x` tag | The release page or docs as an nsite — censorship-proof, no hosting, content-addressed. Charming; only worth it if the GitHub channel being down is a scenario we design for |

### Identity and handlers

| NIP | What it is | The kumaOS read |
| --- | --- | --- |
| [NIP-89](https://github.com/nostr-protocol/nips/blob/master/89.md) (`31989`/`31990`) `draft` | Recommended application handlers: 31990 advertises what kinds an app handles (`k` tag), 31989 recommends it | Better than the draft knew: [NIP-46's own appendix](https://github.com/nostr-protocol/nips/blob/master/46.md) tells remote signers to publish a 31990 with `k: 24133`, plus `nip46.relays` in `.well-known/nostr.json` and a `nostrconnect_url` template. Advertising `kuma-nostrd` this way is not a stretch of the spec, it is the spec's own deployment story — off-distro installs discover the bunker instead of learning it exists from a README (the "distro's advertisement" thread from #28, without running anyone's storage) |
| [NIP-55](https://github.com/nostr-protocol/nips/blob/master/55.md) `draft` | Android signer application (Amber et al.): intents, content resolver, web callback | The bunker's phone-side counterpart. Interop reading, not a work item — but it names the pattern: one key, signer local, clients remote |
| [NIP-05](https://github.com/nostr-protocol/nips/blob/master/05.md) | DNS identity mapping | Collides with the pseudonymity default; the bunker has no name and should stay that way unless the owner gives it one |

### Rightly out of scope

NIP-47 / 57 / 60 / 61 (wallet and zaps — custody of money is a different
trust domain than custody of a signing key, and the policy engine's grammar
does not stretch that far), NIP-29 groups, NIP-77 negentropy (the nip46-relay
holds events for ten minutes; there is nothing to sync), NIP-52/53/64/68
(calendar, streams, chess, pictures — a desktop OS is not a feed). The
index's own recent casualties agree with us by accident: NIP-90 data vending
machines ([unrecommended — "totally out of
control"](https://github.com/nostr-protocol/nips#list), which the registry's
license-plate ecosystem of use-case-specific kinds quietly confirms),
NIP-96 HTTP storage (unrecommended, replaced by Blossom), NIP-EE (unrecommended,
replaced by Marmot).

## The one discussion that unlocks the most

Half the interesting rows above — the declaration backup, machine-to-self
DMs, machine status, the vanish request — share a prerequisite: **an identity
that is the owner's, not the machine's.** The bunker's npub is deliberately
pseudonymous and disposable. The owner npub is the opposite: durable,
identity-bearing, worth stealing, recoverable through the export path only.
Whether that identity lives in the same vault, a second vault, or the user's
existing phone signer (pair *it* as the approver — NIP-46 works in both
directions) is the actual design decision. Everything else on this page is
mechanical once that is settled.

## Provenance

Research pass of 2026-11-08, against nostr-protocol/nips master (every NIP
fetched as raw markdown from the repo) and the
[registry-of-kinds](https://github.com/nostr-protocol/registry-of-kinds)
schema.yaml. Changed vs the draft:

- **NIP-78**: the AUTH paragraph is real and recent (added Sept 2026, commit
  `6aeea60`), but it is SHOULD-level and strfry — the dominant relay software
  — does not restrict 30078 reads by default (`restrictedReadKinds = "4, 1059"`).
  The draft's "author-only by AUTH convention" is now stated as what it is:
  encryption is the only protection that travels. d-tag updated to the
  reverse-DNS convention (`io.kumaos/declaration`).
- **NIP-85**: the release-attestation framing is retracted as an oversell.
  The spec is WoT-metric-shaped, still `draft`, and its deployments are all
  ranking providers. The 30063 row was wrong twice over: the registry
  attributes 30063 to the unmerged NIP-82 PR (Zapstore spec), not NIP-51's
  aging table entry, and the deployed wire (Zapstore, Notedeck's updater,
  ngit-cli) uses kind 3063 asset events with sha256 `x` tags — which is the
  standard that actually fits image-digest attestation.
- **NIP-17 is still `draft`** (the draft implied "accepted"); NIP-40's
  deletion is explicitly advisory ("MAY persist indefinitely"); NIP-38's
  30315 is plaintext (the draft's "encrypted status" was not in the spec);
  NIP-62 is `draft` with thin relay adoption (nostream yes, strfry no).
- **NIP-46**: the NIP's appendix sanctions signer discovery via NIP-89
  (`k: 24133`), which upgrades the handlers row from idea to spec story. No
  local-socket / in-person transport exists in 46.md; relays are the only
  transport. NIP-55 confirmed `draft`, three transports.
- **Marmot** paragraph added: NIP-EE final-unrecommended, superseded by
  Marmot (spec + nostr transport adopted, 2026; White Noise shipping; reuses
  10002/10050; kinds 443/444/445).

Open questions the discussion still owns (unchanged in essence from the
draft, sharpened):

1. Owner identity vs machine identity — unchanged, still the unlock.
2. Whether the declaration backup targets public relays (NIP-44 ciphertext,
   d-tag public) or a private/tailscale relay (NIP-42/NIP-43), and what the
   policy engine says about each.
3. Whether release attestation rides the Zapstore/NIP-82 chain (deployed but
   unmerged PR; kinds may move) or plain signed notes (no dependency on a
   PR that can change under us).
4. Whether the bunker publishes a NIP-65 list under its own npub — no
   precedent found either way; the kind does not require a person, and
   Marmot's account-level use is the closest sanction.

Wider-sweep searches run (2026-11-08), with outcomes:

- "NIP-78 kind 30078 d-tag settings sync Amethyst Damus backup convention" —
  found: Amethyst synced settings, Wisp wallet backup, OrangeCheck protocol
  family, tamagostrich; convention is app-name-prefixed d tags, plaintext
  JSON common, NIP-44-to-self when private. No `kuma*` collision.
- "strfry NIP-42 AUTH enforcement kind 30078" — found: strfry conf defaults
  restricted reads to kinds 4 and 1059 only; 30078 unprotected by default;
  third-party auth proxies exist for strfry's gaps.
- "nostr operating system update channel attestation software distribution" —
  found: Notedeck/Damus nostr-native updater (Mar 2026, trusted release
  pubkey + relay; `notedeck-release` CLI), ngit-cli 3.0 self-updates and OCI
  container publishing over nostr (Aug 2026), fleetmesh's npack
  nostr-native package manager with two-operator attestation, registry kinds
  31871/31872 (Attestation / Attestation Request, community, no NIP).
  Precedents exist and are recent; absence was *not* the finding.
- "NIP-65 relay list service account bunker machine identity" — found
  nothing for service identities; closest is Marmot's account-level reuse.
- "Marmot protocol nostr MLS status 2026" — found: adopted spec, adopted
  nostr transport, White Noise active; NIP-EE unrecommended Dec 2025.
- "NIP-62 relay support adoption" — found: nostream PR #544 (Apr 2026);
  strfry/nostr-rs-relay only issue threads; relay FAQs still say "treat
  anything you publish as permanent."
