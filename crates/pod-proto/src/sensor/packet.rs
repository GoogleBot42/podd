use bytes::BytesMut;
use hex_literal::hex;

use crate::packet::{
    self, HardwareInfo, Packet, PacketError, invalid_structure, validate_packet_at_least,
    validate_packet_size,
};

#[derive(Debug, PartialEq)]
pub enum SensorPacket {
    /// next state, where bootloader = false, firmware = true
    Pong(bool),
    Message(String),
    HardwareInfo(HardwareInfo),
    /// unknown value
    JumpingToFirmware(u8),
    PiezoGainSet(u16, u16),
    /// unknown value, always (0,2)
    VibrationEnabled(u8, u8),
    /// unknown value, always 4
    GetFirmware(u8),
    /// unknown value, always 0
    PiezoFreqSet(u8),
    /// unknown value, always 0
    PiezoEnabled(u8),
    /// occurs in BL -> FW transition
    Init(u16),
    Capacitance(CapacitanceData),
    Piezo(PiezoData),
    Temperature(TemperatureData),
    /// unknown value
    AlarmSet(u8),
    /// Pod 4 (STM32G0) dual-channel ADS piezo stream, opcode 0x34
    Pod4Piezo(Pod4PiezoData),
    /// Pod 4 (STM32G0) 4-channel auxiliary stream, opcode 0x35
    Pod4Aux(Pod4AuxData),
}

/// Pod 4 piezo packet (opcode `0x34`, observed fixed len 214).
///
/// Header (14 bytes): `34 | subtype | freq:u32 | timestamp_ms:u32 | gain:u32`.
/// Body: interleaved `left,right` signed samples, each a big-endian `i32`
/// (a 24-bit ADS ADC value sign-extended to 32 bits, high half `0xFFF9` in the
/// captured unoccupied case). 25 samples per channel at 500 Hz per 50 ms frame.
#[derive(Debug, PartialEq, Clone)]
pub struct Pod4PiezoData {
    /// Firmware subtype/format byte. CONFIRMED constant `0x41` in all captures.
    pub subtype: u8,
    /// Sampling frequency in Hz. CONFIRMED = 500 (matches 25 samples / 50 ms frame).
    pub freq: u32,
    /// Device uptime in milliseconds (shared free-running counter across opcodes).
    /// CONFIRMED against the `[ambient] temp ... 850118` ASCII log timestamps.
    pub timestamp_ms: u32,
    /// ADS piezo gain. CONFIRMED = 400 == free-sleep telemetry gainLeft/gainRight.
    /// May be two `u16` `(0, 400)`; exposed raw as `u32` (see report).
    pub gain: u32,
    /// Left piezo channel, oldest-first. Physically the **odd/second** interleave
    /// slot (CONFIRMED by live per-side occupancy test on a Pod 4, 2026-07-18).
    pub left: Vec<i32>,
    /// Right piezo channel, oldest-first. Physically the **even/first** interleave
    /// slot (CONFIRMED by live per-side occupancy test on a Pod 4, 2026-07-18).
    pub right: Vec<i32>,
}

/// Pod 4 auxiliary sensor packet (opcode `0x35`, observed fixed len 176).
///
/// Header (16 bytes): `35 | subtype | reserved[8] | rate_hz:u16 | timestamp_ms:u32`.
/// Body: 4 interleaved channels, each sample a big-endian `i32`. 10 samples per
/// channel at 200 Hz per 50 ms frame.
///
/// PHYSICAL MEANING UNKNOWN: candidates are the LIS 3-axis accelerometer or raw
/// FDC1004 capacitance (`meas0..3`). In the unoccupied capture channels 0/1 sit at
/// ~`0x7FFF/0x8000` and channels 2/3 near `0x03xx`. Needs a labeled capture to fix.
#[derive(Debug, PartialEq, Clone)]
pub struct Pod4AuxData {
    /// Firmware subtype/format byte. CONFIRMED constant `0x02` in all captures.
    pub subtype: u8,
    /// Reserved header bytes. CONFIRMED all-zero in all captures.
    pub reserved: [u8; 8],
    /// Sample rate in Hz. CONFIRMED = 200 (matches 10 samples / 50 ms frame).
    pub rate_hz: u16,
    /// Device uptime in milliseconds (shared counter, same units as piezo/cap).
    pub timestamp_ms: u32,
    /// 4 interleaved channels, oldest-first.
    pub channels: [Vec<i32>; 4],
}

