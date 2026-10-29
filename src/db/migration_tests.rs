//! Upgrade the previously deployed schema with live pending work, without using
//! credentials or modifying a working database.

use super::*;
use crate::delivery::DeliveryQueue;

#[tokio::test]
async fn upgrading_schema_seven_preserves_pending_work_and_integration_settings() {
    let conn = Connection::open_in_memory().unwrap();
    // Match the production migration runner: old migrations rebuild referenced
    // tables and enable foreign key enforcement only after the upgrade completes.
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    for (index, sql) in MIGRATIONS.iter().take(7).enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.pragma_update(None, "user_version", i64::try_from(index + 1).unwrap())
            .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO users(user_id,first_name,last_name,utc_offset_minutes,timezone)
         VALUES(1,'Ada','',180,'Europe/Moscow');
         INSERT INTO preferences(user_id,nag_interval_minutes,confirmation_check_minutes)
         VALUES(1,60,20);
         INSERT INTO reminders(reminder_id,user_id,description,due_at,recurrence,anchor_local)
         VALUES(7,1,'existing reminder','2026-10-29T06:00:00',1,'2026-10-29T09:00:00');
         INSERT INTO naggings(nagging_id,reminder_id,user_id,description,next_at)
         VALUES(41,7,1,'existing pending','2026-10-29T06:00:00');
         INSERT INTO confirmations(source_nagging_id,reminder_id,user_id,description,due_at)
         VALUES(42,7,1,'existing check','2026-10-29T06:20:00');
         INSERT INTO yandex_connections(user_id,generation,encrypted_tokens,enabled,scenario_id)
         VALUES(1,'generation',X'010203',1,'existing-scenario');
         INSERT INTO yandex_jobs(nagging_id,user_id,generation,scenario_id,created_at)
         VALUES(41,1,'generation','existing-scenario',1793253600);",
    )
    .unwrap();

    let db = Db::init(conn).unwrap();
    assert_eq!(db.preferences(1).await.unwrap().nag_interval_minutes, 60);
    let reminder = db.reminder(1, 7).await.unwrap().unwrap();
    assert_eq!(reminder.description, "existing reminder");
    assert_eq!(
        reminder.anchor_local.unwrap().to_string(),
        "2026-10-29 09:00:00"
    );
    let pending = db.naggings_for_user(1).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, 41);
    assert_eq!(pending[0].description, "existing pending");
    let now = DateTime::parse_from_rfc3339("2026-10-29T07:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(db.due_delivery_users(now).await.unwrap(), vec![1]);
    db.call(|conn| {
        let defaults: (Option<String>, Option<String>, i64) = conn.query_row(
            "SELECT delivery_claim,delivery_lease_until,delivery_attempts FROM naggings WHERE nagging_id=41",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )?;
        assert_eq!(defaults, (None, None, 0));
        let integration: (bool, String, Option<i64>) = conn.query_row(
            "SELECT enabled,scenario_id,retry_after FROM yandex_connections WHERE user_id=1", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )?;
        assert_eq!(integration, (true, "existing-scenario".into(), None));
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM confirmations", [], |row| row.get::<_, i64>(0))?, 1);
        assert_eq!(conn.query_row("SELECT state FROM yandex_jobs", [], |row| row.get::<_, String>(0))?, "pending");
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(version, i64::try_from(MIGRATIONS.len()).unwrap());
        assert!(conn.prepare("PRAGMA foreign_key_check")?.query([])?.next()?.is_none());
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn history_upgrade_preserves_legacy_pending_and_completion_checks() {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    for (index, sql) in MIGRATIONS.iter().take(9).enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.pragma_update(None, "user_version", i64::try_from(index + 1).unwrap())
            .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO users(user_id,first_name,last_name,utc_offset_minutes,timezone)
         VALUES(1,'Ada','',0,'UTC');
         INSERT INTO preferences(user_id,confirmation_check_minutes) VALUES(1,20);
         INSERT INTO naggings(nagging_id,user_id,description,next_at)
         VALUES(41,1,'previous pending','2026-10-29T06:00:00');
         INSERT INTO confirmations(source_nagging_id,user_id,description,due_at)
         VALUES(42,1,'previous check','2026-10-29T06:20:00');",
    )
    .unwrap();
    let db = Db::init(conn).unwrap();
    let now = DateTime::parse_from_rfc3339("2026-10-29T07:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 1);
    let checks = db.due_confirmations(now).await.unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].completion_id, None);
    assert!(
        db.completion_history(1, None, 20)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    db.complete_nagging(1, 41, now).await.unwrap();
    let journal = db.completion_history(1, None, 20).await.unwrap();
    assert_eq!(journal.items.len(), 1);
    assert_eq!(
        journal.items[0].scheduled_at, None,
        "do not invent an old original schedule"
    );
    assert_eq!(journal.items[0].description, "previous pending");
    assert_eq!(
        db.due_confirmations(now + chrono::Duration::minutes(20))
            .await
            .unwrap()
            .len(),
        2
    );
    db.call(|conn| {
        assert!(
            conn.prepare("PRAGMA foreign_key_check")?
                .query([])?
                .next()?
                .is_none()
        );
        Ok(())
    })
    .await
    .unwrap();
}
