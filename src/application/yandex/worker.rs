//! Durable scenario queue orchestration. Preparation is repeatable; the atomic
//! dispatch claim marks the boundary after which an action must never be replayed.

use super::YandexService;
use super::types::{DispatchWindow, Job, Result};
use chrono::Utc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

impl YandexService {
    pub(crate) async fn run_worker(&self, shutdown: CancellationToken) {
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
        self.inner.repository.recover_claims().await?;
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
        self.inner.repository.finish_job(job, result).await?;
        Ok(())
    }

    pub(super) async fn next_job(&self) -> Result<Option<Job>> {
        let now = Utc::now().timestamp();
        Ok(self
            .inner
            .repository
            .next_job(DispatchWindow::at(now))
            .await?)
    }

    pub(super) async fn claim_job(&self, job: Job) -> Result<bool> {
        let now = Utc::now().timestamp();
        Ok(self
            .inner
            .repository
            .claim_job(job, DispatchWindow::at(now))
            .await?)
    }

    pub(super) async fn job_valid(&self, job: Job) -> Result<bool> {
        let now = Utc::now().timestamp();
        Ok(self
            .inner
            .repository
            .job_valid(job, DispatchWindow::at(now))
            .await?)
    }
}
