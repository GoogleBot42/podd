use std::io::ErrorKind;
use std::time::Duration;

use crate::bus::{Command, DeviceSnapshot, StatusTx};
use crate::config::{Config, SideConfig, SidesConfig};
use crate::sensor::presence::PresenseManager;
use crate::sensor::state::{PIEZO_FREQ, PIEZO_GAIN, SensorState};
use crate::sensor::tap::{Tap, TapDetector};
use pod_proto::codec::{CommandTrait, PacketCodec};
use pod_proto::packet::BedSide;
use pod_proto::sensor::command::{AlarmCommand, AlarmPattern};
use pod_proto::sensor::{SensorCommand, SensorPacket};
use pod_proto::serial::{DeviceMode, SerialError, create_framed_port};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use jiff::civil::Time;
use jiff::{Span, Timestamp};
use rumqttc::AsyncClient;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, interval, timeout};
use tokio_serial::SerialStream;
use tokio_util::codec::Framed;

const TIMEOUT: Duration = Duration::from_secs(5);

type Reader = SplitStream<Framed<SerialStream, PacketCodec<SensorPacket>>>;
type Writer = SplitSink<Framed<SerialStream, PacketCodec<SensorPacket>>, SensorCommand>;
type CommandCheck = fn(&SensorState, &Time, &bool, &SidesConfig) -> Option<SensorCommand>;

struct CommandScheduler {
    cmds: Vec<RegisteredCommand>,
    away_mode: bool,
    sides_config: SidesConfig,
    writer: Writer,
}

struct RegisteredCommand {
    name: &'static str,
    interval: Duration,
    last_run: Instant,
    can_run: CommandCheck,
    /// Give up (with a warning) after this many sends whose expected ack never
    /// arrived. The Pod 4 G0 firmware doesn't ack everything the Pod 3 F0 does
    /// (EnableVibration, GetHardwareInfo observed), and endlessly re-sending
    /// every 800ms bombards the MCU — suspected trigger of its hard wedge.
    /// None = unlimited (keepalives/telemetry polls).
    max_attempts: Option<u32>,
    attempts: u32,
}

