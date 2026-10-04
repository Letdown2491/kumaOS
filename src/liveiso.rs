//! Live ISO assembly.
//!
//! `kuma iso` has always built Anaconda installer media through
//! bootc-image-builder. That media carries two root filesystems which
//! share nothing: Fedora's installer environment, and kuma's own image.
//! Measured on a niri build, the pair is about 2.4 GB, and GitHub caps a
//! release asset at 2 GB.
//!
//! This module builds the other shape. The ISO's root filesystem IS the
//! kuma image, squashed once, so the live session and the installer
//! environment are the same bytes and the second root filesystem simply
//! does not exist. Measured inputs: the image is 3.29 GB unpacked and
//! 1.20 GB as a solid zstd stream, so the ISO lands near 1.5 GB.
//!
//! bib cannot produce this. Its `iso` type is an alias for `anaconda-iso`
//! (both are `Legacy: true` in `imagetypes.go` and route to the same
//! generator), and `bootc-installer`, its one container-native ISO type,
//! refuses to generate a manifest without an embedded payload:
//! `error: cannot generate manifest: no installer payload bootc ref set`.
//! An embedded payload is the image a second time, 1.47 GB of already
//! compressed layers, which is worse than the Anaconda media it replaces.
//!
//! The on-disk layout follows the "container-native ISO contract v0.1.0"
//! that ublue-os/titanoboa implements, so media built here boots the way
//! theirs does: the root filesystem at `/LiveOS/squashfs.img`, kernel and
//! initramfs under `/images/pxeboot`, GRUB finding both by volume label.
//! Kuma assembles it directly rather than depending on titanoboa, because
//! the whole of the work is mksquashfs and xorriso in a Fedora container,
//! which is the same shell-out-to-podman shape as every other kuma build
//! step, and one script here is cheaper than a build dependency.
//!
//! The cost of this shape is that the ISO carries no container image, so
//! installing from the live session pulls one over the network. That is
//! nearly free for kuma specifically: first boot converges flatpaks and
//! brew from the network anyway, so an offline install would produce a
//! machine that cannot finish converging.

use crate::config::{Config, Desktop};

/// The ISO's volume label. GRUB searches for the boot device by it, and
/// the kernel finds the squashfs through `root=live:CDLABEL=`, so the
/// two must agree. Keeping it in one const is the enforcement.
pub const ISO_LABEL: &str = "KUMA";

/// The hostname live media carries.
///
/// Not the declaration's. `system.hostname` is baked into every image,
/// so without an override the ISO greets a stranger as
/// `liveuser@<the machine that built it>`. That is the same category as
/// the rule that a published image declares no `[user]`: shareable media
/// must not carry the identity of whoever produced it.
pub const LIVE_HOSTNAME: &str = "kuma";

/// The marker `kuma doctor` reads to know it is running on installer
/// media rather than on a machine.
///
/// Needed because the live layer masks the convergence timers on
/// purpose, and doctor grades a machine on those timers being active.
/// Without the marker the first `kuma doctor` a newcomer ever runs
/// reports failed checks for drift that was deliberate, which is both a
/// bad first impression and simply untrue.
pub const LIVE_MARKER: &str = "/usr/lib/kuma/live";

/// The image this media was built from, recorded beside the marker.
///
/// Installing pulls an image from a registry rather than copying the
/// media, so without this a bare `kuma install` wrote the published
/// image whatever the media happened to be: somebody could build media
/// from their own declaration, boot it, watch their own desktop, install
/// it, and get a different system. Recorded rather than assumed, because
/// only the build knows the answer.
///
/// A separate file from the marker, which says where kuma is running.
/// This says what it came from, and media built by an older kuma has the
/// first and not the second, which is a case that has to work anyway.
pub const LIVE_SOURCE: &str = "/usr/lib/kuma/live-source";

/// What makes container storage work on a live root.
///
/// The whole file rather than a drop-in: containers-storage reads one
/// storage.conf and has no conf.d, so this replaces Fedora's. It says
/// the same thing Fedora's does apart from the mount program, and it
/// exists only inside the ISO, so no installed machine inherits it.
pub const LIVE_STORAGE_CONF: &str = "\
# Written by kuma for live media. The live root is an overlayfs, and the
# overlay driver cannot stack on one without a mount program.
[storage]
driver = \"overlay\"
runroot = \"/run/containers/storage\"
graphroot = \"/var/lib/containers/storage\"

[storage.options.overlay]
mount_program = \"/usr/bin/fuse-overlayfs\"
";

/// The account a live session logs in as.
///
/// A published image declares no `[user]` on purpose, since `[user]` is a
/// property of the image and rides into every machine installed from it.
/// So the ISO cannot borrow the declaration's account and makes its own,
/// which exists only in the derived live layer and never reaches an
/// installed machine.
pub const LIVE_USER: &str = "liveuser";

