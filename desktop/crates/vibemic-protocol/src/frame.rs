//! 帧编解码与流式重组(与 `device/main/vm_protocol.c` 行为一致)。

/// CRC16-CCITT(多项式 0x1021,init 0xFFFF,无反射,xorout 0)。
/// 已知向量:`"123456789"` → `0x29B1`。
pub fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub const MAGIC: [u8; 2] = [0x56, 0x4D];
pub const VERSION: u8 = 1;
/// 单帧 payload 上限(与设备端 `VM_FRAME_MAX` 一致)。
pub const PAYLOAD_MAX: usize = 512;
/// 帧开销:头 6B(magic2+ver1+type1+len2) + CRC 2B。
pub const OVERHEAD: usize = 8;

// 帧类型。
pub mod frame_type {
    pub const CMD_SET_ACTIVE: u8 = 0x01;
    pub const CMD_GET_STATUS: u8 = 0x02;
    pub const CMD_DISCONNECT: u8 = 0x03;
    pub const PROFILE_CHUNK: u8 = 0x04;
    /// Payload: 1 renews the desktop session for 15 seconds; 0 releases it.
    pub const CMD_PC_SESSION: u8 = 0x05;
    pub const STATUS_NOTIFY: u8 = 0x11;
    pub const RESULT: u8 = 0x12;
    pub const BUTTON_EVENT: u8 = 0x13;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub version: u8,
    pub kind: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TooShort,
    BadMagic,
    LengthMismatch,
    PayloadTooLarge,
    BadCrc,
}

impl Frame {
    pub fn new(kind: u8, payload: impl Into<Vec<u8>>) -> Self {
        Frame {
            version: VERSION,
            kind,
            payload: payload.into(),
        }
    }

    /// 编码为完整帧字节串;payload 超长时返回 `Err(PayloadTooLarge)`。
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        if self.payload.len() > PAYLOAD_MAX || self.payload.len() > u16::MAX as usize {
            return Err(FrameError::PayloadTooLarge);
        }
        let len = self.payload.len() as u16;
        let mut out = Vec::with_capacity(OVERHEAD + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.push(self.version);
        out.push(self.kind);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&self.payload);
        // CRC 覆盖 ver..payload(即去掉 magic 的 2 字节)。
        let crc = crc16_ccitt(&out[2..]);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(out)
    }

    /// 解析一个完整帧(长度必须精确匹配)。
    pub fn decode(buf: &[u8]) -> Result<Frame, FrameError> {
        if buf.len() < OVERHEAD {
            return Err(FrameError::TooShort);
        }
        if buf[0..2] != MAGIC {
            return Err(FrameError::BadMagic);
        }
        let plen = u16::from_le_bytes([buf[4], buf[5]]) as usize;
        if buf.len() != OVERHEAD + plen {
            return Err(FrameError::LengthMismatch);
        }
        let expect = u16::from_le_bytes([buf[6 + plen], buf[7 + plen]]);
        if crc16_ccitt(&buf[2..6 + plen]) != expect {
            return Err(FrameError::BadCrc);
        }
        Ok(Frame {
            version: buf[2],
            kind: buf[3],
            payload: buf[6..6 + plen].to_vec(),
        })
    }
}

