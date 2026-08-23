use crate::bus::{Command, DeviceSnapshot, SideSnapshot, StatusTx};
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

/// Safe setpoint (centi-°C, 27.5 °C) for a "turn this side on" with no real
/// setpoint to carry forward.
const DEFAULT_TEMP_CENTI_C: u16 = 2750;

struct CommandTimers {
    last_wake: Instant,
    last_hwinfo: Instant,
    last_left_temp: Instant,
    last_right_temp: Instant,
    last_prime: Instant,
}

/// A manual (API/UI) setpoint that takes precedence over the config schedule
/// for one side. Without this, the scheduler re-asserts the config target
/// every [`TEMP_INT`] and any change made in the web UI reverts in seconds.
struct ManualOverride {
    target: FrozenTarget,
    /// The schedule's enabled flag when the override was taken. When it flips
    /// (sleep/wake boundary or away-mode change) the override expires and the
    /// schedule takes back over.
    config_enabled: bool,
    /// Session end for a `SetPower { on: true, duration_s }` override. When it
    /// passes, the override expires and the schedule takes back over (which
    /// turns the side off outside its window) — "on for N hours" (#31).
    expires_at: Option<Instant>,
}

#[derive(Default)]
struct ManualOverrides {
    left: Option<ManualOverride>,
    right: Option<ManualOverride>,
}

impl ManualOverrides {
    fn side_mut(&mut self, side: &BedSide) -> &mut Option<ManualOverride> {
        match side {
            BedSide::Left => &mut self.left,
            BedSide::Right => &mut self.right,
        }
    }
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
    let mut prime_enabled = cfg.prime_enabled;
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
    let mut overrides = ManualOverrides::default();

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
                prime_enabled,
                &side_config,
                &mut overrides,
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
                prime_enabled = cfg.prime_enabled;
                side_config = cfg.profile.clone();
                // New config = new intent; manual overrides don't outlive it.
                overrides = ManualOverrides::default();
            }

            // api / scheduler commands routed to the Frozen subsystem.
            Some(cmd) = cmd_rx.recv() => {
                if let Some((side, target, expires_at)) = handle_command(&mut writer, &state, dry_run, cmd).await {
                    let cfg_side = side_config.get_side(&side);
                    let config_enabled = FrozenTarget::calc_wanted(
                        &timezone,
                        away_mode,
                        &cfg_side.temperatures,
                        cfg_side.sleep,
                        cfg_side.wake,
                    )
                    .enabled;
                    match expires_at {
                        Some(at) => log::info!(
                            "Manual override on {side:?}: enabled={} temp={} (holds for {}s or until the next schedule boundary)",
                            target.enabled, target.temp, at.duration_since(Instant::now()).as_secs()
                        ),
                        None => log::info!(
                            "Manual override on {side:?}: enabled={} temp={} (holds until the next schedule boundary)",
                            target.enabled, target.temp
                        ),
                    }
                    *overrides.side_mut(&side) = Some(ManualOverride { target, config_enabled, expires_at });
                }
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
            apply_target(&mut s.left, tar);
        }
        if let Some(tar) = &state.right_target {
            apply_target(&mut s.right, tar);
        }
        s.is_priming = state.is_priming;
        s.water_level = state.water_full;
    });
}

