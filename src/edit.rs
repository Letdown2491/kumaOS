//! Day-2 edits: `kuma add` and `kuma remove` rewrite kuma.toml in place.
//! The file is the interface — comments and layout are the owner's, so the
//! edits go through toml_edit (format-preserving) rather than a parse and
//! re-serialize that would flatten the whole document.

use crate::config::Config;
use crate::overrides::Proposal;
use crate::state::{print_actions, Action};
use anyhow::{bail, Context, Result};
use std::path::Path;
use toml_edit::{value, Array, DocumentMut, Item, Table, Value};

const LISTS: &[&str] = &["rpm", "flatpak", "brew"];

/// `kuma edit`: hand the resolved declaration to the person's own editor.
///
/// kuma does not write here and does not read the result. What it
/// contributes is the path, which is the part that is easy to get wrong:
/// a `./kuma.toml` in the current directory outranks
/// `~/.config/kuma/kuma.toml`, so "I edited kuma.toml and nothing
/// changed" has two files behind it and no error message.
///
/// The editor replaces this process rather than being waited on. A
/// desktop entry runs this in a terminal window through `kuma-launch`,
/// and an editor that is a child of a child gets its signals and its
/// terminal resizes through two hops that neither of them handle.
pub fn open(path: &Path, print: bool) -> Result<()> {
    if print {
        println!("{}", path.display());
        return Ok(());
    }
    if !path.exists() {
        // The baked declaration is deliberately not opened instead. It
        // lives in /usr, which is read-only on a booted kuma machine, so
        // editing it is a session of typing that cannot be saved. Named
        // in the message because a person looking at a machine that
        // plainly has a declaration deserves to know why they are being
        // asked to make one.
        if Path::new(crate::state::BAKED_CONFIG).exists() {
            bail!(
                "no declaration at {}; this machine was built from {}, and \
                 `kuma init` writes you a copy to edit",
                path.display(),
                crate::state::BAKED_CONFIG
            );
        }
        bail!("no declaration at {}; `kuma init` writes one", path.display());
    }
    let editor = editor();
    let error = exec_editor(&editor, path);
    // exec only returns on failure.
    Err(error).with_context(|| format!("could not run {editor}"))
}

/// $EDITOR is the person's answer and outranks any default. The
/// fallbacks are ordered by what a kuma image actually has: the base
/// composes ncurses and Fedora's minimal core carries vi, while nano is
/// only ever present because somebody declared it.
fn editor() -> String {
    std::env::var("EDITOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| ["nano", "vim", "vi"].iter().find(|e| on_path(e)).map(|e| (*e).to_string()))
        .unwrap_or_else(|| "vi".to_string())
}

/// Through `sh -c`, because `$EDITOR` is a command line and not a
/// program name. `EDITOR="code -w"`, `EDITOR="emacsclient -c -a ''"` and
/// `EDITOR="nvim -u NONE"` are all ordinary values, and exec'ing one as
/// a filename is ENOENT on a program nobody has. git, crontab and
/// `$VISUAL` all resolve it this way.
///
/// The path arrives as `"$@"` rather than pasted into the string: a home
/// directory can hold a space or a quote, and a command assembled by
/// concatenation is how that becomes two arguments.
fn exec_editor(editor: &str, path: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(editor)
        .arg(path)
        .exec()
}

/// A program somebody can actually run. `is_file` alone would pick an
/// editor that is on PATH and not executable, which is a fallback that
/// resolves and then fails.
fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(program))
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

