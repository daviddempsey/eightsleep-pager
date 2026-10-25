use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::{
    AppState, config::Config, eight_sleep::EightSleepClient, handlers, pagerduty::PagerDutyClient,
};

#[derive(Default)]
struct Backend {
    calls: Mutex<Vec<(String, String)>>,
    primary: AtomicBool,
    reject_auth: AtomicBool,
    reject_pd: AtomicBool,
    reject_alarm: AtomicBool,
    offline: AtomicBool,
    unauthorized_once: AtomicBool,
    invalid_alarm_response: AtomicBool,
    oncalls: Mutex<Option<Value>>,
    intervals: Mutex<Option<Value>>,
    webhook_url: Mutex<String>,
    inactive_subscription: AtomicBool,
}

async fn backend(State(state): State<Arc<Backend>>, request: Request) -> impl IntoResponse {
    let path = request.uri().path().to_string();
    state
        .calls
        .lock()
        .unwrap()
        .push((request.method().to_string(), request.uri().to_string()));
    if path.starts_with("/client/") && state.unauthorized_once.swap(false, Ordering::Relaxed) {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({})));
    }
    let (code, body) = match path.as_str() {
        "/tokens" if state.reject_auth.load(Ordering::Relaxed) => {
            (400, json!({"error":"invalid_grant"}))
        }
        "/tokens" => (
            200,
            json!({"access_token":"fake-token","userId":"USER","expires_in":3600}),
        ),
        "/pd/oncalls" if state.reject_pd.load(Ordering::Relaxed) => (503, json!({})),
        "/pd/oncalls" => (
            200,
            state.oncalls.lock().unwrap().clone().unwrap_or_else(|| json!({"oncalls":[{"user":{"id":"ME"},"escalation_policy":{"id":"POLICY"},"escalation_level":if state.primary.load(Ordering::Relaxed) {1} else {5},"start":null,"end":null}],"more":false})),
        ),
        "/pd/incidents/INC" => (
            200,
            json!({"incident":{"id":"INC","title":"test","urgency":"high","created_at":Utc::now(),"escalation_policy":{"id":"POLICY"}}}),
        ),
        "/pd/users/ME" => (200, json!({"user":{"id":"ME"}})),
        "/pd/webhook_subscriptions" => (200,json!({"webhook_subscriptions":[{"active":!state.inactive_subscription.load(Ordering::Relaxed),"events":["incident.triggered"],"delivery_method":{"url":state.webhook_url.lock().unwrap().clone()}}],"more":false})),
        "/client/users/me" => (200, json!({"user":{"devices":["DEVICE"]}})),
        "/client/devices/DEVICE" => (
            200,
            json!({"result":{"online":!state.offline.load(Ordering::Relaxed),"lastHeard":Utc::now()}}),
        ),
        "/client/users/USER/intervals" => (
            200,
            state.intervals.lock().unwrap().clone().unwrap_or_else(|| json!({"intervals":[{"stages":[{"stage":"deep","duration":300}]}]})),
        ),
        "/app/users/USER/alarms" if state.reject_alarm.load(Ordering::Relaxed) => (503, json!({})),
        "/app/users/USER/alarms" if state.invalid_alarm_response.load(Ordering::Relaxed) => (200, json!({})),
        "/app/users/USER/alarms" if request.method() == reqwest::Method::GET => (200, json!({"alarms":[]})),
        "/app/users/USER/alarms" => (200, json!({"alarm":{"id":"alarm-created"},"alarms":[{"id":"existing-alarm"},{"id":"alarm-created"}]})),
        "/app/users/USER/temperature" => (200, json!({"currentLevel":0})),
        "/ping" if request.headers().contains_key("x-pagerduty-signature") => (200, json!({})),
        _ => (404, json!({})),
    };
    (StatusCode::from_u16(code).unwrap(), axum::Json(body))
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}

fn config() -> Config {
    Config {
        port: 8080,
        eightsleep_email: "test@example.invalid".into(),
        eightsleep_password: "test-password".into(),
        pagerduty_api_token: "test-token".into(),
        pagerduty_user_id: "ME".into(),
        pagerduty_webhook_secret: "webhook-test-secret".into(),
        vibration_power: 80,
        gentle_vibration_power: 40,
        thermal_wake_level: 5,
        escalation_delay_secs: 30,
        timezone: "America/New_York".into(),
        public_webhook_url: None,
    }
}

