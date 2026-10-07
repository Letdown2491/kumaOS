# How kuma behaves

This page explains why kuma works the way it does. Read it when a behaviour
surprises you, or before you trust the machine with something important.
If you want the commands, [getting started](getting-started.md) walks the
path. New words are in [the glossary](glossary.md).

- [What happens to changes you make by hand](#what-happens-to-changes-you-make-by-hand)
- [What updates itself, and what waits for you](#what-updates-itself-and-what-waits-for-you)
- [Boot health and automatic rollback](#boot-health-and-automatic-rollback)
- [Where the base system comes from](#where-the-base-system-comes-from)
- [What every image carries](#what-every-image-carries)
- [Why a desktop installs things you did not name](#why-a-desktop-installs-things-you-did-not-name)
- [kuma in your launcher](#kuma-in-your-launcher)
- [What a build records: kuma.lock](#what-a-build-records-kumalock)
- [Why a file you edited by hand keeps winning](#why-a-file-you-edited-by-hand-keeps-winning)
- [Permissions, and a file kuma does not own](#permissions-and-a-file-kuma-does-not-own)
- [Hibernation](#hibernation)
- [Backups, and the two things a restore needs](#backups-and-the-two-things-a-restore-needs)
- [What your machine trusts](#what-your-machine-trusts)
- [What a declaration does not reproduce](#what-a-declaration-does-not-reproduce)
- [What an install decides that a declaration cannot](#what-an-install-decides-that-a-declaration-cannot)
- [The nostr layer](#the-nostr-layer)

## What happens to changes you make by hand

You will install things without declaring them. That is fine. kuma calls
this **drift**, and treats it as a suggestion rather than a mistake:

```console
$ kuma diff
packages.flatpak
  - org.gnome.Boxes  installed, not declared (convergence removes it)
Ad-hoc flatpaks, kept as yours: io.github.kolunmi.Bazaar

  → kuma capture   keep them: declare what this machine already runs
  → kuma sync      converge now; otherwise the boot/daily run picks this up
```

Read the example carefully, because the two lines mean different things.
Boxes was in your declaration once and you removed it, so the next
convergence uninstalls it. Bazaar is something you installed yourself, and
kuma leaves it alone. Nothing is ever deleted just for being undeclared.
Install apps from a store if you like; the cost of not declaring
something is reproducibility, never survival.

Updates reach undeclared software too. Keeping what exists current is
kuma's job no matter who installed it, so a store app, an ad-hoc `brew
install`, and a declared app are all updated on the same schedule. If you
want to stay on a version, use the ecosystem's own hold: `flatpak mask`
or `brew pin`.

`kuma capture` turns drift into a proposal against your declaration. It
shows you a diff of the *file* and writes nothing until `--yes`. It
gathers flatpaks and brew leaves, because those are the things you can
install by hand. It skips rpms (a bootc machine can't install one
imperatively anyway) and it never touches `[user]` or `[system]`, because
a password hash must not walk into a file you commit.

Snapshots follow the same rule. `kuma snapshot --restore <path>` is a dry
run that says which snapshot a path would come back from and what it
would replace. It restores a path, never a whole subvolume: the accident
people actually have is one file.

## What updates itself, and what waits for you

Two kinds of software, two policies.

Flatpaks and brews update themselves. Convergence runs at boot and on a
daily timer, installs what your declaration names, and updates everything
present whether declared or not. The daily run waits for wall power and
an unmetered network first. If you want an app updated right now, update
it by hand; that is drift by choice, and `kuma capture` is how you would
keep it.

The system image is the opposite. An update replaces the whole operating
system and applies at the next reboot, so automating it would buy you
surprise reboots or a pile of staged updates you never booted. kuma stages
nothing you did not ask for. `kuma update` is yours to run.

What is automated is knowing when to run it:

- `kuma update --check` asks the repos what has moved since this image
  was built, security advisories first. Takes seconds, builds nothing.
- `kuma doctor` reports how old the booted image is and warns past a
  month. On Fedora, a month means at least one kernel you did not take.

```console
warn  deployment: booted image is 41 days old
      → kuma update   recompose against the repos' current packages and rebuild
```

One change is loud enough to announce itself. If an update would move you
to a new Fedora release, `kuma update` says so in plain words before it
stages anything:

```console
This is a Fedora release change: 44 to 45.
```

That is the biggest change kuma can make to a machine, and in a package
diff it would otherwise look like several hundred ordinary lines.

## Boot health and automatic rollback

Every image includes [greenboot](https://github.com/fedora-iot/greenboot-rs).
There is nothing to configure. A declarative system whose one bad update
can strand the machine is not declarative where it counts.

Here is how it works. The first boot of a new deployment arms a rollback
trigger with three attempts. A boot that hangs before the desktop comes
up burns an attempt like any other failure. On a desktop image, a boot
only counts as healthy once the greeter is actually on screen, so "boots
into a black screen" is exactly the failure this catches. After three
failed attempts, the machine boots the previous deployment on its own and
makes that permanent. A bad update costs you three reboots, not the
machine.

Three details worth knowing:

- There are no default health checks beyond the greeter. greenboot's own
  optional package makes DNS resolution required, which is reasonable on
  a server and absurd on a laptop booting offline. Add your own checks
  under `/etc/greenboot/check/required.d/`.
- Machines installed before this feature existed are retrofitted on
  every boot by `kuma-boot-health-sync`, because the boot counter lives
  in bootloader config written at install time.
- The boot menu names what it actually boots. ostree only rewrites an
  entry's title when the kernel or its arguments change, so menus
  otherwise keep stale version names. `kuma-boot-titles.service` reads
  each entry's title from the deployment it points at.

A rollback is never silent. The failed deployment stays in the rollback
slot, and `kuma doctor` reports the boot verdict and whether the
bootloader is counting correctly. And a deployment that used to work but
now fails reboots three times and then waits for a human, because rolling
back cannot fix a problem an update didn't cause.

## Where the base system comes from

The usual way to build a bootc image is to start `FROM` a big base image
and remove what you don't want. Without `system.base` in your
declaration, kuma does the opposite: it composes its own base from
Fedora's packages with `rpm-ostree compose image`, the same tool Fedora
uses to build fedora-bootc.

The starting point is Fedora's minimal bootc manifest, which is
essentially kernel, systemd, bootc, and dnf. kuma adds what a real
machine needs. Nothing is ever removed after the fact; the heavy stuff
fedora-bootc carries for the general case simply never gets included.
Fedora remains the package source for all of it. kuma builds no packages
and no kernels.

The composed image is content-addressed: its name is a hash of the
manifest that produced it. A build can therefore name its base before
any compose has run, an unchanged manifest reuses the image already in
storage, and a changed manifest cannot reuse a stale one.

Two consequences:

- **`system.firmware` is the trim.** If unset, the base ships every
  vendor's firmware, so a machine that declares nothing about its
  hardware still boots with working graphics, wifi, and audio. Name what
  your hardware needs and the rest stays out.
- **`kuma update --check` asks the repos, not a tag.** A composed base
  has no upstream tag to check, and an update recomposes everything. So
  the check asks dnf directly what has a newer version and which of
  those carry security advisories.

Naming a `base` opts out of all of this: any bootc image can be one.

### The base runs sshd

`openssh-server` is in the base and the image enables `sshd.service` by
name. This is deliberate: `kuma vm` and the smoke tests reach the guest
over ssh, and an image they could not reach would break the test harness.

That does not mean the world can reach it. The shipped firewalld zone
does not open ssh at all. The one exception is a rule admitting ssh from
10.0.2.2, the gateway address of qemu's user-mode networking, because
that is where the test machines connect from. (The side effect, stated
in the zone's own description: a network numbered 10.0.2.0/24 could
reach sshd too.)

Sign-ins use Fedora's defaults, so passwords work, and the account
`kuma install` creates is in `wheel`. But a stranger on the coffee-shop
wifi cannot get to the prompt. If you want to reach your own machine
over the network, open it deliberately:

```console
$ sudo firewall-cmd --permanent --zone=public --add-service=ssh
```

Online password guessing is capped: after 50 failed attempts the account
locks for a day. `faillock --user <name> --reset` clears it.

If you declare `[user].ssh_keys`, kuma serves them from
`/etc/kuma/keys/<name>` and never overwrites the user's own
`~/.ssh/authorized_keys`. To require keys outright, drop a config file
into `/etc/ssh/sshd_config.d/`. Files in `/etc` survive image updates,
and `kuma doctor` will report yours as a local modification, which is
exactly what it is.

`[services].disable = ["sshd.service"]` turns the unit off entirely.
sshd is a default, not part of the hardening floor, so your declaration
wins. Boot health, rollback, and the floor itself sit below that line
and cannot be switched off. Disable sshd and `kuma vm` still builds a
disk, but nothing can ssh into it.

### The hardening floor

Below the `[services]` line sits a set of security settings that no
declaration can turn off. It is taken from secureblue's audited
hardening, adapted to what kuma can test. Six files ship in every image:

- `/usr/lib/sysctl.d/70-kuma-hardening.conf` — one file of kernel
  settings: programs may only debug their own children, kernel pointers
  are hidden in `/proc`, perf is root-only, kexec is disabled, SysRq is
  off, coredumps are dropped, address-space randomization gets the
  maximum entropy, and the machine no longer answers ping
  (`icmp_echo_ignore_all`; `sysctl -w` brings it back for the session).
- `/usr/lib/bootc/kargs.d/05-kuma-hardening.toml` — kernel arguments:
  memory is zeroed on free (`init_on_free`, the one setting with a real
  performance cost, a few percent), page allocation is shuffled,
  legacy `vsyscall` and 32-bit `vdso` are off, module signature
  enforcement is on, the initramfs offers no shell and halts instead of
  an emergency prompt, `systemd`'s ssh keygen-on-boot is off, and the
  kernel does not trust the CPU's random number generator.
- `/etc/firewalld/zones/public.xml` — the firewall zone described above.
- `/etc/chrony.conf` — time comes from two independent vendors over
  NTS, an authenticated protocol, replacing the unsigned pool. Time
  matters more than it looks: certificate validation, log ordering, and
  kerberos expiry all depend on it.
- `/etc/NetworkManager/conf.d/kuma-mac.conf` — wifi gets a random MAC
  per network, but the same one every time you join. The access point
  sees a different address on each network; your router's DHCP
  reservation follows the new address once.
- `/etc/security/faillock.conf` — the lockout described above.

For completeness, what secureblue ships that kuma does not, and why: the
settings Fedora's kernel already defaults (like `slab_nomerge`);
`io_uring_disabled`, a real mitigation with a real list of things it
breaks; and `lockdown=confidentiality` plus `mitigations=auto,nosmt`.
The first would disable hibernation, which kuma sets up for. The second
halves your core count. Those are decisions for the machine's owner.

## What every image carries

A bootc image is expected to be self-contained: everything the system
runs is inside it. kuma's images also carry what a machine needs to
understand itself:

- The declaration it was built from, verbatim, at
  `/usr/lib/kuma/kuma.toml`. Comments and formatting intact.
- kuma itself, at `/usr/bin/kuma`.
- The units and helpers that converge flatpaks, brews, the declared
  user, and boot health.
- kuma's signing key, plus a policy naming it for kuma's published
  repository. This pair is in every image, not just published ones,
  because the machine that needs it is the one installed from published
  media, and that machine's `/etc` is whatever the image it installed
  from put there. The rule covers kuma's own repository only; requiring
  signatures everywhere would refuse Fedora's base and your own locally
  built image. `kuma doctor` checks the pair is present and working,
  because a signature nobody checks is a claim rather than a control.

That is the complete set for answering "what am I supposed to be" and
acting on it. A machine therefore needs neither a copy of your
declaration nor any tool you brought. `kuma update --yes` works on a
machine installed from an ISO that has never seen a `kuma.toml`, because
read-only commands fall back to the baked one. `kuma init` on such a
machine writes a copy true to that machine, not a generic starter.

Every image also carries FUSE 2, because AppImages need it. An AppImage
mounts itself as a filesystem before any of its own code runs, and
Fedora ships only FUSE 3. Without this, a downloaded file fails with
`dlopen(): error loading libfuse.so.2` on a machine that is otherwise
complete.

The lock file is deliberately not included. A `kuma.lock` belongs in git
next to the declaration it pins, not on every machine built from it.

Two things to know about the baked declaration. It is world-readable,
because the probe and `kuma init` both need it, so a `password_hash`
line in your declaration can be read by any local user. (The hash that
actually creates the account ships separately, mode 0600.) And it
records what the machine was built to be, not what it is now. Comparing
the two is what `kuma diff` does.

## Why a desktop installs things you did not name

Choosing a desktop installs packages you did not name. That set is
session infrastructure: the parts that have to exist for a session to
work at all. Applications are not in it, even convenient ones, because
the two are reversible in different ways. Delete a line from your
declaration and the next convergence uninstalls it. A package in a
desktop set has no opt-out, so putting one there decides for every user
of that desktop.

You can always add with `packages.rpm`. You cannot subtract from the
set.

Convergence removes exactly one thing you did not declare: a flatpak
**runtime** that no installed application needs any more. Applications
are never touched this way, only the shared platforms underneath them,
and reinstalling one is a download rather than a decision.
[What a desktop contains](desktops.md) lists both desktops in full.

## kuma in your launcher

Every desktop kuma builds puts kuma's own verbs in the launcher. Type
`kuma` into it and eight entries come back: edit the declaration, show
drift, review proposals, system health, check for updates, rebuild, roll
back, snapshots.

They are ordinary `.desktop` files in `/usr/share/applications`, so
whatever launcher finds your applications finds these too. No plugin,
nothing to configure. Each one opens a terminal, runs the verb, and
leaves the window open, because several ask for a password and all of
them print something worth reading. Press enter to close it.

`Edit Declaration` opens the declaration **this machine is actually
using** in `$EDITOR` (falling back to nano, vim, or vi). Which file that
is depends on where you run it: a `kuma.toml` in the current directory
outranks `~/.config/kuma/kuma.toml`. `kuma edit --print` says which one
resolved without opening anything.

No entry writes your declaration without asking.

## What a build records: kuma.lock

`base = "…/fedora-bootc:44"` names a tag, and tags move. One such move,
bootc 1.16.6 to 1.16.7 between two updates, is enough to break every
build that trusted the tag.

After your first build, a `kuma.lock` appears beside your declaration.
There is no verb to learn: `kuma build` reads it and refreshes it, and
`kuma update` is the only thing that moves the pin. Commit it.

The lock pins one thing and records another, on purpose:

- **The base digest is enforced.** Builds resolve `FROM name@sha256:…`
  from the lock, so the same declaration plus the same lock builds from
  the same bytes anywhere.
- **Package versions are recorded, not pinned.** Fedora's mirrors
  garbage-collect old builds within weeks, so a version pin would become
  a build failure unrelated to your declaration. The record exists to be
  diffed, and a lock can never break a build.

A composed base has no tag to distrust, so the lock holds its
content-addressed reference instead. A changed manifest then shows up as
a changed base, the same as editing `system.base`. The pin holds while
that image is still in storage; once it is gone, kuma says so and
composes a fresh one.

That makes an update legible. `git diff kuma.lock` is the whole story:

```console
$ kuma update
base  sha256:9f3ca81b2e4d -> sha256:a71b04ef9c33
      bootc 1.16.6-1.fc44.x86_64 -> 1.16.7-1.fc44.x86_64
      ... and 34 more changed
rpm   36 changed, 2 added

$ kuma update --check
The base is composed locally from Fedora's repos (localhost/kuma-base:m26ccdd18fd07).
20 packages have moved in the repos since this machine booted its image.
      kernel 7.1.7-200.fc44.x86_64 -> 7.1.8-200.fc44.x86_64 (important)
      sqlite-libs 3.51.2-1.fc44.x86_64 -> 3.51.2-2.fc44.x86_64 (important)
      linux-firmware 20260622-1.fc44.noarch -> 20260810-1.fc44.noarch (moderate)
      ripgrep 14.1.1-4.fc44.x86_64 -> 15.2.0-1.fc44.x86_64
      kitty 0.45.1-1.fc44.x86_64 -> 0.45.2-1.fc44.x86_64
rpm   20 moved, 16 with security advisories (5 important, 11 moderate)
```

For a named base, `--check` is one registry query and reports only
whether the base moved. For a composed base, every package is in play,
so the check asks dnf and sorts worst advisory first.

It answers about the running machine when there is one, so it does not
care how kuma was installed and needs no image in podman storage. Repo
metadata is cached under `~/.cache/kuma/dnf`: the first run takes about
half a minute, the rest take seconds. No root either way.

One honest caveat: `--check` is a prediction. dnf reports what it would
upgrade, but a recompose runs its own dependency solve, which can add or
drop packages an upgrade query never sees. The lock diff afterwards is
the record of what actually happened.

## Why a file you edited by hand keeps winning

On an ostree system, every difference between your `/etc` and the
image's defaults in `/usr/etc` is treated as a local modification, and
carried onto every future deployment. Edit a file by hand and it keeps
winning, silently, no matter what later images ship.

That is how ostree is designed to work, and it is a trap. You fix
something by editing `/etc` directly. Later you fix the same thing
properly in the declaration. The hand-edited copy keeps overriding it,
the declared version never gets tested, and nothing tells you why.

`kuma doctor` watches the files your image owns:

```console
ok    etc: 14 files this image owns in /etc, none shadowed locally

warn  etc: local edits shadow the image: /etc/environment. These win over
      every future image, so the declared version never applies
      → sudo cp /usr/etc/environment /etc/environment
```

The cure is `cp`, not `rm`: a deletion is itself a local modification
and carries forward as one.

The same merge has a second edge, and it decides where machine state
can live. A file the image ships lands in `/usr/etc`, not `/etc`. If a
later image drops that file, the merge drops it from `/etc` too.
Anything you expect to survive updates therefore cannot arrive as image
content in `/etc`.

That is why `kuma install` writes the account it asks for to
`/var/lib/kuma/user` and not `/etc/kuma/user`. bootc fills `/var` from
the image once, at install, and never touches it again, which is
exactly what install-time answers need. The hostname has the same
problem and the opposite cure: `/etc/hostname` *is* image content, so
the installed machine writes it at first boot from
`/var/lib/kuma/hostname`, and writing it is what makes it a local
modification, which is what survives.

So the rule has three parts: `/usr` is the image, `/etc` is machine
state the image also has an opinion about, and `/var` is machine state
it does not.

There is deliberately no `kuma capture` for this. Package drift is
yours to keep because a package is your choice. `/etc` content is
kuma's curation, so an edit worth keeping belongs in the image itself.

## Permissions, and a file kuma does not own

A flatpak's permissions are machine state that outlives every rebuild.
They live in override files under `/var/lib/flatpak/overrides` and
`~/.local/share/flatpak/overrides`, and they survive image updates the
way everything in `/var` does. Before they were declarable, a machine
could carry a permission nobody could find again.

`[overrides]` declares them, per app:

```toml
[overrides."org.mozilla.firefox"]
filesystems = ["home", "!xdg-config/kitty"]
sockets = ["wayland"]
environment = { MOZ_ENABLE_WAYLAND = "1" }
```

The shape is flatpak's own override file, so `flatpak override --show`
reads back into it, and a `!` in front of a permission takes it away.

**kuma owns keys, not files.** Flatseal writes the same files, and so
does anyone running `flatpak override` by hand. Convergence therefore
sets the keys you declared, removes the keys it set that you stopped
declaring, and copies every other line through untouched. A key kuma
never set is never kuma's to delete, even when it sits in the same file
and contradicts what you declared. Declaring is how you win that
argument; `kuma diff` is how you find out you are having it.

That makes Flatseal the editor and your declaration the record, and they
meet at `kuma capture`:

```console
$ kuma capture
Would declare in ~/.config/kuma/kuma.toml:
  + org.chromium.Chromium  [overrides] user  filesystems
```

Capture offers permissions only for apps your declaration already
installs. Machines accumulate override files for software that left
years ago, and proposing those would be proposing rubble.

Declared permissions are restored at boot and when you run `kuma sync`,
and never by the daily timer. An app arriving at a random hour is
harmless. A permission reverting at a random hour changes what a
running program can reach, and a toggle that silently flips back
tomorrow afternoon is indistinguishable from a bug. The rule: boot
restores what you declared, and the session in between is yours to
experiment in.

Flatpak keeps two override stores, and kuma writes both. `scope =
"user"` writes the per-user one, applied by a `systemd --user` unit
rather than by root reaching into a home. One app declares into one
store: flatpak merges the two with the user store winning per key, so
an app declared in both would be a file where half your lines quietly
lose to the other half.

## Hibernation

A machine with a swapfile can hibernate: memory is written to the file
and the machine powers off, and the next boot resumes where you were.
If you did not ask for hibernate at install time, `kuma hibernate` sets
it up:

```console
$ kuma hibernate              # what it would make, and where
$ kuma hibernate --yes        # make it; takes effect on the next boot
$ kuma hibernate --off --yes  # take it away again
```

The file defaults to the size of your memory, which is the most a
machine can ever need to save. It is never resized in place: growing it
would move it, and the kernel would then resume from the wrong place on
the disk. To change the size, turn it off and on again.

Setting hibernate up also points the lid at suspend-then-hibernate: a
laptop closed in a bag suspends first, then hibernates on the
firmware's low-battery alarm, before the battery dies.

Why this cannot live in the declaration: the kernel has to be told
where the swapfile physically sits on the disk, as a block offset, and
that number describes one file on one disk. Two machines built from the
same declaration would need different values, and a machine given the
wrong one would hibernate, power off, and boot fresh with the session
gone. So the size is asked at install, and `kuma hibernate` asks on a
running machine. `kuma doctor` compares the offset the kernel was given
against the one the file actually has, because that is the only way
this breaks quietly.

Two limits. First, Secure Boot and hibernate do not go together: a
kernel booted with Secure Boot on runs locked down, and a locked-down
kernel refuses to hibernate, because a hibernate image is a way to
write arbitrary memory back into a running kernel. `kuma doctor` warns
rather than calling the machine ready. Second, hibernate from the
desktop, not over ssh: logind gates it on an active session, and an
ssh login is not one. That is systemd's rule, not kuma's.

## Backups, and the two things a restore needs

`[snapshots]` answers a mistake. It cannot answer a dead disk, because
the copies are on the disk. `[backup]` sends them somewhere else, to a
restic repository:

```toml
[snapshots]
enable = true

[backup]
enable = true
repo = "s3:https://minio.example:9000/kuma"
secret = "backup"
```

**Backups copy from a snapshot, never from the live subvolume.** That
is why enabling backups requires enabling snapshots, and why `kuma
check` says so rather than letting you find out at 3am. A backup taken
while files are being written is a backup of a moment that never
existed.

**The credential is named here and held on the machine**, at
`/var/lib/kuma/secrets/<secret>.env`, mode 0600, put there by hand. A
declaration is written to be committed, and gets baked world-readable
into every image built from it. That is the wrong place for a secret,
and the same boundary that keeps `password_hash` out of `kuma
capture`.

That is not a hole in the file. Naming the credential keeps the
declaration complete: the fact that one exists and what it is called
are both declared, and only the value is elsewhere. The practical
consequence: **restoring a machine needs two things**, this file and
that credential. A repository address carrying its own password is
refused, because nobody decides to put a secret in git; they paste a
restic command line that already worked.

**What the far end can see.** restic encrypts everything on your
machine before sending it. The repository holds ciphertext its operator
cannot read. What they can see is the shape of your traffic: how much
you store, how often, and when.

Excludes are additive on top of a set you cannot remove: `linuxbrew`,
`~/.cache`, `~/.local/share/containers`. Every one is a tree this same
declaration rebuilds.

One thing outside home matters more than anything inside it, and it is
**off by default**:

```toml
network_connections = true   # carries /etc/NetworkManager/system-connections
```

Those files hold a wifi password per network, in the clear, and nothing
else can recreate them: not this declaration (they are out of scope),
not the image (it ships the directory empty), not home (they live in
`/etc`). Restore without them and every network password gets retyped.
They are off by default because turning them on moves secrets off the
machine, and that should be your decision. `kuma doctor` names which
way it is set on every run.

The first copy is your whole home rather than a day's difference, so it
is a command rather than something a timer starts:

```console
$ sudo kuma backup --init
```

**The failure this feature actually has is silence.** The unit exits
cleanly with no credential, no snapshot, or no repository, all three on
purpose, so "last run succeeded" would be true of a machine that never
copied a byte. Only a run that copied something writes the stamp, and
`kuma doctor` checks the stamp, not the unit. How stale is too stale
follows the interval you declared, so a monthly policy is not called
unhealthy on day eight.

### Getting a machine back

A backup nobody has restored is a claim. This is the other half, and it
is the reason the feature exists.

Boot the installer media and point an install at the repository:

```console
$ sudo kuma install --disk /dev/nvme0n1 --restore recovery.env
```

`recovery.env` is one file holding the repository address and its
credentials. It carries `RESTIC_REPOSITORY` as well as the keys,
because the machine being restored has no declaration yet. Put it on
the stick beside the ISO and a dead disk needs nothing else typed. The
install refuses to touch the disk if it cannot open the repository,
since the old machine may still be the only copy.

**Write the values as plain text.** A value containing `$`, a backtick,
a quote, or a backslash is refused, by `kuma install --restore` and by
`kuma backup` alike. Two things read this file, the first boot through
systemd and the verb through a shell loop, and they disagree about what
those characters mean. A password containing one would be a different
password depending on which reader opened the repository. kuma will not
guess which you meant. If a repository predates kuma 0.17 and its
password contains one of those characters, change it with `restic
passwd` before rewriting the file: it was encrypted with the expanded
value.

**The install does not restore anything itself.** It writes the request
and the credential onto the new machine. The first boot does the work,
after the unit that gives `/var/home` its own subvolume has run and
after your account exists. That ordering is forced: `/var/home` does
not exist at install time at all.

If the repository cannot be reached on that first boot, the request
stays and the next boot tries again. A bad day costs a retry, not the
data.

Two things to know before you need this. The image you install must
have been built from a declaration with `[backup]`, since that is what
carries restic and the restore unit; installing your own image is the
normal case. And restoring needs **two** things: this file and the
credential it names.

## What your machine trusts

Every TLS connection the machine makes is checked against a set of
certificate authorities. Adding to that set is one of the most
consequential things a declaration can do, so it has a section:

```toml
[system.ca_certificates]
"my-root-ca" = """
-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJAKZ...
-----END CERTIFICATE-----
"""
```

**The certificate goes in the file rather than beside it.** A
declaration that points at a path elsewhere is not one file any more:
carry it to a second machine and the anchor is missing, with nothing to
say so.

Inlining is safe here for a reason that does not generalise. **A CA
certificate is public by construction.** A private key is not, so one
pasted into this table is refused, not warned about: it would otherwise
be baked world-readable into every image built from the file and pushed
to a registry. A value that is not a certificate at all is refused by
`kuma check` for the ordinary reason.

The anchor is copied to `/etc/pki/ca-trust/source/anchors/`, and
`update-ca-trust` runs in the same build layer that adds it, rather
than being left for a boot to remember. Landing under `/etc` also puts
it inside the `etc` check described above, so a local edit that shadows
it gets reported.

## What a declaration does not reproduce

A machine built from your declaration carries state the file never
mentions. Some of it deliberately so. Everything below survives an
image update and is outside the declaration on purpose:

**Your data.** `/var/home` is yours. A declaration rebuilds a system,
not a home. `[snapshots]` covers a mistake; `[backup]` covers a dead
disk.

**Machine identity.** SSH host keys, the machine ID, the disk layout in
`fstab` and `crypttab`. These are what make this machine this machine.
Copying them onto a second one would be a bug.

**Secrets.** Network connections live in
`/etc/NetworkManager/system-connections` and hold the wifi password in
the file. A declaration is baked world-readable into every image built
from it, which is why secrets stay out.

**Per-app and per-desktop state.** dconf and GSettings, the portal permission
store under `~/.local/share/flatpak/db` (the location and notification
prompts you answered), device pairings, and units you enabled in your own
`systemd --user` manager. `[services]` is system scope only.

**Everything else in `/etc` the image never shipped.** `kuma doctor`
watches the files this image owns and says nothing about the rest. A
real machine carries dozens of legitimately local files, and a check
that lists them all is one you learn to scroll past. Two shapes are
always wrong rather than merely undeclared, and doctor reports both: a
flatpak override pointing at nothing, and an enabled unit with no unit
file behind it.None of this is a to-do list. Some of it is a boundary that will not
move (identity, secrets). Some of it is a decision that could (dconf,
the portal store). None of it is an oversight.

## What an install decides that a declaration cannot

Everything above is about images. An install is where an image meets a
machine, and the two know different things.

An image knows what should be installed. It cannot know who the machine
is for: a published image is pulled by strangers, so it declares no
`[user]`, and a machine installed from one would have no account and no
way in. So `kuma install` asks, writes the answers to
`/var/lib/kuma/user` on the target, and `kuma-user-sync` creates the
account at first boot, exactly as it does for a declared one. The
installer creates nobody; it writes down what the machine should
converge to. If the image does declare a `[user]` and you ask for a
different one, the installer also drops the declared account's autologin
from the greeter, since a greeter cannot log in a user the machine will
not have.

That split decides where things live, for the reason in [why a file you
edited by hand keeps winning](#why-a-file-you-edited-by-hand-keeps-winning):
`/var` is filled from the image once and never touched again, which is
what install-time answers need. A file the installer shipped into `/etc`
would be image content, and the next update would delete it.

The disk itself is machine state. kuma writes the same three partitions
every time: an EFI system partition, a `/boot` outside the root, and a
root that takes the rest. `/boot` is separate even when nothing is
encrypted, so that turning encryption on changes what the third
partition holds, not the shape of the disk. The sizes of the first two
are asked at install, with defaults shown, because they cannot be
revised without installing again. The shape is not asked, because a
machine built from one image should not have to know how to boot four
kinds of one. Whether the root holds a LUKS container is asked at
install too, for the same reason.

**What encryption protects is a disk at rest, and only that.** `/boot`
and the EFI partition sit outside the container on every install,
because a bootloader has to read a kernel before anything is unlocked.
Nothing measures or verifies them, so an attacker with repeated
physical access can modify the initramfs that later asks for your
passphrase. Defending against that needs Secure Boot with measured
boot, which kuma does not do. Encryption answers a stolen or discarded
disk, not a machine somebody keeps visiting.

**The passphrase is handled like the machine's most consequential
secret, because it is.** It reaches `cryptsetup` on a pipe, never a
command line where `ps` would show it, and never a file. It does live
in memory while the install runs, and kuma makes no claim to defend
against something reading another process's memory, which on this
machine already means root. kuma writes it nowhere. There is no
recovery key and no escrow: a lost passphrase is a lost disk. Changing
it later is `cryptsetup luksChangeKey` on the machine itself.

The account password gets the same handling: prompted at a terminal or
read as one line from a pipe, never passed as an argument, so it does
not reach `ps` or shell history. Its hash is written to
`/var/lib/kuma/user` on the target, mode 0600, where `kuma-user-sync`
reads it at first boot. Unlike a declared `[user]`, it is written to
the machine rather than baked into the image, so publishing the image
does not publish it.

A first boot then spends a few minutes on the rest: the account is
made, the hostname applied, and the declared flatpaks and brews
downloaded, about a gigabyte for a full desktop. kuma says so while it
happens: bare `kuma` reports `converging` rather than drift, and offers
no sync, because a sync is what is already running. A machine that does
not match its declaration yet is not a machine that has stopped trying.

None of this is in `kuma.toml`, and none of it should be. Two machines
installed from one declaration can have different disks, different
names, and different people. The declaration says what the system is;
the install says whose it is.

## The nostr layer

The `[nostr]` layer runs a bunker: a small daemon that holds a nostr
signing key and signs for the apps you pair with it, with every request
governed by a policy engine and every decision logged. It has its own
page: [the nostr layer](nostr.md) covers setup, the trust levels, where
the key lives, and what a relay operator can and cannot see.
