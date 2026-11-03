//! 设备麦克风音频串流的解码侧。
//!
//! 设备把 16kHz/16bit 单声道 PCM 经 IMA-ADPCM(4bit/样本,4:1)压缩后按块
//! notify 到 VM_AUDIO 特征值;块自带起始状态,可独立解码:
//!
//! ```text
//! [seq u16-LE][predictor i16-LE][index u8][nibbles...]
//! ```
//!
//! 算法与设备端 `device/main/vm_audio_codec.c` 比特级一致,由同一组黄金向量
//! 约束(参考实现:`device/tools/gen_audio_golden.py`)。

/// 音频块在 `DeviceLink` 上的伪帧类型(winrt 链路把 VM_AUDIO 通知包装成
/// `Frame { kind: AUDIO_CHUNK, payload: 原始块字节 }` 转发,不经协议帧解码)。
pub const AUDIO_CHUNK: u8 = 0x14;

/// 块头长度:seq(2) + predictor(2) + index(1)。
pub const CHUNK_HDR: usize = 5;

/// 标准 IMA 步长表(89 项)。
const STEP_TABLE: [u16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41,
    45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190,
    209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724,
    796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499,
    2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845,
    8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385,
    24623, 27086, 29794, 32767,
];
/// 步长表最大下标(块头 index 的合法上界)。
pub const MAX_STEP_INDEX: u8 = 88;
/// 索引表(按完整 4bit nibble 索引,含符号位)。
const INDEX_TABLE: [i8; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// IMA 解码状态(跨块不需要:块头自带;此结构仅块内使用)。
#[derive(Debug, Clone, Copy)]
pub struct AdpcmState {
    predictor: i16,
    index: u8,
}

impl AdpcmState {
    pub fn new(predictor: i16, index: u8) -> Self {
        AdpcmState { predictor, index }
    }

    fn decode_nibble(&mut self, code: u8) -> i16 {
        // 防御性取下标:构造入口已拒绝越界 index,内部推进也 clamp 过,
        // 这里再兜一层,保证解析设备字节流永远不会 panic。
        let step = STEP_TABLE[(self.index as usize).min(MAX_STEP_INDEX as usize)] as i32;
        let mut diff = step >> 3;
        if code & 4 != 0 {
            diff += step;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 1 != 0 {
            diff += step >> 2;
        }
        if code & 8 != 0 {
            diff = -diff;
        }
        self.index = (self.index as i32 + INDEX_TABLE[code as usize] as i32)
            .clamp(0, 88) as u8;
        self.predictor = (self.predictor as i32 + diff).clamp(-32768, 32767) as i16;
        self.predictor
    }
}

/// 解析并解码一个音频块 → 16bit PCM 样本。
/// 块过短(连块头都不够)或块头状态非法(index > 88)返回 `None`。
pub fn decode_chunk(block: &[u8]) -> Option<Vec<i16>> {
    if block.len() < CHUNK_HDR {
        return None;
    }
    let predictor = i16::from_le_bytes([block[2], block[3]]);
    let index = block[4];
    // 块头来自线缆:必须校验。设备编码器把 index 钳在 0..=88,但链路损坏、
    // 固件格式演进或别的数据源都可能送来越界值 —— 直接拿去索引 89 项步长表
    // 会 panic,而这个解析器正是"malformed 输入视为常态"的位置。
    if index > MAX_STEP_INDEX {
        return None;
    }
    let mut st = AdpcmState::new(predictor, index);
    let nibbles = &block[CHUNK_HDR..];
    let mut pcm = Vec::with_capacity(nibbles.len() * 2);
    for i in 0..nibbles.len() * 2 {
        let code = if i & 1 != 0 {
            nibbles[i >> 1] >> 4
        } else {
            nibbles[i >> 1] & 0x0F
        };
        pcm.push(st.decode_nibble(code));
    }
    Some(pcm)
}

/// 块序号(丢包/乱序检测用)。
pub fn chunk_seq(block: &[u8]) -> Option<u16> {
    if block.len() < CHUNK_HDR {
        return None;
    }
    Some(u16::from_le_bytes([block[0], block[1]]))
}

/// 把 PCM(16kHz 单声道 s16LE)封装成 WAV 字节(vmctl 落盘/调试用)。
pub fn pcm_to_wav(pcm: &[i16], hz: u32) -> Vec<u8> {
    let data_len = pcm.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&hz.to_le_bytes());
    out.extend_from_slice(&(hz * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

// ---- 测试侧编码器(仅用于黄金向量互证,产品路径只解码) ----

#[cfg(test)]
fn encode_nibble(sample: i16, st: &mut AdpcmState) -> u8 {
    let mut diff = sample as i32 - st.predictor as i32;
    let mut code = 0u8;
    if diff < 0 {
        code = 8;
        diff = -diff;
    }
    let mut step = STEP_TABLE[st.index as usize] as i32;
    if diff >= step {
        code |= 4;
        diff -= step;
    }
    step >>= 1;
    if diff >= step {
        code |= 2;
        diff -= step;
    }
    step >>= 1;
    if diff >= step {
        code |= 1;
    }
    let mut probe = *st;
    probe.decode_nibble(code);
    *st = probe;
    code
}

/// 与生成脚本相同的确定性信号(16bit xorshift;u16 移位天然截断,等价 Python 的 & 0xFFFF)。
#[cfg(test)]
fn xorshift_signal(n: usize) -> Vec<i16> {
    let mut xs: u16 = 0xACE1;
    (0..n)
        .map(|_| {
            xs ^= xs << 7;
            xs ^= xs >> 9;
            xs ^= xs << 8;
            xs as i16
        })
        .collect()
}

#[cfg(test)]
fn fnv1a(data: &[u8]) -> u32 {
    data.iter().fold(0x811C_9DC5u32, |h, b| {
        (h ^ *b as u32).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // device/tools/gen_audio_golden.py 生成;与设备 C 测试同源同值。
    const GOLDEN_PACKED_LEN: usize = 2640;
    const GOLDEN_PACKED_FNV: u32 = 0x78F1_EAC2;
    const GOLDEN_DECODED_FNV: u32 = 0xF150_4001;

    #[test]
    fn golden_stream_matches_device_encoder() {
        let pcm = xorshift_signal(320 * 16);
        let mut packed = Vec::with_capacity(GOLDEN_PACKED_LEN);
        let mut st = AdpcmState::new(0, 0);
        for c in 0..16 {
            let block = &pcm[c * 320..(c + 1) * 320];
            packed.extend_from_slice(&(c as u16).to_le_bytes());
            packed.extend_from_slice(&st.predictor.to_le_bytes());
            packed.push(st.index);
            let mut data = vec![0u8; 160];
            for (i, s) in block.iter().enumerate() {
                let code = encode_nibble(*s, &mut st);
                if i & 1 != 0 {
                    data[i >> 1] |= code << 4;
                } else {
                    data[i >> 1] = code;
                }
            }
            packed.extend_from_slice(&data);
        }
        assert_eq!(packed.len(), GOLDEN_PACKED_LEN);
        assert_eq!(fnv1a(&packed), GOLDEN_PACKED_FNV);

        // 逐块独立解码(与 C 测试同口径)
        let mut all_pcm = Vec::with_capacity(pcm.len());
        for c in 0..16 {
            let blk = &packed[c * 165..(c + 1) * 165];
            assert_eq!(chunk_seq(blk), Some(c as u16));
            all_pcm.extend_from_slice(&decode_chunk(blk).unwrap());
        }
        let mut dec_bytes = Vec::with_capacity(all_pcm.len() * 2);
        for s in &all_pcm {
            dec_bytes.extend_from_slice(&s.to_le_bytes());
        }
        assert_eq!(fnv1a(&dec_bytes), GOLDEN_DECODED_FNV);
    }

    #[test]
    fn short_block_rejected_and_wav_shape() {
        assert!(decode_chunk(&[1, 2, 3]).is_none());
        assert!(chunk_seq(&[1, 2, 3]).is_none());
        let wav = pcm_to_wav(&[0, 100, -100], 16000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 6);
    }

    #[test]
    fn decode_rejects_out_of_range_index() {
        // 块头 index 越界必须拒绝:越界值直接索引 89 项步长表会 panic。
        for bad in [MAX_STEP_INDEX + 1, 0x80, 0xFF] {
            let block = [0u8, 0, 0, 0, bad, 0x00, 0x11];
            assert!(decode_chunk(&block).is_none(), "index {bad} 应被拒绝");
        }
        // 边界值 88 仍可解(步长表最后一项)。
        let ok = [0u8, 0, 0, 0, MAX_STEP_INDEX, 0x00, 0x11];
        assert_eq!(decode_chunk(&ok).map(|p| p.len()), Some(4));
    }
}
