# Getting started

There are two ways in. Download the media and install a machine, which
needs nothing but a USB stick. Or build an image from a declaration on any
machine with podman. This walks both, in that order, because installing
gives you a machine that can do the building.

Each step says what to type, what you should see, and what it means. If a
word is unfamiliar, [the glossary](glossary.md) defines it in a line.

## What you need

**To install:** a USB stick, a machine to write it on, and a network
connection on the machine you are installing. Installing downloads the
system image rather than copying it off the media.

**To build your own:** [podman](https://podman.io/). That machine does not
have to run kuma, and a build changes nothing on it: an image lands in
podman's storage and nothing else is touched. Trying a declaration in a
virtual machine also needs KVM and sudo.

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

The media boots to a working desktop before anything is written to a
disk. The ISO's root filesystem *is* a kuma image, not an installer
program sitting beside one. Look around, open a browser, check that your
hardware works. Nothing persists and nothing is written until you
install.

## 3. Install it

Connect to a network first. On the niri desktop, `Super + T` opens a
terminal. Then:

```console
$ kuma install
```

It lists the disks it found and asks which one to use. Then it asks:
whether to encrypt the disk, how big the EFI system partition and `/boot`
should be (enter takes the defaults, 600M and 2G), whether to create a
swapfile so the machine can hibernate, a passphrase for the disk if you
chose encryption, an account name and password, and a hostname. It prints
the partition layout it will write before it writes anything.

**Which image you get.** This media installs
`ghcr.io/letdown2491/kuma:niri`, the desktop image this project
publishes, and says so before it starts. Installing pulls that image from
the registry rather than copying the one you booted, which is why the
network matters.

The account is asked for rather than declared because the image is
shared and you are not. kuma writes your answers onto the target disk,
and the machine creates the account on its first boot.

Encryption is asked here because it cannot be added later without
installing again. Say yes and the machine asks for that passphrase at
every boot, before anything else runs. Nothing keeps a copy of it: a
lost passphrase is a lost disk.

Hibernate is off unless you ask for it. Say yes and kuma makes a
swapfile the size of memory and sets the kernel arguments to resume from
it. One warning the installer gives before it writes: on a disk you
chose *not* to encrypt, hibernating writes the contents of memory to
that disk in the clear. You can add or remove a swapfile later with
`kuma hibernate`, so this is the one question here you are not stuck
with.

This is the one command in kuma that cannot be undone. There is no
staged change to discard and no rollback slot. It refuses a disk with
anything mounted on it, and without `--yes` it only prints the plan.

## 4. The first boot

Take the stick out and boot the machine. In order, you get:

1. A boot splash, and under it, if you chose encryption, a passphrase
   prompt that names the disk it is asking about.
2. A login screen, using the account and hostname you gave the
   installer.
3. A desktop, and then several minutes of quiet work.

That last part is worth expecting. The declared applications and command
line tools download on that first boot, around a gigabyte for a full
desktop. While it runs, `kuma` says `converging` rather than reporting a
problem:

```console
$ kuma
state: converging - flatpak convergence is running now; this is what the
machine is doing, not drift
```

When it settles, the same command says `in-sync`.

At this point you have a working machine running a declaration somebody
else wrote. The rest of this makes it yours.

## 5. Describe the machine

```console
$ kuma init
```

On the machine you just installed, that writes a copy of the declaration
its image was built from, so you start from what you have rather than
from a template. Anywhere else it writes a starter `kuma.toml` in the
current directory.

The machine already has kuma, at `/usr/bin/kuma`, put there by the build
that made its image. Do not install a second copy. On a machine that
does not run kuma, the tool is a single file, and everything it needs is
compiled in:

```console
$ curl -LO https://github.com/Letdown2491/kumaos/releases/latest/download/kuma-x86_64-unknown-linux-musl
$ chmod +x kuma-x86_64-unknown-linux-musl
$ sudo mv kuma-x86_64-unknown-linux-musl /usr/local/bin/kuma
```

The [nostr layer](nostr.md)'s two binaries install the same way, for
building images that enable it:

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
flatpak = ["app.zen_browser.zen"]
brew = ["ripgrep", "gh"]
```

Four things are worth knowing about that file, and the rest can wait.

**Three package lists, because the three behave differently.** `rpm`
becomes part of the image: changing it is a build and a reboot. `flatpak`
and `brew` install on the running machine and need no reboot.

**A password is not in there yet.** Run `kuma passwd`, and paste what it
prints into `[user]` as `password_hash`. Without it the account exists
but cannot log in. Anyone who can read the image can read that hash, so
leave it out of anything you publish.

**You did not name a base image, and you do not have to.** kuma builds
its own foundation out of Fedora's packages. Naming `system.base` opts
out and builds on the image you name instead.

**Your machine's own settings stay out of the file.** Hostname, timezone
and encryption belong to the machine. Two machines built from this one
file can differ on all three.

Check it before building anything:

```console
$ kuma check
```

That validates the file and touches nothing. It is the fast way to find
a typo, and it needs no podman.

## 6. Build it and switch to it

```console
$ kuma build
```

This is the slow step, and only the first time: kuma assembles its base
from Fedora's packages, then layers your declaration on top. Later
builds reuse that base unless something in it changed. What comes out is
an image named `localhost/kuma:latest` in podman's storage, and a
`kuma.lock` file beside your declaration recording exactly what the
build resolved to. Commit the lock along with the declaration.

Building on the machine that will run the image is the shortest path
from a declaration to hardware:

```console
$ kuma switch --yes
```

That stages the image you just built. It lands when you reboot, and the
system you were on stays in the rollback slot.

**Name `podman` in `packages.rpm` if you build this way.** The published
image has it, but only as a dependency of `distrobox`. A declaration
that drops distrobox and does not name podman produces a machine that
cannot build the next image.

## 7. Try changes before you commit to them

```console
$ kuma vm
```

That builds a virtual disk from the image and boots it in a window. It
needs KVM and sudo. Log in as `kuma` with the password `kuma`, which
every VM disk carries so you are never locked out of your own test: a
test disk has no person on it.

This is where you find out that you wanted a different terminal, or that
you forgot a package. Edit `kuma.toml`, run `kuma build` again, then:

```console
$ kuma vm --apply
```

That pushes the new image into the VM that is already running and
switches it over, keeping everything in `/var`, so your applications do
not download again. It is also the real update mechanism, which means
you are testing the thing you will later do to a real machine.

## 8. Media carrying your own declaration

The media in step 1 installs this project's published image. To hand
somebody a stick that installs *yours*:

```console
$ kuma iso --live
```

Out comes `iso/KUMA.iso`, around 1.8 GB, written the same way as step 1.

One thing decides whether it installs what you meant. Installing pulls
an image from a registry, so media built from a local `kuma build`
cannot install that build: `localhost/kuma` means nothing to the machine
being installed, and kuma installs its published image instead, saying
so before it starts. Push your image to a registry first and the media
installs yours, or name it at install time:

```console
$ kuma install --image ghcr.io/<owner>/kuma:<tag>
```

`kuma iso` without `--live` builds traditional Anaconda installer media
instead. It is about a gigabyte larger, and it needs sudo. Use it if you
want Fedora's familiar installer screens.

A `[user]` in the declaration rides into the media as a real account and
password hash, so build shareable media from a declaration without one.
Anaconda's create-a-user screen comes back on its own when you do.

The live session itself runs as `liveuser`, a passwordless account with
passwordless sudo that exists only inside the ISO's read-only filesystem
and never reaches an installed machine. It runs SELinux permissive,
because a container image's real labels are not reachable through a
podman mount; an installed machine is enforcing from its first boot.

## 9. Living with it

Five commands cover ordinary use:

```console
$ kuma                  # where this machine is, and what it can do next
$ kuma doctor           # a health check: image age, convergence, drift, encryption, disk
$ kuma update --check   # what has moved in Fedora's packages, security first
$ kuma update --yes     # build a current image and stage it for next boot
$ kuma rollback --yes   # go back to the deployment you were on before
```

Running bare `kuma` is always safe, and every command ends by naming
what you can legally do next, so you can follow the prompts rather than
remember the verbs. Nothing changes what is running without a reboot: an
update stages, and the change lands when you choose.

**Hibernate, if you did not ask for it at install:**

```console
$ kuma hibernate              # what it would make, and where
$ kuma hibernate --yes        # make it; takes effect on the next boot
$ kuma hibernate --off --yes  # take it away again
```

Why a swapfile cannot be declared, and what Secure Boot has to do with
it: [hibernation](concepts.md#hibernation).

**The nostr layer, if your declaration enabled it.** Setup is once, and
pairing is a command per app:

```console
$ kuma-nostr setup            # asks which road: a fresh key, or one you hold
$ kuma-nostr bunker --qr      # mints a one-time pairing URI, as text and QR
$ kuma-nostr connect <uri>    # or: pair a client's own nostrconnect:// invite
$ kuma-nostr prompts          # what is waiting on you
$ kuma-nostr approve <id>
$ kuma-nostr log              # the activity: what was asked, and how it went
```

A pairing URI works for one app, once, so mint another for the next app.
The panel does all of this with buttons.
[The nostr layer](nostr.md) has the trust levels and where the key
lives.

**Snapshots and backups.** `[snapshots]` covers a mistake;
`[backup]` covers a dead disk. The first copy is your whole home, so it
is a command rather than something a timer starts:

```console
$ sudo kuma backup --init
```

The two keys, the credential file, and what a restore needs:
[backups, and the two things a restore needs](concepts.md#backups-and-the-two-things-a-restore-needs).

**When something is wrong and you want help.** `kuma doctor --report`
prints one document with the findings, which image is booted, and the
declaration the machine was built from. That is what to attach to a bug
report. Your password hash is removed before it prints.

The reasoning behind the loop — why drift is a proposal rather than an
error, what the signature on an update refuses, how a bad update rolls
itself back without you — is [how kuma behaves](concepts.md), which is
where to go when something surprises you.

## Recovering a machine

If the disk dies, boot the installer media and point an install at the
repository instead of starting empty:

```console
$ sudo kuma install --disk /dev/nvme0n1 --restore recovery.env
```

That file holds the repository address and its credentials, so it is the
one thing to keep somewhere other than the machine. The install writes
the request onto the new disk, and the first boot puts your home
directory back. [How it works](concepts.md#getting-a-machine-back).

## Where to go next

- [How kuma behaves](concepts.md) explains the reasoning under all of
  this: why drift is a proposal rather than an error, how rollback
  works, and what a declaration deliberately does not reproduce.
- [The nostr layer](nostr.md) covers the bunker, pairing, and where the
  key lives.
- [Moving over](moving.md) is the path for a machine that already runs
  something: rebase if it boots an image, back up and install if it
  doesn't.
- [What a desktop contains](desktops.md) lists what `desktop = "niri"`
  or `"cosmic"` installed that you never named.
- [Glossary](glossary.md) for any word above that was new.
