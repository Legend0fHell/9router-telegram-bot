use crate::config::{validate_auth_token, validate_router_url};
use crate::models::{Connection, ConnectionListResponse, UsageResponse};
use anyhow::{Context, Result};
use reqwest::header::{HeaderValue, AUTHORIZATION, COOKIE};
use serde::de::DeserializeOwned;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct NineRouterClient {
    client: reqwest::Client,
    base_url: String,
    auth_cookie: HeaderValue,
    api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Up(u16),
    Down(String),
}

impl NineRouterClient {
    pub fn new(base_url: String, auth_token: String, api_key: Option<String>) -> Result<Self> {
        let base_url = validate_router_url(&base_url)?;
        validate_auth_token(&auth_token)?;
        let mut auth_cookie = HeaderValue::from_str(&format!("auth_token={auth_token}"))
            .context("ROUTER_AUTH_TOKEN is not a valid HTTP cookie header")?;
        auth_cookie.set_sensitive(true);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url,
            auth_cookie,
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

    async fn get_dashboard_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .client
            .get(url)
            .header(COOKIE, self.auth_cookie.clone())
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
}
