//! Water-loop freeze guard (issue #186).
//!
//! The Frozen MCU's PID drives the TEC as hard as the setpoint error asks.
//! With a target well below the water temperature the cold plate goes under
//! 0 °C and the heat exchanger ices over: water stops flowing past the plate,
//! the loop warms up from the bed and the reservoir while the MCU keeps
//! "cooling", and nothing recovers until the TEC is off long enough to thaw.
//! Seen live 2026-09-03 (right side, 78 °F target, water climbed to 91 °F
//! against a flat heatsink); Jeremy has also seen it at steady state in the
//! middle of the night. The MCU offers no power-level command, so the guard
//! works entirely on the setpoint it is allowed to send:
//!
//! * **Prevention** — a ramp limiter: the effective target never sits more
//!   than [`FreezeParams::max_cooling_error`] below the current water
//!   temperature and steps down as the water follows, bounding the PID error
//!   and with it the TEC drive. It only ever steps *down* toward the wanted
//!   target; a rising water temperature is never chased upward (that would
//!   hide a freeze).
//! * **Detection** — a frozen exchanger is cooling that is demanded (target
//!   at least [`DEMAND_MARGIN`] below the water) and *stays* lost: the water
//!   sits [`FreezeParams::detect_rise`] above the minimum seen in the last
//!   [`FreezeParams::detect_window`] for a whole [`FreezeParams::detect_hold`]
//!   without starting back down. A rise alone is not a freeze — someone
//!   getting into bed puts 1–3 °C into the loop within minutes and a working
//!   TEC then pulls it back out; the first version of this guard (60 s
//!   confirmation) took every one of those for ice, 55 times in three nights.
//!   Runs on every tick, so a freeze that develops hours into a steady hold
//!   is caught too.
//! * **Recovery** — for [`FreezeParams::thaw`] the side stays *on* with its
//!   setpoint held just above the water: the TEC stops cooling and the pump
//!   keeps circulating loop water past the plate. (Switching the side off
//!   stops the pump; the bed-side water then soaks up body heat and comes
//!   back as a warm slug on resume, which looked like the next freeze.) Then
//!   the ramp brings it back toward the wanted target from wherever the water
//!   is.
//!
//! The guard sits between the resolved wanted target (schedule + manual
//! override) and the `SetTargetTemperature` frame. Everything it returns is
//! `delimiter_safe`, and callers compare the MCU's echo against the *returned*
//! target, so the existing compare-and-resend loop keeps retrying the thaw's
//! hold frame until the firmware confirms it (actuation-safety rule).

use std::collections::VecDeque;

use pod_proto::frozen::packet::FrozenTarget;
use pod_proto::packet::BedSide;
use tokio::time::{Duration, Instant};

use crate::config::FreezeProtectionConfig;

/// Cooling counts as *demanded* only when the effective target is at least
/// this far (centi-°C) below the water. Inside the MCU's deadband the water
/// drifts either way and must not feed the detector.
const DEMAND_MARGIN: i32 = 50;
/// The ramp only steps when its floor has moved down by at least this much
/// (centi-°C), or when it reaches the wanted target — a new setpoint frame
/// every 10 s for a 0.01 °C move would be noise.
const RAMP_STEP_HYSTERESIS: i32 = 25;
/// Water this far (centi-°C) below its peak over the hold is on its way back
/// down: the TEC is winning, so it is a load transient, not ice.
const RECOVERING: i32 = 25;
/// The hold must be backed by samples reaching (nearly) all the way back —
/// the manager ticks every 10 s, so a hold whose oldest sample is younger
/// than this short of the full span has not been watched long enough.
const HOLD_SLACK: Duration = Duration::from_secs(30);
/// While thawing, the setpoint sits this far (centi-°C) above the water: no
/// cooling demand, pump running. It is re-latched only when the water has
/// drifted it outside [`THAW_HOLD_BAND`], so the thaw is a handful of frames.
const THAW_HOLD_MARGIN: i32 = 50;
const THAW_HOLD_BAND: std::ops::RangeInclusive<i32> = 25..=100;

