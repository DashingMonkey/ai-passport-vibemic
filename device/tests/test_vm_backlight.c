// tests/test_vm_backlight.c —— 空闲背光策略纯逻辑 host tests。
//
//   cc -std=c11 -Wall -Wextra -Werror -Imain \
//      tests/test_vm_backlight.c main/vm_backlight.c
//
// 这里锁死的是四件容易出错的事:
//   1. 两个门限的边界:60s 整、180s 整该翻档,差 1ms 不该翻;
//   2. 调暗 ≠ 熄灭:中间那档必须亮着(否则用户分不清"待机"和"关机");
//   3. 录音/删除手势期间不调暗(busy 抑制一切档位);
//   4. u32 毫秒时间戳回绕后,空闲时长仍然算得对(与 vm_keymap 同一口径)。
#include <assert.h>
#include <stdio.h>

#include "vm_backlight.h"

static void test_thresholds(void)
{
    // 刚有动作 / 还没到 60s:全亮
    assert(vm_bl_state_for_idle(0, false) == VM_BL_ON);
    assert(vm_bl_state_for_idle(1, false) == VM_BL_ON);
    assert(vm_bl_state_for_idle(VM_BL_DIM_AFTER_MS - 1, false) == VM_BL_ON);

    // 60s 整开始调暗,一直到 180s 之前
    assert(vm_bl_state_for_idle(VM_BL_DIM_AFTER_MS, false) == VM_BL_DIM);
    assert(vm_bl_state_for_idle(VM_BL_DIM_AFTER_MS + 1, false) == VM_BL_DIM);
    assert(vm_bl_state_for_idle(VM_BL_OFF_AFTER_MS - 1, false) == VM_BL_DIM);

    // 180s 整开始熄屏
    assert(vm_bl_state_for_idle(VM_BL_OFF_AFTER_MS, false) == VM_BL_OFF);
    assert(vm_bl_state_for_idle(VM_BL_OFF_AFTER_MS + 1, false) == VM_BL_OFF);
    // 放了一整天(甚至更久)也还是熄屏,不能因为数值变大而翻回全亮
    assert(vm_bl_state_for_idle(24u * 60u * 60u * 1000u, false) == VM_BL_OFF);
    assert(vm_bl_state_for_idle(UINT32_MAX, false) == VM_BL_OFF);
}

static void test_dim_is_a_real_level(void)
{
    // 档位顺序与门限顺序必须一致(ON < DIM < OFF)
    assert(VM_BL_ON < VM_BL_DIM && VM_BL_DIM < VM_BL_OFF);
    assert(VM_BL_DIM_AFTER_MS < VM_BL_OFF_AFTER_MS);

    // 调暗档必须亮着:0 是"熄屏",拿它当调暗就等于提前黑屏
    assert(vm_bl_percent(VM_BL_ON) == VM_BL_ON_PERCENT);
    assert(vm_bl_percent(VM_BL_DIM) == VM_BL_DIM_PERCENT);
    assert(vm_bl_percent(VM_BL_OFF) == VM_BL_OFF_PERCENT);
    assert(vm_bl_percent(VM_BL_DIM) > 0);
    assert(vm_bl_percent(VM_BL_DIM) < vm_bl_percent(VM_BL_ON));
    assert(vm_bl_percent(VM_BL_OFF) == 0);

    // 日志用的短名
    assert(vm_bl_state_name(VM_BL_ON)[0] == 'o' && vm_bl_state_name(VM_BL_ON)[1] == 'n');
    assert(vm_bl_state_name(VM_BL_DIM)[0] == 'd');
    assert(vm_bl_state_name(VM_BL_OFF)[0] == 'o' && vm_bl_state_name(VM_BL_OFF)[1] == 'f');
}

// 录音(PTT)/ 删除手势期间算持续活动:不管空闲多久都不许调暗或熄屏。
static void test_busy_keeps_the_screen_on(void)
{
    assert(vm_bl_state_for_idle(0, true) == VM_BL_ON);
    assert(vm_bl_state_for_idle(VM_BL_DIM_AFTER_MS, true) == VM_BL_ON);
    assert(vm_bl_state_for_idle(VM_BL_OFF_AFTER_MS, true) == VM_BL_ON);
    assert(vm_bl_state_for_idle(UINT32_MAX, true) == VM_BL_ON);
}

// 空闲时长由调用方用 u32 相减得到((uint32_t)(now_ms - last_ms)),回绕必须安全:
// 回绕后只差几百毫秒时**不能**被算成"放了 49 天"而直接熄屏。
static void test_wraparound_idle_time(void)
{
    const uint32_t last_before_wrap = 0xFFFFFF00u; // 4294967040
    const uint32_t now_after_wrap = 5u;
    const uint32_t idle = now_after_wrap - last_before_wrap; // 261ms
    assert(idle == 261u);
    assert(vm_bl_state_for_idle(idle, false) == VM_BL_ON);

    // 回绕前就开始空闲、回绕后仍在空闲:累计时长照常翻档
    const uint32_t last = 4294967000u; // 回绕前 296ms 的位置
    const uint32_t now = 60000u;       // 已经回绕到 0 之后
    const uint32_t wrapped_idle = now - last; // 实际空闲 60296ms
    assert(wrapped_idle == 60296u);
    assert(vm_bl_state_for_idle(wrapped_idle, false) == VM_BL_DIM);
}

int main(void)
{
    test_thresholds();
    test_dim_is_a_real_level();
    test_busy_keeps_the_screen_on();
    test_wraparound_idle_time();
    puts("vm_backlight host tests: PASS");
    return 0;
}
