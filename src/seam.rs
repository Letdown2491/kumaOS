//! kuma's verbs, as desktop entries.
//!
//! kuma assembles a desktop out of other people's programs, and those
//! programs have no home for `kuma doctor` or the declaration. The menu
//! solved that by drawing its own surface, which meant the surface only
//! existed where fuzzel did: niri, and nowhere else.
//!
//! These entries solve it the other way. A `.desktop` file is the one
//! thing every launcher on every desktop already reads, so kuma's verbs
//! appear in whatever launcher the session shipped without kuma knowing
//! which launcher that is. **kuma integrates through freedesktop
//! standards only** — no plugin API, no IPC, nothing that ties the
//! verbs to one shell. A shell kuma cannot drop is a shell kuma has to
//! maintain.
//!
//! Every entry runs through `/usr/libexec/kuma-launch` rather than
//! naming `kuma` directly. See [`crate::containerfile::KUMA_LAUNCH`]
//! for why `Terminal=true` is not usable here.

/// One entry. `argv` is what follows `kuma`, so the verb a row names is
/// the verb the CLI has, checked against clap in the tests below.
pub(crate) struct Entry {
    /// Basename, without `.desktop`. Not reverse-DNS: that convention
    /// belongs to things with an appstream ID and a domain behind it,
    /// and inventing a domain to look official is how a name outlives
    /// the reason for it.
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    /// One line, and deliberately not clap's `about` string verbatim.
    ///
    /// A launcher renders this as a tooltip under a name, where the CLI
    /// renders its own next to a verb, so the parentheticals that earn
    /// their place in `--help` ("prints unless --yes", "read-only" on a
    /// verb whose name does not say so) are noise in one and load-bearing
    /// in the other. Two of these are the `about` line exactly and the
    /// rest are it minus the part a person clicking cannot use. What is
    /// checked by test is that the verb and its flags are real; the
    /// wording is not, because there is no version of it that is right
    /// in both places.
    pub(crate) comment: &'static str,
    /// A symbolic name from Adwaita, which both desktop sets install.
    /// Deliberately not `/usr/share/icons/kuma`: that theme exists only
    /// because the menu asked fuzzel to resolve icons itself, and it
    /// goes when the menu goes.
    pub(crate) icon: &'static str,
    pub(crate) argv: &'static [&'static str],
    /// What somebody would type looking for this. A launcher matches
    /// the name it was handed and nothing else, so "is my system ok"
    /// finds nothing without these.
    pub(crate) keywords: &'static [&'static str],
}

/// What kuma puts in the launcher.
///
/// These are the eight `kuma` rows the menu drew, and nothing else. The
/// menu's other rows were device settings — wifi, bluetooth, audio,
/// brightness — which belong to whatever shell the session runs and
/// were only ever kuma's because nothing else claimed them.
///
/// **Nothing here writes the declaration without asking.** `capture` is
/// the one entry that can end in a write and it does its own asking; the
/// rest read, or change machine state that a person can see change. An
/// entry is one click with no diff in front of it, which is the same
/// reason the menu had this rule.
pub(crate) const ENTRIES: &[Entry] = &[
    Entry {
        id: "kuma-edit",
        name: "Edit Declaration",
        comment: "Open this machine's kuma.toml in your editor",
        icon: "text-editor-symbolic",
        argv: &["edit"],
        keywords: &["kuma", "declaration", "config", "toml", "system"],
    },
    Entry {
        id: "kuma-drift",
        name: "Show Drift",
        comment: "Show drift between kuma.toml and this machine (read-only)",
        icon: "edit-find-symbolic",
        argv: &["diff"],
        keywords: &["kuma", "drift", "diff", "changes", "declaration"],
    },
    Entry {
        id: "kuma-proposals",
        name: "Review Proposals",
        comment: "Declare what this machine already runs but kuma.toml doesn't name",
        icon: "dialog-information-symbolic",
        argv: &["capture"],
        keywords: &["kuma", "capture", "proposals", "declare", "drift"],
    },
    Entry {
        id: "kuma-doctor",
        name: "System Health",
        comment: "Check this machine: deployment, boot health, convergence, GPU, storage, disk",
        icon: "emblem-system-symbolic",
        argv: &["doctor"],
        keywords: &["kuma", "health", "doctor", "check", "diagnose", "status"],
    },
    Entry {
        id: "kuma-update",
        name: "Check for Updates",
        comment: "See what a rebuild would bring in, without building anything",
        icon: "software-update-available-symbolic",
        argv: &["update", "--check"],
        keywords: &["kuma", "update", "upgrade", "packages", "check"],
    },
    Entry {
        id: "kuma-build",
        name: "Rebuild System",
        comment: "Build the system image from kuma.toml",
        icon: "view-refresh-symbolic",
        argv: &["build"],
        keywords: &["kuma", "build", "rebuild", "image", "apply"],
    },
    Entry {
        id: "kuma-rollback",
        name: "Roll Back",
        comment: "Swap the boot order back to the previous deployment",
        icon: "go-previous-symbolic",
        argv: &["rollback"],
        keywords: &["kuma", "rollback", "revert", "undo", "previous", "boot"],
    },
    Entry {
        id: "kuma-snapshots",
        name: "Snapshots",
        comment: "List the snapshots this machine has taken, or restore a path from one",
        icon: "drive-harddisk-symbolic",
        argv: &["snapshot"],
        keywords: &["kuma", "snapshot", "restore", "backup", "btrfs"],
    },
];

