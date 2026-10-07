# How kuma behaves

This explains the reasoning behind what kuma does, for when the behaviour is
surprising or you want to know what you are trusting. If you are looking for
what to type, [getting started](getting-started.md) walks the path instead,
and [the glossary](glossary.md) defines the vocabulary.

- [What happens to changes you make by hand](#what-happens-to-changes-you-make-by-hand)
- [Where the base system comes from](#where-the-base-system-comes-from)
- [What every image carries](#what-every-image-carries)
- [kuma in your launcher](#kuma-in-your-launcher)
- [Why a desktop installs things you did not name](#why-a-desktop-installs-things-you-did-not-name)
- [What a build records: kuma.lock](#what-a-build-records-kumalock)
- [What updates itself, and what waits for you](#what-updates-itself-and-what-waits-for-you)
- [Why a file you edited by hand keeps winning](#why-a-file-you-edited-by-hand-keeps-winning)
- [Permissions, and a file kuma does not own](#permissions-and-a-file-kuma-does-not-own)
- [Backups, and the two things a restore needs](#backups-and-the-two-things-a-restore-needs)
- [The nostr layer: a bunker, and who may ask it](#the-nostr-layer-a-bunker-and-who-may-ask-it)
  - [Getting a machine back](#getting-a-machine-back)
- [What your machine trusts](#what-your-machine-trusts)
- [What a declaration does not reproduce](#what-a-declaration-does-not-reproduce)
- [Boot health and automatic rollback](#boot-health-and-automatic-rollback)
- [What an install decides that a declaration cannot](#what-an-install-decides-that-a-declaration-cannot)

## What happens to changes you make by hand

Drift is a fork, not an error.

Declarative systems normally treat drift as failure: the machine deviates,
the tool corrects it, the deviation is erased. That is why the thing you
installed in a hurry never makes it into the declaration.

kumaOS gives drift a second exit. Anything the machine has that `kuma.toml`
doesn't name is a proposal against your declaration:

```console
$ kuma diff
packages.flatpak
  - org.gnome.Boxes  installed, not declared (convergence removes it)
Ad-hoc flatpaks, kept as yours: io.github.kolunmi.Bazaar

  → kuma capture   keep them: declare what this machine already runs
  → kuma sync      converge now; otherwise the boot/daily run picks this up
```

Convergence takes back only what it installed. Boxes above was declared once
and no longer is, so it is on the removal list; Bazaar you installed
yourself, so it is undeclared but in no danger. Install applications from a
store if you like: being undeclared costs reproducibility, never survival.

Membership and currency are separate questions, and only the first belongs
to the declaration. It decides what exists. Keeping software current is
kuma's job regardless of who installed it, so a store-installed app, an
ad-hoc `brew install`, and a flatpak runtime are updated on the same
schedule as a declared app. Nothing is left to rot for the crime of not
being written down. Each ecosystem's own hold still works if you want to
stay on a version: `flatpak mask` and `brew pin`.

`kuma capture` prints the proposal and writes nothing until `--yes`; naming
items captures only those. You review a diff of your *declaration*, not of
your system. Experiment imperatively, promote deliberately.

Capture never touches the machine, only the file, so a dry run is as safe as
`kuma diff`. It takes flatpaks and brew leaves, which are the whole mutable
edge. It will not take rpms, because a bootc machine can't install one
imperatively and `[packages].rpm` is already declarative. It takes a
`flatpak --user` install only when you name it, since declaring one makes it
system-wide. And it never touches `[user]` or `[system]`: a password hash and
machine state must not walk into a file you commit.

Snapshots follow the same rule. `kuma snapshot --restore <path>` is a dry run
that names which snapshot the path would come back from and whether a copy on
the machine gets replaced; `--yes` does it. It restores a path, never a whole
subvolume: swapping what `/var/home` *is* while processes hold files open in
it is a reboot-shaped operation, and the accident people actually have is one
file.

## Where the base system comes from

The usual way to build a bootc image is to start `FROM` a general-purpose
base image, then remove what you didn't want. With no `system.base` in the
declaration, kuma instead composes its own with `rpm-ostree compose image`,
the same tool and building blocks Fedora uses to build fedora-bootc.

The compose starts from Fedora's minimal bootc manifest, whose summary is
"effectively just bootc, systemd, kernel, and dnf as a starting point", and
adds what a real machine needs. What fedora-bootc carries for the general
case is never included rather than removed afterward. Fedora stays the
package source: kuma builds no packages and no kernels, and every version
comes from Fedora's repos at compose time.

The composed image is content-addressed, meaning its name is derived from
what is in it. The tag embeds a hash of the manifest that produced it, so a
build can name its base before any compose has run, an unchanged manifest
reuses the image already in storage, and a changed manifest cannot reuse a
stale one.

Two consequences:

- **`system.firmware` is the trim.** Unset, the base ships every vendor's
  firmware, so a machine that declares nothing about its hardware still boots
  with working graphics, wifi, and audio. Name what your hardware needs and
  the rest stays out of the image.
- **`kuma update --check` asks the repos, not a tag.** A composed base has no
  upstream tag whose movement can be checked, and every package in it is in
  play, because an update recomposes the whole thing. So the check asks dnf
  what has a newer version in the repos and which of those carry security
  advisories. Seconds, and it builds nothing.

Naming a `base` opts out of all of it: any bootc image can be one.

**The base runs sshd, and the firewall answers for it.** `openssh-server` is
composed in, and the image enables `sshd.service` by name. The unit is a
curated default rather than an inherited one because `kuma vm` and the boot
stage of the smoke tests both reach a guest over ssh — an image that could
not be reached that way would take the test harness with it. Where the
world's route in used to live — firewalld's default zone, which permitted
ssh outright — the hardening floor has closed the door: the shipped public
zone stops serving `ssh`, and what remains is a rich rule admitting ssh
from 10.0.2.2 alone, qemu's user-mode gateway, the address the test lanes
arrive from. The price is named in the zone description: a network
numbered 10.0.2.0/24 would reach sshd too.

Authentication is Fedora's default, which means passwords work, and the
account `kuma install` creates is in `wheel`. The prompt is no longer
something a stranger on the coffee-shop wifi can reach, but the floor does
not pretend the unit is off: an owner who wants to reach the machine over
the network serves ssh deliberately, with `firewall-cmd --permanent
--zone=public --add-service=ssh`. Online guessing is capped rather than
merely rate-limited — the floor wires `pam_faillock` into the login
stacks, and 50 failed attempts lock the account for a day;
`faillock --user <name> --reset` clears it. If you declare
`[user].ssh_keys`, kuma serves them from `/etc/kuma/keys/<name>` alongside
the user's own `~/.ssh/authorized_keys` and never overwrites it. To
require keys, drop a conf into `/etc/ssh/sshd_config.d/`; `/etc` is merged
rather than replaced, so it survives image updates, and `kuma doctor` will
report it as a local modification because it is one.

`[services].disable = ["sshd.service"]` turns it off on a machine that
doesn't want it. It is a default, not part of kuma's floor: the image enables
it above your `[services]` block, so your declaration wins, the way it does
for anything a desktop enables. Boot health, rollback, and the hardening
floor sit below that line and cannot be switched off. Disable sshd and
`kuma vm` still builds a disk, but nothing will be able to ssh into it.

**The hardening floor.** Below the `[services]` line sits a set no
declaration can switch off, taken from secureblue's audited hardening and
adapted to what kuma's gate can run. Six files ship in every image:

- `/usr/lib/sysctl.d/70-kuma-hardening.conf` — ptrace restricted to
  processes you launched (`kernel.yama.ptrace_scope = 1`), kernel pointers
  gone from `/proc`, perf root-only, kexec disabled, SysRq off, coredumps
  dropped, ASLR entropy at the arch maximum, and the TCP/ICMP set —
  including `icmp_echo_ignore_all`, which is why a debugging session
  cannot ping the machine (`sysctl -w` brings it back for the session).
- `/usr/lib/bootc/kargs.d/05-kuma-hardening.toml` — `init_on_free` (the
  one karg with a measurable cost, a few percent on allocation-heavy
  work), `page_alloc.shuffle`, `vsyscall=none`, `vdso32=0`,
  `module.sig_enforce` (every module in the image is Fedora's own),
  `rd.shell=0` with `rd.emergency=halt` (no initramfs shell on a machine
  whose disk is encrypted), `systemd.ssh_auto=no`, and
  `random.trust_cpu=off`.
- `/etc/firewalld/zones/public.xml` — the zone described above.
- `/etc/chrony.conf` — NTS-authenticated time from two independent
  vendors, replacing the unsigned pool. Time is a root CA revocation,
  log ordering and kerberos expiry; an on-path attacker between the
  machine and a pool could feed it.
- `/etc/NetworkManager/conf.d/kuma-mac.conf` — a random but stable MAC
  per Wi-Fi connection: the access point sees a different address on
  each network the machine joins and the same one every time on each.
  Router-side DHCP reservations follow the new address once.
- `/etc/security/faillock.conf` — the lockout described above.

What secureblue ships that the floor does not, and why: the sysctls and
kargs Fedora's kernel already defaults (`init_on_alloc`, `slab_nomerge`,
`randomize_kstack_offset`); `io_uring_disabled`, a real mitigation with a
real breakage list; and `lockdown=confidentiality` with
`mitigations=auto,nosmt` — the first disables hibernation, which the
image's `resume=` kargs are set up for, and the second halves the cores.
Both are decisions, and the floor does not make an owner's decisions.

## What every image carries

A bootc image is expected to be self-contained: everything the system runs is
in it. kumaOS's images also carry what the machine needs to reason about
itself, which is three things.

The declaration it was built from, verbatim at `/usr/lib/kuma/kuma.toml`,
comments and formatting intact. kuma itself, at `/usr/bin/kuma`. And the
units and helpers that converge flatpaks, brews, the declared user, and boot
health.

They also carry what the machine needs to refuse an update: kuma's signing
key, and a policy naming it for kuma's published repository. That pair is in
every image rather than only in published ones, because the machine that
needs it is the one installed from published media, and that machine's `/etc`
is whatever the image it was installed from put there. The rule covers kuma's
own repository alone; requiring signatures everywhere would refuse Fedora's
base on your next update and your own locally built image on your next
switch. `kuma doctor` grades the pair, since a signature nobody checks is a
claim rather than a control.

That is the complete set needed to answer "what am I supposed to be" and then
act on it, which means a machine needs neither a working copy of your
declaration nor a tool you brought with you. `kuma update --yes` works on a
machine installed from an ISO that has never had a `kuma.toml` anywhere,
because read-only commands fall back to the baked one. `kuma init` on such a
machine seeds a copy true to that machine rather than a generic starter.

Every image also carries FUSE 2, which is what makes an AppImage run by being
executable. An AppImage is a squashfs its runtime mounts before any of its own
code runs, and Fedora ships only FUSE 3, so without this a file you downloaded
fails at `dlopen(): error loading libfuse.so.2` on a machine that is otherwise
complete. It is two small packages, one for the library and one for the setuid
helper it mounts with, and they are not gated on a desktop: needing to edit a
declaration before a downloaded file will run is a poor way to find out.

The lock is deliberately not included. A `kuma.lock` belongs in git next to
the declaration it pins, not on every machine built from it.

Two things worth knowing about the baked declaration. It is world-readable,
because the probe and `kuma init` both need it, so a `password_hash` line in
your declaration is readable by any local user; the hash that actually
creates the account ships separately at 0600. And it records what the machine
was built to be, not what it is now. Comparing the two is what `kuma diff`
does, and what the machine has that the file does not name is a fork rather
than a fault.

## kuma in your launcher

Every desktop kuma builds puts kuma's own verbs in whatever launcher the
session shipped. Type `kuma` into it and eight entries come back: edit the
declaration, show drift, review proposals, system health, check for updates,
rebuild, roll back, snapshots.

They are ordinary `.desktop` files in `/usr/share/applications`, so the
launcher that finds your applications finds these the same way, with no plugin
to install and nothing to configure. That is the point: a desktop entry is the
one integration every desktop already has, so these are on the COSMIC desktop
as well as the niri one, and a desktop kuma has not built yet would get them
too.

Each opens a terminal window, runs the verb and leaves the window open
afterwards, because several of them ask for a password and all of them print
something worth reading. Press enter to close it.

`Edit Declaration` runs `kuma edit`, which opens the declaration **this machine
is actually using** in `$EDITOR`, falling back to nano, vim or vi. Which file
that is depends on where you run it from: a `kuma.toml` in the current
directory outranks `~/.config/kuma/kuma.toml`. `kuma edit --print` says which
one it resolved without opening anything.

No entry writes your declaration without asking. That is the same line drawn in
[what happens to changes you make by hand](#what-happens-to-changes-you-make-by-hand):
`Review proposals` runs `kuma capture`, which prints the proposal and waits, and
`Rebuild` writes an image rather than the file.

## Why a desktop installs things you did not name

Choosing a desktop installs packages you did not name. That set is session
infrastructure: the parts that have to exist for a session to function.
Applications are not in it, even convenient ones, because the two are
reversible in different ways. Delete a line from your declaration and the
next convergence uninstalls it; a package in a desktop set has no opt-out, so
putting one there is a decision you make on behalf of everyone.

You can always add with `packages.rpm`. You cannot subtract from the set.

One thing convergence removes that you did not declare: a Flatpak **runtime**
that no installed application needs any more. Applications are never touched
that way, only the shared platforms underneath them, and reinstalling one is a
download rather than a decision. It is the one place convergence reaches past
what it installed, and it is deliberate.
[What a desktop contains](desktops.md) lists both arms, explains the
non-obvious members, and says where that limit bites.

## What a build records: kuma.lock

`base = "…/fedora-bootc:44"` names a tag, and tags move. One such move, bootc
1.16.6 to 1.16.7 between two updates, is enough to break every build that
trusted the tag.

`kuma.lock` appears beside your declaration after the first build. There is
no verb to learn: `kuma build` reads it and refreshes it, and `kuma update`
is the one thing that moves the pin. Commit it.

What it pins and what it merely records is a deliberate split:

- **The base digest is enforced.** Builds resolve `FROM name@sha256:…` from
  the lock, so the same declaration plus the same lock builds from the same
  bytes anywhere.
- **Package versions are recorded, not pinned.** Fedora's mirrors garbage
  collect old builds within weeks, so a version pin becomes a build failure
  that has nothing to do with your declaration. The record exists to be
  diffed, which needs no enforcement, so a lock can never break a build.

A composed base has no tag to distrust, so the lock holds its
content-addressed reference instead. A changed manifest then reads as a
changed base, as an edited `system.base` would. The pin holds while that
image is still in storage; once it isn't, kuma says so and composes a fresh
one.

That makes an update legible, and `git diff kuma.lock` is the full story:

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

For a named base, `--check` is one registry query and reports only whether
the base moved: a rebuild layers with `dnf install` rather than upgrading, so
the base's own packages cannot move underneath you. A composed base has no
tag to ask about and every package in play, so the check asks dnf instead,
worst advisory first.

It asks the running machine when there is one, which means it does not care
how kuma got installed and needs no image in podman storage; a host that
isn't a kuma machine gets asked about the image it builds, and the output
says which answered. Repo metadata is cached under `~/.cache/kuma/dnf`, so
the first run takes about half a minute and the rest take seconds. No root
either way.

It is a prediction. dnf reports what it would upgrade; a recompose runs its
own depsolve, which can also add or drop packages an upgrade query never
sees. The lock diff afterwards is the record of what happened.

## What updates itself, and what waits for you

Flatpaks and brew formulae converge at boot: what the declaration names
gets installed, and what convergence installed but the declaration no
longer names gets removed — and when the machine already matches its
declaration, the run touches nothing at all. Keeping what is already
there current is the daily timer's question, the same schedule for a
declared app and an ad-hoc one, behind the gate that waits for power and
an unmetered line. A person who wants an application updated this minute
runs the update by hand; that is drift by choice, and the machine's
answer to it is `kuma capture`, not an undo.

The image is the opposite on every count. An update replaces the entire
operating system at once and applies on the next boot, so putting it on a
timer buys either surprise reboots or a queue of staged deployments nobody
has booted while the machine reports itself up to date. kumaOS stages nothing
you did not ask for, and `kuma update` stays yours to run.

What is automated is knowing when to run it:

- `kuma update --check` asks the repos what has moved since this image was
  built, security advisories first. Seconds, and it builds nothing.
- `kuma doctor` reports how old the booted image is and warns past a month,
  which on Fedora means at least one kernel you did not take.

```console
warn  deployment: booted image is 41 days old
      → kuma update   recompose against the repos' current packages and rebuild
```

Neither one applies anything. The machine watches the clock; you decide when
to reboot into a new one.

One change is loud enough to get its own sentence. If an update would move
the machine to a new Fedora release, `kuma update` says so in those words
after the diff and before anything is staged:

```console
This is a Fedora release change: 44 to 45.
```

That is the largest thing kuma can do to a machine, and in the package diff
it otherwise looks like several hundred ordinary lines. `kuma update --check`
tells you which release you are on but does not predict the target: for a
composed base the answer is only knowable by composing, and a guess is worth
less than a fact.

## Why a file you edited by hand keeps winning

On an ostree system, every difference between your `/etc` and the image's
defaults in `/usr/etc` is treated as a local modification and carried onto
every future deployment. A file you edit by hand keeps winning, silently, no
matter what later images ship.

That is working as designed, and it is a trap. You fix something by editing
`/etc` directly, later declare the same fix properly, and the hand-edited
copy goes on overriding it. The declared version can never be tested, and
nothing tells you why.

`kuma doctor` watches the files your image owns:

```console
ok    etc: 14 files this image owns in /etc, none shadowed locally

warn  etc: local edits shadow the image: /etc/environment. These win over
      every future image, so the declared version never applies
      → sudo cp /usr/etc/environment /etc/environment
```

The cure is `cp`, not `rm`: a deletion is itself a local modification and
carries forward as one.

The same merge has a second edge, and it decides where machine state can
live. A file that an image ships lands in `/usr/etc`, so it is not a local
modification. If a later image drops that file, the merge drops it from
`/etc` too. Anything written once and expected to outlive updates therefore
cannot arrive as image content in `/etc`.

That is why `kuma install` writes the account it asks for to
`/var/lib/kuma/user` rather than `/etc/kuma/user`. bootc fills `/var` from the
image once, at install, and never touches it again, which is exactly what
install-time answers need. The hostname has the same problem and the opposite
cure: `/etc/hostname` *is* image content, so the installed machine writes it
at first boot from `/var/lib/kuma/hostname`, and writing it is what makes it a
local modification and therefore what survives.

So the rule has three parts, not two: `/usr` is the image, `/etc` is machine
state the image also has an opinion about, and `/var` is machine state it
does not.

There is deliberately no `kuma capture` for this. Package drift is a fork
because a package is your choice; `/etc` content is kuma's curation, so an
edit worth keeping belongs in the image rather than in your declaration. That
is how the display fix that motivated this check got resolved: the workaround
stopped being a local edit and became something kuma bakes.

## Permissions, and a file kuma does not own

A flatpak's permissions are machine state that outlives every rebuild. They
live in override files under `/var/lib/flatpak/overrides` and
`~/.local/share/flatpak/overrides`, they survive image updates the way
everything in `/var` does, and until they were declarable a machine could
carry a permission nobody could find again.

`[overrides]` declares them, per app:

```toml
[overrides."org.mozilla.firefox"]
filesystems = ["home", "!xdg-config/kitty"]
sockets = ["wayland"]
environment = { MOZ_ENABLE_WAYLAND = "1" }
```

The shape is flatpak's own override file, so `flatpak override --show` reads
back into it, and `!` in front of a permission takes it away.

**kuma owns keys, not files.** Flatseal writes the same files, and so does
anyone running `flatpak override` by hand, so convergence sets the keys you
declared, removes the keys it set that you stopped declaring, and copies every
other line through untouched. A key kuma never set is never kuma's to delete,
even when it sits in the same file and contradicts what you declared.
Declaring is how you win that argument; `kuma diff` is how you see you are
having it.

That makes Flatseal the editor and your declaration the record, and they meet
at `kuma capture`:

```console
$ kuma capture
Would declare in ~/.config/kuma/kuma.toml:
  + org.chromium.Chromium  [overrides] user  filesystems
```

Capture offers permissions only for apps your declaration already installs. A
machine accumulates override files for software that left years ago, and
proposing those would be proposing rubble.

**Permissions converge at boot and when you run `kuma sync`, and never on the
daily timer that carries installs.** An app arriving at a random hour is
harmless. A permission reverting at a random hour changes what a running
program can reach, and a toggle that silently flips back tomorrow afternoon is
indistinguishable from a bug. The rule is one sentence: declared permissions
are restored when you boot, and the session in between is yours to experiment
in.

Flatpak keeps two stores and kuma writes both. `scope = "user"` writes the
per-user one, applied by a `systemd --user` unit rather than by root reaching
into a home. One app declares into one store: flatpak merges the two with the
user store winning per key, so an app declared in both would be a file where
half your lines quietly lose to the other half.

## Backups, and the two things a restore needs

`[snapshots]` answers a mistake. It cannot answer a disk, because the copies
are on the disk. `[backup]` sends them somewhere else, to a restic repository
spelled the way restic spells one:

```toml
[snapshots]
enable = true

[backup]
enable = true
repo = "s3:https://minio.example:9000/kuma"
secret = "backup"
```

**It copies from a snapshot, never from the live subvolume**, which is why
enabling it requires enabling snapshots, and why `kuma check` says so rather
than letting you find out at 3am. A backup taken while files are being
written is a backup of a moment that never existed.

**The credential is named here and held on the machine**, at
`/var/lib/kuma/secrets/<secret>.env`, mode 0600, put there by hand. A
declaration is written to be committed and is baked world-readable into every
image built from it, which is the wrong place for a secret and the same
boundary that keeps `password_hash` out of `kuma capture`.

That is not a hole in the file. Naming the credential is what keeps it
complete: that one exists and what it is called are both declared, and only
the value is elsewhere. The consequence is about recovery, and worth stating
plainly: **restoring a machine needs two things**, this file and that
credential. A repository address carrying its own password is refused, since
that arrives by pasting a restic command line that already worked rather
than by anyone deciding to put a secret in git.

**What the far end can see.** restic encrypts and authenticates
client-side, so the repository holds ciphertext and its operator cannot
read your files, whoever runs it. What they see is the shape of the
traffic: how much you store, how often, and when. The repository password
protects the data and nothing else protects the metadata.

Excludes are additive on top of a curated set that cannot be configured away:
`linuxbrew`, `~/.cache`, `~/.local/share/containers`. Every one is a tree this
same declaration rebuilds.

One thing outside home matters more than anything inside it, and it is **off
by default**:

```toml
network_connections = true   # carries /etc/NetworkManager/system-connections
```

Those files hold a passphrase per network, in the clear, and nothing else can
recreate them: not this declaration, which calls them out of scope, not the
image, which ships that directory empty, and not home, since they live in
`/etc`. Restore without them and every network password gets retyped. They are
off by default because turning them on moves secrets off the machine, and that
should be your decision rather than one made for you, so `kuma doctor` names
which way it is set on every run.

The first copy is your whole home rather than a day's difference, so it is a
command rather than something a timer starts while you are tethered:

```console
$ sudo kuma backup --init
```

**The failure this feature actually has is silence.** The unit exits cleanly
with no credential, no snapshot or no repository, all three on purpose, so
"last run succeeded" is true of a machine that has never copied a byte. So
only a run that copied something writes the stamp, and `kuma doctor` grades
the stamp rather than the unit. How stale is too stale follows the interval
you declared, so a monthly policy is not called unhealthy on day eight.

### Getting a machine back

A backup nobody has restored is a claim. This is the other half, and it is the
reason the feature exists rather than a footnote to it.

Boot the installer media and point an install at the repository:

```console
$ sudo kuma install --disk /dev/nvme0n1 --restore recovery.env
```

`recovery.env` is one file holding the repository address and its credentials.
It carries `RESTIC_REPOSITORY` as well as the keys, because the machine being
restored has no declaration yet, so the address has to come from somewhere. Put
it on the stick beside the ISO and a dead disk needs nothing else typed. An
install refuses it before touching the disk if it could not open the
repository, since the old machine may still be the only copy.

**Write the values as plain text.** A value carrying `$`, a backtick, a quote
or a backslash is refused, by `kuma install --restore` and by `kuma backup`
alike. Two things read this file, the first boot through systemd and the verb
through a shell loop, and they do not agree about what those characters mean,
so a password containing one would be a different password depending on which
one opened the repository. kuma will not guess which you meant. If a repository
predates kuma 0.17 and its password contains one, change it with `restic
passwd` before rewriting the file: it was encrypted with the expanded value.

**The install does not restore anything.** It writes the request and the
credential onto the new machine, and the first boot does the work, after the
unit that gives `/var/home` its own subvolume has run and after your account
exists. That ordering is forced rather than chosen: `/var/home` does not exist
at install time at all, and restoring into a plain directory is exactly the
state that unit steps back from.

If the repository cannot be reached on that first boot, the request stays and
the next boot tries again. A bad day costs a retry, not the data.

Two things follow that are worth knowing before you need them. The image you
install has to have been built from a declaration with `[backup]`, since that
is what carries restic and the restore unit; installing your own image is the
normal case. And restoring needs **two** things, this file and the credential
it names, which is the whole practical consequence of the declaration not
holding secrets.

## The nostr layer: a bunker, and who may ask it

`[nostr]` turns on a bunker: a daemon holding a nostr key that signs for
apps that ask. A phone's nostr app pairs with it and its signing stops
requiring the phone to hold anything; local programs reach it over a
private socket. The key lives in your login keyring, which the session
unlocks when you log in — the same place and mechanism your browser's
certificates and wifi passwords already trust.

**The keyring is the wall, and the vault is honest about that.** The
stored blob is a NIP-49 `ncryptsec` — the key wrapped with a passphrase
kept beside it — so the stored format is already the format an
independent-passphrase vault needs. Today the wrap adds nothing the
keyring does not provide, and the daemon's lock says so plainly: `lock`
drops the key from memory and refuses to sign, and `unlock` re-reads it.
Nobody claims the bunker survives an attacker already running as you in
an unlocked session, because nothing running in your session can make
that promise.

**A newly paired app can ask for everything, and does.** The policy
engine's default level is Ask: every consequential method waits on a
prompt that names the app and the act in words ("Sign a note",
"Update relay list"), with the event's own content beneath for the
judgment itself — and a sensitive cue on the kinds that change
identity, spend privacy or carry weight, because those are the asks
that deserve the read. Nothing signs until a person answers. You
relax an app to
Basic when its everyday requests should stop asking — and everyday is
an explicit safe list: notes, reposts, reactions, long-form, the
social kinds the list vouches for. Everything else asks: sensitive
writes — profile, follows, relay and mute lists, deletions — every
decrypt, NIP-04 encryption (whose job is private messages), and every
kind the list does not name. Trust signs everything
unattended, and the doctor grades any
app holding it Warn by name, because a standing grant is the loudest
thing in the layer. An approved ask can be remembered for an hour at
most; that ceiling is the verb's own. An unanswered ask times out
after five minutes — the refusal travels back to the app, and the log
keeps the expiry. The same ask retried while it waits joins the
first: one card, a count of the retries, one answer for every waiter.
The activity itself is memory, not steam — the log persists across
restarts, capped at the last 500 entries, and the panel's Activity
tab and `kuma-nostr log` both read it.

**The lock is the switch, and the switch is yours to arm.** The
daemon's lock verb drops the keys, and a bunker with no keys refuses
everything by construction. The inactivity switch is that lock on a
fuse: armed with a window (`--inactivity-lock-secs`, an hour at
least), it locks by itself when nothing has unlocked or kept it
alive for that long — the dead man's switch, for a machine that
stops answering with the gate still open. Off by default, because
the desktop daemon's posture is the PAM-open keyring; a person who
wants the fuse passes the window where the daemon starts. A restart
is a fresh window: starting the daemon is a present person's act,
which is also why the keyring being PAM-open does not defeat the
switch — the gate it guards is the one left open afterwards.

**A pairing URI is one app's door, once.** Every minted URI carries
its own one-time secret, and the connect that echoes it burns it: a
second connect with the same secret is refused. `kuma-nostr bunker`
mints a fresh URI per call — one per app, re-minted for each new
pairing — and `rotate` still invalidates every outstanding secret
at a stroke. The connect's echo is constant-time compared, because
a comparison that leaks its own progress is a lock that shows its
keys; the URI-less door — a bunker with nothing outstanding — stays
the person's gate.

**What the app is called, the app says.** The pairing record's name
is the client's own handshake metadata — the spec's optional fields
ride the `connect` method the same way they ride a `nostrconnect://`
URI, and the bunker reads both, as display hints only, never
authorization. A client that stays anonymous is shown as a pubkey
fragment, which is honest in the way a made-up name would not be;
a client that claims a name claimed it about itself. The acts are
not in the list either: a paired app's card is two lines of fact —
what it calls itself, how many asks, how recently — and tapping it
opens the view where the level, the revoke and the delete live.

**The client can hold the door open too.** A `nostrconnect://` URI —
the client's own invite, pasted into `kuma-nostr connect` or the
panel — pairs the client the moment the person pastes it: the paste
is the approval, the handshake goes out on the client's own relays,
and the URI's secret echoes back as the result the client validates.
Every refusal names itself — a missing
secret, no relay, a plaintext relay to a non-loopback host — because
the person holding the URI is the one who can fix it.

**The key the bunker signs with is not your identity.** Pairing an app
gives it the bunker's own key — a dedicated remote signer the layer
generated — so an app learns your npub only by asking, and the pubkey a
stranger watches answer on a relay is pseudonymous by construction. Your
imported identity, if you import one, never signs through the bunker.

**What a relay sees is metadata.** The bunker talks to relays; relays
carry only kind 24133 and 24135 traffic — signing requests and answers —
and every payload is NIP-44 encrypted end to end, so a relay operator
sees which app asked which bunker, how often, and how big, and never a
feed, a profile, or a signature's content. The layer's own default is to
also run a relay on your machine, loopback only, holding nothing on
disk: the bunker's first-boot reachability is your machine's own, and
the relays you declare — `wss://` to the world, `ws://` to loopback
only — are the fallbacks listed after it. The project runs a public one
at `wss://relay.nip46.com`; declaring it is an opt-in, and its operator
lives under exactly the metadata rule above.

**Reaching the socket grants nothing by itself.** The daemon answers on
a 0600 socket under your session's runtime directory, and a peer that is
not your uid is dropped before its first byte is read. What the socket
lets a local program do is ask — the policy engine decides what gets
signed, the same engine and the same log for the CLI, a local app, and a
paired phone.

**Revocation is a state, not a deletion.** `revoke` tombstones the
pairing — the app's connect is refused whatever it carries, the
tombstone survives restarts, and `unrevoke` is the person's way back
(it still needs a freshly minted URI, because the app's original
secret burned at its first connect). `delete` is the person's other
act, and it is a deletion: the record and its standing answers go
outright, and a freshly minted URI pairs the same app again — no
un-revoke needed, because deletion forgot rather than banned. The
pairing is the bond: a
known app's own reconnect re-verifies by identity — a client that
restarted itself needs no fresh URI — and `logout` is the client's
own goodbye, the same deletion under the app's own name.

**A disable is reversible, and a toggle never destroys anything.** The
key lives in your keyring and the pairings in the daemon's state — user
state no image update touches — so `[nostr] enable = false` ships a
machine without the layer and leaves your identity where it was;
re-enabling finds the key and its pairings where they were left.
SECURITY.md carries the full trust model, as it does for the signing
key the image itself verifies.

## What your machine trusts

Every TLS connection the machine makes is checked against a set of certificate
authorities, and that set is state of an awkward kind. You add to it by
dropping a file in a directory and running a command, it survives every
rebuild because it lives in `/etc`, and afterwards nothing on the machine
says why it is there. A machine could trust something its declaration could
not say.

`[system.ca_certificates]` says it, keyed by the name each anchor gets on
disk:

```toml
[system.ca_certificates]
"my-root-ca" = """
-----BEGIN CERTIFICATE-----
MIIBkTCB+wIJAKZ...
-----END CERTIFICATE-----
"""
```

**The certificate goes in the file rather than beside it.** A declaration that
points at a path somewhere else is not one file any more: carry it to a second
machine and the anchor is missing, with nothing to say so.

Inlining is safe here for a reason that does not generalise, and the boundary
is worth stating exactly. **A CA certificate is public by construction.** A
private key is not, so one pasted into this table is refused rather than
warned about: it would otherwise be baked world-readable into every image
built from the file and pushed to a registry. That is `[user]`'s
password-hash boundary, one file format over. A value that is not a
certificate at all is refused by `kuma check` for the ordinary reason.

The anchor is copied to `/etc/pki/ca-trust/source/anchors/` and
`update-ca-trust` runs in the same build layer that adds it, rather than being
left for a boot to remember. Landing under `/etc` also puts it inside the
`etc` check above, so a local edit that ever shadows it is reported rather
than silently winning.

Adding a trust root is the most consequential thing this file can do in one
line, which is why it is named again in `SECURITY.md` alongside the other
roots a declaration opts into.

## What a declaration does not reproduce

A machine that boots from this image carries state the file never mentions,
and the honest thing is to name it rather than let you find out during a
reinstall. Everything below survives an image update and is **deliberately**
outside the declaration.

**Your data.** `/var/home` is yours; a declaration rebuilds a system, not a
home. `[snapshots]` covers a mistake, and nothing here covers a dead disk yet.

**Machine identity.** SSH host keys, the machine ID, the disk layout in
`fstab` and `crypttab`. These are what make this machine this machine, and
copying them into a second one would be a bug rather than a feature.

**Secrets.** Network connections live in `/etc/NetworkManager/system-connections`
and hold the passphrase in the file. A declaration is written to be committed
and is baked world-readable into an image, which is the same boundary that
keeps `[user]`'s password hash out of `kuma capture`.

**Per-app and per-desktop state.** dconf and GSettings, the portal permission
store under `~/.local/share/flatpak/db` (the location and notification prompts
you answered), device pairings, and units you enabled in your own
`systemd --user` manager. `[services]` is system scope only.

**Everything else in `/etc` that the image never shipped.** `kuma doctor`
watches the files this image owns, and deliberately says nothing about the
rest: a real machine carries dozens of legitimately local files, and a check
that lists them all is one you learn to scroll past. The two exceptions are
the shapes that are always wrong rather than merely undeclared, and doctor
reports both: a flatpak override pointing at nothing, and a unit enabled with
no unit file behind it.

None of this is a to-do list. Some of it is a boundary that will not move
(identity, secrets), and some of it is a decision that could (dconf, the
portal store). What it is not is an oversight.

## Boot health and automatic rollback

Every image bakes [greenboot](https://github.com/fedora-iot/greenboot-rs).
There is nothing to configure, because a declarative system whose bad update
can strand the machine isn't declarative where it counts.

The first boot of a new deployment arms a rollback trigger, and a GRUB boot
counter gives it three attempts. A boot that hangs before userspace burns an
attempt just the same. On a desktop image, a boot counts as healthy only once
the greeter is actually on screen, so "boots fine into a black screen" is
precisely what this catches. When the attempts run out, GRUB falls back and
greenboot makes it permanent. A bad update costs three reboots, not the
machine.

Two deliberate choices:

- **No default health checks.** greenboot's optional check package makes DNS
  resolution *required*: reasonable on an always-networked IoT box, absurd on
  a laptop that boots offline. kumaOS installs the core framework and its own
  greeter check. Add your own under `/etc/greenboot/check/required.d/`.
- **Existing machines are retrofitted.** The boot counter is bootloader
  config written once at install time, so a machine installed before boot
  health entered its image would count nothing and reboot-loop forever
  instead of falling back. `kuma-boot-health-sync` converges that on every
  boot, and removes it again if the bootloader learns to count natively.
- **The menu names what it boots.** ostree rewrites a boot entry only when the
  kernel or the kernel arguments move, and a release that reuses the same base
  moves neither, so entries kept naming the version that used to hold their
  slot: a machine running 0.12.0 offered `kumaOS 0.11.0`. The order was still
  right, so it booted the right thing, but the menu is what you read when the
  machine will not come up far enough to run `kuma rollback`.
  `kuma-boot-titles.service` takes each entry's title from the deployment its
  own kernel argument points at, at boot and again after the deployments
  rotate at shutdown.

A rollback isn't silent: the failed deployment stays in the rollback slot,
and `kuma doctor` grades both this boot's verdict and whether the bootloader
can actually count. A previously-good deployment that starts failing reboots
three times and then waits for a human, because rolling back can't fix what
an update didn't break.

## What an install decides that a declaration cannot

Everything above is about images. An install is where an image meets a
machine, and the two know different things.

An image knows what should be installed. It cannot know who the machine is
for: a published image is pulled by strangers, so it declares no `[user]`, and
a machine installed from one would otherwise have no account and no way in.
So `kuma install` asks, writes the answers to `/var/lib/kuma/user` on the
target, and `kuma-user-sync` creates the account at first boot exactly as it
does for a declared one. The installer creates nobody; it writes down what the
machine should converge to. When the image does declare a `[user]` and the
person installing asks for a different one, the installer also drops the
declared account's autologin from the greeter, since a greeter cannot log in
a user the machine will not have; install an image for the account it
declares and the autologin stays, because then it names somebody who exists.

That split decides where things live, for the reason in
[why a file you edited by hand keeps winning](#why-a-file-you-edited-by-hand-keeps-winning):
`/var` is filled from the image once and never touched again, which is what
install-time answers need, while a file the installer shipped into `/etc`
would be image content rather than a local change, and the next update would
delete it.

The disk itself is machine state too. kuma writes the same three partitions
every time: an EFI system partition, a `/boot` outside the root, and a root
that takes the rest. `/boot` is separate even when nothing is encrypted, so
that turning encryption on changes what the third partition holds rather than
the shape of the disk. How big the first two are is asked at install, with
the defaults shown and enter accepting them, because sizes too are a
property of this disk that cannot be revised without installing again; the
shape of the disk is not asked, because a machine built from one image
should not have to know how to boot from four kinds of one. Whether the root
holds a LUKS container is asked at install too and cannot be revised
afterwards without installing again, which is why both are asked before the
plan is printed rather than defaulted either way.

**What encryption protects is a disk at rest, and only that.** `/boot` and
the EFI system partition sit outside the container on every install,
because a bootloader has to read a kernel before anything is unlocked.
Nothing measures or verifies them, so an attacker with repeated physical
access can modify the initramfs that later asks for your passphrase.
Defending against that needs Secure Boot with signed and measured boot,
which kuma does not do. Encryption answers a stolen or discarded disk, not
a machine somebody keeps visiting.

**The passphrase is handled like the machine's most consequential secret,
because it is.** It reaches `cryptsetup` on a pipe, never a command line
where `ps` would show it and never a file. It does live in memory while
the install runs, in kuma's own heap and in a shell variable in the
install script, and kuma makes no claim to defend against something
reading another process's memory, which on this machine already means
root. kuma writes it nowhere, there is no recovery key and no escrow, and
a lost passphrase is a lost disk. Changing it later is `cryptsetup
luksChangeKey` on the machine itself, which kuma has no verb for.

The account password gets the same handling: prompted for at a terminal or
read as one line from a pipe, never passed as an argument, so it does not
reach `ps` or a shell history. Its hash is written to `/var/lib/kuma/user`
on the target, mode 0600, where `kuma-user-sync` reads it at first boot.
Unlike a declared `[user]`, it is written to the machine rather than baked
into the image, so it is not published by publishing the image.

A swapfile is machine state for a sharper reason than the rest of it.
Hibernating writes memory to a file and the kernel has to be told where that
file physically sits on the disk, as a block offset. That number describes one
file on one disk. It is not a property a declaration could carry, because two
machines built from the same file would need different values, and a machine
that got the wrong one would hibernate, power off, and boot fresh with the
session gone. So the size is asked at install, like encryption, and
`kuma hibernate` asks it on a machine that is already running. `kuma doctor`
compares the number the kernel was given against the number the file actually
has, because that is the only way this breaks quietly.

A first boot then spends minutes on the rest of it. The account is made, the
hostname applied, and the declared flatpaks and brews downloaded, which for a
full desktop is about a gigabyte and takes a few minutes on an ordinary
connection. kuma says so while it happens: bare `kuma` reports `converging`
rather than drift, and offers no `sync`, because a sync is what is running. A
machine that does not match its declaration yet is not the same as one that
has stopped trying.

None of that is in `kuma.toml`, and none of it should be. Two machines
installed from one declaration can have different disks, different names, and
different people. The declaration says what the system is; the install says
whose it is.
