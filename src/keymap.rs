//! The keymap: one layout, four layers, one of them image content.
//!
//! A keyboard layout has to land in several places, and only one of them
//! is the desktop session:
//!
//! - the session: `/etc/kuma/keymap.kdl`, an input stanza the session
//!   config includes ahead of its local.kdl include, so the machine's
//!   layout wins over the defaults and a person's own override still
//!   wins over the machine;
//! - the greeter: the same file, appended by the greeter session
//!   wrapper to a runtime copy of the pinned greeter config — niri
//!   merges includes positionally, so the append is the last word there
//!   too;
//! - the TTYs: `/etc/vconsole.conf`, via `localectl set-keymap` —
//!   systemd owns that layer, validates the name against the keymaps it
//!   actually ships, and applies it to the console live;
//! - the initramfs: dracut bakes `vconsole.conf` into the image's
//!   initrd, and that initrd is what the LUKS passphrase prompt speaks.
//!   On a bootc machine the initrd is image content — no runtime verb
//!   can regenerate it — so this layer moves only when a compose
//!   carries `[system].keymap` and regenerates the initrd itself.
//!
//! `kuma keymap apply` moves the first three and says so about the
//! fourth; `kuma keymap status` reads every layer back; `kuma doctor`
//! warns when they disagree. The compose side (containerfile::blocks)
//! writes the stanza this module renders — one owner of the bytes — and
//! the delta compiler refuses a keymap move outright, because a delta
//! that carried the three runtime layers alone would leave the
//! passphrase prompt speaking the old layout: exactly the half-moved
//! state the refusal list exists for.

use anyhow::{bail, Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::host::{note, run_host};
use crate::state::BAKED_CONFIG;

/// The stanza file: the session include and the greeter wrapper's
/// append both read it, so one write moves both layers.
pub(crate) const KEYMAP_KDL: &str = "/etc/kuma/keymap.kdl";

/// The TTY layer. Written by the compose that names a keymap and by
/// `localectl set-keymap` at runtime; dracut copies it into the
/// initramfs at compose time.
pub(crate) const VCONSOLE_CONF: &str = "/etc/vconsole.conf";

/// The session config the include line lives in. Its presence is what
/// tells an image that wires the stanza from one that predates it.
pub(crate) const SESSION_CONFIG: &str = "/etc/niri/config.kdl";

/// The include line the session config carries. One definition: the
/// blocks.rs constant that ships it and this reader are pinned together
/// by a containerfile test, so a moved include is a moved needle.
pub(crate) const KDL_INCLUDE: &str = "include optional=true \"/etc/kuma/keymap.kdl\"";

/// The stanza's vocabulary: what a keymap name may be spelled from.
/// `localectl list-keymaps` names are lowercase with hyphens ("us",
/// "de-latin1"); the value is written into a KDL string and a
/// `KEYMAP=` line verbatim, so quotes, spaces and dollars are out
/// before they can mean something to either parser.
pub(crate) fn validate(layout: &str) -> Result<()> {
    if layout.is_empty() {
        bail!("a keymap name cannot be empty");
    }
    for ch in layout.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' && ch != '_' {
            bail!(
                "keymap name {layout:?} contains {ch:?}; a keymap is spelled from \
                 lowercase letters, digits, '-' and '_' (see `localectl list-keymaps`). \
                 XKB variants are not keymap names — the same value has to work for \
                 the console and the session both."
            );
        }
    }
    // A leading dash turns the name into a flag for whatever command
    // the value feeds — loadkeys, localectl, the stanza's spelling. No
    // real keymap starts with one.
    if layout.starts_with('-') {
        bail!("keymap name {layout:?} starts with '-': names cannot be options");
    }
    Ok(())
}

/// The niri input stanza: the bytes both the compose and `kuma keymap
/// apply` write, so a baked keymap and an applied one are the same
/// file by construction.
pub(crate) fn stanza(layout: &str) -> String {
    format!(
        "// The machine's keymap: written by the compose that named\n\
         // [system].keymap or by `kuma keymap apply` at runtime. The\n\
         // session config includes this file and the greeter's wrapper\n\
         // appends it; niri merges positionally, so this is the last word\n\
         // in both, short of a person's own local.kdl.\n\
         input {{\n\
         \x20   keyboard {{\n\
         \x20       xkb {{\n\
         \x20           layout \"{layout}\"\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n"
    )
}

