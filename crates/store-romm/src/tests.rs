use std::path::{Path, PathBuf};
use std::sync::Arc;

use irori_core::{AppSettings, InstallRequest, OpContext, Outcome, RemoveArgs, SessionExpired};
use irori_depot::test_server::{Resp, TestServer, serve_bytes};
use parking_lot::Mutex;
use serde_json::json;

use super::*;

const TOKEN: &str = "rmm_test";

/// The firmware RomM keeps for the PlayStation platform.
const BIOS_BYTES: &[u8] = b"a PlayStation BIOS";

/// Paths for the BIOS index, once per run.
static ROOT: std::sync::LazyLock<tempfile::TempDir> = std::sync::LazyLock::new(|| {
    let dir = tempfile::tempdir().expect("temp dir");
    Paths::init(Paths::under(dir.path()));
    dir
});

/// RomM's answers; tests change `rom_bytes` and `revoked` as they go.
struct Server {
    rom_bytes: Mutex<Vec<u8>>,
    revoked: Mutex<bool>,
    /// The status of the firmware list.
    firmware_status: Mutex<u16>,
}

fn rom_json(server: &Server) -> serde_json::Value {
    let bytes = server.rom_bytes.lock();
    json!([
        {
            "id": 7,
            "name": "Super Metroid",
            "fs_name_no_ext": "Super Metroid (Japan, USA)",
            "platform_slug": "snes",
            "platform_display_name": "Super Nintendo",
            "summary": "Samus returns.",
            "path_cover_large": "/assets/romm/resources/roms/1/7/cover/big.png?ts=1",
            "regions": ["USA"],
            "files": [
                {"id": 70, "file_name": "manual.pdf", "file_size_bytes": 3, "category": "manual"},
                {
                    "id": 71,
                    "file_name": "Super Metroid (Japan, USA).sfc",
                    "file_size_bytes": bytes.len(),
                    "sha1_hash": hex::encode(<sha1::Sha1 as sha1::Digest>::digest(&*bytes)),
                }
            ]
        },
        {
            "id": 8,
            "name": "Super Mario 64",
            "fs_name_no_ext": "Super Mario 64 (USA)",
            "platform_slug": "n64",
            "platform_display_name": "Nintendo 64",
            "files": [{"id": 80, "file_name": "Super Mario 64 (USA).z64", "file_size_bytes": 8}]
        },
        {
            "id": 9,
            "name": "Panzer Dragoon",
            "fs_name_no_ext": "Panzer Dragoon (USA)",
            "platform_slug": "saturn",
            "platform_display_name": "Sega Saturn",
            "files": [{"id": 90, "file_name": "Panzer Dragoon (USA).chd", "file_size_bytes": 8}]
        },
        {
            "id": 10,
            "platform_id": 5,
            "name": "Final Fantasy VII",
            "fs_name_no_ext": "Final Fantasy VII (USA)",
            "full_path": "roms/psx/Final Fantasy VII (USA)",
            "platform_slug": "psx",
            "platform_display_name": "PlayStation",
            "files": [
                file_json(100, "roms/psx/Final Fantasy VII (USA)", "FF7.cue", None),
                file_json(101, "roms/psx/Final Fantasy VII (USA)", "FF7 (Track 1).bin", None),
                file_json(102, "roms/psx/Final Fantasy VII (USA)/manual", "Manual.pdf", Some("manual")),
            ]
        },
        {
            "id": 12,
            "name": "Zelda",
            "fs_name_no_ext": "Zelda",
            "full_path": "roms/switch/Zelda",
            "platform_slug": "switch",
            "platform_display_name": "Nintendo Switch",
            "files": [
                file_json(120, "roms/switch/Zelda", "Zelda [base].nsp", None),
                file_json(121, "roms/switch/Zelda/update", "Zelda [v65536].nsp", Some("update")),
                file_json(122, "roms/switch/Zelda/dlc", "Zelda DLC.nsp", Some("dlc")),
                file_json(123, "roms/switch/Zelda/manual", "Manual.pdf", Some("manual")),
            ]
        },
        {
            "id": 11,
            "name": "Doom",
            "fs_name_no_ext": "Doom",
            "full_path": "roms/dos/Doom",
            "platform_slug": "dos",
            "platform_display_name": "DOS",
            "files": [
                file_json(110, "roms/dos/Doom", "INSTALL.EXE", Some("game")),
                file_json(111, "roms/dos/Doom", "DOOM.EXE", Some("game")),
                file_json(112, "roms/dos/Doom/DATA", "DOOM1.WAD", None),
            ]
        }
    ])
}

