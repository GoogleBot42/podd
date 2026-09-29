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
//! * **Detection** — while cooling is demanded (target at least
//!   [`DEMAND_MARGIN`] below the water) the water must not rise. A sustained
//!   rise of [`FreezeParams::detect_rise`] above the minimum seen in the last
//!   [`FreezeParams::detect_window`] is a frozen exchanger. Runs on every
//!   tick, so a freeze that develops hours into a steady hold is caught too.
//! * **Recovery** — the side is forced off for [`FreezeParams::thaw`], then
//!   the ramp brings it back toward the wanted target from wherever the water
//!   is.
//!
//! The guard sits between the resolved wanted target (schedule + manual
//! override) and the `SetTargetTemperature` frame. Everything it returns is
//! `delimiter_safe`, and callers compare the MCU's echo against the *returned*
//! target, so the existing compare-and-resend loop keeps retrying the thaw's
//! off frame until the firmware confirms it (actuation-safety rule).

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
/// A rise counts only when every sample in this trailing window is above the
/// threshold (rejects a single glitched reading).
const CONFIRM: Duration = Duration::from_secs(60);
const MIN_CONFIRM_SAMPLES: usize = 2;

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
    /// Forced-off time after a detection.
    pub thaw: Duration,
}

impl From<&FreezeProtectionConfig> for FreezeParams {
    fn from(c: &FreezeProtectionConfig) -> Self {
        let centi = |v: f64, min: i32| ((v * 100.0).round() as i32).max(min);
        FreezeParams {
            enabled: c.enabled,
            max_cooling_error: centi(c.max_cooling_error_c, 25),
            detect_window: Duration::from_secs(c.detect_window_s.max(120)),
            detect_rise: centi(c.detect_rise_c, 20),
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
    /// The side is forced off to thaw a frozen exchanger.
    pub thawing: bool,
    /// Freezes detected since podd started.
    pub freeze_count: u32,
}

#[derive(Debug, Default)]
struct SideGuard {
    /// `(when, water centi-°C)` while cooling has been continuously demanded.
    samples: VecDeque<(Instant, i32)>,
    /// `(wanted, effective)` handed out last tick — the ramp's memory.
    ramp: Option<(FrozenTarget, FrozenTarget)>,
    thaw_until: Option<Instant>,
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
            thawing: g.thaw_until.is_some(),
            freeze_count: g.freeze_count,
        }
    }

    /// The target to actually send for `side`, given the target the schedule
    /// and manual override want, the latest water temperature (centi-°C, if
    /// any has been received) and the current time. Call it every tick that
    /// computes a setpoint — detection lives in here.
    pub fn effective(
        &mut self,
        side: BedSide,
        wanted: FrozenTarget,
        water: Option<u16>,
        now: Instant,
        p: &FreezeParams,
    ) -> FrozenTarget {
        self.side_mut(side).effective(side, wanted, water, now, p)
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
        water: Option<u16>,
        now: Instant,
        p: &FreezeParams,
    ) -> FrozenTarget {
        if !p.enabled {
            self.clear_tracking();
            self.thaw_until = None;
            return wanted;
        }

        if let Some(until) = self.thaw_until {
            if now < until {
                return off(side);
            }
            self.thaw_until = None;
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

        // --- detection: water must not rise while cooling is demanded
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
            self.thaw_until = Some(now + p.thaw);
            log::error!(
                "Freeze guard [{side:?}]: FREEZE DETECTED — water {} rose {} above its {}-min low while the target was {}; \
                 heat exchanger iced over. Forcing the side OFF for {} min to thaw (freeze #{} since start)",
                fmt_temp(Some(w)),
                fmt_delta(w - min),
                p.detect_window.as_secs() / 60,
                fmt_target(&effective),
                p.thaw.as_secs() / 60,
                self.freeze_count,
            );
            self.clear_tracking();
            return off(side);
        }

        effective
    }

