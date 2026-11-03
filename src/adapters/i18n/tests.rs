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
        include_str!("../telegram/commands.rs"),
        include_str!("../telegram/conversation.rs"),
        include_str!("../telegram/panels.rs"),
        include_str!("../telegram/mod.rs"),
        include_str!("../calendar.rs"),
    ] {
        for captures in keys.captures_iter(source) {
            let key = &captures[1];
            assert!(EN.contains_key(key), "missing English text: {key}");
            assert!(RU.contains_key(key), "missing Russian text: {key}");
        }
    }
}