/// The bytes RomM serves for the file with `id` of the disc and folder
/// games.
fn file_bytes(id: u64) -> Vec<u8> {
    format!("file {id}").into_bytes()
}

fn file_json(id: u64, folder: &str, name: &str, category: Option<&str>) -> serde_json::Value {
    json!({
        "id": id,
        "file_name": name,
        "full_path": format!("{folder}/{name}"),
        "file_size_bytes": file_bytes(id).len(),
        "category": category,
    })
}

fn start_server() -> (TestServer, Arc<Server>) {
    let state = Arc::new(Server {
        rom_bytes: Mutex::new(b"SNES ROM v1".to_vec()),
        revoked: Mutex::new(false),
        firmware_status: Mutex::new(200),
    });
    let s = state.clone();
    let server = TestServer::start(move |req| {
        let authorized =
            req.header("authorization") == Some(&format!("Bearer {TOKEN}")) && !*s.revoked.lock();
        if !authorized {
            return Resp {
                body: br#"{"detail":"Invalid token"}"#.to_vec(),
                ..Resp::status(401)
            };
        }
        let path = req.path.as_str();
        if path == "/api/users/me" {
            Resp::json(json!({"id": 1, "username": "samus"}))
        } else if path.starts_with("/api/roms?") {
            Resp::json(json!({"items": rom_json(&s)}))
        } else if path == "/api/firmware?platform_id=5" {
            match *s.firmware_status.lock() {
                200 => Resp::json(json!([
                    {"id": 50, "platform_id": 5, "file_name": "scph5501.bin",
                     "file_size_bytes": BIOS_BYTES.len(), "md5_hash": "", "sha1_hash": ""},
                    {"id": 51, "platform_id": 5, "file_name": "other.bin",
                     "file_size_bytes": 3, "md5_hash": "", "sha1_hash": ""},
                ])),
                status => Resp {
                    body: br#"{"detail":"Insufficient scope"}"#.to_vec(),
                    ..Resp::status(status)
                },
            }
        } else if path == "/api/firmware/50/content/scph5501.bin" {
            serve_bytes(req, BIOS_BYTES, true)
        } else if path.starts_with("/api/roms/7/content/") && path.ends_with("?file_ids=71") {
            serve_bytes(req, &s.rom_bytes.lock(), true)
        } else if let Some((_, id)) = path
            .split_once("/content/")
            .and_then(|(_, rest)| rest.split_once("?file_ids="))
        {
            serve_bytes(req, &file_bytes(id.parse().unwrap()), true)
        } else {
            Resp::status(404)
        }
    });
    (server, state)
}

fn test_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn store(dir: &Path) -> Arc<RommStore> {
    std::sync::LazyLock::force(&ROOT);
    let app = Arc::new(AppSettings {
        default_install_path: dir.join("Games").display().to_string(),
        bios_folder: dir.join("bios").display().to_string(),
        ..Default::default()
    });
    RommStore::with_layout(
        EventBus::new(),
        Arc::new(move || app.clone()),
        Layout {
            config: dir.join("config"),
            cache: dir.join("cache"),
        },
    )
}

fn login_input(server: &TestServer) -> String {
    serde_json::to_string(&signin::SignedIn {
        server: server.base(),
        token: TOKEN.into(),
    })
    .unwrap()
}

async fn signed_in(dir: &Path, server: &TestServer) -> Arc<RommStore> {
    let s = store(dir);
    s.init().await.unwrap();
    let user = s.login(&login_input(server)).await.unwrap();
    assert_eq!(user.display_name, "samus (127.0.0.1)");
    s.refresh(false).await.unwrap();
    s
}

async fn install(s: &RommStore, app: &str) -> Outcome {
    s.install(app, &InstallRequest::default(), OpContext::detached())
        .await
        .unwrap()
}

