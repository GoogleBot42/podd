//! End-to-end harness for the Frozen control loop ([`super::manager::run_on`]).
//!
//! The real loop runs unmodified against a fake Frozen MCU on the other end of
//! a [`tokio::io::duplex`] pipe, with real LSP frames both ways and
//! `dry_run = false`. Tests drive it the way the daemon does — bus
//! [`Command`]s, the config / schedules / settings watches — and assert on
//! what the API would see (the [`StatusTx`](crate::bus::StatusTx) snapshot) and on what the MCU was
//! actually sent. Run under `#[tokio::test(start_paused = true)]` so minutes
//! of loop time take milliseconds.
//!
//! What it stands in for:
//! * **the UART** — a duplex pipe framed with the production
//!   [`PacketCodec<FrozenPacket>`];
//! * **the LED** — an [`IS31FL3194Controller`] over [`NullI2c`];
//! * **MQTT** — a real [`AsyncClient`] whose event loop is never polled (a
//!   down broker): every publish in the loop must stay non-blocking (#203);
//! * **the Frozen MCU** — [`fake_mcu`]: bootloader → firmware wake handshake,
//!   hardware info, a temperature stream, `0xC1` probe replies, setpoint
//!   echoes (`0xC0`, as the firmware sends them), and a first-order thermal
//!   model so a freeze-guard ramp actually converges. Its frame parser
//!   resyncs on every `0x7E` exactly like the firmware's, so an unsafe frame
//!   is dropped here too. It can play a zombie that ignores the first N
//!   setpoint writes (actuation-safety rule).

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use embedded_hal::i2c::{ErrorType, I2c, Operation};
use pod_proto::checksum;
use pod_proto::codec::{PacketCodec, START, command};
use pod_proto::frozen::FrozenPacket;
use pod_proto::frozen::packet::FrozenTarget;
use pod_proto::packet::BedSide;
use rumqttc::{AsyncClient, EventLoop, MqttOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, interval, sleep};
use tokio_util::codec::Framed;

use super::manager::{FrozenError, run_on};
use crate::bus::{Command, DeviceSnapshot};
use crate::config::{Config, SideConfig, SidesConfig};
use crate::health::{HealthMap, HealthRegistry};
use crate::led::IS31FL3194Controller;
use crate::recorder::Recorder;
use crate::schedule::Schedules;
use crate::settings::Settings;

/// An I2C bus that accepts every write: the LED is not under test.
pub struct NullI2c;

impl ErrorType for NullI2c {
    type Error = Infallible;
}

impl I2c for NullI2c {
    fn transaction(&mut self, _addr: u8, _ops: &mut [Operation<'_>]) -> Result<(), Infallible> {
        Ok(())
    }
}

/// How the fake MCU behaves.
#[derive(Clone, Debug)]
pub struct McuSpec {
    /// Starting loop-water temperature per side (`[left, right]`), centi-°C.
    pub water: [u16; 2],
    pub heatsink: u16,
    /// How fast (centi-°C per second) a side's water moves toward its
    /// enabled setpoint. An off side holds its temperature.
    pub rate: f64,
    /// Temperature-stream period.
    pub telemetry_every: Duration,
    /// Send one temperature frame just before announcing firmware mode, so
    /// the host has water readings before its first setpoint tick.
    pub telemetry_before_awake: bool,
    /// Zombie: the first N setpoint frames are silently ignored (no echo, no
    /// effect).
    pub ignore_first_setpoints: u32,
}

impl Default for McuSpec {
    fn default() -> Self {
        McuSpec {
            water: [3110, 3110],
            heatsink: 2500,
            rate: 5.0,
            telemetry_every: Duration::from_secs(2),
            telemetry_before_awake: true,
            ignore_first_setpoints: 0,
        }
    }
}

/// One `SetTargetTemperature` frame as the fake MCU received it.
#[derive(Clone, Debug)]
pub struct Received {
    pub at: Instant,
    pub side: BedSide,
    pub tar: FrozenTarget,
    /// The side's water (centi-°C, rounded) when it arrived.
    pub water: u16,
    /// Dropped by the zombie (no echo, no effect).
    pub ignored: bool,
}

/// The fake MCU's state, shared with the test.
#[derive(Debug)]
pub struct McuState {
    pub firmware: bool,
    /// Loop water per side, centi-°C (fractional: the model runs per tick).
    pub water: [f64; 2],
    pub heatsink: u16,
    /// The setpoint the firmware holds per side (`None` = never set: a reset
    /// MCU comes up with both sides off).
    pub target: [Option<FrozenTarget>; 2],
    pub setpoints: Vec<Received>,
    /// Opcodes of every frame received, in order.
    pub opcodes: Vec<u8>,
    /// Frames the firmware parser threw away (a `0x7E` inside, bad CRC).
    pub dropped_frames: u32,
    ignore_left: u32,
    count: u8,
}

impl McuState {
    fn new(spec: &McuSpec) -> Self {
        McuState {
            firmware: false,
            water: spec.water.map(f64::from),
            heatsink: spec.heatsink,
            target: [None, None],
            setpoints: Vec::new(),
            opcodes: Vec::new(),
            dropped_frames: 0,
            ignore_left: spec.ignore_first_setpoints,
            count: 0,
        }
    }