#[derive(Error, Debug)]
pub enum SensorError {
    #[error("Serial: {0}")]
    Serial(#[from] SerialError),
    #[error("Sensor not responding")]
    Timeout,
}

/// Run the sensor subsystem, retrying forever on failure.
///
/// The sensor MCU is observed to drop off transiently (discovery pings unanswered,
/// or the stream going quiet mid-run). That must NOT take down the control core:
/// the frozen/TEC manager is what keeps the bed at temperature, and presence/piezo
/// data is a nice-to-have by comparison. Each retry reopens the port and redoes
/// discovery (which includes the bootloader->firmware jump), so an MCU that
/// recovers on its own is picked back up automatically.
#[allow(clippy::too_many_arguments)]
pub async fn supervise(
    port: &str,
    bootloader_baud: u32,
    firmware_baud: u32,
    config_tx: watch::Sender<Config>,
    config_rx: watch::Receiver<Config>,
    mut calibrate_rx: mpsc::Receiver<()>,
    client: AsyncClient,
    status: StatusTx,
    mut cmd_rx: mpsc::Receiver<Command>,
    dry_run: bool,
) -> Result<(), SensorError> {
    const RETRY_DELAY: Duration = Duration::from_secs(10);
    // A sensor MCU that answers nothing at either baud is usually hard-wedged:
    // observed live that in-process reopen+rediscovery never brings it back,
    // while the PCAL6416A reset pulse at process start revives it immediately.
    // The reset line is only pulsed in podd_core's bringup (and resets BOTH
    // MCUs), so after this many consecutive failures escalate by returning the
    // error: the process exits and systemd's restart performs the reset.
    const MAX_CONSECUTIVE_FAILURES: u32 = 6;
    // An attempt that survived this long connected and did real work; its
    // eventual failure is a fresh dropout, not part of a bring-up failure run.
    const PROGRESS: Duration = Duration::from_secs(60);
    let mut consecutive = 0u32;
    // Dismissals must survive sensor-task restarts: SensorState is rebuilt per
    // attempt, and a mid-window restart after a double-tap dismissal must not
    // re-fire the alarm.
    let mut dismissed = [false; 2];
    loop {
        let attempt_started = Instant::now();
        let res = run(
            port,
            bootloader_baud,
            firmware_baud,
            config_tx.clone(),
            config_rx.clone(),
            &mut calibrate_rx,
            client.clone(),
            status.clone(),
            &mut cmd_rx,
            dry_run,
            &mut dismissed,
        )
        .await;
        match res {
            Ok(()) => {
                consecutive = 0;
                log::error!("Sensor task exited cleanly; restarting it");
            }
            Err(e) => {
                if attempt_started.elapsed() >= PROGRESS {
                    consecutive = 0;
                }
                consecutive += 1;
                if consecutive >= MAX_CONSECUTIVE_FAILURES {
                    log::error!(
                        "Sensor task failed {consecutive}x in a row ({e}); escalating to a \
                         process restart for an MCU reset"
                    );
                    return Err(e);
                }
                log::error!("Sensor task failed: {e}; retrying in {RETRY_DELAY:?}");
            }
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    port: &str,
    bootloader_baud: u32,
    firmware_baud: u32,
    config_tx: watch::Sender<Config>,
    mut config_rx: watch::Receiver<Config>,
    calibrate_rx: &mut mpsc::Receiver<()>,
    mut client: AsyncClient,
    status: StatusTx,
    cmd_rx: &mut mpsc::Receiver<Command>,
    dry_run: bool,
    dismissed: &mut [bool; 2],
) -> Result<(), SensorError> {
    log::info!("Initializing Sensor Subsystem...");

    let mut presense_man = PresenseManager::new(config_tx, config_rx.clone(), client.clone());

    let mut state = SensorState::default();
    state.alarm_left_dismissed = dismissed[0];
    state.alarm_right_dismissed = dismissed[1];
    state.clock_synced = clock_is_synced();
    state.publish_reset(&mut client).await;

    let (writer, mut reader) =
        run_discovery(port, bootloader_baud, firmware_baud, &mut client, &mut state).await?;
    log::info!("Connected");

    let cfg = config_rx.borrow_and_update();
    let timezone = cfg.timezone.clone();
    let mut scheduler = CommandScheduler::new(cfg.away_mode, cfg.profile.clone(), writer);
    drop(cfg);

    // Defensive: we may have (re)started mid-alarm with no memory of starting
    // one (process restart, sensor-task restart, boot after power loss). Stop
    // both sides once the connection settles; the scheduler re-arms within
    // seconds if an alarm really should be running now. NOT sent immediately:
    // writes in the first moments after (re)discovery get lost (observed live
    // — the freshly reconfigured UART eats them, no ack, no FW reaction).
    let hygiene_at = Instant::now() + Duration::from_secs(2);
    let mut hygiene_done = false;

    let mut taps = TapDetector::default();
    let mut interval = interval(Duration::from_millis(50));
    let mut last_recv = Instant::now();
    let mut last_sync_check = Instant::now();

    loop {
        tokio::select! {
            Some(result) = reader.next() => match result {
                Ok(packet) => {
                    if let SensorPacket::Capacitance(data) = &packet {
                        presense_man.update(data);
                    }

                    // Double tap on a side's piezo while its alarm vibrates =
                    // dismiss (stock Eight Sleep gesture).
                    let tap_now = std::time::Instant::now();
                    let mut double_tapped = Vec::new();
                    match &packet {
                        SensorPacket::Pod4Piezo(d) => {
                            for (side, samples) in
                                [(BedSide::Left, &d.left), (BedSide::Right, &d.right)]
                            {
                                let samples = samples.iter().map(|s| *s as f64);
                                if taps.feed(&side, samples, tap_now) == Some(Tap::Double) {
                                    double_tapped.push(side);
                                }
                            }
                        }
                        SensorPacket::Piezo(d) => {
                            for (side, samples) in [
                                (BedSide::Left, &d.left_samples),
                                (BedSide::Right, &d.right_samples),
                            ] {
                                let samples = samples.iter().map(|s| *s as f64);
                                if taps.feed(&side, samples, tap_now) == Some(Tap::Double) {
                                    double_tapped.push(side);
                                }
                            }
                        }
                        _ => {}
                    }
                    for side in double_tapped {
                        if state.get_alarm_for_side(&side) {
                            log::info!("Double tap on {side} piezo: dismissing alarm");
                            scheduler.send_alarm_stop(dry_run, side).await;
                            state.set_dismissed(&side, true);
                        }
                    }

                    state.handle_packet(&mut client, packet).await;
                    publish_sensor(&status, &state, &presense_man.presence_state());

                    last_recv = Instant::now();
                }
                Err(e) => {
                    log::error!("Packet decode error: {e}");
                }
            },

            _ = interval.tick() => {
                if !hygiene_done && Instant::now() >= hygiene_at {
                    hygiene_done = true;
                    scheduler.send_alarm_stop(dry_run, BedSide::Left).await;
                    scheduler.send_alarm_stop(dry_run, BedSide::Right).await;
                }

                if !state.clock_synced
                    && Instant::now().duration_since(last_sync_check) > Duration::from_secs(5)
                {
                    last_sync_check = Instant::now();
                    state.clock_synced = clock_is_synced();
                    if state.clock_synced {
                        log::info!("System clock is NTP-synced; scheduled alarms armed");
                    }
                }

                // this is not expensive so its fine to do at 20hz
                let now = Timestamp::now().to_zoned(timezone.clone()).time();
                let _ = scheduler.update(&mut state, &now, dry_run).await?;
                dismissed[0] = state.alarm_left_dismissed;
                dismissed[1] = state.alarm_right_dismissed;

                if Instant::now().duration_since(last_recv) > TIMEOUT {
                    break Err(SensorError::Timeout);
                }
            }

            Some(_) = calibrate_rx.recv() => presense_man.start_calibration(),

            Ok(_) = config_rx.changed() => {
                let cfg = config_rx.borrow();
                scheduler.away_mode = cfg.away_mode;
                scheduler.sides_config = cfg.profile.clone();
            }

            // api / scheduler commands routed to the Sensor subsystem.
            Some(cmd) = cmd_rx.recv() => {
                scheduler.handle_command(dry_run, cmd, &mut state).await;
            }
        }
    }
}

/// systemd-timesyncd creates this file once the clock is NTP-synced. The pod
/// has no battery-backed RTC: at boot the clock is a restored pre-shutdown
/// timestamp, which on 2026-07-20 landed inside the morning alarm window on
/// replug and re-fired the alarm. Until sync, wall time can't gate actuation.
const CLOCK_SYNC_FILE: &str = "/run/systemd/timesync/synchronized";

fn clock_is_synced() -> bool {
    if matches!(
        std::env::var("PODD_ASSUME_CLOCK_SYNC").ok().as_deref(),
        Some("1") | Some("true")
    ) {
        return true;
    }
    std::path::Path::new(CLOCK_SYNC_FILE).exists()
}

/// Publish the Sensor subsystem's live telemetry into the state watch.
/// Read-only: derives the snapshot from already-parsed state (SAFE).
fn publish_sensor(status: &StatusTx, state: &SensorState, presence: &crate::sensor::presence::PresenceState) {
    status.send_modify(|s: &mut DeviceSnapshot| {
        s.presence_left = presence.left;
        s.presence_right = presence.right;
        if let Some((l, r)) = state.piezo_gain {
            s.gains = (l, r);
        }
        s.left.is_alarm_vibrating = state.alarm_left_running;
        s.right.is_alarm_vibrating = state.alarm_right_running;
    });
}

impl CommandScheduler {
    fn new(away_mode: bool, sides_config: SidesConfig, writer: Writer) -> Self {
        let now = Instant::now();
        const CONFIG_RES_TIME: Duration = Duration::from_millis(800);
        Self {
            away_mode,
            sides_config,
            writer,
            cmds: vec![
                RegisteredCommand {
                    name: "ping",
                    max_attempts: None,
                    attempts: 0,
                    interval: Duration::from_secs(4),
                    last_run: now,
                    can_run: |_, _, _, _| Some(SensorCommand::Ping),
                },
                RegisteredCommand {
                    name: "probe_temperature",
                    // EXPERIMENT(pod4-wedge): ProbeTemperature is a Pod 3 command;
                    // capped to test whether it wedges the Pod 4 G0 firmware.
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: Duration::from_secs(4),
                    // stagger
                    last_run: now + Duration::from_millis(2500),
                    can_run: |_, _, _, _| Some(SensorCommand::ProbeTemperature),
                },
                RegisteredCommand {
                    name: "hwinfo",
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: CONFIG_RES_TIME,
                    last_run: now,
                    can_run: |state, _, _, _| {
                        if state.hardware_info.is_none() {
                            Some(SensorCommand::GetHardwareInfo)
                        } else {
                            None
                        }
                    },
                },
                RegisteredCommand {
                    name: "enable_vibration",
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: CONFIG_RES_TIME,
                    last_run: now,
                    can_run: |s, _, _, _| {
                        if !s.vibration_enabled {
                            Some(SensorCommand::EnableVibration)
                        } else {
                            None
                        }
                    },
                },
                RegisteredCommand {
                    name: "piezo_gain",
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: CONFIG_RES_TIME,
                    last_run: now,
                    can_run: |state, _, _, _| {
                        if !state.piezo_gain_ok() {
                            Some(SensorCommand::SetPiezoGain(PIEZO_GAIN, PIEZO_GAIN))
                        } else {
                            None
                        }
                    },
                },
                RegisteredCommand {
                    name: "piezo_freq",
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: CONFIG_RES_TIME,
                    last_run: now,
                    can_run: |state, _, _, _| {
                        if state.piezo_enabled && !state.piezo_freq_ok() {
                            Some(SensorCommand::SetPiezoFreq(PIEZO_FREQ))
                        } else {
                            None
                        }
                    },
                },
                RegisteredCommand {
                    name: "enable_piezo",
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: CONFIG_RES_TIME,
                    last_run: now,
                    can_run: |s, _, _, _| {
                        if !s.piezo_enabled {
                            Some(SensorCommand::EnablePiezo)
                        } else {
                            None
                        }
                    },
                },
                RegisteredCommand {
                    name: "left_alarm",
                    max_attempts: None,
                    attempts: 0,
                    interval: Duration::from_secs(5),
                    last_run: now,
                    can_run: |state, now, away, sides_cfg| {
                        if state.vibration_enabled && !away {
                            get_alarm_cmd(state, now, sides_cfg, &BedSide::Left)
                        } else {
                            None
                        }
                    },
                },
                RegisteredCommand {
                    name: "right_alarm",
                    max_attempts: None,
                    attempts: 0,
                    interval: Duration::from_secs(5),
                    last_run: now,
                    can_run: |state, now, away, sides_cfg| {
                        if state.vibration_enabled && !away {
                            get_alarm_cmd(state, now, sides_cfg, &BedSide::Right)
                        } else {
                            None
                        }
                    },
                },
            ],
        }
    }

    /// finds the first command to send and sends it
    /// returns if it send a command
    async fn update(
        &mut self,
        state: &mut SensorState,
        time: &Time,
        dry_run: bool,
    ) -> Result<bool, SensorError> {
        let now = Instant::now();

        // A dismissal holds for the rest of its alarm window; re-arm afterwards.
        for side in [BedSide::Left, BedSide::Right] {
            if state.get_dismissed(&side)
                && !in_alarm_window(self.sides_config.get_side(&side), time)
            {
                state.set_dismissed(&side, false);
            }
        }

        // find command to send
        for reg_cmd in &mut self.cmds {
            if now.duration_since(reg_cmd.last_run) > reg_cmd.interval
                && let Some(sen_cmd) =
                    (reg_cmd.can_run)(&*state, time, &self.away_mode, &self.sides_config)
            {
                if let Some(max) = reg_cmd.max_attempts
                    && reg_cmd.attempts >= max
                {
                    if reg_cmd.attempts == max {
                        reg_cmd.attempts += 1; // warn only once
                        log::warn!(
                            "{}: no ack after {max} attempts; giving up (Pod 4 firmware \
                             doesn't ack everything Pod 3 does)",
                            reg_cmd.name
                        );
                        // The command itself almost certainly took effect — the
                        // G0 firmware just doesn't ack it. Without this, alarms
                        // (gated on vibration_enabled) could never arm on Pod 4.
                        if reg_cmd.name == "enable_vibration" {
                            state.vibration_enabled = true;
                        }
                    }
                    continue;
                }
                // dry_run means no actuation: alarms are only logged. Sensing
                // config (piezo/ping/hwinfo) still goes out, or there would be
                // no presence/HR data to look at while dry-running.
                if dry_run && matches!(sen_cmd, SensorCommand::SetAlarm(_)) {
                    reg_cmd.last_run = now;
                    log::warn!(
                        "[dry-run] {} would send {:?}: {:02X?}",
                        reg_cmd.name,
                        sen_cmd,
                        sen_cmd.to_bytes()
                    );
                    return Ok(true);
                }
                reg_cmd.last_run = now;
                reg_cmd.attempts += 1;
                log::debug!(" -> {:?} (from {})", sen_cmd, reg_cmd.name);
                if let Err(e) = self.writer.send(sen_cmd).await {
                    log::error!("Failed to send {}: {e}", reg_cmd.name);
                }
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Stop any vibration on `side` right now.
    ///
    /// Uses an intensity-0/duration-0 `SetAlarm`: the dedicated ClearAlarm
    /// opcode (0x2D) is unverified and has crashed the MCU (see pod-proto).
    async fn send_alarm_stop(&mut self, dry_run: bool, side: BedSide) {
        let stop = SensorCommand::SetAlarm(AlarmCommand {
            side,
            intensity: 0,
            duration: 0,
            pattern: AlarmPattern::Double,
        });
        if dry_run {
            log::warn!("[dry-run] would send alarm stop: {:02X?}", stop.to_bytes());
            return;
        }
        if let Err(e) = self.writer.send(stop).await {
            log::error!("Failed to send alarm stop for {side}: {e}");
        }
    }

    /// Translate a bus [`Command`] into a Sensor control frame.
    ///
    /// **The actual MCU write is gated behind `dry_run` (default true).** While
    /// dry-running we log the intended frame + bytes and send nothing.
    async fn handle_command(&mut self, dry_run: bool, cmd: Command, state: &mut SensorState) {
        let frame = match cmd {
            Command::ClearAlarm { side } => {
                // Dismissal: hold until the alarm window ends so the scheduler
                // doesn't re-arm in 5s, and drop any manual-alarm grace.
                state.set_dismissed(&side, true);
                state.set_manual_alarm(&side, None);
                Some(SensorCommand::SetAlarm(AlarmCommand {
                    side,
                    intensity: 0,
                    duration: 0,
                    pattern: AlarmPattern::Double,
                }))
            }
            Command::FireAlarm(spec) => {
                // Manually fired (API test alarm): give it grace for its whole
                // duration or the scheduler's out-of-window cancel kills it.
                state.set_dismissed(&spec.side, false);
                state.set_manual_alarm(
                    &spec.side,
                    Some(
                        std::time::Instant::now()
                            + Duration::from_secs(u64::from(spec.duration_s) + 5),
                    ),
                );
                Some(SensorCommand::SetAlarm(AlarmCommand {
                    side: spec.side,
                    intensity: spec.intensity,
                    duration: spec.duration_s,
                    pattern: spec.pattern,
                }))
            }
            other => {
                log::warn!("Sensor subsystem received unroutable command: {other:?}");
                None
            }
        };

        let Some(frame) = frame else { return };

        if dry_run {
            log::warn!(
                "[dry-run] Sensor would send {:?}: {:02X?}",
                frame,
                frame.to_bytes()
            );
        } else {
            if let Err(e) = self.writer.send(frame).await {
                log::error!("Failed to send sensor command: {e}");
            }
        }
    }
}

/// alarm runs from (wake - alarm_offset) to ((wake - alarm_offset) + alarm_duration)
fn in_alarm_window(cfg: &SideConfig, now: &Time) -> bool {
    let Some(alarm_cfg) = cfg.alarm.as_ref() else {
        return false;
    };
    let alarm_start = cfg.wake - Span::new().seconds(alarm_cfg.offset);
    let alarm_end = alarm_start + Span::new().seconds(alarm_cfg.duration);
    *now > alarm_start && *now < alarm_end
}

fn get_alarm_cmd(
    state: &SensorState,
    now: &Time,
    sides_config: &SidesConfig,
    side: &BedSide,
) -> Option<SensorCommand> {
    let cfg = sides_config.get_side(side);
    let alarm_cfg = cfg.alarm.as_ref()?;
    let alarm_running = state.get_alarm_for_side(side);

    if in_alarm_window(cfg, now) {
        if !alarm_running && !state.get_dismissed(side) {
            if !state.clock_synced {
                log::warn!(
                    "Alarm[{side}] is due, but the clock is not NTP-synced yet; \
                     suppressing (wall time can't be trusted without an RTC)"
                );
                return None;
            }
            log::info!("Alarm[{side}] requesting to start");
            return Some(SensorCommand::SetAlarm(AlarmCommand {
                side: *side,
                intensity: alarm_cfg.intensity,
                duration: alarm_cfg.duration,
                pattern: alarm_cfg.pattern.clone(),
            }));
        }
    } else if alarm_running {
        if state.manual_alarm_active(side) {
            // A manually fired alarm (API test) is allowed outside the window.
            return None;
        }
        // Cancel via an intensity-0 SetAlarm (ClearAlarm 0x2D is unverified
        // and has crashed the MCU — see pod-proto). Retries every interval
        // until the FW's "alarm[side] off" message clears alarm_running.
        log::info!("Alarm[{side}] should NOT be running, but is. Cancelling.");
        return Some(SensorCommand::SetAlarm(AlarmCommand {
            side: *side,
            intensity: 0,
            duration: 0,
            pattern: AlarmPattern::Double,
        }));
    }

    None
}

/// tries to connect to the Sensor subsystem at either bootloader baud or firmware baud
async fn run_discovery(
    port: &str,
    bootloader_baud: u32,
    firmware_baud: u32,
    client: &mut AsyncClient,
    state: &mut SensorState,
) -> Result<(Writer, Reader), SerialError> {
    // try bootloader first
    if let Ok((mut writer, mut reader)) =
        ping_device(port, bootloader_baud, firmware_baud, client, state, DeviceMode::Bootloader).await
    {
        writer
            .send(SensorCommand::JumpToFirmware)
            .await
            .map_err(|e| SerialError::Io(std::io::Error::other(e)))?;

        // wait for mode switch
        wait_for_mode(&mut reader, client, state, DeviceMode::Firmware).await?;

        // Release the bootloader-baud port (and its TIOCEXCL exclusive lock)
        // BEFORE reopening at firmware baud. If the old fd is still open, the
        // reopen's TIOCEXCL fails ("Unable to acquire exclusive lock on serial
        // port"), the sensor task errors, and that tears down the whole control
        // core (the frozen/TEC manager gets cancelled with it).
        drop(writer);
        drop(reader);

        return Ok(create_framed_port::<SensorPacket>(port, firmware_baud)?.split());
    }

    // try firmware (happens if program was recently running)
    log::info!("Trying Firmware mode");
    ping_device(port, bootloader_baud, firmware_baud, client, state, DeviceMode::Firmware).await
}

async fn ping_device(
    port: &str,
    bootloader_baud: u32,
    firmware_baud: u32,
    client: &mut AsyncClient,
    state: &mut SensorState,
    mode: DeviceMode,
) -> Result<(Writer, Reader), SerialError> {
    let baud = if mode == DeviceMode::Bootloader {
        bootloader_baud
    } else {
        firmware_baud
    };
    let (mut writer, mut reader) = create_framed_port::<SensorPacket>(port, baud)?.split();

    for _ in 0..3 {
        writer
            .send(SensorCommand::Ping)
            .await
            .map_err(|e| SerialError::Io(std::io::Error::other(e)))?;

        if let Ok(Some(Ok(packet))) = timeout(Duration::from_millis(500), reader.next()).await {
            state.set_device_mode(client, mode).await;
            state.handle_packet(client, packet).await;
            return Ok((writer, reader));
        }
    }

    Err(SerialError::Io(std::io::Error::new(
        ErrorKind::NotFound,
        "Sensor not responding",
    )))
}

async fn wait_for_mode(
    reader: &mut Reader,
    client: &mut AsyncClient,
    state: &mut SensorState,
    target_mode: DeviceMode,
) -> Result<(), SerialError> {
    let timeout_duration = Duration::from_secs(5);
    let start = std::time::Instant::now();

    while state.device_mode != target_mode {
        if start.elapsed() > timeout_duration {
            return Err(SerialError::Io(std::io::Error::new(
                ErrorKind::TimedOut,
                "Timed out waiting for mode change",
            )));
        }

        if let Some(Ok(packet)) = reader.next().await {
            state.handle_packet(client, packet).await;
        }
    }

    Ok(())
}
