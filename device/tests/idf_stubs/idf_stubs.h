// tests/idf_stubs/idf_stubs.h —— 出货代码(main/vm_*.c、vibemic_app.c)的
// 语法/类型检查桩。
//
// 为什么需要它:main/CMakeLists.txt 只把 vibemic_app.c + vm_*.c 编进固件,而
// validate.sh 的宿主机测试全靠 demo_*.c / ui_pixel_math.c —— 也就是说真正出货的
// 代码在 IDF 之外**一次都没被编译过**,BSP API 一变就可能静默腐烂。
//
// 本桩集提供语法检查及出货模块故障注入测试所用的平台 API 声明:
//   - 只声明出货源码用到的类型/常量/函数,签名对齐 ESP-IDF 5.5.3 / NimBLE 真实 API;
//   - 本头不模拟行为;test_vm_*_runtime.c 提供各用例的行为桩;
//   - 与 tests/demo_stubs 相互独立(demo 测试的桩保持不变)。
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

/* ---------------- esp_err / log / system / timer ---------------- */

typedef int esp_err_t;
#define ESP_OK 0
#define ESP_FAIL -1
#define ESP_ERR_NO_MEM 0x101
#define ESP_ERR_INVALID_ARG 0x102
#define ESP_ERR_INVALID_STATE 0x103
#define ESP_ERR_INVALID_SIZE 0x104
#define ESP_ERR_NOT_FOUND 0x105
#define ESP_ERR_NOT_SUPPORTED 0x106
#define ESP_ERR_TIMEOUT 0x107
#define ESP_ERR_NVS_NO_FREE_PAGES 0x1101
#define ESP_ERR_NVS_NEW_VERSION_FOUND 0x1102

const char *esp_err_to_name(esp_err_t error);
void idf_stub_log(const char *tag, const char *format, ...);
#define ESP_LOGE(tag, ...) idf_stub_log(tag, __VA_ARGS__)
#define ESP_LOGW(tag, ...) idf_stub_log(tag, __VA_ARGS__)
#define ESP_LOGI(tag, ...) idf_stub_log(tag, __VA_ARGS__)
#define ESP_LOGD(tag, ...) idf_stub_log(tag, __VA_ARGS__)
#define ESP_ERROR_CHECK(x) do { (void)(x); } while (0)

uint32_t esp_get_free_heap_size(void);
int64_t esp_timer_get_time(void);

/* ---------------- FreeRTOS ---------------- */

typedef int BaseType_t;
typedef unsigned UBaseType_t;
typedef unsigned TickType_t;
typedef void *QueueHandle_t;
typedef void *SemaphoreHandle_t;
typedef struct idf_stub_task *TaskHandle_t;
typedef void (*TaskFunction_t)(void *);

#define pdTRUE 1
#define pdFALSE 0
#define pdPASS 1
#define portMAX_DELAY UINT32_MAX
#define pdMS_TO_TICKS(ms) (ms)
#define configMAX_PRIORITIES 25
#define NIMBLE_HS_STACK_SIZE 4096
#define NIMBLE_CORE 0

QueueHandle_t xQueueCreate(UBaseType_t depth, UBaseType_t item_size);
BaseType_t xQueueSend(QueueHandle_t queue, const void *item, TickType_t wait);
BaseType_t xQueueReceive(QueueHandle_t queue, void *item, TickType_t wait);
void vQueueDelete(QueueHandle_t queue);
BaseType_t xTaskCreate(TaskFunction_t entry, const char *name, unsigned stack,
                       void *arg, unsigned priority, TaskHandle_t *task);
BaseType_t xTaskCreatePinnedToCore(TaskFunction_t entry, const char *name,
                                   unsigned stack, void *arg, unsigned priority,
                                   TaskHandle_t *task, int core);
void vTaskDelete(TaskHandle_t task);
void vTaskDelay(TickType_t ticks);
void vTaskSuspend(TaskHandle_t task);
SemaphoreHandle_t xSemaphoreCreateBinary(void);
BaseType_t xSemaphoreGive(SemaphoreHandle_t semaphore);
BaseType_t xSemaphoreTake(SemaphoreHandle_t semaphore, TickType_t ticks);
void vSemaphoreDelete(SemaphoreHandle_t semaphore);

/* ---------------- NVS ---------------- */

typedef uint32_t nvs_handle_t;
typedef enum { NVS_READONLY = 1, NVS_READWRITE = 2 } nvs_open_mode_t;

esp_err_t nvs_open(const char *name, nvs_open_mode_t mode, nvs_handle_t *out);
void nvs_close(nvs_handle_t handle);
esp_err_t nvs_get_u8(nvs_handle_t handle, const char *key, uint8_t *out);
esp_err_t nvs_set_u8(nvs_handle_t handle, const char *key, uint8_t value);
esp_err_t nvs_commit(nvs_handle_t handle);
esp_err_t nvs_flash_init(void);
esp_err_t nvs_flash_erase(void);

/* ---------------- NimBLE:UUID / GATT ---------------- */