    pub fn water(&self, side: BedSide) -> u16 {
        self.water[side as usize].round() as u16
    }

    /// Setpoint frames the MCU accepted for `side` (zombie drops excluded).
    pub fn accepted(&self, side: BedSide) -> Vec<Received> {
        self.setpoints
            .iter()
            .filter(|r| r.side == side && !r.ignored)
            .cloned()
            .collect()
    }

    pub fn count_opcode(&self, op: u8) -> usize {
        self.opcodes.iter().filter(|&&o| o == op).count()
    }

    /// First-order thermal model: water moves toward an enabled setpoint.
    fn advance(&mut self, dt: Duration, rate: f64) {
        let step = rate * dt.as_secs_f64();
        for i in 0..2 {
            let Some(t) = self.target[i].as_ref().filter(|t| t.enabled) else {
                continue;
            };
            let goal = f64::from(t.temp);
            let w = &mut self.water[i];
            *w = if *w > goal {
                (*w - step).max(goal)
            } else {
                (*w + step).min(goal)
            };
        }
    }

    fn temperature_update(&mut self) -> Vec<u8> {
        let [l, r] = [self.water(BedSide::Left), self.water(BedSide::Right)];
        let h = self.heatsink;
        self.count = self.count.wrapping_add(1);
        let mut p = vec![0x41];
        p.extend(l.to_be_bytes());
        p.extend(r.to_be_bytes());
        p.extend(h.to_be_bytes());
        p.extend([0, self.count]);
        p
    }

