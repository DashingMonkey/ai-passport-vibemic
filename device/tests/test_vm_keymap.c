// tests/test_vm_keymap.c —— 按键映射纯逻辑 host tests(出货代码里第一块被覆盖的模块)。
//
//   cc -std=c11 -Wall -Wextra -Werror \
//      -Itests/bsp_stubs -Icomponents/bsp/include -Imain \
//      tests/test_vm_keymap.c main/vm_keymap.c
//
// 这里锁死的是四件容易出错的事:
//   1. OK 只做 PTT(PRESS/RELEASE),长短击一律不触发设备侧动作;
//   2. UP 有 CLICK/LONG/DOUBLE 槽位,DOWN 只有 CLICK/DOUBLE(DOWN 长按已移除),
//      PRESS/RELEASE 不上报动作;
//   3. bsp 事件号与协议事件号顺序不同(RELEASE 在 bsp 里是最后),必须显式映射;
//   4. 蓝牙硬重置手势(DOWN 按住 10 秒)的边界:恢复手段不能被日常操作误触。
#include <assert.h>
#include <stdio.h>

#include "vm_keymap.h"
#include "vm_protocol.h"

static const bsp_btn_ev_t ALL_EVENTS[] = {
    BSP_BTN_PRESS, BSP_BTN_RELEASE, BSP_BTN_CLICK, BSP_BTN_DOUBLE, BSP_BTN_LONG,
};
#define EVENT_COUNT (sizeof(ALL_EVENTS) / sizeof(ALL_EVENTS[0]))

static void test_ok_is_push_to_talk_only(void)
{
    assert(vm_keymap_slot(BSP_BTN_OK, BSP_BTN_PRESS) == VM_SLOT_OK_PRESS);
    assert(vm_keymap_slot(BSP_BTN_OK, BSP_BTN_RELEASE) == VM_SLOT_OK_RELEASE);
    assert(vm_keymap_slot(BSP_BTN_OK, BSP_BTN_CLICK) == -1);
    assert(vm_keymap_slot(BSP_BTN_OK, BSP_BTN_DOUBLE) == -1);
    assert(vm_keymap_slot(BSP_BTN_OK, BSP_BTN_LONG) == -1);
}

static void test_up_down_slots(void)
{
    assert(vm_keymap_slot(BSP_BTN_UP, BSP_BTN_CLICK) == VM_SLOT_UP_CLICK);
    assert(vm_keymap_slot(BSP_BTN_UP, BSP_BTN_LONG) == VM_SLOT_UP_LONG);
    assert(vm_keymap_slot(BSP_BTN_UP, BSP_BTN_DOUBLE) == VM_SLOT_UP_DOUBLE);
    assert(vm_keymap_slot(BSP_BTN_UP, BSP_BTN_PRESS) == -1);
    assert(vm_keymap_slot(BSP_BTN_UP, BSP_BTN_RELEASE) == -1);

    assert(vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_CLICK) == VM_SLOT_DOWN_CLICK);
    // DOWN 长按没有槽位(清空输入宏已移除):设备侧不发任何键。
    // 按住 10 秒的蓝牙硬重置由 vm_buttons 直接处理,不经过槽位表(见下面的手势用例)。
    assert(vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_LONG) == -1);
    assert(vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_DOUBLE) == VM_SLOT_DOWN_DOUBLE);
    assert(vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_PRESS) == -1);
    assert(vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_RELEASE) == -1);
}

// 双击槽位必须和单击槽位是不同槽:组件把"快按两下"报成 DOUBLE,若两者映射到同一槽,
// 双击就会退化成一次单击(实机上表现为"连按两下等于什么都没发")。
static void test_double_is_not_click(void)
{
    assert(vm_keymap_slot(BSP_BTN_UP, BSP_BTN_DOUBLE) !=
           vm_keymap_slot(BSP_BTN_UP, BSP_BTN_CLICK));
    assert(vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_DOUBLE) !=
           vm_keymap_slot(BSP_BTN_DOWN, BSP_BTN_CLICK));
}

