//! Flatpak permission overrides, converged one key at a time.
//!
//! An override file is a keyfile, and kuma is never its only author:
//! Flatseal writes the same files, and so does anyone who runs `flatpak
//! override` by hand. So the unit of ownership here is the **key**, not
//! the file and not the app. kuma sets the keys a declaration names,
//! removes the keys it set that the declaration stopped naming, and
//! copies every other line through untouched. That is `73771ab`'s rule
//! one level down: convergence takes back only what it gave.

use crate::config::{AppOverride, Overrides, Scope};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const CONTEXT: &str = "Context";
pub const ENVIRONMENT: &str = "Environment";
pub const SESSION_BUS: &str = "Session Bus Policy";
pub const SYSTEM_BUS: &str = "System Bus Policy";

/// One key kuma has authority over, as `group\tkey`. The state file
/// stores these verbatim, one per line, so a group name with a space in
/// it ("Session Bus Policy") needs no quoting and no parser.
pub fn owned_id(group: &str, key: &str) -> String {
    format!("{group}\t{key}")
}

/// Every key a declaration asks for, in the order the file writes them:
/// group, key, value.
pub fn declared_keys(app: &AppOverride) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (key, values) in app.context_lists() {
        if values.is_empty() {
            continue;
        }
        // flatpak writes these as semicolon-terminated lists, trailing
        // separator included; matching it exactly is what lets a
        // declared file compare equal to one flatpak wrote itself.
        let mut value = String::new();
        for v in values {
            value.push_str(v);
            value.push(';');
        }
        out.push((CONTEXT.to_string(), key.to_string(), value));
    }
    for (name, value) in &app.environment {
        out.push((ENVIRONMENT.to_string(), name.clone(), value.clone()));
    }
    for (bus, policy) in &app.session_bus {
        out.push((SESSION_BUS.to_string(), bus.clone(), policy.as_str().to_string()));
    }
    for (bus, policy) in &app.system_bus {
        out.push((SYSTEM_BUS.to_string(), bus.clone(), policy.as_str().to_string()));
    }
    out
}

/// The declared keys as a standalone keyfile. This is what the image
/// bakes: kuma's half of the answer, with nothing of the machine's in
/// it.
pub fn render(app: &AppOverride) -> String {
    let mut out = String::new();
    let mut current = "";
    for (group, key, value) in declared_keys(app) {
        if group != current {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[{group}]\n"));
            current = match group.as_str() {
                CONTEXT => CONTEXT,
                ENVIRONMENT => ENVIRONMENT,
                SESSION_BUS => SESSION_BUS,
                SYSTEM_BUS => SYSTEM_BUS,
                _ => unreachable!("declared_keys() only emits kuma's four groups"),
            };
        }
        out.push_str(&format!("{key}={value}\n"));
    }
    out
}

/// A parsed keyfile that remembers everything it did not understand.
/// Comments, blank lines, and groups kuma has never heard of all survive
/// a round trip, because the file belongs to the machine and kuma is
/// only editing part of it.
#[derive(Debug, Default)]
struct KeyFile {
    groups: Vec<Group>,
}

#[derive(Debug)]
struct Group {
    name: String,
    lines: Vec<Line>,
}

#[derive(Debug)]
enum Line {
    Pair(String, String),
    Raw(String),
}

impl KeyFile {
    fn parse(text: &str) -> Self {
        let mut file = KeyFile::default();
        // Anything before the first header (comments, usually) lives in
        // a nameless group so it keeps its place at the top.
        file.groups.push(Group { name: String::new(), lines: Vec::new() });
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(name) = trimmed.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                file.groups.push(Group { name: name.to_string(), lines: Vec::new() });
                continue;
            }
            let group = file.groups.last_mut().expect("a group always exists");
            match trimmed.split_once('=') {
                Some((key, value)) if !trimmed.starts_with('#') && !key.trim().is_empty() => {
                    group.lines.push(Line::Pair(key.trim().to_string(), value.to_string()));
                }
                _ => group.lines.push(Line::Raw(line.to_string())),
            }
        }
        file
    }

    fn set(&mut self, group: &str, key: &str, value: &str) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.name == group) {
            for line in g.lines.iter_mut() {
                if let Line::Pair(k, v) = line {
                    if k == key {
                        *v = value.to_string();
                        return;
                    }
                }
            }
            g.lines.push(Line::Pair(key.to_string(), value.to_string()));
            return;
        }
        self.groups.push(Group {
            name: group.to_string(),
            lines: vec![Line::Pair(key.to_string(), value.to_string())],
        });
    }

    fn remove(&mut self, group: &str, key: &str) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.name == group) {
            g.lines.retain(|l| !matches!(l, Line::Pair(k, _) if k == key));
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for group in &self.groups {
            // A group kuma emptied goes with its header. A group holding
            // anything else does not, and a comment counts: somebody
            // wrote it to explain their machine to themselves, and
            // taking back the key beside it is no reason to delete it.
            let keeps_something = group.lines.iter().any(|line| match line {
                Line::Pair(_, _) => true,
                Line::Raw(raw) => !raw.trim().is_empty(),
            });
            if !group.name.is_empty() {
                if !keeps_something {
                    continue;
                }
                out.push_str(&format!("[{}]\n", group.name));
            }
            for line in &group.lines {
                match line {
                    Line::Pair(k, v) => out.push_str(&format!("{k}={v}\n")),
                    Line::Raw(raw) => out.push_str(&format!("{raw}\n")),
                }
            }
        }
        out
    }
}

