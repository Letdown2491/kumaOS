# The contract

kumaOS 44.0 begins a set of promises. This page says what they are, what they
cover, and what sits deliberately outside them. Every promise names the
test or check that holds it: a promise whose enforcement is gone is a bug
in kuma, not a change of policy.

The major version names the Fedora base a release builds on: 44.x tracks
Fedora 44, 45.x will track Fedora 45, and the minor and patch carry kuma's
own releases between bases. The major is not a promise boundary -- every
0.x release was an alpha or a beta, and these promises carry through every
release that follows. A promise ends only in a release that announced the
ending beforehand, through the deprecations described below.

## The two promises

**Your declaration keeps working.** Every declaration any release of kuma
has ever accepted parses on every later release. The declaration format
only ever grows: new keys are optional, old keys keep working, and a key
never changes what it means. A declaration that converged a machine on an
early 0.x converges one today.

Portability runs one way. A declaration written for a newer release may
name keys an older release refuses, and kuma refuses loudly rather than
ignoring what it does not know, which is the same refusal that catches a
typo. Upgrade kuma, and the declaration parses.

Held by: the declaration corpus in tests/declarations, which pins real
declarations from v0.4.0 on and parses every one of them.

**A machine that boots kuma keeps booting kuma.** An existing machine
survives moving forward. CI proves the path every machine takes: the
release before the one being published, upgraded inside the guest.
Machines older than that step through intermediate tags the same way they
always have. kumaOS makes no claim about jumping several releases at once.

Held by: the cross-version job, which installs the previous published
release and upgrades it in the guest on every publish.

## What is stable

**The declaration format.** Backward-compatible forever, as above. The
`schema_version` key has never moved and does not move.

**Verbs and flags.** Every verb and flag 44.0 accepts, every later release
accepts with the same meaning. New verbs, new flags and new options arrive
without ceremony.

**`--json` responses.** Every response document keeps the keys it has and
gains fields only. A caller that reads a 44.0 response reads a later
release's response. The verbs that produce a file or a stream rather than an
answer, listed in docs/agents.md, are outside this promise, and the list of
exceptions is itself the promise.

**Published images.** A version tag on kuma's registry is never deleted,
moved or rewritten once published. The older tags are the fixtures
upgrades are tested against and the anchor machines stand on.

## The rules between releases

**Additions are free.** Any release may add verbs, flags, response
fields, optional declaration keys and doctor checks.

**Deprecation is a note, not a removal.** A verb, flag or field headed for
removal is named in the changelog, saying what to use instead, and may
warn in its output. It keeps working until the release that removes it,
which is never the release that first named the removal.

**Breaking changes are announced, then made.** Removing or renaming a
verb, flag or response field, or changing what a declaration key means,
ends a promise. A machine offered an update that made such a change
refuses what it cannot parse rather than guessing: an unparseable
declaration stops the machine, which makes the failure a decision instead
of a surprise.

**Not an interface.** None of these carry promises:

- `kuma.lock`. Generated state, not an authored file; any release may
  discard and rewrite it. The digest pin it carries for `system.base` is
  trust machinery, described in SECURITY.md, not a format promise.
- Exit codes. Scripts read `--json`.
- Prose output. Anything not under `--json` can change wording freely.
- The ISO's on-disk layout and volume label. Media is self-contained and
  stands alone per release.
- `kuma generate`'s output. The Containerfile shape is an implementation,
  and pinning it would make every improvement a compatibility event.
- `KUMA_TRACE` names. Diagnostics: stable enough to diff between runs,
  free to gain detail.

## What 44.0 does not do

README's Not-yet list states absences. These are decisions, which is why
they carry reasons.

**The proprietary NVIDIA driver.** 44.0 does not ship it. NVIDIA machines
boot on nouveau, and a declaration naming `akmod-nvidia` fails the build
with the reason. Shipping it later has a trust cost either way: a prebuilt
third-party module image would make an unsigned pipeline part of the
supply chain of a project whose verification story is signatures, and
building modules in kuma's own builds would couple every kernel update to
an akmods run that can fail the build. The promise is a machine that
boots and converges, and nouveau delivers that.

**The three-partition layout.** Installs write exactly three partitions:
the ESP, /boot, and root taking the remainder, the first two sized at
install time. Dual-boot, extra partitions, RAID and LVM, and installing
beside another system are other tools' jobs. This is the shape the
swapfile hibernate machinery assumes, which is why it is the design rather
than a missing feature. It names what 44.0 does, not what may never exist.

**Reproducible builds, and an SBOM.** kumaOS makes no claim that two builds
of one declaration produce identical bytes, and emits no SBOM. The
resolved versions in `kuma.lock` are adjacent, not the same thing. Both
are stated here as the stance; SECURITY.md's Not-yet section explains why.

**Signing user-built images.** kumaOS's published images are signed, and
machines carrying its policy refuse unsigned ones. An image built from
your own declaration is yours to sign; kuma does not do it for you.

## How the promises are held

- The declaration corpus: declarations from v0.4.0 on, all parsing.
- The response shape tests: one per verb that speaks `--json`, holding the
  exact key set, so a rename or a removal fails CI rather than a caller.
- The cross-version job: the previous published release, upgraded in the
  guest, coming back.
- Doctor's signature check: grades that a machine actually refuses an
  unsigned kuma image, reading its policy the way the machine will.
- The changelog: every reader-visible change lands with the release that
  makes it, saying what to do differently.

## Trust

What a kuma machine trusts, talks to and verifies is in SECURITY.md: the
trust roots, the signing key and its custody, and what happens when that
key is lost or rotated. That page and this one are two halves of one
statement; neither overrides the other.
