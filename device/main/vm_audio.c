// main/vm_audio.c —— 麦克风采集 → IMA-ADPCM → VM_AUDIO notify。
//
// 控制流:OK 按下(且 PC 会话活跃,vm_buttons 已门控)→
// vm_audio_start();OK 松开/链路失败 → vm_audio_stop()。按键回调只投递消息,
// 采集、编码、notify 都在本任务(优先级高于按键任务,避免 UI/输入抖动)。
//
// 块节奏:16kHz 单声道 20ms = 320 样本 → 160B ADPCM + 5B 块头 = 165B/块,
// 50 块/秒(64kbps)。notify 失败(未订阅/断开)即停止,防哑跑耗电。
//
// 空闲休眠:松开 OK 后不立刻关 codec —— 连续对话时每次重开 codec 会丢掉开头
// 几十毫秒;但一直挂着 ES8311 + I2S RX 是持续耗电,所以空闲一段时间后调
// bsp_audio_sleep()(强制 suspend 序列 + 寄存器回读校验),下次按下再唤醒。
#include "vm_audio.h"

#include <string.h>

#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

#include "bsp_audio.h"

#include "vm_audio_codec.h"
#include "vm_hid.h"
#include "vm_link.h"

static const char *TAG = "vm_audio";

#define VM_AUDIO_HZ 16000
#define VM_AUDIO_CHUNK_SAMPLES 320 // 20ms @16kHz
// 音频块失败策略:
// 上行拥塞时(NimBLE msys 池被占满 / 控制器 TX 队列满)`vm_link_send_audio()` 会返回非 0。
// 此时丢掉这一块、计数、按连接间隔退避后**继续**;只有连续失败到上限才认为链路
// 真的不可用。seq 照常递增,主机侧可从序号跳变看出丢了哪些块。
#define VM_AUDIO_MAX_CONSECUTIVE_DROPS 150 // 连续 150 块(3s)发不出去才放弃本次串流
#define VM_AUDIO_TASK_STACK 4096
#define VM_AUDIO_PRIO 6 // 高于 vm_input(5):录音不吃输入抖动
// 空闲多久后让 codec 进低功耗;也是任务轮询周期。
#define VM_AUDIO_IDLE_SLEEP_MS 8000
#define VM_AUDIO_IDLE_POLL_MS 1000

typedef enum {
    VM_AUDIO_MSG_START,
    VM_AUDIO_MSG_STOP,
} audio_msg_t;

static QueueHandle_t s_queue;
static TaskHandle_t s_task;
static volatile bool s_streaming;
static bool s_codec_inited;
static bool s_codec_sleeping;
// 最近一次会话/消息活动时刻(µs);空闲计时用,0 = 尚未活动过。
static int64_t s_last_active_us;

void vm_audio_start(void)
{
    if (!s_queue) return;
    const audio_msg_t msg = VM_AUDIO_MSG_START;
    (void)xQueueSend(s_queue, &msg, 0);
}

void vm_audio_stop(void)
{
    if (!s_queue) return;
    const audio_msg_t msg = VM_AUDIO_MSG_STOP;
    (void)xQueueSend(s_queue, &msg, 0);
}

bool vm_audio_active(void)
{
    return s_streaming;
}

static bool open_codec(void)
{
    if (!s_codec_inited) {
        const esp_err_t e = bsp_audio_init();
        if (e != ESP_OK) {
            ESP_LOGE(TAG, "bsp_audio_init: %s", esp_err_to_name(e));
            return false;
        }
        s_codec_inited = true;
    }
    // 上一次空闲后 codec 已休眠:先唤醒(恢复休眠前的格式),再确认本次格式。
    if (s_codec_sleeping) {
        const esp_err_t e = bsp_audio_wake();
        if (e != ESP_OK) {
            ESP_LOGE(TAG, "bsp_audio_wake: %s", esp_err_to_name(e));
            return false;
        }
        s_codec_sleeping = false;
    }
    const esp_err_t e = bsp_audio_set_format(VM_AUDIO_HZ, 16, 1);
    if (e != ESP_OK) {
        ESP_LOGE(TAG, "set_format %dHz: %s", VM_AUDIO_HZ, esp_err_to_name(e));
        return false;
    }
    return true;
}

