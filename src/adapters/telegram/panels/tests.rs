use super::*;

fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, 0)
        .unwrap()
}

fn actions(panel: &Panel) -> Vec<Action> {
    panel
        .keyboard
        .inline_keyboard
        .iter()
        .flatten()
        .filter_map(|b| match &b.kind {
            teloxide::types::InlineKeyboardButtonKind::CallbackData(data) => Action::parse(data),
            _ => None,
        })
        .collect()
}

#[test]
fn calendar_disables_past_days_and_previous_month() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
    let panel = calendar(Subject::Draft(1), "x", 2026, 10, today);
    let days: Vec<NaiveDate> = actions(&panel)
        .into_iter()
        .filter_map(|a| match a {
            Action::Day(_, d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(days.first(), Some(&today));
    assert_eq!(days.last(), NaiveDate::from_ymd_opt(2026, 10, 31).as_ref());
    assert!(!actions(&panel).contains(&Action::Calendar(Subject::Draft(1), 2026, 9)));
    assert!(actions(&panel).contains(&Action::Calendar(Subject::Draft(1), 2026, 11)));
    assert!(
        panel
            .keyboard
            .inline_keyboard
            .iter()
            .all(|row| row.len() <= 7)
    );
}

#[test]
fn minute_picker_hides_past_times() {
    let now = local(2026, 10, 13, 10, 32);
    let panel = minutes(Subject::Draft(1), "x", now.date(), 10, now);
    let first = actions(&panel).into_iter().find_map(|a| match a {
        Action::At(_, at) => Some(at),
        _ => None,
    });
    assert_eq!(first, Some(local(2026, 10, 13, 10, 35)));
}

#[test]
fn quick_choices_move_evening_to_tomorrow_when_late() {
    let panel = when(Subject::Draft(1), "x", local(2026, 10, 13, 21, 0));
    assert!(actions(&panel).contains(&Action::At(Subject::Draft(1), local(2026, 10, 14, 20, 0))));
}

#[test]
fn russian_flow_localizes_controls_without_changing_user_content() {
    let panel = when_lang(
        Subject::Draft(1),
        "Every day",
        local(2026, 10, 13, 10, 0),
        Lang::Ru,
    );
    assert!(panel.text.starts_with("📝 Every day"));
    assert!(panel.text.contains("Когда напомнить?"));
    assert!(
        panel
            .keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .any(|button| button.text == "Завтра 09:00")
    );
    let panel = calendar_lang(
        Subject::Draft(1),
        "x",
        2026,
        10,
        local(2026, 10, 13, 10, 0).date(),
        Lang::Ru,
    );
    assert!(
        panel
            .keyboard
            .inline_keyboard
            .iter()
            .flatten()
            .any(|button| button.text == "Октябрь 2026")
    );
    let t = local(2026, 1, 1, 0, 0).and_utc();
    assert_eq!(
        relative_lang(t, t + Duration::minutes(200), Lang::Ru),
        "через 3 ч 20 мин"
    );
}

#[test]
fn relative_times() {
    let t = local(2026, 1, 1, 0, 0).and_utc();
    assert_eq!(relative(t, t + Duration::minutes(5)), "in 5 min");
    assert_eq!(relative(t, t + Duration::minutes(200)), "in 3 h 20 min");
    assert_eq!(relative(t, t + Duration::hours(2)), "in 2 h");
    assert_eq!(relative(t, t + Duration::days(4)), "in 4 days");
}
#[test]
fn list_pages_fit_telegram_and_reach_every_reminder() {
    let user = User {
        id: 7,
        first_name: "Ada".into(),
        last_name: String::new(),
        tz: UserTz::parse("UTC").unwrap(),
    };
    let reminders: Vec<_> = (1..=37)
        .map(|id| Reminder {
            id,
            user_id: user.id,
            description: "💛".repeat(1000),
            due_at: local(2026, 10, 30, 9, 0).and_utc(),
            recurrence: Recurrence::Daily,
            anchor_local: None,
            settings: Default::default(),
        })
        .collect();
    for lang in [Lang::Ru, Lang::En] {
        let mut reachable = Vec::new();
        for page in 0..4 {
            let panel = list_page_lang(&reminders, &user, page, lang);
            assert!(panel.text.encode_utf16().count() <= 4096);
            let page_actions = actions(&panel);
            reachable.extend(page_actions.iter().filter_map(|a| match a {
                Action::Manage(id) => Some(*id),
                _ => None,
            }));
            if page < 3 {
                assert!(page_actions.contains(&Action::ListPage(page + 1)));
            }
            if page > 0 {
                assert!(page_actions.contains(&Action::ListPage(page - 1)));
            }
        }
        assert_eq!(reachable, (1..=37).collect::<Vec<_>>());
        let stale = list_page_lang(&reminders, &user, usize::MAX, lang);
        assert!(actions(&stale).contains(&Action::Manage(37)));
    }
}

#[test]
fn elapsed_quick_choices_keep_utc_across_the_repeated_hour() {
    let tz = UserTz::parse("Europe/London").unwrap();
    let now = local(2026, 10, 25, 0, 30).and_utc();
    let panel = when_at_lang(Subject::Draft(1), "pills", now, tz, Lang::En);
    assert!(actions(&panel).contains(&Action::AtInstant(
        Subject::Draft(1),
        now + Duration::hours(1)
    )));
    let repeat = repeat_time_lang(
        Subject::Draft(1),
        "pills",
        ReminderTime::Instant(now + Duration::hours(1)),
        tz,
        Lang::En,
    );
    assert!(actions(&repeat).contains(&Action::SaveInstant(
        Subject::Draft(1),
        now + Duration::hours(1),
        Recurrence::Once
    )));
}
#[test]
fn completion_controls_refer_to_durable_history_and_legacy_checks_stay_compatible() {
    for lang in [Lang::Ru, Lang::En] {
        let undo = completion_keyboard_lang(42, lang);
        assert_eq!(undo.inline_keyboard.len(), 1);
        let check = confirmation_keyboard_lang(Some(42), lang);
        assert_eq!(check.inline_keyboard.len(), 2);
        assert_eq!(check.inline_keyboard[0][0].text, lang.text("check.ack"));
        assert_eq!(check.inline_keyboard[1][0].text, lang.text("check.retry"));
        let panel = Panel {
            text: String::new(),
            keyboard: check,
        };
        assert_eq!(actions(&panel), vec![Action::UndoCompletion(42)]);
        let legacy = confirmation_keyboard_lang(None, lang);
        assert_eq!(legacy.inline_keyboard.len(), 1);
        assert_eq!(legacy.inline_keyboard[0][0].text, lang.text("check.ack"));
    }
}
