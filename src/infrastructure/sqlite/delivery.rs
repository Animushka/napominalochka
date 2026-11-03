//! SQLite adapter for durable Telegram delivery claims and retry scheduling.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, params};

use super::Db;
use super::mapping::{DbTime, new_token};
use super::reminder_settings::EFFECTIVE_CONFIRMATION_CHECK;
use crate::application::delivery::{Claimed, Delivery, DeliveryQueue, Item, retry_delay};
use crate::domain::{Confirmation, Nagging};

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
mod tests;
