# Local changes to preserve

Upstream: `https://github.com/irori-app/irori.git`, branch `main`.
Initial record covers the ten local commits after
`60058b9da144b413cc7d0157bd5796925625d5a7`, through
`647e5b13a6c51826f6359bdce79e328fe32f46a4`.

Preserve the behaviours below when replaying upstream. An upstream implementation
may replace a local implementation when it preserves the behaviour and passes its
checks. Overlapping paths alone are not grounds to skip a change. A proposed
behaviour change these entries do not settle needs a decision.

The checks below are for replays affecting each entry; they are not a record of
tests already run. Use isolated test data and stand-in servers, never real store
accounts or the user's library. Keep the corresponding documentation and all
19 translation catalogs with the behaviour they describe.

## RomM sign-in and offline library

Users can connect their own RomM 5.0+ server, approve Irori in their browser and
browse the supported ROM library offline. Device identity survives sign-ins;
credentials use Irori's secrets storage. Downloaded games remain listed and
playable after sign-out or removal from the server.

- Paths: `crates/romm/`, `crates/store-romm/`, `crates/feature-manage/src/login/`, `crates/feature-manage/src/accounts.rs`, `crates/feature-manage/src/stores.rs`, `crates/feature-manage/locales/`, `crates/irori/src/stores.rs`, `Cargo.toml`, `Cargo.lock`.
- Check: `cargo test --locked -p irori-romm-api -p irori-romm`; `xvfb-run -a cargo test --locked -p irori-manage romm_signs_in_once_approved_in_the_browser` on Linux.
- Upstream changes: port authentication, API and library fixes while retaining self-hosted server addresses, reverse-proxy prefixes, offline access and downloaded games after sign-out. Do not replace browser approval with a password or a bundled server.
- Local commits: `a9f182d2c610f17664d1228c0c51bc1a31eb6bc1`, `565ca4a208dbc1b0e6d5db7f4796a4289beaab26`.

## RomM downloads for every supported system

RomM lists platforms Irori emulates and downloads every game file in its server
folder layout, preserving disc companions and nested folders. The launch selects
a playlist before individual discs, the appropriate console program, a DOS
program or the ScummVM folder. Manuals, patches and unrelated extras stay on the
server. Downloads resume, obey speed limits and verify available hashes; updates
follow file revisions and respect pinned versions. Earlier single-file install
records keep reading without a spurious update.

- Paths: `crates/store-romm/`, `crates/romm/src/model.rs`, `crates/engines/src/systems.rs`, `crates/store-emulation/src/scan.rs`.
- Check: `cargo test --locked -p irori-romm -p irori-romm-api -p irori-engines -p irori-emulation`.
- Upstream changes: port download, import, update and file-selection fixes without narrowing platform coverage, losing companion files or breaking saved installs. Keep the per-system updates and DLC exception below.
- Local commits: `a9f182d2c610f17664d1228c0c51bc1a31eb6bc1`, `565ca4a208dbc1b0e6d5db7f4796a4289beaab26`.

## ROM launch behaviour across stores

RomM games and locally scanned ROMs use the same emulator selection and launch
behaviour: BIOS checks, game and system choices, version pins, missing RetroArch
cores and launch notices. Both are treated as ROMs, so PC-only Wine, prefixes,
sandboxing, mods, save backups, compatibility lookups and automatic PC copy
matching do not apply to them.

- Paths: `crates/core/src/runner.rs`, `crates/core/src/settings.rs`, `crates/engines/src/play.rs`, `crates/store-emulation/src/lib.rs`, `crates/store-romm/src/lib.rs`, `crates/runtime/src/`, `crates/services/src/copies.rs`, `crates/services/src/discord.rs`, `crates/services/src/mods.rs`, `crates/extras/src/`, `crates/feature-game/src/page/`, `crates/ui/src/save_backups.rs`, `crates/irori/src/export.rs`.
- Check: `cargo test --locked -p irori-core -p irori-engines -p irori-emulation -p irori-romm -p irori-runtime -p irori-services -p irori-extras`; `xvfb-run -a cargo test --locked -p irori-ui -p irori-game -p irori` on Linux, leaving ignored tests disabled.
- Upstream changes: port shared emulator fixes for both stores; retain the `romm` runner identity and ROM-specific treatment when upstream adds or changes runner-dependent behaviour.
- Local commit: `a9f182d2c610f17664d1228c0c51bc1a31eb6bc1`.

