//! RomM store backend ([`Runner::Romm`]): the ROMs on the user's RomM
//! server.
//!
//! * Sign-in ([`signin`]): RomM's device flow. The user names the server,
//!   approves Irori on RomM's page in the browser, and the client API token
//!   RomM issues is kept with `irori_secrets` in `auth.json`.
//! * Library: every ROM the account sees, cached in `library.json` so the
//!   library shows offline. ROMs of platforms Irori doesn't play
//!   ([`system_of`]) aren't listed.
//! * Install, update and repair: the ROM's game files (not its manuals,
//!   patches or other extras) download through [`irori_depot`] (resumable,
//!   speed-limited) into a folder of their own, laid out as on the server.
//!   An update is offered when RomM's files change (others, size or hash).
//! * Firmware ([`firmware`](mod@firmware)): the BIOS files and system software
//!   the game's emulator needs come from the firmware RomM keeps for its
//!   platform, when the user lacks them.
//! * Launch: the file with its emulator, as the ROM library plays its games
//!   ([`irori_engines::play`]).

mod firmware;
mod install;
mod records;
pub mod signin;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use irori_core::{
    Event, EventBus, ExtraInfo, GameInfo, HostPlatform, InstallInfo, InstallInfoQuery,
    InstallPlatform, InstallRequest, LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome,
    Paths, RemoveArgs, Runner, SessionExpired, SettingsFn, Store, UpdateRequest, UserInfo,
};
use irori_engines::GameFile;
use irori_engines::systems::{self, System};
use irori_romm_api::{ApiError, Client, Rom, RomFile};
use parking_lot::RwLock;
use url::Url;

pub use records::Layout;
use records::{Auth, InstalledRecord, LibraryCache};

/// Heroic has no RomM library, so there is nothing to import.
pub fn import_from_heroic(_heroic_config_dir: &Path) -> Result<()> {
    Ok(())
}

/// Irori's system for a RomM platform slug (RomM's universal slugs), when
/// Irori plays that platform's games. ROMs of other platforms aren't listed.
pub fn system_of(platform_slug: &str) -> Option<&'static str> {
    Some(match platform_slug {
        "nes" | "famicom" | "fds" => "nes",
        "snes" | "sfam" | "satellaview" => "snes",
        "n64" => "n64",
        "gb" => "gb",
        "gbc" => "gbc",
        "gba" => "gba",
        "nds" | "nintendo-dsi" => "nds",
        "3ds" | "new-nintendo-3ds" => "3ds",
        "ngc" => "gamecube",
        "wii" => "wii",
        "wiiu" => "wiiu",
        "switch" => "switch",
        "sms" => "sms",
        "genesis" => "genesis",
        "gamegear" => "gamegear",
        "segacd" => "segacd",
        "tg16" | "supergrafx" => "pce",
        "psx" => "psx",
        "psp" => "psp",
        "ps2" => "ps2",
        "ps3" => "ps3",
        "ps4" => "ps4",
        "ps5" => "ps5",
        "psvita" => "psvita",
        "dc" => "dreamcast",
        "neo-geo-cd" => "neogeocd",
        "xbox" => "xbox",
        "dos" => "dos",
        "scummvm" => "scummvm",
        "arcade" | "mame" => "arcade",
        _ => return None,
    })
}

/// What downloading a ROM fetches, and what its emulator opens.
pub(crate) struct Download {
    pub system: &'static System,
    /// The game's files with their place in its folder (`/`-separated,
    /// sorted).
    pub files: Vec<(String, RomFile)>,
    pub main: Main,
    /// Some of the files are updates or DLC.
    pub content: bool,
}

pub(crate) enum Main {
    /// This file of the game's folder.
    File(String),
    /// The game's folder itself (ScummVM finds the game there).
    Folder,
    /// The DOS program, picked once the files are on disk
    /// ([`systems::dos_program`]).
    DosProgram,
}

impl Download {
    /// Changes whenever RomM serves different files for the game. A ROM of
    /// one file keeps that file's own revision.
    pub fn revision(&self) -> String {
        let parts: Vec<String> = self.files.iter().map(|(_, f)| f.revision()).collect();
        parts.join(";")
    }