#[tokio::test]
async fn lists_the_roms_of_systems_irori_plays() {
    let (server, _) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;

    let games = s.games();
    assert_eq!(games.len(), 5);
    assert!(s.game("9").is_none(), "Saturn games aren't playable yet");
    let metroid = s.game("7").unwrap();
    assert_eq!(metroid.title, "Super Metroid");
    assert_eq!(metroid.system.as_deref(), Some("snes"));
    assert!(metroid.installable && !metroid.is_installed);
    assert_eq!(
        metroid.art_cover.as_deref(),
        Some(
            format!(
                "{}/assets/romm/resources/roms/1/7/cover/big.png?ts=1",
                server.base()
            )
            .as_str()
        )
    );
    assert_eq!(
        metroid.store_url.as_deref(),
        Some(format!("{}/rom/7", server.base()).as_str())
    );
    let mario = s.game("8").unwrap();
    assert_eq!(mario.system.as_deref(), Some("n64"));
    assert!(mario.installable);

    let info = s
        .install_info("7", InstallPlatform::Linux, &InstallInfoQuery::default())
        .await
        .unwrap();
    assert_eq!(info.download_size, 11);
    assert!(matches!(install(&s, "7").await, Outcome::Done));
    let file: PathBuf = dir
        .path()
        .join("Games/Super Metroid (Japan, USA)/Super Metroid (Japan, USA).sfc");
    assert_eq!(std::fs::read(&file).unwrap(), b"SNES ROM v1");
    let game = s.game("7").unwrap();
    assert!(game.is_installed);
    assert_eq!(
        game.install.as_ref().unwrap().executable,
        file.display().to_string()
    );
    assert!(s.is_available("7").await);
    assert!(!dir.path().join("Games/.romm-download/7").exists());

    // Signed out, the library is gone but the download stays playable.
    s.logout().await.unwrap();
    assert!(s.user().is_none());
    assert_eq!(s.games().len(), 1);
    assert!(s.game("7").unwrap().is_installed);
}

#[tokio::test]
async fn offers_and_applies_updates_when_the_file_changes() {
    let (server, state) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;
    assert!(matches!(install(&s, "7").await, Outcome::Done));
    assert!(s.updatable_games().await.unwrap().is_empty());

    *state.rom_bytes.lock() = b"SNES ROM v2, patched".to_vec();
    s.refresh(false).await.unwrap();
    assert_eq!(s.updatable_games().await.unwrap(), vec!["7".to_string()]);

    // A pinned version is left alone.
    s.set_version_pinned("7", true).await.unwrap();
    assert!(s.updatable_games().await.unwrap().is_empty());
    s.set_version_pinned("7", false).await.unwrap();

    let outcome = s
        .update("7", &UpdateRequest::default(), OpContext::detached())
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Done));
    let record = s.record("7").unwrap();
    assert_eq!(
        std::fs::read(record.path()).unwrap(),
        b"SNES ROM v2, patched"
    );
    assert!(s.updatable_games().await.unwrap().is_empty());

    // Everything survives a restart.
    let again = store(dir.path());
    again.init().await.unwrap();
    assert_eq!(again.games().len(), 5);
    assert!(again.game("7").unwrap().is_installed);

    let install_dir = PathBuf::from(&record.install_path);
    s.uninstall(
        "7",
        RemoveArgs {
            delete_files: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!install_dir.exists());
    assert!(!s.game("7").unwrap().is_installed);
}

#[tokio::test]
async fn a_revoked_token_asks_to_sign_in_again() {
    let (server, state) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;
    *state.revoked.lock() = true;
    let err = s.refresh(false).await.unwrap_err();
    assert!(SessionExpired::of(&err).is_some());

    let fresh = store(&dir.path().join("other"));
    fresh.init().await.unwrap();
    let err = fresh.login(&login_input(&server)).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "RomM did not accept the sign-in. Try again."
    );
}

#[tokio::test]
async fn downloads_every_file_of_disc_and_folder_games() {
    let (server, _) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;

    let info = s
        .install_info("10", InstallPlatform::Linux, &InstallInfoQuery::default())
        .await
        .unwrap();
    assert_eq!(info.download_size, 2 * file_bytes(100).len() as u64);
    assert!(matches!(install(&s, "10").await, Outcome::Done));
    let ff7 = dir.path().join("Games/Final Fantasy VII (USA)");
    assert_eq!(
        std::fs::read(ff7.join("FF7 (Track 1).bin")).unwrap(),
        file_bytes(101)
    );
    assert!(!ff7.join("manual").exists(), "extras stay on the server");
    assert_eq!(
        s.game("10").unwrap().install.as_ref().unwrap().executable,
        ff7.join("FF7.cue").display().to_string()
    );

    assert!(matches!(install(&s, "11").await, Outcome::Done));
    let doom = dir.path().join("Games/Doom");
    assert_eq!(
        std::fs::read(doom.join("DATA/DOOM1.WAD")).unwrap(),
        file_bytes(112)
    );
    let record = s.record("11").unwrap();
    assert_eq!(record.path(), doom.join("DOOM.EXE"));
    assert_eq!(record.files, ["DATA/DOOM1.WAD", "DOOM.EXE", "INSTALL.EXE"]);
    assert!(s.updatable_games().await.unwrap().is_empty());
}

