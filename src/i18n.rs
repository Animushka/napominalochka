//! Explicit RU/EN catalogues shared by chat, calendar and notification presentation.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::model::{InputError, Recurrence};
use crate::service;
use chrono::{Datelike, NaiveDateTime};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    Ru,
    En,
}

static RU: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../locales/bot.ru.json")).expect("valid Russian catalogue")
});
static EN: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../locales/bot.en.json")).expect("valid English catalogue")
});

impl Lang {
    pub fn parse(code: &str) -> Self {
        if code == "en" { Self::En } else { Self::Ru }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }
    pub fn text(self, key: &str) -> String {
        lookup(if self == Self::Ru { &RU } else { &EN }, &EN, key).to_owned()
    }
    pub fn render(self, key: &str, values: &[(&str, String)]) -> String {
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
    pub fn input_error(self, error: &InputError) -> String {
        self.text(match error {
            InputError::InvalidTimezone => "error.timezone",
            InputError::InvalidRecurrence => "error.recurrence",
            InputError::EmptyDescription => "error.empty",
            InputError::InPast => "error.past",
        })
    }
    pub fn service_error(self, error: &service::Error) -> String {
        match error {
            service::Error::NotRegistered => self.text("error.register"),
            service::Error::NotFound => self.text("error.missing"),
            service::Error::Input(error) => self.input_error(error),
            service::Error::InvalidPreferences(_) => self.text("error.preferences"),
            service::Error::Internal(_) => self.text("error.internal"),
        }
    }
    pub fn date(self, at: NaiveDateTime) -> String {
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
    pub fn recurrence(self, recurrence: Recurrence, at: NaiveDateTime) -> String {
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
mod tests {
    use super::*;
    #[test]
    fn catalogue_keys_match_and_fallback_is_explicit() {
        assert_eq!(RU.keys().collect::<Vec<_>>(), EN.keys().collect::<Vec<_>>());
        assert!(RU.values().all(|value| !value.trim().is_empty()));
        assert!(EN.values().all(|value| !value.trim().is_empty()));
        let placeholders = regex::Regex::new(r"\{([a-z_]+)\}").unwrap();
        for (key, russian) in RU.iter() {
            let tokens = |value: &str| {
                placeholders
                    .captures_iter(value)
                    .map(|m| m[1].to_owned())
                    .collect::<std::collections::BTreeSet<_>>()
            };
            assert_eq!(
                tokens(russian),
                tokens(&EN[key]),
                "placeholder mismatch in {key}"
            );
        }
        let empty = BTreeMap::new();
        assert_eq!(lookup(&empty, &EN, "error.internal"), EN["error.internal"]);
        assert_eq!(lookup(&empty, &EN, "missing.key"), "missing.key");
        assert_eq!(Lang::parse("xx"), Lang::Ru);
    }
    #[test]
    fn every_static_chat_key_exists_in_both_catalogues() {
        let keys = regex::Regex::new(r#"\.(?:text|render)\(\s*"([^"\n]+)""#).unwrap();
        for source in [
            include_str!("commands.rs"),
            include_str!("conversation.rs"),
            include_str!("panels.rs"),
            include_str!("telegram.rs"),
            include_str!("calendar.rs"),
        ] {
            for captures in keys.captures_iter(source) {
                let key = &captures[1];
                assert!(EN.contains_key(key), "missing English text: {key}");
                assert!(RU.contains_key(key), "missing Russian text: {key}");
            }
        }
    }
}
