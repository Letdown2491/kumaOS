//! The swapfile a machine hibernates into, and the two ways it gets one.
//!
//! Hibernate is four things and a swapfile is only the first: swap on
//! persistent storage, a `resume=` naming the device that holds it, a
//! `resume_offset=` giving that file's physical position inside the
//! device, and an initramfs that acts on both. Every kuma image already
//! carries the fourth, because dracut's `resume` module and
//! `systemd-hibernate-resume` ride in the initramfs Fedora builds. This
//! module supplies the other three, and grades them afterwards.
//!
//! The offset is why none of this can live in a declaration. It is the
//! physical block address of one file on one disk, so it is not a fact
//! about a system, it is a fact about a machine, in the same family as
//! the partition table and the LUKS passphrase. A published image cannot
//! know it and two machines built from one declaration do not share it.
//! So `kuma install` asks for a size and computes the rest, and
//! `kuma hibernate` does the same on a machine that is already running.
//!
//! The file gets its own top-level subvolume rather than a directory
//! under `/var`, for three reasons that all have teeth:
//!
//! - `bootc install to-filesystem` requires the root it is given to be
//!   empty, so at install time the file cannot be anywhere beneath it.
//!   Beside it, at the filesystem top level, it can be made before the
//!   image is pulled, which is also when running out of disk is cheap to
//!   discover.
//! - btrfs refuses to swapon a file whose extents are shared, so a
//!   snapshot of whatever subvolume held it would break resume at the
//!   next boot rather than at the moment of the snapshot. Its own
//!   subvolume is never a snapshot source.
//! - It is a name doctor can look for on both kinds of machine. A kuma
//!   install puts `/var` inside the root subvolume and an Anaconda
//!   install gives it one of its own, so "where the swapfile lives" is
//!   otherwise a question with two answers.

use anyhow::{bail, Result};

use crate::host::host_output;

/// The subvolume the swapfile lives in, at the filesystem top level
/// beside `root`.
pub const SUBVOL: &str = "swap";

/// Where that subvolume is mounted on the running machine.
pub const MOUNT: &str = "/var/swap";

/// The swapfile itself, as the running machine sees it.
pub const FILE: &str = "/var/swap/swapfile";

/// The priority the swapfile takes, which is below zram's.
///
/// Every kuma image ships `zram-generator-defaults`, so a machine
/// already has compressed swap in RAM at priority 100, and that is the
/// swap it should keep using: it is faster than a disk and it is what
/// the desktop was sized around. The disk file is not there to be swap.
/// It is there so there is somewhere to put memory when the machine is
/// told to hibernate, and it takes real paging only once zram is full.
///
/// Negative rather than merely low, because that is what the kernel
/// gives an unprioritised area anyway. Naming it means the order of the
/// two areas is written down rather than inferred, and systemd picks the
/// disk file for hibernation regardless: it skips zram devices when it
/// looks for somewhere to write an image, since a device that lives in
/// memory cannot hold a copy of memory.
///
/// What is guaranteed is the *order*, not this exact number. The first
/// machine to be looked at reported -1 for the swapfile against zram's
/// 100, having been asked for -2, so something between the fstab line
/// and `swapon` did not carry it through. Both values sit equally far
/// below zram and behave identically, so the gate asserts that the file
/// is below zram rather than pinning a number that would fail a machine
/// which is behaving correctly.
pub const PRIORITY: i32 = -2;

/// Where the lid's suspend-then-hibernate setting lives, once the
/// swapfile below has given the machine somewhere to hibernate into.
pub const LID_DROPIN: &str = "/etc/systemd/logind.conf.d/kuma-suspend-then-hibernate.conf";

/// The whole of the lid half of hibernate.
///
/// Closing a laptop lid is the dominant suspend trigger, and logind is
/// what answers it, so the setting is a logind drop-in rather than a
/// sleep.conf one: sleep.conf tunes what suspend-then-hibernate does
/// once invoked (its `HibernateDelaySec` and friends) but cannot make
/// anything invoke it, while `HandleLidSwitch` is the switch itself.
/// The shell's own suspend paths stay plain suspend on purpose: its
/// session actions try `systemctl suspend` first and nothing kuma ships
/// should turn an explicit "suspend" button into a hibernate, and its
/// idle behaviors only ever lock and turn the screen off.
///
/// Nothing sets `HibernateDelaySec`, which is the deliberate half of
/// this: with a battery and no delay configured, systemd-sleep wakes on
/// the firmware's own low-battery alarm and hibernates just before the
/// machine would have died, which beats any fixed number kuma could
/// guess. Only a machine with no battery falls back to the 2h default.
///
/// No logind restart anywhere, on any path that writes this. Restarting
/// logind on a running desktop is disruptive, and nothing needs it: the
/// runtime verb's resume kernel arguments demand a reboot, the install
/// path is read at first boot, and both of those reload logind for free.
pub fn lid_dropin() -> String {
    "[Login]\n\
     HandleLidSwitch=suspend-then-hibernate\n\
     HandleLidSwitchExternalPower=suspend-then-hibernate\n"
        .to_string()
}

/// Below this a swapfile is not a hibernate image, it is a mistake with
/// a unit suffix. The smallest machine anyone hibernates has more than a
/// gibibyte of memory, so a smaller file can only be a typo.
const MIN_MIB: u64 = 1024;

/// A size as somebody types it, in MiB, or `None` for the words that
/// decline whatever is being sized.
///
/// The spelling half of every size kuma asks for: the swapfile here, and
/// the two partition sizes an install can name (see `partition`). One
/// reader, because two would drift, and the floors are deliberately not
/// here: a swapfile has a hibernate minimum and a partition has its own
/// reasons, and each caller knows which it is asking for.
///
/// The unit is required. `16` is ambiguous in a way that matters here:
/// read as gibibytes it is a hibernate image, read as mebibytes it is a
/// hundredth of one, and the difference would not surface until the
/// machine was told to hibernate and could not. Refusing costs one
/// retype; guessing costs a session.
pub fn parse_mib(text: &str, what: &str) -> Result<Option<u64>> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    if matches!(lower.as_str(), "none" | "no" | "off" | "0") {
        return Ok(None);
    }
    let (digits, per_unit) =
        if let Some(rest) = lower.strip_suffix("gib").or_else(|| lower.strip_suffix('g')) {
            (rest, 1024)
        } else if let Some(rest) = lower.strip_suffix("mib").or_else(|| lower.strip_suffix('m')) {
            (rest, 1)
        } else {
            bail!("{what} size {text:?} has no unit: write it as 16G, or 16384M");
        };
    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{what} size {text:?} is not a number with a unit"))?;
    Ok(Some(count.saturating_mul(per_unit)))
}

/// A swapfile size, or `None` for "no swapfile": the shared spelling
/// plus the one floor that is hibernate's alone.
pub fn parse_size(text: &str) -> Result<Option<u64>> {
    let Some(mib) = parse_mib(text, "swap")? else {
        return Ok(None);
    };
    if mib < MIN_MIB {
        bail!(
            "swap size {text:?} is {mib}M, too small to hibernate into: \
             a hibernate image is about the size of memory in use"
        );
    }
    Ok(Some(mib))
}

/// How big a swapfile this machine would need, from its own `/proc/meminfo`.
///
/// The size of memory, rounded up to a whole gibibyte. The kernel
/// compresses a hibernate image, so in practice it writes less than
/// this, but "less than memory" is not a number anything can promise:
/// the image is the pages that are dirty when the machine is told to
/// sleep, and a machine with everything dirty is exactly the one
/// somebody is trying to put away. Sizing at memory means the answer is
/// never "it depends what you had open".
///
/// Only ever a default. It is offered, not imposed, and a person who
/// knows their machine never fills memory can type a smaller number and
/// get told what they are trading.
pub fn default_size_mib(meminfo: &str) -> Option<u64> {
    ram_mib(meminfo).map(round_up_gib)
}

/// How much memory this machine has, from its own `/proc/meminfo`.
///
/// Split from the default above because the prompt says both numbers:
/// what it proposes, and the memory it proposed it for. Deriving the
/// second from the first in the caller would be the same arithmetic
/// written twice, and the two would eventually disagree in a sentence
/// that exists to be believed.
pub fn ram_mib(meminfo: &str) -> Option<u64> {
    let kib: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // MemTotal is KiB despite the kB, and reads a little under the
    // installed RAM because firmware keeps some back. That shortfall is
    // not memory the kernel can be asked to save, so this is the right
    // number to size against.
    Some(kib.div_ceil(1024))
}

/// A size rounded up to a whole gibibyte, which is the unit every
/// swapfile here is typed in.
fn round_up_gib(mib: u64) -> u64 {
    mib.div_ceil(1024) * 1024
}

