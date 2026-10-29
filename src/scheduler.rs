//! Advances due schedules and supervises independent per-chat delivery workers.
//!
//! Planning only writes durable occurrences. Delivery policy lives in `delivery`;
//! slow Telegram requests never stop planning or another user's notifications.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::delivery;
use crate::delivery::DeliveryQueue;
use crate::health::Heartbeat;
#[cfg(test)]
use crate::model::{Confirmation, Nagging};

// Compatibility for existing Telegram adapters; the transport port belongs to delivery.
pub use crate::delivery::{Delivery, Notifier};

const TICK_INTERVAL: StdDuration = StdDuration::from_secs(10);
const MAINTENANCE_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);
const DRAFT_LIFETIME: Duration = Duration::days(1);

pub async fn run_monitored<N: Notifier>(
    notifier: N,
    db: Db,
    shutdown: CancellationToken,
    heartbeat: Heartbeat,
) {
    run_loop(notifier, db, shutdown, Some(heartbeat)).await;
}

#[cfg(test)]
async fn run<N: Notifier>(notifier: N, db: Db, shutdown: CancellationToken) {
    run_loop(notifier, db, shutdown, None).await;
}

async fn run_loop<N: Notifier>(
    notifier: N,
    db: Db,
    shutdown: CancellationToken,
    heartbeat: Option<Heartbeat>,
) {
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_maintenance = Instant::now();
    let mut workers = delivery::Workers::new(notifier, db.clone(), shutdown.clone());

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            // A permanently busy delivery queue must not starve planning/heartbeat.
            _ = ticker.tick() => {}
            Some((user_id, result)) = workers.next_completion(), if workers.has_running() => {
                if let Err(error) = result {
                    tracing::error!(?error, user_id, "Telegram delivery worker failed");
                }
                continue;
            }
        }

        let now = Utc::now().trunc_subsecs(0);
        let planned = plan(&db, now).await;
        let users = db.due_delivery_users(now).await;
        match (&planned, &users) {
            (Ok(()), Ok(_)) => {
                if let Some(heartbeat) = &heartbeat {
                    heartbeat.beat();
                }
            }
            _ => {
                if let Err(error) = &planned {
                    tracing::error!(?error, "failed to plan reminders");
                }
                if let Err(error) = &users {
                    tracing::error!(?error, "failed to read delivery queue");
                }
            }
        }
        if let Ok(users) = users {
            workers.enqueue(users);
        }
        if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
            maintain(&db, now).await;
            last_maintenance = Instant::now();
        }
    }
    // Dropping in-flight requests leaves their durable leases intact until expiry.
    // A response may have been lost after acceptance, so immediate replay is avoided.
    drop(workers);
    tracing::info!("scheduler stopped");
}

