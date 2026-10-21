//! Stamps the checkout's identity into the binary.
//!
//! `kuma --version` reporting only `0.2.0` would answer the least useful
//! question. Twice now a change has looked broken because the installed
//! binary predated it: the niri portal fix appeared not to work, and a
//! wallpaper swap would have silently baked the old asset. Both times the
//! binary looked fine and said nothing. The commit it was built from, and
//! whether the tree was clean at the time, is the thing worth carrying.
//!
//! Both values degrade to "unknown" rather than failing the build. A
//! release tarball has no `.git`, and a build from one is still valid.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Emitting any rerun-if-changed opts out of cargo's default, which is
    // to rerun on any file in the package. So the git internals AND the
    // sources have to be named: HEAD and the ref catch a commit or a
    // branch switch, the sources catch the edits that make a tree dirty.
    // Miss the first group and a fresh commit keeps the old sha; miss the
    // second and "-dirty" outlives the edit that earned it.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    if let Some(head_ref) = git(&["rev-parse", "--symbolic-full-name", "HEAD"]) {
        let path = format!(".git/{head_ref}");
        if Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    for path in ["src", "assets", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={path}");
    }

    let sha = match git(&["rev-parse", "--short=7", "HEAD"]) {
        Some(sha) if dirty() => format!("{sha}-dirty"),
        Some(sha) => sha,
        None => "unknown".to_string(),
    };
    // The commit's own date, not the build's: two builds of one commit
    // describe the same code, and saying otherwise invites the belief
    // that a rebuild changed something.
    let date = git(&["log", "-1", "--format=%cd", "--date=short"])
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=KUMA_BUILD_SHA={sha}");
    println!("cargo:rustc-env=KUMA_BUILD_DATE={date}");

    plymouth_theme();
    kumaui_pin();
}

/// Embed the vendored plymouth theme into the binary.
///
/// The theme ships inside every kuma binary (house style: WALLPAPER and
/// friends are include_bytes!/include_str! in containerfile.rs), because a
/// released musl binary runs from ~/.cargo/bin with no checkout around it,
/// so runtime reads of `assets/` would only work for a developer. The
/// build script walks `assets/spinner_alt/` and emits one `(name, bytes)`
/// pair per file as an OUT_DIR module; containerfile.rs includes it and
/// stages the files into the build context verbatim.
///
/// The listing is sorted byte-wise, not os_read_dir order, so two builds
/// of the same directory produce byte-identical source (os_read_dir makes
/// no order guarantee; a per-build coin flip here would churn every
/// downstream image digest).
fn plymouth_theme() {
    let dir = Path::new("assets/spinner_alt");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("spinner_alt entry").path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    let mut table = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    // Absolute via CARGO_MANIFEST_DIR: include_bytes! resolves relative
    // paths against OUT_DIR, where this generated file lives.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    for path in &files {
        let name = path.file_name().and_then(|n| n.to_str()).expect("UTF-8 name");
        assert!(!name.contains('"') && !name.contains('\\'), "odd filename {name}");
        table.push_str(&format!(
            "    ({name:?}, include_bytes!(\"{manifest}/assets/spinner_alt/{name}\")),\n"
        ));
    }
    table.push_str("];\n");

    let out_dir = std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR");
    let out_path = Path::new(&out_dir).join("plymouth_theme.rs");
    std::fs::write(&out_path, table)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", out_path.display()));
}

/// Emit the kumaui pin as compile-time env, parsed from the action file
/// whose `default:` line is the one place the pinned kumaui version is
/// named.
///
/// Parse-and-emit rather than letting the crate include_str! the file,
/// and the reason is the publish audit: the action's text carries the
/// runner's RUSTFLAGS (`--remap-path-prefix=/home/runner/...`), and a
/// binary embedding the file embeds that string — the audit reads any
/// `/home/<name>/` in a shipped binary as a leaked build path and
/// refuses the image, which 44.10.0's first publish died of (the first
/// build to carry the pin embed at all). The pin is the file's one
/// cargo-relevant fact; only it travels.
///
/// Both values degrade to empty rather than failing the build: a
/// release tarball carries the action file (it is tracked), but a
/// file whose shape moved parses to nothing, and the empty pin is the
/// consumer's refusal at the call site, not a skipped check.
fn kumaui_pin() {
    println!("cargo:rerun-if-changed=.github/actions/kuma-shell/action.yml");
    let text = std::fs::read_to_string(".github/actions/kuma-shell/action.yml").unwrap_or_default();
    let mut pin = ("", "");
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("default:") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let sha = parts.next().unwrap_or("");
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            break;
        }
        if let Some(idx) = rest.find("# v") {
            let version = rest[idx + 3..].trim();
            if !version.is_empty() {
                pin = (sha, version);
            }
        }
        break;
    }
    println!("cargo:rustc-env=KUMA_UI_PIN_SHA={}", pin.0);
    println!("cargo:rustc-env=KUMA_UI_PIN_VERSION={}", pin.1);
}

/// Trimmed stdout of a successful `git`, or None if git is missing, this
/// is not a repository, or the command failed.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Whether the working tree has changes the sha does not describe.
/// Untracked files count: an asset that exists only locally is baked into
/// the binary exactly like a tracked one.
fn dirty() -> bool {
    git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty())
}
