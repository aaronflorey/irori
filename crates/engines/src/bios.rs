//! The BIOS folder: an index of the user's files, built in the background
//! and cached by path, size and modification time so nothing is hashed
//! twice, and per-system status from it. Before a launch, the files a system
//! needs are linked under the names engines expect.
//!
//! Irori recognises the user's own BIOS files. The only ones it fetches are
//! the firmware of the user's own RomM server ([`wanted`] says which of its
//! files the user is missing).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context as _, Result};
use irori_core::json_store;
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};

use crate::manifest::FirmwareSpec;
use crate::systems::{self, BiosFile, BiosRule, System};

/// Files larger than this are not BIOS images (the largest are a few MiB).
const MAX_BIOS_SIZE: u64 = 32 << 20;
/// Folder levels searched below the BIOS folder.
const MAX_DEPTH: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub path: PathBuf,
    pub size: u64,
    /// Modification time, Unix seconds.
    pub mtime: u64,
    /// Lower-case MD5.
    pub md5: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BiosIndex {
    /// The folder this index is of.
    pub root: PathBuf,
    pub files: Vec<IndexedFile>,
}

/// The folder BIOS files are read from: the user's choice (`configured`,
/// the `biosFolder` setting), else Irori's own.
pub fn folder(configured: &str) -> PathBuf {
    if configured.trim().is_empty() {
        crate::dirs::bios()
    } else {
        irori_core::paths::expand_home(configured)
    }
}

fn index_file() -> PathBuf {
    crate::dirs::cache().join("bios-index.json")
}

/// The saved index (empty when there is none).
pub fn cached_index() -> BiosIndex {
    json_store::read_or_default(&index_file())
}

/// File names Irori knows as BIOS files of some system, lower case.
fn known_names() -> Vec<String> {
    systems::all()
        .iter()
        .flat_map(|s| s.bios.files.iter().map(|f| f.name.to_lowercase()))
        .collect()
}

/// The compiled name patterns of every BIOS file that has one.
fn known_patterns() -> Vec<regex::Regex> {
    systems::all()
        .iter()
        .flat_map(|s| s.bios.files.iter())
        .filter_map(|f| f.pattern.as_deref())
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect()
}

