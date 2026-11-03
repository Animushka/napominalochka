use super::recurrence::ordinal;
use super::*;
use chrono::NaiveDate;
use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::Tz;

fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, 0)
        .unwrap()
}

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    local(y, mo, d, h, mi).and_utc()
}

const UTC: UserTz = UserTz::Fixed(UtcOffset::from_minutes(0).unwrap());

#[test]
fn parses_offsets() {
    for (input, minutes) in [
        ("8", 480),
        ("+8", 480),
        ("-5", -300),
        ("+5:30", 330),
        ("+0545", 345),
        ("UTC-03:30", -210),
        ("+14", 840),
        ("-12", -720),
    ] {
        assert_eq!(
            input.parse::<UtcOffset>().unwrap().minutes(),
            minutes,
            "{input}"
        );
    }
    for input in ["", "abc", "+15", "-13", "+5:60", "8.5"] {
        assert!(input.parse::<UtcOffset>().is_err(), "{input}");
    }
}

#[test]
fn parses_timezones() {
    assert_eq!(
        UserTz::parse("+8"),
        Ok(UserTz::Fixed(UtcOffset::from_minutes(480).unwrap()))
    );
    assert_eq!(
        UserTz::parse("asia/singapore"),
        Ok(UserTz::Named(Tz::Asia__Singapore))
    );
    assert_eq!(
        UserTz::parse("London"),
        Ok(UserTz::Named(Tz::Europe__London))
    );
    assert_eq!(
        UserTz::parse("new york"),
        Ok(UserTz::Named(Tz::America__New_York))
    );
    assert_eq!(
        UserTz::parse("Singapore"),
        Ok(UserTz::Named(Tz::Asia__Singapore))
    );
    assert_eq!(UserTz::parse("UTC"), Ok(UserTz::Named(Tz::Etc__UTC)));
    assert_eq!(UserTz::parse("Atlantis"), Err(InputError::InvalidTimezone));
}

#[test]
fn finds_timezone_from_location() {
    assert_eq!(
        UserTz::from_location(1.3521, 103.8198),
        Some(UserTz::Named(Tz::Asia__Singapore))
    );
    assert_eq!(
        UserTz::from_location(51.5072, -0.1276),
        Some(UserTz::Named(Tz::Europe__London))
    );
}

#[test]
fn next_after_keeps_time_of_day() {
    let due = utc(2026, 1, 1, 8, 0); // Thursday
    let now = utc(2026, 1, 10, 9, 30); // Saturday
    assert_eq!(
        Recurrence::Daily.next_after(due, now, UTC),
        Some(utc(2026, 1, 11, 8, 0))
    );
    assert_eq!(
        Recurrence::Weekdays.next_after(due, now, UTC),
        Some(utc(2026, 1, 12, 8, 0))
    );
    assert_eq!(
        Recurrence::Weekly.next_after(due, now, UTC),
        Some(utc(2026, 1, 15, 8, 0))
    );
    assert_eq!(
        Recurrence::Monthly.next_after(due, now, UTC),
        Some(utc(2026, 2, 1, 8, 0))
    );
    assert_eq!(
        Recurrence::Yearly.next_after(due, now, UTC),
        Some(utc(2027, 1, 1, 8, 0))
    );
    assert_eq!(Recurrence::Once.next_after(due, now, UTC), None);
    assert_eq!(
        Recurrence::Daily.next_after(due, due, UTC),
        Some(utc(2026, 1, 2, 8, 0))
    );
}

#[test]
fn daily_reminder_survives_daylight_saving() {
    let london = UserTz::Named(Tz::Europe__London);
    // 08:00 GMT on 28 March 2026; clocks go forward on 29 March.
    let due = london.utc(local(2026, 3, 28, 8, 0));
    assert_eq!(due, utc(2026, 3, 28, 8, 0));
    let next = Recurrence::Daily.next_after(due, due, london).unwrap();
    assert_eq!(london.local(next), local(2026, 3, 29, 8, 0));
    assert_eq!(next, utc(2026, 3, 29, 7, 0));
}

#[test]
fn non_existent_local_time_moves_forward() {
    let london = UserTz::Named(Tz::Europe__London);
    // 01:30 on 29 March 2026 does not exist in London.
    assert_eq!(
        london.utc(local(2026, 3, 29, 1, 30)),
        utc(2026, 3, 29, 1, 30)
    );
}

