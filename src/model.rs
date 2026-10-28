//! Domain types shared by the database, the scheduler and the conversation handlers.

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, Months, NaiveDateTime, TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use regex::Regex;
use thiserror::Error;

/// Settings are independent of timezone registration and isolated by Telegram user id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    pub language: String,
    pub nag_interval_minutes: i64,
    pub confirmation_check_minutes: i64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: "ru".into(),
            nag_interval_minutes: 10,
            confirmation_check_minutes: 0,
        }
    }
}

/// A single optional check after completion, never part of the nagging loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub id: i64,
    pub user_id: i64,
    pub description: String,
    pub due_at: DateTime<Utc>,
}

/// How often a reminder repeats once it has fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recurrence {
    Once,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

impl Recurrence {
    #[cfg(test)]
    pub const ALL: [Recurrence; 6] = [
        Recurrence::Once,
        Recurrence::Daily,
        Recurrence::Weekdays,
        Recurrence::Weekly,
        Recurrence::Monthly,
        Recurrence::Yearly,
    ];

    /// Legacy schedules use their current local due date as the best available anchor.
    pub fn next_after(
        self,
        due_at: DateTime<Utc>,
        now: DateTime<Utc>,
        tz: UserTz,
    ) -> Option<DateTime<Utc>> {
        self.next_after_anchored(due_at, now, tz, tz.local(due_at))
    }

    /// The first occurrence strictly after `now`, calculated from the original local
    /// calendar date/time. A short month or DST gap changes only that occurrence.
    pub fn next_after_anchored(
        self,
        due_at: DateTime<Utc>,
        now: DateTime<Utc>,
        tz: UserTz,
        anchor_local: NaiveDateTime,
    ) -> Option<DateTime<Utc>> {
        if due_at > now {
            return Some(due_at);
        }
        if self == Recurrence::Once {
            return None;
        }
        let local_now = tz.local(now);
        let days = (local_now.date() - anchor_local.date()).num_days();
        // Jump directly to the current calendar period instead of replaying every missed
        // occurrence. Always calculate from the anchor, never from a previously clamped date.
        let index = match self {
            Recurrence::Once => unreachable!(),
            Recurrence::Daily | Recurrence::Weekdays => days,
            Recurrence::Weekly => days.div_euclid(7),
            Recurrence::Monthly => {
                i64::from(local_now.year() - anchor_local.year()) * 12
                    + i64::from(local_now.month())
                    - i64::from(anchor_local.month())
            }
            Recurrence::Yearly => i64::from(local_now.year() - anchor_local.year()),
        };
        let mut index = u32::try_from(index.max(0)).ok()?;
        loop {
            let local = match self {
                Recurrence::Once => unreachable!(),
                Recurrence::Daily | Recurrence::Weekdays => {
                    anchor_local.checked_add_signed(Duration::days(i64::from(index)))?
                }
                Recurrence::Weekly => {
                    anchor_local.checked_add_signed(Duration::weeks(i64::from(index)))?
                }
                Recurrence::Monthly => anchor_local.checked_add_months(Months::new(index))?,
                Recurrence::Yearly => {
                    anchor_local.checked_add_months(Months::new(index.checked_mul(12)?))?
                }
            };
            if !(self == Recurrence::Weekdays
                && matches!(local.weekday(), Weekday::Sat | Weekday::Sun))
            {
                let next = tz.utc(local);
                if next > now {
                    return Some(next);
                }
            }
            index = index.checked_add(1)?;
        }
    }

    /// When a reminder set for local time `at` first fires: `at` itself, or the next
    /// occurrence if `at` has passed. `None` if a one-off time has passed.
    pub fn first_due(
        self,
        at: NaiveDateTime,
        tz: UserTz,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let mut first = at;
        if self == Recurrence::Weekdays {
            while matches!(first.weekday(), Weekday::Sat | Weekday::Sun) {
                first = first.checked_add_signed(Duration::days(1))?;
            }
        }
        let due = tz.utc(first);
        if due > now {
            Some(due)
        } else {
            self.next_after_anchored(due, now, tz, at)
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Recurrence::Once => "once",
            Recurrence::Daily => "daily",
            Recurrence::Weekdays => "weekdays",
            Recurrence::Weekly => "weekly",
            Recurrence::Monthly => "monthly",
            Recurrence::Yearly => "yearly",
        }
    }

