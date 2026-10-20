# 9router Telegram Bot

A requests-only Telegram notification bot for [9router](https://github.com/decolua/9router), written in Rust using [teloxide 0.17](https://github.com/teloxide/teloxide) and reqwest. Both use rustls, without native TLS/OpenSSL.

## Features & Alerts

The bot continuously monitors your 9router deployment and delivers real-time notifications to your Telegram channel or chat:

On every startup, the bot sends an **ARMED** message to the configured chat before starting monitoring and command polling. Startup fails if that message cannot be delivered; it confirms notification access, not upstream service health.

- 🚨 **Serving Gateway Availability (`/v1`)**:
  Independently of five-minute quota polling, checks `GET /v1` every 60 seconds when healthy and every 10 seconds after a failure or while DOWN (delays after completed checks; no overlapping requests). Three consecutive network/timeout or unsuccessful HTTP responses (including 404) trigger DOWN, once per incident. HTTP 2xx resets the failure streak; HTTP 401 is accepted only without a configured API key, proving reachability rather than successful inference. One accepted response resolves a confirmed incident. Incident duration starts at the first failure, not the third confirming probe.
  Down alerts show the first observed incident time in **UTC+7**. Recovery shows incident time, resolve time, and elapsed incident duration (`h m s`). Times reflect polling observations, not the exact upstream outage boundaries; incident tracking is in memory and restarts begin a fresh observation.
- ⏳ **Weekly Quota Waste Warning**:
  Monitors enabled accounts returned by the usage-eligible providers API, across all pages, skipping `antigravity` accounts entirely. With **>20% remaining** and less than **24 hours**, **12 hours**, or **3 hours** until reset, warnings fire once per tier per account/quota/cycle. Reset timestamps within 60 seconds share an anchored cycle to tolerate upstream timestamp jitter; unlimited quotas are excluded. All accounts qualifying for the same warning tier within a polling cycle are batched into a single consolidated message.
  Quota warning reset deadlines are displayed in **UTC+7** (Asia/Ho_Chi_Minh); threshold calculations still use UTC instants.
- 🎉 **Weekly Quota Reset Notice**:
  Detects when an account's weekly quota cycle has reset (e.g. `resetAt` moves forward by $\ge 6$ hours), excluding `antigravity` accounts. Batches all accounts that reset in the same polling cycle into a single celebratory message announcing the new reset deadline in **UTC+7**.
- 🔑 **Codex Account Authentication Errors**:
  Alerts on Codex accounts returning `401 Unauthorized` or `403 Forbidden` (explicitly ignoring temporary `429 Too Many Requests` rate limits). Multiple errored or recovered accounts in the same polling cycle are batched into single consolidated messages.
- 🎁 **Banked Reset Notifications**:
  Detects and alerts whenever accounts receive additional banked reset credits (`availableCount` increases). Batches all credited accounts in the same cycle into a single message.
- ⚠️ **Dashboard Login Failure**:
  Uses `ROUTER_PASSWORD` to obtain in-memory sessions and refresh them five minutes before JWT expiry, on dashboard requests and polling cycles. Successful refresh is silent. Login failures produce one notice per failure episode; successful login rearms it. Retries wait at least five minutes and honor numeric Retry-After. Password login must be enabled; SSO-only deployments are unsupported.
- 🚀 **Update Available Alert**:
  Queries `/api/version` on 9router (which checks against npm registry `9router/latest`). If `hasUpdate` is true, sends an alert comparing current and latest version. Fires once per new version to prevent repeated notifications.
- 🏆 **Daily usage leaderboard**:
  At **00:00 Asia/Ho_Chi_Minh (UTC+7)**, independently of quota polling, send the top **5 API keys**, **5 models**, and **5 accounts** ranked by recorded request count. Key rows combine model/provider buckets; model rows combine providers; account rows combine models by connection ID. Only key display names are shown, never credentials.

  Each entry has a ranked name, abbreviated requests (`292K`, `4.5M`), estimated USD cost, and aligned monospace **In / Cache / Out** columns. Telegram has no native message tables; names stay outside the compact numeric rows to avoid wide tables on mobile. **In includes Cache** (cached input is a subset, not extra tokens). Costs come from 9router’s recorded pricing estimates, not subscription charges; absent pricing may understate costs. The summary shows the same separate token totals and estimated cost.

  Uses `/api/usage/stats?period=24h`: the upstream rolling 24-hour window at fetch time, approximately the previous local day when fetched at midnight. Recorded requests include all statuses upstream includes, not exclusively successful calls. No timezone dependency or VPS `TZ` setting is needed because UTC+7 has no DST.

  The bot must be running and dashboard authentication valid at midnight. On restart it waits for the next midnight; there is no missed-day backfill, persistent schedule state or automatic retry of failed daily reports. Failures are logged. The live preview exercised aggregation and Telegram delivery; midnight timing is covered by deterministic boundary tests.


## Interactive Bot Commands

- `/status` — View overall 9router service health, /v1 state, total & active accounts, and Codex error count.
- `/quota` — Overview of active account quotas, remaining percentages, and reset deadlines.
- `/codex` — Dedicated Codex accounts list, auth status, and banked reset credit counts.
- `/accounts` — Quick list of registered accounts and active status across providers.
- `/ping` — Measure latency and ping the `/v1` endpoint on demand.
- `/help` — Display command help menu.

Commands only respond in `TELEGRAM_CHAT_ID`; in a group, every member of that configured group can use them. Messages use HTML headings, bold metrics, code emails and expandable error details. Weekly alerts include a ten-cell remaining-quota bar and UTC+7 reset deadline. Quota/Usage URL buttons are derived from `ROUTER_URL`, never credentials.

All notification wording, kawaii tone, and kaomoji live in a dedicated language module at `src/messages.rs`. You can customize greetings, alert headers, reactions, and templates in this single location without touching alert engine logic or Telegram dispatching. Severity labels, metrics, timestamps, and escaped upstream error details remain technical and unchanged.
Quota tier labels are HTML-escaped in the shared formatter (`< 3 hours` becomes `&lt; 3 hours`), so Telegram does not interpret the threshold as a tag.

Dynamic display fields are HTML-escaped, flattened to one line and limited to 240 characters with a visible ellipsis. Longer replies split at complete, balanced lines; no HTML tags or entities are cut. Accounts are not truncated. No new dependencies or newer Telegram rich-message APIs are required.

Usage is fetched concurrently for monitored and command-listed accounts, capped at 16 in-flight requests to bound load on 9router. Deduplication is in memory and resets on restart. Failed notification sends are logged, not retried; there is no durable queue or exactly-once delivery. Monitor logs also report dashboard/usage fetch failures; these failures are not separate Telegram alerts.

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
| `ROUTER_PASSWORD` | Dashboard login password; session cookies remain in memory | *Required* |
| `ROUTER_API_KEY` | Optional 9router API key (e.g. `sk-...`) for authenticated `/v1` pings | *Optional* |
| `POLL_INTERVAL_SECS` | Positive integer delay after each polling cycle (five minutes plus cycle execution time between checks) | `300` |

## Running Locally

```bash
# Build and run
cargo run --release

# Run unit tests
cargo test

# Send preview/test messages to configured Telegram chat
cargo run --locked --example preview_messages
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
