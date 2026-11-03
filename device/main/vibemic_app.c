// main/vibemic_app.c —— VibeMic 应用主入口:初始化顺序、主屏 UI(全新设计)、
// 状态轮询任务。
//
// UI 为衍生应用重设计(不复用 demo 菜单/ui_pixel 外壳,仅用普通 LVGL 控件):
// 单主屏 = 顶栏(蓝牙状态 | 电量,右上角)+ 中央大字 agent 名 + PTT 提示。
// 状态更新一律由 ui_task 轮询 volatile 状态后在 LVGL 锁内刷新 —— 任何任务
// 不直接跨上下文触碰 LVGL 对象。
//
// 文案全部英文:默认 Montserrat 字体无中文字形(上游字体检查单)。
#include <inttypes.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#include "bsp_battery.h"
#include "bsp_button.h"
#include "bsp_display.h"
#include "bsp_i2c.h"
#include "esp_err.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lvgl.h"
#include "nvs_flash.h"

#include "vm_audio.h"
#include "vm_backlight.h"
#include "vm_batt_icon.h"
#include "vm_buttons.h"
#include "vm_hid.h"
#include "vm_keymap.h"
#include "vm_link.h"
#include "vm_profile.h"

static const char *TAG = "vibemic";

#define UI_POLL_MS 250
#define UI_TASK_STACK 3072

// 删除提示的保留时长。必须明显长于轮询周期:一次快速双击的手势只有几十毫秒,
// 若不留住,它可能整个落在两次轮询之间,屏幕上什么都不会出现。800ms ≈ 3 个周期。
#define DELETE_HINT_HOLD_US (800 * 1000)

// 硬重置完成提示的保留时长:这句话要留够时间读完 —— 用户接下来得去 PC 上删除配对
// 再重新配对,不看这句就只会看到"设备连不上了"。5s = 20 个轮询周期。
#define RESET_HINT_HOLD_US (5 * 1000 * 1000)

// ---- 主屏控件 ----

static lv_obj_t *s_scr;
static lv_obj_t *s_bt_label;    // 顶栏左:蓝牙状态
static lv_obj_t *s_batt_label;  // 顶栏右:电量(读数不可用时隐藏)
static lv_obj_t *s_agent_label; // 中央大字:当前 agent
static lv_obj_t *s_hint_label;  // 中央下方:PTT 提示 / 录音计时 / DELETE

// ---- ui_task 采集的上一帧状态 ----

static bool s_last_connected;
static bool s_last_pc_active;
static bool s_last_ptt;
static bool s_last_deleting;
/// 删除提示的保留截止时刻(us);0 = 没有待显示的提示。
/// 快速双击的手势只有几十毫秒,不留住这一行就什么都看不见。
static int64_t s_delete_until_us;
static int64_t s_ptt_started_us;
/// 硬重置完成提示的保留截止时刻(us);0 = 没有待显示的提示。
static int64_t s_reset_hint_until_us;
/// 已经起算过保留期的那一次重置(vm_buttons 的时刻是"本次上电第几次",不是倒计时)。
static int64_t s_last_reset_done_us;
static uint32_t s_last_reset_hold_ms;
static bool s_last_reset_done;
static int s_last_soc = -2; // -2 = 从未读到
static int s_last_notify_drops;

// ---- 空闲背光(策略与门限见 vm_backlight.h)----

/// 最近一次"用户还在动"的时刻(ms)。按键回调(共享 esp_timer 任务)里写,
/// ui_task 里读:u32 毫秒、只做无符号相减,不需要锁。
static volatile uint32_t s_last_activity_ms;
/// 当前已经写进 LEDC 的档位:只有真的翻档才碰硬件、才打日志。
static vm_bl_state_t s_bl_state = VM_BL_ON;

// 按键回调:先记一次活动,再一字不改地交给 vm_buttons —— 从息屏里按下的这一下
// 既要唤醒屏幕,也要照常执行它本来的动作(唤醒不吞按键)。
static void on_button(bsp_btn_t btn, bsp_btn_ev_t ev, void *user)
{
    s_last_activity_ms = (uint32_t)(esp_timer_get_time() / 1000);
    vm_buttons_on_key(btn, ev, user);
}

