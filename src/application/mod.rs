//! Use cases and ports. Dependencies point toward the domain, never toward adapters.
pub(crate) mod delivery;
pub(crate) mod health;
pub(crate) mod ports;
mod reminders;
pub(crate) mod scheduling;
pub(crate) mod yandex;

pub(crate) use reminders::{Error, Profile, ReminderInput, ReminderService, Result};

#[cfg(test)]
pub(crate) use reminders::MAX_DESCRIPTION_CHARS;
#[cfg(test)]
mod reminder_tests;
