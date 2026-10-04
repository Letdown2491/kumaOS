# What a desktop contains

Setting `desktop = "niri"` or `desktop = "cosmic"` installs a set of packages
you did not name. This page says what they are and why.

A desktop is more than the thing you look at: something has to draw windows,
show a login screen, play sound, find printers, ask for your password when an
application needs root, and supply fonts. Naming a desktop is how you get all
of that in one word.

For the exact list any declaration produces, run `kuma generate` and read the
`dnf install` line. That is the authority; this page explains it.

## Three layers, two owners

An image is built in layers, and they are not all curated by the same hand:

1. **Fedora's minimal bootc core.** Kernel, systemd, bootc, dnf. Fedora
   maintains it; kuma includes it unmodified.
2. **kuma's base.** Networking, firmware, and the handful of things any real
   machine needs: `shadow-utils`, `sudo`, `chrony`, `openssh-server`,
   `passwd`, `cryptsetup`, `fwupd`. See
   [where the base system comes from](concepts.md#where-the-base-system-comes-from).
3. **The desktop set.** Everything below.

## The line between a desktop and your declaration

A curated desktop holds session infrastructure; applications belong in
`packages.flatpak`. The reason is reversibility rather than taste, and it is
argued once in
[why a desktop installs things you did not name](concepts.md#why-a-desktop-installs-things-you-did-not-name)
rather than twice here. What that rule costs in practice is
[what you can change](#what-you-can-change), below.

## niri

Hand-assembled, because niri is a window manager rather than a desktop: it
requires nothing beyond itself, so every part of a working session is named
explicitly. Most of that session is now one program. kuma-shell — built from
the kumaui tree and shipped in the image — draws the bar, notifications,
wallpaper, the lock screen, a control centre and the Nostr Signer, where kuma
previously assembled seven separate tools that agreed on colour and on nothing
else.

| | |
|---|---|
| Session | `niri`, `xwayland-satellite`, `greetd`, `kuma-greeter` (tuigreet kept as the comment-swapped fallback) |
| Shell | `kuma-shell` (bar, notifications, wallpaper, idle, lock, control centre, Nostr Signer) |
| Terminal and files | `kitty`, `thunar` (+ archive plugin), `file-roller`, `gvfs`, `udiskie`, `7zip`, `unar` |
| Portals | `xdg-desktop-portal-gtk`, `xdg-desktop-portal-gnome` |
| Audio | `pipewire`, `pipewire-pulseaudio`, `wireplumber`, `pavucontrol` |
| Graphics | `mesa-dri-drivers`, `mesa-vulkan-drivers`, `vulkan-loader` |
| Hardware | `NetworkManager-wifi`, `NetworkManager-tui`, `wpa_supplicant`, `bluez`, `blueman`, `brightnessctl`, `power-profiles-daemon` |
| Printing and discovery | `cups`, `system-config-printer`, `avahi`, `nss-mdns` |
| Screen and clipboard | `grim`, `slurp`, `swappy`, `wf-recorder`, `wl-clipboard` |
| Session glue | `polkit`, `mate-polkit`, `dconf`, `gnome-keyring`, `xsettingsd`, `xdg-user-dirs`, `firewalld`, `flatpak`, `desktop-file-utils` |
| Fonts and icons | sans, mono, emoji, CJK, Font Awesome (free and brands), `adwaita-icon-theme`, `adw-gtk3-theme` |

The control centre owns the everyday cases: bluetooth, audio, brightness,
and the power button in its header opens lock, log
out, suspend, reboot and shut down. The separate settings tools stay for what
it does not reach: `nm-connection-editor` for wifi, a VPN or a static route,
`pavucontrol` for per-application routing, `system-config-printer` for
printers. All of it is machine state rather than system definition: the declaration describes what a
machine is, and picking a network is not that.

**The shell is supervised, because everything that locks runs through it.**
Idle lock, `Super+Alt+L` and locking before suspend are all the shell's, so it
runs as a systemd user service that restarts if it stops rather than as a
one-shot spawn that could vanish quietly. `kuma doctor` grades that it is
running and that its idle watcher is alive. The idle contract — lock at 15
minutes, screens off a minute later, lock before sleep — is compiled into the
shell rather than baked as a config file, so there is nothing a broken
override can silently disagree with; the one failure mode left (a compositor
without the idle protocol, a dead Wayland connection) is what the journal
says, and the doctor grades exactly that. If the shell is not there at all
when the machine is asked to
sleep, the session ends instead of suspending: a session with no shell has no
lock screen, and sleeping into one means an unlocked machine in a bag. The
guard checks the process by name on the way into sleep; noctalia's session-bus
probe is gone with it, because kuma-shell owns no bus name and the guard will
not guess.

**The desktop's own look comes from the image.** The shell's defaults are
compiled in, and the machine's own choices live in
`~/.config/kuma-shell/config.toml`, which the shell reads and the image never
writes: what kuma bakes is the default, and what you change is yours. There is
no exporter and no state file for `kuma diff` to miss — the shell that draws
the desktop reads the same file you would edit.

**The terminal follows the image's palette.** kitty's theme is a static
palette in `/etc/xdg/kitty/kitty.conf` — chosen once, shipped with the image —
so the terminal, thunar, pavucontrol and the rest agree with the shell the way
they always did. The image ships `adw-gtk3-theme` for the GTK3 half: stock
Adwaita GTK3 ignores the colour names a palette can set.

GTK4 applications do not follow, which on a kuma machine mostly means
flatpaks. libadwaita ignores a user stylesheet that redefines its palette, so
they keep their own dark theme.

The bar carries state and little else. `Mod+Shift+/` lists every bind the
session has; the two binds noctalia's panels owned — `Mod+Ctrl+V` for
clipboard history and `Mod+Ctrl+W` for the wallpaper picker — left with
noctalia, because the shell has no panel for either yet.

`Mod+D` opens the shell's launcher. Your applications are in it, and so are
kuma's own verbs, the same desktop entries on every desktop kuma builds.
See [kuma in your launcher](concepts.md#kuma-in-your-launcher).

## COSMIC

COSMIC is experimental. It is built on every push like every other example, so
it compiles and its build-time checks run, and it does boot. What it does not
get is the verification niri gets: the checks that install kuma to a disk and
then boot it and interrogate the running machine are run against niri on every
change, and against COSMIC when someone remembers. Treat it as the second
desktop in the order it is verified, not only in the order it is listed.

Much shorter, because COSMIC curates itself. `cosmic-session` hard-requires
the coherent desktop (compositor, panel, applets, settings, files, terminal,
notifications, OSD, screenshot, portal, fonts), so kuma names the session plus
the hardware enablement a desktop lives on, and little else.

The additions worth knowing: `cosmic-edit`, because the default dock pins it
and the session does not require it, so without it the pin is dead;
`pipewire`, because the session requires the client library but nothing pulls
the daemon; and `udisks2`, because `cosmic-files` mounts removable media
through it directly.

`cosmic-store` is deliberately absent. A store is an application, so which one
a machine gets is the declaration's call.

## Why these are here

Most of a desktop set is unsurprising. These are not, and each one is a
failure someone had to diagnose:

- **`gnome-keyring-pam`** unlocks the login keyring at login. It is a separate
  subpackage that nothing depends on, and the greeter's PAM lines are `-`
  prefixed, so a missing module is skipped in total silence. Without it, login
  succeeds, nothing is logged, and every keyring-using app prompts on launch
  forever.
- **`nss-mdns`** is what makes `.local` names and driverless printer discovery
  actually resolve. `avahi` alone announces without resolving.
- **`zram-generator-defaults`** carries the config that activates zram. The
  base ships the generator without it, so the desktop would have zero swap and
  the OOM killer would take windows under memory pressure. It is swap *in
  memory*, so it cannot hold a copy of memory: hibernating needs a file on a
  disk, which is what `kuma hibernate` makes.
- **`glibc-langpack-en`** provides real locale data. The base ships
  `glibc-minimal-langpack`, so `en_US.UTF-8` fails to resolve and anything
  formatting a date, a time or a number falls back to C.
- **`mesa-vulkan-drivers`** and **`vulkan-loader`**: OpenGL drivers alone
  strand every Vulkan application on software rendering.
- **`fontawesome-fonts-all`** pulls both Font Awesome faces. The shell bundles
  its own icon font, but flatpaks and GTK applications still reach for these
  glyphs and render tofu without them, and the brands live in a different face
  from the rest. It is named instead of the two faces directly because their
  package names carry the major version, which changes under you; this one
  does not, and it adds no files of its own.
- **`google-noto-sans-cjk-vf-fonts`**, because the default sans is latin-only
  and CJK pages render as tofu.
- **`avahi`** is named rather than assumed. It used to arrive with
  fedora-bootc by luck, and kuma's composed base does not carry it.

## What you can change

**You can add.** `packages.rpm` layers anything from Fedora's repos on top of
the desktop set.

**You cannot subtract.** There is no `rpm_exclude` and no per-desktop opt-out,
so a package in a desktop set is in your image. If you want a desktop without
Thunar, the only route today is a fork.

That is a real limit rather than an oversight, and it is why the boundary above
is drawn where it is: everything kuma cannot let you remove is something a
session needs in order to work, and everything else is left to your
declaration where deleting a line is enough.