/// The bootc-preset units the live layer masks on its own behalf:
/// every ostree deployment's presets boot them, so no kuma block is
/// what enables them and their disposition is nobody else's to declare.
const LIVE_PRESET_MASKS: &[&str] = &["boot-complete.target", "bootloader-update.service"];

/// The Containerfile for the live root filesystem: kuma's own image plus
/// the two things a live boot needs and an installed machine never does.
///
/// This layers on top of the built image rather than changing what every
/// kuma image contains. An installed machine mounts its root from disk
/// and has no use for dracut's live modules, and regenerating an
/// initramfs in every build would put a slow step whose failure mode is
/// an unbootable machine into the path of every `kuma build`. Here the
/// derived image is throwaway: if it breaks, one ISO fails to assemble.
///
/// It also means the live rootfs and the installed system deliberately
/// differ, which is only sound because the ISO carries no payload. What
/// gets installed is pulled from a registry, so nothing added here can
/// leak onto the installed machine.
pub fn live_containerfile(config: &Config, base_tag: &str) -> String {
    let mut out = String::new();
    out.push_str("# Generated by kuma for `kuma iso --live`. Throwaway: this is the\n");
    out.push_str("# ISO's live root filesystem, not an image anyone installs.\n");
    out.push_str(&format!("FROM {base_tag}\n\n"));

    // dracut-live carries the dmsquash-live module. Weak deps off for the
    // same reason the rest of kuma installs that way: a live rootfs that
    // pulls in recommends is a bigger squashfs for no stated reason.
    out.push_str(
        "RUN dnf install -y --setopt=install_weak_deps=False dracut-live \\\n \
         && dnf clean all\n\n",
    );

    // The image ships an initramfs built for mounting a root from disk.
    // Rebuild it in place with the live modules.
    //
    // The single-kernel assertion is not defensive padding: dracut takes
    // the version as an argument, `ls` would hand it two names as one
    // string, and the resulting initramfs would be built for a kernel
    // that does not exist. Failing here is much cheaper than finding out
    // at the boot that hangs.
    out.push_str("RUN set -eux; \\\n");
    out.push_str("    kver=\"$(ls /usr/lib/modules)\"; \\\n");
    out.push_str("    test \"$(echo \"$kver\" | wc -l)\" -eq 1; \\\n");
    out.push_str("    dracut --force --no-hostonly --add dmsquash-live \\\n");
    out.push_str("      \"/usr/lib/modules/$kver/initramfs.img\" \"$kver\"\n\n");

    // podman cannot work at all in a live session without this.
    //
    // The live root is an overlayfs, and podman's native overlay driver
    // refuses to stack overlay on overlay: "'overlay' is not supported
    // over overlayfs, a mount_program is required". fuse-overlayfs is
    // that mount program. Without it `kuma install` dies before it
    // reaches a disk, because pulling the image it installs needs
    // container storage like anything else does.
    //
    // Configured explicitly rather than left to auto-detection. podman
    // does look for fuse-overlayfs when native overlay is unsupported,
    // but the install path runs both rootless (the derived layer) and
    // under sudo (bootc install), and a fallback that silently differs
    // between the two is not something to discover on somebody else's
    // hardware.
    out.push_str(
        "RUN dnf install -y --setopt=install_weak_deps=False fuse-overlayfs \\\n \
         && dnf clean all\n",
    );
    out.push_str("COPY live-storage.conf /etc/containers/storage.conf\n\n");

    // A browser, in the live layer only.
    //
    // Kuma's app layer is flatpaks in the declaration, converged at first
    // boot, and this layer masks that convergence because it would pull
    // an app set into a RAM overlay. So a live desktop would otherwise
    // ship with no browser at all, which on media whose whole job is
    // "try this before installing" is the one missing app anybody
    // notices, and the one they need to go read about what they are
    // trying.
    //
    // The rpm rather than preinstalling the flatpak: 283 MiB installed
    // against 700 MiB or so for a flatpak runtime plus the app, and no
    // writable /var needed during the build. It reaches no installed
    // machine, so the declaration keeps owning the real app layer and
    // [[app layering]] reversibility is untouched: nothing here persists,
    // so there is nothing to reverse.
    //
    // No mimeapps fix, checked rather than assumed: Fedora's rpm ships
    // /usr/share/applications/org.mozilla.firefox.desktop, the same id
    // the flatpak exports and the same one kuma's baked mimeapps.list
    // already names as the http handler.
    if config.system.desktop != Desktop::None {
        out.push_str(
            "RUN dnf install -y --setopt=install_weak_deps=False firefox \\\n \
             && dnf clean all\n\n",
        );
    }

    // A live session needs somebody to be. No password rather than a
    // known one: the account is ephemeral, it exists only inside a
    // read-only squashfs, and a documented password on install media is a
    // habit worth not starting.
    out.push_str(&format!(
        "RUN useradd -m -G wheel {LIVE_USER} \\\n \
         && passwd -d {LIVE_USER} \\\n \
         && echo '{LIVE_USER} ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/kuma-live \\\n \
         && chmod 0440 /etc/sudoers.d/kuma-live\n\n"
    ));

    // COPY, never a RUN redirect: podman bind-mounts /etc/hostname for
    // the duration of a build, so `echo x > /etc/hostname` writes into
    // the mount and never reaches the layer. Kuma shipped that exact bug
    // once already, and the symptom is a hostname that silently does not
    // change.
    out.push_str("COPY live-hostname /etc/hostname\n\n");

    // The marker doctor reads to know where it is, read by
    // inspect::live_media.
    out.push_str(&format!(
        "RUN mkdir -p /usr/lib/kuma && printf 'live installer media\\n' > {LIVE_MARKER} \\\n \
         && printf '%s\\n' '{base_tag}' > {LIVE_SOURCE}\n\n"
    ));

    // And the kuma that understands that marker.
    //
    // The base image already carries a kuma, baked from current_exe by
    // the build that produced it, so this looks redundant. It is not:
    // this layer is written by whichever kuma is running now, and the
    // marker above means nothing to a kuma too old to look for it. Media
    // built by a new kuma from an image built by an old one would carry
    // the live marker and a binary that ignores it, and the symptom is
    // the exact one this marker exists to fix, a first `kuma doctor`
    // reporting deliberate masking as failed checks.
    //
    // Same current_exe reasoning as the main build, including the
    // `--version` guard: the copy is the build host's binary, so a
    // musl host or a different arch produces an ELF the image cannot
    // execute, and without the guard that surfaces at first boot rather
    // than here.
    out.push_str("COPY --chmod=755 kuma /usr/bin/kuma\n");
    out.push_str("RUN /usr/bin/kuma --version\n\n");

    // kuma's boot convergence must not run in a live session.
    //
    // Every one of these does the right thing on a real machine and the
    // wrong thing here: kuma-user-sync would create the declaration's
    // account on media that is supposed to carry nobody's identity, and
    // the flatpak and brew syncs would download an app set into a RAM
    // overlay on someone else's network. Masking rather than disabling,
    // because these are pulled in by targets rather than only enabled.
    //
    // kuma-fstab-sync and kuma-boot-health-sync need no masking: both
    // carry ConditionPathExists=/run/ostree-booted, and a live boot is
    // not an ostree boot, so they skip themselves.
    //
    // greenboot and bootupd are masked for a different reason, found by
    // reading a real live boot's journal rather than by reasoning: they
    // do not skip, they FAIL. Every one of them wants a deployment to
    // act on, and a live session has none, so a first boot came up with
    // greenboot-healthcheck, greenboot-set-rollback-trigger and
    // bootloader-update failed and two targets dependency-failed behind
    // them. Harmless in effect and unacceptable in appearance: kuma's
    // own doctor grades a machine on having no failed units, so the
    // media would hand a newcomer five red lines and a doctor that
    // agrees with them.
    // The kuma units the block tables declared Masked, plus the two
    // presets this layer owns. One list the tables can't forget a unit
    // in, because it is the same list that enables them.
    let mut masked = crate::containerfile::live_masks();
    masked.extend_from_slice(LIVE_PRESET_MASKS);
    out.push_str(&format!("RUN systemctl mask {}\n\n", masked.join(" ")));

    // Autologin, by the same greetd initial_session mechanism the
    // declared-user path uses, into whichever greeter this desktop runs.
    // Nobody should have to guess an account name to look at the desktop
    // they are deciding whether to install.
    //
    // The sed is load-bearing and its absence is not a cosmetic bug: a
    // declaration with autologin already put an [initial_session] in
    // this file, appending a second one makes two tables of the same
    // name, and TOML forbids that. greetd then fails to parse its config
    // and the live session never starts at all. Deleting from the header
    // to end of file is safe because kuma's generator appends that table
    // last, and a no-op when the declaration never set one.
    //
    // Which greeter and which session command come from containerfile,
    // the same constants the declared-user path uses. Copying them would
    // mean a session command could change and break autologin here only,
    // in the copy nothing in CI ever boots.
    let (file, command) = match config.system.desktop {
        Desktop::Niri => (crate::containerfile::GREETD_CONF, crate::containerfile::NIRI_SESSION),
        Desktop::Cosmic => {
            (crate::containerfile::COSMIC_GREETER_CONF, crate::containerfile::COSMIC_SESSION)
        }
        // A desktopless image has no greeter to autologin into. The live
        // session is a console, and the account is still there to use.
        Desktop::None => return out,
    };
    out.push_str(&format!(
        "RUN sed -i '/^\\[initial_session\\]/,$d' {file} \\\n \
         && printf '\\n[initial_session]\\ncommand = \"{command}\"\\nuser = \"{LIVE_USER}\"\\n' >> {file}\n"
    ));
    out
}