pub fn add(path: &Path, list: &str, names: &[String], json: bool) -> Result<()> {
    if list == "flatpak" {
        refuse_unknown_flatpaks(names)?;
    }
    let mut doc = load(path)?;
    let arr = list_array_mut(&mut doc, list)?;
    let mut added: Vec<&str> = Vec::new();
    let mut already: Vec<&str> = Vec::new();
    for name in names {
        if contains(arr, name) {
            already.push(name);
            continue;
        }
        push_matching_style(arr, name);
        added.push(name);
    }
    if !added.is_empty() {
        store(path, &doc)?;
    }
    let (actions, converge_note) =
        if added.is_empty() { (Vec::new(), None) } else { apply_edges(list == "rpm") };
    if json {
        crate::response::Response::new()
            .field("list", list)
            .field("declared", added)
            .field("already_declared", already)
            .field("note", converge_note)
            .actions(&actions)
            .print(true, "");
        return Ok(());
    }
    for name in &already {
        println!("{name} is already declared in [packages].{list}");
    }
    if added.is_empty() {
        return Ok(());
    }
    println!("Declared in [packages].{list}: {}", added.join(", "));
    print_apply_hint(&actions, converge_note);
    Ok(())
}

/// Declare items across several lists in one edit, for callers that
/// already know which list each belongs in (`kuma capture`). One document
/// write, so `store`'s validation is all-or-nothing across every list at
/// once; already-declared items are skipped rather than duplicated, and
/// the count of what actually landed comes back.
pub(crate) fn declare(path: &Path, items: &[(&str, &str)]) -> Result<()> {
    let mut doc = load(path)?;
    for (list, name) in items {
        let arr = list_array_mut(&mut doc, list)?;
        if !contains(arr, name) {
            push_matching_style(arr, name);
        }
    }
    store(path, &doc)
}

/// Write captured permissions into `[overrides]`.
///
/// A separate path from `declare` because an override is a table, not an
/// entry in a list, and because the two are proposed together and must
/// land together: one document, one validation, one write.
pub(crate) fn declare_overrides(path: &Path, proposals: &[Proposal]) -> Result<()> {
    let mut doc = load(path)?;
    let overrides = doc.entry("overrides").or_insert(Item::Table(Table::new()));
    let table = overrides.as_table_mut().context("[overrides] is not a table")?;
    // Implicit so the file gets `[overrides."org.example.App"]` and no
    // bare `[overrides]` header above it, which is how a person would
    // have written it by hand.
    table.set_implicit(true);
    for proposal in proposals {
        let entry = table.entry(&proposal.app).or_insert(Item::Table(Table::new()));
        let app = entry
            .as_table_mut()
            .with_context(|| format!("overrides.{} is not a table", proposal.app))?;
        if proposal.scope != crate::config::Scope::System {
            app["scope"] = value(proposal.scope.as_str());
        }
        for key in &proposal.keys {
            let (group, name, values) = crate::overrides::as_declaration(key);
            let section = match group.as_str() {
                crate::overrides::CONTEXT => {
                    let mut arr = Array::new();
                    for v in values {
                        arr.push(v);
                    }
                    app[&name] = value(arr);
                    continue;
                }
                crate::overrides::ENVIRONMENT => "environment",
                crate::overrides::SESSION_BUS => "session-bus",
                _ => "system-bus",
            };
            let sub = app.entry(section).or_insert(Item::Table(Table::new()));
            let sub = sub
                .as_table_mut()
                .with_context(|| format!("overrides.{}.{section} is not a table", proposal.app))?;
            sub[&name] = value(values.first().cloned().unwrap_or_default());
        }
    }
    store(path, &doc)
}

/// All-or-nothing: every name must be found somewhere in [packages] before
/// anything is written, so a typo in one name can't half-apply the rest.
pub fn remove(path: &Path, names: &[String], json: bool) -> Result<()> {
    let mut doc = load(path)?;
    let mut removed: Vec<(&str, &str)> = Vec::new();
    for name in names {
        let list = LISTS
            .iter()
            .find(|list| list_array(&doc, list).is_some_and(|arr| contains(arr, name)))
            .with_context(|| format!("{name} is not declared in any [packages] list"))?;
        let arr = list_array_mut(&mut doc, list)?;
        let idx = position(arr, name).expect("just found it");
        arr.remove(idx);
        removed.push((name, list));
    }
    store(path, &doc)?;
    let (actions, converge_note) = apply_edges(removed.iter().any(|(_, list)| *list == "rpm"));
    if json {
        crate::response::Response::new()
            .field(
                "removed",
                removed
                    .iter()
                    .map(|(name, list)| serde_json::json!({ "item": name, "list": list }))
                    .collect::<Vec<_>>(),
            )
            .field("note", converge_note)
            .actions(&actions)
            .print(true, "");
        return Ok(());
    }
    for (name, list) in &removed {
        println!("Removed {name} from [packages].{list}");
    }
    print_apply_hint(&actions, converge_note);
    Ok(())
}

