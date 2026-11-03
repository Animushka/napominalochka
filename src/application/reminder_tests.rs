use crate::application::MAX_DESCRIPTION_CHARS;
use crate::application::ports::Registration;
use crate::domain::{InputError, Preferences, Recurrence, Snooze, User, UserTz};
use crate::infrastructure::sqlite::Db;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};

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

struct Fixture {
    service: ReminderService,
    user: User,
    db: Db,
}

async fn setup() -> Fixture {
    let db = Db::open_in_memory().unwrap();
    let service = ReminderService::new(db.clone());
    let profile = Profile {
        id: 1,
        first_name: "Ada".into(),
        last_name: String::new(),
    };
    let (user, registration) = service.set_timezone(profile, SGT).await.unwrap();
    assert_eq!(registration, Registration::Created);
    Fixture { service, user, db }
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
    let Fixture { service, user, .. } = setup().await;
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
    assert!(matches!(
        service
            .create(&user, input(&long, tomorrow, Recurrence::Once), now())
            .await,
        Err(Error::Input(InputError::DescriptionTooLong))
    ));
    assert_eq!(service.list(user.id).await.unwrap(), vec![daily]);
}

#[tokio::test]
async fn unicode_description_limit_preserves_valid_text_and_rejects_edits_atomically() {
    let Fixture { service, user, .. } = setup().await;
    let at = local(2026, 10, 14, 9, 0);
    let boundary = "💛".repeat(MAX_DESCRIPTION_CHARS);
    let reminder = service
        .create(
            &user,
            input(&format!("  {boundary}  "), at, Recurrence::Once),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(reminder.description, boundary);
    let draft = service
        .start_draft(user.id, &boundary, now())
        .await
        .unwrap();
    assert_eq!(draft.text, boundary);

    let too_long = format!("{boundary}я");
    assert!(matches!(
        service
            .update(
                &user,
                reminder.id,
                Some(&too_long),
                at,
                Recurrence::Daily,
                now()
            )
            .await,
        Err(Error::Input(InputError::DescriptionTooLong))
    ));
    assert!(matches!(
        service.start_draft(user.id, &too_long, now()).await,
        Err(Error::Input(InputError::DescriptionTooLong))
    ));
    assert_eq!(
        service.reminder(user.id, reminder.id).await.unwrap(),
        reminder
    );
    assert_eq!(service.draft(user.id, draft.id).await.unwrap(), draft);
}

#[tokio::test]
async fn pending_actions_are_private_and_done_creates_exactly_one_followup() {
    let Fixture { service, user, db } = setup().await;
    let (other, _) = service
        .set_timezone(
            Profile {
                id: 2,
                first_name: "Eve".into(),
                last_name: String::new(),
            },
            SGT,
        )
        .await
        .unwrap();
    service
        .set_preferences(user.id, None, None, Some(20))
        .await
        .unwrap();
    let reminder = service
        .create(
            &user,
            input("unfinished", local(2026, 10, 13, 11, 0), Recurrence::Once),
            now(),
        )
        .await
        .unwrap();
    let due = reminder.due_at;
    let nagging = db
        .fire_reminder(reminder.id, due, due)
        .await
        .unwrap()
        .unwrap();
    assert!(service.list(user.id).await.unwrap().is_empty());
    assert!(service.pending(other.id).await.unwrap().is_empty());
    assert!(!service.done_at(other.id, nagging.id, due).await.unwrap());
    assert_eq!(
        service
            .snooze(&other, nagging.id, Snooze::OneHour, due)
            .await
            .unwrap(),
        None
    );
    assert_eq!(service.pending(user.id).await.unwrap()[0].next_at, due);

    for (snooze, until) in [
        (Snooze::TenMinutes, due + Duration::minutes(10)),
        (Snooze::OneHour, due + Duration::hours(1)),
    ] {
        assert_eq!(
            service
                .snooze(&user, nagging.id, snooze, due)
                .await
                .unwrap(),
            Some(until)
        );
        let pending = service.pending(user.id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, nagging.id);
        assert_eq!(pending[0].reminder_id, None);
        assert_eq!(pending[0].description, "unfinished");
        assert_eq!(pending[0].next_at, until);
    }

    assert!(service.done_at(user.id, nagging.id, due).await.unwrap());
    assert!(!service.done_at(user.id, nagging.id, due).await.unwrap());
    assert!(service.pending(user.id).await.unwrap().is_empty());
    assert_eq!(
        service
            .snooze(&user, nagging.id, Snooze::OneHour, due)
            .await
            .unwrap(),
        None
    );
    let checks = db
        .due_confirmations(due + Duration::minutes(20))
        .await
        .unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].user_id, user.id);
}

