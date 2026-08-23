//! The hardware seam.
//!
//! [`PodControl`] is the command interface the HTTP layer drives; a real
//! implementation (later, in `podd-core`) maps these to LSP frames / MQTT / the
//! scheduler. [`MockControl`] records calls for tests and the example server so
//! the whole API is exercisable without any hardware.

use crate::wire::{AlarmJob, Side, VibrationPattern};
use async_trait::async_trait;
use jiff::civil::Time;
use podd_core::bus::{AlarmSpec, Command};
use pod_proto::packet::BedSide;
use pod_proto::sensor::command::AlarmPattern;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Error marker for commands the daemon accepts on the wire but has not wired
/// to the hardware yet. Handlers map it to `501 Not Implemented` so callers
/// aren't told a no-op succeeded (#32).
#[derive(Debug)]
pub struct NotImplemented(pub &'static str);

impl std::fmt::Display for NotImplemented {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not implemented yet", self.0)
    }
}

impl std::error::Error for NotImplemented {}

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

    /// Prime the water circuit now. Always runs, whatever the daily-prime
    /// toggle says.
    async fn prime(&self) -> anyhow::Result<()>;

    /// Apply the "Prime daily?" setting (toggle + local time) to the daemon's
    /// live config. `time` is already validated by the caller.
    async fn set_prime_daily(&self, enabled: bool, time: Time) -> anyhow::Result<()>;

    /// Fire an alarm immediately (the `POST /api/alarm` "test alarm" path).
    async fn fire_alarm(&self, job: AlarmJob) -> anyhow::Result<()>;

    /// Apply a device-settings block (CBOR-encoded on the wire to the cover).
    async fn apply_device_settings(&self, settings: serde_json::Value) -> anyhow::Result<()>;

    /// Reboot the device.
    async fn reboot(&self) -> anyhow::Result<()>;

    /// Trigger a firmware/software update.
    async fn update(&self) -> anyhow::Result<()>;

    /// Generic low-level command escape hatch. Returns a human-readable message
    /// on success; a [`NotImplemented`] `Err` is surfaced to the client as
    /// `501`, any other `Err` as `400 "Invalid command"`.
    async fn execute(&self, command: &str, arg: Option<&str>) -> anyhow::Result<String>;
}

/// A recorded call against a [`MockControl`].
#[derive(Clone, Debug)]
pub enum Call {
    SetTargetTemp(Side, i32),
    SetPower(Side, bool),
    ClearAlarm(Side),
    Prime,
    SetPrimeDaily(bool, Time),
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

    async fn set_prime_daily(&self, enabled: bool, time: Time) -> anyhow::Result<()> {
        self.record(Call::SetPrimeDaily(enabled, time));
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

// ---------------------------------------------------------------------------
// PoddControl — the real hardware seam
// ---------------------------------------------------------------------------

/// Default "power on" session length (seconds). free-sleep uses 43200s (12h).
const POWER_ON_SECONDS: u32 = 43200;

fn to_bed_side(side: Side) -> BedSide {
    match side {
        Side::Left => BedSide::Left,
        Side::Right => BedSide::Right,
    }
}

/// Map the wire vibration pattern to a firmware alarm pattern. The firmware has
/// no distinct "rise" ramp, so `Rise` degrades to `Single`.
fn to_alarm_pattern(p: VibrationPattern) -> AlarmPattern {
    match p {
        VibrationPattern::Double => AlarmPattern::Double,
        VibrationPattern::Rise => AlarmPattern::Single,
    }
}

/// The real [`PodControl`]: maps each API command to a [`Command`] and pushes it
/// onto the bus's mpsc into `podd-core`'s managers. Whether a command actually
/// reaches an MCU is decided downstream by the managers' `dry_run` gate.
pub struct PoddControl {
    commands: mpsc::Sender<Command>,
}

impl PoddControl {
    pub fn new(commands: mpsc::Sender<Command>) -> Self {
        Self { commands }
    }

    async fn send(&self, cmd: Command) -> anyhow::Result<()> {
        self.commands
            .send(cmd)
            .await
            .map_err(|e| anyhow::anyhow!("command channel closed: {e}"))
    }
}

#[async_trait]
impl PodControl for PoddControl {
    async fn set_target_temp(&self, side: Side, temp_f: i32) -> anyhow::Result<()> {
        self.send(Command::SetTargetTempF {
            side: to_bed_side(side),
            f: temp_f,
        })
        .await
    }

    async fn set_power(&self, side: Side, on: bool) -> anyhow::Result<()> {
        self.send(Command::SetPower {
            side: to_bed_side(side),
            on,
            duration_s: if on { POWER_ON_SECONDS } else { 0 },
        })
        .await
    }

    async fn clear_alarm(&self, side: Side) -> anyhow::Result<()> {
        self.send(Command::ClearAlarm {
            side: to_bed_side(side),
        })
        .await
    }

    async fn prime(&self) -> anyhow::Result<()> {
        self.send(Command::Prime).await
    }

    async fn set_prime_daily(&self, enabled: bool, time: Time) -> anyhow::Result<()> {
        self.send(Command::SetPrimeDaily { enabled, time }).await
    }

    async fn fire_alarm(&self, job: AlarmJob) -> anyhow::Result<()> {
        self.send(Command::FireAlarm(AlarmSpec {
            side: to_bed_side(job.side),
            intensity: job.vibration_intensity.clamp(0, 100) as u8,
            duration_s: job.duration.max(0) as u32,
            pattern: to_alarm_pattern(job.vibration_pattern),
        }))
        .await
    }

    // Nothing downstream applies these yet: the dispatcher's warn arms in
    // podd-core just drop them. Fail honestly instead of queueing into the
    // void and reporting success (#32).
    async fn apply_device_settings(&self, _settings: serde_json::Value) -> anyhow::Result<()> {
        Err(NotImplemented("applying device settings").into())
    }

    async fn reboot(&self) -> anyhow::Result<()> {
        Err(NotImplemented("reboot").into())
    }

    async fn update(&self) -> anyhow::Result<()> {
        self.send(Command::Update).await
    }

    async fn execute(&self, _command: &str, _arg: Option<&str>) -> anyhow::Result<String> {
        Err(NotImplemented("the execute escape hatch").into())
    }
}
