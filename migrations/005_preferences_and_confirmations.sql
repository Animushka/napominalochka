-- Preferences exist before timezone registration. Telegram language initializes the row only once.
CREATE TABLE preferences (
    user_id INTEGER PRIMARY KEY,
    language TEXT NOT NULL DEFAULT 'ru' CHECK (language IN ('ru', 'en')),
    nag_interval_minutes INTEGER NOT NULL DEFAULT 10 CHECK (nag_interval_minutes IN (1, 5, 10, 20, 30, 60)),
    confirmation_check_minutes INTEGER NOT NULL DEFAULT 0 CHECK (confirmation_check_minutes IN (0, 20))
);

-- Separate one-time follow-ups: acknowledging a reminder never creates another nagging.
CREATE TABLE confirmations (
    confirmation_id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_nagging_id INTEGER NOT NULL UNIQUE,
    reminder_id INTEGER REFERENCES reminders (reminder_id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users (user_id) ON DELETE CASCADE,
    description TEXT NOT NULL,
    due_at TEXT NOT NULL
);
CREATE INDEX confirmations_due_at ON confirmations (due_at);
