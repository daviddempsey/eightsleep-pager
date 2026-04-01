pub struct Config {
    pub port: u16,
    pub eightsleep_email: String,
    pub eightsleep_password: String,
    pub pagerduty_api_token: String,
    pub pagerduty_user_id: String,
    pub pagerduty_webhook_secret: String,
    pub vibration_power: u8,
    pub gentle_vibration_power: u8,
    pub thermal_wake_level: i8,
    pub escalation_delay_secs: u64,
    pub timezone: String,
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
            gentle_vibration_power: std::env::var("GENTLE_VIBRATION_POWER")
                .unwrap_or_else(|_| "40".to_string())
                .parse()
                .expect("GENTLE_VIBRATION_POWER must be a valid u8"),
            thermal_wake_level: std::env::var("THERMAL_WAKE_LEVEL")
                .unwrap_or_else(|_| "50".to_string())
                .parse()
                .expect("THERMAL_WAKE_LEVEL must be a valid i8"),
            escalation_delay_secs: std::env::var("ESCALATION_DELAY_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .expect("ESCALATION_DELAY_SECS must be a valid u64"),
            timezone: std::env::var("TIMEZONE")
                .unwrap_or_else(|_| "America/New_York".to_string()),
        }
    }
}
