use crate::client::{HealthStatus, NineRouterClient};
use crate::config::Config;
use crate::models::Connection;
use chrono::Utc;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
use teloxide::utils::{command::BotCommands, html::escape};

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
        Command::Help | Command::Start => format!(
            "🤖 <b>9router notification bot</b>\n\n{}",
            escape(&Command::descriptions().to_string())
        ),
        Command::Ping => {
            let start = std::time::Instant::now();
            let health = health_text(client.check_v1_health().await);
            format!(
                "📡 <b>Serving gateway</b>\n{health}\nLatency: <b>{}ms</b>",
                start.elapsed().as_millis()
            )
        }
        Command::Status => {
            let health = health_text(client.check_v1_health().await);
            let expiry = config
                .auth_token_hours_remaining(Utc::now())
                .map(|h| format!("{:.1}h remaining", h.max(0.0)))
                .unwrap_or_else(|| "unknown".into());
            match client.fetch_connections().await {
                Ok(conns)=>format!("📊 <b>9router status</b>\n\n{health}\nDashboard: <b>authenticated</b>\nAccounts: <b>{}</b> · <b>{}</b> enabled\nCodex auth errors: <b>{}</b>\nToken: <b>{expiry}</b>\n<i>Expiry is an unverified JWT claim.</i>",conns.len(),conns.iter().filter(|c|c.is_active.unwrap_or(true)).count(),conns.iter().filter(|c|c.is_codex_auth_error()).count()),
                Err(e)=>format!("📊 <b>9router status</b>\n{health}\n{}",error_text(&e.to_string())),
            }
        }
        Command::Accounts => match client.fetch_connections().await {
            Ok(conns) => {
                let mut sections = vec![format!("👥 <b>Registered accounts · {}</b>", conns.len())];
                for c in conns {
                    sections.push(account_text(&c));
                }
                sections.join("\n\n")
            }
            Err(e) => error_text(&e.to_string()),
        },
        Command::Codex => match client.fetch_connections().await {
            Ok(conns) => {
                let mut sections = vec!["🔐 <b>Codex accounts</b>".into()];
                for c in conns
                    .iter()
                    .filter(|c| c.provider.eq_ignore_ascii_case("codex"))
                {
                    let credits = match client.fetch_usage(&c.id).await {
                        Ok(usage) => usage
                            .reset_credits
                            .and_then(|r| r.available_count)
                            .map(|n| format!("🎁 Banked resets: <b>{n}</b>"))
                            .unwrap_or_else(|| "Banked resets: unknown".into()),
                        Err(e) => error_text(&e.to_string()),
                    };
                    sections.push(format!("{}\n{credits}", account_text(c)));
                }
                if sections.len() == 1 {
                    sections.push("No Codex accounts found.".into());
                }
                sections.join("\n\n")
            }
            Err(e) => error_text(&e.to_string()),
        },
        Command::Quota => match client.fetch_connections().await {
            Ok(conns) => {
                let mut sections = vec!["📈 <b>Active account quotas</b>".into()];
                for c in conns.iter().filter(|c| c.is_active.unwrap_or(true)) {
                    let mut lines = vec![account_text(c)];
                    match client.fetch_usage(&c.id).await {
                        Ok(usage) => match usage.quotas {
                            Some(quotas) if !quotas.is_empty() => {
                                for (name, q) in quotas {
                                    let pct = q
                                        .remaining_pct()
                                        .map(|p| format!("<b>{p:.1}%</b>"))
                                        .unwrap_or_else(|| "unknown/unlimited".into());
                                    let reset = q
                                        .hours_until_reset(Utc::now())
                                        .map(|h| format!("{:.1}h to reset", h.max(0.0)))
                                        .unwrap_or_else(|| "reset unknown".into());
                                    lines.push(format!(
                                        "• {}: {pct} remaining · {reset}",
                                        display(&name)
                                    ));
                                }
                            }
                            _ => lines.push("No quota data available.".into()),
                        },
                        Err(e) => lines.push(error_text(&e.to_string())),
                    }
                    sections.push(lines.join("\n"));
                }
                if sections.len() == 1 {
                    sections.push("No active accounts.".into());
                }
                sections.join("\n\n")
            }
            Err(e) => error_text(&e.to_string()),
        },
    };
    send_formatted(&bot, msg.chat.id, &response, &config.router_url).await
}

