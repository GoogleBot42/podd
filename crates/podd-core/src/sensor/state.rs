use rumqttc::AsyncClient;

use crate::mqtt::{publish_guaranteed_wait, publish_high_freq};
use pod_proto::packet::{BedSide, HardwareInfo};
use pod_proto::sensor::packet::SensorPacket;
use pod_proto::serial::DeviceMode;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SensorState {
    pub device_mode: DeviceMode,
    pub hardware_info: Option<HardwareInfo>,
    pub vibration_enabled: bool,
    pub piezo_gain: Option<(u16, u16)>,
    pub piezo_freq: Option<u32>,
    pub piezo_enabled: bool,
    pub alarm_left_running: bool,
    pub alarm_right_running: bool,
    // Host-side alarm policy, not MCU state. Kept here so the scheduler's
    // plain-fn `can_run` checks (which only see &SensorState) can read them.
    /// User dismissed this side's alarm (double tap / API); holds until the
    /// current alarm window ends so the scheduler doesn't immediately re-arm.
    pub alarm_left_dismissed: bool,
    pub alarm_right_dismissed: bool,
    /// Wall time is NTP-synced. Until then it may be a restored pre-shutdown
    /// timestamp (no RTC battery), so scheduled alarms must not arm.
    pub clock_synced: bool,
    /// Until when a manually fired alarm (API test alarm) may run per side
    /// (left, right). Stops the scheduler's out-of-window cancel from killing
    /// it seconds after it starts.
    pub manual_alarm_until: [Option<std::time::Instant>; 2],
}

pub const PIEZO_GAIN: u16 = 400;
const PIEZO_TOLERANCE: i16 = 6;
pub const PIEZO_FREQ: u32 = 1000;

const TOPIC_MODE: &str = "opensleep/state/sensor/mode";
const TOPIC_HWINFO: &str = "opensleep/state/sensor/hwinfo";
const TOPIC_PIEZO_OK: &str = "opensleep/state/sensor/piezo_ok";
const TOPIC_VIBRATION_ENABLED: &str = "opensleep/state/sensor/vibration_enabled";
const TOPIC_BED_TEMP: &str = "opensleep/state/sensor/bed_temp";
const TOPIC_AMBIENT_TEMP: &str = "opensleep/state/sensor/ambient_temp";
const TOPIC_HUMIDITY: &str = "opensleep/state/sensor/humidity";
const TOPIC_MCU_TEMP: &str = "opensleep/state/sensor/mcu_temp";

impl SensorState {
    pub fn piezo_gain_ok(&self) -> bool {
        match self.piezo_gain {
            Some((l, r)) => {
                (PIEZO_GAIN as i16 - l as i16).abs() < PIEZO_TOLERANCE
                    && (PIEZO_GAIN as i16 - r as i16).abs() < PIEZO_TOLERANCE
            }
            None => false,
        }
    }

    pub fn piezo_freq_ok(&self) -> bool {
        // Pod 3 is configured to PIEZO_FREQ; the Pod 4 G0 firmware samples at
        // a fixed 500 Hz and reports that in its stream header — treat it as
        // healthy instead of re-sending SetPiezoFreq forever.
        matches!(self.piezo_freq, Some(f) if f == PIEZO_FREQ || f == 500)
    }

    pub fn piezo_ok(&self) -> bool {
        self.piezo_enabled && self.piezo_gain_ok() && self.piezo_freq_ok()
    }

    pub async fn set_device_mode(&mut self, client: &mut AsyncClient, mode: DeviceMode) {
        let prev = self.device_mode;
        self.device_mode = mode;

        if prev != mode {
            log::info!("Device mode: {prev:?} -> {mode:?}");
            publish_guaranteed_wait(client, TOPIC_MODE, false, mode.to_string()).await;
        }
    }

    pub fn get_alarm_for_side(&self, side: &BedSide) -> bool {
        match side {
            BedSide::Left => self.alarm_left_running,
            BedSide::Right => self.alarm_right_running,
        }
    }

    pub fn get_dismissed(&self, side: &BedSide) -> bool {
        match side {
            BedSide::Left => self.alarm_left_dismissed,
            BedSide::Right => self.alarm_right_dismissed,
        }
    }

    pub fn set_dismissed(&mut self, side: &BedSide, dismissed: bool) {
        match side {
            BedSide::Left => self.alarm_left_dismissed = dismissed,
            BedSide::Right => self.alarm_right_dismissed = dismissed,
        }
    }

    fn manual_slot(&mut self, side: &BedSide) -> &mut Option<std::time::Instant> {
        match side {
            BedSide::Left => &mut self.manual_alarm_until[0],
            BedSide::Right => &mut self.manual_alarm_until[1],
        }
    }

    pub fn set_manual_alarm(&mut self, side: &BedSide, until: Option<std::time::Instant>) {
        *self.manual_slot(side) = until;
    }

