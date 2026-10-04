//! The boot menu's titles, kept naming what they boot.
//!
//! ostree writes one BLS entry per deployment, and rewrites those files
//! only when the bootloader configuration it compares actually moves:
//! the set of boot checksums and the kernel arguments. A kuma release
//! moves neither. `[base]` in the lock pins a digest, so a rebuild
//! reuses the same composed base and therefore the same kernel, and
//! nothing between releases edits kargs. So every deploy takes ostree's
//! fast path: the `/ostree/boot.N/<osname>/<bootcsum>/<index>` symlinks
//! are rotated onto the new deployment order and the entry files are
//! left exactly as they were.
//!
//! The title is not part of what ostree compares, so it stays put while
//! the deployment underneath it moves, and every entry ends up naming
//! the version that used to hold its slot. Measured on a machine booted
//! into 0.12.0: the default entry read `Kuma 0.11.0` and the rollback
//! entry read `Kuma 0.10.0`. The order is still right, so the menu boots
//! what it should; it just names all of it wrong. That is worth fixing
//! because the menu is what somebody reads when the machine will not
//! come up far enough to run `kuma rollback`, which is the moment being
//! told the wrong version costs the most.
//!
//! The repair is possible because the entry carries the answer itself:
//! its `ostree=` karg names the deployment it boots, and that
//! deployment's own os-release says what it is. Nothing here guesses a
//! version, parses one out of a filename, or predicts a rotation that
//! has not happened yet.
//!
//! On *when* this runs, see `BOOT_TITLES_SERVICE` in containerfile.rs:
//! the rotation happens at shutdown, inside `ostree-finalize-staged`,
//! so a pass that runs when kuma stages an image would write titles for
//! an arrangement that has not happened yet.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Where a booted machine keeps its BLS entries. `/boot/loader` is a
/// symlink into the `loader.N` ostree currently owns, so writing through
/// it lands in the live set.
pub const ENTRIES: &str = "/boot/loader/entries";

/// One entry whose title disagrees with the deployment it boots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retitle {
    pub entry: PathBuf,
    pub from: String,
    pub to: String,
}

impl Retitle {
    /// The file name alone, for a doctor line that has to fit on one.
    pub fn name(&self) -> String {
        self.entry.file_name().unwrap_or_default().to_string_lossy().to_string()
    }
}

/// A BLS field is a key, whitespace, then the value. The whitespace is
/// the whole check: without it `titles` would parse as `title`.
fn is_field(line: &str, key: &str) -> bool {
    line.trim_start().strip_prefix(key).is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

fn field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let line = body.lines().find(|line| is_field(line, key))?;
    Some(line.trim_start()[key.len()..].trim())
}

/// The deployment path out of `options`, e.g.
/// `/ostree/boot.0/default/<bootcsum>/0`. An entry without one is not
/// ostree's (another OS, a rescue entry) and is never touched.
fn ostree_karg(options: &str) -> Option<&str> {
    options.split_whitespace().find_map(|karg| karg.strip_prefix("ostree="))
}

/// The deployment index is the karg's last component, and it is the
/// authority on which slot this entry boots.
fn karg_index(karg: &str) -> Option<u32> {
    karg.rsplit('/').next()?.parse().ok()
}

fn pretty_name(os_release: &str) -> Option<String> {
    let line = os_release.lines().map(str::trim).find(|l| l.starts_with("PRETTY_NAME="))?;
    let value = line["PRETTY_NAME=".len()..].trim();
    let unquoted = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    (!unquoted.is_empty()).then(|| unquoted.to_string())
}

/// What the deployment behind a karg calls itself. `/usr/lib/os-release`
/// is the image's own copy and `/etc/os-release` is normally a symlink
/// to it; both are read because a machine may carry a local one.
fn deployment_name(sysroot: &Path, karg: &str) -> Option<String> {
    let dir = sysroot.join(karg.trim_start_matches('/'));
    ["usr/lib/os-release", "etc/os-release"]
        .iter()
        .filter_map(|rel| std::fs::read_to_string(dir.join(rel)).ok())
        .find_map(|body| pretty_name(&body))
}

/// ostree suffixes a title with `(ostree:N)` when a machine has more
/// than one deployment and writes a bare one when it has a single
/// deployment. The suffix is reproduced only where one already exists,
/// so this never invents menu text ostree would not have written. The
/// index inside it comes from the karg rather than from the old title,
/// because the old title is precisely the thing that is out of date.
fn suffix_index(title: &str) -> Option<u32> {
    let (_, tail) = title.trim_end().strip_suffix(')')?.rsplit_once("(ostree:")?;
    tail.parse().ok()
}

