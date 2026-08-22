use crate::{checksum, packet::Packet};
use bytes::{Buf, BufMut, BytesMut};
use std::marker::PhantomData;
use tokio_util::codec::{Decoder, Encoder};

pub const START: u8 = 0x7E;

/// The LSP frame length field is a single byte, so a payload can never exceed
/// this. Anything longer (e.g. a future firmware-flash chunk) must be split
/// before framing.
pub const MAX_PAYLOAD_LEN: usize = 255;

pub struct PacketCodec<P: Packet> {
    _phantom: PhantomData<P>,
}

impl<P: Packet> PacketCodec<P> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<P: Packet> Default for PacketCodec<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Packet> Decoder for PacketCodec<P> {
    type Item = P;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            let start_pos = memchr::memchr(START, src);

            match start_pos {
                Some(pos) => {
                    // skip bytes until pos
                    if pos > 0 {
                        src.advance(pos);
                    }

                    if src.len() < 2 {
                        return Ok(None); // need more data
                    }

                    let len = src[1] as usize;
                    let total_packet_size = 1 + 1 + len + 2; // start + len + payload + checksum

                    if src.len() < total_packet_size {
                        return Ok(None); // need more data
                    }

                    // get payload
                    let payload_start = 2;
                    let payload_end = 2 + len;
                    let payload = &src[payload_start..payload_end];
                    if payload.is_empty() {
                        log::error!("Empty packet");
                        src.advance(1);
                        continue;
                    }

                    // validate checksum wo/ consuming bytes
                    let checksum_bytes = &src[payload_end..payload_end + 2];
                    let actual_checksum =
                        u16::from_be_bytes([checksum_bytes[0], checksum_bytes[1]]);
                    let expected_checksum = checksum::compute(payload);

                    if actual_checksum != expected_checksum {
                        // bad checksum -> skip only start byte and try again
                        src.advance(1);
                        continue;
                    }

                    // checksum is valid -> try to parse packet
                    src.advance(2); // skip start & len
                    let payload = src.split_to(len); // take payload out
                    src.advance(2); // skip checksum

                    match P::parse(payload) {
                        Ok(packet) => {
                            // consume valid packets
                            return Ok(Some(packet));
                        }
                        Err(e) => {
                            log::error!("{e}");
                            continue;
                        }
                    }
                }
                None => {
                    // no start byte found -> clear buffer
                    src.clear();
                    return Ok(None);
                }
            }
        }
    }
}

pub fn command(mut payload: Vec<u8>) -> Vec<u8> {
    // A hard assert, not debug_assert: `payload.len() as u8` would wrap mod
    // 256, emitting a LEN that disagrees with the checksum — a corrupt frame
    // the MCU silently drops. Failing loudly beats sending that mid-flash,
    // and release builds are exactly where a flash path would run.
    assert!(
        payload.len() <= MAX_PAYLOAD_LEN,
        "LSP payload of {} bytes exceeds the one-byte length field (max {MAX_PAYLOAD_LEN}); \
         split it before framing",
        payload.len(),
    );
    let mut res = Vec::with_capacity(payload.len() + 4);
    let checksum = checksum::compute(&payload);
    res.push(START);
    res.push(payload.len() as u8);
    res.append(&mut payload);
    res.push((checksum >> 8) as u8);
    res.push(checksum as u8);
    res
}

pub trait CommandTrait {
    fn to_bytes(&self) -> Vec<u8>;
}

impl<P: Packet, C: CommandTrait> Encoder<C> for PacketCodec<P> {
    type Error = std::io::Error;

    fn encode(&mut self, item: C, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.put_slice(&item.to_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checksum;
    use crate::frozen::command::FrozenCommand;
    use crate::frozen::packet::FrozenPacket;
    use bytes::BytesMut;

    #[test]
    fn test_crc_known_vector() {
        // golden vector: SetTargetTemperature payload -> CRC-CCITT (0x1D0F seed)
        assert_eq!(checksum::compute(&[0x40, 0x00, 0x01, 0x0E, 0x10]), 0xE6A8);
    }

    #[test]
    fn test_frame_layout() {
        // command() must produce: START | LEN | payload | CRC(BE)
        let framed = command(vec![0x40, 0x00, 0x01, 0x0E, 0x10]);
        assert_eq!(framed[0], START);
        assert_eq!(framed[1], 5); // payload len
        assert_eq!(&framed[2..7], &[0x40, 0x00, 0x01, 0x0E, 0x10]);
        assert_eq!(u16::from_be_bytes([framed[7], framed[8]]), 0xE6A8);
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        // encode a response frame (opcode 0x81 = Pong, firmware) and decode it back
        let payload = vec![0x81u8, 0x00, 0x46];
        let framed = command(payload);

        let mut codec = PacketCodec::<FrozenPacket>::new();
        let mut buf = BytesMut::from(&framed[..]);
        let decoded = codec.decode(&mut buf).expect("decode ok");
        assert_eq!(decoded, Some(FrozenPacket::Pong(true)));
        // decoder consumed the whole frame
        assert!(buf.is_empty());
    }

    #[test]
    fn test_encoder_matches_command() {
        // the Encoder impl must produce exactly what CommandTrait::to_bytes does
        let cmd = FrozenCommand::Ping;
        let mut codec = PacketCodec::<FrozenPacket>::new();
        let mut dst = BytesMut::new();
        codec.encode(cmd.clone(), &mut dst).unwrap();
        assert_eq!(&dst[..], &cmd.to_bytes()[..]);
    }

    #[test]
    fn test_max_payload_frames_fine() {
        // exactly MAX_PAYLOAD_LEN must still frame with a correct LEN byte
        let framed = command(vec![0u8; MAX_PAYLOAD_LEN]);
        assert_eq!(framed[1] as usize, MAX_PAYLOAD_LEN);
        assert_eq!(framed.len(), MAX_PAYLOAD_LEN + 4);
    }

    #[test]
    #[should_panic(expected = "exceeds the one-byte length field")]
    fn test_oversize_payload_panics() {
        // 256 bytes would wrap the LEN byte to 0 -> corrupt frame; must panic
        command(vec![0u8; MAX_PAYLOAD_LEN + 1]);
    }

    #[test]
    fn test_bad_crc_resyncs() {
        // corrupt the CRC: decoder should skip the START byte and not yield a packet
        let mut framed = command(vec![0x81u8, 0x00, 0x46]);
        let last = framed.len() - 1;
        framed[last] ^= 0xFF; // break checksum

        let mut codec = PacketCodec::<FrozenPacket>::new();
        let mut buf = BytesMut::from(&framed[..]);
        let decoded = codec.decode(&mut buf).expect("decode ok");
        assert_eq!(decoded, None);
    }
}
