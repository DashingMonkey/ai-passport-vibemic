// main/vm_buttons.c —— 按键事件解析与动作分发。
//
// 槽位映射与协议事件码在 vm_keymap(纯逻辑,有 host test);本文件只负责:
//   bsp 按键回调 → 队列 → 输入任务 → 上报 PC + 执行设备侧动作。
// 所有事件同时经 VM_EVENT 上报 PC(高级模式统计用,尽力而为)。
//
// OK 键策略:
//   * PC 会话活跃 → **只做 PTT 采集**,不发任何内置档位宏。否则会与桌面端
//     "本地识别 + 键入焦点"叠加(微信档按住 OK 会同时把 Ctrl+Win 按住,
//     把微信自己的语音输入也唤起来)。
//   * PC 会话不活跃 → OK 被待机门控拦住(语音不允许盲发到未知焦点窗口),
//     即设备内置的 OK 档位宏不生效;它们保留在 vm_profile.c 里,
//     供无桌面端场景参考。
//   * UP/DOWN 是通用键:PC 活跃时单击/双击交 PC 注入,PC 离线时设备直发。
#include "vm_buttons.h"

#include <stddef.h>

#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

#include "vm_audio.h"
#include "vm_hid.h"
#include "vm_keymap.h"
#include "vm_link.h"
#include "vm_profile.h"
#include "vm_protocol.h"

static const char *TAG = "vm_btn";

#define INPUT_QUEUE_DEPTH 8
#define BUTTONS_TASK_STACK 3072

// 按住手势靠轮询推进:按住期间没有任何按键事件,只有队列超时这一拍能算时长。
// 100ms 的粒度对 10 秒的手势足够(最多晚触发 100ms),代价是空闲时每 100ms 醒一次。
#define INPUT_POLL_MS 100

typedef struct {
    bsp_btn_t btn;
    bsp_btn_ev_t ev;
    // 事件发生时刻(esp_timer 毫秒)。必须在回调里取:入队与出队之间有等待,
    // 在任务里取时刻会把双击窗口算歪。
    uint32_t at_ms;
} input_event_t;

static QueueHandle_t s_input_queue;
static TaskHandle_t s_input_task;
static volatile bool s_input_ready;
static volatile bool s_ptt_active;
// OK 双击的第二下 = 桌面端的删除手势:这一下不录音,屏上改显示 DELETE。
static volatile bool s_delete_active;
static uint32_t s_ok_release_ms;
static bool s_ok_released;

// DOWN 硬重置手势(见 vm_keymap.h 的 VM_KEY_RESET_HOLD_MS):PRESS/RELEASE 只记账,
// 判定在 input_task 的超时那一拍做。
static volatile bool s_down_held;
static uint32_t s_down_press_ms;
static volatile uint32_t s_reset_hold_ms;
static volatile bool s_reset_fired;
static volatile int64_t s_reset_done_us;

void vm_buttons_on_key(bsp_btn_t btn, bsp_btn_ev_t ev, void *user)
{
    (void)user;
    if (!s_input_ready || !s_input_queue) return;
    const input_event_t input = {
        .btn = btn,
        .ev = ev,
        .at_ms = (uint32_t)(esp_timer_get_time() / 1000),
    };
    (void)xQueueSend(s_input_queue, &input, 0);
}

bool vm_buttons_ptt_active(void)
{
    return s_ptt_active;
}

bool vm_buttons_delete_active(void)
{
    return s_delete_active;
}

uint32_t vm_buttons_reset_hold_ms(void)
{
    return s_down_held ? s_reset_hold_ms : 0;
}

int64_t vm_buttons_reset_done_us(void)
{
    return s_reset_done_us;
}

static void run_slot(int slot)
{
    const vm_profile_t *p = vm_profile_active();
    if (!p->steps[slot] || !p->step_count[slot]) return;
    vm_hid_run_steps(p->steps[slot], p->step_count[slot]);
}

// DOWN 按住 VM_KEY_RESET_HOLD_MS = 蓝牙硬重置(清 bond + 重新广播)。
//
// 这是"链路救不回来时唯一还能做的事":由设备本地执行,不看 PC 会话、不走 profile 槽位
// (所以换档位/PC 离线都不影响它)。一次按住只触发一次,松手后才允许下一次。
static void check_reset_hold(void)
{
    if (!s_down_held || s_reset_fired) return;

    const uint32_t now_ms = (uint32_t)(esp_timer_get_time() / 1000);
    const uint32_t held_ms = now_ms - s_down_press_ms;
    s_reset_hold_ms = held_ms;
    if (!vm_keymap_is_reset_hold(BSP_BTN_DOWN, held_ms)) return;

    s_reset_fired = true;
    ESP_LOGW(TAG, "DOWN 已按住 %ums → 蓝牙硬重置(清配对 + 重新广播)",
             (unsigned)held_ms);
    if (vm_hid_bond_reset() != ESP_OK) {
        // 失败不置"已完成":屏幕上不该出现成功提示,用户得知道还得再试。
        ESP_LOGE(TAG, "蓝牙硬重置失败");
        return;
    }
    s_reset_done_us = esp_timer_get_time();
}

