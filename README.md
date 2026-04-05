# eightsleep-pager

A Rust service that bridges PagerDuty incident notifications with Eight Sleep bed hardware. When you get paged during sleep, your bed wakes you via vibration and temperature change based on your current sleep stage.

## How it works

1. PagerDuty sends a webhook when an incident is triggered
2. The service queries your Eight Sleep bed for your current sleep stage
3. Based on sleep depth, it chooses an appropriate wake strategy:
   - **Awake / Out of bed** — no action (phone notification is enough)
   - **Light / REM sleep** — gentle vibration, escalates after 30s if still asleep
   - **Deep sleep** — immediate full vibration + temperature spike
4. Alarms are automatically cleaned up after 10 minutes

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
- `GET /health` — health check

## Disclaimer

This project uses Eight Sleep's reverse-engineered cloud API, which is undocumented and subject to change at any time.
