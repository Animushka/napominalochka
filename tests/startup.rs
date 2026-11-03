use std::process::Command;

#[test]
fn missing_configuration_exits_before_starting_services() {
    let output = Command::new(env!("CARGO_BIN_EXE_extremely-naggy-bot"))
        .env_remove("TELEGRAM_BOT_TOKEN")
        .output()
        .expect("start the application binary");

    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
    assert!(error.contains("loading configuration"));
    assert!(error.contains("TELEGRAM_BOT_TOKEN environment variable is required"));
}
