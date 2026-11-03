// main/vm_link.c —— VibeMic 协议帧处理:特征值写入 → 帧解码 → 命令队列 →
// 工作任务执行(NVS 写/断连不在 host 任务里做,遵循上游"回调不做慢操作"规则)。
#include "vm_link.h"

#include <string.h>
#include <stdatomic.h>

#include "esp_err.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

#include "host/ble_att.h"
#include "host/ble_gatt.h"
#include "host/ble_hs.h"
#include "host/ble_hs_mbuf.h"
#include "os/os_mbuf.h"

#include "vm_hid.h"
#include "vm_profile.h"
#include "vm_protocol.h"
#include "vm_uuid.h"

static const char *TAG = "vm_link";

#define LINK_QUEUE_DEPTH 8
#define LINK_TASK_STACK 3072

// 音频 notify 失败后的退避区间(ms);实际值取两个连接间隔并夹在这个区间内。
#define VM_LINK_AUDIO_BACKOFF_MIN_MS 20u
#define VM_LINK_AUDIO_BACKOFF_MAX_MS 60u

typedef struct {
    uint8_t type; // VM_FRAME_CMD_* / VM_FRAME_PROFILE_CHUNK
    uint8_t arg;  // SET_ACTIVE 的 profileId
} link_cmd_t;

// 特征值 UUID 与访问回调 arg 都来自 vm_uuid.h(与 vm_hid 的 GATT 表同一份常量;
// 以前两处各写一遍宏与字面量,改一处漏一处会静默错位)。
static const ble_uuid128_t VM_SVC_UUID = VM_UUID128_INIT(VM_UUID_SVC);
static const ble_uuid128_t CHR_VM_CTRL = VM_UUID128_INIT(VM_UUID_CHR_CTRL);
static const ble_uuid128_t CHR_VM_PROFILE = VM_UUID128_INIT(VM_UUID_CHR_PROFILE);
static const ble_uuid128_t CHR_VM_STATUS = VM_UUID128_INIT(VM_UUID_CHR_STATUS);
static const ble_uuid128_t CHR_VM_EVENT = VM_UUID128_INIT(VM_UUID_CHR_EVENT);
static const ble_uuid128_t CHR_VM_AUDIO = VM_UUID128_INIT(VM_UUID_CHR_AUDIO);

static QueueHandle_t s_cmd_queue;
static TaskHandle_t s_cmd_task;

static uint16_t h_ctrl;    // 仅用于日志定位,无操作
static uint16_t h_profile;
static uint16_t h_status;
static uint16_t h_event;
static uint16_t h_audio;
static volatile bool s_status_notify;
static volatile bool s_event_notify;
static volatile bool s_audio_notify;
static atomic_bool s_pc_session;
static atomic_uint s_pc_seen_ms;
#define PC_SESSION_TTL_MS 15000u

// 每个可写特征值一个解码器:帧不得跨特征值交错。
static vm_decoder_t s_dec_ctrl;
static vm_decoder_t s_dec_profile;

static bool s_handles_resolved;

// 当前连接间隔(单位 1.25ms),由 GAP 事件在 NimBLE host 任务里缓存。
// 音频任务需要它算退避量,但 NimBLE 的连接表不是跨任务安全的,所以在事件里查一次存下来。
static volatile uint16_t s_conn_itvl_125us;
static void refresh_conn_itvl(uint16_t conn_handle);

static vm_link_profile_cb_t s_profile_cb;
static void *s_profile_cb_user;

// ---- 应答与通知 ----

static void send_frame_notify(uint16_t val_handle, uint8_t type,
                              const uint8_t *payload, uint16_t len)
{
    // 缓冲必须容得下协议允许的最大帧:8(头+CRC)+ VM_FRAME_MAX。
    // 以前写成 +7,满载荷帧会被 vm_frame_encode 拒绝并静默丢弃。
    uint8_t buf[VM_FRAME_MAX + 8];
    const size_t n = vm_frame_encode(buf, sizeof(buf), VM_PROTO_VERSION, type,
                                     payload, len);
    if (n == 0) return;
    struct os_mbuf *om = ble_hs_mbuf_from_flat(buf, (uint16_t)n);
    if (!om) {
        ESP_LOGW(TAG, "notify 0x%02X: mbuf 分配失败", type);
        return;
    }
    int rc = ble_gatts_notify_custom(vm_hid_conn_handle(), val_handle, om);
    ESP_LOGI(TAG, "notify 0x%02X → handle %u rc=%d", type, val_handle, rc);
}

static void send_result(uint8_t req_type, uint8_t code)
{
    const vm_proto_result_t r = { .req_type = req_type, .code = code };
    uint8_t payload[2];
    const size_t n = vm_payload_encode_result(payload, sizeof(payload), &r);
    send_frame_notify(h_status, VM_FRAME_RESULT, payload, (uint16_t)n);
}

