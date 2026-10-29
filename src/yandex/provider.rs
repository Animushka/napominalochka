//! HTTP adapter for Yandex OAuth and Smart Home. Requests return stable integration
//! errors; upstream bodies, credentials and authorization URLs never leave this boundary.

use super::{Error, Result, Scenario, YandexConfig};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

pub(super) struct Provider {
    pub(super) http: Client,
    pub(super) oauth_base: Url,
    pub(super) api_base: Url,
    client_id: String,
    client_secret: String,
}

#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    pub(super) refresh_token: Option<String>,
    pub(super) token_type: String,
    pub(super) expires_in: i64,
    pub(super) scope: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    status: String,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct ActionResponse {
    status: String,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
}

impl Provider {
    pub(super) fn new(
        config: &YandexConfig,
        oauth_base: Url,
        api_base: Url,
    ) -> anyhow::Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self {
            http,
            oauth_base,
            api_base,
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
        })
    }

    pub(super) async fn exchange(&self, fields: &[(&str, &str)]) -> Result<TokenResponse> {
        let url = self.oauth_base.join("token").map_err(|_| Error::Internal)?;
        let response = self
            .http
            .post(url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(fields)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::RateLimited);
        }
        if response.status().is_client_error() {
            // A rate limit, malformed request or app-level problem does not prove the
            // user's refresh grant is revoked. Preserve their credentials in those cases.
            let error: TokenError = response.json().await.map_err(|_| Error::Unavailable)?;
            return Err(match error.error.as_str() {
                "invalid_grant" | "invalid_scope" => Error::ReconnectRequired,
                _ => Error::Unavailable,
            });
        }
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        response.json().await.map_err(|_| Error::Unavailable)
    }

    pub(super) async fn scenarios(&self, token: &str) -> Result<Vec<Scenario>> {
        let url = self
            .api_base
            .join("v1.0/user/info")
            .map_err(|_| Error::Internal)?;
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(Error::ReconnectRequired);
            }
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
            status if !status.is_success() => return Err(Error::Unavailable),
            _ => {}
        }
        let info: UserInfo = response.json().await.map_err(|_| Error::Unavailable)?;
        if info.status != "ok" {
            return Err(Error::Unavailable);
        }
        Ok(info.scenarios)
    }

    /// Commit the durable claim before calling this method. A request whose outcome
    /// is uncertain is never replayed, including network and response-decoding failures.
    pub(super) async fn action(&self, token: &str, scenario_id: &str) -> Result<()> {
        let mut url = self.api_base.clone();
        url.path_segments_mut()
            .map_err(|_| Error::Internal)?
            .extend(["v1.0", "scenarios", scenario_id, "actions"]);
        // No retry, even for a network error or server failure: the action might have run.
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::DeliveryUncertain)?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(Error::ReconnectRequired);
            }
            StatusCode::NOT_FOUND => return Err(Error::ScenarioMissing),
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
            status if !status.is_success() => return Err(Error::DeliveryUncertain),
            _ => {}
        }
        let body: ActionResponse = response
            .json()
            .await
            .map_err(|_| Error::DeliveryUncertain)?;
        if body.status != "ok" {
            return Err(Error::DeliveryUncertain);
        }
        Ok(())
    }
}
