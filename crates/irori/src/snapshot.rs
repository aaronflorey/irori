//! Screenshots of the whole window without a display: the real `AppShell` over
//! a backend with an in-memory store, drawn by GPUI's offscreen renderer.
//!
//! ```text
//! IRORI_SNAPSHOT_OUT=/tmp/shots IRORI_SNAPSHOT_LINKS=irori://library,irori://settings/wine \
//!   IRORI_SNAPSHOT_THEMES=dark,light IRORI_SNAPSHOT_LANGS=en,de \
//!   cargo test -p irori snapshot -- --ignored --nocapture
//! ```
//!
//! One PNG is written per link, theme and language, named
//! `<link>-<theme>-<lang>.png`. Links are the `irori://` ones Irori opens
//! (`irori://game/<runner>/<app>` shows a game's page); the games are the
//! ones in [`MemoryStore`]. Sizes are `WIDTHxHEIGHT` in logical pixels.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gpui_kit::component::TitleBar;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, HeadlessAppContext, WindowBounds, WindowDecorations,
    WindowOptions, px, size,
};
use irori_core::settings::ThemePreference;
use irori_core::{
    DynStore, ExtraInfo, GameInfo, InstallInfo, InstallInfoQuery, InstallPlatform, InstallRequest,
    LaunchPlan, LaunchRequest, LoginFlow, OpContext, Outcome, Paths, RemoveArgs, Runner, Store,
    UpdateRequest, UserInfo, async_trait,
};
use irori_services::Backend;
use irori_ui::{Route, models::models, nav};

use crate::cli::Cli;
use crate::shell::{self, AppShell};
use crate::{stores, strings};

/// A store with a fixed catalogue, so screens have something to show.
struct MemoryStore {
    games: Vec<Arc<GameInfo>>,
}

impl MemoryStore {
    fn new() -> Arc<Self> {
        let titles: [(&str, &str, bool); 12] = [
            ("1", "Alan Wake", false),
            ("2", "Celeste", true),
            ("3", "Hades", true),
            ("4", "Inside", false),
            ("5", "Outer Wilds", true),
            ("6", "Disco Elysium: The Final Cut", true),
            (
                "7",
                "The Legend of Heroes: Trails in the Sky the 3rd",
                false,
            ),
            ("8", "Return of the Obra Dinn", true),
            ("9", "Stardew Valley", false),
            ("10", "Hollow Knight", true),
            ("11", "Baldur's Gate 3", false),
            ("12", "Pentiment", true),
        ];
        let games = titles
            .into_iter()
            .map(|(app, title, installed)| {
                let install = installed.then(|| irori_core::InstalledInfo {
                    install_path: format!("/nonexistent/irori-snapshots/{app}"),
                    ..Default::default()
                });
                Arc::new(
                    GameInfo {
                        runner: Some(Runner::Gog),
                        app_name: app.into(),
                        title: title.into(),
                        is_installed: installed,
                        installable: true,
                        install,
                        ..Default::default()
                    }
                    .finalize(),
                )
            })
            .collect();
        Arc::new(Self { games })
    }
}

