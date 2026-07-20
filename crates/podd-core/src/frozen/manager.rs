use crate::bus::{Command, DeviceSnapshot, StatusTx};
use crate::config::{Config, SidesConfig};
use crate::frozen::state::FrozenState;
use crate::led::{IS31FL3194Config, IS31FL3194Controller, LedPattern};
use pod_proto::codec::{CommandTrait, PacketCodec};
use pod_proto::frozen::packet::FrozenTarget;
use pod_proto::frozen::{FrozenCommand, FrozenPacket};
use pod_proto::packet::BedSide;
use pod_proto::serial::{SerialError, create_framed_port};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use jiff::{SignedDuration, Timestamp, civil::Time, tz::TimeZone};
use linux_embedded_hal::I2cdev;
use rumqttc::AsyncClient;
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, Instant, interval, sleep};
use tokio_serial::SerialStream;
use tokio_util::codec::Framed;

const HWINFO_INT: Duration = Duration::from_secs(1);
const TEMP_INT: Duration = Duration::from_secs(10);
const MAX_WAKE_ATTEMPTS: u32 = 5;

struct CommandTimers {
    last_wake: Instant,
    last_hwinfo: Instant,
    last_left_temp: Instant,
    last_right_temp: Instant,
    last_prime: Instant,
}

#[derive(Error, Debug)]
pub enum FrozenError {
    #[error("Serial: {0}")]
    Serial(#[from] SerialError),
    #[error("Failed to wake up Frozen")]
    FailedToWake,
}

type Writer = SplitSink<Framed<SerialStream, PacketCodec<FrozenPacket>>, FrozenCommand>;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    port: &str,
    baud: u32,
    mut config_rx: watch::Receiver<Config>,
    mut led: IS31FL3194Controller<I2cdev>,
    mut client: AsyncClient,
    status: StatusTx,
    mut cmd_rx: mpsc::Receiver<Command>,
    dry_run: bool,
) -> Result<(), FrozenError> {
    log::info!("Initializing Frozen Subsystem...");

    let cfg = config_rx.borrow_and_update();
    let led_idle = cfg.led.idle.get_config(cfg.led.band.clone());
    let led_holding = cfg.led.active.get_config(cfg.led.band.clone());
    let led_heating = cfg
        .led
        .heating
        .clone()
        .unwrap_or(LedPattern::SlowBreath(255, 30, 0))
        .get_config(cfg.led.band.clone());
    let led_cooling = cfg
        .led
        .cooling
        .clone()
        .unwrap_or(LedPattern::SlowBreath(0, 60, 255))
        .get_config(cfg.led.band.clone());
    set_led(&mut led, &led_idle);
    let timezone = cfg.timezone.clone();
    let mut away_mode = cfg.away_mode;
    let mut prime = cfg.prime;
    let mut side_config = cfg.profile.clone();
    drop(cfg);

    let (mut writer, mut reader) = create_framed_port::<FrozenPacket>(port, baud)?.split();

    // assume water present until firmware says otherwise
    let mut state = FrozenState {
        water_full: true,
        ..FrozenState::default()
    };
    state.publish_reset(&mut client).await;
    publish_frozen(&status, &state);

    // grab hwinfo @ boot
    send_command(&mut writer, FrozenCommand::Ping).await;
    sleep(Duration::from_millis(200)).await;
    send_command(&mut writer, FrozenCommand::GetHardwareInfo).await;

    let mut interval = interval(Duration::from_millis(20));
    let mut timers = CommandTimers::default();
    let mut was_active = false;
    let mut led_state: Option<LedThermalState> = None;
    let mut wake_attempts = 0;

    loop {
        tokio::select! {
            Some(result) = reader.next() => match result {
                Ok(packet) => {
                    state.handle_packet(&mut client, packet).await;
                    publish_frozen(&status, &state);

                    if state.is_active() != was_active {
                        if was_active {
                            log::info!("Profile ended!");
                        } else {
                            log::info!("Starting profile!");
                        }
                        was_active = !was_active;
                    }

                    // LED mirrors the real thermal state (heating / cooling /
                    // holding / idle), not just profile-active — a bed that is
                    // cooling must not show the "heating" pattern.
                    let wanted_led = led_thermal_state(&state);
                    if led_state != Some(wanted_led) {
                        log::info!("LED: {:?} -> {wanted_led:?}", led_state);
                        led_state = Some(wanted_led);
                        set_led(
                            &mut led,
                            match wanted_led {
                                LedThermalState::Idle => &led_idle,
                                LedThermalState::Heating => &led_heating,
                                LedThermalState::Cooling => &led_cooling,
                                LedThermalState::Holding => &led_holding,
                            },
                        );
                    }
                }
                Err(e) => {
                    log::error!("Packet decode error: {e}");
                }
            },

            // sends commands separated by 20ms
            // before sending any commands, wakes the device by sending ping + jump fw
            _ = interval.tick() => if let Some(cmd) = get_next_command(
                &mut timers,
                &state,
                &timezone,
                &away_mode,
                &prime,
                &side_config
            ) {
                let now = Instant::now();

                // ready to send command
                if state.is_awake() {
                    wake_attempts = 0;
                    send_command(&mut writer, cmd).await;
                }

                // keep trying to wake it up, give it 2 seconds every attempt
                else if now.duration_since(timers.last_wake) > Duration::from_secs(2) {
                    timers.last_wake = now;
                    wake_attempts += 1;

                    if wake_attempts > MAX_WAKE_ATTEMPTS {
                        break Err(FrozenError::FailedToWake)
                    }

                    if let Err(e) = writer.send(FrozenCommand::Ping).await {
                        log::error!("Failed to ping: {e}");
                    }
                    sleep(Duration::from_millis(200)).await;
                    if let Err(e) = writer.send(FrozenCommand::JumpToFirmware).await {
                        log::error!("Failed to send JumpToFirmware: {e}");
                    }
                }
            },

            Ok(_) = config_rx.changed() => {
                let cfg = config_rx.borrow();
                away_mode = cfg.away_mode;
                prime = cfg.prime;
                side_config = cfg.profile.clone();
            }

            // api / scheduler commands routed to the Frozen subsystem.
            Some(cmd) = cmd_rx.recv() => {
                handle_command(&mut writer, &state, dry_run, cmd).await;
            }
        }
    }
}

