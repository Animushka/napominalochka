use super::*;
use crate::domain::Recurrence;
use crate::domain::Snooze;
use crate::domain::{ConfirmationDelay, Language, NaggingId, UserId};
use crate::infrastructure::sqlite::Db;
use chrono::{NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use teloxide::types::InlineKeyboardButtonKind;

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

fn sender() -> Sender {
    Sender {
        chat_id: UserId(7),
        first_name: "Ada".into(),
        last_name: String::new(),
        private: true,
    }
}

fn buttons(markup: &Markup) -> Vec<(String, Action)> {
    let Markup::Inline(keyboard) = markup else {
        return Vec::new();
    };
    keyboard
        .inline_keyboard
        .iter()
        .flatten()
        .filter_map(|b| match &b.kind {
            InlineKeyboardButtonKind::CallbackData(data) => {
                Some((b.text.clone(), Action::parse(data)?))
            }
            _ => None,
        })
        .collect()
}

fn find(markup: &Markup, label: &str) -> Action {
    buttons(markup)
        .into_iter()
        .find(|(text, _)| text == label)
        .unwrap_or_else(|| panic!("no button {label:?} in {:?}", buttons(markup)))
        .1
}

async fn press(db: &ReminderService, action: Action) -> CallbackReply {
    on_callback(
        db,
        UserId(7),
        &action.encode(),
        Some("message"),
        None,
        now(),
    )
    .await
    .unwrap()
}

fn edit_markup(reply: &CallbackReply) -> Markup {
    Markup::Inline(reply.edit.as_ref().unwrap().keyboard.clone())
}

#[tokio::test]
async fn new_user_is_asked_for_timezone_then_continues() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    let replies = on_text(&db, &sender(), "call mum", false, now())
        .await
        .unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].markup, Markup::AskLocation);

    let replies = on_text(&db, &sender(), "Atlantis", false, now())
        .await
        .unwrap();
    assert!(
        replies[0].text.contains("don't recognise"),
        "{}",
        replies[0].text
    );

    let replies = on_text(&db, &sender(), "Singapore", false, now())
        .await
        .unwrap();
    assert!(replies[0].text.contains("Asia/Singapore"));
    assert_eq!(replies[0].markup, Markup::RemoveKeyboard);
    assert!(replies[1].text.contains("call mum"));
    assert!(replies[1].text.contains("When should I remind you?"));
}

#[tokio::test]
async fn quick_choice_flow_saves_in_two_taps() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();

    let replies = on_text(&db, &sender(), "call mum", false, now())
        .await
        .unwrap();
    let tomorrow = find(&replies[0].markup, "Tomorrow 09:00");

    let repeat = press(&db, tomorrow).await;
    let daily = find(&edit_markup(&repeat), "Every day");

    let saved = press(&db, daily).await;
    assert!(
        saved
            .edit
            .as_ref()
            .unwrap()
            .text
            .starts_with("✅ Reminder saved")
    );

    let reminders = store.reminders_for_user(UserId(7)).await.unwrap();
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].description, "call mum");
    assert_eq!(reminders[0].recurrence, Recurrence::Daily);
    assert_eq!(SGT.local(reminders[0].due_at), local(2026, 10, 14, 9, 0));

    // Pressing the same button again does not create a duplicate.
    press(&db, daily).await;
    assert_eq!(store.reminders_for_user(UserId(7)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn calendar_flow_picks_date_hour_and_minute() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();
    let replies = on_text(&db, &sender(), "dentist", false, now())
        .await
        .unwrap();

    let calendar = press(&db, find(&replies[0].markup, "📅 Pick a date")).await;
    let next_month = press(&db, find(&edit_markup(&calendar), "›")).await;
    let hours = press(&db, find(&edit_markup(&next_month), "2")).await;
    let minutes = press(&db, find(&edit_markup(&hours), "14")).await;
    let repeat = press(&db, find(&edit_markup(&minutes), "14:45")).await;
    press(&db, find(&edit_markup(&repeat), "Once")).await;

    let reminders = store.reminders_for_user(UserId(7)).await.unwrap();
    assert_eq!(SGT.local(reminders[0].due_at), local(2026, 11, 2, 14, 45));
    assert_eq!(reminders[0].recurrence, Recurrence::Once);
}

#[tokio::test]
async fn typed_time_answers_the_when_question() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();
    on_text(&db, &sender(), "pay rent", false, now())
        .await
        .unwrap();

    let replies = on_text(&db, &sender(), "fri 6pm", false, now())
        .await
        .unwrap();
    assert!(
        replies[0].text.contains("Fri 16 Oct 2026, 18:00"),
        "{}",
        replies[0].text
    );
    press(&db, find(&replies[0].markup, "Monthly · 16")).await;

    let reminders = store.reminders_for_user(UserId(7)).await.unwrap();
    assert_eq!(reminders[0].description, "pay rent");
    assert_eq!(reminders[0].recurrence, Recurrence::Monthly);
}

