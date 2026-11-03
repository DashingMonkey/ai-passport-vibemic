// Exercise the shipping GATT access callback and lease with injected transport state.
#include <assert.h>
#include <stdio.h>
#include "../main/vm_link.c"

struct os_mbuf { uint8_t bytes[VM_FRAME_MAX + 8]; uint16_t len; };
static struct os_mbuf notification;
static bool encrypted, connected = true;
static uint32_t now_ms;
static int queued;

void idf_stub_log(const char *tag, const char *format, ...) { (void)tag; (void)format; }
int64_t esp_timer_get_time(void) { return (int64_t)now_ms * 1000; }
uint32_t esp_get_free_heap_size(void) { return 1234; }
bool vm_hid_connected(void) { return connected; }
uint16_t vm_hid_conn_handle(void) { return 1; }
void vm_hid_get_battery(uint8_t *pct, uint8_t *charging) { *pct = 80; *charging = 0; }
uint8_t vm_profile_active_id(void) { return 0; }
int ble_gap_conn_find(uint16_t conn, struct ble_gap_conn_desc *out) {
    (void)conn; memset(out, 0, sizeof(*out)); out->sec_state.encrypted = encrypted; return 0;
}
void *ble_hs_mbuf_from_flat(const void *buf, uint16_t len) {
    memcpy(notification.bytes, buf, len); notification.len = len; return &notification;
}
int ble_gatts_notify_custom(uint16_t conn, uint16_t attr, struct os_mbuf *om) {
    (void)conn; (void)attr; (void)om; return 0;
}
int ble_hs_mbuf_to_flat(const struct os_mbuf *om, void *buf, uint16_t max, uint16_t *len) {
    assert(om->len <= max); memcpy(buf, om->bytes, om->len); *len = om->len; return 0;
}
int os_mbuf_append(struct os_mbuf *om, const void *buf, uint16_t len) {
    memcpy(om->bytes, buf, len); om->len = len; return 0;
}
BaseType_t xQueueSend(QueueHandle_t queue, const void *item, TickType_t wait) {
    (void)queue; (void)item; (void)wait; queued++; return pdTRUE;
}
QueueHandle_t xQueueCreate(UBaseType_t depth, UBaseType_t size) {
    (void)depth; (void)size; assert(false); return NULL;
}
BaseType_t xQueueReceive(QueueHandle_t queue, void *item, TickType_t wait) {
    (void)queue; (void)item; (void)wait; assert(false); return pdFALSE;
}
void vQueueDelete(QueueHandle_t queue) { (void)queue; assert(false); }
BaseType_t xTaskCreate(TaskFunction_t entry, const char *name, unsigned stack,
                      void *arg, unsigned priority, TaskHandle_t *task) {
    (void)entry; (void)name; (void)stack; (void)arg; (void)priority; (void)task;
    assert(false); return pdFALSE;
}
int ble_gatts_find_chr(const ble_uuid_t *svc, const ble_uuid_t *chr,
                      uint16_t *def_handle, uint16_t *val_handle) {
    (void)svc; (void)chr; (void)def_handle; (void)val_handle; assert(false); return -1;
}
esp_err_t vm_profile_set_active(uint8_t id) { (void)id; assert(false); return ESP_FAIL; }
esp_err_t vm_hid_disconnect(void) { assert(false); return ESP_FAIL; }

static int write_frame(uint16_t channel, uint8_t type, const uint8_t *data, uint16_t len) {
    struct os_mbuf om;
    om.len = (uint16_t)vm_frame_encode(om.bytes, sizeof(om.bytes), 1, type, data, len);
    struct ble_gatt_access_ctxt ctxt = { .op = BLE_GATT_ACCESS_OP_WRITE_CHR, .om = &om };
    return vm_link_chr_access(1, 0, &ctxt, (void *)(uintptr_t)channel);
}

int main(void) {
    const uint8_t active = 1;
    assert(write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_SET_ACTIVE, &active, 1) == BLE_ATT_ERR_INSUFFICIENT_ENC);
    assert(write_frame(VM_UUID_CHR_PROFILE, VM_FRAME_CMD_SET_ACTIVE, &active, 1) == BLE_ATT_ERR_INSUFFICIENT_ENC);
    assert(queued == 0);
    encrypted = true;
    write_frame(VM_UUID_CHR_PROFILE, VM_FRAME_CMD_SET_ACTIVE, &active, 1);
    write_frame(VM_UUID_CHR_PROFILE, VM_FRAME_CMD_DISCONNECT, NULL, 0);
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_PROFILE_CHUNK, NULL, 0);
    assert(queued == 0);
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_SET_ACTIVE, &active, 1);
    assert(queued == 1);
    write_frame(VM_UUID_CHR_PROFILE, VM_FRAME_PROFILE_CHUNK, NULL, 0);
    assert(queued == 2);

    h_status = 2; h_event = 3;
    s_status_notify = true; s_event_notify = true;
    assert(!vm_link_pc_active()); // stale CCCDs alone cannot activate PC mode
    write_frame(VM_UUID_CHR_PROFILE, VM_FRAME_CMD_PC_SESSION, &active, 1);
    assert(!vm_link_pc_active());
    now_ms = UINT32_MAX - 5000;
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_PC_SESSION, &active, 1);
    assert(vm_link_pc_active());
    now_ms += 14999;
    assert(vm_link_pc_active()); // timer wrap is harmless
    now_ms++;
    assert(!vm_link_pc_active());
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_PC_SESSION, &active, 1);
    assert(vm_link_pc_active());
    struct ble_gap_event event = { .type = BLE_GAP_EVENT_SUBSCRIBE,
        .subscribe = { .attr_handle = h_event, .cur_notify = false } };
    vm_link_gap_event(&event);
    assert(!vm_link_pc_active());
    event.subscribe.cur_notify = true;
    vm_link_gap_event(&event);
    assert(!vm_link_pc_active()); // resubscribe still needs a fresh handshake
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_PC_SESSION, &active, 1);
    assert(vm_link_pc_active());
    const uint8_t inactive = 0;
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_PC_SESSION, &inactive, 1);
    assert(!vm_link_pc_active());
    write_frame(VM_UUID_CHR_CTRL, VM_FRAME_CMD_PC_SESSION, &active, 1);
    event.type = BLE_GAP_EVENT_DISCONNECT;
    vm_link_gap_event(&event);
    assert(!vm_link_pc_active());
    puts("vm_link encryption/channel/lease tests: PASS");
    return 0;
}
