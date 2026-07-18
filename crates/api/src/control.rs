//! The hardware seam.
//!
//! [`PodControl`] is the command interface the HTTP layer drives; a real
//! implementation (later, in `podd-core`) maps these to LSP frames / MQTT / the
//! scheduler. [`MockControl`] records calls for tests and the example server so
//! the whole API is exercisable without any hardware.

use crate::wire::{AlarmJob, Side};
use async_trait::async_trait;
use std::sync::Mutex;

/// Async command interface the API uses to drive the pod. Decoupled from
/// hardware on purpose: the API crate never touches `podd-core` or a UART.
#[async_trait]
pub trait PodControl: Send + Sync {
    /// Set the target temperature for a side, in integer °F (55–110).
    async fn set_target_temp(&self, side: Side, temp_f: i32) -> anyhow::Result<()>;

    /// Turn a side on/off. Maps to a temperature-duration command of
    /// `43200`s (on) or `0`s (off) in the real implementation.
    async fn set_power(&self, side: Side, on: bool) -> anyhow::Result<()>;

    /// Clear (dismiss) a vibrating alarm on a side. Can only clear, never set.
    async fn clear_alarm(&self, side: Side) -> anyhow::Result<()>;

    /// Prime the water circuit.
    async fn prime(&self) -> anyhow::Result<()>;

    /// Fire an alarm immediately (the `POST /api/alarm` "test alarm" path).
    async fn fire_alarm(&self, job: AlarmJob) -> anyhow::Result<()>;

    /// Apply a device-settings block (CBOR-encoded on the wire to the cover).
    async fn apply_device_settings(&self, settings: serde_json::Value) -> anyhow::Result<()>;

    /// Reboot the device.
    async fn reboot(&self) -> anyhow::Result<()>;

    /// Trigger a firmware/software update.
    async fn update(&self) -> anyhow::Result<()>;

    /// Generic low-level command escape hatch. Returns a human-readable message
    /// on success; an `Err` is surfaced to the client as `400 "Invalid command"`.
    async fn execute(&self, command: &str, arg: Option<&str>) -> anyhow::Result<String>;
}

/// A recorded call against a [`MockControl`].
#[derive(Clone, Debug)]
pub enum Call {
    SetTargetTemp(Side, i32),
    SetPower(Side, bool),
    ClearAlarm(Side),
    Prime,
    FireAlarm(AlarmJob),
    ApplyDeviceSettings(serde_json::Value),
    Reboot,
    Update,
    Execute(String, Option<String>),
}

/// In-memory [`PodControl`] that records every call. `execute` succeeds for
/// commands in `valid_commands` and fails otherwise (so unknown commands map to
/// `400 "Invalid command"`).
pub struct MockControl {
    calls: Mutex<Vec<Call>>,
    valid_commands: Vec<String>,
}

impl Default for MockControl {
    fn default() -> Self {
        MockControl {
            calls: Mutex::new(Vec::new()),
            valid_commands: [
                "prime",
                "reboot",
                "setTemperatureDuration",
                "setTemperature",
                "alarmClear",
                "setSettings",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl MockControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all recorded calls, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

#[async_trait]
impl PodControl for MockControl {
    async fn set_target_temp(&self, side: Side, temp_f: i32) -> anyhow::Result<()> {
        self.record(Call::SetTargetTemp(side, temp_f));
        Ok(())
    }

    async fn set_power(&self, side: Side, on: bool) -> anyhow::Result<()> {
        self.record(Call::SetPower(side, on));
        Ok(())
    }

    async fn clear_alarm(&self, side: Side) -> anyhow::Result<()> {
        self.record(Call::ClearAlarm(side));
        Ok(())
    }

    async fn prime(&self) -> anyhow::Result<()> {
        self.record(Call::Prime);
        Ok(())
    }

    async fn fire_alarm(&self, job: AlarmJob) -> anyhow::Result<()> {
        self.record(Call::FireAlarm(job));
        Ok(())
    }

    async fn apply_device_settings(&self, settings: serde_json::Value) -> anyhow::Result<()> {
        self.record(Call::ApplyDeviceSettings(settings));
        Ok(())
    }

    async fn reboot(&self) -> anyhow::Result<()> {
        self.record(Call::Reboot);
        Ok(())
    }

    async fn update(&self) -> anyhow::Result<()> {
        self.record(Call::Update);
        Ok(())
    }

    async fn execute(&self, command: &str, arg: Option<&str>) -> anyhow::Result<String> {
        self.record(Call::Execute(command.to_string(), arg.map(|s| s.to_string())));
        if self.valid_commands.iter().any(|c| c == command) {
            Ok(format!("executed {command}"))
        } else {
            anyhow::bail!("invalid command: {command}")
        }
    }
}
