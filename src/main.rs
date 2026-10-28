mod calendar;
mod callback;
mod commands;
mod config;
mod conversation;
mod db;
mod i18n;
mod model;
mod panels;
mod scheduler;
mod service;
mod telegram;
mod time_input;
mod web;

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use teloxide::adaptors::throttle::Limits;
use teloxide::prelude::*;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use crate::commands::BotInfo;
use crate::config::Config;
use crate::db::Db;
use crate::service::Service;
use crate::telegram::AppState;

fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Configuration error: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    // Sentry must be initialised before the async runtime starts.
    let _sentry = config.sentry_dsn.as_deref().map(|dsn| {
        let mut options = sentry::ClientOptions::default();
        options.release = sentry::release_name!();
        sentry::init((dsn, options))
    });

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(sentry_tracing::layer())
        .init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(?error, "failed to start async runtime");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "fatal error");
            ExitCode::FAILURE
        }
    }
}

async fn run(config: Config) -> anyhow::Result<()> {
    let db = Db::open(&config.database_path)?;
    tracing::info!(path = %config.database_path.display(), "database ready");

    let bot = teloxide::Bot::new(&config.telegram_token).throttle(Limits::default());
    let me = bot.get_me().await.context("connecting to Telegram")?;
    let bot_info = BotInfo {
        name: "Напоминалочка".into(),
        username: me.username().to_owned(),
        web_app: config.web_app_url.clone(),
    };
    tracing::info!(id = %me.id, username = %bot_info.username, "connected to Telegram");

    telegram::publish_commands(&bot).await;
    telegram::publish_menu_button(&bot, config.web_app_url.as_ref()).await;
    if let Some(chat_id) = config.admin_chat_id
        && let Err(error) = bot
            .send_message(
                ChatId(chat_id),
                "Напоминалочка запущена / Napominalochka is online!",
            )
            .await
    {
        tracing::warn!(?error, "failed to notify admin chat");
    }

    let shutdown = CancellationToken::new();
    let service = Service::new(db.clone());
    let scheduler = tokio::spawn(scheduler::run(
        telegram::TelegramNotifier(bot.clone(), service.clone()),
        db,
        shutdown.clone(),
    ));
    let web_state = Arc::new(web::WebState {
        service: service.clone(),
        bot_token: config.telegram_token.clone(),
        web_app_url: config.web_app_url.clone(),
    });
    let web_server = tokio::spawn(web::serve(config.web_listen, web_state, shutdown.clone()));

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(telegram::handle_message))
        .branch(Update::filter_callback_query().endpoint(telegram::handle_callback));

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![Arc::new(AppState { service, bot_info })])
        .default_handler(|_| async {})
        .error_handler(LoggingErrorHandler::with_custom_text(
            "error while handling an update",
        ))
        .build();

    let dispatcher_token = dispatcher.shutdown_token();
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutting down");
        signal_shutdown.cancel();
        if let Ok(done) = dispatcher_token.shutdown() {
            done.await;
        }
    });

    dispatcher.dispatch().await;
    shutdown.cancel();
    scheduler.await.context("scheduler task panicked")?;
    web_server.await.context("web server task panicked")??;
    Ok(())
}

/// Resolves on Ctrl+C or, on Unix, SIGTERM (what `docker stop` sends).
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(?error, "failed to listen for Ctrl+C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(?error, "failed to listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
