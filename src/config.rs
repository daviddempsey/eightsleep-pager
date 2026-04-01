pub struct Config {
    pub port: u16,
    pub eightsleep_email: String,
    pub eightsleep_password: String,
    pub pagerduty_api_token: String,
    pub pagerduty_user_id: String,
    pub pagerduty_webhook_secret: String,
    pub vibration_power: u8,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("PORT must be a valid u16"),
            eightsleep_email: std::env::var("EIGHTSLEEP_EMAIL")
                .expect("EIGHTSLEEP_EMAIL must be set"),
            eightsleep_password: std::env::var("EIGHTSLEEP_PASSWORD")
                .expect("EIGHTSLEEP_PASSWORD must be set"),
            pagerduty_api_token: std::env::var("PAGERDUTY_API_TOKEN")
                .expect("PAGERDUTY_API_TOKEN must be set"),
            pagerduty_user_id: std::env::var("PAGERDUTY_USER_ID")
                .expect("PAGERDUTY_USER_ID must be set"),
            pagerduty_webhook_secret: std::env::var("PAGERDUTY_WEBHOOK_SECRET")
                .expect("PAGERDUTY_WEBHOOK_SECRET must be set"),
            vibration_power: std::env::var("VIBRATION_POWER")
                .unwrap_or_else(|_| "80".to_string())
                .parse()
                .expect("VIBRATION_POWER must be a valid u8"),
        }
    }
}
