//! Download, update and repair: the ROM's game files download through
//! [`irori_depot`] into `<base>/.romm-download/<app>/` (resumable), then
//! move into the game's folder, replacing the previous files only once every
//! new one is complete.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use irori_core::{InstallProgress, OpContext, Outcome, util};

use crate::records::InstalledRecord;
use crate::{RommStore, main_on_disk, session_error};

fn progress_sink(ctx: &OpContext) -> irori_depot::ProgressFn {
    let sink = ctx.progress.clone();
    Arc::new(move |p: irori_depot::Progress| {
        sink(InstallProgress {
            percent: Some(p.percent.clamp(0.0, 100.0)),
            bytes_done: Some(p.bytes_done),
            bytes_total: Some(p.bytes_total),
            eta_secs: p.eta_secs,
            download_speed: Some(p.download_speed),
            disk_speed: Some(p.disk_speed),
            bottleneck: p.bottleneck,
            preparing: p.preparing,
        })
    })
}

fn log_line(ctx: &OpContext, line: &str) {
    if let Some(log) = &ctx.log_file {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(f, "[RomM] {line}");
        }
    }
}

/// Free bytes on the filesystem holding `path` (or its nearest existing parent).
fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    fs4::available_space(existing).ok()
}

impl RommStore {
    fn staging_dir(&self, app: &str, base: &Path) -> PathBuf {
        base.join(".romm-download")
            .join(irori_core::paths::sanitize(app))
    }

    pub(crate) async fn has_partial_download(&self, app: &str) -> bool {
        let base = irori_core::paths::expand_home(&(self.settings)().default_install_path);
        tokio::fs::try_exists(self.staging_dir(app, &base))
            .await
            .unwrap_or(false)
    }

    /// Run the pipeline; errors become [`Outcome::Error`] for the queue.
    pub(crate) async fn run_pipeline(
        &self,
        app: &str,
        install_dir: PathBuf,
        ctx: &OpContext,
    ) -> Result<Outcome> {
        match self.pipeline(app, &install_dir, ctx).await {
            Ok(o) => Ok(o),
            Err(e) => {
                let e = session_error(e);
                log_line(ctx, &format!("failed: {e:#}"));
                if ctx.cancel.is_cancelled() {
                    Ok(Outcome::Abort)
                } else {
                    Ok(Outcome::Error(format!("{e:#}")))
                }
            }
        }
    }

    async fn pipeline(&self, app: &str, install_dir: &Path, ctx: &OpContext) -> Result<Outcome> {
        let (rom, download) = self.download_of(app)?;
        let previous = self.record(app);
        if previous.is_none()
            && std::fs::read_dir(install_dir).is_ok_and(|mut d| d.next().is_some())
        {
            bail!(
                "{} already exists and is not empty. Choose another folder, or import the game from there.",
                install_dir.display()
            );
        }
        let base = install_dir
            .parent()
            .context("invalid install folder")?
            .to_path_buf();
        let size = download.size();
        if let Some(free) = free_space(&base)
            && free < size
        {
            bail!(
                "Not enough free disk space in {}: {} needed, {} available",
                base.display(),
                util::format_bytes(size),
                util::format_bytes(free)
            );
        }
        let staging = self.staging_dir(app, &base);
        tokio::fs::create_dir_all(&staging)
            .await
            .with_context(|| format!("creating {}", staging.display()))?;

        (ctx.step)("Downloading");
        log_line(
            ctx,
            &format!(
                "downloading {} ({} files)",
                rom.title(),
                download.files.len()
            ),
        );
        let api = self.api()?;
        let entries: Vec<irori_depot::FileEntry> = download
            .files
            .iter()
            .map(|(place, file)| {
                let hash = file
                    .sha1_hash
                    .as_deref()
                    .filter(|h| h.len() == 40)
                    .and_then(|h| {
                        irori_depot::Digest::from_hex(irori_depot::Algorithm::Sha1, h).ok()
                    });
                irori_depot::FileEntry::whole(
                    place.clone(),
                    file.file_size_bytes,
                    hash,
                    vec![api.download_url(rom.id, file)],
                )
            })
            .collect();
        let settings = (self.settings)();
        let finished = irori_depot::download(
            &api.download_client()?,
            irori_depot::Job {
                root: &staging,
                files: &entries,
                remove: &[],
            },
            &irori_depot::Options {
                workers: settings.max_workers as usize,
                max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
                ..Default::default()
            },
            progress_sink(ctx),
            &ctx.cancel,
        )
        .await?;
        if finished == irori_depot::Finished::Cancelled {
            log_line(ctx, "download paused; the partial file is kept");
            return Ok(Outcome::Abort);
        }

        tokio::fs::create_dir_all(install_dir)
            .await
            .with_context(|| format!("creating {}", install_dir.display()))?;
        for (place, _) in &download.files {
            let target = irori_depot::path::join(install_dir, place);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            tokio::fs::rename(irori_depot::path::join(&staging, place), &target)
                .await
                .with_context(|| format!("moving {place} to {}", target.display()))?;
        }
        // RomM may now serve the game under other file names.
        let places: Vec<String> = download.files.iter().map(|(p, _)| p.clone()).collect();
        if let Some(old) = &previous {
            for gone in old.files().iter().filter(|p| !places.contains(p)) {
                let _ = tokio::fs::remove_file(irori_depot::path::join(install_dir, gone)).await;
            }
        }
        let record = InstalledRecord {
            title: rom.title().to_string(),
            install_path: install_dir.to_string_lossy().into_owned(),
            file: main_on_disk(&download.main, install_dir).await?,
            files: places,
            system: download.system.id.clone(),
            revision: download.revision(),
            size,
            pinned_version: previous.is_some_and(|p| p.pinned_version),
        };
        self.update_installed(|m| {
            m.insert(app.to_string(), record);
        })
        .await?;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        log_line(
            ctx,
            &format!("downloaded {} to {}", rom.title(), install_dir.display()),
        );
        Ok(Outcome::Done)
    }
}