#[async_trait]
impl Store for MemoryStore {
    fn runner(&self) -> Runner {
        Runner::Gog
    }
    async fn init(&self) -> Result<()> {
        Ok(())
    }
    fn user(&self) -> Option<UserInfo> {
        Some(UserInfo {
            account_id: "1".into(),
            display_name: "Tester".into(),
        })
    }
    fn login_flow(&self) -> LoginFlow {
        LoginFlow::None
    }
    async fn login(&self, _: &str) -> Result<UserInfo> {
        bail!("not supported")
    }
    async fn logout(&self) -> Result<()> {
        Ok(())
    }
    async fn refresh(&self, _: bool) -> Result<()> {
        Ok(())
    }
    fn games(&self) -> Vec<Arc<GameInfo>> {
        self.games.clone()
    }
    fn game(&self, app_name: &str) -> Option<Arc<GameInfo>> {
        self.games.iter().find(|g| g.app_name == app_name).cloned()
    }
    async fn extra_info(&self, _: &str) -> Result<ExtraInfo> {
        Ok(ExtraInfo::default())
    }
    async fn install_info(
        &self,
        _: &str,
        _: InstallPlatform,
        _: &InstallInfoQuery,
    ) -> Result<InstallInfo> {
        bail!("not supported")
    }
    async fn updatable_games(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn install(&self, _: &str, _: &InstallRequest, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn update(&self, _: &str, _: &UpdateRequest, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn repair(&self, _: &str, _: OpContext) -> Result<Outcome> {
        bail!("not supported")
    }
    async fn uninstall(&self, _: &str, _: RemoveArgs) -> Result<()> {
        bail!("not supported")
    }
    async fn import(&self, _: &str, _: &Path, _: InstallPlatform) -> Result<()> {
        bail!("not supported")
    }
    async fn set_install_path(&self, _: &str, _: &Path) -> Result<()> {
        bail!("not supported")
    }
    async fn launch(&self, _: &str, _: &LaunchRequest, _: OpContext) -> Result<LaunchPlan> {
        bail!("not supported")
    }
}

/// `IRORI_SNAPSHOT_<name>` as a comma-separated list, or `default`.
fn list(name: &str, default: &str) -> Vec<String> {
    std::env::var(format!("IRORI_SNAPSHOT_{name}"))
        .unwrap_or_else(|_| default.into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn window_size() -> (f32, f32) {
    let value = list("SIZE", "1280x800").remove(0);
    value
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or_else(|| panic!("IRORI_SNAPSHOT_SIZE must look like 1280x800, not {value:?}"))
}

/// The route an `irori://` link opens, as the command line reads it.
fn route_of(link: &str) -> Route {
    let cli = Cli::parse([link.to_string()]);
    match (cli.open, cli.route) {
        (Some(id), _) => Route::Game(id),
        (None, Some(route)) => route,
        (None, None) if cli.big_picture => Route::BigPicture,
        (None, None) => panic!("{link:?} opens no screen"),
    }
}

fn file_stem(link: &str, theme: &str, language: &str) -> String {
    let name: String = link
        .trim_start_matches("irori://")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{name}-{theme}-{language}")
}

fn isolate_paths() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let root = std::env::temp_dir().join(format!("irori-snapshots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Paths::init(Paths::under(&root));
    });
}

/// Run every part of the app's start that decides what a screen looks like.
fn init_app(backend: &Backend, cx: &mut gpui_kit::App) {
    gpui_kit::init(cx);
    irori_ui::init(backend.clone(), cx);
    irori_ui::i18n::register(strings::CATALOGS);
    irori_ui::hooks::install(stores::hooks(), cx);
    irori_ui::art_picker::install(irori_library::open_art_picker_dialog, cx);
    irori_bigpicture::init(cx);
    irori_library::init(cx);
    irori_manage::init(cx);
    irori_settings::init(cx);
    irori_game::init(cx);
    irori_game::install_hooks(stores::game_hooks(), cx);
}

fn settle(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    for _ in 0..20 {
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .expect("window is open");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "writes screenshots; see the module docs"]
fn snapshot() {
    isolate_paths();
    let out = PathBuf::from(
        std::env::var("IRORI_SNAPSHOT_OUT").unwrap_or_else(|_| "irori-snapshots".into()),
    );
    std::fs::create_dir_all(&out).expect("output folder");
    let (width, height) = window_size();

    let text_system = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        text_system,
        Arc::new(gpui_kit::assets::AllAssets),
        gpui_platform::current_headless_renderer,
    );
    cx.allow_parking();
    let store: DynStore = MemoryStore::new();
    let backend = Backend::start_for_tests(move |_, _| vec![store]).expect("backend starts");
    cx.update(|cx| init_app(&backend, cx));
    let deadline = Instant::now() + Duration::from_secs(10);
    while cx.update(|cx| models(cx).library.read(cx).snapshot.len()) != 12 {
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the library never loaded");
        std::thread::sleep(Duration::from_millis(10));
    }

    for link in list("LINKS", "irori://library") {
        for theme in list("THEMES", "dark") {
            let preference = match theme.as_str() {
                "dark" => ThemePreference::Dark,
                "light" => ThemePreference::Light,
                other => panic!("IRORI_SNAPSHOT_THEMES takes dark or light, not {other:?}"),
            };
            for language in list("LANGS", "en") {
                // Through the settings, as the user would: the shell re-applies
                // the stored preferences whenever the window's appearance changes.
                backend.settings.update_app(|app| {
                    app.ui.theme = preference;
                    app.ui_language = language.clone();
                });
                let wanted = (preference, language.clone());
                let deadline = Instant::now() + Duration::from_secs(5);
                while cx.update(|cx| {
                    let app = &models(cx).settings.read(cx).app;
                    (app.ui.theme, app.ui_language.clone()) != wanted
                }) {
                    cx.run_until_parked();
                    assert!(Instant::now() < deadline, "the settings never changed");
                    std::thread::sleep(Duration::from_millis(10));
                }
                irori_ui::i18n::set_language(&language);
                let route = route_of(&link);
                let (window, _) = cx
                    .update(|cx| {
                        nav::navigator(cx).update(cx, |n, cx| n.replace(route.clone(), cx));
                        let prefs = models(cx).settings.read(cx).app.ui.clone();
                        irori_ui::theme::apply(&prefs, None, cx);
                        let options = WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                Default::default(),
                                size(px(width), px(height)),
                            ))),
                            window_decorations: Some(WindowDecorations::Client),
                            window_background: irori_ui::glass::window_background(
                                shell::initial_glass(&route, cx),
                            ),
                            focus: false,
                            show: false,
                            ..TitleBar::window_options()
                        };
                        gpui_kit::open_window(options, cx, |window, cx| {
                            cx.new(|cx| AppShell::new(window, cx))
                        })
                    })
                    .expect("window opens");
                settle(&mut cx, window);
                let image = cx.capture_screenshot(window).expect("screenshot");
                let path = out.join(format!("{}.png", file_stem(&link, &theme, &language)));
                image.save(&path).expect("png written");
                println!("{}", path.display());
                cx.update_window(window, |_, window, _| window.remove_window())
                    .expect("window closes");
                cx.run_until_parked();
            }
        }
    }
}