// 组装并推当前设备状态(读回调与 STATUS_NOTIFY 共用)。
static size_t fill_status_payload(uint8_t *buf, size_t cap)
{
    uint8_t pct = 0xFF, charging = 0xFF;
    vm_hid_get_battery(&pct, &charging);
    const vm_proto_status_t st = {
        .fw_ver = VM_LINK_FW_VERSION,
        .battery_pct = pct,
        .charging = charging,
        .conn_state = vm_hid_connected() ? VM_CONN_CONNECTED : VM_CONN_ADVERTISING,
        .active_profile = vm_profile_active_id(),
        // bond 槽:当前只有单 bond,未连接时报 NONE;真正的 A/B 槽位
        // 需要双主机支持,尚未实现。
        .bond_slot = vm_hid_connected() ? VM_BOND_A : VM_BOND_NONE,
        .free_heap = esp_get_free_heap_size(),
    };
    return vm_payload_encode_status(buf, cap, &st);
}

void vm_link_notify_button_event(uint8_t button, uint8_t vm_btn_ev)
{
    if (!vm_link_pc_active() || !h_event) {
        return;
    }
    const vm_proto_button_event_t ev = {
        .button = button,
        .event = vm_btn_ev,
        .ts_ms = (uint32_t)(esp_timer_get_time() / 1000),
    };
    uint8_t payload[6];
    const size_t n = vm_payload_encode_button_event(payload, sizeof(payload), &ev);
    send_frame_notify(h_event, VM_FRAME_BUTTON_EVENT, payload, (uint16_t)n);
}

int vm_link_send_audio(const uint8_t *block, size_t len)
{
    if (vm_hid_conn_handle() == VM_HID_CONN_NONE || !h_audio || !s_audio_notify) {
        return -1;
    }
    struct os_mbuf *om = ble_hs_mbuf_from_flat(block, (uint16_t)len);
    if (!om) return -1;
    return ble_gatts_notify_custom(vm_hid_conn_handle(), h_audio, om);
}

// ---- 命令执行(工作任务上下文) ----

static void execute_cmd(const link_cmd_t *cmd)
{
    switch (cmd->type) {
    case VM_FRAME_CMD_SET_ACTIVE:
        if (vm_profile_set_active(cmd->arg) == ESP_OK) {
            send_result(cmd->type, VM_RESULT_OK);
            if (s_profile_cb) s_profile_cb(cmd->arg, s_profile_cb_user);
        } else {
            send_result(cmd->type, VM_RESULT_BAD_ARG);
        }
        break;

    case VM_FRAME_CMD_GET_STATUS: {
        uint8_t payload[11];
        const size_t n = fill_status_payload(payload, sizeof(payload));
        send_frame_notify(h_status, VM_FRAME_STATUS_NOTIFY, payload, (uint16_t)n);
        break;
    }

    case VM_FRAME_CMD_DISCONNECT:
        // 先应答再断(应答走 notify,主机仍连着才能收到)。
        send_result(cmd->type, VM_RESULT_OK);
        vm_hid_disconnect();
        break;

    case VM_FRAME_PROFILE_CHUNK:
        // 分块重组与 NVS 存储尚未实现:这里明确应答"忙碌"。
        send_result(cmd->type, VM_RESULT_BUSY);
        break;

    default:
        ESP_LOGW(TAG, "未知帧类型 0x%02X", cmd->type);
        send_result(cmd->type, VM_RESULT_BAD_ARG);
        break;
    }
}

static void link_task(void *arg)
{
    (void)arg;
    link_cmd_t cmd;
    for (;;) {
        if (xQueueReceive(s_cmd_queue, &cmd, portMAX_DELAY) == pdTRUE) {
            execute_cmd(&cmd);
        }
    }
}

// ---- 帧解码(可从任意任务喂入,解码器仅此处访问) ----

