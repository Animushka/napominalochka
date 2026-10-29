//! SQLite adapter for durable Telegram delivery claims and retry scheduling.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, params};

use super::reminder_settings::EFFECTIVE_CONFIRMATION_CHECK;
use super::{Db, DbTime, new_token};
use crate::delivery::{Claimed, Delivery, DeliveryQueue, Item, retry_delay};
use crate::model::{Confirmation, Nagging};

fn storage(item: &Item) -> (&'static str, &'static str, &'static str) {
    match item {
        Item::Reminder(_) => ("naggings", "nagging_id", "next_at"),
        Item::Confirmation(_) => ("confirmations", "confirmation_id", "due_at"),
    }
}

impl DeliveryQueue for Db {
    async fn due_delivery_users(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<i64>> {
        self.call(move |conn| {
            conn.prepare(
                &format!("SELECT user_id FROM (
                   SELECT user_id, next_at AS scheduled FROM naggings
                   WHERE next_at <= ?1 AND (delivery_lease_until IS NULL OR delivery_lease_until <= ?1)
                   UNION ALL
                   SELECT user_id, due_at AS scheduled FROM confirmations
                   WHERE due_at <= ?1 AND (delivery_lease_until IS NULL OR delivery_lease_until <= ?1)
                   AND ({EFFECTIVE_CONFIRMATION_CHECK}) = 20
                 ) GROUP BY user_id ORDER BY MIN(scheduled), user_id"),
            )?
            .query_map([DbTime(now)], |row| row.get(0))?
            .collect()
        }).await
    }

    /// Re-read the event in a transaction rather than trusting a scheduler snapshot.
    /// A per-chat active lease serializes multiple process/worker claim attempts.
    async fn claim_delivery(
        &self,
        user_id: i64,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> anyhow::Result<Option<Claimed>> {
        let token = new_token()?;
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM naggings WHERE user_id = ?1 AND delivery_lease_until > ?2)
                 OR EXISTS(SELECT 1 FROM confirmations WHERE user_id = ?1 AND delivery_lease_until > ?2)",
                params![user_id, DbTime(now)], |row| row.get(0),
            )?;
            if active { return Ok(None); }
            let candidate = tx.query_row(
                &format!("SELECT kind, id, description, scheduled, reminder_id, attempts, completion_id FROM (
                   SELECT 0 AS kind, nagging_id AS id, description, next_at AS scheduled, reminder_id, delivery_attempts AS attempts, NULL AS completion_id
                   FROM naggings WHERE user_id = ?1 AND next_at <= ?2
                   AND (delivery_lease_until IS NULL OR delivery_lease_until <= ?2)
                   UNION ALL
                   SELECT 1 AS kind, confirmation_id AS id, description, due_at AS scheduled, NULL AS reminder_id, delivery_attempts AS attempts, completion_id
                   FROM confirmations WHERE user_id = ?1 AND due_at <= ?2
                   AND (delivery_lease_until IS NULL OR delivery_lease_until <= ?2)
                   AND ({EFFECTIVE_CONFIRMATION_CHECK}) = 20
                 ) ORDER BY scheduled, kind, id LIMIT 1"),
                params![user_id, DbTime(now)],
                |row| {
                    let id = row.get(1)?;
                    let description = row.get(2)?;
                    let scheduled = row.get::<_, DbTime>(3)?.0;
                    let item = if row.get::<_, i64>(0)? == 0 {
                        Item::Reminder(Nagging { id, user_id, description, next_at: scheduled, reminder_id: row.get(4)? })
                    } else {
                        Item::Confirmation(Confirmation { id, user_id, description, due_at: scheduled, completion_id: row.get(6)? })
                    };
                    Ok((item, row.get::<_, i64>(5)?))
                },
            ).optional()?;
            let Some((item, attempts)) = candidate else { return Ok(None); };
            let (table, id_column, _) = storage(&item);
            tx.execute(
                &format!("UPDATE {table} SET delivery_claim = ?1, delivery_lease_until = ?2 WHERE {id_column} = ?3"),
                params![token, DbTime(lease_until), item.identity().0],
            )?;
            tx.commit()?;
            Ok(Some(Claimed { item, token, attempts }))
        }).await
    }

    async fn finish_delivery(
        &self,
        job: Claimed,
        outcome: Delivery,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let (id, user_id) = job.item.identity();
            let (table, id_column, due_column) = storage(&job.item);
            // Every outcome belongs to one current claim, including a chat-wide
            // Unreachable result. A late response must not erase newer user work.
            let current: bool = tx.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {id_column} = ?1 AND user_id = ?2 AND delivery_claim = ?3)"),
                params![id, user_id, job.token],
                |row| row.get(0),
            )?;
            if !current { return Ok(()); }
            match outcome {
                Delivery::Sent => match job.item {
                    Item::Reminder(_) => {
                        let interval: i64 = tx.query_row("SELECT COALESCE(n.nag_interval_minutes,p.nag_interval_minutes,10) FROM naggings n LEFT JOIN preferences p ON p.user_id=n.user_id WHERE n.user_id=?1 AND n.nagging_id=?2", [user_id,id], |row| row.get(0))?;
                        tx.execute(
                            "UPDATE naggings SET next_at = ?1, delivery_claim = NULL, delivery_lease_until = NULL, delivery_attempts = 0 WHERE nagging_id = ?2 AND delivery_claim = ?3",
                            params![DbTime(now + Duration::minutes(interval)), id, job.token],
                        )?;
                    }
                    Item::Confirmation(_) => {
                        tx.execute("DELETE FROM confirmations WHERE confirmation_id = ?1 AND delivery_claim = ?2", params![id, job.token])?;
                    }
                },
                Delivery::Unreachable => {
                    tx.execute("DELETE FROM naggings WHERE user_id = ?1", [user_id])?;
                    tx.execute("DELETE FROM confirmations WHERE user_id = ?1", [user_id])?;
                }
                Delivery::Failed(_) => {
                    tx.execute(
                        &format!("UPDATE {table} SET {due_column} = ?1, delivery_claim = NULL, delivery_lease_until = NULL, delivery_attempts = delivery_attempts + 1 WHERE {id_column} = ?2 AND delivery_claim = ?3"),
                        params![DbTime(now + retry_delay(job.attempts)), id, job.token],
                    )?;
                }
            }
            tx.commit()
        }).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Recurrence, User, UserTz};
    use chrono::SubsecRound;

    async fn pending(db: &Db, now: DateTime<Utc>) -> Nagging {
        db.upsert_user(User {
            id: 1,
            first_name: "Ada".into(),
            last_name: String::new(),
            tz: UserTz::parse("UTC").unwrap(),
        })
        .await
        .unwrap();
        let reminder = db
            .add_reminder(1, "proof".into(), now, Recurrence::Once)
            .await
            .unwrap();
        db.fire_reminder(reminder.id, now, now)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn only_one_claim_wins_and_snooze_invalidates_its_completion() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now().trunc_subsecs(0);
        let occurrence = pending(&db, now).await;
        let lease = now + Duration::seconds(30);
        let (a, b) = tokio::join!(
            db.claim_delivery(1, now, lease),
            db.claim_delivery(1, now, lease)
        );
        let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
        assert_eq!(claims.len(), 1);
        let until = now + Duration::hours(1);
        db.snooze_nagging(1, occurrence.id, until).await.unwrap();
        db.finish_delivery(claims.into_iter().next().unwrap(), Delivery::Sent, now)
            .await
            .unwrap();
        assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, until);
        assert!(db.claim_delivery(1, now, lease).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn restart_recovers_an_expired_claim_and_ignores_the_old_completion() {
        let path = std::env::temp_dir().join(format!(
            "napominalochka-delivery-{}.sqlite",
            new_token().unwrap()
        ));
        let now = Utc::now().trunc_subsecs(0);
        let lease = now + Duration::seconds(30);
        let old = {
            let db = Db::open(&path).unwrap();
            pending(&db, now).await;
            db.claim_delivery(1, now, lease).await.unwrap().unwrap()
        };
        {
            let db = Db::open(&path).unwrap();
            assert!(db.claim_delivery(1, now, lease).await.unwrap().is_none());
            let recovered = db
                .claim_delivery(1, lease, lease + Duration::seconds(30))
                .await
                .unwrap()
                .unwrap();
            assert_ne!(recovered.token, old.token);
            db.finish_delivery(old, Delivery::Sent, lease)
                .await
                .unwrap();
            assert!(
                db.claim_delivery(1, lease, lease + Duration::seconds(30))
                    .await
                    .unwrap()
                    .is_none()
            );
            db.finish_delivery(recovered, Delivery::Failed("network".into()), lease)
                .await
                .unwrap();
        }
        {
            let db = Db::open(&path).unwrap();
            assert!(
                db.claim_delivery(
                    1,
                    lease + Duration::seconds(59),
                    lease + Duration::seconds(90)
                )
                .await
                .unwrap()
                .is_none()
            );
            assert!(
                db.claim_delivery(
                    1,
                    lease + Duration::minutes(1),
                    lease + Duration::minutes(2)
                )
                .await
                .unwrap()
                .is_some()
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn late_unreachable_after_snooze_preserves_new_nags_and_checks() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now().trunc_subsecs(0);
        let occurrence = pending(&db, now).await;
        let old = db
            .claim_delivery(1, now, now + Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        db.snooze_nagging(1, occurrence.id, now + Duration::hours(1))
            .await
            .unwrap();
        db.set_preferences(1, None, None, Some(20)).await.unwrap();
        let completed = pending(&db, now).await;
        db.complete_nagging(1, completed.id, now).await.unwrap();
        pending(&db, now).await;

        db.finish_delivery(old, Delivery::Unreachable, now)
            .await
            .unwrap();

        assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 2);
        assert_eq!(
            db.due_confirmations(now + Duration::minutes(21))
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn late_unreachable_from_an_expired_claim_cannot_cancel_a_new_claim() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now().trunc_subsecs(0);
        pending(&db, now).await;
        let expires = now + Duration::seconds(30);
        let old = db.claim_delivery(1, now, expires).await.unwrap().unwrap();
        let recovered = db
            .claim_delivery(1, expires, expires + Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(old.token, recovered.token);

        db.finish_delivery(old, Delivery::Unreachable, expires)
            .await
            .unwrap();

        assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 1);
        assert!(
            db.claim_delivery(1, expires, expires + Duration::seconds(30))
                .await
                .unwrap()
                .is_none()
        );
        db.finish_delivery(recovered, Delivery::Sent, expires)
            .await
            .unwrap();
        assert_eq!(
            db.naggings_for_user(1).await.unwrap()[0].next_at,
            expires + Duration::minutes(10)
        );
    }
}