/// 流式解码器:逐字节喂入,凑齐完整且 CRC 正确的帧时返回 `Some`。
/// 坏帧(magic 不符/超长/CRC 错)自动复位并返回 `None`,继续喂后续字节。
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    need: usize, // 0 = 未定型
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            buf: Vec::with_capacity(PAYLOAD_MAX + OVERHEAD),
            need: 0,
        }
    }

    pub fn feed(&mut self, byte: u8) -> Option<Frame> {
        if self.buf.len() >= PAYLOAD_MAX + OVERHEAD {
            self.buf.clear(); // 无效数据塞满:清空等下一个 magic
            self.need = 0;
        }
        self.buf.push(byte);

        if self.buf.len() < 6 {
            // 前缀已可判定 magic 不符:尽早丢弃,减少串口误码堆积。
            if self.buf.len() >= 2 && self.buf[0..2] != MAGIC {
                self.buf.clear();
                if byte == MAGIC[0] {
                    self.buf.push(byte);
                }
            }
            return None;
        }
        if self.buf[0..2] != MAGIC {
            self.buf.clear();
            self.need = 0;
            return None;
        }
        if self.need == 0 {
            let plen = u16::from_le_bytes([self.buf[4], self.buf[5]]) as usize;
            if plen > PAYLOAD_MAX {
                self.buf.clear();
                return None;
            }
            self.need = OVERHEAD + plen;
        }
        if self.buf.len() < self.need {
            return None;
        }

        // 凑齐:校验并交付;失败则复位丢弃整帧。
        let frame = Frame::decode(&self.buf[..self.need]).ok();
        self.buf.clear();
        self.need = 0;
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vectors() {
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
        assert_eq!(crc16_ccitt(&[]), 0xFFFF);
    }

    #[test]
    fn decoder_recovers_from_odd_noise_and_overlapping_magic() {
        let frame = Frame::new(frame_type::CMD_GET_STATUS, []);
        for prefix in [vec![0], vec![0x56], vec![0, 0, 0], vec![0x56, 0x56]] {
            let mut d = Decoder::new();
            for b in prefix {
                assert!(d.feed(b).is_none());
            }
            for _ in 0..20 {
                let decoded: Vec<_> = frame
                    .encode()
                    .unwrap()
                    .into_iter()
                    .filter_map(|b| d.feed(b))
                    .collect();
                assert_eq!(decoded, vec![frame.clone()]);
            }
        }
    }

    #[test]
    fn serial_delimiters_preserve_all_binary_bytes() {
        let mut d = Decoder::new();
        // Includes 0x0A in length, payload and (for some payloads) CRC.
        for byte in 0..=255 {
            let frame = Frame::new(frame_type::BUTTON_EVENT, vec![byte; 10]);
            let mut stream = frame.encode().unwrap();
            stream.push(b'\n');
            let got: Vec<_> = stream.into_iter().filter_map(|b| d.feed(b)).collect();
            assert_eq!(got, vec![frame]);
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let f = Frame::new(frame_type::CMD_SET_ACTIVE, [0x07u8]);
        let bytes = f.encode().unwrap();
        assert_eq!(bytes.len(), OVERHEAD + 1);
        assert_eq!(&bytes[0..6], &[0x56, 0x4D, 0x01, 0x01, 0x01, 0x00]);
        assert_eq!(Frame::decode(&bytes).unwrap(), f);

        let empty = Frame::new(frame_type::CMD_GET_STATUS, Vec::<u8>::new());
        let bytes = empty.encode().unwrap();
        assert_eq!(bytes.len(), OVERHEAD);
        assert_eq!(Frame::decode(&bytes).unwrap(), empty);
    }

    #[test]
    fn decode_detects_damage() {
        let mut bytes = Frame::new(0x01, [1u8]).encode().unwrap();
        *bytes.last_mut().unwrap() ^= 0x01; // 破坏 CRC
        assert_eq!(Frame::decode(&bytes), Err(FrameError::BadCrc));

        bytes = Frame::new(0x01, [1u8]).encode().unwrap();
        assert_eq!(
            Frame::decode(&bytes[..bytes.len() - 1]),
            Err(FrameError::LengthMismatch)
        );
        bytes[0] = 0x00;
        assert_eq!(Frame::decode(&bytes), Err(FrameError::BadMagic));
        assert_eq!(Frame::decode(&[0u8; 4]), Err(FrameError::TooShort));
    }

    #[test]
    fn encode_rejects_oversized() {
        let big = vec![0u8; PAYLOAD_MAX + 1];
        assert_eq!(
            Frame::new(0x04, big).encode(),
            Err(FrameError::PayloadTooLarge)
        );
        let exact = Frame::new(0x04, vec![0u8; PAYLOAD_MAX]);
        assert_eq!(exact.encode().unwrap().len(), PAYLOAD_MAX + OVERHEAD);
    }

    #[test]
    fn decoder_streams_and_recovers() {
        let mut d = Decoder::new();
        // 前置噪声。
        assert!(d.feed(0x00).is_none());
        assert!(d.feed(0x00).is_none());

        let f = Frame::new(frame_type::CMD_DISCONNECT, [0xDE, 0xAD, 0xBE, 0xEF]);
        let bytes = f.encode().unwrap();
        for (i, &b) in bytes.iter().enumerate() {
            let got = d.feed(b);
            if i + 1 == bytes.len() {
                assert_eq!(got, Some(f.clone()));
            } else {
                assert!(got.is_none());
            }
        }
        // 背靠背两帧。
        for _ in 0..2 {
            let mut last = None;
            for &b in &bytes {
                last = d.feed(b);
            }
            assert_eq!(last, Some(f.clone()));
        }
    }

    #[test]
    fn decoder_recovers_from_crc_damage() {
        let mut d = Decoder::new();
        let mut bad = Frame::new(frame_type::CMD_GET_STATUS, Vec::<u8>::new())
            .encode()
            .unwrap();
        *bad.last_mut().unwrap() ^= 0xFF;
        for &b in &bad {
            assert!(d.feed(b).is_none());
        }
        let good = Frame::new(frame_type::CMD_GET_STATUS, Vec::<u8>::new());
        let bytes = good.encode().unwrap();
        let mut last = None;
        for &b in &bytes {
            last = d.feed(b);
        }
        assert_eq!(last, Some(good));
    }

    #[test]
    fn decoder_rejects_hostile_length() {
        let mut d = Decoder::new();
        // 声明 len=0xFFFF → 超 PAYLOAD_MAX,整帧丢弃。
        for &b in &[0x56, 0x4D, 0x01, 0x04, 0xFF, 0xFF] {
            assert!(d.feed(b).is_none());
        }
        assert!(d.buf.is_empty());
    }
}