static void dispatch_frame(const vm_frame_t *f, bool profile_channel)
{
    ESP_LOGI(TAG, "收到帧 type=0x%02X len=%u", f->type, f->len);
    if (f->version != VM_PROTO_VERSION) {
        send_result(f->type, VM_RESULT_BAD_VERSION);
        return;
    }
    if (profile_channel != (f->type == VM_FRAME_PROFILE_CHUNK)) {
        send_result(f->type, VM_RESULT_BAD_ARG);
        return;
    }
    if (f->type == VM_FRAME_CMD_PC_SESSION) {
        if (f->len != 1 || f->payload[0] > 1) {
            send_result(f->type, VM_RESULT_BAD_ARG);
            return;
        }
        atomic_store(&s_pc_seen_ms, (uint32_t)(esp_timer_get_time() / 1000));
        atomic_store(&s_pc_session, f->payload[0] == 1);
        return;
    }
    link_cmd_t cmd = { .type = f->type, .arg = 0 };
    switch (f->type) {
    case VM_FRAME_CMD_SET_ACTIVE:
        if (f->len != 1) {
            send_result(f->type, VM_RESULT_BAD_ARG);
            return;
        }
        cmd.arg = f->payload[0];
        break;
    case VM_FRAME_CMD_GET_STATUS:
    case VM_FRAME_CMD_DISCONNECT:
        if (f->len != 0) {
            send_result(f->type, VM_RESULT_BAD_ARG);
            return;
        }
        break;
    case VM_FRAME_PROFILE_CHUNK:
        break; // 仅入队应答 BUSY,载荷不解析
    default:
        send_result(f->type, VM_RESULT_BAD_ARG);
        return;
    }
    if (xQueueSend(s_cmd_queue, &cmd, 0) != pdTRUE) {
        send_result(f->type, VM_RESULT_BUSY);
    }
}

static void feed_decoder(vm_decoder_t *dec, const uint8_t *data, size_t len,
                         bool profile_channel)
{
    const vm_frame_t *view = NULL;
    for (size_t i = 0; i < len; i++) {
        const int rc = vm_decoder_feed(dec, data[i], &view);
        if (rc == 1) dispatch_frame(view, profile_channel);
        // rc==-1(坏帧)已自动复位;解码器无法定位坏帧来源,计数即可。
    }
}

// ---- GATT 访问回调(NimBLE host 任务:只解码入队) ----

int vm_link_chr_access(uint16_t conn_handle, uint16_t attr_handle,
                       struct ble_gatt_access_ctxt *ctxt, void *arg)
{
    (void)attr_handle;
    const uintptr_t chr = (uintptr_t)arg;

    if (chr == VM_UUID_CHR_STATUS && ctxt->op == BLE_GATT_ACCESS_OP_READ_CHR) {
        // 读回调与 notify 保持同一格式:VM_STATUS 上的载荷始终是协议帧,
        // 同一个特征值只有一种格式,客户端可通用解析。
        uint8_t payload[11];
        const size_t n = fill_status_payload(payload, sizeof(payload));
        if (n == 0) return BLE_ATT_ERR_UNLIKELY;
        uint8_t frame[VM_FRAME_MAX + 8];
        const size_t frame_len = vm_frame_encode(frame, sizeof(frame),
                                                 VM_PROTO_VERSION,
                                                 VM_FRAME_STATUS_NOTIFY, payload,
                                                 (uint16_t)n);
        if (frame_len == 0) return BLE_ATT_ERR_UNLIKELY;
        return os_mbuf_append(ctxt->om, frame, (uint16_t)frame_len);
    }

    ESP_LOGI(TAG, "写访问 chr=0x%04X op=%d", (unsigned)(uintptr_t)chr, ctxt->op);
    if (ctxt->op != BLE_GATT_ACCESS_OP_WRITE_CHR) return BLE_ATT_ERR_UNLIKELY;
    struct ble_gap_conn_desc desc;
    if (ble_gap_conn_find(conn_handle, &desc) != 0 || !desc.sec_state.encrypted) {
        return BLE_ATT_ERR_INSUFFICIENT_ENC;
    }

    // 缓冲必须容得下协议允许的最大帧(见 send_frame_notify 的说明)。
    uint8_t buf[VM_FRAME_MAX + 8];
    uint16_t len = 0;
    if (ble_hs_mbuf_to_flat(ctxt->om, buf, (uint16_t)sizeof(buf), &len) != 0) {
        return BLE_ATT_ERR_INVALID_PDU;
    }
    if (chr == VM_UUID_CHR_CTRL) {
        feed_decoder(&s_dec_ctrl, buf, len, false);
        return 0; // write-with-rsp:帧级结果经 VM_STATUS RESULT 通知
    }
    if (chr == VM_UUID_CHR_PROFILE) {
        feed_decoder(&s_dec_profile, buf, len, true);
        return 0;
    }
    return BLE_ATT_ERR_UNLIKELY;
}

void vm_link_gap_event(struct ble_gap_event *event)
{
    if (event->type == BLE_GAP_EVENT_SUBSCRIBE) {
        ESP_LOGI(TAG, "订阅 attr=%u notify=%d indicate=%d",
                 event->subscribe.attr_handle, event->subscribe.cur_notify,
                 event->subscribe.cur_indicate);
        if (event->subscribe.attr_handle == h_status) {
            s_status_notify = event->subscribe.cur_notify;
        } else if (event->subscribe.attr_handle == h_event) {
            s_event_notify = event->subscribe.cur_notify;
        } else if (event->subscribe.attr_handle == h_audio) {
            s_audio_notify = event->subscribe.cur_notify;
        }
        if (!s_status_notify || !s_event_notify) atomic_store(&s_pc_session, false);
    } else if (event->type == BLE_GAP_EVENT_CONNECT) {
        if (event->connect.status == 0) refresh_conn_itvl(event->connect.conn_handle);
    } else if (event->type == BLE_GAP_EVENT_CONN_UPDATE) {
        refresh_conn_itvl(event->conn_update.conn_handle);
    } else if (event->type == BLE_GAP_EVENT_DISCONNECT) {
        s_status_notify = s_event_notify = s_audio_notify = false;
        s_conn_itvl_125us = 0;
        atomic_store(&s_pc_session, false);
        vm_decoder_init(&s_dec_ctrl);
        vm_decoder_init(&s_dec_profile);
    }
}