static void ui_build(void)
{
    s_scr = lv_obj_create(NULL);
    lv_obj_set_style_bg_color(s_scr, lv_color_hex(0x10141A), 0);

    s_bt_label = lv_label_create(s_scr);
    lv_obj_set_style_text_font(s_bt_label, &lv_font_montserrat_14, 0);
    lv_obj_set_style_text_color(s_bt_label, lv_color_hex(0x8FA3BF), 0);
    lv_obj_align(s_bt_label, LV_ALIGN_TOP_LEFT, 12, 10);
    lv_label_set_text(s_bt_label, "BT ...");

    // 电量固定右上角(上游 UI 规则);读数不可用时隐藏而非画数字。
    // 前缀一个电池图标(见 ui_refresh),颜色与文字同色。
    s_batt_label = lv_label_create(s_scr);
    lv_obj_set_style_text_font(s_batt_label, &lv_font_montserrat_14, 0);
    lv_obj_set_style_text_color(s_batt_label,
                                lv_color_hex(VM_BATT_COLOR_HEX_NORMAL), 0);
    lv_obj_align(s_batt_label, LV_ALIGN_TOP_RIGHT, -12, 10);
    lv_label_set_text(s_batt_label, "");

    s_agent_label = lv_label_create(s_scr);
    lv_obj_set_style_text_font(s_agent_label, &lv_font_montserrat_20, 0);
    lv_obj_set_style_text_color(s_agent_label, lv_color_hex(0xF2F5F9), 0);
    lv_obj_align(s_agent_label, LV_ALIGN_CENTER, 0, -24);
    lv_label_set_text(s_agent_label, "VibeMic");

    s_hint_label = lv_label_create(s_scr);
    lv_obj_set_style_text_font(s_hint_label, &lv_font_montserrat_14, 0);
    lv_obj_set_style_text_color(s_hint_label, lv_color_hex(0x5D7290), 0);
    lv_obj_align(s_hint_label, LV_ALIGN_CENTER, 0, 12);
    lv_label_set_text(s_hint_label, "Hold OK to talk");

    lv_obj_t *keys = lv_label_create(s_scr);
    lv_obj_set_style_text_font(keys, &lv_font_montserrat_14, 0);
    lv_obj_set_style_text_color(keys, lv_color_hex(0x44526A), 0);
    lv_obj_align(keys, LV_ALIGN_BOTTOM_MID, 0, -12);
    lv_label_set_text(keys, "UP send   DOWN cancel");

    lv_screen_load(s_scr);
}

// 状态显示:未连接 / 已连接但 PC 会话未活跃(待机)/ 录音中 / 删除手势中 /
// 蓝牙硬重置(按住倒计时、完成提示)。
// 界面不呈现 agent/profile 概念,主行固定为
// 产品名,状态行只表达 蓝牙/PC 会话/录音/删除/重置 五态;OK 键由 vm_buttons 按
// PC 会话门控 —— 语音不允许盲发到未知焦点窗口。
static void ui_refresh(bool connected, bool pc_active, bool ptt, bool deleting,
                       uint32_t reset_hold_ms, bool reset_done, int64_t now_us,
                       int soc)
{
    lv_label_set_text(s_bt_label, connected ? "BT linked" : "BT pairing...");

    lv_obj_set_style_text_color(s_agent_label,
                                pc_active ? lv_color_hex(0xF2F5F9)
                                          : lv_color_hex(0x44526A), 0);
    lv_label_set_text(s_agent_label, "VibeMic");

    lv_obj_set_style_text_color(s_hint_label, lv_color_hex(0x5D7290), 0);
    if (reset_done) {
        // 设备已经忘记主机,而 PC 还留着旧密钥:不说这一步,用户只会看到"连不上了"。
        lv_obj_set_style_text_color(s_hint_label, lv_color_hex(0xE5A54D), 0);
        lv_label_set_text(s_hint_label, "BT reset.\nDelete pairing on PC");
    } else if (reset_hold_ms > 0) {
        // 倒计时优先于其它状态:这几秒里链路正被拆掉,别的状态都会被它改写。
        const uint32_t left_s =
            (VM_KEY_RESET_HOLD_MS - reset_hold_ms + 999) / 1000;
        lv_obj_set_style_text_color(s_hint_label, lv_color_hex(0xE5A54D), 0);
        lv_label_set_text_fmt(s_hint_label, "BT reset in %us", (unsigned)left_s);
    } else if (pc_active && deleting) {
        // 删除手势(OK 双击的第二下):这一下没有录音,再把 REC 画出来就是骗人。
        lv_obj_set_style_text_color(s_hint_label, lv_color_hex(0xE5A54D), 0);
        lv_label_set_text(s_hint_label, LV_SYMBOL_TRASH " DELETE");
    } else if (pc_active && ptt) {
        const uint32_t secs = (uint32_t)((now_us - s_ptt_started_us) / 1000000);
        lv_obj_set_style_text_color(s_hint_label, lv_color_hex(0xE5484D), 0);
        lv_label_set_text_fmt(s_hint_label, LV_SYMBOL_BULLET " REC %" PRIu32 "s", secs);
    } else if (pc_active) {
        lv_label_set_text(s_hint_label, "Hold OK to talk");
    } else if (connected) {
        lv_label_set_text(s_hint_label, "PC app offline");
    } else {
        lv_label_set_text(s_hint_label, "Not linked. Pair on PC.");
    }

    if (soc < 0) {
        lv_label_set_text(s_batt_label, ""); // 读数不可用:优雅降级
    } else {
        // 电池图标按 1/4 电量分档 + 精确百分比;低电量整块变红。
        // 图标字节由 vm_batt_icon 提供(host tests 锁过),与 LV_SYMBOL_BATTERY_* 相同。
        lv_label_set_text_fmt(s_batt_label, "%s %d%%",
                              vm_batt_icon_symbol(soc), soc);
        lv_obj_set_style_text_color(s_batt_label,
                                    lv_color_hex(vm_batt_icon_color_hex(soc)), 0);
    }
}