/// Assembles the ISO from a live rootfs.
///
/// Runs inside a Fedora container with the live image mounted read-only
/// at /rootfs and the output directory at /output. It builds nothing
/// from the host: mksquashfs, xorriso and the FAT tools are installed
/// into the throwaway container, which is why the host needs no new
/// dependency to build installer media.
///
/// UEFI only, deliberately. BIOS boot needs an El Torito image built
/// with grub2-mkimage and a second set of xorriso flags, and copying the
/// i386-pc modules without that produces an ISO that carries BIOS
/// machinery and still cannot boot a BIOS machine. Better to not claim
/// it. Note this when testing: a VM has to be given UEFI firmware.
///
/// Takes the label and paths as arguments rather than baking them, so a
/// test can run the script against a fixture tree without owning the
/// real one, the same reason `scan_etc` takes its roots.
///
/// The `#` inside the awk-free heredocs is fine, but grub.cfg contains
/// `"#`-free text only by accident, so this stays an r##"…"## literal for
/// the same reason FSTAB_SYNC_SCRIPT does.
pub const BUILD_ISO_SCRIPT: &str = r##"#!/usr/bin/bash
set -euo pipefail

label="${1:?iso volume label}"
rootfs="${2:-/rootfs}"
out="${3:-/output}"

dnf install -y --setopt=install_weak_deps=False \
    squashfs-tools xorriso mtools dosfstools >/dev/null

