//! `pod-probe` — a read-only serial probe for validating `pod-proto` framing
//! against a live Eight Sleep Pod.
//!
//! It is SAFE to run on real hardware: it only ever *reads* frames, and (with
//! `--ping`) sends a single benign `Ping` (opcode `0x01`). It never resets the
//! MCUs, never sends setpoint/control commands, and never jumps firmware.
//!
//! Example:
//!   pod-probe --port /dev/ttymxc2 --baud 38400  --kind frozen --seconds 5
//!   pod-probe --port /dev/ttymxc0 --baud 921600 --kind sensor --seconds 5 --ping
//!
//! For each valid LSP frame (`0x7E | LEN | payload | CRC16-BE`) it prints the
//! raw bytes, the payload, the opcode, and the `pod-proto`-parsed form. Frames
//! with a bad CRC and payloads pod-proto can't parse are still reported (hex +
//! opcode). A one-line `SUMMARY` closes the run. Output is line-oriented and
//! greppable (tags: `FRAME`, `SUMMARY`, `crc=ok`, `crc=BAD`, `parse=err`).

use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::BytesMut;
use clap::{Parser, ValueEnum};
use pod_proto::checksum;
use pod_proto::codec::{CommandTrait, START};
use pod_proto::frozen::{FrozenCommand, FrozenPacket};
use pod_proto::packet::Packet;
use pod_proto::sensor::{SensorCommand, SensorPacket};
use pod_proto::serial::create_port;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Kind {
    Frozen,
    Sensor,
}

#[derive(Parser, Debug)]
#[command(
    name = "pod-probe",
    about = "Read-only serial probe for the Eight Sleep Pod (validates pod-proto framing)",
    long_about = "Reads LSP frames from a Pod MCU UART and prints raw hex + the pod-proto-parsed \
form. SAFE on live hardware: read-only, except an optional single benign Ping (--ping). Never \
resets the MCUs, sends control/setpoint commands, or jumps firmware."
)]
struct Args {
    /// Serial device path, e.g. /dev/ttymxc2 (frozen) or /dev/ttymxc0 (sensor)
    #[arg(long)]
    port: String,

    /// Baud rate. Pod 4: frozen=38400, sensor firmware=921600.
    #[arg(long)]
    baud: u32,

    /// Which subsystem's packet table to parse with.
    #[arg(long, value_enum)]
    kind: Kind,

    /// How long to read for, in seconds.
    #[arg(long, default_value_t = 5)]
    seconds: u64,

    /// Also send ONE benign Ping (0x01) at start and print the Pong reply.
    /// This is the only thing the probe ever writes.
    #[arg(long)]
    ping: bool,
}

/// Running tallies for the end-of-run summary.
#[derive(Default)]
struct Stats {
    bytes_read: usize,
    frames_seen: usize,
    crc_ok: usize,
    crc_bad: usize,
    parsed: usize,
    parse_err: usize,
    pongs: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!(
        "# pod-probe: port={} baud={} kind={:?} seconds={} ping={}",
        args.port, args.baud, args.kind, args.seconds, args.ping
    );
    println!("# READ-ONLY. Frame = 0x7E | LEN | payload | CRC16-BE (CRC-CCITT 0x1D0F).");

    let mut port = create_port(&args.port, args.baud)
        .with_context(|| format!("failed to open {} @ {}", args.port, args.baud))?;

    if args.ping {
        let bytes = match args.kind {
            Kind::Frozen => FrozenCommand::Ping.to_bytes(),
            Kind::Sensor => SensorCommand::Ping.to_bytes(),
        };
        port.write_all(&bytes)
            .await
            .context("failed to send Ping")?;
        port.flush().await.ok();
        println!("# sent Ping: {}", hex_spaced(&bytes));
    }