fn load(path: &Path) -> Result<DocumentMut> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "cannot read {}; run `kuma init` to start one here, or point --config at yours",
            path.display()
        )
    })?;
    text.parse().with_context(|| format!("invalid config in {}", path.display()))
}

/// The edited document must still be a valid declaration — the same rules
/// `kuma build` enforces — before it replaces the file on disk.
fn store(path: &Path, doc: &DocumentMut) -> Result<()> {
    let text = doc.to_string();
    let config: Config =
        toml::from_str(&text).context("refusing to write: the edit breaks the config")?;
    config.validate().context("refusing to write: the edit breaks the config")?;
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

fn list_array<'a>(doc: &'a DocumentMut, list: &str) -> Option<&'a Array> {
    doc.get("packages")?.get(list)?.as_array()
}

fn list_array_mut<'a>(doc: &'a mut DocumentMut, list: &str) -> Result<&'a mut Array> {
    let packages = doc.entry("packages").or_insert(Item::Table(Table::new()));
    let table = packages.as_table_mut().context("[packages] is not a table")?;
    table
        .entry(list)
        .or_insert(Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .with_context(|| format!("packages.{list} is not an array"))
}

fn contains(arr: &Array, name: &str) -> bool {
    position(arr, name).is_some()
}

fn position(arr: &Array, name: &str) -> Option<usize> {
    arr.iter().position(|v| v.as_str() == Some(name))
}

/// Appending to a multi-line array with default decor would glue the new
/// entry onto the last line; give it its own indented line to match.
fn push_matching_style(arr: &mut Array, name: &str) {
    let multiline = arr
        .iter()
        .any(|v| v.decor().prefix().and_then(|p| p.as_str()).is_some_and(|p| p.contains('\n')));
    arr.push(name);
    if multiline {
        let last = arr.len() - 1;
        if let Some(v) = arr.get_mut(last) {
            v.decor_mut().set_prefix("\n    ");
        }
    }
}

/// Flathub's app list as this machine already has it.
///
/// `--cached` is the whole design: it reads appstream data flatpak
/// already downloaded and never touches the network, so declaring a
/// package cannot hang on a captive portal or fail on a plane. When
/// there is no cache to read (flatpak absent, remote never added, a
/// machine that is not this one) it returns None, and None means the
/// check does not run rather than that the name is wrong.
fn flathub_apps() -> Option<Vec<String>> {
    let out = crate::host::host_output(&[
        "flatpak",
        "remote-ls",
        "--system",
        "--cached",
        "--app",
        "--columns=application",
        "flathub",
    ])
    .ok()?;
    let apps: Vec<String> =
        out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    (!apps.is_empty()).then_some(apps)
}

/// A Flathub id nothing knows is worse than a no-op. The converger
/// installs the whole declared list in one `flatpak install`, so one
/// name that does not resolve fails the unit, and the apps that would
/// have installed do not, on this boot and every boot after it. That is
/// a typo taking down convergence for everything else, which is why
/// `[services]` has checked unit names at build time since it existed
/// and this list checked nothing at all.
fn refuse_unknown_flatpaks(names: &[String]) -> Result<()> {
    // None is "could not ask", never "nothing is known": an empty answer
    // would refuse every name on a machine with no cache.
    let Some(known) = flathub_apps() else { return Ok(()) };
    refuse_names_not_in(names, &known)
}

