use crate::config::{validate_auth_token, validate_router_url};
use crate::models::{Connection, ConnectionListResponse, UsageResponse};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderValue, AUTHORIZATION, COOKIE, SET_COOKIE};
use serde::de::DeserializeOwned;
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct NineRouterClient {
    client: reqwest::Client,
    base_url: String,
    password: String,
    session: Arc<Mutex<Session>>,
    api_key: Option<String>,
}

#[derive(Default)]
struct Session {
    cookie: Option<HeaderValue>,
    expires: Option<DateTime<Utc>>,
    retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Up(u16),
    Down(String),
}

impl NineRouterClient {
    pub fn new(base_url: String, password: String, api_key: Option<String>) -> Result<Self> {
        let base_url = validate_router_url(&base_url)?;
        anyhow::ensure!(!password.is_empty(), "ROUTER_PASSWORD must not be empty");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url,
            password,
            session: Arc::new(Mutex::new(Session::default())),
            api_key,
        })
    }

    /// Ping /v1 endpoint to check if the serving gateway is up
    pub async fn check_v1_health(&self) -> HealthStatus {
        let url = format!("{}/v1", self.base_url);
        let mut req = self.client.get(&url);

        if let Some(key) = &self.api_key {
            req = req.header(AUTHORIZATION, format!("Bearer {key}"));
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // 200 OK or 401 Unauthorized (which requires API key) proves server process is UP.
                // 5xx (500, 502 Bad Gateway, 503, 504) means serving endpoint is DOWN.
                if status < 500 {
                    HealthStatus::Up(status)
                } else {
                    HealthStatus::Down(format!("HTTP {status}"))
                }
            }
            Err(e) => HealthStatus::Down(e.to_string()),
        }
    }

    /// Refresh five minutes before expiry; serialized across commands and monitors.
    pub async fn refresh_session(&self) -> Result<()> {
        self.session_cookie().await.map(|_| ())
    }

    pub async fn auth_token_hours_remaining(&self, now: DateTime<Utc>) -> Option<f64> {
        self.session
            .lock()
            .await
            .expires
            .map(|expiry| (expiry - now).num_seconds() as f64 / 3600.0)
    }

    async fn session_cookie(&self) -> Result<HeaderValue> {
        let mut session = self.session.lock().await;
        let now = Utc::now();
        if session
            .expires
            .is_some_and(|expiry| expiry > now + chrono::Duration::minutes(5))
        {
            if let Some(cookie) = &session.cookie {
                return Ok(cookie.clone());
            }
        }
        anyhow::ensure!(
            !session.retry_after.is_some_and(|time| time > now),
            "Dashboard login is cooling down after a failure"
        );
        // Failure cooldown prevents every account/command from retrying a bad password.
        session.retry_after = Some(now + chrono::Duration::minutes(5));
        let response = self
            .client
            .post(format!("{}/api/auth/login", self.base_url))
            .json(&serde_json::json!({"password": self.password}))
            .send()
            .await
            .context("Dashboard login request failed")?;
        if !response.status().is_success() {
            let delay = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(300)
                .max(300);
            session.retry_after = now
                .checked_add_signed(chrono::Duration::seconds(delay))
                .or(Some(now + chrono::Duration::minutes(5)));
            anyhow::bail!("Dashboard login returned HTTP {}", response.status());
        }
        let token = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|v| v.split(';').next()?.trim().strip_prefix("auth_token="))
            .context("Dashboard login did not return auth_token")?;
        validate_auth_token(token)?;
        let payload = token
            .split('.')
            .nth(1)
            .context("Session token has no expiration payload")?;
        let decoded = URL_SAFE_NO_PAD
            .decode(payload)
            .context("Invalid session expiration payload")?;
        let claims: serde_json::Value =
            serde_json::from_slice(&decoded).context("Invalid session expiration JSON")?;
        let expiry = claims
            .get("exp")
            .and_then(|v| v.as_i64())
            .and_then(|v| DateTime::from_timestamp(v, 0))
            .context("Session token has no valid expiration")?;
        anyhow::ensure!(
            expiry > now + chrono::Duration::minutes(5),
            "Login returned a session expiring too soon"
        );
        let mut cookie = HeaderValue::from_str(&format!("auth_token={token}"))?;
        cookie.set_sensitive(true);
        session.cookie = Some(cookie.clone());
        session.expires = Some(expiry);
        session.retry_after = None;
        Ok(cookie)
    }

    async fn get_dashboard_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, self.session_cookie().await?)
            .send()
            .await
            .context("Failed to request dashboard API")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .context("Failed to read dashboard error response")?;
            anyhow::bail!("Dashboard API returned HTTP {status} - {body}");
        }
        response
            .json()
            .await
            .context("Failed to parse dashboard response JSON")
    }

    /// Fetch all registered provider connections
    pub async fn fetch_connections(&self) -> Result<Vec<Connection>> {
        let mut connections = Vec::new();
        let mut page = 1;
        loop {
            let url = format!(
                "{}/api/providers/client?page={page}&pageSize=100&accountStatus=all",
                self.base_url
            );
            let data: ConnectionListResponse = self
                .get_dashboard_json(&url)
                .await
                .context("Failed to fetch providers")?;
            connections.extend(data.connections);
            match data.pagination {
                Some(pagination) if page < pagination.total_pages => page += 1,
                _ => return Ok(connections),
            }
        }
    }

    /// Fetch usage and quota for a single connection
    pub async fn fetch_usage(&self, connection_id: &str) -> Result<UsageResponse> {
        let url = format!("{}/api/usage/{}", self.base_url, connection_id);
        self.get_dashboard_json(&url)
            .await
            .context("Failed to fetch usage")
    }

    pub async fn fetch_usage_stats(&self) -> Result<crate::leaderboard::UsageStats> {
        self.get_dashboard_json(&format!("{}/api/usage/stats?period=24h", self.base_url))
            .await
            .context("Failed to fetch 24-hour usage statistics")
    }

    pub async fn fetch_version(&self) -> Result<crate::models::VersionResponse> {
        self.get_dashboard_json(&format!("{}/api/version", self.base_url))
            .await
            .context("Failed to fetch version info")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[tokio::test]
    async fn fetches_all_pages_and_accepts_legacy_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (page, id) in [(1, "one"), (2, "two"), (1, "legacy")] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(&format!(
                    "GET /api/providers/client?page={page}&pageSize=100&accountStatus=all "
                )));
                let pagination = if id == "legacy" {
                    String::new()
                } else {
                    format!(r#", "pagination": {{"page":{page},"totalPages":2}}"#)
                };
                let body = format!(r#"{{"connections":[{{"id":"{id}"}}]{pagination}}}"#);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let client =
            NineRouterClient::new(format!("http://{address}"), "synthetic_token".into(), None)
                .unwrap();
        {
            let mut session = client.session.lock().await;
            session.cookie = Some(HeaderValue::from_static("auth_token=synthetic"));
            session.expires = Some(Utc::now() + chrono::Duration::hours(24));
        }
        let connections = client.fetch_connections().await.unwrap();
        assert_eq!(
            connections
                .iter()
                .map(|connection| connection.id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        let legacy = client.fetch_connections().await.unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].id, "legacy");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn password_login_refreshes_once_for_concurrent_requests_and_cools_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for success in [true, true, false] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("POST /api/auth/login "));
                assert!(request.ends_with(r#"{"password":"synthetic password"}"#));
                if success {
                    let payload = URL_SAFE_NO_PAD.encode(format!(
                        r#"{{"exp":{}}}"#,
                        (Utc::now() + chrono::Duration::hours(24)).timestamp()
                    ));
                    write!(stream, "HTTP/1.1 200 OK\r\nSet-Cookie: auth_token=e30.{payload}.sig; HttpOnly; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
                } else {
                    write!(stream, "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
            }
        });
        let client = NineRouterClient::new(
            format!("http://{address}"),
            "synthetic password".into(),
            None,
        )
        .unwrap();
        let (a, b) = tokio::join!(client.refresh_session(), client.refresh_session());
        a.unwrap();
        b.unwrap();
        client.session.lock().await.expires = Some(Utc::now() + chrono::Duration::minutes(4));
        let (a, b) = tokio::join!(client.refresh_session(), client.refresh_session());
        a.unwrap();
        b.unwrap();
        client.session.lock().await.expires = Some(Utc::now());
        assert!(client.refresh_session().await.is_err());
        assert!(client.refresh_session().await.is_err());
        assert!(
            client.session.lock().await.retry_after.unwrap()
                > Utc::now() + chrono::Duration::minutes(9)
        );
        server.join().unwrap();
    }
}