/// Why this size cannot be written to this disk.
///
/// `spare_mib` is what can be given away and still leave a working
/// machine behind, and `spare_is` says what that room is measured
/// against. The caller owns that arithmetic because the two callers
/// measure different things: the installer subtracts a layout that does
/// not exist yet, and the verb subtracts what a mounted filesystem
/// reports free. Empty means go ahead.
pub fn objections(size_mib: u64, spare_mib: u64, spare_is: &str) -> Vec<String> {
    let mut out = Vec::new();
    if size_mib > spare_mib {
        out.push(format!(
            "a {} swapfile does not fit: there is {} to spare, {spare_is}",
            size_text(size_mib),
            size_text(spare_mib)
        ));
    }
    out
}

/// What the installer measures its spare room against.
pub const SPARE_AT_INSTALL: &str = "once the ESP, /boot and a minimum system are accounted for";

/// What the verb measures it against, on a machine that is already full
/// of things.
pub const SPARE_ON_DISK: &str =
    "counting free space on the root filesystem and keeping 10G back for a future update";

/// How much of a running machine's free space is not the swapfile's to
/// take.
///
/// An update stages a whole second deployment before it discards the
/// old one, so a machine with no room for one is a machine that cannot
/// be updated. Filling the disk with a hibernate image and discovering
/// that at the next `kuma update` would be trading a feature for the
/// property the whole system is built on.
pub const RESERVE_MIB: u64 = 10 * 1024;

/// What is true about this swapfile that somebody should hear before the
/// disk is written, and that no later run can tell them as cheaply.
///
/// Warnings, not refusals. Both of these are choices somebody is allowed
/// to make: a smaller file on a machine that never fills memory works
/// fine, and an unencrypted disk is a decision already taken by the time
/// this is asked. Naming them is the job; overruling them is not.
pub fn warnings(size_mib: u64, ram_mib: Option<u64>, encrypted: bool) -> Vec<String> {
    let mut out = Vec::new();
    if !encrypted {
        out.push(
            "NOT encrypted: this disk has no LUKS, so hibernating writes the contents of \
             memory to it in the clear, and encryption cannot be added without installing again"
                .to_string(),
        );
    }
    if let Some(ram_mib) = ram_mib {
        if size_mib < ram_mib {
            out.push(format!(
                "{} is smaller than this machine's {} of memory, so hibernating will fail \
                 whenever more than {} of it is dirty",
                size_text(size_mib),
                size_text(ram_mib),
                size_text(size_mib)
            ));
        }
    }
    out
}

/// What to say, before a swapfile is made, about a kernel that will not
/// hibernate anyway.
///
/// Separate from `warnings` because it is a different kind of fact: the
/// size and the encryption are choices somebody is making, and this is a
/// condition of the machine they are making them on.
///
/// The installer can ask this honestly even though it runs from live
/// media, because live media boots through the same shim and the same
/// firmware: if Secure Boot is on for the installer, it will be on for
/// the machine the installer writes, and both kernels lock down the same
/// way. The verb asks the machine it is standing on, which is simpler.
///
/// A warning rather than a refusal. Secure Boot is a firmware setting
/// somebody can change, and a swapfile made today is still the right
/// swapfile when they do.
pub fn lockdown_warning(kernel_allows: bool, lockdown: Option<&str>) -> Option<String> {
    if kernel_allows {
        return None;
    }
    Some(match lockdown {
        Some("none") | None => {
            "this kernel does not offer hibernation, so the swapfile would sit unused".to_string()
        }
        Some(mode) => format!(
            "this kernel is locked down ({mode}), which booting with Secure Boot turns on, \
             and a locked-down kernel refuses to hibernate. The swapfile would sit unused \
             until Secure Boot is turned off in firmware"
        ),
    })
}

/// A size in MiB as a person reads it.
///
/// Gibibytes wherever the number is one, because every size this asks
/// for is typed in gibibytes and a reader comparing "how much do I have"
/// against "how much am I taking" should not have to divide. The
/// fractional case is the one that matters: free space on a real disk is
/// never a whole gibibyte, and printing it as five digits of mebibytes
/// makes the sentence "a 4096G swapfile does not fit: there is 242959M
/// to spare" a puzzle rather than an answer.
pub fn size_text(mib: u64) -> String {
    if mib % 1024 == 0 {
        format!("{}G", mib / 1024)
    } else if mib >= 1024 {
        format!("{:.1}G", mib as f64 / 1024.0)
    } else {
        format!("{mib}M")
    }
}

/// The shell that makes the swapfile, given a mounted btrfs top level.
///
/// One fragment, used by the installer and by the verb, because the
/// whole point is that both produce the same thing: a doctor check that
/// knows where the file is and what its offset should be is only worth
/// having if the two paths cannot drift apart.
///
/// `btrfs filesystem mkswapfile` rather than the truncate, `chattr +C`,
/// `fallocate`, `mkswap` sequence it replaces. Those four have to happen
/// in that order and are silently wrong in any other: `chattr +C` only
/// takes on a file with no data in it, so allocating one line too early
/// leaves a copy-on-write swapfile that btrfs refuses to swapon, at the
/// next boot, with an error naming neither line. mkswapfile does the
/// sequence and is the only spelling that cannot be got subtly wrong.
///
/// Reads `$fsmnt` and `$swap_mib`. Sets `$swap_offset`.
pub fn create_script() -> String {
    format!(
        "# The swapfile, made before anything large is pulled: it is the\n\
         # one step here that can run the disk out of space, and finding\n\
         # that out now costs seconds instead of finding it out after an\n\
         # image has been fetched.\n\
         btrfs subvolume create \"$fsmnt/{SUBVOL}\" >/dev/null\n\
         btrfs filesystem mkswapfile --size \"${{swap_mib}}m\" --uuid clear \\\n  \
         \"$fsmnt/{SUBVOL}/swapfile\" >/dev/null\n\
         # Readable by root and nothing else. It is a copy of memory.\n\
         chmod 600 \"$fsmnt/{SUBVOL}/swapfile\"\n\n\
         # The number that makes resume work, asked of the file rather\n\
         # than computed from it. map-swapfile checks every condition the\n\
         # kernel will check at swapon and refuses to print an offset for\n\
         # a file that would fail one, so this reads the offset and\n\
         # proves the file is usable in the same call.\n\
         swap_offset=$(btrfs inspect-internal map-swapfile -r \"$fsmnt/{SUBVOL}/swapfile\")\n"
    )
}

/// The two `/etc/fstab` lines that make the swapfile swap.
///
/// fstab rather than a pair of hand-written units, because systemd's
/// generator turns these into exactly those units and this way the
/// ordering between them is one option rather than a `Requires=` and an
/// `After=` that have to agree. It is also where an installed machine
/// already keeps this kind of fact, so somebody looking for why there is
/// swap finds it in the first place they look.
///
/// `nodatacow` on the mount belongs to the subvolume rather than to the
/// file: mkswapfile already set the attribute on the file, and this
/// makes anything else that ever lands in that subvolume inherit it
/// rather than quietly becoming a second, broken swapfile candidate.
///
/// `x-systemd.device-timeout=0` because on an encrypted machine the
/// device does not exist until the passphrase is typed, and a default
/// timeout turns a slow person into a failed boot.
///
/// No `defaults` on the swap line, unlike the convention fstab invites.
/// `swapon` takes a real option list and `defaults` is not one of its
/// options, and a machine asked for `defaults,pri=-2` came up at -1:
/// something in that chain stopped reading at the word it did not know.
/// Dropping it costs nothing, because a swap line has no defaults to
/// name in the first place.
pub fn fstab_lines(fs_uuid: &str) -> String {
    format!(
        "\n{FSTAB_OPEN}\n\
         # The swapfile this machine hibernates into. The kernel finds it\n\
         # at resume through the resume= and resume_offset= kernel\n\
         # arguments, not through these lines; these are what make it swap\n\
         # once the system is up, which is what hibernating into it needs.\n\
         UUID={fs_uuid} {MOUNT} btrfs subvol={SUBVOL},noatime,nodatacow,x-systemd.device-timeout=0 0 0\n\
         {FILE} none swap pri={PRIORITY},x-systemd.requires-mounts-for={MOUNT} 0 0\n\
         {FSTAB_CLOSE}\n"
    )
}

/// The markers around kuma's fstab block.
///
/// A pair of sentinels rather than "delete any line mentioning
/// /var/swap", because turning hibernate off has to remove exactly what
/// turning it on added and nothing else. Somebody with their own reason
/// to mention that path gets to keep their line.
pub const FSTAB_OPEN: &str = "# >>> kuma hibernate >>>";
pub const FSTAB_CLOSE: &str = "# <<< kuma hibernate <<<";

