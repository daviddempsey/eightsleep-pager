use crate::AppState;
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

const MAX_CHECK_AGE: i64 = 180;

#[derive(Clone, Default, serde::Serialize)]
struct Check {
    healthy: bool,
    checked_at: i64,
}

pub struct Monitoring {
    checks: Mutex<BTreeMap<&'static str, Check>>,
    pub wake_failures: AtomicU64,
    pub eligibility_failures: AtomicU64,
    pub non_primary_skips: AtomicU64,
    pub wake_attempts: AtomicU64,
    last_failure: AtomicU64,
}

impl Default for Monitoring {
    fn default() -> Self {
        Self {
            checks: Mutex::new(
                [
                    ("eight_sleep", Check::default()),
                    ("pagerduty", Check::default()),
                ]
                .into(),
            ),
            wake_failures: AtomicU64::new(0),
            eligibility_failures: AtomicU64::new(0),
            non_primary_skips: AtomicU64::new(0),
            wake_attempts: AtomicU64::new(0),
            last_failure: AtomicU64::new(0),
        }
    }
}

impl Monitoring {
    pub fn wake_failed(&self) {
        self.wake_failures.fetch_add(1, Ordering::Relaxed);
        self.last_failure
            .store(Utc::now().timestamp() as u64, Ordering::Relaxed);
    }

    pub fn eligibility_failed(&self) {
        self.eligibility_failures.fetch_add(1, Ordering::Relaxed);
        self.last_failure
            .store(Utc::now().timestamp() as u64, Ordering::Relaxed);
    }

    pub fn record(&self, dependency: &'static str, result: &anyhow::Result<()>) {
        self.checks.lock().unwrap().insert(
            dependency,
            Check {
                healthy: result.is_ok(),
                checked_at: Utc::now().timestamp(),
            },
        );
        if let Err(error) = result {
            tracing::error!(dependency, error = %error, "pager dependency check failed");
        }
    }

    pub fn healthy(&self) -> bool {
        let now = Utc::now().timestamp();
        self.checks
            .lock()
            .unwrap()
            .values()
            .all(|c| c.healthy && now - c.checked_at <= MAX_CHECK_AGE)
    }

    pub fn metrics(&self) -> String {
        let mut output = String::new();
        for (name, check) in self.checks.lock().unwrap().iter() {
            output.push_str(&format!("eightsleep_pager_dependency_healthy{{dependency=\"{name}\"}} {}\neightsleep_pager_dependency_checked_at_seconds{{dependency=\"{name}\"}} {}\n", u8::from(check.healthy), check.checked_at));
        }
        for (name, counter) in [
            ("wake_failures", &self.wake_failures),
            ("eligibility_failures", &self.eligibility_failures),
            ("non_primary_skips", &self.non_primary_skips),
            ("wake_attempts", &self.wake_attempts),
        ] {
            output.push_str(&format!(
                "# TYPE eightsleep_pager_{name}_total counter\neightsleep_pager_{name}_total {}\n",
                counter.load(Ordering::Relaxed)
            ));
        }
        output.push_str(&format!(
            "eightsleep_pager_last_failure_timestamp_seconds {}\n",
            self.last_failure.load(Ordering::Relaxed)
        ));
        output
    }
}

pub async fn check_dependencies(state: &AppState) {
    let (eight, pd, webhook) = tokio::join!(
        state.eight_sleep.check_access(),
        state
            .pagerduty
            .check_access(state.config.public_webhook_url.as_deref()),
        check_public_webhook(state)
    );
    state.monitoring.record("eight_sleep", &eight);
    state.monitoring.record("pagerduty", &pd);
    if state.config.public_webhook_url.is_some() {
        state.monitoring.record("webhook", &webhook);
    }
}

async fn check_public_webhook(state: &AppState) -> anyhow::Result<()> {
    if let Some(url) = &state.config.public_webhook_url {
        let body = br#"{"event":{"event_type":"pager.healthcheck","data":{}}}"#;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(state.config.pagerduty_webhook_secret.as_bytes())
                .expect("valid HMAC key");
        mac.update(body);
        reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()?
            .post(url)
            .header(
                "x-pagerduty-signature",
                format!("v1={}", hex::encode(mac.finalize().into_bytes())),
            )
            .body(body.to_vec())
            .send()
            .await?
            .error_for_status()?;
    }
    Ok(())
}

pub async fn run(state: Arc<AppState>) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        check_dependencies(&state).await;
    }
}