async fn fixture(
    backend_state: Arc<Backend>,
) -> (Arc<AppState>, String, Vec<tokio::task::JoinHandle<()>>) {
    let (base, mock_task) = serve(
        Router::new()
            .fallback(backend)
            .with_state(backend_state.clone()),
    )
    .await;
    *backend_state.webhook_url.lock().unwrap() = format!("{base}/ping");
    let mut config = config();
    config.public_webhook_url = Some(format!("{base}/ping"));
    let state = Arc::new(AppState {
        eight_sleep: EightSleepClient::with_urls(&base),
        pagerduty: PagerDutyClient::with_url(&base),
        config,
        monitoring: Default::default(),
    });
    let (app, app_task) = serve(
        Router::new()
            .route("/webhook", post(handlers::webhook))
            .route("/health", get(handlers::health))
            .route("/metrics", get(handlers::metrics))
            .with_state(state.clone()),
    )
    .await;
    (state, app, vec![mock_task, app_task])
}

async fn webhook(base: &str) -> reqwest::Response {
    let body = serde_json::to_vec(&json!({"event":{"id":"EVENT","event_type":"incident.triggered","occurred_at":Utc::now(),"data":{"id":"INC","title":"test","urgency":"high","created_at":Utc::now(),"escalation_policy":{"id":"POLICY"}}}})).unwrap();
    signed_request(base, body).await
}

async fn signed_request(base: &str, body: Vec<u8>) -> reqwest::Response {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"webhook-test-secret").unwrap();
    mac.update(&body);
    let signature = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
    reqwest::Client::new()
        .post(format!("{base}/webhook"))
        .header("x-pagerduty-signature", signature)
        .body(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn matching_primary_sends_vibration_and_temperature() {
    let backend = Arc::new(Backend::default());
    backend.primary.store(true, Ordering::Relaxed);
    let (_, base, tasks) = fixture(backend.clone()).await;
    assert_eq!(webhook(&base).await.status(), StatusCode::OK);
    let calls = backend.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(method, path)| method == "POST" && path.ends_with("/alarms"))
    );
    assert!(
        calls
            .iter()
            .any(|(method, path)| method == "PUT" && path.ends_with("/temperature"))
    );
    assert!(
        calls
            .iter()
            .any(|(_, path)| path.starts_with("/pd/oncalls?")
                && path.contains("since=")
                && path.contains("until=")
                && path.contains("escalation_policy_ids"))
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn wrong_policy_user_and_shift_cannot_qualify_as_primary() {
    for row in [
        json!({"user":{"id":"ME"},"escalation_policy":{"id":"OTHER"},"escalation_level":1}),
        json!({"user":{"id":"OTHER"},"escalation_policy":{"id":"POLICY"},"escalation_level":1}),
        json!({"user":{"id":"ME"},"escalation_policy":{"id":"POLICY"},"escalation_level":1,"end":"2020-01-01T00:00:00Z"}),
        json!({"user":{"id":"ME"},"escalation_policy":{"id":"POLICY"},"escalation_level":1,"start":"2100-01-01T00:00:00Z"}),
    ] {
        let backend = Arc::new(Backend::default());
        *backend.oncalls.lock().unwrap() = Some(json!({"oncalls":[row],"more":false}));
        let (_, base, tasks) = fixture(backend.clone()).await;
        assert_eq!(webhook(&base).await.status(), StatusCode::OK);
        assert!(
            backend
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, path)| path.starts_with("/pd/"))
        );
        for task in tasks {
            task.abort();
        }
    }
}

