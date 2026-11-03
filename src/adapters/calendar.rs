//! iCalendar (RFC 5545) feed of a user's reminders, for subscribing in a phone's calendar
//! app. Alarms then fire from the phone itself, even without a connection.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Duration, Months, NaiveDateTime, TimeZone, Utc};
use url::Url;

use crate::adapters::i18n::Lang;
use crate::domain::{Recurrence, Reminder, UserTz};

/// Alarms per occurrence, in minutes after the start: a little naggy, like the bot.
const ALARM_OFFSETS_MINUTES: [u32; 3] = [0, 5, 10];
/// Suggested refresh interval for calendar apps that honour it.
const REFRESH: &str = "PT15M";

/// The public feed address for a token, under the Mini App's host.
pub(crate) fn feed_url(base: &Url, token: &str) -> Option<Url> {
    base.join(&format!("/calendar/{token}.ics")).ok()
}

/// `webcal://` form of a feed URL, which calendar apps open as a subscription.
pub(crate) fn webcal(url: &Url) -> String {
    format!("webcal://{}", url.as_str().trim_start_matches("https://"))
}

#[allow(dead_code)]
pub(crate) fn instructions(url: &Url) -> String {
    instructions_lang(url, Lang::En)
}

pub(crate) fn instructions_lang(url: &Url, lang: Lang) -> String {
    lang.render(
        "calendar.instructions",
        &[("webcal", webcal(url)), ("url", url.to_string())],
    )
}

#[cfg(test)]
pub(crate) fn feed(reminders: &[Reminder], tz: UserTz, now: DateTime<Utc>) -> String {
    feed_lang(reminders, tz, now, Lang::En)
}

pub(crate) fn feed_lang(
    reminders: &[Reminder],
    tz: UserTz,
    now: DateTime<Utc>,
    lang: Lang,
) -> String {
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
mod tests;