/// What one app's convergence did, so the caller can say it out loud
/// rather than reporting that something happened.
#[derive(Debug, Default, PartialEq)]
pub struct Changed {
    pub set: Vec<String>,
    pub removed: Vec<String>,
}

impl Changed {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.removed.is_empty()
    }
}

/// Merge a declaration into whatever the machine has.
///
/// `previously` is what kuma set last time, as `owned_id` strings. A key
/// in `previously` that the declaration no longer names is kuma's to
/// remove; a key in neither was never kuma's and is left exactly where
/// it is, even when it sits in the same group and contradicts what was
/// just declared. That is not a bug: declaring is how you win that
/// argument, and `kuma diff` is how you see you are having it.
pub fn converge(
    live: &str,
    wanted: &[(String, String, String)],
    previously: &[String],
) -> (String, Changed, Vec<String>) {
    let mut file = KeyFile::parse(live);
    let mut changed = Changed::default();
    let mut now_owned = Vec::new();

    for (group, key, value) in wanted {
        let id = owned_id(group, key);
        let existing = file.groups.iter().find(|g| &g.name == group).and_then(|g| {
            g.lines.iter().find_map(|l| match l {
                Line::Pair(k, v) if k == key => Some(v.clone()),
                _ => None,
            })
        });
        if existing.as_deref() != Some(value.as_str()) {
            changed.set.push(id.clone());
        }
        file.set(group, key, value);
        now_owned.push(id);
    }
    for id in previously {
        if now_owned.contains(id) {
            continue;
        }
        let Some((group, key)) = id.split_once('\t') else { continue };
        let present = file.groups.iter().any(|g| {
            g.name == group && g.lines.iter().any(|l| matches!(l, Line::Pair(k, _) if k == key))
        });
        if present {
            file.remove(group, key);
            changed.removed.push(id.clone());
        }
    }

    // Emptiness is decided by what comes out, not by counting keys: a
    // file whose last key kuma took back is litter and goes, and one
    // that still holds a comment is somebody's and stays.
    let rendered = file.render();
    let rendered = if rendered.trim().is_empty() { String::new() } else { rendered };
    (rendered, changed, now_owned)
}

/// Where a scope's override files live. The system store is flatpak's
/// own; the user store is under the caller's data directory, which is
/// why the user pass runs as the user rather than root reaching into a
/// home.
///
/// Every path here takes a root, so the whole pass can be run against a
/// directory tree in a test rather than asserted about in prose.
pub fn store(scope: Scope, root: &Path, home: &Path) -> PathBuf {
    match scope {
        Scope::System => root.join("var/lib/flatpak/overrides"),
        Scope::User => home.join(".local/share/flatpak/overrides"),
    }
}

/// What the image baked: one keyfile per app, holding kuma's keys and
/// nothing else.
pub fn declared_dir(scope: Scope, root: &Path) -> PathBuf {
    root.join("usr/lib/kuma/overrides").join(scope.as_str())
}

/// What kuma set last time. Machine state, so `/var` for the system
/// pass; the user pass keeps its own beside the user's other state,
/// because root writing into a home is how you race a running Flatseal.
pub fn state_path(scope: Scope, root: &Path, home: &Path) -> PathBuf {
    match scope {
        Scope::System => root.join("var/lib/kuma/overrides-owned"),
        Scope::User => home.join(".local/state/kuma/overrides-owned"),
    }
}

/// Read a baked file back into the keys it asks for. The baked file is
/// the declaration at runtime, so this is the only reader that matters
/// on a machine; `declared_keys()` exists for the build that wrote it.
pub fn parse_declared(text: &str) -> Vec<(String, String, String)> {
    let file = KeyFile::parse(text);
    let mut out = Vec::new();
    for group in &file.groups {
        if group.name.is_empty() {
            continue;
        }
        for line in &group.lines {
            if let Line::Pair(k, v) = line {
                out.push((group.name.clone(), k.clone(), v.clone()));
            }
        }
    }
    out
}

fn read_state(path: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(app), Some(group), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        out.entry(app.to_string()).or_default().push(owned_id(group, key));
    }
    out
}

