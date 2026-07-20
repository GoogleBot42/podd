# Pod 4 Sensor MCU UART Protocol — Reverse-Engineering Report

Date: 2026-07-17. Analysis only (no repo changes). Sources: live captures,
`firmware-sensor-g0.bbin` (STM32G0), opensleep (Pod-3 reference), free-sleep telemetry.

## TL;DR

- **Framing:** opensleep expects the SENSOR to use the **exact same framing as the
  frozen MCU**: `0x7E | LEN | payload | CRC16` where CRC = CRC-CCITT init `0x1D0F`,
  poly `0x1021`, computed over `payload` only, transmitted **big-endian**. Same
  `PacketCodec` / `checksum::compute` as frozen. **Confirmed from code.**
- **Baud (opensleep / Pod 3):** bootloader = **38400**, firmware = **115200**, on
  `/dev/ttymxc0`. Confirmed both in code AND as literal `LE32` constants inside the
  Pod-3 STM32F0 sensor blob (`115200`×9, `38400`×5).
- **Pod 4 reality:** The G0 sensor blob contains **NONE** of the standard bauds
  (no 38400/115200/230400/460800/921600 as data words). The live "115200" capture
  is **not** valid 7E framing (0x7E essentially absent: 1 in 2621 bytes; entropy
  5.08; heavy per-bit pinning). The byte statistics are the **undersampling
  fingerprint** → **the true sensor baud is HIGHER than 115200.** (Inferred, high
  confidence on direction; exact value NOT recoverable from a byte-level capture.)
- **`ce 4a` pattern:** a **wrong-baud aliasing artifact**, not idle (idle=0xFF) and
  not framing. It is the beat between the true bit-clock and the 115200 sample clock.
- **Init required?** In firmware mode the sensor streams **Capacitance (0x33)**
  automatically, but **Piezo (0x32)** requires the host to send `EnablePiezo` +
  `SetPiezoGain`, and **Temperature (0xAF)** is **polled** (`ProbeTemperature`).
  Our passive capture caught frank's already-running stream (piezo enabled) at the
  native high baud → that is why it looks like a dense, high-entropy stream.

---

## 1. What opensleep expects from the sensor MCU

All from `/tmp/opensleep-sensor/src`.

### Transport / framing (identical to frozen)
`common/codec.rs`: `START = 0x7E`. Encoder = `command(payload)`:
```
res = [0x7E, payload.len() as u8, ...payload, crc>>8, crc&0xff]   // CRC big-endian
```
`common/checksum.rs`: CRC-CCITT, `CRC_START=0x1D0F`, `CRC_POLY_CCITT=0x1021`, over
payload. **Same as the confirmed-working frozen MCU.** Decoder scans for `0x7E`,
reads `LEN`, validates CRC, then dispatches on `payload[0]`.

### Baud + discovery (`sensor/manager.rs`)
```
pub const PORT: &str = "/dev/ttymxc0";
const BOOTLOADER_BAUD: u32 = 38400;
const FIRMWARE_BAUD:   u32 = 115200;
```
`run_discovery()`:
1. Open @ **38400**, send `Ping` up to 3× (`7E 01 01 DC BD`).
2. If a valid packet returns → device is in **bootloader**. Send **`JumpToFirmware`**
   (`7E 01 10 DE AD`), wait for the mode-switch packets, then **re-open @ 115200**.
3. If bootloader ping fails → try **firmware** directly @ 115200 (device already running).

So YES — the host **actively drives** the device. The only mode-switch opcode is
`0x10` (JumpToFirmware). There is no separate "start streaming" opcode for
capacitance; piezo/temperature are enabled/polled by the scheduler.

### Commands the host sends (`sensor/command.rs`, verified by unit-test vectors)
| Command | Bytes on wire |
|---|---|
| Ping (0x01) | `7E 01 01 DC BD` |
| GetHardwareInfo (0x02) | `7E 01 02 EC DE` |
| GetFirmwareHash (0x04) | `7E 01 04 8C 18` |
| **JumpToFirmware (0x10)** | `7E 01 10 DE AD` |
| SetPiezoFreq(1000) (0x21) | `7E 05 21 00 00 03 E8 7A 5E` |
| **EnablePiezo (0x28)** | `7E 01 28 69 F6` |
| **SetPiezoGain(400,400) (0x2B)** | `7E 05 2B 01 90 01 90 AB 80` |
| EnableVibration (0x2E) | `7E 01 2E 09 30` |
| ProbeTemperature (0x2F) | `7E 02 2F FF 8C E8` |
| SetAlarm (0x2C) | `7E 08 2C side int patt d3 d2 d1 d0 crc crc` |

