//! Application use cases. No Telegram, HTTP or SQLite dependencies belong here.

pub mod ports;
mod reminders;

pub use reminders::{Error, Profile, ReminderInput, ReminderService, Result};

#[cfg(test)]
pub use reminders::MAX_DESCRIPTION_CHARS;
