// main/vm_hid.c —— NimBLE HID 键盘外设实现。
//
// HID over GATT(HOGP)不用 NimBLE 内置 services/hids(其 API 未随 ESP-IDF 文档化),
// 按 spec 手工建表 —— 与 NimBLE-Arduino NimBLEHIDDevice 同路线:HID 服务(0x1812)
// + Report Map/Report/Boot/ProtocolMode/Info/ControlPoint,报文经
// ble_gatts_notify_custom 发送。相邻报文最小间隔 10ms(防丢键)。
//
// 线程模型:GAP/服务回调运行于 NimBLE host 任务,只更新状态/转发事件;宏步骤由
// 独立 vm_action 任务串行执行,按键回调只投队列(上游规则:回调不做慢操作)。
#include "vm_hid.h"

#include <string.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

#include "host/ble_att.h"
#include "host/ble_gap.h"
#include "host/ble_gatt.h"
#include "host/ble_hs.h"
#include "host/ble_hs_mbuf.h"
#include "host/ble_store.h"
#include "host/util/util.h"
#include "nimble/nimble_port.h"
#include "os/os_mbuf.h"
#include "services/gap/ble_svc_gap.h"
#include "services/gatt/ble_svc_gatt.h"

#include "vm_uuid.h"

// vm_link 提供:VibeMic 特征值访问回调 / GAP 事件转发 / 注册后句柄解析。
int vm_link_chr_access(uint16_t conn_handle, uint16_t attr_handle,
                       struct ble_gatt_access_ctxt *ctxt, void *arg);
void vm_link_gap_event(struct ble_gap_event *event);
void vm_link_services_attached(void);

static const char *TAG = "vm_hid";

// 定义在下面的 GAP 一节;vm_hid_bond_reset() 在"没连接"时也要用它确认还在广播。
static int advertise(void);

#define REPORT_MIN_GAP_MS 10
#define ACTION_QUEUE_DEPTH 8
#define ACTION_TASK_STACK 3072

// ---- UUID 表(具名常量,避免在静态初始化里取复合字面量地址) ----

// VibeMic 服务的 128 位 UUID(宏与 id 常量见 vm_uuid.h,与 vm_link 共用一份)。
// 广播携带该 UUID(macOS retrieveConnectedPeripherals 依赖);adv 字段结构持有
// 非 const 指针,故此对象不能是 const。
static ble_uuid128_t VM_SVC_UUID = VM_UUID128_INIT(VM_UUID_SVC);

static const ble_uuid16_t UUID_SVC_HID = BLE_UUID16_INIT(0x1812);
static const ble_uuid16_t UUID_SVC_BATT = BLE_UUID16_INIT(0x180F);
static const ble_uuid16_t UUID_SVC_DEVINFO = BLE_UUID16_INIT(0x180A);
static const ble_uuid16_t UUID_DSC_REPORT_REF = BLE_UUID16_INIT(0x2908);

static const ble_uuid16_t UUID_CHR_HID_INFO = BLE_UUID16_INIT(0x2A4A);
static const ble_uuid16_t UUID_CHR_REPORT_MAP = BLE_UUID16_INIT(0x2A4B);
static const ble_uuid16_t UUID_CHR_CTRL_POINT = BLE_UUID16_INIT(0x2A4C);
static const ble_uuid16_t UUID_CHR_PROTOCOL_MODE = BLE_UUID16_INIT(0x2A4E);
static const ble_uuid16_t UUID_CHR_REPORT = BLE_UUID16_INIT(0x2A4D);
static const ble_uuid16_t UUID_CHR_BOOT_INPUT = BLE_UUID16_INIT(0x2A22);
static const ble_uuid16_t UUID_CHR_BOOT_OUTPUT = BLE_UUID16_INIT(0x2A23);
static const ble_uuid16_t UUID_CHR_BATT_LEVEL = BLE_UUID16_INIT(0x2A19);
static const ble_uuid16_t UUID_CHR_DI_MANUFACTURER = BLE_UUID16_INIT(0x2A29);
static const ble_uuid16_t UUID_CHR_DI_MODEL = BLE_UUID16_INIT(0x2A24);
static const ble_uuid16_t UUID_CHR_DI_SERIAL = BLE_UUID16_INIT(0x2A25);
static const ble_uuid16_t UUID_CHR_DI_PNP = BLE_UUID16_INIT(0x2A50);

