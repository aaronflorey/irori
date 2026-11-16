---
name: irori-screenshots
description: Render Irori screens to PNG without a display or a running app, in dark or light and in any language, then look at them. Use after changing UI, layout, copy or a theme, when asked to see, check or screenshot a screen, and before saying a UI change works.
version: 1.0.0
source: local
license: MIT
---

# Irori screenshots

Render the real window headlessly and read the PNGs. This is how a coding agent
sees UI; do not describe a screen you did not render.

## Run it

From the repository root:

```sh
IRORI_SNAPSHOT_OUT=<dir> \
IRORI_SNAPSHOT_LINKS=irori://library,irori://settings/wine,irori://game/gog/3 \
IRORI_SNAPSHOT_THEMES=dark,light \
IRORI_SNAPSHOT_LANGS=en,de \
cargo test -p irori snapshot -- --ignored --nocapture
```

It prints one path per PNG, named `<link>-<theme>-<lang>.png`. Open each with
Read. Use a scratch folder for `IRORI_SNAPSHOT_OUT`, never the repository.

| Variable | Takes | Default |
| --- | --- | --- |
| `IRORI_SNAPSHOT_LINKS` | comma-separated `irori://` links | `irori://library` |
| `IRORI_SNAPSHOT_THEMES` | `dark`, `light` | `dark` |
| `IRORI_SNAPSHOT_LANGS` | language codes (`en`, `de`, `ru`, `ja`, `pt_BR`, …) | `en` |
| `IRORI_SNAPSHOT_SIZE` | `WIDTHxHEIGHT`, logical pixels (PNGs are 2x) | `1280x800` |
| `IRORI_SNAPSHOT_OUT` | output folder | `irori-snapshots` |

Links are the ones Irori opens (AGENTS.md, "Commands"): `irori://library`,
`downloads`, `updates`, `accounts`, `stores`, `stats`, `wine`, `emulators`,
`bigpicture`, `settings/<section>`, `stats/year`, and `irori://game/<runner>/<app>`.
The fixture games are GOG apps `1`–`12`; `irori://game/gog/3` is Hades (installed,
files missing). The first build takes a few minutes; later runs take seconds.

## What to look at

- Render **both themes** for anything you changed; the PR template asks for light
  and dark.
- For text or layout changes also render `de` and `ru` (30–40% longer) and a narrow
  size such as `900x600`. Look for clipped, overlapping or wrapped controls.
- Compare against the screen without your change when the question is "did this
  move": stash or check out the base, render, then render again.
- The images are 2x: multiply displayed coordinates by the scale the viewer reports.

## What it does not show

- The data is a fixed set of 12 games in one store (`MemoryStore` in
  `crates/irori/src/snapshot.rs`), with no cover art (placeholders), no playtime,
  no downloads, no accounts, no updates. A screen that depends on other data needs
  that data added to the fixture first.
- Only whole routes. Dialogs, sheets, menus and hover or focus states are not
  reachable yet.
- Only verified on Linux. Software Vulkan (lavapipe, `ls /usr/share/vulkan/icd.d`)
  is enough; no display, compositor or GPU is needed. Do not claim anything about
  Wayland decorations, translucency or the shadow margin from these images.

## Extending the fixture

Edit `snapshot.rs`: add games to `MemoryStore::new` (keep the count check in
`snapshot()` in step) or seed backend state before the window opens. Theme and
language must be set through `backend.settings.update_app`, never by calling
`theme::apply` alone: the shell re-applies the stored preferences whenever the
window's appearance changes and would overwrite it. Keep the harness a test; do not
add a binary or a feature flag for it.

## Reporting

Say which links, themes, languages and size you rendered, what you saw, and what
you could not render. Do not call a change checked on macOS or Windows from these
images.
