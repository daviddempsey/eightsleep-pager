use anyhow::{Context, bail};
use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
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
    last_auth_attempt: Option<DateTime<Utc>>,
}

pub struct EightSleepClient {
    http: Client,
    email: String,
    password: String,
    token: RwLock<Option<TokenState>>,
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
struct AlarmRequest {
    #[serde(rename = "vibrationPattern")]
    vibration_pattern: String,
    #[serde(rename = "vibrationLevel")]
    vibration_level: u8,
    enabled: bool,
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
            .build()
            .expect("failed to create HTTP client");

        Self {
            http,
            email,
            password,
            token: RwLock::new(None),
        }
    }

    async fn ensure_token(&self) -> anyhow::Result<(String, String)> {
        // Fast path: read lock
        {
            let state = self.token.read().await;
            if let Some(ref t) = *state {
                if Utc::now() < t.expires_at - Duration::seconds(30) {
                    return Ok((t.access_token.clone(), t.user_id.clone()));
                }
            }
        }

        // Slow path: write lock
        let mut state = self.token.write().await;

        // Double-check after acquiring write lock
        if let Some(ref t) = *state {
            if Utc::now() < t.expires_at - Duration::seconds(30) {
                return Ok((t.access_token.clone(), t.user_id.clone()));
            }
        }

        // Rate-limit auth attempts
        if let Some(ref t) = *state {
            if let Some(last) = t.last_auth_attempt {
                if Utc::now() - last < Duration::seconds(30) {
                    bail!("auth rate-limited, last attempt was less than 30s ago");
                }
            }
        }

        let new_state = self.authenticate().await?;
        let token = new_state.access_token.clone();
        let user_id = new_state.user_id.clone();
        *state = Some(new_state);
        Ok((token, user_id))
    }

    async fn authenticate(&self) -> anyhow::Result<TokenState> {
        let now = Utc::now();

        let resp = self
            .http
            .post(AUTH_URL)
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
            let body = resp.text().await.unwrap_or_default();
            bail!("Eight Sleep auth failed ({}): {}", status, body);
        }

        let auth: AuthResponse = resp.json().await.context("failed to parse auth response")?;

        Ok(TokenState {
            access_token: auth.access_token,
            user_id: auth.user_id,
            expires_at: now + Duration::seconds(auth.expires_in),
            last_auth_attempt: Some(now),
        })
    }

    pub async fn current_sleep_depth(&self) -> anyhow::Result<SleepDepth> {
        let (token, user_id) = self.ensure_token().await?;

        let resp = self
            .http
            .get(format!("{CLIENT_API}/users/{user_id}/intervals"))
            .bearer_auth(&token)
            .send()
            .await
            .context("failed to fetch sleep intervals")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to get intervals ({}): {}", status, body);
        }

        let data: IntervalsResponse =
            resp.json().await.context("failed to parse intervals")?;

        let depth = data
            .intervals
            .last()
            .and_then(|interval| {
                interval.stages.as_ref()?.last().map(|stage| match stage.stage.as_str() {
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

    pub async fn trigger_vibration(&self, power: u8) -> anyhow::Result<()> {
        let (token, user_id) = self.ensure_token().await?;

        let resp = self
            .http
            .post(format!("{APP_API}/users/{user_id}/alarms"))
            .bearer_auth(&token)
            .json(&AlarmRequest {
                vibration_pattern: "RISE".to_string(),
                vibration_level: power,
                enabled: true,
            })
            .send()
            .await
            .context("failed to trigger vibration")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("failed to trigger vibration ({}): {}", status, body);
        }

        tracing::info!(power, "eight sleep vibration alarm triggered");
        Ok(())
    }

    pub async fn set_temperature(&self, level: i8) -> anyhow::Result<()> {
        let (token, user_id) = self.ensure_token().await?;

        let resp = self
            .http
            .put(format!("{APP_API}/users/{user_id}/temperature"))
            .bearer_auth(&token)
            .json(&TemperatureRequest {
                current_level: level,
            })
            .send()
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
