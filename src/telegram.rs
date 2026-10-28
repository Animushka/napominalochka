//! Telegram glue: turns updates into conversation calls and replies into API requests.

use std::sync::Arc;

use chrono::Utc;
use teloxide::adaptors::Throttle;
use teloxide::prelude::*;
use teloxide::types::{
    ButtonRequest, ChatKind, KeyboardButton, KeyboardMarkup, KeyboardRemove, MenuButton,
    ReplyMarkup, WebAppInfo,
};
use teloxide::utils::command::{BotCommands, ParseError};
use teloxide::{ApiError, RequestError};

use crate::commands::{self, BotInfo, Command};
use crate::conversation::{self, Markup, Reply, Sender};
use crate::i18n::Lang;
use crate::model::{Confirmation, Nagging};
use crate::panels;
use crate::scheduler::{Delivery, Notifier};
use crate::service::Service;

pub type Bot = Throttle<teloxide::Bot>;
pub type HandlerResult = anyhow::Result<()>;

pub struct AppState {
    pub service: Service,
    pub bot_info: BotInfo,
    pub yandex: Option<crate::yandex::Yandex>,
}

pub async fn handle_message(bot: Bot, msg: Message, state: Arc<AppState>) -> HandlerResult {
    let sender = sender_of(&msg);
    let now = Utc::now();
    let service = &state.service;
    service
        .initialize_language(
            sender.chat_id,
            msg.from
                .as_ref()
                .and_then(|user| user.language_code.as_deref()),
        )
        .await?;
    let lang = conversation::language(service, sender.chat_id).await?;
    if let Some(receipt) = msg
        .text()
        .and_then(|text| yandex_receipt(text, &state.bot_info.username))
    {
        if !sender.private
            || msg.forward_origin().is_some()
            || msg
                .from
                .as_ref()
                .is_none_or(|user| user.id.0 as i64 != sender.chat_id)
        {
            return Ok(());
        }
        let key = match &state.yandex {
            Some(integration) => match integration.confirm_auth(sender.chat_id, receipt).await {
                Ok(_) => "yandex.connected",
                Err(_) => "yandex.confirm_failed",
            },
            None => "yandex.unavailable",
        };
        let mut reply = Reply::text(lang.text(key));
        if let Some(url) = state.bot_info.web_app.as_ref() {
            reply.markup = Markup::Inline(teloxide::types::InlineKeyboardMarkup::new([[
                teloxide::types::InlineKeyboardButton::web_app(
                    lang.text("app.open"),
                    WebAppInfo { url: url.clone() },
                ),
            ]]));
        }
        send(&bot, msg.chat.id, reply, lang).await?;
        return Ok(());
    }
    let refresh_menu = sender.private
        && msg
            .text()
            .is_some_and(|text| text.starts_with("/start") || text.starts_with("/language"));

    let result = if let Some(location) = msg.location() {
        conversation::on_location(service, &sender, location.latitude, location.longitude, now)
            .await
    } else if let Some(text) = msg.text().or(msg.caption()) {
        let forwarded = msg.forward_origin().is_some();
        if text.starts_with('/') && !forwarded {
            match Command::parse(text, &state.bot_info.username) {
                Ok(command) => {
                    commands::execute(command, &sender, &state.bot_info, service, now).await
                }
                // Addressed to another bot in a group chat.
                Err(ParseError::WrongBotName(_)) => return Ok(()),
                Err(_) => Ok(vec![Reply::text(lang.text("command.unknown"))]),
            }
        } else if sender.private {
            conversation::on_text(service, &sender, text, forwarded, now).await
        } else {
            // In groups, only commands are handled.
            return Ok(());
        }
    } else {
        return Ok(());
    };

    let replies = result.unwrap_or_else(|error| {
        tracing::error!(?error, chat_id = sender.chat_id, "failed to handle message");
        vec![Reply::text(
            error
                .downcast_ref::<crate::service::Error>()
                .map(|error| lang.service_error(error))
                .unwrap_or_else(|| lang.text("error.internal")),
        )]
    });
    for reply in replies {
        send(&bot, msg.chat.id, reply, lang).await?;
    }
    if refresh_menu {
        let selected = conversation::language(service, sender.chat_id).await?;
        publish_chat_menu_button(&bot, msg.chat.id, state.bot_info.web_app.as_ref(), selected)
            .await;
    }
    Ok(())
}

