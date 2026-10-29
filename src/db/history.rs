//! Durable completion journal and transactional restoration of accidental completion.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Db, DbTime};
use crate::model::{Completion, HistoryPage, ReminderSettings, UndoResult};

// Reuse this predicate for rendering availability and for the mutation itself. A
// historical occurrence must never replace an already newer recurring occurrence.
const CAN_UNDO: &str = "h.undone_at IS NULL AND (
    h.reminder_id IS NULL OR (
        EXISTS (SELECT 1 FROM reminders r WHERE r.reminder_id = h.reminder_id AND r.user_id = h.user_id
                AND (r.last_fired_nagging_id IS NULL OR r.last_fired_nagging_id <= h.source_nagging_id))
        AND NOT EXISTS (SELECT 1 FROM naggings n WHERE n.reminder_id = h.reminder_id AND n.user_id = h.user_id)
        AND NOT EXISTS (SELECT 1 FROM completion_history newer WHERE newer.reminder_id = h.reminder_id
                        AND newer.user_id = h.user_id AND newer.source_nagging_id > h.source_nagging_id)
    )
)";

/// Journal insertion, optional follow-up and removal of the pending occurrence are
/// one transaction. It is shared by modern occurrence buttons and legacy buttons.
pub(super) fn complete(
    tx: &Transaction<'_>,
    user_id: i64,
    nagging_id: i64,
    now: DateTime<Utc>,
) -> rusqlite::Result<bool> {
    let completed = tx.query_row(
        "DELETE FROM naggings WHERE nagging_id=?1 AND user_id=?2 RETURNING description,reminder_id,scheduled_at,nag_interval_minutes,confirmation_check_minutes,voice_enabled",
        [nagging_id,user_id],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,Option<i64>>(1)?,row.get::<_,Option<DbTime>>(2)?,
            ReminderSettings { nag_interval_minutes: row.get(3)?,confirmation_check_minutes:row.get(4)?,voice_enabled:row.get(5)? })),
    ).optional()?;
    let Some((description, reminder_id, scheduled_at, settings)) = completed else {
        return Ok(false);
    };
    tx.execute(
        "INSERT INTO completion_history(user_id,source_nagging_id,reminder_id,description,scheduled_at,completed_at,nag_interval_minutes,confirmation_check_minutes,voice_enabled)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![user_id,nagging_id,reminder_id,description,scheduled_at,DbTime(now),settings.nag_interval_minutes,settings.confirmation_check_minutes,settings.voice_enabled],
    )?;
    let completion_id = tx.last_insert_rowid();
    let inherited: i64 = tx
        .query_row(
            "SELECT confirmation_check_minutes FROM preferences WHERE user_id=?1",
            [user_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let delay = settings.confirmation_check_minutes.unwrap_or(inherited);
    if delay == 20 {
        tx.execute(
            "INSERT INTO confirmations(source_nagging_id,reminder_id,user_id,description,due_at,completion_id)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![nagging_id,reminder_id,user_id,description,DbTime(now+Duration::minutes(delay)),completion_id],
        )?;
    }
    Ok(true)
}

fn completion_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Completion> {
    Ok(Completion {
        id: row.get(0)?,
        user_id: row.get(1)?,
        description: row.get(2)?,
        scheduled_at: row.get::<_, Option<DbTime>>(3)?.map(|time| time.0),
        completed_at: row.get::<_, DbTime>(4)?.0,
        undone_at: row.get::<_, Option<DbTime>>(5)?.map(|time| time.0),
        can_undo: row.get(6)?,
    })
}

impl Db {
    pub async fn completion_history(
        &self,
        user_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> anyhow::Result<HistoryPage> {
        let limit = limit.clamp(1, 100);
        self.call(move |conn| {
            let sql = format!(
                "SELECT h.completion_id,h.user_id,h.description,h.scheduled_at,h.completed_at,h.undone_at,({CAN_UNDO})
                 FROM completion_history h WHERE h.user_id=?1 AND (?2 IS NULL OR h.completion_id < ?2)
                 ORDER BY h.completion_id DESC LIMIT ?3"
            );
            let mut items: Vec<Completion> = conn.prepare(&sql)?.query_map(
                params![user_id,before,i64::try_from(limit+1).unwrap_or(101)],
                |row| Ok(Completion {
                    id: row.get(0)?, user_id: row.get(1)?, description: row.get(2)?,
                    scheduled_at: row.get::<_, Option<DbTime>>(3)?.map(|at| at.0),
                    completed_at: row.get::<_, DbTime>(4)?.0,
                    undone_at: row.get::<_, Option<DbTime>>(5)?.map(|at| at.0),
                    can_undo: row.get(6)?,
                }),
            )?.collect::<rusqlite::Result<_>>()?;
            let has_more = items.len() > limit;
            items.truncate(limit);
            let next_cursor = if has_more { items.last().map(|item|item.id) } else { None };
            Ok(HistoryPage { items, next_cursor })
        }).await
    }

    pub async fn completion_for_source(
        &self,
        user_id: i64,
        nagging_id: i64,
    ) -> anyhow::Result<Option<Completion>> {
        self.call(move |conn| {
            let sql = format!("SELECT h.completion_id,h.user_id,h.description,h.scheduled_at,h.completed_at,h.undone_at,({CAN_UNDO}) FROM completion_history h WHERE h.user_id=?1 AND h.source_nagging_id=?2");
            conn.query_row(&sql,[user_id,nagging_id],completion_from_row).optional()
        }).await
    }

    pub async fn undo_completion(
        &self,
        user_id: i64,
        completion_id: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<UndoResult> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let sql = format!(
                "SELECT h.description,h.reminder_id,h.scheduled_at,h.undone_at,({CAN_UNDO}),h.nag_interval_minutes,h.confirmation_check_minutes,h.voice_enabled
                 FROM completion_history h WHERE h.user_id=?1 AND h.completion_id=?2"
            );
            let entry = tx.query_row(&sql, [user_id,completion_id], |row| {
                Ok((row.get::<_,String>(0)?,row.get::<_,Option<i64>>(1)?,row.get::<_,Option<DbTime>>(2)?,
                    row.get::<_,Option<DbTime>>(3)?,row.get::<_,bool>(4)?,ReminderSettings { nag_interval_minutes:row.get(5)?,confirmation_check_minutes:row.get(6)?,voice_enabled:row.get(7)? }))
            }).optional()?;
            let Some((description,reminder_id,scheduled_at,undone_at,allowed,settings)) = entry else {
                return Ok(UndoResult::NotFound);
            };
            if undone_at.is_some() { return Ok(UndoResult::AlreadyRestored); }
            if !allowed { return Ok(UndoResult::Unavailable); }
            // A surviving recurring reminder uses the user's current settings.
            // Consumed one-offs have no parent, so their journal snapshot is authoritative.
            let settings = match reminder_id {
                Some(reminder_id) => tx.query_row(
                    "SELECT nag_interval_minutes,confirmation_check_minutes,voice_enabled FROM reminders WHERE user_id=?1 AND reminder_id=?2",
                    [user_id,reminder_id],
                    |row| Ok(ReminderSettings { nag_interval_minutes:row.get(0)?,confirmation_check_minutes:row.get(1)?,voice_enabled:row.get(2)? }),
                )?,
                None => settings,
            };
            tx.execute("DELETE FROM confirmations WHERE completion_id=?1 AND user_id=?2", [completion_id,user_id])?;
            tx.execute(
                "INSERT INTO naggings(reminder_id,user_id,description,next_at,scheduled_at,nag_interval_minutes,confirmation_check_minutes,voice_enabled) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![reminder_id,user_id,description,DbTime(now+Duration::minutes(10)),scheduled_at,settings.nag_interval_minutes,settings.confirmation_check_minutes,settings.voice_enabled],
            )?;
            let restored_id = tx.last_insert_rowid();
            if let Some(reminder_id) = reminder_id {
                tx.execute(
                    "UPDATE reminders SET last_fired_nagging_id=?1 WHERE user_id=?2 AND reminder_id=?3",
                    params![restored_id,user_id,reminder_id],
                )?;
            }
            tx.execute(
                "UPDATE completion_history SET undone_at=?1,restored_nagging_id=?2 WHERE completion_id=?3 AND user_id=?4",
                params![DbTime(now),restored_id,completion_id,user_id],
            )?;
            tx.commit()?;
            Ok(UndoResult::Restored)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::{Delivery, DeliveryQueue, Item};
    use crate::model::{Nagging, Recurrence, Reminder, User, UserTz};
    use chrono::SubsecRound;

    async fn setup() -> (Db, DateTime<Utc>) {
        let db = Db::open_in_memory().unwrap();
        register(&db).await;
        (db, Utc::now().trunc_subsecs(0))
    }

    async fn register(db: &Db) {
        db.upsert_user(User {
            id: 1,
            first_name: "Ada".into(),
            last_name: String::new(),
            tz: UserTz::parse("UTC").unwrap(),
        })
        .await
        .unwrap();
        db.set_preferences(1, None, None, Some(20)).await.unwrap();
    }

    async fn occurrence(
        db: &Db,
        now: DateTime<Utc>,
        text: &str,
        recurrence: Recurrence,
    ) -> (Reminder, Nagging) {
        let reminder = db
            .add_reminder(1, text.into(), now - Duration::minutes(1), recurrence)
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
            let (_, nagging) =
                occurrence(&db, now, &format!("entry{index}"), Recurrence::Once).await;
            assert!(
                db.complete_nagging(1, nagging.id, now + Duration::seconds(index))
                    .await
                    .unwrap()
            );
            assert!(
                !db.complete_nagging(1, nagging.id, now + Duration::seconds(index))
                    .await
                    .unwrap()
            );
            ids.push(db.completion_history(1, None, 1).await.unwrap().items[0].id);
        }
        let first = db.completion_history(1, None, 2).await.unwrap();
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
            .completion_history(1, first.next_cursor, 2)
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].id, ids[0]);
        assert_eq!(second.next_cursor, None);
        assert!(
            db.completion_history(2, None, 20)
                .await
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(
            db.undo_completion(2, ids[2], now).await.unwrap(),
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
        assert!(db.complete_nagging(1, nagging.id, now).await.unwrap());
        let entry = db
            .completion_history(1, None, 20)
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
            db.undo_completion(1, entry.id, now + Duration::minutes(1))
                .await
                .unwrap(),
            UndoResult::Restored
        );
        assert_eq!(
            db.undo_completion(1, entry.id, now + Duration::minutes(2))
                .await
                .unwrap(),
            UndoResult::AlreadyRestored
        );
        let pending = db.naggings_for_user(1).await.unwrap();
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
        let journal = db.completion_history(1, None, 20).await.unwrap();
        assert_eq!(journal.items[0].undone_at, Some(now + Duration::minutes(1)));
        assert!(!journal.items[0].can_undo);
        assert!(
            db.complete_nagging(1, pending[0].id, now + Duration::minutes(3))
                .await
                .unwrap()
        );
        assert_eq!(
            db.completion_history(1, None, 20)
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
        db.complete_nagging(1, nagging.id, now).await.unwrap();
        let first = db.completion_history(1, None, 20).await.unwrap().items[0].id;
        let next = db
            .fire_reminder(
                reminder.id,
                now + Duration::days(1),
                now + Duration::days(1),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!db.completion_history(1, None, 20).await.unwrap().items[0].can_undo);
        assert_eq!(
            db.undo_completion(1, first, now + Duration::days(1))
                .await
                .unwrap(),
            UndoResult::Unavailable
        );
        db.complete_nagging(1, next.id, now + Duration::days(1))
            .await
            .unwrap();
        let journal = db.completion_history(1, None, 20).await.unwrap();
        assert_eq!(journal.items.len(), 2);
        assert!(journal.items[0].can_undo);
        assert!(!journal.items[1].can_undo);
        assert_eq!(
            db.undo_completion(1, first, now + Duration::days(1))
                .await
                .unwrap(),
            UndoResult::Unavailable
        );
        db.delete_reminder(1, reminder.id).await.unwrap();
        assert!(
            db.completion_history(1, None, 20)
                .await
                .unwrap()
                .items
                .iter()
                .all(|item| !item.can_undo)
        );
        assert_eq!(
            db.undo_completion(1, journal.items[0].id, now + Duration::days(1))
                .await
                .unwrap(),
            UndoResult::Unavailable
        );
    }

    #[tokio::test]
    async fn concurrent_undo_creates_one_occurrence_and_invalidates_an_inflight_check() {
        let (db, now) = setup().await;
        let (_, nagging) = occurrence(&db, now, "once", Recurrence::Once).await;
        db.complete_nagging(1, nagging.id, now).await.unwrap();
        let entry = db
            .completion_history(1, None, 20)
            .await
            .unwrap()
            .items
            .remove(0);
        let sent_at = now + Duration::minutes(20);
        let claimed = db
            .claim_delivery(1, sent_at, sent_at + Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(claimed.item, Item::Confirmation(_)));
        let (a, b) = tokio::join!(
            db.undo_completion(1, entry.id, sent_at),
            db.undo_completion(1, entry.id, sent_at)
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
        assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn legacy_acknowledgement_records_history_and_unregister_removes_it() {
        let (db, now) = setup().await;
        let (reminder, _) = occurrence(&db, now, "legacy", Recurrence::Daily).await;
        assert!(!db.acknowledge_reminder(2, reminder.id).await.unwrap());
        assert!(db.acknowledge_reminder(1, reminder.id).await.unwrap());
        assert!(!db.acknowledge_reminder(1, reminder.id).await.unwrap());
        assert_eq!(
            db.completion_history(1, None, 20)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        db.delete_user(1).await.unwrap();
        assert!(
            db.completion_history(1, None, 20)
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
            super::super::new_token().unwrap()
        ));
        let db = Db::open(&path).unwrap();
        register(&db).await;
        let now = Utc::now().trunc_subsecs(0);
        let (_, nagging) = occurrence(&db, now, "persistent", Recurrence::Once).await;
        db.complete_nagging(1, nagging.id, now).await.unwrap();
        let id = db.completion_history(1, None, 20).await.unwrap().items[0].id;
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.completion_history(1, None, 20).await.unwrap().items[0].id,
            id
        );
        db.undo_completion(1, id, now + Duration::minutes(1))
            .await
            .unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert!(!db.completion_history(1, None, 20).await.unwrap().items[0].can_undo);
        assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 1);
        drop(db);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn removing_a_newer_unreachable_occurrence_does_not_reenable_an_old_undo() {
        let (db, now) = setup().await;
        db.set_preferences(1, None, None, Some(0)).await.unwrap();
        let (reminder, first) = occurrence(&db, now, "daily", Recurrence::Daily).await;
        db.complete_nagging(1, first.id, now).await.unwrap();
        let completion = db
            .completion_for_source(1, first.id)
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
            .claim_delivery(1, next_at, next_at + Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(&job.item,Item::Reminder(item) if item.id==next.id));
        db.finish_delivery(job, Delivery::Unreachable, next_at)
            .await
            .unwrap();
        assert!(db.naggings_for_user(1).await.unwrap().is_empty());
        assert!(
            !db.completion_for_source(1, first.id)
                .await
                .unwrap()
                .unwrap()
                .can_undo
        );
        assert_eq!(
            db.undo_completion(1, completion.id, next_at).await.unwrap(),
            UndoResult::Unavailable
        );
        assert!(db.naggings_for_user(1).await.unwrap().is_empty());
    }
}
