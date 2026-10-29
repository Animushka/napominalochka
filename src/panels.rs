//! Message text and inline keyboards for each step of the reminder flow.

use chrono::{
    DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc,
};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use crate::callback::{Action, Subject};
use crate::i18n::Lang;
use crate::model::Snooze;
use crate::model::{Recurrence, Reminder, ReminderTime, User, UserTz};

pub struct Panel {
    pub text: String,
    pub keyboard: InlineKeyboardMarkup,
}

/// How far ahead the calendar can be paged.
const MAX_MONTHS_AHEAD: u32 = 60;
/// Keep each page comfortably within Telegram's 4096 UTF-16-unit text budget.
const LIST_PAGE_SIZE: usize = 10;
const LIST_PREVIEW_UNITS: usize = 120;

fn button(label: impl Into<String>, action: Action) -> InlineKeyboardButton {
    InlineKeyboardButton::callback(label, action.encode())
}

fn blank() -> InlineKeyboardButton {
    button(" ", Action::Noop)
}

/// "in 5 min", "in 3 h 20 min", "in 4 days".
pub fn relative_lang(from: DateTime<Utc>, to: DateTime<Utc>, lang: Lang) -> String {
    let minutes = (to - from).num_minutes().max(0);
    match minutes {
        0 => lang.text("relative.now"),
        1..=59 => lang.render("relative.minutes", &[("minutes", minutes.to_string())]),
        60..=2879 => match minutes % 60 {
            0 => lang.render("relative.hours", &[("hours", (minutes / 60).to_string())]),
            m => lang.render(
                "relative.hours_minutes",
                &[
                    ("hours", (minutes / 60).to_string()),
                    ("minutes", m.to_string()),
                ],
            ),
        },
        _ => lang.render("relative.days", &[("days", (minutes / 1440).to_string())]),
    }
}

fn back_or_cancel(lang: Lang, subject: Subject, back: Action) -> Vec<InlineKeyboardButton> {
    let mut row = vec![button(lang.text("button.back"), back)];
    if let Subject::Draft(id) = subject {
        row.push(button(lang.text("button.cancel"), Action::CancelDraft(id)));
    }
    row
}

/// Step 1: quick choices.
pub fn when_lang(subject: Subject, text: &str, now: NaiveDateTime, lang: Lang) -> Panel {
    when_inner(subject, text, now, None, lang)
}

/// Real chat flows retain the current instant for elapsed quick choices across DST.
pub fn when_at_lang(
    subject: Subject,
    text: &str,
    now: DateTime<Utc>,
    tz: UserTz,
    lang: Lang,
) -> Panel {
    when_inner(subject, text, tz.local(now), Some(now), lang)
}