fn yandex_receipt<'a>(text: &'a str, username: &str) -> Option<&'a str> {
    let mut parts = text.split_whitespace();
    let command = parts.next()?;
    if command != "/start" && command != format!("/start@{username}") {
        return None;
    }
    let receipt = parts.next()?.strip_prefix("yandex_")?;
    if parts.next().is_some()
        || receipt.len() != 48
        || !receipt.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    Some(receipt)
}

pub async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    state: Arc<AppState>,
) -> HandlerResult {
    let message = query.regular_message();
    let user_id = message.map_or(query.from.id.0 as i64, |m| m.chat.id.0);
    let data = query.data.as_deref().unwrap_or_default();
    state
        .service
        .initialize_language(user_id, query.from.language_code.as_deref())
        .await?;
    let lang = conversation::language(&state.service, user_id).await?;
    if let Some(code) = data
        .strip_prefix("lang:")
        .filter(|code| matches!(*code, "ru" | "en"))
    {
        state
            .service
            .set_preferences(user_id, Some(code), None, None)
            .await?;
        let selected = Lang::parse(code);
        bot.answer_callback_query(query.id.clone())
            .text(selected.text("language.saved"))
            .await?;
        let panel = commands::language_panel(selected);
        if let Some(message) = message {
            let request = bot.edit_message_text(message.chat.id, message.id, panel.text);
            if let Markup::Inline(keyboard) = panel.markup
                && let Err(error) = request.reply_markup(keyboard).await
            {
                tracing::debug!(?error, "could not refresh language picker");
            }
            if matches!(message.chat.kind, ChatKind::Private(_)) {
                publish_chat_menu_button(
                    &bot,
                    message.chat.id,
                    state.bot_info.web_app.as_ref(),
                    selected,
                )
                .await;
            }
        }
        return Ok(());
    }

    let reply = match conversation::on_callback(
        &state.service,
        user_id,
        data,
        message.and_then(|m| m.text()),
        state.bot_info.web_app.as_ref(),
        Utc::now(),
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) => {
            tracing::error!(?error, user_id, data, "failed to handle button");
            bot.answer_callback_query(query.id.clone())
                .text(lang.text("error.internal"))
                .await?;
            return Ok(());
        }
    };

    let mut answer = bot.answer_callback_query(query.id.clone());
    if let Some(toast) = reply.toast {
        answer = answer.text(toast);
    }
    answer.await?;

    if let (Some(message), Some(panel)) = (message, reply.edit) {
        let unchanged = message.text() == Some(panel.text.as_str())
            && message.reply_markup().map(|m| &m.inline_keyboard)
                == Some(&panel.keyboard.inline_keyboard);
        if !unchanged
            && let Err(error) = bot
                .edit_message_text(message.chat.id, message.id, panel.text)
                .reply_markup(panel.keyboard)
                .await
        {
            tracing::debug!(?error, "could not edit message");
        }
    }
    Ok(())
}

async fn send(bot: &Bot, chat_id: ChatId, reply: Reply, lang: Lang) -> Result<(), RequestError> {
    let request = bot.send_message(chat_id, reply.text);
    let request = match reply.markup {
        Markup::None => request,
        Markup::Inline(keyboard) => request.reply_markup(keyboard),
        Markup::AskLocation => request.reply_markup(ReplyMarkup::Keyboard(
            KeyboardMarkup::new([[
                KeyboardButton::new(lang.text("location.share")).request(ButtonRequest::Location)
            ]])
            .resize_keyboard()
            .one_time_keyboard(),
        )),
        Markup::RemoveKeyboard => {
            request.reply_markup(ReplyMarkup::KeyboardRemove(KeyboardRemove::new()))
        }
    };
    request.await?;
    Ok(())
}

/// Sends reminders as Telegram messages with Done/Snooze buttons.
pub struct TelegramNotifier(pub Bot, pub Service);

