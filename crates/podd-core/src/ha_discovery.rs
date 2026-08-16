//! Home Assistant MQTT discovery.
//!
//! On every broker (re)connect we publish one retained
//! `homeassistant/<component>/<node>/<object>/config` message per entity, so
//! the Pod shows up in HA as a single device with temperature / humidity /
//! presence entities — no manual YAML. Payload format per
//! <https://www.home-assistant.io/integrations/mqtt/#mqtt-discovery>.
//! Harmless without HA: the retained configs just sit on the broker.

use crate::mqtt::{TOPIC_AVAILABILITY, publish_guaranteed_wait};
use crate::{NAME, VERSION, frozen, sensor};
use rumqttc::AsyncClient;
use serde_json::{Value, json};

/// One discovery message: (discovery config topic, retained JSON payload).
type DiscoveryMsg = (String, Value);

/// °C measurement sensor entity.
fn temp_sensor(node: &str, device: &Value, object: &str, name: &str, state_topic: &str) -> DiscoveryMsg {
    (
        format!("homeassistant/sensor/{node}/{object}/config"),
        json!({
            "name": name,
            "unique_id": format!("{node}_{object}"),
            "state_topic": state_topic,
            "unit_of_measurement": "°C",
            "device_class": "temperature",
            "state_class": "measurement",
            "availability_topic": TOPIC_AVAILABILITY,
            "device": device,
        }),
    )
}

/// Occupancy binary sensor ("true"/"false" payloads, matching what the
/// presence task publishes).
fn presence_sensor(node: &str, device: &Value, object: &str, name: &str, state_topic: &str) -> DiscoveryMsg {
    (
        format!("homeassistant/binary_sensor/{node}/{object}/config"),
        json!({
            "name": name,
            "unique_id": format!("{node}_{object}"),
            "state_topic": state_topic,
            "device_class": "occupancy",
            "payload_on": "true",
            "payload_off": "false",
            "availability_topic": TOPIC_AVAILABILITY,
            "device": device,
        }),
    )
}

/// All discovery messages for this device.
fn discovery_messages(device_label: &str) -> Vec<DiscoveryMsg> {
    let node = node_id(device_label);
    let device = json!({
        "identifiers": [node],
        "name": "Eight Sleep Pod",
        "manufacturer": NAME,
        "model": "Eight Sleep Pod",
        "sw_version": VERSION,
    });

    let mut msgs = vec![
        temp_sensor(&node, &device, "left_temp", "Left bed temperature", frozen::state::TOPIC_LEFT_TEMP),
        temp_sensor(&node, &device, "right_temp", "Right bed temperature", frozen::state::TOPIC_RIGHT_TEMP),
        temp_sensor(&node, &device, "left_target_temp", "Left target temperature", frozen::state::TOPIC_LEFT_TARGET_TEMP),
        temp_sensor(&node, &device, "right_target_temp", "Right target temperature", frozen::state::TOPIC_RIGHT_TARGET_TEMP),
        temp_sensor(&node, &device, "heatsink_temp", "Heatsink temperature", frozen::state::TOPIC_HEATSINK_TEMP),
        temp_sensor(&node, &device, "bed_temp", "Bed temperature", sensor::state::TOPIC_BED_TEMP),
        temp_sensor(&node, &device, "ambient_temp", "Ambient temperature", sensor::state::TOPIC_AMBIENT_TEMP),
        temp_sensor(&node, &device, "mcu_temp", "Sensor MCU temperature", sensor::state::TOPIC_MCU_TEMP),
        presence_sensor(&node, &device, "presence_left", "Left presence", sensor::presence::TOPIC_LEFT),
        presence_sensor(&node, &device, "presence_right", "Right presence", sensor::presence::TOPIC_RIGHT),
        presence_sensor(&node, &device, "presence_any", "Bed presence", sensor::presence::TOPIC_ANY),
    ];

    // Humidity has its own class/unit; diagnostic-ish but useful.
    msgs.push((
        format!("homeassistant/sensor/{node}/humidity/config"),
        json!({
            "name": "Humidity",
            "unique_id": format!("{node}_humidity"),
            "state_topic": sensor::state::TOPIC_HUMIDITY,
            "unit_of_measurement": "%",
            "device_class": "humidity",
            "state_class": "measurement",
            "availability_topic": TOPIC_AVAILABILITY,
            "device": device,
        }),
    ));

    msgs
}

/// Discovery node id: "opensleep" plus the device label reduced to
/// [a-z0-9_] (HA rejects other characters in discovery topics).
fn node_id(device_label: &str) -> String {
    let cleaned: String = device_label
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    match cleaned.trim_matches('_') {
        "" => "opensleep".to_string(),
        s => format!("opensleep_{s}"),
    }
}

/// Publish every entity's retained discovery config. Called from the
/// post-ConnAck task, so the event loop is being polled.
pub async fn publish_discovery(client: &mut AsyncClient, device_label: &str) {
    for (topic, payload) in discovery_messages(device_label) {
        publish_guaranteed_wait(client, topic, true, payload.to_string()).await;
    }
    log::info!("published Home Assistant MQTT discovery configs");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_sanitizes() {
        assert_eq!(node_id("Jeremy's Pod 3"), "opensleep_jeremy_s_pod_3");
        assert_eq!(node_id(""), "opensleep");
        assert_eq!(node_id("---"), "opensleep");
    }

    #[test]
    fn discovery_messages_are_well_formed() {
        let msgs = discovery_messages("test-pod");
        assert_eq!(msgs.len(), 12);
        for (topic, payload) in &msgs {
            assert!(topic.starts_with("homeassistant/"), "{topic}");
            assert!(topic.ends_with("/config"), "{topic}");
            // discovery topics allow only [a-zA-Z0-9_-] per level
            assert!(
                topic.chars().all(|c| c.is_ascii_alphanumeric() || "/_-".contains(c)),
                "{topic}"
            );
            let obj = payload.as_object().unwrap();
            for key in ["name", "unique_id", "state_topic", "availability_topic", "device"] {
                assert!(obj.contains_key(key), "{topic} missing {key}");
            }
            assert_eq!(obj["availability_topic"], "opensleep/availability");
            assert!(obj["state_topic"].as_str().unwrap().starts_with("opensleep/"));
        }
        // unique_ids must be unique
        let mut ids: Vec<_> = msgs.iter().map(|(_, p)| p["unique_id"].as_str().unwrap().to_string()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12);
    }
}