    /// Short human description relative to the first occurrence, e.g. "every Tue".
    pub fn describe(self, first: NaiveDateTime) -> String {
        match self {
            Recurrence::Once => "once".into(),
            Recurrence::Daily => "every day".into(),
            Recurrence::Weekdays => "every weekday".into(),
            Recurrence::Weekly => format!("every {}", first.format("%a")),
            Recurrence::Monthly => format!("monthly on the {}", ordinal(first.day())),
            Recurrence::Yearly => format!("every year on {}", first.format("%-d %b")),
        }
    }

    pub(crate) fn to_db(self) -> i64 {
        match self {
            Recurrence::Once => 0,
            Recurrence::Daily => 1,
            Recurrence::Weekly => 2,
            Recurrence::Monthly => 3,
            Recurrence::Weekdays => 4,
            Recurrence::Yearly => 5,
        }
    }

    pub(crate) fn from_db(value: i64) -> Option<Self> {
        Some(match value {
            0 => Recurrence::Once,
            1 => Recurrence::Daily,
            2 => Recurrence::Weekly,
            3 => Recurrence::Monthly,
            4 => Recurrence::Weekdays,
            5 => Recurrence::Yearly,
            _ => return None,
        })
    }
}

impl FromStr for Recurrence {
    type Err = InputError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "once" => Ok(Recurrence::Once),
            "daily" => Ok(Recurrence::Daily),
            "weekdays" => Ok(Recurrence::Weekdays),
            "weekly" => Ok(Recurrence::Weekly),
            "monthly" => Ok(Recurrence::Monthly),
            "yearly" => Ok(Recurrence::Yearly),
            _ => Err(InputError::InvalidRecurrence),
        }
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// A fixed offset from UTC, with minute precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcOffset {
    minutes: i32,
}

impl UtcOffset {
    const MIN_MINUTES: i32 = -12 * 60;
    const MAX_MINUTES: i32 = 14 * 60;

    pub fn from_minutes(minutes: i32) -> Option<Self> {
        (Self::MIN_MINUTES..=Self::MAX_MINUTES)
            .contains(&minutes)
            .then_some(Self { minutes })
    }

    #[cfg(test)]
    pub fn minutes(self) -> i32 {
        self.minutes
    }

    pub fn fixed(self) -> FixedOffset {
        FixedOffset::east_opt(self.minutes * 60).expect("offset range is validated on construction")
    }
}

impl fmt::Display for UtcOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let abs = self.minutes.abs();
        write!(f, "UTC{sign}{:02}:{:02}", abs / 60, abs % 60)
    }
}

impl FromStr for UtcOffset {
    type Err = InputError;

    /// Accepts `8`, `+8`, `-5`, `+5:30`, `+0530` and an optional `UTC`/`GMT` prefix.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)^(?:utc|gmt)?\s*([+-])?(\d{1,2})(?::?(\d{2}))?$").expect("valid regex")
        });

        let caps = PATTERN
            .captures(s.trim())
            .ok_or(InputError::InvalidTimezone)?;
        let hours: i32 = caps[2].parse().map_err(|_| InputError::InvalidTimezone)?;
        let minutes: i32 = caps
            .get(3)
            .map_or(Ok(0), |m| m.as_str().parse())
            .map_err(|_| InputError::InvalidTimezone)?;
        if minutes >= 60 {
            return Err(InputError::InvalidTimezone);
        }

        let total = hours * 60 + minutes;
        let total = if caps.get(1).is_some_and(|m| m.as_str() == "-") {
            -total
        } else {
            total
        };
        UtcOffset::from_minutes(total).ok_or(InputError::InvalidTimezone)
    }
}

/// A user's timezone: an IANA zone (daylight-saving aware) or a fixed UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTz {
    Named(Tz),
    Fixed(UtcOffset),
}

impl UserTz {
    pub fn local(self, at: DateTime<Utc>) -> NaiveDateTime {
        match self {
            UserTz::Named(tz) => at.with_timezone(&tz).naive_local(),
            UserTz::Fixed(offset) => at.with_timezone(&offset.fixed()).naive_local(),
        }
    }