/// Index `root`, reusing digests from `previous` for unchanged files, and
/// save the result. Only plausible BIOS files are hashed: known names, or
/// `.bin`/`.rom`/`.img` files of BIOS size. Blocking.
pub fn scan(root: &Path, previous: &BiosIndex) -> Result<BiosIndex> {
    let names = known_names();
    let patterns = known_patterns();
    let mut reuse: BTreeMap<&Path, &IndexedFile> = BTreeMap::new();
    for f in &previous.files {
        reuse.insert(f.path.as_path(), f);
    }
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let size = meta.len();
            if size == 0 || size > MAX_BIOS_SIZE {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let plausible = names.contains(&name)
                || patterns.iter().any(|p| p.is_match(&name))
                || [".bin", ".rom", ".img"].iter().any(|e| name.ends_with(e));
            if !plausible {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let md5 = match reuse.get(path.as_path()) {
                Some(f) if f.size == size && f.mtime == mtime => f.md5.clone(),
                _ => match md5_file(&path) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::debug!("hashing {}: {e:#}", path.display());
                        continue;
                    }
                },
            };
            files.push(IndexedFile {
                path,
                size,
                mtime,
                md5,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let index = BiosIndex {
        root: root.to_path_buf(),
        files,
    };
    json_store::write(&index_file(), &index).context("saving the BIOS index")?;
    Ok(index)
}

fn md5_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// How one expected BIOS file stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileState {
    /// Found, and a known-good dump (or any dump works).
    Ready,
    /// A file of that name was found but it is not a dump Irori knows; it
    /// may not work.
    Unrecognized,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStatus {
    pub name: String,
    /// Found by a name pattern rather than one exact name.
    #[serde(default)]
    pub pattern: bool,
    pub region: Option<String>,
    pub state: FileState,
    /// The user's file that provides it.
    pub found: Option<PathBuf>,
}

/// How a system's BIOS stands as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SystemState {
    /// The system needs no BIOS, or has every file it needs.
    Ready,
    /// Files are there but not all are known-good dumps.
    Unrecognized,
    /// A required file is missing; games won't start.
    Missing,
    /// Optional files are missing; games start, some features need them.
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBios {
    pub system: String,
    pub state: SystemState,
    pub files: Vec<FileStatus>,
}

/// BIOS status of `system` from `index`. A file is found by its digest
/// whatever its name, else by its name.
pub fn status(system: &System, index: &BiosIndex) -> SystemBios {
    let files: Vec<FileStatus> = system
        .bios
        .files
        .iter()
        .map(|want| {
            let by_digest = index.files.iter().find(|f| want.md5.contains(&f.md5));
            let pattern = want
                .pattern
                .as_deref()
                .and_then(|p| regex::Regex::new(p).ok());
            let by_name = || {
                index.files.iter().find(|f| {
                    f.path.file_name().is_some_and(|n| {
                        let n = n.to_string_lossy();
                        match &pattern {
                            Some(p) => p.is_match(&n),
                            None => n.eq_ignore_ascii_case(&want.name),
                        }
                    })
                })
            };
            let (state, found) = match (by_digest, by_name()) {
                (Some(f), _) => (FileState::Ready, Some(f.path.clone())),
                (None, Some(f)) if want.md5.is_empty() => (FileState::Ready, Some(f.path.clone())),
                (None, Some(f)) => (FileState::Unrecognized, Some(f.path.clone())),
                (None, None) => (FileState::Missing, None),
            };
            FileStatus {
                name: want.name.clone(),
                pattern: want.pattern.is_some(),
                region: want.region.clone(),
                state,
                found,
            }
        })
        .collect();
    let ready = files.iter().filter(|f| f.state == FileState::Ready).count();
    let present = files.iter().filter(|f| f.found.is_some()).count();
    let state = if files.is_empty() {
        SystemState::Ready
    } else {
        match system.bios.rule {
            BiosRule::AnyOf if ready > 0 => SystemState::Ready,
            BiosRule::AnyOf if present > 0 => SystemState::Unrecognized,
            BiosRule::AnyOf => SystemState::Missing,
            BiosRule::All if ready == files.len() => SystemState::Ready,
            BiosRule::All if present == files.len() => SystemState::Unrecognized,
            BiosRule::All => SystemState::Missing,
            BiosRule::Optional if present == files.len() => SystemState::Ready,
            BiosRule::Optional => SystemState::Optional,
        }
    };
    SystemBios {
        system: system.id.clone(),
        state,
        files,
    }
}

/// A file a server offers.
#[derive(Debug, Clone, Copy)]
pub struct Offer<'a> {
    pub name: &'a str,
    /// Lower-case MD5; empty when unknown.
    pub md5: &'a str,
}

/// What `system` still lacks.
enum Lack {
    /// A BIOS file with no match yet (`found`: a file of the name exists but
    /// is no known dump, so only a known dump helps).
    File { want: BiosFile, found: bool },
    /// A firmware archive no file of `root` matches.
    Archive(FirmwareSpec),
}

fn lacking(system: &System, root: &Path, index: &BiosIndex) -> Vec<Lack> {
    let mut out: Vec<Lack> = status(system, index)
        .files
        .iter()
        .zip(&system.bios.files)
        .filter(|(have, _)| have.state != FileState::Ready)
        .map(|(have, want)| Lack::File {
            want: want.clone(),
            found: have.found.is_some(),
        })
        .collect();
    for engine in &system.engines {
        let id = engine.split(':').next().unwrap_or(engine);
        if let Some(spec) = crate::engine(id).and_then(|m| m.firmware.clone())
            && crate::firmware::find_archive(&spec, root).is_none()
        {
            out.push(Lack::Archive(spec));
        }
    }
    out
}

/// Whether `system` lacks a BIOS file or firmware archive, so a server's
/// firmware could help. Blocking.
pub fn lacks(system: &System, root: &Path, index: &BiosIndex) -> bool {
    !lacking(system, root, index).is_empty()
}

/// Which of the server's `offers` (by position) give `system` what it
/// lacks: a file by its known digest, or by its name when none is there yet,
/// and the newest firmware archive. Blocking.
pub fn wanted(system: &System, root: &Path, index: &BiosIndex, offers: &[Offer]) -> Vec<usize> {
    let mut picked = Vec::new();
    for lack in lacking(system, root, index) {
        let found = match lack {
            Lack::File { want, found } => {
                let pattern = want
                    .pattern
                    .as_deref()
                    .and_then(|p| regex::Regex::new(p).ok());
                let by_digest = |o: &Offer| {
                    !o.md5.is_empty() && want.md5.iter().any(|m| m.eq_ignore_ascii_case(o.md5))
                };
                let by_name = |o: &Offer| match &pattern {
                    Some(p) => p.is_match(o.name),
                    None => o.name.eq_ignore_ascii_case(&want.name),
                };
                offers
                    .iter()
                    .position(by_digest)
                    .or_else(|| offers.iter().position(|o| !found && by_name(o)))
            }
            Lack::Archive(spec) => crate::firmware::newest(&spec, offers.iter().map(|o| o.name))
                .and_then(|name| offers.iter().position(|o| o.name == name)),
        };
        if let Some(i) = found
            && !picked.contains(&i)
        {
            picked.push(i);
        }
    }
    picked
}

/// Link every found file of `system` into `target` under its expected
/// name, replacing older links. Returns how many were linked. Blocking.
pub fn link_into(system: &System, index: &BiosIndex, target: &Path) -> Result<usize> {
    let mut linked = 0;
    for file in status(system, index).files {
        let Some(found) = file.found else { continue };
        std::fs::create_dir_all(target)
            .with_context(|| format!("creating {}", target.display()))?;
        // Files found by a pattern keep their own name.
        let name = if file.pattern {
            found
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.name.clone())
        } else {
            file.name.clone()
        };
        let dest = target.join(&name);
        if dest == found {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&dest) {
            if !meta.file_type().is_symlink() {
                // Without symbolic links (Windows) Irori places copies; one of
                // the same size is already in place. Any other real file is
                // the user's and is never replaced.
                if cfg!(not(unix)) && std::fs::metadata(&found).is_ok_and(|f| f.len() == meta.len())
                {
                    linked += 1;
                }
                continue;
            }
            let _ = std::fs::remove_file(&dest);
        }
        link(&found, &dest).with_context(|| format!("linking {}", dest.display()))?;
        linked += 1;
    }
    Ok(linked)
}

