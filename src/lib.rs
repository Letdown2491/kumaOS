//! The nostr layer (44.4.0): code shared between the bins that speak it.
//!
//! `kuma` the system tool does not link any of this; the layer's daemon
//! (`kuma-nostrd`) and CLI (`kuma-nostr`) do. It lives as a library so
//! those bins hold one compiled copy of the vault and its key handling,
//! and so the offline test suite for the pure parts runs here, where the
//! daemons that consume it cannot quietly diverge from what was proven.
//!
//! What is here is deliberately narrow: the vault (a gate-style lock over
//! the Secret Service) and the key import/decode surface. The transport,
//! the policy engine and the socket protocol are daemon concerns and will
//! live beside the daemon when they arrive; what forced their hand into a
//! library was the vault, which both binaries must agree on down to the
//! byte format of what the keyring holds.

pub mod nostr;
pub mod relay;
pub mod wake;

use std::io::Write as _;
use std::path::Path;

/// Write `text` to `file` privately and atomically: created at 0600,
/// renamed into place, never following a symlink. The nostr state
/// files and `kuma.lock` both go through this — the lock's digest is
/// interpolated into `FROM name@…`, so a tampered lock is a build
/// injection, and a lock a reader saw half-written is a rebuild
/// nobody asked for.
///
/// The file is *created* at 0600 rather than written-then-chmodded:
/// `std::fs::write` materializes at umask width and only narrows after,
/// a window an inotify watcher can open through and hold past the
/// chmod (an open fd keeps its read across the rename). create_new is
/// also O_EXCL, which refuses to follow a link planted at the tmp
/// name, and the sync makes the rename mean what it says across a
/// power cut. The state dir is tightened as well as created — a dir
/// made by the pre-0600 daemon is 0755, and 0600 files inside a
/// world-traversable dir are enumerated, readably named, even when
/// their bytes are closed. `pub`, not `pub(crate)`: the kuma bin's
/// lock module is compiled outside this crate and needs it too.
pub fn write_private(file: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::fs::PermissionsExt as _;

    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    let tmp = file.with_file_name(format!(".{name}.kuma-tmp"));
    // A tmp a kill left behind must not wedge every later flush, so it
    // goes before the create_new — which would otherwise refuse it
    // forever.
    let _ = std::fs::remove_file(&tmp);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
