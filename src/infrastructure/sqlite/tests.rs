use super::Db;
use super::mapping::new_token;
use super::schema::MIGRATIONS;
use crate::application::ports::Registration;
use crate::domain::{Preferences, Recurrence, User, UserTz, UtcOffset};
use chrono::Duration as ChronoDuration;
use chrono::TimeZone;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

fn user(id: i64) -> User {
    User {
        id,
        first_name: "Ada".into(),
        last_name: String::new(),
        tz: UserTz::Fixed(UtcOffset::from_minutes(480).unwrap()),
    }
}

#[tokio::test]
async fn calendar_anchors_survive_reopening_and_legacy_rows_gain_an_anchor() {
    let path = std::env::temp_dir().join(format!(
        "napominalochka-anchor-{}.sqlite",
        new_token().unwrap()
    ));
    let db = Db::open(&path).unwrap();
    let mut utc_user = user(1);
    utc_user.tz = UserTz::Fixed(UtcOffset::from_minutes(0).unwrap());
    db.upsert_user(utc_user).await.unwrap();
    let mut london_user = user(2);
    london_user.tz = UserTz::Named(chrono_tz::Tz::Europe__London);
    let london = london_user.tz;
    db.upsert_user(london_user).await.unwrap();

    // An unanchored legacy row is upgraded using its current due local date.
    let monthly = db
        .add_reminder(
            1,
            "monthly".into(),
            utc(2027, 1, 31, 9, 0),
            Recurrence::Monthly,
        )
        .await
        .unwrap();
    assert_eq!(monthly.anchor_local, None);
    let daily_anchor = utc(2026, 3, 28, 1, 30).naive_utc();
    let daily = db
        .add_reminder_with_anchor(
            2,
            "daily".into(),
            london.utc(daily_anchor),
            Recurrence::Daily,
            Some(daily_anchor),
        )
        .await
        .unwrap();
    let leap_anchor = utc(2028, 2, 29, 9, 0).naive_utc();
    let yearly = db
        .add_reminder_with_anchor(
            1,
            "yearly".into(),
            leap_anchor.and_utc(),
            Recurrence::Yearly,
            Some(leap_anchor),
        )
        .await
        .unwrap();
    for reminder in [&monthly, &daily, &yearly] {
        db.fire_reminder(reminder.id, reminder.due_at, reminder.due_at)
            .await
            .unwrap();
    }
    drop(db);

    let db = Db::open(&path).unwrap();
    let monthly = db.reminder(1, monthly.id).await.unwrap().unwrap();
    assert_eq!(monthly.due_at, utc(2027, 2, 28, 9, 0));
    assert_eq!(
        monthly.anchor_local,
        Some(utc(2027, 1, 31, 9, 0).naive_utc())
    );
    db.fire_reminder(monthly.id, monthly.due_at, monthly.due_at)
        .await
        .unwrap();
    assert_eq!(
        db.reminder(1, monthly.id).await.unwrap().unwrap().due_at,
        utc(2027, 3, 31, 9, 0)
    );

    let daily = db.reminder(2, daily.id).await.unwrap().unwrap();
    assert_eq!(
        london.local(daily.due_at),
        utc(2026, 3, 29, 2, 30).naive_utc()
    );
    assert_eq!(daily.anchor_local, Some(daily_anchor));
    db.fire_reminder(daily.id, daily.due_at, daily.due_at)
        .await
        .unwrap();
    assert_eq!(
        london.local(db.reminder(2, daily.id).await.unwrap().unwrap().due_at),
        utc(2026, 3, 30, 1, 30).naive_utc()
    );

    let mut yearly = db.reminder(1, yearly.id).await.unwrap().unwrap();
    for year in 2029..=2032 {
        assert_eq!(yearly.anchor_local, Some(leap_anchor));
        assert_eq!(
            yearly.due_at,
            utc(year, 2, if year == 2032 { 29 } else { 28 }, 9, 0)
        );
        db.fire_reminder(yearly.id, yearly.due_at, yearly.due_at)
            .await
            .unwrap();
        yearly = db.reminder(1, yearly.id).await.unwrap().unwrap();
    }
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn pending_one_offs_and_snoozes_survive_reopening_and_stay_private() {
    let path = std::env::temp_dir().join(format!(
        "napominalochka-pending-{}.sqlite",
        new_token().unwrap()
    ));
    let db = Db::open(&path).unwrap();
    db.upsert_user(user(1)).await.unwrap();
    db.upsert_user(user(2)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let later = due + ChronoDuration::minutes(10);
    let mut nags = Vec::new();
    for (owner, text) in [(1, "first"), (1, "second"), (2, "private")] {
        let reminder = db
            .add_reminder(owner, text.into(), due, Recurrence::Once)
            .await
            .unwrap();
        nags.push(
            db.fire_reminder(reminder.id, due, later)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(db.reminders_for_user(1).await.unwrap().is_empty());
    assert!(db.due_naggings(due).await.unwrap().is_empty());
    assert_eq!(
        db.naggings_for_user(1)
            .await
            .unwrap()
            .iter()
            .map(|n| n.id)
            .collect::<Vec<_>>(),
        vec![nags[0].id, nags[1].id]
    );
    let snoozed_until = due + ChronoDuration::hours(1);
    db.snooze_nagging(1, nags[0].id, snoozed_until)
        .await
        .unwrap();
    drop(db);

    let db = Db::open(&path).unwrap();
    let pending = db.naggings_for_user(1).await.unwrap();
    assert_eq!(
        pending.iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![nags[1].id, nags[0].id]
    );
    assert!(
        pending
            .iter()
            .all(|n| n.user_id == 1 && n.reminder_id.is_none())
    );
    assert_eq!(pending[1].next_at, snoozed_until);
    assert_eq!(db.naggings_for_user(2).await.unwrap()[0].id, nags[2].id);
    assert!(db.naggings_for_user(3).await.unwrap().is_empty());
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn nagging_claim_requires_a_current_due_snapshot_and_only_one_worker_wins() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "task".into(), due, Recurrence::Once)
        .await
        .unwrap();
    db.fire_reminder(reminder.id, due, due).await.unwrap();
    let observed = db.due_naggings(due).await.unwrap().remove(0);
    let next = due + ChronoDuration::minutes(10);
    let mut wrong_owner = observed.clone();
    wrong_owner.user_id = 2;
    assert!(!db.claim_nagging(&wrong_owner, due, next).await.unwrap());
    let (first, second) = tokio::join!(
        db.claim_nagging(&observed, due, next),
        db.claim_nagging(&observed, due, next),
    );
    assert_ne!(first.unwrap(), second.unwrap());
    let rescheduled = db.naggings_for_user(1).await.unwrap().remove(0);
    assert!(
        !db.claim_nagging(&rescheduled, due, next).await.unwrap(),
        "future occurrence cannot be claimed"
    );

    let until = due + ChronoDuration::hours(1);
    db.snooze_nagging(1, observed.id, until).await.unwrap();
    assert!(
        !db.claim_nagging(&rescheduled, next, next + ChronoDuration::minutes(10))
            .await
            .unwrap()
    );
    assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, until);
    let snoozed = db.naggings_for_user(1).await.unwrap().remove(0);
    db.complete_nagging(1, observed.id, next).await.unwrap();
    assert!(
        !db.claim_nagging(&snoozed, until, until + ChronoDuration::minutes(10))
            .await
            .unwrap()
    );
    assert!(db.naggings_for_user(1).await.unwrap().is_empty());
}

#[tokio::test]
async fn snoozing_cancels_only_its_owners_undispatched_yandex_job() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    db.upsert_user(user(2)).await.unwrap();
    db.call(|conn| conn.execute_batch(
            "INSERT INTO yandex_connections (user_id, generation, encrypted_tokens, enabled, scenario_id)
             VALUES (1, 'g1', X'01', 1, 'scenario-1'), (2, 'g2', X'02', 1, 'scenario-2');"
        )).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let later = due + ChronoDuration::hours(1);
    let mut nags = Vec::new();
    for owner in [1, 1, 1, 2] {
        let reminder = db
            .add_reminder(owner, "task".into(), due, Recurrence::Once)
            .await
            .unwrap();
        nags.push(
            db.fire_reminder(reminder.id, due, due)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let claimed_id = nags[1].id;
    let delivered_id = nags[2].id;
    db.call(move |conn| {
        conn.execute(
            "UPDATE yandex_jobs SET state = 'claimed' WHERE nagging_id = ?1",
            [claimed_id],
        )?;
        conn.execute(
            "UPDATE yandex_jobs SET state = 'delivered' WHERE nagging_id = ?1",
            [delivered_id],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(!db.snooze_nagging(2, nags[0].id, later).await.unwrap());
    let count = || {
        db.call(|conn| {
            conn.query_row("SELECT COUNT(*) FROM yandex_jobs", [], |row| {
                row.get::<_, i64>(0)
            })
        })
    };
    assert_eq!(count().await.unwrap(), 4);
    for nagging in &nags[..3] {
        assert!(db.snooze_nagging(1, nagging.id, later).await.unwrap());
    }
    let remaining = db
        .call(|conn| {
            conn.prepare("SELECT nagging_id, state FROM yandex_jobs ORDER BY job_id")?
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
        .unwrap();
    assert_eq!(
        remaining,
        vec![
            (nags[2].id, "delivered".into()),
            (nags[3].id, "pending".into())
        ]
    );
    assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 3);
    assert_eq!(db.naggings_for_user(2).await.unwrap()[0].next_at, due);
    let snoozed = db.naggings_for_user(1).await.unwrap().remove(0);
    assert!(
        db.claim_nagging(&snoozed, later, later + ChronoDuration::minutes(10))
            .await
            .unwrap()
    );
    assert_eq!(
        count().await.unwrap(),
        2,
        "repeating never creates a new voice job"
    );
}

#[tokio::test]
async fn upgrades_a_database_created_by_the_dotnet_bot() {
    let conn = Connection::open_in_memory().unwrap();
    // The .NET bot never enabled foreign keys, so its databases can contain orphaned rows.
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    conn.execute_batch(MIGRATIONS[0]).unwrap();
    conn.execute_batch(
        "INSERT INTO users VALUES (1, 'Ada', '', 8);
             INSERT INTO reminders (user_id, description, datetime, recurring)
                 VALUES (1, 'stretch', '2026-01-01T00:00:00', 1);
             INSERT INTO reminders (user_id, description, datetime, recurring)
                 VALUES (99, 'orphan', '2026-01-01T00:00:00', 0);
             INSERT INTO naggings (reminder_id, user_id, description, datetime)
                 VALUES (1, 1, 'stretch', '2026-01-01T00:01:00');",
    )
    .unwrap();

    let db = Db::init(conn).unwrap();

    let migrated = db.user(1).await.unwrap().unwrap();
    assert_eq!(
        migrated.tz,
        UserTz::Fixed(UtcOffset::from_minutes(480).unwrap())
    );
    let reminders = db.reminders_for_user(1).await.unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].due_at, utc(2026, 1, 1, 0, 0));
    assert_eq!(reminders[0].recurrence, Recurrence::Daily);
    assert!(db.reminders_for_user(99).await.unwrap().is_empty());
    assert!(
        db.due_naggings(utc(2030, 1, 1, 0, 0))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn register_twice_updates_the_user() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(
        db.upsert_user(user(1)).await.unwrap(),
        Registration::Created
    );
    let mut updated = user(1);
    updated.tz = UserTz::Named(chrono_tz::Tz::Europe__London);
    assert_eq!(
        db.upsert_user(updated.clone()).await.unwrap(),
        Registration::Updated
    );
    assert_eq!(db.user(1).await.unwrap(), Some(updated));
}

#[tokio::test]
async fn recurring_reminder_is_rescheduled_and_nags_until_acknowledged() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "pills".into(), due, Recurrence::Daily)
        .await
        .unwrap();

    assert!(
        db.due_reminders(due - ChronoDuration::minutes(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.due_reminders(due).await.unwrap(), vec![reminder.clone()]);

    let nag_at = due + ChronoDuration::minutes(1);
    let nagging = db
        .fire_reminder(reminder.id, due, nag_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db.reminders_for_user(1).await.unwrap()[0].due_at,
        utc(2026, 3, 2, 8, 0)
    );
    assert!(db.due_reminders(due).await.unwrap().is_empty());
    assert_eq!(
        db.due_naggings(nag_at).await.unwrap(),
        vec![nagging.clone()]
    );

    assert!(
        !db.acknowledge_nagging(2, nagging.id).await.unwrap(),
        "other users cannot acknowledge"
    );
    assert!(db.acknowledge_nagging(1, nagging.id).await.unwrap());
    assert!(db.due_naggings(nag_at).await.unwrap().is_empty());
}

#[tokio::test]
async fn next_occurrence_replaces_an_unacknowledged_nagging() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "pills".into(), due, Recurrence::Daily)
        .await
        .unwrap();

    db.fire_reminder(reminder.id, due, due).await.unwrap();
    let next_day = utc(2026, 3, 2, 8, 0);
    let second = db
        .fire_reminder(reminder.id, next_day, next_day)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(db.due_naggings(next_day).await.unwrap(), vec![second]);
}

#[tokio::test]
async fn one_off_reminder_is_consumed_but_keeps_nagging() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "call mum".into(), due, Recurrence::Once)
        .await
        .unwrap();

    let nagging = db
        .fire_reminder(reminder.id, due, due)
        .await
        .unwrap()
        .unwrap();
    assert!(db.reminders_for_user(1).await.unwrap().is_empty());
    let pending = db.due_naggings(due).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, nagging.id);
    assert_eq!(pending[0].reminder_id, None);
}

#[tokio::test]
async fn overdue_reminder_skips_missed_occurrences() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let reminder = db
        .add_reminder(
            1,
            "water plants".into(),
            utc(2026, 3, 1, 8, 0),
            Recurrence::Daily,
        )
        .await
        .unwrap();

    let now = utc(2026, 3, 5, 12, 0);
    db.fire_reminder(reminder.id, now, now).await.unwrap();
    assert_eq!(
        db.reminders_for_user(1).await.unwrap()[0].due_at,
        utc(2026, 3, 6, 8, 0)
    );
}

