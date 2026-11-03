// main/vm_audio_codec.c —— IMA-ADPCM 编解码实现(算法说明见 vm_audio_codec.h;
// 与 tools/gen_audio_golden.py 的 Python 参考实现比特级一致,由
// tests/test_vm_audio_codec.c 的黄金向量约束)。
#include "vm_audio_codec.h"

#include <string.h>

// 标准 IMA 步长表(89 项)与索引表(按完整 4bit nibble 索引,16 项)。
static const uint16_t STEP_TABLE[89] = {
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41,
    45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190,
    209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724,
    796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499,
    2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845,
    8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385,
    24623, 27086, 29794, 32767,
};
static const int8_t INDEX_TABLE[16] = {
    -1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8,
};

static int16_t clamp16(int32_t v)
{
    if (v < -32768) return -32768;
    if (v > 32767) return 32767;
    return (int16_t)v;
}

static uint8_t clamp_index(int32_t i)
{
    if (i < 0) return 0;
    if (i > 88) return 88;
    return (uint8_t)i;
}

// 解一个 nibble 并推进状态(编码器也用它推进,保证 enc/dec 状态一致)。
static int16_t decode_nibble(uint8_t code, int16_t pred, uint8_t idx,
                             uint8_t *new_idx)
{
    const uint32_t step = STEP_TABLE[idx];
    int32_t diff = (int32_t)(step >> 3);
    if (code & 4) diff += (int32_t)step;
    if (code & 2) diff += (int32_t)(step >> 1);
    if (code & 1) diff += (int32_t)(step >> 2);
    if (code & 8) diff = -diff;
    *new_idx = clamp_index((int32_t)idx + INDEX_TABLE[code]);
    return clamp16((int32_t)pred + diff);
}

size_t vm_adpcm_encode(const int16_t *pcm, size_t samples, uint8_t *out,
                       size_t cap, vm_adpcm_state_t *st)
{
    if (!pcm || !out || !st) return 0;
    const size_t bytes = (samples + 1) / 2;
    if (cap < bytes) return 0;

    for (size_t i = 0; i < samples; i++) {
        int32_t diff = (int32_t)pcm[i] - st->predictor;
        uint8_t code = 0;
        if (diff < 0) {
            code = 8;
            diff = -diff;
        }
        uint32_t step = STEP_TABLE[st->index];
        if (diff >= (int32_t)step) {
            code |= 4;
            diff -= (int32_t)step;
        }
        step >>= 1;
        if (diff >= (int32_t)step) {
            code |= 2;
            diff -= (int32_t)step;
        }
        step >>= 1;
        if (diff >= (int32_t)step) {
            code |= 1;
        }
        uint8_t next_idx;
        st->predictor = decode_nibble(code, st->predictor, st->index, &next_idx);
        st->index = next_idx;

        if (i & 1) out[i >> 1] |= (uint8_t)(code << 4);
        else out[i >> 1] = code;
    }
    return bytes;
}

size_t vm_adpcm_decode(const uint8_t *data, size_t bytes, int16_t *out_pcm,
                       size_t cap, vm_adpcm_state_t *st)
{
    if (!data || !out_pcm || !st) return 0;
    const size_t samples = bytes * 2;
    if (cap < samples) return 0;

    for (size_t i = 0; i < samples; i++) {
        const uint8_t code = (i & 1) ? (data[i >> 1] >> 4) : (data[i >> 1] & 0x0F);
        uint8_t next_idx;
        st->predictor = decode_nibble(code, st->predictor, st->index, &next_idx);
        st->index = next_idx;
        out_pcm[i] = st->predictor;
    }
    return samples;
}

size_t vm_audio_pack_chunk(uint8_t *buf, size_t cap, uint16_t seq,
                           const int16_t *pcm, size_t samples,
                           vm_adpcm_state_t *st)
{
    if (!buf || !st) return 0;
    const size_t need = VM_AUDIO_CHUNK_HDR + (samples + 1) / 2;
    if (cap < need) return 0;
    buf[0] = (uint8_t)(seq & 0xFF);
    buf[1] = (uint8_t)(seq >> 8);
    buf[2] = (uint8_t)((uint16_t)st->predictor & 0xFF);
    buf[3] = (uint8_t)((uint16_t)st->predictor >> 8);
    buf[4] = st->index;
    if (vm_adpcm_encode(pcm, samples, buf + VM_AUDIO_CHUNK_HDR,
                        cap - VM_AUDIO_CHUNK_HDR, st) == 0) {
        return 0;
    }
    return need;
}

size_t vm_audio_chunk_header(const uint8_t *buf, size_t len, uint16_t *seq,
                             int16_t *predictor, uint8_t *index)
{
    if (!buf || len < VM_AUDIO_CHUNK_HDR) return 0;
    if (seq) *seq = (uint16_t)buf[0] | ((uint16_t)buf[1] << 8);
    if (predictor) {
        *predictor = (int16_t)((uint16_t)buf[2] | ((uint16_t)buf[3] << 8));
    }
    if (index) *index = buf[4];
    return VM_AUDIO_CHUNK_HDR;
}

uint32_t vm_audio_pace_gap_ms(uint16_t conn_itvl_125us)
{
    if (conn_itvl_125us == 0) return VM_AUDIO_CHUNK_MS;
    // 连接间隔单位 1.25ms;向下取整到 ms(宁可略快也不要无谓地拉长录音时延)。
    const uint32_t itvl_ms = ((uint32_t)conn_itvl_125us * 125u) / 100u;
    return itvl_ms > VM_AUDIO_CHUNK_MS ? itvl_ms : VM_AUDIO_CHUNK_MS;
}
