//! Ordered, transactional SQLite schema migrations.
use rusqlite::Connection;

/// Migrations are applied in order; `PRAGMA user_version` records how many have run.
/// The first one is the schema of the original .NET bot, so existing databases upgrade in place.
pub(super) const MIGRATIONS: &[&str] = &[
    include_str!("../../../migrations/001_initial.sql"),
    include_str!("../../../migrations/002_offsets_and_scheduling.sql"),
    include_str!("../../../migrations/003_timezones_and_drafts.sql"),
    include_str!("../../../migrations/004_calendar_feed.sql"),
    include_str!("../../../migrations/005_preferences_and_confirmations.sql"),
    include_str!("../../../migrations/006_yandex.sql"),
    include_str!("../../../migrations/007_recurrence_anchor.sql"),
    include_str!("../../../migrations/008_delivery_leases.sql"),
    include_str!("../../../migrations/009_yandex_retry.sql"),
    include_str!("../../../migrations/010_completion_history.sql"),
    include_str!("../../../migrations/011_reminder_settings.sql"),
];

pub(super) fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    // Table rebuilds in migrations require foreign key enforcement to be off.
    conn.pragma_update(None, "foreign_keys", false)?;
    let applied: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let applied = usize::try_from(applied).unwrap_or(0);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        let version = i64::try_from(index + 1).expect("migration count fits in i64");
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}
