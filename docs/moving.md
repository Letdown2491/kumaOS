# Moving over

Bringing a machine you already have to kuma, rather than starting from an
empty disk. If the disk is empty or disposable,
[getting started](getting-started.md) is the whole path. If it holds a
system you are using, which of the two ways below is yours turns on one
question: does the machine boot an image today?

## From an image-based system: rebase

Bluefin, Bazzite, Aurora, Silverblue, Kinoite: anything built the way kuma
is, with the system delivered as an image and your files living in `/var`.
A machine like that moves with one command, and nothing on its disk is
destroyed:

```console
$ sudo bootc switch ghcr.io/letdown2491/kuma:niri
```

`kuma:cosmic` is the same image with the experimental COSMIC desktop.
Reboot when the download finishes. On an older rpm-ostree machine the same
move is spelled:

```console
$ sudo rpm-ostree rebase ostree-unverified-registry:ghcr.io/letdown2491/kuma:niri
```

A [rebase](glossary.md) swaps which image the machine boots. The next boot
runs kuma's system and keeps everything in `/var`: your account, your home
directory, your networks, your flatpaks. Nothing is partitioned and nothing
is wiped. kumaOS's three partitions are what `kuma install` writes onto a
disk, and a rebase never goes near a partition table; the disk keeps the
layout its previous system gave it.

What the first boot looks like:

- The desktop is niri, not the one you had. The image decides the
  desktop. Your files are untouched; the screen around them is new.
- The declared applications download, which takes minutes. Bare `kuma`
  reports `converging` until they have, exactly as on a fresh install.
- **Snapshots may be off, and the machine says so.** `[snapshots]` needs
  `/var/home` to be a btrfs subvolume, and the unit that makes one runs
  only while the directory is empty. A machine that arrived with your
  home already in it is left exactly as it is, because making it a
  subvolume now would mean moving your files, and `kuma doctor` reports
  that state rather than letting the timer quietly take nothing.

Then make it yours. On the machine:

```console
$ kuma init
```

That writes a working copy of the declaration the image was built from, so
you start from what you are running. From there it is the loop in
[getting started](getting-started.md): edit the file, `kuma check`, `kuma
build`, `kuma switch --yes`.

The flatpaks you brought with you are drift, not errors. `kuma diff` lists
them, `kuma capture` offers to write them into the declaration, and
nothing is deleted for being undeclared.

**Going back.** The system you were on is still on the disk, in the
rollback slot. `sudo bootc rollback` boots it again.

**Signatures.** The image is signed. Whether the machine you are leaving
checks that signature on the way in is that machine's policy, not kuma's;
[SECURITY.md](../SECURITY.md#verifying-a-release) has the command that
checks it by hand.

## From a package-managed system: back up, install, put back

`kuma install` writes the whole disk and never asks what is on it, and
kuma does not install beside another system. The move is three steps:

1. Back up your home directory, to an external disk or anywhere a fresh
   machine can reach. While you can still ask, note what you had
   installed: `flatpak list`, and your package manager's equivalent. That
   list is about to become your declaration.
2. Install kuma the ordinary way, steps 1 to 3 of
   [getting started](getting-started.md): media, then `kuma install`.
3. Put the files back into the home directory of the account you created
   at install. On a kuma machine that is under `/var/home`.

What travels when the files do: your documents, and the settings that
live inside them. What does not: anything installed into the system,
which means packages, system flatpaks, services enabled, and mounts added
by hand. None
of it travels because none of it is carried: a kuma machine builds the
system half of itself from a declaration rather than inheriting one. Name
the flatpaks under `[packages].flatpak`, the packages under
`[packages].rpm`, and `kuma capture` offers to write down whatever came
along with your files.

Once you are here, make the next move a one-file affair. `[snapshots]`
and `[backup]`, the two keys [getting started](getting-started.md)
describes, mean the machine after this one begins with `kuma install
--restore recovery.env` and your home directory comes back on its first
boot.
