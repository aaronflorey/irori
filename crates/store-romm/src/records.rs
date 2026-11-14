//! What the RomM store keeps on disk: the server and its token
//! (`auth.json`, through `irori_secrets`), this installation's device id
//! (`device-id`), the library last fetched (`library.json` in the cache) and
//! the downloaded ROMs (`installed.json`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use irori_core::util::format_bytes;
use irori_core::{HostPlatform, InstalledInfo, json_store};
use irori_romm_api::{Rom, User};
use serde::{Deserialize, Serialize};

/// Where the store's files live.
#[derive(Debug, Clone)]
pub struct Layout {
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Layout {
    pub fn auth_file(&self) -> PathBuf {
        self.config.join("auth.json")
    }

    pub fn device_file(&self) -> PathBuf {
        self.config.join("device-id")
    }

    pub fn installed_file(&self) -> PathBuf {
        self.config.join("installed.json")
    }

    pub fn library_file(&self) -> PathBuf {
        self.cache.join("library.json")
    }
}

/// The saved sign-in. The token is a secret: it never goes to logs.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    /// The server's base URL ([`irori_romm_api::server_url`]).
    pub server: String,
    pub token: String,
    pub user: User,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("server", &self.server)
            .field("user", &self.user.username)
            .finish_non_exhaustive()
    }
}

/// The library as last fetched, with the server it came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryCache {
    pub server: String,
    pub roms: Vec<Rom>,
}

/// A downloaded ROM.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledRecord {
    pub title: String,
    /// The folder holding the game's files.
    pub install_path: String,
    /// The place in that folder (`/`-separated) of the file the emulator
    /// opens; empty when it opens the folder.
    pub file: String,
    /// Every file downloaded, by place. Empty in records written when a
    /// download was only `file`.
    pub files: Vec<String>,
    /// Irori's system id (`snes`).
    pub system: String,
    /// [`crate::Download::revision`] of the downloaded files.
    pub revision: String,
    pub size: u64,
    /// The user keeps this version: no update checks.
    pub pinned_version: bool,
}

impl InstalledRecord {
    pub fn path(&self) -> PathBuf {
        let dir = Path::new(&self.install_path);
        if self.file.is_empty() {
            dir.to_path_buf()
        } else {
            irori_depot::path::join(dir, &self.file)
        }
    }

    /// The downloaded files, by place.
    pub fn files(&self) -> Vec<String> {
        if self.files.is_empty() && !self.file.is_empty() {
            vec![self.file.clone()]
        } else {
            self.files.clone()
        }
    }

    pub fn to_installed_info(&self) -> InstalledInfo {
        InstalledInfo {
            install_path: self.install_path.clone(),
            executable: self.path().to_string_lossy().into_owned(),
            install_size: format_bytes(self.size),
            install_size_bytes: self.size,
            platform: HostPlatform::current().native(),
            pinned_version: self.pinned_version,
            ..Default::default()
        }
    }
}

pub fn load_auth(layout: &Layout) -> Option<Auth> {
    irori_secrets::load::<Auth>(&layout.auth_file()).filter(|a| !a.token.is_empty())
}

pub fn save_auth(layout: &Layout, auth: &Auth) -> Result<()> {
    irori_secrets::write_json(&layout.auth_file(), auth).context("Could not save the RomM sign-in")
}

/// This installation's id in RomM's device list, made on first use so
/// signing in again replaces the device instead of adding another.
pub fn device_id(layout: &Layout) -> Result<String> {
    let file = layout.device_file();
    if let Ok(id) = std::fs::read_to_string(&file)
        && !id.trim().is_empty()
    {
        return Ok(id.trim().to_string());
    }
    let id = uuid::Uuid::new_v4().to_string();
    json_store::write_bytes(&file, id.as_bytes())
        .with_context(|| format!("saving {}", file.display()))?;
    Ok(id)
}

pub fn load_installed(layout: &Layout) -> BTreeMap<String, InstalledRecord> {
    json_store::read_or_default(&layout.installed_file())
}

pub fn save_installed(
    layout: &Layout,
    installed: &BTreeMap<String, InstalledRecord>,
) -> Result<()> {
    json_store::write(&layout.installed_file(), installed)
        .context("Could not save the list of downloaded RomM games")
}

pub fn load_library(layout: &Layout) -> LibraryCache {
    json_store::read_or_default(&layout.library_file())
}

pub fn save_library(layout: &Layout, library: &LibraryCache) -> Result<()> {
    json_store::write(&layout.library_file(), library)
}