// VibeMic 特征值(逻辑在 vm_link)。
static const ble_uuid128_t UUID_CHR_VM_CTRL = VM_UUID128_INIT(VM_UUID_CHR_CTRL);
static const ble_uuid128_t UUID_CHR_VM_PROFILE = VM_UUID128_INIT(VM_UUID_CHR_PROFILE);
static const ble_uuid128_t UUID_CHR_VM_AUDIO = VM_UUID128_INIT(VM_UUID_CHR_AUDIO);
static const ble_uuid128_t UUID_CHR_VM_STATUS = VM_UUID128_INIT(VM_UUID_CHR_STATUS);
static const ble_uuid128_t UUID_CHR_VM_EVENT = VM_UUID128_INIT(VM_UUID_CHR_EVENT);

// 标准键盘 Report Map(45B:修饰键 1B + 保留 1B + 6 键位)。
static const uint8_t KEYBOARD_REPORT_MAP[] = {
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0xE0, //   Usage Minimum (224)
    0x29, 0xE7, //   Usage Maximum (231)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute) —— 修饰键字节
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Constant) —— 保留字节
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x81, 0x00, //   Input (Data, Array) —— 键位数组
    0xC0,       // End Collection
};

// HID Information:bcdHID 1.01 / 国家码 0 / flags=0x02(normally connectable)。
static const uint8_t HID_INFO[] = { 0x01, 0x01, 0x00, 0x02 };
// PnP ID:USB-IF 来源 / VID 0x1209(pid.codes) / PID 0x8101 / 版本 0.0100。
static const uint8_t PNP_ID[] = { 0x02, 0x09, 0x12, 0x01, 0x81, 0x00, 0x01 };

// 特征值参数 id(access 回调 arg)。
enum {
    CHR_ID_HID_INFO = 1,
    CHR_ID_REPORT_MAP,
    CHR_ID_CTRL_POINT,
    CHR_ID_PROTOCOL_MODE,
    CHR_ID_REPORT,
    CHR_ID_BOOT_INPUT,
    CHR_ID_BOOT_OUTPUT,
    CHR_ID_BATT_LEVEL,
    CHR_ID_DI_MANUFACTURER,
    CHR_ID_DI_MODEL,
    CHR_ID_DI_SERIAL,
    CHR_ID_DI_PNP,
};

// ---- 连接/状态 ----

static SemaphoreHandle_t s_host_stopped;
static TaskHandle_t s_host_task;
static bool s_initialized;
static volatile bool s_start_requested;

static volatile uint16_t s_conn_handle = VM_HID_CONN_NONE;
static volatile bool s_report_notify; // Report 特征值 CCCD notify
static volatile bool s_boot_notify;   // Boot 键盘输入 CCCD notify
static volatile bool s_batt_notify;
static volatile uint8_t s_protocol_mode = 1; // 1=report 协议
static uint8_t s_battery_pct = 0xFF;        // 未知
static uint8_t s_battery_charging = 0xFF;

static vm_hid_conn_cb_t s_conn_cb;
static void *s_conn_cb_user;

// GATT 表内特征值 value handle(add_svcs 注册时由栈写入)。
static uint16_t h_report;
static uint16_t h_boot_input;
static uint16_t h_batt_level;

// ---- 宏执行器 ----

typedef struct {
    const vm_step_t *steps;
    uint8_t count;
} vm_action_msg_t;

static QueueHandle_t s_action_queue;
static TaskHandle_t s_action_task;

void vm_hid_run_steps(const vm_step_t *steps, uint8_t count)
{
    if (!s_action_queue || !steps || !count) return;
    const vm_action_msg_t msg = { .steps = steps, .count = count };
    if (xQueueSend(s_action_queue, &msg, 0) != pdTRUE) {
        ESP_LOGW(TAG, "宏队列满,丢弃 %u 步", count);
    }
}