fn write_state(path: &Path, owned: &BTreeMap<String, Vec<String>>) -> Result<()> {
    let mut text = String::new();
    for (app, ids) in owned {
        for id in ids {
            text.push_str(&format!("{app}\t{id}\n"));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Converge one scope's whole store, and report per app.
///
/// The app list is the union of what the image declares and what kuma
/// set last time, because an app that left the declaration entirely is
/// exactly the one with keys to take back, and reading only the declared
/// directory would never look at it.
pub fn converge_store(scope: Scope, root: &Path, home: &Path) -> Result<Vec<(String, Changed)>> {
    let declared = declared_dir(scope, root);
    let store = store(scope, root, home);
    let state = state_path(scope, root, home);
    let previous = read_state(&state);

    let mut apps: BTreeSet<String> = previous.keys().cloned().collect();
    if let Ok(entries) = std::fs::read_dir(&declared) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                apps.insert(name.to_string());
            }
        }
    }

    let mut owned: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut report = Vec::new();
    for app in apps {
        let wanted = std::fs::read_to_string(declared.join(&app))
            .map(|t| parse_declared(&t))
            .unwrap_or_default();
        let path = store.join(&app);
        let live = std::fs::read_to_string(&path).unwrap_or_default();
        let empty = Vec::new();
        let (out, changed, now) = converge(&live, &wanted, previous.get(&app).unwrap_or(&empty));
        if out.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        } else if out != live {
            std::fs::create_dir_all(&store)
                .with_context(|| format!("creating {}", store.display()))?;
            std::fs::write(&path, &out).with_context(|| format!("writing {}", path.display()))?;
        }
        if !now.is_empty() {
            owned.insert(app.clone(), now);
        }
        if !changed.is_empty() {
            report.push((app, changed));
        }
    }
    // Only when it moved. This runs on every boot of every machine, and
    // a converger that rewrites its own state to say the same thing is a
    // write, an mtime, and a lie about when anything last happened.
    if owned != previous {
        write_state(&state, &owned)?;
    }
    Ok(report)
}

/// One key that does not match the declaration, in whichever direction.
#[derive(Debug, PartialEq)]
pub struct Drift {
    pub app: String,
    pub group: String,
    pub key: String,
    /// "add" when the machine is missing what the declaration says,
    /// "remove" when kuma set a key the declaration stopped naming.
    pub change: &'static str,
}

/// How a key reads to a person. `[Context]` is where most keys live and
/// naming it every time would bury what the line is about, so only the
/// other groups say which one they are.
///
/// One function because two surfaces print these: `kuma diff` reports
/// drift, `kuma capture` proposes, and a key that reads one way in one
/// and another way in the other is two vocabularies for one thing.
pub fn key_label(group: &str, key: &str) -> String {
    if group == CONTEXT {
        key.to_string()
    } else {
        format!("{group}/{key}")
    }
}

impl Drift {
    /// How it reads in a diff: the app, then the key.
    pub fn item(&self) -> String {
        format!("{} {}", self.app, key_label(&self.group, &self.key))
    }
}

fn live_keys(path: &Path) -> BTreeMap<(String, String), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    parse_declared(&text).into_iter().map(|(g, k, v)| ((g, k), v)).collect()
}

/// What `kuma diff` reports: the declaration against the two stores.
///
/// Read-only, like every other observer here. It answers the question
/// the machine cannot answer for itself, which is why a permission that
/// somebody toggled in Flatseal an hour ago shows up as a proposal
/// rather than as a surprise at the next boot.
pub fn drift(declared: &Overrides, root: &Path, home: &Path) -> Vec<Drift> {
    let mut out = Vec::new();
    for scope in [Scope::System, Scope::User] {
        let store = store(scope, root, home);
        let previous = read_state(&state_path(scope, root, home));

        let mut still_declared: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (app, over) in declared.iter().filter(|(_, o)| o.scope == scope) {
            let live = live_keys(&store.join(app));
            let mut ids = Vec::new();
            for (group, key, value) in declared_keys(over) {
                ids.push(owned_id(&group, &key));
                if live.get(&(group.clone(), key.clone())) != Some(&value) {
                    out.push(Drift { app: app.clone(), group, key, change: "add" });
                }
            }
            still_declared.insert(app.as_str(), ids);
        }

        for (app, ids) in &previous {
            let live = live_keys(&store.join(app));
            let empty = Vec::new();
            let kept = still_declared.get(app.as_str()).unwrap_or(&empty);
            for id in ids {
                if kept.contains(id) {
                    continue;
                }
                let Some((group, key)) = id.split_once('\t') else { continue };
                if live.contains_key(&(group.to_string(), key.to_string())) {
                    out.push(Drift {
                        app: app.clone(),
                        group: group.to_string(),
                        key: key.to_string(),
                        change: "remove",
                    });
                }
            }
        }
    }
    out
}