#[cfg(unix)]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(from, to)
}

#[cfg(not(unix))]
fn link(from: &Path, to: &Path) -> std::io::Result<()> {
    // Symbolic links need developer mode on Windows; BIOS files are small.
    std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn psx() -> &'static System {
        systems::get("psx").unwrap()
    }

    #[test]
    fn scans_hashes_once_and_reports_status() {
        let root = crate::test_root("bios");
        let bios = root.join("bios");
        std::fs::create_dir_all(bios.join("sony")).unwrap();
        // A file with a known name but unknown content.
        std::fs::write(bios.join("sony/SCPH5501.BIN"), b"not a real bios").unwrap();
        std::fs::write(bios.join("notes.txt"), b"ignored").unwrap();
        let index = scan(&bios, &BiosIndex::default()).unwrap();
        assert_eq!(index.files.len(), 1);
        let s = status(psx(), &index);
        assert_eq!(s.state, SystemState::Unrecognized);
        assert_eq!(s.files[0].state, FileState::Unrecognized);

        // A known digest is recognised whatever the file is called.
        let mut index = index;
        index.files.push(IndexedFile {
            path: bios.join("renamed.bin"),
            size: 524288,
            mtime: 0,
            md5: "8dd7d5296a650fac7319bce665a6a53c".into(),
        });
        let s = status(psx(), &index);
        assert_eq!(s.state, SystemState::Ready);
        let jp = s.files.iter().find(|f| f.name == "scph5500.bin").unwrap();
        assert_eq!(jp.state, FileState::Ready);

        // Unchanged files keep their digest without hashing.
        let mut previous = cached_index();
        previous.files[0].md5 = "cafe".into();
        let again = scan(&bios, &previous).unwrap();
        assert_eq!(again.files[0].md5, "cafe");
    }

    #[test]
    fn systems_without_files_are_missing_or_optional() {
        let empty = BiosIndex::default();
        assert_eq!(status(psx(), &empty).state, SystemState::Missing);
        let gba = systems::get("gba").unwrap();
        assert_eq!(status(gba, &empty).state, SystemState::Optional);
        let snes = systems::get("snes").unwrap();
        assert_eq!(status(snes, &empty).state, SystemState::Ready);
    }

    #[test]
    fn patterns_find_bios_files_with_many_names() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SCPH-70012_BIOS_V12.bin");
        std::fs::write(&file, b"x").unwrap();
        let index = BiosIndex {
            root: dir.path().into(),
            files: vec![IndexedFile {
                path: file.clone(),
                size: 1,
                mtime: 0,
                md5: "00".into(),
            }],
        };
        let ps2 = systems::get("ps2").unwrap();
        assert_eq!(status(ps2, &index).state, SystemState::Ready);
        assert_eq!(
            status(ps2, &BiosIndex::default()).state,
            SystemState::Missing
        );
        let target = dir.path().join("bios");
        assert_eq!(link_into(ps2, &index, &target).unwrap(), 1);
        assert!(target.join("SCPH-70012_BIOS_V12.bin").exists());
    }

    #[test]
    fn picks_the_offered_files_a_system_lacks() {
        let _ = crate::test_root("bios-wanted");
        let dir = tempfile::tempdir().unwrap();
        let offers = [
            Offer {
                name: "other.bin",
                md5: "",
            },
            Offer {
                name: "SCPH5501.BIN",
                md5: "",
            },
            Offer {
                name: "Firmware 9.0.0.zip",
                md5: "",
            },
            Offer {
                name: "Firmware 19.0.1.zip",
                md5: "",
            },
            Offer {
                name: "prod.keys",
                md5: "",
            },
        ];
        let empty = BiosIndex::default();
        // A PlayStation lacks its BIOS, found by name; nothing else is offered.
        assert_eq!(wanted(psx(), dir.path(), &empty, &offers), [1]);
        // A known digest finds a file whatever it is called.
        let by_digest = [Offer {
            name: "any name.bin",
            md5: "8dd7d5296a650fac7319bce665a6a53c",
        }];
        assert_eq!(wanted(psx(), dir.path(), &empty, &by_digest), [0]);
        // The Switch lacks its keys and, for Eden, the newest firmware.
        let switch = systems::get("switch").unwrap();
        assert_eq!(wanted(switch, dir.path(), &empty, &offers), [4, 3]);

        // What the user has is not fetched again.
        let have = BiosIndex {
            root: dir.path().into(),
            files: vec![IndexedFile {
                path: dir.path().join("prod.keys"),
                size: 1,
                mtime: 0,
                md5: "00".into(),
            }],
        };
        std::fs::write(dir.path().join("firmware.zip"), b"x").unwrap();
        assert!(wanted(switch, dir.path(), &have, &offers).is_empty());
        assert!(!lacks(systems::get("snes").unwrap(), dir.path(), &empty));
    }

    #[test]
    fn links_found_files_under_expected_names() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("my-gba.bin");
        std::fs::write(&mine, b"x").unwrap();
        let index = BiosIndex {
            root: dir.path().into(),
            files: vec![IndexedFile {
                path: mine.clone(),
                size: 1,
                mtime: 0,
                md5: "a860e8c0b6d573d191e4ec7db1b1e4f6".into(),
            }],
        };
        let target = dir.path().join("system");
        let gba = systems::get("gba").unwrap();
        assert_eq!(link_into(gba, &index, &target).unwrap(), 1);
        assert_eq!(std::fs::read(target.join("gba_bios.bin")).unwrap(), b"x");
        // Linking again replaces the link.
        assert_eq!(link_into(gba, &index, &target).unwrap(), 1);
    }
}
