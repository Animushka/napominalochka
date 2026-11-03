//! Construct application services from concrete adapters at the outermost layer.
use super::config::YandexConfig;
use crate::adapters::yandex::{crypto::TokenCipher, provider::Provider};
use crate::application::yandex::YandexService;
use crate::infrastructure::sqlite::Db;
use url::Url;

pub(crate) fn yandex(config: YandexConfig, db: Db) -> anyhow::Result<YandexService> {
    yandex_with_endpoints(
        config,
        db,
        Url::parse("https://oauth.yandex.ru/")?,
        Url::parse("https://api.iot.yandex.net/")?,
    )
}
pub(crate) fn yandex_with_endpoints(
    config: YandexConfig,
    db: Db,
    oauth_base: Url,
    api_base: Url,
) -> anyhow::Result<YandexService> {
    let provider = Provider::new(
        config.client_id,
        config.client_secret,
        config.callback_url,
        oauth_base,
        api_base,
    )?;
    let cipher = TokenCipher::new(&config.token_key)?;
    Ok(YandexService::from_dependencies(db, provider, cipher))
}
