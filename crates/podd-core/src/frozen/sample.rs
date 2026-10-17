//! The Frozen manager's telemetry sample: every input and decision on the
//! thermal path at one instant, for the [recorder](crate::recorder).
//!
//! Temperatures are °C. A target is a number (°C, on), `"off"`, or `null`
//! (not known yet). Per side: `water` (Frozen probe), `want` (schedule /
//! manual override), `eff` (what the freeze guard let through — what podd
//! sends), `mcu` (what the firmware echoed back as its setpoint), `tx`
//! (setpoint frames sent since the previous sample), `ovr` (a manual
//! override holds), `guard` (see [`GuardDiag`]).

use serde_json::{Value, json};
use tokio::time::{Duration, Instant};

use super::freeze::{FreezeGuard, FreezeParams, GuardDiag};
use super::state::FrozenState;
use crate::bus::DeviceSnapshot;
use pod_proto::frozen::packet::FrozenTarget;
use pod_proto::packet::BedSide;

/// Sample period while either side is on, priming or thawing.
pub const BUSY_EVERY: Duration = Duration::from_secs(10);
/// ...and while the bed is idle.
pub const IDLE_EVERY: Duration = Duration::from_secs(60);

/// What the manager tracks outside [`FrozenState`] for a sample.
pub struct Extra<'a> {
    pub freeze: &'a FreezeGuard,
    pub params: &'a FreezeParams,
    pub snapshot: &'a DeviceSnapshot,
    /// Setpoint frames sent per side (left, right) since the last sample.
    pub sends: [u32; 2],
    /// A manual override is held per side.
    pub overrides: [bool; 2],
    /// Since the Frozen manager started.
    pub uptime: Duration,
    pub clock_synced: bool,
}

pub fn period(state: &FrozenState, freeze: &FreezeGuard, p: &FreezeParams, now: Instant) -> Duration {
    let busy = state.is_active()
        || state.is_priming
        || [BedSide::Left, BedSide::Right].into_iter().any(|s| {
            let d = freeze.diag(s, now, p);
            d.thaw_left_s.is_some() || d.wanted.is_some_and(|w| w.enabled)
        });
    if busy { BUSY_EVERY } else { IDLE_EVERY }
}

fn c(v: u16) -> f64 {
    v as f64 / 100.0
}

fn tgt(t: Option<&FrozenTarget>) -> Value {
    match t {
        None => Value::Null,
        Some(t) if !t.enabled => json!("off"),
        Some(t) => json!(c(t.temp)),
    }
}

fn side(
    water: Option<u16>,
    mcu: Option<&FrozenTarget>,
    g: &GuardDiag,
    sends: u32,
    ovr: bool,
) -> Value {
    json!({
        "water": water.map(c),
        "want": tgt(g.wanted.as_ref()),
        "eff": tgt(g.effective.as_ref()),
        "mcu": tgt(mcu),
        "tx": sends,
        "ovr": ovr,
        "guard": {
            "n": g.samples,
            "min": g.window_min.map(|m| m as f64 / 100.0),
            "risen_s": g.risen_s,
            "thaw_s": g.thaw_left_s,
            "freezes": g.freeze_count,
        },
    })
}

/// Build one sample. Takes the firmware lines collected since the previous
/// sample out of `state`.
pub fn build(state: &mut FrozenState, x: &Extra, now: Instant) -> Value {
    let t = state.temp.as_ref();
    let dl = x.freeze.diag(BedSide::Left, now, x.params);
    let dr = x.freeze.diag(BedSide::Right, now, x.params);
    let snap = x.snapshot;
    let cover = snap.cover_env.as_ref().map(|e| {
        json!({
            "bed": e.bed.map(c),
            "ambient": c(e.ambient),
            "humidity": c(e.humidity),
            "mcu": c(e.microcontroller),
        })
    });
    json!({
        "up_s": x.uptime.as_secs(),
        "clock_synced": x.clock_synced,
        "mode": state.device_mode.to_string(),
        "rx": state.rx_frames,
        "heatsink": t.map(|t| c(t.heatsink_temp)),
        // the temperature frame's two trailing bytes, meaning unknown
        "tu_err": t.map(|t| t.error),
        "tu_cnt": t.map(|t| t.count),
        // 0xC1 poll: left, right, probe 4, heatsink
        "probes": state.probes.as_ref().map(|p| {
            [p.left_temp, p.right_temp, p.unknown_temp, p.heatsink_temp].map(c)
        }),
        "water_full": state.water_full,
        "priming": state.is_priming,
        "guard_on": x.params.enabled,
        "left": side(t.map(|t| t.left_temp), state.left_target.as_ref(), &dl, x.sends[0], x.overrides[0]),
        "right": side(t.map(|t| t.right_temp), state.right_target.as_ref(), &dr, x.sends[1], x.overrides[1]),
        "fw": state.fw.take(),
        "cover": cover,
        "presence": [snap.presence_left, snap.presence_right],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pod_proto::frozen::packet::TemperatureUpdate;

    #[test]
    fn a_sample_carries_inputs_decisions_and_firmware_lines() {
        let mut state = FrozenState {
            temp: Some(TemperatureUpdate {
                left_temp: 2750,
                right_temp: 3100,
                heatsink_temp: 2493,
                error: 9,
                count: 7,
            }),
            right_target: Some(FrozenTarget { enabled: true, temp: 2950 }),
            left_target: Some(FrozenTarget::default()),
            water_full: true,
            ..FrozenState::default()
        };
        state.fw.record("FW: pump[right] fast @ 12.000000V 0.400000A");
        let mut guard = FreezeGuard::default();
        let p = FreezeParams::default();
        let now = Instant::now();
        let want = FrozenTarget { enabled: true, temp: 2556 };
        let readings = super::super::freeze::Readings { water: Some(3100), heatsink: Some(2493) };
        let eff = guard.effective(BedSide::Right, want, readings, now, &p);
        assert_eq!(eff.temp, 2950, "ramp holds 1.5 °C under the water");
        let snap = DeviceSnapshot::default();
        let x = Extra {
            freeze: &guard,
            params: &p,
            snapshot: &snap,
            sends: [0, 2],
            overrides: [false, true],
            uptime: Duration::from_secs(90),
            clock_synced: true,
        };
        let v = build(&mut state, &x, now);
        assert_eq!(v["left"]["mcu"], "off");
        assert_eq!(v["left"]["want"], Value::Null);
        assert_eq!(v["right"]["water"], 31.0);
        assert_eq!(v["right"]["want"], 25.56);
        assert_eq!(v["right"]["eff"], 29.5);
        assert_eq!(v["right"]["mcu"], 29.5);
        assert_eq!(v["right"]["tx"], 2);
        assert_eq!(v["right"]["ovr"], true);
        assert_eq!(v["right"]["guard"]["n"], 1);
        assert_eq!(v["heatsink"], 24.93);
        assert_eq!(v["fw"]["pump"]["right"]["mode"], "fast");
        assert_eq!(v["fw"]["pump"]["right"]["a"], 0.4);
        assert!(state.fw.pump.is_empty(), "firmware lines are taken, not repeated");
        assert_eq!(period(&state, &guard, &p, now), BUSY_EVERY);
    }
}