    pub fn size(&self) -> u64 {
        self.files.iter().map(|(_, f)| f.file_size_bytes).sum()
    }
}

/// The download of `rom`, when Irori can play its system and finds the
/// file to open among its game files.
pub(crate) fn download_plan(rom: &Rom) -> Option<Download> {
    let system = systems::get(system_of(&rom.platform_slug)?)?;
    // Names are sanitized per folder, as the ROM library's folders would
    // hold them; anything that still leaves the folder drops the ROM.
    let place_of = |file: &RomFile| -> Option<(String, RomFile)> {
        let place: Vec<String> = rom
            .path_in_rom(file)
            .split(['/', '\\'])
            .filter(|part| !part.is_empty())
            .map(irori_core::paths::sanitize)
            .collect();
        let place = irori_depot::path::normalize(&place.join("/")).ok()?;
        Some((place, file.clone()))
    };
    let mut files = rom.game_files().map(place_of).collect::<Option<Vec<_>>>()?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    // The file the emulator opens is a game file, never an update.
    let main = main_file(system, &files)?;
    let mut extras = rom
        .files_of(&system.content_categories)
        .map(place_of)
        .collect::<Option<Vec<_>>>()?;
    let content = !extras.is_empty();
    files.append(&mut extras);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Some(Download {
        system,
        files,
        main,
        content,
    })
}

fn main_file(system: &System, files: &[(String, RomFile)]) -> Option<Main> {
    let ext = |place: &str| {
        Path::new(place)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    };
    let with = |wanted: &str| {
        files
            .iter()
            .find(|(place, _)| ext(place) == wanted)
            .map(|(place, _)| Main::File(place.clone()))
    };
    if system.id == "dos" {
        let runnable = files
            .iter()
            .any(|(place, _)| ext(place) == "conf" || systems::is_dos_program(Path::new(place)));
        return runnable.then_some(Main::DosProgram);
    }
    // PlayStation 3, 4 and 5 folders open at their program.
    let program = files.iter().find(|(place, _)| {
        let place = place.to_ascii_lowercase();
        system.folder_main.iter().any(|rel| {
            let rel = rel.to_ascii_lowercase();
            place == rel || place.ends_with(&format!("/{rel}"))
        })
    });
    if let Some((place, _)) = program {
        return Some(Main::File(place.clone()));
    }
    // A playlist holds every disc, so it opens before any one of them.
    let order = std::iter::once("m3u").chain(system.extensions.iter().map(String::as_str));
    if let Some(main) = order.filter(|e| system.has_extension(e)).find_map(with) {
        return Some(main);
    }
    if system.folder_games {
        return (!files.is_empty()).then_some(Main::Folder);
    }
    // RetroArch reads zipped cartridges itself; other emulators don't.
    let retroarch = system
        .engines
        .first()
        .is_some_and(|e| e.starts_with("retroarch:"));
    retroarch.then(|| with("zip")).flatten()
}

