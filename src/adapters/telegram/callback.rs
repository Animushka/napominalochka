//! Inline button actions and their compact encoding in Telegram callback data (max 64 bytes).
//!
//! Each button carries everything needed to render the next step (the date or time picked so
//! far), so the flow does not keep per-chat state on the server beyond the draft text.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;

use crate::domain::{Recurrence, Snooze};

/// What a time picker is for: a new reminder (draft) or an existing reminder being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subject {
    Draft(crate::domain::DraftId),
    Reminder(crate::domain::ReminderId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Quick choices: "in 15 min", "tomorrow 09:00", "pick a date".
    When(Subject),
    /// Month calendar.
    Calendar(Subject, i32, u32),
    /// A day was picked; choose the hour.
    Day(Subject, NaiveDate),
    /// An hour was picked; choose the minute.
    Hour(Subject, NaiveDate, u32),
    /// A time was picked; choose how often.
    At(Subject, NaiveDateTime),
    /// An elapsed interval is encoded as UTC so a repeated local hour stays unambiguous.
    AtInstant(Subject, DateTime<Utc>),
    Save(Subject, NaiveDateTime, Recurrence),
    SaveInstant(Subject, DateTime<Utc>, Recurrence),
    CancelDraft(crate::domain::DraftId),
    Manage(crate::domain::ReminderId),
    Delete(crate::domain::ReminderId),
    List,
    ListPage(usize),
    Done(crate::domain::NaggingId),
    /// Undo one recorded completion, including a negative answer to its follow-up.
    UndoCompletion(crate::domain::CompletionId),
    Snooze(crate::domain::NaggingId, Snooze),
    /// `ack:<nagging_id>` from the first Rust release.
    LegacyNaggingAck(crate::domain::NaggingId),
    /// `{"Id": <reminder_id>}` from the .NET bot.
    LegacyReminderAck(crate::domain::ReminderId),
    /// Replace the calendar feed link, revoking the old one.
    CalendarReset,
    /// Calendar padding and headers.
    Noop,
}

const DATE: &str = "%Y%m%d";
const DATE_TIME: &str = "%Y%m%d%H%M";