#[tokio::test]
async fn removing_a_reminder_stops_its_nagging() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "pills".into(), due, Recurrence::Daily)
        .await
        .unwrap();
    db.fire_reminder(reminder.id, due, due).await.unwrap();

    assert!(
        !db.delete_reminder(2, reminder.id).await.unwrap(),
        "other users cannot delete it"
    );
    assert!(db.delete_reminder(1, reminder.id).await.unwrap());
    assert!(db.due_naggings(due).await.unwrap().is_empty());
}

#[tokio::test]
async fn unregistering_removes_everything() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "a".into(), due, Recurrence::Daily)
        .await
        .unwrap();
    db.add_reminder(1, "b".into(), due, Recurrence::Once)
        .await
        .unwrap();
    db.fire_reminder(reminder.id, due, due).await.unwrap();

    assert!(db.delete_user(1).await.unwrap());
    assert!(db.reminders_for_user(1).await.unwrap().is_empty());
    assert!(db.due_naggings(due).await.unwrap().is_empty());
    assert!(!db.delete_user(1).await.unwrap());
}

#[tokio::test]
async fn legacy_acknowledgement_by_reminder_id() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "pills".into(), due, Recurrence::Weekly)
        .await
        .unwrap();
    db.fire_reminder(reminder.id, due, due).await.unwrap();

    assert!(db.acknowledge_reminder(1, reminder.id).await.unwrap());
    assert!(db.due_naggings(due).await.unwrap().is_empty());
}

