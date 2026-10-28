//! Per-user Mini App endpoints and the public OAuth return page.
use crate::i18n::Lang;
use crate::web::{AuthUser, WebState};
use crate::yandex;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/yandex", get(status).put(configure).delete(disconnect))
        .route("/api/yandex/connect", post(connect))
        .route("/api/yandex/scenarios", get(scenarios))
        .route("/api/yandex/test", post(test_scenario))
        .route("/integrations/yandex/callback", get(callback))
        .route("/integrations/yandex/result", get(result_page))
        .route("/privacy", get(privacy_page))
        .layer(middleware::map_response(private_response))
}
async fn private_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}
async fn language(state: &WebState, auth: &AuthUser) -> Lang {
    state.service.preferences(auth.id).await.map_or_else(
        |_| Lang::parse(auth.language_code.as_deref().unwrap_or("ru")),
        |preferences| Lang::parse(&preferences.language),
    )
}
struct ApiError {
    status: StatusCode,
    code: &'static str,
    language: Lang,
}
impl ApiError {
    fn unavailable(language: Lang) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "yandex_unavailable",
            language,
        }
    }
    fn from_yandex(error: yandex::Error, language: Lang) -> Self {
        Self {
            status: StatusCode::from_u16(error.status_code()).unwrap_or(StatusCode::BAD_GATEWAY),
            code: error.code(),
            language,
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match (self.language, self.code) {
            (Lang::Ru, "yandex_not_connected") => "Сначала подключите Яндекс.",
            (Lang::Ru, "yandex_reconnect_required") => "Подключите Яндекс заново.",
            (Lang::Ru, "yandex_scenario_missing" | "yandex_invalid_selection") => {
                "Выберите доступный сценарий в своём аккаунте Яндекса."
            }
            (Lang::Ru, "yandex_rate_limited") => "Подождите минуту перед следующей проверкой.",
            (Lang::Ru, "yandex_delivery_uncertain") => {
                "Ответ от Яндекса не получен. Проверьте колонку: сценарий мог уже запуститься."
            }
            (Lang::Ru, "yandex_invalid_state") => {
                "Ссылка для входа истекла. Начните подключение заново."
            }
            (Lang::Ru, _) => "Не удалось связаться с Яндексом. Попробуйте позже.",
            (Lang::En, "yandex_not_connected") => "Connect your Yandex account first.",
            (Lang::En, "yandex_reconnect_required") => "Please reconnect your Yandex account.",
            (Lang::En, "yandex_scenario_missing" | "yandex_invalid_selection") => {
                "Choose an available scenario from your own Yandex account."
            }
            (Lang::En, "yandex_rate_limited") => "Wait a minute before trying again.",
            (Lang::En, "yandex_delivery_uncertain") => {
                "Yandex did not respond. Check your speaker: the scenario may have already run."
            }
            (Lang::En, "yandex_invalid_state") => {
                "This sign-in link has expired. Start connecting again."
            }
            (Lang::En, _) => "Could not reach Yandex. Please try again later.",
        };
        (
            self.status,
            Json(json!({"error": message, "code": self.code})),
        )
            .into_response()
    }
}
async fn status(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let Some(integration) = &state.yandex else {
        return Ok(Json(
            json!({"available":false,"connected":false,"enabled":false,"requires_reconnect":false,"scenario_id":null,"scenario_name":null,"last_error":null}),
        ));
    };
    let lang = language(&state, &auth).await;
    let value = integration
        .status(auth.id)
        .await
        .map_err(|e| ApiError::from_yandex(e, lang))?;
    Ok(Json(json!(value)))
}
async fn connect(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let lang = language(&state, &auth).await;
    let integration = state
        .yandex
        .as_ref()
        .ok_or_else(|| ApiError::unavailable(lang))?;
    let url = integration
        .start_auth(auth.id, lang.code())
        .await
        .map_err(|e| ApiError::from_yandex(e, lang))?;
    Ok(Json(json!({"url":url})))
}
async fn scenarios(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let lang = language(&state, &auth).await;
    let integration = state
        .yandex
        .as_ref()
        .ok_or_else(|| ApiError::unavailable(lang))?;
    let values = integration
        .scenarios(auth.id)
        .await
        .map_err(|e| ApiError::from_yandex(e, lang))?;
    Ok(Json(json!({"scenarios": values})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    scenario_id: String,
    enabled: bool,
}
async fn configure(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
    Json(input): Json<Selection>,
) -> Result<Json<Value>, ApiError> {
    let lang = language(&state, &auth).await;
    if input.scenario_id.len() > 256 {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "yandex_invalid_selection",
            language: lang,
        });
    }
    let integration = state
        .yandex
        .as_ref()
        .ok_or_else(|| ApiError::unavailable(lang))?;
    let value = integration
        .configure(auth.id, &input.scenario_id, input.enabled)
        .await
        .map_err(|e| ApiError::from_yandex(e, lang))?;
    Ok(Json(json!(value)))
}
async fn test_scenario(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let lang = language(&state, &auth).await;
    let integration = state
        .yandex
        .as_ref()
        .ok_or_else(|| ApiError::unavailable(lang))?;
    integration
        .test(auth.id)
        .await
        .map_err(|e| ApiError::from_yandex(e, lang))?;
    Ok(Json(json!({"ok":true})))
}
async fn disconnect(
    State(state): State<Arc<WebState>>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    let lang = language(&state, &auth).await;
    let integration = state
        .yandex
        .as_ref()
        .ok_or_else(|| ApiError::unavailable(lang))?;
    integration
        .disconnect(auth.id)
        .await
        .map_err(|e| ApiError::from_yandex(e, lang))?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
struct OAuthCallback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
async fn callback(
    State(state): State<Arc<WebState>>,
    Query(query): Query<OAuthCallback>,
) -> Response {
    let mut lang = Lang::Ru;
    let mut outcome = "error";
    if let (Some(integration), Some(nonce)) = (&state.yandex, query.state.as_deref())
        && nonce.len() <= 256
    {
        if query.error.is_some() {
            if let Ok(language) = integration.cancel_auth(nonce).await {
                lang = Lang::parse(&language);
                outcome = "cancelled";
            }
        } else if let Some(code) = query
            .code
            .as_deref()
            .filter(|code| !code.is_empty() && code.len() <= 2048)
            && let Ok(pending) = integration.prepare_auth(nonce, code).await
        {
            lang = Lang::parse(&pending.language);
            if pending.completion_token.len() <= 48
                && pending
                    .completion_token
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
            {
                let (title, text, button) = if lang == Lang::Ru {
                    (
                        "Остался один шаг 💛",
                        "Подтвердите подключение в том же Telegram-аккаунте, из которого начали вход. После этого можно выбрать сценарий и проверить колонку.",
                        "Подтвердить в Telegram",
                    )
                } else {
                    (
                        "One more step 💛",
                        "Confirm the connection using the same Telegram account that started it. Then choose your scenario and test the speaker.",
                        "Confirm in Telegram",
                    )
                };
                return public_page(
                    lang,
                    title,
                    &format!(
                        "<p>{text}</p><a class=button href=\"https://t.me/napominalozhka_bot?start=yandex_{}\">{button}</a>",
                        pending.completion_token
                    ),
                );
            }
        }
    }
    Redirect::to(&format!(
        "/integrations/yandex/result?outcome={outcome}&language={}",
        lang.code()
    ))
    .into_response()
}
#[derive(Deserialize)]
struct PageQuery {
    language: Option<String>,
    outcome: Option<String>,
}
async fn result_page(Query(query): Query<PageQuery>) -> Response {
    let language = Lang::parse(query.language.as_deref().unwrap_or("ru"));
    let (title, text) = match (language, query.outcome.as_deref()) {
        (Lang::Ru, Some("cancelled")) => (
            "Подключение отменено",
            "Можно вернуться в Напоминалочку и подключить Алису позже.",
        ),
        (Lang::En, Some("cancelled")) => (
            "Connection cancelled",
            "You can return to Napominalochka and connect Alice later.",
        ),
        (Lang::Ru, _) => (
            "Не удалось подключить Яндекс",
            "Вернитесь в Напоминалочку и начните подключение заново. Если ошибка повторится, попробуйте позже.",
        ),
        (Lang::En, _) => (
            "Could not connect Yandex",
            "Return to Napominalochka and start connecting again. If it still fails, try again later.",
        ),
    };
    let button = if language == Lang::Ru {
        "Вернуться в Telegram"
    } else {
        "Return to Telegram"
    };
    public_page(
        language,
        title,
        &format!(
            "<p>{text}</p><a class=button href=\"https://t.me/napominalozhka_bot\">{button}</a>"
        ),
    )
}
async fn privacy_page() -> Response {
    public_page(
        Lang::Ru,
        "Данные / Your data",
        include_str!("../web/privacy.html"),
    )
}
fn public_page(language: Lang, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"{}\"><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><meta name=referrer content=no-referrer><title>{title} · Напоминалочка</title><style>:root{{color-scheme:light dark}}*{{box-sizing:border-box}}body{{margin:0;background:#f6f7ef;color:#24352e;font:17px/1.6 system-ui,sans-serif}}main{{max-width:680px;margin:8vh auto;padding:32px;border-radius:28px;background:#fffdf7}}h1{{font-size:28px;line-height:1.2}}h2{{font-size:20px;margin-top:28px}}a{{color:#316b55}}.button{{display:inline-block;padding:12px 20px;border-radius:16px;background:#d1eddd;text-decoration:none;font-weight:600}}@media(max-width:720px){{main{{margin:20px 12px;padding:24px}}}}@media(prefers-color-scheme:dark){{body{{background:#17231d;color:#e4eee6}}main{{background:#23332b}}a{{color:#b3dfc4}}.button{{background:#345344}}}}</style><main><h1>{title}</h1>{body}</main></html>",
        language.code()
    );
    let mut response = Html(html).into_response();
    response.headers_mut().insert("content-security-policy", HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Db, service::Service};
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    fn app() -> Router {
        crate::web::router(Arc::new(WebState {
            service: Service::new(Db::open_in_memory().unwrap()),
            bot_token: "123456:test-token".into(),
            web_app_url: Some(url::Url::parse("https://example.com").unwrap()),
            yandex: None,
        }))
    }

    #[tokio::test]
    async fn every_yandex_operation_requires_telegram_authentication() {
        for (method, path) in [
            ("GET", "/api/yandex"),
            ("POST", "/api/yandex/connect"),
            ("GET", "/api/yandex/scenarios"),
            ("PUT", "/api/yandex"),
            ("POST", "/api/yandex/test"),
            ("DELETE", "/api/yandex"),
        ] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            r#"{"scenario_id":"someone-elses","enabled":true}"#,
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store"
            );
        }
    }

    #[tokio::test]
    async fn invalid_callback_does_not_reflect_secrets_or_remote_errors() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/integrations/yandex/callback?state=SECRET&code=SECRET_CODE&error=evil")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            location,
            "/integrations/yandex/result?outcome=error&language=ru"
        );
        assert_eq!(
            response.headers().get("referrer-policy").unwrap(),
            "no-referrer"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
    }

    #[tokio::test]
    async fn return_page_does_not_reflect_untrusted_query_and_privacy_is_public() {
        let response = app().oneshot(Request::builder()
            .uri("/integrations/yandex/result?outcome=%3Cscript%3Eevil%3C/script%3E&language=en")
            .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get("content-security-policy").is_some());
        let body = String::from_utf8(
            to_bytes(response.into_body(), 100000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains("Could not connect Yandex"));
        assert!(!body.contains("evil"));
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/privacy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