fn nonempty(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

#[derive(Default)]
struct State {
    auth: Option<Auth>,
    roms: HashMap<String, Rom>,
    installed: BTreeMap<String, InstalledRecord>,
    games: Vec<Arc<GameInfo>>,
    by_app: HashMap<String, Arc<GameInfo>>,
}

impl State {
    fn recompose(&mut self) {
        let server = self.auth.as_ref().and_then(|a| Url::parse(&a.server).ok());
        let mut roms: Vec<&Rom> = self.roms.values().collect();
        roms.sort_by_key(|r| r.id);
        let mut games: Vec<Arc<GameInfo>> = roms
            .into_iter()
            .map(|rom| {
                let app = rom.id.to_string();
                Arc::new(game_info(rom, server.as_ref(), self.installed.get(&app)))
            })
            .collect();
        // Downloaded ROMs RomM no longer lists (signed out, removed from
        // the server) stay playable and removable.
        for (app, record) in &self.installed {
            if !self.roms.contains_key(app) {
                games.push(Arc::new(orphan_info(app, record)));
            }
        }
        self.by_app = games
            .iter()
            .map(|g| (g.app_name.clone(), g.clone()))
            .collect();
        self.games = games;
    }
}

fn game_info(rom: &Rom, server: Option<&Url>, installed: Option<&InstalledRecord>) -> GameInfo {
    let native = HostPlatform::current().native();
    let download = download_plan(rom);
    let on_server =
        |path: &str| server.map(|s| format!("{}{path}", s.as_str().trim_end_matches('/')));
    let cover = rom
        .path_cover_large
        .as_deref()
        .and_then(nonempty)
        .and_then(on_server)
        .or_else(|| {
            rom.url_cover
                .as_deref()
                .and_then(nonempty)
                .map(str::to_string)
        });
    GameInfo {
        runner: Some(Runner::Romm),
        app_name: rom.id.to_string(),
        title: rom.title().to_string(),
        description: rom
            .summary
            .as_deref()
            .and_then(nonempty)
            .map(str::to_string),
        store_url: on_server(&format!("/rom/{}", rom.id)),
        art_cover: cover,
        is_installed: installed.is_some(),
        install: installed.map(InstalledRecord::to_installed_info),
        installable: download.is_some(),
        can_run_offline: true,
        is_linux_native: native == InstallPlatform::Linux,
        is_mac_native: native == InstallPlatform::Mac,
        platforms: vec![native],
        system: system_of(&rom.platform_slug).map(str::to_string),
        tags: rom
            .regions
            .iter()
            .cloned()
            .chain(rom.revision.clone().filter(|r| !r.trim().is_empty()))
            .collect(),
        ..Default::default()
    }
    .finalize()
}

fn orphan_info(app: &str, record: &InstalledRecord) -> GameInfo {
    let native = HostPlatform::current().native();
    GameInfo {
        runner: Some(Runner::Romm),
        app_name: app.to_string(),
        title: record.title.clone(),
        is_installed: true,
        install: Some(record.to_installed_info()),
        can_run_offline: true,
        platforms: vec![native],
        system: Some(record.system.clone()),
        ..Default::default()
    }
    .finalize()
}

/// A refused token becomes [`SessionExpired`], so Irori asks to sign in again.
fn session_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.unauthorized => {
            let reason = api.message.clone();
            e.context(SessionExpired::new(Runner::Romm, reason))
        }
        _ => e,
    }
}

/// The RomM store.
pub struct RommStore {
    bus: EventBus,
    settings: SettingsFn,
    layout: Layout,
    state: RwLock<State>,
    /// Serialises changes of `installed.json`.
    files_lock: tokio::sync::Mutex<()>,
    /// Systems whose firmware was looked for this run.
    firmware_checked: parking_lot::Mutex<std::collections::HashSet<String>>,
    /// The notice about RomM refusing firmware was shown this run.
    firmware_denied_told: std::sync::atomic::AtomicBool,
}

impl RommStore {
    pub fn new(bus: EventBus, settings: SettingsFn) -> Arc<Self> {
        let paths = Paths::get();
        Self::with_layout(
            bus,
            settings,
            Layout {
                config: paths.store_config_dir(Runner::Romm),
                cache: paths.store_cache_dir(Runner::Romm),
            },
        )
    }

    /// A store keeping its files in `layout`.
    pub fn with_layout(bus: EventBus, settings: SettingsFn, layout: Layout) -> Arc<Self> {
        Arc::new(Self {
            bus,
            settings,
            layout,
            state: RwLock::new(State::default()),
            files_lock: tokio::sync::Mutex::new(()),
            firmware_checked: Default::default(),
            firmware_denied_told: Default::default(),
        })
    }

    fn api(&self) -> Result<Client> {
        let state = self.state.read();
        let auth = state.auth.as_ref().context("Sign in to RomM first")?;
        Client::new(Url::parse(&auth.server)?, &auth.token)
    }

    fn rom(&self, app: &str) -> Result<Rom> {
        self.state
            .read()
            .roms
            .get(app)
            .cloned()
            .with_context(|| format!("{app} is not in your RomM library"))
    }

    fn record(&self, app: &str) -> Option<InstalledRecord> {
        self.state.read().installed.get(app).cloned()
    }

