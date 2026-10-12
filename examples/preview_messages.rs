use chrono::{Duration, Utc};
use nine_router_bot::{
    alerts::AlertEngine,
    bot::{armed_message, send_formatted},
    client::HealthStatus,
    config::Config,
    leaderboard::{report, UsageRow, UsageStats},
    models::{Connection, QuotaDetail, ResetCredits, UsageResponse},
};
use std::collections::HashMap;
use teloxide::{types::ChatId, Bot};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let bot = Bot::new(&config.teloxide_token);
    let chat_id = ChatId(config.telegram_chat_id);
    let mut engine = AlertEngine::new();
    let now = Utc::now();

    let mut previews = Vec::new();

    // 1. Armed message preview
    previews.push(format!(
        "<i>[PREVIEW ONLY]</i>\n\n{}",
        armed_message(config.poll_interval_secs)
    ));

    // 2. Health alert DOWN preview
    for _ in 0..2 {
        engine.check_v1_health(&HealthStatus::Down("PREVIEW failure".into()));
    }
    let down_alerts = engine.check_v1_health(&HealthStatus::Down(
        "PREVIEW: 502 Bad Gateway while reaching upstream service".into(),
    ));
    for alert in down_alerts {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // 3. Health alert RESOLVED preview
    let up_alerts = engine.check_v1_health(&HealthStatus::Up(200));
    for alert in up_alerts {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // 4. Weekly quota warning tiers (<24h, <12h, <3h)
    let conn = Connection {
        id: "preview-conn-1".into(),
        provider: "codex".into(),
        name: Some("Preview Sakura <Pro & test>".into()),
        email: Some("sakura@example.com".into()),
        is_active: Some(true),
        ..Default::default()
    };
    let reset_at = now + Duration::hours(18);
    let usage = UsageResponse {
        quotas: Some(HashMap::from([(
            "weekly".into(),
            QuotaDetail {
                remaining_percentage: Some(68.5),
                reset_at: Some(reset_at.to_rfc3339()),
                ..Default::default()
            },
        )])),
        ..Default::default()
    };

    // Tier <24h
    let alerts_24h = engine.check_weekly_quotas([(&conn, &usage)], now);
    for alert in alerts_24h {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // Tier <12h (simulated check at reset - 10h)
    let alerts_12h = engine.check_weekly_quotas([(&conn, &usage)], reset_at - Duration::hours(10));
    for alert in alerts_12h {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // Tier <3h (simulated check at reset - 2h)
    let alerts_3h = engine.check_weekly_quotas([(&conn, &usage)], reset_at - Duration::hours(2));
    for alert in alerts_3h {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // 5. Codex 401/403 error & recovery preview
    let mut errored_conn = conn.clone();
    errored_conn.error_code = Some(401);
    errored_conn.last_error = Some("[401]: PREVIEW session revoked <token> & test".into());
    let err_alerts = engine.check_codex_errors(&[errored_conn.clone()]);
    for alert in err_alerts {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    errored_conn.error_code = None;
    errored_conn.last_error = None;
    let cleared_alerts = engine.check_codex_errors(&[errored_conn]);
    for alert in cleared_alerts {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // 6. Banked reset credit increase preview
    let mut credit_conn = conn.clone();
    credit_conn.id = "preview-banked-1".into();
    let initial_credits = UsageResponse {
        reset_credits: Some(ResetCredits {
            available_count: Some(1),
        }),
        ..Default::default()
    };
    engine.check_banked_resets([(&credit_conn, &initial_credits)]);

    let mut credit_conn2 = conn.clone();
    credit_conn2.id = "preview-banked-2".into();
    credit_conn2.name = Some("Preview Loli <Pro & test>".into());
    credit_conn2.email = Some("loli@example.com".into());
    let initial_credits2 = UsageResponse {
        reset_credits: Some(ResetCredits {
            available_count: Some(0),
        }),
        ..Default::default()
    };
    engine.check_banked_resets([(&credit_conn2, &initial_credits2)]);

    let updated_credits = UsageResponse {
        reset_credits: Some(ResetCredits {
            available_count: Some(3),
        }),
        ..Default::default()
    };
    let updated_credits2 = UsageResponse {
        reset_credits: Some(ResetCredits {
            available_count: Some(1),
        }),
        ..Default::default()
    };
    let banked_alerts = engine.check_banked_resets([
        (&credit_conn, &updated_credits),
        (&credit_conn2, &updated_credits2),
    ]);
    for alert in banked_alerts {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // 7. Login failure notice preview
    let token_alerts = engine.check_login_failure(true);
    for alert in token_alerts {
        previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{alert}"));
    }

    // 8. 9router update available preview
    let update_preview = nine_router_bot::messages::update_available_alert("0.5.95", "0.5.96");
    previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{update_preview}"));

    // 9. Weekly quota reset preview
    let reset_preview = nine_router_bot::messages::weekly_resets_alert(&[(
        "Preview Sakura <Pro & test>",
        "codex",
        "sakura@example.com",
        "weekly (7d)",
        "2026-10-15 12:00:00 UTC+7",
    )]);
    previews.push(format!("<i>[PREVIEW ONLY]</i>\n\n{reset_preview}"));

    // 10. Synthetic kawaii daily leaderboard preview
    let synthetic_stats = UsageStats {
        total_requests: 1850,
        total_prompt_tokens: 104_500_000,
        total_completion_tokens: 1_250_000,
        total_cached_tokens: 88_000_000,
        total_cost: 48.75,
        by_api_key: HashMap::from([
            (
                "key-a|m1|p".into(),
                UsageRow {
                    requests: 1120,
                    prompt_tokens: 62_000_000,
                    completion_tokens: 800_000,
                    cached_tokens: 52_000_000,
                    cost: 31.20,
                    key_name: Some("FatCat".into()),
                    ..Default::default()
                },
            ),
            (
                "key-b|m2|p".into(),
                UsageRow {
                    requests: 730,
                    prompt_tokens: 42_500_000,
                    completion_tokens: 450_000,
                    cached_tokens: 36_000_000,
                    cost: 17.55,
                    key_name: Some("Loli".into()),
                    ..Default::default()
                },
            ),
        ]),
        by_model: HashMap::from([
            (
                "gpt-6-sol".into(),
                UsageRow {
                    requests: 1050,
                    prompt_tokens: 58_000_000,
                    completion_tokens: 750_000,
                    cached_tokens: 48_000_000,
                    cost: 29.50,
                    raw_model: Some("gpt-6-sol".into()),
                    ..Default::default()
                },
            ),
            (
                "gpt-6.1-sol".into(),
                UsageRow {
                    requests: 800,
                    prompt_tokens: 46_500_000,
                    completion_tokens: 500_000,
                    cached_tokens: 40_000_000,
                    cost: 19.25,
                    raw_model: Some("gpt-6.1-sol".into()),
                    ..Default::default()
                },
            ),
        ]),
        by_account: HashMap::from([(
            "conn-codex-1".into(),
            UsageRow {
                requests: 1850,
                prompt_tokens: 104_500_000,
                completion_tokens: 1_250_000,
                cached_tokens: 88_000_000,
                cost: 48.75,
                account_name: Some("Primary Sakura".into()),
                provider: Some("codex".into()),
                connection_id: Some("conn-codex-1".into()),
                ..Default::default()
            },
        )]),
    };
    previews.push(format!(
        "<i>[PREVIEW ONLY]</i>\n\n{}",
        report(synthetic_stats, now)
    ));

    println!("Sending {} preview messages to Telegram...", previews.len());
    for preview in &previews {
        send_formatted(&bot, chat_id, preview, &config.router_url).await?;
    }
    println!(
        "Successfully sent {} preview messages to chat {}.",
        previews.len(),
        config.telegram_chat_id
    );

    Ok(())
}