work=$(mktemp -d)
mkdir -p "$work"/iso-root/{images/pxeboot,LiveOS} "$work"/EFI

# Same single-kernel reasoning as the live Containerfile: two kernels
# here would silently copy one image and boot the other's modules.
# Resolved before mksquashfs because the exclude list below names the
# kernel's files by version.
kver="$(ls "$rootfs/usr/lib/modules")"
[ "$(echo "$kver" | wc -l)" -eq 1 ] || { echo >&2 "expected exactly one kernel, got: $kver"; exit 1; }

# The live root filesystem, in one file.
#
# sysroot and ostree are an installed machine's deployment machinery and
# mean nothing to a live boot, which mounts this squashfs as / directly.
# The kernel and initramfs are excluded for the same reason, one level
# up: they are boot artifacts, and the copies this ISO actually loads
# are the ones under images/pxeboot that GRUB names. The pair inside the
# rootfs serves an installed machine's kernel-update machinery, which a
# throwaway live root never runs. The initramfs is the expensive one —
# 242 MB since v0.18 put plymouth in it, because a splash needs a
# framebuffer and dracut's drm module then packs every GPU driver's
# firmware into a --no-hostonly build — and it is already a zstd
# stream, so squashfs stores it at full size. Carrying it twice took
# the ISO from 1.80 GB to 2.11 GB and over the release-asset budget;
# measured, not estimated, on the niri image of 2026-08-27.
# zstd at 19 because this number is the ISO's size: the whole point of
# the exercise is fitting under a 2 GB release asset cap.
#
# -e goes LAST and takes every pattern, because mksquashfs treats every
# remaining argument as an exclude pattern once it sees -e. Writing it
# in the middle does not error: it silently excludes files named "-comp"
# and "19", and falls back to gzip. That cost 60 MB here and it is
# invisible unless you read mksquashfs's summary line, which is how the
# same ordering survives upstream in titanoboa's build_iso.sh.
# NOT -all-root, though every example of this you will find uses it.
# It forces every file in the squashfs to root:root, and a live session
# then cannot write to its own home: liveuser's directory is mode 0700,
# so owning it as root locks the live user out of it entirely. What that
# looks like from the outside is niri erroring, no wallpaper, and a
# terminal that will not open, because everything touching $HOME dies
# the same way. The damage is wider than the live user: every service
# account's directory in the image loses its owner too. Fedora's own
# live media gets away with -all-root because it creates its liveuser at
# boot on the writable overlay rather than baking one into the image.
mksquashfs "$rootfs" "$work/iso-root/LiveOS/squashfs.img" \
    -noappend -comp zstd -Xcompression-level 19 \
    -e sysroot ostree \
       "usr/lib/modules/$kver/initramfs.img" \
       "usr/lib/modules/$kver/vmlinuz"

# The boot pair GRUB actually loads. Copied from the rootfs, not the
# squashfs, so the excludes above do not touch this.
cp "$rootfs/usr/lib/modules/$kver/vmlinuz" "$work/iso-root/images/pxeboot/vmlinuz"
cp "$rootfs/usr/lib/modules/$kver/initramfs.img" "$work/iso-root/images/pxeboot/initrd.img"

# The EFI payload is NOT at /boot/efi. In a bootc image /boot is empty:
# bootupd populates it at install time from what ships unpacked under
# /usr/lib/efi, split between the shim and grub2 packages, each in its
# own version-named directory. The ISO needs both, merged. Globbing is
# the only way to name a version nobody chose; asserting a single match
# keeps a second installed version from being picked arbitrarily.
shim_efi=("$rootfs"/usr/lib/efi/shim/*/EFI)
grub_efi=("$rootfs"/usr/lib/efi/grub2/*/EFI)
[ "${#shim_efi[@]}" -eq 1 ] && [ -d "${shim_efi[0]}" ] || {
    echo >&2 "expected exactly one shim EFI tree, got: ${shim_efi[*]}"; exit 1; }
