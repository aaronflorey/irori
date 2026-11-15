//! RomM protocol client (RomM 5.0 or later).
//!
//! RomM is self-hosted, so every call starts from the server address the
//! user typed ([`server_url`]).
//!
//! * Sign-in ([`device`]): RomM's device authorization flow. Irori asks the
//!   server for a code, the user approves it on RomM's `/pair/device` page in
//!   the browser, and Irori's poll receives a client API token (`rmm_…`).
//! * Library ([`Client::roms`]): every ROM the account can see, page by page,
//!   with its files.
//! * Downloads ([`Client::download_url`]): one file of a ROM, sent with the
//!   token in the `Authorization` header (see [`Client::download_client`]).
//! * Firmware ([`Client::firmware`]): the BIOS and system files the server
//!   keeps for a platform, downloaded the same way ([`Client::firmware_url`]).

mod api;
pub mod device;
mod model;

pub use api::{ApiError, Client};
pub use model::{Firmware, Rom, RomFile, User};

use anyhow::{Context as _, Result, bail};
use url::Url;

/// What Irori asks RomM to allow: the account (to name it), and reading the
/// library, its files and the platforms' firmware.
pub const SCOPES: &[&str] = &["me.read", "roms.read", "platforms.read", "firmware.read"];

/// The server address the user typed, as a base URL without a trailing
/// slash. A bare host gets `https://`; a path (RomM behind a reverse proxy
/// under a prefix) is kept.
pub fn server_url(input: &str) -> Result<Url> {
    let input = input.trim().trim_end_matches('/');
    if input.is_empty() {
        bail!("Enter the address of your RomM server");
    }
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let url = Url::parse(&with_scheme)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
        .with_context(|| format!("{input} is not a web address"))?;
    let mut url = url;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// `base` joined with an absolute `path` (`/api/…`), keeping a prefix the
/// server lives under.
pub(crate) fn join(base: &Url, path: &str) -> String {
    format!("{}{path}", base.as_str().trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_server_address_the_way_people_type_it() {
        for (typed, base) in [
            ("romm.example.com", "https://romm.example.com/"),
            ("https://romm.example.com/", "https://romm.example.com/"),
            ("http://192.168.1.5:8080", "http://192.168.1.5:8080/"),
            (
                "https://home.example.com/romm/",
                "https://home.example.com/romm",
            ),
            ("  romm.lan?x=1#top ", "https://romm.lan/"),
        ] {
            assert_eq!(server_url(typed).unwrap().as_str(), base, "{typed}");
        }
        for typed in ["", "  ", "ftp://romm.lan"] {
            assert!(server_url(typed).is_err(), "{typed}");
        }
        let prefixed = server_url("https://home.example.com/romm").unwrap();
        assert_eq!(
            join(&prefixed, "/api/roms"),
            "https://home.example.com/romm/api/roms"
        );
    }
}