    fn require_record(&self, app: &str) -> Result<InstalledRecord> {
        self.record(app)
            .with_context(|| format!("{app} is not downloaded"))
    }

    /// The ROM and its download, or why there is none.
    fn download_of(&self, app: &str) -> Result<(Rom, Download)> {
        let rom = self.rom(app)?;
        match download_plan(&rom) {
            Some(download) => Ok((rom, download)),
            None => bail!("{} has no files Irori can play", rom.title()),
        }
    }

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
        tokio::task::spawn_blocking(f)
            .await
            .context("a background task failed")
    }

    /// Change the download records, save them and publish the library.
    async fn update_installed(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, InstalledRecord>),
    ) -> Result<()> {
        let _guard = self.files_lock.lock().await;
        let mut installed = self.state.read().installed.clone();
        change(&mut installed);
        let layout = self.layout.clone();
        let snapshot = installed.clone();
        Self::blocking(move || records::save_installed(&layout, &snapshot)).await??;
        {
            let mut state = self.state.write();
            state.installed = installed;
            state.recompose();
        }
        self.bus.emit(Event::LibraryChanged(Runner::Romm));
        Ok(())
    }

    fn base_dir(&self, request: &InstallRequest) -> PathBuf {
        let path = if request.path.as_os_str().is_empty() {
            (self.settings)().default_install_path.clone()
        } else {
            request.path.to_string_lossy().into_owned()
        };
        irori_core::paths::expand_home(&path)
    }
}

#[async_trait]
impl Store for RommStore {
    fn runner(&self) -> Runner {
        Runner::Romm
    }

    async fn init(&self) -> Result<()> {
        let layout = self.layout.clone();
        let (auth, library, installed) = Self::blocking(move || {
            (
                records::load_auth(&layout),
                records::load_library(&layout),
                records::load_installed(&layout),
            )
        })
        .await?;
        let mut state = self.state.write();
        // A library cached from another server isn't this account's.
        if auth.as_ref().is_some_and(|a| a.server == library.server) {
            state.roms = by_app(library.roms);
        }
        state.auth = auth;
        state.installed = installed;
        state.recompose();
        Ok(())
    }

    fn user(&self) -> Option<UserInfo> {
        let state = self.state.read();
        let auth = state.auth.as_ref()?;
        let host = Url::parse(&auth.server)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        Some(UserInfo {
            account_id: auth.user.id.to_string(),
            display_name: format!("{} ({host})", auth.user.username),
        })
    }