#[tokio::test]
async fn fetches_the_bios_a_system_lacks_from_romms_firmware() {
    let (server, _) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;

    assert!(matches!(install(&s, "10").await, Outcome::Done));
    let romm = dir.path().join("bios/RomM");
    assert_eq!(
        std::fs::read(romm.join("scph5501.bin")).unwrap(),
        BIOS_BYTES
    );
    assert!(
        !romm.join("other.bin").exists(),
        "only what the system's emulator looks for is fetched"
    );
    let index = irori_engines::bios::cached_index();
    assert!(
        index
            .files
            .iter()
            .any(|f| f.path == romm.join("scph5501.bin"))
    );
}

#[tokio::test]
async fn a_game_installs_when_romm_refuses_its_firmware() {
    let (server, state) = start_server();
    *state.firmware_status.lock() = 403;
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;
    let mut events = s.bus.subscribe();

    assert!(matches!(install(&s, "10").await, Outcome::Done));
    assert!(!dir.path().join("bios/RomM").exists());
    assert!(s.user().is_some(), "the sign-in stands");
    let mut told = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, Event::Notice(n) if n.title_key == Some("shell.notice.romm_firmware_denied"))
        {
            told += 1;
        }
    }
    assert_eq!(told, 1);
}

#[tokio::test]
async fn downloads_a_switch_games_updates_and_dlc_with_it() {
    let (server, _) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;
    assert!(matches!(install(&s, "12").await, Outcome::Done));

    let zelda = dir.path().join("Games/Zelda");
    for place in [
        "Zelda [base].nsp",
        "update/Zelda [v65536].nsp",
        "dlc/Zelda DLC.nsp",
    ] {
        assert!(zelda.join(place).is_file(), "{place}");
    }
    assert!(!zelda.join("manual").exists(), "extras stay on the server");
    let record = s.record("12").unwrap();
    assert!(record.content);
    assert_eq!(
        record.path(),
        zelda.join("Zelda [base].nsp"),
        "the emulator opens the game, not an update"
    );
    // A game with nothing but its own files has no content folder.
    assert!(matches!(install(&s, "10").await, Outcome::Done));
    assert!(!s.record("10").unwrap().content);
}

#[test]
fn picks_the_file_an_emulator_plays() {
    let file = |id, name: &str| RomFile {
        id,
        file_name: name.into(),
        ..Default::default()
    };
    let rom = |slug: &str, files| Rom {
        platform_slug: slug.into(),
        files,
        ..Default::default()
    };
    let main = |rom: &Rom| match download_plan(rom)?.main {
        Main::File(place) => Some(place),
        _ => None,
    };
    let snes = rom(
        "snes",
        vec![
            file(1, "Game.zip"),
            file(2, "Game.SMC"),
            file(3, "readme.txt"),
        ],
    );
    assert_eq!(main(&snes).as_deref(), Some("Game.SMC"));
    // RetroArch opens zipped cartridges; DuckStation doesn't open zips.
    let zipped = rom("sfam", vec![file(1, "readme.txt"), file(2, "Game.zip")]);
    assert_eq!(main(&zipped).as_deref(), Some("Game.zip"));
    assert!(download_plan(&rom("psx", vec![file(1, "Game.zip")])).is_none());
    // A playlist opens before any of its discs.
    let discs = rom(
        "psx",
        vec![
            file(1, "Disc 1.cue"),
            file(2, "Disc 2.cue"),
            file(3, "Game.m3u"),
        ],
    );
    assert_eq!(main(&discs).as_deref(), Some("Game.m3u"));
    assert!(download_plan(&rom("snes", vec![file(1, "readme.txt")])).is_none());
    assert_eq!(system_of("genesis"), Some("genesis"));
    assert_eq!(system_of("dc"), Some("dreamcast"));
    assert_eq!(system_of("neo-geo-cd"), Some("neogeocd"));
    assert_eq!(system_of("psvita"), Some("psvita"));
    assert_eq!(system_of("saturn"), None);
}
