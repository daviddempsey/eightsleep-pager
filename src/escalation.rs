use std::sync::Arc;
use std::time::Duration;

use crate::eight_sleep::SleepDepth;
use crate::AppState;

const ALARM_CLEANUP_DELAY: Duration = Duration::from_secs(600);

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

fn schedule_alarm_cleanup(state: &Arc<AppState>, alarm_id: String) {
    let state = Arc::clone(state);
    tokio::spawn(async move {
        tokio::time::sleep(ALARM_CLEANUP_DELAY).await;
        if let Err(e) = state.eight_sleep.delete_alarm(&alarm_id).await {
            tracing::warn!(alarm_id, error = %e, "failed to clean up alarm");
        }
    });
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
            match state
                .eight_sleep
                .trigger_vibration(vibration_power, &state.config.timezone)
                .await
            {
                Ok(alarm_id) => schedule_alarm_cleanup(state, alarm_id),
                Err(e) => tracing::error!(incident_id, error = %e, "gentle vibration failed"),
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
                        if let Ok(alarm_id) = state
                            .eight_sleep
                            .trigger_vibration(
                                state.config.vibration_power,
                                &state.config.timezone,
                            )
                            .await
                        {
                            schedule_alarm_cleanup(&state, alarm_id);
                        }
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
            match state
                .eight_sleep
                .trigger_vibration(vibration_power, &state.config.timezone)
                .await
            {
                Ok(alarm_id) => schedule_alarm_cleanup(state, alarm_id),
                Err(e) => tracing::error!(incident_id, error = %e, "full vibration failed"),
            }
            if let Err(e) = state.eight_sleep.set_temperature(temp).await {
                tracing::error!(incident_id, error = %e, "thermal wake failed");
            }
        }
    }
}
