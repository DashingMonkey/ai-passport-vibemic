// main/vm_buttons.h —— 按键事件 → 动作解析:查当前 profile → 投递 HID 宏队列,
// 并向 PC 上报 BUTTON_EVENT。
//
// bsp 按键回调运行于共享 esp_timer 任务:本模块回调只入队;解析与 HID 投递
// 在独立工作任务完成(上游规则:回调禁止慢操作)。
#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "bsp_button.h"

#ifdef __cplusplus
extern "C" {
#endif

// 创建输入队列与工作任务(幂等;失败返回错误码)。
// 随后把 vm_buttons_on_key 注册为 bsp_button_init 的回调。
void vm_buttons_init(void);

// bsp_button_init 回调(esp_timer 任务上下文,只入队)。
void vm_buttons_on_key(bsp_btn_t btn, bsp_btn_ev_t ev, void *user);

// OK 键当前是否处于按住状态(PTT 指示轮询用)。
bool vm_buttons_ptt_active(void);

// OK 双击的第二下是否处于"删除手势"中(屏上显示 DELETE 用)。
// 这一下不启动 PTT:桌面端把 OK 双击当删除,设备不该同时开麦克风演录音。
bool vm_buttons_delete_active(void);

// 蓝牙硬重置手势(vm_keymap.h 的 VM_KEY_RESET_HOLD_MS)的进度与结果,供 UI 轮询:
//   * reset_hold_ms:DOWN 当前已按住的毫秒数;没按住时为 0;
//   * reset_done_us:上一次硬重置完成时刻(esp_timer 微秒);0 = 本次上电还没触发过。
// 手势与 profile 槽位无关:设备本地执行,PC 离线也能用(恢复手段必须随时可用)。
uint32_t vm_buttons_reset_hold_ms(void);
int64_t vm_buttons_reset_done_us(void);

#ifdef __cplusplus
}
#endif
