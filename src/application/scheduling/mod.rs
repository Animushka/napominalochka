//! Advances due schedules and supervises independent per-chat delivery workers.
//!
//! Planning only writes durable occurrences. Delivery policy lives in `delivery`;
//! slow Telegram requests never stop planning or another user's notifications.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, SubsecRound, Utc};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::application::delivery;
use crate::application::delivery::DeliveryQueue;
use crate::application::health::Heartbeat;
use crate::application::ports::planning::PlanningRepository;
#[cfg(test)]
use crate::domain::{Confirmation, Nagging};

#[cfg(test)]
use crate::application::delivery::Delivery;
use crate::application::delivery::Notifier;

const TICK_INTERVAL: StdDuration = StdDuration::from_secs(10);
const MAINTENANCE_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);
const DRAFT_LIFETIME: Duration = Duration::days(1);

/// Owns reminder planning and the bounded delivery workers for one runtime.
pub(crate) struct Scheduler<R, N> {
    repository: R,
    notifier: N,
}

impl<R: PlanningRepository + DeliveryQueue, N: Notifier> Scheduler<R, N> {
    pub(crate) fn new(repository: R, notifier: N) -> Self {
        Self {
            repository,
            notifier,
        }
    }

    pub(crate) async fn run(self, shutdown: CancellationToken, heartbeat: Heartbeat) {
        run_monitored(self.notifier, self.repository, shutdown, heartbeat).await;
    }
}

async fn run_monitored<N: Notifier, R: PlanningRepository + DeliveryQueue>(
    notifier: N,
    db: R,
    shutdown: CancellationToken,
    heartbeat: Heartbeat,
) {
    run_loop(notifier, db, shutdown, Some(heartbeat)).await;
}

async fn run_loop<N: Notifier, R: PlanningRepository + DeliveryQueue>(
    notifier: N,
    db: R,
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
                    tracing::error!(?error, user_id = %user_id, "Telegram delivery worker failed");
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
async fn plan<R: PlanningRepository>(db: &R, now: DateTime<Utc>) -> anyhow::Result<()> {
    for reminder in db.due_reminders(now).await? {
        if db.fire_reminder(reminder.id, now, now).await?.is_some() {
            tracing::info!(
                reminder_id = %reminder.id,
                user_id = %reminder.user_id,
                "reminder planned"
            );
        }
    }
    Ok(())
}

async fn maintain<R: PlanningRepository>(db: &R, now: DateTime<Utc>) {
    if let Err(error) = db.delete_drafts_before(now - DRAFT_LIFETIME).await {
        tracing::error!(?error, "failed to delete old drafts");
    }
    if let Err(error) = db.maintain_storage().await {
        tracing::error!(?error, "failed to maintain reminder storage");
    }
}

#[cfg(test)]
mod tests;
