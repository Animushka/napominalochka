//! Outbound ports used by the Yandex application service.
use super::types::{
    ActionResult, AuthState, Connection, ConnectionFailure, DispatchWindow, Job, NewAuth,
    PendingGrant, Result, Scenario, TokenResponse,
};
use crate::application::ports::StorageResult;
use crate::domain::UserId;
use futures_util::future::BoxFuture;

pub(crate) trait YandexRepository: Send + Sync + 'static {
    fn recover_claims(&self) -> BoxFuture<'_, StorageResult<()>>;
    fn finish_job(
        &self,
        job: Job,
        result: Result<ActionResult>,
    ) -> BoxFuture<'_, StorageResult<()>>;
    fn next_job(&self, window: DispatchWindow) -> BoxFuture<'_, StorageResult<Option<Job>>>;
    fn claim_job(&self, job: Job, window: DispatchWindow) -> BoxFuture<'_, StorageResult<bool>>;
    fn job_valid(&self, job: Job, window: DispatchWindow) -> BoxFuture<'_, StorageResult<bool>>;
    fn save_auth(&self, auth: NewAuth) -> BoxFuture<'_, StorageResult<()>>;
    fn cancel_auth(&self, user_id: UserId, hash: String) -> BoxFuture<'_, StorageResult<()>>;
    fn save_grant(&self, grant: PendingGrant) -> BoxFuture<'_, StorageResult<bool>>;
    fn confirm_auth(
        &self,
        user_id: UserId,
        hash: String,
        now: i64,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn consume_state(
        &self,
        hash: String,
        now: i64,
    ) -> BoxFuture<'_, StorageResult<Option<AuthState>>>;
    fn configure(
        &self,
        user_id: UserId,
        generation: String,
        selected: Option<Scenario>,
        enabled: bool,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn disconnect(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<()>>;
    fn connection(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Option<Connection>>>;
    fn update_tokens(
        &self,
        user_id: UserId,
        generation: String,
        encrypted: Vec<u8>,
        expires_at: i64,
        refresh_after: i64,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn refresh_candidate(&self, now: i64) -> BoxFuture<'_, StorageResult<Option<UserId>>>;
    fn defer_refresh(
        &self,
        user_id: UserId,
        generation: String,
        retry_at: i64,
    ) -> BoxFuture<'_, StorageResult<()>>;
    fn reserve_action(
        &self,
        user_id: UserId,
        generation: String,
        window: DispatchWindow,
    ) -> BoxFuture<'_, StorageResult<bool>>;
    fn record_failure(&self, failure: ConnectionFailure) -> BoxFuture<'_, StorageResult<()>>;
    fn clear_temporary_failure(
        &self,
        user_id: UserId,
        generation: String,
    ) -> BoxFuture<'_, StorageResult<()>>;
}

pub(crate) trait YandexProvider: Send + Sync + 'static {
    fn authorization_url(&self, state: &str, challenge: &str) -> Result<String>;
    fn exchange<'a>(
        &'a self,
        fields: &'a [(&'a str, &'a str)],
    ) -> BoxFuture<'a, Result<TokenResponse>>;
    fn scenarios<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Vec<Scenario>>>;
    fn action<'a>(&'a self, token: &'a str, scenario_id: &'a str) -> BoxFuture<'a, Result<()>>;
}

pub(crate) trait TokenVault: Send + Sync + 'static {
    fn seal(
        &self,
        user_id: UserId,
        generation: &str,
        kind: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>>;
    fn open(
        &self,
        user_id: UserId,
        generation: &str,
        kind: &str,
        envelope: &[u8],
    ) -> Result<Vec<u8>>;
}
