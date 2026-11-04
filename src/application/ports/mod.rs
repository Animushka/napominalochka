//! Persistence contracts grouped by use-case responsibility.
pub(crate) mod planning;
mod storage;
pub(crate) use storage::{StorageError, StorageResult};
mod health;
pub(crate) use health::StorageHealth;
mod users;
pub(crate) use users::UserRepository;
mod preferences;
pub(crate) use preferences::PreferencesRepository;
mod reminders;
pub(crate) use reminders::ReminderStore;
mod drafts;
pub(crate) use drafts::DraftRepository;
mod pending;
pub(crate) use pending::PendingRepository;
mod history;
pub(crate) use history::CompletionRepository;
mod calendar;
pub(crate) use calendar::CalendarRepository;
pub(crate) use users::Registration;

pub(crate) trait ReminderRepository:
    StorageHealth
    + UserRepository
    + PreferencesRepository
    + ReminderStore
    + DraftRepository
    + PendingRepository
    + CompletionRepository
    + CalendarRepository
{
}
impl<
    T: StorageHealth
        + UserRepository
        + PreferencesRepository
        + ReminderStore
        + DraftRepository
        + PendingRepository
        + CompletionRepository
        + CalendarRepository,
> ReminderRepository for T
{
}
