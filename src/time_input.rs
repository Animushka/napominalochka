//! Parses a typed date/time such as "fri 6pm", "25/12 9:00", "in 2 hours" or "tomorrow".
//!
//! The whole message must be a time expression; this is used when the bot has asked
//! "when?", not to pick times out of arbitrary sentences.

use std::sync::LazyLock;

use chrono::{DateTime, Duration, NaiveDateTime, NaiveTime, TimeZone, Utc};
use interim::{Dialect, parse_date_string, parse_duration};
use regex::Regex;

use crate::model::UserTz;

/// Used when a day is given without a time ("friday", "25/12", "tomorrow").
const DEFAULT_TIME: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).expect("valid time");
/// Longer messages are treated as a new reminder rather than an answer to "when?".
const MAX_LEN: usize = 40;

/// Returns the local date/time, or `None` if the text is not a future time expression.
pub fn parse(text: &str, tz: UserTz, now: DateTime<Utc>) -> Option<NaiveDateTime> {
    static HAS_TIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\d{1,2}[:.]\d{2}|\d\s*(am|pm)\b|\bnoon\b|\bmidnight\b|\btonight\b")
            .expect("valid regex")
    });

    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_LEN {
        return None;
    }
    let normalised = normalise(text);
    // "2 hours" and "30 min" keep the current minute; "tomorrow" is not a duration here.
    let is_duration =
        normalised.chars().any(|c| c.is_ascii_digit()) && parse_duration(&normalised).is_ok();

    let mut at = match tz {
        UserTz::Named(zone) => parse_in(&normalised, zone, now)?,
        UserTz::Fixed(offset) => parse_in(&normalised, offset.fixed(), now)?,
    };
    if !is_duration && !HAS_TIME.is_match(&normalised) {
        at = at.date().and_time(DEFAULT_TIME);
    }

    let now_local = tz.local(now);
    if at <= now_local {
        // A bare time that has passed today ("9am" at 10:30) means tomorrow.
        at += Duration::days(1);
        if at <= now_local {
            return None;
        }
    }
    Some(at)
}

fn parse_in<T: TimeZone>(text: &str, tz: T, now: DateTime<Utc>) -> Option<NaiveDateTime> {
    parse_date_string(text, now.with_timezone(&tz), Dialect::Uk)
        .ok()
        .map(|dt| dt.naive_local())
}

/// Rewrites phrases the parser does not know into ones it does.
fn normalise(text: &str) -> String {
    let lower = text.to_lowercase();
    let lower = lower
        .replace("сегодня вечером", "today 20:00")
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
            "послезавтра" => "in 2 days",
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
            "полночь" => "23:59",
            "noon" | "midday" => "12:00",
            "midnight" => "23:59",
            "tonight" => "today 20:00",
            "tmr" | "tmrw" => "tomorrow",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono_tz::Tz;

    const SGT: UserTz = UserTz::Named(Tz::Asia__Singapore);

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
            assert_eq!(parse(input, SGT, now()), Some(expected), "{input}");
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
            assert_eq!(parse(input, SGT, now()), Some(expected), "{input}");
        }
    }

    #[test]
    fn rejects_non_times() {
        for input in [
            "call mum",
            "",
            "buy milk and eggs tomorrow",
            "1/1/2020 08:00",
        ] {
            assert_eq!(parse(input, SGT, now()), None, "{input}");
        }
    }
}
