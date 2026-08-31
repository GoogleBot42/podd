use super::*;
use crate::config::device::{self, DeviceConfig};

// TODO more testing (esp for MQTT)

/// Parse a `DeviceConfig` from a RON snippet using the same options as
/// [`Config::load`] (so `IMPLICIT_SOME` applies to `Option` fields).
fn parse_device(src: &str) -> DeviceConfig {
    let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
    opts.from_str(src).unwrap()
}

#[tokio::test]
async fn test_load_solo_config() {
    let config = Config::load("example_solo.ron").await.unwrap();
    assert_eq!(config.timezone.iana_name().unwrap(), "America/New_York");
    // `away_mode: false` in the file is the legacy whole-bed bool form
    assert_eq!(config.away_mode, AwayMode::default());
    match &config.profile {
        SidesConfig::Solo(profile) => {
            assert_eq!(profile.temperatures, vec![27., 29., 31.]);
        }
        _ => panic!("Expected solo profile"),
    }
    // no `device` section => historical hard-coded defaults (Pod 3 fw baud)
    assert_eq!(config.device, DeviceConfig::default());
    assert_eq!(config.device.frozen_port, device::FROZEN_PORT);
    assert_eq!(config.device.frozen_baud, device::FROZEN_BAUD);
    assert_eq!(config.device.sensor_firmware_baud, device::SENSOR_FIRMWARE_BAUD_POD3);
}

#[test]
fn test_device_defaults_when_empty() {
    // an empty `device: ()` section still fills in every default
    let dev = parse_device("()");
    assert_eq!(dev, DeviceConfig::default());
    assert_eq!(dev.cover, None);
    assert_eq!(dev.sensor_firmware_baud, device::SENSOR_FIRMWARE_BAUD_POD3);
    assert_eq!(dev.pcal6416a_addr, 0x20);
    assert_eq!(dev.led_addr, 0x53);
}

#[test]
fn test_device_cover_pod4_selects_baud() {
    let dev = parse_device("(cover: pod4)");
    assert_eq!(dev.cover, Some(device::Cover::Pod4));
    // cover picks the 921600 firmware baud default
    assert_eq!(dev.sensor_firmware_baud, device::SENSOR_FIRMWARE_BAUD_POD4);
    // everything else still defaulted
    assert_eq!(dev.frozen_port, device::FROZEN_PORT);
}

#[test]
fn test_device_cover_pod3_selects_baud() {
    let dev = parse_device("(cover: pod3)");
    assert_eq!(dev.cover, Some(device::Cover::Pod3));
    assert_eq!(dev.sensor_firmware_baud, device::SENSOR_FIRMWARE_BAUD_POD3);
}

#[test]
fn test_device_explicit_override_beats_cover() {
    // explicit sensor_firmware_baud wins even with cover: pod4
    let dev = parse_device("(cover: pod4, sensor_firmware_baud: 230400)");
    assert_eq!(dev.sensor_firmware_baud, 230400);
}

#[tokio::test]
async fn test_load_pod4_example_config() {
    // repo-root example; tests run with cwd = crates/podd-core
    let config = Config::load("../../config.pod4.example.ron").await.unwrap();
    assert_eq!(config.device.cover, Some(device::Cover::Pod4));
    assert_eq!(
        config.device.sensor_firmware_baud,
        device::SENSOR_FIRMWARE_BAUD_POD4
    );
    assert_eq!(config.device.frozen_port, "/dev/ttymxc2");
    assert_eq!(config.device.sensor_port, "/dev/ttymxc0");
}

#[tokio::test]
async fn test_load_pod3_example_config() {
    let config = Config::load("../../config.pod3.example.ron").await.unwrap();
    assert_eq!(config.device.cover, Some(device::Cover::Pod3));
    assert_eq!(
        config.device.sensor_firmware_baud,
        device::SENSOR_FIRMWARE_BAUD_POD3
    );
}

#[tokio::test]
async fn test_prime_enabled_defaults_true_when_absent() {
    // configs written before `prime_enabled` existed must keep priming — a
    // default-off would silently stop the daily prime on live units
    let config = Config::load("example_solo.ron").await.unwrap();
    assert!(config.prime_enabled);
    // the examples spell it out explicitly
    let config = Config::load("../../config.pod4.example.ron").await.unwrap();
    assert!(config.prime_enabled);
}