#[derive(Debug, PartialEq, Clone)]
pub struct CapacitanceData {
    pub sequence: u32,
    /// Six capacitance channels. Physical side mapping CONFIRMED on a live Pod 4
    /// by per-side occupancy (2026-07-18, vs empty baseline ~[1084,1279,1679,1614,
    /// 1332,909]): a person on the **left** drove channel **1** hardest (~+2100,
    /// with 2 secondary); a person on the **right** drove channel **4** hardest
    /// (~+3800, with 3 secondary). So `[edge, LEFT, left2, right2, RIGHT, edge]`.
    /// Channels 0 and 5 barely respond (edge/reference).
    pub values: [u16; 6],
}

impl CapacitanceData {
    /// Primary left-side presence capacitance (channel 1). Higher = occupied.
    pub fn left(&self) -> u16 {
        self.values[1]
    }
    /// Primary right-side presence capacitance (channel 4). Higher = occupied.
    pub fn right(&self) -> u16 {
        self.values[4]
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct TemperatureData {
    /// ordered LTR
    /// centidegrees celcius
    pub bed: [u16; 8],
    /// centidegrees celcius
    pub ambient: u16,
    /// centidegrees celcius
    pub humidity: u16,
    /// centidegrees celcius
    pub microcontroller: u16,
}

#[derive(Debug, PartialEq, Clone)]
pub struct PiezoData {
    pub freq: u32,
    pub sequence: u32,
    pub gain: (u16, u16),
    pub left_samples: Vec<u16>,
    pub right_samples: Vec<u16>,
}

impl Packet for SensorPacket {
    // responses are cmd + 0x80
    fn parse(buf: BytesMut) -> Result<Self, PacketError> {
        match buf[0] {
            0x07 => packet::parse_message("Sensor/Message", buf).map(SensorPacket::Message),
            0x31 => Self::parse_init(buf),
            0x32 => Self::parse_piezo(buf),
            0x33 => Self::parse_capacitance(buf),
            0x34 => Self::parse_pod4_piezo(buf),
            0x35 => Self::parse_pod4_aux(buf),
            0x81 => packet::parse_pong("Sensor/Pong", buf).map(SensorPacket::Pong),
            0x82 => packet::parse_hardware_info("Sensor/HardwareInfo", buf)
                .map(SensorPacket::HardwareInfo),
            0x84 => Self::parse_get_firmware(buf),
            0x90 => packet::parse_jumping_to_firmware("Sensor/JumpingToFirmware", buf)
                .map(SensorPacket::JumpingToFirmware),
            0xA1 => Self::parse_piezo_freq_set(buf),
            0xA8 => Self::parse_piezo_enabled(buf),
            0xAB => Self::parse_piezo_gain_set(buf),
            0xAC => Self::parse_alarm_set(buf),
            0xAE => Self::parse_vibration_enabled(buf),
            0xAF => Self::parse_temperature(buf),
            _ => Err(PacketError::Unexpected {
                subsystem_name: "Sensor",
                buf: buf.freeze(),
            }),
        }
    }
}

impl SensorPacket {
    fn parse_get_firmware(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_size("Sensor/GetFirmware", &buf, 2)?;
        Ok(SensorPacket::GetFirmware(buf[1]))
    }

    fn parse_alarm_set(buf: BytesMut) -> Result<Self, PacketError> {
        // Pod 3 F0 acks with 2 bytes; the Pod 4 G0 ack is 3 (observed live:
        // AC 00 01 — trailing byte meaning unknown, plausibly the side).
        validate_packet_at_least("Sensor/AlarmSet", &buf, 2)?;
        Ok(SensorPacket::AlarmSet(buf[1]))
    }

    fn parse_piezo_gain_set(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_size("Sensor/PiezoGainSet", &buf, 6)?;
        Ok(SensorPacket::PiezoGainSet(
            u16::from_be_bytes([buf[2], buf[3]]),
            u16::from_be_bytes([buf[4], buf[5]]),
        ))
    }

    fn parse_piezo_freq_set(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_size("Sensor/PiezoFreqSet", &buf, 2)?;
        Ok(SensorPacket::PiezoFreqSet(buf[1]))
    }

    fn parse_piezo_enabled(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_size("Sensor/PiezoEnabled", &buf, 2)?;
        Ok(SensorPacket::PiezoEnabled(buf[1]))
    }

    fn parse_vibration_enabled(buf: BytesMut) -> Result<Self, PacketError> {
        // Pod 3 acks with 3 bytes, the Pod 4 (STM32G0 "pod5") firmware with 2.
        // Rejecting the short form left `vibration_enabled` false forever, so
        // EnableVibration was re-sent every 800ms indefinitely (observed live).
        if buf.len() == 2 {
            return Ok(SensorPacket::VibrationEnabled(buf[1], 0));
        }
        validate_packet_size("Sensor/VibrationEnabled", &buf, 3)?;
        Ok(SensorPacket::VibrationEnabled(buf[1], buf[2]))
    }

    // TODO FIXME new packet 31 00 00 00 0c 00 00 1d 22 00
    /// 31 00 00 00 0b 00 00 XX XX 00
    fn parse_init(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_size("Sensor/Init", &buf, 10)?;

        if buf[1..=6] != hex!("00 00 00 0b 00 00") || buf[9] != 0 {
            log::warn!("Unexpected init packet: {buf:02X?}");
        }

        Ok(SensorPacket::Init(u16::from_be_bytes([buf[7], buf[8]])))
    }

    /// Direct indexing is pretty nasty here, but _should_ be faster than using BytesMut as a buffer.
    /// Strict tests are used to enforce behavior.
    /// If you have a better method please reach out to me!!
    fn parse_capacitance(buf: BytesMut) -> Result<Self, PacketError> {
        // example bad packet: 33 08 46 30 0c 00 00 00 00 00 00 7d 5d 01 00 a3 02 00 fc 03 01 18 04 01 c3 05 01

        validate_packet_size("Sensor/Capacitance", &buf, 27)?;

        let indices_valid = buf[9] == 0
            && buf[12] == 1
            && buf[15] == 2
            && buf[18] == 3
            && buf[21] == 4
            && buf[24] == 5;

        if !indices_valid {
            return Err(invalid_structure(
                "Sensor/Capacitance",
                "invalid indices".to_string(),
                buf,
            ));
        }

        Ok(Self::Capacitance(CapacitanceData {
            sequence: u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]),
            values: [
                u16::from_be_bytes([buf[10], buf[11]]),
                u16::from_be_bytes([buf[13], buf[14]]),
                u16::from_be_bytes([buf[16], buf[17]]),
                u16::from_be_bytes([buf[19], buf[20]]),
                u16::from_be_bytes([buf[22], buf[23]]),
                u16::from_be_bytes([buf[25], buf[26]]),
            ],
        }))
    }

