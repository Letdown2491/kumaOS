# Asset credits

Kuma's code is MIT (see `LICENSE`). The artwork here is not kuma's work and
carries its own terms, recorded so nobody has to guess.

## kuma-wallpaper.jpg

Photograph by **Marek Piwnicki** ([@marekpiwnicki](https://unsplash.com/@marekpiwnicki)),
misty mountain peaks at sunrise, taken in Italy.

- Source: <https://unsplash.com/photos/A1IoRfRQHuk>
- License: [Unsplash License](https://unsplash.com/license), which grants an
  irrevocable, worldwide right to use, copy, modify, and distribute the image
  commercially without permission or attribution. The credit above is given
  because it is deserved, not because the license demands it.
- Modified from the 5423x3050 original: cropped and resized to 2560x1440 and
  re-encoded as JPEG at quality 90 with 4:4:4 chroma, which keeps the sky
  gradient free of banding.

The license forbids two things, neither of which kuma does: selling the image
unmodified, and compiling Unsplash images into a competing image service.

## spinner_alt/ (plymouth theme)

Theme by **Aditya Shakya** ([@adi1090x](https://github.com/adi1090x)), from
the [plymouth-themes](https://github.com/adi1090x/plymouth-themes)
collection (`pack_4/spinner_alt`): a 60-frame grayscale boot spinner.

- Source: <https://github.com/adi1090x/plymouth-themes/tree/master/pack_4/spinner_alt>
- Vendored at upstream commit `5d8817458d764bff4ff9daae94cf1bbaabf16ede`
  (master, 2026-08-26). One file modified from that commit:
  `spinner_alt.script` renders the system-provided password prompt text
  ("Please enter passphrase for disk ...", naming the actual disk)
  where upstream discarded it and hardcoded "Enter Password". Diff
  future updates against that commit and re-apply this edit.
- License: [GPL-3.0](https://www.gnu.org/licenses/gpl-3.0.html), whose full
  text ships inside the theme directory (`spinner_alt/LICENSE`). kuma is
  MIT and links nothing of the theme's code: the files are data copied
  into images it builds, which keeps the two licenses side by side rather
  than combined.
- The directory ships into every image kuma builds at
  `/usr/share/plymouth/themes/spinner_alt/` and is set as plymouth's
  default theme, so it draws the early-boot splash and the LUKS unlock
  prompt on encrypted machines.

## The rest

`kitty.conf` is configuration written for kuma and carries the project's MIT
license.
