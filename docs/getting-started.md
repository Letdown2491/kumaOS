# Getting started

There are two ways in. Download the media and install a machine, which needs
nothing but a USB stick, or build an image from a declaration on any machine
with podman. This walks both, in that order, because installing gives you a
machine that can do the building.

Each step says what to type, what you should see, and what it means. If a word
is unfamiliar, [the glossary](glossary.md) defines it in a line.

## What you need

**To install:** a USB stick, a machine to write it on, and a network
connection on the machine you are installing. Installing downloads the system
image rather than copying it off the media.

**To build your own:** [podman](https://podman.io/). That machine does not
have to run kuma, and nothing a build does changes it: an image lands in
podman's storage and nothing else is touched. Trying a declaration in a
virtual machine needs KVM and sudo on top of that.

## 1. Write the media

```console
$ curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-x86_64.iso
```

Around 1.8 GB. Every release asset is signed, and
[SECURITY.md](../SECURITY.md#verifying-a-release) has the one command that
checks a file came from this project's release workflow. Worth running on
something you are about to boot a machine from.

Write it to a USB stick:

```console
$ sudo dd if=kuma-x86_64.iso of=/dev/sdX bs=4M status=progress
```

Check `/dev/sdX` twice. `lsblk` lists your disks, and `dd` will overwrite
whatever you name without asking.

## 2. Look before you install

The media boots to a working desktop before anything is written to a disk,
because the ISO's root filesystem *is* a kuma image rather than an installer
program sitting beside it. Look around, open the browser, see whether the
hardware works. Nothing persists and nothing is written until you install.

## 3. Install it

Connect to a network first. On the niri desktop, `Super + T` opens a terminal.
Then:

```console
$ kuma install
```

It lists the disks it found and asks which one to use, then asks: whether to
encrypt the disk, how big the EFI system partition and `/boot` should be
(enter takes the defaults, 600M and 2G), whether to create a swapfile so
the machine can hibernate, a passphrase for the disk if you said yes to
encryption, an account name and password, and a hostname. It prints the
partition layout it will write before it writes anything.

**Which image you get.** This media installs
`ghcr.io/letdown2491/kuma:niri`, the desktop image this project publishes, and
says so before it starts. Installing pulls that image from the registry rather
than copying the one you booted, which is why the network matters.

The account is asked for rather than declared because the image is shared and
you are not. kuma writes your answers onto the target, and the machine creates
the account on its first boot.

Encryption is asked here because it cannot be added later without installing
again. Say yes and the machine asks for that passphrase at every boot, before
anything else runs. Nothing keeps a copy of it, so a lost passphrase is a lost
disk.

Hibernate is off unless you ask for it. Say yes and kuma makes a swapfile the
size of memory on the root filesystem and sets the kernel arguments that resume
from it. On a disk you chose not to encrypt, hibernating writes the contents of
memory to that disk in the clear; the installer says so before it writes. You
can add or remove a swapfile later with `kuma hibernate`, so this is the one
question here you are not stuck with.

This is the one command in kuma that cannot be undone. There is no staged
change to discard and no rollback slot. It refuses a disk with anything
mounted on it, and without `--yes` it only prints the plan.

## 4. The first boot

Take the stick out and boot the machine. In order, you get:

1. A boot splash, and under it, if you chose encryption, a passphrase prompt
   that names the disk it is asking about.
2. A login screen, using the account and hostname you gave the installer.
3. A desktop, and then several minutes of quiet work.

That last part is worth expecting. The declared applications and command line
tools download on that first boot, which for a full desktop is around a
gigabyte. While it runs, `kuma` says `converging` rather than reporting that
anything is wrong:

```console
$ kuma
state: converging - flatpak convergence is running now; this is what the
machine is doing, not drift
```

When it settles, the same command says `in-sync`.

At this point you have a working machine running a declaration somebody else
wrote. The rest of this makes it yours.

## 5. Describe the machine

```console
$ kuma init
```

On the machine you just installed, that writes a copy of the declaration its
image was built from, so you start from what you have rather than from a
template. Anywhere else it writes a starter `kuma.toml` in the current
directory.

The machine already has kuma, at `/usr/bin/kuma`, put there by the build that
made its image. Do not install a second copy. On a machine that does not run
kuma, the tool is a single file and everything it needs, including the
wallpaper and the desktop configuration it bakes into images, is compiled in:

```console
$ curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-x86_64-unknown-linux-musl
$ chmod +x kuma-x86_64-unknown-linux-musl
$ sudo mv kuma-x86_64-unknown-linux-musl /usr/local/bin/kuma
```

The release carries the [nostr layer](concepts.md#the-nostr-layer)'s two
binaries the same way, and an image built from a declaration with
`[nostr]` enabled stages them from beside the running kuma — so they
install to the same place when the layer is wanted:

```console
$ curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-nostrd-x86_64-unknown-linux-musl
$ curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-nostr-x86_64-unknown-linux-musl
$ chmod +x kuma-nostrd-x86_64-unknown-linux-musl kuma-nostr-x86_64-unknown-linux-musl
$ sudo mv kuma-nostrd-x86_64-unknown-linux-musl /usr/local/bin/kuma-nostrd
$ sudo mv kuma-nostr-x86_64-unknown-linux-musl /usr/local/bin/kuma-nostr
```

The file is the whole interface. This is a complete one:

```toml
schema_version = 1

[system]
desktop = "niri"

[user]
name = "me"
shell = "fish"

[packages]
rpm = ["fish", "distrobox"]
flatpak = ["org.mozilla.firefox"]
brew = ["ripgrep", "gh"]
```

Four things are worth knowing about that file, and the rest can wait.

**Three package lists, because the three behave differently.** `rpm` becomes
part of the image — changing it is a build and a reboot; `flatpak` and `brew`
install on the running machine and need no reboot.

**A password is not in there yet.** Run `kuma passwd`, and paste what it
prints into `[user]` as `password_hash`. Without it the account exists but
cannot log in. Anyone who can read the image can read that hash, so leave it
out of anything you publish.

**You did not name a base image, and you do not have to.** kuma builds its own
foundation out of Fedora's packages. Naming `system.base` opts out and builds
on the image you name instead.

**Your machine's own settings stay out of the file.** Hostname, timezone and
encryption belong to the machine; two machines built from this one file can
differ on all three.

Check it before building anything:

```console
$ kuma check
```

That validates the file and touches nothing. It is the fast way to find a
typo, and it needs no podman.

## 6. Build it and switch to it

```console
$ kuma build
```

This is the slow step, and only the first time: kuma assembles its base from
Fedora's packages, then layers your declaration on top. Later builds reuse
that base unless something in it changed. What comes out is an image named
`localhost/kuma:latest` in podman's storage, and a `kuma.lock` file beside
your declaration recording exactly what the build resolved to. Commit the lock
along with the declaration.

Building on the machine that will run the image is the shortest path from a
declaration to hardware:

```console
$ kuma switch --yes
```

That stages the image you just built. It lands when you reboot, and the system
you were on stays in the rollback slot.

**Name `podman` in `packages.rpm` if you build this way.** The published image
has it, but as a dependency of `distrobox` rather than in its own right, so a
declaration that drops distrobox and does not name podman produces a machine
that cannot build the next image.

## 7. Try changes before you commit to them

```console
$ kuma vm
```

That builds a virtual disk from the image and boots it in a window. It needs
KVM and sudo. Log in as `kuma` with the password `kuma`, which every VM disk
carries so you are never locked out of your own test: the disk is built by
kuma's own installer, and the account it answers for is the appliance
account, not the declaration's — a test disk has no person on it.

This is where you find out that you wanted a different terminal, or that you
forgot a package. Edit `kuma.toml`, run `kuma build` again, then:

```console
$ kuma vm --apply
```

That pushes the new image into the VM that is already running and switches it
over, keeping everything in `/var`, so your applications do not download
again. It is also the real update mechanism, which means you are testing the
thing you will later do to a real machine.

## 8. Media carrying your own declaration

The media in step 1 installs this project's published image. To hand somebody
a stick that installs *yours*:

```console
$ kuma iso --live
```

Out comes `iso/KUMA.iso`, around 1.8 GB, written the same way as step 1.

One thing decides whether it installs what you meant. Installing pulls an
image from a registry, so media built from a local `kuma build` cannot install
that build: `localhost/kuma` means nothing to the machine being installed, and
kuma installs its published image instead, saying so before it starts. Push
your image to a registry first and the media installs yours, or name it at
install time:

```console
$ kuma install --image ghcr.io/<owner>/kuma:<tag>
```

`kuma iso` without `--live` builds traditional Anaconda installer media
instead. It is about a gigabyte larger for the same system, and it needs sudo.
Use it if you want Fedora's familiar installer screens.

A `[user]` in the declaration rides into the media as a real account and
password hash, so build shareable media from a declaration without one.
Anaconda's create-a-user screen comes back on its own when you do.

The live session itself runs as `liveuser`, a passwordless account with
passwordless sudo that exists only inside the ISO's read-only filesystem and
never reaches an installed machine, and it runs SELinux permissive, since a
container image's real labels are not reachable through a podman mount. An
installed machine is enforcing from its first boot. The media also records
which image to install rather than carrying it, so that file is trust: it
names this project's registry, whose images the signature policy checks, and
only root at the live console could name anything else — the same power that
comes with the keyboard anyway.

## 9. Living with it

Five commands cover ordinary use. All of them read the same declaration.

```console
$ kuma                  # where this machine is, and what it can do next
$ kuma doctor           # a health check: image age, convergence, drift, encryption, disk
$ kuma update --check   # what has moved in Fedora's packages, security first
$ kuma update --yes     # build a current image and stage it for next boot
$ kuma rollback --yes   # go back to the deployment you were on before
```

Running bare `kuma` is always safe and always tells you where you are. Every
command ends by naming what you can legally do next, so you can follow the
prompts rather than remember the verbs. Nothing changes what is running
without a reboot: an update stages, the change lands when you choose, and if
one would move you to a new Fedora release it says so in those words before
it stages — that is the largest change kuma can make, and it otherwise
arrives as several hundred package lines.

**Hibernate, if you did not ask for it at install.** A machine hibernates into
a swapfile, and needs the kernel told where that file physically sits on the
disk. `kuma hibernate` does both:

```console
$ kuma hibernate              # what it would make, and where
$ kuma hibernate --yes        # make it; takes effect on the next boot
$ kuma hibernate --off --yes  # take it away again
```

It defaults to the size of memory, which is the most a machine can ever have
to save. The file is never resized in place: growing it would move it, and
the kernel would then resume from the wrong place on the disk. To change the
size, turn it off and on again.

Setting hibernate up also points the lid at suspend-then-hibernate: a laptop
closed in a bag suspends first and hibernates on the firmware's own
low-battery alarm, before the battery dies. `kuma hibernate --off` takes the
lid setting away with the rest. `kuma doctor` grades the lid, the offset the
kernel was given against the one the file actually has, and a `kuma
hibernate --yes` on a machine that already has a swapfile repairs what
disagrees.

**The nostr layer, if your declaration enabled it.** The bunker starts
with your session; `kuma doctor` will have said the vault is not set up
yet. Setup is once:

```console
$ kuma-nostr setup            # asks which road: a fresh key, or one you hold
```

Then pair a phone or an app from the panel (`Mod+Ctrl+N`, the Pair tab) or
from a terminal:

```console
$ kuma-nostr bunker --qr      # mints a one-time pairing URI, as text and QR
$ kuma-nostr connect <uri>    # or: pair a client's own nostrconnect:// invite
$ kuma-nostr prompts          # what is waiting on you
$ kuma-nostr approve <id>
$ kuma-nostr log              # the activity: what was asked, and how it went
```

A pairing URI pairs one app once — the connect burns it — so mint another
for the next app. The panel does all of this with buttons, keeps the
bunker's keep-alive fed while you are using it, watches for new asks
while it is open, and shows the apps as what they are: a name from
the client's own handshake when it gives one, a pubkey fragment when
it does not, with its asks counted beneath — and its acts (level,
revoke, delete) one tap deeper, in the app's own view. `kuma
doctor` grades the whole layer, and says so
before setup too.
hibernate --yes` on a machine that already has a swapfile repairs what
disagrees.

**Secure Boot and hibernate do not go together.** A kernel that booted with
Secure Boot on runs locked down, and a locked-down kernel refuses to
hibernate, because a hibernate image is a way to write arbitrary memory back
into a running kernel. `kuma doctor` warns rather than calling the machine
ready; turning Secure Boot off in firmware is the only way to have both.

Hibernate from the desktop rather than over ssh: logind gates it on an
active session, and an ssh login is not one. That is not kuma, and
`kuma doctor` will still tell you the machine is ready.

**Your files are the one thing this file cannot rebuild.** A declaration
reproduces a system; it does not reproduce `/var/home`, and it never will,
because that is your work rather than a description of a machine. Two keys
cover the gap:

```toml
[snapshots]
enable = true   # hourly read-only copies, on this disk

[backup]
enable = true                                  # copies somewhere else
repo = "s3:https://minio.example:9000/kuma"
secret = "backup"                              # names a credential; see below
```

Snapshots answer a mistake and cannot answer a dead disk, because they are
on it. `[backup]` copies them offsite with restic, on a timer. The
credential is named in the declaration and kept out of it — put the
repository's keys at `/var/lib/kuma/secrets/backup.env`, mode 0600, then
make the first copy on purpose:

```console
$ sudo kuma backup --init
```

After that `kuma backup` reports without touching the network, and
`kuma doctor` grades how fresh the copies stay, which matters because the way
backups fail is silence rather than errors.
[How kuma behaves](concepts.md#backups-and-the-two-things-a-restore-needs)
explains why the credential lives on the machine, what the far end can see,
and why restoring a machine needs two things.

**On a desktop, `Mod+D` opens the launcher.** Your applications are in it,
and so are kuma's own verbs; each opens a terminal and leaves it open,
because several ask for a password and all print something worth reading.
[kuma in your launcher](concepts.md#kuma-in-your-launcher) has the list and
the one rule about it: no entry writes your declaration without asking.

**When something is wrong and you want help.** `kuma doctor --report` prints
one JSON document with the findings, which kuma is running, which image is
booted and its digest, and the declaration the machine was built from. That is
what to attach to a bug report. The password hash is removed before it prints,
a declaration kuma cannot parse is left out entirely rather than pasted raw,
and where a report quotes what a failed service said, anything shaped like a
password hash is masked in that too.

The reasoning behind the rest of the loop — why drift is a proposal rather
than an error, what the signature on an update refuses, how a bad update
rolls itself back without you — is [how kuma
behaves](concepts.md), which is where to go when something surprises you.

## Recovering a machine

If the disk dies, boot the installer media and point an install at the
repository instead of starting empty:

```console
$ sudo kuma install --disk /dev/nvme0n1 --restore recovery.env
```

That file holds the repository address and its credentials, so it is the one
thing to keep somewhere other than the machine. The install writes the request
onto the new disk and the first boot puts your home directory back.

[How kuma behaves](concepts.md#getting-a-machine-back) explains why the restore
happens at first boot rather than during the install, and what it does when the
repository cannot be reached that day.

## Where to go next

- [How kuma behaves](concepts.md) explains the reasoning under all of this:
  what is pinned, what merges, how rollback works, and why drift is treated as
  a proposal.
- [Moving over](moving.md) is the path for a machine that already runs
  something: rebase if it boots an image, back up and install if it doesn't.
- [What a desktop contains](desktops.md) lists what `desktop = "niri"` or
  `"cosmic"` installed that you never named.
- [Glossary](glossary.md) for any word above that was new.