#[tokio::test]
async fn edit_and_delete_from_the_list() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();
    let replies = on_text(&db, &sender(), "stretch", false, now())
        .await
        .unwrap();
    let repeat = press(&db, find(&replies[0].markup, "1 hour")).await;
    press(&db, find(&edit_markup(&repeat), "Weekdays")).await;
    let reminder = store.reminders_for_user(UserId(7)).await.unwrap().remove(0);

    let list = press(&db, Action::List).await;
    let manage = press(&db, find(&edit_markup(&list), "1. stretch")).await;
    let repeat = press(&db, find(&edit_markup(&manage), "🔁 Change repeat")).await;
    press(&db, find(&edit_markup(&repeat), "Every Tue")).await;
    assert_eq!(
        store
            .reminder(UserId(7), reminder.id)
            .await
            .unwrap()
            .unwrap()
            .recurrence,
        Recurrence::Weekly
    );

    let deleted = press(&db, Action::Delete(reminder.id)).await;
    assert!(deleted.edit.unwrap().text.contains("Removed: stretch"));
    assert!(
        store
            .reminders_for_user(UserId(7))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cancel_discards_the_draft() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();
    let replies = on_text(&db, &sender(), "nothing", false, now())
        .await
        .unwrap();
    let quick = find(&replies[0].markup, "15 min");
    press(&db, find(&replies[0].markup, "✖ Cancel")).await;
    let reply = press(&db, quick).await;
    assert_eq!(
        reply.toast.as_deref(),
        Some("This reminder is no longer available.")
    );
}

