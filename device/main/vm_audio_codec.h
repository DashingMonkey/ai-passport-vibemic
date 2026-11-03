// main/vm_audio_codec.h —— IMA-ADPCM 编解码(纯 C,host-testable,无 ESP 依赖)。
//
// 设备侧语音串流的压缩层:16kHz/16bit 单声道 PCM → 4bit/样本(4:1,64kbps),
// BLE 串流带宽友好,CPU 开销可忽略。参考实现与黄金向量:
// tools/gen_audio_golden.py(设备 C 与主机 Rust 必须与它比特级一致)。
#pragma once

#include <stddef.h>
#include <stdint.h>

typedef struct {
    int16_t predictor;
    uint8_t index; // 0..88
} vm_adpcm_state_t;

// 编码 samples 个 s16 样本 → out(每样本 4bit,低半字节在前,容量 ≥ (samples+1)/2)。
// state 随编码前移(编码器内部用解码器逻辑推进,保证 enc/dec 状态一致)。
// 返回写入字节数;cap 不足返回 0。
size_t vm_adpcm_encode(const int16_t *pcm, size_t samples, uint8_t *out,
                       size_t cap, vm_adpcm_state_t *st);

// 解码 bytes 字节的 ADPCM(低半字节在前)→ out_pcm(容量 ≥ bytes*2 个样本)。
// 返回解码样本数;cap 不足返回 0。
size_t vm_adpcm_decode(const uint8_t *data, size_t bytes, int16_t *out_pcm,
                       size_t cap, vm_adpcm_state_t *st);

// ---- VM_AUDIO 特征值块格式 ----
// [seq u16-LE][predictor i16-LE][index u8][nibbles...]
// 每块自带起始状态:块可独立解码(丢一块不影响后续块),主机无需跨块携带状态。
#define VM_AUDIO_CHUNK_HDR 5

// 打包一块:以 st 的当前状态为块头,编码 samples 个样本写入 buf。
// st 前移到块尾(下一块的头)。返回块总长(5 + (samples+1)/2);cap 不足返回 0。
size_t vm_audio_pack_chunk(uint8_t *buf, size_t cap, uint16_t seq,
                           const int16_t *pcm, size_t samples,
                           vm_adpcm_state_t *st);

// 解析块头(不解码):seq/pred/index 输出参数可 NULL。
// 返回 nibble 数据在 buf 中的偏移(= VM_AUDIO_CHUNK_HDR);格式非法返回 0。
size_t vm_audio_chunk_header(const uint8_t *buf, size_t len, uint16_t *seq,
                             int16_t *predictor, uint8_t *index);

// ---- 发送节奏(背压) ----
// 每块音频固定覆盖 20ms。
#define VM_AUDIO_CHUNK_MS 20u

// 按当前连接间隔算出"两块之间至少间隔多少 ms"。
//
// 一个连接事件至少能带走一个通知,所以连接间隔(ms)就是链路能承受的最小发车间隔:
// 间隔 30ms 时链路只有约 33 包/s 的容量,而音频按 50 块/s 产出 —— 不限制产出就会
// 把 NimBLE 的 msys 池占满、notify 开始失败(录音被截断在 ~17 块)。
// 间隔 ≤ 20ms 时按产出节奏(20ms),不额外延迟。
// conn_itvl_125us 为 0(尚未协商)时同样按 20ms。
uint32_t vm_audio_pace_gap_ms(uint16_t conn_itvl_125us);
