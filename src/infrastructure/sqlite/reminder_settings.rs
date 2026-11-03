//! Typed reminder writes keep partial edits, schedules and inherited settings distinct.

use rusqlite::{OptionalExtension, params};

use super::mapping::{DbLocalTime, DbTime, reminder_from_row};
use super::{Db, REMINDER_COLUMNS};
use crate::domain::{Reminder, ReminderChanges, ReminderWrite};

/// NULL on a surviving parent means inherit, never fall back to an old snapshot.
/// A consumed one-off has no parent and therefore uses its completion snapshot.
/// This expression is correlated to the `confirmations` table in every caller.
pub(super) const EFFECTIVE_CONFIRMATION_CHECK: &str = "COALESCE(
    CASE WHEN EXISTS (SELECT 1 FROM reminders r WHERE r.reminder_id=confirmations.reminder_id AND r.user_id=confirmations.user_id)
      THEN (SELECT r.confirmation_check_minutes FROM reminders r WHERE r.reminder_id=confirmations.reminder_id AND r.user_id=confirmations.user_id)
      ELSE (SELECT h.confirmation_check_minutes FROM completion_history h WHERE h.completion_id=confirmations.completion_id AND h.user_id=confirmations.user_id)
    END,
    (SELECT p.confirmation_check_minutes FROM preferences p WHERE p.user_id=confirmations.user_id),
    0
)";

impl Db {
    pub(crate) async fn insert_reminder(
        &self,
        user_id: i64,
        input: ReminderWrite,
    ) -> anyhow::Result<Reminder> {
        self.call(move |conn| {
            conn.query_row(
                &format!("INSERT INTO reminders(user_id,description,due_at,recurrence,anchor_local,nag_interval_minutes,confirmation_check_minutes,voice_enabled)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8) RETURNING {REMINDER_COLUMNS}"),
                params![user_id,input.description,DbTime(input.due_at),input.recurrence.code(),input.anchor_local.map(DbLocalTime),
                        input.settings.nag_interval_minutes,input.settings.confirmation_check_minutes,input.settings.voice_enabled],
                reminder_from_row,
            )
        }).await
    }

    pub(crate) async fn patch_reminder(
        &self,
        user_id: i64,
        reminder_id: i64,
        changes: ReminderChanges,
    ) -> anyhow::Result<Option<Reminder>> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let schedule = changes.schedule;
            let settings = changes.settings;
            let reminder = tx.query_row(
                &format!("UPDATE reminders SET description=COALESCE(?1,description),
                  due_at=CASE WHEN ?2 THEN ?3 ELSE due_at END,
                  recurrence=CASE WHEN ?2 THEN ?4 ELSE recurrence END,
                  anchor_local=CASE WHEN ?2 THEN ?5 ELSE anchor_local END,
                  nag_interval_minutes=CASE WHEN ?6 THEN ?7 ELSE nag_interval_minutes END,
                  confirmation_check_minutes=CASE WHEN ?6 THEN ?8 ELSE confirmation_check_minutes END,
                  voice_enabled=CASE WHEN ?6 THEN ?9 ELSE voice_enabled END
                  WHERE user_id=?10 AND reminder_id=?11 RETURNING {REMINDER_COLUMNS}"),
                params![changes.description,schedule.is_some(),schedule.as_ref().map(|s|DbTime(s.due_at)),
                    schedule.as_ref().map(|s|s.recurrence.code()),schedule.as_ref().and_then(|s|s.anchor_local).map(DbLocalTime),
                    settings.is_some(),settings.as_ref().and_then(|s|s.nag_interval_minutes),
                    settings.as_ref().and_then(|s|s.confirmation_check_minutes),settings.as_ref().map(|s|s.voice_enabled),
                    user_id,reminder_id],
                reminder_from_row,
            ).optional()?;
            if reminder.is_some() && let Some(settings) = settings {
                tx.execute(
                    "UPDATE naggings SET nag_interval_minutes=?1,confirmation_check_minutes=?2,voice_enabled=?3
                     WHERE user_id=?4 AND reminder_id=?5",
                    params![settings.nag_interval_minutes,settings.confirmation_check_minutes,settings.voice_enabled,user_id,reminder_id],
                )?;
                let inherited_check: i64 = tx.query_row(
                    "SELECT confirmation_check_minutes FROM preferences WHERE user_id=?1",
                    [user_id], |row| row.get(0),
                ).optional()?.unwrap_or(0);
                if settings.confirmation_check_minutes.unwrap_or(inherited_check) == 0 {
                    tx.execute(
                        "DELETE FROM confirmations WHERE user_id=?1 AND reminder_id=?2",
                        [user_id,reminder_id],
                    )?;
                }
                if !settings.voice_enabled {
                    // The worker rechecks queued/claimed work before POST. An external
                    // command already dispatched to the speaker cannot be recalled.
                    tx.execute(
                        "DELETE FROM yandex_jobs WHERE user_id=?1 AND state IN ('pending','claimed')
                         AND nagging_id IN (SELECT nagging_id FROM naggings WHERE user_id=?1 AND reminder_id=?2)",
                        [user_id,reminder_id],
                    )?;
                }
            }
            tx.commit()?;
            Ok(reminder)
        }).await
    }
}

#[cfg(test)]
mod tests;
