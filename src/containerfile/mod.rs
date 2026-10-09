mod blocks;
mod emit;

use crate::config::{Config, Desktop};
use crate::seam;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

// The feature data and its render helpers live in `blocks`, beside the
// spans that emit them; re-exported here so the crate's paths stay as
// they were. The first group has live consumers outside this module;
// the second is read only by this file's own tests.
#[cfg(test)]
pub(crate) use blocks::{
    registries_d, signature_policy, COSIGN_PUB, KUMA_LAUNCH, NETWORK_CONNECTIONS,
};
pub(crate) use blocks::{
    COSIGN_PUB_PATH, COSMIC_GREETER_CONF, COSMIC_SESSION, FLATHUB_URL, GREETD_CONF, NIRI_SESSION,
};
// Read by inspect's doctor check, which grades what a niri config that
// shadows the image's one can still run.
#[cfg(test)]
pub(crate) use blocks::{NIRI_MEDIA_BINDS, NIRI_MENU_BIND};

/// Every /etc path this image owns the *contents* of.
///
/// Derived from the Containerfile the config compiles to rather than
/// listed by hand, so a new write into /etc is covered the day it lands
/// and can't be forgotten by whoever adds it.
///
/// A build writes into /etc three ways, all unambiguous: a COPY whose
/// destination is under /etc, a shell redirect (`>`, `>>`) into one, and
/// a symlink made there with `ln -s`. Reading an /etc file is not owning
/// it, and the difference matters: the keyring assert greps
/// /etc/pam.d/greetd and `niri validate` reads /etc/niri/config.kdl, but
/// only one of those two files is kuma's to have an opinion about.
/// Redirects separate them for free, since a read has none.
///
/// The third way is here because it was missing, and a declared
/// `system.timezone` is what it cost: the timezone lands as
/// `ln -sfn /usr/share/zoneinfo/<zone> /etc/localtime`, which is neither
/// a COPY nor a redirect, so the one file the declaration explicitly
/// claims was owned by the image and watched by nobody. On a machine
/// installed by Anaconda, which writes its own /etc/localtime, that is
/// precisely where the ostree merge makes a declared value never apply.
pub fn etc_paths(config: &Config) -> Vec<String> {
    let mut paths = etc_writes(&generate(config));
    // Hostname is machine state: unpinned, the baked file only seeds the
    // ostree merge default and an admin's `hostnamectl` rename is the
    // sanctioned interface, not drift. A *declared* hostname is an
    // opinion the machine should match, so then it stays owned.
    if config.system.hostname.is_none() {
        paths.retain(|p| p != "/etc/hostname");
    }
    paths
}

fn etc_writes(containerfile: &str) -> Vec<String> {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for line in containerfile.lines() {
        if let Some(dest) =
            line.strip_prefix("COPY ").and_then(|rest| rest.split_whitespace().last())
        {
            if dest.starts_with("/etc/") {
                paths.insert(dest);
            }
        }
        // Split on the shell's separators so a line running several
        // commands is read as several commands, and the destination of
        // each `ln -s` is its own last word. Asked of the whole line
        // first, so the splitting only happens on the lines that could
        // possibly answer yes.
        for segment in line
            .contains("ln -s")
            .then(|| line.split("&&").flat_map(|part| part.split(';')))
            .into_iter()
            .flatten()
        {
            if !segment.contains("ln -s") {
                continue;
            }
            if let Some(dest) = segment.split_whitespace().last() {
                if dest.starts_with("/etc/") {
                    paths.insert(dest);
                }
            }
        }
        let mut rest = line;
        while let Some(at) = rest.find('>') {
            rest = rest[at + 1..].trim_start_matches('>').trim_start();
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(rest.len());
            if rest[..end].starts_with("/etc/") {
                paths.insert(&rest[..end]);
            }
        }
    }
    paths.into_iter().map(str::to_string).collect()
}

/// Compile a kuma config into a Containerfile for a bootc image build.
/// Byte-stable by contract: the goldens in `goldens/` pin the output
/// for the declaration matrix, and a deliberate change is a golden
/// diff someone reads.
pub fn generate(config: &Config) -> String {
    plan(config).text
}

/// One walk over the block registry: the Containerfile text and every
/// file it stages, from the same gates in the same functions, so the
/// two halves of what an image carries cannot disagree.
fn plan(config: &Config) -> emit::Plan {
    let mut e = emit::Emitter::new(config);
    for block in blocks::BLOCKS {
        e.enter_block(block);
        (block.emit)(&mut e);
    }
    e.reconcile();
    e.finish()
}

/// The kuma units a live session on installer media must not run,
/// derived from the same tables that enable them. The bootc-preset
/// units the live layer masks on its own behalf are liveiso's business
/// and stay there.
pub fn live_masks() -> Vec<&'static str> {
    blocks::live_masks()
}

/// Write the full build context: the Containerfile plus any files it
/// COPYs. The text and the files come from one walk (`plan`), so a COPY
/// of something the walk never staged is not a bug to catch later — it
/// is a name that does not exist.
///
/// `config_text` is the declaration verbatim — comments and formatting
/// intact — because it gets baked into the image at /usr/lib/kuma/kuma.toml.
///
/// `kuma_binary` is the running kuma, passed rather than resolved here so
/// the tests can stage a stub instead of copying a 42 MB test harness into
/// a temp directory fourteen times — the same reason `loop_mounts_in` takes
/// its input and `scan_etc` takes its roots.
pub fn write_context(
    config: &Config,
    config_text: &str,
    kuma_binary: &Path,
    dir: &Path,
) -> Result<()> {
    std::fs::write(dir.join("kuma.toml"), config_text)?;
    std::fs::copy(kuma_binary, dir.join("kuma"))
        .with_context(|| format!("staging {} into the build context", kuma_binary.display()))?;
    // The nostr layer's binaries ride the same road: they live beside
    // the running kuma, because the release ships the three together
    // and a build whose siblings are missing is a checkout, not a
    // release. The failure is honest and early — the nostr block's
    // COPY would otherwise name files the context never carried.
    let parent = kuma_binary
        .parent()
        .with_context(|| "the running kuma has no parent directory".to_string())?;
    // The desktop shell rides the same road, and is staged only where
    // an image will COPY it: built from the kumaui tree, beside the
    // running kuma, shipped with the release. On any other desktop the
    // binary is dead weight, so it does not ride.
    if config.system.desktop == Desktop::Niri {
        let shell_sibling = parent.join("kuma-shell");
        std::fs::copy(&shell_sibling, dir.join("kuma-shell")).with_context(|| {
            format!(
                "staging kuma-shell: it is not beside the running kuma ({}); a \
                 desktop image needs the release's binaries together",
                kuma_binary.display()
            )
        })?;
        // The greeter is the same build's second binary, and the
        // image's greetd config points at it: an image whose context
        // carried the shell but not the greeter would COPY a config
        // naming a binary that is not there.
        let greeter_sibling = parent.join("kuma-greeter");
        std::fs::copy(&greeter_sibling, dir.join("kuma-greeter")).with_context(|| {
            format!(
                "staging kuma-greeter: it is not beside the running kuma ({}); a \
                 desktop image needs the release's binaries together",
                kuma_binary.display()
            )
        })?;
        // Koguma is the same build's third binary and the first app
        // among them: the image offers it to the user, so its desktop
        // entry and its icon ride beside the binary — the entry is the
        // app's identity (the name, the keywords, the MimeType it
        // claims) and ships from the tree that builds the app, not
        // regenerated here. The mimeapps pin keeps the entry honest
        // about the ship: installed, not the default.
        let files_sibling = parent.join("kuma-files");
        std::fs::copy(&files_sibling, dir.join("kuma-files")).with_context(|| {
            format!(
                "staging kuma-files: it is not beside the running kuma ({}); a \
                 desktop image needs the release's binaries together",
                kuma_binary.display()
            )
        })?;
        let files_desktop = parent.join("kuma-files.desktop");
        std::fs::copy(&files_desktop, dir.join("kuma-files.desktop")).with_context(|| {
            format!(
                "staging kuma-files.desktop: it is not beside the running kuma ({}) — \
                 the app's entry ships from the kumaui tree beside its binary",
                kuma_binary.display()
            )
        })?;
        let files_icon = parent.join("kuma-files.png");
        std::fs::copy(&files_icon, dir.join("kuma-files.png")).with_context(|| {
            format!(
                "staging kuma-files.png: it is not beside the running kuma ({}) — \
                 the entry names this icon, so the icon rides with it",
                kuma_binary.display()
            )
        })?;
    }
    if config.nostr.enable {
        let mut names = vec!["kuma-nostrd", "kuma-nostr"];
        if config.nostr.relay.enable {
            names.push("nip46-relay");
        }
        for name in names {
            let sibling = parent.join(name);
            std::fs::copy(&sibling, dir.join(name)).with_context(|| {
                format!(
                    "staging {name}: it is not beside the running kuma ({}); a \
                     nostr-enabled image needs the release's binaries together",
                    kuma_binary.display()
                )
            })?;
        }
    }
    let plan = plan(config);
    std::fs::write(dir.join("Containerfile"), &plan.text)?;
    emit::materialize(&plan.files, dir)?;
    Ok(())
}

