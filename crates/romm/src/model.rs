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
    /// RomM's id of the ROM's platform, which names its firmware.
    pub platform_id: u64,
    /// The name RomM matched in its metadata sources; `None` when the ROM
    /// is unidentified.
    pub name: Option<String>,
    /// The file or folder name, without extension.
    pub fs_name_no_ext: String,
    /// Where RomM keeps the ROM: its file, or its folder when it has
    /// several files.
    pub full_path: String,
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

    /// The files that make up the game itself, without manuals, patches,
    /// DLC and the other extras RomM keeps beside it.
    pub fn game_files(&self) -> impl Iterator<Item = &RomFile> {
        self.files.iter().filter(|f| f.is_game())
    }

    /// The files of the given RomM categories (a game's updates and DLC).
    pub fn files_of<'a>(&'a self, categories: &'a [String]) -> impl Iterator<Item = &'a RomFile> {
        self.files.iter().filter(|f| {
            f.category
                .as_deref()
                .is_some_and(|c| categories.iter().any(|w| w == c))
        })
    }

    /// Where `file` sits inside the ROM's folder (`/`-separated), as RomM
    /// names it in downloads: its name, or its path below the folder of a
    /// ROM with several files.
    pub fn path_in_rom<'a>(&self, file: &'a RomFile) -> &'a str {
        let folder = self.full_path.trim_end_matches('/');
        file.full_path
            .strip_prefix(folder)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rest| !folder.is_empty() && !rest.is_empty())
            .unwrap_or(&file.file_name)
    }
}

/// One file of a ROM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RomFile {
    pub id: u64,
    pub file_name: String,
    pub file_size_bytes: u64,
    /// The file's path on the server, below RomM's library folder.
    pub full_path: String,
    pub updated_at: String,
    pub sha1_hash: Option<String>,
    pub md5_hash: Option<String>,
    /// What the file is to the game (`game`, `manual`, `patch`, `dlc`, …).
    /// RomM leaves it out for the game's own files below the ROM's folder.
    pub category: Option<String>,
}

impl RomFile {
    /// Whether the file is part of the game itself.
    pub fn is_game(&self) -> bool {
        self.category.as_deref().is_none_or(|c| c == "game")
    }

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

/// A firmware file of a platform (`GET /api/firmware`, `FirmwareSchema`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Firmware {
    pub id: u64,
    pub platform_id: u64,
    pub file_name: String,
    pub file_size_bytes: u64,
    pub sha1_hash: String,
    pub md5_hash: String,
    /// The file is listed but gone from the server's disk.
    pub missing_from_fs: bool,
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
        assert_eq!(rom.path_in_rom(file), "Super Metroid (Japan, USA).SFC");
    }

    #[test]
    fn places_the_files_of_a_rom_folder() {
        let rom: Rom = serde_json::from_value(serde_json::json!({
            "id": 3,
            "full_path": "roms/dos/Doom",
            "files": [
                {"id": 1, "file_name": "DOOM.EXE", "full_path": "roms/dos/Doom/DOOM.EXE", "category": "game"},
                {"id": 2, "file_name": "DOOM1.WAD", "full_path": "roms/dos/Doom/DATA/DOOM1.WAD"},
                {"id": 3, "file_name": "manual.pdf", "full_path": "roms/dos/Doom/manual/manual.pdf", "category": "manual"}
            ]
        }))
        .unwrap();
        let placed: Vec<&str> = rom.game_files().map(|f| rom.path_in_rom(f)).collect();
        assert_eq!(placed, ["DOOM.EXE", "DATA/DOOM1.WAD"]);
    }
}
