use crate::client::{HealthStatus, NineRouterClient};
use crate::config::Config;
use crate::models::Connection;
use chrono::Utc;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Display available commands.")]
    Help,
    #[command(hide)]
    Start,
    #[command(description = "Overall status of 9router and accounts.")]
    Status,
    #[command(description = "Account quotas and time to reset.")]
    Quota,
    #[command(description = "List registered accounts.")]
    Accounts,
    #[command(description = "Codex accounts and banked reset credits.")]
    Codex,
    #[command(description = "Ping /v1 endpoint.")]
    Ping,
}

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    client: Arc<NineRouterClient>,
    config: Arc<Config>,
) -> ResponseResult<()> {
    if !allowed_chat(msg.chat.id, &config) {
        return Ok(());
    }

    let response = match cmd {
        Command::Help | Command::Start => Command::descriptions().to_string(),
        Command::Ping => {
            let start = std::time::Instant::now();
            let health = client.check_v1_health().await;
            format!(
                "/v1: {}\nElapsed: {}ms",
                health_text(health),
                start.elapsed().as_millis()
            )
        }
        Command::Status => {
            let health = health_text(client.check_v1_health().await);
            let expiry = config
                .auth_token_hours_remaining(Utc::now())
                .map(|h| format!("{:.1} hours remaining (unverified JWT claim)", h.max(0.0)))
                .unwrap_or_else(|| "unknown".into());
            match client.fetch_connections().await {
                Ok(conns) => format!(
                    "9router status\n/v1: {health}\nDashboard API: authenticated\nToken expiry: {expiry}\nAccounts: {} ({} enabled)\nCodex 401/403: {}\nRouter: {}",
                    conns.len(),
                    conns.iter().filter(|c| c.is_active.unwrap_or(true)).count(),
                    conns.iter().filter(|c| c.is_codex_auth_error()).count(),
                    config.router_url
                ),
                Err(e) => format!("9router status\n/v1: {health}\nToken expiry: {expiry}\nDashboard API error: {e}"),
            }
        }
        Command::Accounts => match client.fetch_connections().await {
            Ok(conns) => format_accounts_list(&conns),
            Err(e) => format!("Failed to retrieve accounts: {e}"),
        },
        Command::Codex => match client.fetch_connections().await {
            Ok(conns) => {
                let mut lines = vec!["Codex accounts:".to_string()];
                for c in conns
                    .iter()
                    .filter(|c| c.provider.eq_ignore_ascii_case("codex"))
                {
                    let credits = match client.fetch_usage(&c.id).await {
                        Ok(usage) => usage
                            .reset_credits
                            .and_then(|rc| rc.available_count)
                            .map(|n| format!("Banked resets: {n}"))
                            .unwrap_or_else(|| "Banked resets: unknown".into()),
                        Err(e) => format!("Usage unavailable: {e}"),
                    };
                    lines.push(format!(
                        "{} ({}) — {}\n{credits}",
                        c.display_name(),
                        c.email.as_deref().unwrap_or("no email"),
                        account_status(c)
                    ));
                }
                if lines.len() == 1 {
                    "No Codex accounts found.".into()
                } else {
                    lines.join("\n")
                }
            }
            Err(e) => format!("Failed to retrieve Codex accounts: {e}"),
        },
        Command::Quota => match client.fetch_connections().await {
            Ok(conns) => {
                let mut lines = vec!["Active account quotas:".to_string()];
                for c in conns.iter().filter(|c| c.is_active.unwrap_or(true)) {
                    lines.push(format!("\n{} ({})", c.display_name(), c.provider));
                    match client.fetch_usage(&c.id).await {
                        Ok(usage) => match usage.quotas {
                            Some(quotas) if !quotas.is_empty() => {
                                for (name, q) in quotas {
                                    let pct = q
                                        .remaining_pct()
                                        .map(|p| format!("{p:.1}%"))
                                        .unwrap_or_else(|| "unknown/unlimited".into());
                                    let reset = q
                                        .hours_until_reset(Utc::now())
                                        .map(|h| format!("{:.1}h to reset", h.max(0.0)))
                                        .unwrap_or_else(|| "reset unknown".into());
                                    lines.push(format!("  {name}: {pct} remaining ({reset})"));
                                }
                            }
                            _ => lines.push("  No quota data available.".into()),
                        },
                        Err(e) => lines.push(format!("  Usage unavailable: {e}")),
                    }
                }
                if lines.len() == 1 {
                    "No active accounts.".into()
                } else {
                    lines.join("\n")
                }
            }
            Err(e) => format!("Failed to fetch quotas: {e}"),
        },
    };

    send_text(&bot, msg.chat.id, &response).await
}

fn allowed_chat(chat_id: ChatId, config: &Config) -> bool {
    chat_id.0 == config.telegram_chat_id
}

fn health_text(health: HealthStatus) -> String {
    match health {
        HealthStatus::Up(code) => format!("reachable (HTTP {code})"),
        HealthStatus::Down(err) => format!("DOWN ({err})"),
    }
}

fn account_status(c: &Connection) -> String {
    if c.is_active == Some(false) {
        "disabled".into()
    } else if let Some(code) = c.error_code {
        format!("HTTP {code}")
    } else if c.is_codex_auth_error() {
        "401/403 auth error".into()
    } else {
        "enabled (no recorded error)".into()
    }
}

fn format_accounts_list(conns: &[Connection]) -> String {
    let mut lines = vec![format!("Registered accounts: {}", conns.len())];
    for c in conns {
        lines.push(format!(
            "{} ({}) — {}",
            c.display_name(),
            c.provider,
            account_status(c)
        ));
    }
    lines.join("\n")
}

// Plain text avoids treating upstream names and error bodies as Telegram markup.
pub async fn send_text(bot: &Bot, chat_id: ChatId, mut text: &str) -> ResponseResult<()> {
    while !text.is_empty() {
        let (chunk, rest) = next_chunk(text);
        bot.send_message(chat_id, chunk).await?;
        text = rest;
    }
    Ok(())
}

fn next_chunk(text: &str) -> (&str, &str) {
    // 3500 UTF-8 bytes is also safely below Telegram's 4096 UTF-16-unit limit.
    let mut end = text.len().min(3500);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len() {
        if let Some(newline) = text[..end].rfind('\n') {
            end = newline + 1;
        }
    }
    text.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_restricted_to_configured_chat() {
        let config = Config {
            teloxide_token: "synthetic".into(),
            telegram_chat_id: -100123,
            router_url: "https://router.example.com".into(),
            router_auth_token: "synthetic".into(),
            router_api_key: None,
            poll_interval_secs: 60,
        };
        assert!(allowed_chat(ChatId(-100123), &config));
        assert!(!allowed_chat(ChatId(123), &config));
        assert!(!allowed_chat(ChatId(-100124), &config));
    }

    #[test]
    fn delivery_chunks_preserve_unicode_and_content() {
        let text = format!("{}\n{}", "🤖<&>".repeat(1000), "x".repeat(8000));
        let mut rest = text.as_str();
        let mut recovered = String::new();
        while !rest.is_empty() {
            let (chunk, next) = next_chunk(rest);
            assert!(!chunk.is_empty());
            assert!(chunk.encode_utf16().count() <= 4096);
            recovered.push_str(chunk);
            rest = next;
        }
        assert_eq!(recovered, text);
    }
}