#[tokio::test]
async fn eligibility_failure_requests_retry_without_touching_bed_and_is_alertable() {
    let backend = Arc::new(Backend::default());
    backend.reject_pd.store(true, Ordering::Relaxed);
    let (state, base, tasks) = fixture(backend.clone()).await;
    assert_eq!(
        webhook(&base).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        state
            .monitoring
            .eligibility_failures
            .load(Ordering::Relaxed),
        1
    );
    assert!(
        state
            .monitoring
            .metrics()
            .contains("eightsleep_pager_last_failure_timestamp_seconds ")
    );
    assert!(
        backend
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, path)| path.starts_with("/pd/"))
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn failed_logins_are_throttled_across_wake_attempts() {
    let backend = Arc::new(Backend::default());
    backend.primary.store(true, Ordering::Relaxed);
    backend.reject_auth.store(true, Ordering::Relaxed);
    let (_, base, tasks) = fixture(backend.clone()).await;
    for _ in 0..2 {
        assert_eq!(
            webhook(&base).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    assert_eq!(
        backend
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p == "/tokens")
            .count(),
        1
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn revoked_token_is_replaced_and_original_request_retried() {
    let backend = Arc::new(Backend::default());
    backend.unauthorized_once.store(true, Ordering::Relaxed);
    let (state, _, tasks) = fixture(backend.clone()).await;
    state.eight_sleep.current_sleep_depth().await.unwrap();
    assert_eq!(
        backend
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p == "/tokens")
            .count(),
        2
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn healthy_probes_only_read_bed_and_health_recovers() {
    let backend = Arc::new(Backend::default());
    backend.offline.store(true, Ordering::Relaxed);
    let (state, base, tasks) = fixture(backend.clone()).await;
    crate::monitoring::check_dependencies(&state).await;
    assert_eq!(
        reqwest::get(format!("{base}/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    backend.offline.store(false, Ordering::Relaxed);
    crate::monitoring::check_dependencies(&state).await;
    assert_eq!(
        reqwest::get(format!("{base}/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let metrics = reqwest::get(format!("{base}/metrics"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for dep in ["eight_sleep", "pagerduty", "webhook"] {
        assert!(metrics.contains(&format!("dependency=\"{dep}\"}} 1")));
    }
    assert!(
        backend
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(method, path)| method == "GET" || path == "/tokens" || path == "/ping")
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn login_and_alarm_read_failures_make_health_fail() {
    for auth in [true, false] {
        let backend = Arc::new(Backend::default());
        backend.reject_auth.store(auth, Ordering::Relaxed);
        backend.reject_alarm.store(!auth, Ordering::Relaxed);
        let (state, base, tasks) = fixture(backend).await;
        crate::monitoring::check_dependencies(&state).await;
        assert_eq!(
            reqwest::get(format!("{base}/health"))
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            state
                .monitoring
                .metrics()
                .contains("dependency=\"eight_sleep\"} 0")
        );
        for task in tasks {
            task.abort();
        }
    }
}

#[tokio::test]
async fn failed_alarm_creation_is_recorded_and_not_acknowledged() {
    for invalid_response in [false, true] {
        let backend = Arc::new(Backend::default());
        backend.primary.store(true, Ordering::Relaxed);
        backend
            .reject_alarm
            .store(!invalid_response, Ordering::Relaxed);
        backend
            .invalid_alarm_response
            .store(invalid_response, Ordering::Relaxed);
        let (state, base, tasks) = fixture(backend).await;
        assert_eq!(
            webhook(&base).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(state.monitoring.wake_failures.load(Ordering::Relaxed), 1);
        for task in tasks {
            task.abort();
        }
    }
}

#[tokio::test]
async fn newest_sleep_interval_wins_and_stale_awake_data_is_unknown() {
    let backend = Arc::new(Backend::default());
    let fresh = json!({"ts":Utc::now()-chrono::Duration::seconds(300),"stages":[{"stage":"deep","duration":300}]});
    let stale = json!({"ts":"2020-01-01T00:00:00Z","stages":[{"stage":"awake","duration":300}]});
    *backend.intervals.lock().unwrap() = Some(json!({"intervals":[fresh,stale]}));
    let (state, _, tasks) = fixture(backend.clone()).await;
    assert_eq!(
        state.eight_sleep.current_sleep_depth().await.unwrap(),
        crate::eight_sleep::SleepDepth::Deep
    );
    *backend.intervals.lock().unwrap() = Some(json!({"intervals":[stale]}));
    assert_eq!(
        state.eight_sleep.current_sleep_depth().await.unwrap(),
        crate::eight_sleep::SleepDepth::Unknown
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn signed_nonincident_probe_does_not_contact_dependencies() {
    let backend = Arc::new(Backend::default());
    let (_, base, tasks) = fixture(backend.clone()).await;
    assert_eq!(
        signed_request(
            &base,
            br#"{"event":{"event_type":"pager.healthcheck","data":{}}}"#.to_vec()
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert!(backend.calls.lock().unwrap().is_empty());
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn disabled_upstream_webhook_subscription_makes_health_fail() {
    let backend = Arc::new(Backend::default());
    backend.inactive_subscription.store(true, Ordering::Relaxed);
    let (state, base, tasks) = fixture(backend).await;
    crate::monitoring::check_dependencies(&state).await;
    assert_eq!(
        reqwest::get(format!("{base}/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        state
            .monitoring
            .metrics()
            .contains("dependency=\"pagerduty\"} 0")
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn cleanup_uses_created_alarm_not_first_existing_alarm() {
    let backend = Arc::new(Backend::default());
    let (state, _, tasks) = fixture(backend).await;
    assert_eq!(
        state
            .eight_sleep
            .trigger_vibration(80, "America/New_York")
            .await
            .unwrap(),
        "alarm-created"
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn secondary_incident_does_not_contact_bed() {
    let backend = Arc::new(Backend::default());
    let (_, base, tasks) = fixture(backend.clone()).await;
    assert_eq!(webhook(&base).await.status(), StatusCode::OK);
    assert!(
        backend
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|(_, path)| path.starts_with("/pd/")),
        "non-primary incident reached Eight Sleep"
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn failed_wake_is_not_reported_as_webhook_success() {
    let backend = Arc::new(Backend::default());
    backend.primary.store(true, Ordering::Relaxed);
    backend.reject_auth.store(true, Ordering::Relaxed);
    let (_, base, tasks) = fixture(backend).await;
    assert_eq!(
        webhook(&base).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    for task in tasks {
        task.abort();
    }
}