## Big Picture system filter

The Library tab offers a controller- and mouse-accessible System chip when games
have systems. It cycles through the systems present in catalog order and back
to Any system. Systems visible only in PIN-locked hidden games are not offered,
and a selected system resets when it disappears.

- Paths: `crates/feature-bigpicture/src/model.rs`, `crates/feature-bigpicture/src/view/`, `crates/feature-bigpicture/locales/`.
- Check: `cargo test --locked -p irori-bigpicture the_system_chip_offers_the_library_systems_in_catalog_order`; `xvfb-run -a cargo test --locked -p irori-bigpicture` on Linux. Manually cycle System with a controller and mouse in a library containing ROMs, then lock its hidden games and check the choices.
- Upstream changes: retain filtering, dynamic chip navigation and hidden-game privacy while taking Big Picture fixes or layout changes.
- Local commit: `dd78e927cd446a090526afd74f999744012c670d`.

## Nintendo Switch through Eden

Switch ROMs are discovered locally and through RomM and play with experimental
Eden builds from the project's own release server. Users can also use detected
installs. Lutris imports map yuzu and Eden to Eden and Cemu to the Wii U engine.

- Paths: `assets/engines/eden.json`, `assets/engines/systems.json`, `crates/engines/src/lib.rs`, `crates/engines/src/manifest.rs`, `crates/engines/src/releases.rs`, `crates/engines/src/launch.rs`, `crates/store-romm/src/lib.rs`, `crates/extras/src/lutris_import/`, `docs/design/engines.md`, `website/src/`.
- Check: `cargo test --locked -p irori-engines -p irori-romm`; `cargo test --locked -p irori-extras maps_emulators_to_systems_and_engines`.
- Upstream changes: port emulator and release-discovery fixes while retaining Switch, official Eden release downloads, supported hosts and import mappings. An equivalent source implementation may replace the local Forgejo handling.
- Local commit: `2d693f29fb5d83c865bbc427f08d4e103a2843c9`.

## BIOS and firmware from the user's RomM server

After an install and before the first launch of a system each run, Irori fetches
only missing BIOS or firmware from the user's RomM server into the BIOS folder.
Known digests take priority, otherwise missing files may match by name; the
newest matching firmware archive is selected. Existing user files stay intact.
Firmware failures do not fail the install or launch; missing permission raises
one actionable notice without signing the account out.

Eden receives keys and unpacks firmware before launch into its correct native,
Flatpak or portable data folder. Only firmware previously installed by Irori is
replaced; the user's own firmware stays intact.

- Paths: `crates/engines/src/bios.rs`, `crates/engines/src/firmware.rs`, `crates/engines/src/launch.rs`, `crates/engines/src/manifest.rs`, `crates/romm/`, `crates/store-romm/src/firmware.rs`, `crates/store-romm/src/install.rs`, `crates/store-romm/src/lib.rs`, `crates/irori/locales/`, `assets/engines/eden.json`, `assets/engines/systems.json`.
- Check: `cargo test --locked -p irori-romm-api -p irori-romm -p irori-engines`, including permission refusal, missing BIOS selection and Eden firmware tests.
- Upstream changes: port fixes without adding public BIOS or ROM download sources, replacing user firmware or turning optional firmware access into an account or game failure. Keep old tokens usable when they lack `firmware.read`.
- Local commit: `4e4b0dd49d9a68c996cbe43130fa8c76849038b8`.

## RomM updates and DLC through Eden

Switch downloads include RomM's update and DLC categories alongside the game.
The emulator opens the base game and reads the content folder for that launch.
Only the previous folder Irori added is removed from Eden's configuration;
user-listed folders, unrelated settings and line endings are preserved. Importing
a base game does not require optional update or DLC files.

- Paths: `assets/engines/eden.json`, `assets/engines/systems.json`, `crates/engines/src/content.rs`, `crates/engines/src/launch.rs`, `crates/engines/src/manifest.rs`, `crates/engines/src/systems.rs`, `crates/romm/src/model.rs`, `crates/store-romm/`, `crates/store-emulation/src/lib.rs`.
- Check: `cargo test --locked -p irori-engines -p irori-romm-api -p irori-romm -p irori-emulation`.
- Upstream changes: port content and configuration fixes while retaining download/update tracking, base-game selection and ownership of only Irori's added folder. Do not start downloading unrelated RomM extras.
- Local commit: `76b682da670ed9cfc250b81836943762e3448e2f`.

