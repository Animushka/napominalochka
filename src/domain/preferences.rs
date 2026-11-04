//! Supported preference values. Invalid intervals cannot enter application state.
use derive_more::{Display, FromStr, TryFrom};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, FromStr)]
#[serde(rename_all = "lowercase")]
#[from_str(rename_all = "lowercase")]
pub(crate) enum Language {
    #[default]
    #[display("ru")]
    Ru,
    #[display("en")]
    En,
}
impl Language {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }
    pub(crate) fn from_telegram(code: Option<&str>) -> Self {
        if code.is_some_and(|v| {
            v.eq_ignore_ascii_case("en") || v.to_ascii_lowercase().starts_with("en-")
        }) {
            Self::En
        } else {
            Self::Ru
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr, TryFrom)]
#[repr(i64)]
#[try_from(repr)]
pub(crate) enum NagInterval {
    OneMinute = 1,
    FiveMinutes = 5,
    #[default]
    TenMinutes = 10,
    TwentyMinutes = 20,
    ThirtyMinutes = 30,
    OneHour = 60,
}
impl NagInterval {
    pub(crate) const fn minutes(self) -> i64 {
        self as i64
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr, TryFrom)]
#[repr(i64)]
#[try_from(repr)]
pub(crate) enum ConfirmationDelay {
    #[default]
    Disabled = 0,
    TwentyMinutes = 20,
}
impl ConfirmationDelay {
    pub(crate) const fn minutes(self) -> i64 {
        self as i64
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfirmationDelay, Language, NagInterval};

    #[test]
    fn preference_wire_values_remain_compatible_and_reject_invalid_values() {
        for value in [1, 5, 10, 20, 30, 60] {
            let interval: NagInterval = serde_json::from_value(serde_json::json!(value)).unwrap();
            assert_eq!(interval.minutes(), value);
            assert_eq!(serde_json::to_value(interval).unwrap(), value);
        }
        for value in [0, 2, 99, -1] {
            assert!(serde_json::from_value::<NagInterval>(serde_json::json!(value)).is_err());
        }
        for value in [0, 20] {
            let delay: ConfirmationDelay =
                serde_json::from_value(serde_json::json!(value)).unwrap();
            assert_eq!(delay.minutes(), value);
            assert_eq!(serde_json::to_value(delay).unwrap(), value);
        }
        assert!(serde_json::from_value::<ConfirmationDelay>(serde_json::json!(10)).is_err());
        assert_eq!(serde_json::to_value(Language::En).unwrap(), "en");
        assert_eq!("ru".parse::<Language>().unwrap(), Language::Ru);
        assert!("EN".parse::<Language>().is_err());
        assert!("fr".parse::<Language>().is_err());
    }
}