[ "${#grub_efi[@]}" -eq 1 ] && [ -d "${grub_efi[0]}" ] || {
    echo >&2 "expected exactly one grub2 EFI tree, got: ${grub_efi[*]}"; exit 1; }
cp -aT "${shim_efi[0]}" "$work/EFI"
cp -aT "${grub_efi[0]}" "$work/EFI"

# Firmware booting removable media looks for /EFI/BOOT/BOOTX64.EFI and
# nothing else, and the shim it finds there chainloads grub from that
# same directory. Fedora's own live media is laid out this way. Leaving
# grub only under the vendor directory yields a stick that boots on a
# machine already registered for Fedora and nowhere else, which is a
# failure that looks like bad hardware rather than a bad ISO.
cp "$work/EFI/fedora/grubx64.efi" "$work/EFI/BOOT/grubx64.efi"
cp "$work/EFI/fedora/mmx64.efi" "$work/EFI/BOOT/mmx64.efi" 2>/dev/null || true

# rd.live.image tells dracut this root is a live image; CDLABEL is how it
# finds the ISO again from inside the initramfs, which is why the label
# is one constant on the kuma side. overlayfs rather than the older
# device-mapper snapshot: writes in the live session go to RAM either
# way, but overlayfs does not have a fixed overlay size to run out of.
#
# enforcing=0 is NOT laziness and should not be removed without reading
# this. A container image's real SELinux labels are not reachable from
# either kind of podman mount: storage relabels image content, so every
# file reads back as container_ro_file_t (or container_file_t through a
# running container) rather than bin_t, shell_exec_t and the rest. The
# labels an installed machine has are applied by bootc at install time
# from the policy inside the image, which is why an installed kuma is
# correctly labelled and a squashfs of the same bytes is not. Booting
# that enforcing denies almost everything systemd tries to do. Fedora's
# whole bootc live-media ecosystem does the same thing for the same
# reason: Bazzite's ISO ships `enforcing=0 rd.live.image` verbatim.
#
# The blast radius is the live session only. What gets installed is
# pulled from a registry and labelled by bootc, so an installed machine
# is enforcing from its first boot. The real fix, if this ever matters
# enough, is relabelling the tree with setfiles against the image's own
# file_contexts before squashing, which needs a writable mount and the
# privilege to write security.* xattrs.
#
# Both entries carry a serial console, and tty0 is last so the screen
# stays the primary one: on hardware without a serial port the kernel
# drops ttyS0 and nothing changes, and in a VM the whole boot becomes
# readable. That matters twice. `kuma iso` names GNOME Boxes in its own
# help, so a VM is where most people meet this ISO first, and a live
# boot that fails on somebody else's machine otherwise produces a
# photograph of a screen as its only evidence.
read -r -d '' grub_cfg <<EOF || true
set timeout=10
set default=0
set menu_auto_hide=false

# insmod, and no load_video: that is a function Fedora's own grub.cfg
# defines, not a grub builtin, so calling it here printed
# "can't find command \`load_video'" on every single boot of the ISO.
# Harmless, and the first thing a person sees when they try kuma, which
# makes it exactly the wrong place to leave a red error. insmod all_video
# already loads every video driver grub has.
insmod all_video
set gfxpayload=keep
insmod gzio
insmod part_gpt
insmod chain

search --no-floppy --set=root -l '$label'

menuentry 'Try kuma' {
  linux /images/pxeboot/vmlinuz root=live:CDLABEL=$label rd.live.image rd.live.overlay.overlayfs=1 enforcing=0 console=ttyS0,115200 console=tty0 quiet
  initrd /images/pxeboot/initrd.img
}

menuentry 'Try kuma (verbose)' {
  linux /images/pxeboot/vmlinuz root=live:CDLABEL=$label rd.live.image rd.live.overlay.overlayfs=1 enforcing=0 console=ttyS0,115200 console=tty0
  initrd /images/pxeboot/initrd.img
}
EOF

