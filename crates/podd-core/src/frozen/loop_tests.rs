//! End-to-end scenarios for the Frozen control loop, on the
//! [`loop_harness`](super::loop_harness). Each one asserts on what the API
//! sees (the status snapshot) *and* what the MCU was sent — the composition
//! the per-helper unit tests never covered (PR #211: the snapshot showed the
//! freeze guard's ramp step instead of the user's target, and the UI dial
//! snapped back on every change).

use pod_proto::frozen::packet::FrozenTarget;
use pod_proto::packet::BedSide::{self, Left, Right};
use tokio::time::{Duration, Instant};

use super::freeze::FreezeParams;
use super::loop_harness::{Harness, McuSpec, McuState, config, local};
use crate::bus::Command;
use crate::health::{self, Health};
use crate::schedule::{DailySchedule, PowerBlock, SideSchedule, Schedules};

const fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// °F -> the delimiter-safe centi-°C target the loop builds for it.
fn wanted(side: BedSide, f: i32) -> FrozenTarget {
    let temp = (((f as f64 - 32.0) * 5.0 / 9.0) * 100.0).round() as u16;
    FrozenTarget { enabled: true, temp }.delimiter_safe(side)
}

/// A held °C setpoint as the loop builds it from the profile.
fn held(side: BedSide, centi: u16) -> FrozenTarget {
    FrozenTarget { enabled: true, temp: centi }.delimiter_safe(side)
}

/// What the snapshot shows for a target, °C.
fn shown(t: &FrozenTarget) -> f64 {
    t.temp as f64 / 100.0
}

fn max_cooling_error() -> i32 {
    FreezeParams::default().max_cooling_error
}

/// Every setpoint frame sent to `side` since `since` (zombie-dropped ones
/// included — they were sent) obeyed the freeze guard: never more than
/// `max_cooling_error` below the water it arrived at (±3 for the delimiter
/// nudge), never below the wanted target.
fn assert_ramp_bounded(mcu: &McuState, side: BedSide, since: Instant, want: &FrozenTarget) {
    let limit = max_cooling_error() + 3;
    for r in mcu.setpoints.iter().filter(|r| r.side == side && r.at >= since) {
        if !r.tar.enabled {
            continue;
        }
        assert!(
            r.tar.temp as i32 >= r.water as i32 - limit,
            "{side:?} was sent {} with the water at {}: deeper than the freeze guard allows",
            r.tar.temp,
            r.water
        );
        assert!(
            r.tar.temp as i32 >= want.temp as i32 - 3,
            "{side:?} was sent {} below the wanted {}",
            r.tar.temp,
            want.temp
        );
    }
}

/// The ramp finished on exactly the wanted target, the water followed it,
/// and getting there took real ramp steps.
fn assert_converged(mcu: &McuState, side: BedSide, since: Instant, want: &FrozenTarget) {
    let sent: Vec<u16> = mcu
        .accepted(side)
        .iter()
        .filter(|r| r.at >= since)
        .map(|r| r.tar.temp)
        .collect();
    assert_eq!(mcu.target[side as usize].as_ref(), Some(want), "{side:?} final MCU setpoint");
    assert!(
        (mcu.water(side) as i32 - want.temp as i32).abs() <= 1,
        "{side:?} water {} never reached {}",
        mcu.water(side),
        want.temp
    );
    let mut steps = sent.clone();
    steps.dedup();
    assert!(steps.len() >= 3, "{side:?} expected a multi-step ramp, MCU got {sent:?}");
    assert!(
        steps.windows(2).all(|w| w[1] < w[0]),
        "{side:?} ramp must only step down: {sent:?}"
    );
}

/// Smoke: the loop wakes the MCU over the real framing, reads hardware info
/// once, confirms both profile setpoints and streams temperatures into the
/// snapshot — with MQTT a down broker the whole time.
#[tokio::test(start_paused = true)]
async fn wakes_the_mcu_and_holds_the_profile() {
    local(async {
        let spec = McuSpec { water: [3110, 2700], ..McuSpec::default() };
        let h = Harness::start(config([31.1, 27.0]), Schedules::default(), spec);
        h.run_for(secs(60)).await;

        let (l, r) = (held(Left, 3110), held(Right, 2700));
        {
            let mcu = h.mcu();
            assert!(mcu.firmware);
            assert_eq!(mcu.count_opcode(0x02), 1, "hardware info asked for once");
            assert_eq!(mcu.dropped_frames, 0);
            assert_eq!(mcu.target, [Some(l.clone()), Some(r.clone())]);
            // confirmed setpoints are not resent every 10 s
            assert_eq!(mcu.accepted(Left).len(), 1);
        }
        assert_eq!(h.target_c(Left), Some(shown(&l)));
        assert_eq!(h.target_c(Right), Some(shown(&r)));
        assert!(h.is_on(Left) && h.is_on(Right));
        assert!(h.status.borrow().left.current_temp_c.is_some());
        assert_eq!(h.health.borrow()[health::COVER_CONTROL].health, Health::Healthy);
    })
    .await
}

