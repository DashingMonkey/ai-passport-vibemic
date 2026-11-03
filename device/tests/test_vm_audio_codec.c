// tests/test_vm_audio_codec.c —— IMA-ADPCM 编解码宿主测试。
//
// 黄金向量由 tools/gen_audio_golden.py 生成(确定性 xorshift 信号,无浮点):
// 设备 C 编码器与主机 Rust 解码器必须与该参考比特级一致。常量更新时
// 重新运行生成脚本并同步 desktop/crates/vibemic-protocol/src/audio.rs。
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "vm_audio_codec.h"

#define GOLDEN_PACKED_LEN 2640
#define GOLDEN_PACKED_FNV UINT32_C(0x78F1EAC2)
#define GOLDEN_DECODED_FNV UINT32_C(0xF1504001)

#define CHUNKS 16
#define CHUNK_SAMPLES 320

// 与生成脚本相同的确定性信号(16bit xorshift)。
static uint16_t xorshift16(uint16_t *xs)
{
    *xs ^= (uint16_t)(*xs << 7);
    *xs ^= (uint16_t)(*xs >> 9);
    *xs ^= (uint16_t)(*xs << 8);
    return *xs;
}

static uint32_t fnv1a(const uint8_t *data, size_t len)
{
    uint32_t h = UINT32_C(0x811C9DC5);
    for (size_t i = 0; i < len; i++) {
        h ^= data[i];
        h *= UINT32_C(0x01000193);
    }
    return h;
}

static void test_golden_stream(void)
{
    static int16_t pcm[CHUNKS * CHUNK_SAMPLES];
    static uint8_t packed[CHUNKS * (VM_AUDIO_CHUNK_HDR + CHUNK_SAMPLES / 2)];
    static int16_t decoded[CHUNKS * CHUNK_SAMPLES];

    uint16_t xs = 0xACE1;
    for (size_t i = 0; i < sizeof(pcm) / sizeof(pcm[0]); i++) {
        pcm[i] = (int16_t)xorshift16(&xs);
    }

    // 连续状态编码,每块记录块头 → 与生成脚本 pack_chunks 相同
    vm_adpcm_state_t st = { 0, 0 };
    size_t off = 0;
    for (int c = 0; c < CHUNKS; c++) {
        const size_t n = vm_audio_pack_chunk(packed + off, sizeof(packed) - off,
                                             (uint16_t)c,
                                             pcm + (size_t)c * CHUNK_SAMPLES,
                                             CHUNK_SAMPLES, &st);
        assert(n == VM_AUDIO_CHUNK_HDR + CHUNK_SAMPLES / 2);
        off += n;
    }
    assert(off == GOLDEN_PACKED_LEN);
    assert(fnv1a(packed, off) == GOLDEN_PACKED_FNV);

    // 逐块独立解码(从各块头状态起)→ 与生成脚本参考解码一致
    size_t dout = 0;
    for (int c = 0; c < CHUNKS; c++) {
        const uint8_t *blk = packed + (size_t)c * (VM_AUDIO_CHUNK_HDR + CHUNK_SAMPLES / 2);
        uint16_t seq;
        int16_t pred;
        uint8_t idx;
        assert(vm_audio_chunk_header(blk, 255, &seq, &pred, &idx) ==
               VM_AUDIO_CHUNK_HDR);
        assert(seq == (uint16_t)c);
        vm_adpcm_state_t ds = { pred, idx };
        dout += vm_adpcm_decode(blk + VM_AUDIO_CHUNK_HDR, CHUNK_SAMPLES / 2,
                                decoded + dout, CHUNK_SAMPLES, &ds);
    }
    assert(dout == sizeof(pcm) / sizeof(pcm[0]));

    // 解码结果哈希(小端字节序展开后)
    uint8_t dec_bytes[sizeof(decoded)];
    for (size_t i = 0; i < dout; i++) {
        dec_bytes[2 * i] = (uint8_t)((uint16_t)decoded[i] & 0xFF);
        dec_bytes[2 * i + 1] = (uint8_t)((uint16_t)decoded[i] >> 8);
    }
    assert(fnv1a(dec_bytes, sizeof(dec_bytes)) == GOLDEN_DECODED_FNV);
}

static void test_roundtrip_bounds(void)
{
    // 平滑信号(正弦近似)回环误差必须有界:ADPCM 对慢变信号误差小。
    static int16_t pcm[320], out[320];
    uint8_t data[160];
    for (int i = 0; i < 320; i++) {
        // 整数三角波,幅度 8000,周期 64 样本 —— 免浮点的平滑信号
        const int phase = i % 64;
        const int tri = phase < 32 ? phase : 64 - phase;
        pcm[i] = (int16_t)((tri - 16) * 1000);
    }
    vm_adpcm_state_t st = { 0, 0 };
    assert(vm_adpcm_encode(pcm, 320, data, sizeof(data), &st) == 160);
    vm_adpcm_state_t ds = { 0, 0 };
    assert(vm_adpcm_decode(data, 160, out, 320, &ds) == 320);
    int32_t max_err = 0;
    for (int i = 8; i < 320; i++) { // 跳过起步数样本(状态尚未收敛)
        const int32_t e = out[i] - pcm[i];
        if (e < 0 ? -e > max_err : e > max_err) max_err = e < 0 ? -e : e;
    }
    assert(max_err < 512);
}

static void test_capacity_guards(void)
{
    vm_adpcm_state_t st = { 0, 0 };
    const int16_t two[2] = { 100, -100 };
    const int16_t three[3] = { 100, -100, 100 };
    uint8_t buf[4];
    assert(vm_adpcm_encode(two, 2, buf, 0, &st) == 0);
    assert(vm_adpcm_encode(two, 2, buf, 1, &st) == 1); // 2 样本恰 1 字节
    assert(vm_adpcm_encode(three, 3, buf, 1, &st) == 0); // 3 样本需 2 字节
    assert(vm_adpcm_encode(three, 3, buf, sizeof(buf), &st) == 2); // 奇数样本补齐
    int16_t out[2];
    vm_adpcm_state_t ds = { 0, 0 };
    assert(vm_adpcm_decode(buf, 1, out, 1, &ds) == 0);
    assert(vm_adpcm_decode(buf, 1, out, 2, &ds) == 2);
    uint16_t seq;
    int16_t pred;
    uint8_t idx;
    assert(vm_audio_chunk_header(buf, VM_AUDIO_CHUNK_HDR - 1, &seq, &pred, &idx) == 0);
}

// 发送节奏(背压):链路能带走多少就发多少,间隔 >20ms 时按间隔限速。
static void test_pace_gap(void)
{
    assert(vm_audio_pace_gap_ms(0) == 20);  // 未协商:按产出节奏
    assert(vm_audio_pace_gap_ms(8) == 20);  // 10ms 链路
    assert(vm_audio_pace_gap_ms(12) == 20); // 15ms(当前固件请求值)
    assert(vm_audio_pace_gap_ms(16) == 20); // 20ms:恰好等于产出节奏
    assert(vm_audio_pace_gap_ms(24) == 30); // 30ms:限速到 ~33 块/s
    assert(vm_audio_pace_gap_ms(40) == 50); // 50ms
    assert(vm_audio_pace_gap_ms(80) == 100);
    // 单位换算:1.25ms × 12 = 15ms
    assert(((uint32_t)12 * 125u) / 100u == 15u);
}

int main(void)
{
    test_golden_stream();
    test_roundtrip_bounds();
    test_capacity_guards();
    test_pace_gap();
    printf("test_vm_audio_codec: all passed\n");
    return 0;
}
