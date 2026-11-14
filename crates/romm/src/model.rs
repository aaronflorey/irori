//! What RomM's API answers, reduced to the fields Irori reads. Every field
//! has a default, so a server that leaves one out (or a newer one that
//! changes the rest) still reads.

use serde::{Deserialize, Serialize};

/// The signed-in account (`GET /api/users/me`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: u64,
    pub username: String,
}

/// A ROM in the library (`GET /api/roms`, `SimpleRomSchema`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rom {
    pub id: u64,
    /// The name RomM matched in its metadata sources; `None` when the ROM
    /// is unidentified.
    pub name: Option<String>,
    /// The file or folder name, without extension.
    pub fs_name_no_ext: String,
    /// RomM's platform slug (`snes`, `n64`, …).
    pub platform_slug: String,
    pub platform_display_name: String,
    pub summary: Option<String>,
    /// Server path of the large cover (`/assets/romm/resources/…?ts=…`),
    /// served without sign-in; empty when there is none.
    pub path_cover_large: Option<String>,
    /// The cover's original address at the metadata source.
    pub url_cover: Option<String>,
    pub regions: Vec<String>,
    pub revision: Option<String>,
    pub files: Vec<RomFile>,
}

impl Rom {
    /// The title to show: the identified name, else the file name.
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.fs_name_no_ext)
    }
}

/// One file of a ROM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RomFile {
    pub id: u64,
    pub file_name: String,
    pub file_size_bytes: u64,
    pub updated_at: String,
    pub sha1_hash: Option<String>,
    pub md5_hash: Option<String>,
}

impl RomFile {
    /// The file's extension, lower case, without the dot.
    pub fn extension(&self) -> String {
        std::path::Path::new(&self.file_name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }

    /// Changes whenever RomM serves different bytes for this file: the
    /// file, its size and its content (SHA-1 when RomM hashed it, else the
    /// time RomM last saw it change).
    pub fn revision(&self) -> String {
        let content = self
            .sha1_hash
            .as_deref()
            .filter(|h| !h.is_empty())
            .unwrap_or(&self.updated_at);
        format!("{}:{}:{content}", self.id, self.file_size_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_rom_from_the_library_listing() {
        let rom: Rom = serde_json::from_value(serde_json::json!({
            "id": 12,
            "name": null,
            "fs_name_no_ext": "Super Metroid (Japan, USA)",
            "platform_slug": "snes",
            "platform_display_name": "Super Nintendo",
            "path_cover_large": "/assets/romm/resources/roms/3/12/cover/big.png?ts=1",
            "regions": ["USA"],
            "unknown_field": {"kept": false},
            "files": [{
                "id": 40,
                "file_name": "Super Metroid (Japan, USA).SFC",
                "file_size_bytes": 3145728,
                "updated_at": "2026-01-02T03:04:05Z",
                "sha1_hash": null
            }]
        }))
        .unwrap();
        assert_eq!(rom.title(), "Super Metroid (Japan, USA)");
        let file = &rom.files[0];
        assert_eq!(file.extension(), "sfc");
        assert_eq!(file.revision(), "40:3145728:2026-01-02T03:04:05Z");
        let hashed = RomFile {
            sha1_hash: Some("abc".into()),
            ..file.clone()
        };
        assert_eq!(hashed.revision(), "40:3145728:abc");
    }
}
