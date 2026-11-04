//! Yandex scenarios use cases.
use super::YandexService;
use super::types::{ActionResult, Connection, Error, Job, Result, Scenario, Status};
use crate::domain::UserId;
use tokio_util::sync::CancellationToken;

impl YandexService {
    pub(crate) async fn scenarios(&self, user_id: UserId) -> Result<Vec<Scenario>> {
        let _guard = self.lock(user_id).await;
        let c = self.require_connection(user_id).await?;
        let result = async {
            let token = self.access_token(user_id, &c).await?;
            self.inner.provider.scenarios(&token).await
        }
        .await;
        match &result {
            Err(error) => self.record_failure(user_id, &c.generation, *error).await?,
            Ok(_) => self.clear_temporary_failure(user_id, &c.generation).await?,
        }
        result
    }

    pub(crate) async fn configure(
        &self,
        user_id: UserId,
        scenario_id: &str,
        enabled: bool,
    ) -> Result<Status> {
        let _guard = self.lock(user_id).await;
        let c = self.require_connection(user_id).await?;
        let selected = if !enabled && scenario_id.is_empty() {
            None
        } else if !enabled && c.scenario_id.as_deref() == Some(scenario_id) {
            // Switching off must work even when the provider is unavailable.
            Some(Scenario {
                id: scenario_id.into(),
                name: c.scenario_name.clone().unwrap_or_default(),
                is_active: true,
            })
        } else {
            let token = self.access_token(user_id, &c).await?;
            let scenarios = match self.inner.provider.scenarios(&token).await {
                Ok(scenarios) => scenarios,
                Err(error) => {
                    self.record_failure(user_id, &c.generation, error).await?;
                    return Err(error);
                }
            };
            Some(
                scenarios
                    .into_iter()
                    .find(|s| s.id == scenario_id && s.is_active)
                    .ok_or(Error::InvalidSelection)?,
            )
        };
        let generation = c.generation;
        let changed = self
            .inner
            .repository
            .configure(user_id, generation, selected, enabled)
            .await?;
        if !changed {
            return Err(Error::NotConnected);
        }
        self.status(user_id).await
    }

    pub(crate) async fn disconnect(&self, user_id: UserId) -> Result<()> {
        let _guard = self.lock(user_id).await;
        self.inner.repository.disconnect(user_id).await?;
        Ok(())
    }

    pub(crate) async fn test(&self, user_id: UserId) -> Result<()> {
        let _guard = self.lock(user_id).await;
        let c = self.require_connection(user_id).await?;
        let scenario_id = c.scenario_id.clone().ok_or(Error::ScenarioMissing)?;
        self.reserve_action(user_id, &c.generation).await?;
        let result = self.execute(user_id, &c, &scenario_id, None).await;
        if let Err(error) = result {
            self.record_failure(user_id, &c.generation, error).await?;
        }
        result
    }

    /// Tests use the same preparation/dispatch boundary as queued delivery.
    pub(super) async fn execute(
        &self,
        user_id: UserId,
        c: &Connection,
        scenario_id: &str,
        job: Option<&Job>,
    ) -> Result<()> {
        let shutdown = CancellationToken::new();
        let Some(token) = self
            .prepare_action(user_id, c, scenario_id, &shutdown)
            .await?
        else {
            return Ok(());
        };
        self.dispatch_action(user_id, c, scenario_id, job, &token, &shutdown)
            .await
            .map(|_| ())
    }

    /// Preparation is safe to retry: it may refresh credentials and read scenarios, but
    /// does not execute a device action. No durable action claim exists at this stage.
    pub(super) async fn prepare_action(
        &self,
        user_id: UserId,
        c: &Connection,
        scenario_id: &str,
        shutdown: &CancellationToken,
    ) -> Result<Option<String>> {
        if shutdown.is_cancelled() {
            return Ok(None);
        }
        // Do not interrupt a token refresh halfway through: a provider may rotate the grant.
        let token = self.access_token(user_id, c).await?;
        if shutdown.is_cancelled() {
            return Ok(None);
        }
        let scenarios = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(None),
            result = self.inner.provider.scenarios(&token) => result?,
        };
        if !scenarios.iter().any(|s| s.id == scenario_id && s.is_active) {
            return Err(Error::ScenarioMissing);
        }
        Ok(Some(token))
    }

    /// Check cancellation/ownership immediately before the external side effect. Once
    /// POST begins, allow its bounded HTTP deadline to finish; never cancel and replay it.
    pub(super) async fn dispatch_action(
        &self,
        user_id: UserId,
        c: &Connection,
        scenario_id: &str,
        job: Option<&Job>,
        token: &str,
        shutdown: &CancellationToken,
    ) -> Result<ActionResult> {
        if let Some(job) = job {
            if !self.job_valid(job.clone()).await? {
                return Ok(ActionResult::Skipped);
            }
        } else {
            let current = self.require_connection(user_id).await?;
            if current.generation != c.generation
                || current.scenario_id.as_deref() != Some(scenario_id)
            {
                return Err(Error::NotConnected);
            }
        }
        if shutdown.is_cancelled() {
            return Ok(ActionResult::Cancelled);
        }
        self.inner.provider.action(token, scenario_id).await?;
        self.clear_temporary_failure(user_id, &c.generation).await?;
        Ok(ActionResult::Delivered)
    }

    pub(super) async fn require_connection(&self, user_id: UserId) -> Result<Connection> {
        let c = self.connection(user_id).await?.ok_or(Error::NotConnected)?;
        if c.requires_reconnect {
            return Err(Error::ReconnectRequired);
        }
        if c.encrypted_tokens.is_none() {
            return Err(Error::NotConnected);
        }
        Ok(c)
    }

    pub(super) async fn connection(&self, user_id: UserId) -> Result<Option<Connection>> {
        Ok(self.inner.repository.connection(user_id).await?)
    }
}
