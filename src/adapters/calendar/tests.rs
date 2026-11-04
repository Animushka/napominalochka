use super::*;
use crate::domain::UtcOffset;
use crate::domain::{ReminderId, UserId};
use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;

fn reminder(id: i64, description: &str, due_at: DateTime<Utc>, recurrence: Recurrence) -> Reminder {
    Reminder {
        id: ReminderId(id),
        user_id: UserId(1),
        description: description.into(),
        due_at,
        recurrence,
        anchor_local: None,
        settings: crate::domain::ReminderSettings::default(),
    }
}

fn unfold(ics: &str) -> String {
    ics.replace("\r\n ", "")
}

#[test]
fn builds_a_feed() {
    let london = UserTz::Named(Tz::Europe__London);
    let now = Utc.with_ymd_and_hms(2026, 10, 13, 9, 0, 0).unwrap();
    let due = london.utc(
        NaiveDate::from_ymd_opt(2026, 10, 14)
            .unwrap()
            .and_hms_opt(8, 30, 0)
            .unwrap(),
    );
    let ics = feed(
        &[
            reminder(
                7,
                "Pills; vitamins, water\nand stretch",
                due,
                Recurrence::Weekdays,
            ),
            reminder(8, "Call mum", due, Recurrence::Once),
        ],
        london,
        now,
    );

    assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(ics.ends_with("END:VCALENDAR\r\n"));
    assert!(ics.contains("UID:reminder-7@extremelynaggybot\r\n"));
    assert!(ics.contains("DTSTART;TZID=Europe/London:20261014T083000\r\n"));
    assert!(ics.contains("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\n"));
    assert!(ics.contains("SUMMARY:Pills\\; vitamins\\, water\\nand stretch\r\n"));
    assert!(ics.contains("DTSTAMP:20261013T090000Z\r\n"));
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2);
    assert_eq!(ics.matches("BEGIN:VALARM").count(), 6);
    assert_eq!(
        ics.matches("RRULE").count(),
        1,
        "one-off reminders do not repeat"
    );
}

#[test]
fn builds_feed_links() {
    let base = Url::parse("https://reminders.example.com").unwrap();
    let url = feed_url(&base, "abc").unwrap();
    assert_eq!(
        url.as_str(),
        "https://reminders.example.com/calendar/abc.ics"
    );
    assert_eq!(
        webcal(&url),
        "webcal://reminders.example.com/calendar/abc.ics"
    );
}

#[test]
fn fixed_offset_one_off_reminders_get_utc_times() {
    let tz = UserTz::Fixed(UtcOffset::from_minutes(480).unwrap());
    let due = Utc.with_ymd_and_hms(2026, 10, 14, 1, 0, 0).unwrap();
    let ics = feed(&[reminder(1, "x", due, Recurrence::Once)], tz, due);
    assert!(ics.contains("DTSTART:20261014T010000Z\r\n"));
    assert!(!ics.contains("X-WR-TIMEZONE"));
    assert!(!ics.contains("VTIMEZONE"));
}

fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(h, min, 0)
        .unwrap()
}

#[test]
fn monthly_rules_recover_the_original_day_after_a_short_month() {
    let tz = UserTz::Named(Tz::Europe__London);
    for (day, days) in [(29, "28,29"), (30, "28,29,30"), (31, "28,29,30,31")] {
        let due = tz.utc(local(2027, 2, 28, 9, 0));
        let mut r = reminder(1, "monthly", due, Recurrence::Monthly);
        r.anchor_local = Some(local(2027, 1, day, 9, 0));
        let ics = unfold(&feed(&[r], tz, due));
        assert!(ics.contains("DTSTART;TZID=Europe/London:20270228T090000\r\n"));
        assert!(ics.contains(&format!(
            "RRULE:FREQ=MONTHLY;BYMONTHDAY={days};BYSETPOS=-1\r\n"
        )));
        assert!(!ics.contains("UNTIL="));
        assert!(!ics.contains("COUNT="));
    }
    let due = tz.utc(local(2027, 2, 15, 9, 0));
    let ics = feed(&[reminder(1, "monthly", due, Recurrence::Monthly)], tz, due);
    assert!(ics.contains("RRULE:FREQ=MONTHLY;BYMONTHDAY=15\r\n"));
}

#[test]
fn leap_day_rule_clamps_only_nonleap_years() {
    let tz = UserTz::Named(Tz::Europe__London);
    let due = tz.utc(local(2029, 2, 28, 9, 0));
    let mut r = reminder(1, "birthday", due, Recurrence::Yearly);
    r.anchor_local = Some(local(2028, 2, 29, 9, 0));
    let ics = feed(&[r], tz, due);
    assert!(ics.contains("DTSTART;TZID=Europe/London:20290228T090000\r\n"));
    assert!(ics.contains("RRULE:FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=28,29;BYSETPOS=-1\r\n"));
}

#[test]
fn shifted_first_occurrence_does_not_change_the_recurring_clock() {
    let tz = UserTz::Named(Tz::Europe__London);
    let due = tz.utc(local(2026, 3, 29, 1, 30));
    assert_eq!(tz.local(due), local(2026, 3, 29, 2, 30));
    let mut r = reminder(1, "daily", due, Recurrence::Daily);
    r.anchor_local = Some(local(2026, 3, 28, 1, 30));
    let ics = feed(&[r], tz, due);
    assert!(ics.contains("DTSTART;TZID=Europe/London:20260329T013000\r\n"));
    assert!(!ics.contains("EXDATE;TZID=Europe/London:20260329T013000\r\n"));
    assert_eq!(ics.matches("RDATE:20260329T013000Z\r\n").count(), 1);
    assert!(ics.contains("RRULE:FREQ=DAILY\r\n"));
}