    /// see parse_capacitance doc comment
    fn parse_temperature(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_size("Sensor/Temperature", &buf, 35)?;

        let indices_valid = buf[1] == 0
            && buf[2] == 0
            && buf[5] == 1
            && buf[8] == 2
            && buf[11] == 3
            && buf[14] == 4
            && buf[17] == 5
            && buf[20] == 6
            && buf[23] == 7
            && buf[26] == 8
            && buf[29] == 9
            && buf[32] == 10;

        if !indices_valid {
            return Err(invalid_structure(
                "Sensor/Temperature",
                "invalid indices or spacer".to_string(),
                buf,
            ));
        }

        Ok(SensorPacket::Temperature(TemperatureData {
            bed: [
                u16::from_be_bytes([buf[3], buf[4]]),
                u16::from_be_bytes([buf[6], buf[7]]),
                u16::from_be_bytes([buf[9], buf[10]]),
                u16::from_be_bytes([buf[12], buf[13]]),
                u16::from_be_bytes([buf[15], buf[16]]),
                u16::from_be_bytes([buf[18], buf[19]]),
                u16::from_be_bytes([buf[21], buf[22]]),
                u16::from_be_bytes([buf[24], buf[25]]),
            ],
            ambient: u16::from_be_bytes([buf[27], buf[28]]),
            humidity: u16::from_be_bytes([buf[30], buf[31]]),
            microcontroller: u16::from_be_bytes([buf[33], buf[34]]),
        }))
    }