fn refuse_names_not_in(names: &[String], known: &[String]) -> Result<()> {
    for name in names {
        if known.iter().any(|app| app == name) {
            continue;
        }
        bail!(
            "{name} is not an app Flathub lists.\n\
             A name Flathub does not know fails the converger on every boot, and takes \
             the apps beside it down with it, so it is refused here rather than at 3am.\n\
             If it is new, this machine's cached list may be behind: \
             `flatpak update --appstream` refreshes it."
        );
    }
    Ok(())
}

/// Every list lives in the image (flatpak and brew declarations are baked
/// at /usr/lib/kuma), so the apply path is always a rebuild; the flatpak
/// and brew installs then converge on the machine after the switch. Where
/// the build goes next depends on the machine — same edges as build()'s.
pub(crate) fn apply_edges(rpm: bool) -> (Vec<Action>, Option<&'static str>) {
    let mut actions = vec![Action::new("build", "kuma build", "bake the edit into a new image")];
    if Path::new("/run/ostree-booted").exists() {
        actions.push(Action::new(
            "switch",
            "kuma switch",
            "stage it onto this machine (applies on reboot)",
        ));
    } else {
        actions.push(Action::new("vm", "kuma vm", "boot the result in a QEMU VM"));
    }
    // Not "converges at boot and daily", which read as an alternative to
    // the two edges above it and sent people to `kuma sync` instead: the
    // convergers read what the image baked, so an edit that has not been
    // built cannot reach them however often they run.
    let converge_note = (!rpm).then_some(CONVERGE_NOTE);
    (actions, converge_note)
}

/// What an edit to a converged list actually does, said once so `add`,
/// `remove` and `capture` cannot drift apart about it.
pub(crate) const CONVERGE_NOTE: &str = "this edit takes effect when a build of it boots; \
     `kuma sync` before then converges to the image's declaration, not this one";

pub(crate) fn print_converge_note() {
    println!(
        "\nFlatpak, brew and permission changes take effect when a build of this edit boots. \
         Running `kuma sync` before then converges to the image's declaration, not this one."
    );
}

