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

    let mut at = match tz {
        UserTz::Named(zone) => parse_in(&normalised, zone, now)?,
        UserTz::Fixed(offset) => parse_in(&normalised, offset.fixed(), now)?,
    };
    if !is_duration && !HAS_TIME.is_match(&normalised) {
        at = at.date().and_time(DEFAULT_TIME);
    }

    if at <= now_local {
        // A bare time that has passed today ("9am" at 10:30) means tomorrow.
        // Explicit dates and nonpositive durations must never take this shortcut.
        if !BARE_CLOCK.is_match(&normalised) {
            return None;
        }
        at = at.checked_add_signed(Duration::days(1))?;
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
    fn only_bare_clocks_roll_forward_to_tomorrow() {
        for input in ["9am", "9 am", "09:00", "в 09:00", "at 9am"] {
            assert_eq!(
                parse(input, SGT, now()),
                Some(local(2026, 10, 14, 9, 0)),
                "{input}"
            );
        }
        assert_eq!(
            parse("10:30", SGT, now()),
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
            assert_eq!(parse(input, SGT, now()), None, "{input}");
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
            assert_eq!(parse(input, SGT, now()), None, "{input}");
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
            assert_eq!(parse(input, SGT, now()), Some(expected), "{input}");
        }
        for input in ["послезавтра купить молоко", "послезавтра 25:00"]
        {
            assert_eq!(parse(input, SGT, now()), None, "{input}");
        }
    }

    #[test]
    fn day_after_tomorrow_uses_the_users_local_calendar() {
        let near_midnight = SGT.utc(local(2026, 10, 14, 0, 30));
        assert_eq!(
            parse("послезавтра", SGT, near_midnight),
            Some(local(2026, 10, 16, 9, 0))
        );
        let london = UserTz::Named(Tz::Europe__London);
        let before_dst = london.utc(local(2026, 3, 28, 10, 30));
        assert_eq!(
            parse("послезавтра 10:30", london, before_dst),
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
            assert_eq!(parse(input, SGT, now()), None, "{input}");
        }
    }

    #[test]
    fn midnight_means_zero_hours_on_the_next_day() {
        for text in ["midnight", "полночь", "в полночь"] {
            assert_eq!(
                parse(text, SGT, now()),
                Some(local(2026, 10, 14, 0, 0)),
                "{text}"
            );
        }
    }
}
