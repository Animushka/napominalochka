//! HTTP adapter for Yandex OAuth and Smart Home. Requests return stable integration
//! errors; upstream bodies, credentials and authorization URLs never leave this boundary.

use crate::application::yandex::ports::YandexProvider;
use crate::application::yandex::types::{Error, Result, Scenario, TokenResponse};
use futures_util::future::BoxFuture;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

pub(crate) struct Provider {
    pub(crate) http: Client,
    pub(crate) oauth_base: Url,
    pub(crate) api_base: Url,
    callback_url: Url,
    client_id: String,
    client_secret: String,
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
    pub(crate) fn new(
        client_id: String,
        client_secret: String,
        callback_url: Url,
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
            client_id,
            client_secret,
            callback_url,
        })
    }
}

impl YandexProvider for Provider {
    fn authorization_url(&self, state: &str, challenge: &str) -> Result<String> {
        let mut url = self
            .oauth_base
            .join("authorize")
            .map_err(|_| Error::Internal)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", self.callback_url.as_str())
            .append_pair("scope", "iot:view iot:control")
            .append_pair("force_confirm", "yes")
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    fn exchange<'a>(
        &'a self,
        fields: &'a [(&'a str, &'a str)],
    ) -> BoxFuture<'a, Result<TokenResponse>> {
        Box::pin(async move {
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
        })
    }

    fn scenarios<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Vec<Scenario>>> {
        Box::pin(async move {
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
        })
    }

    /// Commit the durable claim before calling this method. A request whose outcome
    /// is uncertain is never replayed, including network and response-decoding failures.
    fn action<'a>(&'a self, token: &'a str, scenario_id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
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
        })
    }
}