/// Publish the Frozen subsystem's live telemetry into the state watch.
/// Read-only: derives the snapshot from already-parsed state (SAFE).
fn publish_frozen(status: &StatusTx, state: &FrozenState) {
    status.send_modify(|s: &mut DeviceSnapshot| {
        if let Some(t) = &state.temp {
            s.left.current_temp_c = Some(t.left_temp as f64 / 100.0);
            s.right.current_temp_c = Some(t.right_temp as f64 / 100.0);
        }
        if let Some(tar) = &state.left_target {
            s.left.target_temp_c = Some(tar.temp as f64 / 100.0);
            s.left.is_on = tar.enabled;
        }
        if let Some(tar) = &state.right_target {
            s.right.target_temp_c = Some(tar.temp as f64 / 100.0);
            s.right.is_on = tar.enabled;
        }
        s.is_priming = state.is_priming;
        s.water_level = state.water_full;
    });
}

/// °F -> centidegrees Celsius, for building Frozen setpoint frames.
fn f_to_centi_c(f: i32) -> u16 {
    (((f as f64 - 32.0) * 5.0 / 9.0) * 100.0).round().clamp(0.0, u16::MAX as f64) as u16
}

/// Translate a bus [`Command`] into a Frozen control frame.
///
/// **The actual MCU write is gated behind `dry_run` (default true).** While
/// dry-running we log the intended frame + bytes and send nothing, so the
/// managed thermostat/scheduler stays in charge of the bed. Flipping this off is
/// the live cutover — see `TODO(live-cutover)`.
async fn handle_command(writer: &mut Writer, state: &FrozenState, dry_run: bool, cmd: Command) {
    let frame = match cmd {
        Command::SetTargetTempF { side, f } => Some(FrozenCommand::SetTargetTemperature {
            side,
            tar: FrozenTarget {
                enabled: true,
                temp: f_to_centi_c(f),
            }
            .delimiter_safe(side),
        }),
        Command::SetPower {
            side,
            on,
            duration_s: _,
        } => {
            // Preserve the last known target temp for the side; the firmware
            // ignores temp when disabling. duration_s is not yet honored.
            let last = match side {
                BedSide::Left => state.left_target.clone(),
                BedSide::Right => state.right_target.clone(),
            };
            Some(FrozenCommand::SetTargetTemperature {
                side,
                tar: FrozenTarget {
                    enabled: on,
                    temp: last.map(|t| t.temp).unwrap_or(2750),
                }
                .delimiter_safe(side),
            })
        }
        Command::Prime => Some(FrozenCommand::Prime),
        other => {
            log::warn!("Frozen subsystem received unroutable command: {other:?}");
            None
        }
    };

    let Some(frame) = frame else { return };

    if dry_run {
        log::warn!(
            "[dry-run] Frozen would send {frame}: {:02X?} // TODO(live-cutover)",
            frame.to_bytes()
        );
    } else {
        // TODO(live-cutover): live setpoint/control write to the Frozen MCU.
        // Gate behind the safety supervisor (heartbeat, setpoint clamp, faults).
        send_command(writer, frame).await;
    }
}