typedef struct {
    uint8_t type;
    uint8_t value[16];
} ble_uuid_t;

typedef struct { ble_uuid_t u; } ble_uuid16_t;
typedef struct { ble_uuid_t u; } ble_uuid128_t;

#define BLE_UUID16_INIT(id16)                                      \
    { .u = { .type = 2,                                            \
             .value = { (uint8_t)((id16) & 0xFF),                  \
                        (uint8_t)(((id16) >> 8) & 0xFF) } } }
#define BLE_UUID128_INIT(...)                                      \
    { .u = { .type = 3, .value = { __VA_ARGS__ } } }

struct os_mbuf;
struct ble_gatt_access_ctxt {
    int op;
    struct os_mbuf *om;
};
struct ble_gatt_chr_def;
struct ble_gatt_dsc_def;

enum {
    BLE_GATT_ACCESS_OP_READ_CHR = 0,
    BLE_GATT_ACCESS_OP_WRITE_CHR = 1,
    BLE_GATT_ACCESS_OP_READ_DSC = 2,
    BLE_GATT_ACCESS_OP_WRITE_DSC = 3,
};

/// 真实的 NimBLE 里这是**函数类型**,结构体字段写 `ble_gatt_access_fn *access_cb`
/// (指针指向函数类型),照抄可保证赋值类型完全一致。
typedef int ble_gatt_access_fn(uint16_t conn_handle, uint16_t attr_handle,
                               struct ble_gatt_access_ctxt *ctxt, void *arg);
/// ESP-IDF 的 fork 里 characteristics flags 是 32 位值类型(不是指针)。
typedef uint32_t ble_gatt_chr_flags;

#define BLE_GATT_CHR_F_READ 0x0002
#define BLE_GATT_CHR_F_WRITE_NO_RSP 0x0004
#define BLE_GATT_CHR_F_WRITE 0x0008
#define BLE_GATT_CHR_F_NOTIFY 0x0010
#define BLE_GATT_CHR_F_INDICATE 0x0020
#define BLE_GATT_CHR_F_WRITE_ENC 0x0100
#define BLE_GATT_CHR_F_READ_ENC 0x0200
#define BLE_GATT_CHR_F_NOTIFY_ENC 0x0400
#define BLE_GATT_SVC_TYPE_PRIMARY 1
#define BLE_ATT_F_READ 0x01
#define BLE_ATT_F_WRITE 0x02

struct ble_gatt_dsc_def {
    const ble_uuid_t *uuid;
    uint8_t att_flags;
    uint8_t min_key_size;
    ble_gatt_access_fn *access_cb;
    void *arg;
};

struct ble_gatt_cpfd;

struct ble_gatt_chr_def {
    const ble_uuid_t *uuid;
    ble_gatt_access_fn *access_cb;
    void *arg;
    struct ble_gatt_dsc_def *descriptors;
    ble_gatt_chr_flags flags;
    uint8_t min_key_size;
    uint16_t *val_handle;
    struct ble_gatt_cpfd *cpfd;
};

struct ble_gatt_svc_def {
    uint8_t type;
    const ble_uuid_t *uuid;
    const struct ble_gatt_svc_def *includes;
    const struct ble_gatt_chr_def *characteristics;
};

/* ---------------- NimBLE:host / ATT / mbuf ---------------- */

typedef struct {
    uint8_t type;
    uint8_t val[6];
} ble_addr_t;

int ble_att_set_preferred_mtu(uint16_t mtu);
int ble_gatts_count_cfg(const struct ble_gatt_svc_def *svcs);
int ble_gatts_add_svcs(const struct ble_gatt_svc_def *svcs);
int ble_gatts_find_chr(const ble_uuid_t *svc_uuid, const ble_uuid_t *chr_uuid,
                       uint16_t *def_handle, uint16_t *val_handle);
int ble_gatts_notify_custom(uint16_t conn_handle, uint16_t attr_handle,
                            struct os_mbuf *om);
void *ble_hs_mbuf_from_flat(const void *buf, uint16_t len);
int ble_hs_mbuf_to_flat(const struct os_mbuf *om, void *buf, uint16_t max_len,
                        uint16_t *out_len);
int os_mbuf_append(struct os_mbuf *om, const void *data, uint16_t len);
int os_mbuf_pktlen(const struct os_mbuf *om);
#define OS_MBUF_PKTLEN(om) os_mbuf_pktlen(om)

#define BLE_ATT_ERR_INVALID_PDU 0x04
#define BLE_ATT_ERR_UNLIKELY 0x0E
#define BLE_ATT_ERR_INSUFFICIENT_ENC 0x0F

/* ---------------- NimBLE:store ---------------- */

typedef enum {
    BLE_STORE_OBJ_TYPE_PEER_ADDR = 0,
    BLE_STORE_OBJ_TYPE_OUR_SEC = 2,
    BLE_STORE_OBJ_TYPE_CCCD = 3,
} ble_store_obj_type;

struct ble_store_status_event;

