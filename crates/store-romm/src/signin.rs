//! The sign-in the login dialog draws: [`SignIn::start`] asks the server
//! for a code, the user approves it on [`SignIn::url`] in the browser, and
//! [`SignIn::poll`] collects the token, which [`SignIn::login_input`] hands
//! to [`irori_core::Store::login`].

use std::time::Duration;

use anyhow::Result;
use irori_core::{Paths, Runner};
use irori_romm_api::device::{self, DeviceCode, DeviceInfo};
use serde::{Deserialize, Serialize};
use url::Url;

pub use irori_romm_api::device::{DeviceError, Poll};

use crate::records::{self, Layout};

/// What [`irori_core::Store::login`] takes once RomM approved the device.
#[derive(Serialize, Deserialize)]
pub(crate) struct SignedIn {
    pub server: String,
    pub token: String,
}

/// A started sign-in. Cheap to clone.
#[derive(Debug, Clone)]
pub struct SignIn {
    server: Url,
    code: DeviceCode,
}

impl SignIn {
    /// Ask the server the user typed to sign this installation in.
    pub async fn start(server: &str) -> Result<Self> {
        let server = irori_romm_api::server_url(server)?;
        let paths = Paths::get();
        let layout = Layout {
            config: paths.store_config_dir(Runner::Romm),
            cache: paths.store_cache_dir(Runner::Romm),
        };
        let id = tokio::task::spawn_blocking(move || records::device_id(&layout)).await??;
        let host = sysinfo::System::host_name().filter(|h| !h.trim().is_empty());
        let info = DeviceInfo {
            client_device_identifier: id,
            name: host.map_or_else(|| "Irori".to_string(), |h| format!("Irori on {h}")),
            client: "irori".into(),
            platform: Some(std::env::consts::OS.into()),
            client_version: Some(env!("CARGO_PKG_VERSION").into()),
        };
        let code = device::start(&server, &info).await?;
        Ok(Self { server, code })
    }

    /// RomM's approval page for this sign-in.
    pub fn url(&self) -> &str {
        &self.code.verification_url
    }

    /// The code the approval page shows, to match it with this dialog.
    pub fn user_code(&self) -> &str {
        &self.code.user_code
    }

    /// How long to wait between polls.
    pub fn interval(&self) -> Duration {
        self.code.interval
    }

    /// Whether the user approved; [`Poll::Approved`]'s token goes to
    /// [`SignIn::login_input`].
    pub async fn poll(&self) -> Result<Poll> {
        device::poll(&self.server, &self.code).await
    }

    /// The login input for the token RomM issued.
    pub fn login_input(&self, token: &str) -> String {
        serde_json::to_string(&SignedIn {
            server: self.server.to_string(),
            token: token.to_string(),
        })
        .expect("plain strings serialize")
    }
}
