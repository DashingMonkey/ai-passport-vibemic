// main/vm_profile.c —— 内置 profile 表 + NVS 激活态。
#include "vm_profile.h"

#include <string.h>

#include "esp_log.h"
#include "nvs.h"

static const char *TAG = "vm_profile";

#define VM_NVS_NAMESPACE "vm"
#define VM_NVS_KEY_ACTIVE "active"

// ---- 内置 profile 步骤表 ----
//
// ⚠ OK 槽位(VM_SLOT_OK_PRESS / VM_SLOT_OK_RELEASE)在 PC 会话活跃时
// 不触发(vm_buttons.c:PC 在线时 OK 只做 PTT 采集,避免与桌面端"识别 + 键入焦点"
// 叠加);PC 离线时 OK 又被待机门控拦住。也就是说下面这些 OK 动作目前**不可达**,
// 保留它们是为了无桌面端场景。UP/DOWN 的单击与双击槽位仍然生效。
//
// DOWN 长按不设槽位:清空输入宏(Ctrl+A → Delete)已从产品里移除,长按 DOWN 不发任何键
// (vm_keymap.c 的 DOWN 分支里没有 BSP_BTN_LONG)。按住 10 秒的蓝牙硬重置不走槽位表,
// 由 vm_buttons 直接执行 —— 恢复手势必须与档位、PC 会话都无关。
//
// UP/DOWN 双击:DOWN 双击 = 两记 Esc;UP 双击槽位空着(见 vm_profile.h 的说明),
// 空槽在 run_slot() 里是明确的 no-op。

// 切换式语音快捷键:按下发一击,松开发第二击(如 Trae Ctrl+Alt+V)。
static const vm_step_t STEPS_TOGGLE_PRESS[] = {
    { .type = VM_STEP_TAP, .mods = VM_MOD_CTRL | VM_MOD_ALT, .keycode = VM_KC_V },
};
// 按住式语音快捷键:按下保持修饰键,松开抬起(如微信 按住 Ctrl+Win)。
static const vm_step_t STEPS_HOLD_PRESS[] = {
    { .type = VM_STEP_DOWN, .mods = VM_MOD_CTRL | VM_MOD_GUI, .keycode = 0 },
};
static const vm_step_t STEPS_HOLD_RELEASE[] = {
    { .type = VM_STEP_UP },
};
// Windows 听写 Win+H,切换式。
static const vm_step_t STEPS_WINH[] = {
    { .type = VM_STEP_TAP, .mods = VM_MOD_GUI, .keycode = VM_KC_H },
};

static const vm_step_t STEPS_ENTER[] = {
    { .type = VM_STEP_TAP, .keycode = VM_KC_ENTER },
};
static const vm_step_t STEPS_CTRL_ENTER[] = {
    { .type = VM_STEP_TAP, .mods = VM_MOD_CTRL, .keycode = VM_KC_ENTER },
};
static const vm_step_t STEPS_ESC[] = {
    { .type = VM_STEP_TAP, .keycode = VM_KC_ESC },
};
// 双击 = 两记 Esc(通用"连按 Esc 停止/取消"手势;DSH 用双击 Esc 中断当前运行)。
// 中间隔 60ms:TAP 自带 10ms 报文节流,再加 60ms 让两记成为两次独立按键 ——
// 这类手势的窗口通常只有几百毫秒(TAP 20ms + DELAY 60ms ≈ 80ms,两头都安全)。
static const vm_step_t STEPS_ESC_ESC[] = {
    { .type = VM_STEP_TAP, .keycode = VM_KC_ESC },
    { .type = VM_STEP_DELAY, .delay_ms = 60 },
    { .type = VM_STEP_TAP, .keycode = VM_KC_ESC },
};

