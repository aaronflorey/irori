//! System software an engine needs beside BIOS files: Eden's Switch firmware,
//! a zip of `.nca` files that goes into the engine's own storage.
//!
//! The archive comes from the user's BIOS folder (RomM puts it there) and is
//! unpacked before a launch, once per archive. Firmware the user installed
//! themselves is never replaced.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context as _, Result, bail};
use regex::Regex;

use crate::manifest::FirmwareSpec;

/// Folder levels searched below the BIOS folder.
const MAX_DEPTH: usize = 3;
/// One firmware file is far below this; an archive claiming more is not one.
const MAX_ENTRY: u64 = 1 << 30;
const MAX_TOTAL: u64 = 2 << 30;
/// Beside the unpacked files: which archive they came from.
const STAMP: &str = "irori-firmware";

/// What makes one archive newer than another: its version, then its age.
type Newness = (Option<Vec<u64>>, u64);

fn matcher(spec: &FirmwareSpec) -> Option<Regex> {
    Regex::new(&format!("(?i){}", spec.archive)).ok()
}

/// The version in an archive name (`Firmware 19.0.1.zip` is `[19, 0, 1]`),
/// compared as a list of numbers.
pub fn version(name: &str) -> Option<Vec<u64>> {
    name.split(|c: char| !c.is_ascii_digit() && c != '.')
        .filter_map(|candidate| {
            let parts: Vec<&str> = candidate.trim_matches('.').split('.').collect();
            if parts.len() < 2 {
                return None;
            }
            let mut version = parts
                .iter()
                .map(|part| part.parse().ok())
                .collect::<Option<Vec<u64>>>()?;
            while version.last() == Some(&0) {
                version.pop();
            }
            Some(version)
        })
        .max()
}

/// The archive among `names` that `spec` accepts, the highest version first.
pub fn newest<'a>(
    spec: &FirmwareSpec,
    names: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let re = matcher(spec)?;
    names
        .into_iter()
        .filter(|n| re.is_match(n))
        .max_by_key(|n| version(n))
}

