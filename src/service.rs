//! Application layer: every rule about reminders lives here, shared by the chat flow, the
//! Mini App API and the commands. Callers handle presentation only.

use chrono::{DateTime, Duration, NaiveDateTime, NaiveTime, Utc};
use thiserror::Error;

use crate::db::{Db, Registration};
use crate::model::{Draft, InputError, Preferences, Recurrence, Reminder, Snooze, User, UserTz};

pub const MAX_DESCRIPTION_CHARS: usize = 1000;
/// "Snooze until tomorrow" means this local time.
const SNOOZE_MORNING: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).expect("valid time");

#[derive(Debug, Error)]
pub enum Error {
    #[error("Please set your timezone first.")]
    NotRegistered,
    /// The reminder or draft does not exist or belongs to someone else.
    #[error("This reminder is no longer available.")]
    NotFound,
    #[error("Invalid preference: {0}")]
    InvalidPreferences(&'static str),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Who a request is from, as reported by Telegram.
pub struct Profile {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

/// A requested reminder: `at` is local time in the user's timezone.
pub struct ReminderInput<'a> {
    pub description: &'a str,
    pub at: NaiveDateTime,
    pub recurrence: Recurrence,
}

#[derive(Clone)]
pub struct Service {
    db: Db,
}

impl Service {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn preferences(&self, user_id: i64) -> Result<Preferences> {
        Ok(self.db.preferences(user_id).await?)
    }

    /// A manually chosen language is never overwritten by Telegram's language code.
    pub async fn initialize_language(
        &self,
        user_id: i64,
        telegram_language: Option<&str>,
    ) -> Result<()> {
        let language = if telegram_language.is_some_and(|code| {
            code.eq_ignore_ascii_case("en") || code.to_ascii_lowercase().starts_with("en-")
        }) {
            "en"
        } else {
            "ru"
        };
        self.db
            .initialize_language(user_id, language.into())
            .await?;
        Ok(())
    }

    pub async fn set_preferences(
        &self,
        user_id: i64,
        language: Option<&str>,
        nag_interval_minutes: Option<i64>,
        confirmation_check_minutes: Option<i64>,
    ) -> Result<Preferences> {
        if language.is_some_and(|value| !matches!(value, "ru" | "en")) {
            return Err(Error::InvalidPreferences("language"));
        }
        if nag_interval_minutes.is_some_and(|value| ![1, 5, 10, 20, 30, 60].contains(&value)) {
            return Err(Error::InvalidPreferences("nag_interval_minutes"));
        }
        if confirmation_check_minutes.is_some_and(|value| ![0, 20].contains(&value)) {
            return Err(Error::InvalidPreferences("confirmation_check_minutes"));
        }
        Ok(self
            .db
            .set_preferences(
                user_id,
                language.map(str::to_owned),
                nag_interval_minutes,
                confirmation_check_minutes,
            )
            .await?)
    }

    // ---- users ---------------------------------------------------------------------------

    pub async fn user(&self, user_id: i64) -> Result<Option<User>> {
        Ok(self.db.user(user_id).await?)
    }

    pub async fn registered(&self, user_id: i64) -> Result<User> {
        self.user(user_id).await?.ok_or(Error::NotRegistered)
    }

    /// Registers the user or changes their timezone.
    pub async fn set_timezone(&self, profile: Profile, tz: UserTz) -> Result<(User, Registration)> {
        let user = User {
            id: profile.id,
            first_name: profile.first_name,
            last_name: profile.last_name,
            tz,
        };
        let registration = self.db.upsert_user(user.clone()).await?;
        Ok((user, registration))
    }

    /// Deletes the user and all their data. Returns whether they were registered.
    pub async fn unregister(&self, user_id: i64) -> Result<bool> {
        Ok(self.db.delete_user(user_id).await?)
    }

    // ---- reminders -----------------------------------------------------------------------

    pub async fn list(&self, user_id: i64) -> Result<Vec<Reminder>> {
        Ok(self.db.reminders_for_user(user_id).await?)
    }

    pub async fn reminder(&self, user_id: i64, reminder_id: i64) -> Result<Reminder> {
        self.db
            .reminder(user_id, reminder_id)
            .await?
            .ok_or(Error::NotFound)
    }

    pub async fn create(
        &self,
        user: &User,
        input: ReminderInput<'_>,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let description = description(input.description)?;
        let due_at = first_due(input.at, input.recurrence, user.tz, now)?;
        Ok(self
            .db
            .add_reminder(user.id, description, due_at, input.recurrence)
            .await?)
    }

    /// Changes a reminder's time and repeat, and its description unless `description` is
    /// `None`.
    pub async fn update(
        &self,
        user: &User,
        reminder_id: i64,
        description: Option<&str>,
        at: NaiveDateTime,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let description = description.map(self::description).transpose()?;
        let due_at = first_due(at, recurrence, user.tz, now)?;
        self.db
            .update_reminder(user.id, reminder_id, description, due_at, recurrence)
            .await?
            .ok_or(Error::NotFound)
    }

    /// Deletes a reminder and stops its nagging; returns what was deleted.
    pub async fn delete(&self, user_id: i64, reminder_id: i64) -> Result<Reminder> {
        let reminder = self.reminder(user_id, reminder_id).await?;
        self.db.delete_reminder(user_id, reminder_id).await?;
        Ok(reminder)
    }

    pub async fn delete_all(&self, user_id: i64) -> Result<usize> {
        Ok(self.db.delete_all_reminders(user_id).await?)
    }

    // ---- drafts (text waiting for a time) ------------------------------------------------

    pub async fn start_draft(&self, user_id: i64, text: &str, now: DateTime<Utc>) -> Result<Draft> {
        Ok(self
            .db
            .create_draft(user_id, description(text)?, now)
            .await?)
    }

    pub async fn draft(&self, user_id: i64, draft_id: i64) -> Result<Draft> {
        self.db
            .draft(user_id, draft_id)
            .await?
            .ok_or(Error::NotFound)
    }

    /// The user's draft if it was started within `window`.
    pub async fn pending_draft(
        &self,
        user_id: i64,
        window: Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<Draft>> {
        Ok(self.db.pending_draft(user_id, now - window).await?)
    }

    pub async fn cancel_draft(&self, user_id: i64, draft_id: i64) -> Result<()> {
        self.db.delete_draft(user_id, draft_id).await?;
        Ok(())
    }

    /// Turns a draft into a reminder. `NotFound` if it was already saved or cancelled.
    pub async fn save_draft(
        &self,
        user: &User,
        draft_id: i64,
        at: NaiveDateTime,
        recurrence: Recurrence,
        now: DateTime<Utc>,
    ) -> Result<Reminder> {
        let due_at = first_due(at, recurrence, user.tz, now)?;
        self.db
            .save_draft(user.id, draft_id, due_at, recurrence)
            .await?
            .ok_or(Error::NotFound)
    }

    // ---- naggings ------------------------------------------------------------------------

    /// Records completion and optionally schedules one check. Returns `false` if already done.
    #[cfg(test)]
    pub async fn done(&self, user_id: i64, nagging_id: i64) -> Result<bool> {
        self.done_at(user_id, nagging_id, Utc::now()).await
    }

    pub async fn done_at(&self, user_id: i64, nagging_id: i64, now: DateTime<Utc>) -> Result<bool> {
        Ok(self.db.complete_nagging(user_id, nagging_id, now).await?)
    }

    /// Stops naggings by reminder id, for buttons sent by the .NET bot.
    pub async fn done_legacy(&self, user_id: i64, reminder_id: i64) -> Result<bool> {
        Ok(self.db.acknowledge_reminder(user_id, reminder_id).await?)
    }

    /// Postpones a nagging; returns when it will nag again, or `None` if it was already done.
    pub async fn snooze(
        &self,
        user: &User,
        nagging_id: i64,
        snooze: Snooze,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        let until = match snooze {
            Snooze::TenMinutes => now + Duration::minutes(10),
            Snooze::OneHour => now + Duration::hours(1),
            Snooze::TomorrowMorning => {
                let tomorrow = user.tz.local(now).date() + Duration::days(1);
                user.tz.utc(tomorrow.and_time(SNOOZE_MORNING))
            }
        };
        let snoozed = self.db.snooze_nagging(user.id, nagging_id, until).await?;
        Ok(snoozed.then_some(until))
    }

    // ---- calendar feed -------------------------------------------------------------------

    /// The user's feed token; `reset` replaces it, revoking the old feed URL.
    pub async fn calendar_token(&self, user_id: i64, reset: bool) -> Result<String> {
        self.db
            .calendar_token(user_id, reset)
            .await?
            .ok_or(Error::NotRegistered)
    }

    /// The feed owner and their reminders, if the token is valid.
    pub async fn calendar_feed(&self, token: &str) -> Result<Option<(User, Vec<Reminder>)>> {
        let Some(user) = self.db.user_by_calendar_token(token.to_owned()).await? else {
            return Ok(None);
        };
        let reminders = self.db.reminders_for_user(user.id).await?;
        Ok(Some((user, reminders)))
    }
}

fn description(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(InputError::EmptyDescription.into());
    }
    Ok(text.chars().take(MAX_DESCRIPTION_CHARS).collect())
}

/// A time that has passed moves to the next occurrence; for one-off reminders it is an error.
fn first_due(
    at: NaiveDateTime,
    recurrence: Recurrence,
    tz: UserTz,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    recurrence
        .first_due(at, tz, now)
        .ok_or_else(|| InputError::InPast.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono_tz::Tz;

    const SGT: UserTz = UserTz::Named(Tz::Asia__Singapore);

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn now() -> DateTime<Utc> {
        SGT.utc(local(2026, 10, 13, 10, 30))
    }

    async fn setup() -> (Service, User) {
        let service = Service::new(Db::open_in_memory().unwrap());
        let profile = Profile {
            id: 1,
            first_name: "Ada".into(),
            last_name: String::new(),
        };
        let (user, registration) = service.set_timezone(profile, SGT).await.unwrap();
        assert_eq!(registration, Registration::Created);
        (service, user)
    }

    fn input(description: &str, at: NaiveDateTime, recurrence: Recurrence) -> ReminderInput<'_> {
        ReminderInput {
            description,
            at,
            recurrence,
        }
    }

    #[tokio::test]
    async fn validates_new_reminders() {
        let (service, user) = setup().await;
        let tomorrow = local(2026, 10, 14, 9, 0);

        let err = service
            .create(&user, input("   ", tomorrow, Recurrence::Once), now())
            .await;
        assert!(matches!(
            err,
            Err(Error::Input(InputError::EmptyDescription))
        ));

        let past = local(2026, 10, 13, 8, 0);
        let err = service
            .create(&user, input("x", past, Recurrence::Once), now())
            .await;
        assert!(matches!(err, Err(Error::Input(InputError::InPast))));

        let daily = service
            .create(&user, input("x", past, Recurrence::Daily), now())
            .await
            .unwrap();
        assert_eq!(
            SGT.local(daily.due_at),
            local(2026, 10, 14, 8, 0),
            "rolled to next occurrence"
        );

        let long = "a".repeat(MAX_DESCRIPTION_CHARS + 50);
        let clamped = service
            .create(&user, input(&long, tomorrow, Recurrence::Once), now())
            .await
            .unwrap();
        assert_eq!(clamped.description.chars().count(), MAX_DESCRIPTION_CHARS);
    }

    #[tokio::test]
    async fn only_owners_can_change_reminders() {
        let (service, user) = setup().await;
        let other = Profile {
            id: 2,
            first_name: "Eve".into(),
            last_name: String::new(),
        };
        let (eve, _) = service.set_timezone(other, SGT).await.unwrap();
        let at = local(2026, 10, 14, 9, 0);
        let reminder = service
            .create(&user, input("mine", at, Recurrence::Once), now())
            .await
            .unwrap();

        assert!(matches!(
            service.reminder(eve.id, reminder.id).await,
            Err(Error::NotFound)
        ));
        assert!(matches!(
            service
                .update(
                    &eve,
                    reminder.id,
                    Some("hacked"),
                    at,
                    Recurrence::Once,
                    now()
                )
                .await,
            Err(Error::NotFound)
        ));
        assert!(matches!(
            service.delete(eve.id, reminder.id).await,
            Err(Error::NotFound)
        ));

        assert!(matches!(
            service
                .update(&user, reminder.id, Some(" "), at, Recurrence::Weekly, now())
                .await,
            Err(Error::Input(InputError::EmptyDescription))
        ));
        let updated = service
            .update(&user, reminder.id, None, at, Recurrence::Weekly, now())
            .await
            .unwrap();
        assert_eq!(
            (updated.description.as_str(), updated.recurrence),
            ("mine", Recurrence::Weekly)
        );
        assert_eq!(
            service
                .delete(user.id, reminder.id)
                .await
                .unwrap()
                .description,
            "mine"
        );
    }

    #[tokio::test]
    async fn drafts_are_saved_once() {
        let (service, user) = setup().await;
        let draft = service
            .start_draft(user.id, " call mum ", now())
            .await
            .unwrap();
        assert_eq!(draft.text, "call mum");
        assert!(service.start_draft(user.id, "  ", now()).await.is_err());

        let at = local(2026, 10, 14, 9, 0);
        let saved = service
            .save_draft(&user, draft.id, at, Recurrence::Daily, now())
            .await
            .unwrap();
        assert_eq!(saved.description, "call mum");
        assert!(matches!(
            service
                .save_draft(&user, draft.id, at, Recurrence::Daily, now())
                .await,
            Err(Error::NotFound)
        ));
    }

    #[tokio::test]
    async fn snoozes_until_tomorrow_morning_local_time() {
        let (service, user) = setup().await;
        let reminder = service
            .create(
                &user,
                input("pills", local(2026, 10, 13, 11, 0), Recurrence::Once),
                now(),
            )
            .await
            .unwrap();
        let db = &service.db;
        let nagging = db
            .fire_reminder(reminder.id, now(), now())
            .await
            .unwrap()
            .unwrap();

        let until = service
            .snooze(&user, nagging.id, Snooze::TomorrowMorning, now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(SGT.local(until), local(2026, 10, 14, 9, 0));
        assert!(service.done(user.id, nagging.id).await.unwrap());
        assert_eq!(
            service
                .snooze(&user, nagging.id, Snooze::OneHour, now())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn calendar_feed_requires_registration_and_valid_token() {
        let (service, user) = setup().await;
        assert!(matches!(
            service.calendar_token(99, false).await,
            Err(Error::NotRegistered)
        ));
        let token = service.calendar_token(user.id, false).await.unwrap();
        assert_eq!(
            service
                .calendar_feed(&token)
                .await
                .unwrap()
                .map(|(u, _)| u.id),
            Some(user.id)
        );
        assert!(service.calendar_feed("nope").await.unwrap().is_none());
    }
    #[tokio::test]
    async fn preferences_work_before_registration_and_manual_language_wins() {
        let service = Service::new(Db::open_in_memory().unwrap());
        assert_eq!(
            service.preferences(7).await.unwrap(),
            Preferences::default()
        );
        service.initialize_language(7, Some("en-US")).await.unwrap();
        assert_eq!(service.preferences(7).await.unwrap().language, "en");
        service
            .set_preferences(7, Some("ru"), Some(30), Some(20))
            .await
            .unwrap();
        service.initialize_language(7, Some("en")).await.unwrap();
        let preferences = service.preferences(7).await.unwrap();
        assert_eq!(preferences.language, "ru");
        assert_eq!(preferences.nag_interval_minutes, 30);
        assert_eq!(preferences.confirmation_check_minutes, 20);
        assert!(service.user(7).await.unwrap().is_none());
        assert_eq!(
            service.preferences(8).await.unwrap(),
            Preferences::default()
        );
        service.initialize_language(8, Some("de")).await.unwrap();
        assert_eq!(service.preferences(8).await.unwrap().language, "ru");
        for (language, interval, check) in [
            (Some("fr"), None, None),
            (None, Some(2), None),
            (None, None, Some(10)),
        ] {
            assert!(matches!(
                service.set_preferences(7, language, interval, check).await,
                Err(Error::InvalidPreferences(_))
            ));
            assert_eq!(
                service.preferences(7).await.unwrap(),
                preferences,
                "invalid patches must be atomic"
            );
        }
        service.unregister(7).await.unwrap();
        assert_eq!(
            service.preferences(7).await.unwrap(),
            Preferences::default(),
            "unregister also clears preferences before timezone registration"
        );
    }
}