/// (a) Live 2026-10-19: the left side held 88 °F (31.1 °C water) and the
/// user asked for 78 °F. The API must show 78 °F the moment the command is
/// taken and at every tick of the ramp; the MCU must only ever get ramp
/// steps within `max_cooling_error` of the water; and the ramp must land on
/// exactly the wanted target as the water follows.
#[tokio::test(start_paused = true)]
async fn cooling_target_is_reported_while_the_guard_ramps() {
    local(async {
        let h = Harness::start(config([31.1, 31.1]), Schedules::default(), McuSpec::default());
        h.run_for(secs(30)).await;
        let hold = held(Left, 3110);
        assert_eq!(h.mcu().target[0], Some(hold.clone()));
        assert_eq!(h.target_c(Left), Some(shown(&hold)));

        let t0 = Instant::now();
        let want = wanted(Left, 78);
        h.send(Command::SetTargetTempF { side: Left, f: 78 }).await;
        assert_eq!(h.target_c(Left), Some(shown(&want)), "right after the command");
        // the guard really is ramping: the MCU holds a step, not the target
        assert!(h.mcu().target[0].as_ref().is_some_and(|t| t.temp > want.temp + 100));

        h.assert_target_held(Left, shown(&want), secs(240), Duration::from_millis(500)).await;
        h.run_for(secs(120)).await;
        assert_eq!(h.target_c(Left), Some(shown(&want)));

        let mcu = h.mcu();
        assert_ramp_bounded(&mcu, Left, t0, &want);
        assert_converged(&mcu, Left, t0, &want);
        // the other side was never touched
        assert_eq!(mcu.target[1], Some(held(Right, 3110)));
    })
    .await
}

/// Ramp `side` from a 31.1 °C hold toward 78 °F and stop 40 s in, mid-ramp.
async fn mid_ramp_to_78(h: &Harness) -> FrozenTarget {
    h.run_for(secs(30)).await;
    let want = wanted(Left, 78);
    h.send(Command::SetTargetTempF { side: Left, f: 78 }).await;
    h.run_for(secs(40)).await;
    assert!(
        h.mcu().target[0].as_ref().is_some_and(|t| t.temp > want.temp + 50),
        "premise: still mid-ramp"
    );
    want
}

/// (b) "On for N hours" on a side that is already on, mid-ramp, must keep
/// the user's target. Before #211 the power-on took the MCU's echo — the
/// ramp step — as the setpoint to carry forward, and the bed stopped there.
#[tokio::test(start_paused = true)]
async fn power_on_mid_ramp_keeps_the_wanted_target() {
    local(async {
        let h = Harness::start(config([31.1, 31.1]), Schedules::default(), McuSpec::default());
        let want = mid_ramp_to_78(&h).await;

        let t_on = Instant::now();
        h.send(Command::SetPower { side: Left, on: true, duration_s: 3600 }).await;
        assert_eq!(h.target_c(Left), Some(shown(&want)), "right after the power-on");
        h.assert_target_held(Left, shown(&want), secs(240), Duration::from_millis(500)).await;
        h.run_for(secs(120)).await;

        let mcu = h.mcu();
        assert_ramp_bounded(&mcu, Left, t_on, &want);
        assert_eq!(mcu.target[0].as_ref(), Some(&want), "must ramp on to the user's target");
        assert!((mcu.water(Left) as i32 - want.temp as i32).abs() <= 1);
    })
    .await
}