Scheduler cadence: Ping/ProbeTemperature every 4 s; hwinfo/enable-vibration/
piezo-gain/piezo-freq/enable-piezo every 0.8 s until state confirms; alarms every 5 s.

### Packets the host expects back (`sensor/packet.rs`, response = cmd|0x80 or async)
- `0x07` Message (ASCII), `0x31` Init (BL→FW transition), `0x32` **Piezo**,
  `0x33` **Capacitance**, `0x81` Pong, `0x82` HardwareInfo, `0x84` GetFirmware,
  `0x90` JumpingToFirmware, `0xA1/0xA8/0xAB/0xAC/0xAE`, `0xAF` **Temperature**.
- Payload layouts (all big-endian u16):
  - **Capacitance 0x33**: `seq(u32)` + 6×`[idx, hi, lo]` (idx 0..5) → 6 channels LTR.
  - **Temperature 0xAF**: 8×bed + ambient + humidity + microcontroller (centi-°C),
    each preceded by an index byte 0..10.
  - **Piezo 0x32**: `02`, `freq(u32)`, `seq(u32)`, `gain(u16,u16)`, then interleaved
    `left(u16) right(u16)` samples. Common frame sizes 142–254 bytes.
- Pong byte `0x46` = "in firmware", `0x42` = "in bootloader".

This maps 1:1 to the Pod-4 telemetry we have (6-ch capacitance presence, 8 bed
temps, piezo HR/breathing, gain 400).

---

## 2. Raw capture analysis (`cap_ttymxc0_115200.bin`, 2621 B)

Method validated against the **known-good frozen** capture
(`cap_ttymxc2_38400.bin`): my CRC/frame detector finds **7 valid 7E frames** there,
and the fine ratio-sweep locks at ratio≈1.0 → tool is correct.

Sensor @115200 statistics:
- **Entropy 5.08 bits/byte** (structured, not raw-random, not clean-frame-sparse).
- **`0x7E` count = 1** in 2621 bytes. A correct 7E stream would have `0x7E` as one of
  the *most common* bytes (a delimiter every ~20–250 B). Its near-total absence ⇒
  **not valid 7E framing at 115200.**
- **Per-bit P(bit=1): b0=.42 b1=.97 b2=.49 b3=.91 b4=.07 b5=.43 b6=.87 b7=.53.**
  Bits 1,3,6 pinned high; bit 4 pinned low. Strong positional pinning = classic
  **baud-mismatch aliasing**, where fixed received-bit positions repeatedly sample
  the true stream's start(0)/stop(1)/idle(1) levels.
- **`ce 4a` prefix**: appears in short bursts then dissolves into high-entropy bytes.
  It is periodic aliasing (2-byte / 20-cap-bit beat), **not** idle (idle would read
  `0xFF`) and **not** a sync word.

### Direction of the mismatch (decisive)
- Capturing the **frozen 38400** signal **at 115200** (`cap_ttymxc2_115200.bin`)
  gives P(bit)=0.00 for the low bits and lots of `0x00` — the **over**sampling
  fingerprint (true baud < capture baud).
- The sensor @115200 shows the **opposite** — high bits pinned to 1, few zeros,
  high-bit-set bytes — the **under**sampling fingerprint.
- ⇒ **The true sensor baud is HIGHER than 115200.** (Confident on direction.)

### Why exact baud can't be recovered from this file
A byte-level UART capture has already discarded sub-bit timing and inter-byte idle
gaps, so re-slicing to a *higher* baud is information-lossy. Re-baud sweeps
(0.33×–8×) recovered **0 valid CRC frames** in every direction — expected, and
therefore **inconclusive** for the exact value. Only a fresh capture at the right
baud (or a logic-analyzer pulse-width measurement) can confirm.

---

## 3. Firmware mining (`firmware-sensor-g0.bbin`)

`.bbin` = 128-B big-endian header (magic `0x88888888`, body-len BE32 @0x0c). The
G0 raw image: SP=`0x20024000` (≈144 KB RAM ⇒ STM32G0Bx), reset vector `0x080196f9`.

### Identity
- `[sys] g0 v%s running beep boop bop`, `[sys] pod5 hw%sinitialized`,
  `[sys] using platform %s` → this is the **STM32G0 sensor board used on Pod 4/5**.
- The other blobs: `firmware-sensor.bbin` (42 KB) and `-legacy` (40 KB) are the
  STM32F0 **"pod 2.0"** sensors (SP=`0x20002000`, ≈8 KB RAM) — opensleep's target.
- The G0 board is far more capable: `ADS`-family piezo ADCs (SPI), `FDC`
  capacitance-to-digital (6-ch), `LIS`/`TMAG` accel/mag, `SHT40` temp/humidity,
  `TCA8418` keypad, `LP5009` LED drivers, `[alarm]/[motor]` haptics.

