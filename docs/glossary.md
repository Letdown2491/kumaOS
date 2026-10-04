# Glossary

The words kuma's documentation uses, one sentence each. Where a term has a
general meaning and a narrower one here, the narrow one is what kuma means.

**Atomic.** A change that either fully happens or does not happen at all:
kuma builds a whole new system image and switches to it at the next boot,
never edits a running system.

**Backup.** A copy of a snapshot in a restic repository somewhere else, made
on a timer; snapshots survive a mistake, backups survive the disk.

**Bunker.** A remote signer: a daemon holding a nostr key that answers
signing requests from paired apps. The nostr layer runs one per session;
see [the nostr layer](concepts.md#the-nostr-layer).

**Pairing URI.** The one-time invite a client or the bunker mints —
`bunker://` from the bunker, `nostrconnect://` from the client. It
carries its own secret, and the connect that presents it burns it: one
URI pairs one app once.

**Tombstone.** What revoking leaves behind: the pairing record stays,
marked `revoked_at`, and the app's connect is refused whatever it
carries until `unrevoke` clears it. Delete, the person's removal, and
logout, the app's own goodbye, delete instead.

**Base.** The foundation an image is built on. With `system.base` unset,
kuma composes its own from Fedora's packages rather than starting from
somebody else's image.

**bootc.** The Fedora technology kuma builds on, which boots a machine from
a container image and rolls it back; kuma's job is to produce the image,
bootc's is to boot it.

**Capture.** The verb that acts on drift: `kuma capture` proposes declaring
what the machine already runs — flatpaks, brew leaves, unnamed flatpak
permissions — and writes nothing until `--yes`.

**Container image.** A packaged filesystem, the format that runs containers;
a bootc machine boots one instead, so "image" here means the whole operating
system, not an application.

**Contract.** What kuma 44.0 promises about every surface above; see
[the contract](contract.md).

**Convergence.** Making the machine match the declaration: installing what
is named, updating what is present, removing what kuma itself installed and
the declaration no longer names. Runs at boot and on a daily timer.

**Declaration.** Your `kuma.toml`. The file that says what the system is.

**Deployment.** One bootable system on the machine; a bootc machine keeps
more than one, which is what makes rollback instant.

**Digest.** A checksum that names one exact image, as opposed to a tag,
which points at whatever was published most recently; `kuma.lock` pins it.

**Drift.** Anything the machine has that the declaration does not name.
kuma treats it as a proposal to consider rather than a fault to erase; see
[how kuma behaves](concepts.md#what-happens-to-changes-you-make-by-hand).

**ESP.** The EFI system partition: the small FAT partition the firmware
reads the bootloader from, before the operating system exists.

**Flatpak.** A packaged desktop application that carries its own
dependencies and updates independently of the system; needs no reboot.

**greenboot.** The health check that runs on the first boot of a new
system; if it fails three times, the bootloader falls back to the previous
deployment on its own.

**Hibernate.** Writing memory to a swapfile and powering off, so the
machine comes back where it was; it needs the kernel told where that file
sits on the disk. Distinct from suspend, which keeps memory powered, and
from **zram**, which is swap inside memory and cannot hold a copy of it.

**Installer media.** The USB stick a machine boots to be installed. kumaOS's
is live: its root filesystem is the desktop image itself, so what you look
at before installing is what you get.

**Lockdown.** The kernel's restricted mode under Secure Boot; it blocks
hibernation, and nothing kuma configures changes it.

**LUKS.** Linux disk encryption; `kuma install` can put the root filesystem
inside a LUKS container, unlocked by a passphrase at every boot.

**nsec.** A nostr secret key, spelled `nsec1…` when encoded. The thing
that *is* a nostr identity; everything else about the account is
derivable from it. The nostr layer holds one in the login keyring and
signs with it only through the policy engine.

**ncryptsec.** An nsec wrapped by NIP-49 with a passphrase, spelled
`ncryptsec1…`. The form the vault stores, so the stored format is the
format an independent-passphrase vault needs, whatever the keyring
itself proves to be worth.

**Machine state.** What is true of one machine rather than of the system it
runs — hostname, timezone, which wifi network, the volume. kuma
deliberately keeps it out of the declaration; the opposite of **system
definition**.

**ostree.** The technology underneath bootc that stores the system
read-only and merges your `/etc` onto each new deployment.

**Override.** A permission you granted or took away from a flatpak;
`[overrides]` declares them per app, and convergence touches only the keys
you declared, so a Flatseal toggle survives unless you declare otherwise.

**Plymouth.** The program that owns the screen between the firmware and the
login screen, which also draws the encrypted disk's passphrase prompt.

**Podman.** The container tool kuma builds with, rootless; the one build
dependency.

**Rebase.** Pointing a bootc machine at a different image, so the next boot
runs that image's system while everything in `/var` stays; the way an
existing machine moves to kuma. See [moving over](moving.md).

**rpm.** A Fedora package; declared ones become part of the image, so
changing them is a build and a reboot.

**Signature.** Proof that an image was published by this project; machines
carrying kuma's policy refuse an update whose signature does not check out.

**Snapshot.** A read-only copy of `/var/home` as it was at a moment, taken
hourly when `[snapshots]` is enabled; cheap, because btrfs shares unchanged
data.

**Stage.** A new deployment written to the disk and set to boot next time,
without touching what is running; the reboot is yours to choose.

**Subvolume.** A btrfs filesystem within a filesystem, which can be
snapshotted on its own; kuma installs the system into one named `root`.

**Suspend-then-hibernate.** Suspending first and hibernating only when the
battery demands it, which is what closing the lid does on a machine with a
swapfile.

**System definition.** What is true of every machine built from a
declaration — packages, desktop, firmware, shell; changing it means a build
and a reboot. The opposite of **machine state**.

**Tag.** A moving name for an image, like `:44`, which is why a build
records the digest instead.