/// Every layer's layout, read back off the machine. `None` is a real
/// answer (the layer speaks the default), not a read failure — a layer
/// that cannot be read is indistinguishable from one that is unset, the
/// same keep-last-known-state rule the shell's polls run on.
#[derive(Debug, PartialEq)]
pub(crate) struct Layers {
    /// `/etc/kuma/keymap.kdl`: the session's include and the greeter's
    /// append.
    pub chosen: Option<String>,
    /// `/etc/vconsole.conf`: the TTYs.
    pub vconsole: Option<String>,
    /// The image's own `[system].keymap`: the layout its initramfs —
    /// the LUKS passphrase prompt — was baked with.
    pub baked: Option<String>,
    /// Whether the booted image's session config wires the stanza file
    /// at all. False on images that predate the include: the stanza then
    /// reaches only the TTYs.
    pub wired: bool,
}

impl Layers {
    pub(crate) fn read() -> Layers {
        Layers {
            chosen: std::fs::read_to_string(KEYMAP_KDL).ok().as_deref().and_then(parse_stanza),
            vconsole: std::fs::read_to_string(VCONSOLE_CONF)
                .ok()
                .as_deref()
                .and_then(parse_vconsole),
            baked: baked_keymap().ok().flatten(),
            wired: std::fs::read_to_string(SESSION_CONFIG)
                .map(|config| config.contains(KDL_INCLUDE))
                .unwrap_or(false),
        }
    }

    /// Where the layers disagree, as (detail, verb, command, why) — the
    /// shape both `kuma keymap status` narrates and doctor turns into a
    /// Warn with its fix attached. Empty when every named layer agrees;
    /// also empty when no layer names anything, which is agreement by
    /// construction.
    pub(crate) fn drifts(&self) -> Vec<Drift> {
        let mut out = Vec::new();
        let named = self.chosen.is_some() || self.vconsole.is_some() || self.baked.is_some();
        if !named {
            return out;
        }
        // The session and greeter against the TTYs. All four shapes
        // count: two layers set to different layouts, and either one set
        // while the other fell back to the default — both are machines
        // whose keyboard changes under them when they leave the session.
        match (&self.chosen, &self.vconsole) {
            (Some(c), Some(v)) if c != v => out.push(Drift {
                detail: format!("the session and the greeter speak {c:?}, the TTYs speak {v:?}"),
                verb: "keymap",
                cmd: format!("sudo kuma keymap apply {c}"),
                why: "re-apply the layout so the console matches the session".into(),
            }),
            (Some(c), None) => out.push(Drift {
                detail: format!("the session and the greeter speak {c:?}, the TTYs the default"),
                verb: "keymap",
                cmd: format!("sudo kuma keymap apply {c}"),
                why: "re-apply the layout so the console matches the session".into(),
            }),
            (None, Some(v)) => out.push(Drift {
                detail: format!("the TTYs speak {v:?}, the session and the greeter the default"),
                verb: "keymap",
                cmd: format!("sudo kuma keymap apply {v}"),
                why: "apply the layout the console uses, so the session matches".into(),
            }),
            _ => {}
        }
        // The machine against its image. The baked layer is the one no
        // runtime verb can move: the initramfs the LUKS passphrase
        // prompt speaks is image content, and until a compose carries
        // the layout the prompt keeps speaking the baked one.
        if self.chosen.as_deref() != self.baked.as_deref() {
            match (&self.chosen, &self.baked) {
                (Some(c), Some(b)) => out.push(Drift {
                    detail: format!(
                        "the machine speaks {c:?} but the image's initramfs (the LUKS \
                         passphrase prompt) was baked with {b:?}"
                    ),
                    verb: "update",
                    cmd: "sudo kuma update".into(),
                    why: format!(
                        "set [system].keymap = \"{c}\" in kuma.toml; the next full \
                         rebuild bakes it into the initramfs"
                    ),
                }),
                (Some(c), None) => out.push(Drift {
                    detail: format!(
                        "the machine speaks {c:?} but the image bakes no keymap: the LUKS \
                         passphrase prompt still speaks the default layout"
                    ),
                    verb: "update",
                    cmd: "sudo kuma update".into(),
                    why: format!(
                        "set [system].keymap = \"{c}\" in kuma.toml; the next full \
                         rebuild bakes it into the initramfs"
                    ),
                }),
                (None, Some(b)) => out.push(Drift {
                    detail: format!(
                        "the image bakes keymap {b:?} but the machine's stanza file is \
                         gone: the session, the greeter and the TTYs fell back to the \
                         default"
                    ),
                    verb: "keymap",
                    cmd: format!("sudo kuma keymap apply {b}"),
                    why: "restore the machine's layer to the layout the image bakes".into(),
                }),
                // No layer named anything is already handled above.
                (None, None) => {}
            }
        }
        // An image too old to wire the stanza in: `apply` ran on it, the
        // TTYs moved, and the session and greeter cannot follow until an
        // image that carries the include arrives.
        if self.chosen.is_some() && !self.wired {
            out.push(Drift {
                detail: "the booted image's session config predates the keymap include: the \
                         layout reaches the TTYs but not the session or the greeter"
                    .into(),
                verb: "update",
                cmd: "sudo kuma update".into(),
                why: "the next image carries the include the session needs".into(),
            });
        }
        out
    }
}