// 状态轮询任务:所有跨任务状态在此汇聚,LVGL 更新集中持锁。
static void ui_task(void *arg)
{
    (void)arg;
    uint32_t tick = 0;
    for (;;) {
        vTaskDelay(pdMS_TO_TICKS(UI_POLL_MS));
        tick++;

        int soc = s_last_soc;
        if (tick % (10000 / UI_POLL_MS) == 1) { // 启动后 ~250ms 首读
            soc = bsp_battery_soc();
            vm_hid_battery_update(soc < 0 ? 0xFF : (uint8_t)soc, 0xFF);
            // 通知丢弃(mbuf 分配失败)是"按键没反应"的常见原因之一,别让它静默。
            const int drops = vm_hid_notify_drop_count();
            if (drops != s_last_notify_drops) {
                ESP_LOGW(TAG, "通知丢弃累计 %d 次(堆不足)", drops);
                s_last_notify_drops = drops;
            }
        }

        const bool connected = vm_hid_connected();
        const bool pc_active = vm_link_pc_active();
        const bool ptt = vm_buttons_ptt_active() && pc_active;
        const int64_t now_us = esp_timer_get_time();

        // 删除提示要多留一会儿:快速双击时手势本身只有几十毫秒,不留住就等于没显示。
        const bool deleting_now = vm_buttons_delete_active() && pc_active;
        if (deleting_now) {
            s_delete_until_us = now_us + DELETE_HINT_HOLD_US;
        } else if (s_delete_until_us != 0 && now_us >= s_delete_until_us) {
            s_delete_until_us = 0; // 归零这一次会让下面的 deleting 翻回 false,触发收尾重绘
        }
        const bool deleting = deleting_now || (pc_active && s_delete_until_us > now_us);

        // 硬重置:按住期间画倒计时,完成后把提示留 5 秒(用户得去 PC 上删配对,
        // 不看这句就只会看到"设备连不上了")。
        const uint32_t reset_hold_ms = vm_buttons_reset_hold_ms();
        const int64_t reset_done_us = vm_buttons_reset_done_us();
        if (reset_done_us != 0 && reset_done_us != s_last_reset_done_us) {
            s_last_reset_done_us = reset_done_us; // 又完成了一次 → 重新起算保留期
            s_reset_hint_until_us = now_us + RESET_HINT_HOLD_US;
        } else if (s_reset_hint_until_us != 0 && now_us >= s_reset_hint_until_us) {
            s_reset_hint_until_us = 0; // 归零这一次会让重置提示收起,触发收尾重绘
        }
        const bool reset_done = s_reset_hint_until_us > now_us;

        // 空闲背光:录音 / 删除手势 / 按住 DOWN 重置都是"用户正拿着设备",
        // 期间按持续活动处理(不调暗);其余情况按空闲时长翻档(60s 调暗、180s 熄屏)。
        // 这里只写 LEDC 背光、不碰 LVGL,所以放在锁外。
        const bool bl_busy = ptt || deleting_now || reset_hold_ms > 0;
        const uint32_t now_ms = (uint32_t)(now_us / 1000);
        if (bl_busy) s_last_activity_ms = now_ms;
        const uint32_t idle_ms = now_ms - s_last_activity_ms; // 回绕安全(无符号相减)
        const vm_bl_state_t bl = vm_bl_state_for_idle(idle_ms, bl_busy);
        if (bl != s_bl_state) {
            bsp_display_backlight(vm_bl_percent(bl));
            // 只在真的翻档时打一条:实机验证"1 分钟变暗、3 分钟息屏"就是数这些日志。
            ESP_LOGI(TAG, "空闲 %us → 背光 %s(%u%%)", (unsigned)(idle_ms / 1000u),
                     vm_bl_state_name(bl), (unsigned)vm_bl_percent(bl));
            s_bl_state = bl;
        }

        if (ptt && !s_last_ptt) s_ptt_started_us = now_us;

        if (connected != s_last_connected || pc_active != s_last_pc_active ||
            ptt != s_last_ptt || deleting != s_last_deleting ||
            reset_hold_ms != s_last_reset_hold_ms || reset_done != s_last_reset_done ||
            ptt || reset_hold_ms > 0 || s_delete_until_us != 0 ||
            s_reset_hint_until_us != 0 || soc != s_last_soc) {
            // 只有真的画上去了才更新"已显示状态"缓存:加锁失败时若照样更新,
            // 这次变化就再也不会被重绘(界面永久停在上一个状态)。
            if (bsp_lvgl_lock(200)) {
                ui_refresh(connected, pc_active, ptt, deleting, reset_hold_ms,
                           reset_done, now_us, soc);
                bsp_lvgl_unlock();
                s_last_connected = connected;
                s_last_pc_active = pc_active;
                s_last_ptt = ptt;
                s_last_deleting = deleting;
                s_last_reset_hold_ms = reset_hold_ms;
                s_last_reset_done = reset_done;
                s_last_soc = soc;
            }
        }
    }
}