/// A title without its `(ostree:N)` suffix. The slot is identical on
/// both sides of a doctor line about a stale title, so printing it twice
/// is two more words and no more information.
pub fn without_slot(title: &str) -> &str {
    match title.trim_end().rsplit_once(" (ostree:") {
        Some((head, tail)) if tail.trim_end_matches(')').parse::<u32>().is_ok() => head,
        _ => title,
    }
}

/// What this entry says and what it should say. None whenever the
/// question cannot be answered from the machine itself: no title, no
/// `ostree=` karg, an index that will not parse, or a deployment whose
/// os-release cannot be read. Every one of those leaves the entry
/// untouched, because a stale title is a cosmetic defect and a mangled
/// entry is a machine that does not boot.
fn wanted(body: &str, sysroot: &Path) -> Option<(String, String)> {
    let current = field(body, "title")?.to_string();
    let karg = ostree_karg(field(body, "options")?)?;
    let pretty = deployment_name(sysroot, karg)?;
    let wanted = match suffix_index(&current) {
        Some(_) => format!("{pretty} (ostree:{})", karg_index(karg)?),
        None => pretty,
    };
    Some((current, wanted))
}

fn conf_files(entries: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(entries) else { return Vec::new() };
    let mut files: Vec<PathBuf> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "conf"))
        .collect();
    files.sort();
    files
}

/// Every entry whose title disagrees with the deployment it boots.
/// Read-only: this is what `kuma doctor` grades.
pub fn stale(entries: &Path, sysroot: &Path) -> Vec<Retitle> {
    conf_files(entries)
        .into_iter()
        .filter_map(|entry| {
            let body = std::fs::read_to_string(&entry).ok()?;
            let (from, to) = wanted(&body, sysroot)?;
            (from != to).then_some(Retitle { entry, from, to })
        })
        .collect()
}