#[tokio::test]
async fn draft_becomes_a_reminder_once() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let now = utc(2026, 3, 1, 8, 0);
    let first = db.create_draft(1, "old".into(), now).await.unwrap();
    let draft = db.create_draft(1, "call mum".into(), now).await.unwrap();
    assert_eq!(
        db.draft(1, first.id).await.unwrap(),
        None,
        "only the latest draft is kept"
    );
    assert_eq!(
        db.draft(2, draft.id).await.unwrap(),
        None,
        "drafts are private"
    );
    assert_eq!(db.pending_draft(1, now).await.unwrap(), Some(draft.clone()));

    let due = utc(2026, 3, 2, 1, 0);
    let reminder = db
        .save_draft(1, draft.id, due, Recurrence::Weekdays)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reminder.description, "call mum");
    assert_eq!(reminder.recurrence, Recurrence::Weekdays);
    assert_eq!(
        db.save_draft(1, draft.id, due, Recurrence::Once)
            .await
            .unwrap(),
        None
    );
    assert_eq!(db.reminders_for_user(1).await.unwrap(), vec![reminder]);
}

#[tokio::test]
async fn updates_and_snoozes() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "pills".into(), due, Recurrence::Once)
        .await
        .unwrap();

    let later = utc(2026, 3, 3, 9, 0);
    assert_eq!(
        db.update_reminder(2, reminder.id, None, later, Recurrence::Daily)
            .await
            .unwrap(),
        None
    );
    let updated = db
        .update_reminder(
            1,
            reminder.id,
            Some("vitamins".into()),
            later,
            Recurrence::Daily,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            updated.description.as_str(),
            updated.due_at,
            updated.recurrence
        ),
        ("vitamins", later, Recurrence::Daily)
    );

    let nagging = db
        .fire_reminder(reminder.id, later, later)
        .await
        .unwrap()
        .unwrap();
    let snooze_until = later + ChronoDuration::hours(1);
    assert!(
        !db.snooze_nagging(2, nagging.id, snooze_until)
            .await
            .unwrap()
    );
    assert!(
        db.snooze_nagging(1, nagging.id, snooze_until)
            .await
            .unwrap()
    );
    assert!(db.due_naggings(later).await.unwrap().is_empty());
    assert_eq!(db.due_naggings(snooze_until).await.unwrap().len(), 1);
}

