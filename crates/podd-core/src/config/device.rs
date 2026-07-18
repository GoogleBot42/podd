//! Device/hardware wiring config: UART paths + bauds for the two STM32 MCUs,
//! plus the I2C bus and expander/LED addresses.
//!
//! These used to be hard-coded `const`s scattered across `frozen::manager`,
//! `sensor::manager`, `reset`, and `led::controller`. They now live here so a
//! single config can describe both Pod 3 and Pod 4 wiring. The consts below are
//! kept as the defaults, so a config with no `device` section (or with only
//! some fields set) still loads with the historical values.

use serde::{Deserialize, Deserializer, Serialize};

// ---- defaults (the previously hard-coded consts) ----

/// Frozen (TEC/pump) MCU UART. Confirmed on Pod 4 hardware.
pub const FROZEN_PORT: &str = "/dev/ttymxc2";
/// Frozen MCU baud. Confirmed on Pod 4 (7/7 frames valid).
pub const FROZEN_BAUD: u32 = 38400;

/// Sensor (presence/temp/piezo) MCU UART. Confirmed on Pod 4 hardware.
pub const SENSOR_PORT: &str = "/dev/ttymxc0";
/// Sensor MCU bootloader baud.
///
/// Pod 3 uses 38400. The Pod 4 bootloader baud is **unknown** (TODO: confirm
/// against live hardware) — override `sensor_bootloader_baud` if it differs.
pub const SENSOR_BOOTLOADER_BAUD: u32 = 38400;
/// Sensor MCU firmware baud for a Pod 3 cover (opensleep's hard-coded value).
pub const SENSOR_FIRMWARE_BAUD_POD3: u32 = 115200;
/// Sensor MCU firmware baud for a Pod 4 cover. Confirmed on Pod 4 hardware
/// (8x the Pod 3 firmware baud). opensleep's 115200 is WRONG for Pod 4.
pub const SENSOR_FIRMWARE_BAUD_POD4: u32 = 921600;

/// I2C bus hosting the reset expander + LED controller.
pub const I2C_BUS: &str = "/dev/i2c-1";
/// PCAL6416A 16-bit I2C expander (MCU reset lines) address.
pub const PCAL6416A_ADDR: u8 = 0x20;
/// IS31FL3194 LED controller address.
pub const LED_ADDR: u8 = 0x53;

/// Which Pod cover is attached. Selects baud defaults; explicit per-field
/// overrides always win. Written in RON as `cover: pod3` / `cover: pod4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cover {
    Pod3,
    Pod4,
}

impl Cover {
    /// The sensor firmware baud implied by this cover.
    pub fn sensor_firmware_baud(self) -> u32 {
        match self {
            Cover::Pod3 => SENSOR_FIRMWARE_BAUD_POD3,
            Cover::Pod4 => SENSOR_FIRMWARE_BAUD_POD4,
        }
    }
}

/// Hardware wiring for a given Pod: device paths, bauds, and I2C addresses.
///
/// Deserialization is lenient (see the manual `Deserialize` impl): every field
/// is optional and falls back to the module defaults, and `cover` selects the
/// `sensor_firmware_baud` default when that field is not given explicitly.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<Cover>,
    pub frozen_port: String,
    pub frozen_baud: u32,
    pub sensor_port: String,
    pub sensor_bootloader_baud: u32,
    pub sensor_firmware_baud: u32,
    pub i2c_bus: String,
    pub pcal6416a_addr: u8,
    pub led_addr: u8,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            cover: None,
            frozen_port: FROZEN_PORT.to_string(),
            frozen_baud: FROZEN_BAUD,
            sensor_port: SENSOR_PORT.to_string(),
            sensor_bootloader_baud: SENSOR_BOOTLOADER_BAUD,
            // Backward compat: no `device` section => the historical hard-coded
            // firmware baud (Pod 3). Pod 4 configs set `cover: pod4` or override.
            sensor_firmware_baud: SENSOR_FIRMWARE_BAUD_POD3,
            i2c_bus: I2C_BUS.to_string(),
            pcal6416a_addr: PCAL6416A_ADDR,
            led_addr: LED_ADDR,
        }
    }
}

impl<'de> Deserialize<'de> for DeviceConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // All fields optional so partial `device` sections work and missing
        // ones fall back to defaults (serde treats absent Option fields as None).
        #[derive(Deserialize)]
        struct Raw {
            cover: Option<Cover>,
            frozen_port: Option<String>,
            frozen_baud: Option<u32>,
            sensor_port: Option<String>,
            sensor_bootloader_baud: Option<u32>,
            sensor_firmware_baud: Option<u32>,
            i2c_bus: Option<String>,
            pcal6416a_addr: Option<u8>,
            led_addr: Option<u8>,
        }

        let raw = Raw::deserialize(deserializer)?;

        // cover picks the firmware-baud default; an explicit sensor_firmware_baud
        // still overrides it. No cover => historical Pod 3 default.
        let firmware_baud_default = raw
            .cover
            .map(Cover::sensor_firmware_baud)
            .unwrap_or(SENSOR_FIRMWARE_BAUD_POD3);

        Ok(DeviceConfig {
            cover: raw.cover,
            frozen_port: raw.frozen_port.unwrap_or_else(|| FROZEN_PORT.to_string()),
            frozen_baud: raw.frozen_baud.unwrap_or(FROZEN_BAUD),
            sensor_port: raw.sensor_port.unwrap_or_else(|| SENSOR_PORT.to_string()),
            sensor_bootloader_baud: raw.sensor_bootloader_baud.unwrap_or(SENSOR_BOOTLOADER_BAUD),
            sensor_firmware_baud: raw.sensor_firmware_baud.unwrap_or(firmware_baud_default),
            i2c_bus: raw.i2c_bus.unwrap_or_else(|| I2C_BUS.to_string()),
            pcal6416a_addr: raw.pcal6416a_addr.unwrap_or(PCAL6416A_ADDR),
            led_addr: raw.led_addr.unwrap_or(LED_ADDR),
        })
    }
}