#[tokio::test]
async fn only_owners_can_change_reminders() {
    let Fixture { service, user, .. } = setup().await;
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
async fn schedule_anchor_survives_text_edits_and_resets_for_schedule_changes() {
    let Fixture { service, user, db } = setup().await;
    let anchor = local(2027, 1, 31, 9, 0);
    let reminder = service
        .create(
            &user,
            input("month end", anchor, Recurrence::Monthly),
            now(),
        )
        .await
        .unwrap();
    assert_eq!(reminder.anchor_local, Some(anchor));
    db.fire_reminder(reminder.id, reminder.due_at, reminder.due_at)
        .await
        .unwrap();
    let february = local(2027, 2, 28, 9, 0);
    let renamed = service
        .update(
            &user,
            reminder.id,
            Some("renamed"),
            february,
            Recurrence::Monthly,
            reminder.due_at,
        )
        .await
        .unwrap();
    assert_eq!(renamed.anchor_local, Some(anchor));
    assert_eq!(user.tz.local(renamed.due_at), february);
    db.fire_reminder(reminder.id, renamed.due_at, renamed.due_at)
        .await
        .unwrap();
    let march = service.reminder(user.id, reminder.id).await.unwrap();
    assert_eq!(user.tz.local(march.due_at), local(2027, 3, 31, 9, 0));
    // The edit form may still display February, but explicit rename cannot reset it.
    let stale_form_save = service
        .rename(user.id, reminder.id, "edited after firing")
        .await
        .unwrap();
    assert_eq!(stale_form_save.due_at, march.due_at);
    assert_eq!(stale_form_save.anchor_local, Some(anchor));
    assert!(matches!(
        service.rename(2, reminder.id, "other user").await,
        Err(Error::NotFound)
    ));

    let chosen = local(2027, 4, 30, 10, 0);
    let moved = service
        .update(
            &user,
            reminder.id,
            None,
            chosen,
            Recurrence::Monthly,
            renamed.due_at,
        )
        .await
        .unwrap();
    assert_eq!(moved.anchor_local, Some(chosen));
    let repeated = service
        .update(
            &user,
            reminder.id,
            None,
            chosen,
            Recurrence::Weekly,
            renamed.due_at,
        )
        .await
        .unwrap();
    assert_eq!(repeated.anchor_local, Some(chosen));
    assert_eq!(repeated.recurrence, Recurrence::Weekly);
}

#[tokio::test]
async fn creating_inside_a_dst_gap_preserves_the_requested_clock() {
    let Fixture { service, db, .. } = setup().await;
    let (user, _) = service
        .set_timezone(
            Profile {
                id: 1,
                first_name: "Ada".into(),
                last_name: String::new(),
            },
            UserTz::Named(Tz::Europe__London),
        )
        .await
        .unwrap();
    let anchor = local(2026, 3, 29, 1, 30);
    let before = user.tz.utc(local(2026, 3, 28, 12, 0));
    let reminder = service
        .create(&user, input("early", anchor, Recurrence::Daily), before)
        .await
        .unwrap();
    assert_eq!(reminder.anchor_local, Some(anchor));
    assert_eq!(user.tz.local(reminder.due_at), local(2026, 3, 29, 2, 30));
    db.fire_reminder(reminder.id, reminder.due_at, reminder.due_at)
        .await
        .unwrap();
    let next = service.reminder(user.id, reminder.id).await.unwrap();
    assert_eq!(user.tz.local(next.due_at), local(2026, 3, 30, 1, 30));
}

#[tokio::test]
async fn drafts_are_saved_once() {
    let Fixture { service, user, .. } = setup().await;
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
    assert_eq!(saved.anchor_local, Some(at));
    assert!(matches!(
        service
            .save_draft(&user, draft.id, at, Recurrence::Daily, now())
            .await,
        Err(Error::NotFound)
    ));
}

#[tokio::test]
async fn snoozes_until_tomorrow_morning_local_time() {
    let Fixture { service, user, db } = setup().await;
    let reminder = service
        .create(
            &user,
            input("pills", local(2026, 10, 13, 11, 0), Recurrence::Once),
            now(),
        )
        .await
        .unwrap();
    let nagging = db
        .fire_reminder(reminder.id, reminder.due_at, reminder.due_at)
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
    let Fixture { service, user, .. } = setup().await;
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
    let service = ReminderService::new(Db::open_in_memory().unwrap());
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
#[tokio::test]
async fn an_elapsed_duration_preserves_the_second_clock_occurrence() {
    let Fixture { service, .. } = setup().await;
    let london = UserTz::parse("Europe/London").unwrap();
    let (user, _) = service
        .set_timezone(
            Profile {
                id: 1,
                first_name: "Ada".into(),
                last_name: String::new(),
            },
            london,
        )
        .await
        .unwrap();
    let now = DateTime::parse_from_rfc3339("2026-10-25T00:30:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let target = now + Duration::hours(1);
    let draft = service.start_draft(user.id, "proof", now).await.unwrap();
    let reminder = service
        .save_draft_at(&user, draft.id, target, Recurrence::Daily, now)
        .await
        .unwrap();
    assert_eq!(reminder.due_at, target);
    assert_eq!(reminder.anchor_local, Some(london.local(target)));
    let renamed = service
        .update_at(
            &user,
            reminder.id,
            Some("updated"),
            target,
            Recurrence::Daily,
            now,
        )
        .await
        .unwrap();
    assert_eq!(renamed.due_at, target);
    assert_eq!(renamed.description, "updated");
}

#[tokio::test]
async fn an_exact_weekend_selection_still_obeys_weekday_recurrence() {
    let Fixture { service, user, .. } = setup().await;
    let target = SGT.utc(local(2026, 10, 17, 9, 0));
    let draft = service
        .start_draft(user.id, "weekdays", now())
        .await
        .unwrap();
    let reminder = service
        .save_draft_at(&user, draft.id, target, Recurrence::Weekdays, now())
        .await
        .unwrap();
    assert_eq!(reminder.due_at, SGT.utc(local(2026, 10, 19, 9, 0)));
    assert_eq!(reminder.anchor_local, Some(local(2026, 10, 17, 9, 0)));
}