fn print_apply_hint(actions: &[Action], converge_note: Option<&str>) {
    println!();
    print_actions(actions);
    if converge_note.is_some() {
        print_converge_note();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn write(dir: &Path, text: &str) -> std::path::PathBuf {
        let path = dir.join("kuma.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    const CONFIG: &str = "# my system\nschema_version = 1\n\n[packages]\n# tools\nrpm = [\"fish\"]\nflatpak = [\n    \"org.gnome.Loupe\",\n    \"org.gnome.Papers\",\n]\n";

    #[test]
    fn add_preserves_comments_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        super::add(&path, "rpm", &["htop".into()], false).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# my system"));
        assert!(out.contains("# tools"));
        assert!(out.contains("rpm = [\"fish\", \"htop\"]"));
    }

    /// A typo in a Flathub id is not a no-op: the converger installs the
    /// whole declared list in one command, so one name that does not
    /// resolve fails the unit and the apps beside it never install, on
    /// this boot and every boot after. Refusing it at the keyboard is
    /// the same bargain `[services]` has always had with unit names.
    #[test]
    fn a_flathub_id_nothing_knows_is_refused() {
        let known = vec!["org.mozilla.firefox".to_string(), "org.gnome.Loupe".to_string()];
        assert!(super::refuse_names_not_in(&["org.mozilla.firefox".into()], &known).is_ok());
        let err = super::refuse_names_not_in(&["org.mozilla.firefixx".into()], &known).unwrap_err();
        let said = err.to_string();
        assert!(said.contains("org.mozilla.firefixx"), "{said}");
        // and it names the way out, because a cache can be behind a
        // genuinely new app and a refusal with no next move is a wall
        assert!(said.contains("flatpak update --appstream"), "{said}");
    }

    #[test]
    fn add_matches_multiline_style() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        super::add(&path, "flatpak", &["org.gnome.Calculator".into()], false).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("\n    \"org.gnome.Calculator\""));
    }

    #[test]
    fn add_creates_missing_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "schema_version = 1\n");
        super::add(&path, "brew", &["ripgrep".into()], false).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("[packages]"));
        assert!(out.contains("brew = [\"ripgrep\"]"));
    }

    #[test]
    fn add_rejects_names_the_build_would_reject() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        assert!(super::add(&path, "rpm", &["fish; rm -rf /".into()], false).is_err());
        // and the file is untouched
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    }

    /// Capture spans lists in one go (a flatpak and a brew in the same
    /// proposal), and that has to be one document write: two would let
    /// the second fail validation after the first already landed.
    #[test]
    fn declare_writes_several_lists_in_one_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        super::declare(&path, &[("flatpak", "org.gnome.Boxes"), ("brew", "ripgrep")]).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("\n    \"org.gnome.Boxes\""), "multi-line style kept");
        assert!(out.contains("brew = [\"ripgrep\"]"), "missing list created");
        assert!(out.contains("# my system"));
    }

    /// Captured permissions have to land as a table a person would have
    /// written by hand, because the next thing that happens to this file
    /// is somebody reading it. No bare `[overrides]` header, `scope`
    /// only when it is not the default, and the sections nested under
    /// the app rather than flattened into it.
    #[test]
    fn captured_permissions_land_as_a_person_would_have_written_them() {
        use crate::config::{Config, Scope};
        use crate::overrides::Proposal;
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        let proposals = vec![
            Proposal {
                app: "org.mozilla.firefox".into(),
                scope: Scope::System,
                keys: vec![
                    ("Context".into(), "filesystems".into(), "home;!xdg-config/kitty;".into()),
                    ("Environment".into(), "MOZ_ENABLE_WAYLAND".into(), "1".into()),
                ],
            },
            Proposal {
                app: "org.gnome.Loupe".into(),
                scope: Scope::User,
                keys: vec![(
                    "Session Bus Policy".into(),
                    "org.freedesktop.Flatpak".into(),
                    "talk".into(),
                )],
            },
        ];
        super::declare_overrides(&path, &proposals).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("[overrides.\"org.mozilla.firefox\"]"), "{out}");
        assert!(!out.contains("\n[overrides]\n"), "a bare [overrides] header: {out}");
        assert!(
            out.contains("filesystems = [\"home\", \"!xdg-config/kitty\"]"),
            "a semicolon list must come back as an array: {out}"
        );
        assert!(out.contains("[overrides.\"org.mozilla.firefox\".environment]"), "{out}");
        assert!(out.contains("MOZ_ENABLE_WAYLAND = \"1\""), "{out}");
        // scope is written only where it is not the default
        assert!(out.contains("scope = \"user\""), "{out}");
        assert_eq!(out.matches("scope =").count(), 1, "system scope was spelled out: {out}");
        // and the file it wrote is still a declaration kuma accepts
        let config: Config = toml::from_str(&out).unwrap();
        config.validate().unwrap();
        assert_eq!(config.overrides.len(), 2);
        assert!(out.contains("# my system"), "the owner's comment survived");
    }

    /// Capture reads names off the machine, so the file is the last place
    /// a hostile one could land — it writes through the same validating
    /// store() that `add` does, and an all-or-nothing failure leaves the
    /// declaration byte-identical.
    #[test]
    fn declare_rejects_what_the_build_would_reject() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        let bad = [("flatpak", "org.gnome.Boxes"), ("brew", "--nogpgcheck")];
        assert!(super::declare(&path, &bad).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    }

    /// Capture proposes from what the machine has, which can overlap what
    /// the file already says; the overlap is a no-op, not a duplicate.
    #[test]
    fn declare_skips_what_is_already_declared() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        super::declare(&path, &[("rpm", "fish")]).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert_eq!(out.matches("\"fish\"").count(), 1);
    }

    #[test]
    fn remove_finds_the_right_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        super::remove(&path, &["org.gnome.Papers".into(), "fish".into()], false).unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("org.gnome.Papers"));
        assert!(out.contains("rpm = []"));
        assert!(out.contains("org.gnome.Loupe"));
    }

    #[test]
    fn remove_unknown_name_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), CONFIG);
        assert!(super::remove(&path, &["fish".into(), "nope".into()], false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    }
}