/// (b2) Power off then on mid-ramp: the dial must keep the user's target
/// while the side is off, and power-on must ramp back to it.
///
/// Regression for two bugs the harness found in #211: while off, the dial
/// showed the ramp step (28.1 °C here), and power-on drove the bed to the
/// 27.5 °C default instead of the user's 25.56 — the guard forgot the
/// setpoint once the wanted target went "off".
#[tokio::test(start_paused = true)]
async fn power_off_then_on_mid_ramp_restores_the_wanted_target() {
    local(async {
        let h = Harness::start(config([31.1, 31.1]), Schedules::default(), McuSpec::default());
        let want = mid_ramp_to_78(&h).await;

        h.send(Command::SetPower { side: Left, on: false, duration_s: 0 }).await;
        h.run_for(secs(15)).await;
        assert!(!h.is_on(Left));
        assert_eq!(h.mcu().target[0].as_ref().map(|t| t.enabled), Some(false));
        assert_eq!(h.target_c(Left), Some(shown(&want)), "an off side keeps the user's setpoint");

        let t_on = Instant::now();
        h.send(Command::SetPower { side: Left, on: true, duration_s: 0 }).await;
        h.run_for(secs(1)).await;
        assert!(h.is_on(Left));
        h.assert_target_held(Left, shown(&want), secs(240), Duration::from_millis(500)).await;
        h.run_for(secs(120)).await;

        let mcu = h.mcu();
        assert_ramp_bounded(&mcu, Left, t_on, &want);
        assert_eq!(mcu.target[0].as_ref(), Some(&want), "power-on must ramp back to the target");
    })
    .await
}


/// (c) Quick +/- presses: every one is reported exactly, the MCU only ever
/// gets bounded ramp steps, and the ramp ends on the last press.
#[tokio::test(start_paused = true)]
async fn quick_changes_are_each_reported_exactly() {
    local(async {
        let h = Harness::start(config([31.1, 31.1]), Schedules::default(), McuSpec::default());
        h.run_for(secs(30)).await;
        let t0 = Instant::now();
        for f in [80, 79, 78, 77, 76, 77] {
            h.send(Command::SetTargetTempF { side: Left, f }).await;
            let want = wanted(Left, f);
            assert_eq!(h.target_c(Left), Some(shown(&want)), "right after the {f} °F press");
            let (hold, every) = (Duration::from_millis(1500), Duration::from_millis(250));
            h.assert_target_held(Left, shown(&want), hold, every).await;
        }
        let want = wanted(Left, 77);
        h.assert_target_held(Left, shown(&want), secs(300), Duration::from_millis(500)).await;

        let mcu = h.mcu();
        // the presses went through targets down to 76 °F
        assert_ramp_bounded(&mcu, Left, t0, &wanted(Left, 76));
        assert_eq!(mcu.target[0].as_ref(), Some(&want));
        assert!((mcu.water(Left) as i32 - want.temp as i32).abs() <= 1);
    })
    .await
}

/// (d) A zombie MCU that silently drops the first setpoint frames: the loop
/// keeps resending until the firmware echoes, and the API shows the user's
/// target (not the ramp step) the moment the MCU confirms anything.
#[tokio::test(start_paused = true)]
async fn zombie_mcu_setpoints_are_resent_until_confirmed() {
    local(async {
        let spec = McuSpec { ignore_first_setpoints: 6, ..McuSpec::default() };
        let h = Harness::start(config([31.1, 31.1]), Schedules::default(), spec);
        h.wait_for("the MCU to wake", secs(30), |h| h.mcu().firmware).await;
        let t0 = Instant::now();
        let want = wanted(Left, 78);
        h.send(Command::SetTargetTempF { side: Left, f: 78 }).await;

        // wait for the first left echo
        let deadline = Instant::now() + secs(120);
        while h.mcu().target[0].is_none() {
            assert!(Instant::now() < deadline, "left setpoint never confirmed");
            h.run_for(Duration::from_millis(100)).await;
        }
        let ignored = h.mcu().setpoints.iter().filter(|r| r.side == Left && r.ignored).count();
        assert!(ignored >= 2, "premise: the zombie dropped left frames ({ignored})");
        assert_eq!(h.target_c(Left), Some(shown(&want)), "first confirmation");

        h.assert_target_held(Left, shown(&want), secs(240), Duration::from_millis(500)).await;
        h.run_for(secs(120)).await;
        let mcu = h.mcu();
        assert_ramp_bounded(&mcu, Left, t0, &want);
        assert_converged(&mcu, Left, t0, &want);
        assert_eq!(mcu.target[1], Some(held(Right, 3110)), "right confirmed too");
        drop(mcu);
        assert_eq!(h.health.borrow()[health::COVER_CONTROL].health, Health::Healthy);
    })
    .await
}

