use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tracing_subscriber::EnvFilter;

mod config;
mod eight_sleep;
mod escalation;
mod handlers;
mod pagerduty;

use config::Config;
use eight_sleep::EightSleepClient;
use pagerduty::PagerDutyClient;

pub struct AppState {
    pub eight_sleep: EightSleepClient,
    pub pagerduty: PagerDutyClient,
    pub config: Config,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("eightsleep_pager=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::from_env();
    let addr = format!("0.0.0.0:{}", config.port);

    let state = Arc::new(AppState {
        eight_sleep: EightSleepClient::new(
            config.eightsleep_email.clone(),
            config.eightsleep_password.clone(),
        ),
        pagerduty: PagerDutyClient::new(
            config.pagerduty_api_token.clone(),
            config.pagerduty_user_id.clone(),
        ),
        config,
    });

    let app = Router::new()
        .route("/webhook", post(handlers::webhook))
        .route("/health", get(handlers::health))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("eightsleep-pager listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}