    /// `Some(window minimum)` when every sample of the last [`CONFIRM`] sits
    /// at least `detect_rise` above the minimum seen over the window.
    fn frozen(&self, now: Instant, p: &FreezeParams) -> Option<i32> {
        let min = self.samples.iter().map(|(_, t)| *t).min()?;
        let recent: Vec<i32> = self
            .samples
            .iter()
            .filter(|(t, _)| now.duration_since(*t) <= CONFIRM)
            .map(|(_, t)| *t)
            .collect();
        let risen = recent.len() >= MIN_CONFIRM_SAMPLES
            && recent.iter().all(|t| t - min >= p.detect_rise);
        risen.then_some(min)
    }
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

    fn params() -> FreezeParams {
        FreezeParams::default()
    }

    /// Run the guard once per `step` from `t0`, feeding water temps from
    /// `water`; returns every effective target in order.
    fn drive(
        g: &mut FreezeGuard,
        side: BedSide,
        wanted: &FrozenTarget,
        t0: Instant,
        step: Duration,
        water: &[u16],
        p: &FreezeParams,
    ) -> Vec<FrozenTarget> {
        water
            .iter()
            .enumerate()
            .map(|(i, w)| g.effective(side, wanted.clone(), Some(*w), t0 + step * i as u32, p))
            .collect()
    }

    #[test]
    fn params_from_config_floors_and_units() {
        let p = FreezeParams::from(&FreezeProtectionConfig::default());
        assert_eq!(p.max_cooling_error, 150);
        assert_eq!(p.detect_rise, 100);
        assert_eq!(p.detect_window, Duration::from_secs(1800));
        assert_eq!(p.thaw, Duration::from_secs(900));
        // a nonsense config can't disable detection through the numbers
        let silly = FreezeProtectionConfig {
            max_cooling_error_c: 0.0,
            detect_window_s: 1,
            detect_rise_c: 0.0,
            thaw_s: 0,
            ..Default::default()
        };
        let p = FreezeParams::from(&silly);
        assert_eq!(p.max_cooling_error, 25);
        assert_eq!(p.detect_rise, 20);
        assert_eq!(p.detect_window, Duration::from_secs(120));
        assert_eq!(p.thaw, Duration::from_secs(60));
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
        for i in 0..20u32 {
            let got = g.effective(BedSide::Left, on(2000), Some(3000 + i as u16 * 50), now + S * 60 * i, &p);
            assert_eq!(got, on(2000));
        }
        assert_eq!(g.status(BedSide::Left), FreezeStatus::default());
    }

