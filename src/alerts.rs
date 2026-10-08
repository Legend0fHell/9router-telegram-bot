use crate::client::HealthStatus;
use crate::models::{Connection, QuotaDetail, UsageResponse};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QuotaWarningTier {
    LessThan24h = 1,
    LessThan12h = 2,
    LessThan3h = 3,
}

impl QuotaWarningTier {
    pub fn label(&self) -> &'static str {
        match self {
            QuotaWarningTier::LessThan24h => "< 24 hours",
            QuotaWarningTier::LessThan12h => "< 12 hours",
            QuotaWarningTier::LessThan3h => "< 3 hours",
        }
    }
}

#[derive(Debug, Default)]
pub struct AlertState {
    /// Last v1 ping state (None on first startup, Some(true) for UP, Some(false) for DOWN)
    pub last_v1_up: Option<bool>,
    /// First observed failure in the current incident; cleared on recovery.
    pub v1_down_since: Option<DateTime<Utc>>,
    /// Set of codex connection IDs currently in 401/403 error state
    pub codex_error_accounts: HashSet<String>,
    /// One stable reset timestamp and highest notified tier per account and quota.
    pub quota_notified_tiers: HashMap<(String, String), (DateTime<Utc>, Option<QuotaWarningTier>)>,
    /// Key: connection_id -> last known banked reset count
    pub banked_resets: HashMap<String, i64>,
    /// Whether auth token expiry has been alerted
    pub auth_token_expiry_alerted: bool,
    /// Last alerted update version string to prevent repeating notifications for the same version
    pub last_notified_update_version: Option<String>,
}

#[derive(Default)]
pub struct AlertEngine {
    state: AlertState,
}

