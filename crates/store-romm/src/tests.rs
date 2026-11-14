use std::path::{Path, PathBuf};
use std::sync::Arc;

use irori_core::{AppSettings, InstallRequest, OpContext, Outcome, RemoveArgs, SessionExpired};
use irori_depot::test_server::{Resp, TestServer, serve_bytes};
use parking_lot::Mutex;
use serde_json::json;

use super::*;

const TOKEN: &str = "rmm_test";

/// RomM's answers; tests change `rom_bytes` and `revoked` as they go.
struct Server {
    rom_bytes: Mutex<Vec<u8>>,
    revoked: Mutex<bool>,
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
                {"id": 70, "file_name": "manual.pdf", "file_size_bytes": 3},
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
        }
    ])
}

fn start_server() -> (TestServer, Arc<Server>) {
    let state = Arc::new(Server {
        rom_bytes: Mutex::new(b"SNES ROM v1".to_vec()),
        revoked: Mutex::new(false),
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
        } else if path.starts_with("/api/roms/7/content/") && path.ends_with("?file_ids=71") {
            serve_bytes(req, &s.rom_bytes.lock(), true)
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
    let app = Arc::new(AppSettings {
        default_install_path: dir.join("Games").display().to_string(),
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
async fn lists_every_rom_and_downloads_only_snes_games() {
    let (server, _) = start_server();
    let dir = test_dir();
    let s = signed_in(dir.path(), &server).await;

    let games = s.games();
    assert_eq!(games.len(), 2);
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
    assert!(!mario.installable);
    let err = s
        .install_info("8", InstallPlatform::Linux, &InstallInfoQuery::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Irori can't play Nintendo 64 games from RomM yet"
    );
    assert!(matches!(install(&s, "8").await, Outcome::Error(_)));

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
    assert_eq!(again.games().len(), 2);
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

#[test]
fn picks_the_file_an_emulator_plays() {
    let file = |id, name: &str| RomFile {
        id,
        file_name: name.into(),
        ..Default::default()
    };
    let rom = Rom {
        files: vec![
            file(1, "readme.txt"),
            file(2, "Game.zip"),
            file(3, "Game.SMC"),
        ],
        ..Default::default()
    };
    assert_eq!(playable_file(&rom, "snes").map(|f| f.id), Some(3));
    let zipped = Rom {
        files: vec![file(1, "readme.txt"), file(2, "Game.zip")],
        ..Default::default()
    };
    assert_eq!(playable_file(&zipped, "snes").map(|f| f.id), Some(2));
    let none = Rom {
        files: vec![file(1, "readme.txt")],
        ..Default::default()
    };
    assert!(playable_file(&none, "snes").is_none());
    assert_eq!(system_of("sfam"), Some("snes"));
    assert_eq!(system_of("n64"), None);
}
