use anyhow::{Context, bail};
use reqwest::Client;
use serde::Deserialize;

const API_BASE: &str = "https://api.pagerduty.com";

pub struct PagerDutyClient {
    http: Client,
    api_token: String,
    user_id: String,
}

// --- Webhook V3 payload types ---

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub event: WebhookEvent,
}

#[derive(Debug, Deserialize)]
pub struct WebhookEvent {
    pub event_type: String,
    pub data: IncidentData,
}

#[derive(Debug, Deserialize)]
pub struct IncidentData {
    pub id: String,
    pub title: String,
    pub urgency: String,
    pub status: String,
    pub html_url: String,
}

// --- REST API response types ---

#[derive(Debug, Deserialize)]
struct OncallsResponse {
    oncalls: Vec<Oncall>,
}

#[derive(Debug, Deserialize)]
struct Oncall {
    user: OncallUser,
}

#[derive(Debug, Deserialize)]
struct OncallUser {
    id: String,
}

impl PagerDutyClient {
    pub fn new(api_token: String, user_id: String) -> Self {
        Self {
            http: Client::new(),
            api_token,
            user_id,
        }
    }

    pub async fn is_on_call(&self) -> anyhow::Result<bool> {
        let resp = self
            .http
            .get(format!("{API_BASE}/oncalls"))
            .header("Authorization", format!("Token token={}", self.api_token))
            .query(&[("user_ids[]", &self.user_id), ("earliest", &"true".to_string())])
            .send()
            .await
            .context("failed to query on-call status")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("on-call query failed ({}): {}", status, body);
        }

        let data: OncallsResponse = resp.json().await.context("failed to parse oncalls")?;
        Ok(!data.oncalls.is_empty())
    }

    pub async fn acknowledge_incident(&self, incident_id: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(format!("{API_BASE}/incidents/{incident_id}"))
            .header("Authorization", format!("Token token={}", self.api_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "incident": {
                    "type": "incident_reference",
                    "status": "acknowledged"
                }
            }))
            .send()
            .await
            .context("failed to acknowledge incident")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("acknowledge failed ({}): {}", status, body);
        }

        tracing::info!(incident_id, "incident acknowledged");
        Ok(())
    }

    pub async fn snooze_incident(
        &self,
        incident_id: &str,
        duration_secs: u32,
    ) -> anyhow::Result<()> {
        let resp = self
            .http
            .post(format!("{API_BASE}/incidents/{incident_id}/snooze"))
            .header("Authorization", format!("Token token={}", self.api_token))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "duration": duration_secs }))
            .send()
            .await
            .context("failed to snooze incident")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("snooze failed ({}): {}", status, body);
        }

        tracing::info!(incident_id, duration_secs, "incident snoozed");
        Ok(())
    }
}
