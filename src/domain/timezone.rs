//! Timezone and offset value objects and DST conversion rules.
use super::InputError;
use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;
use std::{fmt, str::FromStr, sync::LazyLock};

/// A fixed offset from UTC, with minute precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UtcOffset {
    minutes: i32,
}

impl UtcOffset {
    const MIN_MINUTES: i32 = -12 * 60;
    const MAX_MINUTES: i32 = 14 * 60;

    pub(crate) const fn from_minutes(minutes: i32) -> Option<Self> {
        if minutes >= Self::MIN_MINUTES && minutes <= Self::MAX_MINUTES {
            Some(Self { minutes })
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn minutes(self) -> i32 {
        self.minutes
    }

    pub(crate) fn fixed(self) -> FixedOffset {
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
pub(crate) enum UserTz {
    Named(Tz),
    Fixed(UtcOffset),
}

impl UserTz {
    pub(crate) fn local(self, at: DateTime<Utc>) -> NaiveDateTime {
        match self {
            UserTz::Named(tz) => at.with_timezone(&tz).naive_local(),
            UserTz::Fixed(offset) => at.with_timezone(&offset.fixed()).naive_local(),
        }
    }

    /// Converts a local time to UTC. Ambiguous times (clocks going back) resolve to the first
    /// occurrence; non-existent times (clocks going forward) move forward by the gap.
    pub(crate) fn utc(self, local: NaiveDateTime) -> DateTime<Utc> {
        fn resolve<T: TimeZone>(tz: &T, local: NaiveDateTime) -> DateTime<Utc> {
            if let Some(at) = tz.from_local_datetime(&local).earliest() {
                return at.with_timezone(&Utc);
            }
            // Find the offsets on both sides, then shift by the actual gap. Some zones
            // jump by 30 minutes or an entire date, so adding one hour is incorrect.
            let mut before = None;
            let mut after = None;
            for hours in 1..=48 {
                before = before.or_else(|| {
                    local
                        .checked_sub_signed(Duration::hours(hours))
                        .and_then(|at| tz.from_local_datetime(&at).latest())
                });
                after = after.or_else(|| {
                    local
                        .checked_add_signed(Duration::hours(hours))
                        .and_then(|at| tz.from_local_datetime(&at).earliest())
                });
                if let (Some(before), Some(after)) = (&before, &after) {
                    let gap = after.offset().fix().local_minus_utc()
                        - before.offset().fix().local_minus_utc();
                    if let Some(at) = local
                        .checked_add_signed(Duration::seconds(i64::from(gap)))
                        .and_then(|at| tz.from_local_datetime(&at).earliest())
                    {
                        return at.with_timezone(&Utc);
                    }
                    break;
                }
            }
            // At chrono's representable boundary there may be no adjacent local date.
            // Keep this conversion total and never overflow while looking for an offset.
            local.and_utc()
        }
        match self {
            UserTz::Named(tz) => resolve(&tz, local),
            UserTz::Fixed(offset) => resolve(&offset.fixed(), local),
        }
    }

    /// Current offset from UTC in minutes.
    pub(crate) fn offset_minutes(self, at: DateTime<Utc>) -> i32 {
        let local = self.local(at);
        i32::try_from((local - at.naive_utc()).num_minutes()).unwrap_or(0)
    }

    /// IANA name, if this is a named zone.
    pub(crate) fn name(self) -> Option<&'static str> {
        match self {
            UserTz::Named(tz) => Some(tz.name()),
            UserTz::Fixed(_) => None,
        }
    }

    /// Parses an offset (`+8`, `-3:30`), an IANA name (`Europe/London`) or a city
    /// (`london`, `new york`).
    pub(crate) fn parse(input: &str) -> Result<Self, InputError> {
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
    pub(crate) fn from_location(latitude: f64, longitude: f64) -> Option<Self> {
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