/// Guard parameters in wire units (centi-°C, [`Duration`]), derived from the
/// `freeze_protection` config block with floors that keep a typo from turning
/// the guard into a trigger-happy or useless one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreezeParams {
    pub enabled: bool,
    /// Max effective-target depth below the water, centi-°C (prevention).
    pub max_cooling_error: i32,
    /// Lookback for the water minimum (detection).
    pub detect_window: Duration,
    /// Sustained rise above that minimum that means "frozen", centi-°C.
    pub detect_rise: i32,
    /// How long the water must stay risen, without recovering, to count.
    pub detect_hold: Duration,
    /// Cooling-paused (pump running) time after a detection.
    pub thaw: Duration,
}

impl From<&FreezeProtectionConfig> for FreezeParams {
    fn from(c: &FreezeProtectionConfig) -> Self {
        let centi = |v: f64, min: i32| ((v * 100.0).round() as i32).max(min);
        let detect_hold = Duration::from_secs(c.detect_hold_s.max(60));
        FreezeParams {
            enabled: c.enabled,
            max_cooling_error: centi(c.max_cooling_error_c, 25),
            // the low must be able to predate the hold it is compared against
            detect_window: Duration::from_secs(c.detect_window_s.max(120)).max(detect_hold * 2),
            detect_rise: centi(c.detect_rise_c, 20),
            detect_hold,
            thaw: Duration::from_secs(c.thaw_s.max(60)),
        }
    }
}

impl Default for FreezeParams {
    fn default() -> Self {
        FreezeParams::from(&FreezeProtectionConfig::default())
    }
}

/// What the guard is doing to one side, for the status snapshot / MQTT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FreezeStatus {
    /// Cooling is paused on this side to thaw a frozen exchanger.
    pub thawing: bool,
    /// Freezes detected since podd started.
    pub freeze_count: u32,
}

/// The Frozen MCU's temperatures as the guard sees them each tick (centi-°C;
/// `None` until telemetry has arrived).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Readings {
    /// This side's loop water.
    pub water: Option<u16>,
    /// The TEC heatsink, shared by both sides.
    pub heatsink: Option<u16>,
}

#[derive(Debug)]
struct Thaw {
    until: Instant,
    /// The setpoint (centi-°C) being held above the water.
    hold: Option<u16>,
}

#[derive(Debug, Default)]
struct SideGuard {
    /// `(when, water centi-°C)` while cooling has been continuously demanded.
    samples: VecDeque<(Instant, i32)>,
    /// `(wanted, effective)` handed out last tick — the ramp's memory.
    ramp: Option<(FrozenTarget, FrozenTarget)>,
    thaw: Option<Thaw>,
    freeze_count: u32,
}

/// Both sides' guards. One instance lives in the Frozen manager.
#[derive(Debug, Default)]
pub struct FreezeGuard {
    left: SideGuard,
    right: SideGuard,
}

fn off(side: BedSide) -> FrozenTarget {
    FrozenTarget::default().delimiter_safe(side)
}

impl FreezeGuard {
    fn side_mut(&mut self, side: BedSide) -> &mut SideGuard {
        match side {
            BedSide::Left => &mut self.left,
            BedSide::Right => &mut self.right,
        }
    }

    fn side(&self, side: BedSide) -> &SideGuard {
        match side {
            BedSide::Left => &self.left,
            BedSide::Right => &self.right,
        }
    }

    pub fn status(&self, side: BedSide) -> FreezeStatus {
        let g = self.side(side);
        FreezeStatus {
            thawing: g.thaw.is_some(),
            freeze_count: g.freeze_count,
        }
    }

    /// The target to actually send for `side`, given the target the schedule
    /// and manual override want, the latest temperatures and the current
    /// time. Call it every tick that computes a setpoint — detection lives in
    /// here.
    pub fn effective(
        &mut self,
        side: BedSide,
        wanted: FrozenTarget,
        readings: Readings,
        now: Instant,
        p: &FreezeParams,
    ) -> FrozenTarget {
        self.side_mut(side).effective(side, wanted, readings, now, p)
    }
}

