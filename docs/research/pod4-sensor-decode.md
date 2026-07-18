# Pod 4 Sensor MCU Packet Payload Decode

Date: 2026-07-17. Live Pod 4, sensor MCU on `/dev/ttymxc0` @ 921600, firmware mode.
Transport `0x7E|LEN|payload|CRC16-CCITT(0x1D0F over payload, BE)` — confirmed working.
This report decodes the **payloads** of the sensor opcodes.

## Datasets
- `backup/captures/cap_ttymxc0_921600_10s.bin` (80 KB) → **342 CRC-valid frames**.
- `backup/captures/cap_ttymxc0_921600.bin` (16 KB) → 68 CRC-valid frames (cross-check).
- `firmware-sensor-g0.bbin` (STM32G0, strip 128-byte header) strings.

## Opcode census (10 s capture)
| opcode | frames | len (all fixed) | cadence | meaning |
|---|---|---|---|---|
| 0x07 | 4 | 38/60/69 | sporadic | ASCII debug (ambient temp/humidity, cap timing) |
| 0x33 | 20 | 27 | every 500 ms (2 Hz) | Capacitance presence, 6 ch — **Pod-3 layout** |
| 0x34 | 132 | 214 | every 50 ms (20 Hz) | **Piezo**, 2 ch (L/R), 500 Hz |
| 0x35 | 186 | 176 | every 50 ms (20 Hz) | 4-ch auxiliary, 200 Hz — physical id UNKNOWN |

The three streamed opcodes share **one free-running counter** (all in the 0x000CExxx
range in the same capture). That counter is **device uptime in milliseconds**
(CONFIRMED: 0x07 ASCII logs print the same counter as decimal ms, e.g. `FW: 850118
[ambient] ...`). 0x33 counter += 500/frame, 0x34 & 0x35 += 50/frame → 0x33 at 2 Hz,
0x34/0x35 at 20 Hz. This exactly matches the frame counts (20 : 132 : 186 ≈ 1 : 10 : 10).

---

## 0x33 — Capacitance presence (6 channels)
**Identical to the opensleep Pod-3 layout; the existing pod-proto parser already
decodes it.** Real frame:
`33 | 00 0C E8 8C | 00 00 00 00 | 00 043D | 01 04FF | 02 0691 | 03 064E | 04 0533 | 05 038D`

| off | type | field | value | conf |
|---|---|---|---|---|
| 0 | u8 | opcode 0x33 | — | CONFIRMED |
| 1..5 | u32 BE | sequence = **uptime ms** | 0x000CE88C = 845964 | CONFIRMED |
| 5..9 | u32 | reserved (always 0) | 0 | INFERRED |
| 9,12,15,18,21,24 | u8 | channel index 0..5 | validated | CONFIRMED |
| 10,13,16,19,22,25 (+1) | u16 BE | channel value | [1085,1279,1681,1614,1331,909] | CONFIRMED (unoccupied baseline) |

Which physical pad each of the 6 channels is (left vs right, head/torso/foot) = **UNKNOWN**
(need a presence-labeled capture). Firmware splits sampling into `cap_samplingL` /
`cap_samplingR` (two FDC1004 chips) → likely 3 left + 3 right.

## 0x34 — Piezo (dual channel, ADS ADC)
Header 14 bytes, then interleaved `left,right` big-endian **i32** samples (a 24-bit ADS
value sign-extended — high half `0xFFF9` in the unoccupied capture). 25 pairs → 25/ch.
Real header: `34 41 | 0000 01F4 | 000C E803 | 0000 0190 | FFF9 6ED0 FFF9 2AB3 ...`

| off | type | field | value | conf |
|---|---|---|---|---|
| 0 | u8 | opcode 0x34 | — | CONFIRMED |
| 1 | u8 | subtype/format | 0x41 (constant) | CONFIRMED constant, meaning INFERRED |
| 2..6 | u32 BE | **freq (Hz)** | 500 | CONFIRMED (25 samp / 50 ms) |
| 6..10 | u32 BE | **timestamp_ms** | 0x000CE803 = 845827 | CONFIRMED |
| 10..14 | u32 BE | **gain** | 0x00000190 = **400** | CONFIRMED (== telemetry gainL/R) |
| 14.. | i32 BE ×N | left,right interleaved | left[0]=-430384, right[0]=-447821 | CONFIRMED |

