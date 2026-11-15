//! BIOS files and system firmware from the RomM server. What the game's
//! system lacks (see [`irori_engines::bios::wanted`]) is downloaded from the
//! firmware RomM keeps for the game's platform into the BIOS folder, in a
//! `RomM` folder of its own, and the BIOS index is refreshed so the next
//! launch finds it. Files the user already has are never fetched or replaced.
//!
//! This is best effort: a game doesn't fail to install or start because
//! RomM has no firmware, so every problem here is logged, not returned.

use std::sync::atomic::Ordering;

use irori_core::{Event, Notice, NoticeLevel, OpContext};
use irori_engines::bios::{self, Offer};
use irori_engines::systems::System;
use irori_romm_api::{ApiError, Rom};

use crate::RommStore;
use crate::install::log_line;

/// Where RomM's files go below the BIOS folder.
const FOLDER: &str = "RomM";

impl RommStore {
    /// Whether this is the first time this run that firmware is looked for
    /// for `system`. Launches ask only then; installs always ask.
    pub(crate) fn first_firmware_check(&self, system: &str) -> bool {
        self.firmware_checked.lock().insert(system.to_string())
    }

    /// Download what `system` lacks from the firmware of `rom`'s platform.
    pub(crate) async fn fetch_firmware(&self, rom: &Rom, system: &'static System, ctx: &OpContext) {
        if rom.platform_id == 0 || self.api().is_err() {
            return;
        }
        self.firmware_checked.lock().insert(system.id.clone());
        if let Err(e) = self.try_fetch_firmware(rom, system, ctx).await {
            if ctx.cancel.is_cancelled() {
                return;
            }
            tracing::warn!("RomM firmware for {}: {e:#}", system.name);
            log_line(
                ctx,
                &format!("firmware for {} not fetched: {e:#}", system.name),
            );
        }
    }

    async fn try_fetch_firmware(
        &self,
        rom: &Rom,
        system: &'static System,
        ctx: &OpContext,
    ) -> anyhow::Result<()> {
        use anyhow::Context as _;

        let settings = (self.settings)();
        let root = bios::folder(&settings.bios_folder);
        let index = {
            let root = root.clone();
            Self::blocking(move || bios::scan(&root, &bios::cached_index())).await??
        };
        let lacks = {
            let (root, index) = (root.clone(), index.clone());
            Self::blocking(move || bios::lacks(system, &root, &index)).await?
        };
        if !lacks {
            return Ok(());
        }

        let api = self.api()?;
        let offered = match api.firmware(rom.platform_id).await {
            Ok(list) => list,
            Err(e) => {
                let denied = e.downcast_ref::<ApiError>().is_some_and(|a| a.unauthorized);
                if denied && !self.firmware_denied_told.swap(true, Ordering::Relaxed) {
                    self.bus.notice(
                        Notice::new(
                            NoticeLevel::Warning,
                            "RomM didn’t let Irori read its firmware. Sign in to RomM again to allow it.",
                        )
                        .keys("shell.notice.romm_firmware_denied", None, &[])
                        .account(irori_core::Runner::Romm),
                    );
                }
                return Err(e.context("asking RomM for the platform's firmware"));
            }
        };
        let offered: Vec<_> = offered
            .into_iter()
            .filter(|f| !f.missing_from_fs && !f.file_name.is_empty())
            .collect();
        let offers: Vec<(String, String)> = offered
            .iter()
            .map(|f| (f.file_name.clone(), f.md5_hash.to_lowercase()))
            .collect();
        let picked = {
            let (root, index) = (root.clone(), index.clone());
            Self::blocking(move || {
                let offers: Vec<Offer> = offers
                    .iter()
                    .map(|(name, md5)| Offer { name, md5 })
                    .collect();
                bios::wanted(system, &root, &index, &offers)
            })
            .await?
        };
        if picked.is_empty() {
            return Ok(());
        }

        (ctx.step)("Downloading BIOS files");
        let entries: Vec<irori_depot::FileEntry> = picked
            .iter()
            .map(|&i| {
                let file = &offered[i];
                irori_depot::FileEntry::whole(
                    place(&file.file_name),
                    file.file_size_bytes,
                    digest(file),
                    vec![api.firmware_url(file)],
                )
            })
            .collect();
        for entry in &entries {
            log_line(ctx, &format!("downloading firmware {}", entry.path));
        }
        let finished = irori_depot::download(
            &api.download_client()?,
            irori_depot::Job {
                root: &root,
                files: &entries,
                remove: &[],
            },
            &irori_depot::Options {
                workers: settings.max_workers as usize,
                max_bytes_per_sec: u64::from(settings.max_download_speed) * 1024,
                ..Default::default()
            },
            irori_depot::ignore_progress(),
            &ctx.cancel,
        )
        .await
        .context("downloading the firmware")?;
        if finished == irori_depot::Finished::Cancelled {
            return Ok(());
        }
        Self::blocking(move || bios::scan(&root, &bios::cached_index())).await??;
        self.bus.emit(Event::EnginesChanged);
        Ok(())
    }
}

/// A firmware file's place below the BIOS folder.
fn place(file_name: &str) -> String {
    format!("{FOLDER}/{}", irori_core::paths::sanitize(file_name))
}

/// The digest RomM gives for a firmware file, when it gives a usable one.
fn digest(file: &irori_romm_api::Firmware) -> Option<irori_depot::Digest> {
    let (algorithm, hex) = match (file.sha1_hash.len(), file.md5_hash.len()) {
        (40, _) => (irori_depot::Algorithm::Sha1, &file.sha1_hash),
        (_, 32) => (irori_depot::Algorithm::Md5, &file.md5_hash),
        _ => return None,
    };
    irori_depot::Digest::from_hex(algorithm, hex).ok()
}