/// Planning has no transport dependency and can be tested independently of Telegram.
async fn plan(db: &Db, now: DateTime<Utc>) -> anyhow::Result<()> {
    for reminder in db.due_reminders(now).await? {
        if db.fire_reminder(reminder.id, now, now).await?.is_some() {
            tracing::info!(
                reminder_id = reminder.id,
                user_id = reminder.user_id,
                "reminder planned"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
async fn tick<N: Notifier>(notifier: &N, db: &Db, now: DateTime<Utc>) {
    plan(db, now).await.unwrap();
    delivery::drain(notifier, db, now, &CancellationToken::new())
        .await
        .unwrap();
}

#[cfg(test)]
async fn repeat_due_naggings<N: Notifier>(
    notifier: &N,
    db: &Db,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    delivery::drain(notifier, db, now, &CancellationToken::new()).await
}

async fn maintain(db: &Db, now: DateTime<Utc>) {
    if let Err(error) = db.delete_drafts_before(now - DRAFT_LIFETIME).await {
        tracing::error!(?error, "failed to delete old drafts");
    }
    if let Err(error) = db.vacuum().await {
        tracing::error!(?error, "failed to vacuum database");
    }
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
        let mut sent = notifier.take();
        sent.sort(); // Independent chats intentionally have no global send ordering.
        assert_eq!(sent, vec![(1, "user 1".into()), (2, "user 2".into())]);
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

    #[tokio::test]
    async fn technical_retry_is_independent_of_a_sixty_minute_nag_interval() {
        let (notifier, db) = setup().await;
        db.set_preferences(1, None, Some(60), None).await.unwrap();
        db.add_reminder(1, "pills".into(), at(9, 0), Recurrence::Once)
            .await
            .unwrap();
        notifier.set_outcome(1, Delivery::Failed("network unavailable".into()));
        tick(&notifier, &db, at(9, 0)).await;
        assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, at(9, 1));
        tick(&notifier, &db, at(9, 1)).await;
        assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, at(9, 3));
        notifier.set_outcome(1, Delivery::Sent);
        tick(&notifier, &db, at(9, 3)).await;
        assert_eq!(notifier.take(), vec![(1, "pills".into())]);
        assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, at(10, 3));
    }

    #[tokio::test]
    async fn disabling_checks_during_another_send_cancels_queued_checks() {
        struct DisableChecks {
            db: Db,
            sent: Mutex<Vec<i64>>,
        }
        impl Notifier for DisableChecks {
            async fn send(&self, _: &Nagging) -> Delivery {
                unreachable!()
            }
            async fn send_confirmation(&self, item: &Confirmation) -> Delivery {
                self.sent.lock().unwrap().push(item.id);
                self.db
                    .set_preferences(item.user_id, None, None, Some(0))
                    .await
                    .unwrap();
                Delivery::Sent
            }
        }
        let (_, db) = setup().await;
        db.set_preferences(1, None, None, Some(20)).await.unwrap();
        for text in ["first", "cancelled"] {
            let reminder = db
                .add_reminder(1, text.into(), at(8, 0), Recurrence::Daily)
                .await
                .unwrap();
            let occurrence = db
                .fire_reminder(reminder.id, at(8, 0), at(10, 0))
                .await
                .unwrap()
                .unwrap();
            db.complete_nagging(1, occurrence.id, at(8, 40))
                .await
                .unwrap();
        }
        let notifier = DisableChecks {
            db: db.clone(),
            sent: Mutex::new(Vec::new()),
        };
        tick(&notifier, &db, at(9, 0)).await;
        assert_eq!(notifier.sent.lock().unwrap().len(), 1);
        assert!(db.due_confirmations(at(10, 0)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn slow_chat_does_not_block_another_user_or_shutdown() {
        use std::sync::Arc;
        struct SlowChat {
            first_started: Arc<tokio::sync::Notify>,
            second_sent: Arc<tokio::sync::Notify>,
        }
        impl Notifier for SlowChat {
            async fn send(&self, item: &Nagging) -> Delivery {
                if item.user_id == 1 {
                    self.first_started.notify_one();
                    std::future::pending::<()>().await;
                } else {
                    self.second_sent.notify_one();
                }
                Delivery::Sent
            }
            async fn send_confirmation(&self, _: &Confirmation) -> Delivery {
                unreachable!()
            }
        }
        let (_, db) = setup().await;
        let now = Utc::now().trunc_subsecs(0);
        for user_id in [1, 2] {
            db.add_reminder(
                user_id,
                "proof".into(),
                now - Duration::minutes(1),
                Recurrence::Once,
            )
            .await
            .unwrap();
        }
        let first_started = Arc::new(tokio::sync::Notify::new());
        let second_sent = Arc::new(tokio::sync::Notify::new());
        let shutdown = CancellationToken::new();
        let worker = tokio::spawn(run(
            SlowChat {
                first_started: first_started.clone(),
                second_sent: second_sent.clone(),
            },
            db.clone(),
            shutdown.clone(),
        ));
        tokio::time::timeout(StdDuration::from_secs(2), first_started.notified())
            .await
            .unwrap();
        tokio::time::timeout(StdDuration::from_secs(2), second_sent.notified())
            .await
            .unwrap();
        shutdown.cancel();
        tokio::time::timeout(StdDuration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(
            db.claim_delivery(1, now, now + delivery::CLAIM_DURATION)
                .await
                .unwrap()
                .is_none(),
            "interrupted send keeps its durable lease"
        );
    }

    #[tokio::test]
    async fn completed_workers_refill_all_ready_chats_without_another_planner_tick() {
        struct QuickNotifier {
            sent: tokio::sync::mpsc::UnboundedSender<i64>,
        }
        impl Notifier for QuickNotifier {
            async fn send(&self, item: &Nagging) -> Delivery {
                self.sent.send(item.user_id).unwrap();
                Delivery::Sent
            }
            async fn send_confirmation(&self, _: &Confirmation) -> Delivery {
                unreachable!()
            }
        }
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now().trunc_subsecs(0);
        let total = (delivery::MAX_CONCURRENT_CHATS * 3) as i64;
        for user_id in 1..=total {
            db.upsert_user(User {
                id: user_id,
                first_name: format!("user{user_id}"),
                last_name: String::new(),
                tz: UserTz::parse("UTC").unwrap(),
            })
            .await
            .unwrap();
            db.add_reminder(
                user_id,
                "ready".into(),
                now - Duration::minutes(1),
                Recurrence::Once,
            )
            .await
            .unwrap();
        }
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let worker = tokio::spawn(run(QuickNotifier { sent }, db, shutdown.clone()));
        let delivered = tokio::time::timeout(StdDuration::from_secs(5), async {
            let mut users = std::collections::BTreeSet::new();
            for _ in 0..total {
                users.insert(received.recv().await.unwrap());
            }
            users
        })
        .await;
        shutdown.cancel();
        tokio::time::timeout(StdDuration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            delivered.expect("ready chats must drain before the next 10s planning tick"),
            (1..=total).collect()
        );
    }
}
