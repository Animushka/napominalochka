//! Durable scenario queue orchestration. Preparation is repeatable; the atomic
//! dispatch claim marks the boundary after which an action must never be replayed.

use super::{ACTION_COOLDOWN, ActionResult, Error, Job, MAX_JOB_AGE, Result, Yandex};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl Yandex {
    pub async fn run_worker(&self, shutdown: CancellationToken) {
        if self.recover_claims().await.is_err() {
            tracing::error!("Yandex queue recovery failed");
        }
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                _ = timer.tick() => {}
            }
            if self.worker_tick_until(&shutdown).await.is_err() {
                tracing::warn!("Yandex queue operation failed");
            }
            if shutdown.is_cancelled() {
                break;
            }
            if self.refresh_tick_until(&shutdown).await.is_err() {
                tracing::warn!("Yandex token maintenance failed");
            }
        }
    }

    pub(super) async fn recover_claims(&self) -> Result<()> {
        self.inner.db.call(|conn| {
            let tx = conn.transaction()?;
            tx.execute("UPDATE yandex_connections SET enabled = 0, last_error = 'yandex_delivery_uncertain' WHERE EXISTS (SELECT 1 FROM yandex_jobs j WHERE j.user_id = yandex_connections.user_id AND j.generation = yandex_connections.generation AND j.state = 'claimed')", [])?;
            tx.execute("UPDATE yandex_jobs SET state = 'failed', last_error = 'yandex_delivery_uncertain' WHERE state = 'claimed'", [])?;
            tx.commit()
        }).await?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn worker_tick(&self) -> Result<()> {
        self.worker_tick_until(&CancellationToken::new()).await
    }

    pub(super) async fn worker_tick_until(&self, shutdown: &CancellationToken) -> Result<()> {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let Some(job) = self.next_job().await? else {
            return Ok(());
        };
        let _guard = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            guard = self.lock(job.user_id) => guard,
        };
        let c = match self.require_connection(job.user_id).await {
            Ok(c) => c,
            Err(error) => {
                self.record_failure(job.user_id, &job.generation, error)
                    .await?;
                return Ok(());
            }
        };
        let token = match self
            .prepare_action(job.user_id, &c, &job.scenario_id, shutdown)
            .await
        {
            Ok(Some(token)) => token,
            Ok(None) => return Ok(()),
            Err(error) => {
                self.record_failure(job.user_id, &job.generation, error)
                    .await?;
                return Ok(());
            }
        };
        // The pending row is claimed only after safe preparation. A read timeout or
        // shutdown before this point leaves work pending, not falsely uncertain.
        if shutdown.is_cancelled() || !self.claim_job(job.clone()).await? {
            return Ok(());
        }
        let result = self
            .dispatch_action(
                job.user_id,
                &c,
                &job.scenario_id,
                Some(&job),
                &token,
                shutdown,
            )
            .await;
        if let Err(error) = &result {
            self.record_failure(job.user_id, &job.generation, *error)
                .await?;
        }
        self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            if matches!(&result, Ok(ActionResult::Cancelled)) {
                // No external request began. The previous action was already outside
                // cooldown when claimed, so cancellation need not delay restart delivery.
                tx.execute("UPDATE yandex_connections SET last_action_at = NULL WHERE user_id = ?1 AND generation = ?2", params![job.user_id, job.generation])?;
            }
            let state = match &result {
                Ok(ActionResult::Delivered) => "delivered",
                Ok(ActionResult::Skipped) => "skipped",
                Ok(ActionResult::Cancelled) => "pending",
                Err(_) => "failed",
            };
            tx.execute("UPDATE yandex_jobs SET state = ?1, last_error = ?2 WHERE job_id = ?3 AND state = 'claimed'", params![state, result.err().map(Error::code), job.id])?;
            tx.commit()
        }).await?;
        Ok(())
    }

    pub(super) async fn next_job(&self) -> Result<Option<Job>> {
        let now = Utc::now().timestamp();
        Ok(self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("UPDATE yandex_jobs SET state = 'skipped' WHERE state = 'pending' AND (created_at < ?1 OR NOT EXISTS (SELECT 1 FROM yandex_connections c WHERE c.user_id = yandex_jobs.user_id AND c.generation = yandex_jobs.generation AND c.scenario_id = yandex_jobs.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL))", [now - MAX_JOB_AGE])?;
            // Terminal records are operational metadata, retained for at most one day.
            tx.execute("DELETE FROM yandex_jobs WHERE state IN ('delivered', 'failed', 'skipped') AND created_at < ?1", [now - 86400])?;
            tx.execute("DELETE FROM yandex_oauth_states WHERE expires_at <= ?1", [now])?;
            tx.execute("DELETE FROM yandex_pending_grants WHERE expires_at <= ?1", [now])?;
            let job = tx.query_row("SELECT j.job_id, j.user_id, j.generation, j.scenario_id FROM yandex_jobs j JOIN yandex_connections c ON c.user_id = j.user_id WHERE j.state = 'pending' AND (c.retry_after IS NULL OR c.retry_after <= ?2) AND (c.last_action_at IS NULL OR c.last_action_at <= ?1) ORDER BY j.created_at, j.job_id LIMIT 1", params![now - ACTION_COOLDOWN, now], |row| Ok(Job { id: row.get(0)?, user_id: row.get(1)?, generation: row.get(2)?, scenario_id: row.get(3)? })).optional()?;
            tx.commit()?;
            Ok(job)
        }).await?)
    }

    pub(super) async fn claim_job(&self, job: Job) -> Result<bool> {
        let now = Utc::now().timestamp();
        Ok(self.inner.db.call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute("UPDATE yandex_jobs SET state = 'claimed' WHERE job_id = ?1 AND state = 'pending' AND created_at >= ?2 AND EXISTS (SELECT 1 FROM yandex_connections c WHERE c.user_id = yandex_jobs.user_id AND c.generation = yandex_jobs.generation AND c.scenario_id = yandex_jobs.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL AND (c.last_action_at IS NULL OR c.last_action_at <= ?3))", params![job.id, now - MAX_JOB_AGE, now - ACTION_COOLDOWN])? > 0;
            if changed { tx.execute("UPDATE yandex_connections SET last_action_at = ?1 WHERE user_id = ?2 AND generation = ?3", params![now, job.user_id, job.generation])?; }
            tx.commit()?;
            Ok(changed)
        }).await?)
    }

    pub(super) async fn job_valid(&self, job: Job) -> Result<bool> {
        let now = Utc::now().timestamp();
        Ok(self.inner.db.call(move |conn| conn.query_row("SELECT EXISTS (SELECT 1 FROM yandex_jobs j JOIN yandex_connections c ON j.user_id = c.user_id WHERE j.job_id = ?1 AND j.state = 'claimed' AND j.created_at >= ?2 AND j.generation = c.generation AND j.scenario_id = c.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL)", params![job.id, now - MAX_JOB_AGE], |row| row.get(0))).await?)
    }
}
