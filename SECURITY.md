# Security

Kuma is early and maintained by one person.

Three things here are worth reading even if you never report a bug: what
naming a package in your declaration opts you into, how to check that the
binary you downloaded came from this project, and what happens when the
signing key is lost or rotated.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting:
[**Report a vulnerability**](https://github.com/Letdown2491/kumaos/security/advisories/new).
It reaches the maintainer privately, and it is the only channel. Please
don't open a public issue for one first.

Worth including: the output of `kuma --version`, the declaration that
reproduces it with any password hash removed, and what an attacker ends up
able to do.

Expect a reply in days rather than hours. There is no bounty and no
response guarantee. Only the most recent release is supported; fixes go
out in a new release rather than as backports to older tags.

## What is kuma's to fix

Kuma compiles a declaration into a Containerfile and hands it to podman.
It builds no packages and no kernels. What it adds to an image is its own
binary, the systemd units it writes, and the desktop assets compiled into
it.

So a flaw in the kernel, in systemd, in a Fedora package, or in bootc is
not kuma's to patch. kuma has no patch mechanism of its own because it
needs none: a rebuild resolves against Fedora's packages as they are that
day, which makes updating and patching the same operation. `kuma update`
is how you take security updates.

A flaw in how kuma generates a build, in what it puts in an image, or in
what it runs on your machine is kuma's, and is worth reporting.

## Your declaration is the trust boundary

One short file spans several trust roots, and naming a string is how you
opt into each:

- **`packages.rpm`** comes from Fedora's repositories. You cannot declare
  a third-party repository; there is no key for it. Signature checking is
  dnf's default and kuma never disables it. A name that tries to become a
  flag (`rpm = ["--nogpgcheck"]`) is rejected before it reaches dnf.
- **`system.base`**, when set, is trust in whoever publishes that image.
  Unset, kuma composes a base from Fedora's repositories instead, so the
  trust root is the same as for `packages.rpm`.
- **`packages.flatpak`** is trust in Flathub and in each application's
  publisher. These converge on every boot, as root.
- **`packages.brew`** is trust in Homebrew and in each formula's
  upstream. Naming any formula makes the image fetch Homebrew's tarball
  over HTTPS on first boot, with no signature to check because Homebrew
  publishes none. Formulae install into `/home/linuxbrew`, owned by your
  user, rather than into the image.
- **`services.enable`** starts units that are already in the image. It
  cannot introduce one.
- **`system.ca_certificates`** is trust in a certificate authority, and
  it is the most direct entry on this list: a certificate named there is
  trusted for every TLS connection the machine makes. `kuma check`
  rejects a value that is not a PEM certificate, and rejects one
  containing a private key outright rather than warning, because a key
  there would be baked world-readable into every image built from that
  declaration.

Names in these lists are validated before they reach dnf, flatpak,
systemctl, or brew: no leading dashes, so a name can't become a flag, and
no shell metacharacters. `rpm = ["fish; rm -rf /"]` is rejected by `kuma
check`.

### Two roots your declaration does not name

Choosing a desktop brings in two package sources beyond Fedora's own.
Neither appears in the list above, because neither is something you asked
for by name. They are listed here rather than left for you to find in a
build log.

- **RPM Fusion**, on every desktop build. Fedora's `mesa-va-drivers`
  ships with H.264/H.265/VC-1 decode stripped for patent reasons, so
  video falls back to the CPU. kuma installs RPM Fusion's
  `mesa-va-drivers-freeworld` instead. Getting there means installing
  `rpmfusion-free-release` from a URL, which is the bootstrap every
  third-party Fedora repository has: the package that carries the signing
  key cannot itself be checked against it. dnf reports this as `skipped
  OpenPGP checks for 1 package`. Everything afterwards, including the
  driver itself, is checked against RPM Fusion's key.
- **`fedora-cisco-openh264`**, which Fedora enables by default and which
  reaches the image because the desktop layer installs weak dependencies.
  It is hosted by Cisco rather than Fedora.

Both are the same trust decision Fedora Workstation makes for the same
reason, and a `minimal` declaration reaches neither. If you want a
machine that trusts only Fedora, declare no desktop.

## What an image publishes

A declaration is written to be committed and is baked world-readable
into every image built from it, at `/usr/lib/kuma/kuma.toml`. Two
`[user]` strings publish with it:

- **`user.password_hash`** is readable by anyone who can pull the image,
  who can then start cracking it offline. That is fine for an image that
  never leaves your machine and bad for one you publish, so don't push an
  image built from a declaration that carries one. The committed examples
  declare no user for this reason. `user.ssh_keys` holds public keys and
  is safe to publish.
- **`user.autologin`** means the machine boots to a session with no
  password prompt. It is a deliberate choice for a kiosk or a VM, and a
  poor one for a laptop that leaves the house.

Every image also runs `sshd` with password authentication on, and the
account an install creates is in `wheel`. The firewall does not open ssh
to the world: the only address that reaches it is qemu's user-net
gateway, the lane the test harness arrives from. Online guessing is
capped at 50 failures for a day. What that means on a network you run,
how to open ssh deliberately, and how to turn it off:
[the base system](docs/concepts.md#the-base-runs-sshd).

## What a build pins

`kuma.lock` records what a build resolved. The base digest is enforced,
so the same declaration and the same lock build from the same bytes.
Package versions are recorded but not enforced, because Fedora's mirrors
garbage-collect old builds within weeks and a pinned version would become
a build failure rather than a defense. The record is there to show what
moved between two builds.

`kuma update` is the only thing that moves the pin.

## Verifying a release

Every release asset is signed with Sigstore, keyless, using the release
workflow's own identity. There is no private key to steal. What an
attacker would have to take is push access to this repository.

```console
$ cosign verify-blob \
    --bundle kuma-x86_64-unknown-linux-musl.bundle \
    --certificate-identity-regexp '^https://github.com/Letdown2491/kumaos/.+@refs/tags/' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    kuma-x86_64-unknown-linux-musl
```

Worth doing: the install instructions put this binary in
`/usr/local/bin`, and it goes on to build the filesystem you boot. The
same command appears in every release's notes. That is deliberate: a
release's notes cannot be corrected once people have them, so the two
say one thing. The regexp is narrower than it was at 44.0, on purpose:
the workflow also signs every push to main, for the rolling artifact,
and a prefix match alone would verify one of those as a tagged release.
`@refs/tags/` is the part a tag-triggered run has and a main run does
not.

The installer media is signed the same way:

```console
$ cosign verify-blob \
    --bundle kuma-x86_64.iso.bundle \
    --certificate-identity-regexp '^https://github.com/Letdown2491/kumaos/.+@refs/tags/' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    kuma-x86_64.iso
```

Worth more, if anything. The binary builds a system you then choose to
boot; this file boots one directly, on hardware, before you have
anything to inspect it with. The `.sha256` beside it is not a
substitute, because it is served from the same page: whatever could
replace one could replace both.

Published images are signed with a key pair instead, and `cosign.pub` in
this repository is the public half:

```console
$ cosign verify --key cosign.pub ghcr.io/letdown2491/kuma:niri
```

The two differ for a reason. A podman `policy.json` can name either a
key or a Sigstore identity, but a GitHub Actions certificate carries no
email, and the identity form of the policy requires one. So no policy
file can express "signed by kuma's release workflow". An image a machine
trusts needs a key the policy can name; a blob a person verifies once by
hand does not.

## The signing key and its custody

**Your machine checks that signature without being asked.** Every kuma
image ships the key at `/etc/pki/containers/kuma.pub` and a
`/etc/containers/policy.json` requiring a valid kuma signature for
`ghcr.io/letdown2491/kuma`, so an update that did not come from kuma is
refused rather than installed. `kuma doctor` checks this, because a
signature nobody checks is a claim, not a control.

The rule is deliberately narrow. That policy file is shared by podman
and bootc, so requiring signatures everywhere would refuse Fedora's base
image on your next `kuma update` and refuse your own locally built image
on your next `kuma switch`. Everything other than kuma's own published
repository is left as it was.

Two consequences. The identity is matched at repository level, so any
image kuma signed and published is trusted by any machine tracking that
repository, whatever tag it pulls. And images you build yourself are not
signed and not required to be; they come from your own storage, which
the policy leaves alone.

Publishing is refused without a signature unless somebody explicitly
asks for it, and the workflow verifies its own output against the
committed public key before finishing. That check exists because a
mismatched `cosign.pub` fails silently: the image publishes, and
verification fails everywhere else.

The private half of the key lives in GitHub's secret store; only the
public half is committed, and the check above is what ties the two
together. What happens to that key is worth saying plainly:

**Losing it costs what is unpublished, nothing published.** Signatures
live in the registry, so machines keep verifying and keep upgrading
within what has already been published. Nothing new can be signed. And a
machine whose policy names the lost key cannot receive a new policy by
update, because the update is exactly what that policy refuses. Adopting
a new key is a deliberate step on each machine. That is the design
refusing to make key adoption silent.

**Rotating while the key is held can ride an update.** Images can be
signed with both keys while the policy names both, so the new policy
reaches machines as an ordinary verified update, and the old key retires
once no machine still requires it.

**Rotating because the key is compromised has no in-band path, by
design.** Any path that swapped a machine's key on its own is the path
an attacker holding the key would use too. So a new key reaches machines
the way the first one did: deliberately, by the person responsible for
the machine.

Images kuma builds for you are not signed. They are built on your
machine, from your declaration, and stay in your local container storage
unless you push them somewhere. What these guarantees promise, and what
a release that ends a promise may change, is stated in
[the contract](docs/contract.md).

## The nostr key

The `[nostr]` layer's signing key is a different custody problem: it
lives on the machine it signs for, in your login keyring. Where it sits,
what the lock does and does not protect, and what a paired app can do:
[the nostr layer](docs/nostr.md).

## What runs as root

`init`, `check`, `generate`, and `build` need only rootless podman.

`switch`, `update`, `rollback`, and `sync` call `bootc` and `systemctl`
under sudo. `vm` and `iso` need sudo because bootc-image-builder runs as
root, with one exception: `iso --live` never calls it, and so never
asks. `install` runs `bootc install` in a privileged container with
`/dev` bound in, which is what writing a disk requires. Kuma asks for
sudo at those points and nowhere else.

## Not yet

- Builds are not reproducible, and kuma makes no claim that two builds
  of one declaration produce identical bytes.
- Kuma emits no SBOM. `kuma.lock` records resolved package versions,
  which is adjacent but not the same thing.
- Images built from your own declaration are not signed and are not
  required to be; they come from your own storage, which the policy
  leaves alone. The base image is unsigned, and the policy deliberately
  leaves it alone too: the digest pin in `kuma.lock` is the only check
  on it.
- There is no security advisory history, because there have been no
  advisories.
