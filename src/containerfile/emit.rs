//! The walk and its recorder.
//!
//! One pass over the block registry produces both halves of what an
//! image carries — the Containerfile text and the set of staged files —
//! from the same gates in the same functions, so the two cannot
//! disagree. `generate` keeps the text; `write_context` materializes
//! the files. Staging is a record, not a write, so the pure half of the
//! interface stays pure and a `generate` costs no filesystem at all.
//!
//! Two facts are constructive here rather than test-pinned: a COPY can
//! only name a file [`stage`](Self::stage) returned — a handle, not a
//! string — and a kuma unit can only be enabled by a block whose
//! units table declares its installer-media disposition. Both used to
//! be checked by parsing the emitted text afterwards; parsing is what
//! missed the units nobody could see.

use super::blocks::{Block, Live};
use super::Config;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What one staged file holds. Text and bytes are written as-is; a
/// tree materializes as a directory, so COPY lines can name it as a
/// unit (plymouth's theme, the overrides stores).
#[derive(PartialEq)]
pub(crate) enum Content {
    Text(String),
    Bytes(&'static [u8]),
    Tree(Vec<(String, Content)>),
}

impl From<String> for Content {
    fn from(text: String) -> Content {
        Content::Text(text)
    }
}

impl From<&'static str> for Content {
    fn from(text: &'static str) -> Content {
        Content::Text(text.to_string())
    }
}

impl From<&'static [u8]> for Content {
    fn from(bytes: &'static [u8]) -> Content {
        Content::Bytes(bytes)
    }
}

/// A staged file, as a value only `stage` can produce. A COPY names
/// its source through one of these, so a COPY of a file the walk never
/// staged is not a bug to catch — it is a thing the type will not
/// express.
#[derive(Clone)]
pub(crate) struct Staged {
    name: String,
}

/// One walk's result: the text, and every staged file.
pub(crate) struct Plan {
    pub(crate) text: String,
    pub(crate) files: BTreeMap<String, Content>,
}

/// The recorder a feature-block emits through. `raw` is the faithful
/// scribe — byte identity is the goldens' job, not a pretty-printer's —
/// and `stage`/`copy`/`enable` are the typed doors for the three facts
/// downstream modules used to re-derive by parsing.
pub(crate) struct Emitter<'c> {
    pub(crate) config: &'c Config,
    text: String,
    files: BTreeMap<String, Content>,
    copied: BTreeSet<String>,
    block_name: &'static str,
    block_units: &'static [(&'static str, Live)],
}