// 平面数据通知:本版 NimBLE 的 *_notify_custom 只收 os_mbuf,统一经
// ble_hs_mbuf_from_flat 包装;分配失败(堆尽)时静默丢弃并计数。
static int s_notify_drop;

static void notify_flat(uint16_t conn, uint16_t val_handle, const void *data,
                        uint16_t len)
{
    struct os_mbuf *om = ble_hs_mbuf_from_flat(data, len);
    if (!om) {
        // 计数并周期性可见:静默丢弃会让"按键没反应"变成无解之谜。
        if (++s_notify_drop == 1 || s_notify_drop % 64 == 0) {
            ESP_LOGW(TAG, "notify 丢弃 %d 次(mbuf 分配失败,堆不足)", s_notify_drop);
        }
        return;
    }
    (void)ble_gatts_notify_custom(conn, val_handle, om);
}

int vm_hid_notify_drop_count(void)
{
    return s_notify_drop;
}

// 发一帧键盘报文(8B:修饰键+保留+单键位)。优先当前协议模式对应的特征值,
// 未订阅则尝试另一个;都不通(未连接/未订阅)则静默丢弃。调用方保证节流。
static void send_key_report(uint8_t mods, uint8_t keycode)
{
    if (s_conn_handle == VM_HID_CONN_NONE) return;
    const uint8_t report[8] = { mods, 0, keycode, 0, 0, 0, 0, 0 };

    const bool report_mode = s_protocol_mode != 0;
    const uint16_t first = report_mode && h_report && s_report_notify
                               ? h_report
                               : (h_boot_input && s_boot_notify ? h_boot_input : 0);
    if (first) {
        notify_flat(s_conn_handle, first, report, sizeof(report));
        return;
    }
    const uint16_t fallback = report_mode ? h_boot_input : h_report;
    const bool fallback_on = report_mode ? s_boot_notify : s_report_notify;
    if (fallback && fallback_on) {
        notify_flat(s_conn_handle, fallback, report, sizeof(report));
    }
}

static void action_task(void *arg)
{
    (void)arg;
    vm_action_msg_t msg;
    for (;;) {
        xQueueReceive(s_action_queue, &msg, portMAX_DELAY);
        for (uint8_t i = 0; i < msg.count; i++) {
            const vm_step_t *s = &msg.steps[i];
            switch (s->type) {
            case VM_STEP_TAP:
                send_key_report(s->mods, s->keycode);
                vTaskDelay(pdMS_TO_TICKS(REPORT_MIN_GAP_MS));
                send_key_report(0, 0);
                vTaskDelay(pdMS_TO_TICKS(REPORT_MIN_GAP_MS));
                break;
            case VM_STEP_DOWN:
                send_key_report(s->mods, s->keycode);
                break;
            case VM_STEP_UP:
                send_key_report(0, 0);
                break;
            case VM_STEP_DELAY:
                if (s->delay_ms) vTaskDelay(pdMS_TO_TICKS(s->delay_ms));
                break;
            }
        }
    }
}

void vm_hid_battery_update(uint8_t pct, uint8_t charging)
{
    s_battery_pct = pct;
    s_battery_charging = charging;
    if (s_conn_handle != VM_HID_CONN_NONE && h_batt_level && s_batt_notify) {
        notify_flat(s_conn_handle, h_batt_level, &pct, 1);
    }
}

void vm_hid_get_battery(uint8_t *pct, uint8_t *charging)
{
    if (pct) *pct = s_battery_pct;
    if (charging) *charging = s_battery_charging;
}

bool vm_hid_connected(void)
{
    return s_conn_handle != VM_HID_CONN_NONE;
}

uint16_t vm_hid_conn_handle(void)
{
    return s_conn_handle;
}

