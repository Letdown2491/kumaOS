//! The feature-block registry: what every image carries, as blocks.
//!
//! A block is one gated span of the Containerfile plus the files it
//! stages, in one function with one gate — where the old shape had the
//! text in `generate()`, the staging in `write_context()` behind a
//! re-derived gate, and the units listed again by hand in `liveiso`.
//! Table order IS emission order: the list below is the layer order of
//! every image kuma builds, and a reorder is a visible, reviewable diff.

use super::emit::{Content, Emitter};
use super::*;

/// The curated niri desktop: compositor, greeter, launcher, bar,
/// notifications, terminal, audio, portals, fonts.
pub(crate) const NIRI_PACKAGES: &[&str] = &[
    "flatpak",
    "niri",
    "xwayland-satellite",
    "greetd",
    "tuigreet",
    // The shell: built in this image from the kumaui tree, not packaged.
    // One process for the bar, notifications, wallpaper, idle, lock,
    // control centre and the nostr signer, which is why waybar, mako,
    // swaybg, swayidle, swaylock, wob and wlsunset are all gone from
    // this list. See the builder stage in generate().
    "kitty",
    "pipewire",
    "pipewire-pulseaudio",
    "wireplumber",
    "xdg-desktop-portal-gtk",
    "xdg-desktop-portal-gnome",
    "dconf",
    "gnome-keyring",
    // the PAM module that unlocks the login keyring is a subpackage,
    // and nothing depends on it: without this every keyring-using app
    // prompts on launch, silently, because the greeter's PAM lines are
    // '-' prefixed and skip a missing module without a word
    "gnome-keyring-pam",
    "polkit",
    "mesa-dri-drivers",
    // OpenGL alone strands Vulkan apps on lavapipe software rendering
    "mesa-vulkan-drivers",
    "vulkan-loader",
    "default-fonts-core-sans",
    "default-fonts-core-mono",
    // Both desktop faces, via the metapackage rather than by naming
    // them: waybar needs Free for most glyphs and Brands for the
    // bluetooth one, and the per-face packages carry the major version
    // in their names (fontawesome-6-* became fontawesome-7-* in Fedora
    // 45), so naming the faces breaks the build on every Font Awesome
    // major. The metapackage's name is version-free, it requires
    // exactly those two packages, and it owns no files itself.
    // Kept after the bar that needed it left: the shell bundles its own
    // icon font, but flatpaks and the odd GTK app still reach for these
    // glyphs and render tofu without them.
    "fontawesome-fonts-all",
    // base ships glibc-minimal-langpack only; without real locale data
    // en_US.UTF-8 fails to resolve and anything formatting a date or a
    // number falls back to C
    "glibc-langpack-en",
    // hardware enablement — the minimal base targets servers
    "NetworkManager-wifi",
    // The icons kuma's desktop entries name, by file. GTK drags this in
    // anyway, and that is exactly the kind of luck the seam should not
    // run on: an entry whose icon does not resolve draws a blank square
    // and reports nothing. Named here so it is kuma's dependency and not
    // somebody else's.
    "adwaita-icon-theme",
    // The GTK3 half of "the desktop follows the palette". Stock Adwaita
    // GTK3 ignores the colour names a palette can set: overriding
    // theme_bg_color from the user stylesheet moves nothing, measured on
    // gtk3-3.24.52. adw-gtk3 is libadwaita's look ported back to GTK3 and
    // it does read them, so thunar, pavucontrol, nm-connection-editor and
    // blueman follow the same palette as the shell. In Fedora proper, 175
    // KiB, no new trust root.
    "adw-gtk3-theme",
    // desktop-file-validate: the build checks the entries it generates
    "desktop-file-utils",
    // The shell's control centre owns wifi now, so this is no longer the
    // desktop's answer for it. It stays because it is the TTY answer:
    // when a session refuses to start, this is the only way left to get
    // the network up and fix the machine.
    "NetworkManager-tui",
    "wpa_supplicant",
    "brightnessctl",
    "power-profiles-daemon",
    // device-level settings: the config file covers system definition,
    // but wifi picking, pairing, mixers, and mounts are machine state
    "pavucontrol",
    "nm-connection-editor",
    "bluez",
    "blueman",
    "thunar",
    "thunar-archive-plugin",
    "file-roller",
    "gvfs",
    "gvfs-mtp",
    "wf-recorder",
    // base ships zram-generator but not the defaults that activate it:
    // without this the desktop has zero swap and the OOM killer eats
    // windows under memory pressure
    "zram-generator-defaults",
    // avahi is named, not assumed: the desktop enables avahi-daemon, and
    // "the base ships it" was only ever true of fedora-bootc by luck —
    // kuma's composed base doesn't. nss-mdns makes .local names and
    // driverless printer discovery actually resolve
    "avahi",
    "nss-mdns",
    // (nwg-look would fit here for GTK theme tweaks, but it's COPR-only)
    "libnotify",
    "cups",
    "system-config-printer",
    // session essentials
    "wl-clipboard",
    "xsettingsd",
    "spice-vdagent",
    "xdg-user-dirs",
    "default-fonts-core-emoji",
    "mate-polkit",
    "firewalld",
    // niri's built-in screenshot UI covers the Print keys; grim+slurp are
    // the wlr-screencopy tools everything scriptable builds on
    "grim",
    "slurp",
    // Mod+Print: annotate before sharing (satty is COPR-only)
    "swappy",
    // the XF86Audio sed that makes room for the shell's media binds also
    // drops niri's stock playerctl binds; kuma re-adds them, and nothing
    // else pulls playerctl in now that waybar has left the set
    "playerctl",
    // plug-in automount: thunar only mounts on click, and thunar-volman
    // needs the thunar daemon plus xfconf toggles to do its job
    "udiskie",
    // file-roller alone can't open 7z/rar downloads
    "7zip",
    "unar",
    // default-fonts-core-sans is latin-only: CJK pages render as tofu
    "google-noto-sans-cjk-vf-fonts",
    // quiet identity: baked and configured, never run at shell startup
    "fastfetch",
];

/// niri Recommends these, so they arrive whether or not `NIRI_PACKAGES`
/// names them. Every one is a program kuma deliberately does not ship.
///
/// All four were found the same way and none of them by a test: build
/// the image, then ask it whether the thing you removed is still there.
/// Removing a package from `NIRI_PACKAGES` does nothing when something
/// else recommends it, and the symptom is an image quietly carrying a
/// bar, a lock screen and a launcher that nothing starts.
pub(crate) const NIRI_EXCLUDES: &[&str] = &["alacritty", "waybar", "swaylock", "fuzzel"];

/// The curated COSMIC desktop. Unlike niri's hand-assembled set, COSMIC
/// curates itself: cosmic-session hard-requires the whole coherent
/// desktop (compositor, panel, applets, settings, files, terminal,
/// notifications, OSD, screenshot, portal, fonts), so this list is the
/// session plus the hardware enablement a desktop lives on. pipewire is
/// explicit because nothing in the session requires the daemon, only
/// its client library. cosmic-store is absent, though no longer because
/// convergence would fight it: a store is a user-facing app, so which
/// one a machine gets is the declaration's call, not the desktop set's.
pub(crate) const COSMIC_PACKAGES: &[&str] = &[
    "cosmic-session",
    // the session requires files, term, and settings but not the text
    // editor — which the default dock pins, so ship it or the pin is dead
    "cosmic-edit",
    "flatpak",
    "fastfetch",
    // unlocked with the login password via PAM, same as the niri set,
    // and the -pam subpackage is the module that does the unlocking
    "gnome-keyring",
    "gnome-keyring-pam",
    "pipewire",
    "pipewire-pulseaudio",
    "wireplumber",
    "mesa-dri-drivers",
    "mesa-vulkan-drivers",
    "vulkan-loader",
    "glibc-langpack-en",
    "NetworkManager-wifi",
    "wpa_supplicant",
    "power-profiles-daemon",
    "bluez",
    "firewalld",
    "zram-generator-defaults",
    // same story as the niri set: the desktop enables avahi-daemon, so
    // the desktop ships it
    "avahi",
    "nss-mdns",
    "cups",
    "system-config-printer",
    "spice-vdagent",
    // cosmic-files mounts removable media through udisks2 directly
    "udisks2",
    "default-fonts-core-emoji",
    "google-noto-sans-cjk-vf-fonts",
    // the seam's dependencies, named rather than assumed: kuma's desktop
    // entries draw Adwaita's symbolic icons and the build validates them
    // with desktop-file-validate. Both ride in today on cosmic-session's
    // closure, and a seam that silently loses its icons because an
    // unrelated package stopped depending on a theme is not a seam
    "adwaita-icon-theme",
    "desktop-file-utils",
];

/// COSMIC's packaged dock pins the Firefox flatpak and cosmic-store —
/// neither of which a kuma image guarantees: the browser is the
/// declaration's choice, and the store is deliberately absent. The
/// baked default pins only what the image ships; anything declared is
/// one right-click from a pin.
pub(crate) const COSMIC_FAVORITES: &str = r#"[
    "com.system76.CosmicFiles",
    "com.system76.CosmicEdit",
    "com.system76.CosmicTerm",
    "com.system76.CosmicSettings",
]
"#;

/// The packaged default, pointed at the Kuma wallpaper. filter_by_theme
/// must go false: left on, COSMIC swaps the wallpaper back out for its
/// own theme-matched set.
pub(crate) const COSMIC_BACKGROUND: &str = r#"(
    output: "all",
    source: Path("/usr/share/backgrounds/kuma/kuma-wallpaper.jpg"),
    filter_by_theme: false,
    rotation_frequency: 3600,
    filter_method: Lanczos,
    scaling_mode: Zoom,
    sampling_method: Alphanumeric,
)
"#;

/// kuma's own signing key, baked into the binary so that any build has
/// it. A build runs from the binary and not from a checkout, so reading
/// the repository's copy at build time would work only for people who
/// have the repository.
pub const COSIGN_PUB: &str = include_str!("../../cosign.pub");

/// Where the key lands in the image. Under /etc/pki/containers because
/// that is where a policy can name it and where an administrator would
/// look for it.
pub const COSIGN_PUB_PATH: &str = "/etc/pki/containers/kuma.pub";

/// Require a signature for kuma's own published images, and nothing else.
///
/// The narrow rule is the point. This file is shared by podman and bootc,
/// so a blanket requirement would refuse Fedora's base image on the next
/// `kuma update` and refuse the machine's own locally built images on the
/// next `kuma switch`. What it buys, scoped this way, is that the one
/// registry kuma tells strangers to install from cannot serve them
/// something kuma did not sign.
///
/// `matchRepository`, not `matchRepoDigestOrExact`, and this was measured
/// rather than chosen: cosign records the identity as the bare repository
/// (`ghcr.io/letdown2491/kuma`, no tag), so an exact match can never
/// succeed and the policy would reject every image kuma has ever
/// published. Verified against the live registry in both directions, with
/// the real key and with a wrong one.
pub(crate) fn signature_policy() -> String {
    let repo = crate::published_repo();
    format!(
        r#"{{
  "default": [{{"type": "insecureAcceptAnything"}}],
  "transports": {{
    "docker": {{
      "{repo}": [
        {{
          "type": "sigstoreSigned",
          "keyPath": "{COSIGN_PUB_PATH}",
          "signedIdentity": {{"type": "matchRepository"}}
        }}
      ]
    }},
    "containers-storage": {{"": [{{"type": "insecureAcceptAnything"}}]}},
    "docker-daemon": {{"": [{{"type": "insecureAcceptAnything"}}]}},
    "dir": {{"": [{{"type": "insecureAcceptAnything"}}]}},
    "oci": {{"": [{{"type": "insecureAcceptAnything"}}]}}
  }}
}}
"#
    )
}

/// Without this the policy above cannot find anything to verify: cosign
/// stores a signature as a separate `sha256-<digest>.sig` tag beside the
/// image, and containers/image only looks there when told to.
pub(crate) fn registries_d() -> String {
    let repo = crate::published_repo();
    format!("docker:\n  {repo}:\n    use-sigstore-attachments: true\n")
}

/// Fedora's mesa VA-API driver ships with H.264/H.265/VC-1 decode
/// stripped (patents), so video silently falls back to CPU. RPM
/// Fusion's freeworld build restores it; --allowerasing swaps out
/// the gutted driver if a dependency dragged it in.
///
/// The release RPM arrives by URL, and a URL package carries no repo
/// checksum to be graded against once cached. Measured 2026-09-04,
/// locally and in CI: when a later build finds a cached copy, librepo's
/// resume path appends the re-download beside it under a zero-filled
/// stretch, dnf5 refuses the result ("not a rpm") and then keeps
/// refusing — a file larger than it expects is never re-downloaded —
/// so every build through a shared cache mount after a successful one
/// failed here. Clearing the @commandline cache first costs one
/// 11.5 KiB download per build and keeps the step off the resume path
/// entirely. The repo packages the second dnf installs are checksummed
/// and stay cached as before.
pub(crate) fn mesa_freeworld() -> String {
    dnf_layer(
        "rm -rf /var/cache/libdnf5/@commandline-* \\\n    && dnf -y install --setopt=keepcache=1 \"https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm\" \\\n    && dnf -y install --setopt=keepcache=1 --allowerasing mesa-va-drivers-freeworld",
    )
}

/// The mount that turns dnf's cache from dead weight in the image into a
/// cache that survives across builds.
///
/// Every dnf layer used to end in `dnf clean all`, which threw away the
/// repo metadata (~150 MB) and the downloaded RPMs (hundreds of MB for
/// the desktop set) so they would not bloat the image. The cost was that
/// a cold build, and every `kuma update` (a base bump invalidates every
/// layer), re-downloaded all of it. A build cache mount keeps that data
/// on the host instead of in the layer: it never lands in the image, so
/// there is nothing to clean, and the next build reuses it. `keepcache=1`
/// is what makes dnf leave the RPMs there rather than only the metadata,
/// so an update re-downloads only the packages that actually changed.
pub(crate) const DNF_CACHE: &str = "/var/cache/libdnf5";

/// One dnf RUN layer with the package cache mounted rather than baked.
/// `body` is the shell after `RUN `; callers that install a plain list go
/// through `dnf_install`, and the two-step mesa case builds its own.
pub(crate) fn dnf_layer(body: &str) -> String {
    format!("RUN --mount=type=cache,target={DNF_CACHE} \\\n    {body}\n")
}

/// The common case: install a package list, cached, no clean.
pub(crate) fn dnf_install(packages: &str) -> String {
    dnf_layer(&format!("dnf -y install --setopt=keepcache=1 {packages}"))
}

/// Unlock the keyring with the login password, or every keyring-using
/// app nags for it on launch (browsers most visibly, but far from only
/// them). Autologin skips this: no password is typed.
///
/// Fedora's greeter PAM stacks already call pam_gnome_keyring, so
/// there is nothing to append; but every one of those lines carries a
/// '-' prefix, which tells PAM to skip a missing module in total
/// silence. The module ships in gnome-keyring-pam, a subpackage of
/// gnome-keyring that nothing pulls in, so the desktop lists name it
/// explicitly and this asserts both halves: the module exists on disk,
/// and the stack that actually authenticates this desktop still calls
/// it. Without the assert the failure mode is invisible: login
/// succeeds, nothing is logged, and the keyring is simply never
/// unlocked (which is exactly how it shipped until 2026-08-07).
///
/// `service` is the greeter's own PAM stack file: greetd's own for
/// niri, cosmic-greeter's for COSMIC. They are not interchangeable;
/// asserting the wrong one proves nothing. Which directory the file
/// lives in is the release's business, and it moved: Fedora 45 ships
/// vendor stacks in /usr/lib/pam.d (greetd-0.10.3-10.fc45 owns
/// /usr/lib/pam.d/greetd and nothing in /etc), where Fedora 44 shipped
/// them in /etc/pam.d. The assert names both, and grep exits 0 if
/// either matches -- a match in the vendor stack is as good as one in
/// /etc, because that is the stack PAM reads when /etc has no file of
/// its own (Linux-PAM falls back to the vendor directory). Verified
/// against the Fedora 45 Beta payloads that both directories' greetd
/// stacks still call pam_gnome_keyring; see notes/fedora-45-rebase.md.
/// A missing file under 2>/dev/null keeps the always-absent directory
/// from printing noise on every build.
pub(crate) fn keyring_pam(service: &str) -> String {
    format!(
        "RUN test -f /usr/lib64/security/pam_gnome_keyring.so \\\n    && grep -q pam_gnome_keyring /etc/pam.d/{service} /usr/lib/pam.d/{service} 2>/dev/null\n"
    )
}

/// Two lint warnings' worth of build litter, swept before the verdict.
///
/// Installing packages leaves behind runtime state that a booted machine
/// makes for itself anyway: /run/cups, /run/dnf, /run/mdadm, /run/samba
/// and selinux-policy's scratch files, plus dnf5's own log in /var/log.
/// bootc's nonempty-run-tmp and var-log lints flag both, and both are
/// right. /run and /tmp are tmpfs on a running machine, so anything
/// baked into them is dead weight nothing will ever read, and a shipped
/// logfile is a record of the build host rather than of the machine.
///
/// rm's exit status cannot be the verdict here. podman holds two paths
/// inside /run open for the whole build: /run/secrets, and (because the
/// base's /etc/resolv.conf is a symlink into it)
/// /run/systemd/resolve/stub-resolv.conf. Deleting a live bind mount
/// fails busy no matter what the image wants. Neither survives as
/// content: what lands in the layer is an empty file, which is exactly
/// what "nonempty" means the lint does not count.
///
/// /var/log is the half kuma controls completely, so it is asserted
/// rather than hoped for: a logfile that outlives the sweep fails the
/// build instead of shipping.
pub(crate) const SWEEP: &str = r#"
RUN find /run /tmp -mindepth 1 -delete; \
    find /var/log -type f -delete; \
    ! find /var/log -type f | grep -q .
"#;

/// bootc's own build-time check that the image is a valid bootable
/// container. It runs last, and its verdict is the build's verdict.
///
/// The wrapper exists for one upstream bug. bootc 1.16.7's var-tmpfiles
/// lint walks /usr/lib/tmpfiles.d and opens what it finds, and
/// tpm2-tss-fapi.conf points at securityfs:
///
///   z- /sys/kernel/security/ima/binary_runtime_measurements 0440 root tss - -
///
/// Inside a build that path is unreadable, so the lint aborts the whole
/// run with "Unexpected runtime error running lint var-tmpfiles" and no
/// image is produced. (Note the '-' on 'z-': that is tmpfiles' own
/// ignore-failures modifier, so systemd tolerates the very path the lint
/// will not. Same bailout as bootc-dev/bootc#1481, which hits it through
/// qemu-user instead of securityfs.) It reproduces on the unmodified
/// fedora-bootc:44 base, so nothing kuma builds can avoid it.
///
/// The insult on top: var-tmpfiles is a warning-level lint, so even
/// when it runs it cannot fail a build. Only its crash can.
///
/// Rather than pass --skip var-tmpfiles forever, which would quietly
/// retire the check the day upstream fixes this, tolerate exactly this
/// crash and nothing else: any other lint failure still fails the build
/// (verified against a fatal var-run), warnings still reach the log
/// either way, and once bootc stops crashing the first run simply
/// passes and the fallback goes cold on its own.
///
/// The output is held in a variable rather than a scratch file. A file
/// under /tmp was the obvious way to look at the lint's own stderr twice,
/// and it worked, but the lint reads /tmp while it runs: once the rest of
/// the litter was swept, the only thing left in the image was
/// /tmp/lint.err, and the check spent its last warning reporting itself.
/// Nothing on disk means nothing to notice and nothing to clean up.
pub(crate) const LINT: &str = r#"
RUN said=$(bootc container lint 2>&1); rc=$?; \
    printf '%s\n' "$said"; \
    if [ $rc -ne 0 ] && printf '%s' "$said" | grep -q 'var-tmpfiles: I/O error'; then \
        rc=0; bootc container lint --skip var-tmpfiles || rc=$?; \
    fi; \
    exit $rc
"#;

/// The greeter session. The command is a wrapper, not the greeter
/// directly: see GREETER_SESSION for what it adds and why.
///
/// The tuigreet line stays as a comment — reverting to it is a comment
/// swap from a TTY, the recovery path when the graphical greeter cannot
/// log anyone in. Upgrades reach this file through /etc's ordinary
/// merge: an unmodified config takes the new default on upgrade, a
/// locally edited one keeps winning over the image (kuma doctor names
/// it; `sudo cp /usr/etc/greetd/config.toml /etc/greetd/config.toml`
/// takes the flip by hand).
pub(crate) const GREETD_CONFIG: &str = r#"[terminal]
vt = 1

[default_session]
# kuma-greeter: keep the tuigreet line as a commented fallback,
# reverting is a comment swap from a TTY
command = "/usr/libexec/kuma-greeter-session"
user = "greetd"
# command = "tuigreet --time --remember --greeting 'Welcome to kumaOS' --cmd niri-session"
# user = "greetd"
"#;

/// The greeter compositor's niri config, verbatim from kumaUI's
/// `greeter-niri.kdl` (pinned commit a795f43). Minimal on purpose: one
/// layer-shell window, no keybinds, no session wiring. The layout
/// block matches the shell wallpaper base, so greeter teardown into
/// session start never flashes black. Do not add keybinds here — this
/// niri exists to host the greeter, nothing else. Drift hazard: this
/// is a copy, not a build-time fetch; re-sync it on every kumaUI pin
/// bump that touches the file.
pub(crate) const GREETER_NIRI_KDL: &str = r##"// The greeter compositor config: the smallest niri that can host
// one greeter window. No keybinds, no gestures, no session wiring:
// greetd launches this niri on the test VT, niri launches
// kuma-greeter as its startup command, and GREETD_SOCK flows down
// through the environment. The hotkey overlay is noise on a login
// screen, so it never shows. Ctrl+Alt+F1 (or F3) still switches
// VTs, that is compositor-independent.
layout {
    // Matches the shell wallpaper base (0x11111B) and the session
    // config, so greeter teardown into session start never flashes.
    background-color "#11111B"
}
hotkey-overlay {
    skip-at-startup
}"##;

/// What greetd actually runs: niri hosting kuma-greeter, with the
/// greeter's cache pointed at a directory of its own. The greeter
/// renders the default wallpaper and theme (it cannot know the user
/// before login) and reads only image paths — the one thing it writes
/// is its cache (fontconfig plus Mesa's shader caches). The greetd
/// account's home is not a place the image guarantees is writable, and
/// /var/cache carries the var_t label xdm_t may not create under;
/// /var/lib/greetd is already xdm_var_lib_t, the type xdm_t owns. Its
/// cache directory under there (created at boot by
/// tmpfiles.d/kuma-greeter.conf, 0700 greetd) keeps the login screen
/// from ever fighting SELinux over a home it should not own.
/// Wallpaper, fonts and theme need nothing here: they read image paths.
///
/// The wrapper supervises instead of exec'ing, and this is a measured
/// fix, not a shape preference. greetd's contract (greetd/src/context.rs,
/// `start()`) gives the greeter session 5 seconds to exit on its own
/// after StartSession before it shoots it ("we give the greeter 5
/// seconds to prove itself well-behaved"); the session starts only when
/// the greeter process is gone. With `exec niri -- kuma-greeter` the
/// greeter process is niri, and niri double-forks its `--` child into a
/// transient scope without ever watching it — kuma-greeter exits
/// promptly, niri lingers forever, and every login paid greetd's full
/// 5-second patience plus its 1-second alarm tick (~6 s of journal
/// silence between PAM success and session start, reproduced on this
/// image). tuigreet-class greeters are fast because the greeter process
/// IS the session child. So the wrapper runs niri in the background,
/// runs kuma-greeter against its socket, and the moment the greeter
/// quits it SIGTERMs niri (measured: 0.9 s to a clean exit) and exits —
/// greetd sees a well-behaved greeter and starts the session at once.
pub(crate) const GREETER_SESSION: &str = r#"#!/usr/bin/env bash
export XDG_CACHE_HOME=/var/lib/greetd/cache

# The greeter chain's stderr — bash's own errors, niri's protocol
# errors, kuma-greeter's log lines — goes to VT1 and dies with the
# session, so the journal never sees why a login screen failed. One
# redirect at the top retargets the whole chain to the journal via
# logger: the journal is what the smoke's failure dump reads, it
# survives boots with timestamps, and no file permission can mute it
# (a log FILE can: /var/log is var_log_t, which xdm_t cannot write,
# and a 0700 greetd dir cannot be read back by the smoke's ssh user —
# both measured, Oct 04). RUST_LOG=debug stays until the greeter
# smoke passes; drop it to info after.
export RUST_LOG=info
# bash dies on SIGPIPE when a builtin writes a broken pipe: if the
# logger reader of this redirect ever stalls or dies, the wrapper's
# next echo kills the whole session silently — greetd reads that as
# "greeter exited without creating a session" and shoots a greeter
# that is mid-initialization (measured, Oct 04). The greeter itself
# is Rust and ignores SIGPIPE, which is why it outlived its wrapper.
trap '' PIPE
# A silent death needs a witness: TERM is what systemd sends before
# the cgroup goes, and if the wrapper is killed with the scope its
# last marker never lands. Log it and go.
trap 'echo >&2 "wrapper: SIGTERM at $((SECONDS - started))s"; exit 143' TERM
exec 2> >(exec logger -t kuma-greeter)

# The wrapper knows why the session ends: compositor death, socket
# timeout, or greeter exit with its status. Say so — three laps of a
# silent "greeter exited without creating a session" bought this.
echo >&2 "wrapper: greetd started the session (pid $$)"
niri -c /usr/share/kumaos/greeter-niri.kdl &
niri_pid=$!
started=$SECONDS
cleanup() { kill "$niri_pid" 2>/dev/null; }
trap cleanup EXIT