impl AlertEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check v1 health and emit alert on failure or recovery
    pub fn check_v1_health(&mut self, status: &HealthStatus) -> Vec<String> {
        self.check_v1_health_at(status, Utc::now())
    }

    fn check_v1_health_at(&mut self, status: &HealthStatus, now: DateTime<Utc>) -> Vec<String> {
        let mut alerts = Vec::new();
        let is_up = match status {
            HealthStatus::Up(_) => true,
            HealthStatus::Down(_) => false,
        };

        match self.state.last_v1_up {
            None => {
                // Initial baseline check
                self.state.last_v1_up = Some(is_up);
                if !is_up {
                    self.state.v1_down_since = Some(now);
                    if let HealthStatus::Down(err) = status {
                        alerts.push(crate::messages::v1_down_alert(&local_time(now), err, true));
                    }
                }
            }
            Some(prev_up) => {
                if prev_up && !is_up {
                    // Endpoint went down
                    self.state.last_v1_up = Some(false);
                    self.state.v1_down_since = Some(now);
                    let detail = match status {
                        HealthStatus::Down(err) => err.as_str(),
                        _ => "Unknown error",
                    };
                    alerts.push(crate::messages::v1_down_alert(
                        &local_time(now),
                        detail,
                        false,
                    ));
                } else if !prev_up && is_up {
                    // Endpoint recovered
                    self.state.last_v1_up = Some(true);
                    let code = match status {
                        HealthStatus::Up(c) => *c,
                        _ => 200,
                    };
                    let started = self.state.v1_down_since.take().unwrap_or(now);
                    let seconds = (now - started).num_seconds().max(0);
                    let duration_str = format!(
                        "{}h {}m {}s",
                        seconds / 3600,
                        seconds / 60 % 60,
                        seconds % 60
                    );
                    alerts.push(crate::messages::v1_resolved_alert(
                        code,
                        &local_time(started),
                        &local_time(now),
                        &duration_str,
                    ));
                }
            }
        }

        alerts
    }

    /// Check codex accounts for 401/403 errors (explicitly excluding 429).
    /// Batches newly errored and newly cleared accounts into consolidated messages.
    pub fn check_codex_errors(&mut self, connections: &[Connection]) -> Vec<String> {
        let mut alerts = Vec::new();
        let mut current_errored = HashSet::new();
        let mut new_errored = Vec::new();

        for conn in connections {
            if !conn.provider.eq_ignore_ascii_case("codex") {
                continue;
            }

            if conn.is_codex_auth_error() {
                current_errored.insert(conn.id.clone());

                if !self.state.codex_error_accounts.contains(&conn.id) {
                    let err_desc = conn
                        .last_error
                        .as_deref()
                        .or_else(|| {
                            conn.error_code.map(|c| {
                                if c == 401 {
                                    "HTTP 401 Unauthorized"
                                } else {
                                    "HTTP 403 Forbidden"
                                }
                            })
                        })
                        .unwrap_or("401/403 Authentication Error");

                    new_errored.push((conn, err_desc));
                }
            }
        }

        let mut cleared = Vec::new();
        for recovered_id in self.state.codex_error_accounts.difference(&current_errored) {
            if let Some(conn) = connections.iter().find(|c| c.id == *recovered_id) {
                cleared.push(conn);
            }
        }

        self.state.codex_error_accounts = current_errored;

        if !new_errored.is_empty() {
            let names: Vec<String> = new_errored.iter().map(|(c, _)| c.display_name()).collect();
            let items: Vec<(&str, &str, &str, &str)> = new_errored
                .iter()
                .zip(names.iter())
                .map(|((c, err), name)| {
                    (
                        name.as_str(),
                        c.provider.as_str(),
                        c.email.as_deref().unwrap_or("N/A"),
                        *err,
                    )
                })
                .collect();
            alerts.push(crate::messages::codex_errors_alert(&items));
        }

        if !cleared.is_empty() {
            let names: Vec<String> = cleared.iter().map(|c| c.display_name()).collect();
            let items: Vec<(&str, &str, &str)> = cleared
                .iter()
                .zip(names.iter())
                .map(|(c, name)| {
                    (
                        name.as_str(),
                        c.provider.as_str(),
                        c.email.as_deref().unwrap_or("N/A"),
                    )
                })
                .collect();
            alerts.push(crate::messages::codex_resolved_alert(&items));
        }

        alerts
    }

    /// Check weekly quotas across accounts for >30% remaining quota with <24h, <12h, <3h reset warnings.
    /// Batches warnings of the same tier into a single consolidated message.
    pub fn check_weekly_quotas<'a, I>(&mut self, entries: I, now: DateTime<Utc>) -> Vec<String>
    where
        I: IntoIterator<Item = (&'a Connection, &'a UsageResponse)>,
    {
        // Map tier -> list of formatted item lines
        let mut tier_items: HashMap<QuotaWarningTier, Vec<String>> = HashMap::new();
        let mut reset_events: Vec<(&Connection, &str, String)> = Vec::new();
        for (conn, usage) in entries {
            let quotas = match &usage.quotas {
                Some(q) => q,
                None => continue,
            };

            for (quota_name, quota_detail) in quotas {
                if quota_detail.unlimited == Some(true)
                    || !is_weekly_quota(quota_name, quota_detail)
                {
                    continue;
                }

                let reset_at = match quota_detail.reset_datetime() {
                    Some(reset_at) => reset_at,
                    None => continue,
                };
                let mut cycle_reset_detected = false;
                let cycle = match self
                    .state
                    .quota_notified_tiers
                    .get_mut(&(conn.id.clone(), quota_name.clone()))
                {
                    Some(cycle) => {
                        let reset_shift = reset_at.signed_duration_since(cycle.0);
                        // If the reset timestamp jumped forward by at least 6 hours, a new cycle has started!
                        if reset_shift >= Duration::hours(6) {
                            *cycle = (reset_at, None);
                            cycle_reset_detected = true;
                        } else if reset_shift > Duration::seconds(60) {
                            *cycle = (reset_at, None);
                        } else if reset_shift < -Duration::seconds(60) {
                            continue;
                        }
                        cycle
                    }
                    None => {
                        // Initial baseline establishment (no reset alert on startup)
                        self.state
                            .quota_notified_tiers
                            .entry((conn.id.clone(), quota_name.clone()))
                            .or_insert((reset_at, None))
                    }
                };

                if cycle_reset_detected {
                    reset_events.push((conn, quota_name.as_str(), local_time(reset_at)));
                }

                let remaining_pct = match quota_detail.remaining_pct() {
                    Some(p) if p > 30.0 => p,
                    _ => continue,
                };
                let time_left = cycle.0.signed_duration_since(now);
                if time_left <= Duration::zero() {
                    continue;
                }
                let hours_left = time_left.num_milliseconds() as f64 / 3_600_000.0;
                let current_tier = if time_left < Duration::hours(3) {
                    Some(QuotaWarningTier::LessThan3h)
                } else if time_left < Duration::hours(12) {
                    Some(QuotaWarningTier::LessThan12h)
                } else if time_left < Duration::hours(24) {
                    Some(QuotaWarningTier::LessThan24h)
                } else {
                    None
                };

                if let Some(tier) = current_tier {
                    let highest = cycle.1;

                    let should_alert = match highest {
                        None => true,
                        Some(prev_tier) => tier > prev_tier,
                    };

                    if should_alert {
                        cycle.1 = Some(tier);
                        let filled = (remaining_pct.clamp(0.0, 100.0) / 10.0).round() as usize;
                        let bar = format!(
                            "{}{}",
                            &"██████████"[..filled * '█'.len_utf8()],
                            &"░░░░░░░░░░"[filled * '░'.len_utf8()..]
                        );
                        let name = conn.display_name();
                        let item = crate::messages::quota_item_line(&crate::messages::QuotaItem {
                            name: &name,
                            provider: &conn.provider,
                            email: conn.email.as_deref().unwrap_or("N/A"),
                            quota_name,
                            bar: &bar,
                            remaining_pct,
                            hours_left,
                            reset_time: &local_time(cycle.0),
                        });
                        tier_items.entry(tier).or_default().push(item);
                    }
                }
            }
        }
        let mut alerts = Vec::new();

        if !reset_events.is_empty() {
            let names: Vec<String> = reset_events
                .iter()
                .map(|(c, ..)| c.display_name())
                .collect();
            let items: Vec<(&str, &str, &str, &str, &str)> = reset_events
                .iter()
                .zip(names.iter())
                .map(|((c, q, r), name)| {
                    (
                        name.as_str(),
                        c.provider.as_str(),
                        c.email.as_deref().unwrap_or("N/A"),
                        *q,
                        r.as_str(),
                    )
                })
                .collect();
            alerts.push(crate::messages::weekly_resets_alert(&items));
        }

        // Order by tier severity ascending (24h -> 12h -> 3h)
        for tier in [
            QuotaWarningTier::LessThan24h,
            QuotaWarningTier::LessThan12h,
            QuotaWarningTier::LessThan3h,
        ] {
            if let Some(items) = tier_items.remove(&tier) {
                let emoji = match tier {
                    QuotaWarningTier::LessThan3h => "🚨",
                    QuotaWarningTier::LessThan12h => "⚠️",
                    QuotaWarningTier::LessThan24h => "⏳",
                };
                alerts.push(crate::messages::quota_warning_alert(
                    tier.label(),
                    emoji,
                    &items,
                ));
            }
        }

        alerts
    }

    /// Check for newly added banked resets across connections compared to last time.
    /// Batches all newly credited accounts into a single consolidated message.
    pub fn check_banked_resets<'a, I>(&mut self, entries: I) -> Vec<String>
    where
        I: IntoIterator<Item = (&'a Connection, &'a UsageResponse)>,
    {
        let mut credited = Vec::new();

        for (conn, usage) in entries {
            let available = usage
                .reset_credits
                .as_ref()
                .and_then(|rc| rc.available_count);

            if let Some(current_count) = available {
                if let Some(&prev_count) = self.state.banked_resets.get(&conn.id) {
                    if current_count > prev_count {
                        let diff = current_count - prev_count;
                        credited.push((conn, prev_count, current_count, diff));
                    }
                }
                self.state
                    .banked_resets
                    .insert(conn.id.clone(), current_count);
            }
        }

        if credited.is_empty() {
            return Vec::new();
        }

        let names: Vec<String> = credited.iter().map(|(c, ..)| c.display_name()).collect();
        let items: Vec<(&str, &str, &str, i64, i64, i64)> = credited
            .iter()
            .zip(names.iter())
            .map(|((c, prev, curr, diff), name)| {
                (
                    name.as_str(),
                    c.provider.as_str(),
                    c.email.as_deref().unwrap_or("N/A"),
                    *prev,
                    *curr,
                    *diff,
                )
            })
            .collect();
        vec![crate::messages::banked_resets_alert(&items)]
    }

    /// Warn once when the dashboard's 24-hour auth token has less than 3 hours left.
    pub fn check_auth_token_expiry(&mut self, hours_remaining: Option<f64>) -> Vec<String> {
        let mut alerts = Vec::new();
        if let Some(hours) = hours_remaining {
            if hours < 3.0 && !self.state.auth_token_expiry_alerted {
                self.state.auth_token_expiry_alerted = true;
                alerts.push(crate::messages::auth_token_expiry_alert(hours));
            } else if hours >= 3.0 {
                self.state.auth_token_expiry_alerted = false;
            }
        }
        alerts
    }

    /// Check for available 9router updates from /api/version
    pub fn check_update(&mut self, version_info: &crate::models::VersionResponse) -> Vec<String> {
        let mut alerts = Vec::new();
        if version_info.has_update {
            if let Some(latest) = &version_info.latest_version {
                if self.state.last_notified_update_version.as_deref() != Some(latest) {
                    self.state.last_notified_update_version = Some(latest.clone());
                    let current = version_info.current_version.as_deref().unwrap_or("unknown");
                    alerts.push(crate::messages::update_available_alert(current, latest));
                }
            }
        }
        alerts
    }
}

