// Run the shipping idle task through a failed sleep and two wake attempts.
#include <assert.h>
#include <setjmp.h>
#include <stdio.h>
#include "../main/vm_audio.c"

static jmp_buf finished;
static int step, sleep_calls, wake_calls, read_calls;
static bool bsp_sleeping;
void idf_stub_log(const char *tag, const char *format, ...) { (void)tag; (void)format; }
const char *esp_err_to_name(esp_err_t e) { (void)e; return "injected"; }
int64_t esp_timer_get_time(void) { return 9000000; }
void vTaskDelay(TickType_t ticks) { (void)ticks; }
esp_err_t bsp_audio_init(void) { return ESP_OK; }
esp_err_t bsp_audio_sleep(void) { sleep_calls++; bsp_sleeping = true; return ESP_FAIL; }
esp_err_t bsp_audio_wake(void) {
    wake_calls++;
    if (wake_calls == 1) return ESP_FAIL;
    bsp_sleeping = false; return ESP_OK;
}
esp_err_t bsp_audio_set_format(uint32_t hz, uint8_t bits, uint8_t ch) {
    (void)hz; (void)bits; (void)ch; return bsp_sleeping ? ESP_ERR_INVALID_STATE : ESP_OK;
}
esp_err_t bsp_audio_read(void *pcm, size_t len) {
    (void)pcm; (void)len; read_calls++; return ESP_FAIL; // end the recovered capture
}
bool vm_link_pc_active(void) { return true; }
uint16_t vm_link_conn_itvl_125us(void) { return 0; }
uint32_t vm_link_audio_backoff_ms(void) { return 20; }
int vm_link_send_audio(const uint8_t *bytes, size_t len) { (void)bytes; (void)len; return 0; }
QueueHandle_t xQueueCreate(UBaseType_t depth, UBaseType_t size) {
    (void)depth; (void)size; assert(false); return NULL;
}
BaseType_t xQueueSend(QueueHandle_t queue, const void *item, TickType_t wait) {
    (void)queue; (void)item; (void)wait; assert(false); return pdFALSE;
}
void vQueueDelete(QueueHandle_t queue) { (void)queue; assert(false); }
BaseType_t xTaskCreate(TaskFunction_t entry, const char *name, unsigned stack,
                      void *arg, unsigned priority, TaskHandle_t *task) {
    (void)entry; (void)name; (void)stack; (void)arg; (void)priority; (void)task;
    assert(false); return pdFALSE;
}
BaseType_t xQueueReceive(QueueHandle_t q, void *item, TickType_t wait) {
    (void)q;
    if (!wait) return pdFALSE;
    switch (step++) {
    case 0: return pdFALSE; // idle timeout invokes failing BSP sleep
    case 1:
        assert(sleep_calls == 1 && bsp_sleeping);
        *(audio_msg_t *)item = VM_AUDIO_MSG_START; return pdTRUE;
    case 2:
        assert(wake_calls == 1 && bsp_sleeping && read_calls == 0);
        *(audio_msg_t *)item = VM_AUDIO_MSG_START; return pdTRUE;
    default: longjmp(finished, 1);
    }
}
int main(void) {
    s_codec_inited = true; s_last_active_us = 1;
    if (!setjmp(finished)) audio_task(NULL);
    assert(sleep_calls == 1 && wake_calls == 2 && read_calls == 1);
    assert(!bsp_sleeping && !s_codec_sleeping);
    puts("vm_audio failed sleep/wake recovery: PASS");
    return 0;
}
