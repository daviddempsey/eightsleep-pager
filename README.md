# eightsleep-pager

A Rust service that bridges PagerDuty incident notifications with Eight Sleep bed hardware. When you get paged during sleep, your bed wakes you via vibration and temperature change based on your current sleep stage.

## How it works

1. PagerDuty sends a webhook when an incident is triggered
2. The service verifies that your configured PagerDuty user was **primary (escalation level 1) for this incident's escalation policy at the event time**. Secondary responders, fixed escalation contacts, and primaries for unrelated policies do not wake the bed. Failed eligibility checks return HTTP 503 for retry and raise a monitoring alert.
3. The service queries your Eight Sleep bed for your current sleep stage. It selects the newest interval and treats data older than five minutes as unknown.
4. Based on sleep depth, it chooses an appropriate wake strategy:
   - **Awake / Out of bed** — no action (phone notification is enough)
   - **Light / REM sleep** — gentle vibration, escalates after 30s if still asleep
   - **Deep / unknown sleep** — full vibration + temperature change
5. Vibration uses an alarm in the next local clock minute. Alarms are automatically cleaned up after 10 minutes. Initial wake failures return HTTP 503 for PagerDuty retry; asynchronous escalation and cleanup failures are also monitored.

## Setup

### Prerequisites

- An Eight Sleep bed with an active account
- A PagerDuty account with webhook subscription access
- A server with a public HTTPS endpoint (for receiving webhooks)

### Configuration

Copy `.env.example` to `.env` and fill in your credentials:

```
PORT=8080
EIGHTSLEEP_EMAIL=your-email
EIGHTSLEEP_PASSWORD=your-password
PAGERDUTY_API_TOKEN=u+your-token
PAGERDUTY_USER_ID=PXXXXXX
PAGERDUTY_WEBHOOK_SECRET=your-webhook-secret
VIBRATION_POWER=80
GENTLE_VIBRATION_POWER=40
THERMAL_WAKE_LEVEL=5
ESCALATION_DELAY_SECS=30
TIMEZONE=America/New_York
PUBLIC_WEBHOOK_URL=https://your-domain.com/webhook
```

### PagerDuty Webhook

Create a webhook subscription pointing to your server:

```bash
curl -X POST https://api.pagerduty.com/webhook_subscriptions \
  -H "Authorization: Token token=YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "webhook_subscription": {
      "type": "webhook_subscription",
      "delivery_method": {
        "type": "http_delivery_method",
        "url": "https://your-domain.com/webhook"
      },
      "events": ["incident.triggered"],
      "filter": { "type": "account_reference" }
    }
  }'
```

Use the `secret` from the response as your `PAGERDUTY_WEBHOOK_SECRET`.

### Running

```bash
# Local
cargo run

# Docker
docker compose up -d --build
```

### Endpoints

- `POST /webhook` — PagerDuty webhook receiver (HMAC-verified)
- `GET /health` — HTTP 200 only when periodic dependency checks are successful and less than three minutes old; otherwise HTTP 503
- `GET /live` — process liveness/readiness, independent of external outages
- `GET /metrics` — Prometheus dependency state, check timestamps, primary skips, wake attempts, and failure metrics

## Failure monitoring

Every minute, the service performs read-only checks of Eight Sleep login, device online/last-contact state, sleep intervals, temperature access, alarm access, and PagerDuty on-call API access. With `PUBLIC_WEBHOOK_URL` configured, it also sends a signed non-incident probe through the public HTTPS webhook route; this never triggers a bed action. This checks the route, signature verification, and payload handling, but does not prove that PagerDuty will deliver every future event.

Authentication is cached and retried once on a rejected access token. Failed login attempts back off for one minute, including when no token has ever been obtained. HTTP requests have an eight-second timeout. Update the Kubernetes Secret and roll the deployment after changing account credentials.

For the existing K3s deployment, apply `deploy/deployment-patch.yaml` as a strategic merge patch and set its image to a published immutable digest. Apply `deploy/monitoring.yaml` for independent pod scraping and alerts. The rules use the existing Alertmanager warning/critical route to ntfy; they do not require a separate notifier in the pager.

Dependency and scrape outages alert after two minutes of sustained failure, plus check/scrape/evaluation and notification delay (typically around 3–4 minutes). Stopped health checks alert after three minutes plus one minute pending. Bed action and eligibility errors alert on the next evaluation and remain visible for 15 minutes. Prometheus detects missing pods even when the pager cannot report its own failure. Existing ntfy repeat/resolution behavior applies.

Monitoring confirms API access and bed connectivity. Physical vibration and temperature delivery still require a deliberate hardware test; cloud success alone cannot prove they woke the sleeper.

## Disclaimer

This project uses Eight Sleep's reverse-engineered cloud API, which is undocumented and subject to change at any time.
