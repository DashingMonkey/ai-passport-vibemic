// main/vm_audio.h —— OK 按住期间的麦克风采集与 BLE 串流任务。
#pragma once

#include <stdbool.h>

#include "esp_err.h"

// 启动串流(异步:仅向音频任务投递消息,可在按键回调上下文调用)。
// 采集参数固定 16kHz/16bit 单声道,20ms 一块(165B)经 VM_AUDIO notify。
void vm_audio_start(void);

// 停止串流(异步;块粒度内生效,≤ 20ms + 调度延迟)。
void vm_audio_stop(void);

// 当前是否正在串流(UI/诊断用)。
bool vm_audio_active(void);

// 创建音频任务(app_main 一次)。
esp_err_t vm_audio_init(void);
