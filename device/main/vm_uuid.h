// main/vm_uuid.h —— VibeMic 服务与特征值 UUID 的单一出处。
//
// Base UUID `a7c52e00-31d4-4d2a-8f6b-5e9c1b3f7aXX`;
// BLE_UUID128_INIT 收的是线上小端序(= UUID 字符串反转),故宏按 id(如 0x7a01)展开。
//
// vm_hid(GATT 表)与 vm_link(句柄解析/回调)共用本头,且访问回调的 arg 也用同一组
// 常量,避免"表里挂一个值、回调里比另一个字面量"这种会静默错位的写法。
#pragma once

#include "host/ble_hs.h"

#define VM_UUID128_INIT(id)                                                   \
    BLE_UUID128_INIT((uint8_t)((id) & 0xFF),                                  \
                     (uint8_t)(((id) >> 8) & 0xFF), 0x3f, 0x1b,               \
                     0x9c, 0x5e, 0x6b, 0x8f, 0x2a, 0x4d, 0xd4, 0x31, 0x00,     \
                     0x2e, 0xc5, 0xa7)

enum {
    VM_UUID_SVC = 0x7a00,
    VM_UUID_CHR_CTRL = 0x7a01,    // 命令帧(write,加密)
    VM_UUID_CHR_PROFILE = 0x7a02, // profile 分块(write-no-rsp,尚未实现)
    VM_UUID_CHR_STATUS = 0x7a03,  // 状态与应答(read + notify)
    VM_UUID_CHR_EVENT = 0x7a04,   // 按键事件(notify)
    VM_UUID_CHR_AUDIO = 0x7a05,   // 语音串流块(notify)
};