esp_err_t vm_hid_disconnect(void)
{
    if (s_conn_handle == VM_HID_CONN_NONE) return ESP_ERR_INVALID_STATE;
    return ble_gap_terminate(s_conn_handle, BLE_ERR_REM_USER_CONN_TERM) == 0
               ? ESP_OK
               : ESP_FAIL;
}

esp_err_t vm_hid_bond_reset(void)
{
    if (!s_initialized) return ESP_ERR_INVALID_STATE;

    // 先清记录再断链:断链回调只负责重新广播,不清记录(唯一的例外是 REPEAT_PAIRING
    // 那条路径,它针对的是"重复配对"这一个连接)。
    const int src = ble_store_clear();
    if (src != 0) {
        ESP_LOGE(TAG, "清配对记录失败: %d", src);
        return ESP_FAIL;
    }
    ESP_LOGW(TAG, "配对记录已清空(设备已忘记所有主机)");

    if (s_conn_handle != VM_HID_CONN_NONE) {
        // 断开后走 BLE_GAP_EVENT_DISCONNECT → advertise(),与普通掉线同一条路径。
        if (ble_gap_terminate(s_conn_handle, BLE_ERR_REM_USER_CONN_TERM) != 0) {
            // 记录已经清了,这次的"重置"仍然生效,只是链路还挂着。
            ESP_LOGE(TAG, "重置后断开失败(配对已清空,链路仍在)");
            return ESP_FAIL;
        }
        return ESP_OK;
    }

    // 没连接时自己确认还在广播:恢复路径不能因为"停播"而卡住。
    // (advertise() 平时只由 host 任务在断链/同步后调用;这里多了非 host 任务的调用方,
    //  但写入的 adv 字段与连接参数每次相同,真与 host 任务撞上也只是其中一次 EALREADY。)
    if (!ble_gap_adv_active() && advertise() != 0) {
        ESP_LOGE(TAG, "重置后重新广播失败");
        return ESP_FAIL;
    }
    return ESP_OK;
}

// ---- GATT 访问回调(HID/电池/设备信息;VibeMic 特征值在 vm_link) ----

static int hid_chr_access(uint16_t conn_handle, uint16_t attr_handle,
                          struct ble_gatt_access_ctxt *ctxt, void *arg)
{
    (void)conn_handle;
    (void)attr_handle;
    const int id = (int)(intptr_t)arg;
    switch (id) {
    case CHR_ID_HID_INFO:
        return os_mbuf_append(ctxt->om, HID_INFO, sizeof(HID_INFO));
    case CHR_ID_REPORT_MAP:
        return os_mbuf_append(ctxt->om, KEYBOARD_REPORT_MAP,
                              sizeof(KEYBOARD_REPORT_MAP));
    case CHR_ID_PROTOCOL_MODE:
        if (ctxt->op == BLE_GATT_ACCESS_OP_READ_CHR) {
            uint8_t mode = s_protocol_mode;
            return os_mbuf_append(ctxt->om, &mode, 1);
        }
        if (ctxt->op == BLE_GATT_ACCESS_OP_WRITE_CHR) {
            uint8_t mode = 0;
            if (OS_MBUF_PKTLEN(ctxt->om) != 1 ||
                ble_hs_mbuf_to_flat(ctxt->om, &mode, 1, NULL) != 0 || mode > 1) {
                return BLE_ATT_ERR_UNLIKELY;
            }
            s_protocol_mode = mode;
            return 0;
        }
        return BLE_ATT_ERR_UNLIKELY;
    case CHR_ID_CTRL_POINT:
        // 0=Suspend 1=Exit Suspend;仅记录,不改变行为。
        ESP_LOGI(TAG, "HID control point 写入(忽略)");
        return 0;
    case CHR_ID_REPORT:
    case CHR_ID_BOOT_INPUT:
        if (ctxt->op == BLE_GATT_ACCESS_OP_READ_CHR) {
            const uint8_t idle[8] = { 0 };
            return os_mbuf_append(ctxt->om, idle, sizeof(idle));
        }
        return 0; // 输入报文主机不可写
    case CHR_ID_BOOT_OUTPUT:
        return 0; // LED 输出报文,不驱动
    case CHR_ID_BATT_LEVEL: {
        uint8_t pct = s_battery_pct == 0xFF ? 0 : s_battery_pct;
        return os_mbuf_append(ctxt->om, &pct, 1);
    }
    case CHR_ID_DI_MANUFACTURER:
        return os_mbuf_append(ctxt->om, "FoloToy", sizeof("FoloToy") - 1);
    case CHR_ID_DI_MODEL:
        // 与广播名保持一致(VM_DEVICE_NAME),别再手写第二份字面量。
        return os_mbuf_append(ctxt->om, VM_DEVICE_NAME, sizeof(VM_DEVICE_NAME) - 1);
    case CHR_ID_DI_SERIAL:
        return os_mbuf_append(ctxt->om, "VIBEMIC0001", sizeof("VIBEMIC0001") - 1);
    case CHR_ID_DI_PNP:
        return os_mbuf_append(ctxt->om, PNP_ID, sizeof(PNP_ID));
    default:
        return BLE_ATT_ERR_UNLIKELY;
    }
}