- Both channels vary **smoothly and continuously across frame boundaries** (verified:
  frame0 left ends -430492, frame1 left starts -430235) → real piezo waveform (residual
  bed vibration; both sides unoccupied so amplitude is small).
- **gain** ambiguity: bytes are `00 00 01 90`. Exposed as `u32`=400. Could be two u16
  `(gainL=0, gainR=400)`, but telemetry says both gains are 400, so the single-u32=400
  reading is more consistent. Marked in code.
- Channel→side mapping (is ch0 left or right?) = **UNKNOWN** without a labeled capture.

## 0x35 — 4-channel auxiliary stream
Header 16 bytes, then 4 interleaved big-endian **i32** channels. 10 samples/ch at 200 Hz.
Real header: `35 02 | 00 00 00 00 00 00 00 00 | 00C8 | 000C E7EF | 0000 8000 0000 7FFF ...`

| off | type | field | value | conf |
|---|---|---|---|---|
| 0 | u8 | opcode 0x35 | — | CONFIRMED |
| 1 | u8 | subtype/format | 0x02 (constant) | CONFIRMED constant, meaning INFERRED |
| 2..10 | 8×u8 | reserved (all zero) | 0 | CONFIRMED zero |
| 10..12 | u16 BE | **rate (Hz)** | 0x00C8 = 200 | CONFIRMED (10 samp / 50 ms) |
| 12..16 | u32 BE | **timestamp_ms** | 0x000CE7EF = 845807 | CONFIRMED |
| 16.. | i32 BE ×N | 4 ch interleaved | ch0/1≈0x8000, ch2/3≈0x03xx | CONFIRMED bytes, meaning UNKNOWN |

- ch0/ch1 hover at ~`0x7FFF/0x8000` (midscale/bias of a 16-bit-scaled signal), ch2/ch3
  near `0x03xx` (830/862) rising slowly. All four evolve smoothly = real sensor data.
- **Physical identity UNKNOWN.** Firmware sensor suite present: `ADS` (piezo, →0x34),
  `FDC1004` (4× `meas0..3`, →0x33), `SHT40` (temp/RH, →0x07 ASCII), `LIS` 3-axis accel,
  `TMAG5273` hall. 0x35's 4 channels best fit **LIS accelerometer** (X/Y/Z + temp) or
  **raw FDC meas0..3**. Needs a labeled capture to disambiguate.

## 0x07 — ASCII debug (not structured, but carries real data)
Observed payloads (byte0=0x07, byte1 seemingly a stream id, rest ASCII):
- `FW: 850118 [ambient] temp 21.9005 humidity 29.6756 percent` → **ambient 21.9 °C,
  RH 29.7 %** (SHT40). This is the ambient temp/humidity source; **not** a binary packet.
- `FW: 850118 [sht40] crc mismatches: 0`
- `FW: 850482 [cap_samplingL] min/avg/max cycle time 16ms/17.19ms/19ms`, `[cap_samplingR] ...`

## Where are the 8 bed thermistors?
**Not in the passive stream.** Firmware has a separate `[therm]` / `[tempv2]` subsystem
(`[sensor] therm%u took %ums`, `[therm] bad thermistor selection %u`, `[tempv2]
thermistors not ready`). On Pod-3 the equivalent (`0xAF` Temperature) was **polled**
via `ProbeTemperature`. We captured passively and never polled, so no temp frame
appeared. The bed-temp encoding/scale is therefore **UNKNOWN** until a poll is issued.

---

## Implemented in pod-proto
`crates/pod-proto/src/sensor/packet.rs`:
- New enum variants `SensorPacket::Pod4Piezo(Pod4PiezoData)` (0x34) and
  `SensorPacket::Pod4Aux(Pod4AuxData)` (0x35); dispatch arms added for 0x34/0x35.
