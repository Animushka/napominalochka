-- Reading scenarios or refreshing credentials can fail before any scenario action is sent.
-- Preserve the user's enabled setting and defer queued preparation across restarts.
ALTER TABLE yandex_connections ADD COLUMN retry_after INTEGER;
