//! iCalendar (RFC 5545) feed of a user's reminders, for subscribing in a phone's calendar
//! app. Alarms then fire from the phone itself, even without a connection.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Duration, Months, NaiveDateTime, TimeZone, Utc};
use url::Url;

use crate::i18n::Lang;
use crate::model::{Recurrence, Reminder, UserTz};

/// Alarms per occurrence, in minutes after the start: a little naggy, like the bot.
const ALARM_OFFSETS_MINUTES: [u32; 3] = [0, 5, 10];
/// Suggested refresh interval for calendar apps that honour it.
const REFRESH: &str = "PT15M";

/// The public feed address for a token, under the Mini App's host.
pub fn feed_url(base: &Url, token: &str) -> Option<Url> {
    base.join(&format!("/calendar/{token}.ics")).ok()
}

/// `webcal://` form of a feed URL, which calendar apps open as a subscription.
pub fn webcal(url: &Url) -> String {
    format!("webcal://{}", url.as_str().trim_start_matches("https://"))
}

#[allow(dead_code)]
pub fn instructions(url: &Url) -> String {
    instructions_lang(url, Lang::En)
}

pub fn instructions_lang(url: &Url, lang: Lang) -> String {
    lang.render(
        "calendar.instructions",
        &[("webcal", webcal(url)), ("url", url.to_string())],
    )
}

#[cfg(test)]
pub fn feed(reminders: &[Reminder], tz: UserTz, now: DateTime<Utc>) -> String {
    feed_lang(reminders, tz, now, Lang::En)
}

pub fn feed_lang(reminders: &[Reminder], tz: UserTz, now: DateTime<Utc>, lang: Lang) -> String {
    let mut ics = Ics::default();
    ics.line("BEGIN:VCALENDAR");
    ics.line("VERSION:2.0");
    ics.line("PRODID:-//Napominalochka//Reminders//RU-EN");
    ics.line("CALSCALE:GREGORIAN");
    ics.line("METHOD:PUBLISH");
    ics.line(&format!("X-WR-CALNAME:{}", lang.text("calendar.name")));
    if let Some(name) = tz.name() {
        ics.line(&format!("X-WR-TIMEZONE:{name}"));
    }
    ics.line(&format!("REFRESH-INTERVAL;VALUE=DURATION:{REFRESH}"));
    ics.line(&format!("X-PUBLISHED-TTL:{REFRESH}"));
    // A UTC RRULE would apply BYDAY/BYMONTHDAY to the UTC date, which may differ from
    // the user's local date. Give fixed-offset recurrences a constant local timezone.
    let fixed_timezone = match tz {
        UserTz::Fixed(offset) if reminders.iter().any(|r| r.recurrence != Recurrence::Once) => {
            let offset = offset.fixed().to_string().replace(':', "");
            let id = format!("FixedUTC{offset}");
            ics.line("BEGIN:VTIMEZONE");
            ics.line(&format!("TZID:{id}"));
            ics.line("BEGIN:STANDARD");
            ics.line("DTSTART:19700101T000000");
            ics.line(&format!("TZOFFSETFROM:{offset}"));
            ics.line(&format!("TZOFFSETTO:{offset}"));
            ics.line("END:STANDARD");
            ics.line("END:VTIMEZONE");
            Some(id)
        }
        _ => None,
    };

    for reminder in reminders {
        let summary = escape(&reminder.description);
        ics.line("BEGIN:VEVENT");
        ics.line(&format!("UID:reminder-{}@extremelynaggybot", reminder.id));
        ics.line(&format!("DTSTAMP:{}", utc(now)));
        let local_due = tz.local(reminder.due_at);
        let anchor = reminder.anchor_local.unwrap_or(local_due);
        let recurring = reminder.recurrence != Recurrence::Once;
        let start = if recurring {
            intended_local(reminder.due_at, tz, anchor)
        } else {
            local_due
        };
        let timezone = tz.name().or(if recurring {
            fixed_timezone.as_deref()
        } else {
            None
        });
        date_property(&mut ics, "DTSTART", start, tz, timezone);
        ics.line("DURATION:PT15M");
        if let Some(rule) = rrule(reminder.recurrence, anchor) {
            ics.line(&format!("RRULE:{rule}"));
            let mut additional = BTreeSet::new();
            let mut excluded = BTreeSet::new();
            // Keep the recurring wall-clock anchor even when the first real occurrence
            // was shifted by a DST gap or retained across a timezone change.
            if start != local_due || tz.utc(start) != reminder.due_at {
                // EXDATE wins over RDATE. A gap-adjusted DTSTART already represents
                // the real instant, so excluding it would also remove that RDATE.
                if tz.utc(start) != reminder.due_at {
                    excluded.insert(start);
                }
                additional.insert(reminder.due_at);
            }
            add_gap_occurrences(&mut additional, &mut excluded, reminder, tz, anchor, now);
            for date in excluded {
                date_property(&mut ics, "EXDATE", date, tz, timezone);
            }
            for date in additional {
                ics.line(&format!("RDATE:{}", utc(date)));
            }
        }
        ics.line(&format!("SUMMARY:{summary}"));
        ics.line("TRANSP:TRANSPARENT");
        for minutes in ALARM_OFFSETS_MINUTES {
            ics.line("BEGIN:VALARM");
            ics.line("ACTION:DISPLAY");
            ics.line(&format!("DESCRIPTION:{summary}"));
            ics.line(&format!("TRIGGER:PT{minutes}M"));
            ics.line("END:VALARM");
        }
        ics.line("END:VEVENT");
    }

    ics.line("END:VCALENDAR");
    ics.0
}

