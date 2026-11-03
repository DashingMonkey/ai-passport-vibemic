// main/vm_link.h —— VibeMic 控制面:自定义 GATT 服务上的协议帧处理。
//
// 特征值与 UUID 见 vm_uuid.h(表挂在 vm_hid 的 GATT 表里,本模块提供
// 访问回调与帧逻辑);GATT 访问回调运行于 NimBLE host 任务,慢操作(NVS 写、
// 断连)转发到本模块的工作任务执行。
#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "host/ble_gap.h"

#ifdef __cplusplus
extern "C" {
#endif

// 设备固件协议版本(STATUS_NOTIFY.fwVer,BCD 风格)。
// 0x0002:新增 VM_AUDIO 语音串流;PC 会话活跃时 UP/DOWN 单击改由 PC 分发
//(设备不直发,PC 按此版本门控注入,旧固件双发)。
// 0x0003:音频 notify 失败不再中断整段录音(丢块 + 按连接间隔退避续传);PC 会话
//        活跃时 OK 不再发内置档位宏(只做 PTT);设备名改为 "AI Passport Vibe Mic"。
//        桌面端门控是"≥ 0x0002",因此 0x0003 向后兼容;同时值本身让 `vmctl status`
//        能区分设备上跑的是哪一版。
// 0x0004:DOWN 双击 = 两记 Esc(UP/DOWN 双击槽位落地,见 vm_keymap.h)。桌面端对
//        双击不做版本门控:0x0004 之前的固件对双击本来就不直发(keymap 无该槽位),
//        由 PC 注入不会双发 —— 所以这条语义不必等固件升级就能生效。
// 0x0005:OK 双击的第二下在本机被认成删除手势:不启动 PTT(麦克风不开、不推流),
//        屏上显示 DELETE。判定窗口与桌面端同一个 180ms;桌面端的删除动作不依赖
//        这个版本(它自己判自己),这条改动只影响设备端的行为与显示。
// 0x0006:加密的 PC_SESSION 租约(每5秒续租,15秒失效),不再以 CCCD 判活。
#define VM_LINK_FW_VERSION 0x0006

// ---- 以下三个入口由 vm_hid 调用 ----

// GATT 特征值访问回调(VM_CTRL/VM_PROFILE/VM_STATUS)。
int vm_link_chr_access(uint16_t conn_handle, uint16_t attr_handle,
                       struct ble_gatt_access_ctxt *ctxt, void *arg);

// GAP 事件转发(event 指针仅在调用期间有效,不得保存)。
void vm_link_gap_event(struct ble_gap_event *event);

// GATT 表注册完成后解析特征值句柄、启动工作任务。
esp_err_t vm_link_start(void);
// 兼容 vm_hid 调用点(成功即幂等)。
void vm_link_services_attached(void);

// ---- 供应用层使用 ----

// PC 会话活跃:BLE 已连接、STATUS/EVENT 均订阅且加密 PC_SESSION 租约未过期。
// 桌面链路每5秒续租;进程退出或清理失败后至多15秒恢复裸 HID。
bool vm_link_pc_active(void);

// 上报按键事件(VM_EVENT 特征值;未连接/未订阅时丢弃)。任意任务可调。
void vm_link_notify_button_event(uint8_t button, uint8_t vm_btn_ev);

// 发送一个音频块(VM_AUDIO 特征值,原始块字节,见 vm_audio_codec.h)。
// 返回 0 成功;未连接/未订阅/mbuf 失败返回非 0(调用方应丢块并退避续传,
// 不要因为单块失败就中断整段录音)。
// 任意任务可调(NimBLE 内部有主机锁);50 块/秒节奏,不打印日志防刷屏。
int vm_link_send_audio(const uint8_t *block, size_t len);

// 音频 notify 失败后的建议退避时长(ms):取当前连接间隔的两倍并夹在 20~60ms。
// 连接未建立或查询失败时返回下界。
uint32_t vm_link_audio_backoff_ms(void);

// 当前连接间隔(单位 1.25ms;未连接/未协商时为 0)。音频发送节奏按它计算
// (见 vm_audio_codec.h 的 vm_audio_pace_gap_ms)。
uint16_t vm_link_conn_itvl_125us(void);

// PC 切换激活 profile 后的回调(运行于 vm_link 工作任务;UI 更新须自持
// bsp_lvgl_lock)。
typedef void (*vm_link_profile_cb_t)(uint8_t profile_id, void *user);
void vm_link_set_profile_listener(vm_link_profile_cb_t cb, void *user);

#ifdef __cplusplus
}
#endif