// ---- 生命周期 ----

// 在 NimBLE host 任务上下文里刷新缓存的连接间隔(见 s_conn_itvl_125us 的说明)。
static void refresh_conn_itvl(uint16_t conn_handle)
{
    struct ble_gap_conn_desc desc;
    if (ble_gap_conn_find(conn_handle, &desc) == 0 && desc.conn_itvl > 0) {
        s_conn_itvl_125us = desc.conn_itvl;
    }
}

static int resolve_chr(const ble_uuid128_t *chr, uint16_t *val_handle)
{
    return ble_gatts_find_chr(&VM_SVC_UUID.u, &chr->u, NULL, val_handle);
}

esp_err_t vm_link_start(void)
{
    if (s_cmd_task) return ESP_OK; // 幂等

    s_cmd_queue = xQueueCreate(LINK_QUEUE_DEPTH, sizeof(link_cmd_t));
    if (!s_cmd_queue) return ESP_ERR_NO_MEM;
    if (xTaskCreate(link_task, "vm_link", LINK_TASK_STACK, NULL, 5,
                    &s_cmd_task) != pdPASS) {
        vQueueDelete(s_cmd_queue);
        s_cmd_queue = NULL;
        return ESP_ERR_NO_MEM;
    }
    vm_decoder_init(&s_dec_ctrl);
    vm_decoder_init(&s_dec_profile);
    ESP_LOGI(TAG, "vm_link 工作任务就绪");
    return ESP_OK;
}

void vm_link_services_attached(void)
{
    // host sync 后 GATT 表已分配句柄;幂等,host 重置后重试到成功为止。
    if (!s_handles_resolved) {
        if (resolve_chr(&CHR_VM_CTRL, &h_ctrl) != 0 ||
            resolve_chr(&CHR_VM_PROFILE, &h_profile) != 0 ||
            resolve_chr(&CHR_VM_STATUS, &h_status) != 0 ||
            resolve_chr(&CHR_VM_EVENT, &h_event) != 0 ||
            resolve_chr(&CHR_VM_AUDIO, &h_audio) != 0) {
            ESP_LOGW(TAG, "VM 特征值句柄解析失败,等待下次 sync 重试");
        } else {
            s_handles_resolved = true;
            ESP_LOGI(TAG, "VM 特征值句柄就绪 ctrl=%u profile=%u status=%u event=%u audio=%u",
                     h_ctrl, h_profile, h_status, h_event, h_audio);
        }
    }
    if (vm_link_start() != ESP_OK) {
        ESP_LOGE(TAG, "vm_link 任务启动失败,控制面不可用");
    }
}

bool vm_link_pc_active(void)
{
    const uint32_t now = (uint32_t)(esp_timer_get_time() / 1000);
    return vm_hid_connected() && s_status_notify && s_event_notify &&
           atomic_load(&s_pc_session) &&
           (uint32_t)(now - atomic_load(&s_pc_seen_ms)) < PC_SESSION_TTL_MS;
}

uint32_t vm_link_audio_backoff_ms(void)
{
    const uint16_t itvl = s_conn_itvl_125us;
    if (itvl > 0) {
        // 退避两个连接间隔,给控制器时间把 TX 队列排空(音频 20ms/块的产出速度
        // 必须与链路容量匹配,发不出去时先退一步),并夹在 20~60ms。
        uint32_t ms = ((uint32_t)itvl * 125u * 2u) / 100u;
        if (ms < VM_LINK_AUDIO_BACKOFF_MIN_MS) ms = VM_LINK_AUDIO_BACKOFF_MIN_MS;
        if (ms > VM_LINK_AUDIO_BACKOFF_MAX_MS) ms = VM_LINK_AUDIO_BACKOFF_MAX_MS;
        return ms;
    }
    return VM_LINK_AUDIO_BACKOFF_MIN_MS;
}

uint16_t vm_link_conn_itvl_125us(void)
{
    return s_conn_itvl_125us;
}

void vm_link_set_profile_listener(vm_link_profile_cb_t cb, void *user)
{
    s_profile_cb = cb;
    s_profile_cb_user = user;
}
