mod features;

use super::*;
use crate::db::Db;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

const TOKEN: &str = "123456:test-token";

/// Builds `initData` signed the way Telegram does.
fn sign(fields: &[(&str, &str)], token: &str) -> String {
    let mut sorted: Vec<_> = fields.to_vec();
    sorted.sort();
    let check = sorted
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut secret = HmacSha256::new_from_slice(b"WebAppData").unwrap();
    secret.update(token.as_bytes());
    let mut mac = HmacSha256::new_from_slice(&secret.finalize().into_bytes()).unwrap();
    mac.update(check.as_bytes());
    let hash = hex::encode(mac.finalize().into_bytes());

    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (k, v) in fields {
        serializer.append_pair(k, v);
    }
    serializer.append_pair("hash", &hash);
    serializer.finish()
}

fn init_data(user_id: i64, now: DateTime<Utc>) -> String {
    let user = format!(r#"{{"id":{user_id},"first_name":"Ada","last_name":"Lovelace"}}"#);
    sign(
        &[
            ("auth_date", &now.timestamp().to_string()),
            ("query_id", "AAH"),
            ("user", &user),
        ],
        TOKEN,
    )
}

fn init_data_with_language(user_id: i64, language: &str, now: DateTime<Utc>) -> String {
    let user = serde_json::json!({ "id": user_id, "first_name": "Ada", "language_code": language })
        .to_string();
    sign(
        &[
            ("auth_date", &now.timestamp().to_string()),
            ("query_id", "AAH"),
            ("user", &user),
        ],
        TOKEN,
    )
}

#[test]
fn accepts_valid_init_data() {
    let now = Utc::now();
    assert_eq!(
        verify_init_data(&init_data(42, now), TOKEN, now),
        Some(AuthUser {
            id: 42,
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            language_code: None,
        })
    );
}

#[test]
fn rejects_forged_or_stale_init_data() {
    let now = Utc::now();
    let data = init_data(42, now);
    assert_eq!(verify_init_data(&data, "999:other-token", now), None);
    assert_eq!(
        verify_init_data(&data.replace("42", "43"), TOKEN, now),
        None
    );
    assert_eq!(
        verify_init_data(&data, TOKEN, now + chrono::Duration::days(2)),
        None
    );
    assert_eq!(verify_init_data("user=%7B%7D", TOKEN, now), None);
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, format!("tma {auth}"));
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn api_flow() {
    let app = router(Arc::new(WebState {
        service: Service::new(Db::open_in_memory().unwrap()),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: Some(Url::parse("https://reminders.example.com").unwrap()),
        yandex: None,
    }));
    let ada = init_data_with_language(42, "en", Utc::now());
    let eve = init_data(66, Utc::now());

    assert_eq!(
        call(&app, "GET", "/api/state", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""timezone":null"#), "{body}");

    let reminder = r#"{"description":"call mum","at":"2099-01-02T09:30","recurrence":"weekly"}"#;
    assert_eq!(
        call(&app, "POST", "/api/reminders", Some(&ada), Some(reminder))
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "timezone is required first"
    );

    let (status, body) = call(
        &app,
        "PUT",
        "/api/timezone",
        Some(&ada),
        Some(r#"{"timezone":"Asia/Singapore"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Asia/Singapore"), "{body}");

    let (status, body) = call(&app, "POST", "/api/reminders", Some(&ada), Some(reminder)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains(r#""at":"2099-01-02T09:30""#), "{body}");
    assert!(body.contains(r#""repeat":"every Fri""#), "{body}");
    let id: i64 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    let past = r#"{"description":"x","at":"2000-01-01T09:00","recurrence":"once"}"#;
    assert_eq!(
        call(&app, "POST", "/api/reminders", Some(&ada), Some(past))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    let edited = r#"{"description":"call dad","at":"2099-01-03T10:00","recurrence":"once"}"#;
    let uri = format!("/api/reminders/{id}");
    assert_eq!(
        call(&app, "PUT", &uri, Some(&eve), Some(edited)).await.0,
        StatusCode::BAD_REQUEST
    );
    call(
        &app,
        "PUT",
        "/api/timezone",
        Some(&eve),
        Some(r#"{"timezone":"+0"}"#),
    )
    .await;
    assert_eq!(
        call(&app, "PUT", &uri, Some(&eve), Some(edited)).await.0,
        StatusCode::NOT_FOUND,
        "not eve's"
    );
    assert_eq!(
        call(&app, "DELETE", &uri, Some(&eve), None).await.0,
        StatusCode::NOT_FOUND,
        "not eve's"
    );

    let (status, body) = call(&app, "PUT", &uri, Some(&ada), Some(edited)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("call dad"), "{body}");

    assert_eq!(
        call(&app, "DELETE", &uri, Some(&ada), None).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
    assert!(body.contains(r#""reminders":[]"#), "{body}");
}

#[tokio::test]
async fn preferences_persist_before_registration_and_remain_isolated() {
    let service = Service::new(Db::open_in_memory().unwrap());
    let app = router(Arc::new(WebState {
        service: service.clone(),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: None,
        yandex: None,
    }));
    let ada = init_data_with_language(42, "en", Utc::now());
    let eve = init_data_with_language(66, "ru", Utc::now());
    assert_eq!(
        call(
            &app,
            "PATCH",
            "/api/preferences",
            None,
            Some(r#"{"language":"ru"}"#)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
    assert_eq!(status, StatusCode::OK);
    let state: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(state["language"], "en");
    assert_eq!(state["nag_interval_minutes"], 10);
    assert_eq!(state["confirmation_check_minutes"], 0);
    assert!(state["timezone"].is_null());
    let patch = r#"{"language":"ru","nag_interval_minutes":30,"confirmation_check_minutes":20}"#;
    let (status, body) = call(&app, "PATCH", "/api/preferences", Some(&ada), Some(patch)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let state: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(state["language"], "ru");
    assert_eq!(state["nag_interval_minutes"], 30);
    assert_eq!(state["confirmation_check_minutes"], 20);
    assert!(
        service.user(42).await.unwrap().is_none(),
        "preferences do not register a timezone"
    );
    let (_, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["language"],
        "ru",
        "manual selection wins over Telegram language"
    );
    let (_, body) = call(&app, "GET", "/api/state", Some(&eve), None).await;
    let other: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(other["language"], "ru");
    assert_eq!(other["nag_interval_minutes"], 10);
    assert_eq!(other["confirmation_check_minutes"], 0);
    call(
        &app,
        "PUT",
        "/api/timezone",
        Some(&ada),
        Some(r#"{"timezone":"Europe/Moscow"}"#),
    )
    .await;
    let (_, body) = call(&app, "GET", "/api/state", Some(&ada), None).await;
    let registered: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        registered["nag_interval_minutes"], 30,
        "timezone registration preserves preferences"
    );
}

#[tokio::test]
async fn preferences_validate_whole_patch_before_mutation() {
    let service = Service::new(Db::open_in_memory().unwrap());
    let app = router(Arc::new(WebState {
        service: service.clone(),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: None,
        yandex: None,
    }));
    let auth = init_data(42, Utc::now());
    for invalid in [
        r#"{"language":"fr"}"#,
        r#"{"language":"en","nag_interval_minutes":2}"#,
        r#"{"confirmation_check_minutes":10}"#,
    ] {
        let (status, body) = call(
            &app,
            "PATCH",
            "/api/preferences",
            Some(&auth),
            Some(invalid),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}: {body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
            "invalid_preferences"
        );
        assert_eq!(
            service.preferences(42).await.unwrap(),
            crate::model::Preferences::default()
        );
    }
    assert!(
        !call(
            &app,
            "PATCH",
            "/api/preferences",
            Some(&auth),
            Some(r#"{"user_id":66,"language":"en"}"#)
        )
        .await
        .0
        .is_success(),
        "unknown fields cannot redirect a patch"
    );
    assert_eq!(
        service.preferences(66).await.unwrap(),
        crate::model::Preferences::default()
    );
}

#[tokio::test]
async fn serves_page_and_health() {
    let app = router(Arc::new(WebState {
        service: Service::new(Db::open_in_memory().unwrap()),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: Some(Url::parse("https://reminders.example.com").unwrap()),
        yandex: None,
    }));
    let (status, body) = call(&app, "GET", "/", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("telegram-web-app.js"));
    assert_eq!(
        call(&app, "GET", "/health", None, None).await,
        (StatusCode::OK, "ok".into())
    );
}

#[tokio::test]
async fn calendar_feed_flow() {
    let app = router(Arc::new(WebState {
        service: Service::new(Db::open_in_memory().unwrap()),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: Some(Url::parse("https://reminders.example.com").unwrap()),
        yandex: None,
    }));
    let ada = init_data(42, Utc::now());

    assert_eq!(
        call(&app, "GET", "/api/calendar", Some(&ada), None).await.0,
        StatusCode::BAD_REQUEST
    );
    call(
        &app,
        "PUT",
        "/api/timezone",
        Some(&ada),
        Some(r#"{"timezone":"Europe/London"}"#),
    )
    .await;
    let reminder = r#"{"description":"stretch","at":"2099-01-02T09:30","recurrence":"daily"}"#;
    call(&app, "POST", "/api/reminders", Some(&ada), Some(reminder)).await;

    let (status, body) = call(&app, "GET", "/api/calendar", Some(&ada), None).await;
    assert_eq!(status, StatusCode::OK);
    let link: serde_json::Value = serde_json::from_str(&body).unwrap();
    let url = link["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://reminders.example.com/calendar/"),
        "{url}"
    );
    assert!(
        link["webcal"]
            .as_str()
            .unwrap()
            .starts_with("webcal://reminders.example.com/")
    );
    let path = url.trim_start_matches("https://reminders.example.com");

    let (status, ics) = call(&app, "GET", path, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ics.contains("SUMMARY:stretch"), "{ics}");
    assert!(
        ics.contains("DTSTART;TZID=Europe/London:20990102T093000"),
        "{ics}"
    );

    let (_, body) = call(&app, "POST", "/api/calendar/reset", Some(&ada), None).await;
    let new_url: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_ne!(new_url["url"], link["url"]);
    assert_eq!(
        call(&app, "GET", path, None, None).await.0,
        StatusCode::NOT_FOUND,
        "old link revoked"
    );
    assert_eq!(
        call(&app, "GET", "/calendar/nope.ics", None, None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn pending_api_keeps_fired_items_isolated_and_completes_once() {
    let db = Db::open_in_memory().unwrap();
    let service = Service::new(db.clone());
    for id in [42, 66] {
        service
            .set_timezone(
                Profile {
                    id,
                    first_name: "Test".into(),
                    last_name: String::new(),
                },
                UserTz::parse("+3").unwrap(),
            )
            .await
            .unwrap();
    }
    service
        .set_preferences(42, None, None, Some(20))
        .await
        .unwrap();
    let now = Utc::now();
    let reminder = db
        .add_reminder(
            42,
            "Water plants".into(),
            now - chrono::Duration::minutes(1),
            Recurrence::Once,
        )
        .await
        .unwrap();
    let nag = db
        .fire_reminder(reminder.id, now, now + chrono::Duration::minutes(10))
        .await
        .unwrap()
        .unwrap();
    let app = router(Arc::new(WebState {
        service: service.clone(),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: None,
        yandex: None,
    }));
    let owner = init_data(42, now);
    let other = init_data(66, now);
    let (_, body) = call(&app, "GET", "/api/state", Some(&owner), None).await;
    let state: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(state["reminders"].as_array().unwrap().len(), 0);
    assert_eq!(state["pending"].as_array().unwrap().len(), 1);
    assert_eq!(state["pending"][0]["id"], nag.id);
    assert!(state["pending"][0]["reminder_id"].is_null());
    let (_, body) = call(&app, "GET", "/api/state", Some(&other), None).await;
    assert!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["pending"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let done = format!("/api/pending/{}/done", nag.id);
    let snooze = format!("/api/pending/{}/snooze", nag.id);
    assert_eq!(
        call(&app, "POST", &done, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", &done, Some(&other), None).await.0,
        StatusCode::OK
    );
    assert_eq!(service.pending(42).await.unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            "POST",
            &snooze,
            Some(&other),
            Some(r#"{"duration":"10m"}"#)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &snooze,
            Some(&owner),
            Some(r#"{"duration":"bad"}"#)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &snooze,
            Some(&owner),
            Some(r#"{"duration":"10m"}"#)
        )
        .await
        .0,
        StatusCode::OK
    );
    let pending = service.pending(42).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].next_at >= now + chrono::Duration::minutes(9));
    assert!(pending[0].next_at <= Utc::now() + chrono::Duration::minutes(10));
    for _ in 0..2 {
        let (status, body) = call(&app, "POST", &done, Some(&owner), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["pending"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        db.due_confirmations(now + chrono::Duration::minutes(25))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &snooze,
            Some(&owner),
            Some(r#"{"duration":"1h"}"#)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn text_only_http_edit_preserves_advanced_recurrence_and_anchor() {
    let db = Db::open_in_memory().unwrap();
    let service = Service::new(db.clone());
    let (user, _) = service
        .set_timezone(
            Profile {
                id: 42,
                first_name: "Test".into(),
                last_name: String::new(),
            },
            UserTz::parse("UTC").unwrap(),
        )
        .await
        .unwrap();
    let initial: DateTime<Utc> = "2027-01-31T09:00:00Z".parse().unwrap();
    let reminder = service
        .create(
            &user,
            ReminderInput {
                description: "Monthly",
                at: initial.naive_utc(),
                recurrence: Recurrence::Monthly,
            },
            initial - chrono::Duration::days(1),
        )
        .await
        .unwrap();
    db.fire_reminder(reminder.id, initial, initial)
        .await
        .unwrap();
    let app = router(Arc::new(WebState {
        service: service.clone(),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: None,
        yandex: None,
    }));
    let owner = init_data_with_language(42, "en", Utc::now());
    let other = init_data(66, Utc::now());
    let uri = format!("/api/reminders/{}", reminder.id);
    let body = r#"{"description":"Renamed","at":"2027-01-31T09:00","recurrence":"monthly","preserve_schedule":true}"#;
    let (status, response) = call(&app, "PUT", &uri, Some(&owner), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let changed = service.reminder(42, reminder.id).await.unwrap();
    assert_eq!(
        changed.due_at,
        "2027-02-28T09:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(changed.anchor_local, Some(initial.naive_utc()));
    assert!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["repeat"]
            .as_str()
            .unwrap()
            .contains("31")
    );
    assert_ne!(
        call(&app, "PUT", &uri, Some(&other), Some(body)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn too_long_descriptions_have_stable_code_and_private_api_is_not_cached() {
    let app = router(Arc::new(WebState {
        service: Service::new(Db::open_in_memory().unwrap()),
        bot_token: TOKEN.into(),
        bot_username: "napominalozhka_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: None,
        yandex: None,
    }));
    let owner = init_data(42, Utc::now());
    call(
        &app,
        "PUT",
        "/api/timezone",
        Some(&owner),
        Some(r#"{"timezone":"UTC"}"#),
    )
    .await;
    let body = serde_json::json!({ "description": "🌿".repeat(1001), "at": "2099-01-01T09:00", "recurrence": "once" }).to_string();
    let (status, response) = call(&app, "POST", "/api/reminders", Some(&owner), Some(&body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["code"],
        "description_too_long"
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/state")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}
#[tokio::test]
async fn health_checks_storage_and_stops_advertising_readiness_on_shutdown() {
    let health = crate::health::Health::new();
    let app = router(Arc::new(WebState {
        service: Service::new(Db::open_in_memory().unwrap()),
        bot_token: TOKEN.into(),
        bot_username: "test_bot".into(),
        health: health.clone(),
        web_app_url: None,
        yandex: None,
    }));
    assert_eq!(
        call(&app, "GET", "/health", None, None).await.0,
        StatusCode::OK
    );
    health.stop();
    assert_eq!(
        call(&app, "GET", "/health", None, None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn an_occupied_web_port_is_reported_to_the_worker_supervisor() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let state = Arc::new(WebState {
        service: Service::new(Db::open_in_memory().unwrap()),
        bot_token: TOKEN.into(),
        bot_username: "test_bot".into(),
        health: crate::health::Health::new(),
        web_app_url: None,
        yandex: None,
    });
    let mut supervisor = crate::supervisor::Supervisor::new(std::time::Duration::from_secs(1));
    supervisor.spawn("web", serve(addr, state, shutdown.clone()));
    let error = supervisor
        .run_until(std::future::pending(), shutdown.clone(), || async {})
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("binding web server"));
    assert!(shutdown.is_cancelled());
}