static int report_ref_dsc_access(uint16_t conn_handle, uint16_t attr_handle,
                                 struct ble_gatt_access_ctxt *ctxt, void *arg)
{
    (void)conn_handle;
    (void)attr_handle;
    (void)arg;
    if (ctxt->op != BLE_GATT_ACCESS_OP_READ_DSC) return BLE_ATT_ERR_UNLIKELY;
    // Report Map 里没有 Report ID 项(0x85),按 HOGP 规范 Report Reference 的
    // report id 字段本应为 0,这里给的是 1。不改的原因:改 HID 描述符会让已配对
    // 主机上的缓存失效、必须重新配对后才能验证,而 Windows/macOS 都按
    // "无 report id"消费这份描述符。若将来真给报文加 report id 前缀,同步改这里。
    static const uint8_t ref[2] = { 1, 1 }; // report id 1 / 类型 Input
    return os_mbuf_append(ctxt->om, ref, sizeof(ref));
}

// ---- GATT 表(单表注册:count_cfg/add_svcs 只走一次) ----

static const struct ble_gatt_svc_def GATT_SVCS[] = {
    {
        .type = BLE_GATT_SVC_TYPE_PRIMARY,
        .uuid = &UUID_SVC_HID.u,
        .characteristics = (struct ble_gatt_chr_def[]){
            {
                .uuid = &UUID_CHR_HID_INFO.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_HID_INFO,
                .flags = BLE_GATT_CHR_F_READ,
            },
            {
                .uuid = &UUID_CHR_REPORT_MAP.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_REPORT_MAP,
                .flags = BLE_GATT_CHR_F_READ,
            },
            {
                .uuid = &UUID_CHR_CTRL_POINT.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_CTRL_POINT,
                .flags = BLE_GATT_CHR_F_WRITE_NO_RSP,
            },
            {
                .uuid = &UUID_CHR_PROTOCOL_MODE.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_PROTOCOL_MODE,
                .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_WRITE_NO_RSP,
            },
            {
                .uuid = &UUID_CHR_REPORT.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_REPORT,
                .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
                .val_handle = &h_report,
                .descriptors = (struct ble_gatt_dsc_def[]){
                    {
                        .uuid = &UUID_DSC_REPORT_REF.u,
                        .access_cb = report_ref_dsc_access,
                        .att_flags = BLE_ATT_F_READ,
                    },
                    { 0 },
                },
            },
            {
                .uuid = &UUID_CHR_BOOT_INPUT.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_BOOT_INPUT,
                .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
                .val_handle = &h_boot_input,
            },
            {
                .uuid = &UUID_CHR_BOOT_OUTPUT.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_BOOT_OUTPUT,
                .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_WRITE,
            },
            { 0 },
        },
    },
    {
        .type = BLE_GATT_SVC_TYPE_PRIMARY,
        .uuid = &UUID_SVC_BATT.u,
        .characteristics = (struct ble_gatt_chr_def[]){
            {
                .uuid = &UUID_CHR_BATT_LEVEL.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_BATT_LEVEL,
                .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
                .val_handle = &h_batt_level,
            },
            { 0 },
        },
    },
    {
        .type = BLE_GATT_SVC_TYPE_PRIMARY,
        .uuid = &UUID_SVC_DEVINFO.u,
        .characteristics = (struct ble_gatt_chr_def[]){
            {
                .uuid = &UUID_CHR_DI_MANUFACTURER.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_DI_MANUFACTURER,
                .flags = BLE_GATT_CHR_F_READ,
            },
            {
                .uuid = &UUID_CHR_DI_MODEL.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_DI_MODEL,
                .flags = BLE_GATT_CHR_F_READ,
            },
            {
                .uuid = &UUID_CHR_DI_SERIAL.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_DI_SERIAL,
                .flags = BLE_GATT_CHR_F_READ,
            },
            {
                .uuid = &UUID_CHR_DI_PNP.u,
                .access_cb = hid_chr_access,
                .arg = (void *)(intptr_t)CHR_ID_DI_PNP,
                .flags = BLE_GATT_CHR_F_READ,
            },
            { 0 },
        },
    },
    // VibeMic 控制面服务:特征值逻辑在 vm_link,本表只挂注册项。
    // 所有可写入口要求加密;vm_link 另按特征值限制允许的帧类型。
    {
        .type = BLE_GATT_SVC_TYPE_PRIMARY,
        .uuid = &VM_SVC_UUID.u,
        .characteristics = (struct ble_gatt_chr_def[]){
            {
                .uuid = &UUID_CHR_VM_CTRL.u,
                .access_cb = vm_link_chr_access,
                .arg = (void *)(intptr_t)VM_UUID_CHR_CTRL,
                .flags = BLE_GATT_CHR_F_WRITE | BLE_GATT_CHR_F_WRITE_ENC,
            },
            {
                .uuid = &UUID_CHR_VM_PROFILE.u,
                .access_cb = vm_link_chr_access,
                .arg = (void *)(intptr_t)VM_UUID_CHR_PROFILE,
                .flags = BLE_GATT_CHR_F_WRITE_NO_RSP | BLE_GATT_CHR_F_WRITE_ENC,
            },
            {
                .uuid = &UUID_CHR_VM_STATUS.u,
                .access_cb = vm_link_chr_access,
                .arg = (void *)(intptr_t)VM_UUID_CHR_STATUS,
                .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
            },
            {
                .uuid = &UUID_CHR_VM_EVENT.u,
                .access_cb = vm_link_chr_access,
                .arg = (void *)(intptr_t)VM_UUID_CHR_EVENT,
                .flags = BLE_GATT_CHR_F_NOTIFY,
            },
            {
                // 语音串流:块格式见 vm_audio_codec.h(非协议帧,自带头)
                .uuid = &UUID_CHR_VM_AUDIO.u,
                .access_cb = vm_link_chr_access,
                .arg = (void *)(intptr_t)VM_UUID_CHR_AUDIO,
                .flags = BLE_GATT_CHR_F_NOTIFY,
            },
            { 0 },
        },
    },
    { 0 },
};

