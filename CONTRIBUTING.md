# Contributing

This is about working on kuma itself. For using it, start at
[getting started](docs/getting-started.md); for why it behaves the way it
does, [how kuma behaves](docs/concepts.md).

**Writing.** Prose a reader meets does not use the em dash: the docs, the
changelog, and the strings kuma prints. To a growing share of readers the
mark reads as machine-written before it reads as punctuation, and the
constructions it stands in for are the house voice's own: parentheses, a
colon, a comma, or a sentence of its own. Take the marks out per sentence,
choosing whichever of those the sentence actually wanted; there is no
mechanical replacement, which is why the rule names the character and not
the workload. Internal comments and notes are free to keep theirs. A
hyphen is untouched: compound words and numeric ranges keep the short mark
they always had.

**Smoke tests.** `scripts/smoke.sh` builds every committed example and, on
request, installs or boots it. Five stages: `check` validates the
declaration, `image` builds it and inspects what a successful build doesn't
already prove, `install` writes an encrypted disk and verifies what landed on
it, `boot` makes a disk, boots it headless, and asks the machine whether
the boot was healthy, and `published` installs an image kuma published and
boots the disk that install wrote. That boot verdict is greenboot's own, so
the check that would roll an update back is the one that decides whether the
test passed.

`--published` is the only stage that reads the registry rather than the
tree, so it can go red without anyone having committed anything, and it
lives in its own workflow for that reason. It is also the only one that
boots a disk `kuma install` wrote interactively: every other disk here is
written by the same installer non-interactively, so the boot stage
exercises the same btrfs-root layout a real machine gets. Add
`--upgrade-to <image>` and it installs one
version, upgrades to another, and asks whether the machine survived;
`--encrypted` types the passphrase at the guest's serial console.

`--install` and `--boot` are separate because they answer different
questions. Boot asks whether a machine works. Install asks whether the disk
is the one that was described: that the container opens with the passphrase
that was typed, that the bootloader unlocks the container actually present,
and that the account file says what the installer was told. Those are the
failures nobody can recover from, since by the time they show up the disk
that used to hold something else is gone.

```console
$ cargo test                    # the tier that needs no machine
$ cargo fmt                     # rustfmt.toml settles layout; CI checks it
$ scripts/smoke.sh              # check + image, every example
$ scripts/smoke.sh --install    # plus a real install (needs sudo, no KVM)
$ scripts/smoke.sh --boot       # plus a real boot (needs KVM and sudo)
$ scripts/smoke.sh --boot cosmic
$ scripts/smoke.sh --published ghcr.io/letdown2491/kuma:niri
```

**Building without a compiler.** The machines most likely to run kuma are
image-based and ship no compiler, and layering one onto a bootc host to work
on the tool that builds bootc hosts is the wrong shape.
`scripts/Containerfile.dev` builds a container that has one. Cargo is not in
it: that comes from your home directory, so a build inside the container
shares `target/` and the registry cache with one outside it.

```console
$ scripts/test.sh                   # the suite, in the container
$ scripts/test.sh --test golden     # any cargo test filter
```

CI runs formatting, tests, clippy at `-D warnings`, shellcheck, actionlint,
and the image stage on every committed example, desktops included. That
covers the compose and both desktop arms, so the build-time guards in them
run on a push.

The `boot`, `install`, `hibernate`, `dead-disk`, and `iso` jobs run there
too, for pushes to main and on a daily schedule. All of them ride the niri
example except `dead-disk`, which builds a declaration of its own so the
restore has a machine to bring back. They were local-only on the stated
grounds that they need KVM and sudo, which turned out to be one udev rule:
a hosted runner has `/dev/kvm` and the runner user is simply not in the
`kvm` group. The daily run exists because the base kuma builds on moves
without anyone pushing, so a boot can break on a tree that was green
yesterday. COSMIC is built on every push and not booted; that is what
calling it experimental means here.

`published.yml` is separate: it installs and boots what is on the registry,
upgrades an older published version to the current one, and boots an
encrypted install. `publish.yml` calls it with the image it just pushed, so
the artifact a stranger gets is verified without anybody remembering to ask.
Running it by hand is still there for asking about an image that was
published earlier, which is the only case where the order is yours to get
right: it reads the moving tag, so running it before a publish tests the
previous release.

The image stage is the one part that does not repeat on a tag. Cutting a
release means tagging a commit that already went green on main, so the tag
names a tree those images were already built from, and building them again
only delays the release. Everything else in `ci.yml` still runs on the tag,
with one exception: the hibernate job runs on the daily heartbeat and on a
manual dispatch, never on a push. It is the slowest gate in the file, its
fixture has accumulated documented QEMU artifact classes, and a push gate
that goes red for a fixture reason is one rerun away from being ignored.
So before tagging, dispatch `ci.yml` on the exact commit you intend to tag
and wait for the hibernate job: it is the only automated check that a
hibernated machine comes back, and a release is the thing claiming that
promise.

Two limits in that list worth knowing before you trust it. shellcheck reads
`scripts/smoke.sh` and nothing else, so the shell that images actually run
(the sync units and helpers, which live in Rust string literals in
`containerfile/blocks.rs`) is unchecked; run it by hand against the
generated file if you touch one. And the image stage proves an image builds,
never that it works, so anything whose behaviour appears at runtime has no
gate on a push at all.

