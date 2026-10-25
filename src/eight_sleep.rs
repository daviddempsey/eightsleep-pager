use anyhow::{Context, bail};
use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::RwLock;

const AUTH_URL: &str = "https://auth-api.8slp.net/v1/tokens";
const CLIENT_API: &str = "https://client-api.8slp.net/v1";
const APP_API: &str = "https://app-api.8slp.net/v1";

// Extracted from the Eight Sleep Android APK — these are the app's default credentials,
// embedded in every community library that talks to this API.
const CLIENT_ID: &str = "0894c7f33bb94800a03f1f4df13a4f38";
const CLIENT_SECRET: &str = "f0954a3ed5763ba3d06834c73731a32f15f168f47d4f164751275def86db0c76";

struct TokenState {
    access_token: String,
    user_id: String,
    expires_at: DateTime<Utc>,
}

#[derive(Default)]
struct AuthState {
    token: Option<TokenState>,
    retry_after: Option<Instant>,
}

pub struct EightSleepClient {
    http: Client,
    email: String,
    password: String,
    token: RwLock<AuthState>,
    auth_url: String,
    client_api: String,
    app_api: String,
}

#[derive(Debug, Deserialize)]
struct AuthResponse {
    access_token: String,
    #[serde(rename = "userId")]
    user_id: String,
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct IntervalsResponse {
    intervals: Vec<SleepInterval>,
}

#[derive(Debug, Deserialize)]
pub struct SleepInterval {
    pub ts: Option<DateTime<Utc>>,
    pub stages: Option<Vec<SleepStage>>,
    #[serde(default)]
    pub score: i32,
}

#[derive(Debug, Deserialize)]
pub struct SleepStage {
    pub stage: String,
    pub duration: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepDepth {
    Awake,
    Light,
    Deep,
    Rem,
    OutOfBed,
    Unknown,
}

impl std::fmt::Display for SleepDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Awake => write!(f, "awake"),
            Self::Light => write!(f, "light"),
            Self::Deep => write!(f, "deep"),
            Self::Rem => write!(f, "rem"),
            Self::OutOfBed => write!(f, "out_of_bed"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Serialize)]
struct TemperatureRequest {
    #[serde(rename = "currentLevel")]
    current_level: i8,
}

impl EightSleepClient {
    pub fn new(email: String, password: String) -> Self {
        let http = Client::builder()
            .user_agent("okhttp/4.9.3")
            .timeout(StdDuration::from_secs(8))
            .build()
            .expect("failed to create HTTP client");

        Self {
            http,
            email,
            password,
            token: RwLock::new(AuthState::default()),
            auth_url: AUTH_URL.into(),
            client_api: CLIENT_API.into(),
            app_api: APP_API.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_urls(base: &str) -> Self {
        let mut client = Self::new("test@example.invalid".into(), "test-password".into());
        client.auth_url = format!("{base}/tokens");
        client.client_api = format!("{base}/client");
        client.app_api = format!("{base}/app");
        client
    }

    async fn ensure_token(&self) -> anyhow::Result<(String, String)> {
        // Fast path: read lock
        {
            let state = self.token.read().await;
            if let Some(ref t) = state.token
                && Utc::now() < t.expires_at - Duration::seconds(30)
            {
                return Ok((t.access_token.clone(), t.user_id.clone()));
            }
        }

        // Slow path: write lock
        let mut state = self.token.write().await;

        // Double-check after acquiring write lock
        if let Some(ref t) = state.token
            && Utc::now() < t.expires_at - Duration::seconds(30)
        {
            return Ok((t.access_token.clone(), t.user_id.clone()));
        }

        // Rate-limit auth attempts
        if state
            .retry_after
            .is_some_and(|retry| Instant::now() < retry)
        {
            bail!("Eight Sleep authentication is backing off after a rejected login");
        }

        // Record the attempt before sending: failed logins must be throttled too.
        state.retry_after = Some(Instant::now() + StdDuration::from_secs(60));
        let new_state = self.authenticate().await?;
        let token = new_state.access_token.clone();
        let user_id = new_state.user_id.clone();
        state.token = Some(new_state);
        state.retry_after = None;
        Ok((token, user_id))
    }

    async fn authenticate(&self) -> anyhow::Result<TokenState> {
        let now = Utc::now();

        let resp = self
            .http
            .post(&self.auth_url)
            .json(&serde_json::json!({
                "client_id": CLIENT_ID,
                "client_secret": CLIENT_SECRET,
                "grant_type": "password",
                "username": self.email,
                "password": self.password,
            }))
            .send()
            .await
            .context("failed to reach Eight Sleep auth endpoint")?;

        let status = resp.status();
        if !status.is_success() {
            bail!("Eight Sleep authentication failed ({status}); check saved credentials");
        }

        let auth: AuthResponse = resp.json().await.context("failed to parse auth response")?;

        Ok(TokenState {
            access_token: auth.access_token,
            user_id: auth.user_id,
            expires_at: now + Duration::seconds(auth.expires_in),
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> anyhow::Result<reqwest::Response> {
        for attempt in 0..2 {
            let (token, _) = self.ensure_token().await?;
            let mut request = self.http.request(method.clone(), url).bearer_auth(&token);
            if let Some(ref body) = body {
                request = request.json(body);
            }
            let response = request.send().await.context("Eight Sleep request failed")?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                let mut state = self.token.write().await;
                if state
                    .token
                    .as_ref()
                    .is_some_and(|t| t.access_token == token)
                {
                    state.token = None;
                }
                continue;
            }
            return response
                .error_for_status()
                .context("Eight Sleep rejected request");
        }
        unreachable!()
    }

    /// Read the same dependencies used for waking without changing the bed.
    pub async fn check_access(&self) -> anyhow::Result<()> {
        let (_, user_id) = self.ensure_token().await?;
        let account: serde_json::Value = self
            .request(
                reqwest::Method::GET,
                &format!("{}/users/me", self.client_api),
                None,
            )
            .await?
            .json()
            .await?;
        let devices = account
            .pointer("/user/devices")
            .and_then(|v| v.as_array())
            .context("Eight Sleep account has no devices")?;
        if devices.is_empty() {
            bail!("Eight Sleep account has no devices");
        }
        let device_id = devices[0]
            .as_str()
            .context("invalid Eight Sleep device ID")?;
        let device: serde_json::Value = self
            .request(
                reqwest::Method::GET,
                &format!("{}/devices/{device_id}", self.client_api),
                None,
            )
            .await?
            .json()
            .await?;
        if device.pointer("/result/online").and_then(|v| v.as_bool()) != Some(true) {
            bail!("Eight Sleep bed is offline or its online state is unavailable");
        }
        let last_heard = device
            .pointer("/result/lastHeard")
            .and_then(|v| v.as_str())
            .context("Eight Sleep bed has no last-contact timestamp")?;
        let last_heard = DateTime::parse_from_rfc3339(last_heard)
            .context("invalid Eight Sleep last-contact timestamp")?;
        if Utc::now().signed_duration_since(last_heard).num_seconds() > 300 {
            bail!("Eight Sleep bed has not contacted its cloud for over five minutes");
        }
        // Interval errors must not silently masquerade as a functioning wake integration.
        self.current_sleep_depth().await?;
        self.request(
            reqwest::Method::GET,
            &format!("{}/users/{user_id}/temperature", self.app_api),
            None,
        )
        .await?;
        // The read endpoint is v2, whereas creating/deleting one-off alarms is v1.
        let alarms_url = format!(
            "{}/users/{user_id}/alarms",
            self.app_api.replacen("/v1", "/v2", 1)
        );
        self.request(reqwest::Method::GET, &alarms_url, None)
            .await?;
        Ok(())
    }

    pub async fn current_sleep_depth(&self) -> anyhow::Result<SleepDepth> {
        let (_, user_id) = self.ensure_token().await?;

        let resp = self
            .request(
                reqwest::Method::GET,
                &format!("{}/users/{user_id}/intervals", self.client_api),
                None,
            )
            .await
            .context("failed to fetch sleep intervals")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to get intervals ({}): {}", status, body);
        }

        let data: IntervalsResponse = resp.json().await.context("failed to parse intervals")?;

        let depth = data
            .intervals
            .iter()
            .max_by_key(|interval| interval.ts)
            .and_then(|interval| {
                let stages = interval.stages.as_ref()?;
                let seconds: u64 = stages.iter().map(|s| s.duration).sum();
                let end = interval
                    .ts?
                    .checked_add_signed(Duration::seconds(i64::try_from(seconds).ok()?))?;
                let age = (Utc::now() - end).num_seconds();
                // Completed nights and delayed cloud data cannot establish that the user is awake.
                if !(-60..=300).contains(&age) {
                    return None;
                }
                stages.last().map(|stage| match stage.stage.as_str() {
                    "awake" => SleepDepth::Awake,
                    "light" => SleepDepth::Light,
                    "deep" => SleepDepth::Deep,
                    "rem" => SleepDepth::Rem,
                    "out" => SleepDepth::OutOfBed,
                    _ => SleepDepth::Unknown,
                })
            })
            .unwrap_or(SleepDepth::Unknown);

        Ok(depth)
    }

    pub async fn trigger_vibration(&self, power: u8, tz: &str) -> anyhow::Result<String> {
        let (_, user_id) = self.ensure_token().await?;

        // Create an alarm set to fire 1 minute from now in the user's local timezone.
        // The Eight Sleep API interprets alarm times as local time.
        let tz: chrono_tz::Tz = tz.parse().context("invalid timezone")?;
        let local_now = Utc::now().with_timezone(&tz) + Duration::minutes(1);
        let time = format!("{:02}:{:02}:00", local_now.hour(), local_now.minute());
        let weekday = match local_now.weekday() {
            chrono::Weekday::Mon => "monday",
            chrono::Weekday::Tue => "tuesday",
            chrono::Weekday::Wed => "wednesday",
            chrono::Weekday::Thu => "thursday",
            chrono::Weekday::Fri => "friday",
            chrono::Weekday::Sat => "saturday",
            chrono::Weekday::Sun => "sunday",
        };

        let mut week_days = serde_json::Map::new();
        for day in [
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ] {
            week_days.insert(day.to_string(), serde_json::Value::Bool(day == weekday));
        }

        let resp = self
            .request(
                reqwest::Method::POST,
                &format!("{}/users/{user_id}/alarms", self.app_api),
                Some(serde_json::json!({
                    "enabled": true,
                    "time": time,
                    "repeat": {
                        "enabled": false,
                        "weekDays": week_days,
                    },
                    "vibration": {
                        "enabled": true,
                        "powerLevel": power,
                        "pattern": "RISE",
                    },
                    "thermal": { "enabled": false, "level": 0 },
                    "smart": {
                        "lightSleepEnabled": false,
                        "sleepCapEnabled": false,
                        "sleepCapMinutes": 0,
                    },
                    "audio": { "enabled": false, "trackId": "", "level": 0 },
                    "snoozing": false,
                })),
            )
            .await
            .context("failed to trigger vibration")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to trigger vibration ({}): {}", status, body);
        }

        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let alarm_id = body
            .pointer("/alarm/id")
            .or_else(|| body.pointer("/id"))
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .context("Eight Sleep alarm response contained no created-alarm ID; refusing to use another alarm")?
            .to_string();

        tracing::info!(power, alarm_id, "eight sleep vibration alarm created");
        Ok(alarm_id)
    }

    pub async fn delete_alarm(&self, alarm_id: &str) -> anyhow::Result<()> {
        let (_, user_id) = self.ensure_token().await?;

        let resp = self
            .request(
                reqwest::Method::DELETE,
                &format!("{}/users/{user_id}/alarms/{alarm_id}", self.app_api),
                None,
            )
            .await
            .context("failed to delete alarm")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to delete alarm ({}): {}", status, body);
        }

        tracing::info!(alarm_id, "eight sleep alarm deleted");
        Ok(())
    }

    pub async fn set_temperature(&self, level: i8) -> anyhow::Result<()> {
        let (_, user_id) = self.ensure_token().await?;

        let resp = self
            .request(
                reqwest::Method::PUT,
                &format!("{}/users/{user_id}/temperature", self.app_api),
                Some(serde_json::to_value(TemperatureRequest {
                    current_level: level,
                })?),
            )
            .await
            .context("failed to set temperature")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to set temperature ({}): {}", status, body);
        }

        tracing::info!(level, "eight sleep temperature set");
        Ok(())
    }
}
