//! `pod-proto` — the Eight Sleep Pod LSP serial protocol layer.
//!
//! Vendored/adapted from opensleep (GPL-3.0), <https://github.com/LiamSnow/opensleep>.
//!
//! This crate holds the SoC-agnostic, pure/testable protocol pieces shared by
//! both STM32 subsystems (Frozen + Sensor):
//!   - LSP framing/codec (`0x7E | LEN | payload | CRC16`), CRC-CCITT (seed
//!     `0x1D0F`), and the [`codec::PacketCodec`] `tokio_util` decoder/encoder,
//!   - the shared [`packet::Packet`] trait, packet-parsing helpers,
//!     [`packet::BedSide`] and [`packet::HardwareInfo`],
//!   - serial port creation ([`serial`], via `tokio-serial`) and
//!     [`serial::DeviceMode`],
//!   - each subsystem's packet + command tables ([`frozen`], [`sensor`]),
//!   - the thermostat interpolation math ([`frozen::profile`]).
//!
//! It intentionally does NOT depend on any Linux HAL (no `linux-embedded-hal`)
//! or on the daemon's config/MQTT layers, so it can be reused by other tools
//! (e.g. an MCU flasher) and unit-tested on the host.

pub mod checksum;
pub mod codec;
pub mod packet;
pub mod serial;

pub mod frozen;
pub mod sensor;