for dir in "$work"/EFI/*; do
    [ -d "$dir" ] || continue
    echo "$grub_cfg" > "$dir/grub.cfg"
done

# Fedora also keeps a copy at /EFI on the ISO; grub's compiled-in prefix
# looks for it there.
cp -aT "$work/EFI" "$work/iso-root/EFI"

# The EFI system partition, sized to what goes in it. FAT32 needs room
# for its own tables, so 64M is the floor rather than the content size.
esp_mb=$(du -sm "$work/EFI" | cut -f1)
esp_mb=$(( esp_mb + 32 ))
[ "$esp_mb" -ge 64 ] || esp_mb=64
truncate -s "${esp_mb}M" "$work/uefi.img"
mkfs.fat -F32 "$work/uefi.img" >/dev/null
mcopy -i "$work/uefi.img" -s "$work"/EFI ::

xorriso -as mkisofs \
    -R -J \
    -V "$label" \
    -partition_offset 16 \
    -appended_part_as_gpt \
    -append_partition 2 C12A7328-F81F-11D2-BA4B-00A0C93EC93B "$work/uefi.img" \
    -iso_mbr_part_type EBD0A0A2-B9E5-4433-87C0-68B6B72699C7 \
    -e --interval:appended_partition_2:all:: \
    -no-emul-boot \
    -iso-level 3 \
    -o "$out/$label.iso" \
    "$work/iso-root"

rm -rf "$work"
"##;

#[cfg(test)]
mod tests {
    use crate::config::Config;

    fn config(toml: &str) -> Config {
        let config: Config = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        config
    }

    /// The live layer's whole reason to exist is the initramfs: without
    /// dmsquash-live the ISO assembles fine and then hangs at boot with
    /// no root filesystem, which is an expensive way to find out.
    #[test]
    fn the_live_layer_rebuilds_the_initramfs_with_the_live_module() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "localhost/kuma:latest",
        );
        assert!(out.contains("dracut-live"));
        assert!(out.contains("--add dmsquash-live"));
        assert!(out.contains("initramfs.img"));
    }

    /// A rebuild against two kernels would build an initramfs for a
    /// version that is not there. Cheap to assert, silent if it happens.
    #[test]
    fn the_live_layer_refuses_more_than_one_kernel() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "localhost/kuma:latest",
        );
        assert!(out.contains("wc -l"));
    }

    /// Nobody should have to guess an account name to look at the
    /// desktop they are deciding whether to install.
    #[test]
    fn each_desktop_autologins_into_its_own_greeter() {
        let niri = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "t",
        );
        assert!(niri.contains("/etc/greetd/config.toml"));
        assert!(niri.contains("niri-session"));
        assert!(!niri.contains("cosmic-greeter.toml"));

        let cosmic = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"),
            "t",
        );
        assert!(cosmic.contains("/etc/greetd/cosmic-greeter.toml"));
        assert!(cosmic.contains("start-cosmic"));

        // A desktopless image has no greeter to autologin into, but the
        // account still has to exist for the console.
        let bare = super::live_containerfile(&config("schema_version = 1\n"), "t");
        assert!(!bare.contains("initial_session"));
        assert!(bare.contains(super::LIVE_USER));
    }

    /// Found in a built image, not by reasoning: a declaration with
    /// autologin already wrote an [initial_session], so appending a
    /// second gives greetd a config with two tables of one name. TOML
    /// forbids that, greetd fails to parse, and the live session never
    /// starts. The ISO builds perfectly and boots to nothing.
    #[test]
    fn autologin_replaces_the_declarations_initial_session_rather_than_appending() {
        let out = super::live_containerfile(
            &config(
                "schema_version = 1\n[system]\ndesktop = \"niri\"\n[user]\nname = \"mira\"\nautologin = true\n",
            ),
            "t",
        );
        assert!(out.contains(r"sed -i '/^\[initial_session\]/,$d' /etc/greetd/config.toml"));
        assert_eq!(out.matches("printf '\\n[initial_session]").count(), 1);
    }

    /// Kuma's convergence is right on a machine and wrong on live media:
    /// it would create the declaration's account on media meant to carry
    /// nobody's identity, and pull an app set into a RAM overlay.
    #[test]
    fn the_live_layer_masks_kumas_convergence() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "t",
        );
        for unit in [
            "kuma-user-sync.service",
            "kuma-flatpak-sync.timer",
            "kuma-brew-sync.timer",
            // These three FAIL rather than skip in a live session, and
            // kuma grades a machine on having none of those.
            "greenboot-healthcheck.service",
            "greenboot-set-rollback-trigger.service",
            "bootloader-update.service",
        ] {
            assert!(out.contains(unit), "{unit} not masked in the live layer");
        }
    }

    /// Found in a live session's shell prompt: `liveuser@` the builder's
    /// own hostname.
    /// The declaration's hostname is baked into every image, so media
    /// built from a real declaration announces the machine that built
    /// it. COPY rather than a redirect because podman bind-mounts
    /// /etc/hostname during a build.
    #[test]
    fn the_iso_does_not_carry_the_builders_hostname() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\nhostname = \"testbox\"\n"),
            "t",
        );
        assert!(out.contains("COPY live-hostname /etc/hostname"));
        assert!(!out.contains("testbox"));
        assert!(!out.contains("RUN echo"), "a redirect never reaches the layer");
    }

    /// Found by running `kuma install --yes` in a live session, which
    /// died before touching a disk: "'overlay' is not supported over
    /// overlayfs, a mount_program is required". The live root IS an
    /// overlayfs, so podman cannot do anything at all there without
    /// fuse-overlayfs, and installing needs podman to pull the image it
    /// installs. Nothing about this is visible from a build.
    #[test]
    fn container_storage_works_on_an_overlay_root() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "t",
        );
        assert!(out.contains("fuse-overlayfs"));
        assert!(out.contains("COPY live-storage.conf /etc/containers/storage.conf"));
        assert!(super::LIVE_STORAGE_CONF.contains("mount_program = \"/usr/bin/fuse-overlayfs\""));
    }

    /// doctor grades a machine on convergence timers the live layer
    /// masks on purpose, so without this marker the first doctor a
    /// newcomer runs reports deliberate design as failure.
    ///
    /// The binary ships beside it deliberately. The base image already
    /// has one, but it was baked by whichever kuma built that image, and
    /// a kuma too old to look for the marker ignores it completely,
    /// which reproduces the exact bug the marker fixes.
    #[test]
    fn the_live_layer_marks_itself_as_media_and_ships_a_kuma_that_reads_it() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "t",
        );
        assert!(out.contains(super::LIVE_MARKER));
        assert!(out.contains("COPY --chmod=755 kuma /usr/bin/kuma"));
        assert!(out.contains("RUN /usr/bin/kuma --version"), "guard against an unrunnable ELF");
    }

    /// The media records what it was built from, because installing
    /// pulls an image rather than copying the media, and without this a
    /// bare `kuma install` wrote kuma's published image whatever the
    /// person was looking at.
    #[test]
    fn the_live_layer_records_the_image_it_was_built_from() {
        let out = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "ghcr.io/someone/kuma:cosmic",
        );
        assert!(out.contains(super::LIVE_SOURCE));
        assert!(out.contains("'ghcr.io/someone/kuma:cosmic' > /usr/lib/kuma/live-source"));
        // Beside the marker and in the same layer: media carrying one and
        // not the other is media that says where it is without saying
        // what it is.
        let marker_at = out.find(super::LIVE_MARKER).unwrap();
        let source_at = out.find(super::LIVE_SOURCE).unwrap();
        assert!(source_at > marker_at);
        assert_eq!(out[marker_at..source_at].matches("RUN ").count(), 0);
    }

    /// Live media masks flatpak convergence, so without this a live
    /// desktop has no browser: the one app somebody trying an OS needs
    /// in order to go read about the OS they are trying. Console media
    /// has no desktop to browse from and pays nothing for it.
    #[test]
    fn a_live_desktop_carries_a_browser_and_a_console_does_not() {
        let desktop = super::live_containerfile(
            &config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
            "t",
        );
        assert!(desktop.contains("firefox"));
        // The rpm and the flatpak export the same desktop id, so the
        // baked mimeapps.list needs no rewriting. Pinned because a
        // rewrite appearing here would mean that stopped being true.
        assert!(!desktop.contains("mimeapps"));

        let console = super::live_containerfile(&config("schema_version = 1\n"), "t");
        assert!(!console.contains("firefox"));
    }

    /// The live account is the ISO's, not the declaration's: a published
    /// image declares no [user], and one that did must not have its
    /// account quietly become the live session's.
    #[test]
    fn the_live_account_is_the_isos_own() {
        let out = super::live_containerfile(
            &config(
                "schema_version = 1\n[system]\ndesktop = \"niri\"\n[user]\nname = \"mira\"\nautologin = true\n",
            ),
            "t",
        );
        assert!(out.contains("useradd -m -G wheel liveuser"));
        assert!(!out.contains("mira"));
    }

    /// GRUB searches for the boot device by label and the kernel finds
    /// the squashfs by the same label. They are written in two different
    /// places by two different tools, so pin that they agree.
    #[test]
    fn the_label_reaches_both_grub_and_the_kernel() {
        assert!(super::BUILD_ISO_SCRIPT.contains("root=live:CDLABEL=$label"));
        assert!(super::BUILD_ISO_SCRIPT.contains("search --no-floppy --set=root -l '$label'"));
        assert!(super::BUILD_ISO_SCRIPT.contains("-V \"$label\""));
    }

    /// Both halves of what makes a live boot legible, and both were found
    /// by booting the ISO rather than by reading it.
    ///
    /// `load_video` is a function Fedora's own grub.cfg defines and not a
    /// grub builtin, so calling it printed a red `can't find command`
    /// error on every boot of the installer media. `insmod all_video`
    /// already does the work.
    ///
    /// The serial console is what lets anything observe a live boot at
    /// all: CI asserts against it, and a person whose live session died
    /// on their own hardware otherwise has a photograph of a screen. tty0
    /// comes last so the display stays the primary console.
    #[test]
    fn the_live_boot_is_quiet_about_grub_and_loud_on_serial() {
        // The bare command, not the word: the comment above it in the
        // script explains why it is gone and would match a substring
        // search, which is how this assertion first failed on itself.
        assert!(
            !super::BUILD_ISO_SCRIPT.lines().any(|l| l.trim() == "load_video"),
            "load_video is not a grub builtin; it errors on every boot"
        );
        assert!(super::BUILD_ISO_SCRIPT.contains("insmod all_video"));
        let entries: Vec<&str> = super::BUILD_ISO_SCRIPT
            .lines()
            .filter(|l| l.contains("linux /images/pxeboot"))
            .collect();
        assert_eq!(entries.len(), 2, "both menu entries are the boot paths people take");
        for entry in entries {
            assert!(entry.contains("console=ttyS0,115200"), "no serial console: {entry}");
            let serial = entry.find("console=ttyS0").unwrap();
            let screen = entry.find("console=tty0").expect("tty0 must be listed");
            assert!(screen > serial, "tty0 must come last or the screen stops being primary");
        }
    }

    /// The bug the first fixture hid, and the reason fixtures get built
    /// from what the real image has rather than what the script wants: a
    /// bootc image's /boot is EMPTY, because bootupd fills it at install
    /// time from /usr/lib/efi. Reading /boot/efi/EFI aborts the build on
    /// every real kuma image.
    #[test]
    fn the_efi_payload_comes_from_usr_lib_efi_not_boot() {
        assert!(super::BUILD_ISO_SCRIPT.contains("/usr/lib/efi/shim/*/EFI"));
        assert!(super::BUILD_ISO_SCRIPT.contains("/usr/lib/efi/grub2/*/EFI"));
        assert!(!super::BUILD_ISO_SCRIPT.contains("/boot/efi/EFI"));
    }

    /// Firmware booting a USB stick looks only for
    /// /EFI/BOOT/BOOTX64.EFI, and the shim there chainloads grub from
    /// the same directory. Grub living only under the vendor directory
    /// gives a stick that boots on a machine already registered for
    /// Fedora and nowhere else.
    #[test]
    fn grub_reaches_the_removable_media_path() {
        assert!(super::BUILD_ISO_SCRIPT.contains("$work/EFI/BOOT/grubx64.efi"));
    }

    /// The deployment machinery of an installed machine has no meaning
    /// in a live boot, and sysroot in particular would carry a second
    /// copy of the entire root filesystem into the squashfs.
    ///
    /// The ordering is the load-bearing part. `-e` swallows every
    /// argument after it, so `-e sysroot -e ostree -comp zstd` excludes
    /// files named "-comp" and "zstd" and quietly builds a gzip
    /// filesystem instead. It costs about 60 MB against a cap this
    /// whole module exists to stay under, and nothing fails: the only
    /// evidence is one word in mksquashfs's summary.
    #[test]
    fn the_squashfs_excludes_the_deployment_machinery() {
        let script = super::BUILD_ISO_SCRIPT;
        assert!(script.contains("-e sysroot ostree"));
        assert!(script.contains("-comp zstd -Xcompression-level 19"));
        let comp = script.find("-comp zstd").unwrap();
        let exclude = script.find("-e sysroot").unwrap();
        assert!(comp < exclude, "-e must come last or it eats the compressor flags");
    }

    /// The kernel and initramfs ride the ISO once, under images/pxeboot,
    /// because those are the copies GRUB loads. The same files inside
    /// the squashfs serve an installed machine's kernel-update
    /// machinery, which a throwaway live root never runs — and the
    /// initramfs is a 242 MB zstd stream that squashfs cannot shrink, so
    /// carrying it twice is what took the ISO from 1.80 GB to 2.11 GB
    /// and over the release-asset budget in v0.18. The splash grew it;
    /// the doubling made it fatal.
    #[test]
    fn the_squashfs_carries_no_boot_artifacts() {
        let script = super::BUILD_ISO_SCRIPT;
        assert!(
            script.contains("\"usr/lib/modules/$kver/initramfs.img\""),
            "the initramfs is the ISO's biggest file; it rides once, in pxeboot"
        );
        assert!(script.contains("\"usr/lib/modules/$kver/vmlinuz\""));
        // The exclude list names the kernel's files by version, so the
        // version has to be resolved before mksquashfs runs.
        let kver = script.find("kver=\"$(ls").unwrap();
        let exclude = script.find("usr/lib/modules/$kver/initramfs.img").unwrap();
        assert!(kver < exclude, "kver must exist before the exclude list names it");
    }

    /// Found by booting: `-all-root` makes every file root-owned, and
    /// liveuser's 0700 home then belongs to root, so the live user
    /// cannot write to it. Symptom is a broken desktop with no obvious
    /// cause, since what fails is every program that touches $HOME.
    /// Every copy of this script in the wild passes `-all-root`.
    #[test]
    fn the_squashfs_keeps_ownership() {
        // Comment lines are stripped first: the reason this flag is
        // absent is written out above the command, and a naive search
        // finds the explanation and fails on it.
        let code: String = super::BUILD_ISO_SCRIPT
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!code.contains("-all-root"), "-all-root locks the live user out of its own home");
        assert!(code.contains("mksquashfs"));
    }

    /// A live session boots permissive because a container image's real
    /// SELinux labels are not reachable through a podman mount. Pinned
    /// with its reasoning next to it so nobody removes it as an
    /// oversight; the long version is in the script.
    #[test]
    fn the_live_session_boots_permissive_on_purpose() {
        assert!(super::BUILD_ISO_SCRIPT.contains("enforcing=0"));
    }
}
