use super::*;
use crate::application::delivery::{Delivery, DeliveryQueue, Item};
use crate::domain::{ConfirmationDelay, UserId};
use crate::domain::{Nagging, Recurrence, Reminder, User, UserTz};
use chrono::SubsecRound;

async fn setup() -> (Db, DateTime<Utc>) {
    let db = Db::open_in_memory().unwrap();
    register(&db).await;
    (db, Utc::now().trunc_subsecs(0))
}

async fn register(db: &Db) {
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
        None,
        Some(ConfirmationDelay::TwentyMinutes),
    )
    .await
    .unwrap();
}

async fn occurrence(
    db: &Db,
    now: DateTime<Utc>,
    text: &str,
    recurrence: Recurrence,
) -> (Reminder, Nagging) {
    let reminder = db
        .add_reminder(
            UserId(1),
            text.into(),
            now - Duration::minutes(1),
            recurrence,
        )
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, now, now)
        .await
        .unwrap()
        .unwrap();
    (reminder, nagging)
}

#[tokio::test]
async fn completion_journal_is_private_paginated_and_idempotent() {
    let (db, now) = setup().await;
    let mut ids = Vec::new();
    for index in 0..3 {
        let (_, nagging) = occurrence(&db, now, &format!("entry{index}"), Recurrence::Once).await;
        assert!(
            db.complete_nagging(UserId(1), nagging.id, now + Duration::seconds(index))
                .await
                .unwrap()
        );
        assert!(
            !db.complete_nagging(UserId(1), nagging.id, now + Duration::seconds(index))
                .await
                .unwrap()
        );
        ids.push(
            db.completion_history(UserId(1), None, 1)
                .await
                .unwrap()
                .items[0]
                .id,
        );
    }
    let first = db.completion_history(UserId(1), None, 2).await.unwrap();
    assert_eq!(
        first.items.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![ids[2], ids[1]]
    );
    assert_eq!(first.next_cursor, Some(ids[1]));
    assert!(
        first
            .items
            .iter()
            .all(|item| item.can_undo && item.scheduled_at == Some(now - Duration::minutes(1)))
    );
    let second = db
        .completion_history(UserId(1), first.next_cursor, 2)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].id, ids[0]);
    assert_eq!(second.next_cursor, None);
    assert!(
        db.completion_history(UserId(2), None, 20)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        db.undo_completion(UserId(2), ids[2], now).await.unwrap(),
        UndoResult::NotFound
    );
}

#[tokio::test]
async fn undo_restores_once_cancels_the_check_and_does_not_enqueue_voice() {
    let (db, now) = setup().await;
    db.call(|conn| {
            conn.execute("INSERT INTO yandex_connections(user_id,generation,encrypted_tokens,enabled,scenario_id) VALUES(1,'test',X'01',1,'speaker')",[])?;
            Ok(())
        }).await.unwrap();
    let (_, nagging) = occurrence(&db, now, "completed once", Recurrence::Once).await;
    assert!(
        db.complete_nagging(UserId(1), nagging.id, now)
            .await
            .unwrap()
    );
    let entry = db
        .completion_history(UserId(1), None, 20)
        .await
        .unwrap()
        .items
        .remove(0);
    let check = db
        .due_confirmations(now + Duration::minutes(20))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(check.completion_id, Some(entry.id));
    assert_eq!(
        db.undo_completion(UserId(1), entry.id, now + Duration::minutes(1))
            .await
            .unwrap(),
        UndoResult::Restored
    );
    assert_eq!(
        db.undo_completion(UserId(1), entry.id, now + Duration::minutes(2))
            .await
            .unwrap(),
        UndoResult::AlreadyRestored
    );
    let pending = db.naggings_for_user(UserId(1)).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_ne!(pending[0].id, nagging.id);
    assert_eq!(pending[0].description, "completed once");
    assert_eq!(pending[0].next_at, now + Duration::minutes(11));
    assert!(
        db.due_confirmations(now + Duration::days(1))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.call(
            |conn| conn.query_row("SELECT COUNT(*) FROM yandex_jobs", [], |row| row
                .get::<_, i64>(0))
        )
        .await
        .unwrap(),
        0
    );
    let journal = db.completion_history(UserId(1), None, 20).await.unwrap();
    assert_eq!(journal.items[0].undone_at, Some(now + Duration::minutes(1)));
    assert!(!journal.items[0].can_undo);
    assert!(
        db.complete_nagging(UserId(1), pending[0].id, now + Duration::minutes(3))
            .await
            .unwrap()
    );
    assert_eq!(
        db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items
            .len(),
        2
    );
}

