//! Slash commands. Most things are done with buttons; commands are entry points.

use std::sync::LazyLock;

use chrono::{DateTime, NaiveDateTime, Utc};
use regex::Regex;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, WebAppInfo};
use teloxide::utils::command::BotCommands;
use url::Url;

use crate::conversation::{self, Markup, Reply, Sender};
use crate::i18n::Lang;
use crate::model::Recurrence;
use crate::panels;
use crate::service::{self, ReminderInput, Service};

#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase", description = "Commands:")]
pub enum Command {
    #[command(hide)]
    Start,
    #[command(description = "how to use this bot.")]
    Help,
    #[command(description = "add a reminder, e.g. /remindme call mum.")]
    RemindMe(String),
    #[command(description = "see, change or delete your reminders.", aliases = ["listreminders"], hide_aliases)]
    List,
    #[command(description = "show or change your timezone, e.g. /timezone London.", aliases = ["register"], hide_aliases)]
    Timezone(String),
    #[command(
        description = "get a link to see your reminders in your phone's calendar, with offline alarms."
    )]
    Calendar,
    #[command(description = "delete all your reminders.", aliases = ["removeallreminders"], hide_aliases)]
    ClearAll,
    #[command(description = "delete all your data.")]
    Unregister,
    #[command(description = "about this bot.")]
    About,
    #[command(description = "choose Russian or English.")]
    Language(String),
    /// Kept for users of the old text interface; reminders are now deleted from /list.
    #[command(hide)]
    RemoveReminder(String),
}

pub struct BotInfo {
    pub name: String,
    pub username: String,
    /// Mini App address, if configured.
    pub web_app: Option<Url>,
}

impl BotInfo {
    /// A button opening the Mini App. Telegram only allows these in private chats.
    fn app_button(&self, sender: &Sender, lang: Lang) -> Option<InlineKeyboardButton> {
        self.web_app.as_ref().filter(|_| sender.private).map(|url| {
            InlineKeyboardButton::web_app(lang.text("app.open"), WebAppInfo { url: url.clone() })
        })
    }

    /// Adds the Mini App button to a reply, keeping any inline buttons it already has.
    fn with_app_button(&self, sender: &Sender, mut reply: Reply, lang: Lang) -> Reply {
        let Some(button) = self.app_button(sender, lang) else {
            return reply;
        };
        reply.markup = match reply.markup {
            Markup::Inline(keyboard) => Markup::Inline(keyboard.append_row([button])),
            Markup::None => Markup::Inline(InlineKeyboardMarkup::new([[button]])),
            other => other,
        };
        reply
    }
}