impl SideGuard {
    fn clear_tracking(&mut self) {
        self.samples.clear();
        self.ramp = None;
    }

    fn effective(
        &mut self,
        side: BedSide,
        wanted: FrozenTarget,
        readings: Readings,
        now: Instant,
        p: &FreezeParams,
    ) -> FrozenTarget {
        let water = readings.water;
        if !p.enabled {
            self.clear_tracking();
            self.thaw = None;
            return wanted;
        }

        if let Some(thaw) = &mut self.thaw {
            if now < thaw.until {
                return thaw_target(side, &wanted, water, &mut thaw.hold);
            }
            self.thaw = None;
            log::warn!(
                "Freeze guard [{side:?}]: thaw finished; resuming (water {}, ramping back toward target {})",
                fmt_temp(water.map(i32::from)),
                fmt_target(&wanted),
            );
        }

        let (Some(w), true) = (water, wanted.enabled) else {
            // Off, or no telemetry yet: nothing to ramp, nothing to detect.
            self.clear_tracking();
            return wanted;
        };
        let w = i32::from(w);
        let wanted_temp = i32::from(wanted.temp);

        // --- prevention: ramp the target down no faster than the water follows
        let floor = (w - p.max_cooling_error).max(wanted_temp);
        let eff_temp = match &self.ramp {
            Some((prev_wanted, prev_eff)) if *prev_wanted == wanted => {
                let prev = i32::from(prev_eff.temp);
                if floor >= prev {
                    // water rose or held: never follow it upward
                    prev
                } else if floor == wanted_temp || prev - floor >= RAMP_STEP_HYSTERESIS {
                    floor
                } else {
                    prev
                }
            }
            // new wanted target: restart the ramp from the current water
            _ => floor,
        };
        let effective = if eff_temp == wanted_temp {
            wanted.clone()
        } else {
            FrozenTarget {
                enabled: true,
                temp: eff_temp as u16,
            }
            .delimiter_safe(side)
        };
        match &self.ramp {
            Some((pw, pe)) if *pw == wanted && *pe == effective => {}
            Some((pw, _)) if *pw == wanted && effective == wanted => {
                log::info!("Freeze guard [{side:?}]: ramp reached target {}", fmt_target(&wanted));
            }
            _ if effective != wanted => log::info!(
                "Freeze guard [{side:?}]: ramping toward {} — sending {} (water {})",
                fmt_target(&wanted),
                fmt_target(&effective),
                fmt_temp(Some(w)),
            ),
            _ => {}
        }
        self.ramp = Some((wanted.clone(), effective.clone()));

        // --- detection: cooling demanded, yet the water rose and stays risen
        let demanded = w - i32::from(effective.temp) >= DEMAND_MARGIN;
        if !demanded {
            self.samples.clear();
            return effective;
        }
        self.samples.push_back((now, w));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > p.detect_window)
        {
            self.samples.pop_front();
        }

        if let Some(min) = self.frozen(now, p) {
            self.freeze_count += 1;
            let mut hold = None;
            let held = thaw_target(side, &wanted, water, &mut hold);
            self.thaw = Some(Thaw {
                until: now + p.thaw,
                hold,
            });
            log::error!(
                "Freeze guard [{side:?}]: FREEZE DETECTED — water {} has sat {} above its {}-min low for {} min \
                 without recovering while the target was {} (heatsink {}). Pausing cooling for {} min to thaw, \
                 pump running, setpoint held at {} (freeze #{} since start)",
                fmt_temp(Some(w)),
                fmt_delta(w - min),
                p.detect_window.as_secs() / 60,
                p.detect_hold.as_secs() / 60,
                fmt_target(&effective),
                fmt_temp(readings.heatsink.map(i32::from)),
                p.thaw.as_secs() / 60,
                fmt_target(&held),
                self.freeze_count,
            );
            self.clear_tracking();
            return held;
        }

