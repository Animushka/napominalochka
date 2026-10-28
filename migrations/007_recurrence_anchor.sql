-- Preserve the originally chosen local calendar date/time separately from the next UTC
-- occurrence. NULL keeps legacy rows readable; their next firing records the best available
-- anchor from the current due date in the user's timezone.
ALTER TABLE reminders ADD COLUMN anchor_local TEXT;