// ---- GAP ----

static uint8_t s_addr_type;
static int gap_event(struct ble_gap_event *event, void *arg);

static int advertise(void)
{
    struct ble_hs_adv_fields fields = { 0 };
    fields.flags = BLE_HS_ADV_F_DISC_GEN | BLE_HS_ADV_F_BREDR_UNSUP;
    fields.appearance_is_present = 1;
    fields.appearance = 961; // HID Keyboard
    fields.uuids128 = &VM_SVC_UUID;
    fields.num_uuids128 = 1;
    fields.uuids128_is_complete = 1;
    int rc = ble_gap_adv_set_fields(&fields);
    if (rc != 0) return rc;

    struct ble_hs_adv_fields rsp = { 0 };
    rsp.name = (const uint8_t *)VM_DEVICE_NAME;
    rsp.name_len = (uint8_t)strlen(VM_DEVICE_NAME);
    rsp.name_is_complete = 1;
    rc = ble_gap_adv_rsp_set_fields(&rsp);
    if (rc != 0) return rc;

    struct ble_gap_adv_params params = { 0 };
    params.conn_mode = BLE_GAP_CONN_MODE_UND;
    params.disc_mode = BLE_GAP_DISC_MODE_GEN;
    return ble_gap_adv_start(s_addr_type, NULL, BLE_HS_FOREVER, &params, gap_event,
                             NULL);
}