/// One disagreement, ready for both the status narrator and doctor's
/// Warn-with-fix. `verb`/`cmd`/`why` mirror state::Action's fields.
#[derive(Debug)]
pub(crate) struct Drift {
    pub detail: String,
    pub verb: &'static str,
    pub cmd: String,
    pub why: String,
}

/// The layout out of a stanza file: the `layout "de"` line of the
/// format `stanza()` writes. Anything else in the file is ignored, so a
/// hand-tuned stanza still answers status and doctor with the layout it
/// actually sets.
fn parse_stanza(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("layout ") {
            let value = rest.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The KEYMAP= line out of vconsole.conf, comments skipped.
fn parse_vconsole(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("KEYMAP=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// The baked declaration's `[system].keymap`, read tolerantly: a baked
/// declaration from a newer kuma may carry sections this one cannot
/// parse as Config, and the keymap question should still get its
/// answer. A declaration that is TOML-broken fails here — loud, because
/// the other checks that read it say the same thing.
fn baked_keymap() -> Result<Option<String>> {
    let text = std::fs::read_to_string(BAKED_CONFIG).unwrap_or_default();
    if text.is_empty() {
        return Ok(None);
    }
    let root: toml::Value = toml::from_str(&text).context("the baked declaration is unreadable")?;
    Ok(root
        .get("system")
        .and_then(|system| system.get("keymap"))
        .and_then(|v| v.as_str())
        .map(String::from))
}

/// The write half of `apply`, split from the verb so the mode and the
/// atomicity are testable against a tempdir the way the heal's and the
/// entry writer's are. Temp in the destination's own directory, 0644
/// explicit — the stanza carries no secret and the greeter's own user
/// has to read it, so a hardened root umask must not be able to lock
/// the greeter out of the layout silently — then rename, with the temp
/// cleaned up if the rename refuses. The create covers pre-include
/// images, whose compose never made `/etc/kuma`.
pub(crate) fn write_stanza_file(path: &Path, layout: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| {
        format!("cannot create {}: kuma keymap apply needs root", dir.display())
    })?;
    let tmp = dir.join(".keymap.kdl.kuma-tmp");
    std::fs::write(&tmp, stanza(layout)).with_context(|| {
        format!(
            "cannot write {}: kuma keymap apply needs root — `sudo kuma keymap apply {layout}`",
            path.display()
        )
    })?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err)
            .with_context(|| format!("cannot put the stanza in place at {}", path.display()));
    }
    Ok(())
}

/// `kuma keymap apply`: write the layout into the layers a runtime verb
/// can move, and say plainly which one it cannot. Root's job — the
/// stanza lives in /etc and localectl answers to polkit — and a call
/// without it fails at the first write with the sudo line in the
/// error's context.
pub(crate) fn apply(layout: &str, json: bool) -> Result<()> {
    validate(layout)?;

    // The session and greeter layer: one file, both readers. niri
    // watches the files it includes, so the running session picks the
    // change up on its own; the greeter assembles its copy at wrapper
    // start, so it picks the change up on its next start. Atomic, so a
    // crash mid-write never leaves a half stanza for the greeter's
    // next start to assemble a broken config from.
    write_stanza_file(Path::new(KEYMAP_KDL), layout)?;
    note(&format!(
        "{KEYMAP_KDL}: the session and greeter layer written; the session reloads as \
         niri notices the file, the greeter on its next start"
    ));

    // The TTY layer: systemd validates the name and applies it to the
    // console live, so a name `localectl` never heard of fails here
    // rather than shipping a keyboard nobody can type on.
    run_host(&["localectl", "set-keymap", layout]).with_context(|| {
        format!(
            "localectl set-keymap {layout} failed: the TTY layer did not move (needs \
             root, and the name has to be in `localectl list-keymaps`)"
        )
    })?;
    note(&format!("localectl set-keymap: the TTYs speak {layout:?} now"));

    // The boot layer, read back now that the write landed: the include
    // tells the caller whether this image's session could follow at
    // all, and the initramfs fact gets its own line either way — the
    // one layer this verb cannot move is the one nobody should
    // discover at the passphrase prompt.
    let layers = Layers::read();
    if !layers.wired {
        note(
            "this image's session config predates the keymap include: the session and the \
             greeter pick the layout up on the next image that carries the include",
        );
    }
    note(
        "the initramfs (the LUKS passphrase prompt) is image content: set [system].keymap \
         in kuma.toml and run `kuma update` to move it — a runtime apply never can",
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "applied": layout,
                "chosen": layers.chosen,
                "vconsole": layers.vconsole,
                "baked": layers.baked,
                "session_includes_keymap": layers.wired,
            }))?
        );
    }
    Ok(())
}

