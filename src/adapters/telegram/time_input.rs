//! Parses a typed date/time such as "fri 6pm", "25/12 9:00", "in 2 hours" or "tomorrow".
//!
//! The whole message must be a time expression; this is used when the bot has asked
//! "when?", not to pick times out of arbitrary sentences.

use std::sync::LazyLock;

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use interim::{Dialect, parse_date_string, parse_duration};
use regex::Regex;

use crate::domain::{ReminderTime, UserTz};

/// Used when a day is given without a time ("friday", "25/12", "tomorrow").
const DEFAULT_TIME: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).expect("valid time");
/// Longer messages are treated as a new reminder rather than an answer to "when?".
const MAX_LEN: usize = 40;

/// Keeps elapsed intervals in UTC and calendar choices in local time.
pub(crate) fn parse(text: &str, tz: UserTz, now: DateTime<Utc>) -> Option<ReminderTime> {
    static HAS_TIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\d{1,2}[:.]\d{2}|\d\s*(am|pm)\b|\bnoon\b|\bmidnight\b|\btonight\b")
            .expect("valid regex")
    });
    static BARE_CLOCK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(?:(?:[01]?\d|2[0-3])[:.][0-5]\d|(?:0?[1-9]|1[0-2])(?:[:.][0-5]\d)?\s*(?:am|pm))$",
        )
        .expect("valid regex")
    });

    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_LEN {
        return None;
    }
    let now_local = tz.local(now);
    let mut normalised = normalise(text);
    if let Some(suffix) = normalised
        .strip_prefix("послезавтра")
        .filter(|suffix| suffix.is_empty() || suffix.starts_with(' '))
    {
        let clock = normalise(suffix.trim());
        if !clock.is_empty() && !BARE_CLOCK.is_match(&clock) {
            return None;
        }
        // This names a local calendar day, not a duration of 48 hours. Give the parser
        // an explicit date so the usual default time and optional clock both apply.
        let date = now_local.date().checked_add_signed(Duration::days(2))?;
        normalised = format!("{} {clock}", date.format("%d/%m/%Y"));
    }
    // "2 hours" and "30 min" keep the current minute; "tomorrow" is not a duration here.
    let is_duration =
        normalised.chars().any(|c| c.is_ascii_digit()) && parse_duration(&normalised).is_ok();

    let parsed = match tz {
        UserTz::Named(zone) => parse_in(&normalised, zone, now)?,
        UserTz::Fixed(offset) => parse_in(&normalised, offset.fixed(), now)?,
    };
    if is_duration {
        return (parsed > now).then_some(ReminderTime::Instant(parsed));
    }
    let mut at = tz.local(parsed);
    if !HAS_TIME.is_match(&normalised) {
        at = at.date().and_time(DEFAULT_TIME);
    }

    if tz.utc(at) <= now {
        // A bare time that has passed today ("9am" at 10:30) means tomorrow.
        // Explicit dates and nonpositive durations must never take this shortcut.
        if !BARE_CLOCK.is_match(&normalised) {
            return None;
        }
        at = at.checked_add_signed(Duration::days(1))?;
        if tz.utc(at) <= now {
            return None;
        }
    }
    Some(ReminderTime::Local(at))
}

fn parse_in<T: TimeZone>(text: &str, tz: T, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_date_string(text, now.with_timezone(&tz), Dialect::Uk)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Rewrites phrases the parser does not know into ones it does.
fn normalise(text: &str) -> String {
    let lower = text.to_lowercase();
    let lower = lower
        .replace("сегодня вечером", "today 20:00")
        .replace("послезавтра утром", "послезавтра 09:00")
        .replace("завтра утром", "tomorrow 09:00");
    let lower = lower
        .strip_prefix("через ")
        .or_else(|| lower.strip_prefix("в "))
        .or_else(|| lower.strip_prefix("in "))
        .or_else(|| lower.strip_prefix("at "))
        .or_else(|| lower.strip_prefix("on "))
        .unwrap_or(&lower);
    lower
        .split_whitespace()
        .map(|word| match word {
            "сегодня" => "today",
            "завтра" => "tomorrow",
            "пн" | "понедельник" => "monday",
            "вт" | "вторник" => "tuesday",
            "ср" | "среду" | "среда" => "wednesday",
            "чт" | "четверг" => "thursday",
            "пт" | "пятницу" | "пятница" => "friday",
            "сб" | "субботу" | "суббота" => "saturday",
            "вс" | "воскресенье" => "sunday",
            "мин" | "минуту" | "минуты" | "минут" => "minutes",
            "ч" | "час" | "часа" | "часов" => "hours",
            "день" | "дня" | "дней" => "days",
            "полдень" => "12:00",
            "полночь" => "00:00",
            "noon" | "midday" => "12:00",
            "midnight" => "00:00",
            "tonight" => "today 20:00",
            "tmr" | "tmrw" => "tomorrow",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