#[test]
fn monthly_and_yearly_recurrences_restore_the_original_calendar_day() {
    let month_anchor = local(2027, 1, 31, 9, 0);
    let january = UTC.utc(month_anchor);
    let february = Recurrence::Monthly
        .next_after_anchored(january, january, UTC, month_anchor)
        .unwrap();
    assert_eq!(february, utc(2027, 2, 28, 9, 0));
    assert_eq!(
        Recurrence::Monthly.next_after_anchored(february, february, UTC, month_anchor),
        Some(utc(2027, 3, 31, 9, 0))
    );

    let leap_anchor = local(2028, 2, 29, 9, 0);
    let mut due = UTC.utc(leap_anchor);
    for year in 2029..=2032 {
        due = Recurrence::Yearly
            .next_after_anchored(due, due, UTC, leap_anchor)
            .unwrap();
        assert_eq!(due, utc(year, 2, if year == 2032 { 29 } else { 28 }, 9, 0));
    }
}

#[test]
fn weekday_first_occurrences_skip_future_weekends() {
    let now = utc(2026, 10, 16, 12, 0); // Friday
    for day in [17, 18] {
        assert_eq!(
            Recurrence::Weekdays.first_due(local(2026, 10, day, 9, 0), UTC, now),
            Some(utc(2026, 10, 19, 9, 0))
        );
    }
}

#[test]
fn dst_gap_shifts_only_that_occurrence_and_keeps_the_next_original_clock() {
    let london = UserTz::Named(Tz::Europe__London);
    let anchor = local(2026, 3, 28, 1, 30);
    let before = london.utc(anchor);
    let gap = Recurrence::Daily
        .next_after_anchored(before, before, london, anchor)
        .unwrap();
    assert_eq!(london.local(gap), local(2026, 3, 29, 2, 30));
    let after = Recurrence::Daily
        .next_after_anchored(gap, gap, london, anchor)
        .unwrap();
    assert_eq!(london.local(after), local(2026, 3, 30, 1, 30));

    let weekly_anchor = local(2026, 3, 22, 1, 30);
    let before = london.utc(weekly_anchor);
    let gap = Recurrence::Weekly
        .next_after_anchored(before, before, london, weekly_anchor)
        .unwrap();
    assert_eq!(london.local(gap), local(2026, 3, 29, 2, 30));
    let after = Recurrence::Weekly
        .next_after_anchored(gap, gap, london, weekly_anchor)
        .unwrap();
    assert_eq!(london.local(after), local(2026, 4, 5, 1, 30));
}

#[test]
fn describes_recurrence() {
    let first = local(2026, 10, 13, 9, 0); // Tuesday
    assert_eq!(Recurrence::Weekly.describe(first), "every Tue");
    assert_eq!(Recurrence::Monthly.describe(first), "monthly on the 13th");
    assert_eq!(Recurrence::Yearly.describe(first), "every year on 13 Oct");
    assert_eq!(ordinal(1), "1st");
    assert_eq!(ordinal(22), "22nd");
    assert_eq!(ordinal(11), "11th");
}

#[test]
fn recurrence_round_trips_through_db_value() {
    for r in Recurrence::ALL {
        assert_eq!(Recurrence::from_code(r.code()), Some(r));
    }
}
#[test]
fn local_gaps_use_the_actual_offset_change_and_checked_dates() {
    let half_hour = UserTz::Named(Tz::Australia__Lord_Howe);
    let shifted = half_hour.utc(local(2026, 10, 4, 2, 15));
    assert_eq!(half_hour.local(shifted), local(2026, 10, 4, 2, 45));

    let skipped_date = UserTz::Named(Tz::Pacific__Apia);
    let shifted = skipped_date.utc(local(2011, 12, 30, 12, 0));
    assert_eq!(skipped_date.local(shifted), local(2011, 12, 31, 12, 0));
    for offset in ["-12", "+14"] {
        let tz = UserTz::parse(offset).unwrap();
        let _ = tz.utc(NaiveDateTime::MIN);
        let _ = tz.utc(NaiveDateTime::MAX);
    }
}
