use std::sync::Arc;
use std::time::Duration;

use crate::eight_sleep::SleepDepth;
use crate::AppState;

enum WakeAction {
    None,
    Gentle { vibration_power: u8 },
    Full { vibration_power: u8, temp: i8 },
}

fn choose_action(depth: SleepDepth, state: &AppState) -> WakeAction {
    match depth {
        SleepDepth::Awake | SleepDepth::OutOfBed => WakeAction::None,
        SleepDepth::Light | SleepDepth::Rem => WakeAction::Gentle {
            vibration_power: state.config.gentle_vibration_power,
        },
        SleepDepth::Deep | SleepDepth::Unknown => WakeAction::Full {
            vibration_power: state.config.vibration_power,
            temp: state.config.thermal_wake_level,
        },
    }
}

pub async fn wake(state: &Arc<AppState>, incident_id: &str) {
    let depth = match state.eight_sleep.current_sleep_depth().await {
        Ok(d) => {
            tracing::info!(incident_id, sleep_depth = %d, "detected sleep stage");
            d
        }
        Err(e) => {
            tracing::warn!(
                incident_id,
                error = %e,
                "failed to read sleep depth, falling back to full wake"
            );
            SleepDepth::Unknown
        }
    };

    match choose_action(depth, state) {
        WakeAction::None => {
            tracing::info!(incident_id, "user is awake/out of bed, skipping bed wake");
        }
        WakeAction::Gentle { vibration_power } => {
            if let Err(e) = state.eight_sleep.trigger_vibration(vibration_power, &state.config.timezone).await {
                tracing::error!(incident_id, error = %e, "gentle vibration failed");
            }

            let state = Arc::clone(state);
            let incident_id = incident_id.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(
                    state.config.escalation_delay_secs,
                ))
                .await;

                match state.eight_sleep.current_sleep_depth().await {
                    Ok(d) if d == SleepDepth::Awake || d == SleepDepth::OutOfBed => {
                        tracing::info!(incident_id, "user woke up, skipping escalation");
                    }
                    _ => {
                        tracing::info!(incident_id, "escalating to full wake");
                        let _ = state
                            .eight_sleep
                            .trigger_vibration(state.config.vibration_power, &state.config.timezone)
                            .await;
                        let _ = state
                            .eight_sleep
                            .set_temperature(state.config.thermal_wake_level)
                            .await;
                    }
                }
            });
        }
        WakeAction::Full {
            vibration_power,
            temp,
        } => {
            if let Err(e) = state.eight_sleep.trigger_vibration(vibration_power, &state.config.timezone).await {
                tracing::error!(incident_id, error = %e, "full vibration failed");
            }
            if let Err(e) = state.eight_sleep.set_temperature(temp).await {
                tracing::error!(incident_id, error = %e, "thermal wake failed");
            }
        }
    }
}
