// main/vm_hid.h —— NimBLE BLE 栈:HID 键盘服务 + 电池/设备信息服务 + 广播/连接管理
// + 宏执行器(按键报文队列,统一 10ms 节流)。
//
// 服务布局:一条 BLE 连接同时承载 HID(数据面,OS 直接
// 消费)与 VibeMic 自定义服务(控制面,特征值逻辑在 vm_link,本模块只挂表)。
#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "esp_err.h"
#include "vm_profile.h"

#ifdef __cplusplus
extern "C" {
#endif

#define VM_HID_CONN_NONE 0xFFFF

// 蓝牙设备名(广播 + 扫描响应 + GAP Device Name 特征值共用)。
//
// 改名注意("Passport VibeMic" 为旧广播名,桌面端仍保留该别名):
// - 名字进的是扫描响应(AD 结构 ≤29 字节),当前 20 字节,安全;
// - Windows/macOS 会**缓存已配对设备的名字**:烧了新固件后系统蓝牙面板里可能仍显示旧名,
//   需要删除配对后重新配对才能刷新;
// - 桌面端(vibemic-protocol::uuids)对名字做"忽略大小写与空格"的归一化匹配,并保留
//   旧名别名,所以改名不会让正在用的桌面端连不上。
#define VM_DEVICE_NAME "AI Passport Vibe Mic"

// 启动 BLE 栈(NVS 须已初始化;幂等:已启动返回 ESP_ERR_INVALID_STATE)。
// 失败路径完全回滚,可重试。成功后自动开始可连接广播。
esp_err_t vm_hid_start(void);

bool vm_hid_connected(void);
uint16_t vm_hid_conn_handle(void); // 未连接返回 VM_HID_CONN_NONE

// 主动断开当前连接(REBOOT 前调用)。
esp_err_t vm_hid_disconnect(void);

// 硬重置蓝牙配对:清空全部 bond 记录,断开当前连接并恢复广播。
//
// 与 vm_hid_disconnect() 的区别:那条只断链,主机仍持有密钥、会自动重连;这条让设备
// **忘记所有配对** —— 主机侧的旧密钥随即失效,再连会在加密协商上失败,所以调用方必须
// 把"去系统蓝牙设置里删除本设备,再重新配对"这一步告诉用户(桌面端固件页的
// 「打开蓝牙设置」就是为这类场景准备的,见 desktop/AGENTS.md 的配对善后一节)。
//
// 可跨任务调用:store 与 GAP 的实现内部各自取 host 锁。
esp_err_t vm_hid_bond_reset(void);

// 把一段宏步骤投递给执行队列(立即返回,不阻塞按键回调)。
// 未连接或队列满时丢弃并计数 —— 按键语义是"尽力而为的遥控",不重试。
void vm_hid_run_steps(const vm_step_t *steps, uint8_t count);

// 电量状态更新(周期调用;缓存值并推送给已订阅主机)。
void vm_hid_battery_update(uint8_t pct, uint8_t charging);

// 读取缓存的电量状态(未知时 0xFF;VM_STATUS 组装用)。
void vm_hid_get_battery(uint8_t *pct, uint8_t *charging);

// 累计被丢弃的通知数(mbuf 分配失败;诊断"按键没反应"时先看它)。
int vm_hid_notify_drop_count(void);

typedef void (*vm_hid_conn_cb_t)(bool connected, void *user);
void vm_hid_set_conn_listener(vm_hid_conn_cb_t cb, void *user);

#ifdef __cplusplus
}
#endif