static void input_task(void *arg)
{
    (void)arg;
    input_event_t input;
    for (;;) {
        const bool got =
            xQueueReceive(s_input_queue, &input, pdMS_TO_TICKS(INPUT_POLL_MS)) == pdTRUE;
        // 每轮都推进一次按住判定:按住期间队列是空的,只有超时这一拍能让它前进。
        check_reset_hold();
        if (!got) continue;

        // BUTTON_EVENT 上报(尽力而为;PC 未订阅时内部丢弃)。
        vm_link_notify_button_event((uint8_t)input.btn, vm_keymap_ev_code(input.ev));

        // DOWN 的按下/抬起只给硬重置手势计时(它的长按槽位是空的,不发任何键)。
        if (input.btn == BSP_BTN_DOWN) {
            if (input.ev == BSP_BTN_PRESS) {
                s_down_press_ms = input.at_ms;
                s_down_held = true;
                s_reset_fired = false;
                s_reset_hold_ms = 0;
            } else if (input.ev == BSP_BTN_RELEASE) {
                s_down_held = false;
                s_reset_hold_ms = 0;
                s_reset_fired = false; // 松手结束这一次手势
            }
        }

        // OK 双击的第二下 = 桌面端的删除手势:这一下**不启动 PTT**(麦克风不开、
        // 不推流),屏上由 vibemic_app 改显示 DELETE。事件已在上面照常上报 PC,
        // 桌面端仍按它自己的判定执行删除。
        if (input.btn == BSP_BTN_OK) {
            if (input.ev == BSP_BTN_PRESS &&
                vm_keymap_is_double_press(s_ok_released, s_ok_release_ms, input.at_ms)) {
                s_delete_active = true;
                if (s_ptt_active) { // 正常双击时第一下已收尾;这里是防丢事件的兜底
                    s_ptt_active = false;
                    vm_audio_stop();
                }
                ESP_LOGI(TAG, "OK 双击第二下 → 删除手势(不录音)");
                continue;
            }
            if (input.ev == BSP_BTN_RELEASE) {
                s_ok_release_ms = input.at_ms;
                s_ok_released = true;
                if (s_delete_active) {
                    s_delete_active = false;
                    ESP_LOGI(TAG, "删除手势结束");
                    continue; // 这一下没起录,不需要收尾
                }
            }
        }

        const int slot = vm_keymap_slot(input.btn, input.ev);
        switch (vm_keymap_action(slot, vm_link_pc_active())) {
        case VM_KEY_ACTION_PTT_START:
            s_ptt_active = true;
            vm_audio_start(); // 按住说话:设备侧采集串流(异步投递)
            ESP_LOGI(TAG, "btn=%d ev=%d → PTT start(PC 在线,不发内置宏)", input.btn,
                     input.ev);
            break;
        case VM_KEY_ACTION_PTT_STOP:
            s_ptt_active = false;
            vm_audio_stop();
            ESP_LOGI(TAG, "btn=%d ev=%d → PTT stop(PC 在线,不发内置宏)", input.btn,
                     input.ev);
            break;
        case VM_KEY_ACTION_RUN_SLOT:
            ESP_LOGI(TAG, "btn=%d ev=%d → slot %d (%s)", input.btn, input.ev, slot,
                     vm_profile_active()->name);
            run_slot(slot);
            break;
        case VM_KEY_ACTION_NONE:
        default:
            if (slot >= 0 && vm_keymap_requires_pc(slot)) {
                // 待机门控拦下的 OK:按下时 PC 可能还活跃,这里必须照样收尾停流,
                // 不能依赖"notify 失败会自己停"这种副作用。
                ESP_LOGI(TAG, "PC 会话未活跃,忽略 OK (ev=%d)", input.ev);
                s_ptt_active = false;
                vm_audio_stop();
            }
            // UP/DOWN 单击/双击且 PC 在线:事件已上报,设备不直发(无日志,避免刷屏)。
            break;
        }
    }
}

void vm_buttons_init(void)
{
    if (s_input_task) return;

    s_input_queue = xQueueCreate(INPUT_QUEUE_DEPTH, sizeof(input_event_t));
    if (!s_input_queue) {
        ESP_LOGE(TAG, "输入队列创建失败");
        return;
    }
    if (xTaskCreate(input_task, "vm_input", BUTTONS_TASK_STACK, NULL, 5,
                    &s_input_task) != pdPASS) {
        ESP_LOGE(TAG, "输入任务创建失败");
        vQueueDelete(s_input_queue);
        s_input_queue = NULL;
        return;
    }
    s_input_ready = true;
}
