//! RomM's device authorization flow (`/api/auth/device/*`, RomM 5.0+),
//! modelled on RFC 8628.
//!
//! [`start`] registers the request and returns the page where the user
//! approves it; [`poll`] asks, no faster than [`DeviceCode::interval`],
//! whether they did. The device code is the secret that collects the token;
//! the user code only names the request on the approval page.

use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::api::{ApiError, http_client};

/// What the approval page shows about this device.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// Stable per installation, so signing in again replaces this device
    /// in RomM's list instead of adding one.
    pub client_device_identifier: String,
    pub name: String,
    pub client: String,
    pub platform: Option<String>,
    pub client_version: Option<String>,
}

/// A started sign-in.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// The approval page, with the user code filled in.
    pub verification_url: String,
    pub expires_in: Duration,
    pub interval: Duration,
}

/// The token RomM issued.
#[derive(Clone, Deserialize)]
pub struct DeviceToken {
    pub access_token: String,
    #[serde(default)]
    pub device_id: String,
}

impl std::fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceToken")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// One answer of [`poll`].
#[derive(Debug)]
pub enum Poll {
    /// Not approved yet: ask again after the interval.
    Pending,
    /// Asked too soon: wait longer before the next poll.
    SlowDown,
    Approved(DeviceToken),
}

/// Why the sign-in ended without a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("The sign-in was declined in RomM")]
    Denied,
    #[error("The sign-in code expired before it was approved")]
    Expired,
    #[error("This RomM server can't sign in devices. Irori needs RomM 5.0 or later.")]
    Unsupported,
}

#[derive(Deserialize)]
struct InitAnswer {
    device_code: String,
    user_code: String,
    verification_path_complete: String,
    expires_in: u64,
    interval: u64,
}

/// Ask `server` to sign this device in with [`crate::SCOPES`].
pub async fn start(server: &Url, device: &DeviceInfo) -> Result<DeviceCode> {
    #[derive(Serialize)]
    struct Request<'a> {
        #[serde(flatten)]
        device: &'a DeviceInfo,
        requested_scopes: &'a [&'a str],
    }
    let resp = http_client()?
        .post(crate::join(server, "/api/auth/device/init"))
        .json(&Request {
            device,
            requested_scopes: crate::SCOPES,
        })
        .send()
        .await
        .context("Could not reach the RomM server")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    // Before 5.0 the path falls through to RomM's web app or a 404/405.
    if matches!(status.as_u16(), 404 | 405) {
        return Err(DeviceError::Unsupported.into());
    }
    if !status.is_success() {
        return Err(ApiError::from_answer(status.as_u16(), &text).into());
    }
    let answer: InitAnswer = serde_json::from_str(&text).map_err(|_| DeviceError::Unsupported)?;
    Ok(DeviceCode {
        verification_url: crate::join(server, &answer.verification_path_complete),
        device_code: answer.device_code,
        user_code: answer.user_code,
        expires_in: Duration::from_secs(answer.expires_in),
        interval: Duration::from_secs(answer.interval.max(1)),
    })
}

/// Whether the user approved `code` yet.
pub async fn poll(server: &Url, code: &DeviceCode) -> Result<Poll> {
    let resp = http_client()?
        .post(crate::join(server, "/api/auth/device/token"))
        .json(&serde_json::json!({ "device_code": code.device_code }))
        .send()
        .await
        .context("Could not reach the RomM server")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        let token: DeviceToken =
            serde_json::from_str(&text).context("unexpected answer from RomM's sign-in")?;
        return Ok(Poll::Approved(token));
    }
    let error = ApiError::from_answer(status.as_u16(), &text);
    match (status.as_u16(), error.message.as_str()) {
        (400, "authorization_pending") => Ok(Poll::Pending),
        // RomM also answers 429 when one address polls too often.
        (400, "slow_down") | (429, _) => Ok(Poll::SlowDown),
        (400, "access_denied") => Err(DeviceError::Denied.into()),
        (400, "expired_token") => Err(DeviceError::Expired.into()),
        _ => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use irori_depot::test_server::{Resp, TestServer};
    use serde_json::json;

    fn device() -> DeviceInfo {
        DeviceInfo {
            client_device_identifier: "id-1".into(),
            name: "Irori".into(),
            client: "irori".into(),
            platform: Some("linux".into()),
            client_version: Some("1.0".into()),
        }
    }

    fn detail(status: u16, detail: &str) -> Resp {
        Resp {
            body: json!({ "detail": detail }).to_string().into_bytes(),
            ..Resp::status(status)
        }
    }

    #[tokio::test]
    async fn signs_in_once_the_user_approves() {
        let polls = Arc::new(AtomicUsize::new(0));
        let seen = polls.clone();
        let server = TestServer::start(move |req| match req.path.as_str() {
            "/romm/api/auth/device/init" => {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["client_device_identifier"], "id-1");
                assert_eq!(body["requested_scopes"], json!(crate::SCOPES));
                Resp {
                    status: 201,
                    ..Resp::json(json!({
                        "device_code": "DC",
                        "user_code": "ABCD1234",
                        "verification_path": "/pair/device",
                        "verification_path_complete": "/pair/device?user_code=ABCD1234",
                        "expires_in": 600,
                        "interval": 5
                    }))
                }
            }
            "/romm/api/auth/device/token" => match seen.fetch_add(1, Ordering::SeqCst) {
                0 => detail(400, "authorization_pending"),
                1 => detail(400, "slow_down"),
                _ => Resp::json(json!({
                    "access_token": "rmm_abc",
                    "device_id": "dev-1",
                    "scopes": ["roms.read"],
                    "expires_at": null
                })),
            },
            _ => Resp::status(404),
        });
        let base = crate::server_url(&format!("{}/romm", server.base())).unwrap();
        let code = start(&base, &device()).await.unwrap();
        assert_eq!(
            code.verification_url,
            format!("{}/romm/pair/device?user_code=ABCD1234", server.base())
        );
        assert_eq!(code.interval, Duration::from_secs(5));
        assert!(matches!(poll(&base, &code).await.unwrap(), Poll::Pending));
        assert!(matches!(poll(&base, &code).await.unwrap(), Poll::SlowDown));
        let Poll::Approved(token) = poll(&base, &code).await.unwrap() else {
            panic!("expected a token");
        };
        assert_eq!(token.access_token, "rmm_abc");
        assert!(!format!("{token:?}").contains("rmm_abc"));
    }

    #[tokio::test]
    async fn reports_declined_and_expired_codes_and_old_servers() {
        let server = TestServer::start(|req| match req.path.as_str() {
            "/a/api/auth/device/token" => detail(400, "access_denied"),
            "/b/api/auth/device/token" => detail(400, "expired_token"),
            _ => Resp::status(404),
        });
        let code = DeviceCode {
            device_code: "DC".into(),
            user_code: "U".into(),
            verification_url: String::new(),
            expires_in: Duration::from_secs(1),
            interval: Duration::from_secs(1),
        };
        let at = |prefix: &str| crate::server_url(&format!("{}{prefix}", server.base())).unwrap();
        let denied = poll(&at("/a"), &code).await.unwrap_err();
        assert_eq!(denied.downcast_ref(), Some(&DeviceError::Denied));
        let expired = poll(&at("/b"), &code).await.unwrap_err();
        assert_eq!(expired.downcast_ref(), Some(&DeviceError::Expired));
        let old = start(&at("/c"), &device()).await.unwrap_err();
        assert_eq!(old.downcast_ref(), Some(&DeviceError::Unsupported));
    }
}