## Emulator logos on ROM covers

ROM covers at least 7 rem wide show the chosen emulator's logo when its manifest
provides one. The logo follows the game's choice or system default, updates with
library and settings changes, and loads through the existing art store. Smaller
covers and non-ROM games do not gain a logo.

- Paths: `assets/engines/`, `crates/engines/src/manifest.rs`, `crates/services/src/engine_icons.rs`, `crates/services/src/tests.rs`, `crates/ui/src/models.rs`, `crates/ui/src/components/cover.rs`.
- Check: `cargo test --locked -p irori-services rom_covers_show_the_logo_of_the_emulator_that_plays_them`. With ROM artwork loaded, inspect large covers in light and dark themes, change a game's emulator and check the logo changes; check compact rows remain clear.
- Upstream changes: port art and cover fixes while preserving correct emulator selection, off-thread lookup and optional, fetched project logos. Keep the logo clear of generated cover titles.
- Local commit: `04feea708785a452345be593c8c163a524be1018`.

## Dreamcast, Neo Geo CD and PlayStation Vita

Local ROM discovery and RomM include Dreamcast, Neo Geo CD and Vita. RetroArch's
Flycast and NeoCD cores play the first two; Vita3K is available as an experimental
engine for Vita. Lutris imports recognize the two new RetroArch cores.

- Paths: `assets/engines/retroarch.json`, `assets/engines/vita3k.json`, `assets/engines/systems.json`, `crates/engines/src/lib.rs`, `crates/store-romm/src/`, `crates/extras/src/lutris_import/`, `docs/design/engines.md`, `website/src/`.
- Check: `cargo test --locked -p irori-engines -p irori-romm`; `cargo test --locked -p irori-extras maps_emulators_to_systems_and_engines`.
- Upstream changes: port system, core and emulator fixes while retaining discovery, RomM slug mappings, import support and experimental status for Vita3K.
- Local commit: `6129d32b190d3a4259ae4db1630bcd4cd5de550a`.

## Repository agent skills

The repository carries coding, Rust and fork-maintenance skills for its agent
tools, including reference material, evaluation fixtures and the replay helper's
tests. The project skill configuration keeps those tools available to maintainers.

- Paths: `.agents/skills/`, `.claude/skills/`, `orqen.yaml`.
- Check: `bun test ./.agents/skills/maintain-github-fork/scripts/upstream.test.ts`; confirm the skill files and configured skill names remain available for each agent tool.
- Upstream changes: preserve local skills and their supporting files when taking repository tooling changes; reconcile a replacement skill's behaviour rather than deleting local tooling because upstream lacks it.
- Local commit: `a33a92a122030ae1c6fcb5505d6fe4cc73cc9595`.

## Headless screen snapshots

Maintainers can render real application routes to PNG without a display using
an isolated in-memory backend. The ignored snapshot test accepts route, theme,
language, size and output-folder settings and keeps normal application startup
and tests unchanged. It uses GPUI's offscreen renderer without introducing a
second application binary.

- Paths: `crates/irori/src/snapshot.rs`, `crates/irori/src/main.rs`, `crates/irori/Cargo.toml`, `Cargo.lock`, `.agents/skills/irori-screenshots/`, `.gitignore`, `AGENTS.md`.
- Check: `IRORI_SNAPSHOT_OUT=/tmp/irori-fork-snapshots IRORI_SNAPSHOT_LINKS=irori://library,irori://game/gog/3 IRORI_SNAPSHOT_THEMES=dark,light IRORI_SNAPSHOT_LANGS=en,de cargo test --locked -p irori snapshot -- --ignored --nocapture`; inspect the resulting PNGs. This fixture does not exercise RomM, emulator logos or controller filtering.
- Upstream changes: adapt the harness to shell, routing, GPUI and dependency changes while retaining isolated headless rendering and its environment-variable interface. Keep it test-only and preserve the screenshot skill and instructions.
- Local commit: `647e5b13a6c51826f6359bdce79e328fe32f46a4`.
