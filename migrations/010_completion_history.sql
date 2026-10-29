-- A completion is an auditable user mark even after its pending occurrence is removed.
-- reminder_id is a snapshot, deliberately not a foreign key: deleting a recurring
-- reminder must leave its history intact while making restoration unavailable.
CREATE TABLE completion_history (
    completion_id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users (user_id) ON DELETE CASCADE,
    source_nagging_id INTEGER NOT NULL UNIQUE,
    reminder_id INTEGER,
    description TEXT NOT NULL,
    scheduled_at TEXT,
    completed_at TEXT NOT NULL,
    undone_at TEXT,
    restored_nagging_id INTEGER REFERENCES naggings (nagging_id) ON DELETE SET NULL
);
CREATE INDEX completion_history_user ON completion_history (user_id, completion_id DESC);
CREATE INDEX completion_history_reminder ON completion_history (reminder_id, source_nagging_id);

-- Keep unknown original times unknown for existing pending records. New occurrences
-- capture their scheduled instant before repeats or snoozing change next_at.
ALTER TABLE naggings ADD COLUMN scheduled_at TEXT;
ALTER TABLE confirmations ADD COLUMN completion_id INTEGER REFERENCES completion_history (completion_id) ON DELETE CASCADE;
