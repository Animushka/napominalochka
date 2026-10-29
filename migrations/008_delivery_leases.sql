-- The pending occurrence is also the durable Telegram delivery queue. Claims expire
-- after interrupted requests; snoozing clears its claim so stale completions cannot
-- overwrite the user's chosen time. Retry attempts are independent of nag frequency.
ALTER TABLE naggings ADD COLUMN delivery_claim TEXT;
ALTER TABLE naggings ADD COLUMN delivery_lease_until TEXT;
ALTER TABLE naggings ADD COLUMN delivery_attempts INTEGER NOT NULL DEFAULT 0;

ALTER TABLE confirmations ADD COLUMN delivery_claim TEXT;
ALTER TABLE confirmations ADD COLUMN delivery_lease_until TEXT;
ALTER TABLE confirmations ADD COLUMN delivery_attempts INTEGER NOT NULL DEFAULT 0;