fn get_next_command(
    timers: &mut CommandTimers,
    state: &FrozenState,
    timezone: &TimeZone,
    away_mode: &bool,
    prime_time: &Time,
    side_config: &SidesConfig,
) -> Option<FrozenCommand> {
    let now = Instant::now();

    if state.hardware_info.is_none() && now.duration_since(timers.last_hwinfo) > HWINFO_INT {
        timers.last_hwinfo = now;
        return Some(FrozenCommand::GetHardwareInfo);
    }

    if now.duration_since(timers.last_left_temp) > TEMP_INT {
        let left_cfg = side_config.get_side(&BedSide::Left);
        let wanted_left = FrozenTarget::calc_wanted(
            timezone,
            *away_mode,
            &left_cfg.temperatures,
            left_cfg.sleep,
            left_cfg.wake,
        )
        .delimiter_safe(BedSide::Left);
        timers.last_left_temp = now;
        if state.left_target.as_ref() != Some(&wanted_left) {
            return Some(FrozenCommand::SetTargetTemperature {
                side: BedSide::Left,
                tar: wanted_left,
            });
        }
    }

    if now.duration_since(timers.last_right_temp) > TEMP_INT {
        let right_cfg = side_config.get_side(&BedSide::Right);
        let wanted_right = FrozenTarget::calc_wanted(
            timezone,
            *away_mode,
            &right_cfg.temperatures,
            right_cfg.sleep,
            right_cfg.wake,
        )
        .delimiter_safe(BedSide::Right);
        timers.last_right_temp = now;

        if state.right_target.as_ref() != Some(&wanted_right) {
            return Some(FrozenCommand::SetTargetTemperature {
                side: BedSide::Right,
                tar: wanted_right,
            });
        }
    }

    let now_local = Timestamp::now().to_zoned(timezone.clone()).time();

    // TODO verify it actually started priming
    if !away_mode
        // prime if we are within 30 seconds of prime time AND we havn't tried to prime in the last minute
        && now.duration_since(timers.last_prime) > Duration::from_secs(60)
        && now_local.duration_until(*prime_time).abs() < SignedDuration::from_secs(30)
    {
        timers.last_prime = now;
        return Some(FrozenCommand::Prime);
    }

    None
}

async fn send_command(writer: &mut Writer, cmd: FrozenCommand) {
    let name = cmd.to_string();
    log::debug!(" -> {name}");
    if let Err(e) = writer.send(cmd).await {
        log::error!("Failed to write {name}: {e}");
    }
}

/// What the bed is physically doing, for the LED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedThermalState {
    Idle,
    Heating,
    Cooling,
    Holding,
}

/// Derive the LED state from live targets + water temps. The side furthest
/// from its target wins when both are enabled; within the deadband the bed is
/// "holding" (at temperature).
fn led_thermal_state(state: &FrozenState) -> LedThermalState {
    const DEADBAND_CENTIDEG: i32 = 50; // 0.5 C
    let Some(temp) = &state.temp else {
        return LedThermalState::Idle;
    };
    let mut worst: Option<i32> = None; // target - current, furthest from 0
    for (target, current) in [
        (&state.left_target, temp.left_temp),
        (&state.right_target, temp.right_temp),
    ] {
        if let Some(t) = target
            && t.enabled
        {
            let delta = t.temp as i32 - current as i32;
            if worst.is_none_or(|w: i32| delta.abs() > w.abs()) {
                worst = Some(delta);
            }
        }
    }
    match worst {
        None => LedThermalState::Idle,
        Some(d) if d.abs() <= DEADBAND_CENTIDEG => LedThermalState::Holding,
        Some(d) if d > 0 => LedThermalState::Heating,
        Some(_) => LedThermalState::Cooling,
    }
}

fn set_led(led: &mut IS31FL3194Controller<I2cdev>, cfg: &IS31FL3194Config) {
    if let Err(e) = led.set(cfg) {
        log::error!("Failed to set LED: {e}");
    }
}

impl Default for CommandTimers {
    fn default() -> Self {
        let now = Instant::now();
        let ago = now - Duration::from_secs(60);
        Self {
            last_wake: now,
            last_hwinfo: now,
            last_left_temp: ago,
            last_right_temp: ago,
            last_prime: ago,
        }
    }
}
