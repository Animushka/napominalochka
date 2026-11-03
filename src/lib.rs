//! Napominalochka application: reminder use cases, delivery and transport adapters.
//!
//! The executable delegates startup to [`run`]. Implementation modules stay private;
//! dependencies are assembled at the application boundary.

mod adapters;
mod application;
mod bootstrap;
mod domain;
mod infrastructure;

/// Run the configured bot until shutdown or a mandatory worker fails.
///
/// Reads environment configuration, installs process-wide telemetry, creates the
/// async runtime, and supervises Telegram, HTTP and background delivery workers.
/// Call this once from the executable, outside an existing async runtime.
///
/// # Errors
///
/// Returns an error for invalid configuration, startup failures, or an unexpected
/// worker exit. Shutdown drains workers before the runtime is stopped.
pub fn run() -> anyhow::Result<()> {
    bootstrap::run()
}