    pub fn manual_alarm_active(&self, side: &BedSide) -> bool {
        let until = match side {
            BedSide::Left => self.manual_alarm_until[0],
            BedSide::Right => self.manual_alarm_until[1],
        };
        until.is_some_and(|t| std::time::Instant::now() < t)
    }

    async fn publish_piezo_ok(&self, client: &mut AsyncClient) {
        publish_guaranteed_wait(client, TOPIC_PIEZO_OK, false, self.piezo_ok().to_string()).await;
    }

    pub async fn publish_reset(&self, client: &mut AsyncClient) {
        publish_guaranteed_wait(client, TOPIC_MODE, false, DeviceMode::Unknown.to_string()).await;
    }

    /// [%s] off
    /// [%s] start: power %u, pattern %u, dur %u ms
    /// [%s] no longer running (max duration)
    /// [%s] new sequence run. ramp power to %u
    fn handle_alarm_msg(&mut self, msg: &str) {
        let (bedside, rest) = if let Some(start) = msg.find('[') {
            if let Some(end) = msg.find(']') {
                let bedside = &msg[start + 1..end];
                let remaining = &msg[end + 1..];
                if bedside != "left" && bedside != "right" {
                    log::warn!("Unknown bedside in alarm message: {}", bedside);
                    return;
                }
                (bedside.to_string(), remaining.trim())
            } else {
                log::warn!("Alarm message missing closing bracket: {}", msg);
                return;
            }
        } else {
            log::warn!("Alarm message missing opening bracket: {}", msg);
            return;
        };

        let alarm_running = if bedside == "left" {
            &mut self.alarm_left_running
        } else {
            &mut self.alarm_right_running
        };

        if rest == "off" {
            log::info!("Alarm[{bedside}] off");
            *alarm_running = false;
        } else if rest == "no longer running (max duration)" {
            log::info!("Alarm[{bedside}] duration complete");
            *alarm_running = false;
        } else if let Some(rest) = rest.strip_prefix("start: ") {
            log::info!("Alarm[{bedside}] started: {rest}");
            *alarm_running = true;
        } else if let Some(val) = rest.strip_prefix("new sequence run. ramp power to ") {
            log::debug!("Alarm[{bedside}] ramping power to {val}");
            *alarm_running = true;
        } else {
            log::warn!("Unknown alarm message: {msg}");
        }
    }

    pub async fn handle_packet(&mut self, client: &mut AsyncClient, packet: SensorPacket) {
        match packet {
            SensorPacket::Pong(in_firmware) => {
                log::debug!(" <-- Pong");
                self.set_device_mode(client, DeviceMode::from_pong(in_firmware))
                    .await;
            }
            SensorPacket::HardwareInfo(info) => {
                log::info!("Hardware info: {info}");
                publish_guaranteed_wait(client, TOPIC_HWINFO, true, info.to_string()).await;
                self.hardware_info = Some(info);
            }
            SensorPacket::JumpingToFirmware(code) => {
                log::debug!("Jumping to firmware with code: 0x{code:02X}");
                self.set_device_mode(client, DeviceMode::Firmware).await;
            }
            SensorPacket::Message(msg) => {
                if let Some(stripped) = strip_alarm_prefix(&msg) {
                    self.handle_alarm_msg(stripped);
                } else if let Some(side) = fw_tap_dismissal(&msg) {
                    // The sensor FW detects double taps itself (LIS accel in
                    // the puck) and stops the alarm. Mark the side dismissed
                    // or the scheduler re-arms it 5s later — which is exactly
                    // what made the 2026-07-20 alarm undismissable.
                    log::info!("FW tap-dismissal on {side}; honoring for the rest of the window");
                    self.set_dismissed(&side, true);
                } else {
                    log::debug!("Message: {msg}");
                }
            }
            SensorPacket::PiezoGainSet(l, r) => {
                log::info!("Piezo Gain Set: {l},{r}");
                self.publish_piezo_ok(client).await;
                self.piezo_gain = Some((l, r));
            }
            SensorPacket::PiezoEnabled(val) => {
                log::info!("Piezo Enabled {val:02X}");
                self.publish_piezo_ok(client).await;
                self.piezo_enabled = true;
            }
            SensorPacket::VibrationEnabled(_, _) => {
                log::info!("Vibration Enabled");
                publish_guaranteed_wait(client, TOPIC_VIBRATION_ENABLED, false, "true").await;
                self.vibration_enabled = true;
            }
            SensorPacket::Capacitance(_) => {}
            SensorPacket::Temperature(u) => {
                publish_high_freq(
                    client,
                    TOPIC_BED_TEMP,
                    format!(
                        "{},{},{},{},{},{}",
                        u.bed[0], u.bed[1], u.bed[2], u.bed[3], u.bed[4], u.bed[5]
                    ),
                );
                publish_high_freq(client, TOPIC_AMBIENT_TEMP, u.ambient.to_string());
                publish_high_freq(client, TOPIC_HUMIDITY, u.humidity.to_string());
                publish_high_freq(client, TOPIC_MCU_TEMP, u.microcontroller.to_string());
            }
            SensorPacket::Piezo(u) => {
                let (enabled_changed, gain_changed, freq_changed);
                {
                    enabled_changed = !self.piezo_enabled;
                    gain_changed = self.piezo_gain != Some(u.gain);
                    freq_changed = self.piezo_freq != Some(u.freq);
                    self.piezo_enabled = true;
                    self.piezo_gain = Some(u.gain);
                    self.piezo_freq = Some(u.freq);
                }
                if gain_changed || freq_changed || enabled_changed {
                    self.publish_piezo_ok(client).await;
                }
            }
            // Pod 4 piezo stream: the header carries freq + a single gain. Not
            // registering these left piezo_freq None forever, so the scheduler
            // re-sent SetPiezoFreq every 800ms indefinitely (observed live).
            SensorPacket::Pod4Piezo(u) => {
                let enabled_changed = !self.piezo_enabled;
                let freq_changed = self.piezo_freq != Some(u.freq);
                self.piezo_enabled = true;
                self.piezo_freq = Some(u.freq);
                // The SetPiezoGain ack (0xAB) is the authoritative gain source
                // (its units are verified); the header gain only seeds an
                // otherwise-unknown state so gain scheduling can settle.
                let mut gain_changed = false;
                if self.piezo_gain.is_none() {
                    let gain = u.gain.min(u16::MAX as u32) as u16;
                    self.piezo_gain = Some((gain, gain));
                    gain_changed = true;
                }
                if gain_changed || freq_changed || enabled_changed {
                    self.publish_piezo_ok(client).await;
                }
            }
            SensorPacket::AlarmSet(v) => {
                log::info!("Alarm Set: {v}");
            }
            SensorPacket::Init(v) => {
                log::warn!("Init: {v}");
            }
            _ => {}
        }
    }
}

