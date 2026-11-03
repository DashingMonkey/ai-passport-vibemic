// main/vm_keymap.c —— 见头文件;纯逻辑,host tests 覆盖。
#include "vm_keymap.h"

#include "vm_protocol.h"

bool vm_keymap_is_double_press(bool has_last_release, uint32_t last_release_ms,
                               uint32_t now_ms)
{
    if (!has_last_release) return false;
    // 无符号回绕:时间戳跨过 u32 上限时差值仍然正确。
    return (uint32_t)(now_ms - last_release_ms) <= VM_KEY_DOUBLE_WINDOW_MS;
}

bool vm_keymap_is_reset_hold(bsp_btn_t btn, uint32_t held_ms)
{
    // 只认 DOWN:UP 长按／OK 按住都有各自的既有语义(直发 Ctrl+Enter / 录音推流)。
    return btn == BSP_BTN_DOWN && held_ms >= VM_KEY_RESET_HOLD_MS;
}

int vm_keymap_slot(bsp_btn_t btn, bsp_btn_ev_t ev)
{
    switch (btn) {
    case BSP_BTN_OK:
        // OK 只做 PTT:PRESS/RELEASE 成对,CLICK/DOUBLE/LONG 一律忽略。
        if (ev == BSP_BTN_PRESS) return VM_SLOT_OK_PRESS;
        if (ev == BSP_BTN_RELEASE) return VM_SLOT_OK_RELEASE;
        break;
    case BSP_BTN_UP:
        if (ev == BSP_BTN_CLICK) return VM_SLOT_UP_CLICK;
        if (ev == BSP_BTN_LONG) return VM_SLOT_UP_LONG;
        if (ev == BSP_BTN_DOUBLE) return VM_SLOT_UP_DOUBLE;
        break;
    case BSP_BTN_DOWN:
        // 长按没有槽位(清空输入宏已移除):按住 DOWN 不发任何键,只上报事件。
        // 按住 10 秒的蓝牙硬重置由 vm_buttons 直接执行,刻意不做成槽位 ——
        // 恢复手势不能依赖 PC 会话或某个档位。
        if (ev == BSP_BTN_CLICK) return VM_SLOT_DOWN_CLICK;
        if (ev == BSP_BTN_DOUBLE) return VM_SLOT_DOWN_DOUBLE;
        break;
    }
    return -1;
}

uint8_t vm_keymap_ev_code(bsp_btn_ev_t ev)
{
    switch (ev) {
    case BSP_BTN_PRESS: return VM_BTN_EV_PRESS;
    case BSP_BTN_RELEASE: return VM_BTN_EV_RELEASE;
    case BSP_BTN_CLICK: return VM_BTN_EV_CLICK;
    case BSP_BTN_DOUBLE: return VM_BTN_EV_DOUBLE;
    case BSP_BTN_LONG: return VM_BTN_EV_LONG;
    }
    return VM_BTN_EV_CLICK; // 未知事件按单击上报,便于 PC 侧统计发现异常
}

bool vm_keymap_requires_pc(int slot)
{
    return slot == VM_SLOT_OK_PRESS || slot == VM_SLOT_OK_RELEASE;
}

// UP/DOWN 的单击与双击都归桌面端注入:确认键是 Enter 还是 Ctrl+Enter 由桌面端设置
// 决定,双击那两记的节奏也只有桌面端能按目标软件的窗口调。
static bool pc_handoff(int slot)
{
    return slot == VM_SLOT_UP_CLICK || slot == VM_SLOT_DOWN_CLICK ||
           slot == VM_SLOT_UP_DOUBLE || slot == VM_SLOT_DOWN_DOUBLE;
}

vm_keymap_action_t vm_keymap_action(int slot, bool pc_active)
{
    // 越界槽位一律不动作:vm_keymap_slot() 目前不会返回这种值,但 RUN_SLOT 的消费者
    // 会拿 slot 直接索引 profile 的 steps/step_count 数组 —— 这里挡住比信任调用方便宜。
    if (slot < 0 || slot >= VM_SLOT_COUNT) return VM_KEY_ACTION_NONE;

    if (vm_keymap_requires_pc(slot)) {
        if (!pc_active) return VM_KEY_ACTION_NONE; // 待机门控
        return slot == VM_SLOT_OK_PRESS ? VM_KEY_ACTION_PTT_START
                                        : VM_KEY_ACTION_PTT_STOP;
    }

    // UP/DOWN 单击与双击:PC 在线时交 PC 注入(见 pc_handoff),设备不直发,
    // 避免双发;PC 离线时保持直发(裸键盘模式)。
    if (pc_active && pc_handoff(slot)) {
        return VM_KEY_ACTION_NONE;
    }

    return VM_KEY_ACTION_RUN_SLOT;
}
