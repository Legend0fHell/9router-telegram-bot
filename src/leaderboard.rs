use crate::bot::display;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub total_requests: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_cost: f64,
    pub by_account: HashMap<String, UsageRow>,
    pub by_api_key: HashMap<String, UsageRow>,
    pub by_model: HashMap<String, UsageRow>,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UsageRow {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub connection_id: Option<String>,
    pub account_name: Option<String>,
    pub provider: Option<String>,
    pub key_name: Option<String>,
    pub raw_model: Option<String>,
}

pub fn next_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    let zone = FixedOffset::east_opt(7 * 3600).unwrap();
    let local = now.with_timezone(&zone);
    let tomorrow = local.date_naive().succ_opt().unwrap();
    tomorrow
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(zone)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

pub fn report(stats: UsageStats, fetched_at: DateTime<Utc>) -> String {
    let zone = FixedOffset::east_opt(7 * 3600).unwrap();
    let start = (fetched_at - Duration::hours(24)).with_timezone(&zone);
    let end = fetched_at.with_timezone(&zone);
    let mut keys: HashMap<String, (String, UsageRow)> = HashMap::new();
    for (identity, row) in stats.by_api_key {
        // Upstream object keys contain the full credential followed by model/provider.
        // Use only the credential component as an in-memory identity; never render it.
        let id = identity
            .split('|')
            .next()
            .unwrap_or("local-no-key")
            .to_owned();
        let name = row
            .key_name
            .clone()
            .filter(|n| !n.starts_with("sk-"))
            .unwrap_or_else(|| "Unnamed key".into());
        let entry = keys
            .entry(id)
            .or_insert_with(|| (name, UsageRow::default()));
        add(&mut entry.1, &row);
    }
    let mut models: HashMap<String, UsageRow> = HashMap::new();
    for row in stats.by_model.into_values() {
        let name = row
            .raw_model
            .clone()
            .unwrap_or_else(|| "Unknown model".into());
        add(models.entry(name).or_default(), &row);
    }
    let mut accounts: HashMap<String, (String, UsageRow)> = HashMap::new();
    for (identity, row) in stats.by_account {
        let id = row.connection_id.clone().unwrap_or(identity);
        let name = format!(
            "{} ({})",
            row.account_name.as_deref().unwrap_or("Unknown account"),
            row.provider.as_deref().unwrap_or("unknown")
        );
        let entry = accounts
            .entry(id)
            .or_insert_with(|| (name, UsageRow::default()));
        add(&mut entry.1, &row);
    }
    let mut sections = vec![format!("🏆 <b>Daily usage leaderboard</b>\n{} → {} <b>UTC+7</b>\n<i>Last 24h · top 5 by requests</i>\n\n<b>{} requests · ~${:.2} estimated</b>\nInput <b>{}</b> · Cached <b>{}</b> · Output <b>{}</b>", start.format("%d %b %H:%M"), end.format("%d %b %H:%M"), compact(stats.total_requests), stats.total_cost, compact(stats.total_prompt_tokens), compact(stats.total_cached_tokens), compact(stats.total_completion_tokens))];
    sections.insert(
        1,
        "<i>Otsukaresama, senpai~ here's your daily usage roundup! (ﾉ◕ヮ◕)ﾉ*:･ﾟ✧</i>".into(),
    );
    sections.push(ranking("🔑 Top API keys", keys.into_values().collect()));
    sections.push(ranking("🤖 Top models", models.into_iter().collect()));
    sections.push(ranking("👤 Top accounts", accounts.into_values().collect()));
    sections.push("<i>In = total input, including Cache (cached input subset). Out = output. Costs are 9router’s recorded USD estimates; missing pricing may understate them, not actual subscription charges.</i>".into());
    sections.join("\n\n")
}
fn add(target: &mut UsageRow, row: &UsageRow) {
    target.requests += row.requests;
    target.prompt_tokens += row.prompt_tokens;
    target.completion_tokens += row.completion_tokens;
    target.cached_tokens += row.cached_tokens;
    target.cost += row.cost;
}
fn ranking(title: &str, mut rows: Vec<(String, UsageRow)>) -> String {
    rows.sort_by(|a, b| b.1.requests.cmp(&a.1.requests).then_with(|| a.0.cmp(&b.0)));
    let mut lines = vec![format!("<b>{title}</b>")];
    for (i, (name, row)) in rows
        .into_iter()
        .filter(|(_, r)| r.requests > 0)
        .take(5)
        .enumerate()
    {
        let medal = match i {
            0 => "🥇",
            1 => "🥈",
            2 => "🥉",
            _ => "•",
        };
        lines.push(format!(
            "{medal} <b>{}</b> · <b>{}</b> req · <b>~${:.2}</b>\n<code>In {:>6}  Cache {:>6}  Out {:>6}</code>",
            display(&name), compact(row.requests), row.cost,
            compact(row.prompt_tokens), compact(row.cached_tokens), compact(row.completion_tokens)
        ));
    }
    if lines.len() == 1 {
        lines.push("No recorded usage.".into());
    }
    lines.join("\n")
}

