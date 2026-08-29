use std::io::ErrorKind;
use std::time::Duration;

use crate::alarm::{self, ResolvedAlarm};
use crate::bus::{Command, DeviceSnapshot, StatusTx};
use crate::config::{AwayMode, Config, SidesConfig};
use crate::health::{self, Health, HealthRegistry};
use crate::schedule::{Schedules, SideSchedule};
use crate::settings::{AlarmOverride, Settings};
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
use jiff::{Timestamp, Zoned};
use rumqttc::AsyncClient;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, interval, timeout};
use tokio_serial::SerialStream;
use tokio_util::codec::Framed;

const TIMEOUT: Duration = Duration::from_secs(5);

/// How long a fresh sensor connection is reported as `Started` rather than
/// `Healthy`: the MCU is a zombie for ~60 s after any podd restart (it streams
/// telemetry and answers Ping but silently ignores actuation writes). Purely a
/// reporting threshold — it gates nothing.
const SETTLE: Duration = Duration::from_secs(60);

type Reader = SplitStream<Framed<SerialStream, PacketCodec<SensorPacket>>>;
type Writer = SplitSink<Framed<SerialStream, PacketCodec<SensorPacket>>, SensorCommand>;
type CommandCheck = fn(&SensorState, &Zoned, &AlarmSources) -> Option<SensorCommand>;

/// Everything the scheduled-alarm decision needs besides sensor state: both
/// alarm sources (weekly document + legacy profile), away mode, and the
/// per-side one-shot overrides. Kept fresh from the config/schedules/settings
/// watches by `run`'s select arms.
struct AlarmSources {
    away_mode: AwayMode,
    profile: SidesConfig,
    schedules: Schedules,
    /// `[left, right]` (settings.json `scheduleOverrides.alarm`).
    overrides: [AlarmOverride; 2],
}

impl AlarmSources {
    fn override_for(&self, side: &BedSide) -> &AlarmOverride {
        match side {
            BedSide::Left => &self.overrides[0],
            BedSide::Right => &self.overrides[1],
        }
    }

    /// The weekly document side for `side`. Solo profiles have no per-side
    /// split — both bed sides map to `left`, exactly as the frozen manager's
    /// `wanted_target` does.
    fn weekly_for(&self, side: &BedSide) -> &SideSchedule {
        match (&self.profile, side) {
            (SidesConfig::Solo(_), _) | (SidesConfig::Couples { .. }, BedSide::Left) => {
                &self.schedules.left
            }
            (SidesConfig::Couples { .. }, BedSide::Right) => &self.schedules.right,
        }
    }

    /// The scheduled alarm whose window contains `now`, if any. Away mode and
    /// dismissals are *not* considered — callers gate on them separately so
    /// the cancel path stays reachable.
    fn resolve(&self, side: &BedSide, now: &Zoned) -> Option<ResolvedAlarm> {
        alarm::resolve_alarm(
            self.weekly_for(side),
            self.profile.get_side(side),
            self.override_for(side),
            now,
        )
    }
}

/// The two per-side alarm overrides out of a settings document.
fn overrides_from(settings: &Settings) -> [AlarmOverride; 2] {
    [
        settings.left.schedule_overrides.alarm.clone(),
        settings.right.schedule_overrides.alarm.clone(),
    ]
}

struct CommandScheduler {
    cmds: Vec<RegisteredCommand>,
    sources: AlarmSources,
    writer: Writer,
    /// A manually fired alarm (API test) we haven't seen start yet. The G0
    /// eats writes early in a connection, so resend until the FW confirms.
    pending_fire: Option<PendingFire>,
    /// Observation only: used to surface an unconfirmed alarm write.
    health: HealthRegistry,
}

