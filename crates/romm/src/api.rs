//! The RomM API client, authenticated with a client API token.

use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use url::Url;

use crate::model::{Firmware, Rom, User};

/// ROMs asked for per page.
const PER_PAGE: usize = 500;

/// RomM refused a request; `unauthorized` when the token itself was refused
/// (revoked, expired or lacking a permission), which only a new sign-in fixes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("RomM answered {status}: {message}")]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    pub unauthorized: bool,
}

impl ApiError {
    pub(crate) fn from_answer(status: u16, body: &str) -> Self {
        // FastAPI errors are `{"detail": "…"}`.
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("detail")?.as_str().map(str::to_string))
            .unwrap_or_else(|| body.chars().take(200).collect());
        Self {
            status,
            message,
            unauthorized: matches!(status, 401 | 403),
        }
    }
}

/// An authenticated API client. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for the server at `base` (see [`crate::server_url`]).
    pub fn new(base: Url, token: &str) -> Result<Self> {
        Ok(Self {
            http: http_client()?,
            base,
            token: token.trim().to_string(),
        })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    async fn json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(crate::join(&self.base, path))
            .header(reqwest::header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Could not reach the RomM server")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ApiError::from_answer(status.as_u16(), &text).into());
        }
        serde_json::from_str(&text)
            .with_context(|| format!("unexpected answer from RomM for {path}"))
    }

    /// The account the token belongs to.
    pub async fn me(&self) -> Result<User> {
        self.json("/api/users/me").await
    }

    /// Every ROM the account can see, with its files.
    pub async fn roms(&self) -> Result<Vec<Rom>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Page {
            items: Vec<Rom>,
        }
        let mut all = Vec::new();
        loop {
            let page: Page = self
                .json(&format!(
                    "/api/roms?limit={PER_PAGE}&offset={}&with_files=true\
                     &with_char_index=false&with_filter_values=false\
                     &with_rom_id_index=false&order_by=id",
                    all.len()
                ))
                .await?;
            let n = page.items.len();
            all.extend(page.items);
            if n < PER_PAGE {
                return Ok(all);
            }
        }
    }

    /// The firmware files of the platform with RomM id `platform_id`. A
    /// server older than the firmware endpoint answers 404, and a token
    /// without the `firmware.read` scope 403 ([`ApiError::unauthorized`]).
    pub async fn firmware(&self, platform_id: u64) -> Result<Vec<Firmware>> {
        self.json(&format!("/api/firmware?platform_id={platform_id}"))
            .await
    }

    /// A client for [`Client::download_url`]: it sends the token, and has no
    /// total timeout so large files finish.
    pub fn download_client(&self) -> Result<reqwest::Client> {
        let mut auth = reqwest::header::HeaderValue::from_str(&self.auth_header())
            .context("the RomM token is not a valid header")?;
        auth.set_sensitive(true);
        Ok(reqwest::Client::builder()
            .user_agent(concat!("Irori/", env!("CARGO_PKG_VERSION")))
            .default_headers(reqwest::header::HeaderMap::from_iter([(
                reqwest::header::AUTHORIZATION,
                auth,
            )]))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()?)
    }

    /// Where one file of a ROM downloads from. Naming the file keeps RomM
    /// from zipping the ROM's other files with it.
    pub fn download_url(&self, rom_id: u64, file: &crate::RomFile) -> String {
        let mut url = Url::parse(&crate::join(&self.base, "/api/roms")).expect("valid base");
        url.path_segments_mut()
            .expect("http URL")
            .push(&rom_id.to_string())
            .push("content")
            .push(&file.file_name);
        url.query_pairs_mut()
            .append_pair("file_ids", &file.id.to_string());
        url.into()
    }

    /// Where a firmware file downloads from.
    pub fn firmware_url(&self, firmware: &Firmware) -> String {
        let mut url = Url::parse(&crate::join(&self.base, "/api/firmware")).expect("valid base");
        url.path_segments_mut()
            .expect("http URL")
            .push(&firmware.id.to_string())
            .push("content")
            .push(&firmware.file_name);
        url.into()
    }
}

/// The client RomM calls use: Irori's user agent and timeouts.
pub(crate) fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Irori/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    fn client(server: &TestServer) -> Client {
        Client::new(crate::server_url(&server.base()).unwrap(), "rmm_TOKEN").unwrap()
    }

    #[tokio::test]
    async fn sends_the_token_and_pages_through_the_library() {
        let server = TestServer::start(|req| {
            if req.header("authorization") != Some("Bearer rmm_TOKEN") {
                return Resp {
                    body: br#"{"detail":"Invalid token"}"#.to_vec(),
                    ..Resp::status(401)
                };
            }
            let path = req.path.as_str();
            if path == "/api/users/me" {
                Resp::json(json!({"id": 3, "username": "player", "email": null}))
            } else if path.starts_with("/api/roms?limit=500&offset=0&") {
                let items: Vec<_> = (0..PER_PAGE)
                    .map(|i| json!({"id": i, "fs_name_no_ext": format!("rom {i}")}))
                    .collect();
                Resp::json(json!({"items": items, "total": 501, "limit": 500, "offset": 0}))
            } else if path.starts_with("/api/roms?limit=500&offset=500&") {
                Resp::json(json!({"items": [{"id": 500, "fs_name_no_ext": "last"}]}))
            } else {
                Resp::status(404)
            }
        });
        let api = client(&server);
        assert_eq!(api.me().await.unwrap().username, "player");
        let roms = api.roms().await.unwrap();
        assert_eq!(roms.len(), 501);
        assert_eq!(roms[500].title(), "last");

        let refused = Client::new(api.base().clone(), "rmm_OTHER").unwrap();
        let err = refused.me().await.unwrap_err();
        let api_err = err.downcast_ref::<ApiError>().unwrap();
        assert!(api_err.unauthorized);
        assert_eq!(api_err.message, "Invalid token");
    }

    #[tokio::test]
    async fn lists_a_platforms_firmware() {
        let server = TestServer::start(|req| {
            if req.path == "/api/firmware?platform_id=7" {
                Resp::json(json!([{
                    "id": 3, "platform_id": 7, "file_name": "scph5501.bin",
                    "file_size_bytes": 524288, "md5_hash": "490f666e1afb15b7362b406ed1cea246",
                    "sha1_hash": "", "is_verified": true, "missing_from_fs": false,
                    "unknown_field": 1
                }]))
            } else {
                Resp::status(404)
            }
        });
        let api = client(&server);
        let list = api.firmware(7).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].file_name, "scph5501.bin");
        assert_eq!(list[0].md5_hash, "490f666e1afb15b7362b406ed1cea246");
        assert_eq!(
            api.firmware_url(&list[0]),
            format!("{}/api/firmware/3/content/scph5501.bin", server.base())
        );
        let err = api.firmware(8).await.unwrap_err();
        assert_eq!(err.downcast_ref::<ApiError>().unwrap().status, 404);
    }

    #[test]
    fn download_addresses_name_one_file() {
        let api = Client::new(
            crate::server_url("https://home.example.com/romm").unwrap(),
            "t",
        )
        .unwrap();
        let file = crate::RomFile {
            id: 9,
            file_name: "Chrono Trigger (USA) #1.sfc".into(),
            ..Default::default()
        };
        assert_eq!(
            api.download_url(4, &file),
            "https://home.example.com/romm/api/roms/4/content/Chrono%20Trigger%20(USA)%20%231.sfc?file_ids=9"
        );
    }
}