static const vm_profile_t BUILTINS[] = {
    {
        .id = 0,
        .name = "Trae",
        .steps = {
            [VM_SLOT_OK_PRESS] = STEPS_TOGGLE_PRESS,
            [VM_SLOT_OK_RELEASE] = STEPS_TOGGLE_PRESS, // 切换式:松开再发一次
            [VM_SLOT_UP_CLICK] = STEPS_ENTER,
            [VM_SLOT_UP_LONG] = STEPS_CTRL_ENTER,
            [VM_SLOT_DOWN_CLICK] = STEPS_ESC,
            [VM_SLOT_DOWN_DOUBLE] = STEPS_ESC_ESC,
        },
        .step_count = {
            [VM_SLOT_OK_PRESS] = 1,
            [VM_SLOT_OK_RELEASE] = 1,
            [VM_SLOT_UP_CLICK] = 1,
            [VM_SLOT_UP_LONG] = 1,
            [VM_SLOT_DOWN_CLICK] = 1,
            [VM_SLOT_DOWN_DOUBLE] = 3,
        },
    },
    {
        .id = 1,
        .name = "WeChat",
        .steps = {
            [VM_SLOT_OK_PRESS] = STEPS_HOLD_PRESS,
            [VM_SLOT_OK_RELEASE] = STEPS_HOLD_RELEASE,
            [VM_SLOT_UP_CLICK] = STEPS_ENTER,
            [VM_SLOT_UP_LONG] = STEPS_CTRL_ENTER,
            [VM_SLOT_DOWN_CLICK] = STEPS_ESC,
            [VM_SLOT_DOWN_DOUBLE] = STEPS_ESC_ESC,
        },
        .step_count = {
            [VM_SLOT_OK_PRESS] = 1,
            [VM_SLOT_OK_RELEASE] = 1,
            [VM_SLOT_UP_CLICK] = 1,
            [VM_SLOT_UP_LONG] = 1,
            [VM_SLOT_DOWN_CLICK] = 1,
            [VM_SLOT_DOWN_DOUBLE] = 3,
        },
    },
    {
        .id = 2,
        .name = "Win Dict",
        .steps = {
            [VM_SLOT_OK_PRESS] = STEPS_WINH,
            [VM_SLOT_OK_RELEASE] = STEPS_WINH,
            [VM_SLOT_UP_CLICK] = STEPS_ENTER,
            [VM_SLOT_UP_LONG] = STEPS_CTRL_ENTER,
            [VM_SLOT_DOWN_CLICK] = STEPS_ESC,
            [VM_SLOT_DOWN_DOUBLE] = STEPS_ESC_ESC,
        },
        .step_count = {
            [VM_SLOT_OK_PRESS] = 1,
            [VM_SLOT_OK_RELEASE] = 1,
            [VM_SLOT_UP_CLICK] = 1,
            [VM_SLOT_UP_LONG] = 1,
            [VM_SLOT_DOWN_CLICK] = 1,
            [VM_SLOT_DOWN_DOUBLE] = 3,
        },
    },
};

#define BUILTIN_COUNT (sizeof(BUILTINS) / sizeof(BUILTINS[0]))

static uint8_t s_active_id;

void vm_profile_init(void)
{
    nvs_handle_t h;
    if (nvs_open(VM_NVS_NAMESPACE, NVS_READONLY, &h) == ESP_OK) {
        uint8_t saved = 0;
        if (nvs_get_u8(h, VM_NVS_KEY_ACTIVE, &saved) == ESP_OK &&
            vm_profile_get(saved)) {
            s_active_id = saved;
        }
        nvs_close(h);
    }
    ESP_LOGI(TAG, "profile 就绪:内置 %u 个,激活 id=%u (%s)",
             (unsigned)BUILTIN_COUNT, s_active_id, vm_profile_active()->name);
}

const vm_profile_t *vm_profile_active(void)
{
    return &BUILTINS[s_active_id < BUILTIN_COUNT ? s_active_id : 0];
}

uint8_t vm_profile_active_id(void)
{
    return vm_profile_active()->id;
}

esp_err_t vm_profile_set_active(uint8_t id)
{
    if (!vm_profile_get(id)) return ESP_ERR_INVALID_ARG;
    s_active_id = id;

    nvs_handle_t h;
    esp_err_t err = nvs_open(VM_NVS_NAMESPACE, NVS_READWRITE, &h);
    if (err == ESP_OK) {
        err = nvs_set_u8(h, VM_NVS_KEY_ACTIVE, id);
        if (err == ESP_OK) err = nvs_commit(h);
        nvs_close(h);
    }
    if (err != ESP_OK) {
        // 内存态已切换,仅持久化失败:本次会话仍按新 profile 工作。
        ESP_LOGW(TAG, "激活 id=%u 持久化失败: %s", id, esp_err_to_name(err));
        return err;
    }
    ESP_LOGI(TAG, "激活 profile id=%u (%s)", id, vm_profile_active()->name);
    return ESP_OK;
}

size_t vm_profile_count(void)
{
    return BUILTIN_COUNT;
}

const vm_profile_t *vm_profile_get(uint8_t id)
{
    for (size_t i = 0; i < BUILTIN_COUNT; i++) {
        if (BUILTINS[i].id == id) return &BUILTINS[i];
    }
    return NULL;
}