### Protocol / baud hints
- Host-facing command/response protocol still present: `Received response to 0x%02x`,
  `HWINFO sn %08x pn %u sku %x hw %04x f %x d %x`, `[hf] %s` (host-frame log?),
  `[buttons] sending %u bytes incl lsp cmd byte`.
- **`[sampling] req gain %u %u`** ← directly corresponds to `SetPiezoGain` and to the
  free-sleep telemetry **gainLeft/gainRight = 400** (0x190). Strong evidence the G0
  keeps the same gain/opcode semantics ⇒ **very likely still 7E + same opcode table**,
  just at a different baud.
- **Baud constants:** none. The Pod-3 F0 blob stores its bauds as literals
  (`115200`×9, `38400`×5, `921600`×1). The G0 blob stores **no** standard UART baud
  as a data word. It references **USART1** (`0x40013800`) and **USART2**
  (`0x40004400`); baud is set via a code immediate (HAL init), not a rodata word, so
  it is not greppable. The only large decimal literals present are `1000000`
  (adjacent to timer base `0x40007000` → a **1 MHz µs-timer tick**, probably not a
  baud) and `2000000` (before a function jump-table). Treat 1M/2M as **weak**
  candidates, not confirmed bauds.

---

## 4. Cross-check with Pod 4 telemetry
free-sleep exposes gainLeft/gainRight=400, 6-ch presence, 8 bed temps. These are
exactly the `SetPiezoGain(400,400)` value plus the `Capacitance(6×u16)` and
`Temperature(8 bed + ambient + humidity + mcu)` packets above — consistent with the
opensleep packet layout surviving onto Pod 4, transported over 7E frames at the
(higher) native baud.

---

## Best hypothesis

The Pod 4 (STM32G0) sensor speaks the **same `0x7E|LEN|payload|CRC16(0x1D0F)`
protocol and (very likely) the same opcode table** as the Pod-3 sensor and the
frozen MCU, but at a **higher UART baud than opensleep's 115200** (opensleep is
explicitly Pod-3-tuned/untested on Pod 4). Our passive 115200 capture caught frank's
already-running firmware-mode stream (piezo enabled) undersampled → the `ce 4a`
aliasing garbage. Exact baud is undetermined from the byte capture; leading
empirical candidates (high→low prior): **460800, 921600, 230400, 1000000, 2000000,
500000, 250000**.

An init sequence from the host is **only** needed if the device is sitting in
bootloader (send `Ping` @ the bootloader baud, then `JumpToFirmware` `7E 01 10 DE
AD`). If it is already streaming (our case), just read at the correct baud; to also
get piezo/temperature send `EnablePiezo` + `SetPiezoGain(400,400)` + poll
`ProbeTemperature`.

## Concrete next captures (ordered)

Run each on the live Pod on `/dev/ttymxc0`. Use a CRC/7E frame detector (init
0x1D0F over LEN payload) as the success test — success = many frames with opcodes
`0x31/0x32/0x33/0x82/0xAF`.

**A. Passive baud sweep (do FIRST — non-intrusive; device is likely still
streaming).** With frank running (or just after killing it), capture ~5 s at each,
newest→highest prior:
1. **460800**
2. **921600**
3. **230400**
4. **1000000**
5. **2000000**
6. **500000**, then **250000**
For each, count valid `0x7E`+CRC frames. The correct baud yields dense valid frames.

**B. Definitive physical measurement (best if a scope/logic-analyzer is available).**
Probe the sensor TX line, measure the **shortest pulse width** = 1 bit period;
baud = 1/that. Removes all guessing.

**C. Active discovery at each candidate baud (if A finds nothing / device is idle).**
At each baud in {38400, 115200, 230400, 460800, 921600, 1000000}:
  1. Send `Ping` = `7E 01 01 DC BD`, wait 500 ms for a reply.
  2. A reply whose payload starts `0x81` = **Pong**; byte[2]=`0x46`⇒firmware,
     `0x42`⇒bootloader. This both finds the RX baud and the mode.
  3. If Pong=bootloader: send `JumpToFirmware` = `7E 01 10 DE AD`; watch for `0x90`
     then `0x31` Init; then reopen and repeat the passive sweep (firmware baud may
     differ from bootloader baud, as it does on Pod 3: 38400→115200).

**D. Once a baud yields clean frames, provoke the full stream** to confirm packet
types: send `7E 01 02 EC DE` (HWINFO → 0x82), `7E 01 28 69 F6` (EnablePiezo),
`7E 05 2B 01 90 01 90 AB 80` (SetPiezoGain 400,400 → 0x32 piezo), `7E 02 2F FF 8C
E8` (ProbeTemperature → 0xAF). Capacitance `0x33` should appear on its own.