impl<'c> Emitter<'c> {
    pub(crate) fn new(config: &'c Config) -> Emitter<'c> {
        Emitter {
            config,
            text: String::new(),
            files: BTreeMap::new(),
            copied: BTreeSet::new(),
            block_name: "",
            block_units: &[],
        }
    }

    /// The block about to emit, so `enable` can hold its units to the
    /// disposition table the block itself declares.
    pub(crate) fn enter_block(&mut self, block: &'static Block) {
        self.block_name = block.name;
        self.block_units = block.units;
    }

    /// Verbatim text: FROM, RUN (arbitrary shell), LABEL, comments —
    /// everything the Containerfile is that is not one of the typed
    /// facts below. COPY is a typed fact: the door refuses it, because
    /// a raw COPY is the one shape that used to drift against the
    /// staged files.
    pub(crate) fn raw(&mut self, text: &str) {
        assert!(
            !text.trim_start().starts_with("COPY "),
            "raw COPY in block {}: use copy(), so the source is a file the walk staged",
            self.block_name
        );
        self.text.push_str(text);
    }

    /// Register a staged file, and return the handle COPY names it by.
    /// Idempotent per name with a same-content assert: the wallpaper
    /// is staged by the desktop-common block and copied by both desktop
    /// arms, and a second staging that disagrees with the first is a
    /// bug this catches at the walk rather than in an image.
    pub(crate) fn stage(&mut self, name: &str, content: impl Into<Content>) -> Staged {
        let content = content.into();
        match self.files.get(name) {
            Some(existing) => assert!(
                existing == &content,
                "{name} is staged twice with different contents in block {}",
                self.block_name
            ),
            None => {
                self.files.insert(name.to_string(), content);
            }
        }
        Staged { name: name.to_string() }
    }

    /// Register a staged directory, from (name, content) pairs.
    pub(crate) fn stage_tree(
        &mut self,
        dir: &str,
        files: impl IntoIterator<Item = (String, Content)>,
    ) -> Staged {
        self.files.insert(dir.to_string(), Content::Tree(files.into_iter().collect()));
        Staged { name: dir.to_string() }
    }

    /// A file the build context carries that this walk did not stage:
    /// the kuma binary and the declaration, which `write_context`'s
    /// own parameters supply. A handle like any other, so their COPY
    /// lines go through the same door.
    pub(crate) fn supplied(&mut self, name: &str) -> Staged {
        Staged { name: name.to_string() }
    }

    /// `COPY name dest`
    pub(crate) fn copy(&mut self, file: &Staged, dest: &str) {
        self.text.push_str(&format!("COPY {} {}\n", file.name, dest));
        self.copied.insert(file.name.clone());
    }

    /// `COPY --chmod=755 name dest` — executables, which arrive by the
    /// build's copy rather than a RUN chmod so the mode is in the layer
    /// metadata where an audit can see it.
    pub(crate) fn copy_exec(&mut self, file: &Staged, dest: &str) {
        self.text.push_str(&format!("COPY --chmod=755 {} {}\n", file.name, dest));
        self.copied.insert(file.name.clone());
    }

    /// `COPY --chmod=600 name dest` — the account declaration, readable
    /// by root only because it can carry a password hash.
    pub(crate) fn copy_private(&mut self, file: &Staged, dest: &str) {
        self.text.push_str(&format!("COPY --chmod=600 {} {}\n", file.name, dest));
        self.copied.insert(file.name.clone());
    }

    /// `RUN systemctl enable …` — and the accountability door: a kuma
    /// unit can only be enabled by the block whose units table declares
    /// what it does on installer media. The old parser missed the ones
    /// a compound line hid; there is no line shape here to hide behind.
    pub(crate) fn enable(&mut self, units: &[&str]) {
        for unit in units {
            self.account(unit);
        }
        self.text.push_str(&format!("RUN systemctl enable {}\n", units.join(" ")));
    }

    /// `RUN systemctl --global enable …` — the bunker's shape: a user
    /// unit with no system-scope half.
    pub(crate) fn enable_global(&mut self, units: &[&str]) {
        for unit in units {
            self.account(unit);
        }
        self.text.push_str(&format!("RUN systemctl --global enable {}\n", units.join(" ")));
    }

    /// `--global` first, then system scope, in one RUN: the niri arm's
    /// compound shape, byte-exact.
    pub(crate) fn enable_global_then_system(&mut self, global: &[&str], system: &[&str]) {
        for unit in global.iter().chain(system) {
            self.account(unit);
        }
        self.text.push_str(&format!(
            "RUN systemctl --global enable {} \\\n    && systemctl enable {}\n",
            global.join(" "),
            system.join(" ")
        ));
    }

    /// System scope first, then `--global`: the overrides converger's
    /// compound shape, byte-exact.
    pub(crate) fn enable_system_then_global(&mut self, system: &[&str], global: &[&str]) {
        for unit in system.iter().chain(global) {
            self.account(unit);
        }
        self.text.push_str(&format!(
            "RUN systemctl enable {} \\\n    && systemctl --global enable {}\n",
            system.join(" "),
            global.join(" ")
        ));
    }

    fn account(&mut self, unit: &str) {
        if !unit.starts_with("kuma-") {
            return;
        }
        assert!(
            self.block_units.iter().any(|(u, _)| *u == unit),
            "{unit} is enabled by the {} block, which declares no installer-media \
             disposition for it; add it to the block's units table",
            self.block_name
        );
    }

    pub(crate) fn finish(self) -> Plan {
        Plan { text: self.text, files: self.files }
    }

    /// The whole-walk reconcile: every staged file is COPY'd by some
    /// block, or the context would carry a file no image reads. A
    /// whole-walk property, not a per-block one — the wallpaper is
    /// staged by the desktop-common block and copied by the desktop
    /// arms — so only the full `plan` calls this, never the per-block
    /// test surface.
    pub(crate) fn reconcile(&self) {
        let never_copied: Vec<&str> = self
            .files
            .keys()
            .filter(|name| !self.copied.contains(*name))
            .map(|name| name.as_str())
            .collect();
        assert!(
            never_copied.is_empty(),
            "staged but never COPY'd: {never_copied:?} — the context would carry \
             files no image reads"
        );
    }
}

/// Write every staged file under `dir`.
pub(crate) fn materialize(files: &BTreeMap<String, Content>, dir: &Path) -> Result<()> {
    for (name, content) in files {
        write_content(&dir.join(name), content)
            .with_context(|| format!("staging {name} into the build context"))?;
    }
    Ok(())
}

fn write_content(path: &Path, content: &Content) -> Result<()> {
    match content {
        Content::Text(text) => std::fs::write(path, text),
        Content::Bytes(bytes) => std::fs::write(path, bytes),
        Content::Tree(children) => {
            std::fs::create_dir_all(path)?;
            for (name, content) in children {
                write_content(&path.join(name), content)?;
            }
            return Ok(());
        }
    }
    .with_context(|| format!("staging {} into the build context", path.display()))
}
