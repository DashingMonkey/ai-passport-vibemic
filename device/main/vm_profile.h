// main/vm_profile.h —— profile 模型(按键→动作映射)与设备端存储。
//
// 设备端范围:内置 profile(编译期常量)+ NVS 持久化激活 id;桌面端 JSON profile
// 经 VM_PROFILE 分块写入与解析尚未实现。HID 键码/修饰键位与
// HID 报文语义一致(Usage Page 0x07),由 vm_hid 直接消费。
#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

// 修饰键位(HID 键盘报文第 0 字节)。
enum {
    VM_MOD_CTRL = 0x01,
    VM_MOD_SHIFT = 0x02,
    VM_MOD_ALT = 0x04,
    VM_MOD_GUI = 0x08, // Win / ⌘
};

// 常用 HID 键码(Usage Page 0x07 子集;完整表在桌面端协议 crate 维护)。
enum {
    VM_KC_A = 0x04,        // … 字母 A..Z 依次 0x04..0x1D
    VM_KC_H = 0x0B,
    VM_KC_V = 0x19,
    VM_KC_ENTER = 0x28,
    VM_KC_ESC = 0x29,
    VM_KC_BACKSPACE = 0x2A,
    VM_KC_TAB = 0x2B,
    VM_KC_SPACE = 0x2C,
    VM_KC_DELETE = 0x4C,
};

// 宏步骤。tap=按下+抬起;down/up 配对实现按住式语音(PRESS 发 down,
// RELEASE 发 up);delay 单位 ms。
typedef enum {
    VM_STEP_TAP = 0,
    VM_STEP_DOWN,
    VM_STEP_UP,
    VM_STEP_DELAY,
} vm_step_type_t;

typedef struct {
    vm_step_type_t type;
    uint8_t mods;     // VM_MOD_* 位或(TAP/DOWN 时生效)
    uint8_t keycode;  // HID 键码(TAP/DOWN 时生效)
    uint16_t delay_ms; // DELAY 时长;TAP 内部节流另计
} vm_step_t;

// 触发槽:OK 的按下/抬起、UP/DOWN 的单击、UP 的长按,再加两个双击槽。
// **DOWN 长按刻意没有槽位**(清空输入宏已移除):vm_keymap_slot() 对它返回 -1,
// 设备与桌面端都不产生按键,事件本身仍经 VM_EVENT 上报。设备本地还有一个不经过
// 槽位表的 DOWN 手势 —— 按住 10 秒硬重置蓝牙(见 vm_keymap.h),换档位也影响不到它。
// 新增槽位一律追加在末尾:既有编号是 profile 表指定初始化器与 run_slot() 的下标,
// 插进中间会让所有旧编号位移。
enum {
    VM_SLOT_OK_PRESS = 0,
    VM_SLOT_OK_RELEASE,
    VM_SLOT_UP_CLICK,
    VM_SLOT_UP_LONG,
    VM_SLOT_DOWN_CLICK,
    VM_SLOT_UP_DOUBLE,   // 预留:双击 UP 暂无自然语义(两次 Enter 有误发风险)
    VM_SLOT_DOWN_DOUBLE, // 双击 DOWN = 两记 Esc(软件侧"连按 Esc 停止"手势)
    VM_SLOT_COUNT,
};

typedef struct {
    uint8_t id;
    const char *name;                       // 主屏大字显示
    const vm_step_t *steps[VM_SLOT_COUNT];  // NULL = 该槽无动作
    uint8_t step_count[VM_SLOT_COUNT];
} vm_profile_t;

// NVS 已由调用方初始化(nvs_flash_init)后调用。读取持久化的激活 id,
// 无效或缺失时回退 0。
void vm_profile_init(void);

const vm_profile_t *vm_profile_active(void);
uint8_t vm_profile_active_id(void);

// 切换激活 profile(须为已知 id):写 NVS 并更新内存;NVS 写失败返回错误
// 但内存态仍已切换(设备可用性优先,重启后回旧值)。
esp_err_t vm_profile_set_active(uint8_t id);

size_t vm_profile_count(void);
const vm_profile_t *vm_profile_get(uint8_t id); // 未知 id 返回 NULL

#ifdef __cplusplus
}
#endif
