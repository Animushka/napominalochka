use super::*;

#[test]
fn parses_commands_and_old_names() {
    assert_eq!(
        Command::parse("/start", "naggybot").unwrap(),
        Command::Start
    );
    assert_eq!(
        Command::parse("/list@naggybot", "naggybot").unwrap(),
        Command::List
    );
    assert_eq!(
        Command::parse("/listreminders", "naggybot").unwrap(),
        Command::List
    );
    assert_eq!(
        Command::parse("/register +8", "naggybot").unwrap(),
        Command::Timezone("+8".into())
    );
    assert_eq!(
        Command::parse("/remindme pay rent", "naggybot").unwrap(),
        Command::RemindMe("pay rent".into())
    );
    assert_eq!(
        Command::parse("/removeallreminders", "naggybot").unwrap(),
        Command::ClearAll
    );
    assert!(Command::parse("/unknown", "naggybot").is_err());
}

#[test]
fn parses_legacy_one_line_reminders() {
    let (description, at, recurrence) =
        legacy_reminder("Take my pills 25/12/2026 08:00 Daily").unwrap();
    assert_eq!(description, "Take my pills");
    assert_eq!(at.to_string(), "2026-12-25 08:00:00");
    assert_eq!(recurrence, Recurrence::Daily);
    assert_eq!(
        legacy_reminder("dentist 2/11/2026 9:30").unwrap().2,
        Recurrence::Once
    );
    assert!(legacy_reminder("call mum").is_none());
    assert!(legacy_reminder("x 31/02/2027 08:00").is_none());
}

#[test]
fn help_hides_old_names() {
    let help = Command::descriptions().to_string();
    assert!(help.contains("/list"));
    assert!(!help.contains("listreminders"));
    assert!(!help.contains("removereminder"));
}
#[tokio::test]
async fn language_choice_persists_before_registration_and_localizes_onboarding() {
    let service =
        ReminderService::new(crate::infrastructure::sqlite::Db::open_in_memory().unwrap());
    let sender = Sender {
        chat_id: 71,
        first_name: "Ada".into(),
        last_name: String::new(),
        private: true,
    };
    let bot = BotInfo {
        name: "Напоминалочка".into(),
        username: "napominalozhka_bot".into(),
        web_app: Some(Url::parse("https://example.test/").unwrap()),
    };
    let replies = execute(Command::Start, &sender, &bot, &service, Utc::now())
        .await
        .unwrap();
    assert!(replies[0].text.contains("Привет, Ada!"));
    let replies = execute(
        Command::Language("en".into()),
        &sender,
        &bot,
        &service,
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(replies[0].text.contains("Language saved: English"));
    assert_eq!(service.preferences(71).await.unwrap().language, "en");
    assert!(service.user(71).await.unwrap().is_none());
    let replies = execute(Command::Start, &sender, &bot, &service, Utc::now())
        .await
        .unwrap();
    assert!(replies[0].text.contains("Hello Ada, I'm Napominalochka"));
    execute(
        Command::Language("ru".into()),
        &sender,
        &bot,
        &service,
        Utc::now(),
    )
    .await
    .unwrap();
    let replies = execute(Command::Help, &sender, &bot, &service, Utc::now())
        .await
        .unwrap();
    assert!(replies[0].text.contains("создать напоминание"));
    assert!(replies[0].text.contains("/language"));
    assert_eq!(service.preferences(71).await.unwrap().language, "ru");
}