    /// Irori draws the sign-in: the server's address, then RomM's own
    /// approval page in the browser ([`signin`]).
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::InApp
    }

    /// `input` is [`signin::SignIn::login_input`]'s JSON.
    async fn login(&self, input: &str) -> Result<UserInfo> {
        let signed_in: signin::SignedIn =
            serde_json::from_str(input).context("The RomM sign-in did not finish")?;
        let server = irori_romm_api::server_url(&signed_in.server)?;
        let api = Client::new(server.clone(), &signed_in.token)?;
        let user = match api.me().await {
            Ok(u) => u,
            Err(e) if e.downcast_ref::<ApiError>().is_some_and(|a| a.unauthorized) => {
                bail!("RomM did not accept the sign-in. Try again.")
            }
            Err(e) => return Err(e.context("Could not sign in to RomM")),
        };
        let auth = Auth {
            server: server.to_string(),
            token: signed_in.token,
            user,
        };
        let layout = self.layout.clone();
        let saved = auth.clone();
        Self::blocking(move || records::save_auth(&layout, &saved)).await??;
        {
            let mut state = self.state.write();
            if state.auth.as_ref().is_some_and(|a| a.server != auth.server) {
                state.roms.clear();
            }
            state.auth = Some(auth);
            state.recompose();
        }
        self.bus.emit(Event::AccountChanged(Runner::Romm));
        self.user().context("the sign-in was not kept")
    }

    /// Forgets the token and the library; downloaded ROMs stay.
    async fn logout(&self) -> Result<()> {
        let layout = self.layout.clone();
        Self::blocking(move || {
            for file in [layout.auth_file(), layout.library_file()] {
                match std::fs::remove_file(&file) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(e).with_context(|| format!("removing {}", file.display()));
                    }
                }
            }
            Ok(())
        })
        .await??;
        {
            let mut state = self.state.write();
            state.auth = None;
            state.roms.clear();
            state.recompose();
        }
        self.bus.emit(Event::AccountChanged(Runner::Romm));
        self.bus.emit(Event::LibraryChanged(Runner::Romm));
        Ok(())
    }

    async fn refresh(&self, _full: bool) -> Result<()> {
        if self.state.read().auth.is_none() {
            return Ok(());
        }
        let api = self.api()?;
        let roms = api
            .roms()
            .await
            .map_err(session_error)
            .context("Could not refresh the RomM library")?;
        let cache = LibraryCache {
            server: api.base().to_string(),
            roms,
        };
        let layout = self.layout.clone();
        let saved = cache.clone();
        Self::blocking(move || records::save_library(&layout, &saved)).await??;
        {
            let mut state = self.state.write();
            state.roms = by_app(cache.roms);
            state.recompose();
        }
        self.bus.emit(Event::LibraryChanged(Runner::Romm));
        Ok(())
    }

    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.state.read().games.clone()
    }

    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.state.read().by_app.get(app_name).cloned()
    }

    async fn extra_info(&self, app_name: &str) -> Result<ExtraInfo> {
        let game = self
            .game(app_name)
            .with_context(|| format!("{app_name} is not in your RomM library"))?;
        Ok(ExtraInfo {
            short_description: game.description.clone(),
            store_url: game.store_url.clone(),
            ..Default::default()
        })
    }

    async fn install_info(
        &self,
        app_name: &str,
        _platform: InstallPlatform,
        _query: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        let (rom, download) = self.download_of(app_name)?;
        let resumable = self.has_partial_download(app_name).await;
        Ok(InstallInfo {
            app_name: app_name.to_string(),
            title: rom.title().to_string(),
            // A ROM is the same file on every system.
            platform: HostPlatform::current().native(),
            download_size: download.size(),
            disk_size: download.size(),
            is_native: true,
            resumable,
            ..Default::default()
        })
    }

    /// Downloaded ROMs whose files changed on the server since, as the
    /// library last fetched shows it.
    async fn updatable_games(&self) -> Result<Vec<String>> {
        let state = self.state.read();
        Ok(state
            .installed
            .iter()
            .filter(|(_, record)| !record.pinned_version)
            .filter_map(|(app, record)| {
                let rom = state.roms.get(app)?;
                let download = download_plan(rom)?;
                (download.revision() != record.revision).then(|| app.clone())
            })
            .collect())
    }

    async fn install(
        &self,
        app_name: &str,
        request: &InstallRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let rom = self.rom(app_name)?;
        // RomM's file name keeps the region and revision, so two releases
        // of one title get folders of their own.
        let folder = nonempty(&rom.fs_name_no_ext).unwrap_or(rom.title());
        let dir = self
            .base_dir(request)
            .join(irori_core::paths::sanitize(folder));
        self.run_pipeline(app_name, dir, &ctx).await
    }

    async fn update(
        &self,
        app_name: &str,
        _request: &UpdateRequest,
        ctx: OpContext,
    ) -> Result<Outcome> {
        let record = self.require_record(app_name)?;
        self.run_pipeline(app_name, PathBuf::from(&record.install_path), &ctx)
            .await
    }

    /// Downloads the ROM again.
    async fn repair(&self, app_name: &str, ctx: OpContext) -> Result<Outcome> {
        let record = self.require_record(app_name)?;
        self.run_pipeline(app_name, PathBuf::from(&record.install_path), &ctx)
            .await
    }

    async fn uninstall(&self, app_name: &str, args: RemoveArgs) -> Result<()> {
        let record = self.require_record(app_name)?;
        if args.delete_files {
            let dir = PathBuf::from(&record.install_path);
            let app = (self.settings)();
            Self::blocking(move || -> Result<()> {
                if !dir.exists() {
                    return Ok(());
                }
                irori_core::fs_guard::ensure_deletable(&dir, &app)?;
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("Could not delete {}", dir.display()))
            })
            .await??;
        }
        self.update_installed(|m| {
            m.remove(app_name);
        })
        .await
    }

    /// Registers a folder that already holds the ROM's files (by their
    /// place on the server). Updates are offered once RomM's files differ.
    async fn import(&self, app_name: &str, path: &Path, _platform: InstallPlatform) -> Result<()> {
        let (rom, download) = self.download_of(app_name)?;
        let mut size = 0;
        let mut places = Vec::new();
        for (place, file) in &download.files {
            let found = irori_depot::path::join(path, place);
            match tokio::fs::metadata(&found).await {
                Ok(meta) => {
                    size += meta.len();
                    places.push(place.clone());
                }
                // Updates and DLC are optional extras of a folder.
                Err(_) if !file.is_game() => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("{place} is not in {}", path.display()));
                }
            }
        }
        let content = download
            .files
            .iter()
            .any(|(place, file)| !file.is_game() && places.contains(place));
        let main = main_on_disk(&download.main, path).await?;
        let record = InstalledRecord {
            title: rom.title().to_string(),
            install_path: path.to_string_lossy().into_owned(),
            file: main,
            files: places,
            system: download.system.id.clone(),
            // Unknown until downloaded by Irori: the next check offers it.
            revision: String::new(),
            size,
            pinned_version: false,
            content,
        };
        self.update_installed(|m| {
            m.insert(app_name.to_string(), record);
        })
        .await
    }

    async fn set_install_path(&self, app_name: &str, path: &Path) -> Result<()> {
        self.require_record(app_name)?;
        let p = path.to_string_lossy().into_owned();
        self.update_installed(|m| {
            if let Some(r) = m.get_mut(app_name) {
                r.install_path = p;
            }
        })
        .await
    }

    async fn set_version_pinned(&self, app_name: &str, pinned: bool) -> Result<()> {
        self.require_record(app_name)?;
        self.update_installed(|m| {
            if let Some(r) = m.get_mut(app_name) {
                r.pinned_version = pinned;
            }
        })
        .await
    }

    async fn is_available(&self, app_name: &str) -> bool {
        match self.record(app_name) {
            Some(record) => tokio::fs::try_exists(record.path()).await.unwrap_or(false),
            None => false,
        }
    }

    async fn launch(
        &self,
        app_name: &str,
        request: &LaunchRequest,
        ctx: OpContext,
    ) -> Result<LaunchPlan> {
        let record = self.require_record(app_name)?;
        let rom = self.state.read().roms.get(app_name).cloned();
        if let Some(system) = systems::get(&record.system)
            && let Some(rom) = rom
            && self.first_firmware_check(&record.system)
        {
            self.fetch_firmware(&rom, system, &ctx).await;
        }
        let content_dir = record.content.then(|| PathBuf::from(&record.install_path));
        let game = GameFile {
            path: record.path(),
            system: record.system,
            content_dir,
        };
        irori_engines::play::launch_plan(&self.bus, (self.settings)(), game, request, &ctx).await
    }
}

/// The ROMs Irori lists, by app name.
fn by_app(roms: Vec<Rom>) -> HashMap<String, Rom> {
    roms.into_iter()
        .filter(|r| system_of(&r.platform_slug).is_some())
        .map(|r| (r.id.to_string(), r))
        .collect()
}

/// The place of the file the emulator opens in `dir`, a folder holding the
/// game's files; empty for the folder itself.
async fn main_on_disk(main: &Main, dir: &Path) -> Result<String> {
    match main {
        Main::File(place) => Ok(place.clone()),
        Main::Folder => Ok(String::new()),
        Main::DosProgram => {
            let folder = dir.to_path_buf();
            let program = tokio::task::spawn_blocking(move || systems::dos_program(&folder))
                .await
                .context("looking for the program stopped")?
                .with_context(|| format!("{} holds no DOS program", dir.display()))?;
            let place = program.strip_prefix(dir).unwrap_or(&program);
            Ok(place
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"))
        }
    }
}