#[test]
fn future_dst_gaps_are_added_without_bounding_normal_recurrence() {
    let tz = UserTz::Named(Tz::Europe__London);
    let due = tz.utc(local(2026, 10, 28, 1, 30));
    let mut r = reminder(1, "daily", due, Recurrence::Daily);
    r.anchor_local = Some(local(2026, 10, 28, 1, 30));
    let ics = feed(&[r], tz, due);
    assert!(ics.contains("DTSTART;TZID=Europe/London:20261028T013000\r\n"));
    assert!(ics.contains("RDATE:20270328T013000Z\r\n"));
    assert!(ics.contains("RDATE:20280326T013000Z\r\n"));
    assert_eq!(ics.matches("RDATE:").count(), 2);
    assert!(!ics.contains("EXDATE"));
    assert!(!ics.contains("UNTIL="));
    assert!(!ics.contains("COUNT="));

    // London's gaps are Sundays, so a weekday schedule must not gain Sunday alarms.
    let weekdays = feed(&[reminder(2, "work", due, Recurrence::Weekdays)], tz, due);
    assert!(!weekdays.contains("RDATE:"));
}

#[test]
fn fixed_offset_recurrences_keep_local_calendar_dates() {
    let tz = UserTz::Fixed(UtcOffset::from_minutes(480).unwrap());
    // Monday locally, but still Sunday in UTC: BYDAY must use the local date.
    let due = tz.utc(local(2026, 1, 5, 2, 30));
    let ics = feed(&[reminder(1, "work", due, Recurrence::Weekdays)], tz, due);
    assert!(ics.contains("TZID:FixedUTC+0800\r\n"));
    assert!(ics.contains("TZOFFSETFROM:+0800\r\n"));
    assert!(ics.contains("TZOFFSETTO:+0800\r\n"));
    assert!(ics.contains("DTSTART;TZID=FixedUTC+0800:20260105T023000\r\n"));
    assert!(ics.contains("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\n"));
    assert!(!ics.contains("RDATE:"));
}

#[test]
fn a_month_end_dst_gap_does_not_create_an_alarm_on_the_previous_day() {
    let tz = UserTz::Named(Tz::Europe__London);
    let due = tz.utc(local(2024, 2, 29, 1, 30));
    let mut r = reminder(1, "monthly", due, Recurrence::Monthly);
    r.anchor_local = Some(local(2024, 1, 31, 1, 30));
    let ics = feed(&[r], tz, due);
    assert!(ics.contains("RRULE:FREQ=MONTHLY;BYMONTHDAY=28,29,30,31;BYSETPOS=-1\r\n"));
    assert!(ics.contains("RDATE:20240331T013000Z\r\n"));
    assert!(ics.contains("EXDATE;TZID=Europe/London:20240330T013000\r\n"));
    assert!(!ics.contains("RDATE:20240330"));
}

#[test]
fn a_gap_crossing_midnight_keeps_the_original_local_date_and_clock() {
    let tz = UserTz::Named(Tz::America__Nuuk);
    let anchor = local(2026, 3, 28, 23, 30);
    let due = tz.utc(anchor);
    assert_eq!(tz.local(due), local(2026, 3, 29, 0, 30));
    let mut r = reminder(1, "daily", due, Recurrence::Daily);
    r.anchor_local = Some(anchor);
    let ics = feed(&[r], tz, due);
    assert!(ics.contains("DTSTART;TZID=America/Nuuk:20260328T233000\r\n"));
    assert!(!ics.contains("EXDATE;TZID=America/Nuuk:20260328T233000\r\n"));
    assert!(ics.contains(&format!("RDATE:{}\r\n", utc(due))));
    assert!(ics.contains("RDATE:20270328T013000Z\r\n"));
}

#[test]
fn folds_long_lines_on_character_boundaries() {
    let long = "提醒".repeat(40) + &"a".repeat(100);
    let mut ics = Ics::default();
    ics.line(&format!("SUMMARY:{long}"));
    for physical in ics.0.split("\r\n").filter(|l| !l.is_empty()) {
        assert!(
            physical.len() <= 75,
            "{} octets: {physical}",
            physical.len()
        );
    }
    assert_eq!(unfold(&ics.0), format!("SUMMARY:{long}\r\n"));
}

#[test]
fn a_changed_timezone_preserves_the_real_first_instant_without_excluding_it() {
    let tz = UserTz::Fixed(UtcOffset::from_minutes(180).unwrap());
    let due = tz.utc(local(2026, 10, 28, 12, 0));
    let mut r = reminder(1, "daily", due, Recurrence::Daily);
    r.anchor_local = Some(local(2026, 10, 27, 9, 0));
    let ics = feed(&[r], tz, due);
    assert!(ics.contains("DTSTART;TZID=FixedUTC+0300:20261028T090000\r\n"));
    assert!(ics.contains("EXDATE;TZID=FixedUTC+0300:20261028T090000\r\n"));
    assert!(ics.contains("RDATE:20261028T090000Z\r\n"));
    assert_ne!(tz.utc(local(2026, 10, 28, 9, 0)), due);
}