// OK 双击判定(设备侧掐掉第二下的 PTT):窗口边界、首次按下、以及 u32 时间戳回绕。
// 判定必须"严进严出" —— 宁可把慢一点的双击当两次单击(照常录音),
// 也不能把一次正常按住说话误判成删除。
static void test_ok_double_press_window(void)
{
    // 本次上电还没抬起过:第一次按下永远是单击
    assert(!vm_keymap_is_double_press(false, 0, 0));
    assert(!vm_keymap_is_double_press(false, 12345, 12345));

    // 贴着窗口边界算双击;超出即算下一次单击
    assert(vm_keymap_is_double_press(true, 1000, 1000));
    assert(vm_keymap_is_double_press(true, 1000, 1000 + VM_KEY_DOUBLE_WINDOW_MS));
    assert(!vm_keymap_is_double_press(true, 1000, 1001 + VM_KEY_DOUBLE_WINDOW_MS));

    // 时间戳是 u32 毫秒(约 49 天回绕):跨回绕要按"差 16ms"算,不能算成负数
    assert(vm_keymap_is_double_press(true, 0xFFFFFFFFu - 10u, 5u));
    assert(!vm_keymap_is_double_press(true, 0xFFFFFFFFu - 10u, 500u));
}

// 槽位取值必须落在 vm_profile_t 的 steps/step_count 数组范围内
// (vm_buttons 的 run_slot 直接拿它当下标)。
static void test_slot_range(void)
{
    const bsp_btn_t buttons[] = { BSP_BTN_UP, BSP_BTN_DOWN, BSP_BTN_OK };
    for (size_t b = 0; b < sizeof(buttons) / sizeof(buttons[0]); b++) {
        for (size_t e = 0; e < EVENT_COUNT; e++) {
            const int slot = vm_keymap_slot(buttons[b], ALL_EVENTS[e]);
            assert(slot >= -1);
            assert(slot < VM_SLOT_COUNT);
        }
    }
}

static void test_event_codes_match_protocol_order(void)
{
    assert(vm_keymap_ev_code(BSP_BTN_PRESS) == VM_BTN_EV_PRESS);
    assert(vm_keymap_ev_code(BSP_BTN_RELEASE) == VM_BTN_EV_RELEASE);
    assert(vm_keymap_ev_code(BSP_BTN_CLICK) == VM_BTN_EV_CLICK);
    assert(vm_keymap_ev_code(BSP_BTN_DOUBLE) == VM_BTN_EV_DOUBLE);
    assert(vm_keymap_ev_code(BSP_BTN_LONG) == VM_BTN_EV_LONG);

    // 注意:两套枚举数值不同(bsp 把 RELEASE 放在最后),所以映射必须显式写;
    // 这里把这个事实钉住 —— 哪天有人把映射改成直接强转,这条断言会失败。
    // 先落成 int 再比较:两份枚举类型不同,裸比较会被 -Wenum-compare / MSVC C5287 拦下。
    const int bsp_release = (int)BSP_BTN_RELEASE;
    const int proto_release = (int)VM_BTN_EV_RELEASE;
    const int bsp_press = (int)BSP_BTN_PRESS;
    const int proto_press = (int)VM_BTN_EV_PRESS;
    assert(bsp_release != proto_release);
    assert(bsp_press == proto_press);
}

static void test_pc_gate_scope(void)
{
    assert(vm_keymap_requires_pc(VM_SLOT_OK_PRESS));
    assert(vm_keymap_requires_pc(VM_SLOT_OK_RELEASE));
    assert(!vm_keymap_requires_pc(VM_SLOT_UP_CLICK));
    assert(!vm_keymap_requires_pc(VM_SLOT_UP_LONG));
    assert(!vm_keymap_requires_pc(VM_SLOT_DOWN_CLICK));
    assert(!vm_keymap_requires_pc(VM_SLOT_UP_DOUBLE));
    assert(!vm_keymap_requires_pc(VM_SLOT_DOWN_DOUBLE));
    assert(!vm_keymap_requires_pc(-1));
}