    /// The firmware's replies to one host frame (payloads, unframed).
    fn handle(&mut self, payload: &[u8], now: Instant, spec: &McuSpec) -> Vec<Vec<u8>> {
        self.opcodes.push(payload[0]);
        let mut out = Vec::new();
        match payload {
            // Ping -> Pong(in_firmware)
            [0x01] => {
                if self.firmware && spec.telemetry_before_awake {
                    out.push(self.temperature_update());
                }
                out.push(vec![0x81, 0x00, if self.firmware { 0x46 } else { 0x42 }]);
            }
            [0x02] => out.push(HWINFO.to_vec()),
            // JumpToFirmware
            [0x10] => {
                self.firmware = true;
                if spec.telemetry_before_awake {
                    out.push(self.temperature_update());
                }
                out.push(vec![0x90, 0x00]);
            }
            // GetTemperatures -> 0xC1 four-probe frame
            [0x41] if self.firmware => {
                let [l, r] = [self.water(BedSide::Left), self.water(BedSide::Right)];
                let h = self.heatsink;
                let mut p = vec![0xC1, 0x00];
                for (i, t) in [l, r, h.saturating_sub(300), h].into_iter().enumerate() {
                    p.push(i as u8 + 1);
                    p.extend(t.to_be_bytes());
                }
                out.push(p);
            }
            // Prime -> PrimingStarted
            [0x52] if self.firmware => out.push(vec![0xD2, 0x00]),
            // SetTargetTemperature side enabled temp_hi temp_lo -> 0xC0 echo
            &[0x40, side, enabled, hi, lo] if self.firmware => {
                let side = BedSide::from_repr(side).expect("host sent a bad side");
                let tar = FrozenTarget {
                    enabled: enabled != 0,
                    temp: u16::from_be_bytes([hi, lo]),
                };
                let ignored = self.ignore_left > 0;
                self.setpoints.push(Received {
                    at: now,
                    side,
                    tar: tar.clone(),
                    water: self.water(side),
                    ignored,
                });
                if ignored {
                    self.ignore_left -= 1;
                } else {
                    self.target[side as usize] = Some(tar);
                    out.push(vec![0xC0, 0x00, side as u8, enabled, hi, lo]);
                }
            }
            _ => {}
        }
        out
    }
}

/// `0x82` hardware-info reply (CBOR `[status, map]`), from a live Pod capture.
const HWINFO: [u8; 64] = [
    0x82, 0x00, 0xA6, 0x63, 0x73, 0x6B, 0x75, 0x01, 0x68, 0x64, 0x61, 0x74, 0x65, 0x63, 0x6F,
    0x64, 0x65, 0x1A, 0x00, 0x16, 0x01, 0x0D, 0x6B, 0x66, 0x61, 0x63, 0x74, 0x6F, 0x72, 0x79,
    0x6C, 0x69, 0x6E, 0x65, 0x01, 0x65, 0x68, 0x77, 0x72, 0x65, 0x76, 0x19, 0x05, 0x00, 0x62,
    0x70, 0x6E, 0x19, 0x50, 0x78, 0x68, 0x64, 0x65, 0x76, 0x69, 0x63, 0x65, 0x73, 0x6E, 0x1A,
    0x00, 0x01, 0x08, 0x9C,
];

/// The next complete host frame's payload, parsed the way the firmware does
/// it: it resyncs on *every* `0x7E`, so a frame with the delimiter anywhere
/// after its start byte is lost (the LSP framing has no byte-stuffing).
fn next_frame(buf: &mut Vec<u8>, dropped: &mut u32) -> Option<Vec<u8>> {
    loop {
        let Some(start) = buf.iter().position(|&b| b == START) else {
            buf.clear();
            return None;
        };
        buf.drain(..start);
        if buf.len() < 2 {
            return None;
        }
        let total = buf[1] as usize + 4;
        if let Some(p) = buf[1..buf.len().min(total)].iter().position(|&b| b == START) {
            buf.drain(..1 + p);
            *dropped += 1;
            continue;
        }
        if buf.len() < total {
            return None;
        }
        let payload = buf[2..total - 2].to_vec();
        let crc = u16::from_be_bytes([buf[total - 2], buf[total - 1]]);
        buf.drain(..total);
        if payload.is_empty() || crc != checksum::compute(&payload) {
            *dropped += 1;
            continue;
        }
        return Some(payload);
    }
}

/// The fake Frozen MCU task: answer host frames, stream temperatures, run
/// the thermal model.
async fn fake_mcu(mut io: DuplexStream, mcu: Arc<Mutex<McuState>>, spec: McuSpec) {
    let mut buf = Vec::new();
    let mut rd = [0u8; 512];
    let mut tick = interval(spec.telemetry_every);
    let mut last = Instant::now();
    loop {
        let replies = tokio::select! {
            n = io.read(&mut rd) => {
                let n = match n {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&rd[..n]);
                let mut m = mcu.lock().unwrap();
                let mut out = Vec::new();
                let mut dropped = 0;
                while let Some(p) = next_frame(&mut buf, &mut dropped) {
                    out.extend(m.handle(&p, Instant::now(), &spec));
                }
                m.dropped_frames += dropped;
                out
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let mut m = mcu.lock().unwrap();
                m.advance(now - last, spec.rate);
                last = now;
                if m.firmware { vec![m.temperature_update()] } else { vec![] }
            }
        };
        for p in replies {
            if io.write_all(&command(p)).await.is_err() {
                return;
            }
        }
    }
}

/// Run a test body on a [`tokio::task::LocalSet`]: the loop future is not
/// `Send` (production drives it from `select!`, never `spawn`s it), so
/// [`Harness::start`] spawns it locally.
pub async fn local<F: std::future::Future>(body: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(body).await
}

/// A running Frozen loop wired to a fake MCU ([`fake_mcu`]). Start it inside
/// [`local`].
pub struct Harness {
    pub status: watch::Receiver<DeviceSnapshot>,
    pub cmd: mpsc::Sender<Command>,
    /// Held so the loop's watch arms stay live, as in production; also there
    /// for scenarios that push a config / settings change.
    #[allow(dead_code)]
    pub config: watch::Sender<Config>,
    pub schedules: watch::Sender<Schedules>,
    #[allow(dead_code)]
    pub settings: watch::Sender<Settings>,
    pub mcu: Arc<Mutex<McuState>>,
    pub health: watch::Receiver<HealthMap>,
    pub task: JoinHandle<Result<(), FrozenError>>,
    /// Held, never polled: MQTT is a down broker.
    _eventloop: EventLoop,
}

impl Harness {
    pub fn start(config: Config, schedules: Schedules, spec: McuSpec) -> Harness {
        let (host_io, mcu_io) = tokio::io::duplex(4096);
        let mcu = Arc::new(Mutex::new(McuState::new(&spec)));
        tokio::spawn(fake_mcu(mcu_io, mcu.clone(), spec));

        let (config_tx, config_rx) = watch::channel(config);
        let (schedules_tx, schedules_rx) = watch::channel(schedules);
        let (settings_tx, settings_rx) = watch::channel(Settings::default());
        let (status_tx, status_rx) = watch::channel(DeviceSnapshot::default());
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (health, health_rx) = HealthRegistry::new();
        let (client, eventloop) =
            AsyncClient::new(MqttOptions::new("frozen-harness", "localhost", 1883), 10);

        let task = tokio::task::spawn_local(run_on(
            move || Ok(Framed::new(host_io, PacketCodec::<FrozenPacket>::new())),
            config_rx,
            schedules_rx,
            settings_rx,
            IS31FL3194Controller::new(NullI2c),
            client,
            Arc::new(status_tx),
            cmd_rx,
            health,
            false,
            Recorder::default(),
        ));

        Harness {
            status: status_rx,
            cmd: cmd_tx,
            config: config_tx,
            schedules: schedules_tx,
            settings: settings_tx,
            mcu,
            health: health_rx,
            task,
            _eventloop: eventloop,
        }
    }