A third thing the tests watch that you will meet the first time you change
what an image carries: the generator is pinned by goldens. Five
declarations' worth of Containerfile text and staged-file manifests live in
`src/containerfile/goldens/`, and any change to what an image ships shows
up as a diff against them. The fifth of them, `secrets`, exists so the
staging of the two values that get special handling (a password hash and
a CA anchor, both public fixtures there) is pinned by bytes like the
rest, not left to a unit test a refactor could outlive. That diff is the
review of your change: read
it, don't just regenerate past it. When the change is deliberate, run
`UPDATE_GOLDENS=1 cargo test` and commit the result. The builder label
carries `git describe`, so it is normalized out of the comparison; a golden
that moves is moving something that ships.

actionlint is there because a workflow can be valid YAML and still be
rejected by Actions, which says so by running no job at all and leaving no
log to read.

Third-party actions are pinned by commit SHA, version in a comment, for the
same reason the actionlint container is pinned by digest: a tag is a
mutable name, and these steps hold the workflow token and the signing
identity. Dependabot raises the bumps; reviewing the diff between two SHAs
is the review.

A separate job runs `cargo audit` against the committed `Cargo.lock`, on every
push and again weekly, because a dependency becomes vulnerable when the
advisory lands rather than when someone next touches the tree.

**Cutting a release.** Bump `version` in `Cargo.toml`, refresh the lock,
rename the `Unreleased` section in `CHANGELOG.md` to the new version, commit
all three, then tag:

```console
$ cargo update -p kuma --offline   # Cargo.lock records kuma's own version
$ git tag -a v0.4.0 -m "kuma v0.4.0"
$ git push origin v0.4.0
```

`Cargo.lock` is not optional here. It carries the workspace member's version
too, so bumping only `Cargo.toml` leaves the lock disagreeing and every
`--locked` call fails, which is most of CI.

The tag and `Cargo.toml` have to agree as well. The release workflow checks
and fails rather than publishing a binary whose own `--version` contradicts
the release it sits in.

The changelog is checked twice, once where it can still be fixed cheaply. A
test fails locally when `Cargo.toml`'s version has no section, and the release
workflow fails on a tag whose section is missing, because it builds the release
notes out of that section. Leave a fresh empty `Unreleased` behind for the next
one: entries are meant to land in the same push as the change they describe,
and a section written at tag time is a section written from memory.

What goes in it is what changes a machine, not what changed in the tree. Docs,
tests, and CI stay out. A release with nothing user-facing in it is a release
whose section says so in one line.

Push to `main` first and let it go green. A push that touches anything other
than documentation runs the release workflow itself rather than a copy of it,
so main is a complete rehearsal of every step a tag will run: the same test on
the release target, the same packaging, the same signature. It stops one step
short, publishing nothing, and keeps the binary as a workflow artifact
instead. A problem surfaces there while it costs nothing, instead of once a
tag already exists.

The releases page only ever lists tagged versions. A rolling entry on top of
them reads as a version that shipped, and none did.

A release is two files people use and four that prove them: one static
`x86_64` binary and the live installer ISO, each with a checksum and a
Sigstore bundle beside it. Neither asset name carries a version, which is
what keeps the `releases/latest/download/` URLs in the README and the
walkthrough correct from one release to the next; tests pin the names to
those URLs so a rename cannot quietly break the front door, and pin that
nothing reaches a release page unsigned.

The release is assembled as a draft and published in one step at the end.
`latest/download/<name>` answers from the newest release and does not fall
back, so a release that exists without its ISO would break the documented
download for everyone rather than only for itself.

**Knowing which binary you have.** `kuma --version` reports the commit it
was built from and appends `-dirty` when that tree had uncommitted changes.
Worth checking whenever a change doesn't appear in the image: `kuma build`
runs whatever `kuma` is on `$PATH`, which is not necessarily the tree you
just edited.

**Testing in a VM.** [Getting started](docs/getting-started.md) covers `kuma
vm` and `kuma iso` as a user meets them. What matters when developing:

- Log in as your declared `[user]`, or as the always-present test user
  `kuma`/`kuma` (`ssh -p 2222 kuma@localhost`; your ssh key is injected).
- Pass `--rebuild` after rebuilding the image. Without it the old disk is
  silently reused, and kuma warns when that disk is older.
- `kuma vm --apply` streams the freshly built image into the *running* VM and
  switches inside it, so `/var` survives and flatpaks, brew, and homes do not
  re-download. It is also the real update path, so `bootc rollback` inside
  the VM undoes it.
- Live media is UEFI-only, so give a test VM UEFI firmware and a 3D-capable
  device (`-device virtio-vga-gl -display gtk,gl=on`), or the desktop renders
  nowhere. It runs SELinux permissive, since a container image's real labels
  are not reachable through a podman mount, and installing from it pulls the
  image over the network rather than copying the media. Both are explained
  where they are set, in `src/liveiso.rs`.

**Installing.** The whole destructive half is one generated script
(`src/partition.rs`), which unwinds itself on failure and can be dumped for
shellcheck: `KUMA_DUMP_SCRIPT=/tmp/install.sh cargo test dump_the_script`. It
refuses a disk with anything in use on it, asking `lsblk` rather than only
`/proc/mounts`, because an encrypted root is named by its mapper device in
the mount table.

**Publishing an image.** `.github/workflows/publish.yml`, manual dispatch
only. It builds from a committed example, runs `scripts/publish-audit.sh`
against the result *before* touching the registry, and refuses to push
unsigned unless asked to. The audit is the interesting part: a published
image must carry no account, no hostname, no ssh keys, and no build paths in
the baked binary, and four of those five come from building it from a
declaration with no `[user]`. The fifth does not, which is why the check
exists rather than a rule in a document. Run it locally against any image:
`./scripts/publish-audit.sh localhost/kuma:latest`.

**Inspecting an image.** It's a normal OCI image:
`podman run --rm -it localhost/kuma:latest bash`.