/// The newest archive of `spec` in `root` (modification time breaks ties).
/// Blocking.
pub fn find_archive(spec: &FirmwareSpec, root: &Path) -> Option<PathBuf> {
    let re = matcher(spec)?;
    let mut best: Option<(Newness, PathBuf)> = None;
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
            let name = entry.file_name().to_string_lossy().into_owned();
            if !re.is_match(&name) {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let key = (version(&name), mtime);
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((key, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// Unpack the newest archive of `spec` found in `root` into the engine's
/// `data_dir`, unless it is already there. Returns whether files were
/// written. Blocking.
///
/// Files already in the target folder that the user put there are left
/// alone: only firmware this function unpacked is replaced.
pub fn install(spec: &FirmwareSpec, root: &Path, data_dir: &Path) -> Result<bool> {
    let Some(archive) = find_archive(spec, root) else {
        return Ok(false);
    };
    let meta =
        std::fs::metadata(&archive).with_context(|| format!("reading {}", archive.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let signature = format!(
        "{}\n{}\n{mtime}",
        archive.file_name().unwrap_or_default().to_string_lossy(),
        meta.len()
    );
    let target = data_dir.join(&spec.dir);
    let stamp = data_dir.join(STAMP);
    let ours = std::fs::read_to_string(&stamp).ok();
    let occupied = std::fs::read_dir(&target).is_ok_and(|mut d| d.next().is_some());
    match (ours.as_deref(), occupied) {
        (Some(s), true) if s == signature => return Ok(false),
        (None, true) => return Ok(false),
        _ => {}
    }

    let name = target
        .file_name()
        .context("the firmware folder has no name")?
        .to_string_lossy()
        .into_owned();
    let stage = target.with_file_name(format!("{name}.irori-new"));
    let old = target.with_file_name(format!("{name}.irori-old"));
    let _ = std::fs::remove_dir_all(&stage);
    if !target.exists() && old.exists() {
        // An earlier swap stopped half way: its old firmware is still good.
        std::fs::rename(&old, &target).context("restoring the previous firmware")?;
    }
    std::fs::create_dir_all(&stage).with_context(|| format!("creating {}", stage.display()))?;
    if let Err(e) = unpack(&archive, &stage) {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(e.context(format!("unpacking {}", archive.display())));
    }
    let _ = std::fs::remove_dir_all(&old);
    if target.exists() {
        std::fs::rename(&target, &old).context("replacing the previous firmware")?;
    }
    if let Err(e) = std::fs::rename(&stage, &target) {
        if old.exists() {
            let _ = std::fs::rename(&old, &target);
        }
        return Err(e).with_context(|| format!("moving firmware to {}", target.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    std::fs::write(&stamp, signature).context("recording the installed firmware")?;
    Ok(true)
}

/// The `.nca` files of the zip `archive` into `stage`, flat and lower case,
/// as the emulator's own install leaves them. Other entries are ignored.
fn unpack(archive: &Path, stage: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip =
        zip::ZipArchive::new(std::io::BufReader::new(file)).context("this is not a zip archive")?;
    let mut total = 0u64;
    let mut count = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        // Links and folders hold no firmware; flattening the name below
        // keeps every file inside `stage` whatever its path says.
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let Some(name) = entry
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".nca") else {
            continue;
        };
        let id = stem.strip_suffix(".cnmt").unwrap_or(stem);
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("{name} is not a firmware file name");
        }
        let size = entry.size();
        total = total.saturating_add(size);
        if size == 0 || size > MAX_ENTRY || total > MAX_TOTAL {
            bail!("{name} has an unreasonable size");
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.join(&name))
            .with_context(|| format!("{name} appears twice"))?;
        let copied = std::io::copy(&mut (&mut entry).take(size + 1), &mut out)?;
        if copied != size {
            bail!("{name} is not the size the archive says");
        }
        count += 1;
    }
    if count == 0 {
        bail!("the archive holds no firmware (.nca) files");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn spec() -> FirmwareSpec {
        FirmwareSpec {
            archive: r"^firmware.*\.zip$".into(),
            dir: "nand/system/Contents/registered".into(),
        }
    }

    fn zip_of(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    const A: &str = "0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210";

    #[test]
    fn versions_order_numerically_and_pick_the_newest_archive() {
        assert_eq!(version("Firmware 19.0.1.zip"), Some(vec![19, 0, 1]));
        assert_eq!(version("Firmware.18.0.0.zip"), Some(vec![18]));
        assert_eq!(version("firmware.zip"), None);
        let names = ["Firmware 9.2.0.zip", "Firmware 19.0.1.zip", "notes.zip"];
        assert_eq!(newest(&spec(), names), Some("Firmware 19.0.1.zip"));
        assert_eq!(newest(&spec(), ["keys.zip"]), None);
    }

    #[test]
    fn unpacks_nca_files_flat_and_replaces_only_its_own_firmware() {
        let dir = tempfile::tempdir().unwrap();
        let bios = dir.path().join("bios");
        let data = dir.path().join("eden");
        std::fs::create_dir_all(bios.join("RomM")).unwrap();
        let archive = bios.join("RomM/Firmware 18.0.0.zip");
        zip_of(
            &archive,
            &[
                (&format!("Firmware 18/{}.NCA", A.to_uppercase()), b"one"),
                (&format!("Firmware 18/{B}.cnmt.nca"), b"two"),
                ("Firmware 18/readme.txt", b"ignored"),
            ],
        );
        assert!(install(&spec(), &bios, &data).unwrap());
        let registered = data.join(&spec().dir);
        assert_eq!(
            std::fs::read(registered.join(format!("{A}.nca"))).unwrap(),
            b"one"
        );
        assert_eq!(
            std::fs::read(registered.join(format!("{B}.cnmt.nca"))).unwrap(),
            b"two"
        );
        assert!(!registered.join("readme.txt").exists());
        // Already installed: nothing is written again.
        assert!(!install(&spec(), &bios, &data).unwrap());

        // A newer archive replaces the old files.
        let newer = bios.join("RomM/Firmware 19.0.1.zip");
        zip_of(&newer, &[(&format!("{B}.nca"), b"three")]);
        assert!(install(&spec(), &bios, &data).unwrap());
        assert!(!registered.join(format!("{A}.nca")).exists());
        assert_eq!(
            std::fs::read(registered.join(format!("{B}.nca"))).unwrap(),
            b"three"
        );
        assert!(!registered.with_file_name("registered.irori-old").exists());
    }

    #[test]
    fn leaves_firmware_the_user_installed_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (bios, data) = (dir.path().join("bios"), dir.path().join("eden"));
        std::fs::create_dir_all(&bios).unwrap();
        zip_of(&bios.join("firmware.zip"), &[(&format!("{A}.nca"), b"x")]);
        let registered = data.join(&spec().dir);
        std::fs::create_dir_all(&registered).unwrap();
        std::fs::write(registered.join(format!("{B}.nca")), b"mine").unwrap();
        assert!(!install(&spec(), &bios, &data).unwrap());
        assert_eq!(
            std::fs::read(registered.join(format!("{B}.nca"))).unwrap(),
            b"mine"
        );
        assert!(!registered.join(format!("{A}.nca")).exists());
    }

    #[test]
    fn refuses_archives_that_are_not_firmware() {
        let dir = tempfile::tempdir().unwrap();
        let (bios, data) = (dir.path().join("bios"), dir.path().join("eden"));
        std::fs::create_dir_all(&bios).unwrap();
        for (entries, why) in [
            (
                vec![("../evil.nca", &b"x"[..])],
                "a name that is no firmware id",
            ),
            (vec![("notes.txt", &b"x"[..])], "no firmware files"),
        ] {
            zip_of(&bios.join("firmware.zip"), &entries);
            assert!(install(&spec(), &bios, &data).is_err(), "{why}");
            assert!(!data.join(&spec().dir).exists(), "{why}");
        }
    }
}