/// A weekly-schedule row covering the real clock ±2 h (UTC), every day.
fn schedule_left_at(f: i32) -> Schedules {
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).time();
    let two_h = jiff::SignedDuration::from_hours(2);
    let hhmm = |t: jiff::civil::Time| format!("{:02}:{:02}", t.hour(), t.minute());
    let row = DailySchedule {
        power: PowerBlock {
            on: hhmm(now.wrapping_sub(two_h)),
            off: hhmm(now.wrapping_add(two_h)),
            on_temperature: f,
            enabled: true,
        },
        ..DailySchedule::default()
    };
    Schedules {
        left: SideSchedule {
            sunday: row.clone(),
            monday: row.clone(),
            tuesday: row.clone(),
            wednesday: row.clone(),
            thursday: row.clone(),
            friday: row.clone(),
            saturday: row,
        },
        ..Schedules::default()
    }
}

/// (e) A schedule-driven cooling target reports the schedule's temperature
/// from the first echo on, never the ramp step — both when the loop boots
/// into it and when a Schedule-page save changes it live.
#[tokio::test(start_paused = true)]
async fn schedule_target_is_reported_not_the_ramp_step() {
    local(async {
        let t0 = Instant::now();
        let h = Harness::start(config([31.1, 31.1]), schedule_left_at(80), McuSpec::default());
        let want = wanted(Left, 80);
        h.wait_for("a left echo", secs(60), |h| h.mcu().target[0].is_some()).await;
        assert!(
            h.mcu().target[0].as_ref().is_some_and(|t| t.temp > want.temp + 50),
            "premise: the boot target is being ramped"
        );
        h.assert_target_held(Left, shown(&want), secs(120), Duration::from_millis(500)).await;

        // Schedule page saves a colder night mid-ramp.
        let t1 = Instant::now();
        h.schedules.send(schedule_left_at(76)).unwrap();
        let want = wanted(Left, 76);
        // the new wanted target reaches the snapshot on the next setpoint tick
        h.run_for(secs(11)).await;
        h.assert_target_held(Left, shown(&want), secs(300), Duration::from_millis(500)).await;
        h.run_for(secs(60)).await;

        let mcu = h.mcu();
        assert_ramp_bounded(&mcu, Left, t0, &want);
        assert_converged(&mcu, Left, t1, &want);
    })
    .await
}

/// A setpoint whose plain frame carries a `0x7E` (left 88 °F = 3111: CRC low
/// byte 0x7E) reaches the MCU nudged and confirmed — the fake MCU's parser
/// drops delimiter-carrying frames exactly like the firmware does.
#[tokio::test(start_paused = true)]
async fn delimiter_colliding_setpoint_is_nudged_and_confirmed() {
    local(async {
        let h = Harness::start(config([31.1, 31.1]), Schedules::default(), McuSpec::default());
        h.run_for(secs(30)).await;
        let want = wanted(Left, 88);
        assert_ne!(want.temp, 3111, "premise: 88 °F needs the nudge on the left");
        h.send(Command::SetTargetTempF { side: Left, f: 88 }).await;
        h.run_for(secs(30)).await;

        assert_eq!(h.target_c(Left), Some(shown(&want)));
        let mcu = h.mcu();
        assert_eq!(mcu.target[0].as_ref(), Some(&want));
        assert_eq!(mcu.dropped_frames, 0);
    })
    .await
}

/// The freeze guard holds on the very first setpoints after boot, both
/// sides scheduled deep, even when the MCU says nothing about temperatures
/// until its periodic stream starts.
///
/// The guard passes a wanted target through untouched while it has no water
/// reading yet (`FreezeGuard::effective` with `Readings::water == None`).
/// What keeps that from reaching the MCU is timing: the setpoint ticks that
/// fall due while the MCU is still asleep are spent on the wake handshake,
/// so the first real setpoint goes out ~10 s after wake — by which time the
/// stream (2 s here) has delivered a reading. This test pins that down.
#[tokio::test(start_paused = true)]
async fn first_setpoints_after_boot_are_ramped() {
    local(async {
        let spec = McuSpec { telemetry_before_awake: false, ..McuSpec::default() };
        let t0 = Instant::now();
        let mut both = schedule_left_at(76);
        both.right = both.left.clone();
        let h = Harness::start(config([31.1, 31.1]), both, spec);
        h.wait_for("a left echo", secs(60), |h| h.mcu().target[0].is_some()).await;
        h.run_for(secs(30)).await;

        let mcu = h.mcu();
        assert_ramp_bounded(&mcu, Left, t0, &wanted(Left, 76));
        assert_ramp_bounded(&mcu, Right, t0, &wanted(Right, 76));
    })
    .await
}

