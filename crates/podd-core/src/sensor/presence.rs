use crate::config::{Config, PresenceConfig};
use crate::mqtt::publish_state_retained;
use pod_proto::sensor::packet::CapacitanceData;
use rumqttc::AsyncClient;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

const DEFAULT_THRESHOLD: u16 = 50;
const DEFAULT_DEBOUNCE: u8 = 5;
const CALIBRATION_DURATION: Duration = Duration::from_secs(10);

pub(crate) const TOPIC_ANY: &str = "opensleep/state/presence/any";
pub(crate) const TOPIC_LEFT: &str = "opensleep/state/presence/left";
pub(crate) const TOPIC_RIGHT: &str = "opensleep/state/presence/right";
pub const TOPIC_CALIBRATE: &str = "opensleep/actions/calibrate";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PresenceState {
    pub any: bool,
    pub left: bool,
    pub right: bool,
}

pub struct PresenseManager {
    config_tx: watch::Sender<Config>,
    config_rx: watch::Receiver<Config>,
    config_path: Arc<str>,
    config: Option<PresenceConfig>,
    client: AsyncClient,
    calibration_end: Option<Instant>,
    calibration_samples: Vec<[u16; 6]>,
    debounce: [u8; 6],
    last_state: Option<PresenceState>,
}

impl PresenseManager {
    pub fn new(
        config_tx: watch::Sender<Config>,
        config_rx: watch::Receiver<Config>,
        config_path: Arc<str>,
        client: AsyncClient,
    ) -> Self {
        PresenseManager {
            config: {
                let b = config_rx.borrow();
                if b.presence.is_none() {
                    log::warn!(
                        "No presence config found. Please calibrate using 'opensleep/actions/calibrate' endpoint."
                    );
                }
                b.presence.as_ref().cloned()
            },
            config_tx,
            config_rx,
            config_path,
            client,
            calibration_end: None,
            calibration_samples: Vec::new(),
            debounce: [0u8; 6],
            last_state: None,
        }
    }

    /// The latest debounced presence state (all-false until the first update).
    pub fn presence_state(&self) -> PresenceState {
        self.last_state.clone().unwrap_or_default()
    }

    pub async fn update(&mut self, data: &CapacitanceData) {
        if self.config.is_some() {
            self.update_presence(data);
        }

        if self.calibration_end.is_some() {
            self.update_calibration(data).await;
        }
    }

    fn update_presence(&mut self, data: &CapacitanceData) {
        let config = self.config.as_mut().unwrap();

        for i in 0..6 {
            if exceeds_baseline(data.values[i], config.baselines[i], config.threshold) {
                self.debounce[i] = self.debounce[i].saturating_add(1);
            } else {
                self.debounce[i] = 0;
            }
        }

        let left_present = self.debounce[0..3]
            .iter()
            .any(|&c| c >= config.debounce_count);
        let right_present = self.debounce[3..6]
            .iter()
            .any(|&c| c >= config.debounce_count);

        let state = PresenceState {
            any: left_present || right_present,
            left: left_present,
            right: right_present,
        };

        if self.last_state.as_ref() != Some(&state) {
            self.update_mqtt(&state);
            self.last_state = Some(state);
        }
    }

    fn update_mqtt(&mut self, state: &PresenceState) {
        publish_state_retained(&mut self.client, TOPIC_ANY, state.any.to_string());
        publish_state_retained(&mut self.client, TOPIC_LEFT, state.left.to_string());
        publish_state_retained(&mut self.client, TOPIC_RIGHT, state.right.to_string());
    }

    pub fn start_calibration(&mut self) {
        log::info!("Running calibration for {}", CALIBRATION_DURATION.as_secs());
        self.calibration_end = Some(Instant::now() + CALIBRATION_DURATION);
        self.calibration_samples = vec![];
    }

    async fn update_calibration(&mut self, data: &CapacitanceData) {
        self.calibration_samples.push(data.values);

        if Instant::now() > self.calibration_end.unwrap() {
            self.calibration_end = None;

            if self.calibration_samples.is_empty() {
                log::error!("Calibration failed, no samples collected.");
                return;
            }

            log::info!("Calibration finished. Updating config..");

            let baselines = Self::calculate_baselines(&self.calibration_samples);
            let new_cfg = PresenceConfig {
                baselines,
                threshold: DEFAULT_THRESHOLD,
                debounce_count: DEFAULT_DEBOUNCE,
            };

            // reset
            self.calibration_samples = vec![];
            self.calibration_end = None;

            self.config = Some(new_cfg.clone());

            let mut config = self.config_rx.borrow_and_update().clone();
            config.presence = Some(new_cfg.clone());
            if let Err(e) = self.config_tx.send(config.clone()) {
                log::error!("Failed to update config: {e}");
            } else {
                log::info!("Config updated: {baselines:?}");
            }
            // Persist like the MQTT set-action path does — a calibration that
            // only lives in the watch value is lost on the next restart (#27).
            match config.save(&self.config_path).await {
                Ok(()) => log::info!("Calibration saved to {}", self.config_path),
                Err(e) => log::error!("Failed to save calibration to config file: {e}"),
            }
        }
    }

    fn calculate_baselines(samples: &[[u16; 6]]) -> [u16; 6] {
        let mut sums = [0u32; 6];
        for sample in samples {
            for (sum, &value) in sums.iter_mut().zip(sample) {
                *sum += value as u32;
            }
        }
        let count = samples.len() as u32;
        sums.map(|sum| (sum / count) as u16)
    }
}

/// Baseline and threshold are both u16 and user-settable, so their sum can
/// exceed u16::MAX — compare in u32.
fn exceeds_baseline(value: u16, baseline: u16, threshold: u16) -> bool {
    value as u32 > baseline as u32 + threshold as u32
}

#[cfg(test)]
mod tests {
    use super::exceeds_baseline;

    #[test]
    fn detects_value_above_baseline_plus_threshold() {
        assert!(exceeds_baseline(1500, 1014, 300));
        assert!(!exceeds_baseline(1300, 1014, 300));
    }

    #[test]
    fn baseline_plus_threshold_does_not_wrap() {
        // 65000 + 60000 wraps to 59464 in u16; a quiet reading must not
        // register as present, and the sum must not panic in debug builds.
        assert!(!exceeds_baseline(60000, 65000, 60000));
        assert!(exceeds_baseline(u16::MAX, u16::MAX - 1, 0));
    }

}
