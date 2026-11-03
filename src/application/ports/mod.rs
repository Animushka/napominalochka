//! Outbound persistence and transport contracts.
pub(crate) mod planning;
mod reminders;
pub(crate) use reminders::{Registration, ReminderRepository};
