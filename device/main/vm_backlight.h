// main/vm_backlight.h —— 空闲背光策略(纯逻辑,host test 覆盖)。
//
// 本设备是 520mAh 电池供电的手持件,屏幕不能永远 100% 亮着:上游编码规范
// (docs/development/engineering/coding-conventions.md)明确要求"屏幕空闲时调暗或
// 关闭背光",上游产品规格(specifications.md 的"电源行为")也写了"自动息屏后按
// 功能键唤醒"。策略只有三档,都按"距最近一次用户动作的时长"判定:
//
//   空闲 < 60s   → 全亮 100%
//   空闲 ≥ 60s   → 调暗 20%
//   空闲 ≥ 180s  → 熄屏(背光 0%)
//   任意按键     → 立刻回全亮,且**不吞掉这次按键**(它照常走原有按键流程)
//
// 这里只有"多久算多久"的纯判定;计时取样与背光写入在 vibemic_app.c,所以本模块
// 不 include 任何 ESP-IDF / LVGL 头文件,可在 IDF 之外直接编译与测试。
//
// "熄屏"只关背光,**不发 ST7789 的 Sleep In / DISPOFF**:BSP 里那条路是
// deep sleep 专用的终端接口(bsp_display_prepare_deep_sleep),调用后总线不可在
// 本次运行中恢复,拿它做可恢复的息屏会让按键唤醒再也点不亮屏幕。
#pragma once

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// 调暗 / 熄屏门限(ms)。u32 毫秒时间戳回绕(约 49 天)由"无符号相减"处理,
// 与 vm_keymap 的双击窗口同一口径。
#define VM_BL_DIM_AFTER_MS (60u * 1000u)
#define VM_BL_OFF_AFTER_MS (180u * 1000u)

// 三档亮度(%):0 = 全灭(bsp_display_backlight 的取值域是 0..100)。
#define VM_BL_ON_PERCENT  100u
#define VM_BL_DIM_PERCENT 20u
#define VM_BL_OFF_PERCENT 0u

typedef enum {
    VM_BL_ON = 0, // 全亮
    VM_BL_DIM,    // 调暗:画面还在,能看清状态,但明显是"待机中"
    VM_BL_OFF,    // 熄屏:只关背光(见文件头,不发面板命令)
} vm_bl_state_t;

// 目标档位。idle_ms = 距最近一次用户动作的时长(调用方用 u32 相减得到)。
//
// busy = 正在录音(PTT)/ 删除手势进行中:这类"用户正拿着设备"的状态算持续活动,
// 期间不调暗 —— 按住 OK 说一段长话时屏幕突然黑了,用户只会以为设备挂了。
vm_bl_state_t vm_bl_state_for_idle(uint32_t idle_ms, bool busy);

// 档位 → 背光百分比。
uint8_t vm_bl_percent(vm_bl_state_t state);

// 档位 → 稳定短名(日志/实机验证用,背光只在真的变化时才打一条)。
const char *vm_bl_state_name(vm_bl_state_t state);

#ifdef __cplusplus
}
#endif
