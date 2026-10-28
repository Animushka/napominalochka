//! Configuration from environment variables.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use url::Url;

pub use crate::yandex::YandexConfig;

const DEFAULT_DATABASE_PATH: &str = "data/extremelynaggybot.db";
const DEFAULT_WEB_LISTEN: &str = "0.0.0.0:8080";

#[derive(Debug)]
pub struct Config {
    /// `TELEGRAM_BOT_TOKEN` (required).
    pub telegram_token: String,
    /// `ADMIN_CHATID`: chat that receives an "online" message at startup.
    pub admin_chat_id: Option<i64>,
    /// `SENTRY_DSN`: error reporting is disabled when unset.
    pub sentry_dsn: Option<String>,
    /// `DATABASE_PATH`, default `data/extremelynaggybot.db`.
    pub database_path: PathBuf,
    /// `WEB_APP_URL`: public HTTPS address of the Mini App. The app is not offered when unset.
    pub web_app_url: Option<Url>,
    /// `WEB_LISTEN`: address the web server binds to, default `0.0.0.0:8080`.
    pub web_listen: SocketAddr,
    /// Official Yandex OAuth integration; absent unless all credentials are configured.
    pub yandex: Option<YandexConfig>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let Some(telegram_token) = var("TELEGRAM_BOT_TOKEN") else {
            bail!("TELEGRAM_BOT_TOKEN environment variable is required");
        };
        let admin_chat_id = var("ADMIN_CHATID")
            .map(|id| {
                id.parse()
                    .context("ADMIN_CHATID must be an integer chat id")
            })
            .transpose()?;

        let web_app_url = var("WEB_APP_URL")
            .map(|url| {
                let url = Url::parse(&url)
                    .context("WEB_APP_URL must be a URL, e.g. https://reminders.example.com")?;
                if url.scheme() != "https" {
                    bail!("WEB_APP_URL must use https:// (Telegram requires it)");
                }
                Ok(url)
            })
            .transpose()?;
        let web_listen = var("WEB_LISTEN")
            .as_deref()
            .unwrap_or(DEFAULT_WEB_LISTEN)
            .parse()
            .context("WEB_LISTEN must be an address like 0.0.0.0:8080")?;
        let yandex = match (
            var("YANDEX_CLIENT_ID"),
            var("YANDEX_CLIENT_SECRET"),
            var("YANDEX_TOKEN_KEY"),
        ) {
            (None, None, None) => None,
            (Some(client_id), Some(client_secret), Some(key)) => {
                let key = hex::decode(key)
                    .context("YANDEX_TOKEN_KEY must be 64 hexadecimal characters")?;
                let token_key: [u8; 32] = key
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("YANDEX_TOKEN_KEY must contain 32 bytes"))?;
                let callback_url = web_app_url
                    .as_ref()
                    .context("WEB_APP_URL is required for Yandex OAuth")?
                    .join("/integrations/yandex/callback")?;
                Some(YandexConfig {
                    client_id,
                    client_secret,
                    token_key,
                    callback_url,
                })
            }
            _ => bail!(
                "YANDEX_CLIENT_ID, YANDEX_CLIENT_SECRET and YANDEX_TOKEN_KEY must be configured together"
            ),
        };

        Ok(Self {
            telegram_token,
            web_app_url,
            web_listen,
            yandex,
            admin_chat_id,
            sentry_dsn: var("SENTRY_DSN"),
            database_path: var("DATABASE_PATH")
                .map_or_else(|| DEFAULT_DATABASE_PATH.into(), PathBuf::from),
        })
    }
}

/// Reads an environment variable, treating empty values as unset.
fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}