/// Whether the image's baked overrides are behind the declaration.
///
/// The same trap `/usr/lib/kuma/flatpaks` has: the converger reads what
/// the image baked, so an edit to the declaration reaches nothing until
/// a rebuild, and the honest thing is to say so rather than let `sync`
/// report success over a file it never read.
pub fn image_stale(declared: &Overrides, root: &Path) -> bool {
    // An image with no overrides directory at all is either not a kuma
    // image, in which case this question is not being asked of it, or it
    // predates the feature. The second one is stale exactly when the
    // declaration names a permission: nothing on that machine will ever
    // apply it, however many times a converger runs.
    let kuma_image = root.join("usr/lib/kuma").is_dir();
    for scope in [Scope::System, Scope::User] {
        let dir = declared_dir(scope, root);
        if !dir.is_dir() {
            if kuma_image && declared.values().any(|o| o.scope == scope) {
                return true;
            }
            continue;
        }
        let mut baked: BTreeSet<String> = BTreeSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    baked.insert(name.to_string());
                }
            }
        }
        for (app, over) in declared.iter().filter(|(_, o)| o.scope == scope) {
            baked.remove(app.as_str());
            if std::fs::read_to_string(dir.join(app)).unwrap_or_default() != render(over) {
                return true;
            }
        }
        // baked for an app the declaration no longer names
        if !baked.is_empty() {
            return true;
        }
    }
    false
}

/// An override this machine has and the declaration does not name.
#[derive(Debug, PartialEq)]
pub struct Proposal {
    pub app: String,
    pub scope: Scope,
    pub keys: Vec<(String, String, String)>,
}

/// Whether a key can be written back into a declaration at all. A group
/// flatpak grows later, or a `[Context]` key kuma has no field for, is
/// still copied through by convergence; it just cannot be proposed,
/// because there is nowhere in the schema to put it.
fn representable(group: &str, key: &str) -> bool {
    let spelled_with = |extra: &[char]| {
        key.chars().any(|c| c.is_ascii_alphanumeric())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
    };
    match group {
        CONTEXT => AppOverride::default().context_lists().iter().any(|(name, _)| *name == key),
        // The parser's own alphabets, asked here rather than discovered
        // afterwards: a key kuma cannot spell in a declaration must not
        // be proposed for one, or capture offers an edit the validating
        // write then refuses, taking every other proposal down with it.
        // Convergence still copies such a key through untouched.
        ENVIRONMENT => spelled_with(&['_']),
        SESSION_BUS | SYSTEM_BUS => spelled_with(&['.', '-', '_', '*']),
        _ => false,
    }
}

/// What `kuma capture` offers: permissions this machine carries that the
/// declaration does not name.
///
/// Scoped to apps the declaration already installs, which is a
/// deliberate call and the sweep's evidence agrees: a machine accumulates override
/// files for apps that left years ago, and the user store on the machine
/// this was written on holds five, three of them for software that is
/// not installed. Proposing those would be proposing rubble. It also
/// falls out for free that flatpak's `global` override file, which is
/// not an app at all, is never mistaken for one.
///
/// An app whose two stores both hold undeclared keys is returned as
/// ambiguous rather than guessed at: one app declares into one store,
/// and picking for somebody is how a proposal stops being trustworthy.
pub fn capturable(
    installed_apps: &BTreeSet<&str>,
    declared: &Overrides,
    root: &Path,
    home: &Path,
) -> (Vec<Proposal>, Vec<String>) {
    let mut proposals = Vec::new();
    let mut ambiguous = Vec::new();
    // One read per scope, not one per app: the state file is small, but
    // re-reading it inside the loop made the cost of asking scale with
    // the declaration for no reason.
    let state: Vec<BTreeMap<String, Vec<String>>> = [Scope::System, Scope::User]
        .iter()
        .map(|scope| read_state(&state_path(*scope, root, home)))
        .collect();
    for app in installed_apps {
        let mut per_scope: Vec<Proposal> = Vec::new();
        for (at, scope) in [Scope::System, Scope::User].into_iter().enumerate() {
            if declared.get(*app).is_some_and(|o| o.scope != scope) {
                continue;
            }
            let owned: &[String] = state[at].get(*app).map(Vec::as_slice).unwrap_or_default();
            let already: Vec<String> = declared
                .get(*app)
                .map(|o| declared_keys(o).iter().map(|(g, k, _)| owned_id(g, k)).collect())
                .unwrap_or_default();
            let mut keys: Vec<(String, String, String)> =
                live_keys(&store(scope, root, home).join(app))
                    .into_iter()
                    .filter(|((g, k), _)| {
                        let id = owned_id(g, k);
                        representable(g, k) && !owned.contains(&id) && !already.contains(&id)
                    })
                    .map(|((g, k), v)| (g, k, v))
                    .collect();
            keys.sort();
            if !keys.is_empty() {
                per_scope.push(Proposal { app: app.to_string(), scope, keys });
            }
        }
        match per_scope.len() {
            0 => {}
            1 => proposals.push(per_scope.remove(0)),
            _ => ambiguous.push(app.to_string()),
        }
    }
    (proposals, ambiguous)
}

