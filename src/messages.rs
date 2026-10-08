//! Formatted notification messages and language templates.
//! Modify this module to customize wording, tone, kawaii reactions, and kaomoji.

use crate::bot::display;
pub fn armed_message(interval: u64) -> String {
    format!(
        "🟢 <b>9Imouto armed</b>\n<i>Hai hai! I'll monitor diligently for you, onii-chan~ Don't worry about a thing, I'll let you know right away if anything happens, nya! (｡•̀ᴗ-)✧</i>\n\nMonitoring is starting.\nPolling delay: <b>{interval}s</b>\n\n<code>/status</code> · <code>/quota</code> · <code>/codex</code>\n<code>/accounts</code> · <code>/ping</code> · <code>/help</code>\n\n<i>This confirms notification access, not upstream health.</i>"
    )
}

pub fn v1_down_alert(incident_time: &str, error_detail: &str, is_initial: bool) -> String {
    let title = if is_initial {
        "🚨 <b>[ALERT] 9router /v1 serving endpoint is DOWN!</b>"
    } else {
        "🚨 <b>[ALERT] 9router /v1 serving endpoint has gone DOWN!</b>"
    };
    format!(
        "{title}\n<i>Eeh?! C-Connection lost, onii-chan~ (っ˘̩╭╮˘̩)っ</i>\n• Incident time: <b>{incident_time}</b>\n<blockquote expandable>{}</blockquote>",
        display(&error_detail.replace(['\r', '\n'], " "))
    )
}

pub fn v1_resolved_alert(
    code: u16,
    incident_time: &str,
    resolve_time: &str,
    duration_str: &str,
) -> String {
    format!(
        "✅ <b>[RESOLVED] 9router /v1 serving endpoint is back UP (HTTP {code})!</b>\n<i>Yatta! We're connected again, nya~ (ﾉ◕ヮ◕)ﾉ*:･ﾟ✧</i>\n• Incident time: <b>{incident_time}</b>\n• Resolve time: <b>{resolve_time}</b>\n• Incident duration: <b>{duration_str}</b>"
    )
}

pub fn codex_errors_alert(errored_items: &[(&str, &str, &str, &str)]) -> String {
    let heading = if errored_items.len() == 1 {
        "🚨 <b>[ALERT] Codex account authentication error (401/403)!</b>"
    } else {
        "🚨 <b>[ALERT] Codex accounts authentication error (401/403)!</b>"
    };
    let mut lines = vec![
        heading.to_string(),
        "<i>A-ano... p-please check these accounts, onii-chan~ (；ω；)</i>".to_string(),
    ];
    for (name, provider, email, err_desc) in errored_items {
        lines.push(format!(
            "• Account: <b>{}</b> ({})\n  Email: <code>{}</code>\n  <blockquote expandable>{}</blockquote>",
            display(name),
            display(provider),
            display(email),
            display(&err_desc.replace(['\r', '\n'], " "))
        ));
    }
    lines.join("\n")
}

pub fn codex_resolved_alert(cleared_items: &[(&str, &str, &str)]) -> String {
    let heading = if cleared_items.len() == 1 {
        "✅ <b>[RESOLVED] Codex account 401/403 error has cleared!</b>"
    } else {
        "✅ <b>[RESOLVED] Codex accounts 401/403 errors have cleared!</b>"
    };
    let mut lines = vec![
        heading.to_string(),
        "<i>Yatta! Authentication trouble cleared~ (｡•̀ᴗ-)✧</i>".to_string(),
    ];
    for (name, provider, email) in cleared_items {
        lines.push(format!(
            "• Account: <b>{}</b> ({})\n  Email: <code>{}</code>",
            display(name),
            display(provider),
            display(email)
        ));
    }
    lines.join("\n")
}

pub fn quota_warning_alert(tier_label: &str, emoji: &str, items: &[String]) -> String {
    let heading = if items.len() == 1 {
        format!("{emoji} <b>[QUOTA WARNING] Weekly quota expiring soon ({tier_label})!</b>")
    } else {
        format!("{emoji} <b>[QUOTA WARNING] Weekly quotas expiring soon ({tier_label})!</b>")
    };
    let mut lines = vec![
        heading,
        "<i>Ara ara~ reset time is getting close! (｡•́︿•̀｡)</i>".to_string(),
    ];
    lines.extend_from_slice(items);
    lines.push(
        "<i>Use the remaining quota before reset, onii-chan~ Ganbatte! (ง •̀ω•́)ง</i>".to_string(),
    );
    lines.join("\n")
}

pub struct QuotaItem<'a> {
    pub name: &'a str,
    pub provider: &'a str,
    pub email: &'a str,
    pub quota_name: &'a str,
    pub bar: &'a str,
    pub remaining_pct: f64,
    pub hours_left: f64,
    pub reset_time: &'a str,
}

pub fn quota_item_line(item: &QuotaItem) -> String {
    format!(
        "• Account: <b>{}</b> ({})\n  Email: <code>{}</code> · Quota: <b>{}</b>\n  Remaining: <code>{}</code> <b>{:.1}%</b>\n  Time to reset: <b>{:.1} hours</b> (Reset: <b>{}</b>)",
        display(item.name),
        display(item.provider),
        display(item.email),
        display(item.quota_name),
        item.bar,
        item.remaining_pct,
        item.hours_left,
        item.reset_time
    )
}

pub fn banked_resets_alert(items: &[(&str, &str, &str, i64, i64, i64)]) -> String {
    let heading = if items.len() == 1 {
        "🎁 <b>[NOTICE] Additional banked reset received!</b>"
    } else {
        "🎁 <b>[NOTICE] Additional banked resets received!</b>"
    };

    let mut lines = vec![
        heading.to_string(),
        "<i>Sugoi! A little reset gift for ya, nya~ (ﾉ´ヮ`)ﾉ*:･ﾟ✧</i>".to_string(),
    ];

    for (name, provider, email, prev, curr, diff) in items {
        lines.push(format!(
            "• Account: <b>{}</b> ({})\n  Email: <code>{}</code>\n  Banked resets: <b>{prev} ➔ {curr} (+{diff})</b>",
            display(name),
            display(provider),
            display(email)
        ));
    }

    lines.join("\n")
}

pub fn auth_token_expiry_alert(hours: f64) -> String {
    format!(
        "⚠️ <b>[NOTICE] 9router dashboard auth token expires soon!</b>\n<i>A-ano... please refresh it before it expires~ (´• ω •`)</i>\n• Time remaining: <b>{:.1} hours</b>\nPlease refresh <code>ROUTER_AUTH_TOKEN</code>, onii-chan~",
        hours.max(0.0)
    )
}

pub fn daily_leaderboard_greeting() -> &'static str {
    "<i>Otsukaresama, onii-chan~ here's your daily usage roundup! (ﾉ◕ヮ◕)ﾉ*:･ﾟ✧</i>"
}

pub fn update_available_alert(current: &str, latest: &str) -> String {
    format!(
        "🚀 <b>[UPDATE] New 9router version available!</b>\n<i>Sugoi, onii-chan! A new version was detected, nya~ (ﾉ´ヮ`)ﾉ*:･ﾟ✧</i>\n• Current version: <code>{}</code>\n• Latest version: <b>{}</b>\n\n<i>Update when convenient to get the latest features and fixes!</i>",
        display(current),
        display(latest)
    )
}