#[tokio::test]
async fn test_mqtt_enabled_defaults_true_when_absent() {
    // Configs written before `mqtt.enabled` existed (every live unit) must
    // keep their broker link — a default-off would silently drop Home
    // Assistant on upgrade.
    let config = Config::load("example_solo.ron").await.unwrap();
    assert!(config.mqtt.enabled);
    // and the field round-trips through what `Config::save` writes
    let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
    let mut mqtt = config.mqtt.clone();
    mqtt.enabled = false;
    let ron_str = ron::ser::to_string(&mqtt).unwrap();
    let parsed: MqttConfig = opts.from_str(&ron_str).unwrap();
    assert_eq!(parsed, mqtt);
}

fn parse_away(src: &str) -> AwayMode {
    let opts = ron::Options::default().with_default_extension(Extensions::IMPLICIT_SOME);
    opts.from_str(src).unwrap()
}

#[test]
fn test_away_mode_legacy_bool_forms() {
    // pre-per-side configs said `away_mode: true/false`; a bool sets both sides
    assert_eq!(parse_away("false"), AwayMode { left: false, right: false });
    assert_eq!(parse_away("true"), AwayMode { left: true, right: true });
}

#[test]
fn test_away_mode_per_side_form() {
    assert_eq!(
        parse_away("(left: true, right: false)"),
        AwayMode { left: true, right: false }
    );
    // partial struct: unnamed side defaults to home
    assert_eq!(parse_away("(right: true)"), AwayMode { left: false, right: true });
}

#[test]
fn test_away_mode_round_trips_through_save_format() {
    // what Config::save writes must load back identically
    let away = AwayMode { left: true, right: false };
    let ron_str = ron::ser::to_string(&away).unwrap();
    assert_eq!(parse_away(&ron_str), away);
}

#[test]
fn test_device_partial_override() {
    let dev = parse_device(r#"(frozen_port: "/dev/ttyUSB9", led_addr: 0x30)"#);
    assert_eq!(dev.frozen_port, "/dev/ttyUSB9");
    assert_eq!(dev.led_addr, 0x30);
    // untouched fields keep defaults
    assert_eq!(dev.sensor_port, device::SENSOR_PORT);
    assert_eq!(dev.frozen_baud, device::FROZEN_BAUD);
}

#[tokio::test]
async fn test_config_state_topics_and_payloads() {
    use crate::config::mqtt::ConfigStateTopic as T;

    // Home Assistant subscribes to these exact retained topics — renaming one
    // silently orphans an entity, so pin the strings.
    assert_eq!(T::Prime.topic(), "opensleep/state/config/prime");
    assert_eq!(T::AwayMode.topic(), "opensleep/state/config/away_mode");
    assert_eq!(T::Timezone.topic(), "opensleep/state/config/timezone");

    // Payload rendering is shared by the MQTT action path and the API command
    // path (#106) so the two can't drift.
    let mut config = Config::load("example_solo.ron").await.unwrap();
    assert_eq!(T::Timezone.payload(&config), "America/New_York");
    assert_eq!(T::Prime.payload(&config), config.prime.to_string());
    assert_eq!(T::AwayMode.payload(&config), "false");

    config.away_mode = AwayMode { left: true, right: false };
    assert_eq!(T::AwayMode.payload(&config), "false", "half away is not away");
    config.away_mode = AwayMode { left: true, right: true };
    assert_eq!(T::AwayMode.payload(&config), "true");
}

#[tokio::test]
async fn test_load_couples_config() {
    let config = Config::load("example_couples.ron").await.unwrap();
    assert_eq!(config.timezone.iana_name().unwrap(), "America/New_York");
    assert_eq!(config.away_mode, AwayMode::default());
    match &config.profile {
        SidesConfig::Couples { left, right } => {
            assert_eq!(left.temperatures, vec![27., 29., 31.]);
            assert_eq!(right.temperatures, vec![27., 29., 31.]);
        }
        _ => panic!("Expected couples profile"),
    }
}

#[tokio::test]
async fn test_led_brightness_defaults_when_absent() {
    // configs written before `led.brightness` existed must parse at full
    // brightness — a default-0 would blank the status LED on live units
    let config = Config::load("example_solo.ron").await.unwrap();
    assert_eq!(config.led.brightness, 100);
    // the examples spell it out explicitly
    let config = Config::load("../../config.example.ron").await.unwrap();
    assert_eq!(config.led.brightness, 100);
}
