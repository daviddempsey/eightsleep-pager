use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::AppState;
use crate::pagerduty::{IncidentData, WebhookPayload};

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

    let mut mac = HmacSha256::new_from_slice(state.config.pagerduty_webhook_secret.as_bytes())
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

    let incident: IncidentData = match serde_json::from_value(payload.event.data) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse incident data");
            return StatusCode::BAD_REQUEST;
        }
    };

    match state
        .pagerduty
        .is_primary_for(&incident, payload.event.occurred_at)
        .await
    {
        Ok(false) => {
            state
                .monitoring
                .non_primary_skips
                .fetch_add(1, Ordering::Relaxed);
            tracing::info!(incident_id = %incident.id, "not primary for incident policy; skipping bed wake");
            return StatusCode::OK;
        }
        Err(error) => {
            state.monitoring.eligibility_failed();
            tracing::error!(incident_id = %incident.id, error = %error, "cannot verify primary; skipping bed wake and requesting retry");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        Ok(true) => {}
    }

    tracing::info!(
        incident_id = %incident.id,
        title = %incident.title,
        urgency = %incident.urgency,
        "incident triggered — waking bed"
    );

    state
        .monitoring
        .wake_attempts
        .fetch_add(1, Ordering::Relaxed);
    match crate::escalation::wake(&state, &incident.id).await {
        Ok(()) => StatusCode::OK,
        Err(error) => {
            tracing::error!(incident_id = %incident.id, error = %error, "bed wake failed; requesting webhook retry");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

pub async fn health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.monitoring.healthy() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

pub async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [("content-type", "text/plain; version=0.0.4")],
        state.monitoring.metrics(),
    )
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
