//! Formatting shared by HTTP and Telegram presentation.
use super::i18n::Lang;
use chrono::{DateTime, Utc};

/// "in 5 min", "in 3 h 20 min", "in 4 days".
pub(crate) fn relative_lang(from: DateTime<Utc>, to: DateTime<Utc>, lang: Lang) -> String {
    let minutes = (to - from).num_minutes().max(0);
    match minutes {
        0 => lang.text("relative.now"),
        1..=59 => lang.render("relative.minutes", &[("minutes", minutes.to_string())]),
        60..=2879 => match minutes % 60 {
            0 => lang.render("relative.hours", &[("hours", (minutes / 60).to_string())]),
            m => lang.render(
                "relative.hours_minutes",
                &[
                    ("hours", (minutes / 60).to_string()),
                    ("minutes", m.to_string()),
                ],
            ),
        },
        _ => lang.render("relative.days", &[("days", (minutes / 1440).to_string())]),
    }
}
