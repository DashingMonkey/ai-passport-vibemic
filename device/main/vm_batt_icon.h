// main/vm_batt_icon.h —— 顶栏电池显示的纯逻辑:电量百分比 → 电池图标 + 颜色。
// 不依赖 LVGL/ESP-IDF,便于 host tests 锁边界(同 vm_backlight)。
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// 五档电池图标,与 LVGL 内置符号 LV_SYMBOL_BATTERY_{FULL,3,2,1,EMPTY} 的
// UTF-8 字节完全一致(FontAwesome U+F240..U+F244;montserrat_14 自带这些字形,
// 屏上画的就是"外框 + 按档位填充"的经典电池)。自持一份而不引 lvgl.h:
// 本模块要进 host tests,lvgl.h 没有 host 桩。
#define VM_BATT_ICON_FULL  "\xEF\x89\x80"
#define VM_BATT_ICON_3     "\xEF\x89\x81"
#define VM_BATT_ICON_2     "\xEF\x89\x82"
#define VM_BATT_ICON_1     "\xEF\x89\x83"
#define VM_BATT_ICON_EMPTY "\xEF\x89\x84"

// 档位边界(含):达到该百分比才翻到这一档。四档大致对应 1/4 电量,
// 边界略偏"更满"一档——图标略乐观于读数,好过明明有电却画空瓶。
#define VM_BATT_ICON_FULL_AT 90
#define VM_BATT_ICON_3_AT    65
#define VM_BATT_ICON_2_AT    40
#define VM_BATT_ICON_1_AT    15

// 低于该百分比(且读数可用)整块标红提醒;与 app 的 REC/删除手势同一警示色。
#define VM_BATT_ICON_LOW_AT 20

// 顶栏电量文字的两种颜色(与 vibemic_app 顶栏配色一致)。
#define VM_BATT_COLOR_HEX_NORMAL 0x8FA3BF
#define VM_BATT_COLOR_HEX_LOW    0xE5484D

/// 电量百分比 → 图标字符串。soc >= 100(含超界读数)按满电;
/// soc < 0(读数不可用)返回 EMPTY——调用方此时应直接隐藏整个标签,
/// 不把这里的返回画出来。
const char *vm_batt_icon_symbol(int soc);

/// 电量百分比 → 顶栏文字颜色(十六进制 RGB,配 lv_color_hex 使用)。
/// 只有"读数可用且低于 VM_BATT_ICON_LOW_AT"才算低电量;soc < 0 不算。
uint32_t vm_batt_icon_color_hex(int soc);

#ifdef __cplusplus
}
#endif
