mod application;
mod calendar;
mod callback;
mod commands;
mod config;
mod conversation;
mod db;
mod delivery;
mod frontend;
mod health;
mod i18n;
mod model;
mod panels;
mod scheduler;
mod service;
mod supervisor;
mod telegram;
mod time_input;
mod web;
mod yandex;
mod yandex_web;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

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

    let result = runtime.block_on(run(config));
    runtime.shutdown_timeout(Duration::from_secs(2));
    match result {
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
    let yandex = config
        .yandex
        .map(|settings| yandex::Yandex::new(settings, db.clone()))
        .transpose()
        .context("configuring Yandex integration")?;
    let health = health::Health::new();
    let mut supervisor = supervisor::Supervisor::new(Duration::from_secs(10));
    if let Some(integration) = yandex.clone() {
        let shutdown = shutdown.clone();
        supervisor.spawn("yandex", async move {
            integration.run_worker(shutdown).await;
            Ok(())
        });
    }
    let planner_shutdown = shutdown.clone();
    let heartbeat = health.heartbeat();
    let notifier = telegram::TelegramNotifier(bot.clone(), service.clone());
    supervisor.spawn("planner and delivery", async move {
        scheduler::run_monitored(notifier, db, planner_shutdown, heartbeat).await;
        Ok(())
    });
    let web_state = Arc::new(web::WebState {
        service: service.clone(),
        bot_token: config.telegram_token.clone(),
        bot_username: bot_info.username.clone(),
        web_app_url: config.web_app_url.clone(),
        yandex: yandex.clone(),
        health: health.clone(),
    });
    supervisor.spawn(
        "web",
        web::serve(config.web_listen, web_state, shutdown.clone()),
    );

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(telegram::handle_message))
        .branch(Update::filter_callback_query().endpoint(telegram::handle_callback));

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![Arc::new(AppState {
            service,
            bot_info,
            yandex
        })])
        .default_handler(|_| async {})
        .error_handler(LoggingErrorHandler::with_custom_text(
            "error while handling an update",
        ))
        .build();

    let dispatcher_token = dispatcher.shutdown_token();
    supervisor.spawn("telegram", async move {
        dispatcher.dispatch().await;
        Ok(())
    });
    supervisor
        .run_until(wait_for_shutdown_signal(), shutdown, || async move {
            health.stop();
            tracing::info!("shutting down");
            if let Ok(done) = dispatcher_token.shutdown() {
                done.await;
            }
        })
        .await
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
