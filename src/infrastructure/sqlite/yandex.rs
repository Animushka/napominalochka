//! Atomic Yandex persistence operations. No SQL escapes this adapter.
use super::Db;
use crate::application::ports::StorageResult;
use crate::application::yandex::ports::YandexRepository;
use crate::application::yandex::types::{
    ActionResult, AuthState, Connection, ConnectionFailure, DispatchWindow, Error,
    FailureDisposition, Job, NewAuth, PendingGrant, Result, Scenario,
};
use crate::domain::UserId;
use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension, Row, params};

const CONNECTION_COLUMNS: &str = "generation, encrypted_tokens, expires_at, enabled, requires_reconnect, scenario_id, scenario_name, last_error, refresh_after";

impl YandexRepository for Db {
    fn recover_claims(&self) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(|conn| {
                let tx = conn.transaction()?;
                tx.execute("UPDATE yandex_connections SET enabled = 0, last_error = 'yandex_delivery_uncertain' WHERE EXISTS (SELECT 1 FROM yandex_jobs j WHERE j.user_id = yandex_connections.user_id AND j.generation = yandex_connections.generation AND j.state = 'claimed')", [])?;
                tx.execute("UPDATE yandex_jobs SET state = 'failed', last_error = 'yandex_delivery_uncertain' WHERE state = 'claimed'", [])?;
                tx.commit()
            }).await
        })
    }
    fn finish_job(
        &self,
        job: Job,
        result: Result<ActionResult>,
    ) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(move |conn| {
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
            }).await
        })
    }
    fn next_job(&self, window: DispatchWindow) -> BoxFuture<'_, StorageResult<Option<Job>>> {
        Box::pin(async move {
            self.call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute("UPDATE yandex_jobs SET state = 'skipped' WHERE state = 'pending' AND (created_at < ?1 OR NOT EXISTS (SELECT 1 FROM yandex_connections c WHERE c.user_id = yandex_jobs.user_id AND c.generation = yandex_jobs.generation AND c.scenario_id = yandex_jobs.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL))", [window.oldest_created_at])?;
                // Terminal records are operational metadata, retained for at most one day.
                tx.execute("DELETE FROM yandex_jobs WHERE state IN ('delivered', 'failed', 'skipped') AND created_at < ?1", [window.terminal_before])?;
                tx.execute("DELETE FROM yandex_oauth_states WHERE expires_at <= ?1", [window.now])?;
                tx.execute("DELETE FROM yandex_pending_grants WHERE expires_at <= ?1", [window.now])?;
                let job = tx.query_row("SELECT j.job_id, j.user_id, j.generation, j.scenario_id FROM yandex_jobs j JOIN yandex_connections c ON c.user_id = j.user_id WHERE j.state = 'pending' AND (c.retry_after IS NULL OR c.retry_after <= ?2) AND (c.last_action_at IS NULL OR c.last_action_at <= ?1) ORDER BY j.created_at, j.job_id LIMIT 1", params![window.last_action_before, window.now], |row| Ok(Job { id: row.get(0)?, user_id: row.get(1)?, generation: row.get(2)?, scenario_id: row.get(3)? })).optional()?;
                tx.commit()?;
                Ok(job)
            }).await
        })
    }
    fn claim_job(&self, job: Job, window: DispatchWindow) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            self.call(move |conn| {
                let tx = conn.transaction()?;
                let changed = tx.execute("UPDATE yandex_jobs SET state = 'claimed' WHERE job_id = ?1 AND state = 'pending' AND created_at >= ?2 AND EXISTS (SELECT 1 FROM yandex_connections c WHERE c.user_id = yandex_jobs.user_id AND c.generation = yandex_jobs.generation AND c.scenario_id = yandex_jobs.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL AND (c.last_action_at IS NULL OR c.last_action_at <= ?3))", params![job.id, window.oldest_created_at, window.last_action_before])? > 0;
                if changed { tx.execute("UPDATE yandex_connections SET last_action_at = ?1 WHERE user_id = ?2 AND generation = ?3", params![window.now, job.user_id, job.generation])?; }
                tx.commit()?;
                Ok(changed)
            }).await
        })
    }
    fn job_valid(&self, job: Job, window: DispatchWindow) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            self.call(move |conn| conn.query_row("SELECT EXISTS (SELECT 1 FROM yandex_jobs j JOIN yandex_connections c ON j.user_id = c.user_id WHERE j.job_id = ?1 AND j.state = 'claimed' AND j.created_at >= ?2 AND j.generation = c.generation AND j.scenario_id = c.scenario_id AND c.enabled = 1 AND c.requires_reconnect = 0 AND c.encrypted_tokens IS NOT NULL)", params![job.id, window.oldest_created_at], |row| row.get(0))).await
        })
    }
    fn save_auth(&self, auth: NewAuth) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            let NewAuth {
                user_id,
                generation,
                hash,
                encrypted_verifier,
                now,
                expires_at,
                language,
            } = auth;
            self.call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute("DELETE FROM yandex_oauth_states WHERE expires_at <= ?1", [now])?;
                tx.execute("INSERT INTO yandex_connections (user_id, generation, auth_nonce) VALUES (?1, ?2, ?3) ON CONFLICT(user_id) DO UPDATE SET auth_nonce = excluded.auth_nonce", params![user_id, generation, hash])?;
                tx.execute("DELETE FROM yandex_oauth_states WHERE user_id = ?1", [user_id])?;
                tx.execute("DELETE FROM yandex_pending_grants WHERE user_id = ?1 OR expires_at <= ?2", params![user_id, now])?;
                tx.execute("INSERT INTO yandex_oauth_states (state_hash, user_id, encrypted_verifier, expires_at, language) VALUES (?1, ?2, ?3, ?4, ?5)", params![hash, user_id, encrypted_verifier, expires_at, language])?;
                tx.commit()
            }).await
        })
    }
    fn cancel_auth(&self, user_id: UserId, hash: String) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(move |conn| {
                conn.execute("UPDATE yandex_connections SET auth_nonce = NULL WHERE user_id = ?1 AND auth_nonce = ?2", params![user_id, hash])?;
                Ok(())
            }).await
        })
    }
    fn save_grant(&self, grant: PendingGrant) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            let PendingGrant {
                completion_hash,
                generation,
                encrypted,
                pending_expires,
                expires_at,
                refresh_after,
                user_id,
                state_hash,
            } = grant;
            self.call(move |conn| {
                let tx = conn.transaction()?;
                let n = tx.execute("INSERT INTO yandex_pending_grants (completion_hash, user_id, generation, encrypted_tokens, expires_at, token_expires_at, refresh_after, auth_nonce) SELECT ?1, user_id, ?2, ?3, ?4, ?5, ?6, auth_nonce FROM yandex_connections WHERE user_id = ?7 AND auth_nonce = ?8", params![completion_hash, generation, encrypted, pending_expires, expires_at, refresh_after, user_id, state_hash])?;
                tx.commit()?;
                Ok(n > 0)
            }).await
        })
    }
    fn confirm_auth(
        &self,
        user_id: UserId,
        hash: String,
        now: i64,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            self.call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute("DELETE FROM yandex_pending_grants WHERE expires_at <= ?1 OR token_expires_at <= ?1", [now])?;
                let grant: Option<(String, Vec<u8>, i64, String, i64)> = tx.query_row("DELETE FROM yandex_pending_grants WHERE completion_hash = ?1 AND user_id = ?2 RETURNING generation, encrypted_tokens, token_expires_at, auth_nonce, refresh_after", params![hash, user_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
                let mut confirmed = false;
                if let Some((generation, encrypted, expires_at, nonce, refresh_after)) = grant {
                    confirmed = tx.execute("UPDATE yandex_connections SET generation = ?1, encrypted_tokens = ?2, expires_at = ?3, refresh_after = ?4, enabled = 0, requires_reconnect = 0, scenario_id = NULL, scenario_name = NULL, last_error = NULL, retry_after = NULL, auth_nonce = NULL WHERE user_id = ?5 AND auth_nonce = ?6", params![generation, encrypted, expires_at, refresh_after, user_id, nonce])? > 0;
                    if confirmed { tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1", [user_id])?; }
                }
                tx.commit()?;
                Ok(confirmed)
            }).await
        })
    }
    fn consume_state(
        &self,
        hash: String,
        now: i64,
    ) -> BoxFuture<'_, StorageResult<Option<AuthState>>> {
        Box::pin(async move {
            self.call(move |conn| {
                let tx = conn.transaction()?;
                // DELETE RETURNING consumes even an expired state, preventing replay.
                let row: Option<(UserId, Vec<u8>, i64, crate::domain::Language)> = tx.query_row("DELETE FROM yandex_oauth_states WHERE state_hash = ?1 RETURNING user_id, encrypted_verifier, expires_at, language", [&hash], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
                tx.commit()?;
                Ok(row.filter(|row| row.2 > now).map(|(user_id, encrypted_verifier, _, language)| AuthState { user_id, state_hash: hash, encrypted_verifier, language }))
            }).await
        })
    }
    fn configure(
        &self,
        user_id: UserId,
        generation: String,
        selected: Option<Scenario>,
        enabled: bool,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            self.call(move |conn| {
                let tx = conn.transaction()?;
                let n = tx.execute("UPDATE yandex_connections SET scenario_id = ?1, scenario_name = ?2, enabled = ?3, last_error = NULL, retry_after = NULL WHERE user_id = ?4 AND generation = ?5 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![selected.as_ref().map(|s| &s.id), selected.as_ref().map(|s| &s.name), enabled, user_id, generation])?;
                // A new setting applies to future occurrences, never an old queued reminder.
                tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1 AND state = 'pending'", [user_id])?;
                tx.commit()?;
                Ok(n > 0)
            }).await
        })
    }
    fn disconnect(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(move |conn| {
                conn.execute(
                    "DELETE FROM yandex_connections WHERE user_id = ?1",
                    [user_id],
                )?;
                Ok(())
            })
            .await
        })
    }
    fn connection(&self, user_id: UserId) -> BoxFuture<'_, StorageResult<Option<Connection>>> {
        Box::pin(async move {
            self.call(move |conn| {
                conn.query_row(
                    &format!(
                        "SELECT {CONNECTION_COLUMNS} FROM yandex_connections WHERE user_id = ?1"
                    ),
                    [user_id],
                    connection_from_row,
                )
                .optional()
            })
            .await
        })
    }
    fn update_tokens(
        &self,
        user_id: UserId,
        generation: String,
        encrypted: Vec<u8>,
        expires_at: i64,
        refresh_after: i64,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            self.call(move |conn| Ok(conn.execute("UPDATE yandex_connections SET encrypted_tokens = ?1, expires_at = ?2, refresh_after = ?3 WHERE user_id = ?4 AND generation = ?5 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![encrypted, expires_at, refresh_after, user_id, generation])? > 0)).await
        })
    }
    fn refresh_candidate(&self, now: i64) -> BoxFuture<'_, StorageResult<Option<UserId>>> {
        Box::pin(async move {
            self.call(move |conn| {
                conn.query_row("SELECT user_id FROM yandex_connections WHERE encrypted_tokens IS NOT NULL AND requires_reconnect = 0 AND (expires_at IS NULL OR expires_at <= ?1 OR refresh_after IS NULL OR refresh_after <= ?1) ORDER BY COALESCE(refresh_after, 0), user_id LIMIT 1", [now], |row| row.get::<_, UserId>(0)).optional()
            }).await
        })
    }
    fn defer_refresh(
        &self,
        user_id: UserId,
        generation: String,
        retry_at: i64,
    ) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(move |conn| {
                        conn.execute("UPDATE yandex_connections SET refresh_after = ?1 WHERE user_id = ?2 AND generation = ?3 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0", params![retry_at, user_id, generation])?;
                        Ok(())
                    }).await
        })
    }
    fn reserve_action(
        &self,
        user_id: UserId,
        generation: String,
        window: DispatchWindow,
    ) -> BoxFuture<'_, StorageResult<bool>> {
        Box::pin(async move {
            self.call(move |conn| Ok(conn.execute("UPDATE yandex_connections SET last_action_at = ?1 WHERE user_id = ?2 AND generation = ?3 AND encrypted_tokens IS NOT NULL AND requires_reconnect = 0 AND (last_action_at IS NULL OR last_action_at <= ?4)", params![window.now, user_id, generation, window.last_action_before])? > 0)).await
        })
    }
    fn record_failure(&self, failure: ConnectionFailure) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            let ConnectionFailure {
                user_id,
                generation,
                error,
                disposition,
            } = failure;
            self.call(move |conn| {
                let tx = conn.transaction()?;
                match disposition {
                    FailureDisposition::RetryAt(retry_after) => {
                        tx.execute("UPDATE yandex_connections SET last_error = ?1, retry_after = ?2 WHERE user_id = ?3 AND generation = ?4", params![error.code(), retry_after, user_id, generation])?;
                    }
                    FailureDisposition::Disable | FailureDisposition::Reconnect => {
                        let reconnect = matches!(disposition, FailureDisposition::Reconnect);
                        tx.execute("UPDATE yandex_connections SET enabled = 0, last_error = ?1, requires_reconnect = ?2, encrypted_tokens = CASE WHEN ?2 THEN NULL ELSE encrypted_tokens END, expires_at = CASE WHEN ?2 THEN NULL ELSE expires_at END, refresh_after = CASE WHEN ?2 THEN NULL ELSE refresh_after END, retry_after = NULL WHERE user_id = ?3 AND generation = ?4", params![error.code(), reconnect, user_id, generation])?;
                        tx.execute("DELETE FROM yandex_jobs WHERE user_id = ?1 AND generation = ?2 AND state = 'pending'", params![user_id, generation])?;
                    }
                }
                tx.commit()
            }).await
        })
    }
    fn clear_temporary_failure(
        &self,
        user_id: UserId,
        generation: String,
    ) -> BoxFuture<'_, StorageResult<()>> {
        Box::pin(async move {
            self.call(move |conn| {
                conn.execute("UPDATE yandex_connections SET last_error = NULL, retry_after = NULL WHERE user_id = ?1 AND generation = ?2 AND last_error IN ('yandex_unavailable', 'yandex_rate_limited', 'yandex_internal')", params![user_id, generation])?;
                Ok(())
            }).await
        })
    }
}

fn connection_from_row(row: &Row<'_>) -> rusqlite::Result<Connection> {
    Ok(Connection {
        generation: row.get(0)?,
        encrypted_tokens: row.get(1)?,
        expires_at: row.get(2)?,
        enabled: row.get(3)?,
        requires_reconnect: row.get(4)?,
        scenario_id: row.get(5)?,
        scenario_name: row.get(6)?,
        last_error: row.get(7)?,
        refresh_after: row.get(8)?,
    })
}