/// Fold one side's echoed [`FrozenTarget`] into its snapshot.
///
/// A *disabled* target is the firmware's off sentinel (`temp: 0`), not a
/// setpoint: publishing it reached the UI as `targetTemperatureF: 32`, outside
/// the 55–110 the wire contract allows, and garbled the temperature dial. An
/// off side therefore keeps its last real setpoint (or stays unknown until it
/// has one).
fn apply_target(side: &mut SideSnapshot, tar: &FrozenTarget) {
    if tar.enabled {
        side.target_temp_c = Some(tar.temp as f64 / 100.0);
    }
    side.is_on = tar.enabled;
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
/// Returns the `(side, target, session expiry)` of a manual
/// SetTargetTemperature it actually sent (never in dry-run), so the caller can
/// register a [`ManualOverride`].
async fn handle_command(
    writer: &mut Writer,
    state: &FrozenState,
    dry_run: bool,
    cmd: Command,
) -> Option<(BedSide, FrozenTarget, Option<Instant>)> {
    let mut expires_at = None;
    let frame = match cmd {
        Command::SetTargetTempF { side, f } => Some(FrozenCommand::SetTargetTemperature {
            side,
            tar: FrozenTarget {
                enabled: true,
                temp: f_to_centi_c(f),
            }
            .delimiter_safe(side),
        }),
        Command::SetPower { side, on, duration_s } => {
            // Carry the last *real* setpoint forward (see [`power_on_temp`]);
            // the firmware ignores temp when disabling. The firmware has no
            // session timer, so duration_s becomes the override's expiry (#31).
            if on && duration_s > 0 {
                expires_at = Some(Instant::now() + Duration::from_secs(duration_s as u64));
            }
            let last = match side {
                BedSide::Left => state.left_target.as_ref(),
                BedSide::Right => state.right_target.as_ref(),
            };
            Some(FrozenCommand::SetTargetTemperature {
                side,
                tar: FrozenTarget {
                    enabled: on,
                    temp: power_on_temp(last),
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

    let Some(frame) = frame else { return None };

    if dry_run {
        log::warn!(
            "[dry-run] Frozen would send {frame}: {:02X?} // TODO(live-cutover)",
            frame.to_bytes()
        );
        None
    } else {
        let manual = if let FrozenCommand::SetTargetTemperature { side, tar } = &frame {
            Some((*side, tar.clone(), expires_at))
        } else {
            None
        };
        // TODO(live-cutover): live setpoint/control write to the Frozen MCU.
        // Gate behind the safety supervisor (heartbeat, setpoint clamp, faults).
        send_command(writer, frame).await;
        manual
    }
}

/// Setpoint to use when powering a side on, given the side's last known
/// target.
///
/// SAFETY: a *disabled* stored target is the firmware's off sentinel
/// (`{enabled: false, temp: 0}`) — its temp is not a setpoint. Carrying it
/// into a `SetPower { on: true }` frame drove the bed to 0 °C and a 12 h
/// manual override then held it there. Only an enabled target has a real
/// setpoint to preserve; anything else falls back to
/// [`DEFAULT_TEMP_CENTI_C`].
fn power_on_temp(last: Option<&FrozenTarget>) -> u16 {
    last.filter(|t| t.enabled)
        .map(|t| t.temp)
        .unwrap_or(DEFAULT_TEMP_CENTI_C)
}

#[allow(clippy::too_many_arguments)]
fn get_next_command(
    timers: &mut CommandTimers,
    state: &FrozenState,
    timezone: &TimeZone,
    away_mode: &bool,
    prime_time: &Time,
    prime_enabled: bool,
    side_config: &SidesConfig,
    overrides: &mut ManualOverrides,
) -> Option<FrozenCommand> {
    let now = Instant::now();

    if state.hardware_info.is_none() && now.duration_since(timers.last_hwinfo) > HWINFO_INT {
        timers.last_hwinfo = now;
        return Some(FrozenCommand::GetHardwareInfo);
    }

    // Per side: the schedule's target, unless a live manual override holds.
    let mut wanted_for = |side: BedSide| -> FrozenTarget {
        let cfg = side_config.get_side(&side);
        let config_wanted = FrozenTarget::calc_wanted(
            timezone,
            *away_mode,
            &cfg.temperatures,
            cfg.sleep,
            cfg.wake,
        )
        .delimiter_safe(side);
        resolve_target(overrides.side_mut(&side), config_wanted, side, now)
    };

    if now.duration_since(timers.last_left_temp) > TEMP_INT {
        let wanted_left = wanted_for(BedSide::Left);
        timers.last_left_temp = now;
        if state.left_target.as_ref() != Some(&wanted_left) {
            return Some(FrozenCommand::SetTargetTemperature {
                side: BedSide::Left,
                tar: wanted_left,
            });
        }
    }

    if now.duration_since(timers.last_right_temp) > TEMP_INT {
        let wanted_right = wanted_for(BedSide::Right);
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
    if should_prime(
        *away_mode,
        prime_enabled,
        now.duration_since(timers.last_prime),
        now_local,
        *prime_time,
    ) {
        timers.last_prime = now;
        return Some(FrozenCommand::Prime);
    }

    None
}

/// Whether the *scheduled daily* prime should fire now.
///
/// `prime_enabled` is the UI's "Prime daily?" toggle: with it off the bed
/// never primes on a schedule. It does not gate an explicit
/// [`Command::Prime`] (UI "Prime Now" / MQTT), which always runs.
fn should_prime(
    away_mode: bool,
    prime_enabled: bool,
    since_last_prime: Duration,
    now_local: Time,
    prime_time: Time,
) -> bool {
    prime_enabled
        && !away_mode
        // prime if we are within 30 seconds of prime time AND we havn't tried to prime in the last minute
        && since_last_prime > Duration::from_secs(60)
        && in_prime_window(now_local, prime_time)
}

/// True when `now` is within 30 s (either side) of `prime_time` on the
/// wrapping 24 h clock. A plain `duration_until(..).abs()` reads ~24 h for a
/// prime scheduled just across midnight from `now`, missing the window.
fn in_prime_window(now: Time, prime_time: Time) -> bool {
    let d = now.duration_until(prime_time).abs();
    d.min(SignedDuration::from_hours(24) - d) < SignedDuration::from_secs(30)
}

/// Effective target for a side: the manual override while it lives, else the
/// schedule's. The override expires when the schedule's enabled flag has
/// flipped since it was taken (sleep/wake boundary or away-mode change) or
/// when its session duration elapses — the schedule then takes back over.
fn resolve_target(
    slot: &mut Option<ManualOverride>,
    config_wanted: FrozenTarget,
    side: BedSide,
    now: Instant,
) -> FrozenTarget {
    if let Some(ov) = slot
        && (ov.config_enabled != config_wanted.enabled
            || ov.expires_at.is_some_and(|at| now >= at))
    {
        if ov.config_enabled != config_wanted.enabled {
            log::info!("Manual override on {side:?} expired (schedule boundary)");
        } else {
            log::info!("Manual override on {side:?} expired (session duration elapsed)");
        }
        *slot = None;
    }
    slot.as_ref()
        .map(|ov| ov.target.clone())
        .unwrap_or(config_wanted)
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

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::time;
    use pod_proto::frozen::packet::TemperatureUpdate;

    #[test]
    fn f_to_centi_c_table() {
        assert_eq!(f_to_centi_c(32), 0);
        assert_eq!(f_to_centi_c(212), 10000);
        // 81 F = 27.222.. C
        assert_eq!(f_to_centi_c(81), 2722);
        // 88 F = 31.111.. C
        assert_eq!(f_to_centi_c(88), 3111);
        // below-freezing input clamps to 0 instead of wrapping negative
        assert_eq!(f_to_centi_c(0), 0);
    }

    #[test]
    fn prime_window_plain() {
        let prime = time(15, 0, 0, 0);
        assert!(in_prime_window(time(15, 0, 0, 0), prime));
        assert!(in_prime_window(time(14, 59, 31, 0), prime));
        assert!(in_prime_window(time(15, 0, 29, 0), prime));
        assert!(!in_prime_window(time(14, 59, 30, 0), prime));
        assert!(!in_prime_window(time(15, 0, 30, 0), prime));
        assert!(!in_prime_window(time(3, 0, 0, 0), prime));
    }

    #[test]
    fn prime_window_wraps_midnight() {
        // prime at 00:00:10, now just before midnight — the raw civil diff is
        // ~24 h but the wall-clock distance is 25 s
        let prime = time(0, 0, 10, 0);
        assert!(in_prime_window(time(23, 59, 45, 0), prime));
        assert!(!in_prime_window(time(23, 59, 35, 0), prime));
        // and the mirror case: prime just before midnight, now just after
        let prime = time(23, 59, 55, 0);
        assert!(in_prime_window(time(0, 0, 10, 0), prime));
        assert!(!in_prime_window(time(0, 0, 30, 0), prime));
    }

    #[test]
    fn daily_prime_fires_when_everything_lines_up() {
        let prime = time(15, 0, 0, 0);
        assert!(should_prime(
            false,
            true,
            Duration::from_secs(3600),
            time(15, 0, 0, 0),
            prime
        ));
    }

    #[test]
    fn daily_prime_blocked_by_the_prime_enabled_flag() {
        // "Prime daily?" off in the UI => never primes on a schedule, even at
        // the configured time with everything else satisfied
        let prime = time(15, 0, 0, 0);
        assert!(!should_prime(
            false,
            false,
            Duration::from_secs(3600),
            time(15, 0, 0, 0),
            prime
        ));
    }

    #[test]
    fn daily_prime_still_blocked_by_away_mode_and_the_rate_limit() {
        let prime = time(15, 0, 0, 0);
        // away mode
        assert!(!should_prime(
            true,
            true,
            Duration::from_secs(3600),
            time(15, 0, 0, 0),
            prime
        ));
        // primed less than a minute ago
        assert!(!should_prime(
            false,
            true,
            Duration::from_secs(30),
            time(15, 0, 0, 0),
            prime
        ));
        // outside the window
        assert!(!should_prime(
            false,
            true,
            Duration::from_secs(3600),
            time(3, 0, 0, 0),
            prime
        ));
    }

    fn target(enabled: bool, temp: u16) -> FrozenTarget {
        FrozenTarget { enabled, temp }
    }

    #[test]
    fn resolve_target_no_override_uses_schedule() {
        let mut slot = None;
        let wanted = resolve_target(&mut slot, target(true, 2750), BedSide::Left, Instant::now());
        assert_eq!(wanted, target(true, 2750));
    }

    #[test]
    fn resolve_target_override_holds_while_schedule_state_unchanged() {
        let mut slot = Some(ManualOverride {
            target: target(true, 3000),
            config_enabled: true,
            expires_at: None,
        });
        // schedule still enabled (different temp) — override wins
        let wanted = resolve_target(&mut slot, target(true, 2750), BedSide::Left, Instant::now());
        assert_eq!(wanted, target(true, 3000));
        assert!(slot.is_some());
    }

    #[test]
    fn resolve_target_override_expires_at_schedule_boundary() {
        let mut slot = Some(ManualOverride {
            target: target(true, 3000),
            config_enabled: true,
            expires_at: None,
        });
        // schedule flipped to disabled (wake boundary / away mode) — override
        // expires and the schedule takes back over
        let wanted = resolve_target(&mut slot, target(false, 0), BedSide::Right, Instant::now());
        assert_eq!(wanted, target(false, 0));
        assert!(slot.is_none());
    }

    #[test]
    fn resolve_target_off_override_expires_when_schedule_enables() {
        // user turned the side off mid-day; the sleep boundary re-enables it
        let mut slot = Some(ManualOverride {
            target: target(false, 2750),
            config_enabled: false,
            expires_at: None,
        });
        let wanted = resolve_target(&mut slot, target(true, 2800), BedSide::Left, Instant::now());
        assert_eq!(wanted, target(true, 2800));
        assert!(slot.is_none());
    }

    #[test]
    fn resolve_target_override_holds_until_session_expiry() {
        // "on for N seconds" (#31): the override survives until its deadline
        // passes, then the schedule (off, here) takes back over.
        let now = Instant::now();
        let mut slot = Some(ManualOverride {
            target: target(true, 3000),
            config_enabled: false,
            expires_at: Some(now + Duration::from_secs(600)),
        });
        let wanted = resolve_target(&mut slot, target(false, 0), BedSide::Left, now);
        assert_eq!(wanted, target(true, 3000));
        assert!(slot.is_some());

        let wanted = resolve_target(
            &mut slot,
            target(false, 0),
            BedSide::Left,
            now + Duration::from_secs(601),
        );
        assert_eq!(wanted, target(false, 0));
        assert!(slot.is_none());
    }

    #[test]
    fn apply_target_publishes_only_real_setpoints() {
        let mut side = SideSnapshot::default();

        // nothing known yet + an off side => still unknown (never 0 °C, which
        // the api layer would publish as targetTemperatureF: 32)
        apply_target(&mut side, &target(false, 0));
        assert_eq!(side.target_temp_c, None);
        assert!(!side.is_on);

        // a real setpoint is published
        apply_target(&mut side, &target(true, 2722));
        assert_eq!(side.target_temp_c, Some(27.22));
        assert!(side.is_on);

        // turning the side off keeps the last real setpoint for the UI dial
        apply_target(&mut side, &target(false, 0));
        assert_eq!(side.target_temp_c, Some(27.22));
        assert!(!side.is_on);
    }

    #[test]
    fn power_on_temp_ignores_the_disabled_sentinel() {
        // A side that is off stores `{enabled: false, temp: 0}` — 0 centi-°C
        // is NOT a setpoint. "Turn on" must not drive the bed to 0 °C.
        assert_eq!(power_on_temp(Some(&target(false, 0))), 2750);
        // even a disabled target that happens to carry a stale temp is not a
        // setpoint the user asked for
        assert_eq!(power_on_temp(Some(&target(false, 1500))), 2750);
        // nothing known yet -> the safe default
        assert_eq!(power_on_temp(None), 2750);
    }

    #[test]
    fn power_on_temp_preserves_a_real_setpoint() {
        assert_eq!(power_on_temp(Some(&target(true, 3111))), 3111);
        assert_eq!(power_on_temp(Some(&target(true, 2200))), 2200);
    }

    fn temps(left: u16, right: u16) -> Option<TemperatureUpdate> {
        Some(TemperatureUpdate {
            left_temp: left,
            right_temp: right,
            heatsink_temp: 3000,
            error: 0,
            count: 0,
        })
    }

    #[test]
    fn led_state_idle_without_telemetry_or_targets() {
        assert_eq!(
            led_thermal_state(&FrozenState::default()),
            LedThermalState::Idle
        );
        let state = FrozenState {
            temp: temps(2700, 2700),
            ..FrozenState::default()
        };
        assert_eq!(led_thermal_state(&state), LedThermalState::Idle);
    }

    #[test]
    fn led_state_disabled_targets_are_ignored() {
        let state = FrozenState {
            temp: temps(2000, 2000),
            left_target: Some(target(false, 3000)),
            right_target: Some(target(false, 1000)),
            ..FrozenState::default()
        };
        assert_eq!(led_thermal_state(&state), LedThermalState::Idle);
    }

    #[test]
    fn led_state_heating_cooling_holding() {
        let mut state = FrozenState {
            temp: temps(2700, 2700),
            left_target: Some(target(true, 3000)),
            ..FrozenState::default()
        };
        assert_eq!(led_thermal_state(&state), LedThermalState::Heating);

        state.left_target = Some(target(true, 2000));
        assert_eq!(led_thermal_state(&state), LedThermalState::Cooling);

        // within the 0.5 C deadband
        state.left_target = Some(target(true, 2740));
        assert_eq!(led_thermal_state(&state), LedThermalState::Holding);
    }

    #[test]
    fn led_state_furthest_side_wins() {
        // left is 0.6 C hot (cooling), right is 3 C cold (heating) — right is
        // further from target and decides the LED
        let state = FrozenState {
            temp: temps(2760, 2700),
            left_target: Some(target(true, 2700)),
            right_target: Some(target(true, 3000)),
            ..FrozenState::default()
        };
        assert_eq!(led_thermal_state(&state), LedThermalState::Heating);
    }
}
