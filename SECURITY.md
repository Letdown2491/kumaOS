# Security

Kuma is early and maintained by one person.

Three things here are worth reading even if you never report a bug: what
naming a package in your declaration opts you into, how to check that the
binary you downloaded came from this project, and what happens when the
signing key is lost or rotated.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/Letdown2491/kumaos/security/advisories/new).
It reaches the maintainer privately, and it is the only channel. Please don't
open a public issue for one first.

Worth including: the output of `kuma --version`, the declaration that
reproduces it with any password hash removed, and what an attacker ends up
able to do.

Expect a reply in days rather than hours. There is no bounty and no response
guarantee. Only the most recent release is supported; fixes go out in a new
release rather than as backports to older tags.

## What is kuma's to fix

Kuma compiles a declaration into a Containerfile and hands it to podman. It
builds no packages and no kernels. What it adds to an image is its own binary,
the systemd units it writes, and the desktop assets compiled into it.

So a flaw in the kernel, in systemd, in a Fedora package, or in bootc is not
kuma's to patch, and kuma needs no patch mechanism of its own: a rebuild
resolves against Fedora's packages as they are that day, which makes updating
and patching the same operation. `kuma update` is how you take security
updates.

A flaw in how kuma generates a build, in what it puts in an image, or in what
it runs on your machine is kuma's, and is worth reporting.

## Your declaration is the trust boundary

One short file spans several trust roots, and naming a string is how you opt
into each:

- **`packages.rpm`** comes from Fedora's repositories. You cannot declare a
  third-party repository; there is no key for it. Signature checking is dnf's
  default and kuma never disables it, and a name that tries to become a flag
  (`rpm = ["--nogpgcheck"]`) is rejected before it reaches dnf.
- **`system.base`**, when set, is trust in whoever publishes that image. Unset,
  kuma composes a base from Fedora's repositories instead, so the trust root is
  the same as for `packages.rpm`.
- **`packages.flatpak`** is trust in Flathub and in each application's
  publisher. These converge on every boot, as root.
- **`packages.brew`** is trust in Homebrew and in each formula's upstream.
  Naming any formula (or setting `system.brew`) makes the image fetch
  Homebrew's tarball over HTTPS on first boot, with no signature to check
  because Homebrew publishes none.
  Formulae then install into `/home/linuxbrew`, owned by your user, rather than
  into the image.
- **`services.enable`** starts units that are already in the image. It cannot
  introduce one.
- **`system.ca_certificates`** is trust in a certificate authority, and it is
  the most direct entry on this list: a certificate named there is trusted for
  every TLS connection the machine makes, by every program that reads the
  system trust store. It is copied to `/etc/pki/ca-trust/source/anchors/` and
  `update-ca-trust` runs in the same build layer. `kuma check` rejects a value
  that is not a PEM certificate, and rejects one containing a private key
  outright rather than warning, because a key there would be baked
  world-readable into every image built from that declaration.

Names in these lists are validated before they reach dnf, flatpak, systemctl,
or brew: no leading dashes, so a name can't become a flag, and no shell
metacharacters. `rpm = ["--nogpgcheck"]` and `rpm = ["fish; rm -rf /"]` are
both rejected by `kuma check`.

### Two roots your declaration does not name

Choosing a desktop brings in two package sources beyond Fedora's own, and
neither appears in the list above because neither is something you asked for by
name. They are listed here rather than left for you to find in a build log.

- **RPM Fusion**, on every desktop build. Fedora's `mesa-va-drivers` ships with
  H.264/H.265/VC-1 decode stripped for patent reasons, so video silently falls
  back to the CPU; kuma installs RPM Fusion's `mesa-va-drivers-freeworld`
  instead. Getting there means installing `rpmfusion-free-release` from a URL,
  which is the bootstrap every third-party Fedora repository has: the package
  that carries the signing key cannot itself be checked against it. dnf reports
  this as `skipped OpenPGP checks for 1 package`. Everything afterwards,
  including the driver itself, is checked against RPM Fusion's key.