fn when_inner(
    subject: Subject,
    text: &str,
    now: NaiveDateTime,
    instant: Option<DateTime<Utc>>,
    lang: Lang,
) -> Panel {
    let in_minutes = |m: i64| match instant {
        Some(now) => Action::AtInstant(
            subject,
            now.checked_add_signed(Duration::minutes(m)).unwrap_or(now),
        ),
        None => Action::At(
            subject,
            truncate_to_minute(now.checked_add_signed(Duration::minutes(m)).unwrap_or(now)),
        ),
    };
    let today = now.date();
    let evening = today.and_time(time(20, 0));
    let (evening_label, evening) = if now < evening - Duration::minutes(30) {
        (lang.text("button.evening"), evening)
    } else {
        (
            lang.text("button.tomorrow_evening"),
            evening + Duration::days(1),
        )
    };
    let tomorrow_morning = (today + Duration::days(1)).and_time(time(9, 0));

    let mut rows = vec![
        vec![
            button(lang.text("button.fifteen"), in_minutes(15)),
            button(lang.text("button.hour"), in_minutes(60)),
            button(lang.text("button.three_hours"), in_minutes(180)),
        ],
        vec![
            button(evening_label, Action::At(subject, evening)),
            button(
                lang.text("button.tomorrow_morning"),
                Action::At(subject, tomorrow_morning),
            ),
        ],
        vec![button(
            lang.text("button.pick_date"),
            Action::Calendar(subject, today.year(), today.month()),
        )],
    ];
    let prompt = match subject {
        Subject::Draft(id) => {
            rows.push(vec![button(
                lang.text("button.cancel"),
                Action::CancelDraft(id),
            )]);
            lang.render("when.ask", &[("hint", lang.text("hint.type_time"))])
        }
        Subject::Reminder(id) => {
            rows.push(vec![button(lang.text("button.back"), Action::Manage(id))]);
            lang.text("when.new_time")
        }
    };
    Panel {
        text: format!("📝 {text}\n\n{prompt}"),
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

/// Step 2: month calendar. Past days are not selectable.
pub fn calendar_lang(
    subject: Subject,
    text: &str,
    year: i32,
    month: u32,
    today: NaiveDate,
    lang: Lang,
) -> Panel {
    let first =
        NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(today.with_day(1).unwrap_or(today));
    let this_month = today.with_day(1).unwrap_or(today);
    let last_month = this_month
        .checked_add_months(Months::new(MAX_MONTHS_AHEAD))
        .unwrap_or(this_month);
    let first = first.clamp(this_month, last_month);

    let nav = |enabled: bool, label: &str, target: Option<NaiveDate>| match (enabled, target) {
        (true, Some(d)) => button(label, Action::Calendar(subject, d.year(), d.month())),
        _ => blank(),
    };
    let mut rows = vec![
        vec![
            nav(
                first > this_month,
                "‹",
                first.checked_sub_months(Months::new(1)),
            ),
            button(
                format!(
                    "{} {}",
                    lang.text(&format!("month.{}", first.month())),
                    first.year()
                ),
                Action::Noop,
            ),
            nav(
                first < last_month,
                "›",
                first.checked_add_months(Months::new(1)),
            ),
        ],
        (0..7)
            .map(|d| button(lang.text(&format!("weekday.{d}")), Action::Noop))
            .collect(),
    ];

    let mut week: Vec<InlineKeyboardButton> = (0..first.weekday().num_days_from_monday())
        .map(|_| blank())
        .collect();
    let mut day = first;
    while day.month() == first.month() {
        week.push(if day < today {
            button("·", Action::Noop)
        } else if day == today {
            button(format!("{}•", day.day()), Action::Day(subject, day))
        } else {
            button(day.day().to_string(), Action::Day(subject, day))
        });
        if week.len() == 7 {
            rows.push(std::mem::take(&mut week));
        }
        day += Duration::days(1);
    }
    if !week.is_empty() {
        week.resize_with(7, blank);
        rows.push(week);
    }
    rows.push(back_or_cancel(lang, subject, Action::When(subject)));

    Panel {
        text: format!("📝 {text}\n\n{}", lang.text("date.pick")),
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

/// Step 3: hour of the day.
pub fn hours_lang(
    subject: Subject,
    text: &str,
    date: NaiveDate,
    now: NaiveDateTime,
    lang: Lang,
) -> Panel {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = (0..24)
        .collect::<Vec<u32>>()
        .chunks(6)
        .map(|chunk| {
            chunk
                .iter()
                .map(|&h| {
                    let past = date == now.date() && h < now.hour();
                    if past {
                        button("·", Action::Noop)
                    } else {
                        button(format!("{h:02}"), Action::Hour(subject, date, h))
                    }
                })
                .collect()
        })
        .collect();
    rows.push(back_or_cancel(
        lang,
        subject,
        Action::Calendar(subject, date.year(), date.month()),
    ));

    Panel {
        text: format!(
            "📝 {text}\n🗓 {}\n\n{}",
            lang.date(date.and_time(time(0, 0)))
                .split(", ")
                .next()
                .unwrap_or_default(),
            lang.text("hour.pick")
        ),
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

/// Step 4: minute, in 5-minute steps.
pub fn minutes_lang(
    subject: Subject,
    text: &str,
    date: NaiveDate,
    hour: u32,
    now: NaiveDateTime,
    lang: Lang,
) -> Panel {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = (0..60)
        .step_by(5)
        .collect::<Vec<u32>>()
        .chunks(4)
        .map(|chunk| {
            chunk
                .iter()
                .map(|&m| {
                    let at = date.and_time(time(hour, m));
                    if at <= now {
                        button("·", Action::Noop)
                    } else {
                        button(format!("{hour:02}:{m:02}"), Action::At(subject, at))
                    }
                })
                .collect()
        })
        .collect();
    rows.push(back_or_cancel(lang, subject, Action::Day(subject, date)));

    Panel {
        text: format!(
            "📝 {text}\n🗓 {}\n\n{}",
            lang.date(date.and_time(time(0, 0)))
                .split(", ")
                .next()
                .unwrap_or_default(),
            lang.text("minute.pick")
        ),
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

/// Final step: how often. Tapping an option saves the reminder.
pub fn repeat_lang(subject: Subject, text: &str, at: NaiveDateTime, lang: Lang) -> Panel {
    repeat_inner(subject, text, at, None, lang)
}

pub fn repeat_time_lang(
    subject: Subject,
    text: &str,
    at: ReminderTime,
    tz: UserTz,
    lang: Lang,
) -> Panel {
    repeat_inner(
        subject,
        text,
        at.local(tz),
        match at {
            ReminderTime::Instant(at) => Some(at),
            ReminderTime::Local(_) => None,
        },
        lang,
    )
}

fn repeat_inner(
    subject: Subject,
    text: &str,
    at: NaiveDateTime,
    instant: Option<DateTime<Utc>>,
    lang: Lang,
) -> Panel {
    let option = |label: String, r: Recurrence| {
        button(
            label,
            match instant {
                Some(at) => Action::SaveInstant(subject, at, r),
                None => Action::Save(subject, at, r),
            },
        )
    };
    let mut rows = vec![
        vec![
            option(lang.text("repeat.once"), Recurrence::Once),
            option(lang.text("repeat.daily"), Recurrence::Daily),
            option(lang.text("repeat.weekdays"), Recurrence::Weekdays),
        ],
        vec![
            option(
                lang.render(
                    "repeat.weekly",
                    &[(
                        "day",
                        if lang == Lang::En {
                            at.format("%a").to_string()
                        } else {
                            lang.text(&format!("weekday.{}", at.weekday().num_days_from_monday()))
                        },
                    )],
                ),
                Recurrence::Weekly,
            ),
            option(
                lang.render("repeat.monthly", &[("day", at.day().to_string())]),
                Recurrence::Monthly,
            ),
            option(
                lang.render(
                    "repeat.yearly",
                    &[(
                        "date",
                        if lang == Lang::En {
                            at.format("%-d %b").to_string()
                        } else {
                            at.format("%d.%m").to_string()
                        },
                    )],
                ),
                Recurrence::Yearly,
            ),
        ],
    ];
    let back = match subject {
        Subject::Draft(_) => Action::When(subject),
        Subject::Reminder(id) => Action::Manage(id),
    };
    rows.push(back_or_cancel(lang, subject, back));

    Panel {
        text: format!(
            "📝 {text}\n🗓 {}\n\n{}",
            lang.date(at),
            lang.text("repeat.ask")
        ),
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

fn summary(reminder: &Reminder, user: &User, now: DateTime<Utc>, lang: Lang) -> String {
    let local = user.tz.local(reminder.due_at);
    format!(
        "📝 {}\n🗓 {} ({})\n🔁 {}",
        reminder.description,
        lang.date(local),
        relative_lang(now, reminder.due_at, lang),
        lang.recurrence(reminder.recurrence, reminder.anchor_local.unwrap_or(local))
    )
}

pub fn saved_lang(
    reminder: &Reminder,
    user: &User,
    now: DateTime<Utc>,
    heading: &str,
    lang: Lang,
) -> Panel {
    Panel {
        text: format!("{heading}\n\n{}", summary(reminder, user, now, lang)),
        keyboard: InlineKeyboardMarkup::new([[
            button(lang.text("button.edit"), Action::Manage(reminder.id)),
            button(lang.text("button.undo"), Action::Delete(reminder.id)),
        ]]),
    }
}

pub fn manage_lang(reminder: &Reminder, user: &User, now: DateTime<Utc>, lang: Lang) -> Panel {
    let local = user.tz.local(reminder.due_at);
    let subject = Subject::Reminder(reminder.id);
    Panel {
        text: summary(reminder, user, now, lang),
        keyboard: InlineKeyboardMarkup::new([
            vec![
                button(lang.text("button.change_time"), Action::When(subject)),
                button(
                    lang.text("button.change_repeat"),
                    Action::At(subject, local),
                ),
            ],
            vec![
                button(lang.text("button.delete"), Action::Delete(reminder.id)),
                button(lang.text("button.all_reminders"), Action::List),
            ],
        ]),
    }
}

pub fn list_lang(reminders: &[Reminder], user: &User, lang: Lang) -> Panel {
    list_page_lang(reminders, user, 0, lang)
}

/// Stateless pages keep old `list` callbacks valid and never expose another user's ids:
/// callers supply only reminders already loaded through the owner-scoped service.
pub fn list_page_lang(reminders: &[Reminder], user: &User, page: usize, lang: Lang) -> Panel {
    if reminders.is_empty() {
        return Panel {
            text: lang.text("list.empty"),
            keyboard: InlineKeyboardMarkup::default(),
        };
    }
    let last_page = (reminders.len() - 1) / LIST_PAGE_SIZE;
    let page = page.min(last_page);
    let start = page * LIST_PAGE_SIZE;
    let mut text = lang.render("list.heading", &[("timezone", user.tz.to_string())]);
    let mut rows = Vec::new();
    for (offset, reminder) in reminders
        .iter()
        .skip(start)
        .take(LIST_PAGE_SIZE)
        .enumerate()
    {
        let index = start + offset + 1;
        let local = user.tz.local(reminder.due_at);
        text.push_str(&format!(
            "\n{index}. {}\n    {} · {}",
            truncate_units(&reminder.description, LIST_PREVIEW_UNITS),
            lang.date(local),
            lang.recurrence(reminder.recurrence, reminder.anchor_local.unwrap_or(local))
        ));
        rows.push(vec![button(
            format!("{index}. {}", truncate(&reminder.description, 40)),
            Action::Manage(reminder.id),
        )]);
    }
    if last_page > 0 {
        text.push_str(&lang.render(
            "list.page",
            &[
                ("page", (page + 1).to_string()),
                ("pages", (last_page + 1).to_string()),
            ],
        ));
        let mut navigation = Vec::new();
        if page > 0 {
            navigation.push(button(
                lang.text("list.previous"),
                Action::ListPage(page - 1),
            ));
        }
        if page < last_page {
            navigation.push(button(lang.text("list.next"), Action::ListPage(page + 1)));
        }
        rows.push(navigation);
    }
    text.push_str(&lang.text("list.hint"));
    debug_assert!(
        text.encode_utf16().count() <= 4096,
        "list page exceeds Telegram's message budget"
    );
    Panel {
        text,
        keyboard: InlineKeyboardMarkup::new(rows),
    }
}

fn truncate_units(text: &str, max_units: usize) -> String {
    if text.encode_utf16().count() <= max_units {
        return text.to_owned();
    }
    let mut units = 0;
    let mut truncated = String::new();
    for ch in text.chars() {
        if units + ch.len_utf16() >= max_units {
            break;
        }
        units += ch.len_utf16();
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

fn time(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).expect("valid time")
}

fn truncate_to_minute(at: NaiveDateTime) -> NaiveDateTime {
    at.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(at)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max_chars - 1).collect::<String>())
    }
}

/// Localized labels for the reminder acknowledgement controls.
pub fn nag_keyboard_lang(nagging_id: i64, lang: crate::i18n::Lang) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new([
        vec![button(lang.text("done"), Action::Done(nagging_id))],
        vec![
            button(
                lang.text("snooze.ten"),
                Action::Snooze(nagging_id, Snooze::TenMinutes),
            ),
            button(
                lang.text("snooze.hour"),
                Action::Snooze(nagging_id, Snooze::OneHour),
            ),
            button(
                lang.text("snooze.tomorrow"),
                Action::Snooze(nagging_id, Snooze::TomorrowMorning),
            ),
        ],
    ])
}

pub fn completion_keyboard_lang(completion_id: i64, lang: Lang) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new([[button(
        lang.text("button.undo_completion"),
        Action::UndoCompletion(completion_id),
    )]])
}

/// A legacy check has no journal entry to restore. New checks refer to a durable
/// completion even after the delivery queue removes its successfully sent row.
pub fn confirmation_keyboard_lang(completion_id: Option<i64>, lang: Lang) -> InlineKeyboardMarkup {
    let mut rows = vec![vec![InlineKeyboardButton::callback(
        lang.text("check.ack"),
        "check:ok",
    )]];
    if let Some(id) = completion_id {
        rows.push(vec![button(
            lang.text("check.retry"),
            Action::UndoCompletion(id),
        )]);
    }
    InlineKeyboardMarkup::new(rows)
}

#[allow(dead_code)]
pub fn relative(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    relative_lang(from, to, Lang::En)
}
#[allow(dead_code)]
pub fn when(subject: Subject, text: &str, now: NaiveDateTime) -> Panel {
    when_lang(subject, text, now, Lang::En)
}
#[allow(dead_code)]
pub fn calendar(subject: Subject, text: &str, year: i32, month: u32, today: NaiveDate) -> Panel {
    calendar_lang(subject, text, year, month, today, Lang::En)
}
#[allow(dead_code)]
pub fn hours(subject: Subject, text: &str, date: NaiveDate, now: NaiveDateTime) -> Panel {
    hours_lang(subject, text, date, now, Lang::En)
}
#[allow(dead_code)]
pub fn minutes(
    subject: Subject,
    text: &str,
    date: NaiveDate,
    hour: u32,
    now: NaiveDateTime,
) -> Panel {
    minutes_lang(subject, text, date, hour, now, Lang::En)
}
#[allow(dead_code)]
pub fn repeat(subject: Subject, text: &str, at: NaiveDateTime) -> Panel {
    repeat_lang(subject, text, at, Lang::En)
}
#[allow(dead_code)]
pub fn saved(reminder: &Reminder, user: &User, now: DateTime<Utc>, heading: &str) -> Panel {
    saved_lang(reminder, user, now, heading, Lang::En)
}
#[allow(dead_code)]
pub fn manage(reminder: &Reminder, user: &User, now: DateTime<Utc>) -> Panel {
    manage_lang(reminder, user, now, Lang::En)
}
#[allow(dead_code)]
pub fn list(reminders: &[Reminder], user: &User) -> Panel {
    list_lang(reminders, user, Lang::En)
}

#[cfg(test)]
mod tests {
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
                teloxide::types::InlineKeyboardButtonKind::CallbackData(data) => {
                    Action::parse(data)
                }
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
        assert!(
            actions(&panel).contains(&Action::At(Subject::Draft(1), local(2026, 10, 14, 20, 0)))
        );
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
}
