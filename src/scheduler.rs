//! Background loop that fires due reminders, repeats naggings and maintains the database.
//!
//! Messages go out through a [`Notifier`], so the scheduling logic can be tested without
//! Telegram.

use std::future::Future;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::model::{Confirmation, Nagging};

/// How often the database is polled for due reminders and naggings.
const TICK_INTERVAL: StdDuration = StdDuration::from_secs(10);
const MAINTENANCE_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);
/// Drafts (reminders started but never given a time) are discarded after this long.
const DRAFT_LIFETIME: Duration = Duration::days(1);

/// Outcome of sending one reminder message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// The chat cannot be reached any more (bot blocked, chat deleted, ...).
    Unreachable,
    /// A temporary failure; the nagging is retried at its next interval.
    Failed(String),
}

/// Sends reminder messages to users.
pub trait Notifier: Send + Sync + 'static {
    fn send(&self, nagging: &Nagging) -> impl Future<Output = Delivery> + Send;
    fn send_confirmation(
        &self,
        confirmation: &Confirmation,
    ) -> impl Future<Output = Delivery> + Send;
}

pub async fn run<N: Notifier>(notifier: N, db: Db, shutdown: CancellationToken) {
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_maintenance = Instant::now();

    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => {}
        }

        let now = Utc::now().trunc_subsecs(0);
        tick(&notifier, &db, now).await;

        if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
            maintain(&db, now).await;
            last_maintenance = Instant::now();
        }
    }
    tracing::info!("scheduler stopped");
}

/// Fires reminders that are due and repeats naggings that are due, as of `now`.
pub async fn tick<N: Notifier>(notifier: &N, db: &Db, now: DateTime<Utc>) {
    if let Err(error) = fire_due_reminders(notifier, db, now).await {
        tracing::error!(?error, "failed to process due reminders");
    }
    if let Err(error) = repeat_due_naggings(notifier, db, now).await {
        tracing::error!(?error, "failed to process due naggings");
    }
    if let Err(error) = send_due_confirmations(notifier, db, now).await {
        tracing::error!(?error, "failed to process completion checks");
    }
}

async fn maintain(db: &Db, now: DateTime<Utc>) {
    if let Err(error) = db.delete_drafts_before(now - DRAFT_LIFETIME).await {
        tracing::error!(?error, "failed to delete old drafts");
    }
    match db.vacuum().await {
        Ok(()) => tracing::info!("vacuumed database"),
        Err(error) => tracing::error!(?error, "failed to vacuum database"),
    }
}

async fn fire_due_reminders<N: Notifier>(
    notifier: &N,
    db: &Db,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    for reminder in db.due_reminders(now).await? {
        // Record the firing before sending: a failed send is retried by the nagging itself.
        let interval =
            Duration::minutes(db.preferences(reminder.user_id).await?.nag_interval_minutes);
        if let Some(nagging) = db.fire_reminder(reminder.id, now, now + interval).await? {
            tracing::info!(
                reminder_id = reminder.id,
                user_id = reminder.user_id,
                "reminder fired"
            );
            deliver(notifier, db, &nagging).await?;
        }
    }
    Ok(())
}

async fn repeat_due_naggings<N: Notifier>(
    notifier: &N,
    db: &Db,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    for nagging in db.due_naggings(now).await? {
        let interval =
            Duration::minutes(db.preferences(nagging.user_id).await?.nag_interval_minutes);
        if db.claim_nagging(&nagging, now, now + interval).await? {
            deliver(notifier, db, &nagging).await?;
        }
    }
    Ok(())
}

/// One delivery per completion. Its confirmation button never reenters the nagging loop.
async fn send_due_confirmations<N: Notifier>(
    notifier: &N,
    db: &Db,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    for confirmation in db.due_confirmations(now).await? {
        match notifier.send_confirmation(&confirmation).await {
            Delivery::Sent | Delivery::Unreachable => {
                db.delete_confirmation(confirmation.id).await?
            }
            Delivery::Failed(error) => {
                tracing::warn!(
                    error,
                    confirmation_id = confirmation.id,
                    "failed to send completion check; will retry"
                );
                db.postpone_confirmation(confirmation.id, now + Duration::minutes(1))
                    .await?;
            }
        }
    }
    Ok(())
}

