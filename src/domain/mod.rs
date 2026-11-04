//! Pure domain entities, value objects and scheduling rules.
mod entities;
mod errors;
mod recurrence;
mod timezone;

pub(crate) use entities::{
    Completion, Confirmation, Draft, HistoryPage, Nagging, Preferences, Reminder, ReminderChanges,
    ReminderSchedule, ReminderSettings, ReminderTime, ReminderWrite, Snooze, UndoResult, User,
};
pub(crate) use errors::InputError;
pub(crate) use recurrence::Recurrence;
pub(crate) use timezone::{UserTz, UtcOffset};

#[cfg(test)]
mod tests;

mod ids;
pub(crate) use ids::{
    CompletionId, ConfirmationId, DraftId, NaggingId, ReminderId, UserId, VoiceJobId,
};

mod preferences;
pub(crate) use preferences::{ConfirmationDelay, Language, NagInterval};