struct PendingFire {
    cmd: AlarmCommand,
    last_sent: Instant,
    attempts: u32,
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
    schedules_rx: watch::Receiver<Schedules>,
    settings_rx: watch::Receiver<Settings>,
    config_path: std::sync::Arc<str>,
    mut calibrate_rx: mpsc::Receiver<()>,
    client: AsyncClient,
    status: StatusTx,
    mut cmd_rx: mpsc::Receiver<Command>,
    health: HealthRegistry,
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
            schedules_rx.clone(),
            settings_rx.clone(),
            config_path.clone(),
            &mut calibrate_rx,
            client.clone(),
            status.clone(),
            &mut cmd_rx,
            health.clone(),
            dry_run,
            &mut dismissed,
        )
        .await;
        match res {
            Ok(()) => {
                consecutive = 0;
                log::error!("Sensor task exited cleanly; restarting it");
                health.report(
                    health::SENSOR,
                    Health::Restarting,
                    format!("task exited cleanly; restarting in {RETRY_DELAY:?}"),
                );
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
                    health.report(
                        health::SENSOR,
                        Health::Failed,
                        format!(
                            "failed {consecutive}x in a row ({e}); restarting podd to reset the MCU"
                        ),
                    );
                    return Err(e);
                }
                log::error!("Sensor task failed: {e}; retrying in {RETRY_DELAY:?}");
                health.report(
                    health::SENSOR,
                    Health::Retrying,
                    format!("{e}; retrying in {RETRY_DELAY:?} (attempt {consecutive})"),
                );
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
    mut schedules_rx: watch::Receiver<Schedules>,
    mut settings_rx: watch::Receiver<Settings>,
    config_path: std::sync::Arc<str>,
    calibrate_rx: &mut mpsc::Receiver<()>,
    mut client: AsyncClient,
    status: StatusTx,
    cmd_rx: &mut mpsc::Receiver<Command>,
    health: HealthRegistry,
    dry_run: bool,
    dismissed: &mut [bool; 2],
) -> Result<(), SensorError> {
    log::info!("Initializing Sensor Subsystem...");
    health.report(health::SENSOR, Health::Started, "discovering the sensor MCU");

    let mut presense_man =
        PresenseManager::new(config_tx, config_rx.clone(), config_path, client.clone());

    let mut state = SensorState::default();
    state.alarm_left_dismissed = dismissed[0];
    state.alarm_right_dismissed = dismissed[1];
    state.clock_synced = clock_is_synced();
    publish_clock_health(&health, state.clock_synced);
    state.publish_reset(&mut client).await;

    let (writer, mut reader) =
        run_discovery(port, bootloader_baud, firmware_baud, &mut client, &mut state).await?;
    log::info!("Connected");
    // Not `Healthy` yet: for ~60 s after a (re)start the MCU streams telemetry
    // and answers Ping while silently ignoring actuation writes. `Started` is
    // the honest state for that window; the tick loop below promotes it.
    health.report(
        health::SENSOR,
        Health::Started,
        "connected; MCU may ignore actuation writes for ~60 s after a restart",
    );
    let connected_at = Instant::now();
    let mut settled = false;

    let cfg = config_rx.borrow_and_update();
    let mut timezone = cfg.timezone.clone();
    let sources = AlarmSources {
        away_mode: cfg.away_mode,
        profile: cfg.profile.clone(),
        schedules: schedules_rx.borrow_and_update().clone(),
        overrides: overrides_from(&settings_rx.borrow_and_update()),
    };
    let mut scheduler = CommandScheduler::new(sources, writer, health.clone());
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
                        presense_man.update(data).await;
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

                // Observation only: once the connection has outlived the
                // post-restart write-blind window, call the sensor healthy.
                if !settled && Instant::now().duration_since(connected_at) >= SETTLE {
                    settled = true;
                    health.report(
                        health::SENSOR,
                        Health::Healthy,
                        "connected; streaming telemetry",
                    );
                }

                if !state.clock_synced
                    && Instant::now().duration_since(last_sync_check) > Duration::from_secs(5)
                {
                    last_sync_check = Instant::now();
                    state.clock_synced = clock_is_synced();
                    if state.clock_synced {
                        log::info!("System clock is NTP-synced; scheduled alarms armed");
                    }
                    publish_clock_health(&health, state.clock_synced);
                }

                // this is not expensive so its fine to do at 20hz
                let now = Timestamp::now().to_zoned(timezone.clone());
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
                timezone = cfg.timezone.clone();
                scheduler.sources.away_mode = cfg.away_mode;
                scheduler.sources.profile = cfg.profile.clone();
            }

            // Weekly schedule / settings edits (Schedule page, override
            // dialogs): refresh the alarm sources.
            Ok(_) = schedules_rx.changed() => {
                scheduler.sources.schedules = schedules_rx.borrow().clone();
            }
            Ok(_) = settings_rx.changed() => {
                scheduler.sources.overrides = overrides_from(&settings_rx.borrow());
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

/// Mirror the NTP-sync check onto the health bus. Not synced is `Retrying`,
/// not `Failed`: timesyncd keeps trying, and the only consequence is that
/// scheduled alarms stay disarmed (which is the safe state).
fn publish_clock_health(health: &HealthRegistry, synced: bool) {
    if synced {
        health.report(
            health::CLOCK,
            Health::Healthy,
            "NTP-synced; scheduled alarms armed",
        );
    } else {
        health.report(
            health::CLOCK,
            Health::Retrying,
            "waiting for NTP sync; scheduled alarms are held (no RTC battery)",
        );
    }
}

pub(crate) fn clock_is_synced() -> bool {
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
    fn new(sources: AlarmSources, writer: Writer, health: HealthRegistry) -> Self {
        let now = Instant::now();
        const CONFIG_RES_TIME: Duration = Duration::from_millis(800);
        Self {
            sources,
            writer,
            pending_fire: None,
            health,
            cmds: vec![
                RegisteredCommand {
                    name: "ping",
                    max_attempts: None,
                    attempts: 0,
                    interval: Duration::from_secs(4),
                    last_run: now,
                    can_run: |_, _, _| Some(SensorCommand::Ping),
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
                    can_run: |_, _, _| Some(SensorCommand::ProbeTemperature),
                },
                RegisteredCommand {
                    name: "hwinfo",
                    max_attempts: Some(10),
                    attempts: 0,
                    interval: CONFIG_RES_TIME,
                    last_run: now,
                    can_run: |state, _, _| {
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
                    can_run: |s, _, _| {
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
                    can_run: |state, _, _| {
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
                    can_run: |state, _, _| {
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
                    can_run: |s, _, _| {
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
                    can_run: |state, now, sources| {
                        if state.vibration_enabled {
                            get_alarm_cmd(state, now, sources, &BedSide::Left)
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
                    can_run: |state, now, sources| {
                        if state.vibration_enabled {
                            get_alarm_cmd(state, now, sources, &BedSide::Right)
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
        time: &Zoned,
        dry_run: bool,
    ) -> Result<bool, SensorError> {
        let now = Instant::now();

        // A dismissal holds for the rest of its alarm window; re-arm afterwards.
        for side in [BedSide::Left, BedSide::Right] {
            if state.get_dismissed(&side) && self.sources.resolve(&side, time).is_none() {
                state.set_dismissed(&side, false);
            }
        }

        // Resend an unconfirmed manual fire (the G0 eats early writes).
        let resend = match &mut self.pending_fire {
            Some(pending) => {
                if state.get_alarm_for_side(&pending.cmd.side) {
                    self.pending_fire = None; // FW confirmed the start
                    None
                } else if now.duration_since(pending.last_sent) > Duration::from_secs(2) {
                    if pending.attempts >= 30 {
                        log::warn!(
                            "FireAlarm[{}]: no FW confirmation after {} sends; giving up",
                            pending.cmd.side,
                            pending.attempts
                        );
                        // A write the firmware never confirmed is a real
                        // actuation failure — the thing a status page exists
                        // to show. (The scheduler's own `max_attempts`
                        // give-up is NOT reported: on Pod 4 the G0 simply
                        // doesn't ack commands it did apply.)
                        self.health.report(
                            health::SENSOR,
                            Health::Failed,
                            format!(
                                "alarm[{}] write never confirmed by the firmware after {} sends",
                                pending.cmd.side, pending.attempts
                            ),
                        );
                        self.pending_fire = None;
                        None
                    } else {
                        pending.attempts += 1;
                        pending.last_sent = now;
                        log::info!(
                            "FireAlarm[{}] unconfirmed; resending (attempt {})",
                            pending.cmd.side,
                            pending.attempts
                        );
                        Some(SensorCommand::SetAlarm(pending.cmd.clone()))
                    }
                } else {
                    None
                }
            }
            None => None,
        };
        if let Some(frame) = resend {
            if let Err(e) = self.writer.send(frame).await {
                log::error!("Failed to resend manual alarm: {e}");
            }
            return Ok(true);
        }

        // find command to send
        for reg_cmd in &mut self.cmds {
            if now.duration_since(reg_cmd.last_run) > reg_cmd.interval
                && let Some(sen_cmd) = (reg_cmd.can_run)(&*state, time, &self.sources)
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
                // doesn't re-arm in 5s, drop any manual-alarm grace, and stop
                // retrying an unconfirmed manual fire.
                state.set_dismissed(&side, true);
                state.set_manual_alarm(&side, None);
                if self.pending_fire.as_ref().is_some_and(|p| p.cmd.side == side) {
                    self.pending_fire = None;
                }
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
                            + Duration::from_secs(u64::from(spec.duration_s) + 65),
                    ),
                );
                let cmd = AlarmCommand {
                    side: spec.side,
                    intensity: spec.intensity,
                    duration: spec.duration_s,
                    pattern: spec.pattern,
                };
                if !dry_run {
                    self.pending_fire = Some(PendingFire {
                        cmd: cmd.clone(),
                        last_sent: Instant::now(),
                        attempts: 1,
                    });
                }
                Some(SensorCommand::SetAlarm(cmd))
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

fn get_alarm_cmd(
    state: &SensorState,
    now: &Zoned,
    sources: &AlarmSources,
    side: &BedSide,
) -> Option<SensorCommand> {
    let alarm_running = state.get_alarm_for_side(side);
    // Away mode, a dismissal, a skip-override, or a deleted alarm block must
    // only suppress *starting* an alarm. The cancel branch below has to stay
    // reachable, or toggling away on (or removing the alarm) mid-alarm leaves
    // the bed vibrating until the firmware's max-duration timeout (issue #28).
    let due = if sources.away_mode.get(side) || state.get_dismissed(side) {
        None
    } else {
        sources.resolve(side, now)
    };

    if let Some(due) = due {
        if !alarm_running {
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
                intensity: due.intensity,
                duration: due.duration_s,
                pattern: due.pattern,
            }));
        }
    } else if alarm_running && !state.manual_alarm_active(side) {
        // Out of window, or dismissed mid-window. Cancel via an intensity-0
        // SetAlarm (ClearAlarm 0x2D is unverified and has crashed the MCU —
        // see pod-proto). Retrying here every interval matters: the G0 eats
        // one-shot writes early in a connection, so a single ClearAlarm from
        // the API path is not enough. Manually fired alarms (API test) are
        // exempt for their grace period.
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

/// Window/attribution math lives in `crate::alarm`'s own tests; here we cover
/// the actuation *decision* around it: away mode, dismissals, cancels, and the
/// weekly-vs-profile source selection.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AlarmConfig, SideConfig};
    use jiff::civil::{date, time};
    use jiff::tz::TimeZone;

    /// wake / offset / duration -> a SideConfig whose only relevant fields are
    /// the alarm window ones.
    fn side(wake: jiff::civil::Time, offset: u32, duration: u32) -> SideConfig {
        SideConfig {
            temperatures: vec![27.0],
            sleep: time(21, 0, 0, 0),
            wake,
            alarm: Some(AlarmConfig {
                pattern: AlarmPattern::Double,
                intensity: 50,
                duration,
                offset,
            }),
        }
    }

    /// Sources with the same profile alarm window (07:00 wake, 06:50..07:10)
    /// on both sides, an unowned weekly schedule, and away as given.
    fn sources(left_away: bool) -> AlarmSources {
        AlarmSources {
            away_mode: AwayMode {
                left: left_away,
                right: false,
            },
            profile: SidesConfig::Couples {
                left: side(time(7, 0, 0, 0), 600, 1200),
                right: side(time(7, 0, 0, 0), 600, 1200),
            },
            schedules: Schedules::default(),
            overrides: overrides_from(&Settings::default()),
        }
    }

    /// A fixed non-UTC zoned "now". 2026-08-17 is a Monday.
    fn at(hh: i8, mm: i8) -> Zoned {
        date(2026, 8, 17)
            .at(hh, mm, 0, 0)
            .to_zoned(TimeZone::get("America/Denver").unwrap())
            .unwrap()
    }

    fn synced_state() -> SensorState {
        SensorState {
            clock_synced: true,
            vibration_enabled: true,
            ..Default::default()
        }
    }

    fn is_cancel(cmd: Option<SensorCommand>) -> bool {
        matches!(
            cmd,
            Some(SensorCommand::SetAlarm(AlarmCommand { intensity: 0, .. }))
        )
    }

    #[test]
    fn away_suppresses_alarm_start() {
        let state = synced_state();
        let in_window = at(7, 0);
        assert!(matches!(
            get_alarm_cmd(&state, &in_window, &sources(false), &BedSide::Left),
            Some(SensorCommand::SetAlarm(AlarmCommand { intensity: 50, .. }))
        ));
        assert_eq!(
            get_alarm_cmd(&state, &in_window, &sources(true), &BedSide::Left),
            None
        );
    }

    #[test]
    fn away_still_cancels_running_alarm() {
        // Issue #28: alarm started (away off), then away toggled on
        // mid-window — the cancel must still be issued.
        let mut state = synced_state();
        state.alarm_left_running = true;
        assert!(is_cancel(get_alarm_cmd(
            &state,
            &at(7, 0),
            &sources(true),
            &BedSide::Left
        )));
    }

    #[test]
    fn missing_alarm_config_still_cancels_running_alarm() {
        let mut state = synced_state();
        state.alarm_right_running = true;
        let mut src = sources(false);
        let SidesConfig::Couples { right, .. } = &mut src.profile else {
            unreachable!()
        };
        right.alarm = None;
        assert!(is_cancel(get_alarm_cmd(
            &state,
            &at(7, 0),
            &src,
            &BedSide::Right
        )));
    }

    #[test]
    fn away_cancel_exempts_manual_alarms() {
        let mut state = synced_state();
        state.alarm_left_running = true;
        state.set_manual_alarm(
            &BedSide::Left,
            Some(std::time::Instant::now() + Duration::from_secs(60)),
        );
        assert_eq!(
            get_alarm_cmd(&state, &at(7, 0), &sources(true), &BedSide::Left),
            None
        );
    }

    #[test]
    fn out_of_window_cancels_regardless_of_away() {
        let mut state = synced_state();
        state.alarm_left_running = true;
        let out = at(12, 0);
        assert!(is_cancel(get_alarm_cmd(
            &state,
            &out,
            &sources(false),
            &BedSide::Left
        )));
        assert!(is_cancel(get_alarm_cmd(
            &state,
            &out,
            &sources(true),
            &BedSide::Left
        )));
    }

    #[test]
    fn unsynced_clock_suppresses_alarm_start() {
        let mut state = synced_state();
        state.clock_synced = false;
        assert_eq!(
            get_alarm_cmd(&state, &at(7, 0), &sources(false), &BedSide::Left),
            None
        );
    }

    #[test]
    fn owned_weekly_side_fires_from_the_weekly_alarm_not_the_profile() {
        let state = synced_state();
        let mut src = sources(false);
        // Sunday's row owns the side; its 07:00 alarm rings Monday morning.
        src.schedules.left.sunday.power.enabled = true;
        src.schedules.left.sunday.alarm.enabled = true;
        src.schedules.left.sunday.alarm.time = "07:00".to_string();
        src.schedules.left.sunday.alarm.vibration_intensity = 80;
        src.schedules.left.sunday.alarm.duration = 600;

        // Weekly alarm (intensity 80) instead of the profile's (50).
        assert!(matches!(
            get_alarm_cmd(&state, &at(7, 1), &src, &BedSide::Left),
            Some(SensorCommand::SetAlarm(AlarmCommand { intensity: 80, .. }))
        ));
        // The profile's 06:50 start must NOT fire on an owned side.
        assert_eq!(get_alarm_cmd(&state, &at(6, 51), &src, &BedSide::Left), None);
    }

    #[test]
    fn skip_override_cancels_a_ringing_alarm() {
        // The user hits "skip alarm" while it rings: the resolver drops the
        // window, so the cancel branch must fire.
        let mut state = synced_state();
        state.alarm_left_running = true;
        let mut src = sources(false);
        src.overrides[0] = AlarmOverride {
            disabled: true,
            time_override: String::new(),
            // covers the whole 06:50 window (2026-08-17 is in MDT, -06:00)
            expires_at: "2026-08-17T07:12:00-06:00".to_string(),
        };
        assert!(is_cancel(get_alarm_cmd(
            &state,
            &at(7, 0),
            &src,
            &BedSide::Left
        )));
    }
}
