//! Surface control: close the loop on the cover thermistors.
//!
//! The Frozen MCU regulates the *water* at the hub. What a sleeper feels is
//! the mattress surface, and the two drift apart: body heat and a weak pump
//! leave the surface well above the water (2026-10-18/19, right side: water
//! held at its 25.56 °C setpoint all night while the cover zones under the
//! sleeper sat at 29–31 °C). Stock firmware controls on the water too; this
//! loop is podd's addition.
//!
//! The dial therefore sets the **surface** target. Each tick the side's
//! surface temperature (the mean of its four cover zones) is compared with
//! that target, and an offset is integrated from the error (slowly — the
//! cover's time constant is tens of minutes). The water target sent on is
//! the dial plus the offset, bounded to `[-max_cool, +max_heat]` around the
//! dial, so a bad sensor can never drive the water far from what was asked.
//!
//! Safety: the result still goes through the freeze guard (ramp, freeze
//! detection, thaw), so a deeper cooling target is approached no faster than
//! before. Cover readings that stop changing for [`STALE`] (sensor MCU hung,
//! cover unplugged) don't count: the offset then decays back toward zero.
//! A side that is off holds no offset.
//!
//! This module also remembers each side's last enabled dial setpoint
//! ([`SurfaceLoop::setpoint`]) — what the UI shows and what power-on returns
//! to — because everything downstream of it sees the adjusted target.

use pod_proto::frozen::packet::FrozenTarget;
use pod_proto::packet::BedSide;
use tokio::time::{Duration, Instant};

use crate::config::SurfaceControlConfig;

/// Cover readings unchanged for this long are treated as stale. The sensor
/// MCU reports every 4 s and the thermistors jitter by hundredths, so a
/// live cover never holds still this long.
pub const STALE: Duration = Duration::from_secs(10 * 60);
/// Without fresh readings the offset decays with this time constant.
const DECAY: Duration = Duration::from_secs(10 * 60);
/// The applied offset moves in steps of this many centi-°C, so the MCU gets
/// a new setpoint every few minutes at most, not every tick.
const STEP: i32 = 25;
/// Ticks further apart than this (MCU asleep, a stalled loop) integrate as
/// if only this much time passed.
const MAX_DT: Duration = Duration::from_secs(60);
/// The offset integrates only while the water is within this many centi-°C
/// of the target it is being driven to (dial + applied offset).
const SETTLED: i32 = 100;
/// Absolute water bounds (centi-°C) for an adjusted target, widened to
/// include the dial itself: never colder than stock's minimum level
/// (15 °C), never warmer than 40 °C, unless the dial is.
const FLOOR: i32 = 1500;
const CEILING: i32 = 4000;

/// [`SurfaceControlConfig`] in wire units, clamped so a typo can't make the
/// loop dangerous.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceParams {
    pub enabled: bool,
    /// centi-°C
    pub max_cool: i32,
    /// centi-°C
    pub max_heat: i32,
    /// Immediate offset per unit of surface error (°C per °C).
    pub kp: f64,
    /// Integral offset change per second, per centi-°C of error.
    pub gain_per_s: f64,
    /// centi-°C
    pub deadband: i32,
}

impl From<&SurfaceControlConfig> for SurfaceParams {
    fn from(c: &SurfaceControlConfig) -> Self {
        let centi = |v: f64, max: i32| ((v * 100.0).round() as i32).clamp(0, max);
        SurfaceParams {
            enabled: c.enabled,
            max_cool: centi(c.max_cool_offset_c, 800),
            max_heat: centi(c.max_heat_offset_c, 400),
            kp: c.proportional.clamp(0.0, 3.0),
            gain_per_s: c.gain_per_hour.clamp(0.0, 4.0) / 3600.0,
            deadband: centi(c.deadband_c, 300).max(10),
        }
    }
}

impl Default for SurfaceParams {
    fn default() -> Self {
        SurfaceParams::from(&SurfaceControlConfig::default())
    }
}

#[derive(Debug, Default)]
struct Side {
    /// Integral part of the offset, centi-°C (negative = water colder).
    integral: f64,
    /// The offset currently applied (proportional + integral, rounded to
    /// [`STEP`]).
    applied: i32,
    last_tick: Option<Instant>,
    /// The latest cover reading and when it last changed.
    cover: Option<([f64; 4], Instant)>,
    setpoint: Option<FrozenTarget>,
    /// The fresh surface mean (°C) as of the last tick, while enabled.
    surface: Option<f64>,
}

