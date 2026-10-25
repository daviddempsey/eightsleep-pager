use anyhow::{Context, bail};
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::Deserialize;

const API_BASE: &str = "https://api.pagerduty.com";

pub struct PagerDutyClient {
    http: Client,
    api_token: String,
    user_id: String,
    api_base: String,
}

// --- Webhook V3 payload types ---

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub event: WebhookEvent,
}

#[derive(Debug, Deserialize)]
pub struct WebhookEvent {
    pub event_type: String,
    pub occurred_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct IncidentData {
    pub id: String,
    pub title: String,
    pub urgency: String,
    pub created_at: Option<DateTime<Utc>>,
    pub escalation_policy: Option<Reference>,
}

#[derive(Debug, Deserialize)]
pub struct Reference {
    pub id: String,
}

#[derive(Deserialize)]
struct IncidentResponse {
    incident: IncidentData,
}

// --- REST API response types ---

#[derive(Debug, Deserialize)]
struct OncallsResponse {
    oncalls: Vec<Oncall>,
    more: bool,
}

#[derive(Debug, Deserialize)]
struct Oncall {
    user: OncallUser,
    escalation_policy: Reference,
    escalation_level: u32,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct OncallUser {
    id: String,
}

impl PagerDutyClient {
    pub fn new(api_token: String, user_id: String) -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .build()
                .expect("failed to create PagerDuty client"),
            api_token,
            user_id,
            api_base: API_BASE.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_url(base: &str) -> Self {
        let mut client = Self::new("test-token".into(), "ME".into());
        client.api_base = format!("{base}/pd");
        client
    }

    pub async fn is_on_call(&self) -> anyhow::Result<bool> {
        let url = format!(
            "{}/oncalls?user_ids[]={}&earliest=true",
            self.api_base, self.user_id
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Token token={}", self.api_token))
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

    async fn get(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> anyhow::Result<reqwest::Response> {
        self.http
            .get(format!("{}{path}", self.api_base))
            .header("Authorization", format!("Token token={}", self.api_token))
            .header("Accept", "application/vnd.pagerduty+json;version=2")
            .query(query)
            .send()
            .await
            .context("PagerDuty request failed")?
            .error_for_status()
            .context("PagerDuty rejected request")
    }

    pub async fn check_access(&self, webhook_url: Option<&str>) -> anyhow::Result<()> {
        self.get(
            "/oncalls",
            &[
                ("user_ids[]".into(), self.user_id.clone()),
                ("limit".into(), "1".into()),
            ],
        )
        .await?;
        if let Some(url) = webhook_url {
            let mut offset = 0;
            loop {
                let data: serde_json::Value = self
                    .get(
                        "/webhook_subscriptions",
                        &[
                            ("limit".into(), "100".into()),
                            ("offset".into(), offset.to_string()),
                        ],
                    )
                    .await?
                    .json()
                    .await?;
                let subscriptions = data
                    .get("webhook_subscriptions")
                    .and_then(|v| v.as_array())
                    .context("invalid PagerDuty subscription response")?;
                if subscriptions.iter().any(|s| {
                    s.get("active").and_then(|v| v.as_bool()) == Some(true)
                        && s.pointer("/delivery_method/url").and_then(|v| v.as_str()) == Some(url)
                        && s.get("events")
                            .and_then(|v| v.as_array())
                            .is_some_and(|events| {
                                events
                                    .iter()
                                    .any(|e| e.as_str() == Some("incident.triggered"))
                            })
                }) {
                    break;
                }
                if data.get("more").and_then(|v| v.as_bool()) != Some(true) {
                    bail!(
                        "PagerDuty has no active incident.triggered subscription for the public webhook URL"
                    );
                }
                if subscriptions.is_empty() || offset >= 9900 {
                    bail!("PagerDuty subscription pagination did not complete");
                }
                offset += subscriptions.len();
            }
        }
        Ok(())
    }

    /// Primary must match this incident's policy and the instant it triggered.
    pub async fn is_primary_for(
        &self,
        incident: &IncidentData,
        occurred_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<bool> {
        let details;
        let incident = if incident.escalation_policy.is_some()
            && (occurred_at.is_some() || incident.created_at.is_some())
        {
            incident
        } else {
            details = self
                .get(&format!("/incidents/{}", incident.id), &[])
                .await?
                .json::<IncidentResponse>()
                .await
                .context("invalid PagerDuty incident response")?
                .incident;
            &details
        };
        let policy = &incident
            .escalation_policy
            .as_ref()
            .context("incident has no escalation policy")?
            .id;
        let at = occurred_at
            .or(incident.created_at)
            .context("incident has no event time")?;
        let mut offset = 0;
        loop {
            let data: OncallsResponse = self
                .get(
                    "/oncalls",
                    &[
                        ("user_ids[]".into(), self.user_id.clone()),
                        ("escalation_policy_ids[]".into(), policy.clone()),
                        ("since".into(), at.to_rfc3339()),
                        ("until".into(), (at + Duration::seconds(1)).to_rfc3339()),
                        ("limit".into(), "100".into()),
                        ("offset".into(), offset.to_string()),
                    ],
                )
                .await?
                .json()
                .await
                .context("invalid PagerDuty oncalls response")?;
            if data.oncalls.iter().any(|o| {
                o.user.id == self.user_id
                    && o.escalation_policy.id == *policy
                    && o.escalation_level == 1
                    && o.start.is_none_or(|start| start <= at)
                    && o.end.is_none_or(|end| at < end)
            }) {
                return Ok(true);
            }
            if !data.more {
                return Ok(false);
            }
            if data.oncalls.is_empty() || offset >= 9900 {
                bail!("PagerDuty oncalls pagination did not complete");
            }
            offset += data.oncalls.len();
        }
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
