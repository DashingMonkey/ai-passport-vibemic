// tests/test_vm_batt_icon.c —— 电池图标档位纯逻辑 host tests。
//
//   cc -std=c11 -Wall -Wextra -Werror -Imain \
//      tests/test_vm_batt_icon.c main/vm_batt_icon.c
//
// 这里锁死的是三件容易出错的事:
//   1. 五档边界:90/65/40/15 整点翻档,差 1 不翻;超界读数(>100)按满电;
//   2. 图标字节必须就是 LVGL 的 FontAwesome 电池字形(U+F240..F244 的
//      3 字节 UTF-8)且互不相同——转义写错一个,屏上就是豆腐块或重复图标;
//   3. 低电量红只在 0 <= soc < 20 出现;读数不可用(-1)不算低电量,
//      它的标签会被整个隐藏,颜色无关紧要但行为要稳定。
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "vm_batt_icon.h"

static void test_levels(void)
{
    assert(vm_batt_icon_symbol(-1) == VM_BATT_ICON_EMPTY);
    assert(vm_batt_icon_symbol(0) == VM_BATT_ICON_EMPTY);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_1_AT - 1) == VM_BATT_ICON_EMPTY);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_1_AT) == VM_BATT_ICON_1);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_2_AT - 1) == VM_BATT_ICON_1);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_2_AT) == VM_BATT_ICON_2);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_3_AT - 1) == VM_BATT_ICON_2);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_3_AT) == VM_BATT_ICON_3);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_FULL_AT - 1) == VM_BATT_ICON_3);
    assert(vm_batt_icon_symbol(VM_BATT_ICON_FULL_AT) == VM_BATT_ICON_FULL);
    assert(vm_batt_icon_symbol(100) == VM_BATT_ICON_FULL);
    assert(vm_batt_icon_symbol(101) == VM_BATT_ICON_FULL); // 超界读数按满电
}

// 五个字形必须是 U+F240..U+F244 的 3 字节 UTF-8(EF 89 80..84)且互不相同。
static void test_symbols_are_distinct_battery_glyphs(void)
{
    static const char *const icons[] = {
        VM_BATT_ICON_FULL, VM_BATT_ICON_3, VM_BATT_ICON_2,
        VM_BATT_ICON_1, VM_BATT_ICON_EMPTY,
    };
    for (int i = 0; i < 5; i++) {
        const unsigned char *g = (const unsigned char *)icons[i];
        assert(strlen(icons[i]) == 3);
        assert(g[0] == 0xEF && g[1] == 0x89);        // U+F24x 固定高位字节
        assert(g[2] == (unsigned char)(0x80 + i));   // 依次 F240..F244
        for (int j = i + 1; j < 5; j++) {
            assert(strcmp(icons[i], icons[j]) != 0);
        }
    }
}

static void test_colors(void)
{
    assert(vm_batt_icon_color_hex(-1) == VM_BATT_COLOR_HEX_NORMAL);
    assert(vm_batt_icon_color_hex(0) == VM_BATT_COLOR_HEX_LOW);
    assert(vm_batt_icon_color_hex(VM_BATT_ICON_LOW_AT - 1) == VM_BATT_COLOR_HEX_LOW);
    assert(vm_batt_icon_color_hex(VM_BATT_ICON_LOW_AT) == VM_BATT_COLOR_HEX_NORMAL);
    assert(vm_batt_icon_color_hex(100) == VM_BATT_COLOR_HEX_NORMAL);
    // 两色若相同,"低电量变红"根本不可见
    assert(VM_BATT_COLOR_HEX_LOW != VM_BATT_COLOR_HEX_NORMAL);
}

int main(void)
{
    test_levels();
    test_symbols_are_distinct_battery_glyphs();
    test_colors();
    puts("vm_batt_icon host tests: PASS");
    return 0;
}
