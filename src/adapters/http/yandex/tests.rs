use super::*;
use crate::application::ReminderService;
use crate::infrastructure::sqlite::Db;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

fn app() -> Router {
    crate::adapters::http::router(Arc::new(WebState {
        service: ReminderService::new(Db::open_in_memory().unwrap()),
        bot_token: "123456:test-token".into(),
        bot_username: "custom_reminder_bot".into(),
        health: crate::application::health::Health::new(),
        web_app_url: Some(url::Url::parse("https://example.com").unwrap()),
        yandex: None,
    }))
}

#[test]
fn telegram_links_use_the_current_bot_and_encode_receipts() {
    let receipt = "a".repeat(48);
    assert_eq!(
        telegram_link("another_bot", Some(&receipt)),
        format!("https://t.me/another_bot?start=yandex_{receipt}")
    );
    assert_eq!(
        telegram_link("another_bot", None),
        "https://t.me/another_bot"
    );
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
    let response = app()
        .oneshot(
            Request::builder()
                .uri(
                    "/integrations/yandex/result?outcome=%3Cscript%3Eevil%3C/script%3E&language=en",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let policy = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(policy.contains("style-src 'self'"));
    assert!(policy.contains("script-src 'self'"));
    assert!(!policy.contains("unsafe-inline"));
    let body = String::from_utf8(
        to_bytes(response.into_body(), 100000)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("Could not connect Yandex"));
    assert!(body.contains("https://t.me/custom_reminder_bot"));
    assert!(!body.contains("https://t.me/napominalozhka_bot"));
    assert!(!body.contains("evil"));
    assert!(body.contains("/appearance.js"));
    assert!(body.contains("<link rel=\"stylesheet\" href=\"/assets/"));
    assert!(!body.contains("<style"));
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