fn rrule(recurrence: Recurrence, anchor: NaiveDateTime) -> Option<String> {
    Some(match recurrence {
        Recurrence::Once => return None,
        Recurrence::Daily => "FREQ=DAILY".into(),
        Recurrence::Weekdays => "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".into(),
        Recurrence::Weekly => format!(
            "FREQ=WEEKLY;BYDAY={}",
            ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
                [anchor.weekday().num_days_from_monday() as usize]
        ),
        Recurrence::Monthly if anchor.day() > 28 => format!(
            "FREQ=MONTHLY;BYMONTHDAY={};BYSETPOS=-1",
            (28..=anchor.day())
                .map(|day| day.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ),
        Recurrence::Monthly => format!("FREQ=MONTHLY;BYMONTHDAY={}", anchor.day()),
        Recurrence::Yearly if anchor.month() == 2 && anchor.day() == 29 => {
            "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=28,29;BYSETPOS=-1".into()
        }
        Recurrence::Yearly => format!(
            "FREQ=YEARLY;BYMONTH={};BYMONTHDAY={}",
            anchor.month(),
            anchor.day()
        ),
    })
}

fn date_property(ics: &mut Ics, name: &str, local: NaiveDateTime, tz: UserTz, id: Option<&str>) {
    match id {
        Some(id) => ics.line(&format!(
            "{name};TZID={id}:{}",
            local.format("%Y%m%dT%H%M%S")
        )),
        None => ics.line(&format!("{name}:{}", utc(tz.utc(local)))),
    }
}

fn intended_local(due: DateTime<Utc>, tz: UserTz, anchor: NaiveDateTime) -> NaiveDateTime {
    let local = tz.local(due).date().and_time(anchor.time());
    if tz.utc(local) != due
        && let Some(previous) = local.checked_sub_signed(Duration::days(1))
        && tz.utc(previous) == due
    {
        // Some zones jump forward at 23:00, moving an occurrence into the next date.
        return previous;
    }
    local
}

/// RFC 5545 skips nonexistent times generated by RRULE, while our scheduler shifts them
/// forward. Supply those exceptional instants for two years; ordinary repeats stay unbounded.
fn add_gap_occurrences(
    dates: &mut BTreeSet<DateTime<Utc>>,
    excluded: &mut BTreeSet<NaiveDateTime>,
    reminder: &Reminder,
    tz: UserTz,
    anchor: NaiveDateTime,
    now: DateTime<Utc>,
) {
    let UserTz::Named(zone) = tz else {
        return;
    };
    let Some(until) = now.checked_add_months(Months::new(24)) else {
        return;
    };
    let mut occurrence = reminder
        .recurrence
        .next_after_anchored(reminder.due_at, now, tz, anchor);
    for _ in 0..750 {
        let Some(due) = occurrence.filter(|due| *due <= until) else {
            break;
        };
        let intended = intended_local(due, tz, anchor);
        if zone.from_local_datetime(&intended).earliest().is_none() {
            dates.insert(due);
            let uses_setpos = (reminder.recurrence == Recurrence::Monthly && anchor.day() > 28)
                || (reminder.recurrence == Recurrence::Yearly
                    && anchor.month() == 2
                    && anchor.day() == 29);
            if uses_setpos {
                // After a nonexistent final candidate is removed, BYSETPOS=-1 may
                // select the previous candidate day. It is not a real occurrence.
                if let Some(previous) = (28..intended.day()).rev().find_map(|day| {
                    let candidate = intended.with_day(day)?;
                    zone.from_local_datetime(&candidate)
                        .earliest()
                        .map(|_| candidate)
                }) {
                    excluded.insert(previous);
                }
            }
        }
        occurrence = reminder
            .recurrence
            .next_after_anchored(due, due, tz, anchor);
    }
}

fn utc(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Escapes a TEXT value (RFC 5545 §3.3.11).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Accumulates content lines with CRLF endings, folded at 75 octets without splitting
/// UTF-8 characters (RFC 5545 §3.1).
#[derive(Default)]
struct Ics(String);

impl Ics {
    fn line(&mut self, line: &str) {
        const MAX: usize = 75;
        let mut width = 0;
        for c in line.chars() {
            let len = c.len_utf8();
            if width + len > MAX {
                // Continuation lines start with a space, which counts towards their width.
                self.0.push_str("\r\n ");
                width = 1;
            }
            self.0.push(c);
            width += len;
        }
        self.0.push_str("\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UtcOffset;
    use chrono::{NaiveDate, TimeZone};
    use chrono_tz::Tz;

    fn reminder(
        id: i64,
        description: &str,
        due_at: DateTime<Utc>,
        recurrence: Recurrence,
    ) -> Reminder {
        Reminder {
            id,
            user_id: 1,
            description: description.into(),
            due_at,
            recurrence,
            anchor_local: None,
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
}
