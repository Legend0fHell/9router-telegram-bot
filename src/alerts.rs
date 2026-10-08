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
    /// Set of codex connection IDs currently in 401/403 error state
    pub codex_error_accounts: HashSet<String>,
    /// One stable reset timestamp and highest notified tier per account and quota.
    pub quota_notified_tiers: HashMap<(String, String), (DateTime<Utc>, Option<QuotaWarningTier>)>,
    /// Key: connection_id -> last known banked reset count
    pub banked_resets: HashMap<String, i64>,
    /// Whether auth token expiry has been alerted
    pub auth_token_expiry_alerted: bool,
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
                    if let HealthStatus::Down(err) = status {
                        alerts.push(format!(
                            "🚨 [ALERT] 9router /v1 serving endpoint is DOWN!\nDetails: {err}"
                        ));
                    }
                }
            }
            Some(prev_up) => {
                if prev_up && !is_up {
                    // Endpoint went down
                    self.state.last_v1_up = Some(false);
                    let detail = match status {
                        HealthStatus::Down(err) => err.as_str(),
                        _ => "Unknown error",
                    };
                    alerts.push(format!(
                        "🚨 [ALERT] 9router /v1 serving endpoint has gone DOWN!\nDetails: {detail}"
                    ));
                } else if !prev_up && is_up {
                    // Endpoint recovered
                    self.state.last_v1_up = Some(true);
                    let code = match status {
                        HealthStatus::Up(c) => *c,
                        _ => 200,
                    };
                    alerts.push(format!(
                        "✅ [RESOLVED] 9router /v1 serving endpoint is back UP (HTTP {code})!"
                    ));
                }
            }
        }

        alerts
    }

    /// Check codex accounts for 401/403 errors (explicitly excluding 429)
    pub fn check_codex_errors(&mut self, connections: &[Connection]) -> Vec<String> {
        let mut alerts = Vec::new();
        let mut current_errored = HashSet::new();

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

                    alerts.push(format!(
                        "🚨 [ALERT] Codex account authentication error (401/403)!\n• Account: {}\n• Email: {}\n• Error: {}",
                        conn.display_name(),
                        conn.email.as_deref().unwrap_or("N/A"),
                        err_desc
                    ));
                }
            }
        }

        for recovered_id in self.state.codex_error_accounts.difference(&current_errored) {
            if let Some(conn) = connections.iter().find(|c| c.id == *recovered_id) {
                alerts.push(format!(
                    "✅ [RESOLVED] Codex account {} 401/403 error has cleared!",
                    conn.display_name()
                ));
            }
        }

        self.state.codex_error_accounts = current_errored;
        alerts
    }

    /// Check weekly quotas for >30% remaining quota with <24h, <12h, <3h reset warnings
    pub fn check_weekly_quotas(
        &mut self,
        conn: &Connection,
        usage: &UsageResponse,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        let mut alerts = Vec::new();
        let quotas = match &usage.quotas {
            Some(q) => q,
            None => return alerts,
        };

        for (quota_name, quota_detail) in quotas {
            if quota_detail.unlimited == Some(true) || !is_weekly_quota(quota_name, quota_detail) {
                continue;
            }

            let reset_at = match quota_detail.reset_datetime() {
                Some(reset_at) => reset_at,
                None => continue,
            };
            let cycle = self
                .state
                .quota_notified_tiers
                .entry((conn.id.clone(), quota_name.clone()))
                .or_insert((reset_at, None));

            // ponytail: reset timestamps within 60s share an anchored cycle; use an API cycle ID if provided.
            let reset_shift = reset_at.signed_duration_since(cycle.0);
            if reset_shift > Duration::seconds(60) {
                *cycle = (reset_at, None);
            } else if reset_shift < -Duration::seconds(60) {
                continue;
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
                    let emoji = match tier {
                        QuotaWarningTier::LessThan3h => "🚨",
                        QuotaWarningTier::LessThan12h => "⚠️",
                        QuotaWarningTier::LessThan24h => "⏳",
                    };

                    alerts.push(format!(
                        "{emoji} [QUOTA WARNING] Account weekly quota > 30% expiring soon ({tier_label})!\n• Account: {} ({})\n• Quota: {}\n• Remaining: {:.1}%\n• Time to reset: {:.1} hours",
                        conn.display_name(),
                        conn.provider,
                        quota_name,
                        remaining_pct,
                        hours_left,
                        tier_label = tier.label()
                    ));
                }
            }
        }

        alerts
    }

    /// Check for newly added banked resets compared to last time
    pub fn check_banked_resets(&mut self, conn: &Connection, usage: &UsageResponse) -> Vec<String> {
        let mut alerts = Vec::new();
        let available = usage
            .reset_credits
            .as_ref()
            .and_then(|rc| rc.available_count);

        if let Some(current_count) = available {
            match self.state.banked_resets.get(&conn.id) {
                Some(&prev_count) if current_count > prev_count => {
                    let diff = current_count - prev_count;
                    alerts.push(format!(
                        "🎁 [NOTICE] Additional banked reset received!\n• Account: {} ({})\n• Banked resets: {} ➔ {} (+{diff})",
                        conn.display_name(),
                        conn.provider,
                        prev_count,
                        current_count
                    ));
                }
                _ => {}
            }
            self.state
                .banked_resets
                .insert(conn.id.clone(), current_count);
        }

        alerts
    }

    /// Warn once when the dashboard's 24-hour auth token has less than 3 hours left.
    pub fn check_auth_token_expiry(&mut self, hours_remaining: Option<f64>) -> Vec<String> {
        let mut alerts = Vec::new();
        if let Some(hours) = hours_remaining {
            if hours < 3.0 && !self.state.auth_token_expiry_alerted {
                self.state.auth_token_expiry_alerted = true;
                alerts.push(format!(
                    "⚠️ [NOTICE] 9router dashboard auth token expires in {:.1} hours!\nPlease refresh ROUTER_AUTH_TOKEN.",
                    hours.max(0.0)
                ));
            } else if hours >= 3.0 {
                self.state.auth_token_expiry_alerted = false;
            }
        }
        alerts
    }
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
        assert_eq!(engine.check_weekly_quotas(&conn, &usage, now).len(), 1);
        for seconds in [-60, -1, 0, 1, 60] {
            let jittered = weekly_usage(reset + Duration::seconds(seconds), 60.0);
            assert!(engine.check_weekly_quotas(&conn, &jittered, now).is_empty());
            assert!(engine
                .check_weekly_quotas(&conn, &jittered, reset - Duration::hours(12))
                .is_empty());
        }
        let tier_two_time = reset - Duration::hours(12) + Duration::nanoseconds(1);
        assert_eq!(
            engine
                .check_weekly_quotas(&conn, &usage, tier_two_time)
                .len(),
            1
        );
        assert!(engine
            .check_weekly_quotas(&conn, &usage, tier_two_time)
            .is_empty());
        let tier_three_time = reset - Duration::hours(3) + Duration::nanoseconds(1);
        assert_eq!(
            engine
                .check_weekly_quotas(&conn, &usage, tier_three_time)
                .len(),
            1
        );
        assert!(engine.check_weekly_quotas(&conn, &usage, reset).is_empty());

        for week in 1..=10 {
            let next_reset = reset + Duration::days(7 * week);
            let next_usage = weekly_usage(next_reset, 60.0);
            assert!(engine
                .check_weekly_quotas(&conn, &next_usage, next_reset - Duration::days(7))
                .is_empty());
            assert_eq!(
                engine
                    .check_weekly_quotas(&conn, &next_usage, next_reset - Duration::hours(20))
                    .len(),
                1
            );
            assert_eq!(engine.state.quota_notified_tiers.len(), 1);
        }
        assert!(engine.check_weekly_quotas(&conn, &usage, now).is_empty());
    }

    #[test]
    fn distinct_weekly_quotas_and_accounts_with_same_reset_are_independent() {
        let mut engine = AlertEngine::new();
        let conn = connection();
        let now = now();
        let mut usage = weekly_usage(now + Duration::hours(2), 60.0);
        let quotas = usage.quotas.as_mut().unwrap();
        quotas.insert("weekly-other".into(), quotas["weekly"].clone());
        assert_eq!(engine.check_weekly_quotas(&conn, &usage, now).len(), 2);
        assert!(engine.check_weekly_quotas(&conn, &usage, now).is_empty());
        let mut other_conn = conn;
        other_conn.id = "account-2".into();
        assert_eq!(
            engine.check_weekly_quotas(&other_conn, &usage, now).len(),
            2
        );
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
                .check_weekly_quotas(&conn, &weekly_usage(reset, remaining), now)
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
        assert!(engine.check_weekly_quotas(&conn, &usage, now).is_empty());
        usage
            .quotas
            .as_mut()
            .unwrap()
            .get_mut("weekly")
            .unwrap()
            .unlimited = Some(false);
        assert_eq!(engine.check_weekly_quotas(&conn, &usage, now).len(), 1);
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
                engine.check_weekly_quotas(&conn, &usage, now).len(),
                usize::from(tier.is_some())
            );
            assert_eq!(
                engine.state.quota_notified_tiers[&(conn.id.clone(), "weekly".into())].1,
                tier
            );
        }
    }

    #[test]
    fn banked_resets_only_alert_on_increase() {
        let mut engine = AlertEngine::new();
        let conn = connection();
        for (count, alert_count) in [(1, 0), (2, 1), (2, 0), (0, 0), (1, 1)] {
            let usage = UsageResponse {
                reset_credits: Some(crate::models::ResetCredits {
                    available_count: Some(count),
                }),
                ..Default::default()
            };
            assert_eq!(engine.check_banked_resets(&conn, &usage).len(), alert_count);
        }
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
}