fn allowed_chat(chat_id: ChatId, config: &Config) -> bool {
    chat_id.0 == config.telegram_chat_id
}

// Bound individual display fields before HTML escaping; long upstream errors stay expandable.
pub fn display(value: &str) -> String {
    let mut chars = value.chars();
    let mut text: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    escape(&text.replace(['\r', '\n'], " "))
}

fn error_text(error: &str) -> String {
    format!(
        "⚠️ <b>Request failed</b>\n<blockquote expandable>{}</blockquote>",
        display(error)
    )
}
fn health_text(health: HealthStatus) -> String {
    match health {
        HealthStatus::Up(code) => format!("🟢 <code>/v1</code> reachable · <b>HTTP {code}</b>"),
        HealthStatus::Down(err) => format!(
            "🔴 <b>Serving endpoint down</b>\n<blockquote expandable>{}</blockquote>",
            display(&err)
        ),
    }
}
fn account_text(c: &Connection) -> String {
    let status = if c.is_active == Some(false) {
        "⚪ Disabled".into()
    } else if let Some(code) = c.error_code {
        format!("⚠️ HTTP {code}")
    } else if c.is_codex_auth_error() {
        "🔐 401/403 auth error".into()
    } else {
        "🟢 Enabled · no recorded error".into()
    };
    format!(
        "<b>{}</b> · {}\n{status}",
        display(&c.display_name()),
        display(&c.provider)
    )
}

pub use crate::messages::armed_message;

pub async fn send_formatted(
    bot: &Bot,
    chat_id: ChatId,
    html: &str,
    base_url: &str,
) -> ResponseResult<()> {
    let keyboard = dashboard_buttons(base_url);
    for chunk in message_chunks(html) {
        bot.send_message(chat_id, chunk)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard.clone())
            .await?;
    }
    Ok(())
}
fn dashboard_buttons(base_url: &str) -> InlineKeyboardMarkup {
    // Config validation guarantees HTTP(S), no userinfo/query/fragment; tokens never enter links.
    let base = base_url.trim_end_matches('/');
    InlineKeyboardMarkup::new([[
        InlineKeyboardButton::url(
            "📈 Quota",
            format!("{base}/dashboard/quota")
                .parse()
                .expect("validated router URL"),
        ),
        InlineKeyboardButton::url(
            "📊 Usage",
            format!("{base}/dashboard/usage")
                .parse()
                .expect("validated router URL"),
        ),
    ]])
}
fn message_chunks(html: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    // Templates use balanced tags per line. Display values are bounded before escaping.
    // 3500 source characters also bounds rendered UTF-16 units for our escaped templates.
    for line in html.split_inclusive('\n') {
        if !current.is_empty()
            && current.encode_utf16().count() + line.encode_utf16().count() > 3500
        {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
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
    }
    #[test]
    fn escaped_fields_and_chunks_preserve_complete_sections() {
        assert_eq!(
            display("<b>name & 🦀</b>"),
            "&lt;b&gt;name &amp; 🦀&lt;/b&gt;"
        );
        let line = format!(
            "<blockquote expandable>{}</blockquote>\n",
            display(&"<&🦀>".repeat(1000))
        );
        let html = line.repeat(30);
        let chunks = message_chunks(&html);
        assert_eq!(chunks.concat(), html);
        for chunk in chunks {
            assert!(chunk.encode_utf16().count() <= 4096);
            assert_eq!(
                chunk.matches("<blockquote expandable>").count(),
                chunk.matches("</blockquote>").count()
            );
        }
        let keyboard = dashboard_buttons("https://router.example.com/base/");
        let json = serde_json::to_string(&keyboard).unwrap();
        assert!(json.contains("https://router.example.com/base/dashboard/quota"));
        assert!(!json.contains("auth_token"));
    }
}
