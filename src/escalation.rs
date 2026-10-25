use std::sync::Arc;
use std::time::Duration;

use crate::AppState;
use crate::eight_sleep::SleepDepth;

const ALARM_CLEANUP_DELAY: Duration = Duration::from_secs(600);

enum WakeAction {
    None,
    Gentle { vibration_power: u8 },
    Full,
}

fn choose_action(depth: SleepDepth, state: &AppState) -> WakeAction {
    match depth {
        SleepDepth::Awake | SleepDepth::OutOfBed => WakeAction::None,
        SleepDepth::Light | SleepDepth::Rem => WakeAction::Gentle {
            vibration_power: state.config.gentle_vibration_power,
        },
        SleepDepth::Deep | SleepDepth::Unknown => WakeAction::Full,
    }
}

fn schedule_alarm_cleanup(state: &Arc<AppState>, alarm_id: String) {
    let state = Arc::clone(state);
    tokio::spawn(async move {
        tokio::time::sleep(ALARM_CLEANUP_DELAY).await;
        if let Err(e) = state.eight_sleep.delete_alarm(&alarm_id).await {
            state.monitoring.wake_failed();
            tracing::warn!(alarm_id, error = %e, "failed to clean up alarm");
        }
    });
}

pub async fn wake(state: &Arc<AppState>, incident_id: &str) -> anyhow::Result<()> {
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
                Err(e) => {
                    state.monitoring.wake_failed();
                    return Err(e.context("gentle vibration failed"));
                }
            }

            let state = Arc::clone(state);
            let incident_id = incident_id.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(state.config.escalation_delay_secs)).await;

                match state.eight_sleep.current_sleep_depth().await {
                    Ok(d) if d == SleepDepth::Awake || d == SleepDepth::OutOfBed => {
                        tracing::info!(incident_id, "user woke up, skipping escalation");
                    }
                    _ => {
                        tracing::info!(incident_id, "escalating to full wake");
                        if let Err(error) = full_wake(&state).await {
                            tracing::error!(incident_id, error = %error, "escalated bed wake failed");
                        }
                    }
                }
            });
        }
        WakeAction::Full => {
            full_wake(state).await?;
        }
    }
    Ok(())
}

async fn full_wake(state: &Arc<AppState>) -> anyhow::Result<()> {
    let vibration = state
        .eight_sleep
        .trigger_vibration(state.config.vibration_power, &state.config.timezone)
        .await;
    let temperature = state
        .eight_sleep
        .set_temperature(state.config.thermal_wake_level)
        .await;
    let mut failure = None;
    match vibration {
        Ok(alarm_id) => schedule_alarm_cleanup(state, alarm_id),
        Err(error) => {
            tracing::error!(error = %error, "full vibration failed");
            failure = Some(error);
        }
    }
    if let Err(error) = temperature {
        tracing::error!(error = %error, "thermal wake failed");
        failure = Some(error);
    }
    if let Some(error) = failure {
        state.monitoring.wake_failed();
        return Err(error);
    }
    Ok(())
}
