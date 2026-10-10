# What a desktop contains

Choosing a desktop installs a set of packages you did not name. This page
lists both sets and explains the choices that are not obvious.

For the exact list any declaration produces, run `kuma generate` and read
the `dnf install` line. That is the authority; this page explains it.

## Three layers, two owners

An image is built in layers, and they are not all curated by the same
hand:

1. **Fedora's minimal bootc core.** Kernel, systemd, bootc, dnf. Fedora
   maintains it; kuma includes it unmodified.
2. **kuma's base.** Networking, firmware, and the handful of things any
   real machine needs: `shadow-utils`, `sudo`, `chrony`, `openssh-server`,
   `authselect`, `passwd`, `cryptsetup`, `fwupd`. See
   [where the base system comes from](concepts.md#where-the-base-system-comes-from).
3. **The desktop set.** Everything below.

## The line between a desktop and your declaration

A curated desktop holds session infrastructure; applications belong in
`packages.flatpak`. The reason is reversibility. Delete a line from your
declaration and the next convergence uninstalls it. A package in a
desktop set has no opt-out. The full argument is in [why a desktop
installs things you did not name](concepts.md#why-a-desktop-installs-things-you-did-not-name).

## niri

Hand-assembled, because niri is a window manager rather than a desktop.
It requires nothing beyond itself, so every part of a working session is
named explicitly. Most of that session is now one program: kuma-shell,
built from the kumaui tree and shipped in the image, draws the bar,
notifications, wallpaper, lock screen, control centre and Nostr Signer.
Before that, kuma assembled seven separate tools that agreed on colour
and nothing else.

| | |
|---|---|
| Session | `niri`, `xwayland-satellite`, `greetd`, `kuma-greeter` (tuigreet kept as the comment-swapped fallback) |
| Shell | `kuma-shell` (bar, notifications, wallpaper, idle, lock, control centre, Nostr Signer) |
| Terminal and files | `kuma-files` (Koguma, the file manager), `file-roller`, `gvfs` (+ fuse, mtp, smb), `udiskie`, `7zip`, `unar` — the terminal is `kuma-term`, baked with the image, not packaged |
| Portals | `xdg-desktop-portal-gtk`, `xdg-desktop-portal-gnome` |
| Audio | `pipewire`, `pipewire-pulseaudio`, `wireplumber`, `pavucontrol` |
| Graphics | `mesa-dri-drivers`, `mesa-vulkan-drivers`, `vulkan-loader` |
| Hardware | `NetworkManager-wifi`, `NetworkManager-tui`, `wpa_supplicant`, `bluez`, `blueman`, `brightnessctl`, `power-profiles-daemon` |
| Printing and discovery | `cups`, `system-config-printer`, `avahi`, `nss-mdns` |
| Screen and clipboard | `grim`, `slurp`, `swappy`, `wf-recorder`, `wl-clipboard` |
| Session glue | `polkit`, `mate-polkit`, `dconf`, `gnome-keyring`, `xsettingsd`, `xdg-user-dirs`, `firewalld`, `flatpak`, `desktop-file-utils` |
| Fonts and icons | sans, mono, emoji, CJK, Font Awesome (free and brands), `adwaita-icon-theme`, `adw-gtk3-theme` |

The control centre owns the everyday cases: bluetooth, audio, brightness.
The power button in its header offers lock, log out, suspend, reboot and
shut down. The separate settings tools stay for what it does not reach:
`nm-connection-editor` for wifi, a VPN or a static route; `pavucontrol`
for per-application audio routing; `system-config-printer` for printers.
All of that is machine state, not system definition: the declaration
describes what a machine is, and picking a network is not that.

**The shell is supervised, because everything that locks runs through
it.** Idle lock, `Super+Alt+L`, and locking before suspend are all the
shell's job. So it runs as a systemd user service that restarts if it
stops, not as a one-shot spawn that could vanish quietly. `kuma doctor`
checks that it is running and that its idle watcher is alive.

The idle defaults are: lock at 15 minutes, screens off a minute later,
lock before sleep. Change them in the settings panel or in
`~/.config/kuma-shell/config.toml` under `[idle]` (a `0` disables a
clause). `kuma doctor` reads the same file, so it reports your actual
timeouts, and treats a deliberately disabled lock as a choice rather
than a failure. The one failure it cannot repair is a compositor without
the idle protocol or a dead Wayland connection; the journal says so, and
the doctor reports exactly what the journal says.

One guard worth knowing: if the shell is not running when the machine
goes to sleep, the session ends instead of suspending. A session with no
shell has no lock screen, and sleeping into one means an unlocked
machine in a bag.

**The desktop's look comes from the image.** The shell's defaults are
compiled in, and your choices live in `~/.config/kuma-shell/config.toml`.
The shell reads that file; the image never writes it. What kuma bakes is
the default, and what you change is yours.

**The terminal is baked, and the GTK apps follow the image's palette.**
kuma-term ships inside the image beside the shell and Koguma. The
terminal, pavucontrol and the rest match the shell's palette: the image
also ships `adw-gtk3-theme` for GTK3 applications — stock Adwaita
ignores the colour names a palette can set.

GTK4 applications do not follow, which on a kuma machine mostly means
flatpaks. libadwaita ignores a user stylesheet that redefines its
palette, so they keep their own dark theme.

The bar carries state and little else. `Mod+Shift+/` lists every bind
the session has. Two binds a previous shell owned, `Mod+Ctrl+V` for
clipboard history and `Mod+Ctrl+W` for the wallpaper picker, have no
panel yet.

`Mod+D` opens the launcher. Your applications are in it, and so are
kuma's own verbs, the same desktop entries on every desktop kuma builds.
See [kuma in your launcher](concepts.md#kuma-in-your-launcher).

## COSMIC

COSMIC is experimental. It is built on every push like every other
example, so it compiles and its build-time checks run, and it does boot.
What it does not get is niri's verification: the checks that install
kuma to a disk, boot it, and question the running machine run against
niri on every change, and against COSMIC when someone remembers.

The set is much shorter, because COSMIC curates itself. `cosmic-session`
hard-requires the coherent desktop: compositor, panel, applets,
settings, files, terminal, notifications, OSD, screenshot, portal,
fonts. So kuma names the session plus the hardware enablement a desktop
lives on, and little else.

The additions worth knowing: `cosmic-edit`, because the default dock
pins it and the session does not require it, so without it the pin is
dead. `pipewire`, because the session requires the client library but
nothing pulls the daemon. And `udisks2`, because `cosmic-files` mounts
removable media through it directly.

`cosmic-store` is deliberately absent. A store is an application, so
which one a machine gets is the declaration's call.

## Why these are here

Most of a desktop set is unsurprising. These are not, and each one is a
failure someone had to diagnose:

- **`gnome-keyring-pam`** unlocks the login keyring at login. It is a
  separate subpackage that nothing depends on, and the greeter's PAM
  lines skip a missing module in total silence. Without it, login
  succeeds, nothing is logged, and every keyring-using app prompts on
  launch forever.
- **`nss-mdns`** is what makes `.local` names and driverless printer
  discovery actually resolve. `avahi` alone announces without resolving.
- **`zram-generator-defaults`** carries the config that activates zram.
  The base ships the generator without it, so the desktop would have
  zero swap and the OOM killer would take windows under memory pressure.
  Note the limit: zram is swap *in memory*, so it cannot hold a copy of
  memory. Hibernating needs a file on a disk, which is what `kuma
  hibernate` makes.
- **`glibc-langpack-en`** provides real locale data. The base ships
  `glibc-minimal-langpack`, so `en_US.UTF-8` fails to resolve and
  anything formatting a date or number falls back to C.
- **`mesa-vulkan-drivers`** and **`vulkan-loader`**: OpenGL drivers
  alone strand every Vulkan application on software rendering.
- **`fontawesome-fonts-all`** pulls both Font Awesome faces. The shell
  bundles its own icon font, but flatpaks and GTK applications still
  reach for these glyphs and render empty boxes without them. It is
  named instead of the two faces directly because their package names
  carry a major version that changes under you; this one does not.
- **`google-noto-sans-cjk-vf-fonts`**, because the default sans is
  latin-only and CJK pages render as empty boxes.
- **the color emoji face is vendored, not packaged.** Fedora's noto-emoji
  packaging switched to the COLRv1 format in April 2025, which the GPUI
  renderers (shell, greeter, Koguma, kuma-term) cannot rasterize — every
  emoji drew as an empty box. The image bakes the bitmap (CBDT) build,
  pinned in `assets/noto-emoji/`, beside the packaged faces, which remain
  for every renderer that is not GPUI. See `assets/CREDITS.md`.
- **`avahi`** is named rather than assumed. It used to arrive with
  fedora-bootc by luck, and kuma's composed base does not carry it.

## What you can change

**You can add.** `packages.rpm` layers anything from Fedora's repos on
top of the desktop set.

**You cannot subtract.** There is no `rpm_exclude` and no per-desktop
opt-out, so a package in a desktop set is in your image. If you want a
desktop without blueman, the only route today is a fork.

That is a real limit rather than an oversight, and it is why the
boundary is drawn where it is: everything kuma cannot let you remove is
something a session needs in order to work, and everything else is left
to your declaration, where deleting a line is enough.