/// An fstab with kuma's block taken back out.
///
/// Tolerant of a missing close marker, because a half-removed block is
/// exactly the state a person leaves behind when they edit this by hand
/// and change their mind, and the alternative is a function that gives
/// up on the file it was asked to repair.
pub fn strip_fstab(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == FSTAB_OPEN {
            inside = true;
            // The blank line kuma wrote before the marker goes with it,
            // so that turning hibernate on and off repeatedly does not
            // grow the file by one line each time.
            while out.ends_with("\n\n") {
                out.pop();
            }
            continue;
        }
        if inside {
            inside = line.trim() != FSTAB_CLOSE;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The kernel arguments that turn a swapfile into a resume source.
///
/// `resume=UUID=` rather than a device path, and one spelling for both
/// kinds of install. On a plain disk the path would be stable but
/// unlovely; on an encrypted one it is `/dev/mapper/luks-<uuid>`, a node
/// that does not exist until the initramfs has unlocked the container,
/// so naming it would mean naming something the kernel cannot resolve
/// when it first reads the command line. The UUID is the btrfs
/// filesystem's, which lives *inside* the container, so it appears at
/// exactly the moment the device does and udev resolves it either way.
///
/// The offset is in kernel pages, which is what `map-swapfile -r` prints
/// and what `/sys/power/resume_offset` expects. The physical byte offset
/// that the same command prints without `-r` is a different number, and
/// using it produces a machine that hibernates successfully and then
/// boots fresh.
/// Both values arrive as text rather than as a UUID and a number,
/// because the installer has neither: it is generating shell that will
/// learn them at run time, and passes `$fs_uuid` and `$swap_offset`. The
/// verb, which does know them, passes the values. One function either
/// way is what keeps the installed machine and the converted one saying
/// the same thing.
pub fn kargs(fs_uuid: &str, offset: &str) -> [String; 2] {
    [format!("resume=UUID={fs_uuid}"), format!("resume_offset={offset}")]
}

/// The `rpm-ostree kargs` arguments that make this machine's kernel
/// arguments say exactly this, given what they say now.
///
/// Built by reading first rather than by appending, because appending is
/// the move that produces the failure this whole feature has to avoid.
/// `--append-if-missing` leaves a stale `resume_offset` exactly as it
/// found it, and a stale offset is a machine that hibernates and then
/// boots fresh. So anything already there is deleted by the value it
/// currently has, and the new pair is appended after.
///
/// rpm-ostree rather than `ostree admin kargs edit-in-place`, and the
/// reason is this same replacement: edit-in-place offers only
/// `--append-if-missing`, so it can set these arguments once and can
/// never correct them.
pub fn karg_arguments(current: &str, fs_uuid: &str, offset: &str) -> Vec<String> {
    let mut args = vec!["kargs".to_string()];
    let (resume, resume_offset) = resume_from_cmdline(current);
    if let Some(old) = resume {
        args.push(format!("--delete-if-present=resume={old}"));
    }
    if let Some(old) = resume_offset {
        args.push(format!("--delete-if-present=resume_offset={old}"));
    }
    for karg in kargs(fs_uuid, offset) {
        args.push(format!("--append={karg}"));
    }
    args
}

/// The same, for taking them away again.
///
/// Empty of any `--delete` when there is nothing to delete, which the
/// caller reads as "there is no work here" rather than running
/// rpm-ostree to change nothing and stage a deployment for it.
pub fn karg_removal(current: &str) -> Vec<String> {
    let mut args = vec!["kargs".to_string()];
    let (resume, resume_offset) = resume_from_cmdline(current);
    if let Some(old) = resume {
        args.push(format!("--delete-if-present=resume={old}"));
    }
    if let Some(old) = resume_offset {
        args.push(format!("--delete-if-present=resume_offset={old}"));
    }
    if args.len() == 1 {
        args.clear();
    }
    args
}

/// The whole of turning hibernate on that needs root, as one script.
///
/// A script rather than a sequence of calls from Rust, for the reason
/// the installer's is one: it mounts something, and a failure between
/// the mount and the unmount has to unwind wherever it happened rather
/// than at every `?`.
///
/// Prints the offset on its last line, because the kernel arguments are
/// applied from Rust, where the current ones can be read and replaced
/// rather than blindly appended.
pub fn enable_script() -> String {
    format!(
        "#!/usr/bin/bash\n\
         # Generated by kuma from hibernate::enable_script.\n\
         set -euo pipefail\n\
         dev=${{1:?block device holding the btrfs root}}\n\
         swap_mib=${{2:?swapfile size in MiB}}\n\
         fstab=${{3:?the fstab to add the mount to}}\n\n\
         fsmnt=/run/kuma-swaptop\n\
         mkdir -p \"$fsmnt\"\n\
         cleanup() {{\n    \
         umount \"$fsmnt\" 2>/dev/null || true\n    \
         rmdir \"$fsmnt\" 2>/dev/null || true\n\
         }}\n\
         trap cleanup EXIT\n\n\
         # subvolid=5 is the filesystem top level whatever else the\n\
         # machine mounts. It is not mounted on a booted ostree system,\n\
         # and it is the one place the two kinds of kuma machine agree\n\
         # on: an install kuma did puts /var inside the root subvolume,\n\
         # an Anaconda one gives /var a subvolume of its own, and only\n\
         # the top level is above both of those.\n\
         mount -o subvolid=5 \"$dev\" \"$fsmnt\"\n\n\
         if [ -e \"$fsmnt/{SUBVOL}/swapfile\" ]; then\n    \
         echo \"kuma: a swapfile is already there; nothing was changed\" >&2\n    \
         exit 1\n\
         fi\n\n\
         {create}\n\
         # Asked of the device the root filesystem is on, which on an\n\
         # encrypted machine is the unlocked mapper and not the partition\n\
         # under it. resume= has to name the filesystem holding the file.\n\
         fs_uuid=$(blkid -s UUID -o value \"$dev\")\n\n\
         # Appended, never written over: /etc/fstab carries the boot and\n\
         # ESP entries this machine needs, and it is three-way merged on\n\
         # every update, which is what makes a local addition survive.\n\
         cat >> \"$fstab\" <<FSTAB\n\
         {fstab}FSTAB\n\n\
         umount \"$fsmnt\"\n\
         rmdir \"$fsmnt\"\n\
         trap - EXIT\n\n\
         # The unit names are asked for rather than spelled, because the\n\
         # escaping that turns /var/swap into var-swap.mount is systemd's\n\
         # to define and a hand-written copy of it is a thing that works\n\
         # until the path gains a character.\n\
         systemctl daemon-reload\n\
         systemctl start \"$(systemd-escape -p --suffix=mount {MOUNT})\"\n\n\
         # The label systemd-sleep needs, applied once the file is\n\
         # reachable at the path the policy names. Without it the machine\n\
         # gets a correct swapfile it can never hibernate into, and the\n\
         # failure appears only when somebody tries: systemd-sleep is\n\
         # denied read on anything that is not swapfile_t. The image\n\
         # carries the rule that makes this path a swapfile; restorecon\n\
         # is what applies it. Recursive from the mount point because\n\
         # there are two labels to fix: the subvolume's own root inode\n\
         # has none at all, and systemd-sleep must search the directory\n\
         # before it can read the file inside it. Tolerated when absent,\n\
         # because a machine without SELinux has nothing to label.\n\
          restorecon -RF {MOUNT} 2>/dev/null || true\n\
          systemctl start \"$(systemd-escape -p --suffix=swap {FILE})\"\n\n\
          # Last line, and the only thing on stdout: the caller reads it.\n\
          echo \"$swap_offset\"\n",
        create = create_script(),
        fstab = fstab_lines("$fs_uuid"),
    )
}

/// The whole of turning it off again.
///
/// Reversible because everything in kuma is: a verb that can only be
/// run once is a verb that has to be got right the first time, and this
/// one takes a size somebody is guessing at.
///
/// Every destructive step tolerates having already happened, so that a
/// half-enabled machine (the file made, the fstab written, the boot
/// never taken) can be cleaned up by the same command as a working one.
pub fn disable_script() -> String {
    format!(
        "#!/usr/bin/bash\n\
         # Generated by kuma from hibernate::disable_script.\n\
         set -euo pipefail\n\
         dev=${{1:?block device holding the btrfs root}}\n\
         fstab=${{2:?the fstab to take the mount out of}}\n\
         stripped=${{3:?the fstab as it should read afterwards}}\n\n\
         # Off first. A subvolume holding an active swapfile cannot be\n\
         # deleted, and a swapfile removed while the kernel still has it\n\
         # open is how a machine gets a swap area pointing at freed\n\
         # extents.\n\
         swapoff {FILE} 2>/dev/null || true\n\
         systemctl stop \"$(systemd-escape -p --suffix=swap {FILE})\" 2>/dev/null || true\n\
         systemctl stop \"$(systemd-escape -p --suffix=mount {MOUNT})\" 2>/dev/null || true\n\
         umount {MOUNT} 2>/dev/null || true\n\n\
         # Written through cat rather than moved into place, so the file\n\
         # keeps the inode, the mode and the SELinux label it already\n\
         # had. A renamed /etc/fstab with the wrong label is a machine\n\
         # that boots to an emergency shell.\n\
         cat \"$stripped\" > \"$fstab\"\n\
         systemctl daemon-reload\n\n\
         fsmnt=/run/kuma-swaptop\n\
         mkdir -p \"$fsmnt\"\n\
         cleanup() {{\n    \
         umount \"$fsmnt\" 2>/dev/null || true\n    \
         rmdir \"$fsmnt\" 2>/dev/null || true\n\
         }}\n\
         trap cleanup EXIT\n\
          mount -o subvolid=5 \"$dev\" \"$fsmnt\"\n\
          if [ -d \"$fsmnt/{SUBVOL}\" ]; then\n    \
          rm -f \"$fsmnt/{SUBVOL}/swapfile\"\n    \
          btrfs subvolume delete \"$fsmnt/{SUBVOL}\" >/dev/null\n\
          fi\n\n\
          # The lid goes back to plain suspend with the rest of it. No\
          # logind restart: the reboot the karg removal below asks for\
          # reloads it, same as the one that put the setting in did.\n\
          rm -f {LID_DROPIN}\n"
    )
}

/// What the booted kernel was told about resuming.
pub fn resume_from_cmdline(cmdline: &str) -> (Option<String>, Option<u64>) {
    let mut resume = None;
    let mut offset = None;
    for arg in cmdline.split_whitespace() {
        if let Some(value) = arg.strip_prefix("resume_offset=") {
            offset = value.parse().ok();
        } else if let Some(value) = arg.strip_prefix("resume=") {
            resume = Some(value.to_string());
        }
    }
    (resume, offset)
}

/// Whether this kernel will hibernate at all.
///
/// `/sys/power/state` lists `disk` only when `hibernation_available()`
/// says so, and that function is `!security_locked_down(LOCKDOWN_HIBERNATION)`.
/// So this one file answers the question directly, without kuma having
/// to know why: a locked-down kernel simply stops offering it.
///
/// This is the check kuma shipped without, and a Secure Boot machine is
/// what proved it necessary. Kernel lockdown runs in integrity mode on
/// any machine that booted with Secure Boot on, and integrity mode
/// refuses hibernation because a hibernate image is a way to write
/// arbitrary memory back into a running kernel. Without this, doctor
/// looked at a correct swapfile and correct kernel arguments and said
/// `ok` on a machine that would never hibernate.
pub fn kernel_allows_hibernation(power_state: &str) -> bool {
    power_state.split_whitespace().any(|mode| mode == "disk")
}

/// Which lockdown mode is active, from `/sys/kernel/security/lockdown`.
///
/// The file lists every mode and brackets the current one, as in
/// `none [integrity] confidentiality`. Only ever used to explain a
/// refusal in words somebody can act on, never to decide anything:
/// `/sys/power/state` is the authority, and a machine that refuses
/// hibernation for some other reason should not be told it is Secure
/// Boot's doing.
pub fn active_lockdown(text: &str) -> Option<String> {
    text.split_whitespace()
        .find_map(|mode| mode.strip_prefix('[')?.strip_suffix(']'))
        .map(str::to_string)
}

/// The SELinux type out of a full context, `swapfile_t` out of
/// `system_u:object_r:swapfile_t:s0`.
///
/// None where the machine has no SELinux and `stat` prints `?`, which is
/// not a fault and not something to grade.
pub fn selinux_type(context: &str) -> Option<String> {
    let context = context.trim();
    if context.is_empty() || context == "?" {
        return None;
    }
    context.split(':').nth(2).map(str::to_string)
}

/// The type a swapfile has to have, and the reason this is checked at
/// all.
///
/// `systemd-sleep` can read a `swapfile_t` file and cannot read the
/// `var_t` one that the policy's own default produces for a file under
/// `/var`. A machine with the wrong label has a correct swapfile,
/// correct kernel arguments, active swap, and fails at the moment it is
/// asked to hibernate with `Failed to find location to hibernate to:
/// Permission denied`. Measured on a real machine; every other part of
/// this check passed on the machine that could not do it.
pub const SWAPFILE_TYPE: &str = "swapfile_t";

/// Whether `/proc/swaps` has the swapfile active.
///
/// By the path it was mounted at, which is how swapon reports a file.
pub fn swap_active(proc_swaps: &str) -> bool {
    proc_swaps.lines().skip(1).any(|line| line.split_whitespace().next() == Some(FILE))
}

/// The size the kernel's own swap table reports for the swapfile, in
/// MiB, or None when it is not active swap.
///
/// `/proc/swaps` is world-readable, so this is the one reading about
/// the file that an unprivileged run can still make: the columns are
/// filename, type, size in KiB, used, priority.
pub fn swap_size_mib(proc_swaps: &str) -> Option<u64> {
    proc_swaps
        .lines()
        .skip(1)
        .find_map(|line| {
            let mut cols = line.split_whitespace();
            (cols.next() == Some(FILE)).then(|| cols.nth(1)).flatten()
        })
        .and_then(|kib| kib.parse::<u64>().ok())
        .map(|kib| kib / 1024)
}

/// Everything needed to say whether this machine can really hibernate.
///
/// Gathered rather than asked for one at a time, so that the judgement
/// below is pure and can be tested against every combination of broken
/// that a real machine can be in, none of which is convenient to stage.
#[derive(Debug, Default)]
pub struct Status {
    /// The `resume=` the kernel booted with.
    pub resume: Option<String>,
    /// The `resume_offset=` the kernel booted with.
    pub resume_offset: Option<u64>,
    /// Where the swapfile actually starts now, in pages, or None when
    /// there is no usable swapfile to ask.
    pub file_offset: Option<u64>,
    /// How big it is.
    pub file_mib: Option<u64>,
    /// Whether it is active swap right now.
    pub active: bool,
    /// What the kernel's own swap table says the file holds, in MiB.
    /// Needs no privilege to read, unlike `file_mib`, which is why it
    /// exists: an unprivileged run can say how big the active swapfile
    /// is even when it cannot ask where the file's header lives.
    pub swaps_file_mib: Option<u64>,
    /// What this machine has to hibernate.
    pub ram_mib: Option<u64>,
    /// Whether the kernel will hibernate at all, which is a different
    /// question from whether kuma set it up. False on a machine whose
    /// kernel has been locked down.
    pub kernel_allows: bool,
    /// Which lockdown mode is active, when the machine says. Carried
    /// only to explain the answer above, never to decide it.
    pub lockdown: Option<String>,
    /// Whether the swapfile is there at all, which needs no privilege.
    pub present: bool,
    /// Whether the privileged read ran at all. False means sudo could
    /// not run, which is not an answer and must not grade like one.
    pub asked: bool,
    /// The swapfile's SELinux type, when the machine has SELinux. None
    /// where there is nothing to ask.
    pub selinux_type: Option<String>,
    /// The type on the directory the swapfile sits in. A separate
    /// question from the file's, and one that a correct file hides:
    /// `systemd-sleep` has to search the directory before it can read
    /// anything inside it, so a labelled file in an unlabelled directory
    /// fails exactly as a mislabelled file does.
    pub mount_selinux_type: Option<String>,
    /// Whether the lid is set to suspend-then-hibernate. Needs no
    /// privilege: /etc is world-readable, and the drop-in is kuma's own.
    pub lid_dropin: bool,
}

/// Ask this machine everything the verdict needs.
///
/// The two questions that need root are asked only when the file is
/// there to ask about. A machine with no swapfile is the common case, so
/// running `sudo` twice on every `kuma doctor` to be told "no such file"
/// would make the majority pay a password prompt for a feature they
/// declined. `exists()` needs no privilege: the file is 0600 and owned
/// by root, but the directories above it are not.
pub fn probe() -> Status {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let (resume, resume_offset) = resume_from_cmdline(&cmdline);
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let swaps = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
    let present = std::path::Path::new(FILE).exists();
    // map-swapfile is the reading that matters, and it is not merely a
    // number: it re-checks every condition the kernel checks at swapon,
    // so a file that has become unusable answers None here and grades
    // the same as a missing one. That is the intent. A swapfile the
    // kernel would refuse is not a swapfile.
    // ONE privileged call, and the reason is the verdict rather than the
    // three spawns it saves.
    //
    // These were four `sudo` calls whose failures were each `.ok()`d into
    // the same `None` as "the kernel would refuse this file", so a
    // declined or unpromptable sudo graded identically to an unusable
    // swapfile. `kuma doctor` reported "missing or unusable" about a 15G
    // swapfile that was present, active and correct, and which answer you
    // got depended on whether sudo could prompt: green in a terminal,
    // FAIL for anything reading `doctor --json`, which docs/agents.md
    // points agents at, and invisible to CI because smoke runs doctor
    // through `gsudo`.
    //
    // Every question prints its marker whether or not it can answer, so
    // there are three states rather than two: no markers at all (the
    // shell never ran), a marker with a value (answered), and a marker
    // with nothing after it (asked and unanswerable). The last still
    // grades exactly as a missing file does, which is the documented
    // intent below and stays.
    let probe = present
        .then(|| {
            host_output(&[
                "sudo",
                "sh",
                "-c",
                &format!(
                    "printf '@offset\\n'; btrfs inspect-internal map-swapfile -r {FILE} || true; \
                     printf '@size\\n'; stat -c %s {FILE} || true; \
                     printf '@fcon\\n'; stat -c %C {FILE} || true; \
                     printf '@mcon\\n'; stat -c %C {MOUNT} || true"
                ),
            ])
            .ok()
        })
        .flatten();
    let asked = probe.is_some();
    let section = |name: &str| -> Option<String> {
        let text = probe.as_deref()?;
        let after = text.split(&format!("@{name}\n")).nth(1)?;
        let value = after.split('@').next()?.trim().to_string();
        (!value.is_empty()).then_some(value)
    };
    let file_offset = section("offset").and_then(|v| v.parse().ok());
    let file_mib = section("size").and_then(|v| v.parse::<u64>().ok()).map(|b| b / (1024 * 1024));
    let selinux = section("fcon").and_then(|out| selinux_type(&out));
    let mount_selinux = section("mcon").and_then(|out| selinux_type(&out));
    let power_state = std::fs::read_to_string("/sys/power/state").unwrap_or_default();
    let lockdown = std::fs::read_to_string("/sys/kernel/security/lockdown").ok();
    Status {
        resume,
        resume_offset,
        present,
        asked,
        file_offset,
        file_mib,
        active: swap_active(&swaps),
        swaps_file_mib: swap_size_mib(&swaps),
        ram_mib: ram_mib(&meminfo),
        kernel_allows: kernel_allows_hibernation(&power_state),
        lockdown: lockdown.as_deref().and_then(active_lockdown),
        selinux_type: selinux,
        mount_selinux_type: mount_selinux,
        // Graded by setting, not by byte equality: a person who adds a
        // HibernateDelaySec to kuma's own file has tuned it, not broken
        // it, and the question doctor asks is whether the lid still
        // suspend-then-hibernates.
        lid_dropin: std::fs::read_to_string(LID_DROPIN)
            .is_ok_and(|text| text.contains("HandleLidSwitch=suspend-then-hibernate")),
    }
}

/// What doctor says about it.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Nothing here claims to hibernate. The default, and not a fault:
    /// a machine with no swapfile suspends fine and loses nothing.
    NotSet,
    /// Every part is present and the parts agree.
    Ready(String),
    /// A promise this machine makes and cannot keep.
    Broken(String),
    /// It will work until the day it is needed most.
    Short(String),
    /// The question could not be asked. The swapfile is there and the
    /// privileged read that grades it did not run, which is what a
    /// declined or unpromptable `sudo` looks like. NOT a fault: reporting
    /// one here is how doctor came to tell a correct machine its swapfile
    /// was missing.
    Unasked(String),
    /// Everything kuma controls is right and the kernel still says no.
    /// Not a fault in the setup and not something kuma can fix, but not
    /// something to stay quiet about either: the machine is carrying a
    /// swapfile it will never use.
    Refused(String),
}

/// Grade hibernate the way a machine can be asked, not the way it was
/// configured.
///
/// The offset comparison is the reason this check exists. Every other
/// part of hibernate fails loudly: no swapfile and logind will not offer
/// it, no `resume=` and the initramfs does nothing. A `resume_offset`
/// that no longer matches the file fails silently and in the worst
/// possible way. The machine writes memory to disk, powers off, and
/// boots fresh, and the only sign is a session that is gone. That can
/// happen without anybody doing anything wrong: delete the swapfile and
/// make another the same size and the offset has moved.
pub fn verdict(status: &Status) -> Verdict {
    let claimed = status.resume.is_some() || status.resume_offset.is_some();
    match (claimed, status.file_offset) {
        (false, None) => Verdict::NotSet,
        // Present, and nothing privileged could read it. Only sudo tells
        // these apart, so the verdict has to as well.
        (true, None) if status.present && !status.asked => Verdict::Unasked(format!(
            "{FILE} is there and grading it needs root, which was declined or could not \
             be asked for; run this with sudo to have it checked"
        )),
        (true, None) => Verdict::Broken(format!(
            "the kernel is told to resume from a swapfile, but {FILE} is missing or unusable"
        )),
        (false, Some(_)) => Verdict::Broken(format!(
            "{FILE} exists but no resume= kernel argument names it, so this machine \
             can be told to hibernate and will not come back"
        )),
        (true, Some(actual)) => {
            match status.resume_offset {
                Some(claimed) if claimed != actual => {
                    return Verdict::Broken(format!(
                        "the kernel is told to resume from page {claimed}, but {FILE} now \
                         starts at page {actual}: hibernating would write the image and then \
                         boot fresh, losing the session"
                    ))
                }
                None => {
                    return Verdict::Broken(
                        "resume= is set but resume_offset= is not, and a swapfile needs both"
                            .to_string(),
                    )
                }
                _ => {}
            }
            if !status.active {
                return Verdict::Broken(format!(
                    "{FILE} is not active swap, so there is nowhere to write a hibernate image"
                ));
            }
            // Before the kernel's own refusal below, because this is the
            // half kuma can fix and that one is a firmware setting.
            if let Some(kind) = &status.selinux_type {
                if kind != SWAPFILE_TYPE {
                    return Verdict::Broken(format!(
                        "{FILE} is labelled {kind}, and systemd-sleep can only read \
                         {SWAPFILE_TYPE}: hibernating fails with Permission denied, however \
                         correct everything else is"
                    ));
                }
                // The directory is a second question, and a correct file
                // hides it: systemd-sleep has to search {MOUNT} before it
                // can read what is inside. A machine with the file right
                // and the directory unlabelled graded ok here and could
                // not hibernate, which is what this arm exists for.
                match &status.mount_selinux_type {
                    Some(dir) if dir == "unlabeled_t" => {
                        return Verdict::Broken(format!(
                            "{MOUNT} is labelled {dir}, so systemd-sleep cannot search it to \
                             reach the swapfile, even though the swapfile itself is right"
                        ))
                    }
                    None => {
                        return Verdict::Broken(format!(
                            "{MOUNT} has no SELinux label at all, so systemd-sleep cannot \
                             search it to reach the swapfile"
                        ))
                    }
                    _ => {}
                }
            }
            // Last, and only once everything kuma owns is right, so that
            // "refused" means "your setup is correct and the kernel
            // still will not do it" rather than hiding a real fault
            // behind a firmware setting.
            if !status.kernel_allows {
                let why = match status.lockdown.as_deref() {
                    Some("none") | None => "this kernel does not offer hibernation".to_string(),
                    Some(mode) => format!(
                        "this kernel is locked down ({mode}), which is what booting with \
                         Secure Boot turns on, and a locked-down kernel refuses hibernation"
                    ),
                };
                return Verdict::Refused(format!(
                    "the swapfile and resume are set up correctly, but {why}"
                ));
            }
            match (status.file_mib, status.ram_mib) {
                (Some(file), Some(ram)) if file < ram => Verdict::Short(format!(
                    "swapfile is {}, smaller than this machine's {} of memory: hibernating \
                     fails when more than {} of it is dirty",
                    size_text(file),
                    size_text(ram),
                    size_text(file)
                )),
                (Some(file), _) => Verdict::Ready(format!(
                    "{} swapfile, active, and resume points at it",
                    size_text(file)
                )),
                _ => Verdict::Ready("swapfile active, and resume points at it".to_string()),
            }
        }
    }
}

/// What doctor says about the lid, given the hibernate verdict it sits
/// beside. A separate answer because it is a separate setting: the
/// swapfile can be perfect while the lid still only suspends, and a
/// machine can carry the lid setting with no swapfile behind it.
#[derive(Debug, PartialEq)]
pub enum LidVerdict {
    /// Nothing to say: hibernate itself is off, broken, or unasked, and
    /// whichever of those it is gets its own line from its own check.
    /// A machine whose hibernate is broken has no use for a lid grading,
    /// and one that never asked for hibernate owes nobody a lid line.
    Silent,
    /// Hibernate works and the lid uses it.
    Ok(String),
    /// Hibernate works and the lid does not use it: the machine will
    /// suspend into a battery that runs out instead of hibernating.
    Missing(String),
    /// The lid setting is here and hibernate is not. systemd falls back
    /// to plain suspend, so nothing is dangerous; it is drift between
    /// two things kuma owns, which is the shape doctor names.
    Stale(String),
}

/// Grade the lid against the hibernate it is supposed to ride on.
pub fn lid_verdict(hibernate: &Verdict, lid_dropin: bool) -> LidVerdict {
    match (hibernate, lid_dropin) {
        // Ready means the machine can really hibernate, so the promise
        // the lid makes is one it can keep: suspend first, hibernate
        // when the battery says so, and either way the session comes
        // back locked.
        (Verdict::Ready(_), true) => LidVerdict::Ok(
            "closing the lid suspends, then hibernates before the battery runs out".into(),
        ),
        (Verdict::Ready(_), false) => LidVerdict::Missing(
            "hibernate is set up but closing the lid only suspends, so a machine left \
             asleep in a bag drains and dies instead of hibernating"
                .into(),
        ),
        (Verdict::NotSet, true) => LidVerdict::Stale(
            "the lid is set to suspend-then-hibernate but there is no swapfile to \
             hibernate into, so it silently falls back to plain suspend"
                .into(),
        ),
        // Every other hibernate verdict already says something is wrong
        // with hibernation itself; a second line about the lid would be
        // doctor listing features rather than grading them.
        _ => LidVerdict::Silent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's own table is the unprivileged reading of an active
    /// swapfile: the size from the row the file is mounted at, none
    /// when it is not there. The columns are KiB, and /proc/swaps is
    /// tab-separated in the wild.
    #[test]
    fn the_swap_table_answers_the_size_without_privilege() {
        let table = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                     /dev/zram0\t\t\t\tpartition\t8388604\t\t0\t\t100\n\
                     /var/swap/swapfile\t\tfile\t\t15728636\t0\t\t-1\n";
        assert_eq!(swap_size_mib(table), Some(15728636 / 1024));
        assert_eq!(
            swap_size_mib(
                "Filename\tType\tSize\tUsed\tPriority\n/dev/zram0\tpartition\t8388604\t0\t100\n"
            ),
            None,
            "another active swap is not the swapfile"
        );
    }

    /// A bare number is the one input that is dangerous to guess at, and
    /// both units have to survive both spellings.
    /// A machine whose swapfile could not be READ is not a machine whose
    /// swapfile is missing.
    ///
    /// This is the bug the marked probe exists for: four `.ok()`d sudo
    /// calls made "sudo declined" and "the kernel would refuse this file"
    /// the same `None`, so `kuma doctor` told a correct machine with a
    /// 15G active swapfile that it was missing or unusable, and only when
    /// sudo could not prompt. Green in a terminal, FAIL for `doctor
    /// --json`, invisible to CI because smoke runs doctor through sudo.
    #[test]
    fn a_swapfile_that_could_not_be_read_is_not_a_swapfile_that_is_gone() {
        let unreadable = Status {
            swaps_file_mib: None,
            present: true,
            asked: false,
            resume: Some("UUID=x".into()),
            resume_offset: Some(103032064),
            file_offset: None,
            file_mib: None,
            active: true,
            ram_mib: Some(16 * 1024),
            kernel_allows: true,
            lockdown: None,
            selinux_type: None,
            mount_selinux_type: None,
            lid_dropin: false,
        };
        assert!(
            matches!(verdict(&unreadable), Verdict::Unasked(_)),
            "a declined sudo must not grade as a broken swapfile: {:?}",
            verdict(&unreadable)
        );
        // And a machine that really has no file still grades Broken, which
        // is the documented intent and does not change.
        let gone = Status { present: false, asked: false, ..unreadable };
        assert!(matches!(verdict(&gone), Verdict::Broken(_)), "{:?}", verdict(&gone));
    }

    #[test]
    fn sizes_parse_with_a_unit_and_are_refused_without_one() {
        assert_eq!(parse_size("16G").unwrap(), Some(16 * 1024));
        assert_eq!(parse_size("16g").unwrap(), Some(16 * 1024));
        assert_eq!(parse_size("16GiB").unwrap(), Some(16 * 1024));
        assert_eq!(parse_size("16384M").unwrap(), Some(16 * 1024));
        assert_eq!(parse_size(" 8G ").unwrap(), Some(8 * 1024));
        for none in ["none", "None", "off", "no", "0"] {
            assert_eq!(parse_size(none).unwrap(), None, "{none} declines a swapfile");
        }
        let err = parse_size("16").unwrap_err().to_string();
        assert!(err.contains("16G"), "the error teaches the spelling: {err}");
        assert!(parse_size("16T").is_err(), "an unknown unit is not silently a number");
        assert!(parse_size("512M").is_err(), "half a gibibyte cannot be a hibernate image");
        // The shared spelling half names what it is sizing, so an error
        // about a partition size cannot read as one about a swapfile;
        // and it keeps sizes the swap floor would refuse, because the
        // floor belongs to the caller asking (see partition::resolve_size).
        let err = parse_mib("16", "ESP").unwrap_err().to_string();
        assert!(err.contains("ESP size"), "the error names what it was sizing: {err}");
        assert_eq!(parse_mib("512M", "ESP").unwrap(), Some(512));
    }

    /// Sizes read as the unit they were typed in, and the fractional
    /// case is the one that appears in the sentence a person has to act
    /// on: what the disk has left.
    #[test]
    fn sizes_print_as_gibibytes_wherever_the_number_is_one() {
        assert_eq!(size_text(16 * 1024), "16G");
        assert_eq!(size_text(15 * 1024), "15G");
        assert_eq!(size_text(242_959), "237.3G");
        assert_eq!(size_text(512), "512M");
        assert_eq!(size_text(1536), "1.5G");
    }

    /// The property, not a remembered number: whatever a machine
    /// reports, the default has to be a whole gibibyte and has to be at
    /// least as large as memory, because the hibernate image can never
    /// exceed MemTotal and a file smaller than it can fail.
    ///
    /// Written as an assertion about every input rather than about one,
    /// because the first version of this test asserted that a machine
    /// reporting 15234567 kB should get 16 GiB. It gets 15 GiB, which is
    /// correct: that machine has 14.5 GiB to save, and the extra
    /// gibibyte was the test remembering the number printed on the RAM
    /// stick rather than the number the kernel can be asked to write.
    #[test]
    fn the_default_size_is_a_whole_gibibyte_and_never_smaller_than_memory() {
        for kib in [15234567u64, 8 * 1024 * 1024, 4 * 1024 * 1024 + 1, 65 * 1024 * 1024] {
            let meminfo = format!("MemTotal:       {kib} kB\nMemFree:         800 kB\n");
            let mib = default_size_mib(&meminfo).expect("MemTotal is there to be read");
            assert_eq!(mib % 1024, 0, "{kib} kB gave {mib}M, not a whole gibibyte");
            // Both bounds ceil the way the code does. Truncating here
            // instead was the second version of this test to assert a
            // number rather than the rule: 4194305 kB is a hair over
            // 4 GiB, so a whole gibibyte above it is 5, and only a
            // floored bound calls that too much.
            let memory_mib = kib.div_ceil(1024);
            assert!(mib >= memory_mib, "{kib} kB gave {mib}M, which cannot hold memory");
            assert!(mib < memory_mib + 1024, "{kib} kB gave {mib}M, more than a gibibyte spare");
        }
        // Exactly 8 GiB stays 8 GiB rather than becoming 9.
        let exact = format!("MemTotal:       {} kB\n", 8 * 1024 * 1024);
        assert_eq!(default_size_mib(&exact), Some(8 * 1024));
        assert_eq!(default_size_mib("Buffers: 1 kB\n"), None);
    }

    /// The two facts an install can state and a later run cannot state
    /// as cheaply, and they are warnings rather than refusals because
    /// both are somebody's to decide.
    #[test]
    fn an_unencrypted_disk_and_an_undersized_file_are_both_said_out_loud() {
        let plain = warnings(16 * 1024, Some(16 * 1024), false);
        assert_eq!(plain.len(), 1);
        assert!(plain[0].contains("in the clear"));

        let small = warnings(8 * 1024, Some(16 * 1024), true);
        assert_eq!(small.len(), 1);
        assert!(small[0].contains("8G is smaller"), "{}", small[0]);

        assert!(warnings(16 * 1024, Some(16 * 1024), true).is_empty(), "the good case is silent");
        assert!(objections(16 * 1024, 40 * 1024, SPARE_AT_INSTALL).is_empty());
        let too_big = objections(64 * 1024, 40 * 1024, SPARE_AT_INSTALL);
        assert_eq!(too_big.len(), 1, "a file bigger than the spare room");
        assert!(too_big[0].contains("64G") && too_big[0].contains("40G"), "{}", too_big[0]);
    }

    /// The failure this whole check exists for: everything present,
    /// everything active, and an offset that no longer describes the
    /// file. A machine in this state looks healthy and loses the
    /// session, so it has to grade as broken rather than as a warning.
    #[test]
    fn a_stale_resume_offset_is_broken_rather_than_merely_odd() {
        let status = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            file_offset: Some(999_999),
            file_mib: Some(16 * 1024),
            active: true,
            ram_mib: Some(16 * 1024),
            kernel_allows: true,
            lockdown: Some("none".into()),
            selinux_type: Some(SWAPFILE_TYPE.into()),
            mount_selinux_type: Some("var_t".into()),
            lid_dropin: false,
        };
        match verdict(&status) {
            Verdict::Broken(why) => {
                assert!(why.contains("4096") && why.contains("999999"), "{why}");
                assert!(why.contains("boot fresh"), "it says what actually happens: {why}");
            }
            other => panic!("a stale offset must be broken, got {other:?}"),
        }
    }

    /// Every other shape a machine can be in, including the one that is
    /// not a fault at all.
    #[test]
    fn the_remaining_states_each_grade_as_themselves() {
        assert_eq!(verdict(&Status::default()), Verdict::NotSet);

        let ready = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            file_offset: Some(4096),
            file_mib: Some(16 * 1024),
            active: true,
            ram_mib: Some(16 * 1024),
            kernel_allows: true,
            lockdown: Some("none".into()),
            selinux_type: Some(SWAPFILE_TYPE.into()),
            mount_selinux_type: Some("var_t".into()),
            lid_dropin: false,
        };
        assert!(matches!(verdict(&ready), Verdict::Ready(_)));

        let short = Status { file_mib: Some(4 * 1024), ..ready };
        assert!(matches!(verdict(&short), Verdict::Short(_)));

        let no_karg = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: None,
            resume_offset: None,
            file_offset: Some(4096),
            file_mib: Some(16 * 1024),
            active: true,
            ram_mib: None,
            kernel_allows: true,
            lockdown: None,
            selinux_type: None,
            mount_selinux_type: None,
            lid_dropin: false,
        };
        assert!(matches!(verdict(&no_karg), Verdict::Broken(_)));

        let no_file = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            ..Status::default()
        };
        assert!(matches!(verdict(&no_file), Verdict::Broken(_)));

        let inactive = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            file_offset: Some(4096),
            file_mib: Some(16 * 1024),
            active: false,
            ram_mib: None,
            kernel_allows: true,
            lockdown: None,
            selinux_type: None,
            mount_selinux_type: None,
            lid_dropin: false,
        };
        assert!(matches!(verdict(&inactive), Verdict::Broken(_)));
    }

    /// The finding that a Secure Boot VM produced on the first run of
    /// the gate, kept as a test because kuma shipped without it and was
    /// wrong in the most expensive direction: doctor looked at a correct
    /// swapfile and correct kernel arguments and graded the machine
    /// `ok`, while logind answered CanHibernate `na` and the kernel
    /// would never have done it.
    #[test]
    fn a_locked_down_kernel_refuses_however_correct_the_setup_is() {
        // `/sys/power/state` is the authority, and it is authoritative
        // precisely because the kernel stops listing `disk` when
        // hibernation is locked down.
        assert!(kernel_allows_hibernation("freeze mem disk"));
        assert!(!kernel_allows_hibernation("freeze mem"));
        assert_eq!(
            active_lockdown("none [integrity] confidentiality").as_deref(),
            Some("integrity")
        );
        assert_eq!(active_lockdown("[none] integrity confidentiality").as_deref(), Some("none"));
        assert_eq!(active_lockdown("nothing bracketed here"), None);

        let perfect = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            file_offset: Some(4096),
            file_mib: Some(16 * 1024),
            active: true,
            ram_mib: Some(16 * 1024),
            kernel_allows: false,
            lockdown: Some("integrity".into()),
            selinux_type: Some(SWAPFILE_TYPE.into()),
            mount_selinux_type: Some("var_t".into()),
            lid_dropin: false,
        };
        match verdict(&perfect) {
            Verdict::Refused(why) => {
                assert!(why.contains("set up correctly"), "{why}");
                assert!(why.contains("Secure Boot"), "it names what turned lockdown on: {why}");
            }
            other => panic!("a locked-down kernel must not grade ready, got {other:?}"),
        }

        // And the same fact ahead of time, which is what stops somebody
        // spending sixteen gibibytes on a file that cannot be used.
        let warning = lockdown_warning(false, Some("integrity")).expect("it warns");
        assert!(warning.contains("Secure Boot"), "{warning}");
        assert!(lockdown_warning(true, Some("none")).is_none(), "a normal kernel is silent");
    }

    /// The bug that four runs of the gate were spent reaching, kept as a
    /// test because every other check passed on the machine that had it.
    /// The swapfile was there, active, the right size, the offset
    /// matched the kernel argument, the kernel offered hibernation, and
    /// the machine could not hibernate: systemd-sleep is denied `read`
    /// on a file that is not swapfile_t.
    #[test]
    fn a_swapfile_the_sleep_code_cannot_read_is_broken_however_right_the_rest_is() {
        assert_eq!(selinux_type("system_u:object_r:swapfile_t:s0").as_deref(), Some("swapfile_t"));
        assert_eq!(selinux_type("unconfined_u:object_r:var_t:s0").as_deref(), Some("var_t"));
        // A machine with no SELinux prints `?`, which is not a fault.
        assert_eq!(selinux_type("?"), None);
        assert_eq!(selinux_type("  "), None);

        let mislabelled = Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            file_offset: Some(4096),
            file_mib: Some(16 * 1024),
            active: true,
            ram_mib: Some(16 * 1024),
            kernel_allows: true,
            lockdown: Some("none".into()),
            // What the policy's own default gives a file under /var, and
            // therefore what a plain restorecon produces.
            selinux_type: Some("var_t".into()),
            mount_selinux_type: Some("var_t".into()),
            lid_dropin: false,
        };
        match verdict(&mislabelled) {
            Verdict::Broken(why) => {
                assert!(why.contains("var_t") && why.contains("swapfile_t"), "{why}");
                assert!(why.contains("Permission denied"), "it names the failure: {why}");
            }
            other => panic!("a mislabelled swapfile must be broken, got {other:?}"),
        }

        // Unlabelled machines are not graded on a label they cannot have.
        let no_selinux = Status { selinux_type: None, mount_selinux_type: None, ..mislabelled };
        assert!(matches!(verdict(&no_selinux), Verdict::Ready(_)));
    }

    /// The second half of the same bug, and the half that shipped: the
    /// file was labelled correctly and the directory it sits in was not
    /// labelled at all, so systemd-sleep could not search its way to the
    /// file. Doctor graded that machine `ok` and it could not hibernate.
    #[test]
    fn a_correct_swapfile_in_an_unlabelled_directory_is_still_broken() {
        // A closure rather than one value reused, because Status owns
        // Strings and the three shapes below each need their own.
        let with_dir = |dir: Option<&str>| Status {
            swaps_file_mib: None,
            // A fixture is a machine whose privileged read succeeded.
            present: true,
            asked: true,
            resume: Some("UUID=abc".into()),
            resume_offset: Some(4096),
            file_offset: Some(4096),
            file_mib: Some(16 * 1024),
            active: true,
            ram_mib: Some(16 * 1024),
            kernel_allows: true,
            lockdown: Some("none".into()),
            selinux_type: Some(SWAPFILE_TYPE.into()),
            mount_selinux_type: dir.map(str::to_string),
            lid_dropin: false,
        };
        // No label at all, which is what the installer leaves behind.
        match verdict(&with_dir(None)) {
            Verdict::Broken(why) => assert!(why.contains(MOUNT) && why.contains("search"), "{why}"),
            other => panic!("an unlabelled directory must be broken, got {other:?}"),
        }
        assert!(matches!(verdict(&with_dir(Some("unlabeled_t"))), Verdict::Broken(_)));
        // And the working shape stays working.
        assert!(matches!(verdict(&with_dir(Some("var_t"))), Verdict::Ready(_)));
    }

    /// The command line is parsed rather than pattern-matched, because
    /// `resume=` is a prefix of `resume_offset=` and the obvious
    /// `contains` reading gets them the wrong way round.
    #[test]
    fn resume_and_its_offset_are_told_apart_on_a_real_command_line() {
        let cmdline = "root=UUID=86bf ro resume=UUID=86bf resume_offset=269312 quiet";
        let (resume, offset) = resume_from_cmdline(cmdline);
        assert_eq!(resume.as_deref(), Some("UUID=86bf"));
        assert_eq!(offset, Some(269312));

        let (none, no_offset) = resume_from_cmdline("root=UUID=86bf ro quiet");
        assert!(none.is_none() && no_offset.is_none());
    }

    /// swapon reports a file by the path it was activated at, and the
    /// header line is not a swap area.
    #[test]
    fn the_swapfile_is_found_in_a_real_proc_swaps() {
        let swaps = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                     /dev/zram0                              partition\t8388604\t\t0\t\t100\n\
                     /var/swap/swapfile                      file\t\t16777216\t0\t\t-2\n";
        assert!(swap_active(swaps));
        assert!(!swap_active("Filename\tType\n/dev/zram0  partition\t8388604\t0\t100\n"));
    }

    /// The generated shell has to make the file before it reads the
    /// offset, and has to read the offset the kernel wants rather than
    /// the byte offset printed beside it.
    #[test]
    fn the_create_script_makes_the_file_then_asks_it_for_the_page_offset() {
        let script = create_script();
        let make = script.find("mkswapfile").expect("it makes a swapfile");
        let read = script.find("map-swapfile").expect("it reads the offset");
        assert!(make < read, "the offset is read from a file that exists");
        assert!(script.contains("--uuid clear"));
        assert!(script.contains("map-swapfile -r"), "-r is the page offset the kernel wants");
        assert!(script.contains("chmod 600"), "a copy of memory is not world-readable");
    }

    /// Turning it on has to label the file, or it makes a swapfile the
    /// sleep code cannot read. The order matters as much as the call:
    /// the label is applied at the path the policy names, which only
    /// exists once the subvolume is mounted.
    #[test]
    fn enabling_labels_the_swapfile_after_it_is_mounted_where_the_policy_names_it() {
        let script = enable_script();
        let mount = script.find("--suffix=mount").expect("it mounts the subvolume");
        let label = script.find("restorecon").expect("it labels the file");
        assert!(mount < label, "the label goes on at /var/swap, not in the temporary mount");
        assert!(
            script.contains(&format!("restorecon -RF {MOUNT}")),
            "recursive from the mount point: the directory needs a label too"
        );
    }

    /// Both fstab lines name the same two constants the doctor check
    /// looks for, and the swap line waits for the mount that carries it.
    #[test]
    fn the_fstab_lines_agree_with_the_paths_everything_else_uses() {
        let lines = fstab_lines("86bf4581-fc83-4be5-b65d-bd71e1f59ff6");
        assert!(lines.contains(&format!("{MOUNT} btrfs subvol={SUBVOL}")));
        assert!(lines.contains(&format!("{FILE} none swap")));
        assert!(lines.contains(&format!("x-systemd.requires-mounts-for={MOUNT}")));
        assert!(
            lines.contains("x-systemd.device-timeout=0"),
            "an encrypted disk is slow to appear"
        );
        assert!(lines.contains("86bf4581-fc83-4be5-b65d-bd71e1f59ff6"));
    }

    /// A stale offset is corrected rather than left alone, which is the
    /// whole reason these arguments are built by reading first.
    #[test]
    fn setting_the_kernel_arguments_replaces_what_is_there_rather_than_adding_to_it() {
        let stale = "root=UUID=86bf ro resume=UUID=86bf resume_offset=111 quiet";
        let args = karg_arguments(stale, "86bf", "222");
        assert_eq!(args[0], "kargs");
        assert!(args.contains(&"--delete-if-present=resume_offset=111".to_string()), "{args:?}");
        assert!(args.contains(&"--append=resume_offset=222".to_string()), "{args:?}");
        // The delete has to come before the append, or rpm-ostree
        // removes the argument that was just added.
        let deleted = args.iter().position(|a| a.starts_with("--delete")).unwrap();
        let appended = args.iter().position(|a| a.starts_with("--append")).unwrap();
        assert!(deleted < appended, "delete before append: {args:?}");

        // A machine that has never had these gets appends and nothing else.
        let fresh = karg_arguments("root=UUID=86bf ro quiet", "86bf", "222");
        assert!(!fresh.iter().any(|a| a.starts_with("--delete")), "{fresh:?}");
        assert_eq!(fresh.len(), 3);

        // And removal from a machine with nothing to remove is no work
        // at all, rather than a deployment staged to change nothing.
        assert!(karg_removal("root=UUID=86bf ro quiet").is_empty());
        assert_eq!(karg_removal(stale).len(), 3);
    }

    /// Turning hibernate off has to leave the file exactly as turning it
    /// on found it, including not growing it by a blank line each time.
    #[test]
    fn the_fstab_block_can_be_taken_back_out_without_a_trace() {
        let original = "UUID=1 /boot ext4 defaults 1 2\nUUID=2 /var btrfs subvol=var 0 0\n";
        let with = format!("{original}{}", fstab_lines("abc"));
        assert!(with.contains("/var/swap"));
        assert_eq!(strip_fstab(&with), original, "removal restores the original byte for byte");

        // Twice on, twice off, still the original.
        let twice = format!("{}{}", strip_fstab(&with), fstab_lines("abc"));
        assert_eq!(strip_fstab(&twice), original);

        // Somebody else's line mentioning the same path is not kuma's to
        // delete.
        let theirs = format!("{original}/var/swap/other none swap defaults 0 0\n");
        assert_eq!(strip_fstab(&theirs), theirs);

        // A block whose close marker somebody deleted by hand still ends
        // the removal at the end of the file rather than refusing.
        let broken = format!("{original}{}\nUUID=3 /home btrfs subvol=home 0 0\n", FSTAB_OPEN);
        assert_eq!(strip_fstab(&broken), original);
    }

    /// Both generated scripts have to be shell before they are anything
    /// else, and both of them mount something, so a quoting mistake
    /// leaves a mount behind on a machine somebody is using.
    #[test]
    fn the_verb_scripts_parse_as_shell() {
        for (name, script) in [("enable", enable_script()), ("disable", disable_script())] {
            let out = std::process::Command::new("bash")
                .args(["-n", "/dev/stdin"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(script.as_bytes())?;
                    child.wait_with_output()
                })
                .expect("bash is there to parse it");
            assert!(
                out.status.success(),
                "{name} is not shell:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// Turning it off has to stop the swap before it deletes what the
    /// swap is on: btrfs refuses to delete a subvolume holding an active
    /// swapfile, and a swapfile freed under a live swap area is worse.
    #[test]
    fn disabling_stops_the_swap_before_it_deletes_the_file() {
        let script = disable_script();
        let off = script.find("swapoff").expect("it turns swap off");
        let delete = script.find("subvolume delete").expect("it deletes the subvolume");
        assert!(off < delete, "swapoff has to come first");
        assert!(script.contains("|| true"), "every step tolerates having already happened");
        assert!(
            script.contains("cat \"$stripped\" > \"$fstab\""),
            "the fstab keeps its inode and its label"
        );
    }

    /// The kargs name the filesystem by UUID, which is the one spelling
    /// that works on both an encrypted and a plain machine.
    #[test]
    fn the_kernel_arguments_name_the_filesystem_rather_than_a_device_node() {
        let args = kargs("86bf4581-fc83-4be5-b65d-bd71e1f59ff6", "269312");
        assert_eq!(args[0], "resume=UUID=86bf4581-fc83-4be5-b65d-bd71e1f59ff6");
        assert_eq!(args[1], "resume_offset=269312");
        assert!(
            !args[0].contains("/dev/"),
            "a mapper node does not exist when the kernel reads this"
        );

        // The installer's spelling, where both halves are shell the
        // script has not run yet. Same function, so the two paths cannot
        // drift into naming the argument two different ways.
        let deferred = kargs("$fs_uuid", "$swap_offset");
        assert_eq!(deferred[0], "resume=UUID=$fs_uuid");
        assert_eq!(deferred[1], "resume_offset=$swap_offset");
    }

    /// The lid setting is two lines, one section, and deliberately names
    /// no HibernateDelaySec: the battery-alarm behavior that comes from
    /// NOT naming one is the product. Pinned so a future edit that
    /// "helpfully" adds a delay fails here rather than shipping a
    /// machine that hibernates on a timer instead of on the battery's
    /// own word.
    #[test]
    fn the_lid_setting_is_the_switch_and_no_timer() {
        let dropin = lid_dropin();
        assert_eq!(
            dropin,
            "[Login]\nHandleLidSwitch=suspend-then-hibernate\n\
             HandleLidSwitchExternalPower=suspend-then-hibernate\n"
        );
        assert!(!dropin.contains("HibernateDelaySec"), "the delay is the battery's to say");
        assert_eq!(
            LID_DROPIN, "/etc/systemd/logind.conf.d/kuma-suspend-then-hibernate.conf",
            "the path is a promise the installer and the verb both write to"
        );
    }

    /// Taking hibernate away takes the lid setting with it: a drop-in
    /// that outlives its swapfile is the stale state lid_verdict names,
    /// and this verb is the only thing that could have left it.
    #[test]
    fn disabling_takes_the_lid_setting_with_it() {
        let script = disable_script();
        let rm = script.find(&format!("rm -f {LID_DROPIN}")).expect("it removes the drop-in");
        let delete = script.find("subvolume delete").expect("it deletes the subvolume");
        assert!(
            delete < rm,
            "the swapfile goes first; the lid setting is the cheapest thing there"
        );
    }

    /// The lid grades against the hibernate it rides on, and each
    /// combination says its own thing. Ready-without-it is the failure
    /// that matters most: a machine that CAN hibernate and never will,
    /// because the one trigger laptops actually use was never pointed
    /// at it.
    #[test]
    fn the_lid_is_graded_against_the_hibernate_it_rides_on() {
        let ready = Verdict::Ready("swapfile active".into());
        assert_eq!(
            lid_verdict(&ready, true),
            LidVerdict::Ok(
                "closing the lid suspends, then hibernates before the battery runs out".into()
            )
        );
        assert_eq!(
            lid_verdict(&ready, false),
            LidVerdict::Missing(
                "hibernate is set up but closing the lid only suspends, so a machine left \
             asleep in a bag drains and dies instead of hibernating"
                    .into()
            )
        );
        assert_eq!(
            lid_verdict(&Verdict::NotSet, true),
            LidVerdict::Stale(
                "the lid is set to suspend-then-hibernate but there is no swapfile to \
             hibernate into, so it silently falls back to plain suspend"
                    .into()
            )
        );
        assert_eq!(lid_verdict(&Verdict::NotSet, false), LidVerdict::Silent);
        // A machine whose hibernate is already broken, short, refused or
        // unasked has its line from the hibernate check; the lid stays
        // quiet rather than piling on.
        for verdict in [
            Verdict::Broken("no swapfile".into()),
            Verdict::Short("too small".into()),
            Verdict::Refused("locked down".into()),
            Verdict::Unasked("needs sudo".into()),
        ] {
            for lid in [true, false] {
                assert_eq!(lid_verdict(&verdict, lid), LidVerdict::Silent);
            }
        }
    }
}
