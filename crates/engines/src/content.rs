//! A folder of a game's updates and DLC, handed to an engine that reads them
//! from folders named in its own config file (Eden's external content).
//!
//! Before a launch Irori puts the game's folder in that list, and takes the
//! folder it added for the previous game out. Every other line of the file,
//! and every folder the user listed, stays as it is.

use std::path::Path;

use anyhow::{Context as _, Result};
use irori_core::json_store;

use crate::manifest::ContentFolderSpec;

/// Make `dir` (or nothing) the one folder Irori has in the engine's list.
/// `marker` is where Irori remembers the folder it added last. Blocking.
pub fn sync(
    spec: &ContentFolderSpec,
    config_dir: &Path,
    marker: &Path,
    dir: Option<&Path>,
) -> Result<()> {
    let file = config_dir.join(&spec.file);
    let previous = std::fs::read_to_string(marker)
        .ok()
        .map(|p| normalize(p.trim()));
    let wanted = dir.map(|d| normalize(&d.to_string_lossy()));
    if previous.is_none() && wanted.is_none() {
        return Ok(());
    }
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let (updated, ours) = edit(&text, spec, previous.as_deref(), wanted.as_deref());
    if updated != text {
        std::fs::create_dir_all(config_dir)
            .with_context(|| format!("creating {}", config_dir.display()))?;
        json_store::write_bytes(&file, updated.as_bytes())
            .with_context(|| format!("writing {}", file.display()))?;
    }
    match ours {
        Some(path) => json_store::write_bytes(marker, path.as_bytes())
            .with_context(|| format!("writing {}", marker.display())),
        None => match std::fs::remove_file(marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", marker.display()))
            }
            _ => Ok(()),
        },
    }
}

/// Folders compare with forward slashes, as the engine writes them.
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn unquote(value: &str) -> String {
    normalize(value.trim().trim_matches('"'))
}

/// The ini text with `previous` taken out of the list and `wanted` put in.
/// Also returns the folder Irori now has in the list, when it added one.
fn edit(
    text: &str,
    spec: &ContentFolderSpec,
    previous: Option<&str>,
    wanted: Option<&str>,
) -> (String, Option<String>) {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.lines().collect();
    let header = format!("[{}]", spec.section);
    let start = lines.iter().position(|l| l.trim() == header);
    let end = start.map_or(0, |s| {
        lines[s + 1..]
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .map_or(lines.len(), |n| s + 1 + n)
    });

    let entry_prefix = format!("{}\\", spec.array);
    let size_key = format!("{}\\size", spec.array);
    let mut listed: Vec<(usize, String)> = Vec::new();
    let mut drop = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if start.is_none_or(|s| i <= s || i >= end) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let index = key
            .strip_prefix(&entry_prefix)
            .and_then(|rest| rest.strip_suffix("\\path"))
            .and_then(|n| n.parse::<usize>().ok());
        if let Some(n) = index {
            listed.push((n, unquote(value)));
            drop.push(i);
        } else if key == size_key {
            drop.push(i);
        }
    }
    listed.sort_by_key(|(n, _)| *n);
    let old: Vec<String> = listed.into_iter().map(|(_, v)| v).collect();

    let mut folders: Vec<String> = old
        .iter()
        .filter(|f| previous != Some(f.as_str()))
        .cloned()
        .collect();
    let mut ours = None;
    if let Some(wanted) = wanted {
        // A folder the user listed themselves stays theirs.
        if !folders.iter().any(|f| f == wanted) {
            folders.push(wanted.to_string());
            ours = Some(wanted.to_string());
        }
    }
    if folders == old {
        return (text.to_string(), ours);
    }

    let mut new_lines: Vec<String> = Vec::new();
    for (n, folder) in folders.iter().enumerate() {
        new_lines.push(format!(
            "{}{}\\path={}",
            entry_prefix,
            n + 1,
            quote_if_needed(folder)
        ));
    }
    new_lines.push(format!("{size_key}={}", folders.len()));

    let mut out: Vec<String> = Vec::new();
    // Where the list was, else after the last line with content in the section.
    let insert_at = drop.first().copied().unwrap_or_else(|| {
        let first = start.unwrap_or(0);
        (first..end)
            .rev()
            .find(|&i| i == first || !lines[i].trim().is_empty())
            .map_or(0, |i| i + 1)
    });
    if start.is_none() {
        out.extend(lines.iter().map(|l| l.to_string()));
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(header);
        out.extend(new_lines);
    } else {
        for (i, line) in lines.iter().enumerate() {
            if i == insert_at {
                out.extend(new_lines.iter().cloned());
            }
            if !drop.contains(&i) {
                out.push(line.to_string());
            }
        }
        if insert_at >= lines.len() {
            out.extend(new_lines);
        }
    }
    let mut joined = out.join(eol);
    joined.push_str(eol);
    (joined, ours)
}

