-- NULL inherits the account preference; explicit zero disables only the check.
ALTER TABLE reminders ADD COLUMN nag_interval_minutes INTEGER CHECK (nag_interval_minutes IS NULL OR nag_interval_minutes IN (1,5,10,20,30,60));
ALTER TABLE reminders ADD COLUMN confirmation_check_minutes INTEGER CHECK (confirmation_check_minutes IS NULL OR confirmation_check_minutes IN (0,20));
ALTER TABLE reminders ADD COLUMN voice_enabled INTEGER NOT NULL DEFAULT 1 CHECK (voice_enabled IN (0,1));

-- This watermark deliberately survives deletion of pending transport work. An old
-- completion cannot become restorable after a newer fire's chat was unreachable.
ALTER TABLE reminders ADD COLUMN last_fired_nagging_id INTEGER;

-- A pending one-off has no parent reminder. Preserve its settings with the event.
ALTER TABLE naggings ADD COLUMN nag_interval_minutes INTEGER CHECK (nag_interval_minutes IS NULL OR nag_interval_minutes IN (1,5,10,20,30,60));
ALTER TABLE naggings ADD COLUMN confirmation_check_minutes INTEGER CHECK (confirmation_check_minutes IS NULL OR confirmation_check_minutes IN (0,20));
ALTER TABLE naggings ADD COLUMN voice_enabled INTEGER NOT NULL DEFAULT 1 CHECK (voice_enabled IN (0,1));

-- A consumed one-off restores its saved settings. A surviving recurring parent
-- remains authoritative, including NULL meaning inherit the current account value.
ALTER TABLE completion_history ADD COLUMN nag_interval_minutes INTEGER CHECK (nag_interval_minutes IS NULL OR nag_interval_minutes IN (1,5,10,20,30,60));
ALTER TABLE completion_history ADD COLUMN confirmation_check_minutes INTEGER CHECK (confirmation_check_minutes IS NULL OR confirmation_check_minutes IN (0,20));
ALTER TABLE completion_history ADD COLUMN voice_enabled INTEGER NOT NULL DEFAULT 1 CHECK (voice_enabled IN (0,1));
