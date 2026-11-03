//! Explicit RU/EN catalogues shared by chat, calendar and notification presentation.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::application as service;
use crate::domain::{InputError, Recurrence};
use chrono::{Datelike, NaiveDateTime};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Lang {
    #[default]
    Ru,
    En,
}

static RU: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../locales/bot.ru.json"))
        .expect("valid Russian catalogue")
});
static EN: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../locales/bot.en.json"))
        .expect("valid English catalogue")
});

impl Lang {
    pub(crate) fn parse(code: &str) -> Self {
        if code == "en" { Self::En } else { Self::Ru }
    }
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }
    pub(crate) fn text(self, key: &str) -> String {
        lookup(if self == Self::Ru { &RU } else { &EN }, &EN, key).to_owned()
    }
    pub(crate) fn render(self, key: &str, values: &[(&str, String)]) -> String {
        let template = self.text(key);
        let mut result = String::with_capacity(template.len());
        let mut rest = template.as_str();
        while let Some(open) = rest.find('{') {
            result.push_str(&rest[..open]);
            let Some(close) = rest[open..].find('}').map(|offset| open + offset) else {
                result.push_str(&rest[open..]);
                return result;
            };
            let name = &rest[open + 1..close];
            match values.iter().find(|(key, _)| *key == name) {
                Some((_, value)) => result.push_str(value),
                None => result.push_str(&rest[open..=close]),
            }
            rest = &rest[close + 1..];
        }
        result.push_str(rest);
        result
    }
    pub(crate) fn input_error(self, error: &InputError) -> String {
        self.text(match error {
            InputError::InvalidTimezone => "error.timezone",
            InputError::InvalidRecurrence => "error.recurrence",
            InputError::EmptyDescription => "error.empty",
            InputError::DescriptionTooLong => "error.description_too_long",
            InputError::InPast => "error.past",
        })
    }
    pub(crate) fn service_error(self, error: &service::Error) -> String {
        match error {
            service::Error::NotRegistered => self.text("error.register"),
            service::Error::NotFound => self.text("error.missing"),
            service::Error::CompletionUnavailable => self.text("undo.unavailable"),
            service::Error::Input(error) => self.input_error(error),
            service::Error::InvalidPreferences(_) => self.text("error.preferences"),
            service::Error::Internal(_) => self.text("error.internal"),
        }
    }
    pub(crate) fn date(self, at: NaiveDateTime) -> String {
        if self == Self::En {
            return at.format("%a %-d %b %Y, %H:%M").to_string();
        }
        let weekdays = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
        let months = [
            "янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
        ];
        format!(
            "{} {} {} {}, {}",
            weekdays[at.weekday().num_days_from_monday() as usize],
            at.day(),
            months[at.month0() as usize],
            at.year(),
            at.format("%H:%M")
        )
    }
    pub(crate) fn recurrence(self, recurrence: Recurrence, at: NaiveDateTime) -> String {
        if self == Self::En {
            return recurrence.describe(at);
        }
        match recurrence {
            Recurrence::Once => "один раз".into(),
            Recurrence::Daily => "каждый день".into(),
            Recurrence::Weekdays => "по будням".into(),
            Recurrence::Weekly => format!(
                "каждую неделю ({})",
                ["пн", "вт", "ср", "чт", "пт", "сб", "вс"]
                    [at.weekday().num_days_from_monday() as usize]
            ),
            Recurrence::Monthly => format!("ежемесячно, {}-го", at.day()),
            Recurrence::Yearly => format!("каждый год, {}", at.format("%d.%m")),
        }
    }
}

fn lookup<'a>(
    catalogue: &'a BTreeMap<String, String>,
    fallback: &'a BTreeMap<String, String>,
    key: &'a str,
) -> &'a str {
    catalogue
        .get(key)
        .or_else(|| fallback.get(key))
        .map_or(key, String::as_str)
}

#[cfg(test)]
mod tests;