/// A path the ini reader could take for a comment or a second field is quoted;
/// the engine strips the quotes when it reads.
fn quote_if_needed(path: &str) -> String {
    if path.contains([';', '#', '=']) || path != path.trim() {
        format!("\"{path}\"")
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ContentFolderSpec {
        ContentFolderSpec {
            file: "qt-config.ini".into(),
            section: "UI".into(),
            array: "Paths\\external_content_dirs".into(),
        }
    }

    const CONFIG: &str = "[Controls]\nvibration_enabled=true\n\n[UI]\nPaths\\gamedirs\\1\\path=SDMC\nPaths\\gamedirs\\size=1\nPaths\\external_content_dirs\\1\\path=/home/me/updates\nPaths\\external_content_dirs\\size=1\nPaths\\screenshotPath=/home/me/shots\n\n[Core]\nuse_multi_core=true\n";

    fn lines_of(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|l| l.contains("external_content_dirs"))
            .collect()
    }

    #[test]
    fn adds_the_game_folder_beside_the_users_and_takes_it_out_again() {
        let (added, ours) = edit(CONFIG, &spec(), None, Some("/games/Zelda"));
        assert_eq!(ours.as_deref(), Some("/games/Zelda"));
        assert_eq!(
            lines_of(&added),
            [
                "Paths\\external_content_dirs\\1\\path=/home/me/updates",
                "Paths\\external_content_dirs\\2\\path=/games/Zelda",
                "Paths\\external_content_dirs\\size=2",
            ]
        );
        // Nothing else moved.
        assert!(added.contains("Paths\\screenshotPath=/home/me/shots\n\n[Core]"));
        assert!(added.starts_with("[Controls]\nvibration_enabled=true\n"));

        // Another game replaces it.
        let (next, ours) = edit(&added, &spec(), Some("/games/Zelda"), Some("/games/Mario"));
        assert_eq!(ours.as_deref(), Some("/games/Mario"));
        assert_eq!(
            lines_of(&next),
            [
                "Paths\\external_content_dirs\\1\\path=/home/me/updates",
                "Paths\\external_content_dirs\\2\\path=/games/Mario",
                "Paths\\external_content_dirs\\size=2",
            ]
        );

        // A game without content leaves the user's folder alone.
        let (back, ours) = edit(&next, &spec(), Some("/games/Mario"), None);
        assert_eq!(ours, None);
        assert_eq!(back, CONFIG);
    }

    #[test]
    fn leaves_a_folder_the_user_listed_alone() {
        let (same, ours) = edit(CONFIG, &spec(), None, Some("/home/me/updates"));
        assert_eq!(same, CONFIG);
        assert_eq!(ours, None);
        // Not recorded as ours, so it is never taken out later.
        let (same, _) = edit(CONFIG, &spec(), None, None);
        assert_eq!(same, CONFIG);
    }

    #[test]
    fn creates_the_list_and_the_section_when_they_are_missing() {
        let (text, _) = edit("[UI]\nTheme=x\n", &spec(), None, Some("/games/Zelda"));
        assert_eq!(
            text,
            "[UI]\nTheme=x\nPaths\\external_content_dirs\\1\\path=/games/Zelda\nPaths\\external_content_dirs\\size=1\n"
        );
        let (text, _) = edit("[Core]\nx=1\n", &spec(), None, Some("C:/Games/Zelda"));
        assert_eq!(
            text,
            "[Core]\nx=1\n\n[UI]\nPaths\\external_content_dirs\\1\\path=C:/Games/Zelda\nPaths\\external_content_dirs\\size=1\n"
        );
        let (text, _) = edit("", &spec(), None, Some("/a b;c"));
        assert!(text.contains("path=\"/a b;c\"\n"));
    }

    #[test]
    fn keeps_windows_line_endings() {
        let crlf = CONFIG.replace('\n', "\r\n");
        let (text, _) = edit(&crlf, &spec(), None, Some("/games/Zelda"));
        assert!(text.contains("\r\n") && !text.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn syncs_a_file_and_remembers_what_it_added() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let marker = dir.path().join("home/content-folder");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let game = dir.path().join("Zelda");

        // No config yet: it is created for the launch.
        sync(&spec(), &config, &marker, Some(&game)).unwrap();
        let text = std::fs::read_to_string(config.join("qt-config.ini")).unwrap();
        assert!(text.contains(&format!(
            "Paths\\external_content_dirs\\1\\path={}",
            game.display().to_string().replace('\\', "/")
        )));
        assert!(marker.exists());

        sync(&spec(), &config, &marker, None).unwrap();
        let text = std::fs::read_to_string(config.join("qt-config.ini")).unwrap();
        assert!(text.contains("external_content_dirs\\size=0"));
        assert!(!marker.exists());

        // With nothing to add or remove the file isn't touched.
        let before = std::fs::read(config.join("qt-config.ini")).unwrap();
        sync(&spec(), &config, &marker, None).unwrap();
        assert_eq!(std::fs::read(config.join("qt-config.ini")).unwrap(), before);
    }
}
