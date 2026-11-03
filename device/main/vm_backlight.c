// main/vm_backlight.c —— 空闲背光策略,见 vm_backlight.h。
// 纯逻辑:不含 ESP-IDF / LVGL 依赖,由 tests/test_vm_backlight.c 覆盖。
#include "vm_backlight.h"

vm_bl_state_t vm_bl_state_for_idle(uint32_t idle_ms, bool busy)
{
    if (busy) return VM_BL_ON;
    if (idle_ms >= VM_BL_OFF_AFTER_MS) return VM_BL_OFF;
    if (idle_ms >= VM_BL_DIM_AFTER_MS) return VM_BL_DIM;
    return VM_BL_ON;
}

uint8_t vm_bl_percent(vm_bl_state_t state)
{
    switch (state) {
    case VM_BL_ON:
        return VM_BL_ON_PERCENT;
    case VM_BL_DIM:
        return VM_BL_DIM_PERCENT;
    case VM_BL_OFF:
        return VM_BL_OFF_PERCENT;
    }
    // 未知档位:退回全亮。宁可多耗电,也不要因为一个坏枚举把屏幕留在黑屏上。
    return VM_BL_ON_PERCENT;
}

const char *vm_bl_state_name(vm_bl_state_t state)
{
    switch (state) {
    case VM_BL_ON:
        return "on";
    case VM_BL_DIM:
        return "dim";
    case VM_BL_OFF:
        return "off";
    }
    return "on";
}
