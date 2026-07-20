use strum_macros::{AsRefStr, Display, IntoStaticStr};

use crate::{
    codec::{CommandTrait, command},
    frozen::packet::FrozenTarget,
    packet::BedSide,
};

#[derive(Debug, Clone, Display, AsRefStr, IntoStaticStr)]
pub enum FrozenCommand {
    Ping,
    GetHardwareInfo,
    #[allow(dead_code)]
    GetFirmware,
    JumpToFirmware,
    #[allow(dead_code)]
    Prime,
    #[allow(dead_code)]
    /// call every 10 seconds
    SetTargetTemperature {
        side: BedSide,
        tar: FrozenTarget,
    },
    GetTemperatures,
    Random(u8),
}

impl CommandTrait for FrozenCommand {
    fn to_bytes(&self) -> Vec<u8> {
        use FrozenCommand::*;
        match self {
            // 0x05 is sometimes the first command at boot unclear purpose
            Ping => command(vec![0x01]),
            GetHardwareInfo => command(vec![0x02]),
            GetFirmware => command(vec![0x04]),
            JumpToFirmware => command(vec![0x10]),
            GetTemperatures => command(vec![0x41]),

            /*

            After sending 0x50 command, we get back:

            Response In Test #1:
            D0 00
            28 FF C2 E9 A5 21 56 F3 07 FB
            28 FF 3A CF 23 22 31 12 09 34
            28 FF CE 0B 2C E2 23 56 0A 0F
            28 FF 07 E5 2C E2 20 39 0A 0F

            Response In Test #2:
            D0 00
            28 FF C2 E9 A5 21 56 F3 08 08
            28 FF 3A CF 23 22 31 12 09 3A
            28 FF CE 0B 2C E2 23 56 0A 15
            28 FF 07 E5 2C E2 20 39 0A 15


            <- GOT 0x50 RESPONSE SHOWN ABOVE #2 ->
            Temperature update - Left: 2581, Right: 2581, Heatsink: 2362, Error: 8
            Message: FW: pid[heatsink] 3.062500 0.693750 0.693750 0.000000 0.000000
            Message: FW: pump[left] slow @ 6.030475V 0.169202A
            Message: FW: pump[right] slow @ 6.044009V 0.161510A
            Message: FW: pid[left] 25.812500 0.090498 -0.003750 0.094248 0.000000
            Message: FW: pid[right] 25.812500 0.094561 -0.003750 0.098311 0.000000

            */

            /*
            0x51 -> D1 00 (Flash/calibration status?)

            UTF-8 decode error: invalid utf-8 sequence of 1 bytes from index 16
            Message: FW: flash locked
            Message: FW: cal_info valid

            */
            Prime => command(vec![0x52]),
            Random(cmd) => command(vec![*cmd]),
            SetTargetTemperature { side, tar } => command(vec![
                0x40,
                *side as u8,
                tar.enabled as u8,
                (tar.temp >> 8) as u8,
                tar.temp as u8,
            ]),
        }
    }
}

impl FrozenTarget {
    /// Nudge the setpoint (±0.01 °C steps) until the encoded
    /// SetTargetTemperature frame contains no 0x7E byte after the leading
    /// frame delimiter.
    ///
    /// The wire protocol has no byte-stuffing, and the frozen MCU's parser
    /// resyncs on every 0x7E it sees — a frame whose payload or CRC happens to
    /// contain 0x7E is silently dropped by the MCU (no echo, no effect).
    /// Observed live: (Left, 3111) = 88 °F encodes to `7E 05 40 00 01 0C 27
    /// C6 7E` — CRC low byte 0x7E — so a left-side 88 °F setpoint could never
    /// be set while e.g. (Right, 3111) worked fine. Callers must use the
    /// nudged value both for sending AND for comparing against the MCU's
    /// TargetUpdate echo, or the scheduler re-sends forever.
    pub fn delimiter_safe(self, side: BedSide) -> Self {
        const DELIM: u8 = 0x7E;
        for delta in [0i32, -1, 1, -2, 2, -3, 3] {
            let Ok(temp) = u16::try_from(self.temp as i32 + delta) else {
                continue;
            };
            let candidate = FrozenTarget { temp, ..self.clone() };
            let frame = FrozenCommand::SetTargetTemperature {
                side,
                tar: candidate.clone(),
            }
            .to_bytes();
            if !frame[1..].contains(&DELIM) {
                return candidate;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CommandTrait;
    use hex_literal::hex;

    #[test]
    fn delimiter_safe_nudges_colliding_frames() {
        // (Left, 3111): CRC low byte is 0x7E -> must nudge away from 3111.
        let tar = FrozenTarget { enabled: true, temp: 3111 };
        let raw = FrozenCommand::SetTargetTemperature { side: BedSide::Left, tar: tar.clone() }.to_bytes();
        assert!(raw[1..].contains(&0x7E), "premise: left/3111 frame collides");
        let safe = tar.clone().delimiter_safe(BedSide::Left);
        assert_ne!(safe.temp, 3111);
        assert!((safe.temp as i32 - 3111).abs() <= 3);
        assert!(safe.enabled);
        let frame = FrozenCommand::SetTargetTemperature { side: BedSide::Left, tar: safe }.to_bytes();
        assert!(!frame[1..].contains(&0x7E));

        // (Right, 3111) doesn't collide -> unchanged.
        assert_eq!(tar.clone().delimiter_safe(BedSide::Right).temp, 3111);

        // Exhaustive: every nudged frame is clean over the physical range.
        for side in [BedSide::Left, BedSide::Right] {
            for temp in 0..=4500u16 {
                let safe = FrozenTarget { enabled: true, temp }.delimiter_safe(side);
                let frame =
                    FrozenCommand::SetTargetTemperature { side, tar: safe }.to_bytes();
                assert!(!frame[1..].contains(&0x7E), "side {side:?} temp {temp}");
            }
        }
    }

    #[test]
    fn test_ping() {
        assert_eq!(
            FrozenCommand::Ping.to_bytes(),
            hex!("7E 01 01 DC BD").to_vec()
        );
    }

    #[test]
    fn test_gethardwareinfo() {
        assert_eq!(
            FrozenCommand::GetHardwareInfo.to_bytes(),
            hex!("7E 01 02 EC DE").to_vec()
        );
    }

    #[test]
    fn test_getfirmware() {
        assert_eq!(
            FrozenCommand::GetFirmware.to_bytes(),
            hex!("7E 01 04 8C 18").to_vec()
        );
    }

    #[test]
    fn test_jumptofirmware() {
        assert_eq!(
            FrozenCommand::JumpToFirmware.to_bytes(),
            hex!("7E 01 10 DE AD").to_vec()
        );
    }

    #[test]
    fn test_prime() {
        assert_eq!(
            FrozenCommand::Prime.to_bytes(),
            hex!("7E 01 52 b6 2b").to_vec()
        );
    }

    #[test]
    fn test_temp() {
        let cmd = FrozenCommand::SetTargetTemperature {
            side: BedSide::Left,
            tar: FrozenTarget {
                enabled: true,
                temp: 3600,
            },
        };
        assert_eq!(cmd.to_bytes(), hex!("7E 05 40 00 01 0E 10 E6 A8").to_vec());
    }
}