#[tokio::test]
async fn named_timezone_round_trips() {
    let db = Db::open_in_memory().unwrap();
    let mut u = user(1);
    u.tz = UserTz::Named(chrono_tz::Tz::America__New_York);
    db.upsert_user(u.clone()).await.unwrap();
    assert_eq!(db.user(1).await.unwrap(), Some(u));
}

#[tokio::test]
async fn ids_of_deleted_rows_are_not_reused() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let first = db
        .add_reminder(1, "a".into(), due, Recurrence::Once)
        .await
        .unwrap();
    db.delete_reminder(1, first.id).await.unwrap();
    let second = db
        .add_reminder(1, "b".into(), due, Recurrence::Once)
        .await
        .unwrap();
    assert_ne!(first.id, second.id);

    let nag = db
        .fire_reminder(second.id, due, due)
        .await
        .unwrap()
        .unwrap();
    db.acknowledge_nagging(1, nag.id).await.unwrap();
    let third = db
        .add_reminder(1, "c".into(), due, Recurrence::Once)
        .await
        .unwrap();
    let next_nag = db.fire_reminder(third.id, due, due).await.unwrap().unwrap();
    assert_ne!(nag.id, next_nag.id);
}

#[tokio::test]
async fn calendar_tokens() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(
        db.calendar_token(1, false).await.unwrap(),
        None,
        "unregistered"
    );
    db.upsert_user(user(1)).await.unwrap();
    db.upsert_user(user(2)).await.unwrap();

    let token = db.calendar_token(1, false).await.unwrap().unwrap();
    assert_eq!(token.len(), 64);
    assert_eq!(
        db.calendar_token(1, false).await.unwrap().as_ref(),
        Some(&token),
        "stable"
    );
    assert_ne!(
        db.calendar_token(2, false).await.unwrap().as_ref(),
        Some(&token)
    );
    assert_eq!(
        db.user_by_calendar_token(token.clone())
            .await
            .unwrap()
            .map(|u| u.id),
        Some(1)
    );

    let new = db.calendar_token(1, true).await.unwrap().unwrap();
    assert_ne!(new, token);
    assert_eq!(
        db.user_by_calendar_token(token).await.unwrap(),
        None,
        "old link revoked"
    );
    assert_eq!(
        db.user_by_calendar_token(new).await.unwrap().map(|u| u.id),
        Some(1)
    );
}
#[tokio::test]
async fn preferences_and_completion_checks_survive_reopening() {
    let path = std::env::temp_dir().join(format!(
        "napominalochka-test-{}.sqlite",
        new_token().unwrap()
    ));
    let db = Db::open(&path).unwrap();
    db.set_preferences(7, Some("en".into()), Some(20), Some(20))
        .await
        .unwrap();
    db.upsert_user(user(7)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(7, "done".into(), due, Recurrence::Once)
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, due, due)
        .await
        .unwrap()
        .unwrap();
    db.complete_nagging(7, nagging.id, due).await.unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.preferences(7).await.unwrap(),
        Preferences {
            language: "en".into(),
            nag_interval_minutes: 20,
            confirmation_check_minutes: 20
        }
    );
    assert!(
        db.due_confirmations(due + ChronoDuration::minutes(19))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.due_confirmations(due + ChronoDuration::minutes(20))
            .await
            .unwrap()
            .len(),
        1
    );
    db.set_preferences(8, Some("ru".into()), Some(5), Some(20))
        .await
        .unwrap();
    db.upsert_user(user(8)).await.unwrap();
    let reminder = db
        .add_reminder(
            8,
            "other user's completed task".into(),
            due,
            Recurrence::Once,
        )
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, due, due)
        .await
        .unwrap()
        .unwrap();
    db.complete_nagging(8, nagging.id, due).await.unwrap();
    db.set_preferences(7, None, None, Some(0)).await.unwrap();
    let remaining = db
        .due_confirmations(due + ChronoDuration::days(1))
        .await
        .unwrap();
    assert_eq!(
        remaining.len(),
        1,
        "disabling cancels only the owner's pending checks"
    );
    assert_eq!(remaining[0].user_id, 8);
    db.delete_user(8).await.unwrap();
    assert!(
        db.due_confirmations(due + ChronoDuration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn deleting_a_recurring_reminder_also_cancels_its_completion_check() {
    let db = Db::open_in_memory().unwrap();
    db.upsert_user(user(1)).await.unwrap();
    db.set_preferences(1, None, None, Some(20)).await.unwrap();
    let due = utc(2026, 3, 1, 8, 0);
    let reminder = db
        .add_reminder(1, "daily".into(), due, Recurrence::Daily)
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, due, due)
        .await
        .unwrap()
        .unwrap();
    db.complete_nagging(1, nagging.id, due).await.unwrap();
    assert_eq!(
        db.due_confirmations(due + ChronoDuration::minutes(20))
            .await
            .unwrap()
            .len(),
        1
    );
    db.delete_reminder(1, reminder.id).await.unwrap();
    assert!(
        db.due_confirmations(due + ChronoDuration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
}