# niri prints its socket when ready; wait for the file rather than a
# fixed sleep, and bail if the compositor dies under us
sock=""
for _ in $(seq 1 100); do
  sock=$(ls "$XDG_RUNTIME_DIR"/wayland-? 2>/dev/null | head -1)
  [ -n "$sock" ] && break
  if ! kill -0 "$niri_pid" 2>/dev/null; then
    wait "$niri_pid"
    echo >&2 "wrapper: niri died after $((SECONDS - started))s (exit $?)"
    exit 1
  fi
  sleep 0.1
done
if [ -z "$sock" ]; then
  echo >&2 "wrapper: no wayland socket after 10s, niri still alive"
  exit 1
fi
echo >&2 "wrapper: wayland socket up after $((SECONDS - started))s"

# the greeter runs at trace: its own account of WHY the loop ends is
# the evidence a silent rc=0 quit withholds, and the per-command
# override keeps niri (which shares the exported RUST_LOG) at info so
# naga's compile does not flood the journal
RUST_LOG=trace WAYLAND_DISPLAY=${sock##*/} /usr/bin/kuma-greeter
rc=$?
echo >&2 "wrapper: greeter exited rc=$rc after $((SECONDS - started))s"
# the greeter only exits on success or crash: either way hand the
# session back to greetd immediately, don't make it wait out its
# 5-second patience on a compositor nobody is using
cleanup
trap - EXIT
wait "$niri_pid" 2>/dev/null
echo >&2 "wrapper: niri reaped (exit $?)"
exit $rc
"#;

/// A broken greeter cannot strand a boot: if kuma-greeter dies, greetd
/// exits with it, and this drop-in brings the unit back in two seconds.
/// TTY logins (getty) remain the always-there escape hatch regardless.
pub(crate) const GREETD_RESTART_DROPIN: &str = r#"# The greeter is the boot: without it a broken login screen strands a
# machine nobody can log into. getty remains available on the VTs.
[Service]
Restart=on-failure
RestartSec=2
"#;

/// The greeter's cache directory, created at boot before greetd runs
/// (sysv order: tmpfiles-setup precedes greetd in graphical.target).
/// See GREETER_SESSION for why the greeter cannot write a default
/// cache location.
pub(crate) const GREETER_TMPFILES: &str = "d /var/lib/greetd/cache 0700 greetd greetd -\n";

/// What starts a session, and where each greeter reads it from.
///
/// Shared with `liveiso`, which autologins its own throwaway account the
/// same way. Two copies of these strings means a session command can
/// change and break autologin in exactly one of the two places, and the
/// live one is the copy no CI boots.
pub(crate) const NIRI_SESSION: &str = "niri-session";
pub(crate) const COSMIC_SESSION: &str = "start-cosmic";
pub(crate) const GREETD_CONF: &str = "/etc/greetd/config.toml";
/// cosmic-greeter authenticates against its own file. greetd's exists in
/// a COSMIC image too, pulled in as a dependency, so writing there would
/// look healthy and prove nothing.
pub(crate) const COSMIC_GREETER_CONF: &str = "/etc/greetd/cosmic-greeter.toml";

/// greetd's initial_session is exactly autologin semantics: straight
/// into the desktop at boot, greeter on logout.
pub(crate) fn initial_session(command: &str, user: &str) -> String {
    format!("\n[initial_session]\ncommand = \"{command}\"\nuser = \"{user}\"\n")
}

pub(crate) fn greetd_config(config: &Config) -> String {
    let mut out = GREETD_CONFIG.to_string();
    if let Some(user) = &config.user {
        if user.autologin {
            out.push_str(&initial_session(NIRI_SESSION, &user.name));
        }
    }
    out
}

/// Desktop kernel args. The minimal base ships no auditd, so kernel audit
/// records spray onto the console; `quiet` keeps the console clean without
/// disabling auditing (records still reach the journal). `rhgb` hands the
/// console to plymouth: with it, encrypted machines get the themed splash
/// behind the LUKS prompt and plain machines a spinner instead of boot
/// text. Plymouth is installed unconditionally (base layer), but the
/// desktop is where a splash can be seen; headless keeps textual boots.
pub(crate) const DESKTOP_KARGS: &str = "kargs = [\"quiet\", \"rhgb\"]\n";

/// Declared flatpaks are baked into the image as a list; this oneshot
/// converges the machine to it on boot. The declaration is atomic image
/// content — only the app installs are runtime state.
///
/// The retries cover the timer's Persistent=true catch-up, which fires on
/// resume before Wi-Fi is back — network-online.target only orders boot.
///
/// The low CPU/IO weights matter most on first boot, when every declared
/// app downloads at once while the user logs in for the first time: that
/// storm once starved cosmic-panel into a session with no panel at all.
/// Convergence is background work; the session it converges for is not.
/// Permissions converge at boot and on an explicit `kuma sync`, and
/// deliberately not on the daily timer that carries installs. An install
/// arriving at a random hour is additive and idempotent; a permission
/// reverting at a random hour changes what a running app can reach, and
/// a Flatseal toggle silently flipping back the next afternoon is
/// indistinguishable from a bug. Boot-only buys a rule that fits in a
/// sentence: declared permissions are restored when you boot, and the
/// session in between is yours to experiment in, with `kuma diff` to
/// show the edit and `kuma capture` to keep it.
///
/// Ordered after the installer so an app that arrives this boot has its
/// permissions before it is first launched.
pub(crate) const FLATPAK_OVERRIDES_SERVICE: &str = "\
[Unit]
Description=Converge Flatpak permission overrides to the declaration
After=kuma-flatpak-sync.service

[Service]
Type=oneshot
ExecStart=/usr/bin/kuma flatpak-overrides --scope system

[Install]
WantedBy=multi-user.target
";

/// The user store's half. It runs as the account that owns the files
/// rather than as root reaching into a home, which is both rude and a
/// race against a running Flatseal.
pub(crate) const FLATPAK_OVERRIDES_USER_SERVICE: &str = "\
[Unit]
Description=Converge this account's Flatpak permission overrides

[Service]
Type=oneshot
ExecStart=/usr/bin/kuma flatpak-overrides --scope user

[Install]
WantedBy=default.target
";

pub(crate) const FLATPAK_SYNC_SERVICE: &str = r#"[Unit]
Description=Converge Flatpak applications to the declared list
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=1h
StartLimitBurst=6

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-flatpak-sync
Restart=on-failure
RestartSec=2min
CPUWeight=25
IOWeight=25

[Install]
WantedBy=multi-user.target
"#;

/// Boot-only convergence goes stale on machines that stay up: a
/// two-week uptime means a two-week-old browser. Daily, with catch-up
/// for machines that were asleep at the appointed hour.
///
/// The name is load-bearing: the timer shipped enabled as
/// `kuma-flatpak-sync.timer`, and a machine updated into the gated daily
/// unit keeps that enabled timer and gains the gate with it, through the
/// `Unit=` line below retargeting what the timer fires.
pub(crate) const FLATPAK_SYNC_TIMER: &str = r#"[Unit]
Description=Daily Flatpak convergence

[Timer]
OnCalendar=daily
Persistent=true
RandomizedDelaySec=1h
Unit=kuma-flatpak-sync-daily.service

[Install]
WantedBy=timers.target
"#;

/// The unit the daily timer fires. Deliberately not the boot service:
/// the boot run is the convergence promise and never asks permission,
/// and `kuma sync` starts the boot unit for the same reason. Only the
/// timer's run passes the gate, whose skip marker its ExecStart
/// honours — and only the timer's run is `full`, the mode that carries
/// the currency passes: boot converges, the timer also keeps what the
/// declaration never named current.
pub(crate) const FLATPAK_SYNC_DAILY_SERVICE: &str = r#"[Unit]
Description=Daily Flatpak convergence, skipped on battery or a metered connection
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=1h
StartLimitBurst=6

[Service]
Type=oneshot
RuntimeDirectory=kuma-converge-gate
ExecStartPre=/usr/libexec/kuma-converge-gate
ExecStart=/usr/libexec/kuma-flatpak-sync full
Restart=on-failure
RestartSec=2min
CPUWeight=25
IOWeight=25
"#;

/// Read-only btrfs snapshots of the declared subvolume, pruned to the
/// declared retention. The parameters are baked in by `snapshot_script`.
///
/// Two guards, both of which exit clean rather than fail: a target that
/// isn't btrfs, and a target that is btrfs but not a subvolume. The
/// declaration is one file across many machines, and a laptop laid out
/// with ext4 must not turn into a unit that fails on every timer tick.
///
/// `.snapshots` lives inside the target on purpose. It is a directory
/// holding nested subvolumes, and btrfs does not recurse into those when
/// it snapshots the parent, so the snapshots never contain each other.
///
/// It is traversable (0755) so that recovering a file is a file manager
/// and a copy rather than a root shell. That exposes nothing: a snapshot
/// preserves the permissions of everything inside it, so a home directory
/// that was 0700 in the live tree is still 0700 in every snapshot of it.
/// Only the timestamps in the listing become public.
pub(crate) const SNAPSHOT_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
target='{target}'
keep_recent={keep_recent}
keep_daily={keep_daily}
store="$target/.snapshots"

# -T, so the question is "what filesystem holds this path" rather than
# "what is mounted exactly here". A btrfs subvolume does not have to be a
# mount point: on a machine kuma installed, /var/home is a subvolume
# nested inside the deployment's /var, and the bare form prints nothing
# and sent this script home. Machines whose installer gave /var/home its
# own fstab entry answered either way, which is the only reason this ever
# looked like it worked. `kuma doctor` has always asked with -T; these
# two have to ask the same question or one of them is lying.
[ "$(findmnt -no FSTYPE -T "$target" 2>/dev/null || true)" = btrfs ] || exit 0
btrfs subvolume show "$target" >/dev/null 2>&1 || exit 0

install -d -m 0755 "$store"
btrfs subvolume snapshot -r "$target" "$store/$(date +%Y-%m-%dT%H%M%S)" >/dev/null

# Newest first. Keep keep_recent whatever their age, then the newest
# survivor of each of keep_daily days the recent tier did not already
# cover, and delete the rest. Days the recent tier spans are marked seen
# without spending a daily slot, so keep_daily always buys that many
# *further* days back rather than being eaten by a busy afternoon. Only
# names this script writes are ever considered, let alone deleted.
mapfile -t all < <(ls -1 "$store" 2>/dev/null | grep -E '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}$' | sort -r)
declare -A day_seen=()
recent=0
days=0
for snap in "${all[@]}"; do
    day="${snap%%T*}"
    if [ "$recent" -lt "$keep_recent" ]; then
        recent=$((recent + 1))
        day_seen[$day]=1
        continue
    fi
    if [ -z "${day_seen[$day]:-}" ] && [ "$days" -lt "$keep_daily" ]; then
        day_seen[$day]=1
        days=$((days + 1))
        continue
    fi
    btrfs subvolume delete "$store/$snap" >/dev/null
done
"#;

/// Copy the newest local snapshot to the declared repository.
///
/// **Why it mounts the snapshot over the live path.** restic 0.19.1 has
/// no `--set-path` (checked against the package Fedora ships, not
/// assumed), and its default `--group-by` is `host,paths`. A snapshot
/// directory is named for the minute it was taken, so backing one up
/// directly would hand restic a different source path every night: no
/// parent snapshot would ever match, and every run would re-read every
/// byte of a 93 GB home to discover it already had it. Content
/// addressing means that costs no storage, only the whole disk read
/// nightly, which is the kind of waste that gets a backup turned off.
///
/// Mounting the snapshot over `target` in this unit's own mount
/// namespace fixes both halves at once. restic sees a stable path, so
/// parents match and the run is incremental; and the path it records is
/// the one the files actually live at, so a restore lands where it
/// belongs rather than under some staging directory. `PrivateMounts=yes`
/// keeps the bind inside the unit, so nothing on the running system sees
/// its home replaced by a read-only copy for the duration.
///
/// **Three guards that exit 0 rather than failing**, because each is a
/// machine that is not ready rather than a machine that is broken, and
/// one declaration describes many machines: no credential loaded yet, no
/// snapshot taken yet, and no repository at the far end. The last one is
/// deliberate rather than helpful: seeding 93 GB is `kuma backup --init`,
/// a thing somebody does on purpose while plugged in, not something a
/// timer decides to start on a train. Freshness is what escalates a
/// machine that stays in one of these states, which is why the stamp is
/// only written by a run that actually copied something.
pub(crate) const BACKUP_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
target='{target}'
store="$target/.snapshots"
stamp=/var/lib/kuma/backup-last

export RESTIC_REPOSITORY='{repo}'


if [ -z "${RESTIC_PASSWORD:-}${RESTIC_PASSWORD_FILE:-}" ]; then
    echo "no credential loaded: the declaration names one and this machine has not been given it"
    exit 0
fi

# `|| true` is load-bearing under `set -o pipefail`: with no snapshot
# yet, grep exits 1, the assignment inherits it, and `set -e` would kill
# the script one line above the guard written for exactly that case. The
# guard would have been unreachable on every machine it was for. `head`
# closing the pipe early can end `sort` the same way.
newest=$(ls -1 "$store" 2>/dev/null \
    | grep -E '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}$' | sort -r | head -1 || true)
if [ -z "$newest" ]; then
    echo "no snapshot in $store yet; nothing to copy"
    exit 0
fi

# restic keeps a cache of the repository's index and metadata, and treats
# being unable to open one as fatal rather than as a reason to go slower.
# A systemd system service has no HOME and no XDG_CACHE_HOME, so without
# this every run dies before it reaches the repository, saying "neither
# $XDG_CACHE_HOME nor $HOME are defined" from somewhere that reads like a
# network failure.
#
# /var/cache rather than a temporary directory, because the cache is why
# a second backup is quick: discarding it nightly means re-fetching the
# index to discover nothing changed. Below the guards above, because
# those need no repository and this needs root.
export RESTIC_CACHE_DIR=/var/cache/restic
install -d -m 0700 /var/cache/restic

# Bounded, because restic treats a missing bucket as a transient error
# and retries it with exponential backoff. Asking "is there a repository"
# of a machine that has not been seeded therefore takes minutes to answer
# "no", every night, and restic offers no flag to limit backend retries
# (--retry-lock is about locks; --stuck-request-timeout defaults to 5m).
#
# The two failures are told apart by what restic said rather than by an
# exit status, because a timeout looks identical either way. Both exit 0:
# one is a machine nobody has seeded and the other is a machine that
# cannot reach its repository right now, and neither is broken. Freshness
# is what escalates a machine that stays in either state.
probe=$(timeout 30 restic cat config 2>&1) || {
    if printf '%s' "$probe" | grep -qiE 'does not exist|is there a repository'; then
        echo "no repository at $RESTIC_REPOSITORY yet; seed it once with 'kuma backup --init'"
    else
        echo "cannot reach $RESTIC_REPOSITORY within 30s; nothing copied this run"
    fi
    exit 0
}

# Read-only already, being a btrfs snapshot; the bind is for the path,
# not for the permissions.
# Undone on the way out, and that matters outside this unit rather than
# inside it. Under PrivateMounts=yes the namespace dies with the service
# and takes the bind with it; run by hand, which is the obvious thing to
# try after "systemctl start kuma-snapshot.service" works, an untrapped
# bind leaves the live /var/home replaced by a read-only snapshot until
# the next reboot.
mount --bind "$store/$newest" "$target"
trap 'umount "$target" 2>/dev/null || true' EXIT

restic backup "$target"{extra_paths} \
    --tag kuma \
    --skip-if-unchanged \
    --exclude "$target/.snapshots" \
{excludes}
# Forget every run, prune weekly, and they are different costs. Forgetting
# drops snapshot references and is nearly free. Pruning walks the index and
# repacks pack files that expiry left mostly empty, which on a home
# directory over somebody's uplink can move gigabytes, and restic's own
# advice is to do it from time to time rather than after every backup.
# Retention still takes effect immediately either way; only the reclaiming
# waits.
#
# Timed off a stamp rather than a weekday, because a machine that is never
# on for the chosen day would never prune at all, and that is exactly the
# laptop this is for.
restic forget --tag kuma \
    --keep-daily {keep_daily} --keep-weekly {keep_weekly} --keep-monthly {keep_monthly}
# Before anything writes into it. The stamp below made the directory and
# the prune stamp above it did not, so a machine where /var/lib/kuma did
# not already exist would fail the unit after a backup that worked, which
# is the most expensive place to discover an ordering.
install -d -m 0755 /var/lib/kuma

pruned=/var/lib/kuma/backup-pruned
if [ ! -f "$pruned" ] || [ "$(( $(date -u +%s) - $(cat "$pruned" 2>/dev/null || echo 0) ))" -ge 604800 ]; then
    restic prune
    date -u +%s > "$pruned"
fi

# Epoch first because doctor parses it and this repo carries no date
# library; the readable form beside it is for whoever cats the file.
printf '%s %s\n' "$(date -u +%s)" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$stamp"
"#;

/// The curated excludes, which are additive rather than a default the
/// declaration replaces.
///
/// Every one is a tree this same file already rebuilds, so storing it
/// would be paying to keep a copy of something kuma can recreate from
/// six lines. `/home` is a symlink to `var/home`, which puts Homebrew's
/// entire prefix inside the snapshot target: `packages.brew` reconverges
/// it, so a naive copy would ship the largest rebuildable tree on the
/// machine offsite every night.
/// The one unrecoverable thing that lives outside the snapshot target.
/// Named here rather than spelled twice, since doctor reports on the
/// same path.
pub const NETWORK_CONNECTIONS: &str = "/etc/NetworkManager/system-connections";

pub(crate) const CURATED_EXCLUDES: &[&str] =
    &["/linuxbrew", "/*/.cache", "/*/.local/share/containers"];

/// Restart because a timer that fires on resume finds the network still
/// coming up, and `network-online.target` only orders boot. The start
/// limit is what keeps that from becoming an infinite retry against a
/// repository that is simply gone: six tries an hour, then stop and let
/// doctor's freshness line be the thing that says so.
pub(crate) fn backup_service(config: &Config) -> String {
    format!(
        "[Unit]\nDescription=Copy the newest snapshot to the declared repository\n\
         Wants=network-online.target\nAfter=network-online.target\n\
         After=kuma-snapshot.service\n\
         StartLimitIntervalSec=1h\nStartLimitBurst=6\n\n\
         [Service]\nType=oneshot\nExecStart=/usr/libexec/kuma-backup\n\
         EnvironmentFile=-/var/lib/kuma/secrets/{secret}.env\n\
         PrivateMounts=yes\n\
         Restart=on-failure\nRestartSec=2min\nCPUWeight=25\nIOWeight=25\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        secret = config.backup.secret,
    )
}

pub(crate) fn backup_timer(interval: &str) -> String {
    format!(
        "[Unit]\nDescription=Scheduled offsite backup\n\n[Timer]\nOnCalendar={interval}\nPersistent=true\nRandomizedDelaySec=1h\n\n[Install]\nWantedBy=timers.target\n"
    )
}

/// The script with this declaration's repository, retention and excludes
/// baked in. Validation has already restricted every substitution to a
/// conservative alphabet, and refused a repository with a password in it.
///
/// A declared `~/` means "in every home", which is the only reading that
/// works when the target holds more than one.
pub(crate) fn backup_script(config: &Config) -> String {
    let target = &config.snapshots.target;
    let mut excludes = String::new();
    // `$target` rather than the substituted path, so the script has one
    // definition of where it is looking and the line beside this one
    // (`--exclude "$target/.snapshots"`) is spelled the same way.
    for suffix in CURATED_EXCLUDES {
        excludes.push_str(&format!("    --exclude \"$target{suffix}\" \\\n"));
    }
    for path in &config.backup.exclude {
        let path = match path.strip_prefix("~/") {
            Some(rest) => format!("$target/*/{rest}"),
            None => path.clone(),
        };
        excludes.push_str(&format!("    --exclude \"{path}\" \\\n"));
    }
    // The last continuation has to go, or the blank line after it eats
    // the next command.
    let excludes = excludes.trim_end().trim_end_matches('\\').trim_end().to_string();
    // A second source path, so the one unrecoverable thing outside home
    // rides along when it is asked for. It changes restic's path group,
    // so the first run after switching this on has no parent and rescans
    // once; every run after that is incremental again.
    let extra_paths = if config.backup.network_connections {
        format!(" {NETWORK_CONNECTIONS}")
    } else {
        String::new()
    };
    BACKUP_SCRIPT
        .replace("{extra_paths}", &extra_paths)
        .replace("{target}", target)
        .replace("{repo}", &config.backup.repo)
        .replace("{excludes}", &excludes)
        .replace("{keep_daily}", &config.backup.keep_daily.to_string())
        .replace("{keep_weekly}", &config.backup.keep_weekly.to_string())
        .replace("{keep_monthly}", &config.backup.keep_monthly.to_string())
}

/// Put a home directory back on a machine that has just been installed.
///
/// **Why this is a first-boot unit and not part of `kuma install`.**
/// `/var/home` does not exist at install time. The image ships none;
/// `rpm-ostree-0-integration.conf` has tmpfiles create it every boot,
/// and `kuma-home-subvol` turns it into a btrfs subvolume with the right
/// SELinux label while it is still empty. Restoring during the install
/// would leave an ordinary directory with files in it, which is exactly
/// the state `kuma-home-subvol` steps back from, so the machine would
/// come up with no subvolume, no snapshots and nothing saying why.
///
/// So the install writes the request and the credential onto the target
/// the same way it writes the account, and this runs once, after the
/// subvolume exists and after the account it will own does.
///
/// **The request survives a failed restore, and is cleared only by one
/// that worked.** The first version cleared it first, reasoning that a
/// request outliving its own failure means a unit that re-runs forever.
/// That was wrong twice over. There is no `Restart=` here, so the loop
/// it guarded against cannot happen: what survival actually buys is one
/// attempt per boot. And the failure it caused is the worse one, because
/// a repository briefly unreachable at first boot would discard the
/// restore permanently and bring the machine up empty, with the data
/// still sitting safe somewhere nobody would think to look again.
///
/// A missing credential still clears it, because that is not a bad day,
/// it is a request nothing can ever satisfy.
pub(crate) const RESTORE_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
request=/var/lib/kuma/restore-request
secret=/var/lib/kuma/secrets/restore.env

[ -f "$request" ] || exit 0
if [ ! -r "$secret" ]; then
    echo "a restore was requested and $secret is not there" >&2
    rm -f "$request"
    exit 1
fi

# Nothing is sourced here. The values arrive through the unit's
# EnvironmentFile=, and what this checks is that they arrived: an
# unparseable file leaves the variable unset, and `set -u` would
# otherwise abort with a message about a shell variable rather than
# about the file somebody wrote.
if [ -z "${RESTIC_REPOSITORY:-}" ]; then
    echo "$secret set no RESTIC_REPOSITORY that systemd could parse" >&2
    rm -f "$request"
    exit 1
fi

# Same reason as the converger: a unit has no HOME, and restic will not
# start without somewhere to cache.
export RESTIC_CACHE_DIR=/var/cache/restic
install -d -m 0700 /var/cache/restic

# Both paths the converger stores, not just home. Backing up the network
# connections and then not restoring them defeats the only reason that
# knob exists, and it defeats it silently: the machine comes up, the
# files are all there, and the one thing nothing else can recreate is
# missing. Naming a path the snapshot does not hold restores nothing and
# is not an error, so this is safe when the knob was off.
#
# --tag kuma so a repository somebody also uses by hand cannot hand this
# machine a snapshot kuma never made.
echo "restoring /var/home from $RESTIC_REPOSITORY"
restic restore latest --tag kuma --target / \
    --include /var/home \
    --include /etc/NetworkManager/system-connections

# Only now. Everything above can fail on a bad day (a repository that is
# briefly unreachable, a link that drops), and a bad day must cost a
# retry at the next boot rather than the data.
rm -f "$request"
echo "restore finished"
"#;

/// Ordered behind both of the units that have to have run first, and
/// conditioned on the request file so that every other boot skips it
/// without a unit that failed.
pub(crate) const RESTORE_SERVICE: &str = r#"[Unit]
Description=Restore this machine's home directory from the declared repository
ConditionPathExists=/var/lib/kuma/restore-request
Wants=network-online.target
After=network-online.target
After=kuma-home-subvol.service kuma-user-sync.service
Before=greetd.service

[Service]
Type=oneshot
# systemd PARSES this; it never executes it. The script used to source
# the same file, which ran whatever was on a right-hand side as root on
# first boot, on a file concepts.md tells people to carry on the stick
# beside the ISO. The backup timer beside this one already read it this
# way, so the two halves of one file now agree.
#
# `-` so a missing file is not a unit failure: the script checks for it
# and says so in words, which is the better message.
EnvironmentFile=-/var/lib/kuma/secrets/restore.env
ExecStart=/usr/libexec/kuma-restore
TimeoutStartSec=infinity

[Install]
WantedBy=multi-user.target
"#;

pub(crate) const SNAPSHOT_SERVICE: &str = r#"[Unit]
Description=Snapshot the declared btrfs subvolume

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-snapshot
CPUWeight=25
IOWeight=25

[Install]
WantedBy=multi-user.target
"#;

/// Persistent so a laptop that was asleep at the appointed hour still
/// gets its snapshot, and jittered so a fleet doesn't stampede a shared
/// disk at the top of the hour.
pub(crate) fn snapshot_timer(interval: &str) -> String {
    format!(
        "[Unit]\nDescription=Scheduled btrfs snapshots\n\n[Timer]\nOnCalendar={interval}\nPersistent=true\nRandomizedDelaySec=5m\n\n[Install]\nWantedBy=timers.target\n"
    )
}

/// The script with this declaration's retention baked in. Validation has
/// already restricted every substitution to a conservative alphabet.
pub(crate) fn snapshot_script(config: &Config) -> String {
    SNAPSHOT_SCRIPT
        .replace("{target}", &config.snapshots.target)
        .replace("{keep_recent}", &config.snapshots.keep_recent.to_string())
        .replace("{keep_daily}", &config.snapshots.keep_daily.to_string())
}

/// Toggle screen recording: wf-recorder to ~/Videos, notifications on
/// both edges. SIGINT lets wf-recorder finalize the file properly.
pub(crate) const RECORD_SCRIPT: &str = r#"#!/usr/bin/bash
set -u
if pgrep -x wf-recorder >/dev/null; then
    pkill -INT -x wf-recorder
    notify-send "Recording stopped" "Saved in ~/Videos"
else
    dir="$HOME/Videos"
    mkdir -p "$dir"
    f="$dir/recording-$(date +%Y%m%d-%H%M%S).mp4"
    notify-send "Recording started" "Mod+Alt+R stops it."
    exec wf-recorder -f "$f"
fi
"#;

/// Convergence, not just installation: an app the declaration installed
/// and no longer names is removed, so deleting a line in kuma.toml has
/// the same authority as adding one. That authority covers applications;
/// the unused-runtime prune is the one exception and says so where it is
/// argued below.
///
/// Authority is tracked explicitly, in a state file of what the
/// declaration installed, exactly as the brew sync does — and for the
/// same reason, now that scope can no longer stand in for it. This once
/// removed every undeclared *system* app, reading `--system` as "kuma
/// put it here" and `--user` as "the owner did". That proxy held only
/// while nothing else installed system-wide, and a Flatpak store breaks
/// it: Bazaar's flatpak build talks to the system installation through
/// SystemHelper, so apps a person deliberately installed looked exactly
/// like drift and were silently uninstalled hours later. Convergence now
/// takes back only what it gave. Everything else on the machine is the
/// owner's, and `kuma capture` offers to write it down.
///
/// The uninstall tolerates failure because the state file can name an
/// app the owner already removed by hand, which is not an error.
///
/// **The prune reaches past the declaration, and this is the sentence
/// saying so.** `flatpak uninstall --unused` removes any runtime no
/// installed app needs, including one somebody installed on purpose. It
/// is kept because a runtime nothing references is dead weight of the
/// kind an image-based system exists to avoid, and because a runtime is
/// infrastructure rather than a choice: reinstalling one is a download,
/// not a decision. It runs in `full` mode only — the daily, gated run —
/// because keeping the machine current is the timer's question, not the
/// boot's. The brew converger makes the same trade, and its own script
/// comment admits it; the convergence claim above is true of
/// applications, which is what it was written about.
///
/// Membership and currency are different questions, and only the first
/// belongs to the declaration. Convergence decides what exists; the
/// update decides how old it is, and it is deliberately unscoped — an
/// app the owner installed through the store is theirs to keep, but
/// leaving it to rot is not respect, it is an unpatched browser. The
/// same call covers runtimes, which the declared install reaches only
/// when a declared app happens to demand a newer one. Unscoped or not,
/// currency is the daily run's mode, so it waits for the gate the way
/// every other update does.
///
/// Ordering is load-bearing. The state file is written before the
/// update, so a failed update (a flaky network, one broken remote)
/// cannot leave authority tracking behind and silently strand a removal
/// until the next successful run. Pruning comes last so the runtimes the
/// update orphans go in the same pass.
///
/// Both download paths retry without static deltas, because a remote can
/// serve a delta this machine refuses. ostree caps how large a
/// decompressed delta part may be, the cap is computed per machine, and
/// a delta over it fails byte-identically on every retry: Flathub's
/// Firefox did exactly this and the unit failed six times before systemd
/// stopped trying. Retrying is what the whole download is for. It costs
/// bandwidth on the path that already failed and nothing anywhere else,
/// since the second pass sees only what is still pending.
///
/// The retry belongs on the install too, not just the update. `xargs`
/// exits 123 when the command it ran failed, which is the status that
/// failure wore, and `--or-update` means the declared-install pass is
/// where an app already present gets its new version. Fixing only the
/// update line would have left the failure exactly where it was.
pub(crate) const FLATPAK_SYNC_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
# The daily timer's unit runs the gate as its ExecStartPre; when the gate
# decided to skip, it left the marker, and this run ends green having
# done nothing. The boot unit and `kuma sync` never run the gate, and
# the marker lives in a runtime directory systemd removes when the daily
# unit deactivates, so for every other caller it does not exist: the
# gate is the timer's, never the verb's.
if [ -e /run/kuma-converge-gate/skip ]; then
    exit 0
fi
declared=/usr/lib/kuma/flatpaks
state=/var/lib/kuma/flatpaks-installed
mkdir -p /var/lib/kuma
[ -f "$state" ] || : > "$state"

# Two questions, two answers. CONVERGENCE is the declaration's question —
# what is named exists, what kuma installed and the declaration dropped
# is gone — and it runs for every caller, because it is the promise.
# CURRENCY is everything past that — updating ad-hoc applications,
# pruning unused runtimes — and it runs only in `full` mode, which is
# the daily unit's. A boot that finds the machine matching its
# declaration spawns nothing, calls no vendor, and pays no network
# until the declaration changes; the daily run is where ad-hoc
# applications get kept current, behind the gate, on the schedule the
# docs promise them.
install_declared() {
    xargs -r -a "$declared" flatpak install --system --assumeyes --noninteractive --or-update "$@" flathub
}
converge() {
    install_declared || install_declared --no-static-deltas
    while read -r app; do
        grep -qxF "$app" "$declared" \
            || flatpak uninstall --system --assumeyes --noninteractive "$app" || true
    done < "$state"
    cp "$declared" "$state"
}
# The state file is written only by a run that finished, so equal lists
# mean the last run converged and this one has nothing to do. An
# unequal pair is either a declaration that changed or a run that
# died; both are what converging is for.
if ! cmp -s "$declared" "$state"; then
    converge
fi
if [ "${1:-}" = full ]; then
    flatpak update --system --assumeyes --noninteractive \
        || flatpak update --system --assumeyes --noninteractive --no-static-deltas
    flatpak uninstall --system --unused --assumeyes --noninteractive
fi
"#;

/// The daily timer's gate. Decides whether NOW is a time the machine
/// should converge, by two reads: the battery's own sysfs files and
/// NetworkManager's Metered property. It runs as the ExecStartPre of
/// the daily units the timers trigger — never of the boot units, and
/// never of `kuma sync`, which starts the boot unit. The decision needs
/// live power and network state, which is why it is not in the timer:
/// the service is also where the retries already live.
///
/// Skipping is a decision, not a failure, so a skip is answered green:
/// the gate appends the reason to the stamp (`kuma doctor` grades it as
/// information), leaves the skip marker its unit's ExecStart checks, and
/// exits 0. A proceed clears stale skips, because convergence is about
/// to happen.
///
/// The battery threshold is kuma's own, not a knob: 20%, where uupd's
/// measurement settled too. A declaration key would be promise surface
/// for a number nobody asked to change.
///
/// Both roots are arguments so the decision table is runnable in tests:
/// `[sysfs] [stamp] [marker]`, defaulting to the real paths.
pub(crate) const CONVERGE_GATE_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
sysfs=${1:-/sys/class/power_supply}
stamp=${2:-/var/lib/kuma/convergence-skipped}
marker=${3:-/run/kuma-converge-gate/skip}
mkdir -p "$(dirname "$marker")"
rm -f "$marker"

reasons=""
for supply in "$sysfs"/*/; do
    [ "$(cat "${supply}type" 2>/dev/null)" = "Battery" ] || continue
    status=$(cat "${supply}status" 2>/dev/null || true)
    capacity=$(cat "${supply}capacity" 2>/dev/null || echo 100)
    # A machine is on battery when a battery is discharging, and its
    # charge is that battery's own reading. Charging, full and absent
    # supplies all mean the wall is there.
    if [ "$status" = "Discharging" ] && [ "$capacity" -lt 20 ]; then
        reasons="on battery (${capacity}%)"
        break
    fi
done

# NetworkManager's own property, not a desktop setting: kuma is
# desktop-agnostic and NM is on every kuma image, so the same GNOME
# toggle that pauses uupd pauses this, and niri and COSMIC machines get
# it for free. 1 and 3 are "yes" and "guessed yes". A failed read or an
# unknown answers no: the gate may starve a run on a bad guess, but not
# on a missing answer.
metered=$(busctl get-property org.freedesktop.NetworkManager \
    /org/freedesktop/NetworkManager org.freedesktop.NetworkManager Metered 2>/dev/null \
    | tr -dc '0-9' || true)
case "$metered" in
    1 | 3) reasons="${reasons:+$reasons; }on a metered connection" ;;
esac

if [ -n "$reasons" ]; then
    mkdir -p "$(dirname "$stamp")"
    echo "$(date -u '+%Y-%m-%dT%H:%M:%SZ') ${reasons}" >> "$stamp"
    : > "$marker"
    exit 0
fi
rm -f "$stamp"
"#;

/// Doctor prints a `remote-add` for the same address, so a move here
/// must not leave it sending people somewhere the image disagrees with.
pub const FLATHUB_URL: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// `kuma vm` passes the host's timezone over qemu's fw_cfg channel
/// (bootc-image-builder silently ignores [customizations.timezone] for
/// qcow2 builds). This service adopts it at boot; on real hardware the
/// fw_cfg key doesn't exist and the service is a no-op.
pub(crate) const VM_TZ_SERVICE: &str = r#"[Unit]
Description=Adopt the host timezone passed by kuma vm
Before=systemd-user-sessions.service

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-vm-timezone

[Install]
WantedBy=multi-user.target
"#;

pub(crate) const VM_TZ_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
modprobe -q qemu_fw_cfg 2>/dev/null || exit 0
raw=/sys/firmware/qemu_fw_cfg/by_name/opt/org.kuma.tz/raw
[ -r "$raw" ] || exit 0
tz=$(tr -d '\0' < "$raw")
[ -e "/usr/share/zoneinfo/$tz" ] || exit 0
ln -sfn "../usr/share/zoneinfo/$tz" /etc/localtime
"#;

/// Makes `/var/home` a btrfs subvolume while it is still empty, which is
/// the only window there is.
///
/// `[snapshots]` takes a btrfs snapshot, and a snapshot is of a
/// subvolume. On a machine kuma installed, `/var/home` was an ordinary
/// directory inside the deployment's `/var`: the snapshot script exits 0
/// on a target it cannot snapshot, so the unit succeeded, the timer
/// stayed active, and the machine reported itself healthy while taking
/// nothing, forever. Anaconda-installed machines were never affected,
/// which is how it went unnoticed.
///
/// At boot rather than at install, for two reasons that only showed up
/// by trying the install first. The image ships no `/var/home`, since
/// `rpm-ostree-0-integration.conf` has tmpfiles create it on every boot,
/// so at install time there is nothing to replace. And the directory
/// tmpfiles creates already carries the right SELinux label, which a
/// subvolume made by the installer would have to name for itself.
///
/// Empty is the whole of the safety. Moving home directories is not
/// something a boot-time unit may do, so a machine that already has one
/// is left exactly as it is and `kuma doctor` reports it instead.
pub(crate) const HOME_SUBVOL_SERVICE: &str = r#"[Unit]
Description=Give /var/home its own btrfs subvolume while it is empty
ConditionPathExists=/run/ostree-booted
# The same slot systemd-tmpfiles-setup runs in, one step later, and the
# reason is a race that took two intermittent failures to see as one.
#
# Making /var/home a subvolume means `rmdir` then `btrfs subvolume
# create`, and between those two commands the directory does not exist.
# Twenty five units on a desktop image carry ProtectHome, which makes
# systemd mount something over /var/home before the service starts, so
# the window has two losers. If this converger wins it, firewalld starts
# into a missing directory and dies with 226/NAMESPACE. If the sandboxed
# unit wins it, the mount pins the directory, `rmdir` fails with EBUSY,
# this unit dies under set -e, and /var/home stays an ordinary directory
# forever, which costs [snapshots] everything and says nothing.
#
# Listing those units in Before= was the first attempt and it was the
# wrong shape: they do not write to /var/home, systemd merely binds it
# for them, and there are twenty five of them and counting. Running
# before sysinit.target closes the window against all of them at once,
# including systemd-resolved, -timesyncd and -userdbd, which start too
# early for any multi-user.target ordering to matter.
DefaultDependencies=no
RequiresMountsFor=/var
After=systemd-tmpfiles-setup.service
Before=sysinit.target
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/libexec/kuma-home-subvol

[Install]
WantedBy=sysinit.target
"#;

pub(crate) const HOME_SUBVOL_SCRIPT: &str = r#"#!/usr/bin/bash
# Generated by kuma. Takes the target as an argument so the branch that
# does the work is reachable in a test: on a converged machine it
# returns at the second line forever.
set -euo pipefail
target=${1:-/var/home}

# Every exit says why. Declining is the common case and the right one on
# every boot after the first, but it is also what a missed first boot
# looks like, and those were indistinguishable while both were a silent
# exit 0: the unit succeeds, the machine boots healthy, and the only
# evidence is an inode nobody looks at.
say() { echo "kuma-home-subvol: $*"; }

# Which command died, not just that one did. This unit fails on a small
# fraction of first boots, leaving /var/home an ordinary directory
# forever, and until this trap existed the only trace was `systemctl
# is-failed` saying "failed": the work below is four commands and
# `set -e` names none of them. Costs nothing on the boots that succeed.
trap 'say "FAILED at line $LINENO running: $BASH_COMMAND"' ERR

[ -d "$target" ] || { say "$target does not exist yet; nothing to do"; exit 0; }

# head -1 because findmnt prints a line per mount when something is
# stacked at or under the path, and a two-line answer never equals
# "btrfs", which would decline on a machine that is in fact btrfs.
fstype=$(findmnt -no FSTYPE -T "$target" 2>/dev/null | head -1 || true)
[ "$fstype" = btrfs ] \
    || { say "$target is on ${fstype:-an unknown filesystem}, not btrfs; nothing to do"; exit 0; }

# Inode 256 is every btrfs subvolume root, and asking this way needs no
# privilege and no btrfs command. Already one: nothing to do, on every
# boot after the first.
[ "$(stat -c %i "$target")" = 256 ] && { say "$target is already a subvolume"; exit 0; }

# Only while nothing lives there. This runs before the account converger
# and before any login, so on a first boot it is empty; on any later one
# it holds somebody's home directory and this must leave it alone.
if [ -n "$(ls -A "$target")" ]; then
    say "$target already holds $(ls -A "$target" | tr '\n' ' ')"
    say "leaving it alone: making it a subvolume now would mean moving what is in it"
    exit 0
fi

# Carried across rather than assumed: tmpfiles made this directory with
# the mode and label the system expects, and the subvolume replacing it
# should be indistinguishable.
mode=$(stat -c %a "$target")
label=$(stat -c %C "$target" 2>/dev/null || echo '?')
rmdir "$target"
btrfs subvolume create "$target" >/dev/null
chmod "$mode" "$target"
if [ "$label" != '?' ]; then chcon "$label" "$target"; fi
"#;

/// Runs before any login (console, greeter, ssh) so the declared account
/// exists the first time a login prompt appears.
pub(crate) const USER_SYNC_SERVICE: &str = r#"[Unit]
Description=Converge the declared user account
Before=systemd-user-sessions.service greetd.service

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-user-sync

[Install]
WantedBy=multi-user.target
"#;

/// Creation happens at boot, not image build: /home is machine state
/// (/var/home), so an image-built home directory would exist only on
/// disk-image installs and be missing after a `bootc switch`. The
/// password hash applies only at creation — passwords are machine state.
/// Groups converge additively so imperative grants (docker, libvirt)
/// made on the machine survive.
/// Two sources, and which one wins is the whole point.
///
/// /usr is the image and /etc is machine state, the line kuma draws
/// everywhere else, and a user belongs on both sides of it depending on
/// who the image is for. On a personal image the account is declaration:
/// it is baked, and every machine built from that image is meant to have
/// it. On a *published* image it cannot be, because the image is shared
/// and the person is not — so a published image declares no [user] at
/// all, and a machine installed from one would have no account, no root
/// password, and no way in.
///
/// So an installer writes /var/lib/kuma/user on the target and this prefers
/// it. Same fields, same converger, machine state instead of image
/// content. It also gives a machine that rebased onto kuma a way to
/// declare an account without rebuilding anything.
pub(crate) const USER_SYNC_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
# shellcheck disable=SC1091  # both sources are written by kuma, not present here
# Machine state wins over image content. Neither is an error: this unit
# ships in every image now, including ones that declare no account,
# because a published image is exactly that and still needs the converger
# present for whatever an installer writes later.
#
# /var, not /etc, and the difference is not cosmetic. bootc populates /var
# from the image once at install and never touches it again, which is
# exactly what install-time state needs. /etc is three-way merged on every
# update: a file the installer shipped as image content sits in that
# deployment's /usr/etc unmodified, and the published image it updates
# from has no such file, so the merge deletes it. The account would
# survive (it is created at first boot) while the thing describing it
# vanished, and the machine would quietly stop matching what was written
# down.
if [ -f /usr/lib/kuma/user ]; then
    . /usr/lib/kuma/user
fi
if [ -f /var/lib/kuma/user ]; then
    # The installer answers for the person; the image still answers for
    # itself. Clearing the account keys first means an image that
    # declared a user cannot lend its name, password or groups to the
    # account somebody typed at install time. KUMA_SHELL is the one key
    # deliberately left to carry over: [system].shell describes the
    # image, not a person, so an image that installs fish says so once
    # and an installer that was told nothing about shells inherits it.
    unset KUMA_USER KUMA_PASSWORD_HASH KUMA_GROUPS
    . /var/lib/kuma/user
fi

# Written by `kuma install`, for the same reason and with one extra step.
# /etc/hostname IS image content (every kuma image bakes one), so writing
# it here makes it a local modification, which is what survives the merge.
# Shipping it in the installer's layer instead would revert to the
# published image's hostname on the first update.
if [ -f /var/lib/kuma/hostname ]; then
    want=$(cat /var/lib/kuma/hostname)
    if [ -n "$want" ] && [ "$(cat /etc/hostname 2>/dev/null)" != "$want" ]; then
        echo "$want" > /etc/hostname
        hostnamectl set-hostname "$want" 2>/dev/null || true
    fi
fi

[ -n "${KUMA_USER:-}" ] || exit 0

# Written by the installs `kuma vm` performs: the key that lets the
# machine running the build reach the disk it built. It lands in
# /etc/kuma/keys/$KUMA_USER — the directory AuthorizedKeysFile already
# serves (see SSHD_KUMA_KEYS below) — rather than in the account's own
# authorized_keys, which stays the user's to edit or delete. A file
# written here at boot is a local modification, which is what survives
# the merge: the same rule the hostname block above runs on. Rewritten
# every boot rather than only at account creation, so it repairs itself
# the way the hostname does.
if [ -n "${KUMA_SSH_KEY:-}" ]; then
    mkdir -p /etc/kuma/keys
    echo "$KUMA_SSH_KEY" > "/etc/kuma/keys/$KUMA_USER"
fi

if ! id -u "$KUMA_USER" &>/dev/null; then
    args=(-m)
    [ -n "${KUMA_SHELL:-}" ] && args+=(-s "$KUMA_SHELL")
    useradd "${args[@]}" "$KUMA_USER"
    if [ -n "${KUMA_PASSWORD_HASH:-}" ]; then
        echo "$KUMA_USER:$KUMA_PASSWORD_HASH" | chpasswd -e
    fi
fi
[ -n "${KUMA_SHELL:-}" ] && usermod -s "$KUMA_SHELL" "$KUMA_USER"
for group in ${KUMA_GROUPS:-}; do
    usermod -aG "$group" "$KUMA_USER"
done
"#;

/// Declared keys live in root-owned image content, consulted alongside —
/// never instead of — the user's own ~/.ssh/authorized_keys.
pub(crate) const SSHD_KUMA_KEYS: &str = r#"# Kuma-declared keys, alongside the user's own.
AuthorizedKeysFile .ssh/authorized_keys /etc/kuma/keys/%u
"#;

/// The counter/fallback half of boot health is BOOTLOADER config —
/// written once at install time and never rewritten by bootupd — so a
/// machine installed before greenboot entered the image has a grub.cfg
/// that never decrements boot_counter. greenboot userspace relies on
/// grub for the countdown, and without it a failing update reboots
/// forever instead of falling back (observed empirically: the counter
/// sat at 3 across 40+ consecutive boots). grub sources
/// $prefix/custom.cfg at the end of bootupd's static config — after
/// blscfg registers the deployments, before the menu shows — which is
/// the sanctioned hook for exactly this. Converged, not just written:
/// if grub.cfg ever gains native boot_counter handling (fresh installs
/// have it; bootupd may learn to refresh), the block is removed so the
/// counter is never decremented twice per attempt.
pub(crate) const BOOT_HEALTH_SYNC_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
cfg=/boot/grub2/grub.cfg
custom=/boot/grub2/custom.cfg
begin='# >>> kuma boot-health >>>'
end='# <<< kuma boot-health <<<'
[ -f "$cfg" ] || exit 0

# Remounted rw only on the paths that write, and only once: the
# converged boots of this script are two greps and an exit, and paying
# two remounts of /boot for zero writes on every one of them is churn
# the boot journal records and nobody needs. Greps read a ro mount
# happily.
restore=""
remount_rw() {
    if [ -z "$restore" ] && findmnt -n -o OPTIONS /boot 2>/dev/null | grep -qw ro; then
        mount -o remount,rw /boot
        restore="ro"
    fi
}
finish() { [ "$restore" = ro ] && mount -o remount,ro /boot || true; }
trap finish EXIT

if grep -q boot_counter "$cfg"; then
    # The bootloader counts natively; drop our block if we ever wrote one.
    if [ -f "$custom" ] && grep -qF "$begin" "$custom"; then
        remount_rw
        sed -i "\|^$begin|,\|^$end|d" "$custom"
        [ -s "$custom" ] || rm -f "$custom"
    fi
    exit 0
fi
if [ -f "$custom" ] && grep -qF "$begin" "$custom"; then
    exit 0
fi
remount_rw
cat >> "$custom" <<'EOF'
# >>> kuma boot-health >>>
# Managed by kuma-boot-health-sync; do not edit between these markers.
# Boot-counter fallback for bootloaders installed before greenboot
# entered the image: decrement boot_counter each attempt, and boot the
# previous deployment (menu entry 1) when it runs out. Same logic
# greenboot ships for fresh installs via bootupd's static config.
insmod increment
# Check if boot_counter exists and boot_success=0 to activate this behavior.
if [ -n "${boot_counter}" -a "${boot_success}" = "0" ]; then
  # if countdown has ended, choose to boot rollback deployment,
  # i.e. default=1 on OSTree-based systems.
  if  [ "${boot_counter}" = "0" -o "${boot_counter}" = "-1" ]; then
    set default=1
    set boot_counter=-1
  # otherwise decrement boot_counter
  else
    decrement boot_counter
  fi
  save_env boot_counter
fi

# Reset boot_success for current boot
set boot_success=0
save_env boot_success
# <<< kuma boot-health <<<
EOF
"#;

/// Anaconda writes a `/` line into /etc/fstab describing the root as the
/// filesystem it installed onto (btrfs here). On a bootc machine the root
/// is a composefs overlay, so systemd-remount-fs reads that line, tries to
/// remount `/` with those options, and the kernel refuses:
///
///   mount: /: fsconfig() failed: overlay: No changes allowed in reconfigure.
///
/// Nothing downstream depends on it, which is why this was filed as
/// cosmetic and carried a known-benign downgrade in doctor for months. The
/// reason it earns a converger anyway: nothing else ever rewrites that
/// file. It is machine state written once by the installer, so the failure
/// is permanent on every machine kuma installs, and the only cure anyone
/// had was editing the file by hand on each one. Same category as the
/// bootloader's boot counter above, and fixed the same way.
///
/// Deliberately one-directional, unlike the boot-counter block. Leaving
/// the line commented is safe on any bootc machine: the root is already
/// mounted by the initrd, and that line drives nothing but this remount.
/// Uncommenting it again would not be safe, because the only reason to do
/// so would be a detection mistake, and the cost of that mistake is a
/// machine that fails a mount at boot. So the marker records what was done
/// and a human can reverse it; the script never will.
/// Takes the file as an argument, defaulting to the real one, for the same
/// reason inspect.rs's scan_etc takes its roots: on a machine that is
/// already converged the editing branch never runs, so without a way to
/// point it at a fixture this would ship having never once executed the
/// only lines that matter. The unit passes no argument.
// r##: the awk below prints lines starting `"#`, which closes an r#" literal.
pub(crate) const FSTAB_SYNC_SCRIPT: &str = r##"#!/usr/bin/bash
set -euo pipefail
fstab=${1:-/etc/fstab}
[ -f "$fstab" ] || exit 0
# The whole justification for the edit is that the mounted root is not
# what the line claims. Ask the kernel rather than assuming: on anything
# that is not a composefs overlay this exits having done nothing.
[ "$(findmnt -n -o FSTYPE / 2>/dev/null)" = overlay ] || exit 0

# awk, not sed: the target is "the line whose second field is /", which is
# a field test, and writing it as a regex over the whole line is how you
# end up commenting out /boot or a subvolume that merely mentions root.
# Already-commented lines cannot match, so this is idempotent without
# needing to look for its own marker.
new=$(mktemp)
awk '
$1 !~ /^#/ && $2 == "/" {
    print "# Commented out by kuma-fstab-sync: this machine boots a composefs"
    print "# overlay, and systemd-remount-fs fails every boot trying to remount"
    print "# / with the options below. Uncomment only if / stops being an overlay."
    print "#" $0
    next
}
{ print }
' "$fstab" > "$new"

if cmp -s "$fstab" "$new"; then
    rm -f "$new"
    exit 0
fi
# cat into the existing file rather than mv over it: this keeps the inode,
# the mode, and the SELinux label that a rename from /tmp would replace.
cat "$new" > "$fstab"
rm -f "$new"
# The generated mount units come from this file, so tell systemd it moved.
# Nothing is remounted here: the failing remount is the thing being
# prevented, and this boot has already had it. Skipped when running
# against a fixture, which has no business reloading the host's systemd.
[ -n "${1:-}" ] || systemctl daemon-reload
"##;

/// multi-user, not early boot, and the cost is understood: systemd-remount-fs
/// runs in sysinit, so the machine that installs today still fails this unit
/// once and comes up clean on every boot after. Ordering this before it
/// instead would need DefaultDependencies=no and a hand-built ordering into
/// the middle of early boot, where a cycle does not produce a failed unit,
/// it produces a machine that does not boot. That is a bad trade against a
/// failure whose entire consequence is a red line in `systemctl --failed`.
/// What the policy has to be told, or a swapfile is unusable for the one
/// thing it was made for.
///
/// Measured on a real machine rather than reasoned about. A swapfile at
/// `/var/swap/swapfile` gets `var_t` from the policy's own default, and
/// `systemd-sleep` cannot read a `var_t` file:
///
/// ```text
/// avc: denied { read } for comm="systemd-sleep" name="swapfile"
///      scontext=systemd_sleep_t tcontext=var_t tclass=file
/// systemd-sleep: Failed to find location to hibernate to: Permission denied
/// ```
///
/// `systemd-logind` is denied the same way, which is why `CanHibernate`
/// answers "Access denied" on such a machine rather than yes or no. The
/// machine has a correct swapfile, correct kernel arguments, and cannot
/// hibernate.
///
/// So `restorecon` alone is not the fix: run against the stock policy it
/// produces exactly the `var_t` that fails. The path has to be declared
/// a swapfile first, which is what this does, and then `restorecon`
/// gives `swapfile_t` and hibernation works. Written as policy rather
/// than as a `chcon` somewhere because a rule survives a full relabel
/// and a hand-applied label does not.
pub(crate) const SWAP_FCONTEXT: &str = "\
# Added by kuma. Without this line the policy's default for a file at
# this path is var_t, and systemd-sleep cannot read a var_t file, so the
# machine gets a swapfile it can never hibernate into.
/var/swap/swapfile\t--\tsystem_u:object_r:swapfile_t:s0
";

/// Applying the rule above on the machine that has the file.
///
/// A unit rather than something the installer does, because the
/// installer cannot. It writes the swapfile from whatever host is
/// running it, and setting an SELinux label means writing a
/// `security.selinux` xattr, which a host with SELinux disabled cannot
/// do at all: CI installs from an Ubuntu runner. The label therefore has
/// to be applied by the machine, where the policy lives.
///
/// `restorecon` rather than a tmpfiles `z` line, which was tried first
/// and cannot do it: tmpfiles runs in a domain with no `relabelto` for
/// `swapfile_t` and fails with `Unable to fix SELinux security context
/// of /var/swap/swapfile: Operation not permitted`.
///
/// **Recursive, from the mount point, and that is the whole of a bug this
/// shipped with.** The first version relabelled the file alone. That
/// worked, and the machine still could not hibernate, because the
/// subvolume's own root inode carries no label at all when the installer
/// makes it, and `systemd-sleep` has to *search* the directory before it
/// can read anything inside:
///
/// ```text
/// avc: denied { search } for comm="systemd-sleep" name="swap"
///      scontext=systemd_sleep_t tcontext=unlabeled_t tclass=dir
/// ```
///
/// So there are two labels to fix, not one: `var_t` on the directory,
/// which is the policy's default and only needs applying, and
/// `swapfile_t` on the file, which needs the rule above first.
///
/// Conditioned on both the file and SELinux, so it is silently skipped
/// on the machines that have no swapfile, which is most of them. The
/// condition names the file rather than the directory on purpose: an
/// empty `/var/swap` is not a machine that wanted to hibernate.
pub(crate) const SWAP_LABEL_SERVICE: &str = r#"[Unit]
Description=Give the hibernate swapfile the SELinux label systemd-sleep needs
Documentation=man:restorecon(8)
ConditionSecurity=selinux
ConditionPathExists=/var/swap/swapfile
After=var-swap.mount

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/restorecon -RF /var/swap

[Install]
WantedBy=multi-user.target
"#;

pub(crate) const FSTAB_SYNC_SERVICE: &str = r#"[Unit]
Description=Converge Anaconda's fstab root line for a composefs root
ConditionPathExists=/run/ostree-booted

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-fstab-sync

[Install]
WantedBy=multi-user.target
"#;

/// The boot menu's titles, converged at the only two moments they can be
/// made right.
///
/// `Before=ostree-finalize-staged.service` is the whole design, and it
/// reads backwards. systemd stops units in the reverse of the order it
/// starts them, and the rotation this exists to follow happens in
/// ostree's own ExecStop, at shutdown: the deployment symlinks move onto
/// the new order and the entry files, whose titles ostree does not
/// compare, are left as they were. Ordering *before* finalize-staged at
/// start is therefore what puts this *after* it at stop, with the
/// deployments moved and the titles not yet. ostree wires its own hold
/// unit into this slot the same way; `After=` on that hold unit is what
/// keeps this pass inside the window where /boot is still mounted.
///
/// Doing it when kuma stages an image instead would write titles for an
/// arrangement that has not happened yet: `kuma switch` only stages, and
/// a staged deployment becomes the default at shutdown, or never does.
///
/// ExecStart is the same idempotent pass at boot. It costs a few file
/// reads and it covers the machine that lost power before its shutdown
/// finished, the first boot on an image that predates this unit, and
/// `bootc rollback` rotating the deployments mid-session.
///
/// DefaultDependencies=no, with Conflicts= and Before= on final.target
/// spelled out, because this has to stop in the late slot
/// finalize-staged stops in. The ordinary slot carries an implicit
/// Before=shutdown.target, which contradicts stopping after a unit that
/// stops at final.target, and systemd resolves a contradiction like that
/// by dropping one of the two rules rather than by saying anything.
pub(crate) const BOOT_TITLES_SERVICE: &str = r#"[Unit]
Description=Name the boot menu's entries after the deployments they boot
ConditionPathExists=/run/ostree-booted
DefaultDependencies=no
RequiresMountsFor=/boot /sysroot
After=local-fs.target
After=ostree-finalize-staged-hold.service
Before=ostree-finalize-staged.service
Before=basic.target final.target
Conflicts=final.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/bin/kuma boot-titles
ExecStop=/usr/bin/kuma boot-titles
TimeoutStartSec=30s
TimeoutStopSec=2m

[Install]
WantedBy=basic.target
"#;

/// Before the health check only for tidiness — the hook matters at the
/// NEXT grub run, so any point in this boot converges in time.
pub(crate) const BOOT_HEALTH_SYNC_SERVICE: &str = r#"[Unit]
Description=Converge the bootloader's boot-counter fallback hook
# The same gate its sibling kuma-fstab-sync carries, and the one
# liveiso.rs already credited this unit with having. It did not: it
# skipped on live media only because the grub config it looks for is not
# there, which is an accident rather than a decision, and an accident
# that a future change to the script could remove without anyone noticing.
ConditionPathExists=/run/ostree-booted
RequiresMountsFor=/boot
Before=greenboot-healthcheck.service

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-boot-health-sync

[Install]
WantedBy=multi-user.target
"#;

/// greenboot required check for desktop images. Runs from greenboot's
/// health-check oneshot, which blocks multi-user.target completion — so
/// it must poll display-manager.service directly and must NEVER wait on
/// graphical.target: graphical waits for multi-user, which waits for
/// this very check. That deadlock is one comment away, hence the shout.
pub(crate) const GREETER_CHECK: &str = r#"#!/usr/bin/bash
# A desktop boot is healthy when the greeter is on screen, not when
# multi-user is reached: a broken compositor or greeter update boots
# "fine" into a black screen, and that is exactly the regression this
# check exists to roll back. display-manager.service is the alias both
# greetd (niri) and cosmic-greeter carry; it starts in parallel with
# this check and its Restart= may be mid-retry, so only the deadline
# decides. DO NOT wait on graphical.target here: it waits for
# multi-user.target, which waits for this check: instant deadlock.
#
# Active once is not the same as running. greetd's Restart= makes a
# crash loop look healthy to a single sample: a machine whose greeter
# died five times in four seconds passed this check green, because the
# poll happened to land inside one of the retries. So the greeter has to
# be up, and still up a moment later, and a unit that has given up
# entirely is a failure now rather than in two minutes.
set -u
deadline=$(( SECONDS + 120 ))
settle=5
while true; do
    if systemctl --quiet is-failed display-manager.service; then
        echo "display-manager.service failed to start"
        exit 1
    fi
    if systemctl --quiet is-active display-manager.service; then
        sleep "$settle"
        if systemctl --quiet is-active display-manager.service; then
            exit 0
        fi
        echo "display-manager.service did not stay up; still waiting"
    fi
    if (( SECONDS >= deadline )); then
        echo "display-manager.service not running after 120s"
        exit 1
    fi
    sleep 3
done
"#;

/// Appended to niri's full default config (copied from the package) so the
/// stock keybindings survive; niri configs replace defaults entirely.
pub(crate) const NIRI_EXTRAS: &str = r##"

// adw-gtk3-dark is a real directory theme, so the plain name works
// here. (Stock Adwaita needed GTK_THEME=Adwaita:dark, because the
// settings-layer name "Adwaita-dark" loads as a nonexistent directory
// theme and falls back to light.) This variable outranks gsettings, so
// it has to move with the other three or GTK3 apps keep the old theme
// whatever dconf says.
environment {
    GTK_THEME "adw-gtk3-dark"
    XCURSOR_THEME "Adwaita"
    XCURSOR_SIZE "24"
}

// Kuma session services
spawn-at-startup "/usr/libexec/polkit-mate-authentication-agent-1"
spawn-at-startup "/usr/libexec/kuma-clipboard-bridge"
spawn-at-startup "/usr/libexec/kuma-xsettings"
spawn-at-startup "blueman-applet"
// Automount removable media at the session level.
spawn-at-startup "udiskie"
// The shell is NOT spawned here. It runs as kuma-shell.service, a
// supervised user unit, because a `spawn-at-startup` lands in a
// transient scope and a scope cannot carry Restart=. Every lock this
// machine has goes through that one process: idle, the keybind, and
// lock-before-suspend. When it died, nothing restarted it and nothing
// said so, and the next lid close suspended the machine with an
// unlocked session inside it.
spawn-at-startup "/usr/libexec/kuma-battery-watch"

// Kuma look: rounded windows, quiet neutral focus ring. Window rules are
// additive, so this themes every window without touching the stock layout.
window-rule {
    geometry-corner-radius 8
    clip-to-geometry true
    focus-ring {
        active-color "#b8bec8"
        inactive-color "#333940"
    }
}

// The machine's own deltas, and the LAST thing in this file on purpose:
// niri merges includes positionally, so whatever is here wins over
// everything above it. Without this a user who wants one setting changed
// must copy the whole config into ~/.config/niri/config.kdl, which then
// shadows /etc forever and goes stale the moment an image update rewrites
// the config it was copied from. A few lines in local.kdl cost nothing and
// never expire. optional=true is what lets the build's `niri validate` (and
// every machine that never writes one) pass with the file absent; niri logs
// a reload warning in that case and carries on. Both optional includes and
// ~ expansion landed in niri 26.04, so this needs that or newer.
include optional=true "~/.config/niri/local.kdl"
"##;

/// The shell, supervised.
///
/// niri's `spawn-at-startup` hands the process to systemd as a transient
/// SCOPE, and a scope cannot carry `Restart=`: measured on a booted
/// machine, where the shell sat in `app-niri-noctalia-1441.scope` with
/// nothing watching it. That matters more here than for an ordinary
/// panel, because all three of this desktop's lock paths run through
/// that one process. A crash took the bar with it, which you notice, and
/// the idle lock and the sleep inhibitor, which you do not.
///
/// `PartOf=` and `WantedBy=graphical-session.target` because that target
/// is real in this session (measured active) and niri imports
/// WAYLAND_DISPLAY and NIRI_SOCKET into the user manager's environment
/// before it, so the unit starts with what it needs.
///
/// `Restart=always` rather than `on-failure`: a shell that exits zero has
/// still taken the lock screen with it.
///
/// `Environment=` because a unit inherits nothing from niri's
/// `environment` block. 0.17 learned that the hard way with a config
/// pointer that existed only where the shell no longer read it; the
/// kuma-shell needs less — its defaults are compiled in, the image's
/// policy ships with the binary — and the cursor theme is the one pair
/// that still has to be said here, and in niri's block for everything
/// niri spawns, and nowhere else.
pub(crate) const SHELL_SERVICE: &str = r#"[Unit]
Description=kuma-shell, the kuma desktop shell
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
# The shell draws its own surfaces, so the cursor over the bar and the
# lock screen is themed by these or by nothing. niri's `environment`
# block states the same pair for everything niri spawns; a unit inherits
# nothing from there, so both places carry both values and a test holds
# them together.
Environment=XCURSOR_THEME=Adwaita
Environment=XCURSOR_SIZE=24
ExecStart=/usr/bin/kuma-shell
Restart=always
RestartSec=1
Slice=session.slice

[Install]
WantedBy=graphical-session.target
"#;

/// The lock screen's PAM service, and the reason it is ours to ship.
///
/// kuma-shell authenticates through a chain — kuma-lock, swaylock,
/// vlock — where the first *installed* service wins (the shell's
/// docs/adr/0008): Linux-PAM answers a missing service from the `other`
/// policy, pam_deny here, which is exactly the answer a wrong password
/// gets — the two are indistinguishable from the caller's side, so the
/// shell can only tell them apart by checking /etc/pam.d/<service> and
/// skipping what is not there. On a stock image that check lands on
/// vlock's file, shipped by kbd: a stack shaped like this one by
/// coincidence, named like nothing the session runs. This file makes
/// the chain's first entry real — unlock attempts are named
/// `kuma-lock` in the journal, and the shell picks the stack it was
/// written for.
///
/// The stack is vlock's, on purpose: the locker shape, verified
/// working as a uid-1000 caller. Auth rides system-auth (authselect's
/// symlink; pam_unix verifies through the setuid-root unix_chkpwd
/// helper) and account auto-permits. Deliberately absent: password and
/// session phases. The shell never opens a PAM session, and pam_unix
/// in the account phase under a uid-1000 caller journals a "setuid
/// failed: Operation not permitted" line on every unlock (measured
/// against the `login` service). swaylock is excluded from the image's
/// package set, so the chain skips it by the same existence check — no
/// file here pretends otherwise.
///
/// Landed with the shell switch: the lock screen's chain starts here,
/// so unlock attempts are named `kuma-lock` in the journal and vlock's
/// coincidental stack is skipped by the same existence check that made
/// it first.
pub(crate) const KUMA_LOCK_PAM: &str = r#"#%PAM-1.0
auth       include      system-auth
account    required     pam_permit.so
"#;

/// Do not sleep into an unlocked session.
///
/// The residual case after supervision: the shell is gone at the moment
/// the lid closes, so its logind sleep inhibitor is gone too and the
/// machine suspends with the desktop on screen. Supervision makes that
/// rare; this makes it loud instead of silent.
///
/// Terminating the session is the honest move rather than a harsh one:
/// with the shell dead that session has no bar, no lock screen and no
/// idle handling, so it is already unusable. What changes is that the
/// machine sleeps showing a greeter instead of sleeping showing your
/// work.
///
/// The terminate has a tail the guard also owns: the compositor and
/// the shell are user units, not members of the scope a terminate
/// kills, so niri.service outlived its session by 28 seconds when this
/// fired for real (2026-10-02) — and every password typed at the
/// greeter in that window was refused by niri-session's own
/// already-running check. The guard therefore stops
/// `graphical-session.target` after the session — the same teardown
/// niri-session performs — under a timeout, so a wedged stop cannot
/// stall the sleep this runs under.
///
/// Ordered `Before=sleep.target` and pulled in by it, so it runs on the
/// way down and on every path into sleep rather than only on the lid.
pub(crate) const SLEEP_GUARD_SERVICE: &str = r#"[Unit]
Description=Refuse to suspend a kuma desktop into an unlocked session
Before=sleep.target
StopWhenUnneeded=yes

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-sleep-guard

[Install]
WantedBy=sleep.target
"#;

pub(crate) const SLEEP_GUARD: &str = r#"#!/usr/bin/bash
set -euo pipefail

# Only on a machine kuma gave a shell to. A server has no session to
# protect and no shell to miss.
[ -f /etc/niri/config.kdl ] || exit 0

# A graphical session on the seat, and the account that owns it. Anything
# else (no session, a TTY login, a machine at the greeter) is nothing to
# do here.
while read -r id _rest; do
    # Only the session id is read positionally, and everything else is
    # ASKED. `loginctl list-sessions` has nine columns today and has
    # gained columns before; a guard that reads the wrong one skips
    # silently, and a security guard that skips silently is the exact
    # property this exists to prevent.
    seat=$(loginctl show-session "$id" -p Seat --value 2>/dev/null || true)
    [ "$seat" = "seat0" ] || continue
    type=$(loginctl show-session "$id" -p Type --value 2>/dev/null || true)
    [ "$type" = "wayland" ] || continue
    user=$(loginctl show-session "$id" -p Name --value 2>/dev/null || true)
    [ -n "$user" ] || continue
    # A process is not proof the shell can act, and 0.17's lesson was a
    # shell that hangs rather than exits. Under noctalia that case was
    # caught by ASKING the shell over its session bus — noctalia owned
    # org.freedesktop.ScreenSaver from its first moment, and Peer.Ping
    # was answered by sd-bus itself. kuma-shell owns no bus name (its
    # lock path is logind's Lock signal, not a dbus contract), so the
    # probe has nothing to ask and the guard keeps only the process
    # half: a shell that hangs now passes this check, and the guard
    # will not guess. The upside is structural — kuma-shell holds no
    # logind delay inhibitor either, so a hung shell cannot stall sleep
    # the way the hung noctalia could; the machine suspends on schedule
    # with lock-before-suspend best-effort, same as any locker.
    if ! pgrep -u "$user" -x kuma-shell >/dev/null 2>&1; then
        logger -t kuma-sleep-guard         "the desktop shell is not running in session $id; ending it rather than suspending an unlocked session"
        loginctl terminate-session "$id" || true
        # The terminate kills the session's scope, and the scope is not
        # where the desktop lives: niri and the shell are user units,
        # which outlive the scope by however long the user manager
        # takes to notice — 28 seconds, measured the first time this
        # fired for real. Every password typed at the greeter in that
        # window bought niri-session's "A niri session is already
        # running." — its is-active check saw the ghost, the session
        # command exited, and greetd reset to the greeter. So the guard
        # ends the desktop with the session: graphical-session.target
        # is the umbrella both units hang from (niri BindsTo it, the
        # shell PartOf it), the same teardown niri-session performs.
        # timeout bounds it — a wedged stop must not stall the sleep
        # this runs under — and || true takes the ordinary case, a
        # user manager already gone, without complaint.
        uid=$(id -u "$user")
        timeout 15 runuser -u "$user" -- env XDG_RUNTIME_DIR="/run/user/$uid" \
            systemctl --user --quiet stop graphical-session.target \
            2>/dev/null || true
    fi
done < <(loginctl list-sessions --no-legend 2>/dev/null || true)
"#;

/// Dark by default. Apps learn the preference from the settings portal,
/// which reads org.gnome.desktop.interface from dconf; without it every
/// CSD titlebar and GTK app falls back to light. color-scheme covers
/// GTK4/libadwaita/portal clients, gtk-theme covers GTK3 apps that
/// predate it. A system db sets the default; user settings still win.
pub(crate) const DCONF_PROFILE: &str = "user-db:user\nsystem-db:local\n";
pub(crate) const DCONF_DARK: &str = r#"[org/gnome/desktop/interface]
color-scheme='prefer-dark'
gtk-theme='adw-gtk3-dark'
"#;

/// One bluetooth indicator, not two.
///
/// blueman-applet earns its place as the Bluetooth agent: it answers
/// pairing requests and reconnects devices, and nothing else here does.
/// What it also does is spawn `blueman-tray`, which puts a second
/// bluetooth indicator in the bar's tray beside the bar's own bluetooth
/// widget. The bar then shows one state twice, in two styles, because a
/// tray renders the application's own coloured icon while every other
/// widget is a font glyph, and a tray cannot recolour what it is given.
/// The widget already opens blueman-manager on click, so the tray icon
/// was carrying no capability of its own.
///
/// Written against waybar and still true of the shell that replaced it,
/// which carries `tray` and `bluetooth` in the same group: verified on
/// the booted image, where blueman-applet runs, blueman-tray does not,
/// and one bluetooth glyph reaches the bar.
///
/// Disabling StatusIcon alone does nothing, which is the part worth
/// writing down: ShowConnected declares `__depends__ = ["StatusIcon"]`,
/// and the plugin manager loads a dependency whether or not it was
/// disabled, so the icon comes straight back. ShowConnected exists only
/// to decorate that icon ("adds an indication on the status icon...
/// shows the connections in the tooltip"), so it goes with it and takes
/// nothing else. Both names verified against a running session, after
/// the one-name version was tried and silently did nothing.
pub(crate) const DCONF_BLUEMAN: &str = r#"[org/blueman/general]
plugin-list=['!StatusIcon', '!ShowConnected']
"#;

/// One launch path per session service, and it is kuma's.
///
/// `xdg-desktop-autostart.target` is active in this session, so systemd's
/// xdg-autostart-generator turns every `/etc/xdg/autostart/*.desktop`
/// into a unit. Fedora ships one for blueman and one for the mate polkit
/// agent, and niri-extras.kdl *also* spawns both. They are single
/// instance, so one launch wins and the other quietly loses, and which
/// one wins is a race: measured on one boot, blueman came up under
/// `app-blueman@autostart.service` while the polkit agent came up under
/// niri's own scope. Nothing breaks, but a unit reads `dead` while its
/// program is running, the loser can log noise, and which cgroup owns a
/// process changes from boot to boot.
///
/// kuma's spawn is the one kept, because it is the one this image
/// declares: the alternative depends on the session reaching
/// `xdg-desktop-autostart.target`, and losing the polkit agent that way
/// is invisible until somebody needs an authentication prompt.
///
/// `Hidden=true` rather than masking the generated unit: the generator
/// names the polkit unit
/// `app-polkit\x2dmate\x2dauthentication\x2dagent\x2d1@autostart.service`,
/// and reproducing that escaping in a symlink is a worse thing to depend
/// on than the spec's own "ignore this entry" key. Verified against the
/// running generator, which reports the unit `not-found` with this in
/// place.
pub(crate) fn autostart_off(name: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName={name}\nHidden=true\n")
}

/// Runs a kuma verb in a terminal window, for the desktop entries in
/// [`crate::seam`].
///
/// **`Terminal=true` is the obvious way and does not work here.** It
/// hands the job to whatever the launcher believes a terminal is, which
/// differs per launcher, is configured separately in several, and is
/// nothing at all in some. kuma knows which terminal it put in the
/// image, so it opens that one, and the entries stay identical on niri
/// and COSMIC because the difference lives here instead of in them.
///
/// The window is held open after the verb exits. A terminal launched as
/// `kitty -e <command>` closes the instant the command returns; every
/// one of these prints something worth reading, and the ones that ask
/// for a password are exactly the ones whose window would otherwise
/// vanish as the password is finished.
pub(crate) const KUMA_LAUNCH: &str = r#"#!/usr/bin/bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
    printf 'usage: kuma-launch <verb> [args...]\n' >&2
    exit 2
fi

# Not a fallback chain into xterm: an image either has the terminal its
# desktop set installed or it has no graphical session to launch from.
terminal=
for candidate in kitty cosmic-term; do
    if command -v "$candidate" >/dev/null 2>&1; then
        terminal=$candidate
        break
    fi
done
if [ -z "$terminal" ]; then
    printf 'kuma-launch: this image ships no terminal to run `kuma %s` in\n' "$1" >&2
    exit 1
fi

# The verb and its arguments are passed to the held script as arguments
# rather than pasted into it. Assembling a command line into a string is
# how an argument with a space in it becomes two arguments, and nothing
# here has to be a string.
# A LOGIN shell. Without `-l` nothing sources /etc/profile.d, so a
# desktop entry runs with a smaller PATH than the same person's terminal
# — on a machine with brew packages, `kuma edit` fell back to vi while
# the nano they installed sat unfound. A launcher that opens a terminal
# should hand over the environment that terminal would have had.
exec "$terminal" -e /usr/bin/bash -lc '
"$@"
status=$?
printf "\n"
[ "$status" -eq 0 ] || printf "exited %s\n" "$status"
printf "[kuma] press enter to close "
read -r _
' kuma-launch kuma "$@"
"#;

/// The media keys. The shell owns volume and brightness in its own msg
/// interface — `kuma-shell msg volume-up` adjusts through the same
/// sysmon the bar's widgets read from, so key and widget cannot
/// disagree — so the binds go through it directly and the kuma-osd
/// helper that used to sit here is gone.
///
/// The helper was a script that adjusted with `wpctl` and
/// `brightnessctl` and drew nothing: a comment claimed the shell watched
/// the changes and drew its own OSD from a `[osd.kinds]` config key, and
/// no noctalia has ever had either. Nothing called the OSD, so for the
/// whole life of that script the keys adjusted silently — the change a
/// person can see was missing, which is the only part of a volume key
/// there is.
///
/// Titles are not decoration here. niri shows EVERY bind on the
/// Important Hotkeys overlay and generates the label from the action, so
/// an untitled `spawn` advertises itself as its own command line: the
/// clipboard bind read as its whole `sh -c` pipeline
/// on the first screen of a new machine. The five worth naming are
/// named, and the media keys are hidden outright — they are printed on
/// the keyboard, and ten of them crowd out everything worth reading.
/// The binds are spliced INTO the stock `binds {}` section during the
/// merge (niri rejects a second binds node) while the stock
/// wpctl/brightnessctl lines are sed-stripped.
///
/// Two binds left with noctalia and did not come back: `Mod+Ctrl+V`
/// (clipboard history) and `Mod+Ctrl+W` (wallpaper) opened panels the
/// kuma-shell does not have. A bind that advertises a dead panel is the
/// bug `no_bind_names_a_program_the_image_excludes` exists for, so they
/// are dropped outright — kuma-clipboard-bridge stays for the apps that
/// use it, the image ships one static wallpaper, and a shell panel for
/// either is new work, not part of the switch.
pub(crate) const NIRI_MEDIA_BINDS: &str = r#"    XF86AudioRaiseVolume allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "volume-up"; }

    XF86AudioLowerVolume allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "volume-down"; }

    XF86AudioMute allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "volume-mute"; }

    XF86AudioMicMute allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "mic-mute"; }

    XF86MonBrightnessUp allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "brightness-up"; }

    XF86MonBrightnessDown allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "brightness-down"; }

    XF86AudioPlay allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "media" "play-pause"; }

    XF86AudioStop allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "media" "stop"; }

    XF86AudioNext allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "media" "next"; }

    XF86AudioPrev allow-when-locked=true hotkey-overlay-title=null { spawn "kuma-shell" "msg" "media" "previous"; }

    Mod+Alt+R hotkey-overlay-title="Record the Screen" { spawn "/usr/libexec/kuma-record"; }

    Mod+Print hotkey-overlay-title="Screenshot a Region, then Annotate" { spawn "sh" "-c" "grim -g \"$(slurp)\" - | swappy -f -"; }

    Mod+S hotkey-overlay-title="Toggle Kuma Settings menu" { spawn "kuma-shell" "msg" "settings"; }
"#;

/// System-wide default apps: without associations, opening a PDF or a
/// link from Thunar is app-picker roulette. Flatpak-exported desktop
/// ids for the declared apps, native ids for the in-image tools.
///
/// A `.desktop` file's MimeType= line says an app *can* open a type;
/// this list says which one *wins*. So the entries worth having are the
/// contested types — a type with one claimant resolves to it unaided.
/// Firefox is why most of this list exists: it claims application/pdf,
/// six image types, and four audio/video types, every one of which it
/// would otherwise be free to take from Papers, Loupe, or Celluloid.
/// The in-image contest is inode/directory, which kitty-open.desktop
/// claims alongside thunar.
///
/// text/plain has no entry on purpose: nothing in the image claims it,
/// so a declared editor wins unopposed, and an entry would only pin an
/// app that a declaration is free not to install.
pub(crate) const MIMEAPPS: &str = r#"[Default Applications]
x-scheme-handler/http=org.mozilla.firefox.desktop
x-scheme-handler/https=org.mozilla.firefox.desktop
text/html=org.mozilla.firefox.desktop
application/pdf=org.gnome.Papers.desktop
inode/directory=thunar.desktop
image/png=org.gnome.Loupe.desktop
image/jpeg=org.gnome.Loupe.desktop
image/webp=org.gnome.Loupe.desktop
image/gif=org.gnome.Loupe.desktop
image/avif=org.gnome.Loupe.desktop
image/svg+xml=org.gnome.Loupe.desktop
image/tiff=org.gnome.Loupe.desktop
video/mp4=io.github.celluloid_player.Celluloid.desktop
video/webm=io.github.celluloid_player.Celluloid.desktop
video/ogg=io.github.celluloid_player.Celluloid.desktop
video/x-matroska=io.github.celluloid_player.Celluloid.desktop
audio/mpeg=io.github.celluloid_player.Celluloid.desktop
audio/flac=io.github.celluloid_player.Celluloid.desktop
audio/ogg=io.github.celluloid_player.Celluloid.desktop
audio/webm=io.github.celluloid_player.Celluloid.desktop
application/zip=org.gnome.FileRoller.desktop
"#;

/// Battery warnings, sent to whatever owns
/// `org.freedesktop.Notifications`, which on this desktop is the shell.
/// Polls sysfs: upower-notifier tools (poweralertd) aren't in Fedora's
/// repos. No battery (desktops, VMs) means the loop just idles cheaply.
///
/// The shell's battery widget shows the state in the bar but warns at
/// no threshold (its own settings carry no battery section), so this
/// script remains the only low-and-critical notice a laptop gets. If
/// the shell grows threshold notifications, this is the first thing to
/// go — one state announced by two programs in two styles is the shape
/// `DCONF_BLUEMAN` above exists to undo. Removing this needs a battery
/// to prove, because nothing in a VM ever discharges.
///
/// One watcher per account, by flock. niri re-runs `spawn-at-startup`
/// when its config reloads, and a session's end never reaches this
/// loop, so an unguarded start adds another warning source on every
/// reload and re-login — four was measured on one machine. The lock
/// file lives in the user's runtime directory: it dies with the user
/// manager, and oldest-wins is the right way to be wrong, because the
/// survivor is a working watcher writing to the same user bus either
/// way.
pub(crate) const BATTERY_WATCH: &str = r#"#!/usr/bin/bash
set -u
exec 9>"${XDG_RUNTIME_DIR:?no session runtime directory}/kuma-battery-watch.lock"
flock -n 9 || exit 0
warned=""
while sleep 60; do
    bat=$(ls /sys/class/power_supply 2>/dev/null | grep -m1 "^BAT" || true)
    [ -n "$bat" ] || continue
    cap=$(cat "/sys/class/power_supply/$bat/capacity" 2>/dev/null || echo 100)
    status=$(cat "/sys/class/power_supply/$bat/status" 2>/dev/null || echo Unknown)
    if [ "$status" != "Discharging" ]; then warned=""; continue; fi
    if [ "$cap" -le 5 ] && [ "$warned" != "critical" ]; then
        warned="critical"
        notify-send -u critical "Battery critical: ${cap}%" "Plug in now."
    elif [ "$cap" -le 15 ] && [ -z "$warned" ]; then
        warned="low"
        notify-send "Battery low: ${cap}%"
    fi
done
"#;

/// What `Mod+D` becomes.
///
/// Stock niri binds it to plain fuzzel, and the image no longer ships
/// fuzzel: the shell brought a launcher and fuzzel left with the menu
/// that was the only other thing using it. The key the hand already
/// goes to should open the thing that lists applications.
///
/// Substituted into the stock config rather than added beside it: niri
/// takes the last bind for a key, so a second `Mod+D` would leave the
/// original in the file, working or not depending on merge order.
pub(crate) const NIRI_MENU_BIND: &str = r#"Mod+D hotkey-overlay-title="Toggle Applications menu" { spawn "kuma-shell" "msg" "launcher-toggle"; }"#;

/// The stock line it replaces. Grepped for before the rewrite, so a niri
/// release that renames it fails the build instead of shipping media
/// whose main key does nothing.
///
/// Rewritten rather than left alone even though kuma once put its own
/// menu here: the stock line spawns a program this image does not have,
/// so leaving it is a dead key on the most-used bind there is.
pub(crate) const NIRI_STOCK_LAUNCHER: &str =
    r#"Mod+D hotkey-overlay-title="Run an Application: fuzzel" { spawn "fuzzel"; }"#;

/// niri's stock screen-reader toggle. kuma has never shipped orca, so
/// this has been a key that does nothing since the first niri image —
/// hidden from the overlay by its own `=null`, which is why it survived
/// this long. A dead accessibility key is worse than an absent one: it
/// tells a screen-reader user the machine has a screen reader.
pub(crate) const NIRI_STOCK_ORCA: &str = r#"Super+Alt+S allow-when-locked=true hotkey-overlay-title=null { spawn-sh "pkill orca || exec orca"; }"#;

/// niri's stock lock bind, and what kuma puts in its place.
///
/// This one is advertised on the Important Hotkeys overlay that opens on
/// every first login, so a dead key here is the first thing a new
/// machine shows a person. It was live until the shell replaced
/// swaylock, and swaylock is now excluded from the image outright, which
/// is exactly the shape of change that leaves a bind pointing at nothing.
///
/// The rewrite targets logind, not the shell: the lock screen listens
/// for the session's `Lock` signal, which is the same road the idle
/// timeout and PrepareForSleep take, and a locker that answers to one
/// trigger answers to all of them.
pub(crate) const NIRI_STOCK_LOCK: &str =
    r#"Super+Alt+L hotkey-overlay-title="Lock the Screen: swaylock" { spawn "swaylock"; }"#;
pub(crate) const NIRI_LOCK_BIND: &str =
    r#"Super+Alt+L hotkey-overlay-title="Lock the Screen" { spawn "loginctl" "lock-session"; }"#;

/// GTK theme settings travel two roads: Wayland-native apps read
/// gsettings (the dconf defaults cover those), but X11/XWayland GTK apps
/// only listen to an XSettings daemon — without one they render stock
/// light Adwaita. xsettingsd broadcasts the same dark values there.
pub(crate) const XSETTINGSD_CONF: &str = r#"Net/ThemeName "adw-gtk3-dark"
Net/IconThemeName "Adwaita"
Gtk/ApplicationPreferDarkTheme 1
Gtk/CursorThemeName "Adwaita"
Xft/DPI 98304
"#;

/// GTK's base-layer config, read by every GTK app on every backend when
/// higher-priority sources (XSettings, gsettings) don't reach it. The
/// value is 96 dpi << 10 above: without a broadcast DPI, X11 clients
/// compute one from XWayland's bogus virtual-monitor physical size and
/// render enormous.
pub(crate) const GTK3_SETTINGS_INI: &str = r#"[Settings]
gtk-theme-name = adw-gtk3-dark
gtk-application-prefer-dark-theme = true
gtk-icon-theme-name = Adwaita
"#;

pub(crate) const GTK4_SETTINGS_INI: &str = r#"[Settings]
gtk-application-prefer-dark-theme = true
"#;

/// The two X11 helpers below both start before XWayland does.
///
/// `xwayland-satellite` publishes `DISPLAY` into the session environment
/// when it comes up, which is after `spawn-at-startup` has already run
/// both of these. Waiting is the whole difference between a helper that
/// works and one that exits instantly on every login, so it is written
/// once: two copies of a 30-second timeout is two chances to fix half a
/// bug. Exit 0 rather than fail after the wait, because a session with
/// no XWayland at all is a session with nothing for either to do.
pub(crate) const WAIT_FOR_DISPLAY: &str = r#"for _ in $(seq 60); do
    [ -n "${DISPLAY:-}" ] && break
    DISPLAY=$(systemctl --user show-environment 2>/dev/null | sed -n 's/^DISPLAY=//p')
    [ -n "$DISPLAY" ] && export DISPLAY && break
    sleep 0.5
done
[ -n "${DISPLAY:-}" ] || exit 0
"#;

pub(crate) fn xsettings_launcher() -> String {
    format!(
        "#!/usr/bin/bash\nset -euo pipefail\n{WAIT_FOR_DISPLAY}\
         exec xsettingsd -c /usr/lib/kuma/xsettingsd.conf\n"
    )
}

/// Session half of host<->guest clipboard in `kuma vm`. spice-vdagent's
/// clipboard side is X11, so under niri it rides the xwayland-satellite
/// bridge — wait briefly for DISPLAY to appear in the session
/// environment. No vdagent port (real hardware) means exit quietly.
pub(crate) fn clipboard_bridge() -> String {
    format!(
        "#!/usr/bin/bash\nset -euo pipefail\n\
         [ -e /dev/virtio-ports/com.redhat.spice.0 ] || exit 0\n\
         {WAIT_FOR_DISPLAY}\
         exec spice-vdagent -x\n"
    )
}

/// Joan G. Stark's classic ASCII bear (her "jgs" signature moved here so
/// it doesn't render on every run), wallpaper-bear warm brown ($1) with
/// the wordmark in kuma green ($2). Identity you invoke: fastfetch is
/// baked but nothing runs it at shell startup.
pub(crate) const FASTFETCH_LOGO: &str = r#"$1 .--.              .--.
$1: (\ ". _......_ ." /) :
$1 '.    `        `    .'
$1  /'   _        _   `\
$1 /     0}      {0     \
$1|       /      \       |
$1|     /'        `\     |
$1 \   | .  .==.  . |   /
$1  '._ \.' \__/ './ _.'
$1  /  ``'._-''-_.'``  \
$1          `--`
$2     k   u   m   a  O  S
"#;

/// System-wide default via XDG_CONFIG_DIRS; a user config in
/// ~/.config/fastfetch still wins, same as every other config here.
pub(crate) const FASTFETCH_CONFIG: &str = r#"{
    "logo": {
        "type": "file",
        "source": "/usr/lib/kuma/fastfetch-logo.txt",
        "color": { "1": "38;2;226;190;146", "2": "38;2;126;224;168" },
        "padding": { "top": 1, "right": 3 }
    },
    "modules": [
        "title",
        "separator",
        "os",
        "kernel",
        "uptime",
        "packages",
        "shell",
        "wm",
        "terminal",
        "cpu",
        "gpu",
        "memory",
        "disk",
        "break",
        "colors"
    ]
}
"#;

/// Theme files for the curated desktop. The kitty palette is the only
/// copy: chosen once and shipped static, after the wallpaper-derived
/// render died with noctalia. All system-wide (never /etc/skel): skel
/// only reaches homes created after the image ships, so it strands
/// existing users on stale copies — image updates must retheme every
/// account. User dotfiles still win everywhere:
/// kitty merges /etc/xdg beneath the user's file (so a one-key override
/// keeps the rest of this theme).
pub(crate) const WALLPAPER: &[u8] = include_bytes!("../../assets/kuma-wallpaper.jpg");
pub(crate) const KITTY_CONFIG: &str = include_str!("../../assets/kitty.conf");

/// The vendored plymouth theme (see assets/CREDITS.md), embedded by
/// build.rs as `(filename, bytes)` pairs. Staged into the build context
/// verbatim, installed to /usr/share/plymouth/themes/spinner_alt/, and set
/// as plymouth's default so it draws early boot and the LUKS prompt.
pub(crate) mod plymouth_theme {
    include!(concat!(env!("OUT_DIR"), "/plymouth_theme.rs"));
}

/// The install root every theme file COPY lands under.
pub(crate) const PLYMOUTH_THEME_DIR: &str = "spinner_alt";

/// Rebrand the OS identity: kumaOS, not Fedora. ID_LIKE=fedora keeps tools
/// that sniff os-release (toolbox, distrobox, dnf COPR, …) working. Runs
/// last so every dnf layer before it still sees stock Fedora metadata.
///
/// Two independent axes, and keeping them apart is the point:
///
/// - **The number is kuma's**, taken from the binary that built the image,
///   so PRETTY_NAME says which kuma made this machine. The number alone,
///   deliberately: `io.kuma.builder` already carries the full
///   `<version> (<sha> <date>)` stamp for the "is this the binary with my
///   change in it" question (see build.rs), and a GRUB menu entry is the
///   wrong place to answer it. Between releases every build from main
///   therefore shows the same number, which is the same approximation
///   `--version` makes and is why the sha exists beside it.
/// - **The bear names the Fedora base**, keyed by VERSION_ID below. Two
///   machines reading (Callisto) share a base whatever their numbers say,
///   and a bear you did not expect means the base moved under you.
///
/// **VERSION_ID stays Fedora's** and must: it is machine-readable, and
/// toolbox, distrobox and COPR resolve against it. Only the display
/// fields carry kuma's version. `/usr/lib/fedora-release` likewise keeps
/// Fedora's number, because it exists to be parsed as one.
///
/// Bear names go alphabetically where a letter has a name worth using;
/// the pool is bears real, extinct, mythical and fictional. D and F are
/// skipped deliberately (Deninger and Fozzie were the only candidates and
/// neither earns a place). Assigned through K Kodiak, Fedora 51.
///
/// An unlisted base falls back to no bear, keeping "kumaOS <version>" so a
/// machine still says what built it.
pub(crate) const BRANDING: &str = r#"
RUN . /usr/lib/os-release \
    && case "${VERSION_ID}" in \
        44) CODENAME="Beorn" ;; \
        45) CODENAME="Callisto" ;; \
        46) CODENAME="Ephraim" ;; \
        47) CODENAME="Grizzly" ;; \
        48) CODENAME="Helarctos" ;; \
        49) CODENAME="Iorek" ;; \
        50) CODENAME="Jambavan" ;; \
        51) CODENAME="Kodiak" ;; \
        *) CODENAME="" ;; \
    esac \
    && sed -i \
        -e 's|^NAME=.*|NAME="kumaOS"|' \
        -e "s|^PRETTY_NAME=.*|PRETTY_NAME=\"kumaOS @KUMAVERSION@${CODENAME:+ ($CODENAME)}\"|" \
        -e "s|^VERSION=.*|VERSION=\"@KUMAVERSION@${CODENAME:+ ($CODENAME)}\"|" \
        -e 's|^ID=.*|ID=kuma|' \
        -e 's|^DEFAULT_HOSTNAME=.*|DEFAULT_HOSTNAME="{default_hostname}"|' \
        -e 's|^ANSI_COLOR=.*|ANSI_COLOR="0;38;2;126;224;168"|' \
        /usr/lib/os-release \
    && if [ -n "$CODENAME" ]; then sed -i \
        -e "s|^VERSION_CODENAME=.*|VERSION_CODENAME=$(printf %s "$CODENAME" | tr '[:upper:]' '[:lower:]')|" \
        /usr/lib/os-release; fi \
    && { grep -q '^ID_LIKE=' /usr/lib/os-release || echo 'ID_LIKE="fedora"' >> /usr/lib/os-release; } \
    && { [ ! -f /usr/lib/fedora-release ] || echo "kumaOS release ${VERSION_ID}${CODENAME:+ ($CODENAME)}" > /usr/lib/fedora-release; }
"#;

/// BRANDING with the building binary's version substituted in. Not a
/// const because the version is only knowable at compile time, and not a
/// `format!` because the block is dense with `${...}` the shell needs and
/// `format!` would demand every brace be doubled.
pub(crate) fn branding() -> String {
    BRANDING
        .replace("@KUMAVERSION@", env!("CARGO_PKG_VERSION"))
        .replace("{default_hostname}", crate::install::DEFAULT_HOSTNAME)
}

/// Homebrew lives in /home/linuxbrew — machine-local mutable state, so it
/// can't be image content. First boot installs it; the tarball is the
/// official "untar anywhere" method. Prefix owned by uid 1000, brew's
/// single-user model (same choice Bluefin makes).
///
/// The in-script guard duplicates the unit's ConditionPathExists on
/// purpose: PID 1 evaluates conditions as init_t, and the script runs
/// unconfined, so the two can disagree (they did, for months — see the
/// service comment). Whichever check runs, an installed brew is left alone.
pub(crate) const BREW_SETUP_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
prefix=/home/linuxbrew/.linuxbrew

# This runs as root inside a tree the line at the bottom hands to uid
# 1000, so on the second boot every path here is one that uid can have
# replaced. Both guards (the -x below and the unit's own
# ConditionPathExists) live inside it too, which means that uid decides
# whether root runs this again.
#
# The concrete move is to delete the prefix and leave a symlink in its
# place: `mkdir -p` accepts an existing symlink-to-directory and `tar -C`
# chdirs through it, so root would extract Homebrew's tree at a path
# somebody else chose. That is a root-privileged write of fixed content
# rather than code execution, and on a machine whose account is in wheel
# (which every example declares) the attacker already has root, so no
# boundary is crossed there. It matters for a declaration that keeps its
# user out of wheel, and it is the one place kuma has root operating on
# paths a non-root uid owns.
#
# So: refuse rather than repair. A prefix that exists and is not root's
# is somebody's business, not this unit's, and saying so beats silently
# extracting into it.
#
# The guard above and the download below are deliberately not atomic,
# and that is safe rather than sloppy: between the check and the tar,
# every path this script writes through is root-owned (only root can
# create /home/linuxbrew under a root-owned /var/home), so a non-root
# uid has nothing to interleave with; once the tree has met chown -R it
# is uid-1000-owned, and the check refuses it. There is no window
# between the two states where root writes through a path anybody else
# chose.
for dir in /home/linuxbrew "$prefix" "$prefix/Homebrew" "$prefix/bin"; do
    [ -e "$dir" ] || continue
    if [ -L "$dir" ] || [ ! -d "$dir" ]; then
        echo "kuma: $dir is not a directory; refusing to set up Homebrew here" >&2
        exit 1
    fi
    if [ "$(stat -c %u "$dir")" != 0 ]; then
        echo "kuma: $dir is not root-owned; refusing to write into it as root" >&2
        exit 1
    fi
done

if [ -x "$prefix/bin/brew" ]; then exit 0; fi
mkdir -p "$prefix/Homebrew" "$prefix/bin"
curl -fsSL https://github.com/Homebrew/brew/tarball/HEAD \
    | tar -xz --strip-components=1 -C "$prefix/Homebrew"
ln -sf ../Homebrew/bin/brew "$prefix/bin/brew"
chown -R 1000:1000 /home/linuxbrew
"#;

pub(crate) const BREW_SETUP_SERVICE: &str = r#"[Unit]
Description=Install Homebrew
Wants=network-online.target
After=network-online.target
# The condition must not traverse a symlink: PID 1 checks it as init_t,
# which SELinux lets search home dirs and stat home files but not read
# home symlinks (lnk_file read), so any path through /home (itself a
# symlink to var/home) or ending at bin/brew (a symlink into Homebrew/)
# resolves as "missing" and the condition fails open — this service ran,
# and re-downloaded all of Homebrew, on every single boot. The real
# ruby entry point, named via /var/home, is symlink-free the whole way.
ConditionPathExists=!/var/home/linuxbrew/.linuxbrew/Homebrew/bin/brew

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-brew-setup
CPUWeight=25
IOWeight=25

[Install]
WantedBy=multi-user.target
"#;

/// Converge installed formulae to the declared list. Brew is
/// single-prefix, with no scope split that could stand in for
/// authority, so a state file remembers what the declaration installed
/// and only ever-declared formulae are removal candidates. Ad-hoc `brew
/// install` is untouched. The flatpak sync learned the same trick after
/// its scope proxy turned out to be one a store could break.
///
/// The upgrade is unscoped on purpose, and it is the one thing here
/// that reaches past the declaration. Ad-hoc formulae stay the owner's
/// to keep or remove; they were simply never getting updated, because
/// the upgrade used to name the declared list. Bare `brew upgrade` also
/// covers casks, which nothing else in kuma can even see. It runs in
/// `full` mode — the daily, gated run — along with `autoremove`, after
/// the state file is written so a failure cannot strand authority
/// tracking, and needs no `brew update` first because brew auto-updates
/// its taps before upgrading.
pub(crate) const BREW_SYNC_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
# The flatpak sync's marker check, word for word: the gate is the
# timer's, never the verb's. See CONVERGE_GATE_SCRIPT.
if [ -e /run/kuma-converge-gate/skip ]; then
    exit 0
fi
brew=/home/linuxbrew/.linuxbrew/bin/brew
[ -x "$brew" ] || exit 0
declared=/usr/lib/kuma/brews
state=/home/linuxbrew/.linuxbrew/.kuma-brews
[ -f "$state" ] || : > "$state"
# The convergence/currency split, the flatpak sync's word for word in
# spirit: converge for every caller, `full` — autoremove and the
# unscoped upgrade — for the daily, gated run. The state file is
# written only by a run that finished, so equal lists are a converged
# boot: no ruby, no vendor, no network.
if ! cmp -s "$declared" "$state"; then
    if [ -s "$declared" ]; then
        xargs -a "$declared" "$brew" install
    fi
    while read -r formula; do
        grep -qxF "$formula" "$declared" && continue
        "$brew" uninstall "$formula" || true
    done < "$state"
    cp "$declared" "$state"
fi
if [ "${1:-}" = full ]; then
    "$brew" autoremove
    "$brew" upgrade
fi
"#;

/// brew refuses to run as root; uid 1000 owns the prefix (brew's
/// single-user model). Numeric User= needs no passwd entry, so this
/// works on first boot even before kuma-user-sync creates the account.
/// HOME points inside the prefix so brew's cache lands somewhere the
/// uid can write regardless of which human account exists.
pub(crate) const BREW_SYNC_SERVICE: &str = r#"[Unit]
Description=Converge Homebrew formulae to the declared list
Wants=network-online.target
After=network-online.target kuma-brew-setup.service
StartLimitIntervalSec=1h
StartLimitBurst=6

[Service]
Type=oneshot
User=1000
Environment=HOME=/home/linuxbrew
ExecStart=/usr/libexec/kuma-brew-sync
Restart=on-failure
RestartSec=2min
CPUWeight=25
IOWeight=25

[Install]
WantedBy=multi-user.target
"#;

/// Same rationale as the flatpak timer: boot-only convergence goes
/// stale on machines that stay up. Same name-keeps-the-enablement
/// trick too, retargeting the timer at the gated daily unit.
pub(crate) const BREW_SYNC_TIMER: &str = r#"[Unit]
Description=Daily Homebrew convergence

[Timer]
OnCalendar=daily
Persistent=true
RandomizedDelaySec=1h
Unit=kuma-brew-sync-daily.service

[Install]
WantedBy=timers.target
"#;

/// The unit the daily brew timer fires. The gate runs as root (`+`
/// prefix: ExecStartPre runs as User= otherwise), because the skip
/// stamp it writes is machine state in /var/lib/kuma; the sync itself
/// stays uid 1000 — brew refuses root, and the prefix is owned by it.
pub(crate) const BREW_SYNC_DAILY_SERVICE: &str = r#"[Unit]
Description=Daily Homebrew convergence, skipped on battery or a metered connection
Wants=network-online.target
After=network-online.target kuma-brew-setup.service
StartLimitIntervalSec=1h
StartLimitBurst=6

[Service]
Type=oneshot
User=1000
Environment=HOME=/home/linuxbrew
RuntimeDirectory=kuma-converge-gate
ExecStartPre=+/usr/libexec/kuma-converge-gate
ExecStart=/usr/libexec/kuma-brew-sync full
Restart=on-failure
RestartSec=2min
CPUWeight=25
IOWeight=25
"#;

pub(crate) const BREW_PROFILE_SH: &str = r#"[ -x /home/linuxbrew/.linuxbrew/bin/brew ] \
    && eval "$(/home/linuxbrew/.linuxbrew/bin/brew shellenv)"
"#;

pub(crate) const BREW_PROFILE_FISH: &str = r#"if test -x /home/linuxbrew/.linuxbrew/bin/brew
    /home/linuxbrew/.linuxbrew/bin/brew shellenv | source
end
"#;

/// What a kuma unit does on installer media, where nothing persists
/// and nothing should converge.
///
/// Total on purpose: a block cannot ship units without answering this,
/// because the unaccounted ones were exactly the ones the old
/// line-prefix parser could not see — `kuma-shell.service` and
/// `kuma-sleep-guard.service` are enabled by a compound `--global`
/// line, so no test ever asked what they do on media.
///
/// The reasons are read only by the self-test that keeps them from
/// being empty; a build has no use for them, and that is the point —
/// the reason is data a test can demand rather than a comment nobody
/// re-reads.
#[allow(dead_code)]
pub(super) enum Live {
    /// The live layer masks it: it converges something, and a live
    /// session converges nothing.
    Masked(&'static str),
    /// Enabled, and correct on media. The reason is load-bearing: it is
    /// the difference between "decided this is safe" and "never asked".
    Runs(&'static str),
    /// Skips itself on media via a condition in the unit's own text,
    /// which the self-test verifies rather than trusts.
    Conditioned(&'static str),
}

/// The units this block enables, with what each does on installer
/// media. External units (greenboot's, greetd's) are declared here too
/// when this block is what enables them.
type Units = &'static [(&'static str, Live)];

const MASKED_CONVERGES: Live = Live::Masked("a live session converges nothing");

fn header(e: &mut Emitter<'_>) {
    e.raw("# Generated by kuma. Edit kuma.toml instead.\n");
    e.raw(&format!("FROM {}\n", e.config.base_ref()));
}

/// Desktop layer first: it is large and changes rarely, so keeping it
/// before the user's packages preserves the build cache across edits.
fn desktop_niri(e: &mut Emitter<'_>) {
    let config = e.config;
    if config.system.desktop != Desktop::Niri {
        return;
    }
    let greetd = e.stage("greetd-config.toml", greetd_config(config));
    let niri_extras = e.stage("niri-extras.kdl", NIRI_EXTRAS);
    // The nostr layer's authored pieces ride the texts they belong to,
    // only when the declaration says so: the panel bind beside the
    // media binds, and the scheme handler line in the associations. A
    // user's own settings still win over every one of them.
    let kitty = e.stage("kitty.conf", KITTY_CONFIG);
    let clipboard = e.stage("kuma-clipboard-bridge", clipboard_bridge());
    let xsettings = e.stage("kuma-xsettings", xsettings_launcher());
    let xsettingsd = e.stage("xsettingsd.conf", XSETTINGSD_CONF);
    let binds_text = if config.nostr.enable {
        format!("{NIRI_MEDIA_BINDS}\n{NIRI_NOSTR_BIND}\n")
    } else {
        NIRI_MEDIA_BINDS.to_string()
    };
    let binds = e.stage("niri-binds.kdl", binds_text);
    let mimeapps_text = if config.nostr.enable {
        format!("{MIMEAPPS}\nx-scheme-handler/nostrconnect=kuma-nostr-panel.desktop\n")
    } else {
        MIMEAPPS.to_string()
    };
    let mimeapps = e.stage("mimeapps.list", mimeapps_text);
    let record = e.stage("kuma-record", RECORD_SCRIPT);
    let battery = e.stage("kuma-battery-watch", BATTERY_WATCH);
    let shell = e.stage("kuma-shell.service", SHELL_SERVICE);
    let pam_lock = e.stage("kuma-lock", KUMA_LOCK_PAM);
    let guard_service = e.stage("kuma-sleep-guard.service", SLEEP_GUARD_SERVICE);
    let guard = e.stage("kuma-sleep-guard", SLEEP_GUARD);
    let gtk3 = e.stage("gtk3-settings.ini", GTK3_SETTINGS_INI);
    let gtk4 = e.stage("gtk4-settings.ini", GTK4_SETTINGS_INI);
    let dconf_profile = e.stage("dconf-profile", DCONF_PROFILE);
    let dconf_dark = e.stage("dconf-kuma-dark", DCONF_DARK);
    let dconf_blueman = e.stage("dconf-kuma-blueman", DCONF_BLUEMAN);
    let autostart_blueman = e.stage("autostart-blueman", autostart_off("Blueman Applet"));
    let autostart_polkit =
        e.stage("autostart-polkit-mate", autostart_off("PolicyKit Authentication Agent"));
    // Shared with the desktop-common staging in the greeter-seam
    // block, which this arm copies from: re-staged here for the handle,
    // same contents, and the walk's same-content assert holds the two
    // to each other.
    let kargs = e.stage("kargs-desktop.toml", DESKTOP_KARGS);
    let fastfetch = e.stage("fastfetch-config.jsonc", FASTFETCH_CONFIG);
    let fastfetch_logo = e.stage("fastfetch-logo.txt", FASTFETCH_LOGO);
    let wallpaper = e.stage("kuma-wallpaper.jpg", WALLPAPER);
    // The graphical greeter and everything it reads: the binary is
    // kumaUI's second binary from the same build as the shell, the
    // compositor config and the session wrapper are constants above,
    // the drop-in keeps a crashed greeter from stranding a boot, and
    // the tmpfiles entry creates its cache directory. See each const
    // for its why.
    let greeter_bin = e.supplied("kuma-greeter");
    let greeter_kdl = e.stage("greeter-niri.kdl", GREETER_NIRI_KDL);
    let greeter_session = e.stage("kuma-greeter-session", GREETER_SESSION);
    let greetd_restart = e.stage("greetd-restart.conf", GREETD_RESTART_DROPIN);
    let greeter_tmpfiles = e.stage("kuma-greeter-tmpfiles.conf", GREETER_TMPFILES);

    e.raw("\n");
    // niri's weak deps, which ride in past the package list unless
    // they are named here. alacritty because kuma's terminal is
    // kitty; waybar and swaylock because the shell replaced them and
    // dropping them from NIRI_PACKAGES is not enough to remove them.
    // Measured: an image built without these excludes still had a bar
    // and a lock screen it never starts.
    e.raw(&dnf_install(&format!(
        "{} {}",
        NIRI_EXCLUDES.iter().map(|p| format!("--exclude={p}")).collect::<Vec<_>>().join(" "),
        NIRI_PACKAGES.join(" ")
    )));
    e.raw(&mesa_freeworld());
    // A theme named in four places and present in none is a desktop
    // that silently falls back to light Adwaita, so prove the
    // package put the directory where the four names point.
    e.raw("RUN test -d /usr/share/themes/adw-gtk3-dark\n");
    e.copy(&greetd, "/etc/greetd/config.toml");
    e.copy(&greetd_restart, "/usr/lib/systemd/system/greetd.service.d/10-kuma-restart.conf");
    e.copy(&greeter_tmpfiles, "/usr/lib/tmpfiles.d/kuma-greeter.conf");
    e.copy(&greeter_kdl, "/usr/share/kumaos/greeter-niri.kdl");
    // The greeter compositor's config is validated at build time like
    // the session config below: a kdl that stopped parsing ships as a
    // boot nobody can log in from, so the failure is the build's.
    e.raw("RUN niri validate --config /usr/share/kumaos/greeter-niri.kdl\n");
    e.copy_exec(&greeter_session, "/usr/libexec/kuma-greeter-session");
    e.copy(&kargs, "/usr/lib/bootc/kargs.d/10-kuma-desktop.toml");
    e.copy(&niri_extras, "/usr/lib/kuma/niri-extras.kdl");
    e.copy(&wallpaper, "/usr/share/backgrounds/kuma/kuma-wallpaper.jpg");
    // The shell: built from the kumaui tree and staged into the build
    // context beside the running kuma, the same road kuma-nostrd rides
    // — the release ships its binaries together, and a build whose
    // sibling is missing is a checkout, not a release. The failure is
    // honest and early: this COPY would name a file the context never
    // carried.
    let shell_bin = e.supplied("kuma-shell");
    e.copy_exec(&shell_bin, "/usr/bin/kuma-shell");
    // The greeter rides the same road: kumaUI's build produces both
    // binaries, and an image that ships one without the other ships a
    // greeter config pointing at a binary that is not there.
    e.copy_exec(&greeter_bin, "/usr/bin/kuma-greeter");
    e.copy(&kitty, "/etc/xdg/kitty/kitty.conf");
    // kitty skips settings it doesn't recognise and starts anyway, so a
    // renamed key ships a silently unthemed terminal — which is exactly
    // how foot 1.27 voided this palette before kuma switched. Parse the
    // file with kitty's own loader at build time, and treat BOTH of its
    // complaints as fatal: accumulate_bad_lines catches malformed lines
    // but NOT unknown keys, which are only ever logged to stderr (that
    // asymmetry was verified by sabotage, so don't collapse this into
    // the exit code alone). Grepping kitty's own log keeps the check
    // free of an option allowlist to maintain.
    e.raw(
        "RUN rc=0; kitty +runpy \"import sys; from kitty.config import load_config; bad = []; load_config('/etc/xdg/kitty/kitty.conf', accumulate_bad_lines=bad); sys.exit('malformed kitty.conf lines: %s' % bad if bad else 0)\" 2>/tmp/kitty.err || rc=$?; \\\n    cat /tmp/kitty.err >&2; \\\n    if grep -q 'unknown config key' /tmp/kitty.err; then rc=1; fi; \\\n    rm -f /tmp/kitty.err; exit $rc\n",
    );
    // The theme is a static palette in that file now — the sixteen ANSI
    // slots and background/foreground/cursor, chosen once and shipped —
    // because the wallpaper-derived render died with noctalia. The
    // loader check above is what keeps it honest; there is no template
    // left to render.
    e.copy_exec(&clipboard, "/usr/libexec/kuma-clipboard-bridge");
    e.copy(&fastfetch, "/etc/xdg/fastfetch/config.jsonc");
    e.copy(&fastfetch_logo, "/usr/lib/kuma/fastfetch-logo.txt");
    e.copy_exec(&xsettings, "/usr/libexec/kuma-xsettings");
    e.copy(&xsettingsd, "/usr/lib/kuma/xsettingsd.conf");
    e.copy(&binds, "/usr/lib/kuma/niri-binds.kdl");
    e.copy_exec(&record, "/usr/libexec/kuma-record");
    e.copy_exec(&battery, "/usr/libexec/kuma-battery-watch");
    // The shell as a supervised unit, and the guard that refuses to
    // sleep without it. `--global` because the shell is a user unit
    // and every account on this image should get it; the sleep guard
    // is system-wide because sleep is.
    e.copy(&shell, "/usr/lib/systemd/user/kuma-shell.service");
    e.copy(&guard_service, "/usr/lib/systemd/system/kuma-sleep-guard.service");
    e.copy_exec(&guard, "/usr/libexec/kuma-sleep-guard");
    e.enable_global_then_system(&["kuma-shell.service"], &["kuma-sleep-guard.service"]);
    e.copy(&gtk3, "/etc/gtk-3.0/settings.ini");
    e.copy(&gtk4, "/etc/gtk-4.0/settings.ini");
    e.copy(&mimeapps, "/etc/xdg/mimeapps.list");
    e.copy(&dconf_profile, "/etc/dconf/profile/user");
    e.raw(&keyring_pam("greetd"));
    // The lock screen's own service, beside the other PAM stack this
    // layer touches. Inert until kuma-shell is what kuma-shell.service
    // runs; see KUMA_LOCK_PAM for why it ships ahead of that switch.
    e.copy(&pam_lock, "/etc/pam.d/kuma-lock");
    e.copy(&dconf_dark, "/etc/dconf/db/local.d/10-kuma-dark");
    e.copy(&dconf_blueman, "/etc/dconf/db/local.d/10-kuma-blueman");
    e.raw("RUN dconf update\n");
    e.copy(&autostart_blueman, "/etc/xdg/autostart/blueman.desktop");
    e.copy(&autostart_polkit, "/etc/xdg/autostart/polkit-mate-authentication-agent-1.desktop");
    // The packaged default config is complete (all keybindings); Kuma's
    // config is that plus our session extras, validated at build time.
    // Fedora's default config already spawns waybar — drop that line (and
    // its comment) or the bar starts twice; Kuma's extras spawn it.
    // Upstream's terminal is alacritty; Kuma ships kitty, so rewrite the
    // spawn (and its hotkey-overlay title). grep first: if a niri update
    // stops naming alacritty, fail the build instead of silently
    // shipping a Mod+T that spawns a terminal the image doesn't have.
    // The same grep-before-sed guard covers the two boot-polish tweaks:
    // uncomment the stock skip-at-startup (the overlay is noise during
    // login handoff) and give the layout the shell's wallpaper base, so
    // the gap between niri.service coming up and kuma-shell painting the
    // real wallpaper reads as a fade instead of a black flash. Both are
    // seded rather than appended to NIRI_EXTRAS because niri rejects a
    // second top-level `layout`/`hotkey-overlay` node, and the default
    // config already has both.
    e.raw(
        &format!("RUN grep -q '\"alacritty\"' /usr/share/doc/niri/default-config.kdl \\\n    && grep -qF '{NIRI_STOCK_LAUNCHER}' /usr/share/doc/niri/default-config.kdl \\\n    && grep -qF '{NIRI_STOCK_LOCK}' /usr/share/doc/niri/default-config.kdl \\\n    && grep -qF '{NIRI_STOCK_ORCA}' /usr/share/doc/niri/default-config.kdl \\\n    && grep -qF '// skip-at-startup' /usr/share/doc/niri/default-config.kdl \\\n    && grep -q '^layout {{' /usr/share/doc/niri/default-config.kdl \\\n    && mkdir -p /etc/niri \\\n    && sed -e 's/alacritty/kitty/g' -e '/starts waybar/d' -e '/^spawn-at-startup \"waybar\"$/d' -e '/XF86Audio/d' -e '/XF86MonBrightness/d' -e 's|// skip-at-startup|skip-at-startup|' -e '/^layout {{/a\\    background-color \"#11111B\"' -e 's|{NIRI_STOCK_LAUNCHER}|{NIRI_MENU_BIND}|' -e 's|{NIRI_STOCK_LOCK}|{NIRI_LOCK_BIND}|' -e '/pkill orca/d' -e '/^binds {{/r /usr/lib/kuma/niri-binds.kdl' /usr/share/doc/niri/default-config.kdl > /etc/niri/config.kdl \\\n    && cat /usr/lib/kuma/niri-extras.kdl >> /etc/niri/config.kdl \\\n    && niri validate --config /etc/niri/config.kdl\n"),
    );
    // Every "attach a file" button in every app did nothing, silently.
    //
    // niri's own portals.conf prefers the GNOME backend for everything
    // it does not name, and that backend does not implement FileChooser:
    // it delegates to org.gnome.Nautilus. kuma ships Thunar, so the name
    // is not activatable and the request dies inside the backend. The
    // only trace anywhere is "Delegated FileChooser call failed: The
    // name is not activatable" in the *user* journal, which is why this
    // survived two desktops and a bare-metal install unnoticed.
    //
    // The `default=gnome;gtk;` fallback cannot save it. Fallback is
    // resolved from the .portal files, gnome.portal advertises
    // FileChooser, and the failure only happens later, one level down,
    // where the router cannot see it. So the interface has to be
    // pointed at gtk by name. gtk implements it for real, is already
    // installed for Access and Notification, and is already running.
    //
    // Derived from niri's file rather than copied beside it: the
    // highest-precedence file wins outright instead of merging, so a
    // hand-copy would silently freeze whatever niri's defaults were the
    // day it was written. Both greps fail the build instead of shipping
    // a dead picker: the first if niri stops shipping the file this is
    // derived from, the second if the gtk backend stops implementing
    // the one interface being routed to it.
    e.raw(
        "RUN grep -q '^\\[preferred\\]' /usr/share/xdg-desktop-portal/niri-portals.conf \\\n    && grep -q 'org.freedesktop.impl.portal.FileChooser' /usr/share/xdg-desktop-portal/portals/gtk.portal \\\n    && mkdir -p /etc/xdg-desktop-portal \\\n    && { cat /usr/share/xdg-desktop-portal/niri-portals.conf; echo 'org.freedesktop.impl.portal.FileChooser=gtk;'; } > /etc/xdg-desktop-portal/niri-portals.conf\n",
    );
    // Upstream niri-session imports the ENTIRE greeter environment into
    // the systemd user manager — deprecated (warns in the journal every
    // login) and indiscriminate. Scope it: the XDG_* trio is how
    // niri.service finds the logind session; PATH carries the login
    // shell's profile.d additions (brew) into everything niri spawns.
    // grep first so the build fails if a niri update rewords the script.
    e.raw(
        "RUN grep -qx '    systemctl --user import-environment' /usr/bin/niri-session \\\n    && sed -i 's/^    systemctl --user import-environment$/    systemctl --user import-environment PATH XDG_SESSION_ID XDG_SEAT XDG_VTNR/' /usr/bin/niri-session\n",
    );
    e.raw(
        "RUN systemctl set-default graphical.target && systemctl enable greetd.service firewalld.service power-profiles-daemon.service bluetooth.service cups.service avahi-daemon.service chronyd.service\n",
    );
}

fn desktop_cosmic(e: &mut Emitter<'_>) {
    let config = e.config;
    if config.system.desktop != Desktop::Cosmic {
        return;
    }
    let favorites = e.stage("cosmic-favorites", COSMIC_FAVORITES);
    let background = e.stage("cosmic-background", COSMIC_BACKGROUND);
    // Shared with the greeter-seam block's desktop-common staging; same
    // contents, asserted by the walk.
    let kargs = e.stage("kargs-desktop.toml", DESKTOP_KARGS);
    let fastfetch = e.stage("fastfetch-config.jsonc", FASTFETCH_CONFIG);
    let fastfetch_logo = e.stage("fastfetch-logo.txt", FASTFETCH_LOGO);
    let wallpaper = e.stage("kuma-wallpaper.jpg", WALLPAPER);

    e.raw("\n");
    e.raw(&dnf_install(&COSMIC_PACKAGES.join(" ")));
    e.raw(&mesa_freeworld());
    // Fedora ships cosmic-greeter.service (preset-enabled, aliased as
    // display-manager.service) running `greetd --config
    // /etc/greetd/cosmic-greeter.toml` — kuma writes no greeter config
    // of its own here, unlike the niri arm.
    if let Some(user) = &config.user {
        if user.autologin {
            // same greetd initial_session semantics as the niri arm
            // (straight into the desktop at boot, greeter on logout),
            // appended to the config cosmic-greeter.service reads.
            // test -f first so a moved config fails the build instead
            // of autologin silently not happening.
            e.raw(&format!(
                "RUN test -f /etc/greetd/cosmic-greeter.toml && printf '\\n[initial_session]\\ncommand = \"start-cosmic\"\\nuser = \"{}\"\\n' >> /etc/greetd/cosmic-greeter.toml\n",
                user.name
            ));
        }
    }
    // kuma declares the user and its look is settings, not a wizard —
    // the first-boot setup must not fire. Plain rm so the build fails
    // if COSMIC ever moves the autostart file, instead of the wizard
    // silently resurfacing.
    e.raw("RUN rm /etc/xdg/autostart/com.system76.CosmicInitialSetup.desktop\n");
    // cosmic-comp promotes buffers straight to scanout, and on AMD
    // GFX10+ (seen on Rembrandt/680M) a promoted buffer can carry a
    // DCC-compressed modifier the scanout path reads as raw pixels —
    // intermittent bands of static across the panel. Disabling only
    // overlay scanout was not enough: the band recurred with that var
    // verifiably active, because fullscreen direct scanout takes the
    // same DCC path. Both knobs together are the verified cure on
    // bare metal; the cost is scanout-bypass power savings only,
    // composed output is unaffected. pam_env applies /etc/environment
    // to the greetd-spawned session, which is where cosmic-comp lives.
    // Upstream: pop-os/cosmic-comp#1039, #2152.
    e.raw(
        "RUN printf 'COSMIC_DISABLE_OVERLAY_SCANOUT=1\\nCOSMIC_DISABLE_DIRECT_SCANOUT=1\\n' >> /etc/environment\n",
    );
    // cosmic-greeter authenticates against its own PAM service, not
    // greetd's: asserting /etc/pam.d/greetd here would pass while
    // the stack COSMIC logs in through went unchecked.
    e.raw(&keyring_pam("cosmic-greeter"));
    e.copy(&kargs, "/usr/lib/bootc/kargs.d/10-kuma-desktop.toml");
    e.copy(&fastfetch, "/etc/xdg/fastfetch/config.jsonc");
    e.copy(&fastfetch_logo, "/usr/lib/kuma/fastfetch-logo.txt");
    e.copy(&wallpaper, "/usr/share/backgrounds/kuma/kuma-wallpaper.jpg");
    // Overwrite COSMIC's packaged defaults in place, guarded so the
    // build fails if an update moves them — an override at a path
    // nothing reads would silently ship the stock look.
    e.raw(
        "RUN test -f /usr/share/cosmic/com.system76.CosmicAppList/v1/favorites \\\n    && test -f /usr/share/cosmic/com.system76.CosmicBackground/v1/all\n",
    );
    e.copy(&favorites, "/usr/share/cosmic/com.system76.CosmicAppList/v1/favorites");
    e.copy(&background, "/usr/share/cosmic/com.system76.CosmicBackground/v1/all");
    // cosmic-greeter.service, not greetd.service: it already owns the
    // display-manager alias via preset — enabling greetd would fight
    // it (and did, failing the first prototype build). Explicit enable
    // is idempotent with the preset and keeps intent visible.
    e.raw(
        "RUN systemctl set-default graphical.target && systemctl enable cosmic-greeter.service firewalld.service power-profiles-daemon.service bluetooth.service cups.service avahi-daemon.service chronyd.service\n",
    );
}

/// Both flatpak spans ride this gate rather than their own: an emptied
/// list must still converge, so "is anything declared" is the wrong
/// question for a feature whose job includes taking things back.
fn wants_flatpak(config: &Config) -> bool {
    config.system.desktop != Desktop::None || !config.packages.flatpak.is_empty()
}

fn flatpak_remote(e: &mut Emitter<'_>) {
    if !wants_flatpak(e.config) {
        return;
    }
    if e.config.system.desktop == Desktop::None {
        e.raw("\n");
        e.raw(&dnf_install("flatpak"));
    }
    // Preconfigured-remote mechanism: flatpak reads /etc/flatpak/remotes.d,
    // so Flathub (with its GPG key) ships as image content. Flathub is
    // Kuma's only app source — mask the unit that injects Fedora's
    // registry remote at boot, or non-interactive installs become
    // ambiguous between the two.
    e.raw(&format!(
        "RUN curl --fail -Lo /etc/flatpak/remotes.d/flathub.flatpakrepo {FLATHUB_URL} \\\n    && systemctl mask flatpak-add-fedora-repos.service\n"
    ));
}

fn flatpak_sync(e: &mut Emitter<'_>) {
    let config = e.config;
    if !wants_flatpak(config) {
        return;
    }
    let mut list = config.packages.flatpak.join("\n");
    if !list.is_empty() {
        list.push('\n');
    }
    let flatpaks = e.stage("flatpaks", list);
    let sync = e.stage("kuma-flatpak-sync", FLATPAK_SYNC_SCRIPT);
    let sync_service = e.stage("kuma-flatpak-sync.service", FLATPAK_SYNC_SERVICE);
    let sync_timer = e.stage("kuma-flatpak-sync.timer", FLATPAK_SYNC_TIMER);
    let gate = e.stage("kuma-converge-gate", CONVERGE_GATE_SCRIPT);
    let daily_service = e.stage("kuma-flatpak-sync-daily.service", FLATPAK_SYNC_DAILY_SERVICE);
    // Both stores, always present: the overrides converger treats
    // absence as "nothing to do", and an image with no declared
    // overrides is exactly the image that must take the last ones back.
    let mut stores: Vec<(String, Content)> = Vec::new();
    for scope in [crate::config::Scope::System, crate::config::Scope::User] {
        let apps: Vec<(String, Content)> = config
            .overrides
            .iter()
            .filter(|(_, over)| over.scope == scope)
            .map(|(app, over)| (app.clone(), Content::Text(crate::overrides::render(over))))
            .collect();
        stores.push((scope.as_str().to_string(), Content::Tree(apps)));
    }
    let overrides = e.stage_tree("overrides", stores);
    let over_service = e.stage("kuma-flatpak-overrides.service", FLATPAK_OVERRIDES_SERVICE);
    let over_user = e.stage("kuma-flatpak-overrides-user.service", FLATPAK_OVERRIDES_USER_SERVICE);

    // Ship the declaration and sync even when the list is empty:
    // convergence means an emptied list removes the apps too.
    e.copy(&flatpaks, "/usr/lib/kuma/flatpaks");
    e.copy_exec(&sync, "/usr/libexec/kuma-flatpak-sync");
    e.copy(&sync_service, "/usr/lib/systemd/system/kuma-flatpak-sync.service");
    e.copy(&sync_timer, "/usr/lib/systemd/system/kuma-flatpak-sync.timer");
    e.copy_exec(&gate, "/usr/libexec/kuma-converge-gate");
    e.copy(&daily_service, "/usr/lib/systemd/system/kuma-flatpak-sync-daily.service");
    e.enable(&["kuma-flatpak-sync.service", "kuma-flatpak-sync.timer"]);
    // Overrides ride the same gate rather than their own. An
    // emptied [overrides] table has keys to take back, and gating
    // on "are any declared" would delete the converger in the same
    // build that gives it its last job.
    e.copy(&overrides, "/usr/lib/kuma/overrides");
    e.copy(&over_service, "/usr/lib/systemd/system/kuma-flatpak-overrides.service");
    e.copy(&over_user, "/usr/lib/systemd/user/kuma-flatpak-overrides.service");
    // --global enables it for every account that logs in, which is
    // the only way a unit reaches a home directory without root
    // writing into one.
    e.enable_system_then_global(
        &["kuma-flatpak-overrides.service"],
        &["kuma-flatpak-overrides.service"],
    );
}

fn brew(e: &mut Emitter<'_>) {
    let config = e.config;
    if !(config.system.brew || !config.packages.brew.is_empty()) {
        return;
    }
    let setup = e.stage("kuma-brew-setup", BREW_SETUP_SCRIPT);
    let setup_service = e.stage("kuma-brew-setup.service", BREW_SETUP_SERVICE);
    let profile_sh = e.stage("brew-profile.sh", BREW_PROFILE_SH);
    let profile_fish = e.stage("brew-profile.fish", BREW_PROFILE_FISH);
    let mut list = config.packages.brew.join("\n");
    if !list.is_empty() {
        list.push('\n');
    }
    let brews = e.stage("brews", list);
    let sync = e.stage("kuma-brew-sync", BREW_SYNC_SCRIPT);
    let sync_service = e.stage("kuma-brew-sync.service", BREW_SYNC_SERVICE);
    let sync_timer = e.stage("kuma-brew-sync.timer", BREW_SYNC_TIMER);
    let gate = e.stage("kuma-converge-gate", CONVERGE_GATE_SCRIPT);
    let daily_service = e.stage("kuma-brew-sync-daily.service", BREW_SYNC_DAILY_SERVICE);

    // git-core: brew needs git at runtime to update itself.
    // tar: the setup script unpacks brew's tarball with it. fedora-bootc
    // happened to ship both; a base composed from Fedora's minimal
    // manifest ships neither, so this layer pays for its own tools.
    e.raw("\n");
    e.raw(&dnf_install("git-core tar"));
    e.copy_exec(&setup, "/usr/libexec/kuma-brew-setup");
    e.copy(&setup_service, "/usr/lib/systemd/system/kuma-brew-setup.service");
    e.copy(&profile_sh, "/etc/profile.d/kuma-brew.sh");
    e.copy(&profile_fish, "/etc/fish/conf.d/kuma-brew.fish");
    // Declaration and sync ship even when the list is empty, same as
    // flatpaks: an emptied list must still remove what it installed.
    e.copy(&brews, "/usr/lib/kuma/brews");
    e.copy_exec(&sync, "/usr/libexec/kuma-brew-sync");
    e.copy(&sync_service, "/usr/lib/systemd/system/kuma-brew-sync.service");
    e.copy(&sync_timer, "/usr/lib/systemd/system/kuma-brew-sync.timer");
    e.copy_exec(&gate, "/usr/libexec/kuma-converge-gate");
    e.copy(&daily_service, "/usr/lib/systemd/system/kuma-brew-sync-daily.service");
    e.enable(&["kuma-brew-setup.service", "kuma-brew-sync.service", "kuma-brew-sync.timer"]);
}

fn rpm(e: &mut Emitter<'_>) {
    if e.config.packages.rpm.is_empty() {
        return;
    }
    e.raw("\n");
    e.raw(&dnf_install(&e.config.packages.rpm.join(" ")));
}

/// Named rather than inherited. openssh-server is in the composed base
/// and Fedora's RPM preset already enables it, so every kuma image has
/// run a listening network service that nothing here chose. This
/// changes no behaviour; it makes the choice kuma's, and testable. A
/// preset flip upstream would otherwise silently change what a kuma
/// machine exposes, in either direction: a base that stopped enabling
/// sshd would take `kuma vm` and the boot smoke stage with it, since
/// both reach the guest over ssh.
///
/// Placed before the declaration's own [services] block on purpose.
/// That boundary is what separates a curated default from kuma's
/// floor: a desktop's units are enabled above it and an owner's
/// `disable` can override them, while greenboot, fwupd, and the
/// timezone adoption come after and cannot be turned off. sshd is a
/// default, not a floor.
fn sshd(e: &mut Emitter<'_>) {
    e.raw("\n");
    e.enable(&["sshd.service"]);
}

fn services(e: &mut Emitter<'_>) {
    let list: Vec<String> = e
        .config
        .services
        .enable
        .iter()
        .map(|s| format!("systemctl enable {s}"))
        .chain(e.config.services.disable.iter().map(|s| format!("systemctl disable {s}")))
        .collect();
    if !list.is_empty() {
        e.raw(&format!("\nRUN {}\n", list.join(" && ")));
    }
}

/// FUSE 2, in every image, so an AppImage runs by being executable.
///
/// An AppImage is a squashfs the runtime mounts over FUSE before it
/// starts, and Fedora ships only FUSE 3: `fuse3-libs` and
/// `fusermount3`. The runtime asks for `libfuse.so.2` by name, so a
/// downloaded AppImage on a stock kuma machine died at `dlopen():
/// error loading libfuse.so.2` before any of its own code ran.
///
/// Both packages, because neither implies the other and each one
/// alone gets a different failure. `fuse` requires only fuse-common
/// and `which`, so on its own the dlopen still fails; `fuse-libs`
/// alone loads the library and then dies at `failed to exec
/// fusermount`, since libfuse.so.2 mounts by exec'ing the setuid
/// helper that only `fuse` ships. Measured against a real AppImage,
/// all three ways.
///
/// Not gated on a desktop. AppImages are how a lot of software is
/// shipped to Linux at all, the two packages are well under a
/// megabyte, and the failure they prevent is one a person hits by
/// double-clicking a file they downloaded, which is the worst place
/// to learn that a declaration needed another line. Coexists with
/// FUSE 3: separate libraries, separate helpers, shared fuse-common.
fn fuse(e: &mut Emitter<'_>) {
    e.raw("\n");
    e.raw(&dnf_install("fuse fuse-libs"));
}

/// Boot health, in every image: greenboot arms a GRUB boot counter on
/// the first boot of each new deployment; a boot that never reaches
/// the health check leaves the counter counting down, GRUB falls back
/// to the previous deployment when it hits zero, and greenboot makes
/// that permanent with `bootc rollback`. A bad update costs reboots,
/// not the machine. Rollback triggers only for freshly-updated-into
/// deployments (ConditionNeedsUpdate arms the trigger), so a
/// previously-good deployment that starts failing demands a human
/// instead of rolling back pointlessly. Core package only:
/// greenboot-default-health-checks ships a *required* DNS probe that
/// assumes an always-networked IoT box — it would roll back a laptop
/// that happens to boot offline.
fn greenboot(e: &mut Emitter<'_>) {
    e.raw("\n");
    e.raw(&dnf_install("greenboot"));
}

/// Plymouth, in every image, for the same reason greenboot is: base
/// behavior is not a per-declaration decision. On an encrypted machine
/// the LUKS passphrase prompt IS plymouth's (dracut draws it through
/// plymouth when the module is present), so "no splash please" is not
/// a thing an unencrypted machine gets to decline on another's behalf.
///
/// Four packages and what each is for:
/// - plymouth: the daemon, the theme loader, the dracut module.
/// - plymouth-plugin-script: runs .script themes like spinner_alt.
///   Nothing pulls it in; without it the default theme fails to load
///   and plymouth falls back to its text plugin at boot. Invisible in
///   a build, obvious on tty0.
/// - plymouth-plugin-label: renders Image.Text(). A separate Fedora
///   package, and a silent hole: the script plugin dlopens a label
///   plugin at runtime, finds none, and draws nothing. The first
///   eyes-on boot (2026-08-26) watched a machine hang on the LUKS
///   prompt it was not drawing: spinner animating, no text, no
///   bullets, every serial assertion green.
/// - dejavu-sans-fonts: Image.Text() renders through a font, and the
///   base carries no fonts at all (kuma's terminal font is a
///   desktop-layer flatpak-adjacent install). Without one, password
///   prompts render blank: adi1090x's README warns about exactly this
///   shape on Arch. Which font ends up in the initramfs is dracut's
///   fc-match call, not this name: it installs whatever fontconfig
///   calls the default sans (measured: Noto on a desktop image that
///   ships several families). The package's job is to guarantee a
///   readable face exists at all, on even a minimal image; dejavu-sans
///   is Fedora's boringest such default.
fn plymouth(e: &mut Emitter<'_>) {
    let theme = e.stage_tree(
        "plymouth",
        plymouth_theme::FILES.iter().map(|(name, bytes)| (name.to_string(), Content::Bytes(bytes))),
    );

    e.raw("\n");
    e.raw(&dnf_install("plymouth plymouth-plugin-script plymouth-plugin-label dejavu-sans-fonts"));
    // The theme itself, staged above from build.rs's embedded table;
    // LICENSE travels so the installed copy carries its GPL terms.
    // COPY copies a directory's contents, so the destination names the
    // theme directory plymouth expects to find the files under.
    e.copy(&theme, &format!("/usr/share/plymouth/themes/{PLYMOUTH_THEME_DIR}/"));
    // Setting the default theme has TWO surfaces, and the symlink alone
    // loses. plymouth-populate-initrd (which decides initramfs contents)
    // asks plymouth-set-default-theme, resolving in order: /etc/plymouth/
    // plymouthd.conf [Daemon] Theme, THEN the packaged plymouthd.defaults
    // (Fedora ships Theme=bgrt there), and only last the symlink. Measured
    // 2026-08-26 on an installed disk: bgrt won, spinner_alt lost,
    // populate exited 1 with stderr discarded, and the splash silently
    // shipped broken behind five green encrypted boots because serial
    // unlock never needs graphics. So name the theme in the conf file,
    // and keep the symlink as belt-and-suspenders for anything reading it.
    e.raw(&format!(
        "RUN printf '%s\\n' '[Daemon]' 'Theme={PLYMOUTH_THEME_DIR}' > /etc/plymouth/plymouthd.conf \\\n    && ln -sfn {PLYMOUTH_THEME_DIR}/{PLYMOUTH_THEME_DIR}.plymouth /usr/share/plymouth/themes/default.plymouth\n"
    ));
    // membership must not depend on magic: name the dracut module rather
    // than hoping hostonly detection picks plymouth up. The space on BOTH
    // sides of the value is dracut syntax, not decoration: without the
    // trailing one every dracut run in every kuma image warns
    // "<values> should have surrounding white spaces" (measured 2026-08-26
    // by A/B-ing the conf out of an image).
    e.raw(
        "RUN printf '%s\\n' 'add_dracutmodules+=\" plymouth \"' > /etc/dracut.conf.d/kuma-plymouth.conf\n",
    );
    // Regenerate the initramfs IN THE BUILD, with everything above in
    // place. The one shipping in /usr/lib/modules was made during the
    // BASE COMPOSE, before this Containerfile ran a single line: no conf,
    // no theme, no font. Waiting for "bootc regenerates on kernel update"
    // means the splash only appears after the machine's NEXT kernel bump.
    // Measured 2026-08-26 on an installed disk: its initramfs byte-equal
    // to the compose-time one, spinner_alt count 0. liveiso.rs ships this
    // same pattern for dmsquash-live; the cost objection recorded there
    // (a slow step in every build) does not apply here because podman
    // caches the RUN layer: unchanged inputs skip the dracut minute, and
    // the cost cannot be hidden in a throwaway derived image the way the
    // ISO's can, because this image IS the one machines install from.
    //
    // Two container-only fixups the RUN does first, both measured
    // 2026-08-26:
    // - /var/roothome: dracut recreates the system's toplevel symlinks
    //   inside the initramfs, and /root -> var/roothome dangles in any
    //   bootc image because /var is empty until first boot. Without the
    //   directory, every build dies on "ERROR: installing '/root'".
    // - sysloglvl=0 through --add-confdir: Fedora's 01-dist.conf sets
    //   sysloglvl=5 and a build container has no syslog socket, so an
    //   otherwise clean run prints a fatal-LOOKING "No '/dev/log'" error
    //   that means nothing. The 99- prefix sorts the drop-in after
    //   01-dist.conf (drop-ins load by filename across directories), and
    //   --add-confdir keeps the override scoped to this invocation:
    //   nothing lands in /etc on machines, where syslog works and this
    //   line would be wrong.
    e.raw("RUN set -eux; \\\n");
    e.raw("    kver=\"$(ls /usr/lib/modules)\"; \\\n");
    e.raw("    test \"$(echo \"$kver\" | wc -l)\" -eq 1; \\\n");
    e.raw("    mkdir -p /var/roothome; \\\n");
    e.raw("    dconf=\"$(mktemp -d)\"; \\\n");
    e.raw("    printf '%s\\n' 'sysloglvl=0' > \"$dconf/99-kuma-build.conf\"; \\\n");
    e.raw(
        "    dracut --add-confdir \"$dconf\" --force --no-hostonly --add plymouth \"/usr/lib/modules/$kver/initramfs.img\" \"$kver\"\n",
    );
}

/// Identity, wallpaper, kargs, the greeter check, and kuma's own verbs
/// in whatever launcher the session shipped — everything desktop
/// common, gated on a desktop existing at all.
fn greeter_seam(e: &mut Emitter<'_>) {
    let config = e.config;
    if config.system.desktop == Desktop::None {
        return;
    }
    // The desktop-common files, staged here and copied by this block
    // and by whichever desktop arm runs beside it.
    e.stage("kargs-desktop.toml", DESKTOP_KARGS);
    e.stage("fastfetch-config.jsonc", FASTFETCH_CONFIG);
    e.stage("fastfetch-logo.txt", FASTFETCH_LOGO);
    e.stage("kuma-wallpaper.jpg", WALLPAPER);
    let greeter_check = e.stage("kuma-greeter-check", GREETER_CHECK);
    let launch = e.stage("kuma-launch", KUMA_LAUNCH);
    let mut entries = Vec::new();
    for entry in seam::ENTRIES {
        entries.push(e.stage(&format!("{}.desktop", entry.id), seam::render(entry)));
    }

    e.copy_exec(&greeter_check, "/usr/lib/greenboot/check/required.d/50-kuma-greeter.sh");
    // kuma's own verbs, in whatever launcher the session shipped.
    // On both desktops, deliberately: the seam is the thing being
    // tested, and it is only a seam if it is not niri's.
    e.copy_exec(&launch, "/usr/libexec/kuma-launch");
    for (entry, file) in seam::ENTRIES.iter().zip(&entries) {
        e.copy(file, &seam::path(entry));
    }
    // The build validates what it generated rather than leaving it
    // to the smoke stage. A malformed entry does not fail anything
    // at runtime: it is silently skipped, so the verb simply is not
    // in the launcher and nothing anywhere says why.
    e.raw(&format!(
        "RUN desktop-file-validate {}\n",
        seam::ENTRIES.iter().map(seam::path).collect::<Vec<String>>().join(" ")
    ));
    // And that what the entries name is in the image.
    // desktop-file-validate reads the syntax of the `Exec` line and
    // never whether the program on it exists, which is the half that
    // breaks when a package moves. The icon is checked for the same
    // reason and by file: an entry whose icon does not resolve draws
    // a blank square, which is not an error anywhere.
    //
    // `/usr/bin/kuma` is deliberately not checked here — it is copied
    // much later in the file and proved runnable there, and a guard
    // before its COPY proves nothing.
    e.raw("RUN test -x /usr/libexec/kuma-launch \\\n");
    // Every icon, not the first one. The deleted icon_theme() step
    // failed per icon and searched for the file; checking one name
    // and calling it "the icons are checked" is how the other seven
    // ship as blank squares when Adwaita moves a name.
    for (i, entry) in seam::ENTRIES.iter().enumerate() {
        let last = i + 1 == seam::ENTRIES.len();
        e.raw(&format!(
            "    && find /usr/share/icons/Adwaita -name {}.svg | grep -q .{}\n",
            entry.icon,
            if last { "" } else { " \\" }
        ));
    }
}

/// The boot-time cluster: rollback arming, fstab truth, boot menu
/// names, and the SELinux label the swapfile needs. Every image, even
/// the ones with no swapfile: doctor grades all of these, and a
/// machine that gains a swapfile later must not need a rebuild.
fn boot_health(e: &mut Emitter<'_>) {
    let health_sync = e.stage("kuma-boot-health-sync", BOOT_HEALTH_SYNC_SCRIPT);
    let health_service = e.stage("kuma-boot-health-sync.service", BOOT_HEALTH_SYNC_SERVICE);
    let swap_fcontext = e.stage("kuma-swap-fcontext", SWAP_FCONTEXT);
    let swap_label = e.stage("kuma-swap-label.service", SWAP_LABEL_SERVICE);
    let fstab_sync = e.stage("kuma-fstab-sync", FSTAB_SYNC_SCRIPT);
    let fstab_service = e.stage("kuma-fstab-sync.service", FSTAB_SYNC_SERVICE);
    let boot_titles = e.stage("kuma-boot-titles.service", BOOT_TITLES_SERVICE);

    e.copy_exec(&health_sync, "/usr/libexec/kuma-boot-health-sync");
    e.copy(&health_service, "/usr/lib/systemd/system/kuma-boot-health-sync.service");
    e.copy(&swap_fcontext, "/etc/selinux/targeted/contexts/files/file_contexts.local");
    e.copy(&swap_label, "/usr/lib/systemd/system/kuma-swap-label.service");
    e.copy_exec(&fstab_sync, "/usr/libexec/kuma-fstab-sync");
    e.copy(&fstab_service, "/usr/lib/systemd/system/kuma-fstab-sync.service");
    e.copy(&boot_titles, "/usr/lib/systemd/system/kuma-boot-titles.service");
    e.enable(&[
        "greenboot-healthcheck.service",
        "greenboot-set-rollback-trigger.service",
        "greenboot-success.target",
        "kuma-boot-health-sync.service",
        "kuma-fstab-sync.service",
        "kuma-boot-titles.service",
        "kuma-swap-label.service",
    ]);
}

/// What the machine will and will not accept from a registry. On every
/// image rather than only on published ones: the machine that needs
/// this is the one that installed from the published image and then
/// updates from it, and that machine's /etc comes from whatever image
/// it was installed from.
fn signature(e: &mut Emitter<'_>) {
    let cosign = e.stage("cosign.pub", COSIGN_PUB);
    let policy = e.stage("containers-policy.json", signature_policy());
    let sigstore = e.stage("kuma-sigstore.yaml", registries_d());

    e.raw("\n");
    e.copy(&cosign, COSIGN_PUB_PATH);
    e.copy(&policy, "/etc/containers/policy.json");
    e.copy(&sigstore, "/etc/containers/registries.d/kuma-sigstore.yaml");
}

/// Refreshes LVFS metadata only; it never applies a firmware update on
/// its own. Applying stays a deliberate act — `fwupdmgr update`, or the
/// org.gnome.Firmware flatpak the examples declare, which drives this
/// same daemon over the system bus.
fn fwupd(e: &mut Emitter<'_>) {
    e.enable(&["fwupd-refresh.timer"]);
}

fn snapshots(e: &mut Emitter<'_>) {
    if !e.config.snapshots.enable {
        return;
    }
    let snapshot = e.stage("kuma-snapshot", snapshot_script(e.config));
    let service = e.stage("kuma-snapshot.service", SNAPSHOT_SERVICE);
    let timer = e.stage("kuma-snapshot.timer", snapshot_timer(&e.config.snapshots.interval));

    // btrfs-progs is named rather than assumed: it happens to ride in
    // today, and a snapshot timer that dies on a missing binary would
    // be a backup that silently isn't one.
    e.raw("\n");
    e.raw(&dnf_install("btrfs-progs"));
    e.copy_exec(&snapshot, "/usr/libexec/kuma-snapshot");
    e.copy(&service, "/usr/lib/systemd/system/kuma-snapshot.service");
    e.copy(&timer, "/usr/lib/systemd/system/kuma-snapshot.timer");
    e.enable(&["kuma-snapshot.timer"]);
}

/// Inside the snapshots gate would read as tidier and would be wrong:
/// validation already refuses backup.enable without snapshots.enable,
/// so nesting it would hide that dependency behind an `if` instead of
/// stating it where somebody reading the Containerfile can see it.
fn backup(e: &mut Emitter<'_>) {
    let config = e.config;
    if !config.backup.enable {
        return;
    }
    let backup = e.stage("kuma-backup", backup_script(config));
    let service = e.stage("kuma-backup.service", backup_service(config));
    let timer = e.stage("kuma-backup.timer", backup_timer(&config.backup.interval));
    let restore = e.stage("kuma-restore", RESTORE_SCRIPT);
    let restore_service = e.stage("kuma-restore.service", RESTORE_SERVICE);

    // restic is named for the same reason btrfs-progs is above: a
    // timer that dies on a missing binary is a backup that silently
    // is not one. Fedora packages it, so nothing is vendored.
    e.raw("\n");
    e.raw(&dnf_install("restic"));
    e.copy_exec(&backup, "/usr/libexec/kuma-backup");
    e.copy(&service, "/usr/lib/systemd/system/kuma-backup.service");
    e.copy(&timer, "/usr/lib/systemd/system/kuma-backup.timer");
    e.enable(&["kuma-backup.timer"]);
    // The other end of the promise. Enabled always and gated on a
    // request file, because the machine that needs it has been
    // installed exactly once and there is nobody to start it.
    e.copy_exec(&restore, "/usr/libexec/kuma-restore");
    e.copy(&restore_service, "/usr/lib/systemd/system/kuma-restore.service");
    e.enable(&["kuma-restore.service"]);
}

/// Every image can adopt a kuma vm host timezone; no-op on hardware.
fn vm_timezone(e: &mut Emitter<'_>) {
    let tz = e.stage("kuma-vm-timezone", VM_TZ_SCRIPT);
    let service = e.stage("kuma-vm-timezone.service", VM_TZ_SERVICE);

    e.raw("\n");
    e.copy_exec(&tz, "/usr/libexec/kuma-vm-timezone");
    e.copy(&service, "/usr/lib/systemd/system/kuma-vm-timezone.service");
    e.enable(&["kuma-vm-timezone.service"]);
}

fn timezone(e: &mut Emitter<'_>) {
    let Some(tz) = e.config.system.timezone.clone() else {
        return;
    };
    // test -e first so a typo'd zone fails the build instead of
    // silently producing a dangling /etc/localtime symlink.
    e.raw(&format!(
        "\nRUN test -e /usr/share/zoneinfo/{tz} && ln -sfn /usr/share/zoneinfo/{tz} /etc/localtime\n"
    ));
}

/// The converger ships in EVERY image, including ones that declare no
/// account. A published image declares none by design, and a machine
/// installed from it has no account and no root password, so something
/// has to write /var/lib/kuma/user on the target and something has to act
/// on it at first boot. Shipping the unit only when the image already
/// knows the answer is what made that impossible.
///
/// It is a no-op with neither file present, so a desktop image built
/// from a userless declaration gains one oneshot that exits 0.
/// Before the account converger it is ordered against, and in every
/// image for the same reason that one is: the machine an image gets
/// installed onto is where this matters, and the image cannot know
/// whether that will happen.
fn home_subvol(e: &mut Emitter<'_>) {
    let subvol = e.stage("kuma-home-subvol", HOME_SUBVOL_SCRIPT);
    let service = e.stage("kuma-home-subvol.service", HOME_SUBVOL_SERVICE);

    e.raw("\n");
    e.copy_exec(&subvol, "/usr/libexec/kuma-home-subvol");
    e.copy(&service, "/usr/lib/systemd/system/kuma-home-subvol.service");
    e.enable(&["kuma-home-subvol.service"]);
}

fn user(e: &mut Emitter<'_>) {
    let config = e.config;
    let sync = e.stage("kuma-user-sync", USER_SYNC_SCRIPT);
    let sync_service = e.stage("kuma-user-sync.service", USER_SYNC_SERVICE);
    // Unconditional, like the Containerfile lines that copy them: the
    // converger has to be present in an image that declares no account,
    // because that is the image an installer writes /var/lib/kuma/user onto.
    let mut account = None;
    let mut keys = None;
    let mut sshd_conf = None;
    if let Some(user) = &config.user {
        let mut decl = format!("KUMA_USER='{}'\n", user.name);
        if let Some(shell) = &user.shell {
            decl.push_str(&format!("KUMA_SHELL='/usr/bin/{shell}'\n"));
        }
        if !user.groups.is_empty() {
            decl.push_str(&format!("KUMA_GROUPS='{}'\n", user.groups.join(" ")));
        }
        if let Some(hash) = &user.password_hash {
            decl.push_str(&format!("KUMA_PASSWORD_HASH='{hash}'\n"));
        }
        account = Some(e.stage("kuma-user", decl));
        if !user.ssh_keys.is_empty() {
            let mut joined = user.ssh_keys.join("\n");
            joined.push('\n');
            keys = Some(e.stage("kuma-user-keys", joined));
            sshd_conf = Some(e.stage("kuma-sshd-keys.conf", SSHD_KUMA_KEYS));
        }
    }

    e.raw("\n");
    e.copy_exec(&sync, "/usr/libexec/kuma-user-sync");
    e.copy(&sync_service, "/usr/lib/systemd/system/kuma-user-sync.service");
    e.enable(&["kuma-user-sync.service"]);
    if let Some(user) = &config.user {
        // 600: only the root-run sync service reads this, and it can carry
        // the password hash — no reason to hand that to every local user.
        e.copy_private(account.as_ref().unwrap(), "/usr/lib/kuma/user");
        if let Some(shell) = &user.shell {
            // after the rpm layer, so a shell the config forgot to install
            // fails the build instead of locking the account out at login
            e.raw(&format!("RUN test -x /usr/bin/{shell}\n"));
        }
        if let (Some(keys), Some(sshd_conf)) = (&keys, &sshd_conf) {
            e.copy(keys, &format!("/etc/kuma/keys/{}", user.name));
            e.copy(sshd_conf, "/etc/ssh/sshd_config.d/40-kuma-keys.conf");
        }
    }
}

/// A declared system shell gets the same build-time guard a declared
/// user's does, and needs it more: nothing on a published image will
/// notice it is wrong until an installer creates an account with it,
/// by which point a disk has been written. Same placement reasoning,
/// after the rpm layer that would install it.
fn system_shell(e: &mut Emitter<'_>) {
    if let Some(shell) = &e.config.system.shell {
        e.raw(&format!("RUN test -x /usr/bin/{shell}\n"));
    }
}

/// /etc/hostname ships in every image because DEFAULT_HOSTNAME can't
/// win: the initrd's dracut-built os-release still says "fedora", its
/// systemd sets the kernel hostname first, and the real root won't
/// override a hostname that's already set. Image /etc is only the
/// ostree merge default, so a machine whose admin set a hostname
/// keeps it. COPY, never a RUN redirect: buildah bind-mounts
/// /etc/hostname (like /etc/hosts) into every RUN container, so a
/// redirect writes the runtime mount and never reaches the layer.
fn hostname(e: &mut Emitter<'_>) {
    // The same default `kuma install` falls back to, whose own doc
    // comment claims it "matches what every kuma image bakes" — an
    // invariant that was asserted and not shared.
    let hostname = e.config.system.hostname.as_deref().unwrap_or(crate::install::DEFAULT_HOSTNAME);
    let file = e.stage("hostname", format!("{hostname}\n"));

    e.raw("\n");
    e.copy(&file, "/etc/hostname");
}

fn locale(e: &mut Emitter<'_>) {
    let config = e.config;
    let Some(locale) = &config.system.locale else {
        return;
    };
    // The langpack makes the locale actually exist; without it glibc
    // silently falls back and every app renders C.UTF-8.
    if let Some(lang) = langpack(locale) {
        e.raw("\n");
        e.raw(&dnf_install(&format!("glibc-langpack-{lang}")));
    }
    e.raw(&format!("RUN echo 'LANG={locale}' > /etc/locale.conf\n"));
}

/// Anchors before branding only because everything after this point
/// is cosmetic; what matters is that they land before anything that
/// might need to trust them, and that update-ca-trust runs in the
/// same layer that adds them rather than being left for a boot.
fn ca_anchors(e: &mut Emitter<'_>) {
    let config = e.config;
    // Outside the flatpak gate: trust has nothing to do with apps.
    let mut staged = Vec::new();
    for (name, pem) in &config.system.ca_certificates {
        staged.push((name.clone(), e.stage(&format!("ca-{name}.crt"), pem.clone())));
    }
    if config.system.ca_certificates.is_empty() {
        return;
    }
    e.raw("\n");
    for (name, file) in &staged {
        e.copy(file, &format!("/etc/pki/ca-trust/source/anchors/{name}.crt"));
    }
    e.raw("RUN update-ca-trust\n");
}

fn branding_span(e: &mut Emitter<'_>) {
    // The renderer (os-release identity and friends) is a moved
    // namesake; this span is the door it ships through.
    e.raw(&branding());
}

/// The machine gets the kuma that built it. Everything else needed to
/// run kuma on a machine already shipped — the baked declaration below,
/// the convergence units, thirteen helpers in /usr/libexec — but not
/// the binary that drives them, so an ISO-installed machine could not
/// run the `kuma update --yes` docs/agents.md promises it, and the
/// fallback-to-baked-declaration path had nothing to execute it.
///
/// current_exe rather than a download: no network in the build, and no
/// version skew between the kuma that wrote this image and the kuma
/// that ships in it. The cost is that the binary is the build host's,
/// so a musl host, a different arch, or a glibc newer than the base's
/// produces one this image cannot execute.
///
/// Which is what the RUN is for. Without it the COPY succeeds, the
/// image ships, and the failure surfaces at first boot as a machine
/// whose kuma is an ELF it cannot run — the same class of far-end
/// failure as a shell that was never installed, and guarded the same
/// way (`RUN test -x /usr/bin/{shell}` above).
///
/// Late in the file, beside the declaration, because both layers
/// change on every edit.
fn kuma_binary(e: &mut Emitter<'_>) {
    // Supplied by write_context from its own parameter: the running
    // binary, not something the walk can render.
    let kuma = e.supplied("kuma");
    e.raw("\n");
    e.copy_exec(&kuma, "/usr/bin/kuma");
    e.raw("RUN /usr/bin/kuma --version\n");
}

/// The image carries the declaration it was built from, verbatim: the
/// machine stays self-describing when the original file is gone, and
/// `kuma init` seeds a working copy from it. No new secret exposure —
/// password_hash already ships in the kuma-user declaration. Late in
/// the file because this layer changes on every edit.
fn declaration(e: &mut Emitter<'_>) {
    // Supplied by write_context from its own parameter: the declaration
    // verbatim.
    let toml = e.supplied("kuma.toml");
    e.raw("\n");
    e.copy(&toml, "/usr/lib/kuma/kuma.toml");
}

/// What `kuma build` prunes by: each rebuild strands the previous
/// image as a dangling <none>, and only kuma's own should be reclaimed.
fn labels(e: &mut Emitter<'_>) {
    e.raw("\nLABEL io.kuma.image=\"1\"\n");
    // Which kuma generated this. The declaration alone cannot answer "is
    // this image the one that has my last change in it": an unchanged
    // declaration built by an older binary produces a perfectly current
    // looking image, so the probe reports in-sync and is right by its own
    // definition. Same question VERSION's own doc comment exists for, one
    // level up, and the same cost when it goes unanswered.
    //
    // A label rather than reading /usr/bin/kuma out of the image: the
    // probe behind bare `kuma` is meant to stay cheap, and `podman image
    // inspect` already runs there for the id and the timestamp. Running a
    // container to ask the binary its version would not.
    e.raw(&format!("LABEL io.kuma.builder=\"{}\"\n", crate::VERSION));
    // The version bootupd reads. bootc's install writes the installed
    // image's OCI version into /sysroot/.bootc-aleph.json, and bootupd's
    // reader requires it as a plain string — a null fails
    // bootloader-update.service on every boot. Nothing in kuma's pipeline
    // supplied one: the composed base never carries a version label
    // (rpm-ostree compose image emits none, whichever env runs it), so
    // every image kuma has ever built installed with "version": null and
    // a failed unit — invisible until the failed-unit grading landed, and
    // blamed for a week on the Fedora float. The number alone, like
    // os-release's: the full stamp's home is io.kuma.builder beside it.
    e.raw(&format!("LABEL org.opencontainers.image.version=\"{}\"\n", env!("CARGO_PKG_VERSION")));
}

fn sweep_lint(e: &mut Emitter<'_>) {
    e.raw(SWEEP);
    e.raw(LINT);
}

/// The units a live session on installer media must not run, derived
/// from the same tables that enable them — the old shape was a
/// hand-kept list in liveiso, and a unit that joined the image without
/// joining the list read as accounted-for to a test that only checked
/// the list. Table order; `systemctl mask` does not care about the
/// order of its operands.
pub(super) fn live_masks() -> Vec<&'static str> {
    BLOCKS
        .iter()
        .flat_map(|block| {
            block.units.iter().filter_map(|(unit, live)| match live {
                Live::Masked(_) => Some(*unit),
                _ => None,
            })
        })
        .collect()
}

/// The registry. Order IS emission order: the layer order of every
/// image kuma builds. A reorder here is a golden-visible diff, which
/// is the point — it is a reviewable decision, not an accident.
/// The nostr layer's daemon unit: the first user unit with the full
/// sandbox (docs/unit-sandboxing.md carries the reasoning). The relay
/// set and the inactivity window ride the exec line — `--relay` per
/// entry, `--inactivity-lock-secs` when the switch is armed — because
/// the daemon takes no declaration of its own: the declaration is
/// kuma's, and the unit is the spelling of it systemd can run. The
/// args stay the daemon's interface even now that the declaration
/// carries their values: a binary with no config file of its own is a
/// binary whose every fact arrives on argv, and that is the shape an
/// auditor can hold in their head.
fn nostrd_service(nostr: &crate::config::Nostr) -> String {
    // The list is [local, ...declared]: the bunker's first subscription
    // is the relay on this machine, and the declared set follows as
    // fallbacks. A relay that is not baked (the off switch) leaves the
    // declared set alone — removal was spoken in the declaration.
    let mut relay_args = String::new();
    if nostr.relay.enable {
        relay_args.push_str(&format!(" --relay {LOCAL_RELAY_URL}"));
    }
    for r in &nostr.relays {
        relay_args.push_str(&format!(" --relay {r}"));
    }
    // The switch rides only when armed: absent or 0 is the daemon's
    // own off, and a flag that repeats the default is noise the unit
    // does not need.
    let mut inactivity_arg = String::new();
    if let Some(secs) = nostr.inactivity_lock_secs.filter(|secs| *secs > 0) {
        inactivity_arg.push_str(&format!(" --inactivity-lock-secs {secs}"));
    }
    format!(
        r#"[Unit]
Description=kumaOS nostr bunker
# The bunker is session furniture: it starts and stops with the
# graphical session, and its socket lives under the session's runtime
# directory.
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=/usr/bin/kuma-nostrd{relay_args}{inactivity_arg}
# The state dir has to exist before the sandbox does: ReadWritePaths
# names it, and a namespace that cannot mount a missing path fails the
# unit before the daemon runs. The `+` runs this one command unsandboxed
# — ProtectHome would refuse the write the daemon itself would need.
ExecStartPre=+/usr/bin/mkdir -p %h/.local/state/kuma-nostr
# A bunker that exits zero is still a bunker that stopped answering.
Restart=always
Slice=session.slice

# The sandbox. A bunker holds a signing key; the unit is the reason an
# attacker already inside the session is the only attacker it matters
# against, and these lines are what keep it that way.
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
# The pairing list and the activity log are the daemon's own state, and
# the socket it answers on, the only things it may write. The socket
# lives in the session's runtime directory, which ProtectSystem=strict
# would otherwise mount read-only.
ReadWritePaths=%h/.local/state/kuma-nostr %t
# A unix socket to answer on, and relays over the network.
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
CapabilityBoundingSet=
SystemCallArchitectures=native

[Install]
WantedBy=graphical-session.target
"#
    )
}

/// The nostr block: the daemon and CLI binaries, the hardened unit, and
/// the guard that refuses to bake a bunker that cannot run. The relay
/// set is the declaration's own; the local relay joins the list when
/// the port lands.
fn nostr(e: &mut Emitter<'_>) {
    let config = e.config;
    if !config.nostr.enable {
        return;
    }
    let unit = e.stage("kuma-nostrd.service", nostrd_service(&config.nostr));
    // Supplied by write_context from beside the running binary: the
    // release carries the three together, and a build that cannot find
    // the siblings fails here rather than baking a unit whose
    // ExecStart names nothing.
    let nostrd = e.supplied("kuma-nostrd");
    let cli = e.supplied("kuma-nostr");
    let panel_desktop = e.stage("kuma-nostr-panel.desktop", NOSTR_PANEL_DESKTOP);
    e.raw("\n");
    e.copy_exec(&nostrd, "/usr/bin/kuma-nostrd");
    e.copy_exec(&cli, "/usr/bin/kuma-nostr");
    e.copy(&unit, "/usr/lib/systemd/user/kuma-nostrd.service");
    // --global because the bunker is session furniture for every
    // account: the first login arms it without each user opting in
    // again.
    if config.nostr.relay.enable {
        let relay = e.supplied("nip46-relay");
        let relay_unit = e.stage("nip46-relay.service", relay_service());
        e.copy_exec(&relay, "/usr/bin/nip46-relay");
        e.copy(&relay_unit, "/usr/lib/systemd/user/nip46-relay.service");

        // The tailnet exposure: a converge script and a oneshot, and
        // only when the declaration says serve. The switch is the
        // declaration's own, not an inference from the relay and
        // tailscaled both being on — those two say the relay exists and
        // that the machine talks to a tailnet, and neither says a
        // loopback-only relay belongs on it. Serving also needs
        // tailscaled; a declaration that says serve without it bakes
        // nothing, and the doctor names the missing piece.
        if config.nostr.serve && config.services.enable.iter().any(|s| s == "tailscaled.service") {
            let serve = e.stage("kuma-nostr-serve", NOSTR_SERVE_SCRIPT);
            let serve_unit = e.stage("kuma-nostr-serve.service", NOSTR_SERVE_SERVICE);
            e.copy_exec(&serve, "/usr/libexec/kuma-nostr-serve");
            e.copy(&serve_unit, "/usr/lib/systemd/system/kuma-nostr-serve.service");
            e.enable(&["kuma-nostr-serve.service"]);
        }
    }
    let mut global = vec!["kuma-nostrd.service"];
    if config.nostr.relay.enable {
        global.push("nip46-relay.service");
    }
    e.enable_global(&global);
    // The version guard is the binary-runs proof the kuma COPY carries:
    // a musl-host binary on a glibc base shows up here, at build, where
    // it costs a build, not at boot, where it costs the session.
    e.raw("RUN /usr/bin/kuma-nostrd --version\n");

    // The scheme handler wires a nostrconnect:// link to the shell's
    // panel — the panel itself lives in kuma-shell, which this block
    // never had to carry.
    e.copy(&panel_desktop, "/usr/share/applications/kuma-nostr-panel.desktop");
}

/// The local relay's address, and the port it binds: the bunker's
/// first subscription and the relay unit's exec line agree on both,
/// because the daemon connecting to a relay that is not listening is
/// the one failure the composition can have.
pub(crate) const LOCAL_RELAY_URL: &str = "ws://127.0.0.1:7777";
pub(crate) const LOCAL_RELAY_BIND: &str = "127.0.0.1:7777";

/// The local relay's unit: the ported relay as session furniture,
/// bound to loopback — the strongest privacy story the layer tells is
/// concrete by default. The sandbox is the daemon's, minus the
/// network-shy lines: this is the one component whose job is the
/// network, and its write set is empty.
fn relay_service() -> String {
    format!(
        r#"[Unit]
Description=kumaOS nostr relay (local, loopback only)
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=/usr/bin/nip46-relay --bind {LOCAL_RELAY_BIND}
Restart=always
Slice=session.slice

# Loopback is the boundary: the relay carries signing metadata for the
# machine's own bunker, and widening the bind is a decision made in a
# declaration, not a default.
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
MemoryDenyWriteExecute=yes
LockPersonality=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
RestrictAddressFamilies=AF_INET AF_INET6
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
CapabilityBoundingSet=
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
SystemCallArchitectures=native

[Install]
WantedBy=graphical-session.target
"#
    )
}

/// The converge script: `tailscale serve` is a runtime action in
/// tailscaled's own state — the image cannot bake it, and re-running it
/// is the convergence. Idempotent by construction: the mapping it sets
/// is the mapping this unit wants, every boot.
pub(crate) const NOSTR_SERVE_SCRIPT: &str = r#"#!/usr/bin/bash
set -euo pipefail
# The relay is loopback-only by its own bind; this is what puts it on
# the tailnet, TLS and all, under the machine's ts.net name.
exec tailscale serve --bg 127.0.0.1:7777
"#;

/// The serve unit: a system oneshot, because `tailscale serve` speaks
/// to the system daemon's LocalAPI and the mapping is the node's.
/// Ships only when the declaration runs tailscaled — absent tailscale
/// the relay is local-only and nothing is refused.
pub(crate) const NOSTR_SERVE_SERVICE: &str = r#"[Unit]
Description=Expose the local nostr relay to the tailnet
Wants=tailscaled.service
After=tailscaled.service

[Service]
Type=oneshot
ExecStart=/usr/libexec/kuma-nostr-serve

[Install]
WantedBy=multi-user.target
"#;

/// The bind that opens the approval panel, joining the baked binds only
/// when the block renders — a key that opens a panel that does not
/// exist is exactly the dead-key shape kuma rewrites out of stock niri.
pub(crate) const NIRI_NOSTR_BIND: &str = r#"    Mod+Ctrl+N allow-when-locked=true hotkey-overlay-title="Nostr approvals" { spawn "kuma-shell" "msg" "nostr"; }"#;

/// The scheme handler: a nostrconnect:// link clicked anywhere lands in
/// the approval panel rather than in nothing. The desktop file is the
/// handler; the mimeapps line names it. The verb carries the URI when
/// the browser passes one — the offer lands even with the panel already
/// open — and a bare `nostr` (the app-grid entry clicked with no URI)
/// opens the panel. Neither spelling toggles: a clicked link while the
/// panel is open must land the offer, not close the panel over it. And
/// NoDisplay is deliberately absent: a flatpak browser's scheme clicks
/// go through the portal, whose chooser filters NoDisplay apps to
/// nothing — a hidden handler is a handler no browser can reach. The
/// cost is one honest entry in the app grid; the alternative is the
/// feature not working at all.
pub(crate) const NOSTR_PANEL_DESKTOP: &str = r#"[Desktop Entry]
Type=Application
Name=Nostr Signer
Exec=/usr/bin/kuma-shell msg nostr %u
MimeType=x-scheme-handler/nostrconnect;
"#;

pub(super) static BLOCKS: &[Block] = &[
    Block { name: "header", emit: header, units: &[] },
    Block {
        name: "desktop-niri",
        emit: desktop_niri,
        units: &[
            // The live session is a person at a desktop, and this is its
            // shell: a live boot with no bar and no lock screen is a
            // broken one. Never masked, on purpose — it just was never
            // accounted for anywhere, because the compound --global line
            // that enables it was invisible to the old test's parser.
            ("kuma-shell.service", Live::Runs("the live session is a person at a desktop, and this is its shell")),
            // Guards an unlocked session at suspend, which a live session
            // can be exactly as much as an installed one; the hibernate
            // half is inert with no swapfile to write.
            ("kuma-sleep-guard.service", Live::Runs("it guards an unlocked live session at suspend the same way; its hibernate half is inert with no swapfile")),
        ],
    },
    Block { name: "desktop-cosmic", emit: desktop_cosmic, units: &[] },
    Block { name: "flatpak-remote", emit: flatpak_remote, units: &[] },
    Block {
        name: "flatpak-sync",
        emit: flatpak_sync,
        units: &[
            ("kuma-flatpak-sync.service", MASKED_CONVERGES),
            ("kuma-flatpak-sync.timer", MASKED_CONVERGES),
            // Joined the image in 0.13 and was never added to the old
            // mask list, which the accountability test found the first
            // time it ran.
            ("kuma-flatpak-overrides.service", Live::Masked("it converges flatpak permissions, and a live session converges nothing")),
        ],
    },
    Block {
        name: "brew",
        emit: brew,
        units: &[
            ("kuma-brew-setup.service", Live::Conditioned("runs only where brew is not yet installed: a negated ConditionPathExists in the unit text")),
            ("kuma-brew-sync.service", MASKED_CONVERGES),
            ("kuma-brew-sync.timer", MASKED_CONVERGES),
        ],
    },
    Block { name: "rpm", emit: rpm, units: &[] },
    Block { name: "sshd", emit: sshd, units: &[] },
    Block { name: "services", emit: services, units: &[] },
    Block { name: "fuse", emit: fuse, units: &[] },
    Block { name: "greenboot", emit: greenboot, units: &[] },
    Block { name: "plymouth", emit: plymouth, units: &[] },
    Block { name: "greeter-seam", emit: greeter_seam, units: &[] },
    Block {
        name: "boot-health",
        emit: boot_health,
        units: &[
            ("greenboot-healthcheck.service", Live::Masked("a live boot from squashfs has no health to grade and no fallback slot to fall back to")),
            ("greenboot-set-rollback-trigger.service", Live::Masked("live media has no rollback slot to arm")),
            ("greenboot-success.target", Live::Masked("live media has no deployment to mark healthy")),
            ("kuma-boot-health-sync.service", Live::Conditioned("waits for an ostree boot: ConditionPathExists=/run/ostree-booted")),
            ("kuma-fstab-sync.service", Live::Conditioned("waits for an ostree boot: ConditionPathExists=/run/ostree-booted")),
            ("kuma-boot-titles.service", Live::Conditioned("waits for an ostree boot: ConditionPathExists=/run/ostree-booted")),
            ("kuma-swap-label.service", Live::Conditioned("waits for the swapfile it labels: ConditionPathExists=/var/swap/swapfile")),
        ],
    },
    Block { name: "signature", emit: signature, units: &[] },
    Block { name: "fwupd", emit: fwupd, units: &[] },
    Block {
        name: "snapshots",
        emit: snapshots,
        units: &[(
            "kuma-snapshot.timer",
            Live::Masked("nothing in a live session should be armed on a schedule"),
        )],
    },
    Block {
        name: "backup",
        emit: backup,
        units: &[
            ("kuma-backup.timer", Live::Masked("nothing in a live session should be armed on a schedule")),
            ("kuma-restore.service", Live::Conditioned("waits for a restore request nobody writes on media: ConditionPathExists=/var/lib/kuma/restore-request")),
        ],
    },
    Block {
        name: "nostr",
        emit: nostr,
        units: &[
            ("kuma-nostrd.service", Live::Runs("a live session is a person at a desktop, and the bunker answers its socket either way; its vault is as ephemeral as the session")),
            ("nip46-relay.service", Live::Runs("the local relay is loopback furniture of the same session")),
            ("kuma-nostr-serve.service", Live::Masked("live media's tailscaled is ephemeral, and a serve mapping nothing can reach twice is noise")),
        ],
    },
    Block {
        name: "vm-timezone",
        emit: vm_timezone,
        units: &[(
            "kuma-vm-timezone.service",
            Live::Runs("adopts the host's timezone through qemu fw_cfg, which a live session inside `kuma vm` wants, and exits 0 immediately when that channel is absent"),
        )],
    },
    Block {
        name: "timezone",
        emit: timezone,
        units: &[],
    },
    Block {
        name: "home-subvol",
        emit: home_subvol,
        units: &[(
            "kuma-home-subvol.service",
            Live::Conditioned("waits for an ostree boot: ConditionPathExists=/run/ostree-booted"),
        )],
    },
    Block {
        name: "user",
        emit: user,
        units: &[("kuma-user-sync.service", MASKED_CONVERGES)],
    },
    Block { name: "system-shell", emit: system_shell, units: &[] },
    Block { name: "hostname", emit: hostname, units: &[] },
    Block { name: "locale", emit: locale, units: &[] },
    Block { name: "ca-anchors", emit: ca_anchors, units: &[] },
    Block { name: "branding", emit: branding_span, units: &[] },
    Block { name: "kuma-binary", emit: kuma_binary, units: &[] },
    Block { name: "declaration", emit: declaration, units: &[] },
    Block { name: "labels", emit: labels, units: &[] },
    Block { name: "sweep-lint", emit: sweep_lint, units: &[] },
];

/// The bytes one block contributed to the real output for this
/// declaration — the per-feature test surface, so a feature's tests
/// assert on what it emits rather than grepping the whole file.
///
/// A block's COPY lines for files another block stages appear here
/// (they are its text); the enable RUNs are per-span in this carving,
/// so a single-block walk is the block's true contribution.
#[cfg(test)]
pub(super) fn emitted(config: &Config, name: &str) -> String {
    for block in BLOCKS {
        if block.name == name {
            let mut e = Emitter::new(config);
            e.enter_block(block);
            (block.emit)(&mut e);
            return e.finish().text;
        }
    }
    panic!(
        "no block named {name}; BLOCKS has {:?}",
        BLOCKS.iter().map(|b| b.name).collect::<Vec<_>>()
    );
}

/// One feature-block: a gated span of the Containerfile plus the files
/// it stages, behind one gate, in one place, and the units it enables
/// with what each does on installer media.
pub(super) struct Block {
    /// The row's address: the test surface and the invariant panics
    /// name blocks by it. A walk with nothing to complain about never
    /// reads it, which is the walk working.
    #[allow(dead_code)]
    pub(super) name: &'static str,
    pub(super) emit: fn(&mut Emitter<'_>),
    pub(super) units: Units,
}
