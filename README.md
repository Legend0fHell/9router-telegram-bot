# 9router Telegram Bot

A requests-only Telegram notification bot for [9router](https://github.com/decolua/9router), written in Rust using [teloxide 0.17](https://github.com/teloxide/teloxide) and reqwest. Both use rustls, without native TLS/OpenSSL.

## Features & Alerts

The bot continuously monitors your 9router deployment and delivers real-time notifications to your Telegram channel or chat:

- 🚨 **Serving Gateway Availability (`/v1`)**:
  Alerts when `GET /v1` is unreachable or returns a server error (5xx), and sends a recovery notification. Without an API key, HTTP 401 proves reachability only—not successful inference. Checks run once per polling cycle, including quota-fetch time.
- ⏳ **Weekly Quota Waste Warning**:
  Monitors enabled accounts returned by the usage-eligible providers API, across all pages. With **>30% remaining** and less than **24 hours**, **12 hours**, or **3 hours** until reset, warnings fire once per tier per account/quota/cycle. Reset timestamps within 60 seconds share an anchored cycle to tolerate upstream timestamp jitter; unlimited quotas are excluded.
- 🔑 **Codex Account Authentication Errors**:
  Alerts on Codex accounts returning `401 Unauthorized` or `403 Forbidden` (explicitly ignoring temporary `429 Too Many Requests` rate limits). Notifies when cleared.
- 🎁 **Banked Reset Notifications**:
  Detects and alerts whenever an account receives an additional banked reset credit (`availableCount` increases).
- ⚠️ **Dashboard Auth Token Expiration**:
  Reads the unverified JWT `exp` claim for an informational warning when less than **3 hours** remain. Dashboard sessions normally last 24 hours. Rotate `ROUTER_AUTH_TOKEN` and restart the bot; automatic login is not implemented.

## Interactive Bot Commands

- `/status` — View overall 9router service health, /v1 state, total & active accounts, and Codex error count.
- `/quota` — Overview of active account quotas, remaining percentages, and reset deadlines.
- `/codex` — Dedicated Codex accounts list, auth status, and banked reset credit counts.
- `/accounts` — Quick list of registered accounts and active status across providers.
- `/ping` — Measure latency and ping the `/v1` endpoint on demand.
- `/help` — Display command help menu.

Commands only respond in `TELEGRAM_CHAT_ID`; in a group, every member of that configured group can use them. Replies are plain text, split into Unicode-safe messages without truncating accounts or interpreting upstream strings as markup.

Deduplication is in memory and resets on restart. Failed notification sends are logged, not retried; there is no durable queue or exactly-once delivery. Monitor logs also report dashboard/usage fetch failures; these failures are not separate Telegram alerts.

## Configuration

Copy `.env.example` to `.env` and fill in your values:

```bash
cp .env.example .env
```

| Variable | Description | Default |
|---|---|---|
| `TELOXIDE_TOKEN` | Telegram bot token from [@BotFather](https://t.me/BotFather) | *Required* |
| `TELEGRAM_CHAT_ID` | Alert destination and only chat allowed to use commands | *Required* |
| `ROUTER_URL` | HTTP(S) base URL, without userinfo, query or fragment | *Required* |
| `ROUTER_AUTH_TOKEN` | Dashboard JWT authentication token (`auth_token` cookie) | *Required* |
| `ROUTER_API_KEY` | Optional 9router API key (e.g. `sk-...`) for authenticated `/v1` pings | *Optional* |
| `POLL_INTERVAL_SECS` | Positive integer delay after each polling cycle | `60` |

## Running Locally

```bash
# Build and run
cargo run --release

# Run unit tests
cargo test
```

Tests are isolated: synthetic credentials and local HTTP servers, no production network dependency.

## Dependency review

Checked crates.io stable releases: teloxide 0.17.0, tokio 1.53.2, reqwest 0.13.5, serde 1.0.229, serde_json 1.0.151, chrono 0.4.45, anyhow 1.0.104, tracing 0.1.44, tracing-subscriber 0.3.23, dotenvy 0.15.7 and base64 0.23.1.

Keep reqwest 0.12.28 and base64 0.22.1 to share teloxide's existing dependency versions instead of compiling duplicate HTTP/base64 stacks. The other resolved direct dependencies match those stable releases. `Cargo.lock` pins the build. Tokio no longer enables `full`; chrono only enables `now`. Base64 replaces the handwritten JWT decoder. Removed unused response fields/serialization, dashboard Bearer auth, real credentials and the production-dependent smoke test.

Verification: full Rust test suite plus command/delivery smoke against local router and Telegram HTTP endpoints. Docker build requires a Docker daemon; it was unavailable on the review workstation.

The compiler reports a future-compatibility warning in transitive `proc-macro-error2` 2.0.1 (currently its latest stable release). Application Clippy checks pass; no vendored patch or warning suppression was added.


## Running with Docker / Docker Compose

### Using Docker Compose

```bash
docker compose up -d --build
```

### Using Docker CLI

```bash
docker build -t 9router-telegram-bot .
docker run -d --name 9router-bot --env-file .env 9router-telegram-bot
```