/// Surface control end to end (the dial is a mattress-surface target): a
/// sleeper keeps the right side's cover 3 °C above its water, as on the live
/// bed 2026-10-18/19. The loop must lower the water until the surface sits
/// within the deadband of the dial — never more than the 5 °C bound below
/// it, never faster than the freeze guard allows — while the API keeps
/// showing the dial. When the cover goes stale, the water returns to the
/// dial and the API falls back to reporting the water.
#[tokio::test(start_paused = true)]
async fn surface_loop_cools_the_water_until_the_surface_meets_the_dial() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    local(async {
        let mut cfg = config([31.1, 25.56]);
        // the max gain (time constant 15 min) keeps the simulated hours short
        cfg.surface_control.gain_per_hour = 4.0;
        let spec = McuSpec { water: [3110, 2556], ..McuSpec::default() };
        let h = Harness::start(cfg, Schedules::default(), spec);
        let dial = held(Right, 2556);
        h.run_for(secs(30)).await;
        assert_eq!(h.mcu().target[1].as_ref(), Some(&dial), "premise: on at the dial");

        let stop = Arc::new(AtomicBool::new(false));
        let feeder = h.feed_cover(Right, 3.0, stop.clone());
        let t0 = Instant::now();
        h.assert_target_held(Right, shown(&dial), secs(3600), secs(60)).await;

        let surface = h.surface_c(Right).expect("the API reports the surface");
        assert!((surface - 25.56).abs() <= 0.8, "surface {surface} should be near the dial");
        let water_target = h.mcu().target[1].clone().expect("right on");
        assert!(
            (2250..=2335).contains(&water_target.temp),
            "water target {water_target:?}: dial − ~2.5 °C (3 °C bias less the deadband)"
        );
        // bounded both ways, and the left side is left alone
        assert_ramp_bounded(&h.mcu(), Right, t0, &held(Right, 2556 - 500));
        assert_eq!(h.mcu().target[0].as_ref(), Some(&held(Left, 3110)));

        // the cover freezes: the offset decays and the water goes back to the dial
        stop.store(true, Ordering::Relaxed);
        feeder.await.unwrap();
        h.run_for(secs(3600)).await;
        assert_eq!(h.mcu().target[1].as_ref(), Some(&dial));
        assert_eq!(h.surface_c(Right), None, "stale cover: the API reports the water again");
        assert_eq!(h.target_c(Right), Some(shown(&dial)));
    })
    .await
}

/// Power off and on again with surface control holding an offset: the side
/// comes back at the dial (the offset reset), and the API shows the dial.
#[tokio::test(start_paused = true)]
async fn surface_offset_resets_across_power_off_on() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    local(async {
        let mut cfg = config([31.1, 25.56]);
        cfg.surface_control.gain_per_hour = 4.0;
        let spec = McuSpec { water: [3110, 2556], ..McuSpec::default() };
        let h = Harness::start(cfg, Schedules::default(), spec);
        let dial = held(Right, 2556);
        let _feeder = h.feed_cover(Right, 3.0, Arc::new(AtomicBool::new(false)));
        h.run_for(secs(1800)).await;
        assert!(h.mcu().target[1].as_ref().is_some_and(|t| t.temp < 2556 - 100), "premise: offset held");

        h.send(Command::SetPower { side: Right, on: false, duration_s: 0 }).await;
        h.run_for(secs(15)).await;
        assert!(!h.is_on(Right));
        assert_eq!(h.target_c(Right), Some(shown(&dial)));
        h.send(Command::SetPower { side: Right, on: true, duration_s: 0 }).await;
        h.run_for(secs(1)).await;
        assert_eq!(h.target_c(Right), Some(shown(&dial)));
        let first_on = h.mcu().setpoints.iter().rev().find(|r| r.side == Right && r.tar.enabled).cloned();
        assert_eq!(first_on.map(|r| r.tar), Some(dial), "power-on starts from the dial, not the old offset");
    })
    .await
}