pub async fn execute(
    command: Command,
    sender: &Sender,
    bot: &BotInfo,
    service: &Service,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Reply>> {
    let name = &sender.first_name;
    let lang = conversation::language(service, sender.chat_id).await?;
    let user = service.user(sender.chat_id).await?;
    let brand = if lang == Lang::En || bot.name.trim().is_empty() {
        lang.text("brand")
    } else {
        bot.name.clone()
    };
    let greeting = |key: &str| {
        lang.render(
            key,
            &[
                ("name", name.clone()),
                ("brand", brand.clone()),
                ("usage", usage(sender, lang)),
            ],
        )
    };
    Ok(match command {
        Command::Start => match user {
            Some(_) => {
                vec![bot.with_app_button(sender, Reply::text(greeting("start.returning")), lang)]
            }
            None if bot.app_button(sender, lang).is_some() => {
                vec![bot.with_app_button(sender, Reply::text(greeting("start.app")), lang)]
            }
            None => vec![conversation::ask_timezone_lang(
                sender,
                &greeting("start.timezone"),
                lang,
            )],
        },
        Command::Help => vec![bot.with_app_button(
            sender,
            Reply::text(format!(
                "{}\n\n{}",
                usage(sender, lang),
                lang.text("help.commands")
            )),
            lang,
        )],
        Command::About => vec![Reply::text(lang.text("about"))],
        Command::Language(code) => {
            let code = code.trim().to_ascii_lowercase();
            if code.is_empty() {
                vec![language_panel(lang)]
            } else if matches!(code.as_str(), "ru" | "en") {
                service
                    .set_preferences(sender.chat_id, Some(&code), None, None)
                    .await?;
                let selected = Lang::parse(&code);
                vec![
                    Reply::text(selected.text("language.saved")),
                    language_panel(selected),
                ]
            } else {
                vec![
                    Reply::text(lang.text("language.usage")),
                    language_panel(lang),
                ]
            }
        }
        Command::RemindMe(text) if text.trim().is_empty() => {
            vec![Reply::text(lang.text("remind.empty"))]
        }
        Command::RemindMe(text) => match (&user, legacy_reminder(&text)) {
            (Some(user), Some((description, at, recurrence))) => {
                let input = ReminderInput {
                    description: &description,
                    at,
                    recurrence,
                };
                match service.create(user, input, now).await {
                    Ok(reminder) => vec![
                        panels::saved_lang(
                            &reminder,
                            user,
                            now,
                            &lang.text("reminder.saved"),
                            lang,
                        )
                        .into(),
                    ],
                    Err(service::Error::Input(error)) => {
                        vec![Reply::text(lang.input_error(&error))]
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            _ => conversation::start_reminder_for(service, sender, &text, now).await?,
        },
        Command::List => match user {
            Some(user) => {
                let reminders = service.list(user.id).await?;
                vec![bot.with_app_button(
                    sender,
                    panels::list_lang(&reminders, &user, lang).into(),
                    lang,
                )]
            }
            None => vec![not_registered(sender, lang)],
        },
        Command::Timezone(arg) if arg.trim().is_empty() => {
            let intro = match &user {
                Some(user) => lang.render("timezone.current", &[("timezone", user.tz.to_string())]),
                None => lang.text("timezone.ask"),
            };
            vec![conversation::ask_timezone_lang(sender, &intro, lang)]
        }
        Command::Timezone(arg) => match crate::model::UserTz::parse(&arg) {
            Ok(tz) => conversation::set_timezone(service, sender, tz, now).await?,
            Err(error) => vec![conversation::ask_timezone_lang(
                sender,
                &lang.input_error(&error),
                lang,
            )],
        },
        Command::Calendar => vec![
            conversation::calendar_link(service, sender.chat_id, bot.web_app.as_ref(), false)
                .await?
                .into(),
        ],
        Command::ClearAll => match service.delete_all(sender.chat_id).await? {
            0 => vec![Reply::text(lang.text("reminder.empty"))],
            1 => vec![Reply::text(lang.text("reminder.deleted_one"))],
            n => vec![Reply::text(
                lang.render("reminder.deleted_many", &[("count", n.to_string())]),
            )],
        },
        Command::Unregister => {
            let removed = service.unregister(sender.chat_id).await?;
            vec![Reply::text(if removed {
                lang.render("unregister.done", &[("name", name.clone())])
            } else {
                lang.text("unregister.missing")
            })]
        }
        Command::RemoveReminder(_) => vec![Reply::text(lang.text("remind.manage_hint"))],
    })
}

pub fn language_panel(lang: Lang) -> Reply {
    Reply {
        text: lang.text("language.choose"),
        markup: Markup::Inline(InlineKeyboardMarkup::new([[
            InlineKeyboardButton::callback("🇷🇺 Русский", "lang:ru"),
            InlineKeyboardButton::callback("🇬🇧 English", "lang:en"),
        ]])),
    }
}

/// Command labels are published separately for Telegram's RU/EN menus.
pub fn localized_commands(lang: Lang) -> Vec<teloxide::types::BotCommand> {
    [
        "help",
        "remindme",
        "list",
        "timezone",
        "language",
        "calendar",
        "clearall",
        "unregister",
        "about",
    ]
    .into_iter()
    .map(|command| {
        teloxide::types::BotCommand::new(command, lang.text(&format!("command.{command}")))
    })
    .collect()
}

/// The one-line format of earlier versions:
/// `/remindme <description> <dd/mm/yyyy> <HH:MM> [once|daily|weekly|monthly]`.
fn legacy_reminder(args: &str) -> Option<(String, NaiveDateTime, Recurrence)> {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)^\s*(?P<description>.+?)\s+(?P<at>\d{1,2}/\d{1,2}/\d{4}\s+\d{1,2}:\d{2})(?:\s+(?P<recurrence>once|daily|weekly|monthly))?\s*$",
        )
        .expect("valid regex")
    });
    let caps = PATTERN.captures(args)?;
    let at = NaiveDateTime::parse_from_str(
        &caps["at"].split_whitespace().collect::<Vec<_>>().join(" "),
        "%d/%m/%Y %H:%M",
    )
    .ok()?;
    let recurrence = caps
        .name("recurrence")
        .map_or(Some(Recurrence::Once), |m| m.as_str().parse().ok())?;
    Some((caps["description"].trim().to_owned(), at, recurrence))
}

fn usage(sender: &Sender, lang: Lang) -> String {
    lang.text(if sender.private {
        "usage.private"
    } else {
        "usage.group"
    })
}

fn not_registered(sender: &Sender, lang: Lang) -> Reply {
    conversation::ask_timezone_lang(sender, &lang.text("timezone.no_reminders"), lang)
}

#[cfg(test)]
mod tests {
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
        let service = Service::new(crate::db::Db::open_in_memory().unwrap());
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
}