int ble_store_util_count(int type, int *out_count);
int ble_store_util_delete_peer(const ble_addr_t *peer_addr);
int ble_store_util_status_rr(struct ble_store_status_event *event, void *arg);
int ble_store_clear(void);

/* ---------------- NimBLE:GAP ---------------- */

struct ble_gap_event {
    uint8_t type;
    union {
        struct { int status; uint16_t conn_handle; } connect;
        struct { int reason; uint16_t conn_handle; } disconnect;
        struct {
            uint16_t conn_handle;
            uint16_t attr_handle;
            bool cur_notify;
            bool cur_indicate;
        } subscribe;
        struct { uint16_t conn_handle; } repeat_pairing;
        struct { uint16_t conn_handle; uint16_t mtu; } mtu;
        struct { int status; uint16_t conn_handle; } conn_update;
    };
};

struct ble_gap_conn_desc {
    struct { unsigned encrypted : 1; } sec_state;
    ble_addr_t peer_id_addr;
    uint16_t conn_handle;
    uint16_t conn_itvl;      // 单位 1.25ms(音频退避量按它计算)
    uint16_t conn_latency;
    uint16_t supervision_timeout;
};

struct ble_gap_upd_params {
    uint16_t itvl_min;
    uint16_t itvl_max;
    uint16_t latency;
    uint16_t supervision_timeout;
    uint16_t min_ce_len;
    uint16_t max_ce_len;
};

struct ble_gap_adv_params {
    uint8_t conn_mode;
    uint8_t disc_mode;
    uint16_t itvl_min;
    uint16_t itvl_max;
};

struct ble_hs_adv_fields {
    uint8_t flags;
    uint8_t appearance_is_present;
    uint16_t appearance;
    const ble_uuid128_t *uuids128;
    uint8_t num_uuids128;
    uint8_t uuids128_is_complete;
    const uint8_t *name;
    uint8_t name_len;
    uint8_t name_is_complete;
};

typedef int (*ble_gap_event_fn)(struct ble_gap_event *event, void *arg);

#define BLE_HS_ADV_F_DISC_GEN 0x02
#define BLE_HS_ADV_F_BREDR_UNSUP 0x04
#define BLE_GAP_CONN_MODE_UND 0x01
#define BLE_GAP_DISC_MODE_GEN 0x02
#define BLE_HS_FOREVER INT32_MAX
#define BLE_ERR_REM_USER_CONN_TERM 0x13

enum {
    BLE_GAP_EVENT_CONNECT = 0,
    BLE_GAP_EVENT_DISCONNECT = 1,
    BLE_GAP_EVENT_ADV_COMPLETE = 2,
    BLE_GAP_EVENT_CONN_UPDATE = 3,
    BLE_GAP_EVENT_SUBSCRIBE = 10,
    BLE_GAP_EVENT_MTU = 11,
    BLE_GAP_EVENT_REPEAT_PAIRING = 13,
    BLE_GAP_EVENT_ENC_CHANGE = 14,
};

#define BLE_GAP_REPEAT_PAIRING_RETRY 1
#define BLE_GAP_REPEAT_PAIRING_IGNORE 2

int ble_gap_adv_set_fields(const struct ble_hs_adv_fields *fields);
int ble_gap_adv_rsp_set_fields(const struct ble_hs_adv_fields *fields);
int ble_gap_adv_start(uint8_t own_addr_type, const ble_addr_t *direct_addr,
                      int32_t duration_ms, const struct ble_gap_adv_params *params,
                      ble_gap_event_fn cb, void *arg);
int ble_gap_adv_stop(void);
int ble_gap_adv_active(void);
int ble_gap_terminate(uint16_t conn_handle, uint8_t hci_reason);
int ble_gap_update_params(uint16_t conn_handle, const struct ble_gap_upd_params *params);
int ble_gap_conn_find(uint16_t conn_handle, struct ble_gap_conn_desc *out_desc);
int ble_hs_util_ensure_addr(int prefer_random);
int ble_hs_id_infer_auto(int privacy, uint8_t *out_addr_type);

/* ---------------- NimBLE:host config / services / port ---------------- */

typedef void (*ble_hs_reset_fn)(int reason);
typedef void (*ble_hs_sync_fn)(void);

struct ble_hs_cfg_t {
    ble_hs_reset_fn reset_cb;
    ble_hs_sync_fn sync_cb;
    uint8_t sm_bonding;
    uint8_t sm_sc;
    uint8_t sm_mitm;
    uint8_t sm_io_cap;
    int (*store_status_cb)(struct ble_store_status_event *event, void *arg);
    void *store_status_arg;
    uint8_t sm_our_key_dist;
    uint8_t sm_their_key_dist;
};
extern struct ble_hs_cfg_t ble_hs_cfg;

#define BLE_HS_IO_NO_INPUT_OUTPUT 3

void ble_svc_gap_init(void);
void ble_svc_gatt_init(void);
int ble_svc_gap_device_name_set(const char *name);

esp_err_t nimble_port_init(void);
esp_err_t nimble_port_deinit(void);
int nimble_port_stop(void);
void nimble_port_run(void);
void nimble_port_freertos_deinit(void);
