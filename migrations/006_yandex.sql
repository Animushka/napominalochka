-- Integration settings can exist before Telegram timezone registration.
CREATE TABLE yandex_connections (
    user_id INTEGER PRIMARY KEY,
    generation TEXT NOT NULL,
    encrypted_tokens BLOB,
    expires_at INTEGER,
    refresh_after INTEGER,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    requires_reconnect INTEGER NOT NULL DEFAULT 0 CHECK (requires_reconnect IN (0, 1)),
    scenario_id TEXT,
    scenario_name TEXT,
    last_error TEXT,
    last_action_at INTEGER,
    auth_nonce TEXT
);
CREATE INDEX yandex_connections_refresh ON yandex_connections (refresh_after)
    WHERE encrypted_tokens IS NOT NULL AND requires_reconnect = 0;

CREATE TABLE yandex_oauth_states (
    state_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL UNIQUE REFERENCES yandex_connections (user_id) ON DELETE CASCADE,
    encrypted_verifier BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    language TEXT NOT NULL CHECK (language IN ('ru', 'en'))
);
CREATE INDEX yandex_oauth_expiry ON yandex_oauth_states (expires_at);

-- A callback alone never connects an account. Only the callback browser receives the
-- short-lived receipt; the originating Telegram user must redeem it in Telegram.
CREATE TABLE yandex_pending_grants (
    completion_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL UNIQUE REFERENCES yandex_connections (user_id) ON DELETE CASCADE,
    generation TEXT NOT NULL,
    encrypted_tokens BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    token_expires_at INTEGER NOT NULL,
    refresh_after INTEGER NOT NULL,
    auth_nonce TEXT NOT NULL
);
CREATE INDEX yandex_pending_expiry ON yandex_pending_grants (expires_at);

-- A job belongs to this occurrence, account generation and scenario, never a future selection.
-- Deleting/acknowledging an occurrence also cancels its delivery. One-off reminders themselves
-- are consumed before delivery, so the foreign key must reference the nagging.
CREATE TABLE yandex_jobs (
    job_id INTEGER PRIMARY KEY AUTOINCREMENT,
    nagging_id INTEGER NOT NULL UNIQUE REFERENCES naggings (nagging_id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES yandex_connections (user_id) ON DELETE CASCADE,
    generation TEXT NOT NULL,
    scenario_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'claimed', 'delivered', 'failed', 'skipped')),
    last_error TEXT
);
CREATE INDEX yandex_jobs_pending ON yandex_jobs (state, created_at, job_id);
