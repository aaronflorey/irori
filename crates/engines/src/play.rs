//! Turning a ROM into the [`LaunchPlan`] services run, for every store whose
//! games are ROMs (the ROM library, RomM).
//!
//! The order: the system's BIOS files (no emulator download helps without
//! them), the emulator (the game's own choice, else the system's default,
//! else the first recommended one installed), a missing RetroArch core
//! installed on the way, the installed version (a pinned one if the game
//! pins it), a converting engine's preparation, then the command.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, bail};
use irori_core::{
    AppSettings, Event, EventBus, LaunchPlan, LaunchRequest, Notice, NoticeLevel, OpContext,
};

use crate::launch::{GameFile, LaunchInput};
use crate::{BiosMissing, EngineKind, EngineMissing, EngineRef, EngineUnavailable, Host, bios};

/// The `/dev/udmabuf` warning is shown once per run.
static UDMABUF_WARNED: AtomicBool = AtomicBool::new(false);

/// The emulator a game of `system` plays with: `chosen` (the game's
/// `iroriEngine`) when set, else the system's default in `app`. Blocking.
pub fn choice_for(app: &AppSettings, system: &str, chosen: &str) -> Option<EngineRef> {
    if let Some(c) = EngineRef::parse(chosen) {
        return Some(c);
    }
    let state = crate::state::load();
    crate::default_choice(
        system,
        app.system_engines.get(system).map(String::as_str),
        |c| crate::plays_now(c, &state),
    )
}

/// The plan that plays `game` with the emulator its settings choose.
/// Installing a RetroArch core emits [`Event::EnginesChanged`]; first-time
/// hints go to `bus` as notices.
pub async fn launch_plan(
    bus: &EventBus,
    app: std::sync::Arc<AppSettings>,
    game: GameFile,
    request: &LaunchRequest,
    ctx: &OpContext,
) -> Result<LaunchPlan> {
    let system = crate::systems::get(&game.system)
        .with_context(|| format!("Irori does not know the system {}", game.system))?;
    let settings = request.settings.clone();

    // Checked first: no emulator download helps a game without its BIOS.
    let bios_root = (!app.bios_folder.trim().is_empty())
        .then(|| irori_core::paths::expand_home(&app.bios_folder));
    let sys_id = game.system.clone();
    let index = tokio::task::spawn_blocking(move || -> Result<bios::BiosIndex> {
        let mut index = bios::cached_index();
        if let Some(root) = &bios_root
            && &index.root != root
        {
            index = bios::scan(root, &index)?;
        }
        let system = crate::systems::get(&sys_id).context("unknown system")?;
        BiosMissing::check(system, &index)?;
        Ok(index)
    })
    .await
    .context("checking BIOS files stopped")??;

    let (sys, chosen) = (game.system.clone(), settings.engine.clone());
    let choice = tokio::task::spawn_blocking(move || choice_for(&app, &sys, &chosen))
        .await
        .context("choosing the emulator stopped")?
        .with_context(|| format!("No emulator plays {} games", system.name))?;
    let manifest = crate::engine(&choice.engine)
        .with_context(|| format!("Irori does not know the emulator {}", choice.engine))?;

    // RetroArch cores are small: fetch a missing one on the way.
    if let Some(core) = &choice.core
        && crate::retroarch::installed_core(core).is_none()
    {
        (ctx.step)("Installing the emulator core");
        let installed = crate::retroarch::install_core(core, ctx).await;
        if !matches!(installed, Ok(Some(_))) {
            let missing = EngineMissing {
                engine: manifest.id.clone(),
                name: manifest.name.clone(),
                core: Some(core.clone()),
                pinned: None,
            };
            return Err(match installed {
                Err(e) => anyhow::Error::new(missing).context(format!("{e:#}")),
                _ => missing.into(),
            });
        }
        bus.emit(Event::EnginesChanged);
    }

    let (m, values_set) = (
        manifest.clone(),
        settings
            .engine_options
            .get(&manifest.id)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default(),
    );
    let (pin, own_settings, core) = (
        settings.engine_version.clone(),
        settings.engine_own_settings,
        choice.core.clone(),
    );
    let m2 = m.clone();
    let installation = tokio::task::spawn_blocking(move || -> Result<_> {
        let state = crate::state::load();
        let installs = crate::discovery::find(&m2, &state);
        let found = crate::launch::resolve(
            &m2,
            &installs,
            Some(pin.as_str()),
            state.preferred.get(&m2.id).map(String::as_str),
        );
        match found {
            Ok(installation) => Ok(installation.clone()),
            // Offering to install an engine Irori can't download here
            // would only fail: say why instead.
            Err(missing) if missing.pinned.is_none() => {
                Err(match EngineUnavailable::no_build(&m2, Host::current()) {
                    Some(unavailable) => unavailable.into(),
                    None => missing.into(),
                })
            }
            Err(missing) => Err(missing.into()),
        }
    })
    .await
    .context("finding the emulator stopped")??;

    if manifest.kind == EngineKind::Relinker {
        if crate::relink::udmabuf_denied() && !UDMABUF_WARNED.swap(true, Ordering::Relaxed) {
            bus.notice(
                Notice::new(
                    NoticeLevel::Warning,
                    format!(
                        "{}: this account can't use /dev/udmabuf, so games may run slowly. Add yourself to the group that owns it (often kvm) and sign in again.",
                        manifest.name
                    ),
                )
                .keys(
                    "shell.notice.udmabuf",
                    None,
                    &[("engine", &manifest.name)],
                ),
            );
        }
        if crate::relink::prepare(&manifest, &installation, &game, ctx)
            .await?
            .is_none()
        {
            bail!("The game's conversion was stopped");
        }
    }

    let (command, first_time) = tokio::task::spawn_blocking(move || -> Result<_> {
        let values = m.option_values(&values_set);
        let command = crate::launch::command(&LaunchInput {
            manifest: &m,
            installation: &installation,
            game: &game,
            core: core.as_deref(),
            values: &values,
            bios: &index,
            own_settings,
        })?;
        let first = crate::state::update(|s| s.hotkey_shown.insert(m.id.clone())).unwrap_or(false);
        Ok((command, first))
    })
    .await
    .context("preparing the emulator stopped")??;
    if first_time && !manifest.exit_hotkey.is_empty() {
        bus.notice(
            Notice::new(
                NoticeLevel::Info,
                format!(
                    "{}: to leave a game, {}",
                    manifest.name, manifest.exit_hotkey
                ),
            )
            .keys(
                "shell.notice.exit_hotkey",
                None,
                &[("engine", &manifest.name), ("keys", &manifest.exit_hotkey)],
            ),
        );
    }
    let env = &request.environment;
    Ok(LaunchPlan {
        command: command
            .envs(env.env.clone())
            .args(request.extra_args.clone())
            .wrapped(&env.wrappers),
        track_wineserver: false,
        watch: None,
    })
}