/// "de_DE.UTF-8" → "de": the glibc langpack that provides the locale.
/// Locales without a territory part (C, POSIX, C.UTF-8) need none.
fn langpack(locale: &str) -> Option<&str> {
    let lang = locale.split('_').next()?;
    (locale.contains('_')
        && (2..=3).contains(&lang.len())
        && lang.chars().all(|c| c.is_ascii_lowercase()))
    .then_some(lang)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containerfile::blocks::*;

    fn config(toml: &str) -> Config {
        let config: Config = toml::from_str(toml).unwrap();
        config.validate().unwrap();
        config
    }

    /// A stand-in for the kuma binary: write_context only copies it, and
    /// the real one here would be the 42 MB test harness. Dot-prefixed so
    /// it cannot collide with a name the context actually uses.
    fn context(toml: &str, dir: &Path) {
        // The binaries stage from beside the running one, which is
        // nowhere near the context dir: a copy onto itself truncates
        // to empty, and the manifest would pin the emptiness.
        let bin_home = tempfile::tempdir().unwrap();
        let stub = bin_home.path().join("stub-kuma");
        std::fs::write(&stub, b"not really a binary\n").unwrap();
        let cfg = config(toml);
        // The shell and the nostr siblings are as fake as the stub, and
        // as stable.
        std::fs::write(bin_home.path().join("kuma-shell"), "not really kuma-shell\n").unwrap();
        std::fs::write(bin_home.path().join("kuma-greeter"), "not really kuma-greeter\n").unwrap();
        // Koguma's trio rides with them: the binary, its desktop entry,
        // and the icon the entry names. Written for every case like the
        // shell's stubs, because write_context only reads them where a
        // niri image will COPY them.
        std::fs::write(bin_home.path().join("kuma-files"), "not really kuma-files\n").unwrap();
        std::fs::write(bin_home.path().join("kuma-files.desktop"), "not really kuma-files\n")
            .unwrap();
        std::fs::write(bin_home.path().join("kuma-files.png"), "not really kuma-files\n").unwrap();
        if cfg.nostr.enable {
            let mut names = vec!["kuma-nostrd", "kuma-nostr"];
            if cfg.nostr.relay.enable {
                names.push("nip46-relay");
            }
            for name in names {
                std::fs::write(bin_home.path().join(name), format!("not really {name}\n")).unwrap();
            }
        }
        write_context(&cfg, toml, &stub, dir).unwrap();
    }

    /// The declaration matrix every structural change to the generator
    /// has to keep honest, and the two goldens it pins.
    ///
    /// The Containerfile text is the layer order, the enable grammar,
    /// the COPY spelling: every byte of what podman will read. The
    /// staging manifest is what actually lands beside it — same set of
    /// files, hashed, so a changed script with an unchanged COPY line
    /// is caught here rather than on somebody's machine.
    ///
    /// Regenerate deliberately, never in CI: `UPDATE_GOLDENS=1 cargo
    /// test` and read the diff — a golden that moves is the review.
    const GOLDEN_CASES: &[(&str, &str)] = &[
        ("minimal", "schema_version = 1\n"),
        ("niri", "schema_version = 1\n[system]\ndesktop = \"niri\"\n"),
        ("cosmic", "schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"),
        (
            "nostr",
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n[nostr]\nenable = true\n\
             serve = true\nrelays = [\"wss://relay.nip46.com\", \"ws://127.0.0.1:7777\"]\n",
        ),
        ("everything-on", EVERYTHING_ON),
        ("secrets", SECRETS),
    ];

    #[test]
    fn the_generated_containerfile_is_byte_stable() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/containerfile/goldens");
        let mut regenerate = false;
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            regenerate = true;
        }
        for (name, toml) in GOLDEN_CASES {
            let out = normalize_builder(&generate(&config(toml)));
            let golden = dir.join(format!("{name}.Containerfile"));
            if regenerate {
                std::fs::write(&golden, &out).unwrap();
                continue;
            }
            let expected = normalize_builder(&std::fs::read_to_string(&golden).unwrap_or_default());
            assert_eq!(out, expected, "golden for {name} moved; see the test's doc");
        }
    }

    #[test]
    fn the_nostr_unit_carries_the_relay_set_and_the_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        context(
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n[nostr]\nenable = true\n\
             relays = [\"wss://relay.nip46.com\", \"ws://127.0.0.1:7777\"]\n",
            dir.path(),
        );
        let unit = std::fs::read_to_string(dir.path().join("kuma-nostrd.service")).unwrap();
        // The declared set is the daemon's whole road now: the local
        // relay is opt-in, so a declaration that names two relays runs
        // exactly two.
        assert!(
            unit.contains(
                "ExecStart=/usr/bin/kuma-nostrd --relay wss://relay.nip46.com --relay ws://127.0.0.1:7777"
            ),
            "the declared relays are the daemon's road, in declaration order: {unit}"
        );
        assert!(unit.contains("ProtectSystem=strict"), "the sandbox is the point: {unit}");
        assert!(
            unit.contains("ExecStartPre=+/usr/bin/mkdir -p %h/.local/state/kuma-nostr"),
            "the state dir exists before the sandbox mounts, or ReadWritePaths \
             fails the namespace and the bunker never starts: {unit}"
        );
        assert!(
            unit.contains("Restart=always"),
            "a bunker that exits zero still stopped answering"
        );
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }

    /// The builder label carries `git describe`, so it moves with every
    /// commit — the one line of the Containerfile that is about the
    /// build rather than the declaration. Normalized on both sides, or
    /// no golden would survive the commit that lands it.
    fn normalize_builder(text: &str) -> String {
        text.lines()
            .map(|l| {
                if l.starts_with("LABEL io.kuma.builder=") {
                    "LABEL io.kuma.builder=<version-and-commit>"
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    /// The context side of the same contract. Sorted and hashed, not
    /// byte-ordered: write order is free and only the file set and
    /// contents are observable.
    #[test]
    fn the_staged_context_is_content_stable() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/containerfile/goldens");
        let mut regenerate = false;
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            regenerate = true;
        }
        for (name, toml) in GOLDEN_CASES {
            let staged = tempfile::tempdir().unwrap();
            context(toml, staged.path());
            let mut lines: Vec<String> = Vec::new();
            collect_hashes(staged.path(), staged.path(), &mut lines);
            let manifest = lines.join("\n") + "\n";
            let golden = dir.join(format!("{name}.manifest"));
            if regenerate {
                std::fs::write(&golden, &manifest).unwrap();
                continue;
            }
            let expected = std::fs::read_to_string(&golden).unwrap_or_default();
            assert_eq!(manifest, expected, "staging manifest for {name} moved");
        }
    }

    /// Recursive helper for the manifest: every non-dot file under
    /// `path`, as `relpath sha256`, in sorted order. The recipe itself
    /// is skipped — the text golden pins it, and its builder label
    /// moves with every commit.
    fn collect_hashes(root: &Path, path: &Path, lines: &mut Vec<String>) {
        use sha2::{Digest, Sha256};
        let mut entries: Vec<_> =
            std::fs::read_dir(path).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            if entry.file_name().unwrap().to_str().unwrap().starts_with('.') {
                continue;
            }
            if entry.is_dir() {
                collect_hashes(root, &entry, lines);
                continue;
            }
            if entry.file_name().unwrap() == "Containerfile" {
                continue;
            }
            let bytes = std::fs::read(&entry).unwrap();
            let hash = Sha256::digest(&bytes);
            let rel = entry.strip_prefix(root).unwrap();
            lines.push(format!("{} {}", rel.display(), hex(&hash)));
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The vendored emoji face is a 10 MB binary nobody diffs by eye, so
    /// the only honest review of a re-vendor is this pin refusing to
    /// match. It names upstream v2.048 (tag 20250623, commit
    /// c7a259fc809502bcb45d983f6a78f94dfceb1fbe), the newest build
    /// Fedora no longer packages: the packaged faces are monochrome or
    /// COLRv1, and swash paints neither's color glyphs — see
    /// NOTO_COLOR_EMOJI. Move the file and this pin in the same commit,
    /// and cite the upstream tag in the message; a font that changed
    /// without its provenance moving is a blob nobody can vouch for.
    #[test]
    fn the_vendored_emoji_face_is_the_pinned_upstream_build() {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(NOTO_COLOR_EMOJI);
        assert_eq!(
            hex(&hash),
            "3ed77810c203e1a67735dc19d395f32c23f2d7c0c3696690f4f78e15e57ab816",
            "assets/noto-emoji/NotoColorEmoji.ttf is not the pinned upstream \
             build: re-vendor deliberately and move this pin in the same commit"
        );
    }

    /// The declaration, the units, and the helpers all shipped while the
    /// binary that drives them did not, so an installed machine had no way
    /// to run the `kuma update` docs/agents.md promises it. Every image,
    /// not just VM disks: an ISO install has the same hole.
    #[test]
    fn every_image_ships_the_kuma_that_built_it() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains("COPY --chmod=755 kuma /usr/bin/kuma"));
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1", dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("kuma")).unwrap(),
            "not really a binary\n"
        );
    }

    /// An AppImage is a squashfs its runtime mounts over FUSE 2 before
    /// any of its own code runs, and Fedora ships only FUSE 3.
    ///
    /// Both packages named exactly, because each one alone fails and
    /// they fail differently: without `fuse-libs` the runtime cannot
    /// load libfuse.so.2, and without `fuse` the loaded library cannot
    /// exec the setuid `fusermount` it mounts with. Half of this fix
    /// looks like the whole of it right up until somebody runs an
    /// AppImage.
    #[test]
    fn appimages_run_on_every_image() {
        for toml in ["schema_version = 1", "schema_version = 1\n[system]\ndesktop = \"niri\""] {
            assert!(
                generate(&config(toml)).contains(&dnf_install("fuse fuse-libs")),
                "every image needs both halves of FUSE 2"
            );
        }
    }

    /// Plymouth is base layer: on an encrypted machine the LUKS prompt is
    /// drawn through plymouth, so a headless machine cannot decline it on
    /// behalf of an encrypted one. The four names ride one install line
    /// because each fails differently when missing: no daemon means no
    /// prompt at all, no script plugin means the theme silently falls
    /// back to plymouth's text plugin at boot (invisible in a build,
    /// obvious only on tty0), no label plugin means Image.Text() draws
    /// NOTHING while the machine still waits for the passphrase (the
    /// first eyes-on boot hung exactly there), and no font means the
    /// label plugin has nothing to draw with. Deleting any one of the
    /// four must fail this assert.
    #[test]
    fn every_image_carries_plymouth_and_its_two_quiets() {
        for toml in ["schema_version = 1", "schema_version = 1\n[system]\ndesktop = \"niri\""] {
            assert!(
                generate(&config(toml)).contains(&dnf_install(
                    "plymouth plymouth-plugin-script plymouth-plugin-label dejavu-sans-fonts"
                )),
                "plymouth, its script or label plugin, or its prompt font left the base set"
            );
        }
    }

    /// The theme lands in the image via COPY from staged context files;
    /// these assertions pin the four halves of that journey so any one
    /// breaking alone says which half it was:
    /// stage (write_context) → copy → default-theme symlink → dracut
    /// module named explicitly, and in ORDER, because the dracut drop-in
    /// after the theme changes nothing but reads better in review.
    #[test]
    fn the_spinner_theme_is_installed_and_made_default() {
        let out = generate(&config("schema_version = 1"));
        assert!(
            out.contains("COPY plymouth /usr/share/plymouth/themes/spinner_alt/"),
            "theme never copied into the image"
        );
        assert!(
            out.contains(
                "ln -sfn spinner_alt/spinner_alt.plymouth /usr/share/plymouth/themes/default.plymouth"
            ),
            "spinner_alt not made the default theme"
        );
        assert!(
            out.contains("'Theme=spinner_alt' > /etc/plymouth/plymouthd.conf"),
            "plymouthd.conf must name the theme: the packaged plymouthd.defaults (Theme=bgrt) \
             outranks the symlink, and populate-initrd resolves in that order"
        );
        assert!(
            out.contains("add_dracutmodules+=\" plymouth \""),
            "dracut would populate the initramfs by magic, not instruction"
        );
        // And the composition-time initramfs (which predates every conf
        // and theme file here) must be REBUILT in the build, AFTER every
        // input it consumes: the theme COPY and the symlink both. The
        // invocation carries its two container-only fixups, so a trimmed
        // version of it in a future edit fails here rather than at boot.
        let copy_at = out.find("COPY plymouth ").unwrap();
        let dracut_at = out
            .find("dracut --add-confdir \"$dconf\" --force --no-hostonly --add plymouth")
            .unwrap();
        let link_at = out.find("/usr/share/plymouth/themes/default.plymouth").unwrap();
        assert!(copy_at < dracut_at, "initramfs rebuilt before the theme exists");
        assert!(copy_at < link_at, "symlink before its target exists");
        assert!(
            out.contains("mkdir -p /var/roothome"),
            "without it, dracut dies recreating the dangling /root symlink"
        );
        assert!(
            out.contains("'sysloglvl=0' > \"$dconf/99-kuma-build.conf\""),
            "without it, every build prints a fatal-looking no-syslog error"
        );

        // Staging: build.rs embeds 63 files (60 frames + .plymouth +
        // .script + LICENSE); write_context writes all of them under
        // plymouth/. Counted exactly so a truncated vendoring (half the
        // frames synced) fails loudly instead of shipping a spinner that
        // spins three quarters of a turn.
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1", dir.path());
        let staged = dir.path().join("plymouth");
        let count = std::fs::read_dir(&staged).unwrap().count();
        assert_eq!(count, plymouth_theme::FILES.len(), "staged count drifted from embedded");
        assert_eq!(count, 63, "the vendored theme is not what upstream ships");
        assert!(staged.join("LICENSE").is_file(), "GPL terms must travel with the installed copy");
        // The animation loops mod 60; the script literally names it. And
        // the modification CREDITS.md records: the password prompt must
        // render the system-provided message (it names the disk being
        // unlocked) rather than upstream's hardcoded "Enter Password",
        // which tells nobody what the passphrase opens.
        let script = std::fs::read_to_string(staged.join("spinner_alt.script")).unwrap();
        assert!(script.contains("% 60"), "this is not the theme the .plymouth file promises");
        assert!(
            script.contains("Image.Text(prompt_text"),
            "the LUKS prompt stopped using the system-provided message"
        );
        assert!(
            !script.contains("Image.Text(\"Enter Password\""),
            "the hardcoded prompt text came back with a theme refresh"
        );
    }

    /// rhgb is what hands the console to plymouth for the graphical
    /// splash; without it encrypted machines still unlock (serial asks
    /// through systemd-ask-password-console), they just do it in text.
    /// So the staged kargs file carries rhgb desktop-gated exactly as
    /// quiet always was: headless keeps textual boots, a splash nobody
    /// can see buys nothing there.
    #[test]
    fn desktop_kargs_hand_the_console_to_plymouth() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1", dir.path());
        assert!(
            !dir.path().join("kargs-desktop.toml").exists(),
            "a headless image grew a desktop karg"
        );

        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"niri\"", dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("kargs-desktop.toml")).unwrap(),
            DESKTOP_KARGS
        );
        assert_eq!(DESKTOP_KARGS, "kargs = [\"quiet\", \"rhgb\"]\n");
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\""));
        assert!(out.contains("COPY kargs-desktop.toml /usr/lib/bootc/kargs.d/10-kuma-desktop.toml"));
    }

    /// The staged binary is the build host's, so a musl host or a foreign
    /// arch produces one the image cannot execute. Running it during the
    /// build is what makes that a build failure rather than a machine that
    /// boots with a kuma it cannot run, so the order is the whole point:
    /// a guard before its COPY proves nothing.
    #[test]
    fn the_staged_binary_is_proved_runnable_before_the_image_ships() {
        let out = generate(&config("schema_version = 1"));
        let copied = out.find("COPY --chmod=755 kuma /usr/bin/kuma").unwrap();
        let proved = out.find("RUN /usr/bin/kuma --version").unwrap();
        assert!(copied < proved, "the guard must run after the binary lands");
    }

    /// The image records which kuma generated it, so the probe can answer
    /// "is this image the one that has my last change in it". A label
    /// rather than the binary inside the image, because bare `kuma` is a
    /// cheap probe and already runs `podman image inspect`.
    #[test]
    fn the_image_records_which_kuma_built_it() {
        let out = generate(&config("schema_version = 1"));
        assert!(
            out.contains(&format!("LABEL io.kuma.builder=\"{}\"", crate::VERSION)),
            "no builder label in:\n{out}"
        );
    }

    #[test]
    fn image_carries_its_declaration_verbatim() {
        let toml = "schema_version = 1\n# a comment worth keeping\n";
        let out = generate(&config(toml));
        assert!(out.contains("COPY kuma.toml /usr/lib/kuma/kuma.toml"));
        let dir = tempfile::tempdir().unwrap();
        context(toml, dir.path());
        assert_eq!(std::fs::read_to_string(dir.path().join("kuma.toml")).unwrap(), toml);
    }

    #[test]
    fn minimal_config_is_base_boot_health_and_lint() {
        let out = generate(&config("schema_version = 1"));
        // The default base is kuma's own composed one, content-addressed
        // so the FROM is deterministic before any compose has run.
        assert!(out.contains("FROM localhost/kuma-base:m"));
        // Three dnf layers even in a minimal image, and all three are
        // promises rather than features: greenboot's never-worse-than-
        // before rollback, the FUSE 2 pair that lets a downloaded
        // AppImage run without a declaration naming it, and plymouth
        // drawing the LUKS prompt on any encrypted machine this image
        // ever becomes. Everything else is opt-in, and the count is what
        // keeps it that way.
        assert_eq!(out.matches("dnf -y install").count(), 3);
        assert!(out.contains(&dnf_install("greenboot")));
        assert!(out.contains(&dnf_install("fuse fuse-libs")));
        assert!(out.contains(&dnf_install(
            "plymouth plymouth-plugin-script plymouth-plugin-label dejavu-sans-fonts"
        )));
        assert!(out.contains("bootc container lint"));
        // The lint runs unqualified first: the --skip is a fallback for
        // one upstream crash, never the path a healthy build takes, and
        // it must not swallow any other lint failure. Pin the shape so a
        // future edit can't turn it into a blanket skip.
        assert!(out.contains("RUN said=$(bootc container lint 2>&1); rc=$?;"));
        assert!(out.contains("grep -q 'var-tmpfiles: I/O error'"));
        assert!(out.contains("bootc container lint --skip var-tmpfiles || rc=$?"));
        // the build's exit status is still the lint's
        assert!(out.contains("exit $rc"));
        // Nothing on disk: a scratch file here is litter the lint itself
        // reads back and reports, since /tmp is one of the directories
        // it checks.
        assert!(!out.contains("lint.err"));
    }

    /// Every image was shipping dnf's log and a /run full of state that a
    /// booted machine creates for itself. Two bootc lint warnings said so
    /// on every build, and warnings cannot fail a build, so they were
    /// noise nobody had to act on.
    #[test]
    fn build_litter_is_swept_before_the_image_is_judged() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains("find /run /tmp -mindepth 1 -delete"));
        assert!(out.contains("find /var/log -type f -delete"));

        // The sweep has to precede the lint, or the lint passes judgement
        // on an image that is not the one being shipped.
        let sweep = out.find("find /run /tmp -mindepth 1 -delete").unwrap();
        let lint = out.find("RUN said=$(bootc container lint").unwrap();
        assert!(sweep < lint);

        // The logs are asserted rather than assumed. /run cannot be: the
        // build holds two paths inside it open, so nothing there can be
        // checked for emptiness from within the build itself.
        assert!(out.contains("! find /var/log -type f | grep -q ."));
    }

    #[test]
    fn full_config_generates_all_sections() {
        let out = generate(&config(
            r#"
            schema_version = 1
            [system]
            base = "ghcr.io/example/kuma-gnome:latest"
            [packages]
            rpm = ["fish", "tailscale"]
            [services]
            enable = ["tailscaled.service"]
            disable = ["cups.service"]
            "#,
        ));
        assert!(out.contains("FROM ghcr.io/example/kuma-gnome:latest"));
        assert!(out.contains(&dnf_install("fish tailscale")));
        assert!(out.contains("systemctl enable tailscaled.service"));
        assert!(out.contains("systemctl disable cups.service"));
    }

    #[test]
    fn niri_desktop_generates_curated_layer() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("niri"));
        assert!(out.contains("greetd"));
        assert!(out.contains("NetworkManager-wifi"));
        // full GPU acceleration: RADV for Vulkan, freeworld VA-API for
        // the codecs Fedora's build strips
        assert!(out.contains("mesa-vulkan-drivers"));
        assert!(out.contains("rpmfusion-free-release"));
        assert!(out.contains("--allowerasing mesa-va-drivers-freeworld"));
        // kuma-files' video previews decode through libav: rpmfusion's
        // full build swaps in over the stripped free one
        assert!(out.contains("--allowerasing ffmpeg-libs"));
        assert!(out.contains("COPY greetd-config.toml /etc/greetd/config.toml"));
        assert!(out.contains("niri validate --config /etc/niri/config.kdl"));
        assert!(out.contains("systemctl set-default graphical.target"));
        assert!(out.contains("greetd.service firewalld.service power-profiles-daemon.service"));
        assert!(out.contains("mask flatpak-add-fedora-repos.service"));
        // bare import-environment is deprecated; the patched call must keep
        // the session vars niri.service needs to claim the logind seat
        assert!(out.contains("import-environment PATH XDG_SESSION_ID XDG_SEAT XDG_VTNR"));
    }

    #[test]
    fn branding_always_applied() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains("NAME=\"kumaOS\""));
        assert!(out.contains("ID=kuma"));
        assert!(out.contains("ID_LIKE=\\\"fedora\\\"") || out.contains("ID_LIKE=\"fedora\""));
        // branding must come after every dnf layer
        let brand_at = out.find("NAME=\"kumaOS\"").unwrap();
        assert!(out.rfind("dnf -y install").is_none_or(|dnf_at| dnf_at < brand_at));
    }

    #[test]
    fn niri_desktop_ships_theme_and_wallpaper() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(
            out.contains("COPY kuma-wallpaper.jpg /usr/share/backgrounds/kuma/kuma-wallpaper.jpg")
        );
        // The shell's terminal config, in place of waybar's two files
        // and mako's one.
        assert!(out.contains("COPY kitty.conf /etc/xdg/kitty/kitty.conf"));
        // system-wide, never /etc/skel — skel strands existing homes on
        // stale copies (the fuzzel-DPI lesson)
        assert!(!out.contains("/etc/skel"));
        // an unparseable theme must fail the build, not ship unthemed —
        // and unknown keys only ever reach stderr, so both halves matter
        assert!(out.contains("kitty +runpy"));
        assert!(out.contains("accumulate_bad_lines=bad"));
        assert!(out.contains("grep -q 'unknown config key' /tmp/kitty.err"));
        // The palette owns every colour the terminal shows, all sixteen
        // ANSI slots included, and it is STATIC now: the wallpaper-
        // derived render died with noctalia, so the palette's presence
        // in the shipped file is the whole story.
        assert!(KITTY_CONFIG.contains("background"));
        assert!(KITTY_CONFIG.contains("color0"));
        assert!(KITTY_CONFIG.contains("color15"));
        // and no template placeholder survived into the static file
        assert!(!KITTY_CONFIG.contains("{{"));
        // the GTK3 half only themes anything with adw-gtk3 present, and
        // GTK_THEME outranks gsettings, so all four names move together
        assert!(NIRI_PACKAGES.contains(&"adw-gtk3-theme"));
        assert!(NIRI_EXTRAS.contains("GTK_THEME \"adw-gtk3-dark\""));
        assert!(DCONF_DARK.contains("gtk-theme='adw-gtk3-dark'"));
        assert!(XSETTINGSD_CONF.contains("Net/ThemeName \"adw-gtk3-dark\""));
        assert!(GTK3_SETTINGS_INI.contains("gtk-theme-name = adw-gtk3-dark"));
        assert!(out.contains("RUN test -d /usr/share/themes/adw-gtk3-dark"));
        // the focus ring must stay an outline AROUND the window: niri's
        // default paints it as a filled rect behind the window, and a
        // translucent surface composites over that fill instead of the
        // wallpaper (issue #31)
        assert!(NIRI_EXTRAS.contains("draw-border-with-background false"));
        // upstream niri spawns alacritty; the image ships kitty, so the sed
        // must rewrite the bind, and the grep guard must keep it honest
        assert!(out.contains("grep -q '\"alacritty\"' /usr/share/doc/niri/default-config.kdl"));
        assert!(out.contains("sed -e 's/alacritty/kitty/g'"));
        // niri Recommends alacritty; without the exclude it ships anyway
        for excluded in NIRI_EXCLUDES {
            assert!(out.contains(&format!("--exclude={excluded}")), "{excluded} rides in");
            assert!(!NIRI_PACKAGES.contains(excluded), "{excluded} is both excluded and asked for");
        }
        assert!(out.contains("COPY dconf-profile /etc/dconf/profile/user"));
        assert!(out.contains("COPY dconf-kuma-dark /etc/dconf/db/local.d/10-kuma-dark"));
        assert!(out.contains("COPY dconf-kuma-blueman /etc/dconf/db/local.d/10-kuma-blueman"));
        assert!(out.contains("RUN dconf update"));
        // Both names, because disabling only StatusIcon is a no-op:
        // ShowConnected depends on it and the plugin manager loads a
        // dependency regardless of the disable flag. Dropping either name
        // brings the second bluetooth icon back, and nothing about the
        // bar would fail a test to say so.
        assert!(DCONF_BLUEMAN.contains("'!StatusIcon'"));
        assert!(DCONF_BLUEMAN.contains("'!ShowConnected'"));

        // Exactly one launch path for the two session services this image
        // both spawns and ships an autostart entry for. The pairing is
        // the point: an override here without the matching
        // spawn-at-startup would leave the machine with no bluetooth
        // agent and no polkit agent, and the second of those is silent
        // until somebody needs a password prompt.
        assert!(out.contains("COPY autostart-blueman /etc/xdg/autostart/blueman.desktop"));
        assert!(out.contains("/etc/xdg/autostart/polkit-mate-authentication-agent-1.desktop"));
        assert!(NIRI_EXTRAS.contains("spawn-at-startup \"blueman-applet\""));
        assert!(NIRI_EXTRAS.contains("polkit-mate-authentication-agent-1"));
        assert!(autostart_off("x").contains("Hidden=true"));
    }

    #[test]
    fn desktop_defaults_to_dark_and_bare_terminal() {
        assert!(DCONF_DARK.contains("color-scheme='prefer-dark'"));
        // a titlebar in a tiling compositor renders light Adwaita chrome
        assert!(KITTY_CONFIG.contains("hide_window_decorations yes"));
    }

    #[test]
    fn vm_timezone_adoption_ships_in_every_image() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains("RUN systemctl enable kuma-vm-timezone.service"));
        assert!(VM_TZ_SCRIPT.contains("qemu_fw_cfg/by_name/opt/org.kuma.tz"));
        // guard against a garbage or hostile fw_cfg value
        assert!(VM_TZ_SCRIPT.contains("[ -e \"/usr/share/zoneinfo/$tz\" ] || exit 0"));
    }

    /// sshd was enabled on every kuma machine by Fedora's RPM preset
    /// rather than by anything here, which made a listening network
    /// service the one part of the image kuma had never decided. Naming
    /// it changed no behaviour and pins it against a preset flip
    /// upstream, in either direction: a base that stopped enabling sshd
    /// would otherwise take `kuma vm` and the boot smoke stage with it.
    #[test]
    fn sshd_is_enabled_by_name_in_every_image() {
        for declaration in [
            "schema_version = 1",
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n",
            "schema_version = 1\n[system]\ndesktop = \"cosmic\"\n",
        ] {
            let out = generate(&config(declaration));
            assert!(
                out.contains("RUN systemctl enable sshd.service"),
                "sshd not named for: {declaration}"
            );
        }
    }

    /// A default, not a floor. The declaration's own [services] block is
    /// emitted after kuma's curated enables and before the units that
    /// cannot be turned off, so an owner who disables sshd gets a machine
    /// without it. Move the enable below that block and the disable
    /// becomes a silent no-op, which is the shape of bug that made the
    /// example's `disable` line fight the desktop.
    #[test]
    fn a_declared_disable_beats_the_sshd_default() {
        let out =
            generate(&config("schema_version = 1\n[services]\ndisable = [\"sshd.service\"]\n"));
        let enable = out.find("systemctl enable sshd.service").expect("sshd is enabled by name");
        let disable = out.find("systemctl disable sshd.service").expect("the declared disable");
        assert!(enable < disable, "kuma's default must not outrank the declaration");

        // The floor stays the floor: these are emitted after [services]
        // precisely so a declaration cannot switch them off.
        for floor in ["greenboot-healthcheck.service", "fwupd-refresh.timer"] {
            let at = out.find(floor).expect(floor);
            assert!(at > disable, "{floor} drifted above the declaration's [services]");
        }
    }

    /// The hardening floor is in every image, and it is a floor: emitted
    /// after the declaration's [services] block, where nothing an owner
    /// writes can reach it. secureblue's audited sysctl set, the kargs
    /// half, the firewalld zone that ends the world's route to sshd, NTS
    /// time, the Wi-Fi MAC conf, and faillock. The sshd story it depends
    /// on is the test above's: the unit stays enabled, and the zone's
    /// slirp rule is the lane the gate reaches the guest through.
    #[test]
    fn hardening_floor_is_in_every_image() {
        for declaration in [
            "schema_version = 1",
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n",
            "schema_version = 1\n[system]\ndesktop = \"cosmic\"\n",
        ] {
            let out = generate(&config(declaration));
            for named in [
                "COPY hardening-sysctl.conf /usr/lib/sysctl.d/70-kuma-hardening.conf",
                "COPY kargs-hardening.toml /usr/lib/bootc/kargs.d/05-kuma-hardening.toml",
                "COPY public.zone.xml /etc/firewalld/zones/public.xml",
                "COPY chrony.conf /etc/chrony.conf",
                "COPY kuma-mac.conf /etc/NetworkManager/conf.d/kuma-mac.conf",
                "COPY faillock.conf /etc/security/faillock.conf",
                "authselect enable-feature with-faillock",
            ] {
                assert!(out.contains(named), "{named} missing for: {declaration}");
            }
        }
    }

    /// The zone carries the lane, not the world. The distinction is
    /// indent-shaped and that is the point: a top-level `<service
    /// name="ssh"/>` is the world's route back in and exactly the line
    /// this block exists to remove, while the same element nested in the
    /// rich rule is the slirp lane. The strings differ by indentation
    /// only, so both halves are pinned against a reformat that would
    /// quietly turn one into the other.
    #[test]
    fn the_zone_serves_the_lane_and_not_the_world() {
        assert!(FIREWALLD_PUBLIC_ZONE.contains("10.0.2.2/32"), "the slirp lane");
        assert!(
            !FIREWALLD_PUBLIC_ZONE.contains("\n  <service name=\"ssh\"/>"),
            "ssh is a top-level service again — the world's route is back"
        );
        assert!(
            FIREWALLD_PUBLIC_ZONE.contains("\n    <service name=\"ssh\"/>"),
            "the rule's own ssh element vanished — the gate's lane went with it"
        );
        // The values that motivated the whole change, pinned at the
        // const so a well-meant renumber cannot hollow the floor.
        for line in [
            "kernel.yama.ptrace_scope = 1",
            "kernel.kptr_restrict = 2",
            "kernel.perf_event_paranoid = 3",
            "kernel.kexec_load_disabled = 1",
            "kernel.printk = 3 3 3 3",
            "net.ipv4.icmp_echo_ignore_all = 1",
        ] {
            assert!(HARDENING_SYSCTL.contains(line), "sysctl floor lost: {line}");
        }
    }

    #[test]
    fn boot_health_ships_in_every_image() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains(&dnf_install("greenboot")));
        assert!(out.contains(
            "RUN systemctl enable greenboot-healthcheck.service greenboot-set-rollback-trigger.service greenboot-success.target kuma-boot-health-sync.service"
        ));
        assert!(out
            .contains("COPY --chmod=755 kuma-boot-health-sync /usr/libexec/kuma-boot-health-sync"));
        // the IoT subpackage's *required* DNS probe would roll back a
        // laptop that boots offline
        assert!(!out.contains("greenboot-default-health-checks"));
        // no desktop, no greeter to check
        assert!(!out.contains("kuma-greeter-check"));
    }

    /// Every image, because every image's menu goes stale the same way:
    /// the titles drift on any deploy that reuses the kernel, which is
    /// every kuma deploy that does not move the base.
    #[test]
    fn boot_titles_ship_in_every_image() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains(
            "COPY kuma-boot-titles.service /usr/lib/systemd/system/kuma-boot-titles.service"
        ));
        // The enable line itself, not a bare `contains`: the COPY above
        // ends on exactly that file name, so `contains` was satisfied
        // by the line before it and dropping the unit from the enable
        // would have left this test green.
        assert!(
            out.lines().any(|line| {
                line.starts_with("RUN systemctl enable")
                    && line.contains("kuma-boot-titles.service")
            }),
            "and is enabled"
        );
        // The unit calls the binary the image ships, so the image has to
        // ship one.
        assert!(out.contains("COPY --chmod=755 kuma /usr/bin/kuma"));
    }

    #[test]
    fn boot_health_sync_converges_the_grub_hook() {
        // The heredoc block must carry the exact markers the script
        // greps and strips by — a drifted marker means the sync writes
        // a block it can never find again (double-append forever).
        let begin = "# >>> kuma boot-health >>>";
        let end = "# <<< kuma boot-health <<<";
        assert!(BOOT_HEALTH_SYNC_SCRIPT.contains(&format!("begin='{begin}'")));
        assert!(BOOT_HEALTH_SYNC_SCRIPT.contains(&format!("end='{end}'")));
        assert_eq!(BOOT_HEALTH_SYNC_SCRIPT.matches(begin).count(), 2);
        assert_eq!(BOOT_HEALTH_SYNC_SCRIPT.matches(end).count(), 2);
        // the fallback needs grub's increment module and must remove
        // itself when grub.cfg counts natively (no double decrement)
        assert!(BOOT_HEALTH_SYNC_SCRIPT.contains("insmod increment"));
        assert!(BOOT_HEALTH_SYNC_SCRIPT.contains("grep -q boot_counter \"$cfg\""));
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n", dir.path());
        let script = std::fs::read_to_string(dir.path().join("kuma-boot-health-sync")).unwrap();
        assert!(script.starts_with("#!/usr/bin/bash"));
    }

    /// The cure for the fstab wart has to travel in the image. It was
    /// fixed by hand on one laptop in August 2026 and that fixed exactly
    /// one machine: nothing in a kuma image touched /etc/fstab, the bootc
    /// rpm ships no unit for it, and the ISO kickstart never mentions it,
    /// so every fresh install went on failing systemd-remount-fs forever.
    #[test]
    fn every_image_carries_the_fstab_converger() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains("COPY --chmod=755 kuma-fstab-sync /usr/libexec/kuma-fstab-sync"));
        assert!(out.contains("kuma-fstab-sync.service"));

        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n", dir.path());
        let script = std::fs::read_to_string(dir.path().join("kuma-fstab-sync")).unwrap();
        assert!(script.starts_with("#!/usr/bin/bash"));

        // Three properties the edit is only safe because of, each of which
        // reads like a detail and is the whole guard:
        //
        // it asks the kernel what / actually is instead of assuming any
        // bootc machine is composefs,
        assert!(script.contains("findmnt -n -o FSTYPE /"));
        // it matches the mount point as a FIELD, because "root" appears in
        // subvolume names and /var/roothome is a real mount,
        assert!(script.contains(r#"$1 !~ /^#/ && $2 == "/""#));
        // and it writes back through the existing inode, so the file keeps
        // its mode and SELinux label instead of inheriting a temp file's.
        assert!(script.contains(r#"cat "$new" > "$fstab""#));

        // The unit is inert anywhere that is not an ostree deployment,
        // since on a package system that fstab line is load-bearing.
        assert!(FSTAB_SYNC_SERVICE.contains("ConditionPathExists=/run/ostree-booted"));
    }

    /// The one line that decides whether the boot-menu titles are ever
    /// right, stated as a test because it reads backwards and is
    /// therefore exactly the kind of thing a later reader "fixes".
    ///
    /// The rotation happens in ostree's ExecStop at shutdown. systemd
    /// stops in reverse start order, so this unit runs after that
    /// rotation only while it is ordered BEFORE finalize-staged.
    /// Flipping it to After= makes the unit start later, stop earlier,
    /// and write the titles of an arrangement that is about to change:
    /// green, quiet, and useless.
    #[test]
    fn boot_titles_runs_after_the_rotation_it_follows() {
        assert!(BOOT_TITLES_SERVICE.contains("Before=ostree-finalize-staged.service"));
        assert!(
            !BOOT_TITLES_SERVICE.contains("After=ostree-finalize-staged.service"),
            "After= would order this before the rotation, not after it"
        );
        // Inside the hold unit's window, so /boot is still mounted when
        // the ExecStop pass writes to it.
        assert!(BOOT_TITLES_SERVICE.contains("After=ostree-finalize-staged-hold.service"));
        assert!(BOOT_TITLES_SERVICE.contains("RequiresMountsFor=/boot"));
        // The late shutdown slot. Without DefaultDependencies=no the
        // implicit Before=shutdown.target contradicts stopping after a
        // unit that stops at final.target, and systemd drops one of the
        // two rules silently.
        assert!(BOOT_TITLES_SERVICE.contains("DefaultDependencies=no"));
        assert!(BOOT_TITLES_SERVICE.contains("Conflicts=final.target"));
        // Both passes: the shutdown one is the point, the boot one
        // covers a machine that lost power before finishing a shutdown.
        assert!(BOOT_TITLES_SERVICE.contains("ExecStop=/usr/bin/kuma boot-titles"));
        assert!(BOOT_TITLES_SERVICE.contains("ExecStart=/usr/bin/kuma boot-titles"));
        assert!(BOOT_TITLES_SERVICE.contains("ConditionPathExists=/run/ostree-booted"));
    }

    /// The window for making /var/home a subvolume is one boot wide, and
    /// the guards are what keep it from being a way to lose a home
    /// directory. Run rather than only read: the branch that does the
    /// work needs btrfs, but every branch that refuses to can be
    /// exercised here, and those are the ones that matter.
    #[test]
    fn the_home_subvolume_converger_refuses_everything_it_should() {
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains("COPY --chmod=755 kuma-home-subvol /usr/libexec/kuma-home-subvol"));
        assert!(out.contains("RUN systemctl enable kuma-home-subvol.service"));
        // Before anything can create a home directory in it, which is
        // what makes "only while empty" a check that ever passes.
        assert!(HOME_SUBVOL_SERVICE.contains("ConditionPathExists=/run/ostree-booted"));
        // The early slot, which is what closes the window rather than
        // enumerating who might fall into it. Every one of these lines is
        // load-bearing: without DefaultDependencies=no the unit cannot be
        // ordered before sysinit.target at all, without the tmpfiles
        // ordering /var/home may not exist yet, and without
        // RequiresMountsFor the target may not be on the filesystem this
        // is about to convert.
        for line in [
            "DefaultDependencies=no",
            "RequiresMountsFor=/var",
            "After=systemd-tmpfiles-setup.service",
            "Before=sysinit.target",
            "WantedBy=sysinit.target",
        ] {
            assert!(
                HOME_SUBVOL_SERVICE.contains(line),
                "{line} is what keeps a sandboxed unit from racing the converger"
            );
        }
        // Ordering against individual writers was the wrong shape and is
        // not to be reintroduced: the units that lose this race do not
        // write to /var/home, systemd binds it for them, and there are
        // twenty five of them on a desktop image.
        assert!(!HOME_SUBVOL_SERVICE.contains("Before=kuma-user-sync.service"));

        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n", dir.path());
        let script = dir.path().join("kuma-home-subvol");
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let run = |arg: &std::path::Path| {
            std::process::Command::new("bash")
                .arg(&script)
                .arg(arg)
                .output()
                .expect("run the converger")
        };

        // A path that does not exist is not a fault: tmpfiles makes
        // /var/home at boot, and this can be ordered before it.
        let missing = dir.path().join("nothing-here");
        assert!(run(&missing).status.success());

        // A target that exists on a filesystem that is not btrfs. This is
        // the branch every test machine takes, and the one that must not
        // rmdir anything: the tempdir here is whatever the test host runs.
        let plain = dir.path().join("home");
        std::fs::create_dir(&plain).unwrap();
        std::fs::write(plain.join("someone-lives-here"), "notes\n").unwrap();
        let out = run(&plain);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(plain.join("someone-lives-here").exists(), "it must never delete a home");
        assert!(plain.is_dir());
        // Declining has to say so. A silent exit 0 here is
        // indistinguishable from the first boot where this was supposed
        // to act and did not, which is exactly the case that went
        // unexplained: the unit succeeds either way and the only
        // difference is an inode nobody reads.
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("kuma-home-subvol:"),
            "declining must log a reason, got: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );

        // And the order of the guards: emptiness is checked before
        // anything is removed, on the same line of reasoning.
        let body = std::fs::read_to_string(&script).unwrap();
        let empty_at = body.find(r#"[ -n "$(ls -A "$target")" ]"#).unwrap();
        let rmdir_at = body.find(r#"rmdir "$target""#).unwrap();
        assert!(empty_at < rmdir_at);
    }

    #[test]
    fn greeter_check_guards_every_desktop() {
        for desktop in ["niri", "cosmic"] {
            let out = generate(&config(&format!(
                "schema_version = 1\n[system]\ndesktop = \"{desktop}\"\n"
            )));
            assert!(out.contains(
                "COPY --chmod=755 kuma-greeter-check /usr/lib/greenboot/check/required.d/50-kuma-greeter.sh"
            ));
        }
        // the check must poll the greeter unit, never graphical.target —
        // graphical waits for multi-user, which waits for this check
        assert!(GREETER_CHECK.contains("is-active display-manager.service"));
        assert!(!GREETER_CHECK.contains("is-active graphical.target"));
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n", dir.path());
        let script = std::fs::read_to_string(dir.path().join("kuma-greeter-check")).unwrap();
        assert!(script.starts_with("#!/usr/bin/bash"));
    }

    /// `kuma-launch` is valid bash. Every script kuma embeds has shipped
    /// unchecked at least once, and CI only shellchecks `scripts/`.
    /// **No root-run script sources a credential file, and this is the
    /// grep rather than the fix.**
    ///
    /// The restore unit ran `. /var/lib/kuma/secrets/restore.env` as
    /// root on first boot, so `RESTIC_PASSWORD=$(curl ...|sh)` executed
    /// with everything. That file is not the operator's own private
    /// state either: concepts.md tells people to put it on the stick
    /// beside the ISO, so it is designed to travel between machines and
    /// hands.
    ///
    /// Measured rather than argued, because the difference is the whole
    /// finding: reading `A=$(id -u)` through a read-and-export loop
    /// yields the literal `$(id -u)`, and sourcing the same line yields
    /// `1000`. systemd's EnvironmentFile= does the first.
    #[test]
    fn no_baked_script_sources_a_credential_file() {
        for (name, script) in [
            ("kuma-restore", RESTORE_SCRIPT),
            ("kuma-backup", BACKUP_SCRIPT),
            ("kuma-snapshot", SNAPSHOT_SCRIPT),
            ("kuma-user-sync", USER_SYNC_SCRIPT),
        ] {
            for line in script.lines().map(str::trim) {
                let sources = line.starts_with(". ") || line.starts_with("source ");
                assert!(
                    !(sources && line.contains("secret")),
                    "{name} sources a credential file: {line}"
                );
            }
        }
    }

    /// The sleep guard fires, which is the half a guard usually fails.
    ///
    /// Both paths were run against a booted machine before this was
    /// written: with the shell running it exits 0 silently, and with the
    /// process name changed to one that does not exist it names the
    /// session and reaches the terminate. A guard that cannot fire is
    /// the failure this project has already shipped once (the
    /// mounted-image check in install.rs), so this one was proved
    /// firing rather than assumed.
    #[test]
    fn the_sleep_guard_parses_and_asks_the_right_questions() {
        let out = std::process::Command::new("bash")
            .args(["-n", "/dev/stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(SLEEP_GUARD.as_bytes())?;
                child.wait_with_output()
            })
            .expect("bash -n");
        assert!(out.status.success(), "sleep guard is not valid shell");
        // Only where kuma put a shell, and only for a real session.
        assert!(SLEEP_GUARD.contains("/etc/niri/config.kdl"), "{SLEEP_GUARD}");
        assert!(SLEEP_GUARD.contains("seat0"), "{SLEEP_GUARD}");
        // The property: no shell means the session ends rather than the
        // machine sleeping with the desktop on screen.
        assert!(SLEEP_GUARD.contains("pgrep -u \"$user\" -x kuma-shell"), "{SLEEP_GUARD}");
        assert!(SLEEP_GUARD.contains("loginctl terminate-session"), "{SLEEP_GUARD}");
        // The terminate's tail: the session's units are ended with the
        // session, or a greeter login races their teardown and buys
        // niri-session's "A niri session is already running." — and
        // bounded, so the stop cannot stall the sleep it runs under.
        assert!(SLEEP_GUARD.contains("stop graphical-session.target"), "{SLEEP_GUARD}");
        assert!(SLEEP_GUARD.contains("timeout 15"), "{SLEEP_GUARD}");
        // The bus probe that noctalia's guard carried is gone with it:
        // kuma-shell owns no session-bus name, so there is nothing to
        // ask, and a guard that cannot ask must not guess.
        assert!(!SLEEP_GUARD.contains("org.freedesktop.DBus.Peer"), "{SLEEP_GUARD}");
        // And it runs on the way down, on every path into sleep.
        assert!(SLEEP_GUARD_SERVICE.contains("Before=sleep.target"), "{SLEEP_GUARD_SERVICE}");
        assert!(SLEEP_GUARD_SERVICE.contains("WantedBy=sleep.target"), "{SLEEP_GUARD_SERVICE}");
    }

    /// The unit carries what the shell needs and nothing it cannot use.
    ///
    /// 0.17's lesson, kept as a shape: a unit inherits nothing from
    /// niri's `environment` block, so every variable the shell needs is
    /// asserted in the unit AND in the niri block, because two places
    /// holding one value drift silently. The kuma-shell needs only the
    /// cursor pair — its defaults are compiled in — and it must run the
    /// binary the context stages, not some leftover.
    #[test]
    fn the_shell_unit_carries_the_shells_environment() {
        for var in ["XCURSOR_THEME=Adwaita", "XCURSOR_SIZE=24"] {
            assert!(
                SHELL_SERVICE.contains(&format!("Environment={var}")),
                "the shell unit does not set {var}, so the session will not:\n{SHELL_SERVICE}"
            );
            // Same value on both sides of the seam. The niri block
            // states them as `NAME "value"`.
            let (name, value) = var.split_once('=').unwrap();
            assert!(
                NIRI_EXTRAS.contains(&format!("{name} \"{value}\"")),
                "{name} disagrees between the unit and niri's environment block"
            );
        }
        // The flip's core: the unit starts kuma-shell, and no
        // noctalia variable survives anywhere in either file — a
        // variable pointing at a config directory nothing reads is
        // 0.17's bug wearing today's clothes.
        assert!(SHELL_SERVICE.contains("ExecStart=/usr/bin/kuma-shell"), "{SHELL_SERVICE}");
        assert!(!SHELL_SERVICE.contains("noctalia"), "{SHELL_SERVICE}");
        assert!(!NIRI_EXTRAS.contains("NOCTALIA_"), "{NIRI_EXTRAS}");
    }

    /// The lock screen authenticates under its own name.
    ///
    /// The shell's chain can only skip a service whose /etc/pam.d file
    /// is absent — a missing service and a wrong password are the same
    /// answer from Linux-PAM — so the image ships the chain's first
    /// entry. The test pins both halves: the bytes (the locker stack,
    /// vlock's shape) and the destination (/etc/pam.d, the one
    /// directory the shell's existence check reads — /usr/lib/pam.d,
    /// where Fedora vendors stacks, is invisible to the chain).
    #[test]
    fn the_lock_screen_has_its_own_pam_service() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"niri\"\n", dir.path());
        let staged = std::fs::read_to_string(dir.path().join("kuma-lock")).unwrap();
        assert_eq!(staged, KUMA_LOCK_PAM, "the staged stack is the locker shape, byte for byte");
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(
            out.contains("COPY kuma-lock /etc/pam.d/kuma-lock"),
            "the file lands where the shell's existence check reads it:\n{out}"
        );
        // Scoped to the desktop that runs the shell: a COSMIC image has
        // no kuma-lock chain to feed, and the file would be an unused
        // service name answering from the `other` policy — the exact
        // thing the chain skips.
        let cosmic = generate(&config("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"));
        assert!(
            !cosmic.contains("kuma-lock"),
            "the PAM service rides the shell's desktop, not the image:\n{cosmic}"
        );
    }

    #[test]
    fn kuma_launch_parses_as_shell() {
        use std::io::Write;
        let mut child = std::process::Command::new("bash")
            .args(["-n", "/dev/stdin"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(KUMA_LAUNCH.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// The verb reaches the terminal as separate arguments.
    ///
    /// This is the whole reason the held script takes `"$@"` instead of a
    /// command pasted into a string: `kuma snapshot restore /home/a b`
    /// assembled by concatenation is a different command than the one
    /// asked for, and the failure is silent for every path without a
    /// space in it. A fake terminal that prints its own argv is the only
    /// way to see the boundaries.
    #[test]
    fn kuma_launch_passes_the_verb_as_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        // A fake kitty that prints each argument on its own line, so the
        // boundaries between them are visible in the output.
        std::fs::write(
            bin.join("kitty"),
            "#!/usr/bin/bash
for a in \"$@\"; do printf '%s\\n' \"$a\"; done
",
        )
        .unwrap();
        let launch = dir.path().join("kuma-launch");
        std::fs::write(&launch, KUMA_LAUNCH).unwrap();
        for path in [bin.join("kitty"), launch.clone()] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // CI's runner sporadically answers the exec of a just-written
        // script with ETXTBSY: measured 2026-08-22, 380 tests green and
        // this one red on a tree identical to a passing run. A moment
        // later the same exec answers fine, so ask a few times rather
        // than flake the suite on the filesystem's bookkeeping.
        let mut out = None;
        for _ in 0..5 {
            match std::process::Command::new(&launch)
                .args(["snapshot", "restore", "/home/a b"])
                .env("PATH", bin.to_str().unwrap())
                .output()
            {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                other => {
                    out = Some(other.unwrap());
                    break;
                }
            }
        }
        let out = out.expect("five ETXTBSY in a row is a broken box, not a flake");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let lines: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap().lines().collect();
        assert_eq!(&lines[..3], ["-e", "/usr/bin/bash", "-lc"], "{lines:?}");
        // The held script is one argument spanning several printed lines;
        // $0 and the command follow it, and it is their boundaries this
        // test is about.
        assert_eq!(
            &lines[lines.len() - 5..],
            ["kuma-launch", "kuma", "snapshot", "restore", "/home/a b"],
            "{lines:?}"
        );
    }

    /// No terminal in the image is an error that says so, not a launcher
    /// entry that does nothing when clicked.
    #[test]
    fn kuma_launch_says_when_there_is_no_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("bin");
        std::fs::create_dir(&empty).unwrap();
        let launch = dir.path().join("kuma-launch");
        std::fs::write(&launch, KUMA_LAUNCH).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launch, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new(&launch)
            .arg("doctor")
            .env("PATH", empty.to_str().unwrap())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("no terminal"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Called with nothing, it says how to call it rather than opening a
    /// terminal that runs a bare `kuma`.
    #[test]
    fn kuma_launch_refuses_an_empty_call() {
        let dir = tempfile::tempdir().unwrap();
        let launch = dir.path().join("kuma-launch");
        std::fs::write(&launch, KUMA_LAUNCH).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launch, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new(&launch).output().unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage:"));
    }

    /// The entries and the wrapper reach both desktops, which is the seam
    /// being tested rather than asserted: `kuma menu` was niri-only
    /// because fuzzel was, and a replacement that inherits that is not a
    /// replacement.
    #[test]
    fn the_seam_is_on_every_desktop() {
        for desktop in ["niri", "cosmic"] {
            let out = generate(&config(&format!(
                "schema_version = 1\n[system]\ndesktop = \"{desktop}\""
            )));
            assert!(
                out.contains("COPY --chmod=755 kuma-launch /usr/libexec/kuma-launch"),
                "{desktop} has no wrapper"
            );
            for entry in seam::ENTRIES {
                assert!(
                    out.contains(&format!("COPY {}.desktop {}", entry.id, seam::path(entry))),
                    "{desktop} is missing {}",
                    entry.id
                );
            }
            assert!(out.contains("RUN desktop-file-validate "), "{desktop} does not validate");
        }
    }

    /// And not on a machine with no desktop, which has no launcher to put
    /// them in.
    #[test]
    fn the_seam_is_absent_without_a_desktop() {
        let out = generate(&config("schema_version = 1"));
        assert!(!out.contains("kuma-launch"), "a headless image has no launcher");
    }

    /// A greeter that starts, dies, and is restarted is not a greeter.
    ///
    /// This was found on a real installed machine: greenboot reached its
    /// success target at 15.8s while greetd was on restart 3 of 5, and
    /// gave up at 19.5s. Nobody could log in and boot health said green,
    /// which is the one outcome this check exists to prevent.
    #[test]
    fn the_greeter_check_fails_a_crash_looping_greeter() {
        // Sampled twice with a gap, so one lucky retry cannot pass it.
        assert_eq!(GREETER_CHECK.matches("is-active display-manager.service").count(), 2);
        assert!(GREETER_CHECK.contains("sleep \"$settle\""));
        // And a unit that has exhausted its restarts fails now rather
        // than after two minutes of polling something already dead.
        assert!(GREETER_CHECK.contains("is-failed display-manager.service"));

        // Run it against stub systemctls, since a check that has never
        // executed is a check nobody has tested.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let script = dir.path().join("check");
        std::fs::write(
            &script,
            GREETER_CHECK.replace("SECONDS + 120", "SECONDS + 4").replace("settle=5", "settle=1"),
        )
        .unwrap();
        for (name, body, want) in [
            ("healthy", "case \"$1\" in is-failed) exit 3 ;; *) exit 0 ;; esac", true),
            // is-failed answers yes: the unit gave up.
            ("dead", "case \"$1\" in is-failed) exit 0 ;; *) exit 3 ;; esac", false),
            // Never active, never failed: the deadline decides.
            ("absent", "exit 3", false),
        ] {
            std::fs::write(bin.join("systemctl"), format!("#!/usr/bin/bash\nshift\n{body}\n"))
                .unwrap();
            let mut perms = std::fs::metadata(bin.join("systemctl")).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(bin.join("systemctl"), perms).unwrap();
            let out = std::process::Command::new("bash")
                .arg(&script)
                .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()))
                .output()
                .expect("bash");
            assert_eq!(out.status.success(), want, "{name}: {:?}", out);
        }
    }

    #[test]
    fn timezone_links_localtime() {
        let out =
            generate(&config("schema_version = 1\n[system]\ntimezone = \"America/Denver\"\n"));
        assert!(out.contains(
            "test -e /usr/share/zoneinfo/America/Denver && ln -sfn /usr/share/zoneinfo/America/Denver /etc/localtime"
        ));
        // unset means UTC: no localtime layer at all
        let out = generate(&config("schema_version = 1"));
        assert!(!out.contains("/etc/localtime"));
    }

    #[test]
    fn stock_waybar_spawn_is_deduped() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        // Fedora's default config spawns waybar and kuma no longer ships
        // one, so the sed matters more than it used to rather than less:
        // without it the session starts a bar that is not in the image
        // and the shell's own bar comes up beside a black rectangle.
        assert!(out.contains("-e '/^spawn-at-startup \"waybar\"$/d'"));
        assert_eq!(NIRI_EXTRAS.matches("spawn-at-startup \"waybar\"").count(), 0);
        assert!(!NIRI_PACKAGES.contains(&"waybar"), "nothing left to spawn");
    }

    /// The local-override include is worthless anywhere but last: niri
    /// merges includes positionally, so anything appended after it would
    /// silently outrank the machine's own settings. The extras are catted
    /// onto the end of the config, so last-in-extras is last-in-config.
    #[test]
    fn the_local_override_include_has_the_last_word() {
        let include = "include optional=true \"~/.config/niri/local.kdl\"";
        assert_eq!(
            NIRI_EXTRAS.trim_end().lines().next_back(),
            Some(include),
            "the local override must be the final line of the niri config"
        );
        // required-and-absent would fail `niri validate` in the build, and
        // every machine that never writes the file
        assert!(!NIRI_EXTRAS.contains("include \"~/.config/niri/local.kdl\""));
        // the build appends extras after the merged upstream config, which
        // is the only reason last-in-extras means last-in-config
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("cat /usr/lib/kuma/niri-extras.kdl >> /etc/niri/config.kdl"));
    }

    #[test]
    fn context_includes_theme_files_for_niri() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"niri\"\n", dir.path());
        let wallpaper = std::fs::read(dir.path().join("kuma-wallpaper.jpg")).unwrap();
        assert!(!wallpaper.is_empty());
        let extras = std::fs::read_to_string(dir.path().join("niri-extras.kdl")).unwrap();
        // The wallpaper ships as the image's own file; the shell's
        // default is compiled to point at exactly that path.
        // The shell is a supervised unit now, not a spawn: a niri
        // spawn lands in a transient scope, and a scope cannot restart.
        assert!(!extras.contains("spawn-at-startup \"kuma-shell\""), "{extras}");
        assert!(dir.path().join("kuma-shell.service").exists());
        assert!(SHELL_SERVICE.contains("Restart=always"), "{SHELL_SERVICE}");
        assert!(extras.contains("kuma-clipboard-bridge"));
        assert!(dir.path().join("kuma-clipboard-bridge").exists());
        let greetd = std::fs::read_to_string(dir.path().join("greetd-config.toml")).unwrap();
        assert!(greetd.contains("Welcome to kumaOS"));
        assert!(dir.path().join("kitty.conf").exists());
        let ff = std::fs::read_to_string(dir.path().join("fastfetch-config.jsonc")).unwrap();
        assert!(ff.contains("/usr/lib/kuma/fastfetch-logo.txt"));
        assert!(dir.path().join("fastfetch-logo.txt").exists());
    }

    #[test]
    fn branding_names_the_release() {
        let out = generate(&config("schema_version = 1\n"));
        // One bear per Fedora base, and an unlisted base must still name
        // the kuma version rather than degrading to a bare "kumaOS".
        assert!(out.contains(r#"44) CODENAME="Beorn""#));
        assert!(out.contains(r#"45) CODENAME="Callisto""#));
        assert!(out.contains(r#"*) CODENAME="""#));

        // The number is kuma's own and comes from the building binary, so
        // an image says which kuma made it. The placeholder must be gone:
        // shipping "@KUMAVERSION@" as a machine's PRETTY_NAME is the whole
        // failure this asserts against.
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            !out.contains("@KUMAVERSION@"),
            "the version placeholder reached the Containerfile"
        );
        assert!(out
            .contains(&format!(r#"PRETTY_NAME=\"kumaOS {version}${{CODENAME:+ ($CODENAME)}}\""#)));
        // VERSION is rewritten unconditionally, not only when a bear
        // matched: left alone it keeps Fedora's own string, which on a
        // branched base reads "45 (Rawhide Prerelease)" inside an OS that
        // otherwise calls itself kumaOS.
        assert!(out.contains(&format!(r#"VERSION=\"{version}${{CODENAME:+ ($CODENAME)}}\""#)));

        // VERSION_ID stays Fedora's: toolbox, distrobox and COPR resolve
        // against it, so kuma's version must never be written there.
        assert!(!out.contains(&format!("VERSION_ID={version}")));
        // fedora-release is a compatibility file and keeps Fedora's number.
        assert!(out.contains(r#"echo "kumaOS release ${VERSION_ID}${CODENAME:+ ($CODENAME)}""#));
    }

    #[test]
    fn no_desktop_means_no_desktop_layer() {
        let out = generate(&config("schema_version = 1"));
        assert!(!out.contains("greetd"));
        assert!(!out.contains("graphical.target"));
    }

    /// The policy is the one thing here that fails closed in production
    /// and nowhere else: get it wrong and every `bootc upgrade` from the
    /// published image is refused, on machines belonging to people who
    /// did not build it. So the shipped bytes are pinned rather than
    /// described.
    ///
    /// `matchRepository` is the field that was wrong first and is the
    /// reason this test is exact. cosign records the signed identity as
    /// the bare repository with no tag, so `matchRepoDigestOrExact` —
    /// the obvious choice, and the one initially written — rejects every
    /// image kuma has ever published. Verified against the live registry
    /// with the real key (accepted) and a wrong key (refused).
    #[test]
    fn the_signature_policy_is_the_one_that_was_verified() {
        let policy = signature_policy();
        let parsed: serde_json::Value =
            serde_json::from_str(&policy).expect("policy.json must be valid JSON");

        let repo = "ghcr.io/letdown2491/kuma";
        let rule = &parsed["transports"]["docker"][repo][0];
        assert_eq!(rule["type"], "sigstoreSigned");
        assert_eq!(rule["keyPath"], COSIGN_PUB_PATH);
        assert_eq!(
            rule["signedIdentity"]["type"], "matchRepository",
            "cosign signs the bare repository; any stricter identity rejects every published image"
        );

        // Everything else stays permissive on purpose. This file is
        // shared by podman and bootc, so a blanket requirement refuses
        // Fedora's base on the next update and the machine's own local
        // build on the next switch.
        assert_eq!(parsed["default"][0]["type"], "insecureAcceptAnything");
        for transport in ["containers-storage", "docker-daemon", "dir", "oci"] {
            assert_eq!(
                parsed["transports"][transport][""][0]["type"], "insecureAcceptAnything",
                "{transport} must stay permissive or local images stop working"
            );
        }
        assert!(
            parsed["transports"]["docker"].get("").is_none(),
            "a catch-all docker rule would require signatures from every registry"
        );

        // The other half of the pair: without this the policy has nothing
        // to look at, because cosign stores signatures as a separate tag.
        let rd = registries_d();
        assert!(rd.contains("use-sigstore-attachments: true"));
        assert!(rd.contains(repo));
    }

    /// The key has to be in the binary, because a build runs from the
    /// binary and not from a checkout.
    #[test]
    fn the_signing_key_ships_in_every_image() {
        assert!(COSIGN_PUB.contains("BEGIN PUBLIC KEY"));
        let out = generate(&config("schema_version = 1"));
        assert!(out.contains(&format!("COPY cosign.pub {COSIGN_PUB_PATH}")));
        assert!(out.contains("COPY containers-policy.json /etc/containers/policy.json"));
        assert!(out.contains("registries.d/kuma-sigstore.yaml"));

        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n", dir.path());
        let shipped = std::fs::read_to_string(dir.path().join("cosign.pub")).unwrap();
        assert_eq!(shipped, COSIGN_PUB, "the image must carry the key the policy names");
        let policy = std::fs::read_to_string(dir.path().join("containers-policy.json")).unwrap();
        assert_eq!(policy, signature_policy());
    }

    /// SECURITY.md tells the reader that declaring no desktop reaches no
    /// package source beyond Fedora's. Choosing a desktop is what pulls in
    /// RPM Fusion, so "which declarations touch a third-party repo" is a
    /// promise the trust-boundary section makes and this is what keeps it
    /// true. A regression here would be silent: the image still builds and
    /// still works, it just quietly trusts one more party than the
    /// document says it does.
    #[test]
    fn only_a_desktop_reaches_a_third_party_repo() {
        let minimal = generate(&config("schema_version = 1"));
        assert!(!minimal.contains("rpmfusion"), "a desktopless image reached RPM Fusion");
        assert!(!minimal.contains("freeworld"));
        for desktop in ["niri", "cosmic"] {
            let out = generate(&config(&format!(
                "schema_version = 1\n[system]\ndesktop = \"{desktop}\"\n"
            )));
            assert!(
                out.contains("rpmfusion-free-release"),
                "{desktop} lost the freeworld codecs; SECURITY.md still names RPM Fusion"
            );
        }
    }

    #[test]
    fn desktop_layer_precedes_user_packages() {
        let out = generate(&config(
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n[packages]\nrpm = [\"htop\"]\n",
        ));
        let desktop_at = out.find("niri").unwrap();
        let user_at = out.find("htop").unwrap();
        assert!(desktop_at < user_at);
    }

    #[test]
    fn flatpaks_generate_remote_list_and_sync_service() {
        let out = generate(&config(
            "schema_version = 1\n[packages]\nflatpak = [\"org.mozilla.firefox\"]\n",
        ));
        assert!(out.contains(&dnf_install("flatpak")));
        assert!(out.contains("/etc/flatpak/remotes.d/flathub.flatpakrepo"));
        assert!(out.contains("COPY flatpaks /usr/lib/kuma/flatpaks"));
        assert!(out.contains("systemctl enable kuma-flatpak-sync.service"));
    }

    #[test]
    fn niri_ships_sync_even_without_declared_apps() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("flathub.flatpakrepo"));
        // flatpak comes from the desktop set; no second install layer
        assert!(!out.contains(&dnf_install("flatpak")));
        // convergence: the empty declaration still syncs, removing strays
        assert!(out.contains("systemctl enable kuma-flatpak-sync.service"));
    }

    /// Convergence removes what the declaration installed and dropped,
    /// and nothing else. The removal loop must read the state file: the
    /// moment it reads `flatpak list` instead, every app a store put
    /// here system-wide is swept, which is the bug that kept kuma from
    /// being able to ship a store at all.
    /// Off unless declared, and when declared it bakes its own tools:
    /// a snapshot timer that dies on a missing btrfs binary would be a
    /// backup that silently isn't one.
    #[test]
    fn snapshots_are_opt_in_and_bring_their_own_tools() {
        let out = generate(&config("schema_version = 1\n"));
        assert!(!out.contains("kuma-snapshot"));
        assert!(!out.contains(&dnf_install("btrfs-progs")));

        let out = generate(&config("schema_version = 1\n[snapshots]\nenable = true\n"));
        assert!(out.contains(&dnf_install("btrfs-progs")));
        assert!(out.contains("RUN systemctl enable kuma-snapshot.timer"));
    }

    /// The retention and the target are the declaration's, so they have
    /// to reach the machine; and a target that isn't btrfs has to end the
    /// run cleanly rather than fail a unit on every tick, because one
    /// declaration describes machines laid out differently.
    #[test]
    fn snapshot_script_carries_the_declared_policy() {
        let declared = config(
            "schema_version = 1\n[snapshots]\nenable = true\ntarget = \"/var/data\"\nkeep_recent = 3\nkeep_daily = 90\n",
        );
        let script = snapshot_script(&declared);
        assert!(script.contains("target='/var/data'"));
        assert!(script.contains("keep_recent=3"));
        assert!(script.contains("keep_daily=90"));
        for placeholder in ["{target}", "{keep_recent}", "{keep_daily}"] {
            assert!(!script.contains(placeholder), "{placeholder} was never substituted");
        }
        assert!(script.contains("= btrfs ] || exit 0"), "a non-btrfs target exits clean");
        assert!(script.contains("btrfs subvolume show"), "a non-subvolume exits clean");

        // -T, and the same question doctor asks. Without it findmnt wants
        // a mount point, and a btrfs subvolume need not be one: a
        // /var/home nested inside the deployment's /var answered nothing,
        // so the script exited 0 having taken no snapshot, on every
        // machine kuma installs. The script said the target was not
        // btrfs; doctor, asking with -T, said it was.
        assert!(
            script.contains(r#"findmnt -no FSTYPE -T "$target""#),
            "the filesystem question has to be about the path, not about a mount point"
        );

        let dir = tempfile::tempdir().unwrap();
        context(
            "schema_version = 1\n[snapshots]\nenable = true\ninterval = \"daily\"\n",
            dir.path(),
        );
        let timer = std::fs::read_to_string(dir.path().join("kuma-snapshot.timer")).unwrap();
        assert!(timer.contains("OnCalendar=daily"));
        assert!(timer.contains("Persistent=true"), "a laptop asleep at the hour still snapshots");
    }

    /// The declaration's policy has to survive into the script, and the
    /// script has to be shell. The second half is not pedantry: every
    /// substitution here lands inside a `--exclude` argument list built
    /// by string concatenation, and a stray continuation would eat the
    /// command on the next line, which bash reports at run time on a
    /// machine nobody is watching.
    #[test]
    fn backup_script_carries_the_declared_policy() {
        let declared = config(
            "schema_version = 1\n[snapshots]\nenable = true\ntarget = \"/var/home\"\n\
             [backup]\nenable = true\nrepo = \"s3:https://minio.example:9000/kuma\"\n\
             keep_daily = 3\nkeep_weekly = 2\nkeep_monthly = 1\n\
             exclude = [\"~/Videos\", \"/var/home/shared/scratch\"]\n",
        );
        let script = backup_script(&declared);

        assert!(script.contains("RESTIC_REPOSITORY='s3:https://minio.example:9000/kuma'"));
        assert!(script.contains("--keep-daily 3 --keep-weekly 2 --keep-monthly 1"));
        // Forget is nearly free and prune repacks; doing both nightly
        // moves gigabytes over somebody's uplink to reclaim what one
        // expired snapshot left behind.
        assert!(
            !script.contains("forget --tag kuma --prune"),
            "pruning belongs on its own schedule: {script}"
        );
        assert!(script.contains("restic prune"), "it still prunes, just not every run");
        assert!(script.contains("604800"), "weekly, measured from a stamp rather than a weekday");
        // Both stamps live in /var/lib/kuma, so it has to exist before
        // either is written rather than before only the second.
        let made = script.find("install -d -m 0755 /var/lib/kuma").unwrap();
        for writes in ["$pruned", "$stamp"] {
            let at = script.rfind(&format!("> \"{writes}\"")).unwrap();
            assert!(made < at, "{writes} is written before its directory exists");
        }
        for placeholder in
            ["{target}", "{repo}", "{excludes}", "{keep_daily}", "{keep_weekly}", "{keep_monthly}"]
        {
            assert!(!script.contains(placeholder), "{placeholder} was never substituted");
        }

        // Curated first and always. These are trees this same file
        // rebuilds, so keeping them offsite is paying to store what kuma
        // can recreate.
        for curated in ["$target/linuxbrew", "$target/*/.cache"] {
            assert!(script.contains(&format!("--exclude \"{curated}\"")), "missing {curated}");
        }
        // A declared `~/` means every home, which is the only reading
        // that works when the target holds more than one.
        assert!(script.contains("--exclude \"$target/*/Videos\""), "{script}");
        assert!(script.contains("--exclude \"/var/home/shared/scratch\""));
        // The snapshot store is inside the target, and after the bind
        // mount it is the snapshot's own copy of it.
        assert!(script.contains("--exclude \"$target/.snapshots\""));

        // The bind is the whole reason this is incremental: restic 0.19
        // has no --set-path and groups by host+paths, so a source path
        // named for the minute it was taken never matches a parent.
        assert!(script.contains(r#"mount --bind "$store/$newest" "$target""#), "{script}");
        // Run by hand rather than by the unit, an untrapped bind leaves
        // the live /var/home replaced by a read-only snapshot until the
        // machine reboots.
        assert!(script.contains(r##"trap 'umount "$target""##), "the bind is undone: {script}");

        // Three states that are "not ready yet" rather than "broken",
        // each exiting clean so one declaration can describe machines at
        // different stages of being set up.
        assert!(script.contains("no credential loaded"));
        assert!(script.contains("no snapshot in $store yet"));
        assert!(script.contains("seed it once with 'kuma backup --init'"));
        // Bounded, and it has to be. restic retries a missing bucket
        // with backoff, so an unbounded probe takes minutes to answer
        // "no" on every machine nobody has seeded, every night.
        assert!(script.contains("timeout 30 restic cat config"), "{script}");
        // A unit has no HOME, and restic treats an unopenable cache as
        // fatal, so without this every run dies before it reaches the
        // repository with an error that reads like a network failure.
        assert!(script.contains("export RESTIC_CACHE_DIR=/var/cache/restic"), "{script}");
        assert!(script.contains("cannot reach"), "unreachable reads differently from absent");

        // The stamp is what doctor grades, so it must only be written by
        // a run that copied something. Every early exit is above it.
        let stamp_at = script.find("date -u").expect("the run stamps its success");
        for guard in ["no credential loaded", "no snapshot in $store yet", "no repository at"] {
            assert!(script.find(guard).unwrap() < stamp_at, "{guard} must precede the stamp");
        }

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
            .expect("bash");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// The guards are only worth having if they are reachable, and the
    /// first draft's were not: `newest=$(... | grep ...)` under
    /// `set -o pipefail` exits non-zero when there is no snapshot, so
    /// `set -e` killed the script one line above the message written for
    /// that machine. Reading the rendered script found it; nothing else
    /// would have until a real machine sat there failing quietly.
    ///
    /// So this runs the script rather than reading it, on a target with
    /// no snapshots in it, and asserts it exits clean and says why.
    #[test]
    fn a_machine_with_nothing_to_copy_yet_exits_clean() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().display();
        let declared = config(&format!(
            "schema_version = 1\n[snapshots]\nenable = true\ntarget = \"{target}\"\n\
             [backup]\nenable = true\nrepo = \"b2:kuma\"\n"
        ));
        let script = backup_script(&declared);

        let run = |env: &[(&str, &str)]| {
            let mut cmd = std::process::Command::new("bash");
            cmd.args(["-s"])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            for (k, v) in env {
                cmd.env(k, v);
            }
            cmd.spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(script.as_bytes())?;
                    child.wait_with_output()
                })
                .expect("bash")
        };

        // No credential: the machine is un-provisioned, not broken.
        let out = run(&[]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("no credential loaded"),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );

        // Credential present, no snapshot taken yet. This is the one the
        // pipefail bug made unreachable.
        let out = run(&[("RESTIC_PASSWORD", "x")]);
        assert!(
            out.status.success(),
            "an empty snapshot store must not fail the unit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("no snapshot in"),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );

        // Nothing may have been stamped: freshness must reflect a run
        // that actually copied something.
        assert!(!std::path::Path::new("/var/lib/kuma/backup-last-test").exists());
    }

    /// The unit has to name the secret the declaration named, tolerate
    /// it being absent, and get its own mount namespace. Without the
    /// last one the bind above would replace the running system's home
    /// with a read-only snapshot for the length of the backup.
    #[test]
    fn the_backup_unit_reads_the_named_secret_and_mounts_privately() {
        let dir = tempfile::tempdir().unwrap();
        context(
            "schema_version = 1\n[snapshots]\nenable = true\n\
             [backup]\nenable = true\nrepo = \"b2:kuma\"\nsecret = \"start9\"\n\
             interval = \"03:00\"\n",
            dir.path(),
        );
        let service = std::fs::read_to_string(dir.path().join("kuma-backup.service")).unwrap();
        assert!(
            service.contains("EnvironmentFile=-/var/lib/kuma/secrets/start9.env"),
            "the leading - is what lets an un-provisioned machine boot: {service}"
        );
        assert!(service.contains("PrivateMounts=yes"), "{service}");
        assert!(service.contains("After=kuma-snapshot.service"), "copy after taking: {service}");
        // A timer firing on resume finds the network still coming up,
        // and network-online.target only orders boot.
        assert!(service.contains("Restart=on-failure"));
        assert!(service.contains("StartLimitBurst=6"), "retry, but not forever: {service}");

        let timer = std::fs::read_to_string(dir.path().join("kuma-backup.timer")).unwrap();
        assert!(timer.contains("OnCalendar=03:00"));
        assert!(timer.contains("Persistent=true"), "a laptop asleep at the hour still backs up");
    }

    /// The one unrecoverable thing outside home rides along only when
    /// asked, and the default is that it does not. Doctor is what makes
    /// that default honest rather than a trap, so this only asserts the
    /// converger obeys the knob.
    #[test]
    fn network_connections_ride_along_only_when_asked() {
        let head = "schema_version = 1\n[snapshots]\nenable = true\n\
                    [backup]\nenable = true\nrepo = \"b2:kuma\"\n";
        let off = backup_script(&config(head));
        assert!(
            !off.contains(NETWORK_CONNECTIONS),
            "off by default, because those are passphrases in the clear"
        );

        let on = backup_script(&config(&format!("{head}network_connections = true\n")));
        assert!(
            on.contains(&format!(r#"restic backup "$target" {NETWORK_CONNECTIONS} \"#)),
            "a second source path, not an exclude: {on}"
        );
    }

    /// The scripts are shell and the verb is Rust, so the paths they
    /// share cannot be one constant. They can still be one answer, and
    /// this is what makes them one: doctor grading a stamp the converger
    /// does not write, or a unit loading a credential `kuma backup` does
    /// not name, are both silent failures that look like a healthy
    /// A declaration with every optional feature turned on.
    ///
    /// Most of them default to off, so a fixture that leaves them there
    /// exercises none of the branches that copy files into the image or
    /// stage them into the build context. Two tests were checking those
    /// branches against declarations that never entered them.
    const EVERYTHING_ON: &str = "schema_version = 1\n\
         [system]\ndesktop = \"niri\"\nbrew = true\nhostname = \"probe\"\n\
         timezone = \"Pacific/Auckland\"\n\
         [packages]\nflatpak = [\"org.mozilla.firefox\"]\nbrew = [\"ripgrep\"]\n\
         [services]\nenable = [\"sshd.service\", \"tailscaled.service\"]\n\
         [snapshots]\nenable = true\n\
         [backup]\nenable = true\nrepo = \"b2:kuma\"\nnetwork_connections = true\n\
         [nostr]\nenable = true\nserve = true\nrelays = [\"wss://relay.nip46.com\"]\n\
         [nostr.relay]\nenable = true\n\
         [overrides.\"org.mozilla.firefox\"]\nsockets = [\"wayland\"]\n\
         [user]\nname = \"probe\"\nssh_keys = [\"ssh-ed25519 AAAAC3Nz probe@example\"]\n";

    /// The two declaration values that get special handling because a
    /// careless one is a published secret, staged as files with modes
    /// and destinations the ordinary COPY grammar does not reach:
    /// user.password_hash and system.ca_certificates.
    ///
    /// The values are public by construction, so the fixture can carry
    /// them without the tree holding a secret. The hash is the published
    /// SHA-crypt test vector for the password "Hello world!" at salt
    /// "saltstring"; the anchor is a self-signed throwaway certificate
    /// whose key was destroyed the moment it was written, generated for
    /// this fixture and pinned here byte for byte. The byte-pinning is
    /// the point: the account file must ship `COPY --chmod=600`, the
    /// anchor must land in the trust-store directory, and a refactor
    /// that drops either back to the ordinary path is a golden diff
    /// rather than a unit test somebody has to remember exists.
    const SECRETS: &str = "schema_version = 1\n\
         [user]\nname = \"probe\"\n\
         password_hash = \"$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1\"\n\
         [system.ca_certificates]\n\
         kuma-test = \"\"\"\n\
         -----BEGIN CERTIFICATE-----\n\
         MIIDPzCCAiegAwIBAgIUO8XtpDc29P521fge4xkHXnSBDbswDQYJKoZIhvcNAQEL\n\
         BQAwLzEtMCsGA1UEAwwka3VtYSB0ZXN0IGFuY2hvciAoZml4dHVyZSwgbm90IGEg\n\
         Q0EpMB4XDTI2MDgzMDE3MzUyN1oXDTM2MDgyNzE3MzUyN1owLzEtMCsGA1UEAwwk\n\
         a3VtYSB0ZXN0IGFuY2hvciAoZml4dHVyZSwgbm90IGEgQ0EpMIIBIjANBgkqhkiG\n\
         9w0BAQEFAAOCAQ8AMIIBCgKCAQEAiUKuW7rSYSXUTGzzUs9cGziy4WoLbUH0NhPD\n\
         40kn0LIrD2F6+aGjsxljdmp9CgZov5DDsckUENR7Yom/OjQmNSJ23+bm471+7LDG\n\
         4iurDCDLD1x+DtKRrwRgAst9mTICHPYqYE0VICDyqVgiUELzvfRF6v/th8H2SIk0\n\
         tOwMTQkJ4HNkESHZKbh3RtGdlzT8BNPb0ltY43QRikHd8JoPaYSOJeBJ7IAxjv0E\n\
         todO7BndH4yLEWXITONAv4eJF7lHYMLOex8eSB4QKJRQwg2FdMtSt9tj6RtXo5/r\n\
         AGLqsQg/ykuilhY1NhLHkDou1oMzO5kC1Lz1038yMjY+jtcs9wIDAQABo1MwUTAd\n\
         BgNVHQ4EFgQUnaAsUfUsj3GMjqJNSDk1NMSymYQwHwYDVR0jBBgwFoAUnaAsUfUs\n\
         j3GMjqJNSDk1NMSymYQwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOC\n\
         AQEAIRf92NGpczDyg3N9DURs4rWxQ6dXtwGBQEcyJ+FfIX0skOShl9Lc6BXGfBhT\n\
         OkDp3RBGJEZjvKSg1xe/IJdtj36yF7XisfWNH2o2Zzf0XaM33Bm3g/ATLqN4pG04\n\
         Yu9vy1Olw7p7RGjwUrMdctmImJw2k2OsZf7G9JXL5FacSsLlSd39eeehAU0PH+/q\n\
         Gqsbg1zhoEKgU8W8Lwpd1WZhNuu2GyWM1FN64XSupZT0r+Rd1S/7nrKwL3WluBB8\n\
         NUr2bTvXlbxJXHzyxtVfh38LwugHr0JL/3efLUUzf3rJoWv8RKgYbJr3TSR9hfpy\n\
         649YhIBcU7ptnjU4gq1wTmwbMw==\n\
         -----END CERTIFICATE-----\n\"\"\"\n";

    /// machine. This release has already produced that shape three
    /// times.
    #[test]
    fn the_shell_and_the_verb_agree_on_where_things_live() {
        // The baked lists: written by a COPY here, read as a literal by
        // the generated shell, and read again by four Rust callers that
        // all treat absence as "nothing to do". A drift makes a machine
        // that looks converged and is not.
        use crate::inventory::FLATPAK_STATE;
        use crate::state::{BAKED_BREWS, BAKED_FLATPAKS, BAKED_OVERRIDES};
        let niri = generate(&config(EVERYTHING_ON));
        for (path, script) in
            [(BAKED_FLATPAKS, FLATPAK_SYNC_SCRIPT), (BAKED_BREWS, BREW_SYNC_SCRIPT)]
        {
            assert!(
                script.contains(&format!("declared={path}")),
                "the converger reads a different path than the one Rust decides from: {path}"
            );
            assert!(niri.contains(&format!(" {path}\n")), "nothing copies {path} into the image");
        }
        assert!(
            FLATPAK_SYNC_SCRIPT.contains(&format!("state={FLATPAK_STATE}")),
            "the converger tracks authority somewhere doctor does not read"
        );
        assert!(niri.contains(BAKED_OVERRIDES), "nothing copies {BAKED_OVERRIDES} into the image");

        // The session constants exist so a session command cannot change
        // in one of two places, and their own doc comment says exactly
        // that, while the greeter config beside them spelled the command
        // out again. A const cannot interpolate into a const, so the
        // agreement is asserted instead of deduplicated.
        assert!(
            GREETD_CONFIG.contains(NIRI_SESSION),
            "the greeter starts a session the constants do not name"
        );
        assert!(
            niri.contains(GREETD_CONF),
            "the greeter config is copied somewhere greetd does not read"
        );

        assert!(
            BACKUP_SCRIPT.contains(&format!("stamp={}", crate::backup::STAMP)),
            "the converger stamps somewhere doctor does not read"
        );
        let dir = crate::backup::SECRETS_DIR;
        assert!(
            RESTORE_SCRIPT.contains(&format!("secret={dir}/restore.env")),
            "the restore unit reads a credential the install does not write"
        );
        let service = backup_service(&config(
            "schema_version = 1\n[snapshots]\nenable = true\n\
             [backup]\nenable = true\nrepo = \"b2:kuma\"\nsecret = \"named\"\n",
        ));
        assert!(
            service.contains(&format!("EnvironmentFile=-{dir}/named.env")),
            "the unit loads a credential from somewhere else entirely: {service}"
        );
    }

    /// Root extracting a tarball into a tree uid 1000 owns is the one
    /// place kuma does that, and the guard deciding whether it runs
    /// again lives inside the same tree.
    #[test]
    fn brew_setup_refuses_a_prefix_it_does_not_own() {
        // The check must come before anything that writes, or it is
        // decoration: mkdir -p follows a symlink and tar chdirs through
        // it, so both have to be downstream of the refusal.
        // Line starts, not substrings: the comment above the guard
        // describes the attack and names these same commands, and the
        // first version of this test matched the prose.
        let guard = BREW_SETUP_SCRIPT.find("refusing to write into it").unwrap();
        for writes in ["\nmkdir -p", "\n    | tar -xz", "\nln -sf"] {
            let at = BREW_SETUP_SCRIPT
                .find(writes)
                .unwrap_or_else(|| panic!("the setup no longer runs {writes:?}"));
            assert!(at > guard, "{writes:?} happens before the ownership check that protects it");
        }
        assert!(
            BREW_SETUP_SCRIPT.contains("[ -L \"$dir\" ]"),
            "a symlink in place of the prefix is the move this guards against"
        );

        let out = std::process::Command::new("bash")
            .args(["-n", "/dev/stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(BREW_SETUP_SCRIPT.as_bytes())?;
                child.wait_with_output()
            })
            .expect("bash");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// Every kuma unit an image enables has to be accounted for on live
    /// media: masked, or carrying a condition that makes it skip itself
    /// there.
    ///
    /// The per-block query in action: a feature's tests assert on what
    /// it emits rather than grepping the whole Containerfile, and the
    /// gate is asked by the block, once, rather than re-derived by
    /// every assertion.
    #[test]
    fn a_blocks_text_is_queryable_on_its_own() {
        let niri = config("schema_version = 1\n[system]\ndesktop = \"niri\"\n");
        let minimal = config("schema_version = 1\n");
        assert!(blocks::emitted(&niri, "desktop-niri").contains("kuma-sleep-guard"));
        assert!(blocks::emitted(&niri, "desktop-cosmic").is_empty());
        assert!(blocks::emitted(&minimal, "desktop-niri").is_empty());
        assert!(blocks::emitted(&minimal, "snapshots").is_empty());
        assert!(blocks::emitted(
            &config("schema_version = 1\n[snapshots]\nenable = true\n"),
            "snapshots",
        )
        .contains("kuma-snapshot.timer"));
    }

    /// The nostr unit carries what the declaration armed: a window on
    /// the exec line when the switch is armed, and no flag repeating
    /// the daemon's own off when it is not.
    #[test]
    fn the_nostr_unit_carries_the_switch_the_declaration_armed() {
        let armed = tempfile::tempdir().unwrap();
        context(
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n[nostr]\nenable = true\n\
             relays = [\"wss://relay.nip46.com\"]\ninactivity_lock_secs = 86400\n",
            armed.path(),
        );
        let unit = std::fs::read_to_string(armed.path().join("kuma-nostrd.service")).unwrap();
        assert!(unit.contains("--inactivity-lock-secs 86400"), "{unit}");

        let unarmed = tempfile::tempdir().unwrap();
        context(
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n[nostr]\nenable = true\n",
            unarmed.path(),
        );
        let unit = std::fs::read_to_string(unarmed.path().join("kuma-nostrd.service")).unwrap();
        assert!(!unit.contains("--inactivity-lock-secs"), "{unit}");
    }

    /// Every kuma unit the image enables must declare, in the block
    /// that enables it, what it does on installer media — and the
    /// declaration must be honest: a `Conditioned` unit actually
    /// carries its condition, a `Runs` unit carries its reason.
    ///
    /// The parser this replaced read only lines beginning `RUN
    /// systemctl enable `, so the compound `--global` line that enables
    /// the shell and the sleep guard was invisible to it, and both
    /// units shipped for releases with no disposition at all. Reading
    /// any line that carries `systemctl` and `enable` sees them; the
    /// tables cannot leave one out.
    #[test]
    fn every_enabled_kuma_unit_has_a_live_disposition() {
        use crate::containerfile::blocks::{Block, Live};

        let dir = tempfile::tempdir().unwrap();
        context(EVERYTHING_ON, dir.path());
        let containerfile = std::fs::read_to_string(dir.path().join("Containerfile")).unwrap();

        let enabled: Vec<String> = containerfile
            .lines()
            .filter(|l| l.contains("systemctl") && l.contains("enable"))
            .flat_map(|l| l.split_whitespace())
            .filter(|u| u.ends_with(".service") || u.ends_with(".timer") || u.ends_with(".target"))
            .map(String::from)
            .collect();
        assert!(enabled.len() > 5, "expected the image to enable units: {enabled:?}");

        fn disposition<'a>(unit: &str, blocks: &'a [Block]) -> Option<&'a Live> {
            blocks
                .iter()
                .flat_map(|b| b.units.iter())
                .find(|(u, _)| *u == unit)
                .map(|(_, live)| live)
        }

        // Only kuma's own units need a disposition here: the greeters
        // and firewalls of the world are installed and preset by their
        // packages, and the live layer's own two presets are declared
        // beside its mask line.
        for unit in enabled.iter().filter(|u| u.starts_with("kuma-")) {
            let Some(live) = disposition(unit, blocks::BLOCKS) else {
                panic!("{unit} is enabled by the image and no block declares what it does on installer media");
            };
            match live {
                Live::Masked(reason) => {
                    assert!(!reason.trim().is_empty(), "{unit} masked for no stated reason")
                }
                Live::Runs(reason) => {
                    assert!(
                        !reason.trim().is_empty(),
                        "{unit} called live-safe for no stated reason"
                    )
                }
                Live::Conditioned(reason) => {
                    let text = std::fs::read_to_string(dir.path().join(unit)).unwrap_or_default();
                    assert!(
                        text.contains("ConditionPathExists="),
                        "{unit} is Conditioned ({reason}) but its unit text carries no condition"
                    );
                }
            }
        }

        // And the reverse: every unit a table declares is genuinely
        // enabled, so a row cannot outlive the enable line it described.
        for unit in blocks::BLOCKS.iter().flat_map(|b| b.units.iter().map(|(u, _)| *u)) {
            assert!(
                enabled.iter().any(|e| e == unit),
                "{unit} is declared in a units table but the image never enables it"
            );
        }
    }

    /// The unit that puts a home directory back on a machine that has
    /// just been installed, and the one ordering constraint that makes
    /// it possible at all.
    #[test]
    fn the_restore_runs_after_the_subvolume_exists() {
        // /var/home is not a subvolume until kuma-home-subvol has run,
        // and a restore that beat it would fill an ordinary directory,
        // which is the exact state that unit steps back from. The
        // machine would come up with no subvolume, no snapshots, and
        // nothing saying why.
        assert!(
            RESTORE_SERVICE.contains("After=kuma-home-subvol.service kuma-user-sync.service"),
            "{RESTORE_SERVICE}"
        );
        // Gated on the request rather than on being enabled, because
        // every boot after the first has to skip it without looking
        // like a unit that failed.
        assert!(RESTORE_SERVICE.contains("ConditionPathExists=/var/lib/kuma/restore-request"));
        // A home directory over a slow link outlasts any default.
        assert!(RESTORE_SERVICE.contains("TimeoutStartSec=infinity"));
        // The credential is PARSED by systemd, never executed by a
        // shell. See the sibling test below for what sourcing it did.
        assert!(
            RESTORE_SERVICE.contains("EnvironmentFile=-/var/lib/kuma/secrets/restore.env"),
            "{RESTORE_SERVICE}"
        );

        // Whatever the converger stores, this has to bring back. The
        // network connections were the entire reason for a knob, and
        // restoring only home would lose them silently: the machine
        // comes up looking complete with the one unrecreatable thing
        // missing.
        assert!(
            RESTORE_SCRIPT.contains("--include /var/home")
                && RESTORE_SCRIPT.contains(&format!("--include {NETWORK_CONNECTIONS}")),
            "the restore must cover both paths the backup stores: {RESTORE_SCRIPT}"
        );
        assert!(
            RESTORE_SCRIPT.contains("--tag kuma"),
            "a shared repository must not hand this machine a snapshot kuma never made"
        );

        // The request outlives a failed restore and is cleared only by
        // one that worked. There is no Restart= here, so surviving buys
        // one attempt per boot rather than a loop; clearing first would
        // mean a repository unreachable for one minute at first boot
        // discards the restore for good and the machine comes up empty.
        let restored = RESTORE_SCRIPT.find("restic restore").unwrap();
        let cleared = RESTORE_SCRIPT.rfind(r#"rm -f "$request""#).unwrap();
        assert!(restored < cleared, "a bad day must cost a retry, not the data");
        // Except the one request nothing can ever satisfy.
        let no_secret = RESTORE_SCRIPT.find("is not there").unwrap();
        assert!(
            RESTORE_SCRIPT[..no_secret].matches("restic restore").count() == 0,
            "a missing credential is cleared without attempting anything"
        );

        let out = std::process::Command::new("bash")
            .args(["-n", "/dev/stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(RESTORE_SCRIPT.as_bytes())?;
                child.wait_with_output()
            })
            .expect("bash");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// Declaring no backup must leave no trace of one, and declaring one
    /// must layer the binary it needs. A timer that dies on a missing
    /// restic is a backup that silently is not one.
    #[test]
    fn backup_is_absent_until_declared() {
        let without = generate(&config("schema_version = 1\n[snapshots]\nenable = true\n"));
        assert!(!without.contains("kuma-backup"));
        assert!(!without.contains("kuma-restore"), "no backup means nothing to restore from");
        assert!(!without.contains("restic"));

        let with = generate(&config(
            "schema_version = 1\n[snapshots]\nenable = true\n\
             [backup]\nenable = true\nrepo = \"b2:kuma\"\n",
        ));
        assert!(with.contains("RUN systemctl enable kuma-backup.timer"));
        assert!(with.contains("restic"), "the binary the unit calls has to be in the image");
    }

    /// A remote that serves a delta this machine refuses fails the same
    /// way forever, so every download the converger does has to be able
    /// to fall back to the whole file. The install pass is the one that
    /// caught fire in the field: `--or-update` means it, not the update
    /// line, is where an already-present app takes a new version.
    #[test]
    fn every_flatpak_download_can_retry_without_deltas() {
        for path in ["install_declared || install_declared --no-static-deltas", "flatpak update"] {
            assert!(FLATPAK_SYNC_SCRIPT.contains(path), "missing download path: {path}");
        }
        let update = FLATPAK_SYNC_SCRIPT
            .split("cp \"$declared\" \"$state\"")
            .nth(1)
            .expect("the update runs after the state file is written");
        assert!(
            update.contains(
                "|| flatpak update --system --assumeyes --noninteractive --no-static-deltas"
            ),
            "the update pass must retry without deltas"
        );
        // The retry is a fallback, not the default: deltas are why an
        // update is a few megabytes instead of a few hundred.
        assert_eq!(
            FLATPAK_SYNC_SCRIPT.matches("--no-static-deltas").count(),
            2,
            "one retry per download path, and no path defaulting to whole downloads"
        );
        assert!(
            !FLATPAK_SYNC_SCRIPT.contains("install_declared --no-static-deltas\ninstall"),
            "the fallback runs only after the delta attempt failed"
        );
    }

    /// The declared file reaches the image under its scope, holding
    /// kuma's keys and nothing else, and the two units that apply it are
    /// both installed. The user unit needs `--global`, which is what
    /// puts it in a home directory without root writing into one.
    #[test]
    fn overrides_bake_per_scope_and_enable_both_passes() {
        let toml = "schema_version = 1\n\
             [system]\ndesktop = \"niri\"\n\
             [overrides.\"org.mozilla.firefox\"]\n\
             sockets = [\"wayland\"]\n\
             [overrides.\"org.gnome.Loupe\"]\n\
             scope = \"user\"\n\
             filesystems = [\"xdg-pictures:ro\"]\n";
        let out = generate(&config(toml));
        assert!(out.contains("COPY overrides /usr/lib/kuma/overrides"));
        assert!(out.contains(
            "COPY kuma-flatpak-overrides-user.service /usr/lib/systemd/user/kuma-flatpak-overrides.service"
        ));
        assert!(out.contains("systemctl --global enable kuma-flatpak-overrides.service"));

        let dir = tempfile::tempdir().unwrap();
        context(toml, dir.path());
        let system = dir.path().join("overrides/system/org.mozilla.firefox");
        let user = dir.path().join("overrides/user/org.gnome.Loupe");
        assert_eq!(std::fs::read_to_string(&system).unwrap(), "[Context]\nsockets=wayland;\n");
        assert_eq!(
            std::fs::read_to_string(&user).unwrap(),
            "[Context]\nfilesystems=xdg-pictures:ro;\n"
        );
        // scope decides the directory, and nothing lands in both
        assert!(!dir.path().join("overrides/user/org.mozilla.firefox").exists());
        assert!(!dir.path().join("overrides/system/org.gnome.Loupe").exists());
    }

    /// An image with no overrides declared still ships the converger,
    /// because the declaration that just dropped its last override is
    /// exactly the one with keys to take back. Gating on "any declared"
    /// would delete the converger in the same build that gives it its
    /// last job.
    #[test]
    fn the_override_converger_ships_even_with_nothing_declared() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("systemctl enable kuma-flatpak-overrides.service"));
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"niri\"\n", dir.path());
        assert!(dir.path().join("overrides/system").is_dir());
        assert!(dir.path().join("kuma-flatpak-overrides.service").exists());
    }

    /// Permissions are not on the daily timer, on purpose: an install
    /// arriving at a random hour is harmless, a permission reverting at
    /// a random hour is a Flatseal toggle flipping back tomorrow
    /// afternoon for no reason a person can see.
    #[test]
    fn overrides_converge_at_boot_and_never_on_the_timer() {
        assert!(FLATPAK_OVERRIDES_SERVICE.contains("WantedBy=multi-user.target"));
        assert!(FLATPAK_OVERRIDES_SERVICE.contains("After=kuma-flatpak-sync.service"));
        assert!(
            !FLATPAK_OVERRIDES_SERVICE.contains("OnCalendar")
                && !FLATPAK_SYNC_TIMER.contains("overrides"),
            "nothing may put permission convergence on a clock"
        );
        assert!(FLATPAK_OVERRIDES_USER_SERVICE.contains("WantedBy=default.target"));
        for unit in [FLATPAK_OVERRIDES_SERVICE, FLATPAK_OVERRIDES_USER_SERVICE] {
            assert!(unit.contains("Type=oneshot"));
            assert!(unit.contains("/usr/bin/kuma flatpak-overrides --scope"));
        }
    }

    #[test]
    fn flatpak_sync_removes_only_what_it_installed() {
        assert!(FLATPAK_SYNC_SCRIPT.contains("flatpak uninstall --system"));
        assert!(FLATPAK_SYNC_SCRIPT.contains(r#"done < "$state""#));
        assert!(
            !FLATPAK_SYNC_SCRIPT.contains("flatpak list"),
            "removal candidates must come from the state file, never from what is installed"
        );
        // user-level installs are personal state — never touched
        assert!(!FLATPAK_SYNC_SCRIPT.contains("--user"));
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"niri\"\n", dir.path());
        // an empty declaration is real content: take back everything
        // convergence ever installed, leaving the owner's apps alone
        assert_eq!(std::fs::read_to_string(dir.path().join("flatpaks")).unwrap(), "");
        assert!(dir.path().join("kuma-flatpak-sync").exists());
    }

    /// Both syncs keep software current without claiming it. Scoping the
    /// upgrade to the declared list is what left 13 outdated formulae,
    /// every store-installed app, and a stale runtime on a real machine
    /// whose convergence had been running daily the whole time. So the
    /// upgrade must name nothing: the moment it takes an argument it has
    /// a scope again, and everything outside that scope rots.
    ///
    /// The ordering assertions are the ones worth keeping. Both scripts
    /// run under `set -euo pipefail`, so an upgrade placed before the
    /// state file is written would, on any flaky-network run, abort the
    /// script with authority tracking still describing the previous
    /// declaration, stranding a removal until some later run happened to
    /// have working DNS.
    #[test]
    fn both_syncs_update_what_they_do_not_own() {
        // Match whole lines, not offsets into them. Searching for the
        // command and slicing from the hit hides everything to its left,
        // so a re-scoped `xargs -a "$declared" flatpak update ...` reads
        // as unscoped: the assertion holds exactly when the thing it
        // guards has been undone.
        let line_of = |script: &'static str, needle: &str| -> (usize, &'static str) {
            let mut at = 0;
            for line in script.lines() {
                if line.contains(needle) {
                    return (at, line);
                }
                at += line.len() + 1;
            }
            panic!("{needle} is gone from the sync script");
        };

        let (flatpak_update, line) = line_of(FLATPAK_SYNC_SCRIPT, "flatpak update");
        assert!(
            !line.contains("declared") && !line.contains("xargs"),
            "the update must be unscoped, or undeclared apps and runtimes never move: {line}"
        );
        let (flatpak_state, _) = line_of(FLATPAK_SYNC_SCRIPT, r#"cp "$declared" "$state""#);
        assert!(
            flatpak_state < flatpak_update,
            "authority must be recorded before the update, so a failed update cannot strand it"
        );
        // The prune runs last so runtimes the update orphans go with it.
        let (flatpak_prune, _) = line_of(FLATPAK_SYNC_SCRIPT, "--unused");
        assert!(
            flatpak_prune > flatpak_update,
            "unused runtimes are pruned after the update, not before"
        );

        let (brew_upgrade, line) = line_of(BREW_SYNC_SCRIPT, r#""$brew" upgrade"#);
        assert_eq!(
            line.trim(),
            r#""$brew" upgrade"#,
            "bare upgrade takes formulae and casks alike; an argument list takes neither"
        );
        let (brew_state, _) = line_of(BREW_SYNC_SCRIPT, r#"cp "$declared" "$state""#);
        assert!(brew_state < brew_upgrade, "same ordering rule as the flatpak sync");

        // Membership is still the declaration's alone. Widening the
        // upgrade must not widen the install: only the declared list is
        // ever installed, in both scripts.
        assert!(BREW_SYNC_SCRIPT.contains(r#"xargs -a "$declared" "$brew" install"#));
        assert!(FLATPAK_SYNC_SCRIPT.contains(r#"xargs -r -a "$declared" flatpak install"#));
    }

    #[test]
    fn user_generates_boot_sync_not_build_time_useradd() {
        let out = generate(&config(
            "schema_version = 1\n[user]\nname = \"mira\"\nshell = \"fish\"\nssh_keys = [\"ssh-ed25519 AAAA m@kuma\"]\n[packages]\nrpm = [\"fish\"]\n",
        ));
        // 600: the declaration can carry a password hash — root-only
        assert!(out.contains("COPY --chmod=600 kuma-user /usr/lib/kuma/user"));
        assert!(out.contains("RUN systemctl enable kuma-user-sync.service"));
        assert!(out.contains("COPY kuma-user-keys /etc/kuma/keys/mira"));
        assert!(out.contains("sshd_config.d/40-kuma-keys.conf"));
        // /home is machine state — the account must be created at boot
        assert!(!out.contains("useradd"));
        // the shell check comes after the rpm layer that installs it
        let rpm_at = out.find("keepcache=1 fish").unwrap();
        let check_at = out.find("RUN test -x /usr/bin/fish").unwrap();
        assert!(rpm_at < check_at);

        // A declaration with no [user] bakes no ACCOUNT, but it does ship
        // the converger. That distinction is the whole point: a published
        // image declares no account on purpose, and a machine installed
        // from one has none and no root password either, so an installer
        // writes /var/lib/kuma/user on the target and something has to act on
        // it at first boot. Shipping the unit only when the image already
        // knew the answer is what made that impossible.
        let out = generate(&config("schema_version = 1"));
        assert!(!out.contains("/usr/lib/kuma/user"), "no account data without a declared one");
        assert!(!out.contains("kuma-user-keys"));
        assert!(out.contains("RUN systemctl enable kuma-user-sync.service"));
        // ... and it is a no-op with neither file present, rather than a
        // unit that fails on every boot of a userless image.
        assert!(USER_SYNC_SCRIPT.contains("if [ -f /usr/lib/kuma/user ]"));
        assert!(USER_SYNC_SCRIPT.contains(r#"[ -n "${KUMA_USER:-}" ] || exit 0"#));
    }

    /// A shell the image does not install locks every account made on
    /// that machine out of logging in, and on a published image nothing
    /// notices until an installer has already written a disk. Same
    /// guard the declared user's shell has always had, applied to the
    /// field shareable media can actually carry.
    #[test]
    fn a_declared_system_shell_is_checked_at_build_time() {
        let out = generate(&config(
            "schema_version = 1\n[system]\nshell = \"fish\"\n[packages]\nrpm = [\"fish\"]\n",
        ));
        assert!(out.contains("RUN test -x /usr/bin/fish"));
        // After the layer that would install it, or the guard fails on
        // an image that was going to be fine.
        let rpm_at = out.find("keepcache=1 fish").unwrap();
        assert!(rpm_at < out.find("RUN test -x /usr/bin/fish").unwrap());
        // And nothing is emitted when nothing is declared.
        assert!(!generate(&config("schema_version = 1")).contains("test -x /usr/bin/"));
    }

    /// Machine state beats image content, the same way it does for
    /// hostname and timezone. On a personal image the account is
    /// declaration and gets baked; on a shared one it cannot be, because
    /// the image is shared and the person is not.
    ///
    /// /var rather than /etc, and that is the load-bearing half. bootc
    /// fills /var from the image once at install and never again, while
    /// /etc is three-way merged on every update: a file an installer
    /// shipped as image content is not a local modification, so merging
    /// against a published image that has no such file DELETES it. The
    /// account would outlive the file describing it, and the converger
    /// would quietly stop maintaining groups and shell.
    #[test]
    fn a_written_user_file_outranks_the_baked_one_and_lives_where_updates_cannot_reach() {
        // Both are sourced, machine state second, so the later
        // assignments win key by key. Whole-file precedence was wrong in
        // one direction that mattered: an image declares [system].shell
        // and no person, an installer writes a person and was told
        // nothing about shells, and skipping the image's file entirely
        // made a machine whose shell nobody had asked for.
        let usr = USER_SYNC_SCRIPT.find(". /usr/lib/kuma/user").unwrap();
        let var = USER_SYNC_SCRIPT.find(". /var/lib/kuma/user").unwrap();
        assert!(usr < var, "the machine's own file has to be sourced last");
        // The keys that describe a person do not carry over, so an image
        // that named one cannot lend its password to somebody else's
        // account.
        let unset = USER_SYNC_SCRIPT.find("unset KUMA_USER").unwrap();
        assert!(usr < unset && unset < var);
        assert!(!USER_SYNC_SCRIPT.contains("unset KUMA_SHELL"), "[system].shell is the image's");
        assert!(
            !USER_SYNC_SCRIPT.contains("/etc/kuma/user"),
            "/etc is merged on update; the installer's file would be deleted"
        );
    }

    /// /etc/hostname is image content, so writing it at boot is what
    /// makes it a local modification and therefore what survives the
    /// merge. Shipping it in the installer's layer instead reverts to the
    /// published image's hostname on the first update.
    #[test]
    fn a_written_hostname_is_applied_at_boot_not_shipped_as_image_content() {
        assert!(USER_SYNC_SCRIPT.contains("/var/lib/kuma/hostname"));
        assert!(USER_SYNC_SCRIPT.contains("> /etc/hostname"));
        // Ahead of the account guard, so a machine that named itself but
        // declared no account still gets its name.
        let host = USER_SYNC_SCRIPT.find("/var/lib/kuma/hostname").unwrap();
        let guard = USER_SYNC_SCRIPT.find(r#"[ -n "${KUMA_USER:-}" ] || exit 0"#).unwrap();
        assert!(host < guard);
    }

    #[test]
    fn autologin_adds_initial_session() {
        let with = greetd_config(&config(
            "schema_version = 1\n[user]\nname = \"mira\"\nautologin = true\n",
        ));
        assert!(with.contains("[initial_session]"));
        assert!(with.contains("user = \"mira\""));
        // greeter remains the fallback for logout
        assert!(with.contains("[default_session]"));

        let without = greetd_config(&config("schema_version = 1\n[user]\nname = \"mira\"\n"));
        assert!(!without.contains("[initial_session]"));
    }

    /// Mod+D opens the menu, and the stock line it replaces is grepped
    /// for first. A niri release that renames that line must fail the
    /// build rather than ship media whose main key does nothing.
    #[test]
    fn the_launcher_key_opens_the_menu_and_fails_loudly_if_it_cannot() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(
            out.contains(&format!("grep -qF '{NIRI_STOCK_LAUNCHER}'")),
            "the stock bind is a gate"
        );
        assert!(
            out.contains(&format!("s|{NIRI_STOCK_LAUNCHER}|{NIRI_MENU_BIND}|")),
            "and is replaced"
        );
        assert!(NIRI_MENU_BIND.contains("Mod+D"), "the key is the one the hand already goes to");
        assert!(!NIRI_MENU_BIND.contains("fuzzel"), "plain fuzzel is no longer what it opens");
        assert!(
            !NIRI_MEDIA_BINDS.contains("kuma\" \"menu"),
            "one key for the menu, not a second chord beside it"
        );
        // The stock line is what niri actually ships, not a guess: this
        // is the pairing that makes the grep meaningful.
        assert!(NIRI_STOCK_LAUNCHER.contains(r#"spawn "fuzzel";"#));
    }

    /// Suspending locks, which was a clause of the swayidle line that
    /// left. The contract now lives in the shell's compiled defaults —
    /// `lock_before_suspend = true` beside the 900/960 idle timeouts,
    /// pinned by kuma-shell's own test — so the image-side assertion is
    /// that the binary carrying it is the one this image ships.
    #[test]
    fn suspending_locks_the_screen() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("COPY --chmod=755 kuma-shell /usr/bin/kuma-shell"));
        assert!(SHELL_SERVICE.contains("ExecStart=/usr/bin/kuma-shell"));
    }

    /// Every icon an entry names is checked in the build, not the first.
    #[test]
    fn the_build_checks_every_icon_the_entries_name() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        for entry in seam::ENTRIES {
            assert!(
                out.contains(&format!("-name {}.svg", entry.icon)),
                "{} names {}, which the build never looks for",
                entry.id,
                entry.icon
            );
        }
    }

    // The shell's fallback wallpaper is kuma's — and with noctalia gone
    // there is no other wallpaper in the image to fall back FROM: the
    // file the COPY above ships is the only one, and the shell's
    // compiled default names exactly that path (pinned in kuma-shell's
    // own tests). Nothing to replace and no key pretending to work.

    /// No bind advertises a program the image does not have.
    ///
    /// The lock bind is the one that matters most: niri puts it on the
    /// Important Hotkeys overlay that opens at first login, so a dead key
    /// there is the first thing a new machine tells a person to press.
    /// It went dead the moment swaylock left the set, silently, and it
    /// took looking at a booted VM to notice.
    #[test]
    fn no_bind_names_a_program_the_image_excludes() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        // grepped for before the rewrite, so a niri release that rewords
        // the line fails the build instead of shipping the dead key
        assert!(out.contains(&format!("grep -qF '{NIRI_STOCK_LOCK}'")), "unguarded rewrite");
        assert!(out.contains(&format!("s|{NIRI_STOCK_LOCK}|{NIRI_LOCK_BIND}|")), "not rewritten");
        // Spawn lines only. The prose in NIRI_EXTRAS names every program
        // the shell replaced, which is the point of it.
        let spawns = [NIRI_MENU_BIND, NIRI_LOCK_BIND, NIRI_MEDIA_BINDS, NIRI_EXTRAS]
            .iter()
            .flat_map(|block| block.lines())
            .map(str::trim)
            .filter(|line| !line.starts_with("//") && line.contains("spawn"));
        for line in spawns {
            for excluded in NIRI_EXCLUDES {
                assert!(
                    !line.contains(excluded),
                    "this spawns {excluded}, which the image excludes: {line}"
                );
            }
        }
    }

    #[test]
    fn every_kuma_verb_a_keybinding_spawns_is_a_real_verb() {
        use clap::CommandFactory;
        let cli = crate::Cli::command();
        let verbs: Vec<String> =
            cli.get_subcommands().map(|sub| sub.get_name().to_string()).collect();
        let mut found = 0;
        for bind in [NIRI_MEDIA_BINDS, NIRI_EXTRAS, NIRI_MENU_BIND] {
            for (index, _) in bind.match_indices(r#"spawn "kuma""#) {
                let rest = &bind[index..];
                let verb =
                    rest.split('"').nth(3).expect("a spawn line names a verb after the program");
                assert!(
                    verbs.contains(&verb.to_string()),
                    "a keybinding spawns `kuma {verb}`, which is not a verb"
                );
                found += 1;
            }
        }
        // No floor on `found` any more. kuma's verbs left the keybindings
        // when Mod+D became the shell's launcher; they are desktop
        // entries now, and `seam::tests::every_entry_names_a_real_verb`
        // is what keeps THOSE from naming a verb that does not exist.
        // This still runs because a bind naming a kuma verb may come
        // back, and the dead-key bug it was written for is cheap to
        // re-introduce and invisible when it happens.
        let _ = found;
    }

    #[test]
    fn session_polish_ships_battery_watch_and_media_verb_binds() {
        // Locking, screen-off and lock-before-suspend were swayidle
        // arguments, then noctalia config keys; they are the shell's
        // compiled defaults now (kuma-shell's own tests pin the
        // numbers), so there is no image-side config left to assert.
        // What the image still owns is the wiring: the battery watch
        // rides the session, and the media keys reach the shell.
        assert!(NIRI_EXTRAS.contains("kuma-battery-watch"));
        // Both X11 helpers wait for a DISPLAY that does not exist yet
        // when they are spawned. They share one copy of that wait, so
        // this asks the rendered scripts rather than the const: a
        // refactor that dropped it from one of them would leave a
        // helper that exits on every login and reports nothing.
        for script in [xsettings_launcher(), clipboard_bridge()] {
            assert!(script.contains("systemctl --user show-environment"), "{script}");
            assert!(script.trim_end().ends_with("-x") || script.contains("exec xsettingsd"));
        }
        // media keys go through the shell's own msg interface, which
        // adjusts through the same sysmon the bar's widgets read from
        assert!(NIRI_MEDIA_BINDS.contains("spawn \"kuma-shell\" \"msg\" \"volume-up\""));
        assert!(NIRI_MEDIA_BINDS.contains("spawn \"kuma-shell\" \"msg\" \"brightness-up\""));
        assert!(!NIRI_MEDIA_BINDS.contains("playerctl"));
        assert!(!NIRI_MEDIA_BINDS.contains("kuma-osd"));
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("-e '/XF86Audio/d'"));
        assert!(out.contains("r /usr/lib/kuma/niri-binds.kdl"));
    }

    /// The baked defaults name apps the image actually has: flatpak ids
    /// for the ones the example declares, native ids for the ones the
    /// desktop set installs. A handler pointing at an app nobody installs
    /// is a link that opens nothing, which is how the default browser sat
    /// on Chromium while every real declaration shipped Firefox.
    ///
    /// Native desktop ids look exactly like flatpak ids, so the handlers
    /// the desktop set installs are named here rather than inferred.
    /// Everything else in the list has to come from the declaration —
    /// docs/desktops.md explains the desktop members whose presence is
    /// not self-evident, and every one of them is a failure someone had
    /// to diagnose: a silent keyring, a disabled clock, no swap, tofu.
    /// Dropping one from the set while the page still explains it turns
    /// the page into a lie about the image, and the reverse hides the
    /// only written record of why the package is there at all.
    ///
    /// Only these are pinned, deliberately. The full inventory's home is
    /// `kuma generate`, and asking a doc to track seventy packages would
    /// buy drift in exchange for churn.
    #[test]
    fn the_surprising_desktop_packages_are_the_ones_documented() {
        const EXPLAINED: &[&str] = &[
            "gnome-keyring-pam",
            "nss-mdns",
            "zram-generator-defaults",
            "glibc-langpack-en",
            "mesa-vulkan-drivers",
            "vulkan-loader",
            "avahi",
        ];
        // niri-only: COSMIC's set has no waybar to lose its glyphs, and
        // fonts follow the arm that renders with them.
        const EXPLAINED_NIRI: &[&str] = &["fontawesome-fonts-all", "google-noto-sans-cjk-vf-fonts"];

        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/desktops.md"))
            .unwrap();
        // Scoped to the section that does the explaining, not the whole
        // page. Every package is also named in the inventory tables, so
        // searching the file would assert only that the word appears
        // somewhere, which stays true precisely when the explanation is
        // the thing that got deleted.
        let why = doc
            .split_once("## Why these are here")
            .and_then(|(_, rest)| rest.split_once("\n## "))
            .map(|(section, _)| section)
            .expect("docs/desktops.md explains the non-obvious members");

        for pkg in EXPLAINED {
            assert!(NIRI_PACKAGES.contains(pkg), "{pkg} left the niri set");
            assert!(COSMIC_PACKAGES.contains(pkg), "{pkg} left the COSMIC set");
            assert!(why.contains(pkg), "docs/desktops.md stopped explaining {pkg}");
        }
        for pkg in EXPLAINED_NIRI {
            assert!(NIRI_PACKAGES.contains(pkg), "{pkg} left the niri set");
            assert!(why.contains(pkg), "docs/desktops.md stopped explaining {pkg}");
        }
    }

    /// and the declaration to check is the niri one, because this file
    /// is only baked on the niri arm.
    #[test]
    fn the_baked_defaults_name_apps_the_examples_install() {
        // shipped by the image itself, never by a declaration:
        // kuma-files rides the sibling road, file-roller NIRI_PACKAGES
        const IN_IMAGE: &[&str] = &["kuma-files", "org.gnome.FileRoller"];
        let example =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/niri.toml"))
                .unwrap();
        let declared: Config = toml::from_str(&example).unwrap();
        let mut checked = 0;
        for line in MIMEAPPS.lines() {
            let Some((mime, handler)) = line.split_once('=') else {
                continue; // the [Default Applications] header
            };
            let app = handler.strip_suffix(".desktop").expect("a desktop id");
            if IN_IMAGE.contains(&app) {
                continue;
            }
            assert!(
                declared.packages.flatpak.iter().any(|a| a == app),
                "{app} handles {mime} but examples/niri.toml doesn't install it"
            );
            checked += 1;
        }
        assert!(checked > 0, "nothing was checked: has MIMEAPPS changed shape?");
        // the handler that actually went stale once, still named explicitly
        assert!(MIMEAPPS.contains("x-scheme-handler/https=org.mozilla.firefox.desktop"));
    }

    /// The example's `disable` line must not name a unit kuma's desktop
    /// deliberately enables, or the file argues with the image it
    /// compiles to. It read `avahi-daemon.service` when this was written,
    /// and the obvious replacement (`bluetooth.service`) was enabled by
    /// kuma too, which is how the mistake got made twice: the unit has to
    /// be chosen against kuma's curation, not against the base's defaults.
    #[test]
    fn the_disable_example_does_not_fight_the_desktop() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        // EVERY enabling line, not the first one. There is more than one
        // now (the shell's user unit and the sleep guard have their own),
        // and picking the first made this test read a line it was not
        // written about and fail on a tree that was correct.
        let enabled: Vec<&str> = out
            .lines()
            .filter(|line| line.contains("systemctl enable "))
            .flat_map(|line| line.split_whitespace())
            .filter(|word| word.ends_with(".service"))
            .collect();
        assert!(!enabled.is_empty(), "the desktop arm enables units");
        assert!(enabled.contains(&"avahi-daemon.service"), "sanity: kuma enables avahi");

        let example =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/niri.toml"))
                .unwrap();
        for line in example.lines() {
            let line = line.trim_start().trim_start_matches('#').trim_start();
            let Some(units) = line.strip_prefix("disable = [") else { continue };
            for unit in units.split(['"', ',', ']']).filter(|u| u.ends_with(".service")) {
                assert!(
                    !enabled.contains(&unit),
                    "the example suggests disabling {unit}, which kuma's desktop enables"
                );
            }
        }
    }

    #[test]
    fn daily_driver_glue() {
        // The clipboard and wallpaper binds left with noctalia — the
        // panels they opened were noctalia's, and a bind that advertises
        // a dead panel is the bug the icon test exists for. Their keys
        // are free; a shell panel for either is new work.
        assert!(!NIRI_MEDIA_BINDS.contains(r#"panel-toggle" "clipboard"#));
        assert!(!NIRI_MEDIA_BINDS.contains(r#"panel-toggle" "wallpaper"#));
        assert!(MIMEAPPS.contains("application/pdf=org.gnome.Papers.desktop"));
        assert!(MIMEAPPS.contains("inode/directory=kuma-files.desktop"));
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(out.contains("COPY mimeapps.list /etc/xdg/mimeapps.list"));
        assert!(out.contains("pam_gnome_keyring"));
    }

    #[test]
    fn infra_round_swap_discovery_updates() {
        let out = generate(&config(
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n[packages]\nflatpak = [\"org.gnome.Loupe\"]\n",
        ));
        assert!(out.contains("zram-generator-defaults"));
        assert!(out.contains("avahi-daemon.service chronyd.service"));
        assert!(out.contains("kuma-flatpak-sync.service kuma-flatpak-sync.timer"));
        assert!(FLATPAK_SYNC_TIMER.contains("Persistent=true"));
        // do-not-disturb left with mako; the control centre owns it.
        assert!(!NIRI_MEDIA_BINDS.contains("makoctl"), "no daemon behind that key");
        assert!(NIRI_MEDIA_BINDS.contains("kuma-record"));
        // the XF86Audio sed deletes niri's stock playerctl binds; these
        // re-additions ride the bind-splice file, immune to that sed
        assert!(NIRI_MEDIA_BINDS.contains("play-pause"));
        assert!(NIRI_MEDIA_BINDS.contains("swappy"));
        assert!(NIRI_EXTRAS.contains("spawn-at-startup \"udiskie\""));
    }

    #[test]
    fn context_writes_user_declaration() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[user]\nname = \"mira\"\nshell = \"fish\"\npassword_hash = \"$6$ab$cd\"\nssh_keys = [\"ssh-ed25519 AAAA m@kuma\"]\n", dir.path());
        let decl = std::fs::read_to_string(dir.path().join("kuma-user")).unwrap();
        assert_eq!(
            decl,
            "KUMA_USER='mira'\nKUMA_SHELL='/usr/bin/fish'\nKUMA_GROUPS='wheel'\nKUMA_PASSWORD_HASH='$6$ab$cd'\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("kuma-user-keys")).unwrap(),
            "ssh-ed25519 AAAA m@kuma\n"
        );
        let script = std::fs::read_to_string(dir.path().join("kuma-user-sync")).unwrap();
        assert!(script.contains("chpasswd -e"));
        assert!(script.contains("usermod -aG"));
    }

    /// `shell` is the one [user] field with two code paths, and only the
    /// declared one was covered. Undeclared has to mean *absent*, not
    /// empty: kuma-user-sync guards both its useradd -s and its usermod -s
    /// on `[ -n "${KUMA_SHELL:-}" ]`, so a `KUMA_SHELL=''` line would
    /// still take the guard's false branch, but `usermod -s ''` is what a
    /// later reader would "fix" it into. Pinning absence keeps the two
    /// halves honest about which one is load-bearing.
    #[test]
    fn a_user_with_no_declared_shell_pins_nothing_about_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[user]\nname = \"mira\"\n", dir.path());
        let decl = std::fs::read_to_string(dir.path().join("kuma-user")).unwrap();
        assert_eq!(decl, "KUMA_USER='mira'\nKUMA_GROUPS='wheel'\n");

        // The build-time guard exists to fail a declaration naming a shell
        // the packages never install. With no shell named there is nothing
        // to install and nothing to check, and a stray `test -x /usr/bin/`
        // would fail every build.
        let out = generate(&config("schema_version = 1\n[user]\nname = \"mira\"\n"));
        assert!(!out.contains("test -x /usr/bin/"));
        // The account is still converged; only the shell claim is dropped.
        assert!(out.contains("RUN systemctl enable kuma-user-sync.service"));
    }

    /// The default is the whole point of the field: a declaration that
    /// names a user and stops must still produce an account that can
    /// administer the machine, because nothing else in the declaration
    /// grants sudo. Pinned here because "groups defaults to wheel" reads
    /// like a convenience and is the only path to root on a fresh install.
    #[test]
    fn a_user_who_declares_no_groups_still_lands_in_wheel() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[user]\nname = \"mira\"\n", dir.path());
        let decl = std::fs::read_to_string(dir.path().join("kuma-user")).unwrap();
        assert!(decl.contains("KUMA_GROUPS='wheel'\n"));

        // Asking for none is a different answer from asking for nothing,
        // and the sync script iterates `${KUMA_GROUPS:-}`, so the line has
        // to be gone rather than empty for the loop to run zero times.
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[user]\nname = \"mira\"\ngroups = []\n", dir.path());
        let decl = std::fs::read_to_string(dir.path().join("kuma-user")).unwrap();
        assert!(!decl.contains("KUMA_GROUPS"));
    }

    /// The hostname must ship as a COPY, never a RUN redirect: buildah
    /// bind-mounts /etc/hostname into RUN containers, so a redirect
    /// writes the runtime mount and the image ships no hostname at all —
    /// which is exactly what every image did until 2026-08.
    #[test]
    fn hostname_and_locale_pins() {
        let out = generate(&config(
            "schema_version = 1\n[system]\nhostname = \"kuma-laptop\"\nlocale = \"de_DE.UTF-8\"\n",
        ));
        assert!(out.contains("COPY hostname /etc/hostname"));
        assert!(!out.contains("> /etc/hostname"));
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\nhostname = \"kuma-laptop\"\n", dir.path());
        assert_eq!(std::fs::read_to_string(dir.path().join("hostname")).unwrap(), "kuma-laptop\n");
        // undeclared, the default seeds the ostree merge default
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n", dir.path());
        assert_eq!(std::fs::read_to_string(dir.path().join("hostname")).unwrap(), "kumaos\n");
        assert!(out.contains(&dnf_install("glibc-langpack-de")));
        assert!(out.contains("RUN echo 'LANG=de_DE.UTF-8' > /etc/locale.conf"));
        // C.UTF-8 has no territory, so no langpack layer
        let out = generate(&config("schema_version = 1\n[system]\nlocale = \"C.UTF-8\"\n"));
        assert!(!out.contains("glibc-langpack"));
        assert!(out.contains("LANG=C.UTF-8"));
    }

    #[test]
    fn context_includes_flatpak_list() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[packages]\nflatpak = [\"org.mozilla.firefox\", \"org.gnome.Loupe\"]\n", dir.path());
        let list = std::fs::read_to_string(dir.path().join("flatpaks")).unwrap();
        assert_eq!(list, "org.mozilla.firefox\norg.gnome.Loupe\n");
        let script = std::fs::read_to_string(dir.path().join("kuma-flatpak-sync")).unwrap();
        // remote pinned: multiple remotes offering the same ref would make
        // non-interactive installs fail. Asserted as a property of the
        // install line rather than as adjacent words, which broke the
        // moment a flag was passed through to it: the remote is the last
        // word because xargs appends the app names after it.
        let install = script
            .lines()
            .find(|l| l.contains("flatpak install --system"))
            .expect("the declared list has to be installed by something");
        assert!(install.contains("--or-update"));
        assert!(
            install.trim_end().ends_with("flathub"),
            "the install must name exactly one remote: {install}"
        );
    }

    #[test]
    fn brew_generates_setup_service_and_shell_profiles() {
        let out = generate(&config("schema_version = 1\n[system]\nbrew = true\n"));
        assert!(out.contains("git-core"));
        assert!(out.contains("COPY --chmod=755 kuma-brew-setup /usr/libexec/kuma-brew-setup"));
        assert!(out.contains("systemctl enable kuma-brew-setup.service"));
        assert!(out.contains("/etc/profile.d/kuma-brew.sh"));
        assert!(out.contains("/etc/fish/conf.d/kuma-brew.fish"));

        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\nbrew = true\n", dir.path());
        let script = std::fs::read_to_string(dir.path().join("kuma-brew-setup")).unwrap();
        assert!(script.contains("/home/linuxbrew/.linuxbrew"));
        assert!(dir.path().join("kuma-brew-setup.service").exists());
    }

    #[test]
    fn declared_brews_imply_bootstrap_and_converge() {
        // no system.brew = true needed: the list alone pulls in the bootstrap
        let toml = "schema_version = 1\n[packages]\nbrew = [\"ripgrep\", \"node@22\"]\n";
        let out = generate(&config(toml));
        assert!(out.contains("kuma-brew-setup.service kuma-brew-sync.service kuma-brew-sync.timer"));
        assert!(out.contains("COPY brews /usr/lib/kuma/brews"));

        let dir = tempfile::tempdir().unwrap();
        context(toml, dir.path());
        let list = std::fs::read_to_string(dir.path().join("brews")).unwrap();
        assert_eq!(list, "ripgrep\nnode@22\n");
        let script = std::fs::read_to_string(dir.path().join("kuma-brew-sync")).unwrap();
        // removal authority is scoped to the state file, never `brew list`:
        // ad-hoc installs on the machine must survive convergence
        assert!(script.contains(".kuma-brews"));
        assert!(BREW_SYNC_SERVICE.contains("User=1000"));
        assert!(BREW_SYNC_TIMER.contains("Persistent=true"));
    }

    #[test]
    fn no_brew_by_default() {
        let out = generate(&config("schema_version = 1"));
        assert!(!out.contains("brew"));
    }

    /// The gate's whole decision table, run for real: a stub sysfs tree
    /// and a stub busctl on PATH answer each case, and the stamp and
    /// marker land in a tempdir. The table is the test plan's AC/battery/
    /// metered matrix; each row is a machine state a laptop actually
    /// reaches.
    #[test]
    fn the_convergence_gate_skips_exactly_the_states_it_should() {
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("kuma-converge-gate");
        std::fs::write(&gate, CONVERGE_GATE_SCRIPT).unwrap();
        std::fs::set_permissions(&gate, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        // A stub busctl whose answer the case writes: the only thing the
        // real one is asked for is the Metered property's value.
        let stubs = dir.path().join("bin");
        std::fs::create_dir(&stubs).unwrap();
        std::fs::write(stubs.join("busctl"), "#!/bin/sh\ncat \"$STUB_ANSWER\"\n").unwrap();
        std::fs::set_permissions(
            stubs.join("busctl"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let answer = dir.path().join("answer");
        std::fs::write(&answer, "u 0").unwrap();

        let battery = |status: &str, capacity: &str| -> std::path::PathBuf {
            let sysfs = dir.path().join("sys").join(format!("{status}-{capacity}"));
            let bat = sysfs.join("BAT1");
            std::fs::create_dir_all(&bat).unwrap();
            std::fs::write(bat.join("type"), "Battery").unwrap();
            std::fs::write(bat.join("status"), status).unwrap();
            std::fs::write(bat.join("capacity"), capacity).unwrap();
            sysfs
        };

        let run = |sysfs: &std::path::Path| {
            let stamp =
                dir.path().join(format!("stamp-{}", sysfs.file_name().unwrap().to_str().unwrap()));
            let out = Command::new(&gate)
                .arg(sysfs)
                .arg(&stamp)
                .arg(
                    dir.path()
                        .join(format!("marker-{}", sysfs.file_name().unwrap().to_str().unwrap())),
                )
                .env("PATH", format!("{}:{}", stubs.display(), std::env::var("PATH").unwrap()))
                .env("STUB_ANSWER", &answer)
                .output()
                .unwrap();
            let stamped = std::fs::read_to_string(&stamp).unwrap_or_default();
            (out.status.code().unwrap_or(-1), stamped)
        };

        // A desktop with no power_supply directory at all proceeds, and
        // stamps nothing: the wall is the only power state it has.
        let none = dir.path().join("sys").join("none");
        std::fs::create_dir_all(&none).unwrap();
        assert_eq!(run(&none), (0, String::new()));

        // Discharging below the threshold: the one battery state that
        // waits, with the reading in the reason.
        let (code, stamped) = run(&battery("Discharging", "15"));
        assert_eq!(code, 0, "a skip is a decision, answered green");
        assert!(stamped.contains("on battery (15%)"), "{stamped}");

        // The same battery above it runs.
        assert_eq!(run(&battery("Discharging", "45")), (0, String::new()));

        // Charging, full and unknown statuses are all the wall's answer,
        // whatever the capacity says.
        assert_eq!(run(&battery("Charging", "15")), (0, String::new()));
        assert_eq!(run(&battery("Full", "80")), (0, String::new()));
        assert_eq!(run(&battery("Unknown", "3")), (0, String::new()));

        // A metered connection waits, whatever the power state.
        std::fs::write(&answer, "u 1").unwrap();
        let (code, stamped) = run(&battery("Full", "80"));
        assert_eq!(code, 0);
        assert!(stamped.contains("on a metered connection"), "{stamped}");
        // A guessed yes is a yes; unknown (0) and a failed read are not.
        std::fs::write(&answer, "u 3").unwrap();
        assert!(run(&battery("Full", "80")).1.contains("metered"));
        std::fs::write(&answer, "u 0").unwrap();
        assert_eq!(run(&battery("Full", "80")), (0, String::new()));
        let stamp = dir.path().join("stamp-nobusctl");
        let nobusctl_path = "/usr/bin:/bin"; // coreutils, no stub: busctl is absent
        let out = Command::new(&gate)
            .arg(battery("Full", "80"))
            .arg(&stamp)
            .arg(dir.path().join("marker-nobusctl"))
            .env("PATH", nobusctl_path)
            .output()
            .unwrap();
        assert_eq!(out.status.code().unwrap(), 0, "a missing busctl is not a skip");
        assert!(!stamp.exists(), "and it stamps nothing");

        // Both at once: one line, both reasons.
        std::fs::write(&answer, "u 1").unwrap();
        let (_, stamped) = run(&battery("Discharging", "10"));
        assert!(stamped.contains("on battery (10%)"), "{stamped}");
        assert!(stamped.contains("on a metered connection"), "{stamped}");

        // A proceed clears the skips it is about to answer.
        let stale = dir.path().join("stale-stamp");
        std::fs::write(&stale, "2026-09-01T00:00:00Z on battery (5%)").unwrap();
        std::fs::write(&answer, "u 0").unwrap();
        let _ = Command::new(&gate)
            .arg(battery("Full", "80"))
            .arg(&stale)
            .arg(dir.path().join("marker-stale"))
            .env("PATH", format!("{}:{}", stubs.display(), std::env::var("PATH").unwrap()))
            .env("STUB_ANSWER", &answer)
            .output()
            .unwrap();
        assert!(!stale.exists(), "convergence happening makes old skips stale");
    }

    /// The gate is the timer's and never the verb's: structurally, not
    /// by convention. The timers keep their names — a machine updated
    /// into this release keeps its enabled timer and gains the gate with
    /// it — and fire the daily units, which are the only ones that run
    /// the gate. The boot services and the scripts they share with
    /// `kuma sync` carry the marker check alone, which answers nothing
    /// when the gate never ran.
    #[test]
    fn the_convergence_gate_rides_the_daily_units_only() {
        let toml = "schema_version = 1\n[packages]\nflatpak = [\"org.foo.Bar\"]\n";
        let out = generate(&config(toml));
        assert!(out.contains("COPY --chmod=755 kuma-converge-gate /usr/libexec/kuma-converge-gate"));
        assert!(out.contains("COPY kuma-flatpak-sync-daily.service /usr/lib/systemd/system/"));
        assert!(out.contains("kuma-flatpak-sync.service kuma-flatpak-sync.timer"));

        // The timer keeps its name and name its new target.
        assert!(FLATPAK_SYNC_TIMER.contains("Unit=kuma-flatpak-sync-daily.service"));
        assert!(BREW_SYNC_TIMER.contains("Unit=kuma-brew-sync-daily.service"));
        // The daily unit gates; the boot unit does not.
        assert!(FLATPAK_SYNC_DAILY_SERVICE.contains("ExecStartPre=/usr/libexec/kuma-converge-gate"));
        assert!(!FLATPAK_SYNC_SERVICE.contains("ExecStartPre"));
        // The brew gate runs as root for the stamp's sake, though the
        // sync itself is uid 1000.
        assert!(BREW_SYNC_DAILY_SERVICE.contains("ExecStartPre=+/usr/libexec/kuma-converge-gate"));
        // And both sync scripts honour the marker, so a skipped day ends
        // green having installed nothing.
        assert!(FLATPAK_SYNC_SCRIPT.contains("/run/kuma-converge-gate/skip"));
        assert!(BREW_SYNC_SCRIPT.contains("/run/kuma-converge-gate/skip"));
    }

    #[test]
    fn context_includes_greetd_config_for_niri() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"niri\"\n", dir.path());
        assert!(dir.path().join("Containerfile").exists());
        let greetd = std::fs::read_to_string(dir.path().join("greetd-config.toml")).unwrap();
        assert!(greetd.contains("niri-session"));
        let kargs = std::fs::read_to_string(dir.path().join("kargs-desktop.toml")).unwrap();
        assert!(kargs.contains("quiet"));
    }

    #[test]
    fn cosmic_desktop_composes_from_the_session() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"));
        assert!(out.contains("cosmic-session"));
        // Fedora's cosmic-greeter.service owns the display-manager alias;
        // enabling greetd.service alongside it fails the build
        assert!(out.contains("systemctl enable cosmic-greeter.service"));
        assert!(!out.contains("enable greetd.service"));
        // kuma declares the user — the first-boot wizard must not fire
        assert!(out.contains("rm /etc/xdg/autostart/com.system76.CosmicInitialSetup.desktop"));
        // the session pulls only the pipewire library; the daemon is on us
        assert!(out.contains("pipewire"));
        // kuma-files rides cosmic too: the full libav build swaps in here
        // the same as the niri arm
        assert!(out.contains("--allowerasing ffmpeg-libs"));
        // the store would fight convergence — its installs get removed daily
        assert!(!out.contains("cosmic-store"));
        // the default dock pins the editor; the session alone doesn't pull it
        assert!(out.contains("cosmic-edit"));
        // wallpaper is identity, and the packaged dock/background defaults
        // are overwritten in place, guarded so a moved path fails the build
        assert!(
            out.contains("COPY kuma-wallpaper.jpg /usr/share/backgrounds/kuma/kuma-wallpaper.jpg")
        );
        assert!(out.contains("test -f /usr/share/cosmic/com.system76.CosmicAppList/v1/favorites"));
        assert!(out.contains(
            "COPY cosmic-favorites /usr/share/cosmic/com.system76.CosmicAppList/v1/favorites"
        ));
        assert!(out.contains(
            "COPY cosmic-background /usr/share/cosmic/com.system76.CosmicBackground/v1/all"
        ));
        // the baked dock pins only what the image ships: no store, and no
        // browser — that's the declaration's choice
        assert!(!COSMIC_FAVORITES.contains("Store"));
        assert!(!COSMIC_FAVORITES.contains("firefox"));
        // flathub remote ships in-image, same as the niri desktop
        assert!(out.contains("flathub.flatpakrepo"));
        assert!(out.contains("set-default graphical.target"));
        // codec restoration applies to every desktop, not just niri
        assert!(out.contains("mesa-va-drivers-freeworld"));
        // AMD DCC-on-scanout static bands: both knobs baked, not a
        // hand-edit on the installed machine — overlay alone recurred
        assert!(out.contains("COSMIC_DISABLE_OVERLAY_SCANOUT=1"));
        assert!(out.contains("COSMIC_DISABLE_DIRECT_SCANOUT=1"));
    }

    #[test]
    fn cosmic_autologin_appends_initial_session() {
        let with = generate(&config(
            "schema_version = 1\n[system]\ndesktop = \"cosmic\"\n[user]\nname = \"mira\"\nautologin = true\n",
        ));
        assert!(with.contains("[initial_session]"));
        assert!(with.contains("command = \"start-cosmic\""));
        assert!(with.contains("user = \"mira\""));
        // appended to the config cosmic-greeter.service actually reads
        assert!(with.contains(">> /etc/greetd/cosmic-greeter.toml"));
        let without = generate(&config(
            "schema_version = 1\n[system]\ndesktop = \"cosmic\"\n[user]\nname = \"mira\"\n",
        ));
        assert!(!without.contains("initial_session"));
    }

    /// The keyring failed silently on both desktops until 2026-08-07:
    /// gnome-keyring was installed but gnome-keyring-pam, which carries
    /// the module, was not, and the greeter's '-' prefixed PAM lines
    /// skip a missing module without logging a word. Nothing observable
    /// broke except that every keyring-using app prompted on launch, so
    /// the package and the assert are pinned here per desktop.
    #[test]
    fn keyring_unlocks_with_the_login_password_on_every_desktop() {
        let niri = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        let cosmic = generate(&config("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"));
        for out in [&niri, &cosmic] {
            // the module is a subpackage; nothing else pulls it in
            assert!(out.contains("gnome-keyring-pam"));
            // and the build fails if a Fedora update stops shipping it
            assert!(out.contains("test -f /usr/lib64/security/pam_gnome_keyring.so"));
        }
        // each greeter authenticates against its own PAM service, and
        // asserting the other one would pass while proving nothing. The
        // stack is asserted in either directory, because Fedora 45 moved
        // vendor stacks to /usr/lib/pam.d and the assert reads both
        // (see keyring_pam)
        assert!(niri.contains(
            "grep -q pam_gnome_keyring /etc/pam.d/greetd /usr/lib/pam.d/greetd 2>/dev/null\n"
        ));
        assert!(!niri.contains("/etc/pam.d/cosmic-greeter"));
        assert!(cosmic.contains(
            "grep -q pam_gnome_keyring /etc/pam.d/cosmic-greeter /usr/lib/pam.d/cosmic-greeter 2>/dev/null\n"
        ));
        // greetd's file exists in the COSMIC image too (cosmic-greeter
        // pulls greetd in), so a stale assert there would look healthy
        assert!(!cosmic.contains("pam_gnome_keyring /etc/pam.d/greetd\n"));
    }

    /// Ownership of an /etc file is "this build writes it", and the two
    /// ways to write one are a COPY destination and a shell redirect.
    /// Reading is not owning, which is the distinction the whole /etc
    /// drift check rests on: kuma greps Fedora's /etc/pam.d/greetd and
    /// validates its own /etc/niri/config.kdl, and only the second is
    /// kuma's to have an opinion about.
    #[test]
    fn etc_ownership_is_writes_not_reads() {
        let paths = etc_writes(
            "COPY greetd-config /etc/greetd/config.toml\n\
             COPY --chmod=600 kuma-user /usr/lib/kuma/user\n\
             RUN sed -e 's/x/y/' /usr/share/doc/niri/default.kdl > /etc/niri/config.kdl \\\n\
             RUN cat /usr/lib/kuma/extras.kdl >> /etc/niri/config.kdl\n\
             RUN printf 'A=1\\n' >> /etc/environment\n\
             RUN test -f /usr/lib64/security/pam_gnome_keyring.so \\\n\
             RUN grep -q pam_gnome_keyring /etc/pam.d/greetd\n\
             RUN niri validate --config /etc/niri/config.kdl\n\
             RUN something 2>/dev/null\n",
        );
        assert_eq!(paths, ["/etc/environment", "/etc/greetd/config.toml", "/etc/niri/config.kdl"]);
        // read-only mentions never become ownership, and a non-/etc
        // destination or redirect is not this check's business
        assert!(!paths.iter().any(|p| p.contains("pam.d")));
        assert!(!paths.iter().any(|p| p.contains("dev/null")));

        // The third way, which is how a declared timezone is written.
        // The `test -e` guard in front of it is why the destination has
        // to be the segment's last word rather than the line's.
        let linked = etc_writes(
            "RUN test -e /usr/share/zoneinfo/America/Denver && ln -sfn /usr/share/zoneinfo/America/Denver /etc/localtime\n\
             RUN ln -sfn /usr/lib/systemd/system/foo.service /usr/lib/systemd/system/bar.service\n",
        );
        assert_eq!(linked, ["/etc/localtime"]);
    }

    /// A trusted CA is state that survives every rebuild and that
    /// nothing could declare: the machine this was written on carries a
    /// hand-added anchor for its own infrastructure, and a reinstall
    /// would have lost it silently. Declaring it makes the image carry
    /// it, and because the anchor lands under /etc by a COPY, doctor
    /// watches it for free.
    #[test]
    fn a_declared_ca_anchor_is_baked_trusted_and_watched() {
        let pem = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAKZ\n-----END CERTIFICATE-----\n";
        let toml = format!(
            "schema_version = 1\n[system.ca_certificates]\n\"my-root-ca\" = \"\"\"\n{pem}\"\"\"\n"
        );
        let declared = config(&toml);
        let out = generate(&declared);
        assert!(
            out.contains("COPY ca-my-root-ca.crt /etc/pki/ca-trust/source/anchors/my-root-ca.crt"),
            "{out}"
        );
        // extracted in the same layer that adds it: a trust store that
        // needs a boot to become true is one that is false in the image
        let copied = out.find("anchors/my-root-ca.crt").unwrap();
        let extracted = out.find("RUN update-ca-trust").unwrap();
        assert!(copied < extracted, "update-ca-trust runs before the anchor lands");

        let dir = tempfile::tempdir().unwrap();
        context(&toml, dir.path());
        assert_eq!(std::fs::read_to_string(dir.path().join("ca-my-root-ca.crt")).unwrap(), pem);

        // owned, therefore graded: no separate wiring needed
        assert!(etc_paths(&declared)
            .iter()
            .any(|p| p == "/etc/pki/ca-trust/source/anchors/my-root-ca.crt"));

        // and an image that declares none says nothing about trust
        let bare = config("schema_version = 1\n");
        assert!(!generate(&bare).contains("update-ca-trust"));
    }

    /// The declaration claims a timezone, so doctor has to be able to
    /// say whether the machine has it. It could not: the timezone is the
    /// one thing kuma writes into /etc with `ln -s`, the ownership scan
    /// read only COPY destinations and redirects, and so the single file
    /// `system.timezone` exists to produce was watched by nobody.
    ///
    /// The install path is where that bites. Anaconda writes its own
    /// /etc/localtime, so an installed machine has a local copy before
    /// kuma's ever arrives, and ostree's merge keeps a local file over
    /// every future image. A declared timezone would simply never take
    /// effect, silently, which is the exact failure the /etc check
    /// exists to end.
    ///
    /// What this pins is that the file is owned and therefore graded.
    /// Whether the merge behaves as described is ostree's business and
    /// needs a machine, not a test.
    #[test]
    fn a_declared_timezone_is_a_file_doctor_watches() {
        let declared = config(
            "schema_version = 1\n[system]\ndesktop = \"niri\"\n\
             timezone = \"America/Denver\"\nlocale = \"en_US.UTF-8\"\n",
        );
        let paths = etc_paths(&declared);
        assert!(paths.iter().any(|p| p == "/etc/localtime"), "{paths:?}");
        // locale rides the redirect path and always was owned
        assert!(paths.iter().any(|p| p == "/etc/locale.conf"), "{paths:?}");

        // Undeclared, the image writes neither, so there is nothing to
        // own and nothing to grade: timezone stays machine state.
        let bare = config("schema_version = 1\n[system]\ndesktop = \"niri\"\n");
        let paths = etc_paths(&bare);
        assert!(!paths.iter().any(|p| p == "/etc/localtime"), "{paths:?}");
        assert!(!paths.iter().any(|p| p == "/etc/locale.conf"), "{paths:?}");
    }

    /// Over the real generator, per desktop, because the value of this
    /// list is that nobody has to remember to update it.
    #[test]
    fn etc_paths_track_what_each_desktop_actually_writes() {
        let niri = etc_paths(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(niri.iter().any(|p| p == "/etc/niri/config.kdl"));
        assert!(niri.iter().any(|p| p == "/etc/greetd/config.toml"));

        // /etc/environment is where the COSMIC scanout vars live, and the
        // file whose hand-edited copy motivated the drift check.
        let cosmic = etc_paths(&config("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"));
        assert!(cosmic.iter().any(|p| p == "/etc/environment"));
        assert!(!cosmic.iter().any(|p| p == "/etc/niri/config.kdl"));

        // A headless image owns almost nothing in /etc, and must not
        // claim a desktop's files.
        let minimal = etc_paths(&config("schema_version = 1\n"));
        assert!(!minimal.iter().any(|p| p.contains("niri") || p.contains("greetd")));

        // Unpinned, the baked hostname is machine state (hostnamectl is
        // the sanctioned rename, not drift); declared, it's owned.
        assert!(!niri.iter().any(|p| p == "/etc/hostname"));
        let pinned = etc_paths(&config("schema_version = 1\n[system]\nhostname = \"workbench\"\n"));
        assert!(pinned.iter().any(|p| p == "/etc/hostname"));
    }

    /// A file picker that never appears, which is what niri shipped until
    /// this line existed. The GNOME portal backend advertises FileChooser
    /// and then delegates it to org.gnome.Nautilus, a package kuma does
    /// not install, so the interface has to be named to the gtk backend
    /// rather than left to `default=gnome;gtk;` to resolve.
    ///
    /// COSMIC is deliberately not given the same treatment: its own
    /// backend implements FileChooser, verified by reading cosmic.portal
    /// out of a built COSMIC image. Routing it to gtk there would replace
    /// a working native picker with a worse one.
    #[test]
    fn niri_routes_the_file_chooser_at_a_backend_that_implements_it() {
        let out = generate(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        let conf = "/etc/xdg-desktop-portal/niri-portals.conf";
        assert!(out.contains("org.freedesktop.impl.portal.FileChooser=gtk;"));
        assert!(out.contains(conf));

        // Derived from niri's own file, never a hand-copy: the winning
        // config file replaces rather than merges, so a copy would freeze
        // whatever the other defaults were the day it was written.
        assert!(out.contains("cat /usr/share/xdg-desktop-portal/niri-portals.conf"));

        // Both halves of the routing are guarded, because both can rot
        // without anything else failing: the file this derives from, and
        // the backend it hands the interface to.
        assert!(out.contains("grep -q '^\\[preferred\\]'"));
        assert!(out.contains(
            "grep -q 'org.freedesktop.impl.portal.FileChooser' \
             /usr/share/xdg-desktop-portal/portals/gtk.portal"
        ));

        // Written to /etc, so the drift check owns it like any other file
        // kuma has an opinion about.
        let owned = etc_paths(&config("schema_version = 1\n[system]\ndesktop = \"niri\"\n"));
        assert!(owned.iter().any(|p| p == conf));

        // COSMIC's own backend implements it; leave that alone.
        let cosmic = generate(&config("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n"));
        assert!(!cosmic.contains("niri-portals.conf"));
        assert!(!cosmic.contains("FileChooser"));
    }

    /// The cheap tier of the smoke tests, and the only one that runs
    /// everywhere: every committed example must compile to an image that
    /// keeps the promises kuma makes about *all* images, plus the ones
    /// its own declaration asks for. scripts/smoke.sh takes it from here
    /// and actually builds and boots them; this catches the regressions
    /// that don't need a machine to find, on every `cargo test`.
    ///
    /// The point is coverage that grows by itself: a new example file is
    /// automatically held to the same floor, with nobody remembering to
    /// add a test for it.
    #[test]
    fn every_example_compiles_to_a_kuma_image() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|e| e == "toml")
                || crate::config::tests::is_local_declaration(&path)
            {
                continue;
            }
            let name = path.display().to_string();
            let text = std::fs::read_to_string(&path).unwrap();
            let parsed: Config = toml::from_str(&text).unwrap();
            let out = generate(&parsed);
            let at = |what: &str, ok: bool| assert!(ok, "{name}: {what}");

            // The floor, owed to every image no matter what it declares.
            at(
                "builds FROM the declared base",
                out.contains(&format!("FROM {}", parsed.base_ref())),
            );
            at("runs the bootc lint", out.contains("bootc container lint"));
            at("bakes greenboot", out.contains(&dnf_install("greenboot")));
            at("converges the boot counter", out.contains("kuma-boot-health-sync.service"));
            at("labels itself for GC", out.contains("LABEL io.kuma.image"));
            at("bakes its own declaration", out.contains("COPY kuma.toml /usr/lib/kuma/kuma.toml"));

            // What this particular declaration asked for.
            for pkg in &parsed.packages.rpm {
                at(&format!("installs declared rpm {pkg}"), out.contains(pkg));
            }
            for svc in &parsed.services.enable {
                at(&format!("enables declared service {svc}"), out.contains(svc));
            }
            if parsed.user.is_some() {
                at("converges the declared user", out.contains("kuma-user-sync.service"));
            }
            if !parsed.packages.flatpak.is_empty() {
                at("converges flatpaks", out.contains("kuma-flatpak-sync.service"));
            }

            // A desktop is a promise about what you see at boot, so the
            // greeter check and the keyring unlock ride with every one of
            // them and neither is per-desktop opt-in.
            if parsed.system.desktop != Desktop::None {
                at("guards the greeter in greenboot", out.contains("50-kuma-greeter.sh"));
                at("asserts the keyring PAM module", out.contains("pam_gnome_keyring.so"));
                at("ships identity", out.contains("fastfetch-logo.txt"));
            } else {
                at("stays headless", !out.contains("50-kuma-greeter.sh"));
            }
            checked += 1;
        }
        assert!(checked >= 3, "expected the committed examples, found {checked}");
    }

    #[test]
    fn cosmic_context_ships_identity_not_niri_glue() {
        let dir = tempfile::tempdir().unwrap();
        context("schema_version = 1\n[system]\ndesktop = \"cosmic\"\n", dir.path());
        // identity and kargs travel with every desktop
        assert!(dir.path().join("fastfetch-logo.txt").exists());
        assert!(dir.path().join("kargs-desktop.toml").exists());
        assert!(dir.path().join("kuma-wallpaper.jpg").exists());
        // the dock and background overrides are cosmic-only context
        assert!(dir.path().join("cosmic-favorites").exists());
        assert!(dir.path().join("cosmic-background").exists());
        // flatpak convergence ships even with an empty list
        assert!(dir.path().join("flatpaks").exists());
        // the niri glue stays home: COSMIC provides all of it natively
        assert!(!dir.path().join("greetd-config.toml").exists());
        assert!(!dir.path().join("mako.conf").exists());
        assert!(!dir.path().join("xsettingsd.conf").exists());
    }
}