#[tokio::test]
async fn newer_or_deleted_recurring_occurrences_cannot_be_resurrected() {
    let (db, now) = setup().await;
    let (reminder, nagging) = occurrence(&db, now, "daily", Recurrence::Daily).await;
    db.complete_nagging(UserId(1), nagging.id, now)
        .await
        .unwrap();
    let first = db
        .completion_history(UserId(1), None, 20)
        .await
        .unwrap()
        .items[0]
        .id;
    let next = db
        .fire_reminder(
            reminder.id,
            now + Duration::days(1),
            now + Duration::days(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        !db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items[0]
            .can_undo
    );
    assert_eq!(
        db.undo_completion(UserId(1), first, now + Duration::days(1))
            .await
            .unwrap(),
        UndoResult::Unavailable
    );
    db.complete_nagging(UserId(1), next.id, now + Duration::days(1))
        .await
        .unwrap();
    let journal = db.completion_history(UserId(1), None, 20).await.unwrap();
    assert_eq!(journal.items.len(), 2);
    assert!(journal.items[0].can_undo);
    assert!(!journal.items[1].can_undo);
    assert_eq!(
        db.undo_completion(UserId(1), first, now + Duration::days(1))
            .await
            .unwrap(),
        UndoResult::Unavailable
    );
    db.delete_reminder(UserId(1), reminder.id).await.unwrap();
    assert!(
        db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items
            .iter()
            .all(|item| !item.can_undo)
    );
    assert_eq!(
        db.undo_completion(UserId(1), journal.items[0].id, now + Duration::days(1))
            .await
            .unwrap(),
        UndoResult::Unavailable
    );
}

#[tokio::test]
async fn concurrent_undo_creates_one_occurrence_and_invalidates_an_inflight_check() {
    let (db, now) = setup().await;
    let (_, nagging) = occurrence(&db, now, "once", Recurrence::Once).await;
    db.complete_nagging(UserId(1), nagging.id, now)
        .await
        .unwrap();
    let entry = db
        .completion_history(UserId(1), None, 20)
        .await
        .unwrap()
        .items
        .remove(0);
    let sent_at = now + Duration::minutes(20);
    let claimed = db
        .claim_delivery(UserId(1), sent_at, sent_at + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claimed.item, Item::Confirmation(_)));
    let (a, b) = tokio::join!(
        db.undo_completion(UserId(1), entry.id, sent_at),
        db.undo_completion(UserId(1), entry.id, sent_at)
    );
    let results = [a.unwrap(), b.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == UndoResult::Restored)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == UndoResult::AlreadyRestored)
            .count(),
        1
    );
    db.finish_delivery(claimed, Delivery::Unreachable, sent_at)
        .await
        .unwrap();
    assert_eq!(db.naggings_for_user(UserId(1)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn legacy_acknowledgement_records_history_and_unregister_removes_it() {
    let (db, now) = setup().await;
    let (reminder, _) = occurrence(&db, now, "legacy", Recurrence::Daily).await;
    assert!(
        !db.acknowledge_reminder(UserId(2), reminder.id)
            .await
            .unwrap()
    );
    assert!(
        db.acknowledge_reminder(UserId(1), reminder.id)
            .await
            .unwrap()
    );
    assert!(
        !db.acknowledge_reminder(UserId(1), reminder.id)
            .await
            .unwrap()
    );
    assert_eq!(
        db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    db.delete_user(UserId(1)).await.unwrap();
    assert!(
        db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn journal_and_restored_occurrence_survive_reopening() {
    let path = std::env::temp_dir().join(format!(
        "naggy-history-{}.sqlite",
        super::super::mapping::new_token().unwrap()
    ));
    let db = Db::open(&path).unwrap();
    register(&db).await;
    let now = Utc::now().trunc_subsecs(0);
    let (_, nagging) = occurrence(&db, now, "persistent", Recurrence::Once).await;
    db.complete_nagging(UserId(1), nagging.id, now)
        .await
        .unwrap();
    let id = db
        .completion_history(UserId(1), None, 20)
        .await
        .unwrap()
        .items[0]
        .id;
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items[0]
            .id,
        id
    );
    db.undo_completion(UserId(1), id, now + Duration::minutes(1))
        .await
        .unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert!(
        !db.completion_history(UserId(1), None, 20)
            .await
            .unwrap()
            .items[0]
            .can_undo
    );
    assert_eq!(db.naggings_for_user(UserId(1)).await.unwrap().len(), 1);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn removing_a_newer_unreachable_occurrence_does_not_reenable_an_old_undo() {
    let (db, now) = setup().await;
    db.set_preferences(UserId(1), None, None, Some(ConfirmationDelay::Disabled))
        .await
        .unwrap();
    let (reminder, first) = occurrence(&db, now, "daily", Recurrence::Daily).await;
    db.complete_nagging(UserId(1), first.id, now).await.unwrap();
    let completion = db
        .completion_for_source(UserId(1), first.id)
        .await
        .unwrap()
        .unwrap();
    assert!(completion.can_undo);
    let next_at = now + Duration::days(1);
    let next = db
        .fire_reminder(reminder.id, next_at, next_at)
        .await
        .unwrap()
        .unwrap();
    let job = db
        .claim_delivery(UserId(1), next_at, next_at + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(&job.item,Item::Reminder(item) if item.id==next.id));
    db.finish_delivery(job, Delivery::Unreachable, next_at)
        .await
        .unwrap();
    assert!(db.naggings_for_user(UserId(1)).await.unwrap().is_empty());
    assert!(
        !db.completion_for_source(UserId(1), first.id)
            .await
            .unwrap()
            .unwrap()
            .can_undo
    );
    assert_eq!(
        db.undo_completion(UserId(1), completion.id, next_at)
            .await
            .unwrap(),
        UndoResult::Unavailable
    );
    assert!(db.naggings_for_user(UserId(1)).await.unwrap().is_empty());
}