impl Notifier for TelegramNotifier {
    async fn send(&self, nagging: &Nagging) -> Delivery {
        let lang = self
            .1
            .preferences(nagging.user_id)
            .await
            .map(|p| Lang::parse(&p.language))
            .unwrap_or_default();
        let result = self
            .0
            .send_message(
                ChatId(nagging.user_id),
                format!("⏰ {}", nagging.description),
            )
            .reply_markup(panels::nag_keyboard_lang(nagging.id, lang))
            .await;
        match result {
            Ok(_) => Delivery::Sent,
            Err(RequestError::Api(error)) if is_unreachable(&error) => Delivery::Unreachable,
            Err(error) => Delivery::Failed(error.to_string()),
        }
    }
    async fn send_confirmation(&self, confirmation: &Confirmation) -> Delivery {
        let lang = self
            .1
            .preferences(confirmation.user_id)
            .await
            .map(|p| Lang::parse(&p.language))
            .unwrap_or_default();
        let text = lang.render(
            "confirmation",
            &[("description", confirmation.description.clone())],
        );
        match self
            .0
            .send_message(ChatId(confirmation.user_id), text)
            .await
        {
            Ok(_) => Delivery::Sent,
            Err(RequestError::Api(error)) if is_unreachable(&error) => Delivery::Unreachable,
            Err(error) => Delivery::Failed(error.to_string()),
        }
    }
}

fn is_unreachable(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::BotBlocked
            | ApiError::BotKicked
            | ApiError::BotKickedFromSupergroup
            | ApiError::ChatNotFound
            | ApiError::UserDeactivated
            | ApiError::CantInitiateConversation
    )
}

fn sender_of(msg: &Message) -> Sender {
    let (first_name, last_name) = match &msg.from {
        Some(user) => (
            user.first_name.clone(),
            user.last_name.clone().unwrap_or_default(),
        ),
        None => (msg.chat.title().unwrap_or("👋").to_owned(), String::new()),
    };
    Sender {
        chat_id: msg.chat.id.0,
        first_name,
        last_name,
        private: matches!(msg.chat.kind, ChatKind::Private(_)),
    }
}

/// Sets the button next to the message box: the Mini App if configured, otherwise the
/// command list.
pub async fn publish_menu_button(bot: &Bot, web_app_url: Option<&url::Url>) {
    let button = match web_app_url {
        Some(url) => MenuButton::WebApp {
            text: Lang::Ru.text("app.menu"),
            web_app: WebAppInfo { url: url.clone() },
        },
        None => MenuButton::Commands,
    };
    if let Err(error) = bot.set_chat_menu_button().menu_button(button).await {
        tracing::warn!(?error, "failed to set the menu button");
    }
}

async fn publish_chat_menu_button(
    bot: &Bot,
    chat_id: ChatId,
    web_app: Option<&url::Url>,
    lang: Lang,
) {
    let button = match web_app {
        Some(url) => MenuButton::WebApp {
            text: lang.text("app.menu"),
            web_app: WebAppInfo { url: url.clone() },
        },
        None => MenuButton::Commands,
    };
    if let Err(error) = bot
        .set_chat_menu_button()
        .chat_id(chat_id)
        .menu_button(button)
        .await
    {
        tracing::warn!(?error, "failed to set localized menu button");
    }
}

/// Publishes the default Russian command menu and explicit RU/EN translations.
pub async fn publish_commands(bot: &Bot) {
    if let Err(error) = bot
        .set_my_commands(commands::localized_commands(Lang::Ru))
        .await
    {
        tracing::warn!(?error, "failed to publish the default command list");
    }
    for lang in [Lang::Ru, Lang::En] {
        if let Err(error) = bot
            .set_my_commands(commands::localized_commands(lang))
            .language_code(lang.code())
            .await
        {
            tracing::warn!(?error, "failed to publish a localized command list");
        }
    }
}

#[cfg(test)]
mod yandex_link_tests {
    use super::yandex_receipt;

    #[test]
    fn accepts_only_exact_bot_start_receipts() {
        let receipt = "a".repeat(48);
        assert_eq!(
            yandex_receipt(&format!("/start yandex_{receipt}"), "our_bot"),
            Some(receipt.as_str())
        );
        assert_eq!(
            yandex_receipt(&format!("/start@our_bot yandex_{receipt}"), "our_bot"),
            Some(receipt.as_str())
        );
        for text in [
            format!("/start@other_bot yandex_{receipt}"),
            format!("/start yandex_{receipt} extra"),
            "/start yandex_nothex".into(),
            "/start".into(),
        ] {
            assert_eq!(yandex_receipt(&text, "our_bot"), None);
        }
    }
}