// 一次完整录音会话:阻塞读 I2S → 编码 → notify,直到 STOP 或链路失败。
static void stream_session(void)
{
    static int16_t pcm[VM_AUDIO_CHUNK_SAMPLES];
    static uint8_t pkt[VM_AUDIO_CHUNK_HDR + VM_AUDIO_CHUNK_SAMPLES / 2];

    if (!open_codec()) return;

    vm_adpcm_state_t st = { 0, 0 };
    uint16_t seq = 0;
    size_t chunks = 0;
    uint32_t dropped = 0;
    uint32_t consecutive_drops = 0;
    // 背压:按链路能力限制发送节奏(间隔 > 20ms 的链路只有 <50 包/s 的容量,
    // 不限制产出就会把 msys 池占满。见 vm_audio_codec.h 的 vm_audio_pace_gap_ms)。
    const uint32_t gap_ms = vm_audio_pace_gap_ms(vm_link_conn_itvl_125us());
    int64_t next_send_us = 0;
    s_streaming = true;
    ESP_LOGI(TAG, "串流开始 16kHz/mono/ADPCM(发送间隔下限 %ums)", (unsigned)gap_ms);

    while (s_streaming) {
        if (!vm_link_pc_active()) break;
        audio_msg_t msg;
        if (xQueueReceive(s_queue, &msg, 0) == pdTRUE && msg == VM_AUDIO_MSG_STOP) {
            break;
        }
        // 到点之前先等一等(等待期间麦克风数据由 I2S 环形缓冲自然丢弃,
        // 不会积压成延迟;丢的是"最新之外"的旧数据)。
        const int64_t now = esp_timer_get_time();
        if (next_send_us > now) {
            vTaskDelay(pdMS_TO_TICKS((uint32_t)((next_send_us - now) / 1000)));
        }

        const esp_err_t e = bsp_audio_read(pcm, sizeof(pcm));
        if (e != ESP_OK) {
            ESP_LOGE(TAG, "采集失败: %s", esp_err_to_name(e));
            break;
        }
        const size_t n = vm_audio_pack_chunk(pkt, sizeof(pkt), seq++, pcm,
                                             VM_AUDIO_CHUNK_SAMPLES, &st);
        if (n == 0) {
            ESP_LOGE(TAG, "音频打包失败,停止串流");
            break;
        }
        const int64_t sent_at = esp_timer_get_time();
        next_send_us = (sent_at > next_send_us ? sent_at : next_send_us) +
                       (int64_t)gap_ms * 1000;

        if (vm_link_send_audio(pkt, n) != 0) {
            dropped++;
            consecutive_drops++;
            if (consecutive_drops == 1 || consecutive_drops % 25 == 0) {
                ESP_LOGW(TAG, "音频 notify 失败(连续 %u 块,累计丢 %u 块),退避 %ums 后继续",
                         (unsigned)consecutive_drops, (unsigned)dropped,
                         (unsigned)vm_link_audio_backoff_ms());
            }
            if (consecutive_drops >= VM_AUDIO_MAX_CONSECUTIVE_DROPS) {
                ESP_LOGE(TAG, "连续 %u 块音频发不出去,判定链路不可用,停止本次串流",
                         (unsigned)consecutive_drops);
                break;
            }
            // 背压:给控制器时间把已排队的通知送出去(退避量取自当前连接间隔)。
            vTaskDelay(pdMS_TO_TICKS(vm_link_audio_backoff_ms()));
            continue;
        }
        consecutive_drops = 0;
        chunks++;
    }

    s_streaming = false;
    s_last_active_us = esp_timer_get_time();
    ESP_LOGI(TAG, "串流结束 %u 块 / %ums(丢 %u 块)", (unsigned)chunks,
             (unsigned)(chunks * VM_AUDIO_CHUNK_SAMPLES * 1000 / VM_AUDIO_HZ),
             (unsigned)dropped);
}

static void audio_task(void *arg)
{
    (void)arg;
    audio_msg_t msg;
    for (;;) {
        if (xQueueReceive(s_queue, &msg, pdMS_TO_TICKS(VM_AUDIO_IDLE_POLL_MS)) == pdTRUE) {
            if (msg == VM_AUDIO_MSG_START) {
                if (!s_streaming) stream_session();
            } else if (msg == VM_AUDIO_MSG_STOP) {
                s_streaming = false; // stream_session 每块间检查该标志
            }
            s_last_active_us = esp_timer_get_time();
            continue;
        }

        // 队列空:空闲够久就把 codec 睡下去(见文件头的功耗说明)。
        if (s_streaming || !s_codec_inited || s_codec_sleeping) continue;
        if (s_last_active_us == 0) continue;
        if (esp_timer_get_time() - s_last_active_us <
            (int64_t)VM_AUDIO_IDLE_SLEEP_MS * 1000) {
            continue;
        }
        const esp_err_t e = bsp_audio_sleep();
        // BSP 即使返回失败也已关闭 I2S 并进入 sleeping;下一次必须走 wake 恢复。
        s_codec_sleeping = true;
        if (e == ESP_OK) {
            s_last_active_us = 0;
            ESP_LOGI(TAG, "空闲 %dms,ES8311 已进入低功耗", VM_AUDIO_IDLE_SLEEP_MS);
        } else {
            // BSP 已进入 sleeping;保留状态,等下一次录音走 wake 重建。
            s_last_active_us = esp_timer_get_time();
            ESP_LOGW(TAG, "ES8311 空闲休眠失败: %s", esp_err_to_name(e));
        }
    }
}

esp_err_t vm_audio_init(void)
{
    if (s_task) return ESP_OK; // 幂等

    s_queue = xQueueCreate(4, sizeof(audio_msg_t));
    if (!s_queue) return ESP_ERR_NO_MEM;
    if (xTaskCreate(audio_task, "vm_audio", VM_AUDIO_TASK_STACK, NULL,
                    VM_AUDIO_PRIO, &s_task) != pdPASS) {
        vQueueDelete(s_queue);
        s_queue = NULL;
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}