    #[test]
    fn heating_and_off_are_untouched() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        // heating: target above water => no ramp
        assert_eq!(g.effective(BedSide::Left, on(3111), Some(2700), now, &params()), on(3111));
        // off passes through (with whatever temp it carries)
        let off_t = FrozenTarget { enabled: false, temp: 2750 };
        assert_eq!(g.effective(BedSide::Left, off_t.clone(), Some(2700), now, &params()), off_t);
        // no telemetry yet: can't ramp, don't try
        assert_eq!(g.effective(BedSide::Right, on(2000), None, now, &params()), on(2000));
    }

    #[test]
    fn ramp_limits_the_cooling_error_and_steps_down_to_target() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        let p = params();
        // water 31.00, wanted 25.56 (78 F): first frame is water - 1.5
        let e = g.effective(BedSide::Right, on(2556), Some(3100), now, &p);
        assert_eq!(e.temp, 2950);
        assert!(e.enabled);
        // water falls 0.1: inside hysteresis, hold
        let e = g.effective(BedSide::Right, on(2556), Some(3090), now + S * 10, &p);
        assert_eq!(e.temp, 2950);
        // water falls 0.3: step down
        let e = g.effective(BedSide::Right, on(2556), Some(3070), now + S * 20, &p);
        assert_eq!(e.temp, 2920);
        // water within 1.5 of the wanted target: the wanted target itself
        let e = g.effective(BedSide::Right, on(2556), Some(2700), now + S * 30, &p);
        assert_eq!(e, on(2556));
        // and it stays there as the water settles
        let e = g.effective(BedSide::Right, on(2556), Some(2560), now + S * 40, &p);
        assert_eq!(e, on(2556));
    }

    #[test]
    fn ramp_never_follows_the_water_upward() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        let p = params();
        let e = g.effective(BedSide::Left, on(2000), Some(3000), now, &p);
        assert_eq!(e.temp, 2850);
        // water rises: the effective target holds instead of rising with it
        let e = g.effective(BedSide::Left, on(2000), Some(3200), now + S * 10, &p);
        assert_eq!(e.temp, 2850);
    }

    #[test]
    fn ramp_restarts_when_the_wanted_target_changes() {
        let mut g = FreezeGuard::default();
        let now = Instant::now();
        let p = params();
        let e = g.effective(BedSide::Left, on(2000), Some(2600), now, &p);
        assert_eq!(e.temp, 2450);
        // user raises the target above the water: heating, sent verbatim
        let e = g.effective(BedSide::Left, on(2800), Some(2600), now + S * 10, &p);
        assert_eq!(e, on(2800));
        // user drops it again: ramp restarts from the current water
        let e = g.effective(BedSide::Left, on(1800), Some(2600), now + S * 20, &p);
        assert_eq!(e.temp, 2450);
    }

    #[test]
    fn ramp_output_is_delimiter_safe() {
        // (Left, 3111) is the known 0x7E collision; a wanted target far below
        // makes the ramp want exactly 3111 when the water is 3261.
        let mut g = FreezeGuard::default();
        let e = g.effective(BedSide::Left, on(2000), Some(3261), Instant::now(), &params());
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
        // then the freeze: +0.05 C per 10 s sample
        let t1 = t0 + S * 300;
        let rising: Vec<u16> = (0..60).map(|i| 2556 + i * 5).collect();
        let out = drive(&mut g, BedSide::Right, &wanted, t1, S * 10, &rising, &p);
        let first_off = out.iter().position(|e| !e.enabled).expect("freeze must be detected");
        // demand starts at +0.5 C (sample 10); rise of 1.0 above that low is
        // sample 30; confirmed one minute later
        assert!((30..=40).contains(&first_off), "detected at sample {first_off}");
        assert!(out[first_off..].iter().all(|e| !e.enabled), "stays off for the thaw");
        let st = g.status(BedSide::Right);
        assert!(st.thawing);
        assert_eq!(st.freeze_count, 1);
        // still off just before the thaw ends
        let detected_at = t1 + S * 10 * first_off as u32;
        let e = g.effective(BedSide::Right, wanted.clone(), Some(2400), detected_at + p.thaw - S, &p);
        assert!(!e.enabled);
        // thaw over: back on, ramped from the (now thawed, colder) water
        let e = g.effective(BedSide::Right, wanted.clone(), Some(2800), detected_at + p.thaw, &p);
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
        let rising: Vec<u16> = (0..40).map(|i| 3100 + i * 5).collect();
        let out = drive(&mut g, BedSide::Right, &on(2556), t0, S * 10, &rising, &p);
        let first_off = out.iter().position(|e| !e.enabled).expect("freeze must be detected");
        // ramp floor sits 1.5 below the water from sample 0, so demand is
        // immediate; 1.0 rise = sample 20, confirmed by sample ~26
        assert!((20..=30).contains(&first_off), "detected at sample {first_off}");
    }

    #[test]
    fn a_plateau_or_small_excursion_is_not_a_freeze() {
        // Someone gets into bed: +0.4 C over ten minutes, then it plateaus.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let wanted = on(2556);
        let mut water: Vec<u16> = (0..60).map(|i| 2556 + (i * 40 / 60)).collect();
        water.extend(std::iter::repeat_n(2596, 120));
        let out = drive(&mut g, BedSide::Right, &wanted, t0, S * 10, &water, &p);
        assert!(out.iter().all(|e| e.enabled), "no thaw for a 0.4 C excursion");
        assert_eq!(g.status(BedSide::Right).freeze_count, 0);
    }

    #[test]
    fn a_single_glitched_sample_is_not_a_freeze() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let wanted = on(2000);
        // cooling demanded throughout (water 3.0 above target after the ramp)
        let mut water = vec![2300u16; 30];
        water[15] = 2500; // one bad reading, +2.0 C
        let out = drive(&mut g, BedSide::Left, &wanted, t0, S * 10, &water, &p);
        assert!(out.iter().all(|e| e.enabled));
    }

    #[test]
    fn normal_cooldown_never_trips() {
        // Water falls monotonically toward the target: the window minimum is
        // always the newest sample, so there is no rise to see.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let water: Vec<u16> = (0..120).map(|i| 3100 - i * 5).collect();
        let out = drive(&mut g, BedSide::Left, &on(2500), t0, S * 10, &water, &p);
        assert!(out.iter().all(|e| e.enabled));
        assert_eq!(*out.last().unwrap(), on(2500));
    }

    #[test]
    fn user_raising_the_target_does_not_look_like_a_freeze() {
        // 65 F -> 78 F: the water legitimately rises 7 C, but with the target
        // above the water that is heating, not a demanded cool.
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let water: Vec<u16> = (0..120).map(|i| 1833 + i * 6).collect();
        let out = drive(&mut g, BedSide::Left, &on(2556), t0, S * 10, &water, &p);
        assert!(out.iter().all(|e| e.enabled));
        assert_eq!(g.status(BedSide::Left).freeze_count, 0);
    }

    #[test]
    fn thaw_holds_off_even_against_a_fresh_manual_target() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let rising: Vec<u16> = (0..40).map(|i| 3100 + i * 5).collect();
        drive(&mut g, BedSide::Left, &on(2556), t0, S * 10, &rising, &p);
        assert!(g.status(BedSide::Left).thawing);
        // the user pokes a new setpoint mid-thaw: still off until the thaw ends
        let e = g.effective(BedSide::Left, on(2200), Some(3000), t0 + S * 500, &p);
        assert_eq!(e, off(BedSide::Left));
        // an explicit off from the user is off too
        let user_off = FrozenTarget { enabled: false, temp: 2556 };
        let e = g.effective(BedSide::Left, user_off, Some(3000), t0 + S * 510, &p);
        assert!(!e.enabled);
    }

    #[test]
    fn turning_the_guard_off_mid_thaw_releases_the_side() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let rising: Vec<u16> = (0..40).map(|i| 3100 + i * 5).collect();
        drive(&mut g, BedSide::Left, &on(2556), t0, S * 10, &rising, &p);
        assert!(g.status(BedSide::Left).thawing);
        let disabled = FreezeParams {
            enabled: false,
            ..params()
        };
        let e = g.effective(BedSide::Left, on(2556), Some(3000), t0 + S * 500, &disabled);
        assert_eq!(e, on(2556));
        assert!(!g.status(BedSide::Left).thawing);
    }

    #[test]
    fn sides_are_independent() {
        let mut g = FreezeGuard::default();
        let p = params();
        let t0 = Instant::now();
        let rising: Vec<u16> = (0..40).map(|i| 3100 + i * 5).collect();
        drive(&mut g, BedSide::Right, &on(2556), t0, S * 10, &rising, &p);
        assert!(g.status(BedSide::Right).thawing);
        assert!(!g.status(BedSide::Left).thawing);
        let e = g.effective(BedSide::Left, on(3111), Some(3000), t0 + S * 400, &p);
        assert_eq!(e, on(3111));
    }
}
