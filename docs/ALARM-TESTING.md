# Live alarm testing (operator protocol)

**Who this is for:** an operator verifying vibration alarms on a **real, armed
Pod** (`PODD_DRY_RUN=false`) for the first time — issue #5's "make an alarm,
dismiss an alarm, set its duration", proven on hardware. Everything here was
written against the as-built engine (`crates/podd-core/src/alarm.rs`, the
sensor manager, and the Schedule-page UI); the staged order goes from least to
most machinery so a failure points at one layer.

**Safety framing:** this vibrates a bed that people sleep in. Run it in the
daytime, with everyone who shares the bed aware, testing **your own side
first**. Read the kill-switch section before starting anything.

---

## Kill switches (know these before stage 1)

Fastest first:

1. **Double-tap the mattress on the vibrating side** (the stock Eight Sleep
   gesture). podd detects it on the piezo stream and sends the stop; journal
   shows `Double tap on <Side> piezo: dismissing alarm`.
2. **UI dismissal** — the Control Temp page shows a dismissal dialog while a
   side reports `isAlarmVibrating`; dismissing PATCHes it false and podd sends
   the stop (and keeps re-sending an intensity-0 `SetAlarm` every scheduler
   interval while the firmware still reports the alarm running).
3. **Wait it out.** The MCU runs the alarm to its `duration` on its own. Every
   stage below uses short durations (10–60 s) precisely so that "do nothing"
   is always a safe exit.

Two **non**-kill-switches, so nobody reaches for them under stress:

- `systemctl stop podd` does **not** stop a vibrating alarm — the vibration
  runs in MCU firmware until its duration ends. Worse, a restart puts the
  sensor MCU in its ~60 s zombie window (ignores actuation writes), so
  restarting *delays* your ability to send a stop. Short durations are the
  backstop, not systemd.
- Unplugging the Pod obviously works but is never needed at these durations,
  and costs you the NTP-sync gate on the way back up.

---

## Preconditions (all stages)

- [ ] podd has been up **≥ 2 minutes** (past the sensor MCU's post-restart
      zombie window; startup hygiene stops have gone out).
- [ ] Clock is NTP-synced: journal shows
      `System clock is NTP-synced; scheduled alarms armed` (scheduled alarms
      are held until then — no RTC battery).
- [ ] `GET /api/serverStatus` healthy, sensor subsystem up (no active
      dropout-recover cycle in the journal in the last minute).
- [ ] A journal follow is running in a terminal you can see from the bed:
      `journalctl -u podd -f`.
- [ ] You know which physical side is which in the UI (left/right).
- [ ] Away mode is **off** for the side under test (away suppresses alarm
      starts).

---

## Stage 1 — dry-run rehearsal (first time on a unit)

Goal: prove the resolution + command path end-to-end with actuation logged
instead of sent. Skip only if this unit has already had a live alarm fire
under the current podd build.

1. Set dry-run (`PODD_DRY_RUN=true`, or remove the arming drop-in), restart
   podd, wait out the preconditions.
2. Run stage 2's manual test. Expect
   `[dry-run] Sensor would send SetAlarm... {bytes}` in the journal and **no
   vibration**. The pending-fire retry is bypassed in dry-run — one log line
   is success.
3. Run stage 4's scheduled-alarm setup with a time a few minutes out. At the
   scheduled time expect `Alarm[<Side>] requesting to start` followed by a
   `[dry-run] ... would send SetAlarm` line (re-logged each scheduler
   interval for the whole window).
4. Disable the test alarm again, re-arm (`PODD_DRY_RUN=false`), restart, wait
   out the preconditions before stage 2.

Each restart costs a zombie window and a possible sensor-MCU dropout cycle —
plan the rehearsal and the live run as two sittings, not a rapid toggle.

---

## Stage 2 — manual test alarm, 10 s (armed)

Smallest live step: one side, fixed short duration, no scheduling involved.

**UI path (preferred):** Schedule page → pick your side → open a day's alarm
accordion → **Test alarm**. It fires the accordion's intensity/pattern at a
hardcoded 10 s duration, behind a confirm dialog.

**API path (equivalent):**

```sh
curl -s -X POST http://<pod-ip>:3000/api/alarm \
  -H 'Content-Type: application/json' \
  -d '{"side":"right","vibrationIntensity":50,"vibrationPattern":"double","duration":10,"force":true}'
```