/// A proposal as the declaration spells it: the value flatpak writes,
/// turned back into what the schema takes. `[Context]` keys are
/// semicolon-terminated lists there and arrays here; everything else is
/// a single value.
pub fn as_declaration(key: &(String, String, String)) -> (String, String, Vec<String>) {
    let (group, name, value) = key;
    if group == CONTEXT {
        let items = value.split(';').filter(|p| !p.is_empty()).map(str::to_string).collect();
        (group.clone(), name.clone(), items)
    } else {
        (group.clone(), name.clone(), vec![value.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BusPolicy;

    fn app_of(toml: &str) -> AppOverride {
        toml::from_str(toml).unwrap()
    }

    /// The baked file is what flatpak would have written, separator for
    /// separator. If this drifts, a declared override and a hand-made
    /// one stop comparing equal and `kuma diff` reports drift forever.
    #[test]
    fn a_declaration_renders_as_flatpak_writes_it() {
        let a = app_of(
            "filesystems = [\"home\", \"!xdg-config/kitty\"]\n\
             sockets = [\"wayland\"]\n\
             [environment]\n\
             MOZ_ENABLE_WAYLAND = \"1\"\n\
             [session-bus]\n\
             \"org.freedesktop.Flatpak\" = \"talk\"\n",
        );
        assert_eq!(
            render(&a),
            "[Context]\n\
             filesystems=home;!xdg-config/kitty;\n\
             sockets=wayland;\n\
             \n\
             [Environment]\n\
             MOZ_ENABLE_WAYLAND=1\n\
             \n\
             [Session Bus Policy]\n\
             org.freedesktop.Flatpak=talk\n"
        );
    }

    /// The whole feature in one test: kuma's key is set, the key beside
    /// it in the same group is somebody else's and survives untouched.
    #[test]
    fn convergence_edits_its_own_key_and_no_other() {
        let live = "[Context]\nfilesystems=home;\nsockets=x11;\n";
        let a = app_of("filesystems = [\"host\"]\n");
        let (out, changed, owned) = converge(live, &declared_keys(&a), &[]);
        assert_eq!(out, "[Context]\nfilesystems=host;\nsockets=x11;\n");
        assert_eq!(changed.set, vec![owned_id(CONTEXT, "filesystems")]);
        assert!(changed.removed.is_empty());
        assert_eq!(owned, vec![owned_id(CONTEXT, "filesystems")]);
    }

    /// Undeclaring removes only what kuma set. The same test proves the
    /// other half: `sockets` was never kuma's, so dropping every
    /// declared key does not make it kuma's to delete.
    #[test]
    fn undeclaring_a_key_removes_it_and_leaves_the_rest() {
        let live = "[Context]\nfilesystems=host;\nsockets=x11;\n";
        let previously = vec![owned_id(CONTEXT, "filesystems")];
        let (out, changed, owned) = converge(live, &[], &previously);
        assert_eq!(out, "[Context]\nsockets=x11;\n");
        assert_eq!(changed.removed, previously);
        assert!(owned.is_empty());
    }

    /// A file kuma emptied is deleted rather than left as an empty
    /// stanza, and the caller learns that by being handed "".
    #[test]
    fn the_last_key_leaving_empties_the_file() {
        let live = "[Context]\nfilesystems=host;\n";
        let (out, _, _) = converge(live, &[], &[owned_id(CONTEXT, "filesystems")]);
        assert!(out.is_empty(), "nothing left, so nothing to write: {out:?}");
    }

    /// Comments and groups kuma does not know about are the machine's,
    /// and a converger that eats them is one nobody will run twice.
    #[test]
    fn everything_kuma_does_not_understand_survives() {
        let live =
            "# set by hand, 2026\n[Context]\nsockets=x11;\n\n[Some Future Group]\nkey=value\n";
        let a = app_of("devices = [\"dri\"]\n");
        let (out, _, _) = converge(live, &declared_keys(&a), &[]);
        assert!(out.starts_with("# set by hand, 2026\n"), "{out}");
        assert!(out.contains("[Some Future Group]\nkey=value\n"), "{out}");
        assert!(out.contains("devices=dri;"), "{out}");
        assert!(out.contains("sockets=x11;"), "{out}");
    }

    /// The state file has a writer and a reader and no format anywhere
    /// else, so the only thing keeping them agreeing is that they were
    /// written together. A key whose group holds a space ("Session Bus
    /// Policy") is the shape that would break a naive separator.
    #[test]
    fn the_state_file_survives_a_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state");
        let mut owned: BTreeMap<String, Vec<String>> = BTreeMap::new();
        owned.insert(
            "org.example.App".into(),
            vec![
                owned_id(CONTEXT, "filesystems"),
                owned_id(SESSION_BUS, "org.freedesktop.Flatpak"),
            ],
        );
        owned.insert("org.example.Other".into(), vec![owned_id(ENVIRONMENT, "GTK_THEME")]);
        write_state(&path, &owned).unwrap();
        assert_eq!(read_state(&path), owned);
    }

    /// A machine that declares no permissions ends up with no state
    /// file either. The converger runs on every boot of every machine,
    /// and one that creates a file to record that it owns nothing is
    /// litter of exactly the kind this rung went looking for.
    #[test]
    fn owning_nothing_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        std::fs::create_dir_all(declared_dir(Scope::System, root)).unwrap();
        assert!(converge_store(Scope::System, root, &home).unwrap().is_empty());
        assert!(
            !state_path(Scope::System, root, &home).exists(),
            "an empty state file was written"
        );
    }

    /// A comment inside a group is somebody explaining their machine to
    /// themselves, and taking back the key beside it is no reason to
    /// delete it. The module's own promise is that everything kuma does
    /// not understand survives; the first version kept comments only
    /// when they sat above the first group header.
    #[test]
    fn a_comment_survives_the_key_it_sat_beside_being_taken_back() {
        let live = "[Context]\n# turned this off deliberately\nfilesystems=host;\n";
        let (out, changed, _) = converge(live, &[], &[owned_id(CONTEXT, "filesystems")]);
        assert_eq!(changed.removed, vec![owned_id(CONTEXT, "filesystems")]);
        assert!(out.contains("# turned this off deliberately"), "the note was eaten: {out:?}");
        assert!(!out.contains("filesystems"), "{out:?}");
    }

    /// A file kuma has never written a key into is not kuma's to touch,
    /// whatever is in it. This one passed the day it was written: an app
    /// is visited only if the declaration names it or the state file
    /// claims a key in it. It stays as the guard on that, because the
    /// obvious future change here is to widen what the pass looks at.
    #[test]
    fn a_file_kuma_never_owned_is_not_deleted_for_having_no_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let store_dir = store(Scope::System, root, &home);
        std::fs::create_dir_all(&store_dir).unwrap();
        let path = store_dir.join("org.example.Notes");
        std::fs::write(&path, "[Context]\n# nothing here yet, on purpose\n").unwrap();

        converge_store(Scope::System, root, &home).unwrap();
        assert!(path.exists(), "a file kuma never touched was deleted");
        assert!(std::fs::read_to_string(&path).unwrap().contains("on purpose"));
    }

    /// Converging twice with the same declaration must report nothing
    /// the second time. Without this the boot unit would announce
    /// changes on every boot and the report would mean nothing.
    #[test]
    fn a_second_pass_changes_nothing() {
        let a = app_of("filesystems = [\"home\"]\n");
        let (once, first, owned) = converge("", &declared_keys(&a), &[]);
        assert!(!first.is_empty());
        let (twice, second, _) = converge(&once, &declared_keys(&a), &owned);
        assert_eq!(once, twice);
        assert!(second.is_empty(), "second pass reported {second:?}");
    }

    /// A key kuma set, then a person changed by hand, is kuma's to set
    /// back: that is what declaring it means. The report says so, which
    /// is the difference between convergence and a silent fight.
    #[test]
    fn a_hand_edit_to_a_declared_key_is_taken_back_and_reported() {
        let a = app_of("sockets = [\"wayland\"]\n");
        let owned = vec![owned_id(CONTEXT, "sockets")];
        let (out, changed, _) = converge("[Context]\nsockets=x11;\n", &declared_keys(&a), &owned);
        assert_eq!(out, "[Context]\nsockets=wayland;\n");
        assert_eq!(changed.set, owned);
    }

    /// The whole runtime pass against a directory tree: what the image
    /// baked reaches the store, a key that was already there and is not
    /// kuma's survives, running twice reports nothing, and undeclaring
    /// the app takes back kuma's keys and only kuma's.
    ///
    /// This is the test that would have to fail for the feature to be
    /// wrong on a real machine; everything above it is a unit of it.
    #[test]
    fn a_scope_converges_against_a_real_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let app = "org.example.App";

        let declared = declared_dir(Scope::System, root);
        std::fs::create_dir_all(&declared).unwrap();
        let decl = app_of("sockets = [\"wayland\"]\n");
        std::fs::write(declared.join(app), render(&decl)).unwrap();

        // the machine already had an opinion about a different key
        let store_dir = store(Scope::System, root, &home);
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(store_dir.join(app), "[Context]\nfilesystems=home;\n").unwrap();

        let report = converge_store(Scope::System, root, &home).unwrap();
        let live = std::fs::read_to_string(store_dir.join(app)).unwrap();
        assert!(live.contains("sockets=wayland;"), "{live}");
        assert!(live.contains("filesystems=home;"), "kuma ate a key that was not its own: {live}");
        assert_eq!(report.len(), 1);

        // idempotent: the boot after this one has nothing to say
        assert!(converge_store(Scope::System, root, &home).unwrap().is_empty());

        // undeclared: kuma's key goes, the machine's key stays
        std::fs::remove_file(declared.join(app)).unwrap();
        let report = converge_store(Scope::System, root, &home).unwrap();
        assert_eq!(report.len(), 1);
        let live = std::fs::read_to_string(store_dir.join(app)).unwrap();
        assert!(!live.contains("sockets"), "{live}");
        assert!(live.contains("filesystems=home;"), "{live}");

        // and the state file no longer claims a key kuma does not set
        let state = std::fs::read_to_string(state_path(Scope::System, root, &home)).unwrap();
        assert!(state.is_empty(), "state still claims {state:?}");
    }

    /// A file that only ever held kuma's keys is removed rather than
    /// left behind empty, on the same pass that stops declaring them.
    #[test]
    fn a_file_kuma_emptied_is_deleted_from_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let app = "org.example.Only";
        let declared = declared_dir(Scope::User, root);
        std::fs::create_dir_all(&declared).unwrap();
        std::fs::write(declared.join(app), render(&app_of("devices = [\"dri\"]\n"))).unwrap();
        converge_store(Scope::User, root, &home).unwrap();
        let path = store(Scope::User, root, &home).join(app);
        assert!(path.exists());

        std::fs::remove_file(declared.join(app)).unwrap();
        converge_store(Scope::User, root, &home).unwrap();
        assert!(!path.exists(), "an emptied override file was left behind");
    }

    /// Baked and read back must be the same keys. The build writes the
    /// file and the machine reads it, and nothing else connects the two,
    /// so a change to either side that is not a change to both is a
    /// machine that converges to something the declaration did not say.
    #[test]
    fn what_the_build_writes_is_what_the_machine_reads() {
        let decl = app_of(
            "filesystems = [\"home\"]\n\
             [environment]\n\
             GTK_THEME = \"Adwaita:dark\"\n\
             [system-bus]\n\
             \"org.freedesktop.UPower\" = \"talk\"\n",
        );
        assert_eq!(parse_declared(&render(&decl)), declared_keys(&decl));
    }

    /// diff reports both directions, and the negative in the middle is
    /// the one that matters: a key nobody declared and kuma never set is
    /// not drift, it is somebody's machine, and reporting it would make
    /// diff cry wolf on every override anyone ever wrote by hand.
    #[test]
    fn drift_reports_both_directions_and_leaves_strangers_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let store_dir = store(Scope::System, root, &home);
        std::fs::create_dir_all(&store_dir).unwrap();
        // the machine has one key kuma set, and one it never touched
        std::fs::write(
            store_dir.join("org.example.App"),
            "[Context]\nsockets=x11;\nfilesystems=home;\n",
        )
        .unwrap();
        let state = state_path(Scope::System, root, &home);
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "org.example.App\tContext\tsockets\n").unwrap();

        // declaring something else entirely: sockets is kuma's to take
        // back, devices is missing, filesystems was never kuma's
        let mut declared: Overrides = Default::default();
        declared.insert("org.example.App".into(), app_of("devices = [\"dri\"]\n"));
        let found = drift(&declared, root, &home);
        let items: Vec<String> =
            found.iter().map(|d| format!("{} {}", d.change, d.item())).collect();
        assert_eq!(items, vec!["add org.example.App devices", "remove org.example.App sockets"]);
        assert!(
            !items.iter().any(|i| i.contains("filesystems")),
            "diff claimed a key kuma never set: {items:?}"
        );
    }

    /// A declaration that matches the machine is silent. Without this a
    /// converged machine would report drift forever and the section
    /// would teach people to ignore it.
    #[test]
    fn a_converged_store_shows_no_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let mut declared: Overrides = Default::default();
        declared.insert("org.example.App".into(), app_of("sockets = [\"wayland\"]\n"));
        let dir = declared_dir(Scope::System, root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("org.example.App"), render(&declared["org.example.App"])).unwrap();
        converge_store(Scope::System, root, &home).unwrap();
        assert_eq!(drift(&declared, root, &home), vec![]);
    }

    /// The declaration being ahead of the image is the trap `kuma sync`
    /// walks into: the converger reads what the image baked, so an edit
    /// reaches nothing until a rebuild. diff is where that gets said.
    #[test]
    fn an_edit_the_image_has_not_baked_reads_as_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = declared_dir(Scope::System, root);
        std::fs::create_dir_all(&dir).unwrap();
        let mut declared: Overrides = Default::default();
        declared.insert("org.example.App".into(), app_of("sockets = [\"wayland\"]\n"));

        // baked nothing yet
        assert!(image_stale(&declared, root));
        // baked exactly this
        std::fs::write(dir.join("org.example.App"), render(&declared["org.example.App"])).unwrap();
        assert!(!image_stale(&declared, root));
        // declaration changed since the build
        declared.insert("org.example.App".into(), app_of("sockets = [\"x11\"]\n"));
        assert!(image_stale(&declared, root));
        // an app dropped from the declaration but still baked
        declared.clear();
        assert!(image_stale(&declared, root));
    }

    /// Capture offers permissions for apps the declaration installs, and
    /// only the keys that are somebody's rather than kuma's. The two
    /// negatives are the point: an override file for an app that left
    /// years ago is rubble, and flatpak's `global` file is not an app at
    /// all, so neither is ever proposed.
    #[test]
    fn capture_offers_only_declared_apps_and_only_keys_kuma_never_set() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let store_dir = store(Scope::User, root, &home);
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(
            store_dir.join("org.example.Kept"),
            "[Context]\nsockets=x11;\ndevices=dri;\n",
        )
        .unwrap();
        std::fs::write(store_dir.join("org.example.Gone"), "[Context]\nsockets=x11;\n").unwrap();
        std::fs::write(store_dir.join("global"), "[Context]\nfilesystems=host;\n").unwrap();
        // kuma set `devices` itself, so it is not a proposal
        let state = state_path(Scope::User, root, &home);
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "org.example.Kept\tContext\tdevices\n").unwrap();

        let installed: BTreeSet<&str> = ["org.example.Kept"].into_iter().collect();
        let (proposals, ambiguous) = capturable(&installed, &Default::default(), root, &home);
        assert!(ambiguous.is_empty());
        assert_eq!(proposals.len(), 1, "{proposals:?}");
        assert_eq!(proposals[0].app, "org.example.Kept");
        assert_eq!(proposals[0].scope, Scope::User);
        assert_eq!(
            proposals[0].keys,
            vec![("Context".to_string(), "sockets".to_string(), "x11;".to_string())]
        );
    }

    /// A key the declaration could not spell is not proposed for it.
    /// Capture writes through the same validating path `kuma add` uses,
    /// so a key with a space in its name would not land quietly: it
    /// would fail the write and take every other proposal with it.
    #[test]
    fn a_key_the_declaration_cannot_spell_is_not_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        let store_dir = store(Scope::System, root, &home);
        std::fs::create_dir_all(&store_dir).unwrap();
        std::fs::write(
            store_dir.join("org.example.App"),
            "[Environment]\nGTK_THEME=Adwaita\nnot a var name=x\n\
             [Some Future Group]\nwhatever=1\n",
        )
        .unwrap();
        let installed: BTreeSet<&str> = ["org.example.App"].into_iter().collect();
        let (proposals, _) = capturable(&installed, &Default::default(), root, &home);
        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].keys,
            vec![("Environment".to_string(), "GTK_THEME".to_string(), "Adwaita".to_string())],
            "only what the schema can hold"
        );
    }

    /// One app declares into one store, so an app carrying undeclared
    /// keys in both is named rather than guessed at. Picking a store for
    /// somebody is how a proposal stops being something you can trust
    /// without reading the machine yourself.
    #[test]
    fn an_app_with_keys_in_both_stores_is_not_guessed_at() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/mira");
        for scope in [Scope::System, Scope::User] {
            let dir = store(scope, root, &home);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("org.example.Both"), "[Context]\nsockets=x11;\n").unwrap();
        }
        let installed: BTreeSet<&str> = ["org.example.Both"].into_iter().collect();
        let (proposals, ambiguous) = capturable(&installed, &Default::default(), root, &home);
        assert!(proposals.is_empty(), "{proposals:?}");
        assert_eq!(ambiguous, vec!["org.example.Both"]);
    }

    /// Bus policies are written as flatpak spells them, and the parser
    /// is what stops "Talk" or "yes" from reaching the file.
    #[test]
    fn bus_policies_take_only_flatpaks_four_words() {
        let a = app_of("[system-bus]\n\"org.freedesktop.UPower\" = \"see\"\n");
        assert_eq!(a.system_bus["org.freedesktop.UPower"], BusPolicy::See);
        assert!(toml::from_str::<AppOverride>("[system-bus]\nx = \"Talk\"\n").is_err());
    }
}
