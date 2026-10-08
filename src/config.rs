use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub teloxide_token: String,
    pub telegram_chat_id: i64,
    pub router_url: String,
    pub router_auth_token: String,
    pub router_api_key: Option<String>,
    pub poll_interval_secs: u64,
}

#[derive(Deserialize)]
struct JwtPayload {
    exp: Option<i64>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let teloxide_token = env::var("TELOXIDE_TOKEN")
            .context("TELOXIDE_TOKEN environment variable is required")?;

        let chat_id_str = env::var("TELEGRAM_CHAT_ID")
            .context("TELEGRAM_CHAT_ID environment variable is required")?;
        let telegram_chat_id = chat_id_str
            .parse::<i64>()
            .context("TELEGRAM_CHAT_ID must be a valid 64-bit integer")?;

        let router_url = validate_router_url(
            &env::var("ROUTER_URL").context("ROUTER_URL environment variable is required")?,
        )?;

        let router_auth_token = env::var("ROUTER_AUTH_TOKEN")
            .context("ROUTER_AUTH_TOKEN environment variable is required")?;
        validate_auth_token(&router_auth_token)?;

        let router_api_key = env::var("ROUTER_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let poll_interval_secs = match env::var("POLL_INTERVAL_SECS") {
            Ok(value) => parse_poll_interval(Some(&value))?,
            Err(env::VarError::NotPresent) => parse_poll_interval(None)?,
            Err(error) => return Err(error).context("POLL_INTERVAL_SECS must be valid Unicode"),
        };

        Ok(Self {
            teloxide_token,
            telegram_chat_id,
            router_url,
            router_auth_token,
            router_api_key,
            poll_interval_secs,
        })
    }

    /// Read the unverified JWT expiration claim for informational alerts only.
    pub fn auth_token_expiry(&self) -> Option<DateTime<Utc>> {
        let payload_b64 = self.router_auth_token.split('.').nth(1)?;
        let bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
        let payload: JwtPayload = serde_json::from_slice(&bytes).ok()?;
        DateTime::from_timestamp(payload.exp?, 0)
    }

    /// Hours until the informational expiration claim, without verifying the JWT.
    pub fn auth_token_hours_remaining(&self, now: DateTime<Utc>) -> Option<f64> {
        let expiry = self.auth_token_expiry()?;
        let duration = expiry.signed_duration_since(now);
        Some(duration.num_seconds() as f64 / 3600.0)
    }
}

pub(crate) fn validate_router_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).context("ROUTER_URL must be a valid HTTP(S) URL")?;
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default());
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && authority.is_some_and(|value| !value.contains('@'))
            && url.query().is_none()
            && url.fragment().is_none()
            && value == value.trim()
            && !value.bytes().any(|byte| byte.is_ascii_control()),
        "ROUTER_URL must be HTTP(S) with no userinfo, query, fragment or whitespace"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub(crate) fn validate_auth_token(value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value.bytes().all(|byte| {
                matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
            }),
        "ROUTER_AUTH_TOKEN must be a nonempty cookie value without whitespace or cookie separators"
    );
    Ok(())
}

fn parse_poll_interval(value: Option<&str>) -> Result<u64> {
    let seconds = value
        .unwrap_or("60")
        .parse::<u64>()
        .context("POLL_INTERVAL_SECS must be a positive integer")?;
    anyhow::ensure!(seconds > 0, "POLL_INTERVAL_SECS must be greater than zero");
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_expiry_decoding() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":1791488528}"#);
        let token = format!("e30.{payload}.");
        let cfg = Config {
            teloxide_token: "test".into(),
            telegram_chat_id: 123,
            router_url: "https://router.example".into(),
            router_auth_token: token,
            router_api_key: None,
            poll_interval_secs: 60,
        };

        let expiry = cfg.auth_token_expiry();
        assert!(expiry.is_some());
        assert_eq!(expiry.unwrap().timestamp(), 1791488528);
    }

    #[test]
    fn rejects_invalid_configuration() {
        for url in [
            "file:///tmp/router",
            "https://user:pass@router.example",
            "https://@router.example",
            "https://router.example?key=value",
            "https://router.example#fragment",
            " https://router.example",
        ] {
            assert!(validate_router_url(url).is_err(), "{url}");
        }
        assert_eq!(
            validate_router_url("https://router.example/base/").unwrap(),
            "https://router.example/base"
        );
        for token in [
            "",
            "token;other=value",
            "token\r\n",
            "token value",
            "\"token\"",
            "a,b",
            "a\\b",
        ] {
            assert!(validate_auth_token(token).is_err());
        }
        assert!(validate_auth_token("synthetic.jwt_token-value").is_ok());
        assert_eq!(parse_poll_interval(None).unwrap(), 60);
        assert_eq!(parse_poll_interval(Some("5")).unwrap(), 5);
        for interval in ["0", "-1", "abc", "", "18446744073709551616"] {
            assert!(parse_poll_interval(Some(interval)).is_err());
        }
    }
}
