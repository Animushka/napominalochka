//! Calendar recurrence rules, including local calendar anchors.
use super::{InputError, UserTz};
use chrono::{DateTime, Datelike, Duration, Months, NaiveDateTime, Utc, Weekday};
use std::str::FromStr;

/// How often a reminder repeats once it has fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recurrence {
    Once,
    Daily,
    Weekdays,
    Weekly,
    Monthly,
    Yearly,
}

impl Recurrence {
    #[cfg(test)]
    pub(crate) const ALL: [Recurrence; 6] = [
        Recurrence::Once,
        Recurrence::Daily,
        Recurrence::Weekdays,
        Recurrence::Weekly,
        Recurrence::Monthly,
        Recurrence::Yearly,
    ];

    /// Legacy schedules use their current local due date as the best available anchor.
    pub(crate) fn next_after(
        self,
        due_at: DateTime<Utc>,
        now: DateTime<Utc>,
        tz: UserTz,
    ) -> Option<DateTime<Utc>> {
        self.next_after_anchored(due_at, now, tz, tz.local(due_at))
    }

    /// The first occurrence strictly after `now`, calculated from the original local
    /// calendar date/time. A short month or DST gap changes only that occurrence.
    pub(crate) fn next_after_anchored(
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
    pub(crate) fn first_due(
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

    pub(crate) fn as_str(self) -> &'static str {
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
    pub(crate) fn describe(self, first: NaiveDateTime) -> String {
        match self {
            Recurrence::Once => "once".into(),
            Recurrence::Daily => "every day".into(),
            Recurrence::Weekdays => "every weekday".into(),
            Recurrence::Weekly => format!("every {}", first.format("%a")),
            Recurrence::Monthly => format!("monthly on the {}", ordinal(first.day())),
            Recurrence::Yearly => format!("every year on {}", first.format("%-d %b")),
        }
    }

    pub(crate) fn code(self) -> i64 {
        match self {
            Recurrence::Once => 0,
            Recurrence::Daily => 1,
            Recurrence::Weekly => 2,
            Recurrence::Monthly => 3,
            Recurrence::Weekdays => 4,
            Recurrence::Yearly => 5,
        }
    }

    pub(crate) fn from_code(value: i64) -> Option<Self> {
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

pub(super) fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}