### Confidence
- Confirmed: 7E/CRC framing + opcode table + bauds are what opensleep uses for the
  Pod-3 sensor; F0 blob literally contains 38400/115200; frozen 7E@38400 works;
  G0 blob is the Pod-4/5 board; gain-400 semantics match.
- Inferred (high confidence): 115200 capture is undersampled ⇒ true baud > 115200;
  `ce 4a` is a wrong-baud artifact.
- Inferred (medium): G0 still uses 7E + same opcodes (supported by the shared
  gain/response strings but not byte-proven at the correct baud).
- Unknown: the exact numeric baud — must be measured (plan A/B/C).

---

## 5. LIVE VALIDATION + behavioral findings (2026-07-19/20, podd driving the real Pod 4 cover)

Everything below is from podd running live against the G0 sensor (and frozen)
MCUs — this section supersedes the open questions above.

### Transport: CONFIRMED
- Baud = **921600** in firmware mode, **38400** in bootloader mode. Same
  `7E | LEN | payload | CRC16(CCITT, init 0x1D0F, payload-only, BE)` framing.
- Discovery works: bootloader ping → Pong (byte2 `0x42`) → JumpToFirmware →
  reopen at 921600 → streams `0x33` (capacitance) / `0x34` (piezo, 214B) /
  `0x35` (accelerometer aux, 176B).
- `0x34` header carries `freq:u32` = **500 Hz fixed** (not Pod 3's 1000) and a
  single `gain:u32`.

### ⚠ Framing hazard (bit us on the FROZEN MCU; applies to every LSP frame)
**There is no byte-stuffing.** The MCUs' RX parsers resync on every `0x7E`
byte, so any *command* frame whose payload or CRC happens to contain `0x7E` is
**silently dropped** — no error, no echo, no effect. Real-world hit: frozen
`SetTargetTemperature(Left, 3111)` (= 88.0 °F, an entirely ordinary setpoint)
encodes to `7E 05 40 00 01 0C 27 C6 7E` — CRC low byte = the delimiter — and
was dropped forever, which presented as "the left side never heats at 88 °F"
(the right side at the same temperature has a different side byte, hence a
different CRC, and worked). Mitigation: `FrozenTarget::delimiter_safe` in
`pod-proto` nudges setpoints ±0.01 °C until the frame is clean. **Any new
host→MCU command type must check its encoded frame for interior `0x7E`.**

### Commands the G0 firmware does NOT ack (Pod 3-isms)
Observed live; podd's scheduler now caps these at 10 attempts instead of
re-sending every 800 ms forever:

| Command | Pod 3 behavior | Pod 4 G0 behavior |
|---|---|---|
| `EnableVibration` (0x??→ack 0xAE) | 3-byte ack | usually **no ack**; a 2-byte `AE xx` was seen once (parser now accepts both). podd assumes enabled after the attempt cap so alarms can still arm. |
| `GetHardwareInfo` | CBOR ack | **no reply observed** |
| `ProbeTemperature` (0x2F→0xAF) | 0xAF temp reply | **no reply observed** (bed temps come via the stream) |
| `SetPiezoFreq` | ack + applied | moot — G0 samples at fixed 500 Hz, reported in the 0x34 header; podd treats 500 as healthy |
| `SetPiezoGain` (0x2B→0xAB) | ack `AB 00 hi lo hi lo` | **works** (ack parses, gain 400→405 within tolerance) |
| `Ping` (0x01→0x81) | Pong | **works** (firmware mode byte2 = 0x46) |

### ⚠ Open: the ~60s sensor wedge
The G0 sensor intermittently goes **completely silent** (stream stops, then no
response to pings at either baud) roughly 54–65 s after connect — but not
every run, and **not deterministically caused by host traffic** (reproduced
with all scheduler commands stopped; also seen a back-to-back run with
identical traffic survive). When hard-wedged, port-reopen + rediscovery never
revives it; the **PCAL6416A reset pulse** (podd startup) revives it in ~3 s
every time. podd mitigation: `sensor::supervise` retries in-process every 10 s
(the frozen/TEC manager keeps holding temperature throughout) and escalates to
a process restart — which pulses the reset — after 6 consecutive fast
failures. Next RE step: capture what stock `frank` sends the G0 (existing
captures only recorded the MCU→host direction), and work out per-MCU reset
bits on the PCAL6416A (port-0 output semantics are only half-understood) so a
sensor reset needn't disturb the frozen MCU.