- `Pod4PiezoData { subtype, freq, timestamp_ms, gain, left: Vec<i32>, right: Vec<i32> }`.
- `Pod4AuxData { subtype, reserved:[u8;8], rate_hz, timestamp_ms, channels:[Vec<i32>;4] }`.
- Parsers validate header length and body alignment (0x34 body %8, 0x35 body %16) and
  return `InvalidStructure` otherwise.
- Pod-3 structs/parsers (Piezo/Capacitance/Temperature) left intact for cherry-picking.
- 3 new tests parse **real CRC-validated captured frames** (`test_pod4_piezo_real`,
  `test_pod4_aux_real`, `test_pod4_capacitance_real`) and assert exact header fields,
  sample counts, first-sample values and plausible ranges.

Tests: `cargo test -p pod-proto` → 54 passed (51 prior + 3). Full workspace → 86 passed
(was 83). No warnings.

---

## Confidence summary
- CONFIRMED: framing; opcode census; shared counter = uptime ms; 0x33 = Pod-3 cap
  layout; 0x34 = dual piezo i32, freq 500, gain 400 (matches telemetry), timestamp;
  0x35 = 4-ch i32, rate 200, timestamp; ambient 21.9 °C / RH 29.7 % via 0x07.
- INFERRED: subtype bytes (0x41/0x02) are format tags; 0x34 gain is a single u32 (not
  split L/R); 0x35 is accel or raw-FDC.
- UNKNOWN: per-channel physical mapping (which cap pad / which piezo side is ch0);
  0x35's sensor identity; bed-thermistor packet + its temperature scale.

## Labeled captures to run on the live Pod (ordered)
1. **Person on LEFT side only, ~30 s @921600 passive.** Watch 0x33 (which of the 6 cap
   channels rise) and 0x34 (which piezo channel gains amplitude). Fixes left/right and
   channel→pad mapping for both cap and piezo.
2. **Person on RIGHT side only, ~30 s.** Confirms the complementary mapping.
3. **Poll temperature** (send the Pod-4 `ProbeTemperature` equivalent; try the Pod-3
   `7E 02 2F FF 8C E8`) and capture the response opcode. Identifies the bed-thermistor
   packet, then set a **known bed temperature** (heat/cool to a fixed setpoint) to fix
   the temp scale (expect centi-°C like Pod-3, but must verify).
4. **Tap / shake the Pod (no one on bed), ~10 s.** If 0x35 ch values swing with motion →
   it is the LIS accelerometer; if flat → it is raw FDC capacitance. Disambiguates 0x35.
5. **Change piezo gain** via `SetPiezoGain(g,g)` and re-capture 0x34 — confirms the
   header `gain` field tracks the setting (and whether it is one u32 or two u16).
6. **Empty-bed vs firm-press on each cap pad** — labels each of the 6 0x33 channels to a
   physical location (head/torso/foot × L/R).

---

## RESOLVED (2026-07-18) — physical side mapping, from live per-side occupancy

Captured the sensor stream with a person on the **left** side, on the **right** side,
and empty (`backup/captures/cap_ttymxc0_921600_{left,right,10s}.bin`), and diffed
channel amplitudes (`scratchpad/analyze_sides.rs`). Empty cap baseline
≈ `[1084,1279,1679,1614,1332,909]`.

- **0x33 capacitance → sides:** left occupancy drove **channel 1** hardest
  (1279→3381), right occupancy drove **channel 4** hardest (1332→5106); channels
  2 and 3 are the secondary left/right responders; channels 0 and 5 barely move.
  Layout: `[edge, LEFT, left2, right2, RIGHT, edge]`. Exposed as
  `CapacitanceData::left()` (ch1) / `right()` (ch4). **CONFIRMED.**
- **0x34 piezo → sides:** the **first/even** interleave slot is the **RIGHT** piezo
  (stddev 760→414831 when on the right), the **second/odd** slot is the **LEFT**
  piezo (stddev 1001→1873356 when on the left) — i.e. inverted vs opensleep's Pod-3
  order. `Pod4PiezoData.left/.right` corrected accordingly. **CONFIRMED.**

Still unmapped: within-side pad geometry (head/torso/foot) of the secondary cap
channels; 0x35 aux identity (accelerometer vs FDC); bed-thermistor scale (polled,
not in the passive stream).