static int gap_event(struct ble_gap_event *event, void *arg)
{
    (void)arg;
    vm_link_gap_event(event);

    switch (event->type) {
    case BLE_GAP_EVENT_CONNECT:
        if (event->connect.status != 0) {
            ESP_LOGW(TAG, "连接失败 status=%d,重新广播", event->connect.status);
            s_conn_handle = VM_HID_CONN_NONE;
            advertise();
            return 0;
        }
        s_conn_handle = event->connect.conn_handle;
        s_report_notify = s_boot_notify = s_batt_notify = false;
        ESP_LOGI(TAG, "已连接 handle=%u", s_conn_handle);
        // MTU 交换由主机发起(central 流程;本机 peripheral-only 未编 gattc)。
        // 我方偏好已由 ble_att_set_preferred_mtu(247) 声明,主机协商后走
        // BLE_GAP_EVENT_MTU → 连接参数更新。
        if (s_conn_cb) s_conn_cb(true, s_conn_cb_user);
        return 0;

    case BLE_GAP_EVENT_DISCONNECT:
        s_conn_handle = VM_HID_CONN_NONE;
        s_report_notify = s_boot_notify = s_batt_notify = false;
        ESP_LOGI(TAG, "已断开 reason=%d,重新广播", event->disconnect.reason);
        if (s_conn_cb) s_conn_cb(false, s_conn_cb_user);
        advertise();
        return 0;

    case BLE_GAP_EVENT_SUBSCRIBE: {
        const uint16_t h = event->subscribe.attr_handle;
        if (h == h_report) s_report_notify = event->subscribe.cur_notify;
        if (h == h_boot_input) s_boot_notify = event->subscribe.cur_notify;
        if (h == h_batt_level) s_batt_notify = event->subscribe.cur_notify;
        return 0;
    }

    case BLE_GAP_EVENT_MTU:
        // MTU 谈妥后申请连接参数:PTT 场景要低延迟(7.5–15ms;
        // Apple 建议的下限是 15ms,故取 15ms)。
        // 取 15ms 还有一个硬理由:音频以 50 块/秒(20ms 一块)产出,链路必须有
        // ≥50 包/s 的容量;30ms 间隔只有约 33 包/s,上行会迅速积压 → NimBLE msys
        // 池被占满 → 通知发不出去 → 录音被截断(录音恒定卡在
        // 约 17 块 / 0.34s)。主机可以拒绝或改写这个请求,设备侧另有丢块退避兜底。
        if (s_conn_handle != VM_HID_CONN_NONE) {
            struct ble_gap_upd_params p = {
                .itvl_min = 12,             // 15ms
                .itvl_max = 12,             // 15ms
                .latency = 0,
                .supervision_timeout = 400, // 4s(> 2 × 15ms,满足规范)
                .min_ce_len = 0,
                .max_ce_len = 0,
            };
            ble_gap_update_params(s_conn_handle, &p);
        }
        return 0;

    case BLE_GAP_EVENT_REPEAT_PAIRING: {
        // 重复配对:删旧 bond 重试(双主机 bond 列表管理尚未实现)。
        struct ble_gap_conn_desc desc;
        if (ble_gap_conn_find(event->repeat_pairing.conn_handle, &desc) == 0) {
            ble_store_util_delete_peer(&desc.peer_id_addr);
        }
        return BLE_GAP_REPEAT_PAIRING_RETRY;
    }

    default:
        return 0;
    }
}

