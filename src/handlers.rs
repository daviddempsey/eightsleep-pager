use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::pagerduty::WebhookPayload;
use crate::AppState;

type HmacSha256 = Hmac<Sha256>;

pub async fn webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // HMAC signature verification
    let signature = match headers.get("x-pagerduty-signature") {
        Some(v) => v.to_str().unwrap_or(""),
        None => {
            tracing::warn!("webhook received without signature header");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let expected_hex = match signature.strip_prefix("v1=") {
        Some(h) => h,
        None => {
            tracing::warn!("invalid signature format");
            return StatusCode::UNAUTHORIZED;
        }
    };

    let mut mac =
        HmacSha256::new_from_slice(state.config.pagerduty_webhook_secret.as_bytes())
            .expect("HMAC accepts any key size");
    mac.update(&body);
    let computed = hex::encode(mac.finalize().into_bytes());

    if !constant_time_eq(computed.as_bytes(), expected_hex.as_bytes()) {
        tracing::warn!("HMAC signature mismatch");
        return StatusCode::UNAUTHORIZED;
    }

    // Parse webhook payload
    let payload: WebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse webhook payload");
            return StatusCode::BAD_REQUEST;
        }
    };

    if payload.event.event_type != "incident.triggered" {
        tracing::debug!(
            event_type = %payload.event.event_type,
            "ignoring non-triggered event"
        );
        return StatusCode::OK;
    }

    let incident = &payload.event.data;
    tracing::info!(
        incident_id = %incident.id,
        title = %incident.title,
        urgency = %incident.urgency,
        "incident triggered — waking bed"
    );

    // Trigger vibration alarm
    match state
        .eight_sleep
        .trigger_vibration(state.config.vibration_power)
        .await
    {
        Ok(()) => {
            tracing::info!(
                incident_id = %incident.id,
                power = state.config.vibration_power,
                "vibration alarm triggered"
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to trigger vibration alarm");
        }
    }

    // Always return 200 to prevent PagerDuty from retrying
    StatusCode::OK
}

pub async fn health() -> impl IntoResponse {
    StatusCode::OK
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
