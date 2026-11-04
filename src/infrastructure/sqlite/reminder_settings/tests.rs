use super::*;
use crate::application::delivery::{Delivery, DeliveryQueue};
use crate::domain::{ConfirmationDelay, NagInterval, UserId};
use crate::domain::{Recurrence, ReminderSettings, User, UserTz};
use chrono::{Duration, SubsecRound, Utc};

async fn setup() -> (Db, chrono::DateTime<Utc>) {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(User {
        id: UserId(1),
        first_name: "Ada".into(),
        last_name: String::new(),
        tz: UserTz::parse("UTC").unwrap(),
    })
    .await
    .unwrap();
    db.set_preferences(
        UserId(1),
        None,
        Some(NagInterval::OneHour),
        Some(ConfirmationDelay::TwentyMinutes),
    )
    .await
    .unwrap();
    (db, Utc::now().trunc_subsecs(0))
}

fn write(now: chrono::DateTime<Utc>, settings: ReminderSettings) -> ReminderWrite {
    ReminderWrite {
        description: "custom".into(),
        due_at: now,
        recurrence: Recurrence::Once,
        anchor_local: None,
        settings,
    }
}

#[tokio::test]
async fn typed_patch_preserves_omitted_settings_and_can_explicitly_restore_inheritance() {
    let (db, now) = setup().await;
    let settings = ReminderSettings {
        nag_interval_minutes: Some(NagInterval::FiveMinutes),
        confirmation_check_minutes: Some(ConfirmationDelay::Disabled),
        voice_enabled: false,
    };
    let reminder = db
        .insert_reminder(UserId(1), write(now, settings.clone()))
        .await
        .unwrap();
    assert_eq!(reminder.settings, settings);
    let renamed = db
        .patch_reminder(
            UserId(1),
            reminder.id,
            ReminderChanges {
                description: Some("renamed".into()),
                schedule: None,
                settings: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed.settings, settings);
    assert_eq!(renamed.due_at, now);
    assert_eq!(renamed.description, "renamed");
    assert!(
        db.patch_reminder(
            UserId(2),
            reminder.id,
            ReminderChanges {
                description: None,
                schedule: None,
                settings: Some(ReminderSettings::default())
            }
        )
        .await
        .unwrap()
        .is_none()
    );
    let inherited = db
        .patch_reminder(
            UserId(1),
            reminder.id,
            ReminderChanges {
                description: None,
                schedule: None,
                settings: Some(ReminderSettings::default()),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inherited.settings, ReminderSettings::default());
}

#[tokio::test]
async fn one_off_delivery_completion_and_undo_keep_their_override_snapshots() {
    let (db, now) = setup().await;
    let settings = ReminderSettings {
        nag_interval_minutes: Some(NagInterval::FiveMinutes),
        confirmation_check_minutes: Some(ConfirmationDelay::Disabled),
        voice_enabled: false,
    };
    let reminder = db
        .insert_reminder(UserId(1), write(now, settings))
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, now, now)
        .await
        .unwrap()
        .unwrap();
    assert!(db.reminder(UserId(1), reminder.id).await.unwrap().is_none());
    let job = db
        .claim_delivery(UserId(1), now, now + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    db.finish_delivery(job, Delivery::Sent, now).await.unwrap();
    assert_eq!(
        db.naggings_for_user(UserId(1)).await.unwrap()[0].next_at,
        now + Duration::minutes(5)
    );
    db.complete_nagging(UserId(1), nagging.id, now + Duration::minutes(1))
        .await
        .unwrap();
    assert!(
        db.due_confirmations(now + Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
    let entry = db
        .completion_for_source(UserId(1), nagging.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.completion_for_source(UserId(2), nagging.id)
            .await
            .unwrap()
            .is_none()
    );
    db.undo_completion(UserId(1), entry.id, now + Duration::minutes(2))
        .await
        .unwrap();
    let restored_at = now + Duration::minutes(12);
    let job = db
        .claim_delivery(UserId(1), restored_at, restored_at + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    db.finish_delivery(job, Delivery::Sent, restored_at)
        .await
        .unwrap();
    let pending = db.naggings_for_user(UserId(1)).await.unwrap().remove(0);
    assert_eq!(pending.next_at, restored_at + Duration::minutes(5));
    db.complete_nagging(UserId(1), pending.id, restored_at)
        .await
        .unwrap();
    assert!(
        db.due_confirmations(restored_at + Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn global_check_off_cancels_inherited_checks_but_keeps_explicit_twenty_minutes() {
    let (db, now) = setup().await;
    for settings in [
        ReminderSettings::default(),
        ReminderSettings {
            confirmation_check_minutes: Some(ConfirmationDelay::TwentyMinutes),
            ..ReminderSettings::default()
        },
    ] {
        let reminder = db
            .insert_reminder(UserId(1), write(now, settings))
            .await
            .unwrap();
        let nagging = db
            .fire_reminder(reminder.id, now, now)
            .await
            .unwrap()
            .unwrap();
        db.complete_nagging(UserId(1), nagging.id, now)
            .await
            .unwrap();
    }
    assert_eq!(
        db.due_confirmations(now + Duration::minutes(20))
            .await
            .unwrap()
            .len(),
        2
    );
    db.set_preferences(UserId(1), None, None, Some(ConfirmationDelay::Disabled))
        .await
        .unwrap();
    assert_eq!(
        db.due_confirmations(now + Duration::minutes(20))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.due_delivery_users(now + Duration::minutes(20))
            .await
            .unwrap(),
        vec![UserId(1)]
    );
    let job = db
        .claim_delivery(
            UserId(1),
            now + Duration::minutes(20),
            now + Duration::minutes(21),
        )
        .await
        .unwrap()
        .unwrap();
    db.finish_delivery(job, Delivery::Sent, now + Duration::minutes(20))
        .await
        .unwrap();
    assert!(
        db.due_confirmations(now + Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn per_reminder_voice_never_bypasses_the_account_switch_and_off_cancels_queued_work() {
    let (db, now) = setup().await;
    db.call(|conn| { conn.execute("INSERT INTO yandex_connections(user_id,generation,encrypted_tokens,enabled,scenario_id) VALUES(1,'test',X'01',0,'speaker')",[])?;Ok(()) }).await.unwrap();
    let first = db
        .insert_reminder(UserId(1), write(now, ReminderSettings::default()))
        .await
        .unwrap();
    db.fire_reminder(first.id, now, now).await.unwrap();
    assert_eq!(voice_jobs(&db).await, 0);
    db.call(|conn| {
        conn.execute(
            "UPDATE yandex_connections SET enabled=1 WHERE user_id=1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let muted = db
        .insert_reminder(
            UserId(1),
            write(
                now,
                ReminderSettings {
                    voice_enabled: false,
                    ..ReminderSettings::default()
                },
            ),
        )
        .await
        .unwrap();
    db.fire_reminder(muted.id, now, now).await.unwrap();
    assert_eq!(voice_jobs(&db).await, 0);
    let mut input = write(now, ReminderSettings::default());
    input.recurrence = Recurrence::Daily;
    let audible = db.insert_reminder(UserId(1), input).await.unwrap();
    db.fire_reminder(audible.id, now, now).await.unwrap();
    assert_eq!(voice_jobs(&db).await, 1);
    let selected = ReminderSettings {
        nag_interval_minutes: Some(NagInterval::OneMinute),
        confirmation_check_minutes: Some(ConfirmationDelay::TwentyMinutes),
        voice_enabled: false,
    };
    db.patch_reminder(
        UserId(1),
        audible.id,
        ReminderChanges {
            description: None,
            schedule: None,
            settings: Some(selected),
        },
    )
    .await
    .unwrap();
    assert_eq!(voice_jobs(&db).await, 0);
    db.call(move |conn| {
            let settings:(Option<i64>,Option<i64>,bool)=conn.query_row("SELECT nag_interval_minutes,confirmation_check_minutes,voice_enabled FROM naggings WHERE reminder_id=?1",[audible.id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            assert_eq!(settings,(Some(1),Some(20),false));Ok(())
        }).await.unwrap();
}

async fn voice_jobs(db: &Db) -> i64 {
    db.call(|conn| conn.query_row("SELECT COUNT(*) FROM yandex_jobs", [], |row| row.get(0)))
        .await
        .unwrap()
}

#[tokio::test]
async fn switching_a_reminders_check_off_cancels_followups_and_undo_uses_current_settings() {
    for check_override in [Some(ConfirmationDelay::Disabled), None] {
        let (db, now) = setup().await;
        let mut input = write(
            now,
            ReminderSettings {
                confirmation_check_minutes: Some(ConfirmationDelay::TwentyMinutes),
                nag_interval_minutes: Some(NagInterval::FiveMinutes),
                voice_enabled: true,
            },
        );
        input.recurrence = Recurrence::Daily;
        let reminder = db.insert_reminder(UserId(1), input).await.unwrap();
        let nagging = db
            .fire_reminder(reminder.id, now, now)
            .await
            .unwrap()
            .unwrap();
        db.complete_nagging(UserId(1), nagging.id, now)
            .await
            .unwrap();
        let entry = db
            .completion_for_source(UserId(1), nagging.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            db.due_confirmations(now + Duration::minutes(20))
                .await
                .unwrap()
                .len(),
            1
        );
        if check_override.is_none() {
            db.set_preferences(UserId(1), None, None, Some(ConfirmationDelay::Disabled))
                .await
                .unwrap();
            assert_eq!(
                db.due_confirmations(now + Duration::minutes(20))
                    .await
                    .unwrap()
                    .len(),
                1,
                "global change alone keeps explicit checks"
            );
        }
        db.patch_reminder(
            UserId(1),
            reminder.id,
            ReminderChanges {
                description: None,
                schedule: None,
                settings: Some(ReminderSettings {
                    nag_interval_minutes: Some(NagInterval::ThirtyMinutes),
                    confirmation_check_minutes: check_override,
                    voice_enabled: false,
                }),
            },
        )
        .await
        .unwrap();
        assert!(
            db.due_confirmations(now + Duration::minutes(20))
                .await
                .unwrap()
                .is_empty()
        );
        db.undo_completion(UserId(1), entry.id, now + Duration::minutes(1))
            .await
            .unwrap();
        let settings=db.call(|conn|conn.query_row("SELECT nag_interval_minutes,confirmation_check_minutes,voice_enabled FROM naggings WHERE user_id=1",[],|row|Ok((row.get::<_,Option<i64>>(0)?,row.get::<_,Option<i64>>(1)?,row.get::<_,bool>(2)?)))).await.unwrap();
        assert_eq!(
            settings,
            (
                Some(30),
                check_override.map(ConfirmationDelay::minutes),
                false
            )
        );
    }
}

#[tokio::test]
async fn queued_check_uses_the_surviving_parents_current_inheritance_policy() {
    let (db, now) = setup().await;
    let mut input = write(
        now,
        ReminderSettings {
            confirmation_check_minutes: Some(ConfirmationDelay::TwentyMinutes),
            ..ReminderSettings::default()
        },
    );
    input.recurrence = Recurrence::Daily;
    let reminder = db.insert_reminder(UserId(1), input).await.unwrap();
    let nagging = db
        .fire_reminder(reminder.id, now, now)
        .await
        .unwrap()
        .unwrap();
    db.complete_nagging(UserId(1), nagging.id, now)
        .await
        .unwrap();
    db.patch_reminder(
        UserId(1),
        reminder.id,
        ReminderChanges {
            description: None,
            schedule: None,
            settings: Some(ReminderSettings::default()),
        },
    )
    .await
    .unwrap();
    let due = now + Duration::minutes(20);
    assert_eq!(db.due_confirmations(due).await.unwrap().len(), 1);
    // Exercise selection/claim filtering separately from set_preferences cleanup.
    db.call(|conn| {
        conn.execute(
            "UPDATE preferences SET confirmation_check_minutes=0 WHERE user_id=1",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(db.due_delivery_users(due).await.unwrap().is_empty());
    assert!(
        db.claim_delivery(UserId(1), due, due + Duration::seconds(30))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(db.due_confirmations(due).await.unwrap().len(), 1);
    db.set_preferences(
        UserId(1),
        None,
        None,
        Some(ConfirmationDelay::TwentyMinutes),
    )
    .await
    .unwrap();
    db.set_preferences(UserId(1), None, None, Some(ConfirmationDelay::Disabled))
        .await
        .unwrap();
    assert!(db.due_confirmations(due).await.unwrap().is_empty());
    assert!(db.due_delivery_users(due).await.unwrap().is_empty());
}
