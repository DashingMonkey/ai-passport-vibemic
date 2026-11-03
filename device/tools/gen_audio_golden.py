#!/usr/bin/env python3
"""IMA-ADPCM + 音频块格式的黄金向量生成器(参考实现)。

设备端 C(vm_audio_codec.c)与主机端 Rust(vibemic-protocol/src/audio.rs)必须
比特级一致;本脚本用同一算法生成确定性测试信号与期望哈希,两侧测试各自断言
同样的常量。改动编码格式时重新运行本脚本并同步两侧常量。

块格式(VM_AUDIO 特征值 payload,非协议帧):
  [seq u16-LE][predictor i16-LE][index u8][nibbles...]

用法: python gen_audio_golden.py   → 打印 golden 常量
"""

STEP_TABLE = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45,
    50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230,
    253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327,
    3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442,
    11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
]
INDEX_TABLE = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8]


def clamp16(v: int) -> int:
    return -32768 if v < -32768 else (32767 if v > 32767 else v)


def clamp_index(i: int) -> int:
    return 0 if i < 0 else (88 if i > 88 else i)


def decode_nibble(code: int, pred: int, idx: int):
    step = STEP_TABLE[idx]
    diff = step >> 3
    if code & 4:
        diff += step
    if code & 2:
        diff += step >> 1
    if code & 1:
        diff += step >> 2
    if code & 8:
        diff = -diff
    return clamp16(pred + diff), clamp_index(idx + INDEX_TABLE[code])


def encode_sample(s: int, pred: int, idx: int):
    diff = s - pred
    code = 0
    if diff < 0:
        code = 8
        diff = -diff
    step = STEP_TABLE[idx]
    if diff >= step:
        code |= 4
        diff -= step
    step >>= 1
    if diff >= step:
        code |= 2
        diff -= step
    step >>= 1
    if diff >= step:
        code |= 1
    # 编码器用解码器逻辑推进状态,保证两侧一致
    pred, idx = decode_nibble(code, pred, idx)
    return code, pred, idx


def xorshift_signal(n: int, seed: int = 0xACE1):
    xs = seed
    out = []
    for _ in range(n):
        xs ^= (xs << 7) & 0xFFFF
        xs ^= xs >> 9
        xs ^= (xs << 8) & 0xFFFF
        out.append(xs - 0x10000 if xs & 0x8000 else xs)
    return out


def pack_chunks(pcm, chunk_samples: int):
    chunks = bytearray()
    pred, idx = 0, 0
    seq = 0
    decoded_all = []
    for off in range(0, len(pcm), chunk_samples):
        block = pcm[off:off + chunk_samples]
        nibbles = []
        head_pred, head_idx = pred, idx
        for s in block:
            code, pred, idx = encode_sample(s, pred, idx)
            nibbles.append(code)
        data = bytearray()
        for i in range(0, len(nibbles), 2):
            b = nibbles[i]
            if i + 1 < len(nibbles):
                b |= nibbles[i + 1] << 4
            data.append(b)
        chunks += seq.to_bytes(2, "little")
        chunks += head_pred.to_bytes(2, "little", signed=True)
        chunks += bytes([head_idx])
        chunks += data
        # 参考解码(从块头状态起)
        dpred, didx = head_pred, head_idx
        for i in range(len(block)):
            code = nibbles[i]
            v, didx = decode_nibble(code, dpred, didx)
            dpred = v
            decoded_all.append(v)
        seq = (seq + 1) & 0xFFFF
    return bytes(chunks), decoded_all


def fnv1a(data: bytes) -> int:
    h = 0x811C9DC5
    for b in data:
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF
    return h


if __name__ == "__main__":
    pcm = xorshift_signal(320 * 16)
    packed, decoded = pack_chunks(pcm, 320)
    print(f"GOLDEN_PACKED_LEN = {len(packed)}")
    print(f"GOLDEN_PACKED_FNV = 0x{fnv1a(packed):08X}")
    dec_bytes = b"".join(v.to_bytes(2, "little", signed=True) for v in decoded)
    print(f"GOLDEN_DECODED_FNV = 0x{fnv1a(dec_bytes):08X}")