    /// see parse_capacitance doc comment
    /// common sizes: 174, 254, 202, 142, 178
    fn parse_piezo(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_at_least("Sensor/Piezo", &buf, 20)?;

        if buf[1] != 0x02 {
            log::warn!("Unexpected Piezo header: {:02X}", buf[1]);
        }

        let freq = u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]);
        let sequence = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]);
        let gain = (
            u16::from_be_bytes([buf[10], buf[11]]),
            u16::from_be_bytes([buf[12], buf[13]]),
        );

        let num_samples = (buf.len() - 14) >> 2;
        let mut left_samples = Vec::with_capacity(num_samples);
        let mut right_samples = Vec::with_capacity(num_samples);

        for sample_num in 0..num_samples {
            let idx = 14 + (sample_num << 2);
            left_samples.push(u16::from_be_bytes([buf[idx], buf[idx + 1]]));
            right_samples.push(u16::from_be_bytes([buf[idx + 2], buf[idx + 3]]));
        }

        Ok(SensorPacket::Piezo(PiezoData {
            freq,
            sequence,
            gain,
            left_samples,
            right_samples,
        }))
    }

    /// Pod 4 dual-channel ADS piezo (opcode 0x34).
    /// Header is 14 bytes; the body is interleaved `left,right` big-endian `i32`.
    const POD4_PIEZO_HEADER: usize = 14;
    fn parse_pod4_piezo(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_at_least("Sensor/Pod4Piezo", &buf, Self::POD4_PIEZO_HEADER)?;

        let body = buf.len() - Self::POD4_PIEZO_HEADER;
        // must be a whole number of left/right i32 pairs (8 bytes)
        if !body.is_multiple_of(8) {
            return Err(invalid_structure(
                "Sensor/Pod4Piezo",
                format!("body {body} is not a multiple of 8 (l/r i32 pair)"),
                buf,
            ));
        }

        let subtype = buf[1];
        let freq = u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]);
        let timestamp_ms = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]);
        let gain = u32::from_be_bytes([buf[10], buf[11], buf[12], buf[13]]);

        let pairs = body / 8;
        let mut left = Vec::with_capacity(pairs);
        let mut right = Vec::with_capacity(pairs);
        for pair in 0..pairs {
            let idx = Self::POD4_PIEZO_HEADER + (pair << 3);
            // Physical side mapping CONFIRMED on live Pod 4 (occupancy test
            // 2026-07-18: one person on each side vs empty baseline): the
            // FIRST/even interleave slot is the RIGHT piezo, the SECOND/odd slot
            // is the LEFT piezo. (opensleep's Pod-3 order was the opposite.)
            right.push(i32::from_be_bytes([
                buf[idx],
                buf[idx + 1],
                buf[idx + 2],
                buf[idx + 3],
            ]));
            left.push(i32::from_be_bytes([
                buf[idx + 4],
                buf[idx + 5],
                buf[idx + 6],
                buf[idx + 7],
            ]));
        }

        Ok(SensorPacket::Pod4Piezo(Pod4PiezoData {
            subtype,
            freq,
            timestamp_ms,
            gain,
            left,
            right,
        }))
    }

    /// Pod 4 4-channel auxiliary stream (opcode 0x35).
    /// Header is 16 bytes; the body is 4 interleaved channels of big-endian `i32`.
    const POD4_AUX_HEADER: usize = 16;
    const POD4_AUX_CHANNELS: usize = 4;
    fn parse_pod4_aux(buf: BytesMut) -> Result<Self, PacketError> {
        validate_packet_at_least("Sensor/Pod4Aux", &buf, Self::POD4_AUX_HEADER)?;

        let body = buf.len() - Self::POD4_AUX_HEADER;
        // must be a whole number of 4-channel i32 groups (16 bytes)
        if !body.is_multiple_of(Self::POD4_AUX_CHANNELS * 4) {
            return Err(invalid_structure(
                "Sensor/Pod4Aux",
                format!("body {body} is not a multiple of 16 (4x i32)"),
                buf,
            ));
        }

        let subtype = buf[1];
        let reserved: [u8; 8] = buf[2..10].try_into().unwrap();
        let rate_hz = u16::from_be_bytes([buf[10], buf[11]]);
        let timestamp_ms = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);

        let records = body / 4;
        let mut channels: [Vec<i32>; 4] = Default::default();
        for rec in 0..records {
            let idx = Self::POD4_AUX_HEADER + (rec << 2);
            let val = i32::from_be_bytes([buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]);
            channels[rec % Self::POD4_AUX_CHANNELS].push(val);
        }

        Ok(SensorPacket::Pod4Aux(Pod4AuxData {
            subtype,
            reserved,
            rate_hz,
            timestamp_ms,
            channels,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{Bytes, BytesMut};
    use hex_literal::hex;

    #[test]
    fn test_pong() {
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&hex!("81 00 42")[..])),
            Ok(SensorPacket::Pong(false))
        );

        assert_eq!(
            SensorPacket::parse(BytesMut::from(&hex!("81 00 46")[..])),
            Ok(SensorPacket::Pong(true))
        );

        assert!(SensorPacket::parse(BytesMut::from(&hex!("81 01 01")[..])).is_err());
        assert!(SensorPacket::parse(BytesMut::from(&hex!("81 00 01")[..])).is_err());
    }

    #[test]
    fn test_jumping_to_firmware() {
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0x90, 0x01][..])),
            Ok(SensorPacket::JumpingToFirmware(1))
        );
        assert!(SensorPacket::parse(BytesMut::from(&[0x90][..])).is_err());
    }

    #[test]
    fn test_set_gain() {
        let data = hex!("AB 00 01 95 01 95");
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&data[..])),
            Ok(SensorPacket::PiezoGainSet(405, 405))
        );
        assert!(SensorPacket::parse(BytesMut::from(&hex!("AB 01")[..])).is_err());
    }

    #[test]
    fn test_vibration_enabled() {
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0xAE, 0, 2][..])),
            Ok(SensorPacket::VibrationEnabled(0, 2))
        );
        // Pod 4 short-form ack.
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0xAE, 1][..])),
            Ok(SensorPacket::VibrationEnabled(1, 0))
        );
        assert!(SensorPacket::parse(BytesMut::from(&[0xAE][..])).is_err());
    }

    #[test]
    fn test_get_fw() {
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0x84, 4][..])),
            Ok(SensorPacket::GetFirmware(4))
        );
        assert!(SensorPacket::parse(BytesMut::from(&[0x84][..])).is_err());
    }

    #[test]
    fn test_message() {
        let data = hex!("07 00 48 65 6C 6C 6F");
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&data[..])),
            Ok(SensorPacket::Message("Hello".into()))
        );

        let invalid_utf8 = hex!("07 00 FF");
        assert!(SensorPacket::parse(BytesMut::from(&invalid_utf8[..])).is_err());
    }

    #[test]
    fn test_capacitance() {
        let mut data = hex!(
            "33 01 02 03 04 00 00 00 00"
            "00 01 02"
            "01 03 04"
            "02 05 06"
            "03 07 08"
            "04 09 0A"
            "05 0B 0C"
        );
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&data[..])),
            Ok(SensorPacket::Capacitance(CapacitanceData {
                sequence: 0x01020304,
                values: [0x0102, 0x0304, 0x0506, 0x0708, 0x090A, 0x0B0C]
            }))
        );

        // test bad index
        data[9] = 99;
        assert!(SensorPacket::parse(BytesMut::from(&data[..])).is_err());
    }

    #[test]
    fn test_piezo() {
        let data = hex!(
            "32 02 00 00"
            "03 E8"
            "00 00 00 01"
            "00 01"
            "00 01"
            "00 01 00 02"
            "00 03 00 04"
        );
        let parsed = SensorPacket::parse(BytesMut::from(&data[..])).unwrap();
        match parsed {
            SensorPacket::Piezo(piezo) => {
                assert_eq!(piezo.freq, 1000);
                assert_eq!(piezo.sequence, 1);
                assert_eq!(piezo.gain, (1, 1));
                assert_eq!(piezo.left_samples, vec![1, 3]);
                assert_eq!(piezo.right_samples, vec![2, 4]);
            }
            _ => panic!("Wrong packet type"),
        }

        assert!(SensorPacket::parse(BytesMut::from(&hex!("32 02 00 00")[..])).is_err());
    }

    #[test]
    fn test_bed_temp() {
        let data = hex!(
            "AF 00"
            "00 01 02"
            "01 03 04"
            "02 05 06"
            "03 07 08"
            "04 09 0A"
            "05 0B 0C"
            "06 0D 0E"
            "07 0F 10"
            "08 11 12"
            "09 13 14"
            "0A 15 16"
        );
        let parsed = SensorPacket::parse(BytesMut::from(&data[..])).unwrap();
        match parsed {
            SensorPacket::Temperature(temp) => {
                assert_eq!(
                    temp.bed,
                    [
                        0x0102, 0x0304, 0x0506, 0x0708, 0x090A, 0x0B0C, 0x0D0E, 0x0F10
                    ]
                );
                assert_eq!(temp.ambient, 0x1112);
                assert_eq!(temp.humidity, 0x1314);
                assert_eq!(temp.microcontroller, 0x1516);
            }
            _ => panic!("Wrong packet type"),
        }

        let mut bad_index = data;
        bad_index[32] = 0x99;
        let result = SensorPacket::parse(BytesMut::from(&bad_index[..]));
        assert!(result.is_err());
    }

    #[test]
    fn test_alarm_set() {
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0xAC, 0x01][..])),
            Ok(SensorPacket::AlarmSet(0x01))
        );
        // Pod 4 G0 form (3 bytes), observed live on SetAlarm.
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0xAC, 0x00, 0x01][..])),
            Ok(SensorPacket::AlarmSet(0x00))
        );
        assert!(SensorPacket::parse(BytesMut::from(&[0xAC][..])).is_err());
    }

    // --- Pod 4 (STM32G0) real captured frames ---
    // Source: backup/captures/cap_ttymxc0_921600_10s.bin, first frame of each opcode.
    // CRC-validated by the throwaway analyzer before hardcoding here.

    /// Real Pod-4 capacitance frame (opcode 0x33) — parses with the Pod-3 layout.
    #[test]
    fn test_pod4_capacitance_real() {
        let data = hex!(
            "33 00 0C E8 8C 00 00 00 00 00 04 3D"
            "01 04 FF 02 06 91 03 06 4E 04 05 33"
            "05 03 8D"
        );
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&data[..])),
            Ok(SensorPacket::Capacitance(CapacitanceData {
                // 845964 ms uptime
                sequence: 0x000C_E88C,
                // both sides unoccupied
                values: [1085, 1279, 1681, 1614, 1331, 909],
            }))
        );
    }

    /// Real Pod-4 dual-channel piezo frame (opcode 0x34, len 214).
    #[test]
    fn test_pod4_piezo_real() {
        let data = hex!(
            "34 41 00 00 01 F4 00 0C E8 03 00 00"
            "01 90 FF F9 6E D0 FF F9 2A B3 FF F9"
            "6F EE FF F9 2A B3 FF F9 6E 19 FF F9"
            "30 6F FF F9 6F 29 FF F9 31 7C FF F9"
            "6E 22 FF F9 31 7C FF F9 6E 3C FF F9"
            "32 D5 FF F9 6C 36 FF F9 34 E3 FF F9"
            "6A 3F FF F9 37 DD FF F9 69 74 FF F9"
            "35 F0 FF F9 69 80 FF F9 34 66 FF F9"
            "69 75 FF F9 35 24 FF F9 69 54 FF F9"
            "32 7A FF F9 6A 20 FF F9 2D 78 FF F9"
            "6B 2F FF F9 2C 11 FF F9 6B F1 FF F9"
            "2D C4 FF F9 6D AE FF F9 32 25 FF F9"
            "6E 36 FF F9 32 61 FF F9 6B E9 FF F9"
            "31 35 FF F9 69 66 FF F9 2F 54 FF F9"
            "68 FC FF F9 2D 62 FF F9 69 FC FF F9"
            "2D 8D FF F9 6A 62 FF F9 2D CF FF F9"
            "6C 03 FF F9 2C 1E FF F9 6D 44 FF F9"
            "2A 5D FF F9 6E 64 FF F9 29 37"
        );
        let parsed = SensorPacket::parse(BytesMut::from(&data[..])).unwrap();
        match parsed {
            SensorPacket::Pod4Piezo(p) => {
                assert_eq!(p.subtype, 0x41);
                assert_eq!(p.freq, 500);
                assert_eq!(p.timestamp_ms, 0x000C_E803); // 845827 ms
                assert_eq!(p.gain, 400); // == telemetry gainLeft/gainRight
                assert_eq!(p.left.len(), 25);
                assert_eq!(p.right.len(), 25);
                // first interleaved pair, big-endian i32 (24-bit ADS sign-extended)
                // First/even interleave slot = RIGHT, second/odd = LEFT (live Pod 4).
                assert_eq!(p.right[0], -430384); // 0xFFF9_6ED0 (even slot -> right)
                assert_eq!(p.left[0], -447821); // 0xFFF9_2AB3 (odd slot -> left)
                // both channels smooth & bounded around their bias
                for &v in p.left.iter().chain(p.right.iter()) {
                    assert!((-460_000..-420_000).contains(&v), "sample out of range: {v}");
                }
            }
            other => panic!("Wrong packet type: {other:?}"),
        }

        // truncated / misaligned body must error
        assert!(SensorPacket::parse(BytesMut::from(&hex!("34 41 00 00 01 F4")[..])).is_err());
    }

    /// Real Pod-4 4-channel auxiliary frame (opcode 0x35, len 176).
    #[test]
    fn test_pod4_aux_real() {
        let data = hex!(
            "35 02 00 00 00 00 00 00 00 00 00 C8"
            "00 0C E7 EF 00 00 80 00 00 00 7F FF"
            "00 00 03 3E 00 00 03 5E 00 00 7F FF"
            "00 00 7F FF 00 00 03 3E 00 00 03 5E"
            "00 00 7F FF 00 00 7F FF 00 00 03 3E"
            "00 00 03 5E 00 00 80 00 00 00 80 00"
            "00 00 03 4F 00 00 03 5E 00 00 7F FF"
            "00 00 7F FF 00 00 03 4F 00 00 03 5E"
            "00 00 7F FF 00 00 7F FF 00 00 03 4F"
            "00 00 03 5E 00 00 7F FF 00 00 7F FE"
            "00 00 03 4F 00 00 03 5E 00 00 7F FF"
            "00 00 80 00 00 00 03 4F 00 00 03 53"
            "00 00 7F FF 00 00 7F FF 00 00 03 4F"
            "00 00 03 53 00 00 7F FE 00 00 7F FF"
            "00 00 03 4F 00 00 03 53"
        );
        let parsed = SensorPacket::parse(BytesMut::from(&data[..])).unwrap();
        match parsed {
            SensorPacket::Pod4Aux(a) => {
                assert_eq!(a.subtype, 0x02);
                assert_eq!(a.reserved, [0u8; 8]);
                assert_eq!(a.rate_hz, 200);
                assert_eq!(a.timestamp_ms, 0x000C_E7EF); // 845807 ms
                assert_eq!(a.channels[0].len(), 10);
                assert_eq!(a.channels[3].len(), 10);
                // ch0/ch1 sit near +0x8000, ch2/ch3 near +0x03xx
                assert_eq!(a.channels[0][0], 0x8000); // 32768
                assert_eq!(a.channels[1][0], 0x7FFF); // 32767
                assert_eq!(a.channels[2][0], 0x033E); // 830
                assert_eq!(a.channels[3][0], 0x035E); // 862
            }
            other => panic!("Wrong packet type: {other:?}"),
        }

        // misaligned body must error
        assert!(
            SensorPacket::parse(BytesMut::from(&hex!("35 02 00 00 00 00 00 00 00 00 00 C8 00 00 00 00 00")[..]))
                .is_err()
        );
    }

    #[test]
    fn test_unexpected() {
        assert_eq!(
            SensorPacket::parse(BytesMut::from(&[0x99][..])),
            Err(PacketError::Unexpected {
                subsystem_name: "Sensor",
                buf: Bytes::from(&[0x99][..])
            })
        );
    }
}