/// Sends one reminder message. Delivery failures are logged rather than propagated so that
/// one unreachable chat does not hold up everyone else's reminders.
async fn deliver<N: Notifier>(notifier: &N, db: &Db, nagging: &Nagging) -> anyhow::Result<()> {
    match notifier.send(nagging).await {
        Delivery::Sent => {}
        Delivery::Unreachable => {
            let removed = db.delete_naggings_for_user(nagging.user_id).await?;
            tracing::warn!(
                user_id = nagging.user_id,
                removed,
                "chat unreachable; stopped nagging it"
            );
        }
        Delivery::Failed(error) => {
            tracing::warn!(
                error,
                nagging_id = nagging.id,
                "failed to send reminder; will retry"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::model::{Recurrence, User, UserTz, UtcOffset};
    use chrono::TimeZone;

    /// Records messages instead of sending them; per-user outcomes can be scripted.
    #[derive(Default)]
    struct FakeNotifier {
        sent: Mutex<Vec<(i64, String)>>,
        outcomes: Mutex<HashMap<i64, Delivery>>,
    }

    impl FakeNotifier {
        fn set_outcome(&self, user_id: i64, delivery: Delivery) {
            self.outcomes.lock().unwrap().insert(user_id, delivery);
        }

        fn take(&self) -> Vec<(i64, String)> {
            std::mem::take(&mut self.sent.lock().unwrap())
        }
    }

    impl Notifier for FakeNotifier {
        async fn send(&self, nagging: &Nagging) -> Delivery {
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .get(&nagging.user_id)
                .cloned()
                .unwrap_or(Delivery::Sent);
            if outcome == Delivery::Sent {
                self.sent
                    .lock()
                    .unwrap()
                    .push((nagging.user_id, nagging.description.clone()));
            }
            outcome
        }

        async fn send_confirmation(&self, confirmation: &Confirmation) -> Delivery {
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .get(&confirmation.user_id)
                .cloned()
                .unwrap_or(Delivery::Sent);
            if outcome == Delivery::Sent {
                self.sent.lock().unwrap().push((
                    confirmation.user_id,
                    format!("check: {}", confirmation.description),
                ));
            }
            outcome
        }
    }

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 13, h, m, 0).unwrap()
    }

    async fn setup() -> (FakeNotifier, Db) {
        let db = Db::open_in_memory().unwrap();
        for id in [1, 2] {
            db.upsert_user(User {
                id,
                first_name: "Ada".into(),
                last_name: String::new(),
                tz: UserTz::Fixed(UtcOffset::from_minutes(0).unwrap()),
            })
            .await
            .unwrap();
            db.set_preferences(id, None, Some(1), None).await.unwrap();
        }
        (FakeNotifier::default(), db)
    }

    #[tokio::test]
    async fn stale_repeat_snapshot_skips_snoozed_and_completed_occurrences() {
        struct InterleavingNotifier {
            db: Db,
            snooze_id: i64,
            complete_id: i64,
            sent: Mutex<Vec<i64>>,
        }
        impl Notifier for InterleavingNotifier {
            async fn send(&self, nagging: &Nagging) -> Delivery {
                self.sent.lock().unwrap().push(nagging.id);
                // repeat_due_naggings already loaded every due occurrence. Emulate actions
                // arriving through the app while the first external delivery is in flight.
                self.db
                    .snooze_nagging(1, self.snooze_id, at(10, 0))
                    .await
                    .unwrap();
                self.db
                    .complete_nagging(1, self.complete_id, at(9, 0))
                    .await
                    .unwrap();
                Delivery::Sent
            }
            async fn send_confirmation(&self, _: &Confirmation) -> Delivery {
                unreachable!("only processing repeat deliveries")
            }
        }
        let (_, db) = setup().await;
        let mut naggings = Vec::new();
        for text in ["first", "snoozed", "completed"] {
            let reminder = db
                .add_reminder(1, text.into(), at(9, 0), Recurrence::Once)
                .await
                .unwrap();
            naggings.push(
                db.fire_reminder(reminder.id, at(9, 0), at(9, 0))
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let notifier = InterleavingNotifier {
            db: db.clone(),
            snooze_id: naggings[1].id,
            complete_id: naggings[2].id,
            sent: Mutex::new(Vec::new()),
        };
        repeat_due_naggings(&notifier, &db, at(9, 0)).await.unwrap();
        assert_eq!(*notifier.sent.lock().unwrap(), vec![naggings[0].id]);
        let pending = db.naggings_for_user(1).await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].next_at, at(9, 1));
        assert_eq!(pending[1].id, naggings[1].id);
        assert_eq!(
            pending[1].next_at,
            at(10, 0),
            "stale snapshot must preserve snooze"
        );
    }

    #[tokio::test]
    async fn fires_when_due_then_nags_every_minute_until_done() {
        let (notifier, db) = setup().await;
        db.add_reminder(1, "pills".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();

        tick(&notifier, &db, at(8, 59)).await;
        assert!(notifier.take().is_empty(), "not due yet");

        tick(&notifier, &db, at(9, 0)).await;
        assert_eq!(notifier.take(), vec![(1, "pills".into())]);

        tick(&notifier, &db, at(9, 0) + Duration::seconds(30)).await;
        assert!(notifier.take().is_empty(), "waits a full interval");

        tick(&notifier, &db, at(9, 1)).await;
        assert_eq!(notifier.take(), vec![(1, "pills".into())], "nags again");

        let nagging = db.due_naggings(at(9, 2)).await.unwrap().remove(0);
        db.acknowledge_nagging(1, nagging.id).await.unwrap();
        tick(&notifier, &db, at(9, 5)).await;
        assert!(notifier.take().is_empty(), "done stops the nagging");
    }

    #[tokio::test]
    async fn next_occurrence_replaces_an_unacknowledged_nagging() {
        let (notifier, db) = setup().await;
        db.add_reminder(1, "stretch".into(), at(9, 0), Recurrence::Daily)
            .await
            .unwrap();
        tick(&notifier, &db, at(9, 0)).await;
        notifier.take();

        // A day later: exactly one message for the new occurrence, none from the old nagging.
        let next_day = at(9, 0) + Duration::days(1);
        tick(&notifier, &db, next_day).await;
        assert_eq!(notifier.take(), vec![(1, "stretch".into())]);
        assert_eq!(
            db.due_naggings(next_day + Duration::minutes(1))
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn snoozed_nagging_waits() {
        let (notifier, db) = setup().await;
        db.add_reminder(1, "pills".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        tick(&notifier, &db, at(9, 0)).await;
        notifier.take();

        let nagging = db.due_naggings(at(9, 1)).await.unwrap().remove(0);
        db.snooze_nagging(1, nagging.id, at(10, 0)).await.unwrap();
        tick(&notifier, &db, at(9, 30)).await;
        assert!(notifier.take().is_empty());
        tick(&notifier, &db, at(10, 0)).await;
        assert_eq!(notifier.take(), vec![(1, "pills".into())]);
    }

    #[tokio::test]
    async fn unreachable_chat_stops_nagging_without_affecting_others() {
        let (notifier, db) = setup().await;
        db.add_reminder(1, "blocked".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        db.add_reminder(2, "fine".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        notifier.set_outcome(1, Delivery::Unreachable);

        tick(&notifier, &db, at(9, 0)).await;
        assert_eq!(notifier.take(), vec![(2, "fine".into())]);

        tick(&notifier, &db, at(9, 1)).await;
        assert_eq!(
            notifier.take(),
            vec![(2, "fine".into())],
            "user 1 is no longer nagged"
        );
        assert!(
            db.due_naggings(at(23, 0))
                .await
                .unwrap()
                .iter()
                .all(|n| n.user_id == 2)
        );
    }

    #[tokio::test]
    async fn temporary_failure_is_retried() {
        let (notifier, db) = setup().await;
        db.add_reminder(1, "pills".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        notifier.set_outcome(1, Delivery::Failed("timeout".into()));

        tick(&notifier, &db, at(9, 0)).await;
        assert!(notifier.take().is_empty());

        notifier.set_outcome(1, Delivery::Sent);
        tick(&notifier, &db, at(9, 1)).await;
        assert_eq!(notifier.take(), vec![(1, "pills".into())]);
    }
    #[tokio::test]
    async fn intervals_are_per_user_and_new_default_is_ten_minutes() {
        let (notifier, db) = setup().await;
        db.set_preferences(1, None, Some(10), None).await.unwrap();
        db.set_preferences(2, None, Some(30), None).await.unwrap();
        for id in [1, 2] {
            db.add_reminder(id, format!("user {id}"), at(9, 0), Recurrence::Once)
                .await
                .unwrap();
        }
        tick(&notifier, &db, at(9, 0)).await;
        assert_eq!(notifier.take().len(), 2);
        tick(&notifier, &db, at(9, 9)).await;
        assert!(notifier.take().is_empty());
        tick(&notifier, &db, at(9, 10)).await;
        assert_eq!(notifier.take(), vec![(1, "user 1".into())]);
        tick(&notifier, &db, at(9, 30)).await;
        assert_eq!(
            notifier.take(),
            vec![(1, "user 1".into()), (2, "user 2".into())]
        );
        assert_eq!(
            crate::model::Preferences::default().nag_interval_minutes,
            10
        );
    }

    #[tokio::test]
    async fn completion_creates_one_check_twenty_minutes_later_without_new_nagging() {
        let (notifier, db) = setup().await;
        db.set_preferences(1, None, None, Some(20)).await.unwrap();
        db.add_reminder(1, "already completed".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        tick(&notifier, &db, at(9, 0)).await;
        notifier.take();
        let nagging = db.due_naggings(at(9, 1)).await.unwrap().remove(0);
        assert!(
            !db.complete_nagging(2, nagging.id, at(9, 1)).await.unwrap(),
            "owner only"
        );
        assert!(db.complete_nagging(1, nagging.id, at(9, 1)).await.unwrap());
        assert!(
            !db.complete_nagging(1, nagging.id, at(9, 1)).await.unwrap(),
            "repeated Done is idempotent"
        );
        tick(&notifier, &db, at(9, 20)).await;
        assert!(notifier.take().is_empty());
        tick(&notifier, &db, at(9, 21)).await;
        assert_eq!(
            notifier.take(),
            vec![(1, "check: already completed".into())]
        );
        tick(&notifier, &db, at(10, 0)).await;
        assert!(notifier.take().is_empty());
        assert!(db.due_naggings(at(10, 0)).await.unwrap().is_empty());
        assert!(db.due_confirmations(at(10, 0)).await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn failed_completion_check_is_durable_and_retried_without_nagging() {
        let (notifier, db) = setup().await;
        db.set_preferences(1, None, None, Some(20)).await.unwrap();
        let reminder = db
            .add_reminder(1, "completed".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        let nagging = db
            .fire_reminder(reminder.id, at(9, 0), at(9, 1))
            .await
            .unwrap()
            .unwrap();
        db.complete_nagging(1, nagging.id, at(9, 0)).await.unwrap();
        notifier.set_outcome(1, Delivery::Failed("temporary timeout".into()));
        tick(&notifier, &db, at(9, 20)).await;
        assert!(notifier.take().is_empty());
        assert_eq!(db.due_confirmations(at(9, 21)).await.unwrap().len(), 1);
        notifier.set_outcome(1, Delivery::Sent);
        tick(&notifier, &db, at(9, 20) + Duration::seconds(30)).await;
        assert!(notifier.take().is_empty());
        tick(&notifier, &db, at(9, 21)).await;
        assert_eq!(notifier.take(), vec![(1, "check: completed".into())]);
        tick(&notifier, &db, at(10, 0)).await;
        assert!(notifier.take().is_empty());
        assert!(db.due_naggings(at(10, 0)).await.unwrap().is_empty());
    }
}