    /// Converts a local time to UTC. Ambiguous times (clocks going back) resolve to the first
    /// occurrence; non-existent times (clocks going forward) move forward by the gap.
    pub fn utc(self, local: NaiveDateTime) -> DateTime<Utc> {
        fn resolve<T: TimeZone>(tz: &T, local: NaiveDateTime) -> DateTime<Utc> {
            (0..=3)
                .find_map(|h| {
                    tz.from_local_datetime(&(local + Duration::hours(h)))
                        .earliest()
                })
                .map_or_else(|| local.and_utc(), |dt| dt.with_timezone(&Utc))
        }
        match self {
            UserTz::Named(tz) => resolve(&tz, local),
            UserTz::Fixed(offset) => resolve(&offset.fixed(), local),
        }
    }

    /// Current offset from UTC in minutes.
    pub fn offset_minutes(self, at: DateTime<Utc>) -> i32 {
        let local = self.local(at);
        i32::try_from((local - at.naive_utc()).num_minutes()).unwrap_or(0)
    }

    /// IANA name, if this is a named zone.
    pub fn name(self) -> Option<&'static str> {
        match self {
            UserTz::Named(tz) => Some(tz.name()),
            UserTz::Fixed(_) => None,
        }
    }

    /// Parses an offset (`+8`, `-3:30`), an IANA name (`Europe/London`) or a city
    /// (`london`, `new york`).
    pub fn parse(input: &str) -> Result<Self, InputError> {
        let input = input.trim();
        if let Ok(offset) = input.parse::<UtcOffset>() {
            return Ok(UserTz::Fixed(offset));
        }
        // Prefer canonical "Region/City" zones over legacy aliases such as "Singapore".
        let city = input.replace(' ', "_");
        let by_city = chrono_tz::TZ_VARIANTS.iter().find(|tz| {
            tz.name().split_once('/').is_some_and(|_| {
                tz.name()
                    .rsplit('/')
                    .next()
                    .is_some_and(|last| last.eq_ignore_ascii_case(&city))
            })
        });
        if let Some(tz) = by_city {
            return Ok(UserTz::Named(*tz));
        }
        Tz::from_str_insensitive(input)
            .map(UserTz::Named)
            .map_err(|_| InputError::InvalidTimezone)
    }

    /// Looks up the timezone at a location. Loads the boundary data on demand, as it is only
    /// needed when a user shares their location.
    pub fn from_location(latitude: f64, longitude: f64) -> Option<Self> {
        let finder = tzf_rs::DefaultFinder::new();
        Tz::from_str(finder.get_tz_name(longitude, latitude))
            .ok()
            .map(UserTz::Named)
    }
}

impl fmt::Display for UserTz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserTz::Named(tz) => {
                let offset = UtcOffset::from_minutes(UserTz::Named(*tz).offset_minutes(Utc::now()));
                match offset {
                    Some(offset) => write!(f, "{} ({offset})", tz.name()),
                    None => f.write_str(tz.name()),
                }
            }
            UserTz::Fixed(offset) => offset.fmt(f),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub tz: UserTz,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub id: i64,
    pub user_id: i64,
    pub description: String,
    pub due_at: DateTime<Utc>,
    pub recurrence: Recurrence,
    /// Original local schedule; NULL in migrated rows until their next firing.
    pub anchor_local: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nagging {
    pub id: i64,
    pub reminder_id: Option<i64>,
    pub user_id: i64,
    pub description: String,
    pub next_at: DateTime<Utc>,
}

/// How long to postpone a nagging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snooze {
    TenMinutes,
    OneHour,
    TomorrowMorning,
}

/// Text the user sent that is waiting for a time to be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: i64,
    pub user_id: i64,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

/// User-correctable input problems, translated by the presentation layer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum InputError {
    #[error(
        "I don't recognise that timezone. Try a city (London, New York), a timezone (Asia/Singapore) or an offset (+8, -3:30)."
    )]
    InvalidTimezone,
    #[error("Repeat must be one of: once, daily, weekdays, weekly, monthly, yearly.")]
    InvalidRecurrence,
    #[error("Please enter what to remind you about.")]
    EmptyDescription,
    #[error("Reminder text must be 1000 characters or fewer.")]
    DescriptionTooLong,
    #[error("That time has already passed.")]
    InPast,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        local(y, mo, d, h, mi).and_utc()
    }

    const UTC: UserTz = UserTz::Fixed(UtcOffset { minutes: 0 });

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
            Ok(UserTz::Fixed(UtcOffset { minutes: 480 }))
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
            assert_eq!(Recurrence::from_db(r.to_db()), Some(r));
        }
    }
}
