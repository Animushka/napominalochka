//! Input validation failures interpreted by presentation adapters.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum InputError {
    #[error(
        "I don't recognise that timezone. Try a city (London, New York), a timezone (Asia/Singapore) or an offset (+8, -3:30)."
    )]
    InvalidTimezone,
    #[error("Repeat must be one of: once, daily, weekdays, weekly, monthly, yearly.")]
    InvalidRecurrence,
    #[error("Please enter what to remind you about.")]
    EmptyDescription,
    #[error("Reminder text must be 1000 characters or fewer.")]
    DescriptionTooLong,
    #[error("That time has already passed.")]
    InPast,
}
