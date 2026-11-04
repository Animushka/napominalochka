use super::*;
use crate::adapters::http::dto::LOCAL_FORMAT;
use crate::domain::{ConfirmationDelay, Language, NagInterval, ReminderId, UserId};
use crate::domain::{ReminderSettings, User};
use chrono::Duration;

async fn setup() -> (Router, ReminderService, Db, String) {
    let db = Db::open_in_memory().unwrap();
    let service = ReminderService::new(db.clone());
    for id in [42, 66] {
        service
            .set_timezone(
                Profile {
                    id: UserId(id),
                    first_name: "Fixture".into(),
                    last_name: String::new(),
                },
                UserTz::parse("UTC").unwrap(),
            )
            .await
            .unwrap();
    }
    service
        .set_preferences(
            UserId(42),
            Some(Language::En),
            Some(NagInterval::OneHour),
            Some(ConfirmationDelay::TwentyMinutes),
        )
        .await
        .unwrap();
    let app = router(Arc::new(WebState {
        service: service.clone(),
        bot_token: TOKEN.into(),
        bot_username: "test_bot".into(),
        health: crate::application::health::Health::new(),
        web_app_url: None,
        yandex: None,
    }));
    (app, service, db, init_data(UserId(42), Utc::now()))
}

#[tokio::test]
async fn history_http_flow_is_private_paginated_and_undo_is_idempotent() {
    let (app, _, db, owner) = setup().await;
    let now = Utc::now();
    let reminder = db
        .add_reminder(
            UserId(42),
            "completed once".into(),
            now - Duration::minutes(1),
            Recurrence::Once,
        )
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, now, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/api/history", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/pending/{}/done", nagging.id),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, body) = call(&app, "GET", "/api/history?limit=1", Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK);
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["description"], "completed once");
    assert_eq!(page["items"][0]["can_undo"], true);
    assert!(
        page["items"][0]["completed_at"]
            .as_str()
            .unwrap()
            .contains('T')
    );
    assert!(page["items"][0].get("user_id").is_none());
    let id = page["items"][0]["id"].as_i64().unwrap();
    let stranger = init_data(UserId(66), Utc::now());
    let other: serde_json::Value = serde_json::from_str(
        &call(&app, "GET", "/api/history", Some(&stranger), None)
            .await
            .1,
    )
    .unwrap();
    assert!(other["items"].as_array().unwrap().is_empty());
    let undo = format!("/api/history/{id}/undo");
    assert_eq!(
        call(&app, "POST", &undo, Some(&stranger), None).await.0,
        StatusCode::NOT_FOUND
    );
    for _ in 0..2 {
        let (status, body) = call(&app, "POST", &undo, Some(&owner), None).await;
        assert_eq!(status, StatusCode::OK);
        let state: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["pending"].as_array().unwrap().len(), 1);
    }
    let restored = db.naggings_for_user(UserId(42)).await.unwrap().remove(0);
    assert_ne!(restored.id, nagging.id);
    assert!(restored.next_at >= now + Duration::minutes(9));
    assert!(
        db.due_confirmations(now + Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/pending/{}/done", restored.id),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let page: serde_json::Value = serde_json::from_str(
        &call(&app, "GET", "/api/history?limit=1", Some(&owner), None)
            .await
            .1,
    )
    .unwrap();
    let cursor = page["next_cursor"].as_i64().unwrap();
    let older: serde_json::Value = serde_json::from_str(
        &call(
            &app,
            "GET",
            &format!("/api/history?limit=1&before={cursor}"),
            Some(&owner),
            None,
        )
        .await
        .1,
    )
    .unwrap();
    assert_eq!(older["items"][0]["id"], id);
    assert_eq!(older["items"][0]["can_undo"], false);
    assert!(!older["items"][0]["undone_at"].is_null());
    assert_eq!(
        call(&app, "GET", "/api/history?before=-1", Some(&owner), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn undoing_an_obsolete_occurrence_returns_a_stable_conflict() {
    let (app, _, db, owner) = setup().await;
    let now = Utc::now();
    let r = db
        .add_reminder(
            UserId(42),
            "daily".into(),
            now - Duration::minutes(1),
            Recurrence::Daily,
        )
        .await
        .unwrap();
    let n = db.fire_reminder(r.id, now, now).await.unwrap().unwrap();
    db.complete_nagging(UserId(42), n.id, now).await.unwrap();
    let entry = db
        .completion_for_source(UserId(42), n.id)
        .await
        .unwrap()
        .unwrap();
    let next = db.reminder(UserId(42), r.id).await.unwrap().unwrap();
    db.fire_reminder(r.id, next.due_at, next.due_at)
        .await
        .unwrap();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/history/{}/undo", entry.id),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"],
        "completion_unavailable"
    );
    assert_eq!(db.naggings_for_user(UserId(42)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn settings_http_edits_are_atomic_and_do_not_change_user_defaults_or_schedule() {
    let (app, service, db, owner) = setup().await;
    let at = (Utc::now() + Duration::days(1))
        .format(LOCAL_FORMAT)
        .to_string();
    let create=serde_json::json!({"description":"custom","at":at,"recurrence":"daily","settings":{"nag_interval_minutes":5,"confirmation_check_minutes":0,"voice_enabled":false}}).to_string();
    let (status, body) = call(&app, "POST", "/api/reminders", Some(&owner), Some(&create)).await;
    assert_eq!(status, StatusCode::CREATED);
    let item: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(item["settings"]["nag_interval_minutes"], 5);
    let id = item["id"].as_i64().unwrap();
    let old = db
        .reminder(UserId(42), ReminderId(id))
        .await
        .unwrap()
        .unwrap();
    let invalid=serde_json::json!({"description":"must not save","at":"ignored","recurrence":"daily","preserve_schedule":true,"settings":{"nag_interval_minutes":3,"confirmation_check_minutes":20,"voice_enabled":true}}).to_string();
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/api/reminders/{id}"),
            Some(&owner),
            Some(&invalid)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        db.reminder(UserId(42), ReminderId(id))
            .await
            .unwrap()
            .unwrap(),
        old
    );
    let reset=serde_json::json!({"description":"renamed","at":"ignored","recurrence":"daily","preserve_schedule":true,"settings":{"nag_interval_minutes":null,"confirmation_check_minutes":null,"voice_enabled":true}}).to_string();
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/api/reminders/{id}"),
            Some(&owner),
            Some(&reset)
        )
        .await
        .0,
        StatusCode::OK
    );
    let current = db
        .reminder(UserId(42), ReminderId(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.description, "renamed");
    assert_eq!(current.due_at, old.due_at);
    assert_eq!(current.anchor_local, old.anchor_local);
    assert_eq!(current.settings, ReminderSettings::default());
    assert_eq!(
        service
            .preferences(UserId(42))
            .await
            .unwrap()
            .nag_interval_minutes,
        NagInterval::OneHour
    );
    assert_eq!(
        service
            .preferences(UserId(42))
            .await
            .unwrap()
            .confirmation_check_minutes,
        ConfirmationDelay::TwentyMinutes
    );
    let user: User = service.registered(UserId(42)).await.unwrap();
    assert_eq!(user.id, UserId(42));
}

#[tokio::test]
async fn invalid_preference_patches_are_rejected_before_mutation() {
    let (app, service, _, owner) = setup().await;
    let before = service.preferences(UserId(42)).await.unwrap();
    for input in [
        serde_json::json!({"language":"fr"}),
        serde_json::json!({"language":"EN"}),
        serde_json::json!({"nag_interval_minutes":2}),
        serde_json::json!({"confirmation_check_minutes":10}),
    ] {
        let (status, body) = call(
            &app,
            "PATCH",
            "/api/preferences",
            Some(&owner),
            Some(&input.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["code"], "invalid_preferences");
        assert_eq!(service.preferences(UserId(42)).await.unwrap(), before);
    }
}