/// Both sides' surface loops. One instance lives in the Frozen manager.
#[derive(Debug, Default)]
pub struct SurfaceLoop {
    left: Side,
    right: Side,
}

impl SurfaceLoop {
    fn side_mut(&mut self, side: BedSide) -> &mut Side {
        match side {
            BedSide::Left => &mut self.left,
            BedSide::Right => &mut self.right,
        }
    }

    fn side(&self, side: BedSide) -> &Side {
        match side {
            BedSide::Left => &self.left,
            BedSide::Right => &self.right,
        }
    }

    /// The last dial setpoint `side` was on at — what the UI shows and what
    /// power-on returns to. Kept while the side is off; `None` until the side
    /// has been on.
    pub fn setpoint(&self, side: BedSide) -> Option<&FrozenTarget> {
        self.side(side).setpoint.as_ref()
    }

    /// The mattress surface temperature (°C) the loop last controlled on —
    /// `None` while disabled or the cover readings are stale.
    pub fn surface_c(&self, side: BedSide) -> Option<f64> {
        self.side(side).surface
    }

    /// The water offset currently applied to `side`, °C.
    pub fn offset_c(&self, side: BedSide) -> f64 {
        self.side(side).applied as f64 / 100.0
    }

    /// The water target to hand the freeze guard for `side`, given the dial
    /// target (schedule / manual override) and the side's cover zones (°C).
    /// Call it every tick that computes a setpoint — the integrator advances
    /// here.
    pub fn adjust(
        &mut self,
        side: BedSide,
        dial: FrozenTarget,
        cover: Option<[f64; 4]>,
        water: Option<u16>,
        now: Instant,
        p: &SurfaceParams,
    ) -> FrozenTarget {
        let s = self.side_mut(side);
        if dial.enabled {
            s.setpoint = Some(dial.clone());
        }
        s.cover = match (cover, s.cover) {
            (None, _) => None,
            (Some(v), Some((old, at))) if old == v => Some((old, at)),
            (Some(v), _) => Some((v, now)),
        };
        let dt = s
            .last_tick
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t).min(MAX_DT));
        s.last_tick = Some(now);

        let fresh = s
            .cover
            .filter(|(_, at)| now.saturating_duration_since(*at) <= STALE)
            .map(|(v, _)| v.iter().sum::<f64>() / v.len() as f64);
        s.surface = fresh.filter(|_| p.enabled);

        if !p.enabled || !dial.enabled {
            s.integral = 0.0;
            s.applied = 0;
            return dial;
        }

        // Anti-windup: only integrate once the water has reached the target
        // it is being driven to. During a warm-up or a ramp the cover lags
        // the water by most of an hour, and integrating that lag pinned both
        // sides at the bound on the first night.
        let settled = water.is_some_and(|w| {
            (i32::from(w) - (i32::from(dial.temp) + s.applied)).abs() <= SETTLED
        });
        // The surface error outside the deadband (soft: continuous at its
        // edge, so the proportional part never jumps), centi-°C.
        let mut proportional = 0.0;
        match fresh {
            Some(surface) => {
                let err = surface * 100.0 - f64::from(dial.temp);
                let e = err.signum() * (err.abs() - f64::from(p.deadband)).max(0.0);
                // Immediate: a surface 2 °C too warm takes ~2 °C off the
                // water at once (second night, 2026-10-20: the integral
                // alone took 40 min to move 0.5 °C while the sleeper's
                // surface climbed to 27.7 °C).
                proportional = -p.kp * e;
                if settled {
                    s.integral -= e * p.gain_per_s * dt.as_secs_f64();
                }
            }
            None => s.integral *= (-dt.as_secs_f64() / DECAY.as_secs_f64()).exp(),
        }
        let (lo, hi) = (-f64::from(p.max_cool), f64::from(p.max_heat));
        s.integral = s.integral.clamp(lo, hi);
        let offset = (proportional + s.integral).clamp(lo, hi);

        // Hysteresis: hold the applied step until the offset is clearly past
        // it, so sensor jitter at a rounding boundary can't chatter frames.
        let applied = if (offset - f64::from(s.applied)).abs() >= 0.75 * f64::from(STEP) {
            (offset / f64::from(STEP)).round() as i32 * STEP
        } else {
            s.applied
        };
        if applied != s.applied {
            log::info!(
                "Surface control [{side:?}]: water offset {:+.2}°C (dial {:.2}°C, surface {})",
                applied as f64 / 100.0,
                f64::from(dial.temp) / 100.0,
                fresh.map_or("stale".to_string(), |v| format!("{v:.2}°C")),
            );
            s.applied = applied;
        }
        if applied == 0 {
            return dial;
        }
        let d = i32::from(dial.temp);
        let temp = (d + applied).clamp(FLOOR.min(d), CEILING.max(d));
        FrozenTarget {
            enabled: true,
            temp: temp as u16,
        }
        .delimiter_safe(side)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pod_proto::packet::BedSide::{Left, Right};

    const TICK: Duration = Duration::from_secs(10);

    fn on(temp: u16) -> FrozenTarget {
        FrozenTarget { enabled: true, temp }
    }

    /// Run `ticks` ticks, nudging the cover every tick so it stays fresh;
    /// returns the last target.
    fn run(
        l: &mut SurfaceLoop,
        side: BedSide,
        dial: FrozenTarget,
        surface: f64,
        t0: Instant,
        ticks: u32,
        p: &SurfaceParams,
    ) -> FrozenTarget {
        let mut out = dial.clone();
        for i in 0..ticks {
            let j = (i % 2) as f64 * 0.01;
            // the water has reached the last target sent: settled
            out = l.adjust(side, dial.clone(), Some([surface + j; 4]), Some(out.temp), t0 + TICK * i, p);
        }
        out
    }

    #[test]
    fn a_surface_on_target_leaves_the_dial_alone() {
        let mut l = SurfaceLoop::default();
        let out = run(&mut l, Right, on(2556), 25.8, Instant::now(), 720, &SurfaceParams::default());
        assert_eq!(out, on(2556));
        assert_eq!(l.offset_c(Right), 0.0);
    }

    #[test]
    fn a_warm_surface_cools_the_water_at_once_then_trims_to_the_bound() {
        // Live 2026-10-20, right side: dial 25.56 °C, the sleeper's surface
        // at ~28.6 °C. The first tick already takes the error outside the
        // deadband off the water: 3.04 − 0.5 = 2.54 → −2.5 °C.
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        let t0 = Instant::now();
        let out = run(&mut l, Right, on(2556), 28.6, t0, 1, &p);
        assert_eq!(l.offset_c(Right), -2.5);
        assert!((2303..=2309).contains(&out.temp), "{out:?}");
        // the integral keeps trimming while the surface stays warm, up to
        // the 5 °C cooling bound and never past it
        let out = run(&mut l, Right, on(2556), 28.6, t0 + TICK, 3000, &p);
        assert_eq!(out.temp, 2056);
        assert_eq!(l.offset_c(Right), -5.0);
    }

    #[test]
    fn a_cold_surface_never_warms_the_water_by_default() {
        // First night, 2026-10-20: the cold bedtime cover wound both sides up
        // to a +2 °C warming offset before anyone got in.
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        let out = run(&mut l, Left, on(3110), 20.4, Instant::now(), 5000, &p);
        assert_eq!(l.offset_c(Left), 0.0);
        assert_eq!(out, on(3110));
    }

    #[test]
    fn a_configured_heat_bound_caps_warming() {
        let p = SurfaceParams { max_heat: 200, ..SurfaceParams::default() };
        let mut l = SurfaceLoop::default();
        let out = run(&mut l, Left, on(3110), 27.0, Instant::now(), 5000, &p);
        assert_eq!(l.offset_c(Left), 2.0);
        assert_eq!(out, on(3310).delimiter_safe(Left));
    }

    #[test]
    fn no_integration_while_the_water_is_still_getting_there() {
        // A ramp in progress: water 4 °C off its target, surface 1 °C warm.
        // The proportional part acts at once; the integral must not grow.
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        let t0 = Instant::now();
        for i in 0..720 {
            let j = (i % 2) as f64 * 0.01;
            let out = l.adjust(Right, on(2000), Some([21.0 + j; 4]), Some(2400), t0 + TICK * i, &p);
            assert_eq!(out, on(1950).delimiter_safe(Right), "tick {i}");
        }
        assert_eq!(l.offset_c(Right), -0.5);
    }

    #[test]
    fn a_stale_cover_decays_the_offset_back_to_the_dial() {
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        let t0 = Instant::now();
        run(&mut l, Right, on(2556), 30.0, t0, 2000, &p);
        assert_eq!(l.offset_c(Right), -5.0);
        // the cover freezes at one value for good
        let mut t = t0 + TICK * 2000;
        let mut out = on(0);
        for _ in 0..(6 * 60 * 3) {
            out = l.adjust(Right, on(2556), Some([30.0; 4]), Some(2556), t, &p);
            t += TICK;
        }
        assert_eq!(out, on(2556), "three hours without a fresh reading");
        assert_eq!(l.offset_c(Right), 0.0);
    }

    #[test]
    fn no_cover_at_all_means_no_offset() {
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        let t0 = Instant::now();
        for i in 0..1000 {
            assert_eq!(l.adjust(Right, on(2556), None, Some(2556), t0 + TICK * i, &p), on(2556));
        }
    }

    #[test]
    fn off_resets_the_offset_and_keeps_the_setpoint() {
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        let t0 = Instant::now();
        run(&mut l, Right, on(2556), 30.0, t0, 2000, &p);
        assert_eq!(l.offset_c(Right), -5.0);
        let off = FrozenTarget::default();
        assert_eq!(l.adjust(Right, off.clone(), Some([30.0; 4]), Some(2056), t0 + TICK * 2000, &p), off);
        assert_eq!(l.offset_c(Right), 0.0);
        assert_eq!(l.setpoint(Right), Some(&on(2556)));
        // back on: only the proportional part (4.45 − 0.5 → −4.0), the
        // integral starts over
        let out = l.adjust(Right, on(2556), Some([30.01; 4]), Some(2556), t0 + TICK * 2001, &p);
        assert_eq!(l.offset_c(Right), -4.0);
        assert_eq!(out, on(2156).delimiter_safe(Right));
    }

    #[test]
    fn disabled_passes_the_dial_through() {
        let p = SurfaceParams {
            enabled: false,
            ..SurfaceParams::default()
        };
        let mut l = SurfaceLoop::default();
        assert_eq!(run(&mut l, Right, on(2556), 30.0, Instant::now(), 2000, &p), on(2556));
    }

    #[test]
    fn a_long_gap_between_ticks_integrates_at_most_a_minute() {
        let p = SurfaceParams { kp: 0.0, ..SurfaceParams::default() };
        let mut l = SurfaceLoop::default();
        let t0 = Instant::now();
        l.adjust(Right, on(2556), Some([30.0; 4]), Some(2556), t0, &p);
        l.adjust(Right, on(2556), Some([30.01; 4]), Some(2556), t0 + Duration::from_secs(3600), &p);
        // 3.9 °C error for (capped) 60 s at 1 °C/h/°C = −0.07 °C → rounds to 0
        assert_eq!(l.offset_c(Right), 0.0);
    }

    #[test]
    fn the_absolute_floor_holds_for_a_cold_dial() {
        let p = SurfaceParams::default();
        let mut l = SurfaceLoop::default();
        // dial 16 °C, surface warm: offset wants -5, floor stops at 15 °C
        let out = run(&mut l, Right, on(1600), 25.0, Instant::now(), 5000, &p);
        assert_eq!(out.temp, 1500);
        // a dial already under the floor is never pushed further down
        let mut l = SurfaceLoop::default();
        let out = run(&mut l, Right, on(1300), 25.0, Instant::now(), 5000, &p);
        assert_eq!(out.temp, 1300);
    }

    #[test]
    fn config_typos_are_clamped() {
        let p = SurfaceParams::from(&SurfaceControlConfig {
            enabled: true,
            max_cool_offset_c: 50.0,
            max_heat_offset_c: -3.0,
            proportional: 9.0,
            gain_per_hour: 100.0,
            deadband_c: 0.0,
        });
        assert_eq!(p.max_cool, 800);
        assert_eq!(p.max_heat, 0);
        assert!((p.gain_per_s - 4.0 / 3600.0).abs() < 1e-12);
        assert_eq!(p.kp, 3.0);
        assert_eq!(p.deadband, 10);
    }
}