/// `kuma keymap status`: every layer's layout, plus the drift lines —
/// the same words doctor grades, so a caller never reconciles two
/// vocabularies for one fact.
pub(crate) fn status(json: bool) -> Result<()> {
    let layers = Layers::read();
    let drifts: Vec<String> = layers.drifts().iter().map(|d| d.detail.clone()).collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "chosen": layers.chosen,
                "vconsole": layers.vconsole,
                "baked": layers.baked,
                "session_includes_keymap": layers.wired,
                "drift": drifts,
            }))?
        );
        return Ok(());
    }
    let say = |label: &str, value: &Option<String>| match value {
        Some(km) => println!("{label}: {km}"),
        None => println!("{label}: (default)"),
    };
    say(&format!("session and greeter ({KEYMAP_KDL})"), &layers.chosen);
    say(&format!("TTYs ({VCONSOLE_CONF})"), &layers.vconsole);
    say("image's baked [system].keymap", &layers.baked);
    if !layers.wired {
        println!(
            "this image's session config predates the keymap include: the stanza reaches \
             only the TTYs"
        );
    }
    println!(
        "the initramfs (the LUKS passphrase prompt) speaks the baked layout: a runtime \
         apply cannot move it, a compose carrying [system].keymap can"
    );
    for detail in drifts {
        println!("drift: {detail}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_names_are_console_vocabulary() {
        for ok in ["de", "us", "de-latin1", "dvorak", "es_winkeys", "cz-qwerty"] {
            validate(ok).unwrap_or_else(|e| panic!("{ok} should validate: {e}"));
        }
        for bad in [
            "",            // empty
            "De",          // the vocabulary is lowercase
            "de latin1",   // a space is two names, or an argv split
            "de-latin1 ",  // trailing whitespace into vconsole.conf
            "\"de\"",      // quotes into the KDL string
            "de$(reboot)", // the value rides an echo and a stanza
            "-de",         // leading dash: a flag for whatever runs it
        ] {
            assert!(validate(bad).is_err(), "{bad:?} should refuse");
        }
    }

    #[test]
    fn the_stanza_is_what_the_session_include_and_the_greeter_append() {
        let text = stanza("de");
        assert!(text.starts_with("//"), "the stanza explains itself");
        assert!(text.contains("input {"), "{text}");
        assert!(text.contains("layout \"de\""), "{text}");
        // The parse of our own format round-trips: status reads back
        // what apply wrote.
        assert_eq!(parse_stanza(&text).as_deref(), Some("de"));
    }

    #[test]
    fn a_hand_tuned_stanza_still_answers_its_layout() {
        // The stanza file is /etc content, and /etc is where people
        // edit. The parser reads the layout line and ignores the rest.
        let tuned = "// mine\ninput {\n    keyboard {\n        xkb {\n \
                    // variant kept by hand\n            layout \"us\"  \n        }\n    }\n}\n";
        assert_eq!(parse_stanza(tuned).as_deref(), Some("us"));
        assert_eq!(parse_stanza("no layout here"), None);
    }

    #[test]
    fn vconsole_reads_the_keymap_line_only() {
        let conf = "# Edited by kuma\nKEYMAP=de-latin1\nFONT=eurlatgr\n";
        assert_eq!(parse_vconsole(conf).as_deref(), Some("de-latin1"));
        // A commented-out line is a memory, not a setting.
        assert_eq!(parse_vconsole("# KEYMAP=us\nKEYMAP=de\n").as_deref(), Some("de"));
        assert_eq!(parse_vconsole(""), None);
    }

    #[test]
    fn layers_that_agree_are_not_drift() {
        let all_none = Layers { chosen: None, vconsole: None, baked: None, wired: true };
        assert!(all_none.drifts().is_empty(), "nothing named anywhere agrees by construction");

        let all_de = Layers {
            chosen: Some("de".into()),
            vconsole: Some("de".into()),
            baked: Some("de".into()),
            wired: true,
        };
        assert!(all_de.drifts().is_empty());

        // Only the console named, with the image agreeing on it: the
        // stanza file is still gone, and that is drift twice over —
        // the session fell back to the default AND the file the image
        // bakes is missing. Both lines name the missing piece.
        let console_only = Layers {
            chosen: None,
            vconsole: Some("us".into()),
            baked: Some("us".into()),
            wired: true,
        };
        let drifts = console_only.drifts();
        assert_eq!(drifts.len(), 2, "{drifts:?}");
        assert!(drifts[0].cmd.contains("apply us"), "{}", drifts[0].cmd);
        assert!(drifts[1].detail.contains("stanza file is gone"), "{}", drifts[1].detail);
    }

    #[test]
    fn a_runtime_apply_without_a_recompose_is_the_warn_the_check_exists_for() {
        // The trap from the issue: the session moved, the prompt did
        // not, and nothing told anybody.
        let applied = Layers {
            chosen: Some("de".into()),
            vconsole: Some("de".into()),
            baked: None,
            wired: true,
        };
        let drifts = applied.drifts();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert!(drifts[0].detail.contains("LUKS"), "{}", drifts[0].detail);
        assert_eq!(drifts[0].verb, "update");
        assert!(drifts[0].cmd.contains("kuma update"), "{}", drifts[0].cmd);
        assert!(drifts[0].why.contains("[system].keymap"), "{}", drifts[0].why);
    }

    #[test]
    fn layer_mismatches_each_name_the_pair_that_disagrees() {
        let crossed = Layers {
            chosen: Some("de".into()),
            vconsole: Some("us".into()),
            baked: Some("de".into()),
            wired: true,
        };
        let drifts = crossed.drifts();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert!(drifts[0].detail.contains("us"), "{}", drifts[0].detail);
        assert!(drifts[0].cmd.contains("apply de"), "{}", drifts[0].cmd);

        // Baked one way, applied another: the prompt speaks the old one.
        let rebaked = Layers {
            chosen: Some("fr".into()),
            vconsole: Some("fr".into()),
            baked: Some("de".into()),
            wired: true,
        };
        let drifts = rebaked.drifts();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert!(drifts[0].detail.contains("fr"), "{}", drifts[0].detail);
        assert!(drifts[0].detail.contains("de"), "{}", drifts[0].detail);

        // The stanza file deleted: everything fell back to the default,
        // the image still bakes the layout — apply it back.
        let deleted =
            Layers { chosen: None, vconsole: None, baked: Some("de".into()), wired: true };
        let drifts = deleted.drifts();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert!(drifts[0].cmd.contains("apply de"), "{}", drifts[0].cmd);
    }

    #[test]
    fn the_stanza_write_is_the_entry_writer_s_own_shape() {
        // The audit's fix: temp in the destination's own directory, the
        // mode set before the rename, no temp left behind. The mode is
        // the security-relevant half — the greeter's own user reads
        // this file, so a hardened root umask must not be able to land
        // it 0600 and lock the greeter out of the layout silently.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nested").join("keymap.kdl");
        write_stanza_file(&dest, "de").unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the stanza is world-readable on purpose");
        assert!(
            !dir.path().join("nested").join(".keymap.kdl.kuma-tmp").exists(),
            "the rename leaves no temp behind"
        );
        assert_eq!(parse_stanza(&std::fs::read_to_string(&dest).unwrap()).as_deref(), Some("de"));
        // A second write is the rename over the old file: the bytes
        // move, the mode holds.
        write_stanza_file(&dest, "us").unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(parse_stanza(&std::fs::read_to_string(&dest).unwrap()).as_deref(), Some("us"));
    }

    #[test]
    fn an_image_without_the_include_says_what_did_not_move() {
        let old_image = Layers {
            chosen: Some("de".into()),
            vconsole: Some("de".into()),
            baked: Some("de".into()),
            wired: false,
        };
        let drifts = old_image.drifts();
        assert_eq!(drifts.len(), 1, "{drifts:?}");
        assert!(drifts[0].detail.contains("predates the keymap include"), "{}", drifts[0].detail);
    }
}
