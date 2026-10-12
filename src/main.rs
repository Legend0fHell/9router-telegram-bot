use nine_router_bot::alerts::AlertEngine;
use nine_router_bot::bot::{armed_message, handle_command, send_formatted, Command};
use nine_router_bot::client::NineRouterClient;
use nine_router_bot::config::Config;
use nine_router_bot::models::{Connection, UsageResponse};
use std::sync::Arc;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::ChatId;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,nine_router_bot=debug".into()),
        )
        .init();

    info!("Starting 9router Telegram Bot...");

    let config = Arc::new(Config::from_env()?);
    let client = Arc::new(NineRouterClient::new(
        config.router_url.clone(),
        config.router_password.clone(),
        config.router_api_key.clone(),
    )?);

    let bot = Bot::new(&config.teloxide_token);
    send_formatted(
        &bot,
        ChatId(config.telegram_chat_id),
        &armed_message(config.poll_interval_secs),
        &config.router_url,
    )
    .await?;

    // Spawn periodic alert monitor loop
    let monitor_bot = bot.clone();
    let monitor_client = Arc::clone(&client);
    let monitor_config = Arc::clone(&config);

    tokio::spawn(async move {
        run_monitor_loop(monitor_bot, monitor_client, monitor_config).await;
    });

    tokio::spawn(run_health_loop(
        bot.clone(),
        Arc::clone(&client),
        Arc::clone(&config),
    ));

    let daily_bot = bot.clone();
    let daily_client = Arc::clone(&client);
    let daily_config = Arc::clone(&config);
    tokio::spawn(async move {
        loop {
            let due = nine_router_bot::leaderboard::next_midnight(chrono::Utc::now());
            let delay = (due - chrono::Utc::now()).to_std().unwrap_or_default();
            tokio::time::sleep(delay).await;
            match daily_client.fetch_usage_stats().await {
                Ok(stats) => {
                    let report = nine_router_bot::leaderboard::report(stats, chrono::Utc::now());
                    if let Err(e) = send_formatted(
                        &daily_bot,
                        ChatId(daily_config.telegram_chat_id),
                        &report,
                        &daily_config.router_url,
                    )
                    .await
                    {
                        error!("Daily leaderboard delivery failed: {e}");
                    }
                }
                Err(_) => error!("Daily leaderboard fetch failed; report not sent"),
            }
        }
    });
    info!("Setting up Telegram bot command dispatcher...");
    let handler = Update::filter_message()
        .filter_command::<Command>()
        .endpoint(handle_command);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![client, config])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn run_health_loop(bot: Bot, client: Arc<NineRouterClient>, config: Arc<Config>) {
    let mut engine = AlertEngine::new();
    loop {
        let health = client.check_v1_health().await;
        for alert in engine.check_v1_health(&health) {
            if let Err(e) = send_formatted(
                &bot,
                ChatId(config.telegram_chat_id),
                &alert,
                &config.router_url,
            )
            .await
            {
                error!("Failed to deliver health alert: {e}");
            }
        }
        tokio::time::sleep(engine.health_poll_delay()).await;
    }
}

async fn run_monitor_loop(bot: Bot, client: Arc<NineRouterClient>, config: Arc<Config>) {
    let mut engine = AlertEngine::new();
    let chat_id = ChatId(config.telegram_chat_id);
    let interval = Duration::from_secs(config.poll_interval_secs);

    info!(
        "Alert monitor loop running (interval: {}s, target chat: {})",
        config.poll_interval_secs, config.telegram_chat_id
    );

    loop {
        let mut alerts = Vec::new();

        // Refresh before expiry even when no dashboard command is running.
        let login_failed = match client.refresh_session().await {
            Ok(()) => false,
            Err(e) => {
                warn!("Dashboard session refresh failed: {e}");
                true
            }
        };
        alerts.extend(engine.check_login_failure(login_failed));

        // 3. Check for 9router updates
        match client.fetch_version().await {
            Ok(ver) => {
                alerts.extend(engine.check_update(&ver));
            }
            Err(e) => {
                warn!("Failed to fetch 9router version: {e}");
            }
        }

        // 4. Fetch connections
        match client.fetch_connections().await {
            Ok(conns) => {
                // Check Codex accounts for 401/403 errors (ignoring 429)
                alerts.extend(engine.check_codex_errors(&conns));

                // Check quota & banked resets for active connections
                let mut usage_batch = Vec::new();
                for conn in conns.iter().filter(|c| c.is_active.unwrap_or(true)) {
                    match client.fetch_usage(&conn.id).await {
                        Ok(usage) => {
                            usage_batch.push((conn, usage));
                        }
                        Err(e) => {
                            warn!(
                                "Failed to fetch usage for {} ({}): {e}",
                                conn.display_name(),
                                conn.id
                            );
                        }
                    }
                }

                let refs: Vec<(&Connection, &UsageResponse)> =
                    usage_batch.iter().map(|(c, u)| (*c, u)).collect();
                alerts.extend(engine.check_weekly_quotas(refs.iter().copied(), chrono::Utc::now()));
                alerts.extend(engine.check_banked_resets(refs));
            }
            Err(e) => {
                error!("Monitor loop failed to fetch connections: {e}");
            }
        }

        for alert in alerts {
            if let Err(e) = send_formatted(&bot, chat_id, &alert, &config.router_url).await {
                error!("Failed to deliver alert to Telegram chat {}: {e}", chat_id);
            }
        }

        tokio::time::sleep(interval).await;
    }
}
