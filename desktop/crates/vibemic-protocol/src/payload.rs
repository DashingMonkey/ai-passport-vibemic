//! 帧载荷结构:全部小端。

// RESULT code。
pub mod result_code {
    pub const OK: u8 = 0;
    pub const BAD_CRC: u8 = 1;
    pub const BAD_VERSION: u8 = 2;
    pub const STORAGE: u8 = 3;
    pub const BUSY: u8 = 4;
    pub const BAD_ARG: u8 = 5;
}

// BUTTON_EVENT.event。
pub mod button_event {
    pub const PRESS: u8 = 0;
    pub const RELEASE: u8 = 1;
    pub const CLICK: u8 = 2;
    pub const DOUBLE: u8 = 3;
    pub const LONG: u8 = 4;
}

// STATUS_NOTIFY.conn_state。
pub mod conn_state {
    pub const ADVERTISING: u8 = 0;
    pub const CONNECTED: u8 = 1;
}

// STATUS_NOTIFY.bond_slot。
pub mod bond_slot {
    pub const NONE: u8 = 0;
    pub const A: u8 = 1;
    pub const B: u8 = 2;
}

// 按键编号(与设备 bsp_btn_t 数值一致)。
pub mod button_id {
    pub const UP: u8 = 0;
    pub const DOWN: u8 = 1;
    pub const OK: u8 = 2;
}

pub const BATTERY_UNKNOWN: u8 = 0xFF;
pub const CHARGING_UNKNOWN: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadError;

/// STATUS_NOTIFY(11B):fwVer(2) battery(1) charging(1) connState(1)
/// activeProfileId(1) bondSlot(1) freeHeap(4)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusNotify {
    pub fw_ver: u16,
    pub battery_pct: Option<u8>,
    pub charging: Option<bool>,
    pub conn_state: u8,
    pub active_profile_id: u8,
    pub bond_slot: u8,
    pub free_heap: u32,
}

impl StatusNotify {
    pub fn to_bytes(&self) -> [u8; 11] {
        let mut out = [0u8; 11];
        out[0..2].copy_from_slice(&self.fw_ver.to_le_bytes());
        out[2] = self.battery_pct.unwrap_or(BATTERY_UNKNOWN);
        out[3] = self.charging.map(|c| c as u8).unwrap_or(CHARGING_UNKNOWN);
        out[4] = self.conn_state;
        out[5] = self.active_profile_id;
        out[6] = self.bond_slot;
        out[7..11].copy_from_slice(&self.free_heap.to_le_bytes());
        out
    }

    pub fn parse(buf: &[u8]) -> Result<Self, PayloadError> {
        if buf.len() != 11 {
            return Err(PayloadError);
        }
        Ok(StatusNotify {
            fw_ver: u16::from_le_bytes([buf[0], buf[1]]),
            battery_pct: (buf[2] != BATTERY_UNKNOWN).then_some(buf[2]),
            charging: if buf[3] == CHARGING_UNKNOWN {
                None
            } else {
                Some(buf[3] != 0)
            },
            conn_state: buf[4],
            active_profile_id: buf[5],
            bond_slot: buf[6],
            free_heap: u32::from_le_bytes([buf[7], buf[8], buf[9], buf[10]]),
        })
    }
}

/// RESULT(2B):reqType(1) code(1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandResult {
    pub req_type: u8,
    pub code: u8,
}

impl CommandResult {
    pub fn to_bytes(&self) -> [u8; 2] {
        [self.req_type, self.code]
    }

    pub fn parse(buf: &[u8]) -> Result<Self, PayloadError> {
        if buf.len() != 2 {
            return Err(PayloadError);
        }
        Ok(CommandResult {
            req_type: buf[0],
            code: buf[1],
        })
    }
}

/// BUTTON_EVENT(6B):button(1) event(1) tsMs(4)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonEvent {
    pub button: u8,
    pub event: u8,
    pub ts_ms: u32,
}

