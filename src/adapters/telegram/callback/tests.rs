use super::*;
use crate::domain::{CompletionId, DraftId, NaggingId, ReminderId};

#[test]
fn round_trips_and_fits_in_64_bytes() {
    let date = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
    let at = date.and_hms_opt(23, 55, 0).unwrap();
    let big = i64::MAX;
    for action in [
        Action::When(Subject::Draft(DraftId(big))),
        Action::Calendar(Subject::Reminder(ReminderId(big)), 2026, 12),
        Action::Day(Subject::Draft(DraftId(big)), date),
        Action::Hour(Subject::Draft(DraftId(big)), date, 23),
        Action::At(Subject::Reminder(ReminderId(big)), at),
        Action::Save(Subject::Draft(DraftId(big)), at, Recurrence::Yearly),
        Action::AtInstant(Subject::Draft(DraftId(big)), at.and_utc()),
        Action::SaveInstant(Subject::Draft(DraftId(big)), at.and_utc(), Recurrence::Once),
        Action::ListPage(usize::MAX),
        Action::CancelDraft(DraftId(big)),
        Action::Manage(ReminderId(big)),
        Action::Delete(ReminderId(big)),
        Action::List,
        Action::Done(NaggingId(big)),
        Action::UndoCompletion(CompletionId(big)),
        Action::Snooze(NaggingId(big), Snooze::TenMinutes),
        Action::Snooze(NaggingId(big), Snooze::OneHour),
        Action::Snooze(NaggingId(big), Snooze::TomorrowMorning),
        Action::LegacyNaggingAck(NaggingId(big)),
        Action::LegacyReminderAck(ReminderId(big)),
        Action::CalendarReset,
        Action::Noop,
    ] {
        let data = action.encode();
        assert!(data.len() <= 64, "{data} is {} bytes", data.len());
        assert_eq!(Action::parse(&data), Some(action), "{data}");
    }
}

#[test]
fn rejects_garbage() {
    for data in [
        "",
        "w:x1",
        "c:d1:202613",
        "H:d1:2026010124",
        "S:d1:202601010900:9",
        "n:1:zz",
        "undo:0",
        "undo:-1",
        "undo:1:2",
        "undo:é",
        "{}",
        "c:d1:123éx",
        "H:d1:1234567éx",
        "U:d1:9999999999999999999999",
        "list:-1",
        "U:d1:8210266876799",
        "A:d1:+26214212312359",
    ] {
        assert_eq!(Action::parse(data), None, "{data}");
    }
}