fn local_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&chrono::FixedOffset::east_opt(7 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M:%S UTC+7")
        .to_string()
}

/// Helper to determine if a quota entry is weekly
fn is_weekly_quota(name: &str, detail: &QuotaDetail) -> bool {
    let lower_name = name.to_lowercase();
    if lower_name.contains("weekly") || lower_name.contains("7d") {
        return true;
    }
    if let Some(dn) = &detail.display_name {
        if dn.to_lowercase().contains("weekly") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn connection() -> Connection {
        Connection {
            id: "account-1".into(),
            provider: "codex".into(),
            ..Default::default()
        }
    }

    fn weekly_usage(reset_at: DateTime<Utc>, remaining: f64) -> UsageResponse {
        UsageResponse {
            quotas: Some(HashMap::from([(
                "weekly".into(),
                QuotaDetail {
                    remaining_percentage: Some(remaining),
                    reset_at: Some(reset_at.to_rfc3339()),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
    }

    #[test]
    fn health_alerts_only_on_transitions_or_initial_failure() {
        let mut engine = AlertEngine::new();
        assert!(engine.check_v1_health(&HealthStatus::Up(200)).is_empty());
        let down = HealthStatus::Down("<network & error>".into());
        assert_eq!(engine.check_v1_health(&down).len(), 1);
        assert!(engine.check_v1_health(&down).is_empty());
        assert_eq!(engine.check_v1_health(&HealthStatus::Up(200)).len(), 1);
        assert!(engine.check_v1_health(&HealthStatus::Up(200)).is_empty());
        assert_eq!(AlertEngine::new().check_v1_health(&down).len(), 1);
    }

    #[test]
    fn incident_duration_starts_at_first_failure_and_resets_after_recovery() {
        let mut engine = AlertEngine::new();
        let down = HealthStatus::Down("network".into());
        let first = engine.check_v1_health_at(&down, now());
        assert!(first[0].contains("2026-10-08 19:00:00 UTC+7"));
        assert!(engine
            .check_v1_health_at(&down, now() + Duration::minutes(2))
            .is_empty());
        let resolved =
            engine.check_v1_health_at(&HealthStatus::Up(200), now() + Duration::seconds(3665));
        assert!(resolved[0].contains("2026-10-08 20:01:05 UTC+7"));
        assert!(resolved[0].contains("1h 1m 5s"));
        assert!(engine.state.v1_down_since.is_none());
        engine.check_v1_health_at(&down, now() + Duration::hours(2));
        let resolved = engine.check_v1_health_at(
            &HealthStatus::Up(200),
            now() + Duration::hours(2) + Duration::seconds(5),
        );
        assert!(resolved[0].contains("0h 0m 5s"));
        assert_eq!(
            local_time(now() + Duration::hours(6)),
            "2026-10-09 01:00:00 UTC+7"
        );
    }

    #[test]
    fn codex_errors_deduplicate_recover_and_ignore_removed_accounts_and_429() {
        let mut engine = AlertEngine::new();
        let mut conn = connection();
        conn.error_code = Some(429);
        assert!(engine.check_codex_errors(&[conn.clone()]).is_empty());
        conn.error_code = Some(401);
        assert_eq!(engine.check_codex_errors(&[conn.clone()]).len(), 1);
        assert!(engine.check_codex_errors(&[conn.clone()]).is_empty());
        conn.error_code = Some(403);
        assert!(engine.check_codex_errors(&[conn.clone()]).is_empty());
        conn.error_code = None;
        assert_eq!(engine.check_codex_errors(&[conn.clone()]).len(), 1);
        assert!(engine.check_codex_errors(&[conn.clone()]).is_empty());
        conn.error_code = Some(403);
        assert_eq!(engine.check_codex_errors(&[conn.clone()]).len(), 1);
        assert!(engine.check_codex_errors(&[]).is_empty());
        assert!(engine.state.codex_error_accounts.is_empty());
        conn.error_code = None;
        assert!(engine.check_codex_errors(&[conn]).is_empty());
    }

    #[test]
    fn weekly_tiers_deduplicate_jitter_and_rearm_next_cycle_without_growing_state() {
        let mut engine = AlertEngine::new();
        let conn = connection();
        let now = now();
        let reset = now + Duration::hours(20);
        let usage = weekly_usage(reset, 60.0);
        assert_eq!(engine.check_weekly_quotas([(&conn, &usage)], now).len(), 1);
        for seconds in [-60, -1, 0, 1, 60] {
            let jittered = weekly_usage(reset + Duration::seconds(seconds), 60.0);
            assert!(engine
                .check_weekly_quotas([(&conn, &jittered)], now)
                .is_empty());
            assert!(engine
                .check_weekly_quotas([(&conn, &jittered)], reset - Duration::hours(12))
                .is_empty());
        }
        let tier_two_time = reset - Duration::hours(12) + Duration::nanoseconds(1);
        assert_eq!(
            engine
                .check_weekly_quotas([(&conn, &usage)], tier_two_time)
                .len(),
            1
        );
        assert!(engine
            .check_weekly_quotas([(&conn, &usage)], tier_two_time)
            .is_empty());
        let tier_three_time = reset - Duration::hours(3) + Duration::nanoseconds(1);
        assert_eq!(
            engine
                .check_weekly_quotas([(&conn, &usage)], tier_three_time)
                .len(),
            1
        );
        assert!(engine
            .check_weekly_quotas([(&conn, &usage)], reset)
            .is_empty());

        for week in 1..=10 {
            let next_reset = reset + Duration::days(7 * week);
            let next_usage = weekly_usage(next_reset, 60.0);
            // The jump forward into the next weekly cycle emits exactly 1 reset notice!
            let reset_alerts =
                engine.check_weekly_quotas([(&conn, &next_usage)], next_reset - Duration::days(7));
            assert_eq!(reset_alerts.len(), 1);
            assert!(reset_alerts[0].contains("Weekly quota reset!"));

            // Further checks inside the cycle before warning tiers produce no extra alerts
            assert!(engine
                .check_weekly_quotas([(&conn, &next_usage)], next_reset - Duration::days(6))
                .is_empty());

            // Approaching expiration (<24h) emits the tier warning
            assert_eq!(
                engine
                    .check_weekly_quotas([(&conn, &next_usage)], next_reset - Duration::hours(20))
                    .len(),
                1
            );
            assert_eq!(engine.state.quota_notified_tiers.len(), 1);
        }
        assert!(engine
            .check_weekly_quotas([(&conn, &usage)], now)
            .is_empty());
    }

    #[test]
    fn distinct_weekly_quotas_and_accounts_with_same_reset_are_batched_in_single_message() {
        let mut engine = AlertEngine::new();
        let conn1 = connection();
        let mut conn2 = connection();
        conn2.id = "account-2".into();
        let now = now();
        let mut usage = weekly_usage(now + Duration::hours(2), 60.0);
        let quotas = usage.quotas.as_mut().unwrap();
        quotas.insert("weekly-other".into(), quotas["weekly"].clone());

        // Two quotas on conn1 + two quotas on conn2 all in tier <3h -> exactly 1 combined alert message!
        let alerts = engine.check_weekly_quotas([(&conn1, &usage), (&conn2, &usage)], now);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("account-1"));
        assert!(alerts[0].contains("account-2"));
        assert!(alerts[0].contains("Weekly quotas expiring soon (< 3 hours)!"));

        assert!(engine
            .check_weekly_quotas([(&conn1, &usage), (&conn2, &usage)], now)
            .is_empty());
        assert_eq!(engine.state.quota_notified_tiers.len(), 4);
    }

    #[test]
    fn weekly_remaining_must_be_strictly_above_thirty_and_limited() {
        let mut engine = AlertEngine::new();
        let conn = connection();
        let now = now();
        let reset = now + Duration::hours(2);
        for remaining in [0.0, 29.9, 30.0, f64::NAN] {
            assert!(engine
                .check_weekly_quotas([(&conn, &weekly_usage(reset, remaining))], now)
                .is_empty());
        }
        let mut usage = weekly_usage(reset, 30.0001);
        usage
            .quotas
            .as_mut()
            .unwrap()
            .get_mut("weekly")
            .unwrap()
            .unlimited = Some(true);
        assert!(engine
            .check_weekly_quotas([(&conn, &usage)], now)
            .is_empty());
        usage
            .quotas
            .as_mut()
            .unwrap()
            .get_mut("weekly")
            .unwrap()
            .unlimited = Some(false);
        assert_eq!(engine.check_weekly_quotas([(&conn, &usage)], now).len(), 1);
    }

    #[test]
    fn weekly_reset_thresholds_are_exact_including_subseconds() {
        let now = now();
        let conn = connection();
        for (time_left, tier) in [
            (Duration::hours(24), None),
            (
                Duration::hours(24) - Duration::nanoseconds(1),
                Some(QuotaWarningTier::LessThan24h),
            ),
            (Duration::hours(12), Some(QuotaWarningTier::LessThan24h)),
            (
                Duration::hours(12) - Duration::nanoseconds(1),
                Some(QuotaWarningTier::LessThan12h),
            ),
            (Duration::hours(3), Some(QuotaWarningTier::LessThan12h)),
            (
                Duration::hours(3) - Duration::nanoseconds(1),
                Some(QuotaWarningTier::LessThan3h),
            ),
            (Duration::nanoseconds(1), Some(QuotaWarningTier::LessThan3h)),
            (Duration::zero(), None),
            (-Duration::nanoseconds(1), None),
        ] {
            let mut engine = AlertEngine::new();
            let usage = weekly_usage(now + time_left, 60.0);
            assert_eq!(
                engine.check_weekly_quotas([(&conn, &usage)], now).len(),
                usize::from(tier.is_some())
            );
            assert_eq!(
                engine.state.quota_notified_tiers[&(conn.id.clone(), "weekly".into())].1,
                tier
            );
        }
    }

    #[test]
    fn banked_resets_only_alert_on_increase_and_batches_multiple_accounts() {
        let mut engine = AlertEngine::new();
        let conn1 = connection();
        let mut conn2 = connection();
        conn2.id = "account-2".into();
        conn2.name = Some("Second Account".into());

        let make_usage = |count| UsageResponse {
            reset_credits: Some(crate::models::ResetCredits {
                available_count: Some(count),
            }),
            ..Default::default()
        };

        // Initial baseline establishes count without alert
        assert!(engine
            .check_banked_resets([(&conn1, &make_usage(1)), (&conn2, &make_usage(0))])
            .is_empty());

        // Both increase simultaneously -> exactly ONE combined alert message
        let alerts =
            engine.check_banked_resets([(&conn1, &make_usage(3)), (&conn2, &make_usage(2))]);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("Additional banked resets received!"));
        assert!(alerts[0].contains("account-1"));
        assert!(alerts[0].contains("1 ➔ 3 (+2)"));
        assert!(alerts[0].contains("Second Account"));
        assert!(alerts[0].contains("0 ➔ 2 (+2)"));

        // Subsequent poll with no further increase produces no alert
        assert!(engine
            .check_banked_resets([(&conn1, &make_usage(3)), (&conn2, &make_usage(2))])
            .is_empty());
    }

    #[test]
    fn token_expiry_warns_below_three_hours_and_rearms_on_refresh() {
        let mut engine = AlertEngine::new();
        for hours in [None, Some(24.0), Some(3.0)] {
            assert!(engine.check_auth_token_expiry(hours).is_empty());
        }
        assert_eq!(
            engine
                .check_auth_token_expiry(Some(3.0 - 1.0 / 3600.0))
                .len(),
            1
        );
        assert!(engine.check_auth_token_expiry(Some(2.0)).is_empty());
        assert!(engine.check_auth_token_expiry(None).is_empty());
        assert!(engine.check_auth_token_expiry(Some(0.0)).is_empty());
        assert!(engine.check_auth_token_expiry(Some(3.0)).is_empty());
        assert_eq!(engine.check_auth_token_expiry(Some(0.0)).len(), 1);
        assert_eq!(
            AlertEngine::new().check_auth_token_expiry(Some(-1.0)).len(),
            1
        );
    }

    #[test]
    fn update_check_only_alerts_once_per_new_version() {
        let mut engine = AlertEngine::new();
        let no_update = crate::models::VersionResponse {
            current_version: Some("0.5.95".into()),
            latest_version: Some("0.5.95".into()),
            has_update: false,
        };
        assert!(engine.check_update(&no_update).is_empty());

        let update_v1 = crate::models::VersionResponse {
            current_version: Some("0.5.95".into()),
            latest_version: Some("0.5.96".into()),
            has_update: true,
        };
        let alerts = engine.check_update(&update_v1);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("New 9router version available!"));
        assert!(alerts[0].contains("0.5.95"));
        assert!(alerts[0].contains("0.5.96"));

        // Repeating the same check yields no new alerts
        assert!(engine.check_update(&update_v1).is_empty());

        // Another new version triggers an alert again
        let update_v2 = crate::models::VersionResponse {
            current_version: Some("0.5.95".into()),
            latest_version: Some("0.5.97".into()),
            has_update: true,
        };
        let alerts2 = engine.check_update(&update_v2);
        assert_eq!(alerts2.len(), 1);
        assert!(alerts2[0].contains("0.5.97"));
    }

    #[test]
    fn weekly_resets_detect_forward_jump_and_batch_multiple_accounts() {
        let mut engine = AlertEngine::new();
        let conn1 = connection();
        let mut conn2 = connection();
        conn2.id = "account-2".into();
        conn2.name = Some("Second Account".into());

        let now = now();
        let cycle1 = now + Duration::hours(10);
        let usage1 = weekly_usage(cycle1, 20.0);

        // Initial setup establishes baseline (no alert)
        assert!(engine
            .check_weekly_quotas([(&conn1, &usage1), (&conn2, &usage1)], now)
            .is_empty());

        // Jumps forward by 5 hours (< 6h threshold) -> not considered a new cycle
        let small_jump = weekly_usage(cycle1 + Duration::hours(5), 20.0);
        assert!(engine
            .check_weekly_quotas([(&conn1, &small_jump)], now)
            .is_empty());

        // Jumps forward by 7 days (>= 6h threshold) for both accounts -> single batched alert
        let next_cycle = cycle1 + Duration::days(7);
        let next_usage = weekly_usage(next_cycle, 100.0);
        let alerts =
            engine.check_weekly_quotas([(&conn1, &next_usage), (&conn2, &next_usage)], now);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("Weekly quotas reset!"));
        assert!(alerts[0].contains("account-1"));
        assert!(alerts[0].contains("Second Account"));

        // Subsequent poll with same reset timestamp produces no alert
        assert!(engine
            .check_weekly_quotas([(&conn1, &next_usage), (&conn2, &next_usage)], now)
            .is_empty());
    }
}