        effective
    }

    /// `Some(window minimum)` when the water has stayed at least
    /// `detect_rise` above the minimum seen over the window for the whole
    /// trailing `detect_hold`, and is not on its way back down.
    fn frozen(&self, now: Instant, p: &FreezeParams) -> Option<i32> {
        let min = self.samples.iter().map(|(_, t)| *t).min()?;
        let (_, latest) = *self.samples.back()?;
        let held = || {
            self.samples
                .iter()
                .filter(|(t, _)| now.duration_since(*t) <= p.detect_hold)
        };
        let (oldest, _) = held().next()?;
        let watched = now.duration_since(*oldest) + HOLD_SLACK >= p.detect_hold;
        let risen = held().all(|(_, t)| t - min >= p.detect_rise);
        let peak = held().map(|(_, t)| *t).max()?;
        let recovering = peak - latest >= RECOVERING;
        (watched && risen && !recovering).then_some(min)
    }
}

/// The target to send while thawing: the side stays on, so the pump keeps
/// circulating, with the setpoint just above the water so the TEC stops
/// cooling. A wanted target that is off stays off, and one already above the
/// hold (the user turned the heat up mid-thaw) is sent as is — neither cools.
fn thaw_target(
    side: BedSide,
    wanted: &FrozenTarget,
    water: Option<u16>,
    hold: &mut Option<u16>,
) -> FrozenTarget {
    if !wanted.enabled {
        return wanted.clone();
    }
    let Some(w) = water.map(i32::from) else {
        // nothing to hold against; off is the one setpoint sure not to cool
        return off(side);
    };
    if i32::from(wanted.temp) >= w + THAW_HOLD_MARGIN {
        return wanted.clone();
    }
    let temp = hold
        .filter(|h| THAW_HOLD_BAND.contains(&(i32::from(*h) - w)))
        .unwrap_or((w + THAW_HOLD_MARGIN) as u16);
    *hold = Some(temp);
    FrozenTarget {
        enabled: true,
        temp,
    }
    .delimiter_safe(side)
}

fn fmt_temp(centi: Option<i32>) -> String {
    match centi {
        Some(c) => format!("{:.2}°C", c as f64 / 100.0),
        None => "n/a".to_string(),
    }
}

fn fmt_delta(centi: i32) -> String {
    format!("{:.2}°C", centi as f64 / 100.0)
}