fn compact(value: u64) -> String {
    let (scale, suffix) = if value >= 999_950_000 {
        (1_000_000_000.0, "B")
    } else if value >= 999_950 {
        (1_000_000.0, "M")
    } else if value >= 1_000 {
        (1_000.0, "K")
    } else {
        return value.to_string();
    };
    format!("{:.1}", value as f64 / scale)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
        + suffix
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn midnight_uses_vietnam_not_host_timezone() {
        let parse = |s| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert_eq!(
            next_midnight(parse("2026-10-08T16:59:59Z")),
            parse("2026-10-08T17:00:00Z")
        );
        assert_eq!(
            next_midnight(parse("2026-10-08T17:00:00Z")),
            parse("2026-10-09T17:00:00Z")
        );
        assert_eq!(
            next_midnight(parse("2026-12-31T18:00:00Z")),
            parse("2027-01-01T17:00:00Z")
        );
    }
    #[test]
    fn combines_keys_and_models_without_leaking_credentials() {
        let stats:UsageStats=serde_json::from_value(serde_json::json!({"totalRequests":9,"totalPromptTokens":90,"totalCompletionTokens":9,"totalCachedTokens":60,"totalCost":1.25,"byApiKey":{
            "secret-one|m1|p":{"requests":3,"promptTokens":30,"completionTokens":3,"cachedTokens":20,"cost":0.5,"keyName":"Team <A>"},
            "secret-one|m2|p":{"requests":4,"promptTokens":40,"completionTokens":4,"cachedTokens":30,"cost":0.25,"keyName":"Team <A>"},
            "secret-two|m1|p":{"requests":2,"promptTokens":20,"completionTokens":2,"keyName":"Other"}
        },"byModel":{"a":{"requests":3,"rawModel":"m1"},"b":{"requests":4,"rawModel":"m1"},"c":{"requests":2,"rawModel":"m2"}},"byAccount":{"a":{"connectionId":"acct","accountName":"Account <A>","provider":"codex","requests":3},"b":{"connectionId":"acct","accountName":"Account <A>","provider":"codex","requests":4}}})).unwrap();
        let html = report(stats, Utc::now());
        assert!(!html.contains("secret-"));
        assert_eq!(html.matches("Team &lt;A&gt;").count(), 1);
        assert!(html.contains("<b>7</b> req · <b>~$0.75</b>"));
        assert!(html.contains("In     70  Cache     50  Out      7"));
        assert_eq!(html.matches("Account &lt;A&gt;").count(), 1);
        assert_eq!(compact(4_500_000), "4.5M");
        assert_eq!(compact(292_000), "292K");
        assert_eq!(compact(999_950), "1M");
        assert_eq!(compact(999), "999");
        assert_eq!(html.matches("<b>m1</b>").count(), 1);
    }
}