/// Extract the alarm-status part of a firmware log message.
///
/// The Pod 3 F0 firmware logs `FW: alarm[left] off`; the Pod 4 G0 firmware
/// prefixes a millis counter: `FW: 604914 alarm[left] off`. Matching only the
/// Pod 3 form left `alarm_*_running` false forever on Pod 4, so podd never
/// noticed a running alarm and never tried to cancel one (2026-07-20 incident).
fn strip_alarm_prefix(msg: &str) -> Option<&str> {
    let rest = msg.strip_prefix("FW: ")?;
    let rest = rest
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start();
    rest.strip_prefix("alarm")
}

/// Detect the FW's own accelerometer tap-dismissal message and which side it
/// came from. Observed live (Pod 4 G0):
/// `FW: 1392074 [lisR] dismissing alarm (2 taps)`
fn fw_tap_dismissal(msg: &str) -> Option<BedSide> {
    if !msg.starts_with("FW:") || !msg.contains("dismissing alarm") {
        return None;
    }
    if msg.contains("[lisL]") {
        Some(BedSide::Left)
    } else if msg.contains("[lisR]") {
        Some(BedSide::Right)
    } else {
        log::warn!("FW tap-dismissal with unknown side: {msg}");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_pod3_and_pod4_alarm_prefixes() {
        assert_eq!(
            strip_alarm_prefix("FW: alarm[left] off"),
            Some("[left] off")
        );
        assert_eq!(
            strip_alarm_prefix("FW: 604914 alarm[right] no longer running (max duration)"),
            Some("[right] no longer running (max duration)")
        );
        assert_eq!(
            strip_alarm_prefix("FW: 4914 alarm[left] start: power 80, pattern 1, dur 600000 ms"),
            Some("[left] start: power 80, pattern 1, dur 600000 ms")
        );
        assert_eq!(strip_alarm_prefix("FW: 123 something else"), None);
        assert_eq!(strip_alarm_prefix("unrelated"), None);
    }

    #[test]
    fn fw_tap_dismissal_parses_side() {
        assert_eq!(
            fw_tap_dismissal("FW: 1392074 [lisR] dismissing alarm (2 taps)"),
            Some(BedSide::Right)
        );
        assert_eq!(
            fw_tap_dismissal("FW: 55 [lisL] dismissing alarm (4 taps)"),
            Some(BedSide::Left)
        );
        assert_eq!(fw_tap_dismissal("FW: 55 [lisR] some other message"), None);
        assert_eq!(fw_tap_dismissal("[lisR] dismissing alarm (2 taps)"), None);
    }

    #[test]
    fn alarm_msgs_track_running_state() {
        let mut state = SensorState::default();

        state.handle_alarm_msg("[left] start: power 80, pattern 1, dur 600000 ms");
        assert!(state.alarm_left_running);
        assert!(!state.alarm_right_running);

        state.handle_alarm_msg("[right] new sequence run. ramp power to 80");
        assert!(state.alarm_right_running);

        state.handle_alarm_msg("[left] off");
        assert!(!state.alarm_left_running);

        state.handle_alarm_msg("[right] no longer running (max duration)");
        assert!(!state.alarm_right_running);
    }
}