impl ButtonEvent {
    pub fn to_bytes(&self) -> [u8; 6] {
        let mut out = [0u8; 6];
        out[0] = self.button;
        out[1] = self.event;
        out[2..6].copy_from_slice(&self.ts_ms.to_le_bytes());
        out
    }

    pub fn parse(buf: &[u8]) -> Result<Self, PayloadError> {
        if buf.len() != 6 {
            return Err(PayloadError);
        }
        Ok(ButtonEvent {
            button: buf[0],
            event: buf[1],
            ts_ms: u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]),
        })
    }
}

/// PROFILE_CHUNK 头(6B + data):profileId(1) seq(2) total(2) dataLen(1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHeader {
    pub profile_id: u8,
    pub seq: u16,
    pub total: u16,
}

impl ChunkHeader {
    /// 编码一个分块(data_len ≤ 255)。
    pub fn frame_bytes(&self, data: &[u8]) -> Result<Vec<u8>, PayloadError> {
        if data.len() > u8::MAX as usize {
            return Err(PayloadError);
        }
        let mut out = Vec::with_capacity(6 + data.len());
        out.push(self.profile_id);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.total.to_le_bytes());
        out.push(data.len() as u8);
        out.extend_from_slice(data);
        Ok(out)
    }

    /// 解析分块,返回 (头, data 切片)。
    pub fn parse(buf: &[u8]) -> Result<(ChunkHeader, &[u8]), PayloadError> {
        if buf.len() < 6 || buf[5] as usize + 6 != buf.len() {
            return Err(PayloadError);
        }
        Ok((
            ChunkHeader {
                profile_id: buf[0],
                seq: u16::from_le_bytes([buf[1], buf[2]]),
                total: u16::from_le_bytes([buf[3], buf[4]]),
            },
            &buf[6..],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip_and_byte_order() {
        let st = StatusNotify {
            fw_ver: 0x0102,
            battery_pct: Some(87),
            charging: Some(false),
            conn_state: conn_state::CONNECTED,
            active_profile_id: 2,
            bond_slot: bond_slot::A,
            free_heap: 0x12345678,
        };
        let bytes = st.to_bytes();
        assert_eq!(bytes[7], 0x78); // free_heap 低字节在前
        assert_eq!(bytes[10], 0x12);
        assert_eq!(StatusNotify::parse(&bytes).unwrap(), st);

        let unknown = StatusNotify {
            battery_pct: None,
            charging: None,
            fw_ver: 1,
            conn_state: 0,
            active_profile_id: 0,
            bond_slot: 0,
            free_heap: 0,
        };
        assert_eq!(unknown.to_bytes()[2], BATTERY_UNKNOWN);
        assert_eq!(unknown.to_bytes()[3], CHARGING_UNKNOWN);
        assert_eq!(StatusNotify::parse(&unknown.to_bytes()).unwrap(), unknown);
    }

    #[test]
    fn result_and_event_roundtrip() {
        let r = CommandResult {
            req_type: 0x01,
            code: result_code::BAD_ARG,
        };
        assert_eq!(CommandResult::parse(&r.to_bytes()).unwrap(), r);
        assert!(CommandResult::parse(&[0]).is_err());

        let e = ButtonEvent {
            button: button_id::OK,
            event: button_event::RELEASE,
            ts_ms: 0x01020304,
        };
        assert_eq!(ButtonEvent::parse(&e.to_bytes()).unwrap(), e);
        assert!(ButtonEvent::parse(&[0; 5]).is_err());
    }

    #[test]
    fn chunk_roundtrip() {
        let h = ChunkHeader {
            profile_id: 3,
            seq: 2,
            total: 5,
        };
        let bytes = h.frame_bytes(b"abcd").unwrap();
        assert_eq!(bytes.len(), 10);
        let (h2, data) = ChunkHeader::parse(&bytes).unwrap();
        assert_eq!(h2, h);
        assert_eq!(data, b"abcd");
        assert!(ChunkHeader::parse(&bytes[..9]).is_err());
        assert!(h.frame_bytes(&[0u8; 256]).is_err());
    }
}