/// Rewrite one entry's title, touching nothing else.
///
/// Every other line is copied through byte for byte, so no bug in here
/// can reach `options`, `linux` or `initrd` — the three lines that
/// decide whether the entry boots at all. Temp file, fsync, rename
/// inside the same directory, then fsync the directory: this runs during
/// shutdown, so "written" has to mean on the disk rather than in a cache
/// the power cut takes with it, and a half-written entry in /boot is a
/// menu line nobody would be around to notice.
fn write_title(entry: &Path, title: &str) -> std::io::Result<()> {
    let body = std::fs::read_to_string(entry)?;
    let mut out = String::with_capacity(body.len() + title.len());
    for line in body.lines() {
        if is_field(line, "title") {
            out.push_str("title ");
            out.push_str(title);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    let dir = entry.parent().unwrap_or_else(|| Path::new("."));
    let name = entry.file_name().unwrap_or_default().to_string_lossy().to_string();
    let tmp = dir.join(format!(".{name}.kuma"));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(out.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    if let Err(err) = std::fs::rename(&tmp, entry) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Ok(dir) = std::fs::File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Fix every entry that disagrees, and report what moved.
///
/// One entry failing does not stop the others: on a two-entry machine
/// the rollback entry is the one that matters most, and abandoning it
/// because the default entry was unwritable would be backwards. The
/// first error is still returned, so the unit fails and `kuma doctor`
/// reports it under `units` rather than the whole thing passing
/// silently.
pub fn apply(entries: &Path, sysroot: &Path) -> std::io::Result<Vec<Retitle>> {
    let mut moved = Vec::new();
    let mut failed = None;
    for retitle in stale(entries, sysroot) {
        match write_title(&retitle.entry, &retitle.to) {
            Ok(()) => moved.push(retitle),
            Err(err) => {
                if failed.is_none() {
                    failed = Some(err);
                }
            }
        }
    }
    match failed {
        Some(err) => Err(err),
        None => Ok(moved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSUM: &str = "29619d56d899fd0b60d748e00f6f0cccec25269c702e3fadc939c91c893820dd";

    fn entry_text(title: &str, index: u32) -> String {
        format!(
            "title {title}\nversion {}\noptions ostree=/ostree/boot.0/default/{CSUM}/{index} rd.luks.uuid=luks-f5d1 rhgb quiet root=UUID=86bf rw\nlinux /ostree/default-{CSUM}/vmlinuz-7.1.8-200.fc44.x86_64\ninitrd /ostree/default-{CSUM}/initramfs-7.1.8-200.fc44.x86_64.img\n",
            index + 1
        )
    }

    /// A deployment at `index` that calls itself `pretty`, wired up the
    /// way ostree wires one: a symlink from the boot slot to the deploy
    /// directory, and the os-release inside it.
    fn deployment(root: &Path, index: u32, checksum: &str, pretty: &str) {
        let deploy = root.join(format!("ostree/deploy/default/deploy/{checksum}.0"));
        std::fs::create_dir_all(deploy.join("usr/lib")).unwrap();
        std::fs::write(
            deploy.join("usr/lib/os-release"),
            format!("ID=kuma\nPRETTY_NAME=\"{pretty}\"\nVERSION_ID=44\n"),
        )
        .unwrap();
        let slot = root.join(format!("ostree/boot.0/default/{CSUM}"));
        std::fs::create_dir_all(&slot).unwrap();
        std::os::unix::fs::symlink(&deploy, slot.join(index.to_string())).unwrap();
    }

    /// A laptop on 2026-08-18, booted into 0.12.0: two deployments, and
    /// two entry titles each naming the version that used to hold the
    /// slot.
    fn off_by_one() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let entries = root.path().join("boot/loader/entries");
        std::fs::create_dir_all(&entries).unwrap();
        deployment(root.path(), 0, "eabbce68", "kumaOS 0.12.0 (Beorn)");
        deployment(root.path(), 1, "8e44abe7", "kumaOS 0.11.0 (Beorn)");
        std::fs::write(
            entries.join("ostree-2.conf"),
            entry_text("kumaOS 0.11.0 (Beorn) (ostree:0)", 0),
        )
        .unwrap();
        std::fs::write(
            entries.join("ostree-1.conf"),
            entry_text("kumaOS 0.10.0 (Beorn) (ostree:1)", 1),
        )
        .unwrap();
        root
    }

    fn entries_of(root: &tempfile::TempDir) -> PathBuf {
        root.path().join("boot/loader/entries")
    }

    fn titles(root: &tempfile::TempDir) -> Vec<String> {
        conf_files(&entries_of(root))
            .iter()
            .map(|path| {
                let body = std::fs::read_to_string(path).unwrap();
                field(&body, "title").unwrap().to_string()
            })
            .collect()
    }

    /// The whole bug, as measured: every entry names the version that
    /// used to hold its slot, and after a pass every entry names what it
    /// actually boots.
    #[test]
    fn every_title_names_the_deployment_it_boots() {
        let root = off_by_one();
        let found = stale(&entries_of(&root), root.path());
        assert_eq!(found.len(), 2, "both entries are wrong before the fix");
        let moved = apply(&entries_of(&root), root.path()).unwrap();
        assert_eq!(moved.len(), 2);
        assert_eq!(
            titles(&root),
            vec![
                "kumaOS 0.11.0 (Beorn) (ostree:1)".to_string(),
                "kumaOS 0.12.0 (Beorn) (ostree:0)".to_string(),
            ],
            "ostree-1 boots the 0.11.0 deployment, ostree-2 boots 0.12.0"
        );
        assert!(stale(&entries_of(&root), root.path()).is_empty(), "and it stays fixed");
    }

    /// The index in the rewritten title comes from the karg, which is
    /// what the entry boots, and never from the title being replaced.
    /// Sabotage: reusing the old title's index reproduces the bug in the
    /// fix, and only this catches it, because the deployment names alone
    /// would still look right.
    #[test]
    fn the_slot_number_comes_from_the_karg() {
        let root = tempfile::tempdir().unwrap();
        let entries = root.path().join("boot/loader/entries");
        std::fs::create_dir_all(&entries).unwrap();
        deployment(root.path(), 0, "eabbce68", "kumaOS 0.12.0 (Beorn)");
        std::fs::write(entries.join("a.conf"), entry_text("kumaOS 0.9.0 (Beorn) (ostree:7)", 0))
            .unwrap();
        apply(&entries, root.path()).unwrap();
        assert_eq!(titles(&root), vec!["kumaOS 0.12.0 (Beorn) (ostree:0)".to_string()]);
    }

    /// The three lines that decide whether the entry boots at all are
    /// copied through byte for byte.
    #[test]
    fn nothing_but_the_title_is_rewritten() {
        let root = off_by_one();
        let path = entries_of(&root).join("ostree-2.conf");
        let before = std::fs::read_to_string(&path).unwrap();
        apply(&entries_of(&root), root.path()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        let lines = |text: &str| {
            text.lines()
                .filter(|line| !is_field(line, "title"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(&before), lines(&after));
        assert_ne!(before, after, "the title itself did move");
    }

    /// An entry that already agrees is not rewritten at all: nothing to
    /// report, and no write to /boot on a machine with nothing to fix.
    #[test]
    fn an_agreeing_entry_is_left_alone() {
        let root = off_by_one();
        apply(&entries_of(&root), root.path()).unwrap();
        assert!(apply(&entries_of(&root), root.path()).unwrap().is_empty());
    }

    /// Another operating system's entry has no `ostree=` karg, and kuma
    /// has no business retitling it.
    #[test]
    fn a_foreign_entry_is_never_touched() {
        let root = off_by_one();
        let foreign = entries_of(&root).join("other-os.conf");
        let text = "title Some Other Linux\nlinux /vmlinuz\noptions root=UUID=1234 rw\n";
        std::fs::write(&foreign, text).unwrap();
        apply(&entries_of(&root), root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), text);
    }

    /// A single-deployment machine gets a bare title from ostree, and a
    /// bare title is what it keeps: the suffix is reproduced, never
    /// invented.
    #[test]
    fn a_bare_title_stays_bare() {
        let root = tempfile::tempdir().unwrap();
        let entries = root.path().join("boot/loader/entries");
        std::fs::create_dir_all(&entries).unwrap();
        deployment(root.path(), 0, "eabbce68", "kumaOS 0.12.0 (Beorn)");
        std::fs::write(entries.join("a.conf"), entry_text("kumaOS 0.11.0 (Beorn)", 0)).unwrap();
        apply(&entries, root.path()).unwrap();
        assert_eq!(titles(&root), vec!["kumaOS 0.12.0 (Beorn)".to_string()]);
    }

    /// A deployment whose os-release cannot be read answers nothing, so
    /// the entry keeps the title it has. The alternative is writing a
    /// title from a guess.
    #[test]
    fn an_unreadable_deployment_leaves_the_title_alone() {
        let root = tempfile::tempdir().unwrap();
        let entries = root.path().join("boot/loader/entries");
        std::fs::create_dir_all(&entries).unwrap();
        let text = entry_text("kumaOS 0.11.0 (Beorn) (ostree:0)", 0);
        std::fs::write(entries.join("a.conf"), &text).unwrap();
        assert!(stale(&entries, root.path()).is_empty());
        assert!(apply(&entries, root.path()).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(entries.join("a.conf")).unwrap(), text);
    }

    #[test]
    fn pretty_name_is_unquoted_and_a_field_needs_its_whitespace() {
        assert_eq!(
            pretty_name("PRETTY_NAME=\"kumaOS 0.12.0 (Beorn)\"\n").unwrap(),
            "kumaOS 0.12.0 (Beorn)"
        );
        assert_eq!(pretty_name("PRETTY_NAME='kumaOS 0.12.0'\n").unwrap(), "kumaOS 0.12.0");
        assert_eq!(pretty_name("PRETTY_NAME=kumaOS\n").unwrap(), "kumaOS");
        assert!(pretty_name("ID=kuma\n").is_none());
        assert!(is_field("title Kuma", "title"));
        assert!(!is_field("titles Kuma", "title"));
        assert!(!is_field("title", "title"));
    }

    #[test]
    fn the_karg_is_read_out_of_a_real_options_line() {
        let options = "ostree=/ostree/boot.0/default/abc/1 rd.luks.uuid=luks-f5d1 rw";
        assert_eq!(ostree_karg(options).unwrap(), "/ostree/boot.0/default/abc/1");
        assert_eq!(karg_index("/ostree/boot.0/default/abc/1").unwrap(), 1);
        assert!(ostree_karg("root=UUID=1234 rw").is_none());
        assert_eq!(suffix_index("kumaOS 0.12.0 (Beorn) (ostree:3)").unwrap(), 3);
        assert!(suffix_index("kumaOS 0.12.0 (Beorn)").is_none());
        assert_eq!(without_slot("kumaOS 0.12.0 (Beorn) (ostree:3)"), "kumaOS 0.12.0 (Beorn)");
        // A version that ends in a parenthesis of its own keeps it.
        assert_eq!(without_slot("kumaOS 0.12.0 (Beorn)"), "kumaOS 0.12.0 (Beorn)");
    }
}