static esp_err_t nvs_prepare(void)
{
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        // 产品固件拥有本机 NVS:分区结构变化/写满时重建,保留其余分区。
        ESP_LOGW(TAG, "NVS 需重建: %s", esp_err_to_name(err));
        nvs_flash_erase();
        err = nvs_flash_init();
    }
    return err;
}

void app_main(void)
{
    ESP_LOGI(TAG, "VibeMic 启动(feature/vibemic)");

    bsp_i2c_init();

    // 屏幕是唯一 UI 载体:失败则无可降级,打日志退出(同上游基线策略)。
    if (bsp_display_init() != ESP_OK || !bsp_lvgl_init()) {
        ESP_LOGE(TAG, "显示初始化失败,VibeMic 无法继续");
        return;
    }
    // 空闲背光的计时从这一刻起算:屏幕刚点亮,不能一上来就算作"已经空闲"。
    s_last_activity_ms = (uint32_t)(esp_timer_get_time() / 1000);
    bsp_display_backlight(VM_BL_ON_PERCENT);

    esp_err_t nvs_err = nvs_prepare();
    if (nvs_err != ESP_OK) {
        // bond 持久化与 profile 激活态都依赖 NVS;失败继续跑(HID 当次会话可用)。
        ESP_LOGE(TAG, "NVS 初始化失败: %s", esp_err_to_name(nvs_err));
    }

    const bool battery_ok = bsp_battery_init() == ESP_OK;
    if (!battery_ok) ESP_LOGW(TAG, "电量计不可用,UI 隐藏电量");

    vm_profile_init();
    vm_buttons_init();
    ESP_ERROR_CHECK(vm_audio_init()); // 语音串流任务(OK 按住期间采集)
    // vm_link_set_profile_listener / vm_hid_set_conn_listener:设备菜单与
    // 桌面联动的接入点;当前由 ui_task 轮询状态,不注册回调。

    if (vm_hid_start() != ESP_OK) {
        ESP_LOGE(TAG, "BLE 启动失败:检查 NimBLE 配置");
        return;
    }
    if (bsp_button_init(on_button, NULL) != ESP_OK) {
        ESP_LOGE(TAG, "按键初始化失败(设备只显示状态,不发键)");
    }

    if (bsp_lvgl_lock(1000)) {
        ui_build();
        bsp_lvgl_unlock();
    }

    TaskHandle_t ui;
    if (xTaskCreate(ui_task, "vm_ui", UI_TASK_STACK, NULL, 4, &ui) != pdPASS) {
        ESP_LOGE(TAG, "UI 任务创建失败(屏幕停止刷新,其余功能不受影响)");
    }
    ESP_LOGI(TAG, "就绪:内置 profile %u 个,激活 \"%s\"",
             (unsigned)vm_profile_count(), vm_profile_active()->name);
}