    /// Let the loop run for `d` of (paused, auto-advancing) time; the loop
    /// must still be alive afterwards.
    pub async fn run_for(&self, d: Duration) {
        sleep(d).await;
        assert!(!self.task.is_finished(), "the Frozen loop exited");
    }

    /// Run the loop until `cond` holds, failing after `limit`.
    pub async fn wait_for(&self, what: &str, limit: Duration, cond: impl Fn(&Harness) -> bool) {
        let start = Instant::now();
        while !cond(self) {
            assert!(start.elapsed() < limit, "timed out after {limit:?} waiting for {what}");
            self.run_for(Duration::from_millis(100)).await;
        }
    }

    pub async fn send(&self, cmd: Command) {
        self.cmd.send(cmd).await.expect("Frozen loop gone");
        // let the loop take it
        sleep(Duration::from_millis(1)).await;
    }

    /// The setpoint the API shows for `side`, °C.
    pub fn target_c(&self, side: BedSide) -> Option<f64> {
        let s = self.status.borrow();
        match side {
            BedSide::Left => s.left.target_temp_c,
            BedSide::Right => s.right.target_temp_c,
        }
    }

    pub fn is_on(&self, side: BedSide) -> bool {
        let s = self.status.borrow();
        match side {
            BedSide::Left => s.left.is_on,
            BedSide::Right => s.right.is_on,
        }
    }

    pub fn mcu(&self) -> std::sync::MutexGuard<'_, McuState> {
        self.mcu.lock().unwrap()
    }

    /// Poll the API snapshot every `every` for `d`, asserting each time that
    /// `side` shows exactly `want_c`.
    pub async fn assert_target_held(
        &self,
        side: BedSide,
        want_c: f64,
        d: Duration,
        every: Duration,
    ) {
        let start = Instant::now();
        while start.elapsed() < d {
            let got = self.target_c(side);
            if got != Some(want_c) {
                let mcu = self.mcu();
                panic!(
                    "API target for {side:?} is {got:?}, want {want_c} ({:?} into the hold; \
                     MCU holds {:?}, water {})",
                    start.elapsed(),
                    mcu.target[side as usize],
                    mcu.water(side),
                );
            }
            self.run_for(every).await;
        }
    }
}

/// A test config: UTC, no daily prime, freeze guard at its defaults, and a
/// constant-temperature profile (`[left, right]` °C) whose window is open
/// for hours either side of the real clock (the schedule resolvers read the
/// wall clock, which paused tokio time does not move).
pub fn config(temps_c: [f32; 2]) -> Config {
    let src = std::fs::read_to_string("example_couples.ron").expect("example_couples.ron");
    let opts = ron::Options::default()
        .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME);
    let mut cfg: Config = opts.from_str(&src).expect("example_couples.ron parses");
    cfg.timezone = jiff::tz::TimeZone::UTC;
    cfg.prime_enabled = false;
    cfg.away_mode = Default::default();
    cfg.freeze_protection = Default::default();
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).time();
    let six_h = jiff::SignedDuration::from_hours(6);
    let side = |t: f32| SideConfig {
        temperatures: vec![t],
        sleep: now.wrapping_sub(six_h),
        wake: now.wrapping_add(six_h),
        alarm: None,
    };
    cfg.profile = SidesConfig::Couples {
        left: side(temps_c[0]),
        right: side(temps_c[1]),
    };
    cfg
}