fn fmt_target(t: &FrozenTarget) -> String {
    if t.enabled {
        fmt_temp(Some(i32::from(t.temp)))
    } else {
        "off".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn on(temp: u16) -> FrozenTarget {
        FrozenTarget { enabled: true, temp }
    }

    fn water(w: u16) -> Readings {
        Readings {
            water: Some(w),
            heatsink: Some(2500),
        }
    }

    fn params() -> FreezeParams {
        FreezeParams::default()
    }

    /// Run the guard once per `step` from `t0`, feeding water temps from
    /// `temps`; returns every effective target in order.
    fn drive(
        g: &mut FreezeGuard,
        side: BedSide,
        wanted: &FrozenTarget,
        t0: Instant,
        step: Duration,
        temps: &[u16],
        p: &FreezeParams,
    ) -> Vec<FrozenTarget> {
        temps
            .iter()
            .enumerate()
            .map(|(i, w)| g.effective(side, wanted.clone(), water(*w), t0 + step * i as u32, p))
            .collect()
    }

    /// Water climbing 0.05 °C per 10 s sample (the 2026-09-03 rate) from
    /// `from`, for `n` samples.
    fn rising(from: u16, n: u16) -> Vec<u16> {
        (0..n).map(|i| from + i * 5).collect()
    }

    /// Drive `side` into a thaw (cooling toward 25.56 with the water climbing
    /// from 31.00); returns the time of the tick that detected the freeze.
    fn freeze(g: &mut FreezeGuard, side: BedSide, t0: Instant, p: &FreezeParams) -> Instant {
        for (i, w) in rising(3100, 100).into_iter().enumerate() {
            let now = t0 + S * 10 * i as u32;
            g.effective(side, on(2556), water(w), now, p);
            if g.status(side).thawing {
                return now;
            }
        }
        panic!("freeze must be detected");
    }

    #[test]
    fn params_from_config_floors_and_units() {
        let p = FreezeParams::from(&FreezeProtectionConfig::default());
        assert_eq!(p.max_cooling_error, 150);
        assert_eq!(p.detect_rise, 100);
        assert_eq!(p.detect_window, Duration::from_secs(1800));
        assert_eq!(p.detect_hold, Duration::from_secs(600));
        assert_eq!(p.thaw, Duration::from_secs(180));
        // a nonsense config can't disable detection through the numbers
        let silly = FreezeProtectionConfig {
            max_cooling_error_c: 0.0,
            detect_window_s: 1,
            detect_rise_c: 0.0,
            detect_hold_s: 0,
            thaw_s: 0,
            ..Default::default()
        };
        let p = FreezeParams::from(&silly);
        assert_eq!(p.max_cooling_error, 25);
        assert_eq!(p.detect_rise, 20);
        assert_eq!(p.detect_window, Duration::from_secs(120));
        assert_eq!(p.detect_hold, Duration::from_secs(60));
        assert_eq!(p.thaw, Duration::from_secs(60));
        // the window always reaches back past the hold it is compared against
        let short_window = FreezeProtectionConfig {
            detect_window_s: 300,
            ..Default::default()
        };
        assert_eq!(FreezeParams::from(&short_window).detect_window, Duration::from_secs(1200));
    }

    #[test]
    fn disabled_guard_passes_everything_through() {
        let p = FreezeParams {
            enabled: false,
            ..params()
        };
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        // deep cooling demand, water rising fast: still verbatim
        for i in 0..120u32 {
            let got = g.effective(BedSide::Left, on(2000), water(3000 + i as u16 * 5), now + S * 10 * i, &p);
            assert_eq!(got, on(2000));
        }
        assert_eq!(g.status(BedSide::Left), FreezeStatus::default());
    }

    #[test]
    fn heating_and_off_are_untouched() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        // heating: target above water => no ramp
        assert_eq!(g.effective(BedSide::Left, on(3111), water(2700), now, &params()), on(3111));
        // off passes through (with whatever temp it carries)
        let off_t = FrozenTarget { enabled: false, temp: 2750 };
        assert_eq!(g.effective(BedSide::Left, off_t.clone(), water(2700), now, &params()), off_t);
        // no telemetry yet: can't ramp, don't try
        assert_eq!(g.effective(BedSide::Right, on(2000), Readings::default(), now, &params()), on(2000));
    }

    #[test]
    fn ramp_limits_the_cooling_error_and_steps_down_to_target() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        let p = params();
        // water 31.00, wanted 25.56 (78 F): first frame is water - 1.5
        let e = g.effective(BedSide::Right, on(2556), water(3100), now, &p);
        assert_eq!(e.temp, 2950);
        assert!(e.enabled);
        // water falls 0.1: inside hysteresis, hold
        let e = g.effective(BedSide::Right, on(2556), water(3090), now + S * 10, &p);
        assert_eq!(e.temp, 2950);
        // water falls 0.3: step down
        let e = g.effective(BedSide::Right, on(2556), water(3070), now + S * 20, &p);
        assert_eq!(e.temp, 2920);
        // water within 1.5 of the wanted target: the wanted target itself
        let e = g.effective(BedSide::Right, on(2556), water(2700), now + S * 30, &p);
        assert_eq!(e, on(2556));
        // and it stays there as the water settles
        let e = g.effective(BedSide::Right, on(2556), water(2560), now + S * 40, &p);
        assert_eq!(e, on(2556));
    }

    #[test]
    fn ramp_never_follows_the_water_upward() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        let p = params();
        let e = g.effective(BedSide::Left, on(2000), water(3000), now, &p);
        assert_eq!(e.temp, 2850);
        // water rises: the effective target holds instead of rising with it
        let e = g.effective(BedSide::Left, on(2000), water(3200), now + S * 10, &p);
        assert_eq!(e.temp, 2850);
    }

    #[test]
    fn ramp_restarts_when_the_wanted_target_changes() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        let p = params();
        let e = g.effective(BedSide::Left, on(2000), water(2600), now, &p);
        assert_eq!(e.temp, 2450);
        // user raises the target above the water: heating, sent verbatim
        let e = g.effective(BedSide::Left, on(2800), water(2600), now + S * 10, &p);
        assert_eq!(e, on(2800));
        // user drops it again: ramp restarts from the current water
        let e = g.effective(BedSide::Left, on(1800), water(2600), now + S * 20, &p);
        assert_eq!(e.temp, 2450);
    }

    #[test]
    fn ramp_output_is_delimiter_safe() {
        // (Left, 3111) is the known 0x7E collision; a wanted target far below
        // makes the ramp want exactly 3111 when the water is 3261.
        let mut g = FreezeGuard::default();
        let e = g.effective(BedSide::Left, on(2000), water(3261), Instant::now(), &params());
        assert_ne!(e.temp, 3111);
        assert_eq!(e, on(3111).delimiter_safe(BedSide::Left));
    }

    #[test]
    fn steady_state_freeze_is_detected_and_thawed() {
        // The night case: target held for hours, then the exchanger ices and
        // the water climbs at ~0.3 C/min with the target unchanged.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let wanted = on(2556);
        // 30 samples at 10 s: at target, holding
        let held = drive(&mut g, BedSide::Right, &wanted, t0, S * 10, &[2556; 30], &p);
        assert!(held.iter().all(|e| *e == wanted));
        // then the freeze
        let t1 = t0 + S * 300;
        let temps = rising(2556, 100);
        let out = drive(&mut g, BedSide::Right, &wanted, t1, S * 10, &temps, &p);
        let first = out.iter().position(|e| *e != wanted).expect("freeze must be detected");
        // demand starts at +0.5 C (sample 10); a rise of 1.0 above that low is
        // sample 30; it has lasted the 10-minute hold by sample ~90
        assert!((85..=95).contains(&first), "detected at sample {first}");
        let st = g.status(BedSide::Right);
        assert!(st.thawing);
        assert_eq!(st.freeze_count, 1);
        // thawing: the side stays on (pump running), never asking for cooling
        for (e, w) in out[first..].iter().zip(&temps[first..]).take(18) {
            assert!(e.enabled, "the side stays on through the thaw");
            assert!(e.temp > *w, "setpoint {} must sit above the water {w}", e.temp);
            assert!(e.temp - *w <= 100, "and only just above it");
        }
        // still held just before the thaw ends
        let detected_at = t1 + S * 10 * first as u32;
        let e = g.effective(BedSide::Right, wanted.clone(), water(3000), detected_at + p.thaw - S, &p);
        assert!(e.enabled && e.temp > 3000);
        // thaw over: cooling again, ramped from wherever the water is
        let e = g.effective(BedSide::Right, wanted.clone(), water(2800), detected_at + p.thaw, &p);
        assert!(e.enabled);
        assert_eq!(e.temp, 2650);
        assert!(!g.status(BedSide::Right).thawing);
        assert_eq!(g.status(BedSide::Right).freeze_count, 1);
    }

    #[test]
    fn freeze_during_initial_cooldown_is_detected() {
        // The 2026-09-03 case: target dropped well below the water and the
        // water rose instead of falling.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let out = drive(&mut g, BedSide::Right, &on(2556), t0, S * 10, &rising(3100, 100), &p);
        let first = out.iter().position(|e| e.temp > 3100).expect("freeze must be detected");
        // ramp floor sits 1.5 below the water from sample 0, so demand is
        // immediate; 1.0 rise = sample 20, held ten minutes by sample ~80
        assert!((75..=85).contains(&first), "detected at sample {first}");
    }

    #[test]
    fn a_frozen_plateau_is_detected() {
        // The loop warms 1.5 C and then just sits there against the bed: no
        // longer rising, but not coming back either.
        let mut g = FreezeGuard::default();
        let p = params();
        let mut temps = rising(2606, 30);
        temps.extend(std::iter::repeat_n(2756, 90));
        drive(&mut g, BedSide::Left, &on(2556), Instant::now(), S * 10, &temps, &p);
        assert_eq!(g.status(BedSide::Left).freeze_count, 1);
    }

    #[test]
    fn getting_into_bed_is_not_a_freeze() {
        // What the first guard tripped on every night (2026-09-30 .. 10-02):
        // a body puts ~2 C into the loop over a few minutes, then the TEC
        // takes most of twenty minutes to pull it back to the target.
        let mut g = FreezeGuard::default();
        let p = params();
        let wanted = on(3110);
        let mut temps: Vec<u16> = (0..36).map(|i| 3110 + i * 6).collect(); // +2.1 C in 6 min
        temps.extend((0..120).map(|i| 3320 - i * 2).take_while(|t| *t >= 3110)); // -0.12 C/min
        temps.extend(std::iter::repeat_n(3110, 60));
        let out = drive(&mut g, BedSide::Left, &wanted, Instant::now(), S * 10, &temps, &p);
        assert!(out.iter().all(|e| *e == wanted), "cooling must not be interrupted");
        assert_eq!(g.status(BedSide::Left), FreezeStatus::default());
    }

    #[test]
    fn a_plateau_or_small_excursion_is_not_a_freeze() {
        // +0.4 C over ten minutes, then it plateaus.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let wanted = on(2556);
        let mut temps: Vec<u16> = (0..60).map(|i| 2556 + (i * 40 / 60)).collect();
        temps.extend(std::iter::repeat_n(2596, 120));
        let out = drive(&mut g, BedSide::Right, &wanted, t0, S * 10, &temps, &p);
        assert!(out.iter().all(|e| *e == wanted), "no thaw for a 0.4 C excursion");
        assert_eq!(g.status(BedSide::Right).freeze_count, 0);
    }

    #[test]
    fn a_target_the_tec_cannot_reach_is_not_a_freeze() {
        // Cooling flat out and levelling off 3 C short of a deep target: the
        // water never comes back *up*, so there is nothing to thaw.
        let mut g = FreezeGuard::default();
        let p = params();
        let mut temps: Vec<u16> = (0..100).map(|i| 2800 - i * 5).collect();
        temps.extend(std::iter::repeat_n(2300, 400));
        let out = drive(&mut g, BedSide::Left, &on(2000), Instant::now(), S * 10, &temps, &p);
        assert_eq!(g.status(BedSide::Left), FreezeStatus::default());
        // still cooling, the ramp parked its 1.5 C below the water
        assert_eq!(*out.last().unwrap(), on(2150));
    }

    #[test]
    fn a_single_glitched_sample_is_not_a_freeze() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let wanted = on(2000);
        // cooling demanded throughout (water 3.0 above target after the ramp)
        let mut temps = vec![2300u16; 150];
        temps[15] = 2500; // one bad reading, +2.0 C
        let out = drive(&mut g, BedSide::Left, &wanted, t0, S * 10, &temps, &p);
        assert!(out.iter().all(|e| e.enabled));
        assert_eq!(g.status(BedSide::Left).freeze_count, 0);
    }

    #[test]
    fn normal_cooldown_never_trips() {
        // Water falls monotonically toward the target: the window minimum is
        // always the newest sample, so there is no rise to see.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let temps: Vec<u16> = (0..120).map(|i| 3100 - i * 5).collect();
        let out = drive(&mut g, BedSide::Left, &on(2500), t0, S * 10, &temps, &p);
        assert!(out.iter().all(|e| e.enabled));
        assert_eq!(*out.last().unwrap(), on(2500));
        assert_eq!(g.status(BedSide::Left).freeze_count, 0);
    }

    #[test]
    fn user_raising_the_target_does_not_look_like_a_freeze() {
        // 65 F -> 78 F: the water legitimately rises 7 C, but with the target
        // above the water that is heating, not a demanded cool.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let temps: Vec<u16> = (0..120).map(|i| 1833 + i * 6).collect();
        let out = drive(&mut g, BedSide::Left, &on(2556), t0, S * 10, &temps, &p);
        assert!(out.iter().all(|e| e.enabled));
        assert_eq!(g.status(BedSide::Left).freeze_count, 0);
    }

    #[test]
    fn thaw_setpoint_follows_the_water_in_few_frames() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t = freeze(&mut g, BedSide::Left, Instant::now(), &p);
        let wanted = on(2556);
        let safe = |t: u16| on(t).delimiter_safe(BedSide::Left);
        // latched 0.5 above the water it first sees
        let e = g.effective(BedSide::Left, wanted.clone(), water(3400), t + S * 10, &p);
        assert_eq!(e, safe(3450));
        // small drift either way: same frame, nothing to resend
        let e = g.effective(BedSide::Left, wanted.clone(), water(3420), t + S * 20, &p);
        assert_eq!(e, safe(3450));
        let e = g.effective(BedSide::Left, wanted.clone(), water(3360), t + S * 30, &p);
        assert_eq!(e, safe(3450));
        // the water catches up with the setpoint: step above it again
        let e = g.effective(BedSide::Left, wanted.clone(), water(3440), t + S * 40, &p);
        assert_eq!(e, safe(3490));
        // the water drops away (meltwater): don't leave a heating demand behind
        let e = g.effective(BedSide::Left, wanted.clone(), water(3300), t + S * 50, &p);
        assert_eq!(e, safe(3350));
        // the hold is delimiter-safe like every other setpoint
        let e = g.effective(BedSide::Left, wanted.clone(), water(3061), t + S * 60, &p);
        assert_ne!(e.temp, 3111);
        assert_eq!(e, safe(3111));
        // telemetry gone: off is the one setpoint sure not to cool
        let e = g.effective(BedSide::Left, wanted, Readings::default(), t + S * 70, &p);
        assert_eq!(e, off(BedSide::Left));
    }

    #[test]
    fn thaw_holds_against_a_fresh_manual_cooling_target() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t = freeze(&mut g, BedSide::Left, Instant::now(), &p);
        // the user pokes a new cooling setpoint mid-thaw: still no cooling
        let e = g.effective(BedSide::Left, on(2200), water(3000), t + S * 10, &p);
        assert_eq!(e, on(3050));
        // a heating setpoint is no threat to the exchanger: sent as is
        let e = g.effective(BedSide::Left, on(3500), water(3000), t + S * 20, &p);
        assert_eq!(e, on(3500));
        // an explicit off from the user is off
        let user_off = FrozenTarget { enabled: false, temp: 2556 };
        let e = g.effective(BedSide::Left, user_off.clone(), water(3000), t + S * 30, &p);
        assert_eq!(e, user_off);
        assert!(g.status(BedSide::Left).thawing);
    }

    #[test]
    fn turning_the_guard_off_mid_thaw_releases_the_side() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t = freeze(&mut g, BedSide::Left, Instant::now(), &p);
        let disabled = FreezeParams {
            enabled: false,
            ..params()
        };
        let e = g.effective(BedSide::Left, on(2556), water(3000), t + S * 10, &disabled);
        assert_eq!(e, on(2556));
        assert!(!g.status(BedSide::Left).thawing);
    }

    #[test]
    fn sides_are_independent() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t = freeze(&mut g, BedSide::Right, Instant::now(), &p);
        assert!(!g.status(BedSide::Left).thawing);
        let e = g.effective(BedSide::Left, on(3111), water(3000), t + S * 10, &p);
        assert_eq!(e, on(3111));
    }
}