impl Action {
    pub(crate) fn encode(self) -> String {
        match self {
            Action::When(s) => format!("w:{}", subject(s)),
            Action::Calendar(s, year, month) => format!("c:{}:{year:04}{month:02}", subject(s)),
            Action::Day(s, date) => format!("D:{}:{}", subject(s), date.format(DATE)),
            Action::Hour(s, date, hour) => {
                format!("H:{}:{}{hour:02}", subject(s), date.format(DATE))
            }
            Action::At(s, at) => format!("A:{}:{}", subject(s), at.format(DATE_TIME)),
            Action::AtInstant(s, at) => format!("U:{}:{}", subject(s), at.timestamp()),
            Action::Save(s, at, r) => {
                format!("S:{}:{}:{}", subject(s), at.format(DATE_TIME), r.code())
            }
            Action::SaveInstant(s, at, r) => {
                format!("V:{}:{}:{}", subject(s), at.timestamp(), r.code())
            }
            Action::CancelDraft(id) => format!("x:{id}"),
            Action::Manage(id) => format!("m:{id}"),
            Action::Delete(id) => format!("del:{id}"),
            Action::List => "list".into(),
            Action::ListPage(page) => format!("list:{page}"),
            Action::Done(id) => format!("n:{id}:ok"),
            Action::UndoCompletion(id) => format!("undo:{id}"),
            Action::Snooze(id, snooze) => format!(
                "n:{id}:{}",
                match snooze {
                    Snooze::TenMinutes => "10m",
                    Snooze::OneHour => "1h",
                    Snooze::TomorrowMorning => "tm",
                }
            ),
            Action::LegacyNaggingAck(id) => format!("ack:{id}"),
            Action::LegacyReminderAck(id) => format!(r#"{{"Id":{id}}}"#),
            Action::CalendarReset => "cal:reset".into(),
            Action::Noop => "-".into(),
        }
    }

    pub(crate) fn parse(data: &str) -> Option<Self> {
        // Every format generated here is ASCII, including legacy JSON ids. Reject invalid
        // input before any byte slicing and cap parsing work at Telegram's callback limit.
        if data.len() > 64 || !data.is_ascii() {
            return None;
        }
        if data.starts_with('{') {
            #[derive(Deserialize)]
            struct Legacy {
                #[serde(rename = "Id")]
                id: crate::domain::ReminderId,
            }
            return serde_json::from_str::<Legacy>(data)
                .ok()
                .map(|legacy| Action::LegacyReminderAck(legacy.id));
        }

        let parts: Vec<&str> = data.split(':').collect();
        Some(match parts.as_slice() {
            ["-"] => Action::Noop,
            ["list"] => Action::List,
            ["list", page] => Action::ListPage(page.parse().ok()?),
            ["cal", "reset"] => Action::CalendarReset,
            ["w", s] => Action::When(parse_subject(s)?),
            ["c", s, ym] if ym.len() == 6 => {
                let month: u32 = ym[4..].parse().ok()?;
                NaiveDate::from_ymd_opt(ym[..4].parse().ok()?, month, 1)?;
                Action::Calendar(parse_subject(s)?, ym[..4].parse().ok()?, month)
            }
            ["D", s, date] if date.len() == 8 => Action::Day(
                parse_subject(s)?,
                NaiveDate::parse_from_str(date, DATE).ok()?,
            ),
            ["H", s, dh] if dh.len() == 10 => {
                let hour: u32 = dh[8..].parse().ok().filter(|h| *h < 24)?;
                Action::Hour(
                    parse_subject(s)?,
                    NaiveDate::parse_from_str(&dh[..8], DATE).ok()?,
                    hour,
                )
            }
            ["A", s, at] if at.len() == 12 => Action::At(
                parse_subject(s)?,
                NaiveDateTime::parse_from_str(at, DATE_TIME).ok()?,
            ),
            ["S", s, at, r] if at.len() == 12 => Action::Save(
                parse_subject(s)?,
                NaiveDateTime::parse_from_str(at, DATE_TIME).ok()?,
                Recurrence::from_code(r.parse().ok()?)?,
            ),
            ["U", s, timestamp] => Action::AtInstant(parse_subject(s)?, parse_instant(timestamp)?),
            ["V", s, timestamp, r] => Action::SaveInstant(
                parse_subject(s)?,
                parse_instant(timestamp)?,
                Recurrence::from_code(r.parse().ok()?)?,
            ),
            ["x", id] => Action::CancelDraft(id.parse().ok()?),
            ["m", id] => Action::Manage(id.parse().ok()?),
            ["del", id] => Action::Delete(id.parse().ok()?),
            ["n", id, "ok"] => Action::Done(id.parse().ok()?),
            ["undo", id] => Action::UndoCompletion(
                id.parse::<crate::domain::CompletionId>()
                    .ok()
                    .filter(|id| id.0 > 0)?,
            ),
            ["n", id, "10m"] => Action::Snooze(id.parse().ok()?, Snooze::TenMinutes),
            ["n", id, "1h"] => Action::Snooze(id.parse().ok()?, Snooze::OneHour),
            ["n", id, "tm"] => Action::Snooze(id.parse().ok()?, Snooze::TomorrowMorning),
            ["ack", id] => Action::LegacyNaggingAck(id.parse().ok()?),
            _ => return None,
        })
    }
}

fn parse_instant(timestamp: &str) -> Option<DateTime<Utc>> {
    let at = DateTime::from_timestamp(timestamp.parse().ok()?, 0)?;
    // Match the four-digit calendar supported by the chat picker. This also excludes
    // chrono's UTC boundary where applying a timezone offset could overflow local time.
    (1..=9999).contains(&at.year()).then_some(at)
}

fn subject(s: Subject) -> String {
    match s {
        Subject::Draft(id) => format!("d{id}"),
        Subject::Reminder(id) => format!("r{id}"),
    }
}

fn parse_subject(s: &str) -> Option<Subject> {
    let (kind, id) = s.split_at_checked(1)?;
    match kind {
        "d" => Some(Subject::Draft(id.parse().ok()?)),
        "r" => Some(Subject::Reminder(id.parse().ok()?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