Verify:

- [ ] Vibration on the chosen side, roughly matching the intensity.
- [ ] It stops on its own at ~10 s.
- [ ] `GET /api/deviceStatus` showed `isAlarmVibrating: true` for that side
      during the run, false after.
- [ ] Journal: if the firmware confirmed on the first write, there is nothing
      to see; `FireAlarm[<side>] unconfirmed; resending (attempt N)` lines
      mean the retry-until-confirmed path is doing its job (fine unless it
      gives up: `no FW confirmation after 30 sends` + a sensor health
      `Failed` — that's a real actuation failure, stop and diagnose).

Manual fires are delimiter-safe (duration nudged ±3 s if a `0x7E` would land
in the frame) — don't be surprised by a 10 s test running 7–13 s.

---

## Stage 3 — dismissals (armed, 45 s manual alarm)

The 10 s test window is too short to fumble a gesture; use a 45 s manual fire
per attempt (API call above with `"duration":45`).

**3a — double-tap:** fire, let it vibrate a few seconds, double-tap the
mattress surface on that side (two distinct taps, between 0.2 s and 1.5 s
apart — an unhurried knock-knock). Verify:

- [ ] Vibration stops within ~a second.
- [ ] Journal: `Double tap on <Side> piezo: dismissing alarm`.
- [ ] No re-fire afterwards.

Tap detection is deliberately conservative (a false dismissal is worse than a
missed one) — if a double-tap doesn't take, tap again, harder and crisper.
Record how many attempts it took; that's tuning data for the detector.

**3b — UI dismissal:** fire again (45 s), open the Control Temp page, use the
alarm-dismissal dialog. Verify vibration stops and `isAlarmVibrating` clears.

---

## Stage 4 — scheduled alarm end-to-end (armed)

The real thing: a `schedules.json` alarm resolved by the engine, fired by the
scheduler, dismissed by double-tap.

**Day-attribution gotcha (matches the UI):** an alarm time **before noon**
belongs to the morning *after* its weekday row — Monday's row with `07:00`
rings Tuesday 07:00. At noon or later it rings on the row's own day. So when
scheduling a test N minutes from now:

- Testing in the **afternoon/evening** (now ≥ 12:00): put the alarm on
  **today's** tab at a time a few minutes out.
- Testing in the **morning** (target time < 12:00): the ringing morning
  belongs to **yesterday's** row — edit yesterday's tab. (Editing today's tab
  with `09:40` schedules *tomorrow* 09:40.)

Steps:

1. On the Schedule page (your side, correct day tab per above): set the alarm
   time ~5 min out, intensity ~50, duration **60 s**, enable it, save. The
   day's `power` must be enabled — a powered-off day never rings.
2. Confirm no `scheduleOverrides.alarm` skip/move is pending for the side
   (Control Temp page alarm banner).
3. Wait. At the scheduled minute verify:
   - [ ] Journal: `Alarm[<Side>] requesting to start`.
   - [ ] Vibration starts (scheduled sends are delimiter-safe too — start may
         be nudged up to ±3 s).
   - [ ] `isAlarmVibrating: true`; if MQTT/HA is wired, the alarm state
         reaches Home Assistant.
4. Double-tap to dismiss mid-window. Verify it stops **and stays stopped**
   for the rest of the 60 s window (the dismissal latches until the window
   ends; the scheduler must not re-arm it 5 s later).
5. **Afterwards, immediately disable the test alarm** on the Schedule page
   and save. Verify the journal after the next schedules reload shows no
   surprises (in particular, no
   `profile alarm is IGNORED` shadowed-alarm warning appearing/regressing).

**Optional 4b — overrides:** repeat with a second scheduled alarm and
exercise the Control Temp page dialogs: *skip next* (window passes silently),
then *move* to a few minutes out (rings at the moved time, not the base
time). Each override is one-shot and expires two minutes past its target.

---

## Wrap-up

- Leave the unit with every test alarm `enabled: false` (or deleted) and
  verify `GET /api/schedules` agrees.
- Note results — including duration-accuracy observations (issue #36 tracks
  the unverified duration units; a timed stage-2/stage-4 run is exactly the
  evidence it needs) and tap-detection attempt counts — on issue #5.