// 动作策略矩阵:这是"OK 键策略"的唯一实现处,锁死它比在真机上试更可靠。
static void test_action_matrix(void)
{
    // OK:PC 在线 → 只做 PTT 采集(绝不 RUN_SLOT,否则会与桌面端识别+键入叠加)
    assert(vm_keymap_action(VM_SLOT_OK_PRESS, true) == VM_KEY_ACTION_PTT_START);
    assert(vm_keymap_action(VM_SLOT_OK_RELEASE, true) == VM_KEY_ACTION_PTT_STOP);
    // OK:PC 离线 → 待机门控拦住(不盲发语音键)
    assert(vm_keymap_action(VM_SLOT_OK_PRESS, false) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_OK_RELEASE, false) == VM_KEY_ACTION_NONE);

    // UP/DOWN 单击:PC 在线交 PC 注入;离线时设备直发
    assert(vm_keymap_action(VM_SLOT_UP_CLICK, true) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_DOWN_CLICK, true) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_UP_CLICK, false) == VM_KEY_ACTION_RUN_SLOT);
    assert(vm_keymap_action(VM_SLOT_DOWN_CLICK, false) == VM_KEY_ACTION_RUN_SLOT);

    // UP/DOWN 双击:与单击同一条分工(PC 在线交 PC 注入两记 Esc,离线设备直发)
    assert(vm_keymap_action(VM_SLOT_UP_DOUBLE, true) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_DOWN_DOUBLE, true) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_UP_DOUBLE, false) == VM_KEY_ACTION_RUN_SLOT);
    assert(vm_keymap_action(VM_SLOT_DOWN_DOUBLE, false) == VM_KEY_ACTION_RUN_SLOT);

    // UP 长按:两种情况下都由设备直发(通用键,不交 PC)
    assert(vm_keymap_action(VM_SLOT_UP_LONG, true) == VM_KEY_ACTION_RUN_SLOT);
    assert(vm_keymap_action(VM_SLOT_UP_LONG, false) == VM_KEY_ACTION_RUN_SLOT);

    // 无槽位事件:永远不触发设备侧动作
    assert(vm_keymap_action(-1, true) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(-1, false) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_COUNT, true) == VM_KEY_ACTION_NONE);
    assert(vm_keymap_action(VM_SLOT_COUNT, false) == VM_KEY_ACTION_NONE);
}

// 蓝牙硬重置手势(DOWN 按住 10 秒,见 vm_keymap.h 的 VM_KEY_RESET_HOLD_MS)。
// 这是"链路救不回来时唯一还能做的事",所以两边都要钉住:到点必须触发,
// 差 1ms 不能触发,别的键按多久都不算。
static void test_reset_hold_gesture(void)
{
    assert(!vm_keymap_is_reset_hold(BSP_BTN_DOWN, 0));
    assert(!vm_keymap_is_reset_hold(BSP_BTN_DOWN, VM_KEY_RESET_HOLD_MS - 1));
    assert(vm_keymap_is_reset_hold(BSP_BTN_DOWN, VM_KEY_RESET_HOLD_MS));
    assert(vm_keymap_is_reset_hold(BSP_BTN_DOWN, VM_KEY_RESET_HOLD_MS + 1));

    // 只认 DOWN:UP 长按在 500ms 就直发 Ctrl+Enter,OK 按住即开始录音推流
    assert(!vm_keymap_is_reset_hold(BSP_BTN_UP, VM_KEY_RESET_HOLD_MS));
    assert(!vm_keymap_is_reset_hold(BSP_BTN_OK, VM_KEY_RESET_HOLD_MS));
    assert(!vm_keymap_is_reset_hold(BSP_BTN_UP, 60000));
    assert(!vm_keymap_is_reset_hold(BSP_BTN_OK, 60000));

    // 时间戳回绕:按住时长是 u32 相减的结果,回绕后应算作"很短",不能触发
    const uint32_t press_ms = 0xFFFFFFFFu - 10u;
    const uint32_t now_ms = 5u; // 实际只按住 16ms
    assert(!vm_keymap_is_reset_hold(BSP_BTN_DOWN, now_ms - press_ms));
}

int main(void)
{
    test_ok_is_push_to_talk_only();
    test_up_down_slots();
    test_double_is_not_click();
    test_ok_double_press_window();
    test_slot_range();
    test_event_codes_match_protocol_order();
    test_pc_gate_scope();
    test_action_matrix();
    test_reset_hold_gesture();
    puts("vm_keymap host tests: PASS");
    return 0;
}