#[tokio::test]
async fn snooze_and_done_on_a_nag() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();
    let reminder = store
        .add_reminder(UserId(7), "pills".into(), now(), Recurrence::Once)
        .await
        .unwrap();
    let nagging = store
        .fire_reminder(reminder.id, now(), now())
        .await
        .unwrap()
        .unwrap();

    let reply = on_callback(
        &db,
        UserId(7),
        &Action::Snooze(nagging.id, Snooze::TomorrowMorning).encode(),
        Some("⏰ pills"),
        None,
        now(),
    )
    .await
    .unwrap();
    assert!(
        reply
            .edit
            .unwrap()
            .text
            .contains("Snoozed until Wed 14 Oct 2026, 09:00")
    );
    assert!(store.due_naggings(now()).await.unwrap().is_empty());

    let reply = on_callback(
        &db,
        UserId(7),
        &Action::Done(nagging.id).encode(),
        Some("⏰ pills"),
        None,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(reply.edit.unwrap().text, "✅ pills");
    assert!(
        store
            .due_naggings(now() + Duration::days(2))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn other_users_cannot_press_your_buttons() {
    let store = Db::open_in_memory().unwrap();
    let db = ReminderService::new(store.clone());
    db.set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    db.set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&db, &sender(), SGT, now()).await.unwrap();
    let mut other = sender();
    other.chat_id = UserId(8);
    set_timezone(&db, &other, SGT, now()).await.unwrap();

    let replies = on_text(&db, &sender(), "secret", false, now())
        .await
        .unwrap();
    let quick = find(&replies[0].markup, "15 min");
    let reply = on_callback(&db, UserId(8), &quick.encode(), None, None, now())
        .await
        .unwrap();
    assert_eq!(
        reply.toast.as_deref(),
        Some("This reminder is no longer available.")
    );
}
#[tokio::test]
async fn typed_duration_survives_callbacks_and_saves_the_exact_fold_instant() {
    let store = Db::open_in_memory().unwrap();
    let service = ReminderService::new(store.clone());
    service
        .set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    let tz = UserTz::parse("Europe/London").unwrap();
    let now = local(2026, 10, 25, 0, 30).and_utc();
    set_timezone(&service, &sender(), tz, now).await.unwrap();
    on_text(&service, &sender(), "pills", false, now)
        .await
        .unwrap();
    let replies = on_text(&service, &sender(), "in 30 minutes", false, now)
        .await
        .unwrap();
    let action = find(&replies[0].markup, "Once");
    let target = now + Duration::minutes(30);
    assert!(matches!(action, Action::SaveInstant(_, at, Recurrence::Once) if at == target));
    on_callback(
        &service,
        UserId(7),
        &action.encode(),
        Some("pills"),
        None,
        now,
    )
    .await
    .unwrap();
    let reminder = store.reminders_for_user(UserId(7)).await.unwrap().remove(0);
    assert_eq!(reminder.due_at, target);
    assert_eq!(reminder.anchor_local, Some(tz.local(target)));
}

#[tokio::test]
async fn list_navigation_is_owner_scoped_and_clamps_stale_pages() {
    let store = Db::open_in_memory().unwrap();
    let service = ReminderService::new(store.clone());
    service
        .set_preferences(UserId(7), Some(Language::En), None, None)
        .await
        .unwrap();
    set_timezone(&service, &sender(), SGT, now()).await.unwrap();
    for index in 1..=35 {
        store
            .add_reminder(
                UserId(7),
                format!("item {index}"),
                now() + Duration::days(1),
                Recurrence::Daily,
            )
            .await
            .unwrap();
    }
    let second = press(&service, Action::ListPage(1)).await;
    assert!(second.edit.as_ref().unwrap().text.contains("11. item 11"));
    let last = press(&service, Action::ListPage(usize::MAX)).await;
    assert!(last.edit.as_ref().unwrap().text.contains("35. item 35"));
    let other = Sender {
        chat_id: UserId(8),
        ..sender()
    };
    set_timezone(&service, &other, SGT, now()).await.unwrap();
    let empty = on_callback(
        &service,
        UserId(8),
        &Action::ListPage(1).encode(),
        None,
        None,
        now(),
    )
    .await
    .unwrap();
    assert!(!empty.edit.unwrap().text.contains("item"));
}
async fn completion_fixture(
    recurrence: Recurrence,
) -> (Db, ReminderService, crate::domain::Nagging) {
    let store = Db::open_in_memory().unwrap();
    let service = ReminderService::new(store.clone());
    service
        .set_preferences(
            UserId(7),
            Some(Language::En),
            None,
            Some(ConfirmationDelay::TwentyMinutes),
        )
        .await
        .unwrap();
    set_timezone(&service, &sender(), SGT, now()).await.unwrap();
    let reminder = store
        .add_reminder(UserId(7), "pills".into(), now(), recurrence)
        .await
        .unwrap();
    let pending = store
        .fire_reminder(reminder.id, now(), now())
        .await
        .unwrap()
        .unwrap();
    (store, service, pending)
}

#[tokio::test]
async fn completing_and_undoing_from_chat_restores_once_without_resetting_its_timer() {
    for done in [
        Action::Done as fn(NaggingId) -> Action,
        Action::LegacyNaggingAck,
    ] {
        let (store, service, pending) = completion_fixture(Recurrence::Once).await;
        let reply = on_callback(
            &service,
            UserId(7),
            &done(pending.id).encode(),
            Some("⏰ pills"),
            None,
            now(),
        )
        .await
        .unwrap();
        assert_eq!(reply.edit.as_ref().unwrap().text, "✅ pills");
        let undo = find(&edit_markup(&reply), "↩️ Undo completion");
        let completion = service
            .completion_for_source(UserId(7), pending.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(undo, Action::UndoCompletion(completion.id));
        assert!(store.naggings_for_user(UserId(7)).await.unwrap().is_empty());

        let undo_at = now() + Duration::minutes(2);
        let reply = on_callback(
            &service,
            UserId(7),
            &undo.encode(),
            Some("✅ pills"),
            None,
            undo_at,
        )
        .await
        .unwrap();
        assert_eq!(reply.toast.as_deref(), Some("Completion undone"));
        assert!(reply.edit.unwrap().text.contains("10 minutes"));
        let restored = store.naggings_for_user(UserId(7)).await.unwrap();
        assert_eq!(restored.len(), 1);
        assert_ne!(restored[0].id, pending.id);
        assert_eq!(restored[0].next_at, undo_at + Duration::minutes(10));
        assert!(
            store
                .due_confirmations(now() + Duration::hours(1))
                .await
                .unwrap()
                .is_empty()
        );

        let replay = on_callback(
            &service,
            UserId(7),
            &undo.encode(),
            None,
            None,
            undo_at + Duration::minutes(1),
        )
        .await
        .unwrap();
        assert_eq!(replay.toast.as_deref(), Some("Already undone"));
        assert_eq!(store.naggings_for_user(UserId(7)).await.unwrap(), restored);
        let journal = store.completion_history(UserId(7), None, 20).await.unwrap();
        assert_eq!(journal.items.len(), 1);
        assert!(journal.items[0].undone_at.is_some());
    }
}

#[tokio::test]
async fn a_negative_followup_answer_works_after_the_sent_queue_row_was_removed() {
    use crate::application::delivery::{Delivery, DeliveryQueue};
    let (store, service, pending) = completion_fixture(Recurrence::Once).await;
    on_callback(
        &service,
        UserId(7),
        &Action::Done(pending.id).encode(),
        Some("⏰ pills"),
        None,
        now(),
    )
    .await
    .unwrap();
    let at = now() + Duration::minutes(20);
    let confirmation = store.due_confirmations(at).await.unwrap().remove(0);
    let panel = Panel {
        text: String::new(),
        keyboard: panels::confirmation_keyboard_lang(confirmation.completion_id, Lang::En),
    };
    let retry = find(&Markup::Inline(panel.keyboard), "No, remind me again");
    let job = store
        .claim_delivery(UserId(7), at, at + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    store
        .finish_delivery(job, Delivery::Sent, at)
        .await
        .unwrap();
    assert!(
        store
            .due_confirmations(at + Duration::hours(1))
            .await
            .unwrap()
            .is_empty()
    );
    let reply = on_callback(
        &service,
        UserId(7),
        &retry.encode(),
        Some("Follow-up check"),
        None,
        at + Duration::minutes(1),
    )
    .await
    .unwrap();
    assert_eq!(reply.toast.as_deref(), Some("Completion undone"));
    assert_eq!(
        store.naggings_for_user(UserId(7)).await.unwrap()[0].next_at,
        at + Duration::minutes(11)
    );
}

#[tokio::test]
async fn another_user_cannot_undo_a_completion_from_a_forwarded_button() {
    let (store, service, pending) = completion_fixture(Recurrence::Once).await;
    service
        .set_preferences(UserId(8), Some(Language::En), None, None)
        .await
        .unwrap();
    let done = on_callback(
        &service,
        UserId(7),
        &Action::Done(pending.id).encode(),
        Some("⏰ pills"),
        None,
        now(),
    )
    .await
    .unwrap();
    let undo = find(&edit_markup(&done), "↩️ Undo completion");
    let reply = on_callback(
        &service,
        UserId(8),
        &undo.encode(),
        Some("forwarded"),
        None,
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        reply.toast.as_deref(),
        Some("This reminder is no longer available.")
    );
    assert!(store.naggings_for_user(UserId(7)).await.unwrap().is_empty());
    assert!(store.naggings_for_user(UserId(8)).await.unwrap().is_empty());
    let journal = store.completion_history(UserId(7), None, 20).await.unwrap();
    assert!(journal.items[0].undone_at.is_none());
}

#[tokio::test]
async fn an_old_chat_undo_cannot_restore_a_previous_recurring_occurrence() {
    let (store, service, pending) = completion_fixture(Recurrence::Daily).await;
    let done = on_callback(
        &service,
        UserId(7),
        &Action::Done(pending.id).encode(),
        Some("⏰ pills"),
        None,
        now(),
    )
    .await
    .unwrap();
    let undo = find(&edit_markup(&done), "↩️ Undo completion");
    let next_day = now() + Duration::days(1);
    let current = store
        .fire_reminder(pending.reminder_id.unwrap(), next_day, next_day)
        .await
        .unwrap()
        .unwrap();
    let reply = on_callback(&service, UserId(7), &undo.encode(), None, None, next_day)
        .await
        .unwrap();
    assert_eq!(
        reply.toast.as_deref(),
        Some(Lang::En.text("undo.unavailable").as_str())
    );
    assert!(reply.edit.is_none());
    assert_eq!(
        store.naggings_for_user(UserId(7)).await.unwrap(),
        vec![current]
    );
    assert!(
        store
            .completion_history(UserId(7), None, 20)
            .await
            .unwrap()
            .items[0]
            .undone_at
            .is_none()
    );
}
