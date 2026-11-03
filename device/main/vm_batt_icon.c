// main/vm_batt_icon.c —— 电池图标档位:见 vm_batt_icon.h。
#include <stdbool.h>

#include "vm_batt_icon.h"

const char *vm_batt_icon_symbol(int soc)
{
    if (soc >= VM_BATT_ICON_FULL_AT) return VM_BATT_ICON_FULL;
    if (soc >= VM_BATT_ICON_3_AT) return VM_BATT_ICON_3;
    if (soc >= VM_BATT_ICON_2_AT) return VM_BATT_ICON_2;
    if (soc >= VM_BATT_ICON_1_AT) return VM_BATT_ICON_1;
    return VM_BATT_ICON_EMPTY; // 含 soc < 0:调用方负责隐藏标签
}

uint32_t vm_batt_icon_color_hex(int soc)
{
    const bool low = soc >= 0 && soc < VM_BATT_ICON_LOW_AT;
    return low ? VM_BATT_COLOR_HEX_LOW : VM_BATT_COLOR_HEX_NORMAL;
}