- **`fedora-cisco-openh264`**, which Fedora enables by default and which reaches
  the image because the desktop layer installs weak dependencies. It is hosted
  by Cisco rather than Fedora.

Both are the same trust decision Fedora Workstation makes for the same reason,
and a `minimal` declaration reaches neither. If you want a machine that trusts
only Fedora, declare no desktop.

## What an image publishes

A declaration is written to be committed and is baked world-readable into
every image built from it, at `/usr/lib/kuma/kuma.toml`. Two `[user]` strings
publish with it:

- **`user.password_hash`** is readable by anyone who can pull the image, who
  can then start cracking it offline. That is fine for an image that never
  leaves your machine and bad for one you publish, so don't push an image
  built from a declaration that carries one. The committed examples declare no
  user for this reason; `user.ssh_keys` holds public keys and is safe to
  publish.
- **`user.autologin`** means the machine boots to a session with no password
  prompt. It is a deliberate choice for a kiosk or a VM, and it is not a good
  one for a laptop that leaves the house.

Every image also runs `sshd` with password authentication on, and the account
an install creates is in `wheel`; what that means on a network you do not run,
and the ways to turn it off, are argued in
[how kuma behaves](docs/concepts.md#where-the-base-system-comes-from).

## What a build pins

`kuma.lock` records what a build resolved. The base digest is enforced, so the
same declaration and the same lock build from the same bytes. Package versions
are recorded but not enforced, because Fedora's mirrors garbage collect old
builds within weeks and a pinned version would become a build failure rather
than a defense. The record is there to show what moved between two builds.

`kuma update` is the only thing that moves the pin.

## Verifying a release

Every release asset is signed with Sigstore, keyless, using the release
workflow's own identity. No private key exists to be stolen; what an attacker
would have to take is push access to this repository.

```console
$ cosign verify-blob \
    --bundle kuma-x86_64-unknown-linux-musl.bundle \
    --certificate-identity-regexp '^https://github.com/Letdown2491/kumaos/.+@refs/tags/' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    kuma-x86_64-unknown-linux-musl
```

Worth doing: the install instructions put this binary in `/usr/local/bin` and
it goes on to build the filesystem you boot. That is the same command every
release's notes carry, deliberately: a release's notes cannot be corrected
once people have them, so the two say one thing. The regexp is narrower
than it was at 44.0, and deliberately so: the workflow signs every push to
main too, for the rolling artifact, and a prefix match alone would verify
one of those exactly as a tagged release. `@refs/tags/` is the part a
tag-triggered run has and a main run does not.

The installer media on the same page is signed the same way, and every
release's notes quote this command beside the binary's:

```console
$ cosign verify-blob \
    --bundle kuma-x86_64.iso.bundle \
    --certificate-identity-regexp '^https://github.com/Letdown2491/kumaos/.+@refs/tags/' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    kuma-x86_64.iso
```

Worth more, if anything. The binary builds a system you then choose to boot;
this file boots one directly, on hardware, before you have anything to
inspect it with. The `.sha256` beside it is not a substitute, because it is
served from the same page: whatever could replace one could replace both.

Published images are signed with a key pair instead, and `cosign.pub` in this
repository is the public half:

```console
$ cosign verify --key cosign.pub ghcr.io/letdown2491/kuma:niri
```

The two differ for a reason rather than by accident. A `policy.json`
`sigstoreSigned` requirement takes exactly one of `keyPath`, `fulcio` or
`pki`, and the `fulcio` block requires both `oidcIssuer` and `subjectEmail`.
A GitHub Actions certificate carries no email, only a URI SAN naming the
workflow, so no policy file can express "signed by kuma's release workflow".
An image people configure a machine to trust needs a key a policy can name; a
blob a person verifies once by hand does not.

## The signing key and its custody

**Your machine checks that signature without being asked.** Every kuma image
ships the key at `/etc/pki/containers/kuma.pub` and a `/etc/containers/policy.json`
requiring a valid kuma signature for `ghcr.io/letdown2491/kuma`, so an update
that did not come from kuma is refused rather than installed. `kuma doctor`
grades this, because a signature nobody checks is a claim and not a control.

The rule is deliberately narrow. That policy file is shared by podman and
bootc, so requiring signatures everywhere would refuse Fedora's base image on
your next `kuma update` and refuse your own locally built image on your next
`kuma switch`. Everything other than kuma's own published repository is left
as it was.

Two consequences worth knowing. The identity is matched at repository level,
because that is what cosign records: a signature is accepted for
`ghcr.io/letdown2491/kuma` regardless of which tag you pull, so anything kuma
signed and published is trusted by any machine tracking that repository. And
images you build yourself are not signed and are not required to be; they come
from your own storage, which the policy leaves alone.

Publishing is refused without a signature unless somebody explicitly asks for
it, and the workflow verifies its own output against the committed public key
before finishing, because a `cosign.pub` that does not match the signing
secret fails silently: the image publishes and verification fails everywhere
else.

The private half of the key lives in GitHub's secret store; only the public
half is committed, and the check above is what ties the two together. What
happens to that key is worth saying plainly:

**Losing it costs what is unpublished, nothing published.** Signatures live
in the registry, so machines keep verifying and keep upgrading within what
has already been published. Nothing new can be signed, and a machine whose
policy names the lost key cannot receive a new policy by update, because
the update is exactly what that policy refuses. Adopting a new key is a
deliberate step on each machine, which is the design refusing to make key
adoption silent.

**Rotating while the key is held can ride an update.** Images can be signed
with both keys while the policy names both, so the new policy reaches
machines as an ordinary verified update, and the old key retires once no
machine still requires it.

**Rotating because the key is compromised has no in-band path, by design.**
Any path that swapped a machine's key on its own is the path an attacker
holding the key would use too, so a new key reaches machines the way the
first one did: deliberately, by the person responsible for the machine.

Images kuma builds for you are not signed. They're built on your machine, from
your declaration, and stay in your local container storage unless you push
them somewhere. What these guarantees promise and what a release that ends a
promise may change is stated in docs/contract.md.

## The nostr key and its custody

The `[nostr]` layer's daemon holds a signing key, and a key that answers
requests from apps is a different custody problem from the one above:
this one lives *on the machine it signs for*, and the questions are what
holds it, what unlock means, and what an asking app can do.

**What holds it: the login keyring, at rest.** The key is stored in the
Secret Service's login collection — the same store your browser's
certificates and wifi passwords use — as a NIP-49 `ncryptsec`, a
passphrase-wrapped form whose passphrase sits beside it. That is stated
plainly because it is the design: the keyring is the wall, the wrap is
the stored format, and the wrap adds no second secret today. It exists
so the upgrade to an independent-passphrase vault is a change of what
fills the same blob, never a migration of anything.

**The key that signs is not your identity.** The layer generates a
dedicated remote-signer key and the bunker signs with that. An app
paired to the bunker learns your nostr identity only if you import one
and answer a `get_public_key` with it in hand — and the pubkey a relay
operator watches answer is the bunker's, pseudonymous by construction.

**What unlock means: a gate, not a wall.** The session's PAM unlocks
the keyring at login, so the daemon reads the key unattended and comes
up answering; `kuma-nostr lock` drops the key from memory and refuses
everything until `unlock` re-reads it. The honest sentence: none of this
protects against an attacker already running as your user inside your
unlocked session, because nothing in that position can make that
promise. What the lock is for is everything narrower — a guest at the
keyboard, a script you did not watch, a moment you want the signer
quiet.

**What a paired app can do is the policy engine's answer, and the
default is Ask.** Every consequential method waits on a prompt that
names the app and shows the exact event before anything is signed; a
newly paired app can do nothing unattended. Relaxing an app to Basic
lets it sign unattended only the kinds an explicit safe list vouches
for — the everyday social surface; sensitive writes (profile, follows,
relay and mute lists, deletions), the decrypt methods, NIP-04
encryption — whose job is private messages — and every kind the list
does not name still ask; Trust removes the asks, and is graded Warn by
name
by `kuma doctor`, because a standing grant is the loudest thing in the
layer. An approved ask can be remembered for an hour at most — the
ceiling is the verb itself, and nothing in the layer mints a longer
standing grant. Every decision lands in an activity log whose privacy
mode is structural: the record carries the method, the event kind, and
the verdict, never a param.

**What an app holds is a per-app pairing, not the key.** Pairing grants
the right to *ask*; it never hands out key material. A pairing URI
carries a one-time secret, and the connect that presents it burns it:
a URI pairs one app once, a second connect with the same secret is
refused, and a bunker with no outstanding secrets pairs nobody until
the person mints a fresh URI. Revoking an app tombstones it — the
connect is refused whatever the app carries, the tombstone survives
restarts, and `unrevoke` clears it (the way back in is still a fresh
URI) — deleting one removes it outright (a fresh URI pairs again),
and the app's own `logout` is the same deletion under the app's name.
Either way the key is untouched, and a known app reconnects by its own
identity: its requests are signed with the key it paired with, so a
client restart is a hello, not a stranger.

**The vault can lock itself.** The inactivity switch — a declaration
window, an hour's floor, off by default — runs the same lock the
panel's verb runs when nothing has unlocked or kept the bunker alive
for that long. It exists for the machine that stops answering with the
gate still open; the desktop's default posture is the PAM-open keyring,
so the switch is yours to arm.

**What the sandbox bounds.** The daemon runs as a user unit under the
graphical session with the full systemd sandbox: no new privileges, a
read-only system and home, one writable directory for its own state, a
reduced syscall filter, and an empty capability set. The socket it
answers on is 0600 under the session's runtime directory, and a peer
whose uid is not the daemon's own is dropped before its first byte is
read. Reaching the socket grants nothing by itself — asking is what it
buys, and the policy engine is still the decider.

**What a relay sees: metadata, never content.** Relays carry only
signing traffic (kinds 24133 and 24135), and every payload is NIP-44
encrypted end to end, so a relay operator learns which app asked which
bunker, how often, and how large — no feed, no profile, no signature
content. The layer bakes a relay on the machine itself, loopback only,
in memory; declared relays are fallbacks, `wss://` to the world or
`ws://` to loopback, and a declaration asking for plaintext to the
public internet is refused at build.

**Losing the key costs the nostr identity, and nothing else on the
machine.** The bunker's key is unrelated to the image-signing key above,
to disk encryption, and to the account; deleting the vault (`destroy`)
is unrecoverable for the nostr identity alone, and the destroy verb is
a dry run until confirmed for exactly that reason.

## What runs as root

`init`, `check`, `generate`, and `build` need only rootless podman.

`switch`, `update`, `rollback`, and `sync` call `bootc` and `systemctl` under
sudo. `vm` and `iso` need sudo because bootc-image-builder runs as root, with
one exception: `iso --live` never calls it, and so never asks. `install` runs
`bootc install` in a privileged container with `/dev` bound in, which is what
writing a disk requires. Kuma asks for sudo at those points and nowhere else.

## Not yet

- Builds are not reproducible, and kuma makes no claim that two builds of one
  declaration produce identical bytes.
- Kuma emits no SBOM. `kuma.lock` records resolved package versions, which is
  adjacent but not the same thing.
- Images built from your own declaration are not signed and are not required
  to be; they come from your own storage, which the policy leaves alone. The
  base image is unsigned, and the policy deliberately leaves it alone too:
  the digest pin in `kuma.lock` is the only check on it.
- There is no security advisory history, because there have been no advisories.