    let mut stats = Stats::default();
    let mut acc = BytesMut::with_capacity(4096);
    let mut chunk = [0u8; 1024];
    let deadline = Instant::now() + Duration::from_secs(args.seconds);

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, port.read(&mut chunk)).await {
            // timeout elapsed -> done reading
            Err(_) => break,
            Ok(Ok(0)) => {
                // EOF: unusual for a serial port, but stop cleanly
                println!("# EOF on serial port");
                break;
            }
            Ok(Ok(n)) => {
                stats.bytes_read += n;
                acc.extend_from_slice(&chunk[..n]);
                drain_frames(&mut acc, args.kind, &mut stats);
            }
            Ok(Err(e)) => {
                eprintln!("# read error: {e}");
                break;
            }
        }
    }

    println!(
        "SUMMARY frames={} crc=ok:{} crc=bad:{} parse=ok:{} parse=err:{} pongs={} bytes={}",
        stats.frames_seen,
        stats.crc_ok,
        stats.crc_bad,
        stats.parsed,
        stats.parse_err,
        stats.pongs,
        stats.bytes_read,
    );

    Ok(())
}

/// Pull as many complete frames out of `acc` as possible, printing each.
///
/// Mirrors `pod_proto::codec`'s resync: on a bad CRC we advance a single byte
/// and retry, so a spurious 0x7E in the stream doesn't desync us permanently.
fn drain_frames(acc: &mut BytesMut, kind: Kind, stats: &mut Stats) {
    loop {
        // find frame start
        let Some(start) = acc.iter().position(|&b| b == START) else {
            acc.clear();
            return;
        };
        if start > 0 {
            let _ = acc.split_to(start);
        }

        // need at least START + LEN
        if acc.len() < 2 {
            return;
        }

        let len = acc[1] as usize;
        if len == 0 {
            // empty payload -> skip the START byte and resync
            let _ = acc.split_to(1);
            continue;
        }

        let total = 1 + 1 + len + 2; // START + LEN + payload + CRC16
        if acc.len() < total {
            return; // need more bytes
        }

        let payload = &acc[2..2 + len];
        let crc_actual = u16::from_be_bytes([acc[2 + len], acc[3 + len]]);
        let crc_expected = checksum::compute(payload);

        stats.frames_seen += 1;

        if crc_actual == crc_expected {
            stats.crc_ok += 1;
            let frame: Vec<u8> = acc[..total].to_vec();
            let payload_vec: Vec<u8> = payload.to_vec();
            print_frame(kind, &frame, &payload_vec, stats);
            let _ = acc.split_to(total); // consume the whole frame
        } else {
            stats.crc_bad += 1;
            println!(
                "FRAME crc=BAD  op=0x{:02X} len={} raw={} payload={} (crc got={:04X} want={:04X})",
                payload[0],
                len,
                hex_spaced(&acc[..total]),
                hex_spaced(payload),
                crc_actual,
                crc_expected,
            );
            let _ = acc.split_to(1); // resync
        }
    }
}

/// Print one CRC-valid frame: raw + payload + opcode + parsed form.
fn print_frame(kind: Kind, frame: &[u8], payload: &[u8], stats: &mut Stats) {
    let opcode = payload[0];
    let buf = BytesMut::from(payload);

    let parsed: String = match kind {
        Kind::Frozen => match FrozenPacket::parse(buf) {
            Ok(p) => {
                stats.parsed += 1;
                if matches!(p, FrozenPacket::Pong(_)) {
                    stats.pongs += 1;
                }
                format!("{p:?}")
            }
            Err(e) => {
                stats.parse_err += 1;
                format!("<parse=err: {e}>")
            }
        },
        Kind::Sensor => match SensorPacket::parse(buf) {
            Ok(p) => {
                stats.parsed += 1;
                if matches!(p, SensorPacket::Pong(_)) {
                    stats.pongs += 1;
                }
                format!("{p:?}")
            }
            Err(e) => {
                stats.parse_err += 1;
                format!("<parse=err: {e}>")
            }
        },
    };

    println!(
        "FRAME crc=ok   op=0x{:02X} len={} raw={} payload={} parsed={}",
        opcode,
        payload.len(),
        hex_spaced(frame),
        hex_spaced(payload),
        parsed,
    );
}

/// Uppercase, space-separated hex, e.g. `7E 03 81 00 46`.
fn hex_spaced(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
