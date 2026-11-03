// main/vm_keymap.h —— 按键事件 → 动作槽位 / 协议事件码的纯映射逻辑。
//
// 从 vm_buttons.c 抽出来单独成模块,是为了让这层"按键语义"能被宿主机测试覆盖:
// 这里是纯查表逻辑,不含任何 ESP-IDF / LVGL 依赖,可在 IDF 之外直接编译与测试。
//
// 语义:OK = PTT(只认 PRESS/RELEASE,长短击全部禁用;但**双击的第二下**是删除
// 手势,见 vm_keymap_is_double_press),
// UP/DOWN 单击 = 确认/取消,UP 长按 = 备用槽位(DOWN 长按**没有槽位**:该手势已移除,
// 长按 DOWN 不发任何键;按住 10 秒是设备本地的蓝牙硬重置,见 vm_keymap_is_reset_hold),
// UP/DOWN 双击 = 独立的双击槽位(DOWN 双击 = 两记 Esc,给"连按 Esc 停止"类软件用;
// UP 双击槽位预留为空 —— 两次 Enter 有误发风险,见 vm_profile.h)。
#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "bsp_button.h"
#include "vm_profile.h"

#ifdef __cplusplus
extern "C" {
#endif

// OK 双击判定窗口(ms):与桌面端同一口径
// (desktop/app/src-tauri/src/device.rs 的 OK_DOUBLE_WINDOW_MS)。
// 也和按键组件自己的短按窗口一致(BSP_BTN_SHORT_PRESS_MS = 180)。
#define VM_KEY_DOUBLE_WINDOW_MS 180

// 纯判定:这次按下是否落在"上次抬起"之后的双击窗口内。
//
// has_last_release = false(本次上电还没抬起过)时永远返回 false —— 第一次按键是单击。
// 时间戳用 esp_timer 毫秒(u32 约 49 天回绕),所以差值必须靠无符号回绕比较。
//
// 设备侧为什么也要判一次:桌面端把 OK 双击当"删除"时,只会丢掉音频、不会告诉设备,
// 设备若照旧启动 PTT,屏幕上就会一直演"录音中"(而且麦克风真的在推流)。两边用同一
// 个窗口各判一次,设备才能把这一下直接掐掉、改显示 DELETE。
bool vm_keymap_is_double_press(bool has_last_release, uint32_t last_release_ms,
                               uint32_t now_ms);

// 蓝牙硬重置手势:DOWN 持续按住这么久(ms)触发。
//
// 为什么是 10 秒、为什么只认 DOWN:
//   * 这是"链路救不回来时唯一还能做的事",必须几乎不可能误触,所以远长于长按(500ms);
//   * 硬件上识别不了多键组合(三键共用一个 ADC 分压节点,见 bsp_pins.h),只能单键;
//   * UP 长按在 500ms 就会直发 Ctrl+Enter(会打进焦点窗口),OK 按住即开始录音推流 ——
//     三个键里只有 DOWN 的长按槽位是空的,拿它承载恢复手势没有副作用。
#define VM_KEY_RESET_HOLD_MS 10000

// 该键持续按住 held_ms 是否构成硬重置手势(纯判定,host test 覆盖)。
// held_ms 由 u32 毫秒时间戳相减得到,回绕安全(只做 >= 比较)。
bool vm_keymap_is_reset_hold(bsp_btn_t btn, uint32_t held_ms);

// 事件 → 动作槽位;返回 -1 表示该事件不触发设备侧动作(仍会经 VM_EVENT 上报 PC)。
int vm_keymap_slot(bsp_btn_t btn, bsp_btn_ev_t ev);

// bsp 事件 → 协议 BUTTON_EVENT.event(vm_protocol.h 的 VM_BTN_EV_*)。
// bsp_btn_ev_t 的枚举顺序与协议取值不同(RELEASE 在 bsp 里排在最后),必须显式映射。
uint8_t vm_keymap_ev_code(bsp_btn_ev_t ev);

// 该槽位是否依赖"PC 会话活跃"(只有 OK 的 PTT 需要:语音不允许盲发到未知焦点窗口)。
bool vm_keymap_requires_pc(int slot);

// 槽位在"PC 会话是否活跃"时该做的设备侧动作(纯策略,host test 覆盖)。
typedef enum {
    VM_KEY_ACTION_NONE = 0,  // 不触发设备侧动作(事件仍照常上报 PC)
    VM_KEY_ACTION_PTT_START, // OK 按下:开始采集串流(PC 在线时设备只做这件事)
    VM_KEY_ACTION_PTT_STOP,  // OK 抬起:停止采集串流
    VM_KEY_ACTION_RUN_SLOT,  // 执行 profile 槽位宏(设备直发 HID)
} vm_keymap_action_t;

// 决定动作。这是"OK 键策略"的唯一实现处(vm_buttons.c 只做分发):
//   * OK 槽位:pc_active → PTT_START/STOP(**不发**内置档位宏,否则会与桌面端
//     "本地识别 + 键入焦点"叠加);!pc_active → NONE(待机门控:不盲发语音键)。
//   * UP/DOWN 单击与双击:pc_active → NONE(交 PC 注入确认/取消,双击的两记也由
//     PC 定节奏);!pc_active → RUN_SLOT(裸键盘模式,随时可用)。
//   * UP 长按与其它槽位:始终 RUN_SLOT。
vm_keymap_action_t vm_keymap_action(int slot, bool pc_active);

#ifdef __cplusplus
}
#endif
