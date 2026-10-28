//! iCalendar (RFC 5545) feed of a user's reminders, for subscribing in a phone's calendar
//! app. Alarms then fire from the phone itself, even without a connection.

use chrono::{DateTime, Utc};
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

    for reminder in reminders {
        let summary = escape(&reminder.description);
        ics.line("BEGIN:VEVENT");
        ics.line(&format!("UID:reminder-{}@extremelynaggybot", reminder.id));
        ics.line(&format!("DTSTAMP:{}", utc(now)));
        // Recurring events use the user's zone so they keep their local time across
        // daylight saving changes. Calendar apps resolve IANA names without VTIMEZONE data.
        match tz.name() {
            Some(name) => ics.line(&format!(
                "DTSTART;TZID={name}:{}",
                tz.local(reminder.due_at).format("%Y%m%dT%H%M%S")
            )),
            None => ics.line(&format!("DTSTART:{}", utc(reminder.due_at))),
        }
        ics.line("DURATION:PT15M");
        if let Some(rule) = rrule(reminder.recurrence) {
            ics.line(&format!("RRULE:{rule}"));
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

fn rrule(recurrence: Recurrence) -> Option<&'static str> {
    match recurrence {
        Recurrence::Once => None,
        Recurrence::Daily => Some("FREQ=DAILY"),
        Recurrence::Weekdays => Some("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
        Recurrence::Weekly => Some("FREQ=WEEKLY"),
        Recurrence::Monthly => Some("FREQ=MONTHLY"),
        Recurrence::Yearly => Some("FREQ=YEARLY"),
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
    fn fixed_offset_users_get_utc_times() {
        let tz = UserTz::Fixed(UtcOffset::from_minutes(480).unwrap());
        let due = Utc.with_ymd_and_hms(2026, 10, 14, 1, 0, 0).unwrap();
        let ics = feed(&[reminder(1, "x", due, Recurrence::Daily)], tz, due);
        assert!(ics.contains("DTSTART:20261014T010000Z\r\n"));
        assert!(!ics.contains("X-WR-TIMEZONE"));
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
}
