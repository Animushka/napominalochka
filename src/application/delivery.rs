//! Delivery policy and the transport port. Scheduling never waits for an external API.
//!
//! Pending occurrences are claimed immediately before transport. A durable expiring
//! claim prevents concurrent workers from sending the same event, and makes unfinished
//! work available after a restart. External APIs cannot provide exactly-once delivery:
//! a response lost after Telegram accepted a message can still result in a later repeat.

use crate::domain::UserId;
use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::Context;
use chrono::{DateTime, Duration, SubsecRound, Utc};
#[cfg(test)]
use futures_util::stream;
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio_util::sync::CancellationToken;

use crate::domain::{Confirmation, Nagging};

pub(crate) const MAX_CONCURRENT_CHATS: usize = 8;
const MAX_BATCH_PER_CHAT: usize = 32;
const SEND_TIMEOUT: StdDuration = StdDuration::from_secs(6);
pub(crate) const CLAIM_DURATION: Duration = Duration::seconds(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delivery {
    Sent,
    /// The user blocked the bot or the chat was removed.
    Unreachable,
    /// Retry transport independently from the user's reminder interval.
    Failed(String),
}

pub(crate) trait Notifier: Send + Sync + 'static {
    fn send(&self, nagging: &Nagging) -> impl Future<Output = Delivery> + Send;
    fn send_confirmation(
        &self,
        confirmation: &Confirmation,
    ) -> impl Future<Output = Delivery> + Send;
}

#[derive(Debug, Clone)]
pub(crate) enum Item {
    Reminder(Nagging),
    Confirmation(Confirmation),
}

impl Item {
    pub(crate) fn identity(&self) -> (i64, UserId) {
        match self {
            Self::Reminder(item) => (item.id.0, item.user_id),
            Self::Confirmation(item) => (item.id.0, item.user_id),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Claimed {
    pub(crate) item: Item,
    pub(crate) token: String,
    pub(crate) attempts: i64,
}

/// Persistence port; delivery policy does not depend on SQLite or its schema.
pub(crate) trait DeliveryQueue: Send + Sync {
    fn due_delivery_users(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = crate::application::ports::StorageResult<Vec<UserId>>> + Send;
    fn claim_delivery(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> impl Future<Output = crate::application::ports::StorageResult<Option<Claimed>>> + Send;
    fn finish_delivery(
        &self,
        job: Claimed,
        outcome: Delivery,
        now: DateTime<Utc>,
    ) -> impl Future<Output = crate::application::ports::StorageResult<()>> + Send;
}

type ChatWork = Pin<Box<dyn Future<Output = (UserId, anyhow::Result<()>)> + Send>>;

/// Bounded delivery workers with a FIFO of ready chats. Completing one chat starts
/// the next immediately; the planner's polling interval never throttles this queue.
pub(crate) struct Workers<N, Q> {
    notifier: Arc<N>,
    queue: Q,
    shutdown: CancellationToken,
    running: FuturesUnordered<ChatWork>,
    busy: BTreeSet<UserId>,
    pending: VecDeque<UserId>,
    queued: BTreeSet<UserId>,
}

impl<N: Notifier, Q: DeliveryQueue + Clone + 'static> Workers<N, Q> {
    pub(crate) fn new(notifier: N, queue: Q, shutdown: CancellationToken) -> Self {
        Self {
            notifier: Arc::new(notifier),
            queue,
            shutdown,
            running: FuturesUnordered::new(),
            busy: BTreeSet::new(),
            pending: VecDeque::new(),
            queued: BTreeSet::new(),
        }
    }

    pub(crate) fn has_running(&self) -> bool {
        !self.running.is_empty()
    }

    pub(crate) fn enqueue(&mut self, users: impl IntoIterator<Item = UserId>) {
        for user_id in users {
            if !self.busy.contains(&user_id) && self.queued.insert(user_id) {
                self.pending.push_back(user_id);
            }
        }
        self.refill();
    }

    pub(crate) async fn next_completion(&mut self) -> Option<(UserId, anyhow::Result<()>)> {
        let finished = self.running.next().await?;
        self.busy.remove(&finished.0);
        self.refill();
        Some(finished)
    }

    fn refill(&mut self) {
        while self.busy.len() < MAX_CONCURRENT_CHATS && !self.shutdown.is_cancelled() {
            let Some(user_id) = self.pending.pop_front() else {
                break;
            };
            self.queued.remove(&user_id);
            if !self.busy.insert(user_id) {
                continue;
            }
            let notifier = Arc::clone(&self.notifier);
            let queue = self.queue.clone();
            let shutdown = self.shutdown.clone();
            self.running.push(Box::pin(async move {
                // Start the injected monotonic clock when this worker is actually
                // polled, not before potentially slow planning or queue maintenance.
                let now = Utc::now().trunc_subsecs(0);
                let result = deliver_chat(&*notifier, &queue, user_id, now, &shutdown).await;
                (user_id, result)
            }));
        }
    }
}

/// The retry schedule is deliberately separate from the user-selected nag frequency.
pub(crate) fn retry_delay(previous_failures: i64) -> Duration {
    let multiplier = 1_i64 << previous_failures.clamp(0, 3);
    Duration::seconds((60 * multiplier).min(300))
}

/// Each chat is sequential; independent chats share a bounded number of requests.
#[cfg(test)]
pub(crate) async fn drain<N: Notifier, Q: DeliveryQueue>(
    notifier: &N,
    db: &Q,
    now: DateTime<Utc>,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let users = db.due_delivery_users(now).await?;
    let mut work = stream::iter(users)
        .map(|user_id| deliver_chat(notifier, db, user_id, now, shutdown))
        .buffer_unordered(MAX_CONCURRENT_CHATS);
    while let Some(result) = work.next().await {
        result?;
    }
    Ok(())
}

pub(crate) async fn deliver_chat<N: Notifier, Q: DeliveryQueue>(
    notifier: &N,
    db: &Q,
    user_id: UserId,
    now: DateTime<Utc>,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let started = tokio::time::Instant::now();
    for _ in 0..MAX_BATCH_PER_CHAT {
        if shutdown.is_cancelled() {
            break;
        }
        let attempt_now = current_time(now, started)?;
        let Some(job) = db
            .claim_delivery(user_id, attempt_now, attempt_now + CLAIM_DURATION)
            .await?
        else {
            break;
        };
        // An interrupted request may already have reached Telegram. Keep its lease
        // until expiry rather than immediately replaying a potentially accepted send.
        let send = async {
            match &job.item {
                Item::Reminder(item) => notifier.send(item).await,
                Item::Confirmation(item) => notifier.send_confirmation(item).await,
            }
        };
        let outcome = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            result = tokio::time::timeout(SEND_TIMEOUT, send) => {
                result.unwrap_or_else(|_| Delivery::Failed("Telegram delivery timed out".into()))
            }
        };
        if let Delivery::Failed(ref error) = outcome {
            tracing::warn!(
                user_id = %user_id,
                error,
                "Telegram delivery failed; technical retry scheduled"
            );
        }
        db.finish_delivery(job, outcome, current_time(now, started)?)
            .await?;
    }
    Ok(())
}

/// Keep deterministic injected clocks while advancing them during a slow chat batch.
fn current_time(
    base: DateTime<Utc>,
    started: tokio::time::Instant,
) -> anyhow::Result<DateTime<Utc>> {
    let elapsed = Duration::from_std(started.elapsed()).context("delivery interval overflow")?;
    base.checked_add_signed(elapsed)
        .context("delivery timestamp overflow")
}