static void on_reset(int reason)
{
    ESP_LOGE(TAG, "NimBLE host 重置: %d", reason);
}

static void on_sync(void)
{
    int rc = ble_hs_util_ensure_addr(0);
    if (rc == 0) rc = ble_hs_id_infer_auto(0, &s_addr_type);
    if (rc == 0 && s_start_requested) rc = advertise();
    if (rc != 0) {
        ESP_LOGE(TAG, "同步/广播启动失败: %d", rc);
        return;
    }
    // GATT 句柄在 host 启动时才分配,add_svcs 后太早;sync 后解析一次。
    vm_link_services_attached();
}

static void host_task(void *arg)
{
    (void)arg;
    nimble_port_run();
    // 不用 nimble_port_freertos 包装:它私存任务句柄且忽略创建失败(同 demo_ble)。
    xSemaphoreGive(s_host_stopped);
    for (;;) vTaskSuspend(NULL);
}

esp_err_t vm_hid_start(void)
{
    if (s_initialized) return ESP_ERR_INVALID_STATE;

    s_start_requested = true;
    esp_err_t err = nimble_port_init();
    if (err != ESP_OK) return err;
    s_initialized = true;

    s_host_stopped = xSemaphoreCreateBinary();
    s_action_queue = xQueueCreate(ACTION_QUEUE_DEPTH, sizeof(vm_action_msg_t));
    if (!s_host_stopped || !s_action_queue) {
        err = ESP_ERR_NO_MEM;
        goto failed;
    }
    if (xTaskCreate(action_task, "vm_action", ACTION_TASK_STACK, NULL, 5,
                    &s_action_task) != pdPASS) {
        err = ESP_ERR_NO_MEM;
        goto failed;
    }

    ble_svc_gap_init();
    ble_svc_gatt_init();
    ble_hs_cfg.reset_cb = on_reset;
    ble_hs_cfg.sync_cb = on_sync;
    // Just-Works bonding:无显示无键盘;LE Secure Connections 开启。
    ble_hs_cfg.sm_bonding = 1;
    ble_hs_cfg.sm_sc = 1;
    ble_hs_cfg.sm_mitm = 0;
    ble_hs_cfg.sm_io_cap = BLE_HS_IO_NO_INPUT_OUTPUT;
    ble_hs_cfg.store_status_cb = ble_store_util_status_rr;

    int rc = ble_svc_gap_device_name_set(VM_DEVICE_NAME);
    if (rc == 0) rc = ble_att_set_preferred_mtu(247);
    if (rc == 0) rc = ble_gatts_count_cfg(GATT_SVCS);
    if (rc == 0) rc = ble_gatts_add_svcs(GATT_SVCS);
    if (rc != 0) {
        ESP_LOGE(TAG, "GATT 注册失败: %d", rc);
        err = ESP_FAIL;
        goto failed;
    }
    if (xTaskCreatePinnedToCore(host_task, "nimble_host", NIMBLE_HS_STACK_SIZE, NULL,
                                configMAX_PRIORITIES - 4, &s_host_task,
                                NIMBLE_CORE) != pdPASS) {
        err = ESP_ERR_NO_MEM;
        goto failed;
    }
    ESP_LOGI(TAG, "HID 键盘服务就绪,广播名 \"%s\"", VM_DEVICE_NAME);
    return ESP_OK;

failed:
    s_start_requested = false;
    if (s_action_task) {
        vTaskDelete(s_action_task);
        s_action_task = NULL;
    }
    if (s_action_queue) {
        vQueueDelete(s_action_queue);
        s_action_queue = NULL;
    }
    if (s_host_stopped) {
        vSemaphoreDelete(s_host_stopped);
        s_host_stopped = NULL;
    }
    // host 任务未创建时 nimble_port_stop 会返回 BLE_HS_EALREADY,直接反初始化。
    nimble_port_deinit();
    s_initialized = false;
    return err;
}

void vm_hid_set_conn_listener(vm_hid_conn_cb_t cb, void *user)
{
    s_conn_cb = cb;
    s_conn_cb_user = user;
}
