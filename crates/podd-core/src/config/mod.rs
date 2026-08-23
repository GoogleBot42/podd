use jiff::{civil::Time, tz::TimeZone};
use ron::extensions::Extensions;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use tokio::fs;

use crate::led::{CurrentBand, LedPattern};
use pod_proto::packet::BedSide;
use pod_proto::sensor::command::AlarmPattern;

pub mod device;
pub mod mqtt;
#[cfg(test)]
mod tests;

pub use device::{Cover, DeviceConfig};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse RON: {0}")]
    Ron(#[from] ron::error::SpannedError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LEDConfig {
    /// No side enabled (outside the schedule window, away mode, etc.).
    pub idle: LedPattern,
    /// A side is enabled and AT its target temperature ("holding"). Kept under
    /// its legacy name so existing configs parse unchanged.
    pub active: LedPattern,
    /// Actively raising the water temperature toward a target.
    /// Defaults to a red-orange slow breath.
    #[serde(default)]
    pub heating: Option<LedPattern>,
    /// Actively lowering the water temperature toward a target.
    /// Defaults to a blue slow breath.
    #[serde(default)]
    pub cooling: Option<LedPattern>,
    pub band: CurrentBand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MqttConfig {
    pub server: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlarmConfig {
    pub pattern: AlarmPattern,
    pub intensity: u8,
    /// duration in seconds (TODO plz verify)
    pub duration: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceConfig {
    pub baselines: [u16; 6],
    pub threshold: u16,
    pub debounce_count: u8,
}

fn default_true() -> bool {
    true
}

fn time_de<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Time, D::Error> {
    let s = String::deserialize(deserializer)?;
    Time::strptime("%H:%M", &s).map_err(serde::de::Error::custom)
}

fn time_ser<S: Serializer>(time: &Time, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&time.strftime("%H:%M").to_string())
}

fn timezone_de<'de, D: Deserializer<'de>>(deserializer: D) -> Result<TimeZone, D::Error> {
    let tzname = String::deserialize(deserializer)?;
    TimeZone::get(&tzname).map_err(serde::de::Error::custom)
}

fn timezone_ser<S: Serializer>(tz: &TimeZone, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(tz.iana_name().unwrap())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SideConfig {
    /// degrees celcius
    pub temperatures: Vec<f32>,
    #[serde(deserialize_with = "time_de", serialize_with = "time_ser")]
    pub sleep: Time,
    #[serde(deserialize_with = "time_de", serialize_with = "time_ser")]
    pub wake: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alarm: Option<AlarmConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SidesConfig {
    Solo(SideConfig),
    Couples { left: SideConfig, right: SideConfig },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(deserialize_with = "timezone_de", serialize_with = "timezone_ser")]
    pub timezone: TimeZone,
    pub away_mode: bool,
    #[serde(deserialize_with = "time_de", serialize_with = "time_ser")]
    pub prime: Time,
    /// Whether the *daily* prime at [`Config::prime`] runs at all (the UI's
    /// "Prime daily?" toggle). Defaults to true so configs written before this
    /// field existed keep priming exactly as they did. An explicit prime
    /// request (UI "Prime Now", MQTT) is unaffected.
    #[serde(default = "default_true")]
    pub prime_enabled: bool,
    pub led: LEDConfig,
    pub mqtt: MqttConfig,
    pub profile: SidesConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<PresenceConfig>,
    /// Hardware wiring (UART paths/bauds, I2C bus, expander/LED addrs). Absent
    /// section => historical hard-coded defaults (see [`DeviceConfig`]).
    #[serde(default)]
    pub device: DeviceConfig,
}

impl Config {
    pub async fn load(path: &str) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(path).await?;
        let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
        let config = opts.from_str(&content)?;
        Ok(config)
    }

    pub async fn save(&self, path: &str) -> Result<(), ConfigError> {
        let content = ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| ConfigError::Io(std::io::Error::other(e)))?;
        fs::write(path, content).await?;
        Ok(())
    }
}

impl SidesConfig {
    pub fn get_side(&self, side: &BedSide) -> &SideConfig {
        match self {
            SidesConfig::Solo(cfg) => cfg,
            SidesConfig::Couples { left, right } => match side {
                BedSide::Left => left,
                BedSide::Right => right,
            },
        }
    }

    pub fn is_solo(&self) -> bool {
        matches!(self, SidesConfig::Solo(_))
    }

    pub fn is_couples(&self) -> bool {
        matches!(self, SidesConfig::Couples { .. })
    }

    pub fn unwrap_solo_mut(&mut self) -> &mut SideConfig {
        match self {
            SidesConfig::Solo(c) => c,
            SidesConfig::Couples { left: _, right: _ } => panic!(),
        }
    }

    pub fn unwrap_left_mut(&mut self) -> &mut SideConfig {
        match self {
            SidesConfig::Solo(_) => panic!(),
            SidesConfig::Couples { left, right: _ } => left,
        }
    }

    pub fn unwrap_right_mut(&mut self) -> &mut SideConfig {
        match self {
            SidesConfig::Solo(_) => panic!(),
            SidesConfig::Couples { left: _, right } => right,
        }
    }
}
