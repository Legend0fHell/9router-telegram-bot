use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ConnectionListResponse {
    pub connections: Vec<Connection>,
    pub pagination: Option<ConnectionPagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionPagination {
    #[serde(rename = "totalPages")]
    pub total_pages: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Connection {
    pub id: String,
    pub provider: String,
    pub name: Option<String>,
    pub email: Option<String>,
    #[serde(rename = "isActive")]
    pub is_active: Option<bool>,
    #[serde(rename = "lastError")]
    pub last_error: Option<String>,
    #[serde(rename = "errorCode")]
    pub error_code: Option<i64>,
}

impl Connection {
    pub fn display_name(&self) -> String {
        self.name
            .as_deref()
            .or(self.email.as_deref())
            .unwrap_or(&self.id)
            .to_string()
    }

    /// An explicit error code is authoritative; otherwise read the leading `[NNN]:`.
    pub fn is_codex_auth_error(&self) -> bool {
        if !self.provider.eq_ignore_ascii_case("codex") {
            return false;
        }
        let code = self.error_code.or_else(|| {
            let error = self.last_error.as_deref()?.as_bytes();
            if error.len() < 6
                || error[0] != b'['
                || &error[4..6] != b"]:"
                || !error[1..4].iter().all(u8::is_ascii_digit)
            {
                return None;
            }
            Some(
                ((error[1] - b'0') as i64) * 100
                    + ((error[2] - b'0') as i64) * 10
                    + (error[3] - b'0') as i64,
            )
        });
        matches!(code, Some(401 | 403))
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct UsageResponse {
    #[serde(rename = "resetCredits")]
    pub reset_credits: Option<ResetCredits>,
    pub quotas: Option<HashMap<String, QuotaDetail>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ResetCredits {
    #[serde(rename = "availableCount")]
    pub available_count: Option<i64>,
}
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionResponse {
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub has_update: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct QuotaDetail {
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    #[serde(rename = "remainingPercentage")]
    pub remaining_percentage: Option<f64>,
    #[serde(rename = "resetAt")]
    pub reset_at: Option<String>,
    pub unlimited: Option<bool>,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
}

impl QuotaDetail {
    pub fn remaining_pct(&self) -> Option<f64> {
        if self.unlimited == Some(true) {
            return None;
        }
        if let Some(pct) = self.remaining_percentage.filter(|pct| pct.is_finite()) {
            return Some(pct);
        }
        if let Some(total) = self.total {
            if !total.is_finite() || total <= 0.0 {
                return None;
            }
        }
        let pct = if let Some(remaining) = self.remaining.filter(|rem| rem.is_finite()) {
            match self.total {
                Some(total) => Some((remaining / total) * 100.0),
                None if remaining <= 100.0 => Some(remaining),
                None => None,
            }
        } else if let (Some(used), Some(total)) = (self.used, self.total) {
            if used.is_finite() {
                Some(((total - used).max(0.0) / total) * 100.0)
            } else {
                None
            }
        } else {
            None
        };
        pct.filter(|pct| pct.is_finite())
    }

    pub fn reset_datetime(&self) -> Option<DateTime<Utc>> {
        let s = self.reset_at.as_deref()?;
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    pub fn hours_until_reset(&self, now: DateTime<Utc>) -> Option<f64> {
        let reset_dt = self.reset_datetime()?;
        let duration = reset_dt.signed_duration_since(now);
        Some(duration.num_seconds() as f64 / 3600.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codex_auth_error_detection() {
        let mut conn = Connection {
            id: "1".into(),
            provider: "codex".into(),
            name: Some("test codex".into()),
            error_code: Some(401),
            ..Default::default()
        };
        assert!(conn.is_codex_auth_error());

        conn.error_code = Some(403);
        assert!(conn.is_codex_auth_error());

        // 429 must NOT be considered auth error
        conn.error_code = Some(429);
        assert!(!conn.is_codex_auth_error());

        conn.error_code = Some(500);
        conn.last_error = Some("[401]: unauthorized".into());
        assert!(!conn.is_codex_auth_error());
        conn.error_code = Some(401);
        conn.last_error = Some("[429]: rate limited".into());
        assert!(conn.is_codex_auth_error());

        conn.error_code = None;
        for error in ["[401]: unauthorized", "[403]: forbidden"] {
            conn.last_error = Some(error.into());
            assert!(conn.is_codex_auth_error());
        }
        for error in [
            "[429]: retry 401 seconds later",
            "account 1401 failed",
            "request id 4030",
            "upstream returned [401]: denied",
            "[401] missing colon",
            "[4010]: denied",
            " [401]: denied",
        ] {
            conn.last_error = Some(error.into());
            assert!(!conn.is_codex_auth_error(), "{error}");
        }

        // Non-codex provider
        conn.provider = "claude".into();
        conn.error_code = Some(401);
        assert!(!conn.is_codex_auth_error());
    }

    #[test]
    fn test_quota_remaining_percentage_calculation() {
        let q1 = QuotaDetail {
            used: Some(34.0),
            total: Some(100.0),
            remaining: Some(66.0),
            remaining_percentage: Some(66.0),
            ..Default::default()
        };
        assert_eq!(q1.remaining_pct(), Some(66.0));

        let q2 = QuotaDetail {
            used: Some(10.0),
            total: Some(200.0),
            ..Default::default()
        };
        assert_eq!(q2.remaining_pct(), Some(95.0));

        let mut quota = QuotaDetail {
            remaining: Some(40.0),
            ..Default::default()
        };
        assert_eq!(quota.remaining_pct(), Some(40.0));
        quota.total = Some(200.0);
        assert_eq!(quota.remaining_pct(), Some(20.0));
        for total in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            quota.total = Some(total);
            assert_eq!(quota.remaining_pct(), None);
        }
        quota.remaining_percentage = Some(66.0);
        assert_eq!(quota.remaining_pct(), Some(66.0));
        quota.unlimited = Some(true);
        assert_eq!(quota.remaining_pct(), None);
        quota.unlimited = None;
        quota.remaining_percentage = Some(f64::NAN);
        quota.total = Some(200.0);
        assert_eq!(quota.remaining_pct(), Some(20.0));
        quota.remaining = Some(f64::INFINITY);
        assert_eq!(quota.remaining_pct(), None);
    }
}