/// The file `id` is written to inside the image.
pub(crate) fn path(entry: &Entry) -> String {
    format!("/usr/share/applications/{}.desktop", entry.id)
}

/// One `.desktop` file.
///
/// `Terminal=false` is not a claim that these produce no output: every
/// one of them prints, and several ask for a password. It says the
/// *launcher* must not try to supply the terminal, because
/// `Terminal=true` resolves to whatever that launcher believes a
/// terminal is, which differs per launcher and is nothing at all in
/// several. `kuma-launch` opens the terminal kuma knows it shipped.
///
/// `StartupNotify=false` because none of these map a window under their
/// own name — the window belongs to the terminal — and a launcher that
/// waits for one shows a spinner until it gives up.
///
/// One main category, not two. `System;Settings;` reads better and
/// `desktop-file-validate` hints against it: a menu that files by main
/// category then shows the same entry twice.
pub(crate) fn render(entry: &Entry) -> String {
    let exec = std::iter::once("/usr/libexec/kuma-launch")
        .chain(entry.argv.iter().copied())
        .collect::<Vec<&str>>()
        .join(" ");
    let mut keywords = entry.keywords.join(";");
    keywords.push(';');
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={name}\n\
         Comment={comment}\n\
         Icon={icon}\n\
         Exec={exec}\n\
         Terminal=false\n\
         Categories=System;X-Kuma;\n\
         Keywords={keywords}\n\
         StartupNotify=false\n",
        name = entry.name,
        comment = entry.comment,
        icon = entry.icon,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::collections::BTreeSet;

    /// Every entry names a verb the CLI actually has.
    ///
    /// The verb lives in a string the compiler never reads, so renaming
    /// one leaves an entry that draws in the launcher and does nothing.
    /// That exact bug shipped once already in a keybinding, which is why
    /// clap's own list is the authority here rather than a second list
    /// somebody has to remember to edit.
    #[test]
    fn every_entry_names_a_real_verb() {
        let cli = crate::Cli::command();
        let verbs: BTreeSet<String> =
            cli.get_subcommands().map(|sub| sub.get_name().to_string()).collect();
        for entry in ENTRIES {
            assert!(
                verbs.contains(entry.argv[0]),
                "{} runs `kuma {}`, which is not a verb",
                entry.id,
                entry.argv[0]
            );
        }
    }

    /// Every flag an entry passes is a flag that verb accepts.
    ///
    /// `update --check` is the only one today, and the reason to check
    /// it is that it is exactly the sort of thing that gets renamed to
    /// `--dry-run` by somebody who never opens a launcher.
    #[test]
    fn every_entry_flag_is_accepted() {
        let cli = crate::Cli::command();
        for entry in ENTRIES {
            let flags: BTreeSet<String> = cli
                .get_subcommands()
                .find(|sub| sub.get_name() == entry.argv[0])
                .expect("verb checked by every_entry_names_a_real_verb")
                .get_arguments()
                .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
                .collect();
            for arg in &entry.argv[1..] {
                assert!(
                    flags.contains(*arg),
                    "{} passes `{arg}` to `kuma {}`, which does not take it",
                    entry.id,
                    entry.argv[0]
                );
            }
        }
    }

    /// **An allowlist, because a list of verbs to refuse cannot know
    /// about a verb that does not exist yet.** A launcher entry is one
    /// click with nothing in front of it, so a verb that writes the
    /// declaration without asking must not be reachable from one. Every
    /// verb an entry names is written here with the reason it is safe.
    #[test]
    fn no_entry_writes_the_declaration_unasked() {
        const ALLOWED: &[(&str, &str)] = &[
            ("edit", "opens the file in the person's own editor; they save it themselves"),
            ("diff", "read-only"),
            ("capture", "prints the proposal and asks; --yes is the only writer and is not passed"),
            ("doctor", "read-only"),
            ("update", "--check builds nothing and writes nothing"),
            ("build", "writes an image, never the declaration"),
            ("rollback", "changes the boot order, never the declaration"),
            ("snapshot", "lists; restoring a path is asked for on the command line"),
        ];
        for entry in ENTRIES {
            let verb = entry.argv[0];
            assert!(
                ALLOWED.iter().any(|(name, _)| *name == verb),
                "{} runs `kuma {verb}`, which is not in the seam's allowlist; \
                 add it there with the reason it is safe to reach in one click",
                entry.id
            );
            assert!(
                !entry.argv.contains(&"--yes"),
                "{} passes --yes; every write from a launcher must be confirmed by the verb itself",
                entry.id
            );
        }
    }

    /// Distinct ids, so one entry cannot overwrite another in
    /// `/usr/share/applications`.
    #[test]
    fn ids_are_unique() {
        let ids: BTreeSet<&str> = ENTRIES.iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), ENTRIES.len(), "two entries share an id");
    }

    /// The generated file is a desktop entry, in the shape
    /// `desktop-file-validate` accepts. The build runs the real
    /// validator; this catches the same thing without podman.
    #[test]
    fn render_has_the_required_keys() {
        let rendered = render(&ENTRIES[0]);
        assert!(rendered.starts_with("[Desktop Entry]\n"), "{rendered}");
        for key in ["Type=Application", "Name=", "Exec=", "Icon=", "Terminal=false"] {
            assert!(rendered.contains(key), "missing {key} in {rendered}");
        }
        assert!(rendered.ends_with('\n'), "{rendered}");
    }

    /// Every entry goes through the wrapper. An `Exec` that named `kuma`
    /// directly would draw a window that vanishes with the output in it
    /// on any launcher, and would draw nothing at all on one that
    /// supplies no terminal.
    #[test]
    fn every_exec_goes_through_kuma_launch() {
        for entry in ENTRIES {
            let rendered = render(entry);
            assert!(
                rendered.contains("Exec=/usr/libexec/kuma-launch "),
                "{} does not run through the wrapper",
                entry.id
            );
        }
    }

    /// Field codes are the launcher's, not ours. `%f`, `%U` and friends
    /// mean "substitute the files the user dropped on this", and kuma's
    /// verbs take none: a stray one would hand a filename to a verb that
    /// would then refuse to start.
    #[test]
    fn no_exec_carries_a_field_code() {
        for entry in ENTRIES {
            assert!(!render(entry).contains('%'), "{} carries a field code in its Exec", entry.id);
        }
    }

    /// The keyword list is what makes these findable. An entry nobody
    /// can search for is an entry in a menu of submenus, which is the
    /// shape this replaced.
    #[test]
    fn every_entry_is_searchable_by_kuma() {
        for entry in ENTRIES {
            assert!(
                entry.keywords.contains(&"kuma"),
                "{} cannot be found by typing kuma",
                entry.id
            );
            assert!(entry.keywords.len() >= 3, "{} has too few keywords", entry.id);
        }
    }

    /// Values are single-line. A newline in a name or comment ends the
    /// key and starts a line the parser reads as a malformed key, which
    /// `desktop-file-validate` catches in the build and nothing catches
    /// before it.
    #[test]
    fn no_value_spans_lines() {
        for entry in ENTRIES {
            for value in [entry.name, entry.comment, entry.icon] {
                assert!(!value.contains('\n'), "{} has a multi-line value", entry.id);
            }
        }
    }

    #[test]
    fn path_lands_in_the_shared_applications_dir() {
        assert_eq!(path(&ENTRIES[0]), "/usr/share/applications/kuma-edit.desktop");
    }
}
