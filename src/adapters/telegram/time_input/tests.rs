use super::*;
use chrono::{NaiveDate, NaiveDateTime};
use chrono_tz::Tz;

const SGT: UserTz = UserTz::Named(Tz::Asia__Singapore);

fn parse_local(text: &str, tz: UserTz, now: DateTime<Utc>) -> Option<NaiveDateTime> {
    parse(text, tz, now).map(|at| at.local(tz))
}

fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, 0)
        .unwrap()
}

/// Tuesday 13 October 2026, 10:30 in Singapore.
fn now() -> DateTime<Utc> {
    SGT.utc(local(2026, 10, 13, 10, 30))
}

#[test]
fn parses_common_inputs() {
    for (input, expected) in [
        ("fri 6pm", local(2026, 10, 16, 18, 0)),
        ("Friday", local(2026, 10, 16, 9, 0)),
        ("tomorrow", local(2026, 10, 14, 9, 0)),
        ("tmr 7:30am", local(2026, 10, 14, 7, 30)),
        ("in 2 hours", local(2026, 10, 13, 12, 30)),
        ("30 min", local(2026, 10, 13, 11, 0)),
        ("25/12 9:00", local(2026, 12, 25, 9, 0)),
        ("25 dec", local(2026, 12, 25, 9, 0)),
        ("21:30", local(2026, 10, 13, 21, 30)),
        ("9am", local(2026, 10, 14, 9, 0)),
        ("at noon", local(2026, 10, 13, 12, 0)),
        ("tonight", local(2026, 10, 13, 20, 0)),
        ("1/11/2026 07:15", local(2026, 11, 1, 7, 15)),
    ] {
        assert_eq!(parse_local(input, SGT, now()), Some(expected), "{input}");
    }
}

#[test]
fn parses_russian_time_expressions() {
    for (input, expected) in [
        ("завтра", local(2026, 10, 14, 9, 0)),
        ("завтра 07:30", local(2026, 10, 14, 7, 30)),
        ("через 20 минут", local(2026, 10, 13, 10, 50)),
        ("через 2 часа", local(2026, 10, 13, 12, 30)),
        ("пятницу 18:00", local(2026, 10, 16, 18, 0)),
        ("сегодня вечером", local(2026, 10, 13, 20, 0)),
    ] {
        assert_eq!(parse_local(input, SGT, now()), Some(expected), "{input}");
    }
}

#[test]
fn only_bare_clocks_roll_forward_to_tomorrow() {
    for input in ["9am", "9 am", "09:00", "в 09:00", "at 9am"] {
        assert_eq!(
            parse_local(input, SGT, now()),
            Some(local(2026, 10, 14, 9, 0)),
            "{input}"
        );
    }
    assert_eq!(
        parse_local("10:30", SGT, now()),
        Some(local(2026, 10, 14, 10, 30))
    );
    for input in [
        "today 09:00",
        "сегодня 09:00",
        "13/10/2026 09:00",
        "12/10/2026 11:00",
        "yesterday 11:00",
        "today",
    ] {
        assert_eq!(parse_local(input, SGT, now()), None, "{input}");
    }
}

#[test]
fn rejects_nonpositive_durations() {
    for input in [
        "0 minutes",
        "in 0 hours",
        "in -1 hours",
        "-30 minutes",
        "через 0 минут",
        "через -1 час",
    ] {
        assert_eq!(parse_local(input, SGT, now()), None, "{input}");
    }
}

#[test]
fn day_after_tomorrow_accepts_an_optional_clock() {
    for (input, expected) in [
        ("послезавтра", local(2026, 10, 15, 9, 0)),
        ("Послезавтра 18:30", local(2026, 10, 15, 18, 30)),
        ("послезавтра в 18:30", local(2026, 10, 15, 18, 30)),
        ("послезавтра 7am", local(2026, 10, 15, 7, 0)),
        ("послезавтра утром", local(2026, 10, 15, 9, 0)),
    ] {
        assert_eq!(parse_local(input, SGT, now()), Some(expected), "{input}");
    }
    for input in ["послезавтра купить молоко", "послезавтра 25:00"]
    {
        assert_eq!(parse_local(input, SGT, now()), None, "{input}");
    }
}

#[test]
fn day_after_tomorrow_uses_the_users_local_calendar() {
    let near_midnight = SGT.utc(local(2026, 10, 14, 0, 30));
    assert_eq!(
        parse_local("послезавтра", SGT, near_midnight),
        Some(local(2026, 10, 16, 9, 0))
    );
    let london = UserTz::Named(Tz::Europe__London);
    let before_dst = london.utc(local(2026, 3, 28, 10, 30));
    assert_eq!(
        parse_local("послезавтра 10:30", london, before_dst),
        Some(local(2026, 3, 30, 10, 30))
    );
}

#[test]
fn rejects_non_times() {
    for input in [
        "call mum",
        "",
        "buy milk and eggs tomorrow",
        "1/1/2020 08:00",
    ] {
        assert_eq!(parse_local(input, SGT, now()), None, "{input}");
    }
}

#[test]
fn midnight_means_zero_hours_on_the_next_day() {
    for text in ["midnight", "полночь", "в полночь"] {
        assert_eq!(
            parse_local(text, SGT, now()),
            Some(local(2026, 10, 14, 0, 0)),
            "{text}"
        );
    }
}
#[test]
fn elapsed_intervals_preserve_the_future_instant_across_clock_changes() {
    let london = UserTz::Named(Tz::Europe__London);
    let before_fold = local(2026, 10, 25, 0, 30).and_utc();
    for (input, minutes) in [("in 30 minutes", 30), ("через 1 час", 60)] {
        assert_eq!(
            super::parse(input, london, before_fold),
            Some(ReminderTime::Instant(
                before_fold + Duration::minutes(minutes)
            )),
            "{input}"
        );
    }
    let before_gap = local(2026, 3, 29, 0, 45).and_utc();
    assert_eq!(
        super::parse("30 minutes", london, before_gap),
        Some(ReminderTime::Instant(before_gap + Duration::minutes(30)))
    );
}
