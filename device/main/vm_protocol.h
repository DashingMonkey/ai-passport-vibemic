// main/vm_protocol.h —— VibeMic 协议帧编解码(纯逻辑,无 ESP-IDF 依赖,可跑 host tests)。
//
// 帧格式:
//   [0x56 0x4D] magic 'V''M' | [ver]=1 | [type] | [len]u16-LE | [payload] | [crc16]u16-LE
//   CRC16-CCITT (poly 0x1021, init 0xFFFF, no reflect, xorout 0),覆盖 ver..payload。
// 串口兜底通道在每帧后追加 '\n' 分隔(BLE 通道不加)。
// 协议常量同时维护在 desktop/crates/vibemic-protocol。
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define VM_PROTO_MAGIC0 0x56 // 'V'
#define VM_PROTO_MAGIC1 0x4D // 'M'
#define VM_PROTO_VERSION 1

// 帧大小上限:覆盖 ATT_MTU 247 下的最大分块(payload ~237B)与串口整帧场景。
#define VM_FRAME_MAX 512

// 帧类型。
enum {
    VM_FRAME_CMD_SET_ACTIVE = 0x01, // PC→设备  profileId(1B)
    VM_FRAME_CMD_GET_STATUS = 0x02, // PC→设备  —
    VM_FRAME_CMD_DISCONNECT = 0x03, // PC→设备  —
    VM_FRAME_PROFILE_CHUNK  = 0x04, // PC→设备  profileId(1) seq(2) total(2) dataLen(1) data…
    VM_FRAME_CMD_PC_SESSION = 0x05, // PC→设备  active(1B):1=续租15秒,0=结束
    VM_FRAME_STATUS_NOTIFY  = 0x11, // 设备→PC  见 vm_proto_status_t
    VM_FRAME_RESULT         = 0x12, // 设备→PC  reqType(1) code(1)
    VM_FRAME_BUTTON_EVENT   = 0x13, // 设备→PC  button(1) event(1) tsMs(4)
};

// RESULT code。
enum {
    VM_RESULT_OK = 0,
    VM_RESULT_BAD_CRC = 1,
    VM_RESULT_BAD_VERSION = 2,
    VM_RESULT_STORAGE = 3,
    VM_RESULT_BUSY = 4,
    VM_RESULT_BAD_ARG = 5, // 载荷/参数无效(如未知 profileId)
};

// BUTTON_EVENT.event 取值(与 bsp_btn_ev_t 的映射在 vm_buttons 完成)。
enum {
    VM_BTN_EV_PRESS = 0,
    VM_BTN_EV_RELEASE = 1,
    VM_BTN_EV_CLICK = 2,
    VM_BTN_EV_DOUBLE = 3,
    VM_BTN_EV_LONG = 4,
};

// 连接状态(STATUS_NOTIFY.connState)。
enum {
    VM_CONN_ADVERTISING = 0,
    VM_CONN_CONNECTED = 1,
};

// bond 槽(STATUS_NOTIFY.bondSlot)。
enum {
    VM_BOND_NONE = 0,
    VM_BOND_A = 1,
    VM_BOND_B = 2,
};

// STATUS_NOTIFY 载荷(11B,全部小端)。fwVer = BCD 风格版本号,高字节主版本。
// battery_pct / charging 的"未知"值都是 0xFF(与桌面端 CHARGING_UNKNOWN 一致)。
typedef struct {
    uint16_t fw_ver;
    uint8_t battery_pct;      // 0..100,未知 0xFF
    uint8_t charging;         // 0/1,未知 0xFF
    uint8_t conn_state;       // VM_CONN_*
    uint8_t active_profile;   // 当前激活 profile id
    uint8_t bond_slot;        // VM_BOND_*(未连接 = NONE)
    uint32_t free_heap;       // 设备剩余堆字节
} vm_proto_status_t;

// RESULT 载荷。
typedef struct {
    uint8_t req_type; // 被应答的帧类型
    uint8_t code;     // VM_RESULT_*
} vm_proto_result_t;

// BUTTON_EVENT 载荷。
typedef struct {
    uint8_t button;   // 0=UP 1=DOWN 2=OK(与 bsp_btn_t 数值一致)
    uint8_t event;    // VM_BTN_EV_*
    uint32_t ts_ms;   // 设备启动以来毫秒
} vm_proto_button_event_t;

// PROFILE_CHUNK 头(6B,其后跟 dataLen 字节分块数据)。total/seq 从 0 计。
typedef struct {
    uint8_t profile_id;
    uint16_t seq;
    uint16_t total;
    uint8_t data_len;
    const uint8_t *data;
} vm_proto_chunk_t;

// 一帧的只读视图(payload 指向解码器内部缓冲或调用者缓冲)。
typedef struct {
    uint8_t version;
    uint8_t type;
    uint16_t len;
    const uint8_t *payload;
} vm_frame_t;

// CRC16-CCITT(init 0xFFFF),多项式 0x1021,无反射。逐位实现,帧短无需查表。
uint16_t vm_crc16(const uint8_t *data, size_t len);

// 编码一帧到 buf,返回总字节数;cap 不足或 len 超过 u16 时返回 0。
size_t vm_frame_encode(uint8_t *buf, size_t cap, uint8_t version, uint8_t type,
                       const uint8_t *payload, uint16_t len);

// 校验缓冲中的完整帧(长度须正好一致):magic/版本/CRC 全对返回 0,否则 -1。
int vm_frame_verify(const uint8_t *buf, size_t len);

// 流式解码器:逐字节喂入,凑齐完整且校验通过的帧时通过 *out 交付(视图为解码器
// 内部成员,payload 指向解码器缓冲,仅在下次 feed 前有效)。返回 1=交付一帧,
// 0=继续等待,-1=帧错误(已自动复位,继续喂后续字节即可)。
typedef struct {
    uint8_t buf[VM_FRAME_MAX + 8];
    uint16_t fill;      // 已收字节数
    uint32_t need;      // 当前帧期望总长(0=未定型)
    vm_frame_t frame;   // 最近交付的帧视图(payload 指向 buf)
} vm_decoder_t;

void vm_decoder_init(vm_decoder_t *d);
int vm_decoder_feed(vm_decoder_t *d, uint8_t byte, const vm_frame_t **out);

// 载荷编解码(小端)。编码函数返回写入字节数,入参指针可为 NULL 的字段填 0;
// 解码函数入参缓冲不足返回 0,成功返回载荷字节数。
size_t vm_payload_encode_status(uint8_t *buf, size_t cap, const vm_proto_status_t *st);
size_t vm_payload_encode_result(uint8_t *buf, size_t cap, const vm_proto_result_t *r);
size_t vm_payload_encode_button_event(uint8_t *buf, size_t cap,
                                      const vm_proto_button_event_t *ev);
int vm_payload_decode_status(const uint8_t *buf, size_t len, vm_proto_status_t *st);
int vm_payload_decode_result(const uint8_t *buf, size_t len, vm_proto_result_t *r);
int vm_payload_decode_button_event(const uint8_t *buf, size_t len,
                                   vm_proto_button_event_t *ev);
int vm_payload_decode_chunk(const uint8_t *buf, size_t len, vm_proto_chunk_t *ch);

#ifdef __cplusplus
}
#endif
